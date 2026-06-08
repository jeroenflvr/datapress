//! PyO3 bindings exposing [`datapress_client`] to Python.
//!
//! The native module is intentionally thin: requests cross the FFI
//! boundary as JSON strings and responses come back as JSON strings (or
//! raw Arrow IPC bytes). The ergonomic surface — dict in / dict out,
//! optional `pyarrow` decoding — lives in the pure-Python wrapper
//! (`datap_rs_client/__init__.py`), mirroring how `datap_rs` layers
//! Python over the native `datapress` module.

use datapress_client::blocking::Client as BlockingClient;
use datapress_client::{ClientError, Predicate, QueryRequest};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

pyo3::create_exception!(
    _datapress_client,
    DataPressError,
    pyo3::exceptions::PyException,
    "Raised when a DataPress request fails (transport, HTTP status, or decode error)."
);

fn to_py_err(err: ClientError) -> PyErr {
    DataPressError::new_err(err.to_string())
}

fn parse<T: serde::de::DeserializeOwned>(json: &str, what: &str) -> PyResult<T> {
    serde_json::from_str(json)
        .map_err(|e| DataPressError::new_err(format!("invalid {what} JSON: {e}")))
}

fn dump<T: serde::Serialize>(value: &T) -> PyResult<String> {
    serde_json::to_string(value)
        .map_err(|e| DataPressError::new_err(format!("failed to encode response: {e}")))
}

/// Synchronous DataPress client backed by the Rust blocking client.
#[pyclass(module = "datap_rs_client._datapress_client")]
struct Client {
    inner: BlockingClient,
}

#[pymethods]
impl Client {
    #[new]
    #[pyo3(signature = (
        base_url,
        *,
        api_base = None,
        admin_token = None,
        bearer_token = None,
        timeout_secs = None,
    ))]
    fn new(
        base_url: &str,
        api_base: Option<String>,
        admin_token: Option<String>,
        bearer_token: Option<String>,
        timeout_secs: Option<f64>,
    ) -> PyResult<Self> {
        let mut builder = BlockingClient::builder(base_url);
        if let Some(api_base) = api_base {
            builder = builder.api_base(api_base);
        }
        if let Some(token) = admin_token {
            builder = builder.admin_token(token);
        }
        if let Some(token) = bearer_token {
            builder = builder.bearer_token(token);
        }
        if let Some(secs) = timeout_secs {
            builder = builder.timeout(std::time::Duration::from_secs_f64(secs));
        }
        let async_client = builder.build().map_err(to_py_err)?;
        let inner = BlockingClient::from_async(async_client).map_err(to_py_err)?;
        Ok(Self { inner })
    }

    /// Liveness probe — returns the `/healthz` body as a JSON string.
    fn healthz(&self, py: Python<'_>) -> PyResult<String> {
        let v = py.detach(|| self.inner.healthz()).map_err(to_py_err)?;
        dump(&v)
    }

    /// Readiness probe — returns the `/readyz` body as a JSON string.
    fn readyz(&self, py: Python<'_>) -> PyResult<String> {
        let v = py.detach(|| self.inner.readyz()).map_err(to_py_err)?;
        dump(&v)
    }

    /// List registered dataset names.
    fn datasets(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        py.detach(|| self.inner.datasets()).map_err(to_py_err)
    }

    /// Fetch the schema for `dataset` as a JSON string.
    fn schema(&self, py: Python<'_>, dataset: &str) -> PyResult<String> {
        let v = py
            .detach(|| self.inner.schema(dataset))
            .map_err(to_py_err)?;
        dump(&v)
    }

    /// Count matching rows. `predicates_json` is a JSON array of
    /// predicate objects (or `None` for unfiltered).
    #[pyo3(signature = (dataset, predicates_json = None))]
    fn count(&self, py: Python<'_>, dataset: &str, predicates_json: Option<&str>) -> PyResult<u64> {
        let predicates: Vec<Predicate> = match predicates_json {
            Some(s) => parse(s, "predicates")?,
            None => Vec::new(),
        };
        py.detach(|| self.inner.count(dataset, &predicates))
            .map_err(to_py_err)
    }

    /// Run a structured query. `request_json` is a serialized
    /// `QueryRequest`; returns the JSON response envelope as a string.
    fn query_json(&self, py: Python<'_>, dataset: &str, request_json: &str) -> PyResult<String> {
        let request: QueryRequest = parse(request_json, "query request")?;
        let resp = py
            .detach(|| self.inner.query_json(dataset, &request))
            .map_err(to_py_err)?;
        dump(&resp)
    }

    /// Run a structured query and return the raw Arrow IPC stream bytes.
    fn query_arrow<'py>(
        &self,
        py: Python<'py>,
        dataset: &str,
        request_json: &str,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let request: QueryRequest = parse(request_json, "query request")?;
        let bytes = py
            .detach(|| self.inner.query_arrow_bytes(dataset, &request))
            .map_err(to_py_err)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Run a read-only SQL statement; returns the JSON envelope as a
    /// string.
    #[pyo3(signature = (sql, max_rows = None))]
    fn sql(&self, py: Python<'_>, sql: &str, max_rows: Option<u64>) -> PyResult<String> {
        let resp = py
            .detach(|| self.inner.sql(sql, max_rows))
            .map_err(to_py_err)?;
        dump(&resp)
    }

    /// Trigger an in-place reload of `dataset`; returns the JSON body.
    fn reload(&self, py: Python<'_>, dataset: &str) -> PyResult<String> {
        let v = py
            .detach(|| self.inner.reload(dataset))
            .map_err(to_py_err)?;
        dump(&v)
    }
}

#[pymodule]
fn _datapress_client(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Client>()?;
    m.add("DataPressError", m.py().get_type::<DataPressError>())?;
    Ok(())
}
