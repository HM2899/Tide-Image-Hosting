use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tide_shared::{ApiErrorBody, ApiErrorResponse};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Quota(String),
    #[error("{0}")]
    FileTooLarge(String),
    #[error("{0}")]
    FileType(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    External(String),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::BadRequest(_) => "BAD_REQUEST",
            AppError::Quota(_) => "QUOTA_EXCEEDED",
            AppError::FileTooLarge(_) => "FILE_TOO_LARGE",
            AppError::FileType(_) => "FILE_TYPE_NOT_ALLOWED",
            AppError::Unauthorized(_) => "UNAUTHORIZED",
            AppError::Forbidden(_) => "FORBIDDEN",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::Conflict(_) => "CONFLICT",
            AppError::External(_) => "EXTERNAL_SERVICE_ERROR",
            AppError::Sqlx(_) => "DATABASE_ERROR",
            AppError::Io(_) => "IO_ERROR",
            AppError::Http(_) => "HTTP_ERROR",
            AppError::Anyhow(_) => "INTERNAL_ERROR",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Quota(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::FileTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::FileType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::External(_) => StatusCode::BAD_GATEWAY,
            AppError::Sqlx(_) | AppError::Io(_) | AppError::Http(_) | AppError::Anyhow(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = ApiErrorResponse {
            success: false,
            error: ApiErrorBody {
                code: self.code().to_string(),
                message: self.to_string(),
            },
        };
        (status, Json(body)).into_response()
    }
}

pub fn empty_ok() -> Json<tide_shared::ApiResponse<serde_json::Value>> {
    Json(tide_shared::ok(json!({})))
}
