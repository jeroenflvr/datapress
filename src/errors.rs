use actix_web::{http::StatusCode, HttpResponse, ResponseError};

#[derive(Debug)]
pub enum AppError {
    UnknownColumn(String),
    UnknownOperator(String),
    InvalidValue(String),
    Internal(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::UnknownColumn(c)   => write!(f, "unknown column: {c}"),
            AppError::UnknownOperator(o) => write!(f, "unknown operator: {o}"),
            AppError::InvalidValue(v)    => write!(f, "invalid value: {v}"),
            AppError::Internal(s)        => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<arrow::error::ArrowError> for AppError {
    fn from(e: arrow::error::ArrowError) -> Self { AppError::Internal(e.to_string()) }
}

impl From<parquet::errors::ParquetError> for AppError {
    fn from(e: parquet::errors::ParquetError) -> Self { AppError::Internal(e.to_string()) }
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _                     => StatusCode::BAD_REQUEST,
        }
    }

    fn error_response(&self) -> HttpResponse {
        if matches!(self, AppError::Internal(_)) { log::error!("{self}"); }
        HttpResponse::build(self.status_code())
            .json(serde_json::json!({ "error": self.to_string() }))
    }
}
