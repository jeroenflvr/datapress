//! `datapress-core` — shared types used by every backend.
//!
//! Backend-agnostic pieces: configuration parsing, error types, request /
//! response models, schema description, and admin-token auth.

pub mod admin;
#[cfg(feature = "auth")]
pub mod auth;
pub mod backend;
pub mod banner;
pub mod config;
#[cfg(feature = "docs")]
pub mod docs;
// Shared gzip-compressed DuckDB-WASM vendor store, embedded once and reused by
// the explorer and the self-hosted docs site (avoids duplicating ~77 MB of
// wasm in the binary / PyPI wheel).
#[cfg(any(feature = "docs", feature = "explorer"))]
pub(crate) mod duckdb_vendor;
pub mod errors;
#[cfg(feature = "explorer")]
pub mod explorer;
pub mod handlers;
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod models;
// Shared OIDC discovery for the Swagger UI + explorer OAuth2 login flows.
#[cfg(any(feature = "swagger", feature = "explorer"))]
pub mod oauth2;
pub mod refresh;
pub mod schema;
pub mod server;
pub mod sql;
pub mod storage;
#[cfg(feature = "swagger")]
pub mod swagger;
pub mod timeout;
