//! Asynchronous DataPress client.

use serde_json::Value as JsonValue;

use crate::error::{ClientError, Result};
use crate::models::{
    CreateQueryRequest, DatasetStatusEntry, QueryRequest, QueryResponse, ReloadAllResponse,
    SavedQueryEntry, SqlRequest, SqlResponse,
};

const ARROW_IPC_MIME: &str = "application/vnd.apache.arrow.stream";

/// Builder for [`Client`].
#[derive(Debug)]
pub struct ClientBuilder {
    base_url: String,
    api_base: String,
    admin_token: Option<String>,
    bearer_token: Option<String>,
    inner: reqwest::ClientBuilder,
}

impl ClientBuilder {
    /// Start building a client for the given server base URL, e.g.
    /// `http://127.0.0.1:8000`. A configured server prefix (e.g.
    /// `/datapress`) should be included here.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_base: "/api/v1".into(),
            admin_token: None,
            bearer_token: None,
            inner: reqwest::Client::builder(),
        }
    }

    /// Override the versioned API mount path. Defaults to `/api/v1`.
    pub fn api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into();
        self
    }

    /// Set the admin token sent as `X-Admin-Token` on mutating endpoints
    /// (currently [`Client::reload`]).
    pub fn admin_token(mut self, token: impl Into<String>) -> Self {
        self.admin_token = Some(token.into());
        self
    }

    /// Set an OAuth2 bearer token, attached as `Authorization: Bearer …`
    /// to every request (for servers with `auth` enabled).
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    /// Set the per-request timeout.
    pub fn timeout(mut self, dur: std::time::Duration) -> Self {
        self.inner = self.inner.timeout(dur);
        self
    }

    /// Accept invalid or untrusted TLS certificates.
    ///
    /// **Dangerous**: disables certificate verification, leaving the
    /// connection open to man-in-the-middle attacks. Intended only for
    /// development against servers using self-signed certificates.
    pub fn danger_accept_invalid_certs(mut self, accept: bool) -> Self {
        self.inner = self.inner.danger_accept_invalid_certs(accept);
        self
    }

    /// Provide a pre-configured [`reqwest::ClientBuilder`] to customise
    /// the underlying HTTP client (proxies, TLS, pools, …).
    pub fn reqwest_builder(mut self, b: reqwest::ClientBuilder) -> Self {
        self.inner = b;
        self
    }

    /// Finish building.
    pub fn build(self) -> Result<Client> {
        let base_url = self.base_url.trim_end_matches('/').to_string();
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err(ClientError::InvalidBaseUrl(self.base_url));
        }
        let http = self.inner.build()?;
        Ok(Client {
            http,
            base_url,
            api_base: self.api_base.trim_end_matches('/').to_string(),
            admin_token: self.admin_token,
            bearer_token: self.bearer_token,
        })
    }
}

/// Asynchronous client for a running DataPress server.
///
/// Cheap to clone (wraps an `Arc` internally via [`reqwest::Client`]);
/// share one instance across tasks.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_base: String,
    admin_token: Option<String>,
    bearer_token: Option<String>,
}

impl Client {
    /// Construct a client with defaults for `base_url`.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        ClientBuilder::new(base_url).build()
    }

    /// Start a [`ClientBuilder`].
    pub fn builder(base_url: impl Into<String>) -> ClientBuilder {
        ClientBuilder::new(base_url)
    }

    // ----------------------------------------------------------- urls --

    fn api_url(&self, path: &str) -> String {
        format!("{}{}{}", self.base_url, self.api_base, path)
    }

    fn apply_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req;
        if let Some(t) = &self.admin_token {
            req = req.header("X-Admin-Token", t);
        }
        if let Some(t) = &self.bearer_token {
            req = req.bearer_auth(t);
        }
        req
    }

    // ------------------------------------------------------- requests --

    async fn get_json(&self, url: String) -> Result<JsonValue> {
        let req = self.apply_headers(self.http.get(&url).header("Accept", "application/json"));
        Self::json_response(req.send().await?).await
    }

    async fn post_json<B: serde::Serialize>(&self, url: String, body: &B) -> Result<JsonValue> {
        let req = self
            .apply_headers(self.http.post(&url).header("Accept", "application/json"))
            .json(body);
        Self::json_response(req.send().await?).await
    }

    async fn json_response(resp: reqwest::Response) -> Result<JsonValue> {
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(ClientError::from_response(status.as_u16(), body));
        }
        if body.is_empty() {
            return Ok(JsonValue::Null);
        }
        serde_json::from_str(&body).map_err(|e| ClientError::Decode(e.to_string()))
    }

    // --------------------------------------------------------- probes --

    /// Liveness probe — `GET {base_url}/healthz`.
    /// The `base_url` you supply must already include any configured
    /// `server.prefix` (e.g. `http://host:8000/dp`).
    pub async fn healthz(&self) -> Result<JsonValue> {
        self.get_json(format!("{}/healthz", self.base_url)).await
    }

    /// Readiness probe — `GET {base_url}/readyz`. Returns a `503` error
    /// while the server is still loading datasets.
    /// The `base_url` must include any configured `server.prefix`.
    pub async fn readyz(&self) -> Result<JsonValue> {
        self.get_json(format!("{}/readyz", self.base_url)).await
    }

    // ------------------------------------------------------- metadata --

    /// List registered dataset names.
    pub async fn datasets(&self) -> Result<Vec<String>> {
        let v = self.get_json(self.api_url("/datasets")).await?;
        // Newer servers return `{"datasets": [ {name, …}, … ]}`; tolerate a
        // bare array and a list of strings too.
        let arr = match &v {
            JsonValue::Object(map) => map.get("datasets").cloned().unwrap_or(JsonValue::Null),
            other => other.clone(),
        };
        let names = match arr {
            JsonValue::Array(items) => items
                .into_iter()
                .filter_map(|it| match it {
                    JsonValue::String(s) => Some(s),
                    JsonValue::Object(o) => {
                        o.get("name").and_then(|n| n.as_str()).map(str::to_owned)
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(names)
    }

    /// Fetch the schema description for `dataset`.
    pub async fn schema(&self, dataset: &str) -> Result<JsonValue> {
        self.get_json(self.api_url(&format!("/datasets/{dataset}/schema")))
            .await
    }

    /// Count matching rows. `predicates` is the same predicate shape used
    /// by [`QueryRequest`]; `None`/empty = unfiltered.
    pub async fn count(
        &self,
        dataset: &str,
        predicates: &[crate::models::Predicate],
    ) -> Result<u64> {
        let body = serde_json::json!({ "predicates": predicates });
        let out = self
            .post_json(self.api_url(&format!("/datasets/{dataset}/count")), &body)
            .await?;
        out.get("count")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| ClientError::Decode("count: missing `count` field".into()))
    }

    // -------------------------------------------------------- queries --

    /// Run a structured query and return the decoded JSON envelope.
    pub async fn query_json(&self, dataset: &str, request: &QueryRequest) -> Result<QueryResponse> {
        let v = self
            .post_json(self.api_url(&format!("/datasets/{dataset}/query")), request)
            .await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// Run a raw read-only SQL statement (`POST /sql`). The endpoint must
    /// be enabled server-side (`[sql].enabled = true`), else a `404` is
    /// returned.
    pub async fn sql(&self, sql: impl Into<String>, max_rows: Option<u64>) -> Result<SqlResponse> {
        let body = SqlRequest {
            sql: sql.into(),
            max_rows,
        };
        let v = self.post_json(self.api_url("/sql"), &body).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// Trigger an in-place reload of `dataset` (requires `admin_token` or
    /// the configured reload scopes).
    pub async fn reload(&self, dataset: &str) -> Result<JsonValue> {
        self.post_json(
            self.api_url(&format!("/datasets/{dataset}/reload")),
            &serde_json::json!({}),
        )
        .await
    }

    // ------------------------------------------------------- queries API --

    /// Create a runtime dataset (`POST /api/v1/queries`). Requires
    /// `admin_token` or the `datasets:manage` scope.
    pub async fn create_query(&self, request: &CreateQueryRequest) -> Result<SavedQueryEntry> {
        let v = self.post_json(self.api_url("/queries"), request).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// List runtime-created datasets (`GET /api/v1/queries`). Requires
    /// `admin_token` or the `datasets:manage` scope.
    pub async fn list_queries(&self) -> Result<Vec<SavedQueryEntry>> {
        let v = self.get_json(self.api_url("/queries")).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// Delete a runtime dataset (`DELETE /api/v1/queries/{name}`). Requires
    /// `admin_token` or the `datasets:manage` scope. Returns `403` if the
    /// dataset was not created via the API.
    pub async fn delete_query(&self, name: &str) -> Result<JsonValue> {
        let url = self.api_url(&format!("/queries/{name}"));
        let req = self.apply_headers(self.http.delete(&url).header("Accept", "application/json"));
        Self::json_response(req.send().await?).await
    }

    /// Fetch the full status entry for `dataset`
    /// (`GET /api/v1/datasets/{name}/status`).
    pub async fn dataset_status(&self, dataset: &str) -> Result<DatasetStatusEntry> {
        let v = self
            .get_json(self.api_url(&format!("/datasets/{dataset}/status")))
            .await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// Enqueue a reload of every reloadable dataset
    /// (`POST /api/v1/datasets/reload-all`). Requires `admin_token` or
    /// the `datasets:manage` scope. Returns `202` with `enqueued`/`skipped` lists.
    pub async fn reload_all(&self) -> Result<ReloadAllResponse> {
        let v = self
            .post_json(self.api_url("/datasets/reload-all"), &serde_json::json!({}))
            .await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    // ----------------------------------------------------------- arrow --

    /// Run a structured query against the Arrow IPC streaming endpoint
    /// (`POST /datasets/{name}/query/stream`), returning the raw IPC
    /// stream bytes. Use [`Client::query_arrow`] to decode them into
    /// record batches.
    pub async fn query_arrow_bytes(
        &self,
        dataset: &str,
        request: &QueryRequest,
    ) -> Result<bytes::Bytes> {
        let url = self.api_url(&format!("/datasets/{dataset}/query/stream"));
        let req = self
            .apply_headers(self.http.post(&url).header("Accept", ARROW_IPC_MIME))
            .json(request);
        let resp = req.send().await?;
        let status = resp.status();
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = resp.bytes().await?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&body).into_owned();
            return Err(ClientError::from_response(status.as_u16(), text));
        }
        if !ctype.contains("arrow") {
            return Err(ClientError::UnexpectedContentType(ctype));
        }
        Ok(body)
    }

    /// Run a structured query and decode the Arrow IPC response into a
    /// vector of [`arrow::record_batch::RecordBatch`].
    #[cfg(feature = "arrow")]
    pub async fn query_arrow(
        &self,
        dataset: &str,
        request: &QueryRequest,
    ) -> Result<Vec<arrow::record_batch::RecordBatch>> {
        let bytes = self.query_arrow_bytes(dataset, request).await?;
        decode_ipc_stream(&bytes)
    }
}

/// Decode an Arrow IPC stream into its record batches.
#[cfg(feature = "arrow")]
pub fn decode_ipc_stream(bytes: &[u8]) -> Result<Vec<arrow::record_batch::RecordBatch>> {
    use arrow::ipc::reader::StreamReader;
    let reader = StreamReader::try_new(std::io::Cursor::new(bytes), None)?;
    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch?);
    }
    Ok(batches)
}
