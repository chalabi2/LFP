use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

/// Application error types
#[derive(Error, Debug)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("JSON error: {0}")]
    JsonError(String),

    #[error("Not found: {0}")]
    NotFoundError(String),

    #[error("Request error: {0}")]
    RequestError(String),

    #[error("Cache error: {0}")]
    CacheError(String),

    #[error("Database migration error: {0}")]
    MigrationError(String),

    #[error("Database connection error: {0}")]
    DbConnectionError(String),

    #[error("Database error: {0}")]
    DatabaseError(String),
}

// Utility methods for error conversion
impl AppError {
    pub fn from_reqwest_error(err: reqwest::Error) -> Self {
        AppError::RequestError(err.to_string())
    }

    pub fn from_sqlx_error(err: sqlx::Error) -> Self {
        AppError::DatabaseError(err.to_string())
    }

    pub fn from_serde_error(err: serde_json::Error) -> Self {
        AppError::JsonError(err.to_string())
    }

    pub fn from_redis_error(err: redis::RedisError) -> Self {
        AppError::CacheError(err.to_string())
    }
}

// From trait implementations for common error types
impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        Self::from_reqwest_error(err)
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        Self::from_sqlx_error(err)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        Self::from_serde_error(err)
    }
}

impl From<std::env::VarError> for AppError {
    fn from(err: std::env::VarError) -> Self {
        AppError::ConfigError(format!("Environment variable error: {}", err))
    }
}

impl From<redis::RedisError> for AppError {
    fn from(err: redis::RedisError) -> Self {
        Self::from_redis_error(err)
    }
}

// Implement axum's IntoResponse for HTTP error responses
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AppError::JsonError(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::DbConnectionError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::ConfigError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::NotFoundError(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::RequestError(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::CacheError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::MigrationError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };

        // Create a JSON response with error details
        let body = Json(json!({
            "error": {
                "message": error_message,
                "code": status.as_u16()
            }
        }));

        (status, body).into_response()
    }
}
