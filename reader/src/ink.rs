//! The page model: vector ink as the source of truth.
//!
//! A page holds two layers:
//!   - `strokes`: the user's pen strokes, captured live from the digitizer;
//!   - `patches`: the AI's contributions, each an id'd set of strokes so a
//!     patch can be erased (by pi or by the user's rubber) as a unit.
//!
//! The framebuffer is only a cache: any region can be re-rendered from the
//! vectors (white fill + stamping every intersecting stroke), which is what
//! makes erasing clean — removing a patch that crossed the user's writing
//! re-renders the user's ink intact underneath.
//!
//! Compositing is darkest-wins, like real ink: the AI's gray never eats the
//! user's black where they cross, in live drawing and re-renders alike.
//!
//! Persistence: one JSON file per page (ints, coords x10) in the book's ink
//! dir. Small, dependency-free, debuggable with jq.

use crate::fb::{Framebuffer, SCREEN_H, SCREEN_W};
use serde_json::{json, Value};

pub const USER_GRAY: u8 = 0; /* user ink: black */
pub const AI_GRAY: u8 = 110; /* AI ink: mid gray, distinct on GL16 */

#[derive(Clone, Copy)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
    pub r: f32, /* stamp radius, px */
}

#[derive(Clone)]
pub struct Stroke {
    pub pts: Vec<Pt>,
    pub gray: u8,
}

pub struct Patch {
    pub id: u64,
    pub strokes: Vec<Stroke>,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32, /* inclusive */
    pub y1: i32,
}

impl Rect {
    pub fn union(self, o: Rect) -> Rect {
        Rect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }
    pub fn pad(self, p: i32) -> Rect {
        Rect { x0: self.x0 - p, y0: self.y0 - p, x1: self.x1 + p, y1: self.y1 + p }
    }
    pub fn clamp_screen(self) -> Rect {
        Rect {
            x0: self.x0.clamp(0, SCREEN_W - 1),
            y0: self.y0.clamp(0, SCREEN_H - 1),
            x1: self.x1.clamp(0, SCREEN_W - 1),
            y1: self.y1.clamp(0, SCREEN_H - 1),
        }
    }
    pub fn w(&self) -> i32 {
        self.x1 - self.x0 + 1
    }
    pub fn h(&self) -> i32 {
        self.y1 - self.y0 + 1
    }
}

/// One horizontal "row" of ink on the page (see `Page::ink_bands`).
pub struct Band {
    pub y0: i32,
    pub y1: i32,
    pub x0: i32,
    pub x1: i32,
    pub user: bool, /* contains any user ink (not just patches) */
}

pub fn stroke_bbox(s: &Stroke) -> Option<Rect> {
    let mut it = s.pts.iter();
    let first = it.next()?;
    let mut r = Rect {
        x0: (first.x - first.r) as i32,
        y0: (first.y - first.r) as i32,
        x1: (first.x + first.r).ceil() as i32,
        y1: (first.y + first.r).ceil() as i32,
    };
    for p in it {
        r.x0 = r.x0.min((p.x - p.r) as i32);
        r.y0 = r.y0.min((p.y - p.r) as i32);
        r.x1 = r.x1.max((p.x + p.r).ceil() as i32);
        r.y1 = r.y1.max((p.y + p.r).ceil() as i32);
    }
    Some(r)
}

pub fn patch_bbox(p: &Patch) -> Option<Rect> {
    p.strokes.iter().filter_map(stroke_bbox).reduce(Rect::union)
}

/* ---- stamping ------------------------------------------------------------ */

fn gray_to_565(g: u8) -> u16 {
    let g = g as u16;
    ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3)
}

fn lum_of_565(c: u16) -> u8 {
    (((c >> 5) & 0x3F) as u32 * 255 / 63) as u8
}

/// Stamp one disc, darkest-wins, honoring the fb's y-clip band.
///
/// ON SCREEN all ink is black — the gray tag only distinguishes layers in
/// the snapshots sent to pi (so it can tell its ink from the user's).
fn stamp(fb: &mut Framebuffer, cx: i32, cy: i32, r: i32, gray: u8) {
    let gray = if gray < 250 { 0 } else { gray };
    let (cy0, cy1) = (fb.clip_y0, fb.clip_y1);
    let px = fb.pixels();
    let c = gray_to_565(gray);
    for j in -r..=r {
        let y = cy + j;
        if y < cy0 || y >= cy1 {
            continue;
        }
        for i in -r..=r {
            let x = cx + i;
            if x < 0 || x >= SCREEN_W {
                continue;
            }
            if i * i + j * j <= r * r {
                let idx = (y * SCREEN_W + x) as usize;
                if gray < lum_of_565(px[idx]) {
                    px[idx] = c;
                }
            }
        }
    }
}

/// Stamp the segment between two points (interpolated discs), used both for
/// live pen input and for re-renders.
pub fn stamp_segment(fb: &mut Framebuffer, a: Pt, b: Pt, gray: u8) {
    let steps = (b.x - a.x).abs().max((b.y - a.y).abs()).ceil().max(1.0) as i32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let r = (a.r + (b.r - a.r) * t).round().max(1.0) as i32;
        stamp(
            fb,
            (a.x + (b.x - a.x) * t).round() as i32,
            (a.y + (b.y - a.y) * t).round() as i32,
            r,
            gray,
        );
    }
}

fn stamp_stroke(fb: &mut Framebuffer, s: &Stroke) {
    if s.pts.len() == 1 {
        stamp(fb, s.pts[0].x.round() as i32, s.pts[0].y.round() as i32, s.pts[0].r.round().max(1.0) as i32, s.gray);
        return;
    }
    for w in s.pts.windows(2) {
        stamp_segment(fb, w[0], w[1], s.gray);
    }
}

/* ---- the page ------------------------------------------------------------ */

#[derive(Default)]
pub struct Page {
    pub strokes: Vec<Stroke>,
    pub patches: Vec<Patch>,
    pub next_patch: u64,
    pub dirty: bool, /* unsaved changes */
}

impl Page {
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.patches.is_empty()
    }

    pub fn add_patch(&mut self, strokes: Vec<Stroke>) -> u64 {
        let id = self.next_patch;
        self.next_patch += 1;
        self.patches.push(Patch { id, strokes });
        self.dirty = true;
        id
    }

    pub fn remove_patch(&mut self, id: u64) -> Option<Rect> {
        let i = self.patches.iter().position(|p| p.id == id)?;
        let bbox = patch_bbox(&self.patches[i]);
        self.patches.remove(i);
        self.dirty = true;
        bbox
    }

    /// Rubber pass at (x, y): drop every stroke (user or AI) that comes
    /// within `r` of the point. Returns the union bbox of what vanished.
    pub fn erase_at(&mut self, x: f32, y: f32, r: f32) -> Option<Rect> {
        let hit = |s: &Stroke| -> bool {
            match stroke_bbox(s) {
                Some(b)
                    if x >= (b.x0 as f32 - r)
                        && x <= (b.x1 as f32 + r)
                        && y >= (b.y0 as f32 - r)
                        && y <= (b.y1 as f32 + r) => {}
                _ => return false,
            }
            /* distance point-to-segments, cheap enough per stroke */
            let rr = |p: &Pt| (p.r + r) * (p.r + r);
            if s.pts.len() == 1 {
                let (dx, dy) = (s.pts[0].x - x, s.pts[0].y - y);
                return dx * dx + dy * dy <= rr(&s.pts[0]);
            }
            s.pts.windows(2).any(|w| {
                let (ax, ay, bx, by) = (w[0].x, w[0].y, w[1].x, w[1].y);
                let (px, py) = (x - ax, y - ay);
                let (vx, vy) = (bx - ax, by - ay);
                let len2 = vx * vx + vy * vy;
                let t = if len2 > 0.0 { ((px * vx + py * vy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let (dx, dy) = (px - vx * t, py - vy * t);
                dx * dx + dy * dy <= rr(&w[0])
            })
        };

        let mut gone: Option<Rect> = None;
        let mut take = |b: Option<Rect>| {
            if let Some(b) = b {
                gone = Some(gone.map_or(b, |g| g.union(b)));
            }
        };
        let before = self.strokes.len();
        let mut kept = Vec::with_capacity(before);
        for s in self.strokes.drain(..) {
            if hit(&s) {
                take(stroke_bbox(&s));
            } else {
                kept.push(s);
            }
        }
        self.strokes = kept;
        for p in &mut self.patches {
            let mut kept = Vec::with_capacity(p.strokes.len());
            for s in p.strokes.drain(..) {
                if hit(&s) {
                    take(stroke_bbox(&s));
                } else {
                    kept.push(s);
                }
            }
            p.strokes = kept;
        }
        self.patches.retain(|p| !p.strokes.is_empty());
        if gone.is_some() {
            self.dirty = true;
        }
        gone
    }

    /// Merge every stroke's bbox (user ink AND patches) into horizontal
    /// bands — the page's "rows of ink", top to bottom. This is what makes
    /// placement a measurement instead of a vision problem: the pause
    /// message hands pi these numbers.
    pub fn ink_bands(&self) -> Vec<Band> {
        const GAP: i32 = 22; /* rows closer than this merge into one band */
        let mut boxes: Vec<(Rect, bool)> = Vec::new();
        for s in &self.strokes {
            if let Some(b) = stroke_bbox(s) {
                boxes.push((b.clamp_screen(), true));
            }
        }
        for p in &self.patches {
            for s in &p.strokes {
                if let Some(b) = stroke_bbox(s) {
                    boxes.push((b.clamp_screen(), false));
                }
            }
        }
        boxes.sort_by_key(|(b, _)| b.y0);
        let mut bands: Vec<Band> = Vec::new();
        for (b, user) in boxes {
            match bands.last_mut() {
                Some(band) if b.y0 <= band.y1 + GAP => {
                    band.y1 = band.y1.max(b.y1);
                    band.x0 = band.x0.min(b.x0);
                    band.x1 = band.x1.max(b.x1);
                    band.user |= user;
                }
                _ => bands.push(Band { y0: b.y0, y1: b.y1, x0: b.x0, x1: b.x1, user }),
            }
        }
        bands
    }

    /// Median height of the user's ink rows — a proxy for how big their
    /// handwriting is (patches excluded: we want to match the human).
    pub fn user_line_height(&self) -> Option<i32> {
        let mut hs: Vec<i32> = self
            .ink_bands()
            .into_iter()
            .filter(|b| b.user)
            .map(|b| (b.y1 - b.y0).clamp(24, 160))
            .collect();
        if hs.is_empty() {
            return None;
        }
        hs.sort_unstable();
        Some(hs[hs.len() / 2])
    }

    /// Stamp every stroke that touches `r` — NO background fill; the caller
    /// paints white (a note page) or the book raster (a PDF page) first.
    pub fn stamp_region(&self, fb: &mut Framebuffer, r: Rect) {
        let r = r.clamp_screen();
        fb.set_clip(r.y0, r.y1 + 1);
        let touches = |s: &Stroke| {
            stroke_bbox(s).is_some_and(|b| {
                b.x1 >= r.x0 && b.x0 <= r.x1 && b.y1 >= r.y0 && b.y0 <= r.y1
            })
        };
        for s in self.strokes.iter().filter(|s| touches(s)) {
            stamp_stroke(fb, s);
        }
        for p in &self.patches {
            for s in p.strokes.iter().filter(|s| touches(s)) {
                stamp_stroke(fb, s);
            }
        }
        fb.clear_clip();
    }

    /// Plot this page's strokes into an existing 1/`div`-scale grayscale
    /// buffer (user ink black, AI ink gray, darkest-wins) — the buffer
    /// starts as white or as the downscaled book page.
    pub fn snapshot_into(&self, buf: &mut [u8], div: i32) {
        let (w, h) = (SCREEN_W / div, SCREEN_H / div);
        let k = 1.0 / div as f32;
        let mut plot = |x: f32, y: f32, r: f32, g: u8| {
            let (cx, cy) = ((x * k).round() as i32, (y * k).round() as i32);
            let ri = (r * k).round().max(1.0) as i32;
            for j in -ri..=ri {
                for i in -ri..=ri {
                    let (px, py) = (cx + i, cy + j);
                    if px >= 0 && px < w && py >= 0 && py < h && i * i + j * j <= ri * ri {
                        let idx = (py * w + px) as usize;
                        if g < buf[idx] {
                            buf[idx] = g;
                        }
                    }
                }
            }
        };
        let mut draw_stroke = |s: &Stroke| {
            if s.pts.len() == 1 {
                plot(s.pts[0].x, s.pts[0].y, s.pts[0].r, s.gray);
            }
            for wnd in s.pts.windows(2) {
                let (a, b) = (wnd[0], wnd[1]);
                let steps = ((b.x - a.x).abs().max((b.y - a.y).abs()) * k).ceil().max(1.0) as i32;
                for i in 0..=steps {
                    let t = i as f32 / steps as f32;
                    plot(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.r + (b.r - a.r) * t, s.gray);
                }
            }
        };
        for s in &self.strokes {
            draw_stroke(s);
        }
        for p in &self.patches {
            for s in &p.strokes {
                draw_stroke(s);
            }
        }
    }

    /// Render the page alone into a fresh white snapshot at 1/`div` scale.
    pub fn snapshot(&self, div: i32) -> (i32, i32, Vec<u8>) {
        let (w, h) = (SCREEN_W / div, SCREEN_H / div);
        let mut buf = vec![255u8; (w * h) as usize];
        self.snapshot_into(&mut buf, div);
        (w, h, buf)
    }

    /* -- persistence -- */

    fn stroke_to_json(s: &Stroke) -> Value {
        let mut flat = Vec::with_capacity(s.pts.len() * 3);
        for p in &s.pts {
            flat.push(json!((p.x * 10.0).round() as i64));
            flat.push(json!((p.y * 10.0).round() as i64));
            flat.push(json!((p.r * 10.0).round() as i64));
        }
        json!({ "g": s.gray, "p": flat })
    }

    fn stroke_from_json(v: &Value) -> Option<Stroke> {
        let gray = v["g"].as_u64().unwrap_or(0) as u8;
        let flat = v["p"].as_array()?;
        let mut pts = Vec::with_capacity(flat.len() / 3);
        for c in flat.chunks_exact(3) {
            pts.push(Pt {
                x: c[0].as_f64()? as f32 / 10.0,
                y: c[1].as_f64()? as f32 / 10.0,
                r: c[2].as_f64()? as f32 / 10.0,
            });
        }
        Some(Stroke { pts, gray })
    }

    pub fn save(&mut self, path: &str) -> std::io::Result<()> {
        let doc = json!({
            "v": 1,
            "next_patch": self.next_patch,
            "strokes": self.strokes.iter().map(Self::stroke_to_json).collect::<Vec<_>>(),
            "patches": self.patches.iter().map(|p| json!({
                "id": p.id,
                "strokes": p.strokes.iter().map(Self::stroke_to_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&doc)?)?;
        std::fs::rename(&tmp, path)?;
        self.dirty = false;
        Ok(())
    }

    pub fn load(path: &str) -> Option<Page> {
        let bytes = std::fs::read(path).ok()?;
        let v: Value = serde_json::from_slice(&bytes).ok()?;
        let strokes = v["strokes"]
            .as_array()?
            .iter()
            .filter_map(Self::stroke_from_json)
            .collect();
        let patches = v["patches"]
            .as_array()?
            .iter()
            .filter_map(|p| {
                Some(Patch {
                    id: p["id"].as_u64()?,
                    strokes: p["strokes"].as_array()?.iter().filter_map(Self::stroke_from_json).collect(),
                })
            })
            .collect();
        Some(Page {
            strokes,
            patches,
            next_patch: v["next_patch"].as_u64().unwrap_or(0),
            dirty: false,
        })
    }
}
