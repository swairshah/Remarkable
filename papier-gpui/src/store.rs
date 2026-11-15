//! Local state: settings, the offline library snapshot, a disk cache for
//! version-stamped assets (covers, page rasters, ink), and the pending-ink
//! overlay.
//!
//! Pending ink: a write lands in the VM's inbound tree, but what reads
//! serve can stay stale until the tablet wakes, pulls and pushes back.
//! Until the doc's version moves past our write, the local copy is the
//! freshest truth and wins (same rule as the iPad app).

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
};

use serde_json::{Value, json};

use crate::model::{InkPage, Library};

pub const DEFAULT_SERVER: &str = "http://100.90.235.68:8000";

fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("papier-gpui")
}

fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("papier-gpui")
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("papier-gpui")
}

fn hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn write_atomic(path: &PathBuf, data: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, data).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

// ------------------------------------------------------------- settings

#[derive(Clone, Debug)]
pub struct Settings {
    pub server: String,
    /// "system" | "light" | "dark"
    pub theme: String,
    /// "white" | "paper" | "soft"
    pub paper: String,
    pub sort: String,
    /// "grid" | "list"
    pub view: String,
    pub raw: Value,
}

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        let raw: Value = fs::read(Self::path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or(json!({}));
        let s = |k: &str, d: &str| raw.get(k).and_then(Value::as_str).unwrap_or(d).to_owned();
        Settings {
            server: s("server", DEFAULT_SERVER),
            theme: s("theme", "system"),
            paper: s("paper", "paper"),
            sort: s("sort", "recent"),
            view: s("view", "grid"),
            raw,
        }
    }

    pub fn save(&mut self) {
        let mut raw = self.raw.as_object().cloned().unwrap_or_default();
        raw.insert("server".into(), json!(self.server));
        raw.insert("theme".into(), json!(self.theme));
        raw.insert("paper".into(), json!(self.paper));
        raw.insert("sort".into(), json!(self.sort));
        raw.insert("view".into(), json!(self.view));
        self.raw = Value::Object(raw);
        write_atomic(&Self::path(), &serde_json::to_vec_pretty(&self.raw).unwrap_or_default());
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.raw.get(key)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        if let Some(obj) = self.raw.as_object_mut() {
            obj.insert(key.into(), value);
        } else {
            self.raw = json!({ key: value });
        }
        self.save();
    }
}

// -------------------------------------------------------- offline library

pub struct Snapshot {
    pub library: Library,
    pub etag: Option<String>,
    pub synced_at: Option<u64>,
}

fn library_path(server: &str) -> PathBuf {
    cache_dir().join(hash(server)).join("library.json")
}

/// Bump when `Doc` gains fields: an older snapshot still opens offline, but
/// its ETag is dropped so the next sync refetches the whole manifest
/// (otherwise a 304 would keep the old, field-less docs forever).
const SNAPSHOT_SCHEMA: u64 = 2;

pub fn save_library(server: &str, lib: &Library, etag: Option<&str>, synced_at: u64) {
    let v = json!({ "schema": SNAPSHOT_SCHEMA, "library": lib, "etag": etag, "synced_at": synced_at });
    write_atomic(&library_path(server), &serde_json::to_vec(&v).unwrap_or_default());
}

pub fn load_library(server: &str) -> Option<Snapshot> {
    let v: Value = serde_json::from_slice(&fs::read(library_path(server)).ok()?).ok()?;
    Some(Snapshot {
        library: serde_json::from_value(v.get("library")?.clone()).ok()?,
        etag: v
            .get("etag")
            .and_then(Value::as_str)
            .filter(|_| v.get("schema").and_then(Value::as_u64) == Some(SNAPSHOT_SCHEMA))
            .map(str::to_owned),
        synced_at: v.get("synced_at").and_then(Value::as_u64),
    })
}

// ------------------------------------------------------------ asset cache

/// Versioned URLs (covers, page rasters) are immutable: cache by URL.
fn asset_path(url: &str) -> PathBuf {
    cache_dir().join("assets").join(hash(url))
}

pub fn cached_asset(url: &str) -> Option<Vec<u8>> {
    fs::read(asset_path(url)).ok()
}

pub fn store_asset(url: &str, data: &[u8]) {
    write_atomic(&asset_path(url), data);
}

/// Last ink seen for a page, used when the server is unreachable.
fn offline_ink_path(server: &str, doc: &str, key: &str) -> PathBuf {
    cache_dir().join(hash(server)).join("ink").join(hash(doc)).join(format!("{key}.json"))
}

pub fn save_offline_ink(server: &str, doc: &str, key: &str, page: &InkPage) {
    write_atomic(&offline_ink_path(server, doc, key), &page.serialized());
}

pub fn load_offline_ink(server: &str, doc: &str, key: &str) -> Option<InkPage> {
    InkPage::parse(&fs::read(offline_ink_path(server, doc, key)).ok()?)
}

// ----------------------------------------------------------- pending ink

fn pending_path(doc: &str, key: &str) -> PathBuf {
    data_dir().join("pending").join(doc).join(format!("{key}.json"))
}

/// Record a successful upload: keep the page locally, stamped with the doc
/// version it was based on, until the mirror version moves.
/// `unsent` marks a write the server hasn't acknowledged yet (offline):
/// it survives version changes and is retried on the next good sync.
pub fn remember_pending(doc: &str, key: &str, page: &InkPage, base_version: &str, unsent: bool) {
    let mut v = page.to_value();
    v["_baseVersion"] = json!(base_version);
    if unsent {
        v["_unsent"] = json!(true);
    }
    write_atomic(&pending_path(doc, key), &serde_json::to_vec(&v).unwrap_or_default());
}

/// Every write still waiting for the server: (doc, key, page, base version).
pub fn unsent() -> Vec<(String, String, InkPage, String)> {
    let mut out = Vec::new();
    let Ok(docs) = fs::read_dir(data_dir().join("pending")) else { return out };
    for d in docs.flatten() {
        let Ok(files) = fs::read_dir(d.path()) else { continue };
        for f in files.flatten() {
            let Some(v) = fs::read(f.path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { continue };
            if v.get("_unsent").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let Some(page) = InkPage::from_value(&v) else { continue };
            let key = f.path().file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let base = v.get("_baseVersion").and_then(Value::as_str).unwrap_or("").to_owned();
            out.push((d.file_name().to_string_lossy().to_string(), key, page, base));
        }
    }
    out
}

/// The freshest known ink for a page: the local pending copy while the
/// mirror still shows the version we based it on.
pub fn pending_ink(doc: &str, key: &str, current_version: &str) -> Option<InkPage> {
    let path = pending_path(doc, key);
    let v: Value = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
    let unsent = v.get("_unsent").and_then(Value::as_bool) == Some(true);
    match v.get("_baseVersion").and_then(Value::as_str) {
        Some(base) if base != current_version && !unsent => {
            // The doc changed server-side since our write: the server is truth again.
            let _ = fs::remove_file(&path);
            None
        }
        _ => InkPage::from_value(&v),
    }
}

/// Drop pending dirs of documents the server no longer lists.
pub fn prune_pending(lib: &Library) {
    let Ok(dirs) = fs::read_dir(data_dir().join("pending")) else { return };
    for d in dirs.flatten() {
        let name = d.file_name().to_string_lossy().to_string();
        if !lib.docs.iter().any(|doc| doc.id == name) {
            let _ = fs::remove_dir_all(d.path());
        }
    }
}

pub fn forget_pending_doc(doc: &str) {
    let _ = fs::remove_dir_all(data_dir().join("pending").join(doc));
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
