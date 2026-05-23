//! Backend-agnostic schema model for a registered dataset.
//!
//! Both backends introspect their parquet source at startup and produce a
//! [`DatasetSchema`]. Predicate validation, identifier quoting, and the
//! `GET /api/datasets/{name}/schema` response all go through this type.

use std::collections::HashMap;

use serde::Serialize;

use crate::errors::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogicalType {
    Bool,
    Int,
    Float,
    Utf8,
    /// Timestamp / Date / Time — serialised as string, requires CAST when
    /// projected from DuckDB.
    Temporal,
    /// Anything else (Decimal, Binary, List, Struct …) — round-tripped as
    /// best the encoder can manage; predicates over these are rejected.
    Other,
}

impl LogicalType {
    /// True iff the type must be cast to VARCHAR when projected through
    /// DuckDB's `json_object()` call.
    pub fn needs_cast(self) -> bool { matches!(self, LogicalType::Temporal) }
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub name:     String,
    pub logical:  LogicalType,
    /// Original backend-specific type name (e.g. "TIMESTAMP", "VARCHAR",
    /// "Float64") — included in the schema response for clients.
    pub sql_type: String,
    pub nullable: bool,
}

#[derive(Debug, Clone)]
pub struct DatasetSchema {
    pub name:    String,
    pub columns: Vec<ColumnInfo>,
    /// lowercase name → index in `columns`.
    pub by_name: HashMap<String, usize>,
}

impl DatasetSchema {
    pub fn new(name: impl Into<String>, columns: Vec<ColumnInfo>) -> Self {
        let by_name = columns.iter().enumerate()
            .map(|(i, c)| (c.name.to_lowercase(), i))
            .collect();
        Self { name: name.into(), columns, by_name }
    }

    /// Case-insensitive lookup. Returns the canonical `ColumnInfo`.
    pub fn find(&self, name: &str) -> Result<&ColumnInfo, AppError> {
        self.by_name
            .get(&name.to_lowercase())
            .map(|&i| &self.columns[i])
            .ok_or_else(|| AppError::UnknownColumn(name.into()))
    }

    /// Quoted identifier safe for WHERE / SELECT clauses.
    /// Double-quotes embedded `"` per SQL spec.
    pub fn quote_ident(name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}
