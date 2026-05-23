//! Admin endpoint authentication.
//!
//! Reads the expected token from the `ADMIN_TOKEN` environment variable at
//! startup. If the variable is unset or empty, all admin endpoints refuse
//! every request — they are effectively disabled. This is the secure default:
//! you must explicitly opt in by setting `ADMIN_TOKEN` to a non-empty value.
//!
//! Clients authenticate by sending `X-Admin-Token: <value>`. The comparison
//! is constant-time to avoid leaking the token via timing side channels.

use std::sync::OnceLock;

use actix_web::HttpRequest;

use crate::errors::AppError;

static EXPECTED: OnceLock<Option<String>> = OnceLock::new();

fn expected() -> Option<&'static str> {
    EXPECTED
        .get_or_init(|| {
            std::env::var("ADMIN_TOKEN").ok().filter(|s| !s.is_empty())
        })
        .as_deref()
}

/// Verify the request carries a valid admin token.
///
/// Returns `Err(AppError::Forbidden)` when the token is missing, malformed,
/// or does not match. Returns `Err(AppError::Forbidden)` (not 500) when the
/// server has no `ADMIN_TOKEN` configured at all — admin endpoints stay
/// disabled by default.
pub fn require_admin(req: &HttpRequest) -> Result<(), AppError> {
    let expected = expected().ok_or_else(|| {
        AppError::Forbidden(
            "admin endpoints are disabled (set ADMIN_TOKEN env var to enable)".into(),
        )
    })?;

    let presented = req
        .headers()
        .get("X-Admin-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(AppError::Forbidden("invalid or missing X-Admin-Token".into()))
    }
}

/// Constant-time byte comparison. Returns false immediately when lengths
/// differ (length itself isn't secret); otherwise XORs every byte so the
/// runtime doesn't depend on where the first difference occurs.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
