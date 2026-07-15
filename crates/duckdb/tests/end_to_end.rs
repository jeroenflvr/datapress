//! End-to-end tests for the DuckDB backend.
//!
//! Builds a small parquet on disk via DuckDB itself (no extra parquet
//! writer crate), loads it through the public `load_registry` API, then
//! exercises the `Backend` trait — query, count, predicate matrix,
//! group_by, distinct, ordering, pagination, and the Arrow IPC roundtrip.

use std::sync::Arc;

use arrow::array::{Array, Int32Array, StringArray};
use arrow::datatypes::DataType;
use arrow::ipc::reader::StreamReader;
use futures_util::StreamExt;
use serde_json::Value;
use tempfile::TempDir;

use datapress_core::backend::Backend;
use datapress_core::config::{
    AppConfig, DatasetConfig, IndexConfig, ServerConfig, SourceConfig, SourceKind,
};
use datapress_core::models::{Aggregation, CountRequest, OrderBy, Predicate, QueryRequest};
use datapress_duckdb::db::{Registry, load_registry};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn empty_req() -> QueryRequest {
    QueryRequest {
        columns: vec![],
        predicates: vec![],
        group_by: vec![],
        aggregations: vec![],
        having: vec![],
        distinct: false,
        order_by: vec![],
        limit: None,
        page: 1,
        page_size: 1000,
    }
}

/// Build a tiny parquet at `dir/sample.parquet` with 5 rows:
///   id | name    | score | city
///   1  | "Anna"  | 10.5  | "NYC"
///   2  | "Bob"   | 20.0  | "LA"
///   3  | "Cara"  | NULL  | "NYC"
///   4  | "Dan"   | 40.0  | "NYC"
///   5  | "Eve"   | 50.5  | "LA"
fn write_sample_parquet(dir: &std::path::Path) -> std::path::PathBuf {
    let parquet = dir.join("sample.parquet");
    let conn = duckdb::Connection::open_in_memory().unwrap();
    // Build the rows via UNION ALL and COPY to parquet.
    let sql = format!(
        "COPY (
            SELECT 1 AS id, 'Anna' AS name, 10.5 AS score, 'NYC' AS city
            UNION ALL SELECT 2, 'Bob',  20.0, 'LA'
            UNION ALL SELECT 3, 'Cara', NULL, 'NYC'
            UNION ALL SELECT 4, 'Dan',  40.0, 'NYC'
            UNION ALL SELECT 5, 'Eve',  50.5, 'LA'
            ORDER BY id
         ) TO '{}' (FORMAT PARQUET);",
        parquet.display()
    );
    conn.execute_batch(&sql).expect("write parquet");
    parquet
}

fn make_registry(parquet: &std::path::Path) -> Arc<Registry> {
    make_registry_at(&parquet.display().to_string())
}

/// Like `make_registry`, but takes an arbitrary source `location` string
/// (file, directory, or glob) so tests can exercise multi-file datasets.
fn make_registry_at(location: &str) -> Arc<Registry> {
    let cfg = AppConfig {
        server: ServerConfig::default(),
        docs: datapress_core::config::DocsConfig::default(),
        swagger: datapress_core::config::SwaggerConfig::default(),
        auth: datapress_core::config::AuthConfig::default(),
        metrics: datapress_core::config::MetricsConfig::default(),
        explorer: datapress_core::config::ExplorerConfig::default(),
        sql: datapress_core::config::SqlConfig::default(),
        datafusion: datapress_core::config::DataFusionConfig::default(),
        datasets: vec![DatasetConfig {
            name: "people".into(),
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: location.to_string(),
                sql: None,
                depends_on: vec![],
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
            predicate_filter: Default::default(),
            projection_filter: Default::default(),
            on_start: datapress_core::config::OnStart::Eager,
            refresh: None,
            materialize: None,
        }],
    };
    Arc::new(load_registry(&cfg).expect("load_registry"))
}

/// Like `make_registry`, but registers the dataset with `lazy = true` so
/// the DuckDB backend serves it from a streaming view over the parquet
/// scan instead of materialising an in-memory table.
fn make_registry_lazy(location: &str) -> Arc<Registry> {
    let cfg = AppConfig {
        server: ServerConfig::default(),
        docs: datapress_core::config::DocsConfig::default(),
        swagger: datapress_core::config::SwaggerConfig::default(),
        auth: datapress_core::config::AuthConfig::default(),
        metrics: datapress_core::config::MetricsConfig::default(),
        explorer: datapress_core::config::ExplorerConfig::default(),
        sql: datapress_core::config::SqlConfig::default(),
        datafusion: datapress_core::config::DataFusionConfig::default(),
        datasets: vec![DatasetConfig {
            name: "people".into(),
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: location.to_string(),
                sql: None,
                depends_on: vec![],
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: true,
            predicate_filter: Default::default(),
            projection_filter: Default::default(),
            on_start: datapress_core::config::OnStart::Eager,
            refresh: None,
            materialize: None,
        }],
    };
    Arc::new(load_registry(&cfg).expect("load_registry"))
}

/// Build a hive-partitioned dataset under `dir`:
///   dir/city=NYC/part.parquet  -> id, name, score   (3 rows)
///   dir/city=LA/part.parquet   -> id, name, score   (2 rows)
/// The partition key `city` is encoded *only* in the directory name, never
/// inside the parquet files. Returns the dataset root `dir`.
fn write_hive_dataset(dir: &std::path::Path) {
    for (city, rows) in [
        (
            "NYC",
            "SELECT 1 AS id, 'Anna' AS name, 10.5 AS score
                 UNION ALL SELECT 3, 'Cara', 30.0
                 UNION ALL SELECT 4, 'Dan',  40.0",
        ),
        (
            "LA",
            "SELECT 2 AS id, 'Bob' AS name, 20.0 AS score
                 UNION ALL SELECT 5, 'Eve', 50.5",
        ),
    ] {
        let part_dir = dir.join(format!("city={city}"));
        std::fs::create_dir_all(&part_dir).unwrap();
        let parquet = part_dir.join("part.parquet");
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let sql = format!("COPY ({rows}) TO '{}' (FORMAT PARQUET);", parquet.display());
        conn.execute_batch(&sql).expect("write hive parquet");
    }
}

fn parse_rows(s: &str) -> Vec<Value> {
    let v: Value = serde_json::from_str(s).expect("valid json");
    v.as_array().expect("json array").clone()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn names_and_summary() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    assert_eq!(reg.names(), vec!["people".to_string()]);
    let s = reg.summary("people").unwrap();
    assert_eq!(s.rows, 5);
    assert_eq!(s.columns, 4);

    assert!(reg.summary("missing").is_err());
}

/// Build a registry whose `people` dataset carries the given column-access
/// filters, exercising the DuckDB `introspect_schema` + `with_filters`
/// wiring end to end.
fn make_registry_with_filters(
    location: &str,
    predicate_filter: datapress_core::config::ColumnFilter,
    projection_filter: datapress_core::config::ColumnFilter,
) -> Result<Arc<Registry>, datapress_core::errors::AppError> {
    let cfg = AppConfig {
        server: ServerConfig::default(),
        docs: datapress_core::config::DocsConfig::default(),
        swagger: datapress_core::config::SwaggerConfig::default(),
        auth: datapress_core::config::AuthConfig::default(),
        metrics: datapress_core::config::MetricsConfig::default(),
        explorer: datapress_core::config::ExplorerConfig::default(),
        sql: datapress_core::config::SqlConfig::default(),
        datafusion: datapress_core::config::DataFusionConfig::default(),
        datasets: vec![DatasetConfig {
            name: "people".into(),
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: location.to_string(),
                sql: None,
                depends_on: vec![],
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
            predicate_filter,
            projection_filter,
            on_start: datapress_core::config::OnStart::Eager,
            refresh: None,
            materialize: None,
        }],
    };
    load_registry(&cfg).map(Arc::new)
}

#[actix_web::test]
async fn projection_filter_is_attached_to_schema_at_registration() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let excl = datapress_core::config::ColumnFilter {
        include: vec![],
        exclude: vec!["score".into()],
    };
    let reg = make_registry_with_filters(&parquet.display().to_string(), Default::default(), excl)
        .expect("load_registry");

    let schema = reg.schema("people").expect("schema");
    assert!(schema.projection_filter.is_active());
    assert!(!schema.is_visible("score"));
    assert!(schema.is_visible("id"));
    let visible: Vec<_> = schema.visible_columns().iter().map(|c| &c.name).collect();
    assert_eq!(visible, vec!["id", "name", "city"]);
}

#[actix_web::test]
async fn unknown_filter_column_fails_registration() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let bad = datapress_core::config::ColumnFilter {
        include: vec![],
        exclude: vec!["ghost".into()],
    };
    let err =
        match make_registry_with_filters(&parquet.display().to_string(), Default::default(), bad) {
            Ok(_) => panic!("registration should fail on unknown filter column"),
            Err(e) => e,
        };
    assert!(matches!(
        err,
        datapress_core::errors::AppError::InvalidValue(_)
    ));
}

#[actix_web::test]
async fn full_scan_returns_all_rows() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let mut req = empty_req();
    req.order_by = vec![OrderBy {
        col: "id".into(),
        dir: Some("asc".into()),
    }];
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["id"], Value::from(1));
    assert_eq!(rows[0]["name"], Value::from("Anna"));
}

#[actix_web::test]
async fn lazy_dataset_queries_via_streaming_view() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry_lazy(&parquet.display().to_string());

    // Full scan returns every row, just like an eager table.
    let mut req = empty_req();
    req.order_by = vec![OrderBy {
        col: "id".into(),
        dir: Some("asc".into()),
    }];
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["name"], Value::from("Anna"));

    // Predicates push down through the view into the parquet reader.
    let mut filtered = empty_req();
    filtered.predicates = vec![Predicate {
        col: "city".into(),
        op: "eq".into(),
        val: Some("LA".into()),
    }];
    let la = parse_rows(&reg.query("people", &filtered).await.unwrap());
    assert_eq!(la.len(), 2);

    // Count goes through the same view.
    let n = reg
        .count(
            "people",
            &CountRequest {
                predicates: filtered.predicates.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(n, 2);
}

#[actix_web::test]
async fn predicate_matrix() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let cases: Vec<(Predicate, usize)> = vec![
        (
            Predicate {
                col: "name".into(),
                op: "eq".into(),
                val: Some("Bob".into()),
            },
            1,
        ),
        (
            Predicate {
                col: "name".into(),
                op: "neq".into(),
                val: Some("Bob".into()),
            },
            4,
        ),
        (
            Predicate {
                col: "id".into(),
                op: "gt".into(),
                val: Some(2.into()),
            },
            3,
        ),
        (
            Predicate {
                col: "id".into(),
                op: "gte".into(),
                val: Some(2.into()),
            },
            4,
        ),
        (
            Predicate {
                col: "id".into(),
                op: "lt".into(),
                val: Some(3.into()),
            },
            2,
        ),
        (
            Predicate {
                col: "id".into(),
                op: "lte".into(),
                val: Some(3.into()),
            },
            3,
        ),
        (
            Predicate {
                col: "name".into(),
                op: "like".into(),
                val: Some("A%".into()),
            },
            1,
        ),
        (
            Predicate {
                col: "name".into(),
                op: "ilike".into(),
                val: Some("a%".into()),
            },
            1,
        ),
        (
            Predicate {
                col: "city".into(),
                op: "in".into(),
                val: Some(serde_json::json!(["NYC", "LA"])),
            },
            5,
        ),
        (
            Predicate {
                col: "city".into(),
                op: "in".into(),
                val: Some(serde_json::json!(["LA"])),
            },
            2,
        ),
        (
            Predicate {
                col: "score".into(),
                op: "is_null".into(),
                val: None,
            },
            1,
        ),
        (
            Predicate {
                col: "score".into(),
                op: "is_not_null".into(),
                val: None,
            },
            4,
        ),
    ];

    for (pred, expected) in cases {
        let op = pred.op.clone();
        let col = pred.col.clone();
        let mut req = empty_req();
        req.predicates = vec![pred];
        let rows = parse_rows(&reg.query("people", &req).await.unwrap());
        assert_eq!(
            rows.len(),
            expected,
            "predicate {op} on {col} returned {} rows (expected {expected})",
            rows.len()
        );
    }
}

#[actix_web::test]
async fn unknown_column_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let mut req = empty_req();
    req.predicates = vec![Predicate {
        col: "nope".into(),
        op: "eq".into(),
        val: Some(1.into()),
    }];
    let err = reg.query("people", &req).await.expect_err("error");
    let msg = err.to_string();
    assert!(msg.contains("unknown column"), "got: {msg}");
}

#[actix_web::test]
async fn count_endpoint_matches_predicates() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let n = reg
        .count("people", &CountRequest { predicates: vec![] })
        .await
        .unwrap();
    assert_eq!(n, 5);

    let n = reg
        .count(
            "people",
            &CountRequest {
                predicates: vec![Predicate {
                    col: "city".into(),
                    op: "eq".into(),
                    val: Some("NYC".into()),
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(n, 3);
}

#[actix_web::test]
async fn group_by_with_default_count_and_named_aggs() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    // Implicit COUNT(*) AS count.
    let mut req = empty_req();
    req.group_by = vec!["city".into()];
    req.order_by = vec![OrderBy {
        col: "city".into(),
        dir: Some("asc".into()),
    }];
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 2);
    let la = rows.iter().find(|r| r["city"] == "LA").unwrap();
    let nyc = rows.iter().find(|r| r["city"] == "NYC").unwrap();
    assert_eq!(la["count"], Value::from(2));
    assert_eq!(nyc["count"], Value::from(3));

    // Explicit SUM + AVG + MIN + MAX with custom alias, ordered by an
    // aggregation alias. The JSON path runs the aggregation in an inner
    // subquery so `ORDER BY <alias>` resolves against a real output column.
    let mut req = empty_req();
    req.group_by = vec!["city".into()];
    req.aggregations = vec![
        Aggregation {
            col: Some("score".into()),
            op: "sum".into(),
            alias: Some("total".into()),
        },
        Aggregation {
            col: Some("score".into()),
            op: "min".into(),
            alias: None,
        },
        Aggregation {
            col: Some("score".into()),
            op: "max".into(),
            alias: None,
        },
    ];
    req.order_by = vec![OrderBy {
        col: "total".into(),
        dir: Some("asc".into()),
    }];
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 2);
    // NYC has scores 10.5, NULL, 40.0 — sum = 50.5, min = 10.5, max = 40.0.
    // LA  has scores 20.0, 50.5            — sum = 70.5, min = 20.0, max = 50.5.
    // Ordered by `total` ASC → NYC (50.5) before LA (70.5).
    assert_eq!(rows[0]["city"], Value::from("NYC"));
    assert_eq!(rows[1]["city"], Value::from("LA"));
    let la = rows.iter().find(|r| r["city"] == "LA").unwrap();
    let nyc = rows.iter().find(|r| r["city"] == "NYC").unwrap();
    assert_eq!(la["total"].as_f64().unwrap(), 70.5);
    assert_eq!(nyc["total"].as_f64().unwrap(), 50.5);
    assert_eq!(la["min_score"].as_f64().unwrap(), 20.0);
    assert_eq!(nyc["max_score"].as_f64().unwrap(), 40.0);
}

#[actix_web::test]
async fn group_by_with_having_filters_groups() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    // HAVING on the implicit COUNT(*) alias: keep only groups with > 2 rows.
    // NYC has 3 rows, LA has 2 — only NYC survives.
    let mut req = empty_req();
    req.group_by = vec!["city".into()];
    req.having = vec![Predicate {
        col: "count".into(),
        op: "gt".into(),
        val: Some(Value::from(2)),
    }];
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["city"], Value::from("NYC"));
    assert_eq!(rows[0]["count"], Value::from(3));

    // HAVING on a named aggregation alias, combined with ORDER BY. Keep
    // groups whose summed score is >= 60 — LA (70.5) qualifies, NYC (50.5)
    // does not.
    let mut req = empty_req();
    req.group_by = vec!["city".into()];
    req.aggregations = vec![Aggregation {
        col: Some("score".into()),
        op: "sum".into(),
        alias: Some("total".into()),
    }];
    req.having = vec![Predicate {
        col: "total".into(),
        op: "gte".into(),
        val: Some(Value::from(60)),
    }];
    req.order_by = vec![OrderBy {
        col: "total".into(),
        dir: Some("desc".into()),
    }];
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["city"], Value::from("LA"));
    assert_eq!(rows[0]["total"].as_f64().unwrap(), 70.5);

    // HAVING without group_by is rejected.
    let mut req = empty_req();
    req.having = vec![Predicate {
        col: "count".into(),
        op: "gt".into(),
        val: Some(Value::from(1)),
    }];
    assert!(reg.query("people", &req).await.is_err());
}

#[actix_web::test]
async fn distinct_dedups_projection() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let mut req = empty_req();
    req.columns = vec!["city".into()];
    req.distinct = true;
    req.order_by = vec![OrderBy {
        col: "city".into(),
        dir: Some("asc".into()),
    }];
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    let cities: Vec<&str> = rows.iter().map(|r| r["city"].as_str().unwrap()).collect();
    assert_eq!(cities, vec!["LA", "NYC"]);
}

#[actix_web::test]
async fn pagination_and_limit_cap() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let mut req = empty_req();
    req.order_by = vec![OrderBy {
        col: "id".into(),
        dir: Some("asc".into()),
    }];
    req.page_size = 2;
    req.page = 2;
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], 3);
    assert_eq!(rows[1]["id"], 4);

    // Top-level limit truncates the last page.
    req.page = 1;
    req.page_size = 10;
    req.limit = Some(3);
    let rows = parse_rows(&reg.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 3);
}

#[actix_web::test]
async fn arrow_ipc_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let mut req = empty_req();
    req.columns = vec!["id".into(), "name".into()];
    req.order_by = vec![OrderBy {
        col: "id".into(),
        dir: Some("asc".into()),
    }];

    let bytes = reg.query_arrow("people", &req).await.expect("arrow ipc");
    assert!(!bytes.is_empty());

    let reader = StreamReader::try_new(std::io::Cursor::new(bytes), None).expect("stream reader");
    let schema = reader.schema();
    let fields: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(fields, vec!["id", "name"]);

    let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 5);

    // First batch should at least contain the first row. DuckDB returns
    // integer literals as INT32 by default, so accept either width.
    let first = &batches[0];
    assert!(matches!(
        first.column(0).data_type(),
        DataType::Int32 | DataType::Int64,
    ));
    let ids = first
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("int32");
    let names = first
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("utf8");
    assert_eq!(ids.value(0), 1);
    assert_eq!(names.value(0), "Anna");
}

#[actix_web::test]
async fn arrow_stream_all_ignores_page_size() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let mut req = empty_req();
    req.page_size = 2;

    let stream = reg
        .query_arrow_stream_all("people", &req)
        .await
        .expect("arrow stream");
    let chunks = stream.collect::<Vec<_>>().await;
    let mut bytes = Vec::new();
    for chunk in chunks {
        bytes.extend_from_slice(&chunk.unwrap());
    }

    let reader = StreamReader::try_new(std::io::Cursor::new(bytes), None).unwrap();
    let rows: usize = reader.map(|batch| batch.unwrap().num_rows()).sum();
    assert_eq!(rows, 5);
}

#[actix_web::test]
async fn arrow_ipc_with_group_by_emits_typed_columns() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let reg = make_registry(&parquet);

    let mut req = empty_req();
    req.group_by = vec!["city".into()];
    req.aggregations = vec![Aggregation {
        col: Some("score".into()),
        op: "sum".into(),
        alias: Some("total".into()),
    }];

    let bytes = reg.query_arrow("people", &req).await.expect("arrow ipc");
    let reader = StreamReader::try_new(std::io::Cursor::new(bytes), None).unwrap();
    let schema = reader.schema();
    let fields: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(fields, vec!["city", "total"]);

    let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2);
}

// ---------------------------------------------------------------------------
// Hive-partitioned / multi-file directory datasets
//
// Layout: `city=NYC/part.parquet`, `city=LA/part.parquet` — the partition
// key `city` lives only in the directory name. DuckDB's `read_parquet`
// auto-detects hive `key=value` segments, so both the multi-file union and
// the partition column are surfaced.
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn hive_glob_unions_all_files() {
    let tmp = TempDir::new().unwrap();
    write_hive_dataset(tmp.path());
    let glob = format!("{}/city=*/*.parquet", tmp.path().display());
    let reg = make_registry_at(&glob);

    // Multi-file union: all 5 rows across both partitions are visible.
    let rows = parse_rows(&reg.query("people", &empty_req()).await.unwrap());
    assert_eq!(rows.len(), 5, "expected union of both partition files");
}

#[actix_web::test]
async fn hive_partition_column_is_surfaced() {
    let tmp = TempDir::new().unwrap();
    write_hive_dataset(tmp.path());
    let glob = format!("{}/city=*/*.parquet", tmp.path().display());
    let reg = make_registry_at(&glob);

    let rows = parse_rows(&reg.query("people", &empty_req()).await.unwrap());
    let has_city = rows
        .first()
        .map(|r| r.get("city").is_some())
        .unwrap_or(false);
    assert!(
        has_city,
        "hive partition column `city` was not surfaced. row keys: {:?}",
        rows.first()
            .and_then(|r| r.as_object())
            .map(|o| o.keys().collect::<Vec<_>>())
    );
}

// ===========================================================================
// Phase 2B: query-kind source tests (DuckDB)
// ===========================================================================

/// Build a registry with file-backed + query datasets.
fn make_query_registry(
    tmp: &TempDir,
    file_datasets: &[(&str, &str)], // (name, CREATE TABLE SQL)
    query_datasets: &[(&str, &str, &[&str])], // (name, sql, depends_on)
) -> Arc<Registry> {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let mut datasets = Vec::new();

    for (name, create_sql) in file_datasets {
        let parquet = tmp.path().join(format!("{name}.parquet"));
        // Create the data via DuckDB, then write to parquet.
        conn.execute_batch(&format!(
            "{create_sql} COPY {name} TO '{}' (FORMAT PARQUET);",
            parquet.display()
        ))
        .unwrap();
        datasets.push(DatasetConfig {
            name: (*name).into(),
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: parquet.to_str().unwrap().to_string(),
                sql: None,
                depends_on: vec![],
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
            predicate_filter: Default::default(),
            projection_filter: Default::default(),
            on_start: datapress_core::config::OnStart::Eager,
            refresh: None,
            materialize: None,
        });
    }

    for (name, sql, deps) in query_datasets {
        datasets.push(DatasetConfig {
            name: (*name).into(),
            source: SourceConfig {
                kind: SourceKind::Query,
                location: String::new(),
                sql: Some((*sql).into()),
                depends_on: deps.iter().map(|s| (*s).into()).collect(),
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy: false,
            predicate_filter: Default::default(),
            projection_filter: Default::default(),
            on_start: datapress_core::config::OnStart::Eager,
            refresh: None,
            materialize: None,
        });
    }

    let cfg = AppConfig {
        server: ServerConfig::default(),
        docs: datapress_core::config::DocsConfig::default(),
        swagger: datapress_core::config::SwaggerConfig::default(),
        auth: datapress_core::config::AuthConfig::default(),
        metrics: datapress_core::config::MetricsConfig::default(),
        explorer: datapress_core::config::ExplorerConfig::default(),
        sql: datapress_core::config::SqlConfig::default(),
        datafusion: datapress_core::config::DataFusionConfig::default(),
        datasets,
    };
    Arc::new(load_registry(&cfg).expect("load_registry"))
}

// R2.5: schema accessible for a query dataset.
#[actix_web::test]
async fn duck_query_source_single_dataset() {
    let tmp = TempDir::new().unwrap();
    let reg = make_query_registry(
        &tmp,
        &[(
            "people",
            "CREATE TABLE people AS SELECT 1 AS id, 'Anna' AS name UNION ALL \
             SELECT 2, 'Bob' UNION ALL SELECT 3, 'Cara';",
        )],
        &[(
            "top2",
            "SELECT id, name FROM people WHERE id <= 2",
            &["people"],
        )],
    );

    let schema = reg.schema("top2").expect("schema");
    assert!(schema.columns.iter().any(|c| c.name == "id"));
    assert!(schema.columns.iter().any(|c| c.name == "name"));

    let result = reg
        .query_sql("SELECT id FROM top2 ORDER BY id", &[], 100)
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&result).unwrap();
    let ids: Vec<i64> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![1, 2]);
}

// Query over a join of two datasets.
#[actix_web::test]
async fn duck_query_source_join_two_datasets() {
    let tmp = TempDir::new().unwrap();
    let reg = make_query_registry(
        &tmp,
        &[
            (
                "a",
                "CREATE TABLE a AS SELECT 1 AS id, 'Anna' AS name UNION ALL \
                 SELECT 2, 'Bob' UNION ALL SELECT 3, 'Cara';",
            ),
            (
                "b",
                "CREATE TABLE b AS SELECT 1 AS id, 1.0 AS score UNION ALL \
                 SELECT 2, 2.0 UNION ALL SELECT 3, 3.0;",
            ),
        ],
        &[(
            "joined",
            "SELECT a.id, a.name, b.score FROM a JOIN b ON a.id = b.id",
            &["a", "b"],
        )],
    );

    let result = reg
        .query_sql("SELECT id FROM joined ORDER BY id", &[], 100)
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 3);
}

// Chained query-over-query.
#[actix_web::test]
async fn duck_query_source_chain() {
    let tmp = TempDir::new().unwrap();
    let reg = make_query_registry(
        &tmp,
        &[("a", "CREATE TABLE a AS SELECT id FROM range(1, 6) t(id);")],
        &[
            ("b", "SELECT id FROM a WHERE id <= 3", &["a"]),
            ("c", "SELECT id FROM b WHERE id <= 2", &["b"]),
        ],
    );

    let result = reg
        .query_sql("SELECT id FROM c ORDER BY id", &[], 100)
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&result).unwrap();
    let ids: Vec<i64> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![1, 2]);
}

// R2.6: reload of a query dataset.
#[actix_web::test]
async fn duck_query_source_reload() {
    let tmp = TempDir::new().unwrap();
    let parquet = write_sample_parquet(tmp.path());
    let cfg = AppConfig {
        server: ServerConfig::default(),
        docs: datapress_core::config::DocsConfig::default(),
        swagger: datapress_core::config::SwaggerConfig::default(),
        auth: datapress_core::config::AuthConfig::default(),
        metrics: datapress_core::config::MetricsConfig::default(),
        explorer: datapress_core::config::ExplorerConfig::default(),
        sql: datapress_core::config::SqlConfig::default(),
        datafusion: datapress_core::config::DataFusionConfig::default(),
        datasets: vec![
            DatasetConfig {
                name: "people".into(),
                source: SourceConfig {
                    kind: SourceKind::Parquet,
                    location: parquet.to_str().unwrap().to_string(),
                    sql: None,
                    depends_on: vec![],
                },
                s3: None,
                index: IndexConfig::default(),
                columns: vec![],
                dict_encode: true,
                lazy: false,
                predicate_filter: Default::default(),
                projection_filter: Default::default(),
                on_start: datapress_core::config::OnStart::Eager,
                refresh: None,
                materialize: None,
            },
            DatasetConfig {
                name: "high_scores".into(),
                source: SourceConfig {
                    kind: SourceKind::Query,
                    location: String::new(),
                    sql: Some("SELECT id, name FROM people WHERE score >= 40.0".into()),
                    depends_on: vec!["people".into()],
                },
                s3: None,
                index: IndexConfig::default(),
                columns: vec![],
                dict_encode: true,
                lazy: false,
                predicate_filter: Default::default(),
                projection_filter: Default::default(),
                on_start: datapress_core::config::OnStart::Eager,
                refresh: None,
                materialize: None,
            },
        ],
    };
    let reg = Arc::new(load_registry(&cfg).expect("load_registry"));
    let backend = reg.clone() as Arc<dyn datapress_core::backend::Backend>;

    // Initial state: ids 4 and 5 have score >= 40.
    let r1 = backend
        .query_sql("SELECT id FROM high_scores ORDER BY id", &[], 100)
        .await
        .unwrap();
    let rows1: serde_json::Value = serde_json::from_str(&r1).unwrap();
    assert_eq!(rows1.as_array().unwrap().len(), 2);

    // Reload derived dataset.
    let stats = reg.reload("high_scores").await.expect("reload");
    assert_eq!(stats.rows, 2);
}
