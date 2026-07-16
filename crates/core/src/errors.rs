use actix_web::{HttpResponse, ResponseError, http::StatusCode};

#[derive(Debug)]
pub enum AppError {
    UnknownColumn(String),
    UnknownOperator(String),
    InvalidValue(String),
    NotFound(String),
    Unauthorized(String),
    Forbidden(String),
    Unavailable(String),
    /// HTTP 409 Conflict — e.g. deleting a dataset that has dependents.
    Conflict(String),
    /// Dataset is registered but not yet ready to serve queries (pending or
    /// building). The HTTP response carries `Retry-After: 2` to guide clients.
    NotReady {
        dataset: String,
        state: String,
    },
    /// A dataset's source resolved to no data (no matching files / no rows).
    /// Used at startup to log-and-skip the dataset rather than aborting.
    EmptyDataset(String),
    Internal(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::UnknownColumn(c) => write!(f, "unknown column: {c}"),
            AppError::UnknownOperator(o) => write!(f, "unknown operator: {o}"),
            AppError::InvalidValue(v) => write!(f, "invalid value: {v}"),
            AppError::NotFound(n) => write!(f, "not found: {n}"),
            AppError::Unauthorized(m) => write!(f, "unauthorized: {m}"),
            AppError::Forbidden(m) => write!(f, "forbidden: {m}"),
            AppError::Unavailable(m) => write!(f, "service unavailable: {m}"),
            AppError::Conflict(m) => write!(f, "conflict: {m}"),
            AppError::NotReady { dataset, state } => {
                write!(f, "dataset '{dataset}' is not ready (state: {state})")
            }
            AppError::EmptyDataset(m) => write!(f, "empty dataset: {m}"),
            AppError::Internal(s) => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for AppError {}

// ---------------------------------------------------------------------------
// Backend-specific error conversions (cfg-gated so each binary only pulls in
// what it needs).
// ---------------------------------------------------------------------------

#[cfg(feature = "duckdb")]
impl From<duckdb::Error> for AppError {
    fn from(e: duckdb::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

#[cfg(feature = "datafusion")]
impl From<arrow::error::ArrowError> for AppError {
    fn from(e: arrow::error::ArrowError) -> Self {
        AppError::Internal(e.to_string())
    }
}

#[cfg(feature = "datafusion")]
impl From<parquet::errors::ParquetError> for AppError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        AppError::Internal(e.to_string())
    }
}

#[cfg(feature = "datafusion")]
impl From<datafusion::error::DataFusionError> for AppError {
    fn from(e: datafusion::error::DataFusionError) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Unavailable(_) | AppError::NotReady { .. } => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    fn error_response(&self) -> HttpResponse {
        if matches!(self, AppError::Internal(_)) {
            log::error!("{self}");
        }
        let mut resp = HttpResponse::build(self.status_code())
            .json(serde_json::json!({ "error": self.to_string() }));
        if let AppError::NotReady { .. } = self {
            resp.headers_mut().insert(
                actix_web::http::header::HeaderName::from_static("retry-after"),
                actix_web::http::header::HeaderValue::from_static("2"),
            );
        }
        resp
    }
}
