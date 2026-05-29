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
use parquet::arrow::ArrowWriter;
use serde_json::Value;
use tempfile::TempDir;

use datapress_core::config::{
    AppConfig, DatasetConfig, IndexConfig, ServerConfig, SourceConfig, SourceKind,
};
use datapress_core::models::QueryRequest;
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

/// Build a hive-partitioned dataset under `dir`:
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
    let cfg = AppConfig {
        server:   ServerConfig::default(),
        docs:     datapress_core::config::DocsConfig::default(),
        swagger:  datapress_core::config::SwaggerConfig::default(),
        auth:     datapress_core::config::AuthConfig::default(),
        datasets: vec![DatasetConfig {
            name:    "people".into(),
            source:  SourceConfig {
                kind:     SourceKind::Parquet,
                location: location.to_string(),
            },
            s3:          None,
            index:       IndexConfig::default(),
            columns:     vec![],
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
async fn hive_partition_column_eager() {
    let tmp = TempDir::new().unwrap();
    write_hive_dataset(tmp.path());
    let glob = format!("{}/city=*/*.parquet", tmp.path().display());
    let store = make_store(&glob, false).await;

    let rows = parse_rows(&store.query("people", &empty_req()).await.unwrap());
    let has_city = rows.first().map(|r| r.get("city").is_some()).unwrap_or(false);
    assert!(
        has_city,
        "hive partition column `city` was not surfaced (eager). row keys: {:?}",
        rows.first().and_then(|r| r.as_object()).map(|o| o.keys().collect::<Vec<_>>())
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
    assert_eq!(rows.len(), 5, "lazy: expected union of both partition files");
    let has_city = rows.first().map(|r| r.get("city").is_some()).unwrap_or(false);
    assert!(
        has_city,
        "hive partition column `city` was not surfaced (lazy). row keys: {:?}",
        rows.first().and_then(|r| r.as_object()).map(|o| o.keys().collect::<Vec<_>>())
    );
}
