//! The book model: a PDF pre-rendered on the desk side plus everything the
//! tablet adds on top.
//!
//! A book is a directory under the books dir (tools/mkbook.py builds and
//! pushes it):
//!
//!   meta.json        { title, pages }              — from mkbook
//!   pages/0001.png   full-screen 1404x1872 gray8   — from mkbook
//!   text/0001.json   { text, words:[[x0,y0,x1,y1,w],..] } in page px
//!   state.json       { seq, next_note, pos }       — OURS (survives re-push)
//!   ink/pdf-0001.json, ink/note-0001.json          — ink overlays (ink.rs)
//!
//! The reading order (`seq`) is PDF pages interleaved with inserted note
//! pages; "page N" everywhere in the UI and in pi's tools means the N-th
//! entry of that sequence, 1-based. Ink lives in per-ENTRY files keyed by
//! pdf page number / note id, so inserting a note never renumbers anyone's
//! ink.

use crate::fb::{Framebuffer, SCREEN_H, SCREEN_W};
use crate::ink::Page;
use crate::png_dec;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
pub enum Entry {
    Pdf(usize), /* 0-based pdf page */
    Note(u64),  /* note id */
}

pub struct Word {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub text: String,
}

/// One row of the home screen's book list.
pub struct BookInfo {
    pub slug: String,
    pub title: String,
    pub pages: usize,   /* pdf pages */
    pub pos: usize,     /* saved seq position, 0-based */
    pub seq_len: usize, /* pdf pages + notes */
    pub opened: u64,    /* state.json mtime, for ordering */
}

pub fn books_dir() -> String {
    if let Ok(d) = std::env::var("READER_BOOKS") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/reader/books")
}

fn read_json(path: &str) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Every valid book bundle, most recently opened first.
pub fn scan() -> Vec<BookInfo> {
    let dir = books_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else { return out };
    for e in rd.flatten() {
        let slug = e.file_name().to_string_lossy().to_string();
        let base = format!("{dir}/{slug}");
        let Some(meta) = read_json(&format!("{base}/meta.json")) else { continue };
        let pages = meta["pages"].as_u64().unwrap_or(0) as usize;
        if pages == 0 {
            continue;
        }
        let title = meta["title"].as_str().unwrap_or(&slug).to_string();
        let (pos, seq_len, opened) = match read_json(&format!("{base}/state.json")) {
            Some(st) => (
                st["pos"].as_u64().unwrap_or(0) as usize,
                st["seq"].as_array().map_or(pages, |a| a.len()),
                std::fs::metadata(format!("{base}/state.json"))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs()),
            ),
            None => (0, pages, 0),
        };
        out.push(BookInfo { slug, title, pages, pos: pos.min(seq_len.saturating_sub(1)), seq_len, opened });
    }
    out.sort_by(|a, b| b.opened.cmp(&a.opened).then_with(|| a.title.cmp(&b.title)));
    out
}

pub struct Book {
    pub dir: String,
    pub slug: String,
    pub title: String,
    pub pdf_pages: usize,
    pub seq: Vec<Entry>,
    next_note: u64,
    pub current: usize, /* seq index */
    pub page: Page,     /* ink overlay of the current entry */
    raster: Option<Vec<u8>>, /* decoded gray8 SCREEN_W x SCREEN_H, None on notes */
    state_dirty: bool,
}

impl Book {
    pub fn open(slug: &str) -> Option<Book> {
        let dir = format!("{}/{slug}", books_dir());
        let meta = read_json(&format!("{dir}/meta.json"))?;
        let pdf_pages = meta["pages"].as_u64().unwrap_or(0) as usize;
        if pdf_pages == 0 {
            return None;
        }
        let title = meta["title"].as_str().unwrap_or(slug).to_string();
        let _ = std::fs::create_dir_all(format!("{dir}/ink"));

        let (seq, next_note, pos) = match read_json(&format!("{dir}/state.json")) {
            Some(st) => {
                let seq: Vec<Entry> = st["seq"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| {
                                if let Some(p) = v["p"].as_u64() {
                                    /* a re-pushed shorter PDF may strand refs */
                                    ((p as usize) < pdf_pages).then_some(Entry::Pdf(p as usize))
                                } else {
                                    v["n"].as_u64().map(Entry::Note)
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let seq = if seq.is_empty() { default_seq(pdf_pages) } else { seq };
                (
                    seq,
                    st["next_note"].as_u64().unwrap_or(1),
                    st["pos"].as_u64().unwrap_or(0) as usize,
                )
            }
            None => (default_seq(pdf_pages), 1, 0),
        };

        let mut b = Book {
            dir,
            slug: slug.to_string(),
            title,
            pdf_pages,
            seq,
            next_note,
            current: 0,
            page: Page::default(),
            raster: None,
            state_dirty: false,
        };
        b.goto(pos.min(b.seq.len().saturating_sub(1)));
        b.save_state(); /* stamps "opened" for the home ordering */
        Some(b)
    }

    pub fn count(&self) -> usize {
        self.seq.len()
    }

    pub fn entry(&self, i: usize) -> Option<Entry> {
        self.seq.get(i).copied()
    }

    /// The short label for entry `i`: its printed-page number or "note".
    pub fn label(&self, i: usize) -> String {
        match self.seq.get(i) {
            Some(Entry::Pdf(p)) => format!("p.{}", p + 1),
            Some(Entry::Note(_)) => "note".into(),
            None => String::new(),
        }
    }

    fn ink_path(&self, e: Entry) -> String {
        match e {
            Entry::Pdf(p) => format!("{}/ink/pdf-{:04}.json", self.dir, p + 1),
            Entry::Note(n) => format!("{}/ink/note-{:04}.json", self.dir, n),
        }
    }

    fn raster_path(&self, pdf_page: usize) -> String {
        format!("{}/pages/{:04}.png", self.dir, pdf_page + 1)
    }

    fn text_path(&self, pdf_page: usize) -> String {
        format!("{}/text/{:04}.json", self.dir, pdf_page + 1)
    }

    /// Decode the full-screen raster for a pdf page (None for missing files
    /// or decode errors — the page then renders as blank paper).
    pub fn load_raster(&self, pdf_page: usize) -> Option<Vec<u8>> {
        let data = std::fs::read(self.raster_path(pdf_page)).ok()?;
        match png_dec::decode_png_gray(&data) {
            Ok((w, h, buf)) if w == SCREEN_W as u32 && h == SCREEN_H as u32 => Some(buf),
            Ok((w, h, _)) => {
                eprintln!("reader: page {} raster is {w}x{h}, want {SCREEN_W}x{SCREEN_H}", pdf_page + 1);
                None
            }
            Err(e) => {
                eprintln!("reader: page {} png: {e}", pdf_page + 1);
                None
            }
        }
    }

    pub fn load_ink(&self, e: Entry) -> Page {
        Page::load(&self.ink_path(e)).unwrap_or_default()
    }

    /// The extracted text of a pdf page (empty if missing).
    pub fn page_text(&self, pdf_page: usize) -> String {
        read_json(&self.text_path(pdf_page))
            .and_then(|v| v["text"].as_str().map(String::from))
            .unwrap_or_default()
    }

    /// The word boxes of a pdf page, in page pixels.
    pub fn words(&self, pdf_page: usize) -> Vec<Word> {
        let Some(v) = read_json(&self.text_path(pdf_page)) else { return Vec::new() };
        let Some(arr) = v["words"].as_array() else { return Vec::new() };
        arr.iter()
            .filter_map(|w| {
                let a = w.as_array()?;
                Some(Word {
                    x0: a.first()?.as_i64()? as i32,
                    y0: a.get(1)?.as_i64()? as i32,
                    x1: a.get(2)?.as_i64()? as i32,
                    y1: a.get(3)?.as_i64()? as i32,
                    text: a.get(4)?.as_str()?.to_string(),
                })
            })
            .collect()
    }

    /* -- navigation ---------------------------------------------------- */

    /// Jump straight to seq index `i` (saves the outgoing entry's ink).
    pub fn goto(&mut self, i: usize) {
        let i = i.min(self.seq.len().saturating_sub(1));
        self.save_current_ink();
        self.current = i;
        let e = self.seq[i];
        self.page = self.load_ink(e);
        self.raster = match e {
            Entry::Pdf(p) => self.load_raster(p),
            Entry::Note(_) => None,
        };
        self.state_dirty = true;
    }

    pub fn flip(&mut self, delta: i32) -> bool {
        let t = self.current as i32 + delta;
        if t < 0 || t as usize >= self.seq.len() {
            return false;
        }
        self.goto(t as usize);
        true
    }

    /// Insert a fresh note page after seq index `after`; returns its index.
    pub fn insert_note(&mut self, after: usize) -> usize {
        let id = self.next_note;
        self.next_note += 1;
        let at = (after + 1).min(self.seq.len());
        self.seq.insert(at, Entry::Note(id));
        if self.current >= at {
            self.current += 1; /* the page on screen kept its identity */
        }
        self.state_dirty = true;
        self.save_state();
        at
    }

    /* -- persistence ---------------------------------------------------- */

    pub fn save_current_ink(&mut self) {
        if !self.page.dirty {
            return;
        }
        let Some(&e) = self.seq.get(self.current) else { return };
        let path = self.ink_path(e);
        if let Err(err) = self.page.save(&path) {
            eprintln!("reader: save {path}: {err}");
        }
    }

    pub fn save_state(&mut self) {
        let seq: Vec<Value> = self
            .seq
            .iter()
            .map(|e| match e {
                Entry::Pdf(p) => json!({ "p": p }),
                Entry::Note(n) => json!({ "n": n }),
            })
            .collect();
        let doc = json!({
            "v": 1,
            "seq": seq,
            "next_note": self.next_note,
            "pos": self.current,
        });
        let path = format!("{}/state.json", self.dir);
        let tmp = format!("{path}.tmp");
        if std::fs::write(&tmp, serde_json::to_vec(&doc).unwrap_or_default())
            .and_then(|_| std::fs::rename(&tmp, &path))
            .is_err()
        {
            eprintln!("reader: could not save {path}");
        }
        self.state_dirty = false;
    }

    pub fn save_all(&mut self) {
        self.save_current_ink();
        if self.state_dirty {
            self.save_state();
        }
    }

    /* -- rendering ------------------------------------------------------ */

    /// Paint the background of region `r`: the page raster, or white for
    /// note pages / missing rasters.
    fn paint_background(&self, fb: &mut Framebuffer, r: crate::ink::Rect) {
        let r = r.clamp_screen();
        match &self.raster {
            Some(buf) => {
                for y in r.y0..=r.y1 {
                    let row = &buf[(y * SCREEN_W) as usize..((y + 1) * SCREEN_W) as usize];
                    let px = fb.pixels();
                    for x in r.x0..=r.x1 {
                        let g = row[x as usize] as u16;
                        px[(y * SCREEN_W + x) as usize] =
                            ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3);
                    }
                }
            }
            None => fb.fill_rect(r.x0, r.y0, r.w(), r.h(), crate::draw::WHITE),
        }
    }

    /// Re-render a region: raster (or white) + every stroke, darkest-wins.
    pub fn render_region(&self, fb: &mut Framebuffer, r: crate::ink::Rect) -> bool {
        self.paint_background(fb, r);
        self.page.stamp_region(fb, r);
        false
    }

    pub fn render_full(&self, fb: &mut Framebuffer) {
        self.render_region(
            fb,
            crate::ink::Rect { x0: 0, y0: 0, x1: SCREEN_W - 1, y1: SCREEN_H - 1 },
        );
    }

    /// Snapshot the CURRENT entry for pi at 1/`div` scale: downsampled
    /// raster, user ink black, AI ink gray.
    pub fn snapshot(&self, div: i32) -> (i32, i32, Vec<u8>) {
        let (w, h) = (SCREEN_W / div, SCREEN_H / div);
        let mut buf = downsample(self.raster.as_deref(), div);
        self.page.snapshot_into(&mut buf, div);
        (w, h, buf)
    }

    /// Snapshot ANY entry (loads ink + raster from disk unless current).
    pub fn snapshot_of(&self, i: usize, div: i32) -> Option<(i32, i32, Vec<u8>, Page)> {
        if i == self.current {
            let (w, h, buf) = self.snapshot(div);
            return Some((w, h, buf, Page::default())); /* caller uses self.page */
        }
        let e = *self.seq.get(i)?;
        let ink = self.load_ink(e);
        let raster = match e {
            Entry::Pdf(p) => self.load_raster(p),
            Entry::Note(_) => None,
        };
        let (w, h) = (SCREEN_W / div, SCREEN_H / div);
        let mut buf = downsample(raster.as_deref(), div);
        ink.snapshot_into(&mut buf, div);
        Some((w, h, buf, ink))
    }
}

fn default_seq(pdf_pages: usize) -> Vec<Entry> {
    (0..pdf_pages).map(Entry::Pdf).collect()
}

/// Box-filter a full-screen gray raster down by `div` (white when absent).
fn downsample(raster: Option<&[u8]>, div: i32) -> Vec<u8> {
    let (w, h) = (SCREEN_W / div, SCREEN_H / div);
    let Some(src) = raster else {
        return vec![255u8; (w * h) as usize];
    };
    let mut out = vec![255u8; (w * h) as usize];
    let n = (div * div) as u32;
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0u32;
            for j in 0..div {
                for i in 0..div {
                    acc += src[((y * div + j) * SCREEN_W + x * div + i) as usize] as u32;
                }
            }
            out[(y * w + x) as usize] = (acc / n) as u8;
        }
    }
    out
}

/* ---- phrase underlining ---------------------------------------------------- */

fn norm_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Find occurrence `nth` (1-based) of `phrase` in the page's words and
/// return (matched word boxes, total matches). Matching is case- and
/// punctuation-insensitive and heals end-of-line hyphenation.
pub fn find_phrase(words: &[Word], phrase: &str, nth: usize) -> (Option<Vec<usize>>, usize) {
    let want: Vec<String> = phrase.split_whitespace().map(norm_token).filter(|t| !t.is_empty()).collect();
    if want.is_empty() || words.is_empty() {
        return (None, 0);
    }
    let toks: Vec<String> = words.iter().map(|w| norm_token(&w.text)).collect();
    let hyphen: Vec<bool> = words.iter().map(|w| w.text.trim_end().ends_with('-')).collect();

    let mut matches: Vec<Vec<usize>> = Vec::new();
    for start in 0..words.len() {
        let (mut i, mut k) = (start, 0usize);
        let mut used: Vec<usize> = Vec::new();
        while k < want.len() && i < words.len() {
            if toks[i].is_empty() {
                if used.is_empty() {
                    break; /* don't start a match on punctuation */
                }
                used.push(i); /* punctuation inside the phrase: absorb */
                i += 1;
                continue;
            }
            if toks[i] == want[k] {
                used.push(i);
                i += 1;
                k += 1;
            } else if hyphen[i]
                && i + 1 < words.len()
                && format!("{}{}", toks[i], toks[i + 1]) == want[k]
            {
                used.push(i);
                used.push(i + 1);
                i += 2;
                k += 1;
            } else {
                break;
            }
        }
        if k == want.len() {
            matches.push(used);
        }
    }
    let total = matches.len();
    let picked = matches.into_iter().nth(nth.saturating_sub(1));
    (picked, total)
}

/// Build hand-drawn-looking underline strokes beneath the given word boxes
/// (grouped into visual lines).
pub fn underline_strokes(words: &[Word], picked: &[usize]) -> Vec<crate::ink::Stroke> {
    use crate::ink::{Pt, Stroke};
    /* group into lines: consecutive boxes whose vertical centers agree */
    let mut lines: Vec<Vec<&Word>> = Vec::new();
    for &i in picked {
        let w = &words[i];
        match lines.last_mut() {
            Some(line) => {
                let prev = line.last().unwrap();
                let (cy, pcy) = ((w.y0 + w.y1) / 2, (prev.y0 + prev.y1) / 2);
                let tol = ((prev.y1 - prev.y0).max(w.y1 - w.y0) / 2).max(6);
                if (cy - pcy).abs() <= tol && w.x0 >= prev.x0 - 40 {
                    line.push(w);
                } else {
                    lines.push(vec![w]);
                }
            }
            None => lines.push(vec![w]),
        }
    }
    let mut out = Vec::new();
    for line in lines {
        let x0 = line.iter().map(|w| w.x0).min().unwrap() as f32 - 4.0;
        let x1 = line.iter().map(|w| w.x1).max().unwrap() as f32 + 4.0;
        let y = line.iter().map(|w| w.y1).max().unwrap() as f32 + 7.0;
        let mut pts = Vec::new();
        let n = (((x1 - x0) / 14.0).ceil() as i32).max(2);
        for i in 0..=n {
            let t = i as f32 / n as f32;
            pts.push(Pt {
                x: x0 + (x1 - x0) * t,
                y: (y + (t * 19.0).sin() * 1.4).min(SCREEN_H as f32 - 2.0),
                r: 1.7,
            });
        }
        out.push(Stroke { pts, gray: crate::ink::AI_GRAY });
    }
    out
}
