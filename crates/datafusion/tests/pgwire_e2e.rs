//! End-to-end tests for the PostgreSQL wire-protocol server.
//!
//! These exercise the real `serve_pgwire` entry point over a loopback TCP
//! socket using the `tokio-postgres` client — the same driver Power BI /
//! psql / JDBC clients speak underneath. Two scenarios are covered:
//!
//!   1. Cleartext password auth (no TLS): simple queries, typed column
//!      round-trips, an extended-protocol prepared statement with `$1`,
//!      session-maintenance statements (`DISCARD ALL` & friends) that pooling
//!      drivers issue on connection reset, and a proof that the *wrong*
//!      password is rejected.
//!   2. Password auth over TLS with a self-signed cert generated at runtime.
//!
//! The whole file is gated on the `pgwire` feature so the default build (and
//! the crates that don't opt in) never pull in the server or its test-only
//! client dependencies.
#![cfg(feature = "pgwire")]

use std::net::{IpAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use arrow::array::{Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use tempfile::TempDir;

use datapress_core::config::{
    AppConfig, DatasetConfig, IndexConfig, PgwireConfig, ServerConfig, SourceConfig, SourceKind,
};
use datapress_datafusion::pgwire::{serve_pgwire, spawn_pgwire};
use datapress_datafusion::store::Store;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const USER: &str = "datapress";
const PASSWORD: &str = "s3cr3t-pw";

/// Write a small `id|name` parquet file: (1,"Anna"), (3,"Cara"), (4,"Dan").
fn write_people_parquet(path: &std::path::Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![1_i64, 3, 4])),
            Arc::new(StringArray::from(vec!["Anna", "Cara", "Dan"])),
        ],
    )
    .unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

/// Build a `Store` over a single-file `people` parquet dataset.
async fn make_people_store(location: &str) -> Store {
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
        }],
    };
    Store::load(&cfg).await.expect("Store::load")
}

/// Register an in-memory `types_fixture` table on the store's shared context
/// covering a spread of Arrow types — including a `Utf8View` column (`s`), the
/// exact type DataFusion infers for Parquet string columns and the one that
/// used to leak the Arrow name `"Utf8View"` into `information_schema.columns`.
/// Registering directly on the context (rather than via Parquet) lets us pin
/// `Utf8View` deterministically without depending on Parquet view inference.
fn register_types_fixture(store: &Store) {
    use arrow::array::{
        BooleanArray, Date32Array, Float32Array, Float64Array, Int16Array, Int32Array,
        StringViewArray, TimestampMicrosecondArray,
    };
    use arrow::datatypes::TimeUnit;
    use datafusion::datasource::MemTable;

    let schema = Arc::new(Schema::new(vec![
        Field::new("s", DataType::Utf8View, true),
        Field::new("t", DataType::Utf8, true),
        Field::new("b", DataType::Boolean, true),
        Field::new("i2", DataType::Int16, true),
        Field::new("i4", DataType::Int32, true),
        Field::new("i8", DataType::Int64, true),
        Field::new("f4", DataType::Float32, true),
        Field::new("f8", DataType::Float64, true),
        Field::new("ts", DataType::Timestamp(TimeUnit::Microsecond, None), true),
        Field::new("d", DataType::Date32, true),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringViewArray::from(vec!["CA"])),
            Arc::new(StringArray::from(vec!["text-val"])),
            Arc::new(BooleanArray::from(vec![true])),
            Arc::new(Int16Array::from(vec![7_i16])),
            Arc::new(Int32Array::from(vec![42_i32])),
            Arc::new(Int64Array::from(vec![99_i64])),
            Arc::new(Float32Array::from(vec![1.5_f32])),
            Arc::new(Float64Array::from(vec![2.5_f64])),
            Arc::new(TimestampMicrosecondArray::from(vec![
                1_700_000_000_000_000_i64,
            ])),
            Arc::new(Date32Array::from(vec![19_000_i32])),
        ],
    )
    .unwrap();
    let table = MemTable::try_new(schema, vec![vec![batch]]).unwrap();
    store
        .session_context()
        .register_table("types_fixture", Arc::new(table))
        .expect("register types_fixture");
}

/// Grab an ephemeral loopback port by binding then immediately releasing it.
/// There's an inherent (tiny) race between release and the pgwire server
/// re-binding, but it's more than good enough for a local test.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn loopback() -> IpAddr {
    IpAddr::from([127, 0, 0, 1])
}

// ---------------------------------------------------------------------------
// Test 1: cleartext password auth (no TLS)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pgwire_password_auth_queries() {
    let dir = TempDir::new().unwrap();
    let parquet = dir.path().join("people.parquet");
    write_people_parquet(&parquet);
    let store = make_people_store(parquet.to_str().unwrap()).await;

    let port = free_port();
    let cfg = PgwireConfig {
        enabled: true,
        listen: loopback(),
        port,
        username: USER.into(),
        password: Some(PASSWORD.into()),
        tls_cert: None,
        tls_key: None,
    };
    let ctx = store.session_context().clone();
    let server = tokio::spawn(async move {
        let _ = serve_pgwire(ctx, cfg).await;
    });

    // Connect with the correct password, retrying until the listener is up.
    let good =
        format!("host=127.0.0.1 port={port} user={USER} password={PASSWORD} dbname=datapress");
    let client = {
        let mut attempt = None;
        for _ in 0..50 {
            match tokio_postgres::connect(&good, tokio_postgres::NoTls).await {
                Ok((client, connection)) => {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    attempt = Some(client);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        attempt.expect("pgwire server did not become reachable")
    };

    // (a) aggregate — count(*) comes back as pg int8 / i64.
    let row = client
        .query_one("SELECT count(*) AS c FROM people", &[])
        .await
        .expect("count query");
    let c: i64 = row.get("c");
    assert_eq!(c, 3);

    // (b) typed column round-trip (int8 + text).
    let rows = client
        .query("SELECT id, name FROM people ORDER BY id", &[])
        .await
        .expect("select query");
    assert_eq!(rows.len(), 3);
    let id0: i64 = rows[0].get("id");
    let name0: &str = rows[0].get("name");
    assert_eq!(id0, 1);
    assert_eq!(name0, "Anna");

    // (c) extended protocol — prepared statement with a bound `$1` param.
    let rows = client
        .query("SELECT name FROM people WHERE id = $1", &[&3_i64])
        .await
        .expect("prepared query");
    assert_eq!(rows.len(), 1);
    let name: &str = rows[0].get("name");
    assert_eq!(name, "Cara");

    // (d) introspection canaries — cheap proof that `setup_pg_catalog` ran and
    // both `pg_catalog` and `information_schema` are queryable, which is what
    // psql `\dt`/`\d` and BI navigators (DBeaver, Power BI/Npgsql) rely on.
    //
    // pg_catalog.pg_type must exist and be non-empty (the builtin type rows).
    let row = client
        .query_one("SELECT count(*) AS c FROM pg_catalog.pg_type", &[])
        .await
        .expect("pg_catalog.pg_type query");
    let pg_type_rows: i64 = row.get("c");
    assert!(
        pg_type_rows > 0,
        "pg_catalog.pg_type should be populated, got {pg_type_rows}"
    );

    // information_schema.tables must list the registered `people` dataset.
    let rows = client
        .query("SELECT table_name FROM information_schema.tables", &[])
        .await
        .expect("information_schema.tables query");
    let table_names: Vec<String> = rows
        .iter()
        .map(|r| r.get::<_, String>("table_name"))
        .collect();
    assert!(
        table_names.iter().any(|t| t == "people"),
        "information_schema.tables should include 'people', got {table_names:?}"
    );

    // current_schema() must resolve (library UDF wins on the pgwire path) and
    // return the default schema name.
    let row = client
        .query_one("SELECT current_schema() AS s", &[])
        .await
        .expect("current_schema query");
    let schema: &str = row.get("s");
    assert_eq!(schema, "public");

    // (d1) constraint views — Power BI / Npgsql query
    // `information_schema.table_constraints` (and, right after,
    // `key_column_usage` / `referential_constraints`) when loading a table.
    // DataFusion's built-in `information_schema` implements none of them, so an
    // unpatched server fails the table load with "table
    // 'information_schema.table_constraints' not found". Our provider serves
    // them empty and correctly-shaped (datasets have no declared constraints,
    // so zero rows is truthful). Each must return 0 rows, not error.
    for view in [
        "table_constraints",
        "key_column_usage",
        "referential_constraints",
    ] {
        let row = client
            .query_one(
                &format!("SELECT count(*) AS n FROM information_schema.{view}"),
                &[],
            )
            .await
            .unwrap_or_else(|e| panic!("information_schema.{view} must be queryable: {e}"));
        let n: i64 = row.get("n");
        assert_eq!(
            n, 0,
            "information_schema.{view} must be empty, got {n} rows"
        );
    }

    // (d2) Npgsql type-load query — the verbatim SQL Npgsql 4.x sends on
    // `Open()` to populate its type map. The `datafusion-pg-catalog` emulation
    // links `pg_type.typnamespace`/`typreceive` to `pg_namespace`/`pg_proc` in a
    // way that makes both inner joins match nothing, so unrepaired this returns
    // ZERO rows — which Npgsql accepts silently and then blows up on the first
    // result set ("type currently unknown to Npgsql"). Our `NpgsqlTypeLoadHook`
    // rewrites it at parse time. A silent-empty catalog result is worse than an
    // error, so this asserts the base types come back and makes a regression
    // loud. Sent via `query` (extended protocol) — the path the hook repairs.
    const NPGSQL_TYPE_LOAD: &str = "SELECT ns.nspname, a.typname, a.oid, a.typrelid, a.typbasetype, CASE WHEN pg_proc.proname = 'array_recv' THEN 'a' ELSE a.typtype END AS type, CASE WHEN pg_proc.proname = 'array_recv' THEN a.typelem WHEN a.typtype = 'r' THEN rngsubtype ELSE 0 END AS elemoid, CASE WHEN pg_proc.proname IN ('array_recv', 'oidvectorrecv') THEN 3 WHEN a.typtype = 'r' THEN 2 WHEN a.typtype = 'd' THEN 1 ELSE 0 END AS ord FROM pg_catalog.pg_type AS a JOIN pg_catalog.pg_namespace AS ns ON (ns.oid = a.typnamespace) JOIN pg_catalog.pg_proc ON pg_proc.oid = a.typreceive LEFT OUTER JOIN pg_catalog.pg_class AS cls ON (cls.oid = a.typrelid) LEFT OUTER JOIN pg_catalog.pg_type AS b ON (b.oid = a.typelem) LEFT OUTER JOIN pg_catalog.pg_class AS elemcls ON (elemcls.oid = b.typrelid) LEFT OUTER JOIN pg_catalog.pg_range ON (pg_range.rngtypid = a.oid) WHERE a.typtype IN ('b', 'r', 'e', 'd') OR (a.typtype = 'c' AND cls.relkind = 'c') OR (pg_proc.proname = 'array_recv' AND (b.typtype IN ('b', 'r', 'e', 'd') OR (b.typtype = 'p' AND b.typname IN ('record', 'void')) OR (b.typtype = 'c' AND elemcls.relkind = 'c'))) OR (a.typtype = 'p' AND a.typname IN ('record', 'void')) ORDER BY ord";
    let rows = client
        .query(NPGSQL_TYPE_LOAD, &[])
        .await
        .expect("Npgsql type-load query");
    let typnames: Vec<String> = rows.iter().map(|r| r.get::<_, String>("typname")).collect();
    assert!(
        !typnames.is_empty(),
        "Npgsql type-load query must not return an empty result set"
    );
    for want in ["text", "int4", "bool", "int8", "float8", "varchar"] {
        assert!(
            typnames.iter().any(|t| t == want),
            "Npgsql type-load result must include base type '{want}', got {typnames:?}"
        );
    }
    // The array types must resolve too (their `type`='a' with `elemoid` set is
    // what lets Npgsql read array columns) — a proxy that the rewrite kept the
    // elem/receive linkage intact, not just the scalar rows.
    assert!(
        typnames.iter().any(|t| t == "_int4"),
        "Npgsql type-load result must include array type '_int4', got {typnames:?}"
    );

    // (e) session-maintenance statements — pooling drivers (Npgsql/Power BI)
    // issue `DISCARD ALL` (and friends) when resetting a pooled connection.
    // DataFusion can't plan them, so without our interception hook they'd fail
    // with `XX000: Unsupported SQL statement`. Each must succeed over the simple
    // protocol (`batch_execute`), and the connection must stay usable after.
    for stmt in ["DISCARD ALL", "RESET ALL", "DEALLOCATE ALL", "UNLISTEN *"] {
        client
            .batch_execute(stmt)
            .await
            .unwrap_or_else(|e| panic!("session-maintenance statement `{stmt}` failed: {e}"));
    }

    // The connection still works after the resets.
    let row = client
        .query_one("SELECT count(*) AS c FROM people", &[])
        .await
        .expect("query after session reset");
    let c: i64 = row.get("c");
    assert_eq!(c, 3);

    // Negative: a statement that merely *contains* DISCARD ALL as a string
    // literal must NOT be swallowed — it must run and return the literal,
    // proving the hook matches on the parsed statement, not a substring.
    let row = client
        .query_one("SELECT 'DISCARD ALL' AS s", &[])
        .await
        .expect("string-literal query");
    let literal: &str = row.get("s");
    assert_eq!(literal, "DISCARD ALL");

    // The wrong password must be rejected outright.
    let bad = format!("host=127.0.0.1 port={port} user={USER} password=wrong-pw dbname=datapress");
    let denied = tokio_postgres::connect(&bad, tokio_postgres::NoTls).await;
    assert!(
        denied.is_err(),
        "connection with an incorrect password must be rejected"
    );

    server.abort();
    drop(store);
}

// ---------------------------------------------------------------------------
// Test 2: password auth over TLS (self-signed cert)
// ---------------------------------------------------------------------------

/// A `ServerCertVerifier` that accepts anything — the test uses a throwaway
/// self-signed cert, so we deliberately skip validation client-side.
#[derive(Debug)]
struct NoCertVerify;

impl rustls::client::danger::ServerCertVerifier for NoCertVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pgwire_password_auth_over_tls() {
    let dir = TempDir::new().unwrap();
    let parquet = dir.path().join("people.parquet");
    write_people_parquet(&parquet);
    let store = make_people_store(parquet.to_str().unwrap()).await;

    // Self-signed cert + PKCS8 key written to disk for the server to load.
    let issued =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let cert_path = dir.path().join("server.crt");
    let key_path = dir.path().join("server.key");
    std::fs::write(&cert_path, issued.cert.pem()).unwrap();
    std::fs::write(&key_path, issued.key_pair.serialize_pem()).unwrap();

    let port = free_port();
    let cfg = PgwireConfig {
        enabled: true,
        listen: loopback(),
        port,
        username: USER.into(),
        password: Some(PASSWORD.into()),
        tls_cert: Some(cert_path),
        tls_key: Some(key_path),
    };
    let ctx = store.session_context().clone();
    let server = tokio::spawn(async move {
        let _ = serve_pgwire(ctx, cfg).await;
    });

    // Client TLS config that trusts the throwaway cert.
    let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoCertVerify))
    .with_no_client_auth();
    let tls = tokio_postgres_rustls::MakeRustlsConnect::new(tls_config);

    let conn_str = format!(
        "host=127.0.0.1 port={port} user={USER} password={PASSWORD} dbname=datapress sslmode=require"
    );
    let client = {
        let mut attempt = None;
        for _ in 0..50 {
            match tokio_postgres::connect(&conn_str, tls.clone()).await {
                Ok((client, connection)) => {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    attempt = Some(client);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        attempt.expect("pgwire TLS server did not become reachable")
    };

    let row = client
        .query_one("SELECT count(*) AS c FROM people", &[])
        .await
        .expect("count query over TLS");
    let c: i64 = row.get("c");
    assert_eq!(c, 3);

    server.abort();
    drop(store);
}

// ---------------------------------------------------------------------------
// Test 3: dedicated large-stack runtime (`spawn_pgwire`)
// ---------------------------------------------------------------------------

/// The production entry point hosts pgwire on its own OS thread + multi-thread
/// runtime with large worker stacks so DataFusion's recursive SQL planner can
/// absorb the deeply nested `pg_catalog`/`information_schema` introspection
/// queries BI clients (DBeaver, …) fire on connect without overflowing the
/// stack and aborting the process. This exercises that path end to end:
/// start via [`spawn_pgwire`], serve a query, then prove the handle's `Drop`
/// signals shutdown and joins the thread without hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pgwire_dedicated_runtime_serves_and_stops() {
    let dir = TempDir::new().unwrap();
    let parquet = dir.path().join("people.parquet");
    write_people_parquet(&parquet);
    let store = make_people_store(parquet.to_str().unwrap()).await;

    let port = free_port();
    let cfg = PgwireConfig {
        enabled: true,
        listen: loopback(),
        port,
        username: USER.into(),
        // Loopback with no password is permitted by config validation and keeps
        // the test focused on the runtime plumbing rather than auth.
        password: None,
        tls_cert: None,
        tls_key: None,
    };
    let ctx = store.session_context().clone();

    // Start on the dedicated large-stack runtime — the same path `lib::serve`
    // uses in production.
    let server = spawn_pgwire(ctx, cfg).expect("spawn pgwire runtime");

    let conn = format!("host=127.0.0.1 port={port} user={USER} dbname=datapress");
    let client = {
        let mut attempt = None;
        for _ in 0..50 {
            match tokio_postgres::connect(&conn, tokio_postgres::NoTls).await {
                Ok((client, connection)) => {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    attempt = Some(client);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        attempt.expect("pgwire dedicated-runtime server did not become reachable")
    };

    // A data query and a pg_catalog introspection query both plan and run on
    // the large-stack workers.
    let row = client
        .query_one("SELECT count(*) AS c FROM people WHERE id > 0", &[])
        .await
        .expect("count query on dedicated runtime");
    let c: i64 = row.get("c");
    assert_eq!(c, 3);

    let row = client
        .query_one("SELECT count(*) AS c FROM pg_catalog.pg_type", &[])
        .await
        .expect("pg_catalog.pg_type query on dedicated runtime");
    let pg_type_rows: i64 = row.get("c");
    assert!(pg_type_rows > 0, "pg_catalog.pg_type should be populated");

    // Drop the client connection first, then tear the server down. `Drop`
    // signals the listener and joins the pgwire thread; the test hanging here
    // would mean shutdown never completes.
    drop(client);
    drop(server);
    drop(store);
}

// ---------------------------------------------------------------------------
// Test 4: pathological catalog query must not abort the process
// ---------------------------------------------------------------------------

/// Regression + crash-containment for the DBeaver "view columns" stack overflow.
///
/// DBeaver reads `pg_catalog.pg_roles` (via `SELECT a.oid, a.*, pd.description
/// FROM pg_catalog.pg_roles a LEFT JOIN pg_catalog.pg_shdescription pd …`) when
/// listing a table's columns. `datafusion-pg-catalog` 0.17.2 shipped a broken
/// blanket `impl<T> PgCatalogContextProvider for Arc<T>` whose `roles()` calls
/// `self.roles()` — re-dispatching to the `Arc<T>` impl itself, i.e. unbounded
/// self-recursion. Because we previously handed `setup_pg_catalog` an
/// `Arc<AuthManager>`, any read of `pg_roles` recursed until the stack
/// overflowed and **aborted the whole process** (HTTP API included). The fix
/// passes the `AuthManager` value so the direct, non-recursive impl is used.
///
/// A stack overflow is not `catch_unwind`-able (it aborts), so the only real
/// containment is removing the infinite recursion. This test therefore asserts
/// the *process survives*: the minimal crashing query returns, the exact
/// DBeaver query returns the emulated role listing, a deeply nested
/// (finite-but-pathological) query returns a clean error instead of crashing,
/// the same connection stays usable afterwards, and a second independent
/// connection is unaffected. Runs on the production `spawn_pgwire` path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pgwire_pathological_pg_roles_query_survives() {
    let dir = TempDir::new().unwrap();
    let parquet = dir.path().join("people.parquet");
    write_people_parquet(&parquet);
    let store = make_people_store(parquet.to_str().unwrap()).await;

    let port = free_port();
    let cfg = PgwireConfig {
        enabled: true,
        listen: loopback(),
        port,
        username: USER.into(),
        password: None,
        tls_cert: None,
        tls_key: None,
    };
    let ctx = store.session_context().clone();
    let server = spawn_pgwire(ctx, cfg).expect("spawn pgwire runtime");

    let conn = format!("host=127.0.0.1 port={port} user={USER} dbname=datapress");
    let connect = || async {
        let mut attempt = None;
        for _ in 0..50 {
            match tokio_postgres::connect(&conn, tokio_postgres::NoTls).await {
                Ok((client, connection)) => {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    attempt = Some(client);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        attempt.expect("pgwire server did not become reachable")
    };
    let client = connect().await;

    // (a) Minimal crashing form from the bisect: `SELECT * FROM pg_roles`
    // alone used to overflow the stack. It must now return without aborting.
    let rows = client
        .query("SELECT * FROM pg_catalog.pg_roles", &[])
        .await
        .expect("SELECT * FROM pg_catalog.pg_roles must not crash the server");
    assert!(
        !rows.is_empty(),
        "pg_roles should list the seeded default role"
    );

    // (b) The verbatim DBeaver "view columns" query — correct results: the
    // emulated `pg_roles` lists the default `postgres` role.
    const DBEAVER_VIEW_COLUMNS: &str = "SELECT a.oid, a.*, pd.description \
FROM pg_catalog.pg_roles a \
LEFT JOIN pg_catalog.pg_shdescription pd ON a.oid = pd.objoid \
ORDER BY a.rolname";
    let rows = client
        .query(DBEAVER_VIEW_COLUMNS, &[])
        .await
        .expect("DBeaver view-columns query must succeed");
    let rolnames: Vec<String> = rows.iter().map(|r| r.get::<_, String>("rolname")).collect();
    assert!(
        rolnames.iter().any(|n| n == "postgres"),
        "pg_roles listing must include the default 'postgres' role, got {rolnames:?}"
    );

    // (c) Defense-in-depth: a deeply nested (finite) expression exceeds the
    // parser's recursion limit and must come back as a clean ERROR to this one
    // statement — never a stack overflow that takes the process down.
    let deep = format!("SELECT {}1{}", "(".repeat(256), ")".repeat(256));
    let err = client.query(&deep, &[]).await;
    assert!(
        err.is_err(),
        "a pathologically nested query should error, not crash"
    );

    // (d) The connection is still usable after the error — the failure was
    // scoped to that one statement.
    let row = client
        .query_one("SELECT count(*) AS c FROM people", &[])
        .await
        .expect("connection must stay usable after a failed statement");
    assert_eq!(row.get::<_, i64>("c"), 3);

    // (e) A second, independent connection is unaffected — proving the earlier
    // pathological queries never poisoned the shared server/runtime.
    let client2 = connect().await;
    let rows = client2
        .query("SELECT * FROM pg_catalog.pg_roles", &[])
        .await
        .expect("second connection must serve pg_roles too");
    assert!(!rows.is_empty());

    drop(client);
    drop(client2);
    drop(server);
    drop(store);
}

// ---------------------------------------------------------------------------
// Test 5: information_schema.columns reports PostgreSQL type names
// ---------------------------------------------------------------------------

/// Regression for the Power BI DirectQuery "couldn't fold" bug: DataFusion's
/// built-in `information_schema.columns` fills `data_type` with Arrow type
/// names (`"Utf8View"`, `"Int64"`, …) and has no `udt_name`. Power BI reads
/// column metadata from this view and treats `data_type` as a PostgreSQL type;
/// `"Utf8View"` is not a valid pg type, so string columns silently failed to
/// fold while `"Boolean"` (coincidentally pg-valid) worked. Our provider
/// overrides `columns` to report pg type names + `udt_name`.
///
/// Asserts, for every column of the fixture table: (a) `data_type` is a
/// PostgreSQL-valid name (allowlist), (b) `udt_name` is non-null; plus the
/// explicit case that the `Utf8View` column `s` reports `text`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pgwire_information_schema_columns_reports_pg_types() {
    let dir = TempDir::new().unwrap();
    let parquet = dir.path().join("people.parquet");
    write_people_parquet(&parquet);
    let store = make_people_store(parquet.to_str().unwrap()).await;
    register_types_fixture(&store);

    let port = free_port();
    let cfg = PgwireConfig {
        enabled: true,
        listen: loopback(),
        port,
        username: USER.into(),
        password: None,
        tls_cert: None,
        tls_key: None,
    };
    let ctx = store.session_context().clone();
    let server = spawn_pgwire(ctx, cfg).expect("spawn pgwire runtime");

    let conn = format!("host=127.0.0.1 port={port} user={USER} dbname=datapress");
    let client = {
        let mut attempt = None;
        for _ in 0..50 {
            match tokio_postgres::connect(&conn, tokio_postgres::NoTls).await {
                Ok((client, connection)) => {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    attempt = Some(client);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        attempt.expect("pgwire server did not become reachable")
    };

    // The exact metadata query shape Power BI / pg clients issue.
    let rows = client
        .query(
            "SELECT column_name, data_type, udt_name, is_nullable \
             FROM information_schema.columns \
             WHERE table_name = 'types_fixture' \
             ORDER BY ordinal_position",
            &[],
        )
        .await
        .expect("information_schema.columns query");

    assert_eq!(
        rows.len(),
        10,
        "types_fixture has 10 columns, got {}",
        rows.len()
    );

    // PostgreSQL-valid `data_type` values (SQL-standard / pg information_schema
    // spellings). Any Arrow name (e.g. "Utf8View") would fail this allowlist.
    const PG_DATA_TYPES: &[&str] = &[
        "text",
        "boolean",
        "smallint",
        "integer",
        "bigint",
        "real",
        "double precision",
        "numeric",
        "date",
        "timestamp without time zone",
        "timestamp with time zone",
        "time without time zone",
        "bytea",
        "ARRAY",
        "interval",
    ];

    let mut seen_s = false;
    for row in &rows {
        let col: String = row.get("column_name");
        let data_type: String = row.get("data_type");
        let udt_name: Option<String> = row.get("udt_name");
        assert!(
            PG_DATA_TYPES.contains(&data_type.as_str()),
            "column {col}: data_type '{data_type}' is not a PostgreSQL type name"
        );
        assert!(
            udt_name.as_deref().is_some_and(|u| !u.is_empty()),
            "column {col}: udt_name must be non-null/non-empty, got {udt_name:?}"
        );
        if col == "s" {
            seen_s = true;
            assert_eq!(
                data_type, "text",
                "the Utf8View column 's' must report 'text', got '{data_type}'"
            );
            assert_eq!(udt_name.as_deref(), Some("text"));
        }
    }
    assert!(seen_s, "fixture must contain the Utf8View column 's'");

    drop(client);
    drop(server);
    drop(store);
}

// ---------------------------------------------------------------------------
// Test 6: binary-encoding sweep — one-row SELECT per column
// ---------------------------------------------------------------------------

/// Exercises the server's binary result encoding for every fixture column type
/// over the extended protocol (tokio-postgres `query` prepares + requests
/// binary results). A one-row `SELECT <col>` forces the server to binary-encode
/// that column; the test asserts each returns exactly one row, and decodes the
/// scalar types to their natural Rust type to prove the bytes round-trip.
/// Temporal columns are exercised for encode-without-error but not decoded
/// (that would need tokio-postgres's chrono/time feature).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pgwire_binary_encoding_sweep() {
    let dir = TempDir::new().unwrap();
    let parquet = dir.path().join("people.parquet");
    write_people_parquet(&parquet);
    let store = make_people_store(parquet.to_str().unwrap()).await;
    register_types_fixture(&store);

    let port = free_port();
    let cfg = PgwireConfig {
        enabled: true,
        listen: loopback(),
        port,
        username: USER.into(),
        password: None,
        tls_cert: None,
        tls_key: None,
    };
    let ctx = store.session_context().clone();
    let server = spawn_pgwire(ctx, cfg).expect("spawn pgwire runtime");

    let conn = format!("host=127.0.0.1 port={port} user={USER} dbname=datapress");
    let client = {
        let mut attempt = None;
        for _ in 0..50 {
            match tokio_postgres::connect(&conn, tokio_postgres::NoTls).await {
                Ok((client, connection)) => {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    attempt = Some(client);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        attempt.expect("pgwire server did not become reachable")
    };

    // Every column: one-row SELECT must return exactly one row over the binary
    // extended protocol (proves the server binary-encodes the type at all).
    for col in ["s", "t", "b", "i2", "i4", "i8", "f4", "f8", "ts", "d"] {
        let rows = client
            .query(&format!("SELECT {col} FROM types_fixture LIMIT 1"), &[])
            .await
            .unwrap_or_else(|e| panic!("binary SELECT of column {col} failed: {e}"));
        assert_eq!(rows.len(), 1, "column {col}: expected one row");
    }

    // Decode the scalar types to their natural Rust type — proves the binary
    // bytes decode, not just that a DataRow was sent.
    let r = client
        .query_one(
            "SELECT s, t, b, i2, i4, i8, f4, f8 FROM types_fixture LIMIT 1",
            &[],
        )
        .await
        .expect("typed binary row");
    assert_eq!(r.get::<_, &str>("s"), "CA");
    assert_eq!(r.get::<_, &str>("t"), "text-val");
    assert!(r.get::<_, bool>("b"));
    assert_eq!(r.get::<_, i16>("i2"), 7);
    assert_eq!(r.get::<_, i32>("i4"), 42);
    assert_eq!(r.get::<_, i64>("i8"), 99);
    assert_eq!(r.get::<_, f32>("f4"), 1.5);
    assert_eq!(r.get::<_, f64>("f8"), 2.5);

    drop(client);
    drop(server);
    drop(store);
}
