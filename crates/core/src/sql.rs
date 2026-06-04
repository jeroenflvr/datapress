//! Shared safety gate for the raw-SQL endpoint (`POST /api/v1/sql`).
//!
//! Raw SQL is a much larger attack surface than the structured `/query`
//! endpoint, so every statement is parsed and validated *before* it is
//! handed to a backend engine. The same gate runs for DuckDB and
//! DataFusion, giving both backends identical safety semantics — and
//! keeping the "which tables may this query touch?" policy in one place.
//!
//! Guarantees enforced by [`validate`]:
//! - exactly one statement, and it is a read-only `SELECT` / `WITH … SELECT`,
//! - every referenced table is a registered dataset — no file-reading
//!   table functions (`read_parquet`, `read_csv`, …), no unknown tables,
//! - no file-reading scalar functions (`read_text`, `read_blob`, …),
//! - at most `max_datasets` distinct datasets are referenced. Phase 1
//!   passes `1`, enforcing the single-dataset rule; raising this bound is
//!   all that's needed to allow cross-dataset joins later.
//!
//! CTE-defined names are tracked per query scope and excluded from the
//! dataset allowlist check, so `WITH t AS (SELECT … FROM events) SELECT …`
//! is accepted (it still only touches `events`).

use std::collections::HashSet;
use std::ops::ControlFlow;

use sqlparser::ast::{Expr, ObjectName, Query, Statement, Visit, Visitor};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::errors::AppError;

/// File-reading / external-access functions that must never run through
/// the SQL endpoint, in either table or scalar position. Table-position
/// functions are already blocked by the relation allowlist; this list
/// closes the scalar-position gap (e.g. `SELECT read_text('/etc/passwd')`).
const DENIED_FUNCTIONS: &[&str] = &[
    "read_text",
    "read_blob",
    "read_csv",
    "read_csv_auto",
    "read_parquet",
    "parquet_scan",
    "read_json",
    "read_json_auto",
    "read_json_objects",
    "read_ndjson",
    "read_ndjson_auto",
    "read_ndjson_objects",
    "sniff_csv",
    "glob",
];

/// A validated, ready-to-execute SQL query.
#[derive(Debug)]
pub struct ValidatedSql {
    /// The trimmed, semicolon-free SQL string, safe to wrap and execute.
    pub sql: String,
    /// The distinct dataset names the query references (lowercased). Empty
    /// for table-less queries such as `SELECT 1`.
    pub datasets: Vec<String>,
}

/// Validate `sql` for the raw-SQL endpoint.
///
/// `allowed` is the set of registered dataset names, **lowercased** by the
/// caller (matching is case-insensitive). `max_datasets` caps how many
/// distinct datasets a single statement may touch (phase 1 = `1`).
///
/// On success returns the cleaned SQL ready to be wrapped in an outer
/// `LIMIT` and executed by the backend.
pub fn validate(
    sql: &str,
    allowed: &HashSet<String>,
    max_datasets: usize,
) -> Result<ValidatedSql, AppError> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidValue("sql must not be empty".into()));
    }

    let statements = Parser::parse_sql(&GenericDialect {}, trimmed)
        .map_err(|e| AppError::InvalidValue(format!("could not parse SQL: {e}")))?;
    if statements.len() != 1 {
        return Err(AppError::InvalidValue(
            "exactly one SQL statement is allowed".into(),
        ));
    }
    let stmt = &statements[0];
    if !matches!(stmt, Statement::Query(_)) {
        return Err(AppError::InvalidValue(
            "only read-only SELECT queries are allowed".into(),
        ));
    }

    let mut checker = ScopeCheck {
        allowed,
        cte_names: HashSet::new(),
        referenced: HashSet::new(),
        violation: None,
    };
    let _ = stmt.visit(&mut checker);
    if let Some(err) = checker.violation {
        return Err(AppError::InvalidValue(err));
    }

    let mut datasets: Vec<String> = checker.referenced.into_iter().collect();
    datasets.sort();
    if datasets.len() > max_datasets {
        return Err(AppError::InvalidValue(format!(
            "this endpoint allows at most {max_datasets} dataset(s) per query; \
             the statement references {}",
            datasets.len()
        )));
    }

    Ok(ValidatedSql {
        sql: trimmed.to_string(),
        datasets,
    })
}

struct ScopeCheck<'a> {
    allowed: &'a HashSet<String>,
    cte_names: HashSet<String>,
    referenced: HashSet<String>,
    violation: Option<String>,
}

impl Visitor for ScopeCheck<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        // Record CTE names *before* visiting the query body so references
        // to them inside the body are recognised and not mistaken for
        // unknown tables. Nested `WITH` clauses are handled the same way
        // as the visitor descends into subqueries.
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.cte_names.insert(cte.alias.name.value.to_lowercase());
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        let ident = relation
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(|i| i.value.to_lowercase())
            .unwrap_or_default();

        if self.cte_names.contains(&ident) {
            return ControlFlow::Continue(());
        }
        if let Some(name) = self.allowed.get(&ident) {
            self.referenced.insert(name.clone());
            return ControlFlow::Continue(());
        }
        self.violation = Some(format!(
            "table '{ident}' is not a registered dataset accessible from the SQL endpoint"
        ));
        ControlFlow::Break(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::Function(func) = expr {
            let fname = func
                .name
                .0
                .last()
                .and_then(|p| p.as_ident())
                .map(|i| i.value.to_lowercase())
                .unwrap_or_default();
            if DENIED_FUNCTIONS.contains(&fname.as_str()) {
                self.violation =
                    Some(format!("function '{fname}' is not allowed in the SQL endpoint"));
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_lowercase()).collect()
    }

    #[test]
    fn accepts_single_dataset_select() {
        let v = validate("SELECT a, b FROM events WHERE a > 1", &allowed(&["events"]), 1).unwrap();
        assert_eq!(v.datasets, vec!["events".to_string()]);
    }

    #[test]
    fn case_insensitive_table_match() {
        let v = validate("SELECT * FROM Events", &allowed(&["events"]), 1).unwrap();
        assert_eq!(v.datasets, vec!["events".to_string()]);
    }

    #[test]
    fn strips_trailing_semicolon() {
        let v = validate("SELECT 1 FROM events;", &allowed(&["events"]), 1).unwrap();
        assert_eq!(v.sql, "SELECT 1 FROM events");
    }

    #[test]
    fn allows_cte_over_single_dataset() {
        let sql = "WITH t AS (SELECT * FROM events) SELECT count(*) FROM t";
        let v = validate(sql, &allowed(&["events"]), 1).unwrap();
        assert_eq!(v.datasets, vec!["events".to_string()]);
    }

    #[test]
    fn allows_tableless_select() {
        let v = validate("SELECT 1 + 1", &allowed(&["events"]), 1).unwrap();
        assert!(v.datasets.is_empty());
    }

    #[test]
    fn rejects_unknown_table() {
        let err = validate("SELECT * FROM secrets", &allowed(&["events"]), 1).unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    #[test]
    fn rejects_second_dataset_join() {
        let err = validate(
            "SELECT * FROM events e JOIN other o ON e.id = o.id",
            &allowed(&["events", "other"]),
            1,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    #[test]
    fn allows_two_datasets_when_limit_raised() {
        let v = validate(
            "SELECT * FROM events e JOIN other o ON e.id = o.id",
            &allowed(&["events", "other"]),
            2,
        )
        .unwrap();
        assert_eq!(v.datasets.len(), 2);
    }

    #[test]
    fn rejects_non_select() {
        let err = validate("DELETE FROM events", &allowed(&["events"]), 1).unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    #[test]
    fn rejects_multiple_statements() {
        let err = validate("SELECT 1 FROM events; SELECT 2 FROM events", &allowed(&["events"]), 1)
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    #[test]
    fn rejects_file_table_function() {
        let err = validate("SELECT * FROM read_parquet('/etc/passwd')", &allowed(&["events"]), 1)
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    #[test]
    fn rejects_file_scalar_function() {
        let err = validate(
            "SELECT read_text('/etc/passwd') FROM events",
            &allowed(&["events"]),
            1,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    #[test]
    fn rejects_empty_sql() {
        let err = validate("   ", &allowed(&["events"]), 1).unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(_)));
    }
}
