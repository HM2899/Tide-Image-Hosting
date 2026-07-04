use crate::app::{AppConfig, AppState};
use crate::auth::{hash_token, random_token};
use crate::error::{AppError, AppResult};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{Duration, Utc};
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const REDACTED_VALUE: &str = "********";
const ENCRYPTED_PREFIX: &str = "enc:v1:";
const SENSITIVE_FIELDS: &[&str] = &[
    "client_secret",
    "secret_access_key",
    "private_key",
    "private_key_passphrase",
    "api_key",
    "api_token",
    "fastapi_token",
    "access_token",
    "refresh_token",
    "session_token",
    "secret_key",
    "password",
    "password_encrypted",
    "smtp_password",
];

pub fn verification_code() -> String {
    let code: u32 = rand::random::<u32>() % 1_000_000;
    format!("{code:06}")
}

pub fn code_hash(email: &str, purpose: &str, code: &str, secret: &str) -> String {
    hex::encode(Sha256::digest(format!("{email}:{purpose}:{code}:{secret}")))
}

pub async fn store_email_code(
    state: &AppState,
    email: &str,
    purpose: &str,
    code: &str,
) -> AppResult<()> {
    let normalized = email.trim().to_lowercase();
    let hash = code_hash(&normalized, purpose, code, &state.config.session_secret);
    sqlx::query(
        "INSERT INTO email_verifications (email,code_hash,purpose,expires_at) VALUES ($1,$2,$3,$4)",
    )
    .bind(normalized)
    .bind(hash)
    .bind(purpose)
    .bind(Utc::now() + Duration::minutes(10))
    .execute(&state.pool)
    .await?;
    Ok(())
}

pub async fn verify_email_code(
    state: &AppState,
    email: &str,
    purpose: &str,
    code: &str,
) -> AppResult<()> {
    let normalized = email.trim().to_lowercase();
    let hash = code_hash(&normalized, purpose, code, &state.config.session_secret);
    let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM email_verifications WHERE email=$1 AND purpose=$2 AND code_hash=$3 AND used_at IS NULL AND expires_at > now() ORDER BY created_at DESC LIMIT 1")
        .bind(&normalized)
        .bind(purpose)
        .bind(hash)
        .fetch_optional(&state.pool)
        .await?;
    let id =
        id.ok_or_else(|| AppError::BadRequest("email verification code is invalid".to_string()))?;
    sqlx::query("UPDATE email_verifications SET used_at=now() WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

pub async fn send_email_code(
    state: &AppState,
    email: &str,
    purpose: &str,
    code: &str,
) -> AppResult<()> {
    let smtp = active_smtp(&state.pool).await?;
    if smtp.host.is_empty() || !smtp.enabled {
        sqlx::query("INSERT INTO system_logs (level,module,message,context_json) VALUES ('info','email','email code generated without smtp',$1)")
            .bind(serde_json::json!({"email":email,"purpose":purpose}))
            .execute(&state.pool)
            .await?;
        return Ok(());
    }
    let from = format!("{} <{}>", smtp.from_name, smtp.from_email)
        .parse::<Mailbox>()
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    let to = email
        .parse::<Mailbox>()
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    let message = Message::builder()
        .from(from)
        .to(to)
        .subject("潮汐图床邮箱验证码")
        .body(format!("你的验证码是 {code}，10 分钟内有效。"))
        .map_err(|err| AppError::External(err.to_string()))?;
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
        .map_err(|err| AppError::External(err.to_string()))?
        .port(smtp.port as u16);
    if let Some(auth) = smtp_auth(&state.config, &smtp)? {
        builder = builder.credentials(Credentials::new(auth.username, auth.password));
    }
    let transport = builder.build();
    transport
        .send(message)
        .await
        .map_err(|err| AppError::External(err.to_string()))?;
    Ok(())
}

pub async fn create_session(
    state: &AppState,
    user_id: Uuid,
    ip: Option<String>,
    user_agent: Option<String>,
) -> AppResult<String> {
    let token = random_token();
    let token_hash = hash_token(&token);
    sqlx::query("INSERT INTO sessions (user_id,token_hash,ip,user_agent,expires_at) VALUES ($1,$2,$3,$4,$5)")
        .bind(user_id)
        .bind(token_hash)
        .bind(ip)
        .bind(user_agent)
        .bind(Utc::now() + Duration::days(30))
        .execute(&state.pool)
        .await?;
    Ok(token)
}

pub async fn user_id_from_session(state: &AppState, token: &str) -> AppResult<Uuid> {
    let hash = hash_token(token);
    sqlx::query_scalar("SELECT user_id FROM sessions WHERE token_hash=$1 AND revoked_at IS NULL AND expires_at > now()")
        .bind(hash)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid session".to_string()))
}

pub async fn revoke_session(state: &AppState, token: &str) -> AppResult<()> {
    let hash = hash_token(token);
    sqlx::query("UPDATE sessions SET revoked_at=now() WHERE token_hash=$1")
        .bind(hash)
        .execute(&state.pool)
        .await?;
    Ok(())
}

pub fn encrypt_value(config: &AppConfig, value: &str) -> AppResult<String> {
    let key = Sha256::digest(config.encryption_key.as_bytes());
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|err| AppError::BadRequest(err.to_string()))?;
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let encrypted = cipher
        .encrypt(nonce, value.as_bytes())
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    Ok(format!(
        "{ENCRYPTED_PREFIX}{}.{}",
        STANDARD.encode(nonce_bytes),
        STANDARD.encode(encrypted)
    ))
}

pub fn decrypt_value(config: &AppConfig, value: &str) -> AppResult<String> {
    let encrypted_value = if let Some(value) = value.strip_prefix(ENCRYPTED_PREFIX) {
        value
    } else if is_encrypted_value(value) {
        value
    } else {
        return Ok(value.to_string());
    };
    decrypt_encrypted_pair(config, encrypted_value)
}

fn decrypt_encrypted_pair(config: &AppConfig, value: &str) -> AppResult<String> {
    let Some((nonce, encrypted)) = value.split_once('.') else {
        return Err(AppError::BadRequest(
            "encrypted value is invalid".to_string(),
        ));
    };
    let key = Sha256::digest(config.encryption_key.as_bytes());
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|err| AppError::BadRequest(err.to_string()))?;
    let nonce_bytes = STANDARD
        .decode(nonce)
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    let encrypted_bytes = STANDARD
        .decode(encrypted)
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce_bytes), encrypted_bytes.as_ref())
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    String::from_utf8(plain).map_err(|err| AppError::BadRequest(err.to_string()))
}

pub fn encrypt_sensitive_json(config: &AppConfig, mut value: Value) -> AppResult<Value> {
    transform_sensitive_json(&mut value, &mut |text| {
        if text.is_empty() || text == REDACTED_VALUE || is_encrypted_value(text) {
            Ok(text.to_string())
        } else {
            encrypt_value(config, text)
        }
    })?;
    Ok(value)
}

pub fn decrypt_sensitive_json(config: &AppConfig, mut value: Value) -> AppResult<Value> {
    transform_sensitive_json(&mut value, &mut |text| decrypt_value(config, text))?;
    Ok(value)
}

pub fn redact_sensitive_json(mut value: Value) -> Value {
    redact_sensitive_json_value(&mut value);
    value
}

pub fn preserve_redacted_sensitive_json(incoming: Value, existing: Value) -> Value {
    match (incoming, existing) {
        (Value::Object(mut incoming), Value::Object(existing)) => {
            for (key, value) in incoming.iter_mut() {
                if is_sensitive_field(key)
                    && value
                        .as_str()
                        .is_some_and(|text| text.is_empty() || text == REDACTED_VALUE)
                    && let Some(existing_value) = existing.get(key)
                {
                    *value = existing_value.clone();
                    continue;
                }
                let previous = existing.get(key).cloned().unwrap_or(Value::Null);
                *value = preserve_redacted_sensitive_json(value.clone(), previous);
            }
            Value::Object(incoming)
        }
        (Value::Array(incoming), Value::Array(existing)) => Value::Array(
            incoming
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    preserve_redacted_sensitive_json(
                        value,
                        existing.get(index).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect(),
        ),
        (incoming, _) => incoming,
    }
}

fn transform_sensitive_json(
    value: &mut Value,
    transform: &mut impl FnMut(&str) -> AppResult<String>,
) -> AppResult<()> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_sensitive_field(key) {
                    if let Value::String(text) = value {
                        *text = transform(text)?;
                    } else {
                        transform_sensitive_json(value, transform)?;
                    }
                } else {
                    transform_sensitive_json(value, transform)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                transform_sensitive_json(item, transform)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn redact_sensitive_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_sensitive_field(key) {
                    if let Value::String(text) = value {
                        if !text.is_empty() {
                            *text = REDACTED_VALUE.to_string();
                        }
                    } else {
                        redact_sensitive_json_value(value);
                    }
                } else {
                    redact_sensitive_json_value(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_sensitive_json_value(item);
            }
        }
        _ => {}
    }
}

fn is_sensitive_field(field: &str) -> bool {
    let field = field.to_ascii_lowercase();
    SENSITIVE_FIELDS.contains(&field.as_str())
        || field.ends_with("_secret")
        || field.ends_with("_token")
        || field.ends_with("_password")
        || field.contains("private_key")
}

fn is_encrypted_value(value: &str) -> bool {
    if let Some(value) = value.strip_prefix(ENCRYPTED_PREFIX) {
        return encrypted_pair_looks_valid(value);
    }
    encrypted_pair_looks_valid(value)
}

fn encrypted_pair_looks_valid(value: &str) -> bool {
    let Some((nonce, encrypted)) = value.split_once('.') else {
        return false;
    };
    STANDARD
        .decode(nonce)
        .map(|bytes| bytes.len() == 12)
        .unwrap_or(false)
        && STANDARD.decode(encrypted).is_ok()
}

#[derive(sqlx::FromRow)]
struct SmtpRow {
    host: String,
    port: i32,
    username: String,
    password_encrypted: String,
    from_email: String,
    from_name: String,
    enabled: bool,
}

async fn active_smtp(pool: &sqlx::PgPool) -> AppResult<SmtpRow> {
    Ok(sqlx::query_as::<_, SmtpRow>(
        "SELECT host,port,username,password_encrypted,from_email,from_name,enabled FROM smtp_settings WHERE enabled=true ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?
    .unwrap_or(SmtpRow {
        host: String::new(),
        port: 587,
        username: String::new(),
        password_encrypted: String::new(),
        from_email: "noreply@example.com".to_string(),
        from_name: "潮汐图床".to_string(),
        enabled: false,
    }))
}

struct SmtpAuth {
    username: String,
    password: String,
}

fn smtp_auth(config: &AppConfig, smtp: &SmtpRow) -> AppResult<Option<SmtpAuth>> {
    let username = smtp.username.trim();
    if username.is_empty() {
        return Ok(None);
    }
    let password = decrypt_value(config, smtp.password_encrypted.trim())?;
    if password.is_empty() {
        return Err(AppError::BadRequest(
            "smtp password is not configured".to_string(),
        ));
    }
    Ok(Some(SmtpAuth {
        username: username.to_string(),
        password,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_config() -> AppConfig {
        AppConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            database_url: "postgres://example".to_string(),
            public_base_url: "http://localhost:8080".to_string(),
            session_secret: "session-secret".to_string(),
            encryption_key: "encryption-secret".to_string(),
            local_storage_root: "/tmp/tide".to_string(),
            local_storage_public_prefix: "/files".to_string(),
            ai_service_url: "http://localhost:8000".to_string(),
            initial_admin_email: "admin@example.com".to_string(),
            initial_admin_username: "admin".to_string(),
            initial_admin_password: "ChangeMe123!".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn sensitive_json_encrypts_decrypts_and_redacts_nested_values() {
        let config = test_config();
        let input = json!({
            "service_url": "http://ai",
            "api_token": "fastapi-token",
            "providers": [
                {"client_secret": "onedrive-secret"},
                {"public": "visible"}
            ],
            "config_json": {"secret_key": "turnstile-secret"}
        });

        let encrypted = encrypt_sensitive_json(&config, input.clone()).expect("encrypt json");

        assert_ne!(encrypted["api_token"], input["api_token"]);
        assert_ne!(
            encrypted["providers"][0]["client_secret"],
            input["providers"][0]["client_secret"]
        );
        assert_ne!(
            encrypted["config_json"]["secret_key"],
            input["config_json"]["secret_key"]
        );

        let redacted = redact_sensitive_json(encrypted.clone());
        assert_eq!(redacted["api_token"], REDACTED_VALUE);
        assert_eq!(redacted["providers"][0]["client_secret"], REDACTED_VALUE);
        assert_eq!(redacted["config_json"]["secret_key"], REDACTED_VALUE);
        assert_eq!(redacted["providers"][1]["public"], "visible");

        let decrypted = decrypt_sensitive_json(&config, encrypted).expect("decrypt json");
        assert_eq!(decrypted, input);
    }

    #[test]
    fn redacted_sensitive_values_preserve_existing_ciphertext() {
        let config = test_config();
        let existing = encrypt_sensitive_json(
            &config,
            json!({
                "api_token": "old-token",
                "config_json": {"secret_key": "old-secret"},
                "plain": "old"
            }),
        )
        .expect("encrypt existing");
        let incoming = json!({
            "api_token": REDACTED_VALUE,
            "config_json": {"secret_key": REDACTED_VALUE},
            "plain": "new"
        });

        let preserved = preserve_redacted_sensitive_json(incoming, existing);
        let encrypted = encrypt_sensitive_json(&config, preserved).expect("encrypt preserved");
        let decrypted = decrypt_sensitive_json(&config, encrypted).expect("decrypt preserved");

        assert_eq!(decrypted["api_token"], "old-token");
        assert_eq!(decrypted["config_json"]["secret_key"], "old-secret");
        assert_eq!(decrypted["plain"], "new");
    }

    #[test]
    fn blank_sensitive_values_preserve_existing_ciphertext() {
        let config = test_config();
        let existing = encrypt_sensitive_json(
            &config,
            json!({
                "secret_access_key": "old-secret",
                "session_token": "old-session",
                "bucket": "images"
            }),
        )
        .expect("encrypt existing");
        let incoming = json!({
            "secret_access_key": "",
            "session_token": "",
            "bucket": "images-v2"
        });

        let preserved = preserve_redacted_sensitive_json(incoming, existing);
        let encrypted = encrypt_sensitive_json(&config, preserved).expect("encrypt preserved");
        let decrypted = decrypt_sensitive_json(&config, encrypted).expect("decrypt preserved");

        assert_eq!(decrypted["secret_access_key"], "old-secret");
        assert_eq!(decrypted["session_token"], "old-session");
        assert_eq!(decrypted["bucket"], "images-v2");
    }

    #[test]
    fn plain_strings_with_dots_are_not_treated_as_encrypted_values() {
        let config = test_config();
        let value = decrypt_value(&config, "plain.value").expect("plain dotted value");

        assert_eq!(value, "plain.value");
    }

    #[test]
    fn smtp_auth_decrypts_configured_password() {
        let config = test_config();
        let encrypted = encrypt_value(&config, "smtp-secret").expect("encrypt smtp password");
        let row = SmtpRow {
            host: "smtp.example.com".to_string(),
            port: 587,
            username: "mailer@example.com".to_string(),
            password_encrypted: encrypted,
            from_email: "noreply@example.com".to_string(),
            from_name: "潮汐图床".to_string(),
            enabled: true,
        };

        let auth = smtp_auth(&config, &row)
            .expect("smtp auth")
            .expect("auth configured");

        assert_eq!(auth.username, "mailer@example.com");
        assert_eq!(auth.password, "smtp-secret");
    }

    #[test]
    fn smtp_auth_is_optional_without_username() {
        let config = test_config();
        let row = SmtpRow {
            host: "smtp.example.com".to_string(),
            port: 587,
            username: String::new(),
            password_encrypted: String::new(),
            from_email: "noreply@example.com".to_string(),
            from_name: "潮汐图床".to_string(),
            enabled: true,
        };

        assert!(smtp_auth(&config, &row).expect("smtp auth").is_none());
    }
}
