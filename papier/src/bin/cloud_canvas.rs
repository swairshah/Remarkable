//! papier cloud-canvas — pi's page tools executed against a synced copy of
//! a document on the VM, using the SAME libreink crates as the tablet, so
//! cloud pi ink is pixel-identical to on-device pi ink.
//!
//! The pi session service (papier-upload.js) speaks the tablet's tool
//! protocol on a unix socket and shells each command into this binary:
//! one JSON command per stdin line -> one JSON result per stdout line
//! (same shapes as papier's handle_ipc_request; `goto` never reaches us —
//! the service answers it by queueing an event for the iPad).
//!
//! Reads resolve overlay-first (the inbound tree, where iPad + pi writes
//! live) and fall back to the mirror (the tablet's pushed truth); ALL
//! writes land in the overlay, so the tablet's next pull applies them.
//!
//!   PAPIER_CLOUD_MIRROR   mirror doc dir  (…/papier/docs/<id>)
//!   PAPIER_CLOUD_OVERLAY  overlay doc dir (…/papier-inbound/docs/<id>)
//!   PAPIER_CLOUD_FONT     default pi face key (serif|script|sans|garamond)

use libreink_core::fb::{SCREEN_H, SCREEN_W};
use libreink_core::{png, png_dec};
use libreink_page::{patch_bbox, Page, Pt, Stroke, AI_GRAY};
use libreink_svg::{self as svg_ink, PiFont};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::OnceLock;

const SNAP_DIV: i32 = 2;

#[derive(Clone, Copy, PartialEq)]
enum Entry {
    Pdf(usize),
    Note(u64),
}

struct DocView {
    mirror: String,
    overlay: String,
}

impl DocView {
    fn from_env() -> DocView {
        DocView {
            mirror: std::env::var("PAPIER_CLOUD_MIRROR").unwrap_or_default(),
            overlay: std::env::var("PAPIER_CLOUD_OVERLAY").unwrap_or_default(),
        }
    }

    /// Effective READ path: overlay copy when present, else the mirror's.
    fn read_path(&self, rel: &str) -> String {
        let o = format!("{}/{rel}", self.overlay);
        if std::path::Path::new(&o).exists() {
            o
        } else {
            format!("{}/{rel}", self.mirror)
        }
    }

    /// WRITE path: always the overlay (parents created).
    fn write_path(&self, rel: &str) -> String {
        let p = format!("{}/{rel}", self.overlay);
        if let Some(parent) = std::path::Path::new(&p).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        p
    }

    fn read_json(&self, rel: &str) -> Option<Value> {
        let bytes = std::fs::read(self.read_path(rel)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn meta(&self) -> Value {
        self.read_json("meta.json").unwrap_or_else(|| json!({}))
    }

    fn is_notebook(&self) -> bool {
        self.meta()["kind"].as_str() == Some("notebook")
    }

    fn pdf_pages(&self) -> usize {
        self.meta()["pages"].as_u64().unwrap_or(0) as usize
    }

    /// (seq, next_note) — state.json when present, defaults otherwise.
    fn state(&self) -> (Vec<Entry>, u64) {
        if let Some(st) = self.read_json("state.json") {
            if let Some(arr) = st["seq"].as_array() {
                let seq: Vec<Entry> = arr
                    .iter()
                    .filter_map(|v| {
                        if let Some(p) = v["p"].as_u64() {
                            Some(Entry::Pdf(p as usize))
                        } else {
                            v["n"].as_u64().map(Entry::Note)
                        }
                    })
                    .collect();
                let max_note = seq
                    .iter()
                    .filter_map(|e| if let Entry::Note(n) = e { Some(*n) } else { None })
                    .max()
                    .unwrap_or(0);
                let next = st["next_note"].as_u64().unwrap_or(0).max(max_note + 1);
                if !seq.is_empty() {
                    return (seq, next);
                }
            }
        }
        if self.is_notebook() {
            (vec![Entry::Note(1)], 2)
        } else {
            ((0..self.pdf_pages()).map(Entry::Pdf).collect(), 1)
        }
    }

    fn label(&self, seq: &[Entry], i: usize) -> String {
        match seq.get(i) {
            Some(Entry::Pdf(p)) => format!("p.{}", p + 1),
            Some(Entry::Note(_)) => "note".into(),
            None => String::new(),
        }
    }

    fn ink_rel(e: Entry) -> String {
        match e {
            Entry::Pdf(p) => format!("ink/pdf-{:04}.json", p + 1),
            Entry::Note(n) => format!("ink/note-{:04}.json", n),
        }
    }

    fn load_ink(&self, e: Entry) -> Page {
        Page::load(&self.read_path(&Self::ink_rel(e))).unwrap_or_default()
    }

    fn save_ink(&self, e: Entry, page: &mut Page) -> Result<(), String> {
        page.save(&self.write_path(&Self::ink_rel(e)))
            .map_err(|e| e.to_string())
    }

    fn load_raster(&self, pdf_page: usize) -> Option<Vec<u8>> {
        let rel = format!("pages/{:04}.png", pdf_page + 1);
        let data = std::fs::read(self.read_path(&rel)).ok()?;
        match png_dec::decode_png_gray(&data) {
            Ok((w, h, mut buf)) if w == SCREEN_W as u32 && h == SCREEN_H as u32 => {
                boost_contrast(&mut buf);
                Some(buf)
            }
            _ => None,
        }
    }

    fn page_text(&self, pdf_page: usize) -> String {
        self.read_json(&format!("text/{:04}.json", pdf_page + 1))
            .and_then(|v| v["text"].as_str().map(String::from))
            .unwrap_or_default()
    }

    fn words(&self, pdf_page: usize) -> Vec<Word> {
        let Some(v) = self.read_json(&format!("text/{:04}.json", pdf_page + 1)) else {
            return Vec::new();
        };
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
}

/* ---- copied verbatim from papier's doc.rs (same rendering/matching) ---- */

struct Word {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    text: String,
}

fn boost_contrast(buf: &mut [u8]) {
    static LUT: OnceLock<[u8; 256]> = OnceLock::new();
    let lut = LUT.get_or_init(|| {
        const BLACK: f32 = 30.0;
        const WHITE: f32 = 250.0;
        let mut t = [0u8; 256];
        for (g, o) in t.iter_mut().enumerate() {
            *o = (((g as f32 - BLACK) / (WHITE - BLACK)) * 255.0).clamp(0.0, 255.0) as u8;
        }
        t
    });
    for p in buf.iter_mut() {
        *p = lut[*p as usize];
    }
}

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

fn norm_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn find_phrase(words: &[Word], phrase: &str, nth: usize) -> (Option<Vec<usize>>, usize) {
    let want: Vec<String> =
        phrase.split_whitespace().map(norm_token).filter(|t| !t.is_empty()).collect();
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
                    break;
                }
                used.push(i);
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

fn underline_strokes(words: &[Word], picked: &[usize]) -> Vec<Stroke> {
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
        out.push(Stroke { id: 0, pts, gray: AI_GRAY });
    }
    out
}

/* ---- command handlers -------------------------------------------------- */

fn req_page(req: &Value, count: usize) -> Result<usize, Value> {
    match req["page"].as_u64() {
        Some(p) if p >= 1 && (p as usize) <= count => Ok(p as usize - 1),
        Some(p) => Err(json!({ "ok": false, "error": format!("no page {p} (document has {count})") })),
        None => Err(json!({ "ok": false, "error": "page required (the session passes the current page)" })),
    }
}

fn default_font() -> PiFont {
    std::env::var("PAPIER_CLOUD_FONT")
        .ok()
        .and_then(|k| PiFont::from_key(&k))
        .unwrap_or(PiFont::Serif)
}

fn add_patch(d: &DocView, e: Entry, strokes: Vec<Stroke>, texts: Vec<libreink_page::TextRun>) -> Result<(u64, Option<[i32; 4]>), String> {
    let mut page = d.load_ink(e);
    let id = page.add_patch(strokes, texts);
    let bbox = patch_bbox(page.patches.last().unwrap()).map(|b| [b.x0, b.y0, b.x1, b.y1]);
    d.save_ink(e, &mut page)?;
    Ok((id, bbox))
}

fn handle(d: &DocView, req: &Value) -> Value {
    let (seq, next_note) = d.state();
    let count = seq.len();
    match req["cmd"].as_str().unwrap_or("") {
        "view" => {
            let idx = match req_page(req, count) { Ok(i) => i, Err(e) => return e };
            let e = seq[idx];
            let ink = d.load_ink(e);
            let raster = match e {
                Entry::Pdf(p) => d.load_raster(p),
                Entry::Note(_) => None,
            };
            let (w, h) = (SCREEN_W / SNAP_DIV, SCREEN_H / SNAP_DIV);
            let mut buf = downsample(raster.as_deref(), SNAP_DIV);
            ink.snapshot_into(&mut buf, SNAP_DIV);
            let data = png::encode_gray(w as u32, h as u32, &buf);
            let patches: Vec<Value> = ink
                .patches
                .iter()
                .map(|p| {
                    json!({
                        "id": p.id,
                        "bbox": patch_bbox(p).map(|b| json!([b.x0, b.y0, b.x1, b.y1])).unwrap_or(json!(null)),
                    })
                })
                .collect();
            json!({
                "ok": true, "page": idx + 1, "page_count": count,
                "label": d.label(&seq, idx),
                "page_width": SCREEN_W, "page_height": SCREEN_H,
                "image_scale": SNAP_DIV,
                "png_base64": png::base64(&data),
                "patches": patches,
            })
        }
        "draw" => {
            let Some(svg) = req["svg"].as_str() else {
                return json!({ "ok": false, "error": "missing 'svg'" });
            };
            let idx = match req_page(req, count) { Ok(i) => i, Err(e) => return e };
            let (strokes, texts, notes) = match svg_ink::parse(svg, 1.0, default_font()) {
                Ok(v) => v,
                Err(e) => return json!({ "ok": false, "error": e }),
            };
            match add_patch(d, seq[idx], strokes, texts) {
                Ok((id, bbox)) => json!({
                    "ok": true, "id": id, "page": idx + 1,
                    "bbox": bbox.map(|b| json!(b)).unwrap_or(json!(null)),
                    "layout": "", "notes": notes,
                }),
                Err(e) => json!({ "ok": false, "error": format!("save: {e}") }),
            }
        }
        "underline" => {
            let Some(phrase) = req["phrase"].as_str().filter(|p| !p.trim().is_empty()) else {
                return json!({ "ok": false, "error": "missing 'phrase'" });
            };
            let nth = req["occurrence"].as_u64().unwrap_or(1).max(1) as usize;
            let idx = match req_page(req, count) { Ok(i) => i, Err(e) => return e };
            let Entry::Pdf(p) = seq[idx] else {
                return json!({ "ok": false, "error": "nothing printed on that page to underline" });
            };
            let words = d.words(p);
            if words.is_empty() {
                return json!({ "ok": false, "error": "no word geometry for this page" });
            }
            let (picked, total) = find_phrase(&words, phrase, nth);
            let Some(picked) = picked else {
                let err = if total == 0 {
                    format!("phrase not found on page {} — quote it exactly as it appears (matching ignores case and punctuation)", idx + 1)
                } else {
                    format!("only {total} occurrence(s) on page {}", idx + 1)
                };
                return json!({ "ok": false, "error": err, "matches": total });
            };
            let strokes = underline_strokes(&words, &picked);
            match add_patch(d, seq[idx], strokes, Vec::new()) {
                Ok((id, bbox)) => json!({
                    "ok": true, "id": id, "page": idx + 1, "matches": total,
                    "bbox": bbox.map(|b| json!(b)).unwrap_or(json!(null)),
                }),
                Err(e) => json!({ "ok": false, "error": format!("save: {e}") }),
            }
        }
        "erase" => {
            let Some(id) = req["id"].as_u64() else {
                return json!({ "ok": false, "error": "missing 'id'" });
            };
            let idx = match req_page(req, count) { Ok(i) => i, Err(e) => return e };
            let e = seq[idx];
            let mut page = d.load_ink(e);
            if page.remove_patch(id).is_none() {
                return json!({ "ok": false, "error": format!("no patch #{id} on page {}", idx + 1) });
            }
            match d.save_ink(e, &mut page) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "ok": false, "error": format!("save: {e}") }),
            }
        }
        "insert_note" => {
            let after = match req["after_page"].as_u64() {
                Some(p) if p >= 1 && (p as usize) <= count => p as usize - 1,
                Some(p) => return json!({ "ok": false, "error": format!("no page {p} (document has {count})") }),
                None => return json!({ "ok": false, "error": "after_page required (the session passes the current page)" }),
            };
            let mut new_seq: Vec<Value> = Vec::new();
            for (i, e) in seq.iter().enumerate() {
                new_seq.push(match e {
                    Entry::Pdf(p) => json!({ "p": p }),
                    Entry::Note(n) => json!({ "n": n }),
                });
                if i == after {
                    new_seq.push(json!({ "n": next_note }));
                }
            }
            let pos = d
                .read_json("state.json")
                .and_then(|s| s["pos"].as_u64())
                .unwrap_or(after as u64);
            let state = json!({ "next_note": next_note + 1, "pos": pos, "seq": new_seq });
            let path = d.write_path("state.json");
            if let Err(e) = std::fs::write(&path, serde_json::to_vec(&state).unwrap()) {
                return json!({ "ok": false, "error": format!("state save: {e}") });
            }
            let mut blank = Page::default();
            let _ = d.save_ink(Entry::Note(next_note), &mut blank);
            json!({ "ok": true, "page": after + 2, "page_count": count + 1 })
        }
        "page_text" => {
            let from = req["from"].as_u64().unwrap_or(0);
            if from < 1 || from as usize > count {
                return json!({ "ok": false, "error": format!("no page {from} (document has {count})") });
            }
            let to = req["to"].as_u64().unwrap_or(from).clamp(from, (from + 7).min(count as u64));
            let mut out = String::new();
            for i in (from as usize - 1)..(to as usize) {
                match seq[i] {
                    Entry::Pdf(p) => {
                        out.push_str(&format!("--- page {} (p.{}) ---\n{}\n", i + 1, p + 1, d.page_text(p)));
                    }
                    Entry::Note(_) => {
                        out.push_str(&format!("--- page {} (note page — handwriting only, use canvas_view) ---\n", i + 1));
                    }
                }
            }
            json!({ "ok": true, "from": from, "to": to, "page_count": count, "text": out })
        }
        other => json!({ "ok": false, "error": format!("unknown cmd '{other}'") }),
    }
}

fn main() {
    let d = DocView::from_env();
    if d.mirror.is_empty() && d.overlay.is_empty() {
        eprintln!("cloud-canvas: set PAPIER_CLOUD_MIRROR / PAPIER_CLOUD_OVERLAY");
        std::process::exit(2);
    }
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(&line) {
            Ok(req) => handle(&d, &req),
            Err(e) => json!({ "ok": false, "error": format!("bad request: {e}") }),
        };
        let _ = writeln!(out, "{resp}");
        let _ = out.flush();
    }
}
