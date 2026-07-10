//! Read the tablet's OWN document store (xochitl) — the stock app keeps
//! every synced document as <uuid>.pdf + <uuid>.metadata JSON under one
//! flat directory. Listing it locally is what makes the reader's TABLET
//! LIBRARY view always current: there is nothing to sync, we just look.

use serde_json::Value;

pub fn dir() -> String {
    if let Ok(d) = std::env::var("READER_XOCHITL") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/remarkable/xochitl")
}

pub struct XDoc {
    pub uuid: String,
    pub name: String,
    pub kb: u64,
    pub mtime: u64, /* xochitl lastModified, ms */
}

fn read_meta(base: &str, uuid: &str) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(format!("{base}/{uuid}.metadata")).ok()?).ok()
}

/// Every non-trashed PDF document, newest first.
pub fn scan_pdfs() -> Vec<XDoc> {
    let base = dir();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&base) else { return out };
    for e in rd.flatten() {
        let fname = e.file_name().to_string_lossy().to_string();
        let Some(uuid) = fname.strip_suffix(".metadata") else { continue };
        let Some(meta) = read_meta(&base, uuid) else { continue };
        if meta["deleted"].as_bool().unwrap_or(false) {
            continue;
        }
        let parent = meta["parent"].as_str().unwrap_or("").to_string();
        if parent == "trash" {
            continue;
        }
        if !matches!(meta["type"].as_str(), None | Some("DocumentType")) {
            continue;
        }
        let Ok(pdf) = std::fs::metadata(format!("{base}/{uuid}.pdf")) else { continue };
        let mtime = meta["lastModified"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .or(meta["lastModified"].as_u64())
            .unwrap_or(0);
        out.push(XDoc {
            uuid: uuid.to_string(),
            name: meta["visibleName"].as_str().unwrap_or(uuid).trim().to_string(),
            kb: pdf.len() / 1024,
            mtime,
        });
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    out
}

pub fn pdf_path(uuid: &str) -> String {
    format!("{}/{uuid}.pdf", dir())
}

pub fn slugify(name: &str) -> String {
    let mut s: String = name
        .trim_end_matches(".pdf")
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    let mut s: String = s.chars().take(60).collect();
    if s.is_empty() {
        s = "book".into();
    }
    s
}
