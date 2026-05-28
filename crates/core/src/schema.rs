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

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> DatasetSchema {
        DatasetSchema::new("ds", vec![
            ColumnInfo { name: "Id".into(),    logical: LogicalType::Int,      sql_type: "BIGINT".into(),    nullable: false },
            ColumnInfo { name: "When".into(),  logical: LogicalType::Temporal, sql_type: "TIMESTAMP".into(), nullable: true  },
        ])
    }

    #[test]
    fn quote_ident_plain() {
        assert_eq!(DatasetSchema::quote_ident("foo"), "\"foo\"");
    }

    #[test]
    fn quote_ident_escapes_inner_quote() {
        assert_eq!(DatasetSchema::quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn find_case_insensitive_returns_canonical_name() {
        let sch = s();
        let c = sch.find("ID").expect("found");
        assert_eq!(c.name, "Id");
    }

    #[test]
    fn find_unknown_column() {
        let sch = s();
        let err = sch.find("nope").unwrap_err();
        assert!(matches!(err, AppError::UnknownColumn(_)));
    }

    #[test]
    fn needs_cast_only_temporal() {
        assert!(LogicalType::Temporal.needs_cast());
        for t in [LogicalType::Bool, LogicalType::Int, LogicalType::Float, LogicalType::Utf8, LogicalType::Other] {
            assert!(!t.needs_cast());
        }
    }
}
