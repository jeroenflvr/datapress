use actix_web::{http::StatusCode, HttpResponse, ResponseError};

#[derive(Debug)]
pub enum AppError {
    UnknownColumn(String),
    UnknownOperator(String),
    InvalidValue(String),
    Db(duckdb::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::UnknownColumn(c)  => write!(f, "unknown column: {c}"),
            AppError::UnknownOperator(o) => write!(f, "unknown operator: {o}"),
            AppError::InvalidValue(v)   => write!(f, "invalid value: {v}"),
            AppError::Db(e)             => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<duckdb::Error> for AppError {
    fn from(e: duckdb::Error) -> Self {
        AppError::Db(e)
    }
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            AppError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _               => StatusCode::BAD_REQUEST,
        }
    }

    fn error_response(&self) -> HttpResponse {
        if matches!(self, AppError::Db(_)) {
            log::error!("{self}");
        }
        HttpResponse::build(self.status_code())
            .json(serde_json::json!({ "error": self.to_string() }))
    }
}
