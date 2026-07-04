use reqwest::Client;
use sqlx::PgPool;
use std::env;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<AppConfig>,
    pub http: Client,
}

impl AppState {
    pub fn new(pool: PgPool, config: AppConfig) -> Self {
        Self {
            pool,
            config: Arc::new(config),
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub public_base_url: String,
    pub session_secret: String,
    pub encryption_key: String,
    pub local_storage_root: String,
    pub local_storage_public_prefix: String,
    pub ai_service_url: String,
    pub initial_admin_email: String,
    pub initial_admin_username: String,
    pub initial_admin_password: String,
    pub github_oauth_client_id: String,
    pub github_oauth_client_secret: String,
    pub github_oauth_authorize_url: String,
    pub github_oauth_token_url: String,
    pub github_oauth_user_url: String,
    pub github_oauth_emails_url: String,
    pub linuxdo_oauth_client_id: String,
    pub linuxdo_oauth_client_secret: String,
    pub linuxdo_oauth_authorize_url: String,
    pub linuxdo_oauth_token_url: String,
    pub linuxdo_oauth_user_url: String,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let port = read_env("APP_PORT", "8080").parse()?;
        Ok(Self {
            host: read_env("APP_HOST", "0.0.0.0"),
            port,
            database_url: required_env("DATABASE_URL")?,
            public_base_url: read_env("PUBLIC_BASE_URL", ""),
            session_secret: read_env("SESSION_SECRET", "development-session-secret-change-me"),
            encryption_key: read_env(
                "CONFIG_ENCRYPTION_KEY",
                "development-config-secret-change-me",
            ),
            local_storage_root: read_env("LOCAL_STORAGE_ROOT", "/data/storage"),
            local_storage_public_prefix: read_env("LOCAL_STORAGE_PUBLIC_PREFIX", "/files"),
            ai_service_url: env::var("AI_SERVICE_URL")
                .unwrap_or_else(|_| format!("http://127.0.0.1:{port}")),
            initial_admin_email: read_env("INITIAL_ADMIN_EMAIL", "admin@example.com"),
            initial_admin_username: read_env("INITIAL_ADMIN_USERNAME", "admin"),
            initial_admin_password: read_env("INITIAL_ADMIN_PASSWORD", "ChangeMe123!"),
            github_oauth_client_id: read_env("GITHUB_OAUTH_CLIENT_ID", ""),
            github_oauth_client_secret: read_env("GITHUB_OAUTH_CLIENT_SECRET", ""),
            github_oauth_authorize_url: read_env(
                "GITHUB_OAUTH_AUTHORIZE_URL",
                "https://github.com/login/oauth/authorize",
            ),
            github_oauth_token_url: read_env(
                "GITHUB_OAUTH_TOKEN_URL",
                "https://github.com/login/oauth/access_token",
            ),
            github_oauth_user_url: read_env("GITHUB_OAUTH_USER_URL", "https://api.github.com/user"),
            github_oauth_emails_url: read_env(
                "GITHUB_OAUTH_EMAILS_URL",
                "https://api.github.com/user/emails",
            ),
            linuxdo_oauth_client_id: read_env("LINUXDO_OAUTH_CLIENT_ID", ""),
            linuxdo_oauth_client_secret: read_env("LINUXDO_OAUTH_CLIENT_SECRET", ""),
            linuxdo_oauth_authorize_url: read_env(
                "LINUXDO_OAUTH_AUTHORIZE_URL",
                "https://connect.linux.do/oauth2/authorize",
            ),
            linuxdo_oauth_token_url: read_env(
                "LINUXDO_OAUTH_TOKEN_URL",
                "https://connect.linux.do/oauth2/token",
            ),
            linuxdo_oauth_user_url: read_env(
                "LINUXDO_OAUTH_USER_URL",
                "https://connect.linux.do/api/user",
            ),
        })
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            database_url: "postgres://example".to_string(),
            public_base_url: "http://localhost:8080".to_string(),
            session_secret: "development-session-secret-change-me".to_string(),
            encryption_key: "development-config-secret-change-me".to_string(),
            local_storage_root: "/tmp/tide".to_string(),
            local_storage_public_prefix: "/files".to_string(),
            ai_service_url: "http://127.0.0.1:8080".to_string(),
            initial_admin_email: "admin@example.com".to_string(),
            initial_admin_username: "admin".to_string(),
            initial_admin_password: "ChangeMe123!".to_string(),
            github_oauth_client_id: String::new(),
            github_oauth_client_secret: String::new(),
            github_oauth_authorize_url: "https://github.com/login/oauth/authorize".to_string(),
            github_oauth_token_url: "https://github.com/login/oauth/access_token".to_string(),
            github_oauth_user_url: "https://api.github.com/user".to_string(),
            github_oauth_emails_url: "https://api.github.com/user/emails".to_string(),
            linuxdo_oauth_client_id: String::new(),
            linuxdo_oauth_client_secret: String::new(),
            linuxdo_oauth_authorize_url: "https://connect.linux.do/oauth2/authorize".to_string(),
            linuxdo_oauth_token_url: "https://connect.linux.do/oauth2/token".to_string(),
            linuxdo_oauth_user_url: "https://connect.linux.do/api/user".to_string(),
        }
    }
}

fn read_env(key: &str, fallback: &str) -> String {
    env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn required_env(key: &str) -> anyhow::Result<String> {
    env::var(key).map_err(|_| anyhow::anyhow!("{key} is required"))
}
