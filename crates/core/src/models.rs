use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::errors::AppError;
use crate::schema::DatasetSchema;

#[derive(Clone, Deserialize)]
pub struct Predicate {
    pub col: String,
    /// eq | neq | gt | gte | lt | lte | like | ilike | in | is_null | is_not_null
    pub op:  String,
    pub val: Option<JsonValue>,
}

/// A single `ORDER BY` clause entry.
///
/// `dir` is case-insensitive; accepted values are `"asc"` (default) and
/// `"desc"`. Omitted = ascending.
#[derive(Clone, Deserialize)]
pub struct OrderBy {
    pub col: String,
    #[serde(default)]
    pub dir: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct QueryRequest {
    /// Columns to return. Empty = all columns.
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub predicates: Vec<Predicate>,
    /// Sort spec. Empty = unsorted (engine order).
    #[serde(default)]
    pub order_by: Vec<OrderBy>,
    /// Hard cap on total rows returned across all pages. `None` = no cap
    /// beyond `page_size`.
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
}

impl QueryRequest {
    /// Translate `order_by` into a validated SQL fragment, e.g.
    /// `"\"a\" ASC, \"b\" DESC"`. Returns `Ok(None)` if no ordering was
    /// requested. Unknown columns or directions produce an `AppError`.
    pub fn order_by_sql(&self, schema: &DatasetSchema) -> Result<Option<String>, AppError> {
        if self.order_by.is_empty() {
            return Ok(None);
        }
        let parts: Vec<String> = self.order_by.iter()
            .map(|o| {
                let info = schema.find(&o.col)?;
                let dir  = match o.dir.as_deref().unwrap_or("asc").to_ascii_lowercase().as_str() {
                    "asc"  => "ASC",
                    "desc" => "DESC",
                    other  => return Err(AppError::InvalidValue(format!(
                        "order_by direction must be 'asc' or 'desc' (got '{other}')"
                    ))),
                };
                Ok(format!("{} {dir}", DatasetSchema::quote_ident(&info.name)))
            })
            .collect::<Result<_, _>>()?;
        Ok(Some(parts.join(", ")))
    }

    /// Compute the effective SQL `LIMIT` and `OFFSET` for this request,
    /// honouring both `page`/`page_size` and the optional top-level `limit`
    /// cap. `page_size_cap` is the per-page maximum the backend enforces
    /// (typically 1000).
    ///
    /// Semantics: pagination still drives offset; `limit` caps the total
    /// number of rows ever returned across all pages. Once `offset >=
    /// limit`, the effective LIMIT is `0` (empty page).
    pub fn effective_limit_offset(&self, page_size_cap: u64) -> (u64, u64) {
        let page      = self.page.max(1);
        let page_size = self.page_size.clamp(1, page_size_cap);
        let offset    = (page - 1) * page_size;
        let limit = match self.limit {
            Some(cap) => {
                if offset >= cap { 0 } else { page_size.min(cap - offset) }
            }
            None => page_size,
        };
        (limit, offset)
    }
}

fn default_page() -> u64 { 1 }
fn default_page_size() -> u64 { 100 }

/// Body for `POST /api/datasets/{name}/count`. Predicates are optional —
/// an empty body (or `{}`) counts every row in the dataset.
#[derive(Clone, Deserialize, Default)]
pub struct CountRequest {
    #[serde(default)]
    pub predicates: Vec<Predicate>,
}
