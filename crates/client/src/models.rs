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
