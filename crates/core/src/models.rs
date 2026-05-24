use serde::Deserialize;
use serde_json::Value as JsonValue;

#[derive(Deserialize)]
pub struct Predicate {
    pub col: String,
    /// eq | neq | gt | gte | lt | lte | like | ilike | in | is_null | is_not_null
    pub op:  String,
    pub val: Option<JsonValue>,
}

#[derive(Deserialize)]
pub struct QueryRequest {
    /// Columns to return. Empty = all columns.
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub predicates: Vec<Predicate>,
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
}

fn default_page() -> u64 { 1 }
fn default_page_size() -> u64 { 100 }

/// Body for `POST /api/datasets/{name}/count`. Predicates are optional —
/// an empty body (or `{}`) counts every row in the dataset.
#[derive(Deserialize, Default)]
pub struct CountRequest {
    #[serde(default)]
    pub predicates: Vec<Predicate>,
}
