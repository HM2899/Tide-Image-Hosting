use crate::app::AppState;
use crate::auth::{
    create_jwt, hash_password, load_user_by_identifier, map_user_unique_error, normalize_username,
    random_token, verify_password,
};
use crate::error::{AppError, AppResult, empty_ok};
use crate::services::security;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::Deserialize;
use serde_json::{Value, json};
use tide_shared::{AuthUser, LoginRequest, LoginResponse, RegisterRequest, ok};
use uuid::Uuid;

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/oauth/{provider}", get(oauth_start))
        .route("/oauth/{provider}/callback", get(oauth_callback))
        .route("/email/send-code", post(send_code))
        .route("/email/verify", post(verify_email))
        .route("/password/change", post(change_password))
        .route("/password/reset/request", post(reset_request))
        .route("/password/reset/confirm", post(reset_confirm))
        .route("/me", get(me))
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> AppResult<impl IntoResponse> {
    if req.email.trim().is_empty() || req.password.len() < 8 || req.username.trim().is_empty() {
        return Err(AppError::BadRequest(
            "email username and password are required".to_string(),
        ));
    }
    let username = normalize_username(&req.username)?;
    let settings: serde_json::Value =
        sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='auth'")
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| json!({"email_verification_required":true}));
    if settings
        .get("email_verification_required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
    {
        let code = req.code.as_deref().ok_or_else(|| {
            AppError::BadRequest("email verification code is required".to_string())
        })?;
        security::verify_email_code(&state, &req.email, "register", code).await?;
    }
    let password_hash = hash_password(&req.password)?;
    let user = sqlx::query_as::<_, crate::models::UserRow>(
        "INSERT INTO users (email,username,password_hash,role,status) VALUES ($1,$2,$3,'user','active') RETURNING id,email,username,password_hash,role,status,login_failed_count,locked_until",
    )
    .bind(req.email.trim().to_lowercase())
    .bind(&username)
    .bind(password_hash)
    .fetch_one(&state.pool)
    .await
    .map_err(map_user_unique_error)?;
    sqlx::query("INSERT INTO user_profiles (user_id,display_name) VALUES ($1,$2)")
        .bind(user.id)
        .bind(&user.username)
        .execute(&state.pool)
        .await?;
    let avatar_url: Option<String> = sqlx::query_scalar("SELECT avatar_url FROM users WHERE id=$1")
        .bind(user.id)
        .fetch_one(&state.pool)
        .await
        .ok()
        .flatten();
    let token = create_jwt(&user, &state.config.session_secret)?;
    let session = security::create_session(&state, user.id, None, None).await?;
    Ok(with_session_cookie(
        Json(ok(LoginResponse {
            token,
            user: auth_user(user, avatar_url),
        })),
        &session,
    ))
}

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> AppResult<impl IntoResponse> {
    let user = load_user_by_identifier(&state.pool, &req.identifier).await?;
    if user
        .locked_until
        .map(|locked_until| locked_until > chrono::Utc::now())
        .unwrap_or(false)
    {
        return Err(AppError::Forbidden(
            "too many failed login attempts".to_string(),
        ));
    }
    if !verify_password(&req.password, &user.password_hash)? {
        let failed_count = user.login_failed_count + 1;
        let security_settings: serde_json::Value =
            sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='security'")
                .fetch_optional(&state.pool)
                .await?
                .unwrap_or_else(|| json!({"login_failed_limit":10,"login_lock_minutes":30}));
        let limit = security_settings
            .get("login_failed_limit")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(10);
        let lock_minutes = security_settings
            .get("login_lock_minutes")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(30);
        if failed_count >= limit {
            sqlx::query("UPDATE users SET login_failed_count=$2, locked_until=now()+($3 || ' minutes')::interval, updated_at=now() WHERE id=$1")
                .bind(user.id)
                .bind(failed_count)
                .bind(lock_minutes.to_string())
                .execute(&state.pool)
                .await?;
        } else {
            sqlx::query("UPDATE users SET login_failed_count=$2, updated_at=now() WHERE id=$1")
                .bind(user.id)
                .bind(failed_count)
                .execute(&state.pool)
                .await?;
        }
        return Err(AppError::Unauthorized(
            "invalid username/email or password".to_string(),
        ));
    }
    if user.status != "active" {
        return Err(AppError::Forbidden("user is not active".to_string()));
    }
    sqlx::query(
        "UPDATE users SET login_failed_count=0, locked_until=NULL, updated_at=now() WHERE id=$1",
    )
    .bind(user.id)
    .execute(&state.pool)
    .await?;
    let avatar_url: Option<String> = sqlx::query_scalar("SELECT avatar_url FROM users WHERE id=$1")
        .bind(user.id)
        .fetch_one(&state.pool)
        .await
        .ok()
        .flatten();
    let token = create_jwt(&user, &state.config.session_secret)?;
    let session = security::create_session(&state, user.id, None, None).await?;
    Ok(with_session_cookie(
        Json(ok(LoginResponse {
            token,
            user: auth_user(user, avatar_url),
        })),
        &session,
    ))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> AppResult<impl IntoResponse> {
    if let Some(session) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(extract_session_cookie)
    {
        security::revoke_session(&state, &session).await?;
    }
    let mut response = empty_ok().into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("tide_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    Ok(response)
}

async fn oauth_start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> AppResult<Response> {
    let provider = oauth_provider(&state, &provider)?;
    ensure_oauth_configured(&provider)?;
    let state_token = random_token();
    let redirect_uri = oauth_redirect_uri(&state, provider.id)?;
    let scope = match provider.id {
        "github" => "read:user user:email",
        "linuxdo" => "openid profile email",
        _ => "",
    };
    let authorize_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
        provider.authorize_url,
        urlencoding::encode(&provider.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(scope),
        urlencoding::encode(&state_token),
    );
    let cookie = format!(
        "tide_oauth_{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600",
        provider.id, state_token
    );
    let mut response = Redirect::to(&authorize_url).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|err| AppError::BadRequest(err.to_string()))?,
    );
    Ok(response)
}

async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<OAuthCallbackQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let provider = oauth_provider(&state, &provider)?;
    ensure_oauth_configured(&provider)?;
    if let Some(error) = query.error {
        return Ok(oauth_redirect_response(
            &state,
            provider.id,
            None,
            Some(format!("第三方登录失败：{error}")),
        ));
    }
    let code = query
        .code
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("missing oauth code".to_string()))?;
    let expected_state = oauth_state_cookie(&headers, provider.id)
        .ok_or_else(|| AppError::BadRequest("missing oauth state".to_string()))?;
    if query.state.as_deref() != Some(expected_state.as_str()) {
        return Err(AppError::BadRequest("invalid oauth state".to_string()));
    }

    let token = exchange_oauth_code(&state, &provider, &code).await?;
    let profile = fetch_oauth_profile(&state, &provider, &token.access_token).await?;
    let user = upsert_oauth_user(&state, &provider, profile).await?;
    let jwt = create_jwt(&user, &state.config.session_secret)?;
    let session = security::create_session(&state, user.id, None, None).await?;
    Ok(oauth_redirect_response(
        &state,
        provider.id,
        Some((jwt, session)),
        None,
    ))
}

#[derive(Deserialize)]
struct SendCodeRequest {
    email: String,
    purpose: String,
}

#[derive(Deserialize)]
struct VerifyEmailRequest {
    email: String,
    purpose: String,
    code: String,
}

#[derive(Deserialize)]
struct ResetRequest {
    email: String,
}

#[derive(Deserialize)]
struct ResetConfirmRequest {
    email: String,
    code: String,
    password: String,
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    old_password: String,
    new_password: String,
}

async fn send_code(
    State(state): State<AppState>,
    Json(req): Json<SendCodeRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let purpose = normalize_purpose(&req.purpose)?;
    let code = security::verification_code();
    security::store_email_code(&state, &req.email, purpose, &code).await?;
    security::send_email_code(&state, &req.email, purpose, &code).await?;
    Ok(empty_ok())
}

async fn verify_email(
    State(state): State<AppState>,
    Json(req): Json<VerifyEmailRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let purpose = normalize_purpose(&req.purpose)?;
    security::verify_email_code(&state, &req.email, purpose, &req.code).await?;
    Ok(empty_ok())
}

async fn change_password(
    State(state): State<AppState>,
    user: crate::auth::CurrentUser,
    Json(req): Json<ChangePasswordRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    if req.new_password.len() < 8 {
        return Err(AppError::BadRequest(
            "password must be at least 8 characters".to_string(),
        ));
    }
    let existing: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=$1")
        .bind(user.id)
        .fetch_one(&state.pool)
        .await?;
    if !verify_password(&req.old_password, &existing)? {
        return Err(AppError::Forbidden("old password is invalid".to_string()));
    }
    let hash = hash_password(&req.new_password)?;
    sqlx::query("UPDATE users SET password_hash=$2, updated_at=now() WHERE id=$1")
        .bind(user.id)
        .bind(hash)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}

async fn reset_request(
    State(state): State<AppState>,
    Json(req): Json<ResetRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    let code = security::verification_code();
    security::store_email_code(&state, &req.email, "reset_password", &code).await?;
    security::send_email_code(&state, &req.email, "reset_password", &code).await?;
    Ok(empty_ok())
}

async fn reset_confirm(
    State(state): State<AppState>,
    Json(req): Json<ResetConfirmRequest>,
) -> AppResult<Json<tide_shared::ApiResponse<serde_json::Value>>> {
    if req.password.len() < 8 {
        return Err(AppError::BadRequest(
            "password must be at least 8 characters".to_string(),
        ));
    }
    security::verify_email_code(&state, &req.email, "reset_password", &req.code).await?;
    let hash = hash_password(&req.password)?;
    sqlx::query("UPDATE users SET password_hash=$2, updated_at=now() WHERE email=$1")
        .bind(req.email.trim().to_lowercase())
        .bind(hash)
        .execute(&state.pool)
        .await?;
    Ok(empty_ok())
}

async fn me(
    State(state): State<AppState>,
    user: crate::auth::CurrentUser,
) -> Json<tide_shared::ApiResponse<AuthUser>> {
    let avatar_url: Option<String> = sqlx::query_scalar("SELECT avatar_url FROM users WHERE id=$1")
        .bind(user.id)
        .fetch_one(&state.pool)
        .await
        .ok()
        .flatten();
    Json(ok(AuthUser {
        id: user.id,
        email: user.email,
        username: user.username,
        role: user.role,
        status: user.status,
        avatar_url,
    }))
}

#[derive(Clone)]
struct OAuthProvider {
    id: &'static str,
    client_id: String,
    client_secret: String,
    authorize_url: String,
    token_url: String,
    user_url: String,
}

#[derive(Deserialize)]
struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
}

#[derive(Clone)]
struct OAuthProfile {
    provider_user_id: String,
    username: String,
    email: String,
    avatar_url: String,
}

#[derive(Deserialize)]
struct GitHubUser {
    id: i64,
    login: String,
    email: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct GitHubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

#[derive(Deserialize)]
struct LinuxDoUser {
    sub: Option<String>,
    id: Option<Value>,
    username: Option<String>,
    login: Option<String>,
    name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
    picture: Option<String>,
}

fn oauth_provider(state: &AppState, provider: &str) -> AppResult<OAuthProvider> {
    match provider {
        "github" => Ok(OAuthProvider {
            id: "github",
            client_id: state.config.github_oauth_client_id.clone(),
            client_secret: state.config.github_oauth_client_secret.clone(),
            authorize_url: state.config.github_oauth_authorize_url.clone(),
            token_url: state.config.github_oauth_token_url.clone(),
            user_url: state.config.github_oauth_user_url.clone(),
        }),
        "linuxdo" => Ok(OAuthProvider {
            id: "linuxdo",
            client_id: state.config.linuxdo_oauth_client_id.clone(),
            client_secret: state.config.linuxdo_oauth_client_secret.clone(),
            authorize_url: state.config.linuxdo_oauth_authorize_url.clone(),
            token_url: state.config.linuxdo_oauth_token_url.clone(),
            user_url: state.config.linuxdo_oauth_user_url.clone(),
        }),
        _ => Err(AppError::NotFound("oauth provider not found".to_string())),
    }
}

fn ensure_oauth_configured(provider: &OAuthProvider) -> AppResult<()> {
    if provider.client_id.trim().is_empty() || provider.client_secret.trim().is_empty() {
        return Err(AppError::Forbidden(format!(
            "{} oauth is not configured",
            provider.id
        )));
    }
    Ok(())
}

fn oauth_redirect_uri(state: &AppState, provider: &str) -> AppResult<String> {
    let base = state.config.public_base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(AppError::BadRequest(
            "PUBLIC_BASE_URL is required for oauth login".to_string(),
        ));
    }
    Ok(format!("{base}/api/auth/oauth/{provider}/callback"))
}

async fn exchange_oauth_code(
    state: &AppState,
    provider: &OAuthProvider,
    code: &str,
) -> AppResult<OAuthTokenResponse> {
    let redirect_uri = oauth_redirect_uri(state, provider.id)?;
    let response = state
        .http
        .post(&provider.token_url)
        .header(ACCEPT, "application/json")
        .form(&[
            ("client_id", provider.client_id.as_str()),
            ("client_secret", provider.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::External(format!(
            "oauth token exchange failed with {status}"
        )));
    }
    serde_json::from_str(&body)
        .map_err(|err| AppError::External(format!("invalid oauth token response: {err}")))
}

async fn fetch_oauth_profile(
    state: &AppState,
    provider: &OAuthProvider,
    access_token: &str,
) -> AppResult<OAuthProfile> {
    match provider.id {
        "github" => fetch_github_profile(state, provider, access_token).await,
        "linuxdo" => fetch_linuxdo_profile(state, provider, access_token).await,
        _ => Err(AppError::NotFound("oauth provider not found".to_string())),
    }
}

async fn fetch_github_profile(
    state: &AppState,
    _provider: &OAuthProvider,
    access_token: &str,
) -> AppResult<OAuthProfile> {
    let user: GitHubUser = state
        .http
        .get(&state.config.github_oauth_user_url)
        .bearer_auth(access_token)
        .header(USER_AGENT, "tide-image-hosting")
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let email = if let Some(email) = user.email.filter(|value| !value.trim().is_empty()) {
        email
    } else {
        let emails: Vec<GitHubEmail> = state
            .http
            .get(&state.config.github_oauth_emails_url)
            .bearer_auth(access_token)
            .header(USER_AGENT, "tide-image-hosting")
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        emails
            .iter()
            .find(|item| item.primary && item.verified)
            .or_else(|| emails.iter().find(|item| item.verified))
            .map(|item| item.email.clone())
            .ok_or_else(|| AppError::BadRequest("github email is not available".to_string()))?
    };
    Ok(OAuthProfile {
        provider_user_id: user.id.to_string(),
        username: user.login,
        email,
        avatar_url: user.avatar_url.unwrap_or_default(),
    })
}

async fn fetch_linuxdo_profile(
    state: &AppState,
    provider: &OAuthProvider,
    access_token: &str,
) -> AppResult<OAuthProfile> {
    let user: LinuxDoUser = state
        .http
        .get(&provider.user_url)
        .bearer_auth(access_token)
        .header(ACCEPT, "application/json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let provider_user_id = user
        .sub
        .or_else(|| user.id.as_ref().and_then(json_value_to_string))
        .ok_or_else(|| AppError::BadRequest("linuxdo user id is not available".to_string()))?;
    let username = user
        .username
        .or(user.login)
        .or(user.name)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("linuxdo-{provider_user_id}"));
    let email = user
        .email
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("linuxdo email is not available".to_string()))?;
    Ok(OAuthProfile {
        provider_user_id,
        username,
        email,
        avatar_url: user.avatar_url.or(user.picture).unwrap_or_default(),
    })
}

async fn upsert_oauth_user(
    state: &AppState,
    provider: &OAuthProvider,
    profile: OAuthProfile,
) -> AppResult<crate::models::UserRow> {
    if let Some(user) = sqlx::query_as::<_, crate::models::UserRow>(
        "SELECT u.id,u.email,u.username,u.password_hash,u.role,u.status,u.login_failed_count,u.locked_until
         FROM oauth_accounts oa
         JOIN users u ON u.id=oa.user_id
         WHERE oa.provider=$1 AND oa.provider_user_id=$2",
    )
    .bind(provider.id)
    .bind(&profile.provider_user_id)
    .fetch_optional(&state.pool)
    .await?
    {
        update_oauth_account(state, user.id, provider.id, &profile).await?;
        if user.status != "active" {
            return Err(AppError::Forbidden("user is not active".to_string()));
        }
        return Ok(user);
    }

    let email = profile.email.trim().to_lowercase();
    let user = if let Some(user) = sqlx::query_as::<_, crate::models::UserRow>(
        "SELECT id,email,username,password_hash,role,status,login_failed_count,locked_until FROM users WHERE lower(email)=$1",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?
    {
        if user.status != "active" {
            return Err(AppError::Forbidden("user is not active".to_string()));
        }
        user
    } else {
        create_oauth_user(state, provider, &profile, &email).await?
    };
    link_oauth_account(state, user.id, provider.id, &profile, &email).await?;
    Ok(user)
}

async fn create_oauth_user(
    state: &AppState,
    provider: &OAuthProvider,
    profile: &OAuthProfile,
    email: &str,
) -> AppResult<crate::models::UserRow> {
    let username = unique_oauth_username(state, provider.id, &profile.username).await?;
    let password_hash = hash_password(&random_token())?;
    let user = sqlx::query_as::<_, crate::models::UserRow>(
        "INSERT INTO users (email,username,password_hash,avatar_url,role,status) VALUES ($1,$2,$3,$4,'user','active') RETURNING id,email,username,password_hash,role,status,login_failed_count,locked_until",
    )
    .bind(email)
    .bind(&username)
    .bind(password_hash)
    .bind(profile.avatar_url.trim())
    .fetch_one(&state.pool)
    .await
    .map_err(map_user_unique_error)?;
    sqlx::query("INSERT INTO user_profiles (user_id,display_name) VALUES ($1,$2)")
        .bind(user.id)
        .bind(&user.username)
        .execute(&state.pool)
        .await?;
    Ok(user)
}

async fn unique_oauth_username(
    state: &AppState,
    provider: &str,
    preferred: &str,
) -> AppResult<String> {
    let base = oauth_username_base(provider, preferred);
    for index in 0..20 {
        let candidate = if index == 0 {
            base.clone()
        } else {
            format!("{base}-{index}")
        };
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE lower(username)=lower($1))",
        )
        .bind(&candidate)
        .fetch_one(&state.pool)
        .await?;
        if !exists {
            return normalize_username(&candidate);
        }
    }
    let suffix = &random_token()[..8];
    normalize_username(&format!("{base}-{suffix}"))
}

fn oauth_username_base(provider: &str, preferred: &str) -> String {
    let sanitized: String = preferred
        .trim()
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                Some(ch)
            } else if ch.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-').trim_matches('_');
    let base = if sanitized.is_empty() {
        format!("{provider}-user")
    } else {
        sanitized.to_string()
    };
    base.chars().take(48).collect()
}

async fn update_oauth_account(
    state: &AppState,
    user_id: Uuid,
    provider: &str,
    profile: &OAuthProfile,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE oauth_accounts SET provider_username=$4,email=$5,avatar_url=$6,updated_at=now() WHERE provider=$1 AND provider_user_id=$2 AND user_id=$3",
    )
    .bind(provider)
    .bind(&profile.provider_user_id)
    .bind(user_id)
    .bind(profile.username.trim())
    .bind(profile.email.trim().to_lowercase())
    .bind(profile.avatar_url.trim())
    .execute(&state.pool)
    .await?;
    Ok(())
}

async fn link_oauth_account(
    state: &AppState,
    user_id: Uuid,
    provider: &str,
    profile: &OAuthProfile,
    email: &str,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO oauth_accounts (user_id,provider,provider_user_id,provider_username,email,avatar_url)
         VALUES ($1,$2,$3,$4,$5,$6)
         ON CONFLICT (provider, provider_user_id)
         DO UPDATE SET user_id=EXCLUDED.user_id,provider_username=EXCLUDED.provider_username,email=EXCLUDED.email,avatar_url=EXCLUDED.avatar_url,updated_at=now()",
    )
    .bind(user_id)
    .bind(provider)
    .bind(&profile.provider_user_id)
    .bind(profile.username.trim())
    .bind(email)
    .bind(profile.avatar_url.trim())
    .execute(&state.pool)
    .await?;
    Ok(())
}

fn oauth_redirect_response(
    state: &AppState,
    provider: &str,
    auth: Option<(String, String)>,
    error: Option<String>,
) -> Response {
    let base = state.config.public_base_url.trim().trim_end_matches('/');
    let target = if let Some((token, _)) = auth.as_ref() {
        format!(
            "{base}/#oauth:token={}&provider={}",
            urlencoding::encode(token),
            provider
        )
    } else {
        let message = error.unwrap_or_else(|| "第三方登录失败".to_string());
        format!(
            "{base}/#oauth:error={}&provider={}",
            urlencoding::encode(&message),
            provider
        )
    };
    let mut response = Redirect::to(&target).into_response();
    let clear_oauth = format!("tide_oauth_{provider}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if let Ok(value) = HeaderValue::from_str(&clear_oauth) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    if let Some((_, session)) = auth {
        let session_cookie =
            format!("tide_session={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000");
        if let Ok(value) = HeaderValue::from_str(&session_cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

fn oauth_state_cookie(headers: &HeaderMap, provider: &str) -> Option<String> {
    let name = format!("tide_oauth_{provider}");
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| extract_named_cookie(cookies, &name))
}

fn extract_named_cookie(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn json_value_to_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
        .or_else(|| value.as_u64().map(|number| number.to_string()))
}

fn with_session_cookie<T: serde::Serialize>(body: Json<T>, session: &str) -> Response {
    let mut response = body.into_response();
    let cookie = format!("tide_session={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

fn extract_session_cookie(header: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == "tide_session").then(|| value.to_string())
    })
}

fn normalize_purpose(purpose: &str) -> AppResult<&'static str> {
    match purpose {
        "register" => Ok("register"),
        "login" => Ok("login"),
        "reset_password" => Ok("reset_password"),
        "change_email" => Ok("change_email"),
        _ => Err(AppError::BadRequest("invalid email purpose".to_string())),
    }
}

fn auth_user(user: crate::models::UserRow, avatar_url: Option<String>) -> AuthUser {
    AuthUser {
        id: user.id,
        email: user.email,
        username: user.username,
        role: user.role,
        status: user.status,
        avatar_url,
    }
}
