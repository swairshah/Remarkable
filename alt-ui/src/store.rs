//! The document store: every notebook and book bundle under the data dir.
//!
//! Layout (PAPER_DATA_DIR, default ~/.local/share/alt-ui):
//!
//!   docs/<id>/          one document (doc.rs) — book bundles keep the
//!                       reader layout byte-for-byte; notebooks are the
//!                       same minus pages/ + text/
//!     meta.json         { v, kind:"notebook"|"book", title, folder, pages?, created }
//!                       (book bundles from mkbook.py lack kind/v — they
//!                       read as books by the pages>0 rule)
//!     state.json ink/ [pages/ text/] thumb.png
//!   folders.json        { v, folders:[..] }
//!   settings.json       { last_doc, ... }
//!
//! Folders are a meta.json FIELD, not subdirectories: moving a doc
//! rewrites one file and no ink path ever changes.

use crate::doc::DocKind;
use serde_json::{json, Value};

pub fn data_dir() -> String {
    if let Ok(d) = std::env::var("PAPER_DATA_DIR") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/alt-ui")
}

pub fn docs_dir() -> String {
    format!("{}/docs", data_dir())
}

pub fn read_json(path: &str) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// One cell of the home grid.
pub struct DocInfo {
    pub id: String,
    pub title: String,
    pub kind: DocKind,
    pub folder: String, /* "" = root */
    pub pages: usize,   /* pdf pages (0 for notebooks) */
    pub pos: usize,     /* saved seq position, 0-based */
    pub seq_len: usize,
    pub opened: u64, /* state.json mtime, for ordering */
}

/// Every valid document, most recently opened first.
pub fn scan() -> Vec<DocInfo> {
    let dir = docs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else { return out };
    for e in rd.flatten() {
        let id = e.file_name().to_string_lossy().to_string();
        let base = format!("{dir}/{id}");
        let Some(meta) = read_json(&format!("{base}/meta.json")) else { continue };
        let pages = meta["pages"].as_u64().unwrap_or(0) as usize;
        let kind = match meta["kind"].as_str() {
            Some("notebook") => DocKind::Notebook,
            _ if pages > 0 => DocKind::Book,
            _ => continue, /* neither a notebook nor a plausible bundle */
        };
        let title = meta["title"].as_str().unwrap_or(&id).to_string();
        let folder = meta["folder"].as_str().unwrap_or("").to_string();
        let default_len = match kind {
            DocKind::Notebook => 1,
            DocKind::Book => pages,
        };
        let (pos, seq_len, opened) = match read_json(&format!("{base}/state.json")) {
            Some(st) => (
                st["pos"].as_u64().unwrap_or(0) as usize,
                st["seq"].as_array().map_or(default_len, |a| a.len()),
                std::fs::metadata(format!("{base}/state.json"))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs()),
            ),
            None => (0, default_len, 0),
        };
        out.push(DocInfo {
            id,
            title,
            kind,
            folder,
            pages,
            pos: pos.min(seq_len.saturating_sub(1)),
            seq_len,
            opened,
        });
    }
    out.sort_by(|a, b| b.opened.cmp(&a.opened).then_with(|| a.title.cmp(&b.title)));
    out
}

/* ---- mutations ------------------------------------------------------------ */

fn write_json(path: &str, v: &Value) -> bool {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, serde_json::to_vec(v).unwrap_or_default())
        .and_then(|_| std::fs::rename(&tmp, path))
        .is_ok()
}

fn update_meta(id: &str, f: impl FnOnce(&mut Value)) -> bool {
    let p = format!("{}/{id}/meta.json", docs_dir());
    let Some(mut meta) = read_json(&p) else { return false };
    f(&mut meta);
    write_json(&p, &meta)
}

fn epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn base36(mut n: u64) -> String {
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut s = Vec::new();
    loop {
        s.push(D[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// A brand-new empty notebook in `folder`; returns its id.
pub fn create_notebook(title: &str, folder: &str) -> Option<String> {
    let mut id = format!("nb-{}", base36(epoch()));
    let dir = docs_dir();
    let mut n = 1;
    while std::path::Path::new(&format!("{dir}/{id}")).exists() {
        n += 1;
        id = format!("nb-{}-{n}", base36(epoch()));
    }
    std::fs::create_dir_all(format!("{dir}/{id}/ink")).ok()?;
    let meta = json!({ "v": 1, "kind": "notebook", "title": title, "folder": folder, "created": epoch() });
    write_json(&format!("{dir}/{id}/meta.json"), &meta).then_some(id)
}

pub fn rename(id: &str, title: &str) -> bool {
    update_meta(id, |m| m["title"] = json!(title))
}

pub fn set_folder(id: &str, folder: &str) -> bool {
    update_meta(id, |m| m["folder"] = json!(folder))
}

pub fn delete(id: &str) -> bool {
    std::fs::remove_dir_all(format!("{}/{id}", docs_dir())).is_ok()
}

/// Recursive copy with " copy" suffixed to the title; returns the new id.
pub fn duplicate(id: &str) -> Option<String> {
    let dir = docs_dir();
    let mut new_id = format!("{id}-copy");
    let mut n = 1;
    while std::path::Path::new(&format!("{dir}/{new_id}")).exists() {
        n += 1;
        new_id = format!("{id}-copy-{n}");
    }
    copy_tree(&format!("{dir}/{id}"), &format!("{dir}/{new_id}")).ok()?;
    update_meta(&new_id, |m| {
        let t = m["title"].as_str().unwrap_or(id).to_string();
        m["title"] = json!(format!("{t} copy"));
    });
    Some(new_id)
}

fn copy_tree(src: &str, dst: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let (s, d) = (format!("{src}/{name}"), format!("{dst}/{name}"));
        if e.path().is_dir() {
            copy_tree(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

/* ---- folders ---------------------------------------------------------------
 * folders.json holds the authoritative list (so empty folders exist);
 * meta.json folder fields are unioned in defensively. */

fn folders_path() -> String {
    format!("{}/folders.json", data_dir())
}

pub fn folders() -> Vec<String> {
    let mut out: Vec<String> = read_json(&folders_path())
        .and_then(|v| {
            v["folders"].as_array().map(|a| {
                a.iter().filter_map(|f| f.as_str().map(String::from)).collect()
            })
        })
        .unwrap_or_default();
    for d in scan() {
        if !d.folder.is_empty() && !out.contains(&d.folder) {
            out.push(d.folder);
        }
    }
    out.sort();
    out
}

pub fn add_folder(name: &str) -> bool {
    let mut fs = folders();
    if name.is_empty() || fs.iter().any(|f| f == name) {
        return false;
    }
    fs.push(name.to_string());
    fs.sort();
    let _ = std::fs::create_dir_all(data_dir());
    write_json(&folders_path(), &json!({ "v": 1, "folders": fs }))
}

/* ---- legacy import ---------------------------------------------------------
 * One-time adoption of the sibling apps' data: reader book bundles move in
 * unchanged (they're byte-compatible), notebook pages become one imported
 * notebook. Same-filesystem renames — instant and hand-reversible. */

fn legacy_reader_dir() -> String {
    if let Ok(d) = std::env::var("PAPER_LEGACY_READER") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/reader/books")
}

fn legacy_notebook_dir() -> String {
    if let Ok(d) = std::env::var("PAPER_LEGACY_NOTEBOOK") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/notebook/pages")
}

/// Move legacy reader books + notebook pages into the store (idempotent
/// via a marker file). Returns (books, notebook pages) moved.
pub fn import_legacy() -> (usize, usize) {
    let marker = format!("{}/.import-done", data_dir());
    if std::path::Path::new(&marker).exists() {
        return (0, 0);
    }
    let dir = docs_dir();
    let _ = std::fs::create_dir_all(&dir);

    let mut books = 0;
    if let Ok(rd) = std::fs::read_dir(legacy_reader_dir()) {
        for e in rd.flatten() {
            let slug = e.file_name().to_string_lossy().to_string();
            if !e.path().is_dir() {
                continue;
            }
            let mut dest = format!("{dir}/{slug}");
            let mut n = 1;
            while std::path::Path::new(&dest).exists() {
                n += 1;
                dest = format!("{dir}/{slug}-{n}");
            }
            if std::fs::rename(e.path(), &dest).is_ok() {
                books += 1;
                println!("paper: imported book '{slug}'");
            }
        }
    }

    let mut pages = 0;
    let nb_src = legacy_notebook_dir();
    if let Ok(rd) = std::fs::read_dir(&nb_src) {
        let mut nums: Vec<u64> = rd
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.strip_prefix("page-")?.strip_suffix(".json")?.parse().ok()
            })
            .collect();
        nums.sort_unstable();
        if !nums.is_empty() {
            let nb = format!("{dir}/nb-imported");
            let _ = std::fs::create_dir_all(format!("{nb}/ink"));
            for &n in &nums {
                if std::fs::rename(
                    format!("{nb_src}/page-{n:04}.json"),
                    format!("{nb}/ink/note-{n:04}.json"),
                )
                .is_ok()
                {
                    pages += 1;
                }
            }
            let seq: Vec<Value> = nums.iter().map(|n| json!({ "n": n })).collect();
            let next = nums.last().copied().unwrap_or(0) + 1;
            write_json(
                &format!("{nb}/meta.json"),
                &json!({ "v": 1, "kind": "notebook", "title": "Notebook (imported)", "folder": "", "created": epoch() }),
            );
            write_json(
                &format!("{nb}/state.json"),
                &json!({ "v": 1, "seq": seq, "next_note": next, "pos": 0 }),
            );
            println!("paper: imported {pages} notebook pages as 'Notebook (imported)'");
        }
    }

    let _ = std::fs::write(&marker, b"1");
    (books, pages)
}

