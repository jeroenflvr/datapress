//! `datapress-core` — shared types used by every backend.
//!
//! Backend-agnostic pieces: configuration parsing, error types, request /
//! response models, schema description, and admin-token auth.

pub mod admin;
pub mod backend;
pub mod banner;
pub mod config;
pub mod errors;
pub mod handlers;
pub mod models;
pub mod schema;
pub mod server;
pub mod timeout;
