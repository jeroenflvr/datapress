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

/// A single aggregation in a `group_by` query.
///
/// `op` is one of `count | sum | avg | min | max` (case-insensitive).
/// `col` is required for every op except `count`, where it may be omitted
/// to mean `COUNT(*)`. `alias` is the JSON output key; if omitted, it
/// defaults to `count` for `COUNT(*)` and `{op}_{col}` otherwise.
#[derive(Clone, Deserialize)]
pub struct Aggregation {
    #[serde(default)]
    pub col:   Option<String>,
    pub op:    String,
    #[serde(default)]
    pub alias: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct QueryRequest {
    /// Columns to return. Empty = all columns. Ignored when `group_by` is
    /// non-empty (the SELECT list is then derived from `group_by` + `aggregations`).
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub predicates: Vec<Predicate>,
    /// Group-by columns. Empty = no grouping (regular row scan). When set,
    /// the response shape is `{ group_col_1, …, alias_1, … }` per row.
    #[serde(default)]
    pub group_by: Vec<String>,
    /// Aggregations to compute over each group. When `group_by` is set and
    /// this is empty, an implicit `{ op: "count" }` is added.
    #[serde(default)]
    pub aggregations: Vec<Aggregation>,
    /// Return only distinct rows over the projected columns. Mutually
    /// exclusive with `group_by` / `aggregations`.
    #[serde(default)]
    pub distinct: bool,
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

/// One resolved aggregation, ready for SQL emission.
#[derive(Clone)]
pub struct AggSpec {
    /// Canonical column name from the schema, or `None` for `COUNT(*)`.
    pub col:   Option<String>,
    pub op:    AggOp,
    /// Output alias (JSON key). Always set after planning.
    pub alias: String,
}

#[derive(Clone, Copy)]
pub enum AggOp { Count, Sum, Avg, Min, Max }

impl AggOp {
    pub fn as_sql(self) -> &'static str {
        match self {
            AggOp::Count => "COUNT",
            AggOp::Sum   => "SUM",
            AggOp::Avg   => "AVG",
            AggOp::Min   => "MIN",
            AggOp::Max   => "MAX",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            AggOp::Count => "count",
            AggOp::Sum   => "sum",
            AggOp::Avg   => "avg",
            AggOp::Min   => "min",
            AggOp::Max   => "max",
        }
    }
}

/// Validated `GROUP BY` plan: canonical group columns + resolved aggregations.
#[derive(Clone)]
pub struct AggPlan {
    pub group_cols: Vec<String>,
    pub aggs:       Vec<AggSpec>,
}

impl AggPlan {
    /// All output names exposed by this plan, in SELECT order: group
    /// columns first, then aggregation aliases. Used by `order_by`
    /// validation when grouping is active.
    pub fn output_names(&self) -> Vec<String> {
        let mut v = self.group_cols.clone();
        v.extend(self.aggs.iter().map(|a| a.alias.clone()));
        v
    }
}

impl QueryRequest {
    /// Resolve the `group_by` + `aggregations` request into a validated
    /// plan, or return `Ok(None)` when no grouping was requested.
    ///
    /// When `group_by` is non-empty and `aggregations` is empty, an
    /// implicit `COUNT(*) AS count` is added so the plan always has at
    /// least one output value.
    pub fn agg_plan(&self, schema: &DatasetSchema) -> Result<Option<AggPlan>, AppError> {
        if self.distinct && (!self.group_by.is_empty() || !self.aggregations.is_empty()) {
            return Err(AppError::InvalidValue(
                "distinct is mutually exclusive with group_by / aggregations".into()));
        }
        if self.group_by.is_empty() {
            if !self.aggregations.is_empty() {
                return Err(AppError::InvalidValue(
                    "aggregations require a non-empty group_by".into()));
            }
            return Ok(None);
        }

        let mut group_cols = Vec::with_capacity(self.group_by.len());
        for name in &self.group_by {
            group_cols.push(schema.find(name)?.name.clone());
        }

        let raw_aggs: Vec<Aggregation> = if self.aggregations.is_empty() {
            vec![Aggregation { col: None, op: "count".into(), alias: None }]
        } else {
            self.aggregations.clone()
        };

        let mut aggs = Vec::with_capacity(raw_aggs.len());
        for a in &raw_aggs {
            let op = match a.op.to_ascii_lowercase().as_str() {
                "count" => AggOp::Count,
                "sum"   => AggOp::Sum,
                "avg"   => AggOp::Avg,
                "min"   => AggOp::Min,
                "max"   => AggOp::Max,
                other   => return Err(AppError::InvalidValue(format!(
                    "unknown aggregation op '{other}' (expected count|sum|avg|min|max)"
                ))),
            };
            let col = match (op, a.col.as_deref()) {
                (AggOp::Count, None)     => None,
                (_, None)                => return Err(AppError::InvalidValue(format!(
                    "aggregation '{}' requires a 'col'", op.name()
                ))),
                (_, Some(c))             => Some(schema.find(c)?.name.clone()),
            };
            let alias = a.alias.clone().unwrap_or_else(|| match (op, col.as_deref()) {
                (AggOp::Count, None) => "count".into(),
                (_, Some(c))         => format!("{}_{}", op.name(), c.to_lowercase()),
                _ => unreachable!(),
            });
            aggs.push(AggSpec { col, op, alias });
        }

        Ok(Some(AggPlan { group_cols, aggs }))
    }

    /// Translate `order_by` into a validated SQL fragment, e.g.
    /// `"\"a\" ASC, \"b\" DESC"`. Returns `Ok(None)` if no ordering was
    /// requested.
    ///
    /// When `plan` is `Some`, sort keys must reference a group-by column
    /// or an aggregation alias (the only names in scope after `GROUP BY`).
    /// When `plan` is `None`, sort keys are validated against the dataset
    /// schema.
    pub fn order_by_sql(
        &self,
        schema: &DatasetSchema,
        plan:   Option<&AggPlan>,
    ) -> Result<Option<String>, AppError> {
        if self.order_by.is_empty() {
            return Ok(None);
        }
        let parts: Vec<String> = self.order_by.iter()
            .map(|o| {
                let dir = match o.dir.as_deref().unwrap_or("asc").to_ascii_lowercase().as_str() {
                    "asc"  => "ASC",
                    "desc" => "DESC",
                    other  => return Err(AppError::InvalidValue(format!(
                        "order_by direction must be 'asc' or 'desc' (got '{other}')"
                    ))),
                };
                let ident = match plan {
                    Some(p) => {
                        let lc = o.col.to_lowercase();
                        let allowed = p.output_names();
                        allowed.iter()
                            .find(|n| n.to_lowercase() == lc)
                            .map(|n| DatasetSchema::quote_ident(n))
                            .ok_or_else(|| AppError::UnknownColumn(format!(
                                "{} (must be a group_by column or aggregation alias)",
                                o.col
                            )))?
                    }
                    None => DatasetSchema::quote_ident(&schema.find(&o.col)?.name),
                };
                Ok(format!("{ident} {dir}"))
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
fn default_page_size() -> u64 { 1000 }

/// Body for `POST /api/datasets/{name}/count`. Predicates are optional —
/// an empty body (or `{}`) counts every row in the dataset.
#[derive(Clone, Deserialize, Default)]
pub struct CountRequest {
    #[serde(default)]
    pub predicates: Vec<Predicate>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ColumnInfo, DatasetSchema, LogicalType};

    fn schema() -> DatasetSchema {
        DatasetSchema::new("t", vec![
            ColumnInfo { name: "id".into(),     logical: LogicalType::Int,     sql_type: "BIGINT".into(),   nullable: false },
            ColumnInfo { name: "name".into(),   logical: LogicalType::Utf8,    sql_type: "VARCHAR".into(),  nullable: true  },
            ColumnInfo { name: "score".into(),  logical: LogicalType::Float,   sql_type: "DOUBLE".into(),   nullable: true  },
            ColumnInfo { name: "Mixed".into(),  logical: LogicalType::Utf8,    sql_type: "VARCHAR".into(),  nullable: true  },
        ])
    }

    fn empty_req() -> QueryRequest {
        QueryRequest {
            columns: vec![],
            predicates: vec![],
            group_by: vec![],
            aggregations: vec![],
            distinct: false,
            order_by: vec![],
            limit: None,
            page: 1,
            page_size: 1000,
        }
    }

    // ---- agg_plan -----------------------------------------------------------

    #[test]
    fn agg_plan_none_when_no_group_by() {
        let r = empty_req();
        assert!(r.agg_plan(&schema()).unwrap().is_none());
    }

    #[test]
    fn agg_plan_rejects_aggs_without_group_by() {
        let mut r = empty_req();
        r.aggregations = vec![Aggregation { col: Some("score".into()), op: "sum".into(), alias: None }];
        let err = r.agg_plan(&schema()).err().expect("expected error");
        assert!(matches!(err, AppError::InvalidValue(_)), "got {err:?}");
    }

    #[test]
    fn agg_plan_implicit_count_star() {
        let mut r = empty_req();
        r.group_by = vec!["name".into()];
        let plan = r.agg_plan(&schema()).unwrap().unwrap();
        assert_eq!(plan.group_cols, vec!["name"]);
        assert_eq!(plan.aggs.len(), 1);
        assert_eq!(plan.aggs[0].alias, "count");
        assert!(plan.aggs[0].col.is_none());
        assert!(matches!(plan.aggs[0].op, AggOp::Count));
    }

    #[test]
    fn agg_plan_default_alias_format() {
        let mut r = empty_req();
        r.group_by = vec!["name".into()];
        r.aggregations = vec![
            Aggregation { col: Some("score".into()), op: "Sum".into(),  alias: None },
            Aggregation { col: Some("Mixed".into()), op: "MAX".into(),  alias: Some("hi".into()) },
        ];
        let plan = r.agg_plan(&schema()).unwrap().unwrap();
        assert_eq!(plan.aggs[0].alias, "sum_score");
        assert_eq!(plan.aggs[1].alias, "hi");
        // Canonical column name is preserved from the schema (case fix).
        assert_eq!(plan.aggs[1].col.as_deref(), Some("Mixed"));
    }

    #[test]
    fn agg_plan_unknown_op() {
        let mut r = empty_req();
        r.group_by = vec!["name".into()];
        r.aggregations = vec![Aggregation { col: Some("score".into()), op: "median".into(), alias: None }];
        let err = r.agg_plan(&schema()).err().expect("expected error");
        assert!(matches!(err, AppError::InvalidValue(m) if m.contains("median")));
    }

    #[test]
    fn agg_plan_non_count_requires_col() {
        let mut r = empty_req();
        r.group_by = vec!["name".into()];
        r.aggregations = vec![Aggregation { col: None, op: "avg".into(), alias: None }];
        let err = r.agg_plan(&schema()).err().expect("expected error");
        assert!(matches!(err, AppError::InvalidValue(m) if m.contains("avg")));
    }

    #[test]
    fn agg_plan_unknown_group_col() {
        let mut r = empty_req();
        r.group_by = vec!["nope".into()];
        let err = r.agg_plan(&schema()).err().expect("expected error");
        assert!(matches!(err, AppError::UnknownColumn(_)));
    }

    #[test]
    fn agg_plan_distinct_conflicts_with_group_by() {
        let mut r = empty_req();
        r.distinct = true;
        r.group_by = vec!["name".into()];
        let err = r.agg_plan(&schema()).err().expect("expected error");
        assert!(matches!(err, AppError::InvalidValue(_)));
    }

    // ---- order_by_sql -------------------------------------------------------

    #[test]
    fn order_by_none_when_empty() {
        let r = empty_req();
        assert!(r.order_by_sql(&schema(), None).unwrap().is_none());
    }

    #[test]
    fn order_by_default_asc_and_quoting() {
        let mut r = empty_req();
        r.order_by = vec![OrderBy { col: "ID".into(), dir: None }];
        let sql = r.order_by_sql(&schema(), None).unwrap().unwrap();
        // Canonical name from schema preserved + quoted.
        assert_eq!(sql, "\"id\" ASC");
    }

    #[test]
    fn order_by_desc_case_insensitive() {
        let mut r = empty_req();
        r.order_by = vec![OrderBy { col: "name".into(), dir: Some("DESC".into()) }];
        let sql = r.order_by_sql(&schema(), None).unwrap().unwrap();
        assert_eq!(sql, "\"name\" DESC");
    }

    #[test]
    fn order_by_bad_direction() {
        let mut r = empty_req();
        r.order_by = vec![OrderBy { col: "id".into(), dir: Some("backwards".into()) }];
        let err = r.order_by_sql(&schema(), None).unwrap_err();
        assert!(matches!(err, AppError::InvalidValue(m) if m.contains("backwards")));
    }

    #[test]
    fn order_by_unknown_col_no_plan() {
        let mut r = empty_req();
        r.order_by = vec![OrderBy { col: "missing".into(), dir: None }];
        let err = r.order_by_sql(&schema(), None).unwrap_err();
        assert!(matches!(err, AppError::UnknownColumn(_)));
    }

    #[test]
    fn order_by_with_plan_restricts_to_outputs() {
        let mut r = empty_req();
        r.group_by = vec!["name".into()];
        r.aggregations = vec![Aggregation { col: Some("score".into()), op: "sum".into(), alias: Some("total".into()) }];
        let plan = r.agg_plan(&schema()).unwrap().unwrap();

        // Allowed: group col + alias.
        r.order_by = vec![
            OrderBy { col: "name".into(),  dir: Some("asc".into())  },
            OrderBy { col: "TOTAL".into(), dir: Some("desc".into()) },
        ];
        let sql = r.order_by_sql(&schema(), Some(&plan)).unwrap().unwrap();
        assert_eq!(sql, "\"name\" ASC, \"total\" DESC");

        // Not allowed: raw schema column that isn't in the group/agg output.
        r.order_by = vec![OrderBy { col: "id".into(), dir: None }];
        let err = r.order_by_sql(&schema(), Some(&plan)).unwrap_err();
        assert!(matches!(err, AppError::UnknownColumn(_)));
    }

    // ---- effective_limit_offset --------------------------------------------

    #[test]
    fn limit_offset_first_page_default() {
        let r = empty_req();
        assert_eq!(r.effective_limit_offset(1000), (1000, 0));
    }

    #[test]
    fn limit_offset_pagination() {
        let mut r = empty_req();
        r.page = 3;
        r.page_size = 50;
        assert_eq!(r.effective_limit_offset(1000), (50, 100));
    }

    #[test]
    fn limit_offset_caps_page_size_to_max() {
        let mut r = empty_req();
        r.page_size = 10_000;
        assert_eq!(r.effective_limit_offset(1000), (1000, 0));
    }

    #[test]
    fn limit_offset_page_zero_treated_as_one() {
        let mut r = empty_req();
        r.page = 0;
        r.page_size = 10;
        assert_eq!(r.effective_limit_offset(1000), (10, 0));
    }

    #[test]
    fn limit_offset_top_level_cap_truncates_last_page() {
        let mut r = empty_req();
        r.page = 2;
        r.page_size = 50;
        r.limit = Some(75); // offset 50, only 25 rows remain under cap.
        assert_eq!(r.effective_limit_offset(1000), (25, 50));
    }

    #[test]
    fn limit_offset_top_level_cap_exhausted_returns_zero() {
        let mut r = empty_req();
        r.page = 3;
        r.page_size = 50;
        r.limit = Some(75); // offset 100 >= 75 -> empty page.
        assert_eq!(r.effective_limit_offset(1000), (0, 100));
    }
}
