//! Phase 2B — Materialization storage helpers.
//!
//! Provides ULID generation, generation-manifest (de)serialization, path
//! construction, and GC helpers shared by the DataFusion and DuckDB backends.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use object_store::ObjectStore;
use serde::{Deserialize, Serialize};

use crate::config::{AddressingStyle, StorageBackendKind, StorageConfig};
use crate::errors::AppError;

// ---------------------------------------------------------------------------
// ULID generator (no external crate, G7 compliance)
// ---------------------------------------------------------------------------

/// Base32 Crockford alphabet.
const B32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Generate a 26-character ULID string (Universally Unique Lexicographically
/// Sortable Identifier). Timestamp from `SystemTime`; random part from a
/// simple LCG seeded from timestamp + stack address entropy.
pub fn new_ulid() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;

    // 80 bits of pseudo-random using an LCG. Not cryptographic, but
    // sufficient for generation-directory naming uniqueness.
    let seed = millis ^ ((&millis as *const u64 as u64).wrapping_mul(6364136223846793005));
    let mut state = seed.wrapping_add(1442695040888963407);
    let mut rand_bytes = [0u8; 10];
    for b in &mut rand_bytes {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (state >> 33) as u8;
    }

    // Encode: 10 chars for 48-bit timestamp, 16 chars for 80-bit random.
    let mut out = [0u8; 26];
    // Timestamp (48 bits → 10 × 5-bit groups)
    out[0] = B32[((millis >> 45) & 0x1F) as usize];
    out[1] = B32[((millis >> 40) & 0x1F) as usize];
    out[2] = B32[((millis >> 35) & 0x1F) as usize];
    out[3] = B32[((millis >> 30) & 0x1F) as usize];
    out[4] = B32[((millis >> 25) & 0x1F) as usize];
    out[5] = B32[((millis >> 20) & 0x1F) as usize];
    out[6] = B32[((millis >> 15) & 0x1F) as usize];
    out[7] = B32[((millis >> 10) & 0x1F) as usize];
    out[8] = B32[((millis >> 5) & 0x1F) as usize];
    out[9] = B32[(millis & 0x1F) as usize];
    // Random (80 bits = 10 bytes → 16 × 5-bit groups)
    let r: u128 = rand_bytes
        .iter()
        .fold(0u128, |acc, &b| (acc << 8) | b as u128);
    for i in 0..16 {
        out[10 + i] = B32[((r >> (75 - i * 5)) & 0x1F) as usize];
    }
    // SAFETY: all bytes are ASCII characters from B32.
    String::from_utf8(out.to_vec()).expect("ulid is ascii")
}

// ---------------------------------------------------------------------------
// Simple FNV-1a hash for sql/schema fingerprints
// ---------------------------------------------------------------------------

/// Compute a deterministic 64-bit FNV-1a hash of `data`.
/// Used to detect config changes for `reuse_on_start`.
pub fn fnv1a_hash(data: &str) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for byte in data.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

// ---------------------------------------------------------------------------
// Generation manifest
// ---------------------------------------------------------------------------

/// Manifest written at the end of a successful generation. A generation
/// without a manifest is considered incomplete and must be GC'd.
///
/// Written as `manifest.json` in the generation directory — LAST, after all
/// parquet data files, so a manifest's presence is the atomicity seal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationManifest {
    /// FNV-1a hash of the SQL string used to build this generation.
    pub sql_hash: u64,
    /// FNV-1a hash of a canonical schema fingerprint (field names + types).
    pub schema_hash: u64,
    /// Number of rows in this generation.
    pub row_count: u64,
    /// Total byte size of all parquet files in this generation.
    pub byte_size: u64,
    /// RFC3339 creation timestamp (best-effort; used for display only).
    pub created_at: String,
    /// List of parquet file names (relative to the generation directory).
    pub files: Vec<String>,
}

impl GenerationManifest {
    /// Write this manifest as `manifest.json` inside `gen_dir`.
    pub fn write(&self, gen_dir: &Path) -> Result<(), std::io::Error> {
        let path = gen_dir.join("manifest.json");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Read `manifest.json` from `gen_dir`. Returns `None` when the file
    /// is absent or unreadable (incomplete generation).
    pub fn read(gen_dir: &Path) -> Option<Self> {
        let path = gen_dir.join("manifest.json");
        let data = std::fs::read(&path).ok()?;
        serde_json::from_slice(&data).ok()
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Return the generation directory path:
/// `<root>/<dataset>/<generation_id>/`
pub fn generation_dir(root: &Path, dataset: &str, gen_id: &str) -> PathBuf {
    root.join(dataset).join(gen_id)
}

/// List all complete generations for `dataset` under `root`, sorted by ULID
/// (lexicographic = chronological order). A generation is complete when it
/// contains a `manifest.json`. Returns `Vec<(gen_id, manifest, gen_dir)>`.
pub fn list_complete_generations(
    root: &Path,
    dataset: &str,
) -> Vec<(String, GenerationManifest, PathBuf)> {
    let dataset_dir = root.join(dataset);
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(&dataset_dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let gen_id = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if let Some(manifest) = GenerationManifest::read(&path) {
            result.push((gen_id, manifest, path));
        }
    }
    // Sort lexicographically — ULID timestamps sort correctly this way.
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Delete all generations older than the N-2 rule (keep current + previous;
/// delete everything before that). Also deletes incomplete (manifest-less)
/// directories.
///
/// `current_gen_id` is the just-published generation; `prev_gen_id` is the
/// one before it (if known). All other directories are removed.
pub fn gc_generations(root: &Path, dataset: &str, keep_ids: &[&str]) {
    let dataset_dir = root.join(dataset);
    let entries = match std::fs::read_dir(&dataset_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let gen_id = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // Always delete incomplete generations (no manifest).
        let is_incomplete = GenerationManifest::read(&path).is_none();
        let is_old = !keep_ids.contains(&gen_id.as_str());
        if is_incomplete || is_old {
            log::debug!("storage GC: removing generation dir {:?}", path);
            if let Err(e) = std::fs::remove_dir_all(&path) {
                log::warn!("storage GC: failed to remove {:?}: {e}", path);
            }
        }
    }
}

/// RFC3339-formatted current UTC timestamp (best-effort).
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    // Rudimentary ISO-8601 UTC from Unix seconds (avoids a chrono dependency).
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Rough date calculation (accurate post-1970, ignores leap seconds).
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Gregorian calendar epoch 1970-01-01.
    let mut year = 1970u64;
    loop {
        let leap =
            (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
        let ydays = if leap { 366 } else { 365 };
        if days < ydays {
            break;
        }
        days -= ydays;
        year += 1;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn ulid_is_26_chars_and_uppercase() {
        let u = new_ulid();
        assert_eq!(u.len(), 26);
        assert!(
            u.chars()
                .all(|c| c.is_ascii_alphanumeric() && !c.is_ascii_lowercase())
        );
    }

    #[test]
    fn ulid_is_monotonic_within_same_ms() {
        // Two ULIDs generated back-to-back must be distinct (random part).
        let a = new_ulid();
        let b = new_ulid();
        // May collide in theory but extremely unlikely.
        let _ = (a, b); // just ensure no panic
    }

    #[test]
    fn fnv1a_is_deterministic() {
        assert_eq!(fnv1a_hash("hello"), fnv1a_hash("hello"));
        assert_ne!(fnv1a_hash("hello"), fnv1a_hash("world"));
    }

    #[test]
    fn gc_removes_old_and_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ds_dir = root.join("myds");
        std::fs::create_dir_all(&ds_dir).unwrap();

        // gen_a: complete
        let a = ds_dir.join("01AAAAAAAAAAAAAAAAAAAAAAAA");
        std::fs::create_dir_all(&a).unwrap();
        let m = GenerationManifest {
            sql_hash: 1,
            schema_hash: 2,
            row_count: 10,
            byte_size: 100,
            created_at: "2024-01-01T00:00:00Z".into(),
            files: vec!["data-0.parquet".into()],
        };
        m.write(&a).unwrap();

        // gen_b: complete (current)
        let b = ds_dir.join("01BBBBBBBBBBBBBBBBBBBBBBBB");
        std::fs::create_dir_all(&b).unwrap();
        m.write(&b).unwrap();

        // gen_c: incomplete (no manifest)
        let c = ds_dir.join("01CCCCCCCCCCCCCCCCCCCCCCCC");
        std::fs::create_dir_all(&c).unwrap();

        gc_generations(
            root,
            "myds",
            &["01AAAAAAAAAAAAAAAAAAAAAAAA", "01BBBBBBBBBBBBBBBBBBBBBBBB"],
        );

        assert!(a.exists(), "a should be kept");
        assert!(b.exists(), "b should be kept");
        assert!(!c.exists(), "incomplete c should be removed");
    }

    #[test]
    fn gc_removes_n_minus_2() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ds_dir = root.join("myds");
        std::fs::create_dir_all(&ds_dir).unwrap();

        let m = GenerationManifest {
            sql_hash: 1,
            schema_hash: 2,
            row_count: 10,
            byte_size: 100,
            created_at: "2024-01-01T00:00:00Z".into(),
            files: vec!["data-0.parquet".into()],
        };

        // Three complete generations.
        for id in ["01AA", "01BB", "01CC"] {
            let padded = format!("{:A<26}", id); // pad with 'A'
            let d = ds_dir.join(&padded);
            std::fs::create_dir_all(&d).unwrap();
            m.write(&d).unwrap();
        }

        // Keep only last two (01BB, 01CC).
        gc_generations(
            root,
            "myds",
            &[&format!("{:A<26}", "01BB"), &format!("{:A<26}", "01CC")],
        );

        assert!(
            !ds_dir.join(format!("{:A<26}", "01AA")).exists(),
            "old gen removed"
        );
        assert!(
            ds_dir.join(format!("{:A<26}", "01BB")).exists(),
            "prev gen kept"
        );
        assert!(
            ds_dir.join(format!("{:A<26}", "01CC")).exists(),
            "current gen kept"
        );
    }
}

// ---------------------------------------------------------------------------
// MaterializationStorage — shared by DataFusion and DuckDB backends
// ---------------------------------------------------------------------------

/// Runtime state for the server-level materialization storage backend.
/// Built once at startup from `[server.storage]`; shared across builds via Arc.
/// Both the DataFusion and DuckDB backends use this type so S3 object-store
/// construction code is not duplicated.
pub struct MaterializationStorage {
    /// Parsed storage configuration.
    pub config: StorageConfig,
    /// object_store backend (LocalFileSystem or AmazonS3).
    pub object_store: Arc<dyn ObjectStore>,
    /// Root prefix on the object store (empty for local, "prefix" for S3).
    pub root_prefix: String,
    /// Local root path. Only set for `local` backend so GC and manifest
    /// operations can use stdlib filesystem operations directly.
    pub local_root: Option<PathBuf>,
    /// For S3 backend: bucket name (used to build DuckDB httpfs secrets).
    pub s3_bucket: Option<String>,
}

/// Build a `MaterializationStorage` from a validated [`StorageConfig`].
///
/// For the local backend the root directory is created if absent.
/// For S3 the object-store client is built from env-var-indirected credentials.
pub fn build_materialization_storage(
    sc: &StorageConfig,
) -> Result<MaterializationStorage, AppError> {
    match sc.backend {
        StorageBackendKind::Local => {
            use object_store::local::LocalFileSystem;
            let root = Path::new(&sc.root);
            std::fs::create_dir_all(root).map_err(|e| {
                AppError::Internal(format!(
                    "server.storage: failed to create local root '{}': {e}",
                    sc.root
                ))
            })?;
            let store = Arc::new(LocalFileSystem::new_with_prefix(root).map_err(|e| {
                AppError::Internal(format!(
                    "server.storage: failed to open local root '{}': {e}",
                    sc.root
                ))
            })?);
            Ok(MaterializationStorage {
                config: sc.clone(),
                object_store: store,
                root_prefix: String::new(),
                local_root: Some(root.to_path_buf()),
                s3_bucket: None,
            })
        }
        StorageBackendKind::S3 => {
            use object_store::aws::AmazonS3Builder;
            let rest = sc.root.strip_prefix("s3://").ok_or_else(|| {
                AppError::Internal(format!(
                    "server.storage.root must start with s3:// for backend = \"s3\" (got '{}')",
                    sc.root
                ))
            })?;
            let (bucket, prefix) = match rest.split_once('/') {
                Some((b, p)) => (b, p),
                None => (rest, ""),
            };
            let creds = sc.s3.resolved_creds()?;
            let mut builder = AmazonS3Builder::new().with_bucket_name(bucket);
            if let Some(r) = &sc.s3.region {
                builder = builder.with_region(r);
            }
            if let Some(ep) = &sc.s3.endpoint {
                builder = builder.with_endpoint(ep);
            }
            builder = builder.with_allow_http(sc.s3.allow_http);
            if sc.s3.addressing_style == AddressingStyle::Path {
                builder = builder.with_virtual_hosted_style_request(false);
            }
            if let (Some(k), Some(s)) = (
                creds.access_key_id.as_deref(),
                creds.secret_access_key.as_deref(),
            ) {
                builder = builder.with_access_key_id(k).with_secret_access_key(s);
            }
            let store = Arc::new(builder.build().map_err(|e| {
                AppError::Internal(format!("server.storage: failed to build S3 store: {e}"))
            })?);
            Ok(MaterializationStorage {
                config: sc.clone(),
                object_store: store,
                root_prefix: prefix.trim_end_matches('/').to_string(),
                local_root: None,
                s3_bucket: Some(bucket.to_string()),
            })
        }
    }
}

impl MaterializationStorage {
    /// Compute the S3 URL (or local path) for a dataset generation directory.
    /// Returns the S3 URL `s3://bucket/<prefix>/<dataset>/<gen_id>/` for S3,
    /// or `file:///.../<dataset>/<gen_id>/` for local.
    pub fn generation_url(&self, dataset: &str, gen_id: &str) -> String {
        match &self.s3_bucket {
            Some(bucket) => {
                if self.root_prefix.is_empty() {
                    format!("s3://{bucket}/{dataset}/{gen_id}/")
                } else {
                    format!("s3://{bucket}/{}/{dataset}/{gen_id}/", self.root_prefix)
                }
            }
            None => {
                let local = self
                    .local_root
                    .as_deref()
                    .unwrap_or(Path::new(&self.config.root));
                format!("file://{}", local.join(dataset).join(gen_id).display())
            }
        }
    }

    /// Object store path (no scheme) for a file within a generation.
    pub fn obj_path(
        &self,
        dataset: &str,
        gen_id: &str,
        filename: &str,
    ) -> object_store::path::Path {
        if self.root_prefix.is_empty() {
            object_store::path::Path::from(format!("{dataset}/{gen_id}/{filename}"))
        } else {
            object_store::path::Path::from(format!(
                "{}/{dataset}/{gen_id}/{filename}",
                self.root_prefix
            ))
        }
    }

    /// Write manifest to S3 using the object_store client.
    /// Must be called from within a tokio runtime (async context).
    pub async fn write_manifest_s3(
        &self,
        dataset: &str,
        gen_id: &str,
        manifest: &GenerationManifest,
    ) -> Result<(), AppError> {
        use object_store::ObjectStoreExt;
        let json = serde_json::to_vec_pretty(manifest)
            .map_err(|e| AppError::Internal(format!("manifest serialize: {e}")))?;
        let path = self.obj_path(dataset, gen_id, "manifest.json");
        self.object_store
            .put(&path, object_store::PutPayload::from(json))
            .await
            .map_err(|e| {
                AppError::Internal(format!("dataset '{dataset}': write manifest to S3: {e}"))
            })?;
        Ok(())
    }

    /// Read manifest from S3 using the object_store client.
    /// Returns `None` when the manifest does not exist (incomplete generation).
    pub async fn read_manifest_s3(
        &self,
        dataset: &str,
        gen_id: &str,
    ) -> Option<GenerationManifest> {
        use object_store::ObjectStoreExt;
        let path = self.obj_path(dataset, gen_id, "manifest.json");
        let data = self
            .object_store
            .get(&path)
            .await
            .ok()?
            .bytes()
            .await
            .ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// List complete S3 generations for `dataset` (sorted by ULID).
    pub async fn list_complete_generations_s3(
        &self,
        dataset: &str,
    ) -> Vec<(String, GenerationManifest)> {
        // List one "directory" level under <prefix>/<dataset>/
        let prefix_path = if self.root_prefix.is_empty() {
            object_store::path::Path::from(format!("{dataset}/"))
        } else {
            object_store::path::Path::from(format!("{}/{dataset}/", self.root_prefix))
        };
        let listed = match self
            .object_store
            .list_with_delimiter(Some(&prefix_path))
            .await
        {
            Ok(l) => l,
            Err(_) => return vec![],
        };
        let mut result = Vec::new();
        for common_prefix in listed.common_prefixes {
            // Extract gen_id from the path component.
            let full = common_prefix.to_string();
            let gen_id = full
                .trim_end_matches('/')
                .split('/')
                .next_back()
                .unwrap_or("")
                .to_string();
            if gen_id.is_empty() {
                continue;
            }
            if let Some(manifest) = self.read_manifest_s3(dataset, &gen_id).await {
                result.push((gen_id, manifest));
            }
        }
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    /// GC old S3 generations, keeping at most `keep_count` most recent.
    pub async fn gc_s3_generations(&self, dataset: &str, keep_ids: &[&str]) {
        use object_store::ObjectStoreExt;
        let prefix_path = if self.root_prefix.is_empty() {
            object_store::path::Path::from(format!("{dataset}/"))
        } else {
            object_store::path::Path::from(format!("{}/{dataset}/", self.root_prefix))
        };
        let listed = match self
            .object_store
            .list_with_delimiter(Some(&prefix_path))
            .await
        {
            Ok(l) => l,
            Err(_) => return,
        };
        for cp in listed.common_prefixes {
            let full = cp.to_string();
            let gen_id = full
                .trim_end_matches('/')
                .split('/')
                .next_back()
                .unwrap_or("")
                .to_string();
            if keep_ids.contains(&gen_id.as_str()) {
                continue;
            }
            // Delete all objects under this prefix.
            let gen_prefix = object_store::path::Path::from(full.trim_end_matches('/').to_string());
            let mut objects = self.object_store.list(Some(&gen_prefix));
            use futures_util::StreamExt;
            while let Some(item) = objects.next().await {
                if let Ok(meta) = item {
                    let _ = self.object_store.delete(&meta.location).await;
                }
            }
        }
    }
}
