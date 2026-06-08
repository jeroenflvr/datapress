//! # datapress-client
//!
//! Async + blocking Rust client for a running [DataPress] dataset server.
//!
//! It wraps the JSON and Arrow IPC HTTP endpoints so you don't have to
//! hand-roll request bodies and response decoding. The crate is
//! deliberately lightweight: only `reqwest` + `serde` by default, with
//! Arrow IPC decoding behind the (default-on) `arrow` feature and a
//! synchronous client behind the `blocking` feature.
//!
//! ## Async
//!
//! ```no_run
//! # async fn run() -> datapress_client::Result<()> {
//! use datapress_client::{Client, QueryRequest, Predicate};
//!
//! let client = Client::new("http://127.0.0.1:8000")?;
//! let names = client.datasets().await?;
//!
//! let req = QueryRequest::builder()
//!     .columns(["State", "Severity"])
//!     .predicate(Predicate::new("Severity", "gte", 3))
//!     .page_size(10_000)
//!     .build();
//! let resp = client.query_json("accidents", &req).await?;
//! println!("{} rows", resp.data.len());
//! # Ok(())
//! # }
//! ```
//!
//! ## Blocking (feature `blocking`)
//!
//! ```no_run
//! # #[cfg(feature = "blocking")]
//! # fn run() -> datapress_client::Result<()> {
//! use datapress_client::blocking::Client;
//!
//! let client = Client::new("http://127.0.0.1:8000")?;
//! let count = client.count("accidents", &[])?;
//! println!("{count} rows");
//! # Ok(())
//! # }
//! ```
//!
//! [DataPress]: https://github.com/jeroenflvr/datapress

mod client;
mod error;
mod models;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use client::{Client, ClientBuilder};
pub use error::{ClientError, Result};
pub use models::{
    Aggregation, OrderBy, Predicate, QueryRequest, QueryRequestBuilder, QueryResponse, SqlRequest,
    SqlResponse,
};

#[cfg(feature = "arrow")]
pub use client::decode_ipc_stream;
