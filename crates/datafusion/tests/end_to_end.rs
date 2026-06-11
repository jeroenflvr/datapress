//! End-to-end tests for the DataFusion backend.
//!
//! Focused on multi-file / hive-partitioned directory layouts: builds a
//! `city=NYC/part.parquet` + `city=LA/part.parquet` tree on disk (the
//! partition key lives only in the directory name, never inside the files),
//! loads it through the public `Store` API, and checks both the multi-file
//! union and whether the partition column is surfaced — in eager and lazy
//! modes.

use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use futures_util::StreamExt;
use parquet::arrow::ArrowWriter;
use serde_json::Value;
use tempfile::TempDir;

use datapress_core::config::{
    AppConfig, DatasetConfig, IndexConfig, ServerConfig, SourceConfig, SourceKind,
};
use datapress_core::models::{Aggregation, Predicate, QueryRequest};
use datapress_datafusion::store::Store;

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

/// Write `id|name|score` rows to `path` as a single-row-group parquet file.
fn write_parquet(path: &std::path::Path, ids: &[i64], names: &[&str], scores: &[f64]) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("score", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(ids.to_vec())),
            Arc::new(StringArray::from(names.to_vec())),
            Arc::new(Float64Array::from(scores.to_vec())),
        ],
    )
    .unwrap();

    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

/// Write `id|name|score` rows as a Delta table rooted at `dir` (a fresh,
/// empty directory). Uses the `deltalake` crate's write op so the test
/// exercises the same on-disk format the DataFusion backend reads.
#[allow(deprecated)]
async fn write_delta(dir: &std::path::Path, ids: &[i64], names: &[&str], scores: &[f64]) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("score", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(ids.to_vec())),
            Arc::new(StringArray::from(names.to_vec())),
            Arc::new(Float64Array::from(scores.to_vec())),
        ],
    )
    .unwrap();
    let url = deltalake::ensure_table_uri(dir.to_str().unwrap()).expect("ensure_table_uri");
    let ops = deltalake::DeltaOps::try_from_url(url)
        .await
        .expect("DeltaOps::try_from_url");
    ops.write(vec![batch]).await.expect("delta write");
}

/// Build a `Store` over a single Delta-table dataset named `people`.
async fn make_delta_store(location: &str) -> Store {
    make_delta_store_lazy(location, false).await
}

async fn make_delta_store_lazy(location: &str, lazy: bool) -> Store {
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
                kind: SourceKind::Delta,
                location: location.to_string(),
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy,
        }],
    };
    Store::load(&cfg).await.expect("Store::load")
}

///   dir/city=NYC/part.parquet  -> 3 rows
///   dir/city=LA/part.parquet   -> 2 rows
/// The partition key `city` is encoded only in the directory name.
fn write_hive_dataset(dir: &std::path::Path) {
    let nyc = dir.join("city=NYC");
    let la = dir.join("city=LA");
    std::fs::create_dir_all(&nyc).unwrap();
    std::fs::create_dir_all(&la).unwrap();
    write_parquet(
        &nyc.join("part.parquet"),
        &[1, 3, 4],
        &["Anna", "Cara", "Dan"],
        &[10.5, 30.0, 40.0],
    );
    write_parquet(
        &la.join("part.parquet"),
        &[2, 5],
        &["Bob", "Eve"],
        &[20.0, 50.5],
    );
}

async fn make_store(location: &str, lazy: bool) -> Store {
    make_store_with_max_page_size(location, lazy, ServerConfig::default().max_page_size).await
}

async fn make_store_with_max_page_size(location: &str, lazy: bool, max_page_size: u64) -> Store {
    let cfg = AppConfig {
        server: ServerConfig {
            max_page_size,
            ..ServerConfig::default()
        },
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
            },
            s3: None,
            index: IndexConfig::default(),
            columns: vec![],
            dict_encode: true,
            lazy,
        }],
    };
    Store::load(&cfg).await.expect("Store::load")
}

fn parse_rows(s: &str) -> Vec<Value> {
    let v: Value = serde_json::from_str(s).expect("valid json");
    v.as_array().expect("json array").clone()
}

fn pred(col: &str, op: &str, val: Value) -> Predicate {
    Predicate {
        col: col.into(),
        op: op.into(),
        val: Some(val),
    }
}

fn req_with(preds: Vec<Predicate>) -> QueryRequest {
    QueryRequest {
        predicates: preds,
        ..empty_req()
    }
}

/// Single parquet file whose `name` column includes a value with an
/// embedded single quote and a SQL-injection-looking string.
fn write_people(path: &std::path::Path) {
    write_parquet(
        path,
        &[1, 2, 3, 4],
        &["Anna", "O'Brien", "Bob", "' OR '1'='1"],
        &[10.0, 20.0, 30.0, 40.0],
    );
}

fn write_many_people(path: &std::path::Path, rows: usize) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("score", DataType::Float64, false),
    ]));
    let ids = (0..rows).map(|i| i as i64).collect::<Vec<_>>();
    let names = (0..rows).map(|i| format!("person-{i}")).collect::<Vec<_>>();
    let scores = (0..rows).map(|i| i as f64).collect::<Vec<_>>();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(names)),
            Arc::new(Float64Array::from(scores)),
        ],
    )
    .unwrap();

    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn hive_glob_unions_all_files_eager() {
    let tmp = TempDir::new().unwrap();
    write_hive_dataset(tmp.path());
    let glob = format!("{}/city=*/*.parquet", tmp.path().display());
    let store = make_store(&glob, false).await;

    let rows = parse_rows(&store.query("people", &empty_req()).await.unwrap());
    assert_eq!(rows.len(), 5, "expected union of both partition files");
}

#[actix_web::test]
async fn delta_local_reads_and_filters() {
    let tmp = TempDir::new().unwrap();
    write_delta(
        tmp.path(),
        &[1, 2, 3, 4],
        &["Anna", "Bob", "Cara", "Dan"],
        &[10.0, 20.0, 30.0, 40.0],
    )
    .await;
    let store = make_delta_store(tmp.path().to_str().unwrap()).await;

    // Full scan returns every row from the delta table.
    let rows = parse_rows(&store.query("people", &empty_req()).await.unwrap());
    assert_eq!(rows.len(), 4);

    // Discovery surfaces the delta table under its dataset name.
    assert!(store.names().contains(&"people".to_string()));

    // Predicate pushdown filters through the materialised table.
    let filtered = parse_rows(
        &store
            .query("people", &req_with(vec![pred("name", "eq", Value::from("Bob"))]))
            .await
            .unwrap(),
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["id"], Value::from(2));
}

#[actix_web::test]
async fn delta_local_lazy_reads_and_filters() {
    let tmp = TempDir::new().unwrap();
    write_delta(
        tmp.path(),
        &[1, 2, 3, 4],
        &["Anna", "Bob", "Cara", "Dan"],
        &[10.0, 20.0, 30.0, 40.0],
    )
    .await;
    let store = make_delta_store_lazy(tmp.path().to_str().unwrap(), true).await;

    // Lazy delta streams via the deltalake DataFusion provider; full scan
    // still returns every row.
    let rows = parse_rows(&store.query("people", &empty_req()).await.unwrap());
    assert_eq!(rows.len(), 4);

    // Discovery surfaces the delta table under its dataset name.
    assert!(store.names().contains(&"people".to_string()));

    // Predicate pushdown filters through the lazy provider (Delta file
    // skipping + parquet row-group pruning).
    let filtered = parse_rows(
        &store
            .query("people", &req_with(vec![pred("name", "eq", Value::from("Bob"))]))
            .await
            .unwrap(),
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["id"], Value::from(2));
}

#[actix_web::test]
async fn hive_partition_column_eager() {
    let tmp = TempDir::new().unwrap();
    write_hive_dataset(tmp.path());
    let glob = format!("{}/city=*/*.parquet", tmp.path().display());
    let store = make_store(&glob, false).await;

    let rows = parse_rows(&store.query("people", &empty_req()).await.unwrap());
    let has_city = rows
        .first()
        .map(|r| r.get("city").is_some())
        .unwrap_or(false);
    assert!(
        has_city,
        "hive partition column `city` was not surfaced (eager). row keys: {:?}",
        rows.first()
            .and_then(|r| r.as_object())
            .map(|o| o.keys().collect::<Vec<_>>())
    );
}

#[actix_web::test]
async fn hive_partition_column_lazy() {
    let tmp = TempDir::new().unwrap();
    write_hive_dataset(tmp.path());
    // Lazy mode registers a ListingTable rooted at the directory.
    let root = tmp.path().display().to_string();
    let store = make_store(&root, true).await;

    let rows = parse_rows(&store.query("people", &empty_req()).await.unwrap());
    assert_eq!(
        rows.len(),
        5,
        "lazy: expected union of both partition files"
    );
    let has_city = rows
        .first()
        .map(|r| r.get("city").is_some())
        .unwrap_or(false);
    assert!(
        has_city,
        "hive partition column `city` was not surfaced (lazy). row keys: {:?}",
        rows.first()
            .and_then(|r| r.as_object())
            .map(|o| o.keys().collect::<Vec<_>>())
    );
}

#[actix_web::test]
async fn arrow_sql_path_honours_page_size_above_1000() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("people.parquet");
    write_many_people(&file, 1500);
    let store = make_store(&file.display().to_string(), true).await;

    let mut req = empty_req();
    req.page_size = 1200;

    let bytes = store.query_arrow("people", &req).await.unwrap();
    let reader =
        arrow::ipc::reader::StreamReader::try_new(std::io::Cursor::new(bytes), None).unwrap();
    let rows: usize = reader.map(|batch| batch.unwrap().num_rows()).sum();
    assert_eq!(
        rows, 1200,
        "DataFusion SQL path must not clamp pages to 1000 rows"
    );
}

#[actix_web::test]
async fn arrow_sql_path_clamps_to_configured_max_page_size() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("people.parquet");
    write_many_people(&file, 1500);
    let store = make_store_with_max_page_size(&file.display().to_string(), true, 750).await;

    let mut req = empty_req();
    req.page_size = 1200;

    let bytes = store.query_arrow("people", &req).await.unwrap();
    let reader =
        arrow::ipc::reader::StreamReader::try_new(std::io::Cursor::new(bytes), None).unwrap();
    let rows: usize = reader.map(|batch| batch.unwrap().num_rows()).sum();
    assert_eq!(
        rows, 750,
        "DataFusion SQL path must clamp to server.max_page_size"
    );
}

#[actix_web::test]
async fn raw_sql_preserves_identifier_case() {
    // Parquet column names are case-sensitive. The raw-SQL endpoint must
    // match dataset & column names case-insensitively (like DuckDB) by
    // rewriting them to quoted canonical names, so `SELECT state` resolves
    // against a column literally named `State`.
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("people.parquet");

    let schema = Arc::new(Schema::new(vec![
        Field::new("State", DataType::Utf8, false),
        Field::new("id", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["CA", "CA", "NY"])),
            Arc::new(Int64Array::from(vec![1_i64, 2, 3])),
        ],
    )
    .unwrap();
    let f = std::fs::File::create(&file).unwrap();
    let mut writer = ArrowWriter::try_new(f, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();

    let store = make_store(&file.display().to_string(), true).await;

    // Lowercase `state` must match the case-sensitive Parquet column
    // `State` (the exact failure the user reported), and so must the
    // mixed-case spellings.
    for ident in ["state", "State", "STATE"] {
        let out = store
            .query_sql(
                &format!(
                    "SELECT {ident}, COUNT(*) AS n FROM people GROUP BY {ident} ORDER BY n DESC"
                ),
                100,
            )
            .await
            .unwrap_or_else(|e| panic!("`{ident}` should resolve case-insensitively: {e:?}"));
        let rows = parse_rows(&out);
        assert_eq!(rows.len(), 2, "expected one row per distinct State");
        let ca = rows
            .iter()
            .find(|r| r["State"].as_str() == Some("CA"))
            .expect("CA group present");
        assert_eq!(ca["n"], Value::from(2));
    }

    // A mixed-case table name must also resolve.
    let out = store
        .query_sql("SELECT COUNT(*) AS n FROM PEOPLE", 100)
        .await
        .expect("case-insensitive table name should resolve");
    let rows = parse_rows(&out);
    assert_eq!(rows[0]["n"], Value::from(3));
}

#[actix_web::test]
async fn arrow_stream_all_ignores_page_size() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("people.parquet");
    write_many_people(&file, 1500);
    let store = make_store_with_max_page_size(&file.display().to_string(), true, 750).await;

    let mut req = empty_req();
    req.page_size = 10;

    let stream = store.query_arrow_stream_all("people", &req).await.unwrap();
    let chunks = stream.collect::<Vec<_>>().await;
    let mut bytes = Vec::new();
    for chunk in chunks {
        bytes.extend_from_slice(&chunk.unwrap());
    }

    let reader =
        arrow::ipc::reader::StreamReader::try_new(std::io::Cursor::new(bytes), None).unwrap();
    let rows: usize = reader.map(|batch| batch.unwrap().num_rows()).sum();
    assert_eq!(rows, 1500);
}

// ---------------------------------------------------------------------------
// Parameterised predicates — values are bound as typed params, never
// interpolated into the SQL text (lazy mode always takes the SQL path).
// ---------------------------------------------------------------------------

/// A value containing a single quote must match itself exactly — proving the
/// literal is bound as data, not spliced into the query where the quote
/// would otherwise terminate the string.
#[actix_web::test]
async fn predicate_eq_value_with_quote() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("people.parquet");
    write_people(&file);
    let store = make_store(&file.display().to_string(), true).await;

    let req = req_with(vec![pred("name", "eq", Value::String("O'Brien".into()))]);
    let rows = parse_rows(&store.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 1, "exactly one row should match O'Brien");
    assert_eq!(rows[0]["name"], Value::String("O'Brien".into()));
}

/// An injection-looking value must be treated as an opaque literal: it only
/// matches the row whose `name` is literally that string, never the whole
/// table.
#[actix_web::test]
async fn predicate_injection_is_treated_as_literal() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("people.parquet");
    write_people(&file);
    let store = make_store(&file.display().to_string(), true).await;

    let inject = Value::String("' OR '1'='1".into());
    let req = req_with(vec![pred("name", "eq", inject.clone())]);
    let rows = parse_rows(&store.query("people", &req).await.unwrap());
    assert_eq!(
        rows.len(),
        1,
        "must match only the literal row, not the whole table"
    );
    assert_eq!(rows[0]["name"], inject);

    // count() shares the same parameterised path.
    let n = store
        .count(
            "people",
            &datapress_core::models::CountRequest {
                predicates: req.predicates,
            },
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
}

/// `in` binds each element as its own placeholder.
#[actix_web::test]
async fn predicate_in_binds_each_element() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("people.parquet");
    write_people(&file);
    let store = make_store(&file.display().to_string(), true).await;

    let req = req_with(vec![pred(
        "name",
        "in",
        Value::Array(vec![
            Value::String("Anna".into()),
            Value::String("Bob".into()),
        ]),
    )]);
    let rows = parse_rows(&store.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 2);
}

/// Numeric predicates bind as typed scalars and coerce against the column.
#[actix_web::test]
async fn predicate_numeric_range() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("people.parquet");
    write_people(&file);
    let store = make_store(&file.display().to_string(), true).await;

    let req = req_with(vec![pred("score", "gte", serde_json::json!(25.0))]);
    let rows = parse_rows(&store.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 2, "scores 30 and 40 are >= 25");
}

/// `having` filters groups after aggregation. The same shared
/// `having_plan` resolver and clause builder feed both backends, so this
/// confirms the DataFusion SQL dialect accepts the emitted
/// `HAVING <agg-expr> <op> ?` form.
#[actix_web::test]
async fn group_by_with_having_filters_groups() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("g.parquet");
    // Two "a" rows, one "b" row: grouping by name gives counts 2 and 1.
    write_parquet(&file, &[1, 2, 3], &["a", "a", "b"], &[10.0, 20.0, 30.0]);
    let store = make_store(&file.display().to_string(), true).await;

    // HAVING on the implicit COUNT(*) alias keeps only the "a" group.
    let mut req = empty_req();
    req.group_by = vec!["name".into()];
    req.having = vec![pred("count", "gt", serde_json::json!(1))];
    let rows = parse_rows(&store.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], Value::from("a"));
    assert_eq!(rows[0]["count"], Value::from(2));

    // HAVING on a named SUM alias: a -> 30, b -> 30; keep >= 30 returns both.
    let mut req = empty_req();
    req.group_by = vec!["name".into()];
    req.aggregations = vec![Aggregation {
        col: Some("score".into()),
        op: "sum".into(),
        alias: Some("total".into()),
    }];
    req.having = vec![pred("total", "gte", serde_json::json!(30))];
    let rows = parse_rows(&store.query("people", &req).await.unwrap());
    assert_eq!(rows.len(), 2);

    // HAVING without group_by is rejected.
    let mut req = empty_req();
    req.having = vec![pred("count", "gt", serde_json::json!(1))];
    assert!(store.query("people", &req).await.is_err());
}
