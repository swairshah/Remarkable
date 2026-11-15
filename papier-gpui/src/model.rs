//! The papier cloud data model: the library manifest (/papier/api/library)
//! and the libreink-page ink JSON schema, byte-compatible with the tablet,
//! the web viewer and the iPad app.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Library {
    pub v: i64,
    #[serde(default)]
    pub generation: String,
    pub docs: Vec<Doc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Doc {
    pub id: String,
    /// "/papier/data/" (tablet mirror) or "/papier/inbound/" (web/iPad added).
    pub base: String,
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub added_at: Option<f64>,
    #[serde(default)]
    pub mtime: Option<f64>,
    pub meta: DocMeta,
    /// Bumps whenever any doc file changes.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub seq: Vec<SeqEntry>,
    #[serde(default)]
    pub ink: Option<Vec<String>>,
    /// The book's source PDF is on the server (the web viewer renders
    /// pages from it; we use its per-page renders when a raster is missing).
    #[serde(default)]
    pub has_source: bool,
    #[serde(default)]
    pub src_version: Option<String>,
}

impl Doc {
    pub fn is_notebook(&self) -> bool {
        self.meta.kind.as_deref() == Some("notebook")
    }
    pub fn page_w(&self) -> f32 {
        self.meta.w.unwrap_or(1404.0) as f32
    }
    pub fn page_h(&self) -> f32 {
        self.meta.h.unwrap_or(1872.0) as f32
    }
    pub fn title(&self) -> &str {
        &self.meta.title
    }
    pub fn page_count(&self) -> usize {
        if self.seq.is_empty() { self.meta.pages.unwrap_or(1).max(1) as usize } else { self.seq.len() }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DocMeta {
    pub title: String,
    #[serde(default)]
    pub pages: Option<i64>,
    #[serde(default)]
    pub w: Option<f64>,
    #[serde(default)]
    pub h: Option<f64>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub folder: Option<String>,
    /// mkbook's crop of the source page (fractions), or margins (device px).
    #[serde(default)]
    pub crop: Option<Vec<f64>>,
    #[serde(default)]
    pub margins: Option<Vec<f64>>,
}

/// One entry of a document's page sequence: {"p": N} (0-based pdf page)
/// or {"n": N} (1-based note page).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeqEntry {
    Pdf(u32),
    Note(u32),
}

impl SeqEntry {
    /// The ink file stem, matching papier-library.js inkKey().
    pub fn ink_key(&self) -> String {
        match self {
            SeqEntry::Pdf(p) => format!("pdf-{:04}", p + 1),
            SeqEntry::Note(n) => format!("note-{n:04}"),
        }
    }
    pub fn to_json(self) -> Value {
        match self {
            SeqEntry::Pdf(p) => json!({ "p": p }),
            SeqEntry::Note(n) => json!({ "n": n }),
        }
    }
}

impl<'de> Deserialize<'de> for SeqEntry {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        if let Some(p) = v.get("p").and_then(Value::as_u64) {
            return Ok(SeqEntry::Pdf(p as u32));
        }
        if let Some(n) = v.get("n").and_then(Value::as_u64) {
            return Ok(SeqEntry::Note(n as u32));
        }
        Err(serde::de::Error::custom("seq entry has neither p nor n"))
    }
}

impl Serialize for SeqEntry {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

// ------------------------------------------------------------------ ink

/// A point of a stroke in page coordinates (1404x1872 space); `r` is the
/// half-width of the mark. Serialized as flat [x*10, y*10, r*10, ...].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkPoint {
    pub x: f32,
    pub y: f32,
    pub r: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InkStroke {
    pub id: u64,
    /// 0 = black.
    pub gray: i64,
    pub points: Vec<InkPoint>,
}

impl InkStroke {
    pub fn bounds(&self) -> [f32; 4] {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in &self.points {
            b[0] = b[0].min(p.x - p.r);
            b[1] = b[1].min(p.y - p.r);
            b[2] = b[2].max(p.x + p.r);
            b[3] = b[3].max(p.y + p.r);
        }
        b
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InkText {
    pub x: f32,
    /// Baseline.
    pub y: f32,
    pub size: f32,
    pub gray: i64,
    pub text: String,
}

/// pi's ink lives in patches; the user's in top-level strokes.
#[derive(Clone, Debug, PartialEq)]
pub struct InkPatch {
    pub id: u64,
    pub strokes: Vec<InkStroke>,
    pub texts: Vec<InkText>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InkPage {
    pub next_patch: u64,
    pub next_stroke: u64,
    pub strokes: Vec<InkStroke>,
    pub patches: Vec<InkPatch>,
    /// Highlighter marks: chisel strokes (g=180, r=17) drawn as a wash.
    pub highlights: Vec<InkStroke>,
    /// Keys this app doesn't know, passed through untouched on save.
    pub extra: serde_json::Map<String, Value>,
}

pub const HL_GRAY: i64 = 180;
pub const HL_R: f32 = 17.0;

impl Default for InkPage {
    fn default() -> Self {
        Self { next_patch: 1, next_stroke: 1, strokes: Vec::new(), patches: Vec::new(), highlights: Vec::new(), extra: Default::default() }
    }
}

fn uint(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
}

fn stroke_from(v: &Value) -> Option<InkStroke> {
    let flat = v.get("p")?.as_array()?;
    let mut points = Vec::with_capacity(flat.len() / 3);
    for c in flat.chunks_exact(3) {
        points.push(InkPoint {
            x: c[0].as_f64()? as f32 / 10.0,
            y: c[1].as_f64()? as f32 / 10.0,
            r: c[2].as_f64()? as f32 / 10.0,
        });
    }
    Some(InkStroke { id: uint(v.get("i")).unwrap_or(0), gray: v.get("g").and_then(Value::as_i64).unwrap_or(0), points })
}

fn text_from(v: &Value) -> Option<InkText> {
    let t = v.get("t")?.as_str()?.to_owned();
    let x = v.get("x")?.as_f64()? as f32;
    let y = v.get("y")?.as_f64()? as f32;
    let s = v.get("s").and_then(Value::as_f64).unwrap_or(400.0) as f32;
    Some(InkText {
        x: x / 10.0,
        y: y / 10.0,
        size: if s > 0.0 { s / 10.0 } else { 40.0 },
        gray: v.get("g").and_then(Value::as_i64).unwrap_or(0),
        text: t,
    })
}

pub fn stroke_json(s: &InkStroke) -> Value {
    let mut flat = Vec::with_capacity(s.points.len() * 3);
    for p in &s.points {
        flat.push((p.x * 10.0).round() as i64);
        flat.push((p.y * 10.0).round() as i64);
        flat.push((p.r * 10.0).round() as i64);
    }
    json!({ "i": s.id, "g": s.gray, "p": flat })
}

pub fn patch_json(p: &InkPatch) -> Value {
    json!({
        "id": p.id,
        "strokes": p.strokes.iter().map(stroke_json).collect::<Vec<_>>(),
        "texts": p.texts.iter().map(|t| json!({
            "x": (t.x * 10.0).round() as i64,
            "y": (t.y * 10.0).round() as i64,
            "s": (t.size * 10.0).round() as i64,
            "g": t.gray,
            "t": t.text,
        })).collect::<Vec<_>>(),
    })
}

impl InkPage {
    pub fn parse(data: &[u8]) -> Option<InkPage> {
        let v: Value = serde_json::from_slice(data).ok()?;
        Self::from_value(&v)
    }

    pub fn from_value(v: &Value) -> Option<InkPage> {
        if v.get("v").and_then(Value::as_i64) != Some(1) {
            return None;
        }
        let strokes = v.get("strokes").and_then(Value::as_array).map(|a| a.iter().filter_map(stroke_from).collect()).unwrap_or_default();
        let patches = v
            .get("patches")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        Some(InkPatch {
                            id: uint(p.get("id"))?,
                            strokes: p.get("strokes").and_then(Value::as_array).map(|a| a.iter().filter_map(stroke_from).collect()).unwrap_or_default(),
                            texts: p.get("texts").and_then(Value::as_array).map(|a| a.iter().filter_map(text_from).collect()).unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let strokes_of = |k: &str| v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(stroke_from).collect()).unwrap_or_default();
        let known = ["v", "next_patch", "next_stroke", "strokes", "patches", "highlights"];
        let extra = v.as_object().map(|o| o.iter().filter(|(k, _)| !known.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        Some(InkPage {
            next_patch: uint(v.get("next_patch")).unwrap_or(1),
            next_stroke: uint(v.get("next_stroke")).unwrap_or(1),
            strokes,
            patches,
            highlights: strokes_of("highlights"),
            extra,
        })
    }

    pub fn to_value(&self) -> Value {
        let mut v = json!({
            "v": 1,
            "next_patch": self.next_patch,
            "next_stroke": self.next_stroke,
            "strokes": self.strokes.iter().map(stroke_json).collect::<Vec<_>>(),
            "patches": self.patches.iter().map(patch_json).collect::<Vec<_>>(),
        });
        let obj = v.as_object_mut().unwrap();
        // Always present, even empty: the server keeps the stored highlights
        // when a write has no `highlights` key (a client that doesn't know
        // the layer), so leaving it out would resurrect erased ones.
        obj.insert("highlights".into(), Value::Array(self.highlights.iter().map(stroke_json).collect()));
        for (k, val) in &self.extra {
            obj.insert(k.clone(), val.clone());
        }
        v
    }

    pub fn serialized(&self) -> Vec<u8> {
        serde_json::to_vec(&self.to_value()).unwrap_or_else(|_| b"{}".to_vec())
    }
}

/// state.json
pub struct DocState {
    pub next_note: u32,
    pub pos: usize,
    pub seq: Vec<SeqEntry>,
}

impl DocState {
    pub fn serialized(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "next_note": self.next_note,
            "pos": self.pos,
            "seq": self.seq.iter().map(|s| s.to_json()).collect::<Vec<_>>(),
        }))
        .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Recent,
    Added,
    Title,
}

impl Sort {
    pub const ALL: [Sort; 3] = [Sort::Recent, Sort::Added, Sort::Title];
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Sort::Recent => "Recently updated",
            Sort::Added => "Recently added",
            Sort::Title => "Title",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Sort::Recent => "recent",
            Sort::Added => "added",
            Sort::Title => "title",
        }
    }
    pub fn from_key(k: &str) -> Sort {
        Self::ALL.into_iter().find(|s| s.key() == k).unwrap_or(Sort::Recent)
    }
    pub fn sort(self, docs: &mut [Doc]) {
        docs.sort_by(|a, b| {
            let primary = match self {
                Sort::Recent => b.mtime.unwrap_or(0.0).total_cmp(&a.mtime.unwrap_or(0.0)),
                Sort::Added => b.added_at.or(b.mtime).unwrap_or(0.0).total_cmp(&a.added_at.or(a.mtime).unwrap_or(0.0)),
                Sort::Title => std::cmp::Ordering::Equal,
            };
            primary
                .then_with(|| a.meta.title.to_lowercase().cmp(&b.meta.title.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_round_trips() {
        let src = br#"{"v":1,"zz":[1],"highlights":[{"i":7,"g":180,"p":[1,2,170]}],"next_patch":3,"next_stroke":9,"strokes":[{"i":4,"g":0,"p":[100,200,17,110,210,20]}],"patches":[{"id":2,"strokes":[{"i":5,"g":0,"p":[1,2,3]}],"texts":[{"x":500,"y":600,"s":400,"g":0,"t":"hello"}]}]}"#;
        let page = InkPage::parse(src).unwrap();
        assert_eq!(page.strokes[0].points[1], InkPoint { x: 11.0, y: 21.0, r: 2.0 });
        assert_eq!(page.patches[0].texts[0].text, "hello");
        assert_eq!(page.highlights[0].gray, 180);
        assert!(page.extra.contains_key("zz"));
        let again = InkPage::parse(&page.serialized()).unwrap();
        assert_eq!(page, again);
    }

    #[test]
    fn erased_highlights_are_sent_as_empty() {
        let page = InkPage::default();
        let v: Value = serde_json::from_slice(&page.serialized()).unwrap();
        assert_eq!(v.get("highlights"), Some(&Value::Array(vec![])));
    }

    #[test]
    fn seq_keys() {
        let seq: Vec<SeqEntry> = serde_json::from_str(r#"[{"p":0},{"n":3}]"#).unwrap();
        assert_eq!(seq[0].ink_key(), "pdf-0001");
        assert_eq!(seq[1].ink_key(), "note-0003");
    }
}
