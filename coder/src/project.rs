//! Projects: the sidebar's data source. One directory per project under
//! `~/.local/share/coder/projects/<slug>/`, containing:
//!
//!   meta.json    {"name","url","branch","summary"} — pi maintains it with
//!                its ordinary file tools when it clones/registers a repo
//!   SUMMARY.md   pi's living description of the codebase (DOCS reader)
//!   pages/       this project's ink pages (page-NNNN.json, managed by
//!                the app itself)
//!
//! The actual git clones live on the VM (`ssh exedev@remarkable.exe.xyz`,
//! `~/coder/<slug>`) — the tablet only ever holds metadata + ink. This
//! module only READS the metadata; pi writes it.
//!
//! A special always-present project `notes` (no repo) is the scratch pad
//! where clone requests and cross-repo questions live.

use serde_json::Value;

pub const NOTES_SLUG: &str = "notes";

#[derive(Clone)]
pub struct Project {
    pub slug: String,    /* dir name here AND repo dir name on the VM */
    pub name: String,    /* display name (meta.json "name", else slug) */
    pub url: String,     /* remote url; "" for notes/local-only */
    pub summary: String, /* one-liner for the sidebar + pause messages */
    pub pages: usize,    /* ink pages on disk (0 = none yet) */
    pub mtime: i64,      /* most recent activity (meta or pages) */
}

pub fn root() -> String {
    if let Ok(d) = std::env::var("CODER_DATA_DIR") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/coder")
}

pub fn projects_dir() -> String {
    format!("{}/projects", root())
}

pub fn dir_of(slug: &str) -> String {
    format!("{}/{}", projects_dir(), slug)
}

pub fn pages_dir(slug: &str) -> String {
    format!("{}/pages", dir_of(slug))
}

pub fn summary_path(slug: &str) -> String {
    format!("{}/SUMMARY.md", dir_of(slug))
}

/// A slug is a plain directory name — never a path.
pub fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Make sure the notes pad exists (first launch).
pub fn ensure_notes() {
    let dir = pages_dir(NOTES_SLUG);
    let _ = std::fs::create_dir_all(&dir);
}

fn mtime_of(path: &str) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn count_pages(slug: &str) -> usize {
    let dir = pages_dir(slug);
    let mut n = 0usize;
    while std::path::Path::new(&format!("{dir}/page-{:04}.json", n + 1)).exists() {
        n += 1;
    }
    n
}

/// Page/raster file paths for a project that is NOT the one on screen
/// (headless tool operations follow pi's turn, not the user's eyes).
pub fn page_path(slug: &str, i: usize) -> String {
    format!("{}/page-{:04}.json", pages_dir(slug), i + 1)
}

pub fn render_path(slug: &str, i: usize) -> String {
    format!("{}/render-{:04}.skr", pages_dir(slug), i + 1)
}

fn read_meta(slug: &str) -> (String, String, String) {
    let v = std::fs::read(format!("{}/meta.json", dir_of(slug)))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let name = v
        .as_ref()
        .and_then(|v| v["name"].as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(slug)
        .to_string();
    let url = v.as_ref().and_then(|v| v["url"].as_str()).unwrap_or("").to_string();
    let summary = v.as_ref().and_then(|v| v["summary"].as_str()).unwrap_or("").to_string();
    (name, url, summary)
}

pub fn get(slug: &str) -> Project {
    let (name, url, summary) = read_meta(slug);
    let mtime = mtime_of(&format!("{}/meta.json", dir_of(slug)))
        .max(mtime_of(&pages_dir(slug)))
        .max(mtime_of(&summary_path(slug)));
    Project { slug: slug.to_string(), name, url, summary, pages: count_pages(slug), mtime }
}

/// All projects: `notes` pinned first, the rest newest-activity first.
pub fn scan() -> Vec<Project> {
    ensure_notes();
    let mut out = vec![get(NOTES_SLUG)];
    let mut repos: Vec<Project> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(projects_dir()) {
        for e in rd.flatten() {
            let slug = e.file_name().to_string_lossy().to_string();
            if slug == NOTES_SLUG || !valid_slug(&slug) {
                continue;
            }
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            repos.push(get(&slug));
        }
    }
    repos.sort_by(|a, b| b.mtime.cmp(&a.mtime).then(a.slug.cmp(&b.slug)));
    out.extend(repos);
    out
}

pub fn exists(slug: &str) -> bool {
    valid_slug(slug) && std::path::Path::new(&dir_of(slug)).is_dir()
}

/// The DOCS reader's list: projects that have a SUMMARY.md.
pub struct DocItem {
    pub slug: String,
    pub title: String,
    pub date: String,
    pub kb: u64,
}

pub fn docs() -> Vec<DocItem> {
    let mut items: Vec<(i64, DocItem)> = Vec::new();
    for p in scan() {
        let path = summary_path(&p.slug);
        let Ok(md) = std::fs::metadata(&path) else { continue };
        let title = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| title_of(&s))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| p.name.clone());
        let mtime = mtime_of(&path);
        items.push((
            mtime,
            DocItem { slug: p.slug.clone(), title, date: ymd(mtime), kb: md.len().div_ceil(1024) },
        ));
    }
    items.sort_by_key(|(m, _)| -*m);
    items.into_iter().map(|(_, i)| i).collect()
}

pub fn read_summary(slug: &str) -> Option<String> {
    if !valid_slug(slug) {
        return None;
    }
    std::fs::read_to_string(summary_path(slug)).ok()
}

/// Title: YAML frontmatter `title:` if present, else the first heading /
/// non-empty line.
fn title_of(s: &str) -> Option<String> {
    let mut lines = s.lines();
    if s.starts_with("---") {
        lines.next();
        for l in lines.by_ref() {
            if l.trim() == "---" {
                break;
            }
            if let Some(t) = l.strip_prefix("title:") {
                return Some(t.trim().trim_matches('"').to_string());
            }
        }
    }
    lines
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim_start_matches('#').trim().to_string())
}

/// Unix seconds -> "yyyy-mm-dd" (civil-from-days, Hinnant's algorithm).
fn ymd(secs: i64) -> String {
    let z = secs.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
