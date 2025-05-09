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
    #[error("Database error: {0}")]
    DbError(String),

    #[error("Failed to connect to database: {0}")]
    DbConnectionError(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error("Task join error: {0}")]
    TaskJoinError(String),

    #[error("HTTP request error: {0}")]
    RequestError(String),

    #[error("Invalid IP address: {0}")]
    InvalidIpError(String),

    #[error("JSON parsing error: {0}")]
    JsonError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Resource not found: {0}")]
    NotFoundError(String),

    #[error("Invalid input: {0}")]
    ValidationError(String),
}

// Utility methods for error conversion
impl AppError {
    pub fn from_reqwest_error(err: reqwest::Error) -> Self {
        AppError::RequestError(err.to_string())
    }

    pub fn from_sqlx_error(err: sqlx::Error) -> Self {
        AppError::DbError(err.to_string())
    }

    pub fn from_serde_error(err: serde_json::Error) -> Self {
        AppError::JsonError(err.to_string())
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

// Implement axum's IntoResponse for HTTP error responses
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AppError::DbError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::DbConnectionError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::ServerError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::TaskJoinError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::RequestError(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::InvalidIpError(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::JsonError(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::ConfigError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::NotFoundError(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::ValidationError(_) => (StatusCode::BAD_REQUEST, self.to_string()),
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
