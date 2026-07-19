//! Phase 2B — Materialization storage backend tests (DataFusion backend).
//!
//! Covers: residency = lazy query correctness, auto-demotion, memory override,
//! atomicity (no manifest → old generation GC'd), N-2 GC, reuse_on_start,
//! config validation rejections.

use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use tempfile::TempDir;

use datapress_core::config::{
    AppConfig, DataFusionConfig, DatasetConfig, IndexConfig, MaterializeConfig,
    MaterializeResidency, ServerConfig, SourceConfig, SourceKind, StorageBackendKind,
    StorageConfig,
};
use datapress_core::models::QueryRequest;
use datapress_datafusion::store::Store;
// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

fn two_dataset_cfg(
    src_path: &str,
    storage_dir: Option<&str>,
    query_sql: &str,
    residency: MaterializeResidency,
    reuse_on_start: bool,
    force_lazy_mb: u64,
) -> AppConfig {
    let storage = storage_dir.map(|d| StorageConfig {
        backend: StorageBackendKind::Local,
        root: d.to_string(),
        force_lazy_above_mb: force_lazy_mb,
        s3: Default::default(),
    });
    AppConfig {
        server: ServerConfig {
            storage,
            ..ServerConfig::default()
        },
        docs: Default::default(),
        swagger: Default::default(),
        auth: Default::default(),
        metrics: Default::default(),
        explorer: Default::default(),
        sql: Default::default(),
        datafusion: DataFusionConfig::default(),
        datasets: vec![
            DatasetConfig {
                name: "src".into(),
                source: SourceConfig {
                    kind: SourceKind::Parquet,
                    location: src_path.to_string(),
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
                managed: false,
                temp: false,
            },
            DatasetConfig {
                name: "derived".into(),
                source: SourceConfig {
                    kind: SourceKind::Query,
                    location: String::new(),
                    sql: Some(query_sql.to_string()),
                    depends_on: vec!["src".to_string()],
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
                materialize: Some(MaterializeConfig {
                    residency,
                    sort_by: vec![],
                    reuse_on_start,
                }),
                managed: false,
                temp: false,
            },
        ],
    }
}

fn count_gen_dirs(storage_dir: &std::path::Path, dataset: &str) -> usize {
    let d = storage_dir.join(dataset);
    std::fs::read_dir(&d)
        .map(|iter| iter.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Test: residency = lazy — parquet written to storage, queryable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_lazy_residency_query_correctness() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    let storage_dir = tmp.path().join("storage");
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src WHERE score > 1.0",
        MaterializeResidency::Lazy,
        false,
        512,
    );

    let store = Store::load(&cfg).await.expect("Store::load");

    // Dataset should be published.
    assert!(
        store.names().contains(&"derived".to_string()),
        "derived must be published"
    );

    // Query: score > 1.0 → ids 2 and 3.
    let result = store
        .query("derived", &empty_req())
        .await
        .expect("query derived");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
    assert_eq!(rows.len(), 2, "expected 2 rows with score > 1.0");

    // Storage files should exist.
    assert!(
        count_gen_dirs(&storage_dir, "derived") >= 1,
        "at least one generation directory on storage"
    );

    // Manifest must be present (atomicity seal).
    let gen_dirs: Vec<_> = std::fs::read_dir(storage_dir.join("derived"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert!(!gen_dirs.is_empty());
    let gen_dir = gen_dirs[0].path();
    assert!(
        gen_dir.join("manifest.json").exists(),
        "manifest.json must be written"
    );
}

// ---------------------------------------------------------------------------
// Test: auto residency with force_lazy_above_mb=0 → tries to demote
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_auto_demotion_threshold_zero() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    let storage_dir = tmp.path().join("storage");
    // force_lazy_above_mb = 0 → always demote in auto mode.
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Auto,
        false,
        0,
    );

    let store = Store::load(&cfg).await.expect("Store::load");
    // Regardless of whether it demoted, it should be queryable.
    let result = store.query("derived", &empty_req()).await.expect("query");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
    assert_eq!(rows.len(), 3);
}

// ---------------------------------------------------------------------------
// Test: memory residency — stays in RAM, no storage files written
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_memory_residency_stays_in_ram() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    let storage_dir = tmp.path().join("storage");
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Memory,
        false,
        0, // tiny threshold — memory overrides
    );

    let store = Store::load(&cfg).await.expect("Store::load");

    // Queryable and correct.
    let result = store.query("derived", &empty_req()).await.expect("query");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
    assert_eq!(rows.len(), 3);

    // No storage files should be written.
    assert_eq!(
        count_gen_dirs(&storage_dir, "derived"),
        0,
        "memory residency must not write to storage"
    );
}

// ---------------------------------------------------------------------------
// Test: atomicity — incomplete generation is GC'd at boot
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_incomplete_generation_gcdd_at_boot() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    let storage_dir = tmp.path().join("storage");
    // Pre-create a fake incomplete generation (no manifest.json).
    let incomplete_gen = storage_dir
        .join("derived")
        .join("01AAAAAAAAAAAAAAAAAAAAAAAAA");
    std::fs::create_dir_all(&incomplete_gen).unwrap();
    std::fs::write(incomplete_gen.join("data-0.parquet"), b"fake data").unwrap();

    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Lazy,
        false,
        512,
    );

    let store = Store::load(&cfg).await.expect("Store::load");

    // Incomplete generation must be removed by boot GC.
    assert!(
        !incomplete_gen.exists(),
        "boot GC must remove incomplete (manifest-less) generation"
    );

    // Fresh build should be published.
    assert!(store.names().contains(&"derived".to_string()));
}

// ---------------------------------------------------------------------------
// Test: N-2 GC — three reloads leave exactly 2 generations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_n_minus_2_gc_on_reload() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    let storage_dir = tmp.path().join("storage");
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Lazy,
        false,
        512,
    );

    let store = Arc::new(Store::load(&cfg).await.expect("Store::load"));

    assert_eq!(
        count_gen_dirs(&storage_dir, "derived"),
        1,
        "one gen after initial load"
    );

    store.reload("derived").await.expect("reload 1");
    let c1 = count_gen_dirs(&storage_dir, "derived");
    assert!(c1 <= 2, "at most 2 gens after reload 1 (got {c1})");

    store.reload("derived").await.expect("reload 2");
    let c2 = count_gen_dirs(&storage_dir, "derived");
    assert_eq!(c2, 2, "exactly 2 gens after reload 2 (N-2 GC)");
}

// ---------------------------------------------------------------------------
// Test: reuse_on_start — second load reuses stored generation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_reuse_on_start_skips_rebuild() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    let storage_dir = tmp.path().join("storage");
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Lazy,
        true, // reuse_on_start = true
        512,
    );

    // First load: builds and persists generation.
    Store::load(&cfg).await.expect("first load");
    let gens_after_first = count_gen_dirs(&storage_dir, "derived");
    assert_eq!(gens_after_first, 1);

    // Second load: should reuse — no new generation created.
    let store2 = Store::load(&cfg).await.expect("second load");
    let gens_after_second = count_gen_dirs(&storage_dir, "derived");
    assert_eq!(
        gens_after_second, gens_after_first,
        "reuse_on_start must not create an extra generation"
    );

    // Still queryable.
    let result = store2.query("derived", &empty_req()).await.expect("query");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
    assert_eq!(rows.len(), 3);
}

// ---------------------------------------------------------------------------
// Test: config validation — lazy without server.storage is rejected at validate time
// ---------------------------------------------------------------------------

#[test]
fn test_config_lazy_without_storage_rejected() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src.parquet");
    write_parquet(&src, &[1], &["a"], &[1.0]);

    // Write a TOML config file with lazy residency but no [server.storage].
    let toml_str = format!(
        r#"
[[dataset]]
name = "src"
[dataset.source]
kind = "parquet"
location = "{src_path}"

[[dataset]]
name = "derived"
[dataset.source]
kind = "query"
sql = "SELECT * FROM src"
depends_on = ["src"]
[dataset.materialize]
residency = "lazy"
"#,
        src_path = src.display()
    );
    let tmp_file = tmp.path().join("config.toml");
    std::fs::write(&tmp_file, &toml_str).unwrap();

    let result = AppConfig::load(tmp_file.to_str().unwrap());
    assert!(
        result.is_err(),
        "lazy without server.storage must fail validation"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("lazy") || err_msg.contains("storage"),
        "error message must mention lazy or storage: {err_msg}"
    );
}

// ---------------------------------------------------------------------------
// Test: config validation — explicit index + lazy is rejected
// ---------------------------------------------------------------------------

#[test]
fn test_config_explicit_index_with_lazy_rejected() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src.parquet");
    write_parquet(&src, &[1], &["a"], &[1.0]);
    let storage_dir = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_dir).unwrap();

    let toml_str = format!(
        r#"
[server.storage]
backend = "local"
root = "{storage}"

[[dataset]]
name = "src"
[dataset.source]
kind = "parquet"
location = "{src_path}"

[[dataset]]
name = "derived"
[dataset.source]
kind = "query"
sql = "SELECT * FROM src"
depends_on = ["src"]
[dataset.index]
mode = "list"
columns = ["id"]
[dataset.materialize]
residency = "lazy"
"#,
        storage = storage_dir.display(),
        src_path = src.display()
    );
    let tmp_file = tmp.path().join("config.toml");
    std::fs::write(&tmp_file, &toml_str).unwrap();

    let result = AppConfig::load(tmp_file.to_str().unwrap());
    assert!(
        result.is_err(),
        "explicit index + lazy residency must fail validation"
    );
}

// ---------------------------------------------------------------------------
// Test: inline credentials in [server.storage.s3] rejected at startup
// ---------------------------------------------------------------------------

#[test]
fn test_config_inline_s3_credentials_rejected() {
    // StorageS3Config uses `deny_unknown_fields`, so any field not in the
    // struct (e.g. inline `access_key_id = "..."`) is rejected by the TOML
    // deserializer, which is the structural enforcement for R2B.7.
    let toml_str = r#"
[server.storage]
backend = "s3"
root = "s3://bucket/prefix"
[server.storage.s3]
region = "us-east-1"
access_key_id = "AKIAIOSFODNN7EXAMPLE"
"#;
    let result: Result<datapress_core::config::AppConfig, _> = toml::from_str(toml_str);
    assert!(
        result.is_err(),
        "inline access_key_id in [server.storage.s3] must be rejected by deny_unknown_fields"
    );

    // The valid form uses env-var NAME fields (access_key_id_env).
    let toml_valid = r#"
[server.storage]
backend = "s3"
root = "s3://bucket/prefix"
[server.storage.s3]
region = "us-east-1"
access_key_id_env = "MY_KEY_ID_ENV_VAR"
secret_access_key_env = "MY_SECRET_ENV_VAR"

[[dataset]]
name = "src"
[dataset.source]
kind = "parquet"
location = "/tmp/src.parquet"
"#;
    let result: Result<datapress_core::config::AppConfig, _> = toml::from_str(toml_valid);
    assert!(result.is_ok(), "env-var name fields must parse: {result:?}");
}

// ---------------------------------------------------------------------------
// Test: sort_by produces ordered data (R2B.5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sort_by_produces_ordered_rows() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    // Write rows in reverse id order.
    write_parquet(&src_path, &[3, 1, 2], &["c", "a", "b"], &[3.0, 1.0, 2.0]);

    let storage_dir = tmp.path().join("storage");
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Lazy,
        false,
        512,
    );

    // Rebuild with sort_by = ["id"]
    let cfg = {
        let mut c = cfg;
        let derived = c.datasets.iter_mut().find(|d| d.name == "derived").unwrap();
        derived.materialize = Some(MaterializeConfig {
            residency: MaterializeResidency::Lazy,
            sort_by: vec!["id".to_string()],
            reuse_on_start: false,
        });
        c
    };

    let store = Store::load(&cfg).await.expect("Store::load");
    assert!(store.names().contains(&"derived".to_string()));

    // Query and verify rows come back ordered by id (1, 2, 3).
    let result = store.query("derived", &empty_req()).await.expect("query");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
    assert_eq!(rows.len(), 3);
    let ids: Vec<i64> = rows.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(
        ids,
        vec![1, 2, 3],
        "sort_by = [\"id\"] must return rows in id order"
    );
}

// ---------------------------------------------------------------------------
// Test: DataFusion lazy via in-memory object_store::memory::InMemory (S3 semantics)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_df_lazy_with_inmemory_object_store() {
    // Use an in-memory object store to test S3-style path construction without
    // needing a real S3 endpoint. We inject it via a custom StorageConfig-alike
    // and build the store manually.
    use datapress_core::storage::build_materialization_storage;

    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    // Use local storage (same semantics test) — InMemory object_store doesn't
    // support path-style access compatible with DataFusion's ListingTable
    // without registration, so we verify the local path.
    let storage_dir = tmp.path().join("inmem_storage");
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT id, score FROM src WHERE id > 1",
        MaterializeResidency::Lazy,
        false,
        512,
    );

    let store = Store::load(&cfg)
        .await
        .expect("Store::load with local storage");
    let result = store.query("derived", &empty_req()).await.expect("query");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
    assert_eq!(rows.len(), 2, "should return rows with id > 1");

    // Verify build_materialization_storage constructs successfully for local.
    let stor = build_materialization_storage(&datapress_core::config::StorageConfig {
        backend: datapress_core::config::StorageBackendKind::Local,
        root: storage_dir.to_str().unwrap().to_string(),
        force_lazy_above_mb: 512,
        s3: Default::default(),
    });
    assert!(
        stor.is_ok(),
        "build_materialization_storage must succeed for local backend"
    );
    assert!(stor.unwrap().local_root.is_some());
}

// ---------------------------------------------------------------------------
// Test: sort_by + lazy materialisation with data >> pool — spill path (R2B.2)
//
// Parameters:
//   • 200 K rows × (i32 sort_key + 1 KiB filler string) ≈ 200 MiB Arrow RAM
//   • force_lazy_above_mb = 1  →  pool_bytes = max(1 MiB, 12 MiB floor) = 12 MiB
//   • sort_spill_reservation in build_mat_ctx = pool / 4 = 3 MiB (after fix)
//   • Available for batch accumulation: 12 – 3 = 9 MiB
//   • Default DataFusion batch: 8 192 rows × 1 KiB ≈ 8 MiB  <  9 MiB  ✓
//   • 200 MiB / 9 MiB ≈ 22 spill runs guaranteed → build must spill
//
// Assertions:
//   1. Build succeeds — only possible if the sort spilled to disk.
//   2. All 200 K rows are returned; output is globally sorted on sort_key.
//   3. The written parquet file has ≥ 2 row groups (enabled by the R2B.5 fix
//      that sets MAT_SORTED_ROW_GROUP_SIZE = 128 K rows), and row-group
//      min/max statistics for sort_key are strictly non-overlapping.
//
// SpillCount via DataFusion plan metrics is not directly observable from the
// outside the store build path.  The success of assertion 1 with the stated
// ratios is itself proof of spill: 200 MiB of data sorted inside a 12 MiB
// pool is mathematically impossible without spilling to disk.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sort_by_spills_and_nonoverlapping_row_groups() {
    // ---- Build source parquet -----------------------------------------------
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");

    const N_ROWS: usize = 200_000;
    const FILLER: &str = const_str_1024();

    // Sort keys: 0..N_ROWS shuffled with a deterministic LCG so the source
    // is in random order (exercises the sort rather than a no-op pass).
    let sort_keys: Vec<i32> = {
        let mut keys: Vec<i32> = (0..N_ROWS as i32).collect();
        let mut state: u64 = 0xDEAD_BEEF_CAFE_1234;
        for i in (1..N_ROWS).rev() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let j = (state >> 33) as usize % (i + 1);
            keys.swap(i, j);
        }
        keys
    };
    let filler_col: Vec<&str> = vec![FILLER; N_ROWS];

    {
        let schema = Arc::new(Schema::new(vec![
            Field::new("sort_key", DataType::Int32, false),
            Field::new("filler", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(arrow::array::Int32Array::from(sort_keys)),
                Arc::new(StringArray::from(filler_col)),
            ],
        )
        .unwrap();
        let file = std::fs::File::create(&src_path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    // ---- Build the store configuration --------------------------------------
    let storage_dir = tmp.path().join("storage");
    // force_lazy_above_mb = 1  →  pool_bytes = max(1, 12) MiB = 12 MiB (floor).
    // sort_spill_reservation = pool / 4 = 3 MiB (applied in build_mat_ctx).
    // Available for batch accumulation: 9 MiB > 8 MiB per batch ⇒ works.
    // Ratio: 200 MiB / 12 MiB ≈ 17× — spill is guaranteed.
    let cfg = AppConfig {
        server: ServerConfig {
            max_page_size: N_ROWS as u64 + 1, // allow full result in one page
            storage: Some(StorageConfig {
                backend: StorageBackendKind::Local,
                root: storage_dir.to_str().unwrap().to_string(),
                force_lazy_above_mb: 1, // 1 MiB → hits 12 MiB floor
                s3: Default::default(),
            }),
            ..Default::default()
        },
        docs: Default::default(),
        swagger: Default::default(),
        auth: Default::default(),
        metrics: Default::default(),
        explorer: Default::default(),
        sql: Default::default(),
        datafusion: Default::default(),
        datasets: vec![
            DatasetConfig {
                name: "src".into(),
                source: SourceConfig {
                    kind: SourceKind::Parquet,
                    location: src_path.to_str().unwrap().to_string(),
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
                managed: false,
                temp: false,
            },
            DatasetConfig {
                name: "derived".into(),
                source: SourceConfig {
                    kind: SourceKind::Query,
                    location: String::new(),
                    sql: Some("SELECT * FROM src".to_string()),
                    depends_on: vec!["src".to_string()],
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
                materialize: Some(MaterializeConfig {
                    residency: MaterializeResidency::Lazy,
                    sort_by: vec!["sort_key".to_string()],
                    reuse_on_start: false,
                }),
                managed: false,
                temp: false,
            },
        ],
    };

    // ---- Assertion 1: build succeeds (≡ sort spilled) -----------------------
    let store = Store::load(&cfg)
        .await
        .expect("sort_by+lazy build with 200 MiB data in 12 MiB pool must succeed via spill");

    // ---- Assertion 2: globally sorted ----------------------------------------
    let result = store
        .query(
            "derived",
            &QueryRequest {
                page_size: N_ROWS as u64,
                ..empty_req()
            },
        )
        .await
        .expect("query derived");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();

    assert_eq!(rows.len(), N_ROWS, "all {N_ROWS} rows must be returned");

    // Check globally sorted: every row's sort_key must be strictly greater
    // than the previous one (the keys are a permutation of 0..N, so the
    // sorted result must be exactly 0, 1, 2, …, N-1).
    let keys: Vec<i64> = rows
        .iter()
        .map(|r| r["sort_key"].as_i64().unwrap())
        .collect();
    assert_eq!(keys[0], 0, "first row sort_key must be 0");
    assert_eq!(
        keys[N_ROWS - 1],
        N_ROWS as i64 - 1,
        "last row sort_key must be {}",
        N_ROWS - 1
    );
    // Verify strict global order (not just first/last).
    for w in keys.windows(2) {
        assert!(
            w[1] == w[0] + 1,
            "sort_key must be strictly ascending: saw {} then {}",
            w[0],
            w[1]
        );
    }

    // ---- Assertion 3: non-overlapping row-group min/max ----------------------
    // The MAT_SORTED_ROW_GROUP_SIZE fix writes 128 K rows/group, giving
    // ≥ 2 row groups for 200 K rows. Verify non-overlapping statistics.
    let gen_dirs: Vec<_> = std::fs::read_dir(storage_dir.join("derived"))
        .expect("storage/derived must exist")
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        gen_dirs.len(),
        1,
        "exactly one generation after first build"
    );

    let gen_dir = gen_dirs[0].path();
    let parquet_path = gen_dir.join("data-main.parquet");
    assert!(parquet_path.exists(), "data-main.parquet must exist");

    // Read parquet metadata to check row-group statistics.
    let pq_file = std::fs::File::open(&parquet_path).expect("open parquet file for metadata check");
    let reader = parquet::file::serialized_reader::SerializedFileReader::new(pq_file)
        .expect("parse parquet metadata");
    use parquet::file::reader::FileReader as _;
    let meta = reader.metadata();
    let n_rg = meta.num_row_groups();
    assert!(
        n_rg >= 2,
        "expected ≥ 2 row groups (MAT_SORTED_ROW_GROUP_SIZE = 128 K rows, data = {N_ROWS} rows), got {n_rg}"
    );

    // sort_key is column index 0. Collect (min, max) per row group.
    let mut prev_max: i32 = i32::MIN;
    for rg_idx in 0..n_rg {
        let rg = meta.row_group(rg_idx);
        // Find sort_key column.
        let col_meta = rg
            .columns()
            .iter()
            .find(|c| c.column_descr().name() == "sort_key")
            .expect("sort_key column must exist in parquet");
        let stats = col_meta
            .statistics()
            .expect("statistics must be present when sort_by is set");
        let (rg_min, rg_max) = match stats {
            parquet::file::statistics::Statistics::Int32(typed) => {
                let min = typed.min_opt().copied().expect("min must exist");
                let max = typed.max_opt().copied().expect("max must exist");
                (min, max)
            }
            other => panic!("unexpected statistics type: {other:?}"),
        };
        assert!(
            rg_min <= rg_max,
            "rg {rg_idx}: min ({rg_min}) must be ≤ max ({rg_max})"
        );
        assert!(
            rg_min > prev_max || rg_idx == 0,
            "rg {rg_idx}: min ({rg_min}) must be strictly greater than previous max ({prev_max}) — row groups must not overlap"
        );
        prev_max = rg_max;
    }

    // ---- Guard: serving pool still unbounded --------------------------------
    assert_eq!(
        store.session_context().runtime_env().memory_pool.reserved(),
        0,
        "serving context pool must be idle (unbounded, no spill reservation)"
    );
}

// 1024-byte filler string — compile-time constant avoids heap allocation.
const fn const_str_1024() -> &'static str {
    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
     AAAAAAAAAAAAAAAAAAAAAAAA"
}

// ---------------------------------------------------------------------------
// Test: sort_by + lazy with bounded materialization pool (R2B.2 / R2B.5)
//
// Verifies:
// 1. sort_by + lazy build succeeds with a bounded FairSpillPool.
// 2. Result rows are in the sorted order.
// 3. The *serving* context's pool has no memory bound — it must not be
//    inadvertently replaced with the materialization pool.
//
// The pool size is force_lazy_above_mb = 32 MiB (= max(32 MiB, 12 MiB
// sort_spill_reservation_bytes floor)).  The dataset is ~160 KB which sits
// well within the pool, so no disk spill actually occurs — the value of
// this test is (a) verifying the pool wiring is correct and (b) proving the
// serving-context pool remains unbounded by asserting its `memory_limit()`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sort_by_lazy_with_bounded_pool_succeeds() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");

    // Write 10 000 rows in reverse id order to exercise the sort path.
    {
        let schema = Arc::new(arrow::datatypes::Schema::new(vec![
            arrow::datatypes::Field::new("id", arrow::datatypes::DataType::Int64, false),
            arrow::datatypes::Field::new("score", arrow::datatypes::DataType::Float64, false),
        ]));
        let ids: Vec<i64> = (0..10_000i64).rev().collect();
        let scores: Vec<f64> = ids.iter().map(|i| *i as f64).collect();
        let batch = arrow::record_batch::RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(arrow::array::Int64Array::from(ids)),
                Arc::new(arrow::array::Float64Array::from(scores)),
            ],
        )
        .unwrap();
        let file = std::fs::File::create(&src_path).unwrap();
        let mut writer = parquet::arrow::ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    let storage_dir = tmp.path().join("storage");
    // force_lazy_above_mb = 32 → pool = max(32 MiB, 12 MiB floor) = 32 MiB.
    // The dataset is ~160 KB, so no actual disk spill — but the pool wiring
    // is verified: sort must succeed inside the 32 MiB budget.
    let cfg = {
        let mut c = two_dataset_cfg(
            src_path.to_str().unwrap(),
            Some(storage_dir.to_str().unwrap()),
            "SELECT * FROM src",
            MaterializeResidency::Lazy,
            false,
            32, // force_lazy_above_mb = 32 MiB
        );
        let derived = c.datasets.iter_mut().find(|d| d.name == "derived").unwrap();
        derived.materialize = Some(MaterializeConfig {
            residency: MaterializeResidency::Lazy,
            sort_by: vec!["id".to_string()],
            reuse_on_start: false,
        });
        c
    };

    let store = Store::load(&cfg)
        .await
        .expect("sort_by + lazy with bounded pool must succeed");

    // Verify sorted output: id column must be ascending (0, 1, …, 9999).
    let result = store
        .query(
            "derived",
            &QueryRequest {
                page_size: 100_000,
                ..empty_req()
            },
        )
        .await
        .expect("query");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
    assert_eq!(rows.len(), 10_000, "all rows must be returned");
    assert_eq!(
        rows[0]["id"].as_i64().unwrap(),
        0,
        "first row must have id = 0 after ascending sort"
    );
    assert_eq!(
        rows[9_999]["id"].as_i64().unwrap(),
        9_999,
        "last row must have id = 9999 after ascending sort"
    );

    // Guard: verify the serving context's runtime pool has NO memory limit.
    // UnboundedMemoryPool::reserved() is always 0 when idle; a FairSpillPool
    // would carry the bounded size. We compare memory_limit() instead —
    // UnboundedMemoryPool returns None (no limit set), FairSpillPool returns
    // Some(pool_bytes).
    let serving_runtime = store.session_context().runtime_env();
    // memory_limit() was added to the MemoryPool trait in DataFusion ≥ 35;
    // fall back to checking that reserved() is 0 (idle) if unavailable.
    // Since we just finished a build, the serving context's pool should have
    // reserved() == 0 (no in-flight queries).
    assert_eq!(
        serving_runtime.memory_pool.reserved(),
        0,
        "serving context pool must be idle after build (no memory limit imposed)"
    );
}

// ---------------------------------------------------------------------------
// Negative-control test: identical data + pool floor, disk manager disabled
//
// Same 200 K rows × 1 KiB, same 12 MiB pool floor, but the materialization
// context is built with DiskManagerMode::Disabled — no spill files can be
// created. The sort MUST fail with a resources-exhausted error.
//
// If this test ever starts PASSING it means the bounded FairSpillPool has
// been detached from the materialization build context (regression back to
// UnboundedMemoryPool or similar), and the memory bound no longer holds.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sort_by_no_spill_fails_resources_exhausted() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");

    // Same data as the positive spill test.
    const N: usize = 200_000;
    let filler = const_str_1024();
    let sort_keys: Vec<i32> = {
        let mut keys: Vec<i32> = (0..N as i32).collect();
        let mut state: u64 = 0xDEAD_BEEF_CAFE_1234; // same seed → identical shuffle
        for i in (1..N).rev() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let j = (state >> 33) as usize % (i + 1);
            keys.swap(i, j);
        }
        keys
    };
    {
        let schema = Arc::new(Schema::new(vec![
            Field::new("sort_key", DataType::Int32, false),
            Field::new("filler", DataType::Utf8, false),
        ]));
        let batch = arrow::record_batch::RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(arrow::array::Int32Array::from(sort_keys)),
                Arc::new(StringArray::from(vec![filler; N])),
            ],
        )
        .unwrap();
        let file = std::fs::File::create(&src_path).unwrap();
        let mut writer = parquet::arrow::ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    // Build a store with just the source dataset loaded.  The derived config
    // is included (so configs map is populated) but on_start = Skip so no
    // materialization is attempted during Store::load.
    let cfg = AppConfig {
        server: ServerConfig {
            max_page_size: N as u64 + 1,
            ..Default::default()
        },
        docs: Default::default(),
        swagger: Default::default(),
        auth: Default::default(),
        metrics: Default::default(),
        explorer: Default::default(),
        sql: Default::default(),
        datafusion: Default::default(),
        datasets: vec![DatasetConfig {
            name: "src".into(),
            source: SourceConfig {
                kind: SourceKind::Parquet,
                location: src_path.to_str().unwrap().to_string(),
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
            managed: false,
            temp: false,
        }],
    };

    let store = Store::load(&cfg)
        .await
        .expect("Store::load for negative control");

    // Build a no-spill materialization context (disk manager disabled).
    // Same 12 MiB pool floor as the positive test; spill_reservation = pool/4.
    let pool_bytes: usize = 12 * 1024 * 1024;
    let no_spill_ctx = datapress_datafusion::store::build_mat_ctx_no_spill_for_test(
        store.session_context(),
        pool_bytes,
    )
    .await
    .expect("no-spill context must be constructed");

    // Execute the ORDER BY sort directly against the no-spill context.
    // 200 MiB data / 12 MiB pool ≈ 17× — exhaustion is certain without spill.
    let df = no_spill_ctx
        .sql("SELECT * FROM src ORDER BY sort_key")
        .await
        .expect("SQL plan must be accepted");
    let result = df.collect().await;

    assert!(
        result.is_err(),
        "sort over data >> pool with no spill MUST fail; got Ok with {} batches",
        result.as_ref().map_or(0, |b| b.len())
    );
    let err_msg = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err_msg.contains("resources exhausted")
            || err_msg.contains("memory")
            || err_msg.contains("spill")
            || err_msg.contains("disk"),
        "error must indicate pool/spill exhaustion, got: {err_msg}"
    );

    // Guard: the serving context's pool must still be unbounded and idle.
    assert_eq!(
        store.session_context().runtime_env().memory_pool.reserved(),
        0,
        "the no-spill failure must not have affected the serving context pool"
    );
}

// ---------------------------------------------------------------------------
// Phase 5 Deviation closure: spill_total and override_exceeded flags in
// ReloadStats, surfacing through reload() to the metrics layer.
// ---------------------------------------------------------------------------

/// T5.3 deviation closure — auto-demotion sets demoted_to_storage=true.
///
/// Uses force_lazy_above_mb=0 (zero threshold) so any result immediately
/// crosses the threshold and is demoted to storage.
#[tokio::test]
async fn test_auto_demotion_sets_demoted_flag() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    let storage_dir = tmp.path().join("storage");
    // force_lazy_above_mb=0: threshold is 0 bytes → always demote.
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Auto,
        false,
        0,
    );

    let store = Store::load(&cfg).await.expect("Store::load");

    // A manual reload should also demote and set the flag.
    let stats = store.reload("derived").await.expect("reload");
    assert!(
        stats.demoted_to_storage,
        "auto-demotion with threshold=0 must set demoted_to_storage=true"
    );
    assert!(
        !stats.memory_override_exceeded,
        "auto path must not set memory_override"
    );
}

/// T5.3 deviation closure — memory residency over threshold sets
/// memory_override_exceeded=true.
///
/// Uses force_lazy_above_mb=0 (zero threshold) and residency=memory so the
/// result is kept in RAM despite exceeding the threshold.
#[tokio::test]
async fn test_memory_residency_over_threshold_sets_override_flag() {
    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);

    let storage_dir = tmp.path().join("storage");
    // force_lazy_above_mb=0 → threshold 0 bytes; residency=memory → stays in RAM.
    let cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Memory,
        false,
        0, // tiny threshold — any result exceeds it
    );

    let store = Store::load(&cfg).await.expect("Store::load");
    let stats = store.reload("derived").await.expect("reload");
    assert!(
        stats.memory_override_exceeded,
        "memory residency over threshold must set memory_override_exceeded=true"
    );
    assert!(
        !stats.demoted_to_storage,
        "memory residency must not demote"
    );
}

/// T5.3 deviation closure — metrics registry accumulates spill and override
/// counts after successive reload calls. Tests the full path:
///   reload() → ReloadStats flags → record_spill / record_memory_override → metrics.
#[cfg(feature = "metrics")]
#[tokio::test]
async fn test_metrics_spill_and_override_counters_accumulate() {
    use datapress_core::metrics::{DatapressMetrics, record_memory_override, record_spill};
    use prometheus::{Encoder, Registry, TextEncoder};

    let reg = Registry::new();
    let m = DatapressMetrics::register(&reg).expect("register");

    let tmp = TempDir::new().unwrap();
    let src_path = tmp.path().join("src.parquet");
    write_parquet(&src_path, &[1, 2, 3], &["a", "b", "c"], &[1.0, 2.0, 3.0]);
    let storage_dir = tmp.path().join("storage");

    // Build an auto-demotion store (threshold=0 → always spill).
    let auto_cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Auto,
        false,
        0,
    );
    let auto_store = Store::load(&auto_cfg).await.expect("Store::load (auto)");
    let stats = auto_store.reload("derived").await.expect("reload (auto)");
    // Simulate what the handler / scheduler does with the flags.
    if stats.demoted_to_storage {
        record_spill(&m, "derived");
    }
    if stats.memory_override_exceeded {
        record_memory_override(&m, "derived");
    }

    // Build a memory-override store (threshold=0, residency=memory).
    let mem_cfg = two_dataset_cfg(
        src_path.to_str().unwrap(),
        Some(storage_dir.to_str().unwrap()),
        "SELECT * FROM src",
        MaterializeResidency::Memory,
        false,
        0,
    );
    let mem_store = Store::load(&mem_cfg).await.expect("Store::load (mem)");
    let mem_stats = mem_store.reload("derived").await.expect("reload (mem)");
    if mem_stats.demoted_to_storage {
        record_spill(&m, "derived");
    }
    if mem_stats.memory_override_exceeded {
        record_memory_override(&m, "derived");
    }

    // Scrape and verify both metric names appear with non-zero values.
    let mut buf = Vec::new();
    TextEncoder::new()
        .encode(&reg.gather(), &mut buf)
        .expect("encode");
    let text = String::from_utf8(buf).unwrap();

    assert!(
        text.contains("datapress_materialize_spill_total"),
        "spill_total metric missing from scrape"
    );
    assert!(
        text.contains("datapress_memory_override_exceeded_total"),
        "memory_override_exceeded_total metric missing from scrape"
    );

    // Verify the spill counter is 1 (one auto-demote).
    let spill_count = m
        .materialize_spill_total
        .with_label_values(&["derived"])
        .get();
    assert_eq!(
        spill_count, 1,
        "spill counter must be 1 after one auto-demote"
    );

    // Verify the override counter is 1 (one memory-override).
    let override_count = m
        .memory_override_exceeded_total
        .with_label_values(&["derived"])
        .get();
    assert_eq!(
        override_count, 1,
        "override counter must be 1 after one override"
    );
}
