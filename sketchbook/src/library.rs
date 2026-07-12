//! The library: a directory of markdown files pi curates — distilled web
//! finds, reference notes, things the user asked to keep. pi manages it
//! with its ordinary file tools (the conventions live in its system
//! prompt); this module only READS it for the on-device browser.
//!
//! Conventions: flat dir, one item per `*.md` file, kebab-case filename,
//! first `# ` line is the title.

pub struct LibItem {
    pub file: String,  /* filename inside the library dir */
    pub title: String, /* first heading, else the filename */
    pub date: String,  /* yyyy-mm-dd from mtime */
    pub kb: u64,
}

pub fn dir() -> String {
    if let Ok(d) = std::env::var("SKETCHBOOK_LIBRARY") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/sketchbook/library")
}

/// All items, newest first.
pub fn scan() -> Vec<LibItem> {
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut items: Vec<(i64, LibItem)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let title = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|s| title_of(&s))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| name.trim_end_matches(".md").replace('-', " "));
        items.push((
            mtime,
            LibItem { file: name, title, date: ymd(mtime), kb: md.len().div_ceil(1024) },
        ));
    }
    items.sort_by_key(|(m, _)| -*m);
    items.into_iter().map(|(_, i)| i).collect()
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

pub fn read(file: &str) -> Option<String> {
    /* the browser only ever passes names scan() produced, but never follow
     * a path component anywhere */
    if file.contains('/') || file.contains("..") {
        return None;
    }
    std::fs::read_to_string(format!("{}/{}", dir(), file)).ok()
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
