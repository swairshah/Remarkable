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
    /// Stable per-page id (assigned when the stroke lands in a Page;
    /// 0 = not yet adopted). Selection and undo hold (Owner, id) refs, so
    /// they survive page flips and concurrent AI patches.
    pub id: u64,
    pub pts: Vec<Pt>,
    pub gray: u8,
}

/// A run of TYPESET text (as opposed to plotter-stroke Hershey text): pi's
/// Garamond, rendered with the real outline font (text.rs / fontdue) rather
/// than as pen strokes. It lives on a patch so it erases/undoes/snapshots
/// as a unit and syncs to the web (where the browser renders true Garamond).
#[derive(Clone)]
pub struct TextRun {
    pub x: f32, /* baseline start, page px */
    pub y: f32,
    pub size: f32,
    pub gray: u8, /* USER_GRAY on screen; AI_GRAY only tags pi's snapshot */
    pub text: String,
}

pub struct Patch {
    pub id: u64,
    pub strokes: Vec<Stroke>,
    pub texts: Vec<TextRun>,
}

/// The removable content of a patch — what undo stashes and restores.
pub type PatchBody = (Vec<Stroke>, Vec<TextRun>);

/// Bounding box of a typeset run (page px). Height from the font's line
/// metrics so descenders/ascenders are covered.
pub fn text_run_bbox(t: &TextRun) -> Option<Rect> {
    if t.text.trim().is_empty() {
        return None;
    }
    let w = crate::text::width(crate::text::Face::Body, t.size, &t.text);
    let asc = crate::text::ascent(crate::text::Face::Body, t.size).ceil() as i32;
    let lh = crate::text::line_h(crate::text::Face::Body, t.size);
    Some(Rect {
        x0: t.x as i32 - 2,
        y0: t.y as i32 - asc - 2,
        x1: t.x as i32 + w + 2,
        y1: t.y as i32 - asc + lh + 2,
    })
}

/// Which layer a stroke lives in: the user's ink or an AI patch.
#[derive(Clone, Copy, PartialEq)]
pub enum Owner {
    User,
    Patch(u64),
}

/// A stroke lifted out of the page (erase / cut), remembering its home so
/// undo can put it back exactly where it came from.
pub struct OwnedStroke {
    pub owner: Owner,
    pub stroke: Stroke,
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
    p.strokes
        .iter()
        .filter_map(stroke_bbox)
        .chain(p.texts.iter().filter_map(text_run_bbox))
        .reduce(Rect::union)
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
    pub next_stroke: u64, /* id source; healed on load */
    pub dirty: bool,      /* unsaved changes */
}

impl Page {
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.patches.is_empty()
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_stroke = self.next_stroke.max(1);
        let id = self.next_stroke;
        self.next_stroke += 1;
        id
    }

    /// Land a finished user stroke; returns its assigned id.
    pub fn push_stroke(&mut self, mut s: Stroke) -> u64 {
        let id = self.alloc_id();
        s.id = id;
        self.strokes.push(s);
        self.dirty = true;
        id
    }

    pub fn add_patch(&mut self, mut strokes: Vec<Stroke>, texts: Vec<TextRun>) -> u64 {
        for s in &mut strokes {
            s.id = self.alloc_id();
        }
        let id = self.next_patch;
        self.next_patch += 1;
        self.patches.push(Patch { id, strokes, texts });
        self.dirty = true;
        id
    }

    /// Re-add a patch under its ORIGINAL id (undo of an erased patch).
    pub fn add_patch_with_id(&mut self, id: u64, body: PatchBody) {
        let (strokes, texts) = body;
        self.next_patch = self.next_patch.max(id + 1);
        for s in &strokes {
            self.next_stroke = self.next_stroke.max(s.id + 1);
        }
        self.patches.push(Patch { id, strokes, texts });
        self.dirty = true;
    }

    /// Remove a user stroke by id (undo of AddStroke).
    pub fn remove_stroke_by_id(&mut self, id: u64) -> Option<(Stroke, Rect)> {
        let i = self.strokes.iter().position(|s| s.id == id)?;
        let s = self.strokes.remove(i);
        let b = stroke_bbox(&s)?;
        self.dirty = true;
        Some((s, b))
    }

    /// Lift the referenced strokes out of the page (lasso delete / cut).
    /// Empty patches are pruned; patch ids are never reused so undo can
    /// recreate them.
    pub fn remove_strokes_by_ids(&mut self, refs: &[(Owner, u64)]) -> (Vec<OwnedStroke>, Option<Rect>) {
        let mut out = Vec::new();
        let mut gone: Option<Rect> = None;
        let mut take = |owner: Owner, s: Stroke, gone: &mut Option<Rect>| {
            if let Some(b) = stroke_bbox(&s) {
                *gone = Some(gone.map_or(b, |g| g.union(b)));
            }
            out.push(OwnedStroke { owner, stroke: s });
        };
        let wanted = |owner: Owner, id: u64| refs.iter().any(|&(o, i)| o == owner && i == id);
        let mut kept = Vec::with_capacity(self.strokes.len());
        for s in self.strokes.drain(..) {
            if wanted(Owner::User, s.id) {
                take(Owner::User, s, &mut gone);
            } else {
                kept.push(s);
            }
        }
        self.strokes = kept;
        for p in &mut self.patches {
            let pid = p.id;
            let mut kept = Vec::with_capacity(p.strokes.len());
            for s in p.strokes.drain(..) {
                if wanted(Owner::Patch(pid), s.id) {
                    take(Owner::Patch(pid), s, &mut gone);
                } else {
                    kept.push(s);
                }
            }
            p.strokes = kept;
        }
        /* keep a patch that still has typeset text even if all its strokes
         * were lassoed out */
        self.patches.retain(|p| !p.strokes.is_empty() || !p.texts.is_empty());
        if gone.is_some() {
            self.dirty = true;
        }
        (out, gone)
    }

    /// Put lifted strokes back where they came from (undo of erase/delete).
    /// A patch that vanished when emptied is recreated under its old id.
    pub fn insert_owned(&mut self, strokes: Vec<OwnedStroke>) -> Option<Rect> {
        let mut dirty: Option<Rect> = None;
        for os in strokes {
            if let Some(b) = stroke_bbox(&os.stroke) {
                dirty = Some(dirty.map_or(b, |d| d.union(b)));
            }
            self.next_stroke = self.next_stroke.max(os.stroke.id + 1);
            match os.owner {
                Owner::User => self.strokes.push(os.stroke),
                Owner::Patch(pid) => {
                    self.next_patch = self.next_patch.max(pid + 1);
                    match self.patches.iter_mut().find(|p| p.id == pid) {
                        Some(p) => p.strokes.push(os.stroke),
                        None => self.patches.push(Patch { id: pid, strokes: vec![os.stroke], texts: Vec::new() }),
                    }
                }
            }
        }
        if dirty.is_some() {
            self.dirty = true;
        }
        dirty
    }

    /// Translate the referenced strokes; returns the union of the source
    /// and destination bboxes (the region to re-render). Unresolved ids
    /// are skipped — LIFO undo ordering makes that safe.
    pub fn translate_strokes(&mut self, refs: &[(Owner, u64)], dx: f32, dy: f32) -> Option<Rect> {
        let wanted = |owner: Owner, id: u64| refs.iter().any(|&(o, i)| o == owner && i == id);
        let mut dirty: Option<Rect> = None;
        let shift = |s: &mut Stroke, dirty: &mut Option<Rect>| {
            if let Some(b) = stroke_bbox(s) {
                *dirty = Some(dirty.map_or(b, |d| d.union(b)));
            }
            for p in &mut s.pts {
                p.x += dx;
                p.y += dy;
            }
            if let Some(b) = stroke_bbox(s) {
                *dirty = Some(dirty.map_or(b, |d| d.union(b)));
            }
        };
        for s in &mut self.strokes {
            if wanted(Owner::User, s.id) {
                shift(s, &mut dirty);
            }
        }
        for p in &mut self.patches {
            let pid = p.id;
            for s in &mut p.strokes {
                if wanted(Owner::Patch(pid), s.id) {
                    shift(s, &mut dirty);
                }
            }
        }
        if dirty.is_some() {
            self.dirty = true;
        }
        dirty
    }

    pub fn remove_patch(&mut self, id: u64) -> Option<Rect> {
        self.take_patch(id).map(|(_, b)| b)
    }

    /// Remove a patch and hand back its content (undo needs strokes + texts).
    pub fn take_patch(&mut self, id: u64) -> Option<(PatchBody, Rect)> {
        let i = self.patches.iter().position(|p| p.id == id)?;
        let bbox = patch_bbox(&self.patches[i])?;
        let p = self.patches.remove(i);
        self.dirty = true;
        Some(((p.strokes, p.texts), bbox))
    }

    /// The patch whose typeset text a rubber pass at (x, y, r) lands on
    /// (Garamond runs erase whole-patch — they aren't stroke geometry).
    pub fn text_patch_at(&self, x: f32, y: f32, r: f32) -> Option<u64> {
        for p in &self.patches {
            for t in &p.texts {
                if let Some(b) = text_run_bbox(t) {
                    if x >= b.x0 as f32 - r && x <= b.x1 as f32 + r
                        && y >= b.y0 as f32 - r && y <= b.y1 as f32 + r
                    {
                        return Some(p.id);
                    }
                }
            }
        }
        None
    }

    /// Rubber pass at (x, y): drop every stroke (user or AI) that comes
    /// within `r` of the point. Returns the union bbox of what vanished
    /// plus the lifted strokes (the rubber batch feeds one undo op).
    pub fn erase_at(&mut self, x: f32, y: f32, r: f32) -> Option<(Rect, Vec<OwnedStroke>)> {
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
        let mut lifted: Vec<OwnedStroke> = Vec::new();
        let take = |owner: Owner, s: Stroke, gone: &mut Option<Rect>, lifted: &mut Vec<OwnedStroke>| {
            if let Some(b) = stroke_bbox(&s) {
                *gone = Some(gone.map_or(b, |g| g.union(b)));
            }
            lifted.push(OwnedStroke { owner, stroke: s });
        };
        let mut kept = Vec::with_capacity(self.strokes.len());
        for s in self.strokes.drain(..) {
            if hit(&s) {
                take(Owner::User, s, &mut gone, &mut lifted);
            } else {
                kept.push(s);
            }
        }
        self.strokes = kept;
        for p in &mut self.patches {
            let pid = p.id;
            let mut kept = Vec::with_capacity(p.strokes.len());
            for s in p.strokes.drain(..) {
                if hit(&s) {
                    take(Owner::Patch(pid), s, &mut gone, &mut lifted);
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
        gone.map(|g| (g, lifted))
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
        /* typeset runs: real font glyphs (black on screen), clipped to r */
        for p in &self.patches {
            for t in &p.texts {
                if text_run_bbox(t).is_some_and(|b| {
                    b.x1 >= r.x0 && b.x0 <= r.x1 && b.y1 >= r.y0 && b.y0 <= r.y1
                }) {
                    let y_top = (t.y - crate::text::ascent(crate::text::Face::Body, t.size)).round() as i32;
                    crate::text::draw_line(fb, t.x as i32, y_top, crate::text::Face::Body, t.size, &t.text);
                }
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
        /* typeset runs render into the snapshot at their gray tag (AI ink
         * stays gray so pi can tell its own writing apart), scaled by 1/div */
        let (bw, bh) = (w, h);
        for p in &self.patches {
            for t in &p.texts {
                let y_top = (t.y - crate::text::ascent(crate::text::Face::Body, t.size)) * k;
                crate::text::draw_gray_level(
                    buf, bw, bh,
                    (t.x * k).round() as i32,
                    y_top.round() as i32,
                    crate::text::Face::Body,
                    t.size * k,
                    &t.text,
                    t.gray,
                );
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
        json!({ "i": s.id, "g": s.gray, "p": flat })
    }

    fn stroke_from_json(v: &Value) -> Option<Stroke> {
        let id = v["i"].as_u64().unwrap_or(0); /* pre-id pages heal on load */
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
        Some(Stroke { id, pts, gray })
    }

    fn text_from_json(v: &Value) -> Option<TextRun> {
        Some(TextRun {
            x: v["x"].as_f64()? as f32 / 10.0,
            y: v["y"].as_f64()? as f32 / 10.0,
            size: v["s"].as_f64().map(|s| s as f32 / 10.0).filter(|&s| s > 0.0).unwrap_or(40.0),
            gray: v["g"].as_u64().unwrap_or(0) as u8,
            text: v["t"].as_str()?.to_string(),
        })
    }

    pub fn save(&mut self, path: &str) -> std::io::Result<()> {
        let text_to_json = |t: &TextRun| json!({
            "x": (t.x * 10.0).round() as i64,
            "y": (t.y * 10.0).round() as i64,
            "s": (t.size * 10.0).round() as i64,
            "g": t.gray,
            "t": t.text,
        });
        let doc = json!({
            "v": 1,
            "next_patch": self.next_patch,
            "next_stroke": self.next_stroke,
            "strokes": self.strokes.iter().map(Self::stroke_to_json).collect::<Vec<_>>(),
            "patches": self.patches.iter().map(|p| json!({
                "id": p.id,
                "strokes": p.strokes.iter().map(Self::stroke_to_json).collect::<Vec<_>>(),
                "texts": p.texts.iter().map(text_to_json).collect::<Vec<_>>(),
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
        let strokes: Vec<Stroke> = v["strokes"]
            .as_array()?
            .iter()
            .filter_map(Self::stroke_from_json)
            .collect();
        let patches: Vec<Patch> = v["patches"]
            .as_array()?
            .iter()
            .filter_map(|p| {
                Some(Patch {
                    id: p["id"].as_u64()?,
                    strokes: p["strokes"].as_array()?.iter().filter_map(Self::stroke_from_json).collect(),
                    texts: p["texts"].as_array().map_or_else(Vec::new, |a| {
                        a.iter().filter_map(Self::text_from_json).collect()
                    }),
                })
            })
            .collect();
        let mut page = Page {
            strokes,
            patches,
            next_patch: v["next_patch"].as_u64().unwrap_or(0),
            next_stroke: v["next_stroke"].as_u64().unwrap_or(0),
            dirty: false,
        };
        /* heal ids: pre-id pages (or foreign writers) get fresh ones */
        let max_id = page
            .strokes
            .iter()
            .chain(page.patches.iter().flat_map(|p| p.strokes.iter()))
            .map(|s| s.id)
            .max()
            .unwrap_or(0);
        let mut next = page.next_stroke.max(max_id + 1).max(1);
        for s in page
            .strokes
            .iter_mut()
            .chain(page.patches.iter_mut().flat_map(|p| p.strokes.iter_mut()))
        {
            if s.id == 0 {
                s.id = next;
                next += 1;
            }
        }
        page.next_stroke = next;
        Some(page)
    }
}
