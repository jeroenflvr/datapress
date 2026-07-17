//! Request and response types for the structured query API.
//!
//! These mirror the server-side `QueryRequest` shape but are
//! **serialize-first** (the server's copy is deserialize-only) and carry
//! no engine dependencies, so this crate stays lightweight and
//! publishable on its own.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// A single filter predicate.
///
/// `op` is one of `eq | neq | gt | gte | lt | lte | like | ilike | in |
/// is_null | is_not_null`. `val` is omitted for the null checks and is an
/// array for `in`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Predicate {
    pub col: String,
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub val: Option<JsonValue>,
}

impl Predicate {
    /// Binary/`like` predicate: `col op val`.
    pub fn new(col: impl Into<String>, op: impl Into<String>, val: impl Into<JsonValue>) -> Self {
        Self {
            col: col.into(),
            op: op.into(),
            val: Some(val.into()),
        }
    }

    /// A value-less predicate (`is_null` / `is_not_null`).
    pub fn unary(col: impl Into<String>, op: impl Into<String>) -> Self {
        Self {
            col: col.into(),
            op: op.into(),
            val: None,
        }
    }
}

/// One `ORDER BY` entry. `dir` is `"asc"` (default) or `"desc"`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderBy {
    pub col: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

impl OrderBy {
    pub fn asc(col: impl Into<String>) -> Self {
        Self {
            col: col.into(),
            dir: Some("asc".into()),
        }
    }
    pub fn desc(col: impl Into<String>) -> Self {
        Self {
            col: col.into(),
            dir: Some("desc".into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 7 — Saved Queries + Status models
// ---------------------------------------------------------------------------

/// Whether a runtime-created dataset survives a server restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedQueryKind {
    /// Ephemeral — lost on restart.
    Temp,
    /// Persisted to `datasets.d/` and rebuilt on next startup.
    Query,
}

impl std::fmt::Display for SavedQueryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SavedQueryKind::Temp => f.write_str("temp"),
            SavedQueryKind::Query => f.write_str("query"),
        }
    }
}

/// Request body for `POST /api/v1/queries`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateQueryRequest {
    /// Dataset name.
    pub name: String,
    /// Read-only SQL statement to materialise.
    pub sql: String,
    /// `"temp"` (default) or `"query"`.
    #[serde(default = "default_query_kind")]
    pub kind: SavedQueryKind,
    /// Optional refresh schedule (only meaningful for `kind = "query"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<JsonValue>,
    /// TTL for temp datasets (e.g. `"2h"`, `"30m"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    /// Materialization options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialize: Option<JsonValue>,
    /// Index options (DataFusion-only; ignored on DuckDB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<JsonValue>,
}

fn default_query_kind() -> SavedQueryKind {
    SavedQueryKind::Temp
}

impl CreateQueryRequest {
    /// Build a minimal `temp` query request.
    pub fn temp(name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sql: sql.into(),
            kind: SavedQueryKind::Temp,
            refresh: None,
            ttl: None,
            materialize: None,
            index: None,
        }
    }

    /// Build a minimal persisted `query` request.
    pub fn persisted(name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sql: sql.into(),
            kind: SavedQueryKind::Query,
            refresh: None,
            ttl: None,
            materialize: None,
            index: None,
        }
    }
}

/// One entry in `GET /api/v1/queries` and the response for `POST /api/v1/queries`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedQueryEntry {
    pub name: String,
    pub kind: SavedQueryKind,
    /// Inferred `depends_on` list returned by the server.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Current lifecycle state (e.g. `"published"`, `"building"`).
    pub state: String,
}

/// Response for `POST /api/v1/datasets/reload-all`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReloadAllResponse {
    pub enqueued: Vec<String>,
    pub skipped: Vec<String>,
}

/// Full status entry returned by `GET /api/v1/datasets/{name}/status`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatasetStatusEntry {
    pub name: String,
    /// Lifecycle state: `"pending"`, `"building"`, `"published"`, `"failed"`.
    #[serde(rename = "state")]
    pub state: String,
    /// Source kind: `"parquet"`, `"delta"`, `"query"`.
    pub kind: String,
    /// Effective residency: `"memory"` or `"lazy"`.
    pub residency: String,
    /// Storage generation size in bytes (null for memory-resident).
    #[serde(default)]
    pub storage_bytes: Option<u64>,
    /// Generation identifier (ULID, null for memory-resident).
    #[serde(default)]
    pub generation_id: Option<String>,
    /// RFC-3339 timestamp of last successful publish.
    #[serde(default)]
    pub last_refresh_at: Option<String>,
    /// Build duration in ms for the last successful publish.
    #[serde(default)]
    pub last_refresh_duration_ms: Option<u128>,
    /// RFC-3339 of the next scheduled fire.
    #[serde(default)]
    pub next_refresh_at: Option<String>,
    /// What triggered the last successful publish.
    #[serde(default)]
    pub refresh_source: Option<String>,
    /// Consecutive scheduler failures since last success.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Last error message (truncated to 500 chars).
    #[serde(default)]
    pub last_error: Option<String>,
    /// Number of columns (0 when not yet published).
    pub columns: usize,
    /// Number of rows (0 when not yet published).
    pub rows: usize,
    /// Whether current generation is lazy.
    pub lazy: bool,
    /// Upstream dataset names this dataset depends on.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// One aggregation in a `group_by` query.
///
/// `op` is `count | sum | avg | min | max`. `col` is required for every op
/// except `count`. `alias` is the output key; defaults server-side to
/// `count` for `COUNT(*)` and `{op}_{col}` otherwise.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Aggregation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<String>,
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

impl Aggregation {
    /// `COUNT(*)` with an optional alias.
    pub fn count(alias: Option<&str>) -> Self {
        Self {
            col: None,
            op: "count".into(),
            alias: alias.map(str::to_owned),
        }
    }

    /// An aggregation over a named column (`sum`, `avg`, `min`, `max`,
    /// or `count`).
    pub fn over(op: impl Into<String>, col: impl Into<String>, alias: Option<&str>) -> Self {
        Self {
            col: Some(col.into()),
            op: op.into(),
            alias: alias.map(str::to_owned),
        }
    }
}

/// A structured query, sent as the body of `POST /datasets/{name}/query`.
///
/// Build one with [`QueryRequest::builder`]. Fields left at their defaults
/// are omitted from the wire payload so the server applies its own
/// defaults (page size, etc.).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct QueryRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<Predicate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregations: Vec<Aggregation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub having: Vec<Predicate>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub distinct: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order_by: Vec<OrderBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u64>,
}

impl QueryRequest {
    /// Start building a query.
    pub fn builder() -> QueryRequestBuilder {
        QueryRequestBuilder::default()
    }
}

/// Fluent builder for [`QueryRequest`].
#[derive(Clone, Debug, Default)]
pub struct QueryRequestBuilder {
    inner: QueryRequest,
}

impl QueryRequestBuilder {
    /// Restrict the projection to these columns. Empty = all columns.
    pub fn columns<I, S>(mut self, cols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.inner.columns = cols.into_iter().map(Into::into).collect();
        self
    }

    /// Add a filter predicate (ANDed with the others).
    pub fn predicate(mut self, p: Predicate) -> Self {
        self.inner.predicates.push(p);
        self
    }

    /// Group by these columns.
    pub fn group_by<I, S>(mut self, cols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.inner.group_by = cols.into_iter().map(Into::into).collect();
        self
    }

    /// Add an aggregation.
    pub fn aggregation(mut self, a: Aggregation) -> Self {
        self.inner.aggregations.push(a);
        self
    }

    /// Add a post-aggregation (`HAVING`) predicate.
    pub fn having(mut self, p: Predicate) -> Self {
        self.inner.having.push(p);
        self
    }

    /// Return only distinct rows over the projected columns.
    pub fn distinct(mut self, yes: bool) -> Self {
        self.inner.distinct = yes;
        self
    }

    /// Add a sort key.
    pub fn order_by(mut self, o: OrderBy) -> Self {
        self.inner.order_by.push(o);
        self
    }

    /// Cap the total number of rows returned.
    pub fn limit(mut self, n: u64) -> Self {
        self.inner.limit = Some(n);
        self
    }

    /// Set the (1-based) page number.
    pub fn page(mut self, n: u64) -> Self {
        self.inner.page = Some(n);
        self
    }

    /// Set the page size.
    pub fn page_size(mut self, n: u64) -> Self {
        self.inner.page_size = Some(n);
        self
    }

    /// Finish building.
    pub fn build(self) -> QueryRequest {
        self.inner
    }
}

/// JSON envelope returned by `POST /datasets/{name}/query`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResponse {
    /// One object per row.
    pub data: Vec<JsonValue>,
    /// Echoed page number.
    #[serde(default)]
    pub page: Option<u64>,
    /// Echoed page size.
    #[serde(default)]
    pub page_size: Option<u64>,
}

/// JSON envelope returned by `POST /sql`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SqlResponse {
    /// One object per row.
    pub data: Vec<JsonValue>,
    /// Effective row cap applied by the server.
    #[serde(default)]
    pub max_rows: Option<u64>,
}

/// Raw-SQL request body (`POST /sql`).
#[derive(Clone, Debug, Serialize)]
pub struct SqlRequest {
    pub sql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,
}
