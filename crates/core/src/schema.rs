//! Backend-agnostic schema model for a registered dataset.
//!
//! Both backends introspect their parquet source at startup and produce a
//! [`DatasetSchema`]. Predicate validation, identifier quoting, and the
//! `GET /api/datasets/{name}/schema` response all go through this type.

use std::collections::HashMap;

use serde::Serialize;

use crate::config::ColumnFilter;
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
    pub fn needs_cast(self) -> bool {
        matches!(self, LogicalType::Temporal)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub logical: LogicalType,
    /// Original backend-specific type name (e.g. "TIMESTAMP", "VARCHAR",
    /// "Float64") — included in the schema response for clients.
    pub sql_type: String,
    pub nullable: bool,
}

#[derive(Debug, Clone)]
pub struct DatasetSchema {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    /// lowercase name → index in `columns`.
    pub by_name: HashMap<String, usize>,
    /// Columns a caller may filter on. Empty = no restriction.
    pub predicate_filter: ColumnFilter,
    /// Columns a caller may see / return. Columns denied here are hidden as
    /// if they did not exist. Empty = no restriction.
    pub projection_filter: ColumnFilter,
}

impl DatasetSchema {
    pub fn new(name: impl Into<String>, columns: Vec<ColumnInfo>) -> Self {
        let by_name = columns
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.to_lowercase(), i))
            .collect();
        Self {
            name: name.into(),
            columns,
            by_name,
            predicate_filter: ColumnFilter::default(),
            projection_filter: ColumnFilter::default(),
        }
    }

    /// Attach column-level access filters, validating that every name they
    /// list actually exists in the schema. A typo in a denylist would
    /// otherwise silently expose a column, so unknown names are a hard
    /// error at registration time.
    pub fn with_filters(
        mut self,
        predicate_filter: ColumnFilter,
        projection_filter: ColumnFilter,
    ) -> Result<Self, AppError> {
        for (ctx, filter) in [
            ("predicate_filter", &predicate_filter),
            ("projection_filter", &projection_filter),
        ] {
            filter.validate(&self.name, ctx)?;
            for col in filter.listed() {
                if !self.by_name.contains_key(&col.to_lowercase()) {
                    return Err(AppError::InvalidValue(format!(
                        "dataset '{}': {ctx} references unknown column '{col}'",
                        self.name
                    )));
                }
            }
        }
        self.predicate_filter = predicate_filter;
        self.projection_filter = projection_filter;
        Ok(self)
    }

    /// Whether either access filter restricts anything.
    pub fn has_column_filters(&self) -> bool {
        self.predicate_filter.is_active() || self.projection_filter.is_active()
    }

    /// Whether `col` (any case) is visible under the projection filter.
    /// Columns absent from the schema are reported as not visible.
    pub fn is_visible(&self, name: &str) -> bool {
        self.projection_filter.allows(name)
    }

    /// The columns visible under the projection filter, in schema order.
    pub fn visible_columns(&self) -> Vec<&ColumnInfo> {
        self.columns
            .iter()
            .filter(|c| self.projection_filter.allows(&c.name))
            .collect()
    }

    /// Case-insensitive lookup. Returns the canonical `ColumnInfo`.
    pub fn find(&self, name: &str) -> Result<&ColumnInfo, AppError> {
        self.by_name
            .get(&name.to_lowercase())
            .map(|&i| &self.columns[i])
            .ok_or_else(|| AppError::UnknownColumn(name.into()))
    }

    /// Like [`find`](Self::find), but a column hidden by the projection
    /// filter is reported as `UnknownColumn` — hidden columns must not be
    /// distinguishable from ones that never existed.
    pub fn find_visible(&self, name: &str) -> Result<&ColumnInfo, AppError> {
        let col = self.find(name)?;
        if self.projection_filter.allows(&col.name) {
            Ok(col)
        } else {
            Err(AppError::UnknownColumn(name.into()))
        }
    }

    /// Resolve a column referenced by a predicate: it must be visible
    /// (hidden columns → `UnknownColumn`) *and* allowed by the predicate
    /// filter (otherwise `Forbidden`).
    pub fn find_for_predicate(&self, name: &str) -> Result<&ColumnInfo, AppError> {
        let col = self.find_visible(name)?;
        if self.predicate_filter.allows(&col.name) {
            Ok(col)
        } else {
            Err(AppError::Forbidden(format!(
                "column '{}' may not be used in predicates on dataset '{}'",
                col.name, self.name
            )))
        }
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
        DatasetSchema::new(
            "ds",
            vec![
                ColumnInfo {
                    name: "Id".into(),
                    logical: LogicalType::Int,
                    sql_type: "BIGINT".into(),
                    nullable: false,
                },
                ColumnInfo {
                    name: "When".into(),
                    logical: LogicalType::Temporal,
                    sql_type: "TIMESTAMP".into(),
                    nullable: true,
                },
            ],
        )
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
        for t in [
            LogicalType::Bool,
            LogicalType::Int,
            LogicalType::Float,
            LogicalType::Utf8,
            LogicalType::Other,
        ] {
            assert!(!t.needs_cast());
        }
    }

    fn excl(cols: &[&str]) -> ColumnFilter {
        ColumnFilter {
            include: vec![],
            exclude: cols.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn incl(cols: &[&str]) -> ColumnFilter {
        ColumnFilter {
            include: cols.iter().map(|s| s.to_string()).collect(),
            exclude: vec![],
        }
    }

    #[test]
    fn with_filters_rejects_unknown_column() {
        let err = s()
            .with_filters(excl(&["ghost"]), ColumnFilter::default())
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    #[test]
    fn with_filters_rejects_include_and_exclude() {
        let both = ColumnFilter {
            include: vec!["Id".into()],
            exclude: vec!["When".into()],
        };
        let err = s().with_filters(ColumnFilter::default(), both).unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    #[test]
    fn projection_exclude_hides_column() {
        // case-insensitive match against the excluded name
        let sch = s()
            .with_filters(ColumnFilter::default(), excl(&["when"]))
            .unwrap();
        assert!(sch.is_visible("Id"));
        assert!(!sch.is_visible("When"));
        let visible: Vec<_> = sch.visible_columns().iter().map(|c| &c.name).collect();
        assert_eq!(visible, vec!["Id"]);
        // hidden column looks like it never existed
        assert!(matches!(
            sch.find_visible("When").unwrap_err(),
            AppError::UnknownColumn(_)
        ));
    }

    #[test]
    fn projection_include_is_an_allowlist() {
        let sch = s()
            .with_filters(ColumnFilter::default(), incl(&["Id"]))
            .unwrap();
        assert!(sch.is_visible("Id"));
        assert!(!sch.is_visible("When"));
    }

    #[test]
    fn predicate_denied_column_is_forbidden_but_visible() {
        let sch = s()
            .with_filters(excl(&["When"]), ColumnFilter::default())
            .unwrap();
        // still selectable
        assert!(sch.find_visible("When").is_ok());
        // but not usable in a predicate
        assert!(matches!(
            sch.find_for_predicate("When").unwrap_err(),
            AppError::Forbidden(_)
        ));
        assert!(sch.find_for_predicate("Id").is_ok());
    }

    #[test]
    fn hidden_column_in_predicate_is_unknown_not_forbidden() {
        // projection-hidden takes precedence: don't reveal existence
        let sch = s()
            .with_filters(ColumnFilter::default(), excl(&["When"]))
            .unwrap();
        assert!(matches!(
            sch.find_for_predicate("When").unwrap_err(),
            AppError::UnknownColumn(_)
        ));
    }
}
