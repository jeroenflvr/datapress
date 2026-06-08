//! Build script for `datapress-core`.
//!
//! The dataset explorer (`explorer` feature) and the self-hosted docs site
//! (`docs` feature) both ship the vendored DuckDB-WASM assets. The `*.wasm`
//! blobs are ~77 MB raw; embedding them — let alone twice — pushes the PyPI
//! `datap-rs` wheel over PyPI's 100 MB per-file limit.
//!
//! To keep the binary (and wheel) small we stage assets into `$OUT_DIR` at
//! build time:
//!
//! * `$OUT_DIR/duckdb_vendor/` — a *single* gzip-compressed copy of the
//!   vendored DuckDB-WASM assets (the `*.wasm` blobs become `<name>.gz`,
//!   served with `Content-Encoding: gzip`; everything else is copied
//!   verbatim). Staged whenever `explorer` or `docs` is enabled and shared by
//!   both via `crate::duckdb_vendor`.
//! * `$OUT_DIR/docs_site/` — a copy of the built MkDocs site with the large
//!   `*.wasm` blobs under `assets/vendor/duckdb/` stripped out (they resolve
//!   from the shared store instead). Staged only when `docs` is enabled.
//!
//! The on-disk sources under `docs/src/assets/vendor/duckdb/` and `docs/site/`
//! are left untouched so the public documentation site keeps serving them as
//! plain static assets.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let explorer = env::var_os("CARGO_FEATURE_EXPLORER").is_some();
    let docs = env::var_os("CARGO_FEATURE_DOCS").is_some();
    if !explorer && !docs {
        return;
    }

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_dir = Path::new(&manifest_dir);
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_dir = Path::new(&out_dir);

    // Shared gzipped DuckDB-WASM store, used by both the explorer and the
    // self-hosted docs site.
    stage_duckdb_vendor(
        &manifest_dir.join("../../docs/src/assets/vendor/duckdb"),
        &out_dir.join("duckdb_vendor"),
    );

    // Embedded docs site (with the heavy wasm blobs stripped).
    if docs {
        stage_docs_site(
            &manifest_dir.join("../../docs/site"),
            &out_dir.join("docs_site"),
        );
    }
}

/// Mirror the vendored DuckDB-WASM dir into `dst`, gzip-compressing the large
/// `*.wasm` blobs (as `<name>.gz`) and copying every other file verbatim.
fn stage_duckdb_vendor(src_dir: &Path, dst_dir: &Path) {
    println!("cargo:rerun-if-changed={}", src_dir.display());
    let _ = fs::remove_dir_all(dst_dir);
    fs::create_dir_all(dst_dir).expect("create duckdb_vendor out dir");

    for entry in fs::read_dir(src_dir).expect("read vendored duckdb dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());

        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let bytes = fs::read(&path).expect("read vendored asset");

        if name.ends_with(".wasm") {
            // Gzip the wasm binaries; handlers serve the `.gz` with
            // `Content-Encoding: gzip`.
            let gz_path = dst_dir.join(format!("{name}.gz"));
            let file = fs::File::create(&gz_path).expect("create gzipped asset");
            let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::best());
            encoder.write_all(&bytes).expect("gzip write");
            encoder.finish().expect("gzip finish");
        } else {
            fs::write(dst_dir.join(name.as_ref()), &bytes).expect("copy asset");
        }
    }
}

/// Recursively copy the built MkDocs site into `dst`, skipping the large
/// vendored DuckDB-WASM `*.wasm` blobs (served from the shared gzipped store).
fn stage_docs_site(src_dir: &Path, dst_dir: &Path) {
    println!("cargo:rerun-if-changed={}", src_dir.display());
    let _ = fs::remove_dir_all(dst_dir);
    fs::create_dir_all(dst_dir).expect("create docs_site out dir");
    copy_tree(src_dir, dst_dir);
}

/// Recursively copy `src` into `dst`, omitting `*.wasm` files (the binary
/// serves those from the shared gzipped DuckDB-WASM store).
fn copy_tree(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).expect("read docs site dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        let name = entry.file_name();
        let target: PathBuf = dst.join(&name);

        if path.is_dir() {
            fs::create_dir_all(&target).expect("create docs subdir");
            copy_tree(&path, &target);
        } else if path.is_file() {
            if name.to_string_lossy().ends_with(".wasm") {
                continue;
            }
            fs::copy(&path, &target).expect("copy docs file");
        }
    }
}
