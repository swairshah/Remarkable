//! An open document: the page sequence, one model per page (ink loaded
//! with the pending overlay, debounced saves), the page raster, the tools,
//! the flip/zoom view, the reMarkable right-edge toolbar, and pi.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, Bounds, Context, Corners, MouseButton, MouseDownEvent, MouseMoveEvent, PathBuilder, RenderImage, ScrollWheelEvent,
    SharedString, Task, TextRun, Timer, Window, canvas, div, point, prelude::*, px, quad, size, svg,
};

use crate::{
    api::{PiState, pi_query},
    app::{Dialog, Papier, Screen, bg},
    assets::icon,
    ink::{self, History, Live, Mark, PI_BLUE, PageRaster, Snap, Width},
    model::{Doc, DocState, HL_GRAY, HL_R, InkPage, SeqEntry},
    store::{self, Settings},
    theme::*,
};

const MARGIN: f32 = 16.0;
/// Space between pages in the strip.
const GAP: f32 = 18.0;
/// The comfortable reading/writing width of a page (logical px).
const READ_W: f32 = 860.0;
const SAVE_PAUSE: Duration = Duration::from_millis(2800);
const DRAW_IN: f32 = 1.4;
/// The tablet's eraser radius, in page units (= tablet px).
const ERASER_R: f32 = 22.0;
const MAX_RASTER: f32 = 4096.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Pen,
    Eraser,
    Highlighter,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Sync {
    Loading,
    Clean,
    Dirty,
    Saving,
    Saved,
    Error(String),
}

pub struct PageModel {
    #[allow(dead_code)]
    pub entry: SeqEntry,
    pub sync: Sync,
    pub base: InkPage,
    pub ready: bool,
    history: History,
    save_gen: u64,
    raster: Option<PageRaster>,
    full_dirty: bool,
    dirty_rect: Option<[f32; 4]>,
    /// Fresh pi patches drawing themselves in.
    animating: HashMap<u64, Instant>,
    patch_dirty: HashSet<u64>,
    /// Bumped on every change to `base`; a background raster built from an
    /// older generation is thrown away.
    edit_gen: u64,
    /// A background full render is in flight.
    building: bool,
}

impl PageModel {
    fn new(entry: SeqEntry) -> PageModel {
        PageModel {
            entry,
            sync: Sync::Loading,
            base: InkPage::default(),
            ready: false,
            history: History::default(),
            save_gen: 0,
            raster: None,
            full_dirty: true,
            dirty_rect: None,
            animating: HashMap::new(),
            patch_dirty: HashSet::new(),
            edit_gen: 0,
            building: false,
        }
    }

    fn snap(&self) -> Snap {
        (self.base.strokes.clone(), self.base.highlights.clone())
    }

    fn next_id(&mut self) -> u64 {
        let max = self
            .base
            .strokes
            .iter()
            .chain(&self.base.highlights)
            .chain(self.base.patches.iter().flat_map(|p| &p.strokes))
            .map(|s| s.id)
            .max()
            .unwrap_or(0);
        let id = self.base.next_stroke.max(max + 1);
        self.base.next_stroke = id + 1;
        id
    }

    fn dirty(&mut self, b: [f32; 4]) {
        self.dirty_rect = Some(self.dirty_rect.map_or(b, |d| ink::union(d, b)));
    }
}

/// What the raster shows: committed marks, minus pi patches still drawing
/// themselves in (those are painted live until their animation ends).
fn marks<'a>(base: &'a InkPage, skip: &HashSet<u64>) -> Vec<Mark<'a>> {
    let mut v: Vec<Mark> = base.highlights.iter().map(|s| Mark { stroke: s, rgb: 0xb4b4b4, wash: true }).collect();
    v.extend(base.strokes.iter().map(|s| Mark { stroke: s, rgb: ink::ink_rgb(s.gray), wash: false }));
    for p in base.patches.iter().filter(|p| !skip.contains(&p.id)) {
        v.extend(p.strokes.iter().map(|s| Mark { stroke: s, rgb: PI_BLUE, wash: false }));
    }
    v
}

struct LiveInk {
    key: String,
    page: usize,
    tool: Tool,
    live: Option<Live>,
    last: (f32, f32),
    changed: bool,
    before: Snap,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    /// One page wide, pages stacked, scrolled vertically.
    Scroll,
    /// Two pages side by side, turned like a book.
    Spread,
}

/// A leaf being turned in spread mode, as a page curl (see curl.rs): the
/// point you grabbed and where it is now, in the leaf's own frame (u from
/// the spine outward, v down). `dir` +1 turns forward (right leaf goes left).
#[derive(Clone, Copy)]
pub struct Turn {
    pub dir: i32,
    origin: (f32, f32),
    finger: (f32, f32),
    anim: Option<TurnAnim>,
}

#[derive(Clone, Copy)]
struct TurnAnim {
    from: (f32, f32),
    to: (f32, f32),
    t0: Instant,
    ms: f32,
    complete: bool,
    /// A keyboard/toolbar turn: the corner lifts as it crosses.
    lift: bool,
}

impl Turn {
    /// 0 → 1 as the grabbed point crosses from its edge to the far page.
    fn progress(&self, w: f32) -> f32 {
        ((self.origin.0 - self.finger.0) / (2.0 * self.origin.0.max(w * 0.5))).clamp(0.0, 1.0)
    }
}

/// A timed glide of the scroll position (page up/down, go to, toolbar).
struct Glide {
    from: (f32, f32),
    to: (f32, f32),
    t0: Instant,
    ms: f32,
}

pub struct Pi {
    pub busy: bool,
    pub mode: String,
    pub font: String,
    since: i64,
    epoch: f64,
    _task: Option<Task<()>>,
}

/// The page strip: every page the same size, stacked top to bottom.
#[derive(Clone, Copy)]
pub struct Layout {
    /// Screen x of the pages' left edge.
    pub x: f32,
    /// Screen y of page 0's top.
    pub top: f32,
    pub w: f32,
    pub h: f32,
    /// Screen px per page unit.
    pub s: f32,
    pub n: usize,
    /// The viewport left of the toolbar.
    pub aw: f32,
    pub vh: f32,
    pub content_w: f32,
    pub content_h: f32,
    /// Two-page mode: the spread's left page index, and the spine's x.
    pub spread: Option<(usize, f32)>,
}

impl Layout {
    pub fn page_x(&self, i: usize) -> f32 {
        match self.spread {
            Some((_, spine)) => if i % 2 == 0 { spine - self.w } else { spine },
            None => self.x,
        }
    }

    pub fn page_y(&self, i: usize) -> f32 {
        match self.spread {
            Some(_) => self.top,
            None => self.top + i as f32 * (self.h + GAP),
        }
    }

    /// The page under a screen point, with the point in page units.
    pub fn hit(&self, x: f32, y: f32) -> Option<(usize, f32, f32)> {
        if let Some((left, _)) = self.spread {
            return (left..(left + 2).min(self.n)).find_map(|i| {
                let (px_, py_) = ((x - self.page_x(i)) / self.s, (y - self.top) / self.s);
                (px_ >= 0.0 && py_ >= -4.0 && px_ <= self.w / self.s && py_ <= self.h / self.s + 4.0).then_some((i, px_, py_))
            });
        }
        let i = ((y - self.top) / (self.h + GAP)).floor();
        if i < 0.0 || i as usize >= self.n {
            return None;
        }
        let i = i as usize;
        let (px_, py_) = ((x - self.x) / self.s, (y - self.page_y(i)) / self.s);
        (px_ >= -4.0 && py_ >= -4.0 && px_ <= self.w / self.s + 4.0 && py_ <= self.h / self.s + 4.0).then_some((i, px_, py_))
    }

    /// Pages touching the viewport.
    pub fn visible(&self) -> std::ops::Range<usize> {
        if let Some((left, _)) = self.spread {
            return left..(left + 2).min(self.n);
        }
        let first = ((-self.top) / (self.h + GAP)).floor().max(0.0) as usize;
        let last = ((self.vh - self.top) / (self.h + GAP)).floor().max(0.0) as usize;
        first.min(self.n)..(last + 1).min(self.n)
    }

    /// The page you're "on": the one under a line 40% down the window.
    pub fn current(&self) -> usize {
        if let Some((left, _)) = self.spread {
            return left;
        }
        let i = ((self.vh * 0.4 - self.top) / (self.h + GAP)).floor().max(0.0) as usize;
        i.min(self.n.saturating_sub(1))
    }
}

pub struct DocView {
    pub doc: Doc,
    pub seq: Vec<SeqEntry>,
    /// The current page (drives pi, the counter, the saved position).
    pub index: usize,
    pub pages: HashMap<String, PageModel>,
    pub tool: Tool,
    pub width: Width,
    pub finger_draws: bool,
    pub toolbar_open: bool,
    /// 1.0 = the comfortable reading width.
    pub zoom: f32,
    scroll: (f32, f32),
    glide: Option<Glide>,
    /// Momentum after a finger lifts: velocity (px/s) and the last step.
    fling: Option<((f32, f32), Instant)>,
    pinching_at: Option<Instant>,
    live: Option<LiveInk>,
    pub indicator_until: Option<Instant>,
    pub pi: Pi,
    hl_popup: Option<(String, u64, (f32, f32))>,
    /// Restore the saved page once the first layout is known.
    pending_jump: Option<usize>,
    pub mode: ViewMode,
    pub turn: Option<Turn>,
    /// Horizontal drag accumulated by a finger or touchpad in spread mode.
    turn_dx: f32,
    turn_start_y: f32,
    /// The leaf's size at the last layout (for keyboard turns).
    leaf_size: (f32, f32),
    /// The curl rendered for the last frame (retired when replaced).
    curl_img: Option<Arc<RenderImage>>,
    /// The page index last acted on (loads, pi, saved position): a turn,
    /// a scroll or a jump all funnel through the comparison with this.
    reported: Option<usize>,
    /// Page pitch (h + gap) at the last frame: when the layout changes
    /// size, the scroll position is rescaled so the same page stays put.
    pitch: f32,
}

impl DocView {
    pub fn new(doc: Doc, settings: &Settings) -> DocView {
        let seq = if doc.seq.is_empty() { vec![SeqEntry::Note(1)] } else { doc.seq.clone() };
        let index = (settings.get(&format!("pos-{}", doc.id)).and_then(|v| v.as_u64()).unwrap_or(0) as usize).min(seq.len() - 1);
        let mode = settings.get(&format!("pimode-{}", doc.id)).and_then(|v| v.as_str()).unwrap_or("quiet").to_owned();
        DocView {
            index,
            seq,
            doc,
            pages: HashMap::new(),
            tool: Tool::Pen,
            width: Width::from_key(settings.get("width").and_then(|v| v.as_str()).unwrap_or("medium")),
            finger_draws: false,
            toolbar_open: settings.get("toolbar").and_then(|v| v.as_bool()).unwrap_or(true),
            zoom: settings.get("zoom").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            scroll: (0.0, 0.0),
            glide: None,
            fling: None,
            pinching_at: None,
            live: None,
            indicator_until: None,
            pi: Pi { busy: false, mode, font: "serif".into(), since: 0, epoch: 0.0, _task: None },
            hl_popup: None,
            pending_jump: Some(index),
            reported: None,
            mode: if settings.get("view_mode").and_then(|v| v.as_str()) == Some("spread") { ViewMode::Spread } else { ViewMode::Scroll },
            turn: None,
            turn_dx: 0.0,
            turn_start_y: 0.0,
            leaf_size: (400.0, 530.0),
            curl_img: None,
            pitch: 0.0,
        }
    }

    pub fn key(&self) -> String {
        self.seq[self.index].ink_key()
    }

    pub fn adopt_version(&mut self, fresh: Doc) {
        if !fresh.seq.is_empty() && fresh.seq != self.seq {
            let current = self.seq[self.index];
            self.seq = fresh.seq.clone();
            self.index = self.seq.iter().position(|e| *e == current).unwrap_or(self.index.min(self.seq.len() - 1));
        }
        self.doc = fresh;
    }

    pub fn into_images(self) -> Vec<Arc<RenderImage>> {
        let mut v: Vec<Arc<RenderImage>> = self.pages.into_values().filter_map(|m| m.raster.and_then(|r| r.image)).collect();
        v.extend(self.curl_img);
        v
    }

    // ------------------------------------------------------------ layout

    fn base_w(&self, vp: (f32, f32)) -> f32 {
        let aw = vp.0 - if self.toolbar_open { STRIP_W } else { 0.0 };
        (aw - 2.0 * MARGIN).min(READ_W).max(100.0)
    }

    pub fn layout(&self, vp: (f32, f32)) -> Layout {
        let aw = vp.0 - if self.toolbar_open { STRIP_W } else { 0.0 };
        let (pw, ph) = (self.doc.page_w(), self.doc.page_h());
        let w = self.base_w(vp) * self.zoom;
        let s = w / pw;
        let h = ph * s;
        let n = self.seq.len();
        let content_w = w + 2.0 * MARGIN;
        let content_h = n as f32 * (h + GAP) + GAP;
        let x = if content_w <= aw { (aw - w) / 2.0 } else { MARGIN - self.scroll.0 };
        if self.mode == ViewMode::Spread {
            // two pages side by side, fitted, meeting at the spine
            let s = ((aw - 2.0 * MARGIN) / (2.0 * pw)).min((vp.1 - 2.0 * MARGIN) / ph);
            let (w, h) = (pw * s, ph * s);
            let left = self.index - self.index % 2;
            return Layout { x: aw / 2.0 - w, top: (vp.1 - h) / 2.0, w, h, s, n, aw, vh: vp.1, content_w: aw, content_h: vp.1, spread: Some((left, aw / 2.0)) };
        }
        Layout { x, top: GAP - self.scroll.1, w, h, s, n, aw, vh: vp.1, content_w, content_h, spread: None }
    }

    fn max_scroll(&self, vp: (f32, f32)) -> (f32, f32) {
        let l = self.layout(vp);
        ((l.content_w - l.aw).max(0.0), (l.content_h - vp.1).max(0.0))
    }

    fn clamp(&mut self, vp: (f32, f32)) {
        let (mx, my) = self.max_scroll(vp);
        self.scroll = (self.scroll.0.clamp(0.0, mx), self.scroll.1.clamp(0.0, my));
    }

    /// The whole page in the window.
    fn fit_page_zoom(&self, vp: (f32, f32)) -> f32 {
        let (pw, ph) = (self.doc.page_w(), self.doc.page_h());
        ((vp.1 - 2.0 * GAP) * pw / ph / self.base_w(vp)).min(1.0)
    }

    pub fn set_zoom(&mut self, z: f32, anchor: (f32, f32), vp: (f32, f32)) {
        let z = z.clamp(self.fit_page_zoom(vp) * 0.9, 4.0);
        let ratio = z / self.zoom;
        let before = self.layout(vp);
        // content point under the anchor stays put
        let cy = anchor.1 - before.top + GAP;
        let cx = if before.content_w > before.aw { anchor.0 + self.scroll.0 } else { anchor.0 - before.x + MARGIN };
        self.zoom = z;
        self.scroll.1 = cy * ratio - GAP - anchor.1 + GAP;
        let after = self.layout(vp);
        if after.content_w > after.aw {
            self.scroll.0 = cx * ratio - anchor.0;
        }
        self.pitch = after.h + GAP;
        self.clamp(vp);
    }

    /// Fit toolbar button: reading width <-> the whole page.
    pub fn toggle_fit(&mut self, vp: (f32, f32)) {
        let fit = self.fit_page_zoom(vp);
        let page = self.index;
        self.zoom = if (self.zoom - fit).abs() < 0.01 { 1.0 } else { fit };
        self.jump_to(page, vp);
    }

    pub fn chrome_hit(&self, x: f32, y: f32, vp: (f32, f32)) -> bool {
        if self.toolbar_open && x >= vp.0 - STRIP_W {
            return true;
        }
        if !self.toolbar_open && x >= vp.0 - 58.0 && y <= 58.0 {
            return true;
        }
        if let Some((_, _, (px_, py_))) = self.hl_popup {
            if (x - px_).abs() < 26.0 && (y - py_).abs() < 26.0 {
                return true;
            }
        }
        false
    }

    fn page_top_scroll(&self, i: usize, vp: (f32, f32)) -> f32 {
        let l = self.layout(vp);
        let (_, my) = self.max_scroll(vp);
        (i as f32 * (l.h + GAP)).clamp(0.0, my)
    }

    pub fn jump_to(&mut self, i: usize, vp: (f32, f32)) {
        self.glide = None;
        self.fling = None;
        self.scroll.1 = self.page_top_scroll(i, vp);
        self.pitch = self.layout(vp).h + GAP;
        self.clamp(vp);
    }

    /// Glide to a scroll position (ease-out cubic). Far jumps land
    /// directly — no blur of twenty pages.
    fn glide_to(&mut self, to: (f32, f32), vp: (f32, f32)) {
        self.fling = None;
        let (mx, my) = self.max_scroll(vp);
        let to = (to.0.clamp(0.0, mx), to.1.clamp(0.0, my));
        let l = self.layout(vp);
        if (to.1 - self.scroll.1).abs() > 2.5 * (l.h + GAP) {
            self.scroll = to;
            return;
        }
        let ms = (160.0 + (to.1 - self.scroll.1).abs() * 0.12).min(320.0);
        self.glide = Some(Glide { from: self.scroll, to, t0: Instant::now(), ms });
    }

    pub fn glide_to_page(&mut self, i: usize, vp: (f32, f32)) {
        if self.mode == ViewMode::Spread {
            let (left, target) = (self.index - self.index % 2, i.min(self.seq.len() - 1));
            let t = target - target % 2;
            if t == left + 2 {
                self.turn_by(1);
            } else if t + 2 == left {
                self.turn_by(-1);
            } else {
                self.turn = None;
                self.index = t;
            }
            return;
        }
        let y = self.page_top_scroll(i.min(self.seq.len() - 1), vp);
        self.glide_to((self.scroll.0, y), vp);
    }

    pub fn glide_by(&mut self, dy: f32, vp: (f32, f32)) {
        if self.mode == ViewMode::Spread {
            self.turn_by(if dy > 0.0 { 1 } else { -1 });
            return;
        }
        let base = self.glide.as_ref().map_or(self.scroll, |g| g.to);
        self.glide_to((base.0, base.1 + dy), vp);
    }

    // --------------------------------------------------------- two pages

    pub fn set_mode(&mut self, mode: ViewMode, vp: (f32, f32)) {
        if mode == self.mode {
            return;
        }
        self.stop();
        self.turn = None;
        self.mode = mode;
        if mode == ViewMode::Scroll {
            self.pending_jump = Some(self.index);
        }
        let _ = vp;
    }

    fn spread_target(&self, dir: i32) -> Option<usize> {
        let left = self.index - self.index % 2;
        let t = left as i64 + 2 * dir as i64;
        (t >= 0 && (t as usize) < self.seq.len()).then_some(t as usize)
    }

    /// Animate a whole turn (keys, toolbar, pi's goto): the lower corner
    /// peels up, crosses the spine and settles on the far page.
    pub fn turn_by(&mut self, dir: i32) {
        let can = self.spread_target(dir).is_some();
        let (w, h) = self.leaf_size;
        match &mut self.turn {
            // a second tap mid-turn finishes the one in flight
            Some(t) if t.dir == dir => {
                let to = (-t.origin.0, t.origin.1);
                t.anim = Some(TurnAnim { from: t.finger, to, t0: Instant::now(), ms: 200.0, complete: true, lift: false });
            }
            Some(_) => {}
            None if can => {
                let origin = (w, h * 0.86);
                self.turn = Some(Turn {
                    dir,
                    origin,
                    finger: (w - 1.0, origin.1),
                    anim: Some(TurnAnim { from: (w - 1.0, origin.1), to: (-w, origin.1), t0: Instant::now(), ms: 640.0, complete: true, lift: true }),
                });
            }
            None => {}
        }
    }

    /// Spread mode: a drag takes hold of the leaf where you touched it.
    fn turn_drag(&mut self, dx: f32, at: (f32, f32), vp: (f32, f32)) {
        let l = self.layout(vp);
        if self.turn.as_ref().is_some_and(|t| t.anim.is_some()) {
            return;
        }
        if self.turn.is_none() {
            if self.turn_dx == 0.0 {
                self.turn_start_y = at.1;
            }
            self.turn_dx += dx;
            if self.turn_dx.abs() < 6.0 {
                return;
            }
            let dir = if self.turn_dx < 0.0 { 1 } else { -1 };
            if self.spread_target(dir).is_none() {
                return;
            }
            // the leaf's outer edge, at the height you touched
            let v = (self.turn_start_y - l.top).clamp(0.0, l.h);
            self.turn = Some(Turn { dir, origin: (l.w, v), finger: (l.w, v), anim: None });
        } else {
            self.turn_dx += dx;
        }
        let t = self.turn.as_mut().unwrap();
        let du = if t.dir > 0 { self.turn_dx } else { -self.turn_dx };
        t.finger = crate::curl::constrain(t.origin, (t.origin.0 + du, t.origin.1 + (at.1 - self.turn_start_y)), l.h);
    }

    /// Spread mode: the hand lets go — finish the turn or let it fall back.
    fn turn_release(&mut self, vx: f32) {
        self.turn_dx = 0.0;
        let w = self.leaf_size.0;
        let Some(t) = self.turn.as_mut() else { return };
        if t.anim.is_some() {
            return;
        }
        let along = -(t.dir as f32) * vx; // >0: flicked the way it's turning
        let p = t.progress(w);
        let complete = (p > 0.25 && along > -300.0) || along > 500.0;
        let to = if complete { (-t.origin.0, t.origin.1) } else { t.origin };
        let dist = (to.0 - t.finger.0).hypot(to.1 - t.finger.1);
        let ms = 150.0 + 380.0 * (dist / (2.0 * w)).min(1.0);
        t.anim = Some(TurnAnim { from: t.finger, to, t0: Instant::now(), ms, complete, lift: false });
    }

    // ------------------------------------------------------------ gestures

    /// One finger on the paper: the content follows it.
    pub fn drag(&mut self, delta: (f32, f32), at: (f32, f32), vp: (f32, f32)) {
        self.hl_popup = None;
        if self.mode == ViewMode::Spread {
            self.turn_drag(delta.0, at, vp);
            return;
        }
        self.glide = None;
        self.fling = None;
        self.scroll.0 -= delta.0;
        self.scroll.1 -= delta.1;
        self.clamp(vp);
        self.indicator_until = Some(Instant::now() + Duration::from_millis(1200));
    }

    /// The finger lifted while moving: carry on with momentum.
    pub fn fling(&mut self, finger_vel: (f32, f32)) {
        if self.mode == ViewMode::Spread {
            self.turn_release(finger_vel.0);
            return;
        }
        let v = (-finger_vel.0, -finger_vel.1);
        if v.0.hypot(v.1) > 100.0 {
            self.fling = Some(((v.0.clamp(-6000.0, 6000.0), v.1.clamp(-9000.0, 9000.0)), Instant::now()));
        }
    }

    pub fn stop(&mut self) {
        self.fling = None;
        self.glide = None;
        if self.mode == ViewMode::Spread && self.turn.as_ref().is_some_and(|t| t.anim.is_none()) {
            self.turn_release(0.0);
        }
    }

    pub fn wheel(&mut self, d: (f32, f32), at: (f32, f32), vp: (f32, f32)) {
        if self.mode == ViewMode::Spread {
            // a two-finger touchpad swipe holds the leaf like a finger
            if d.0.abs() >= d.1.abs() * 0.5 || self.turn.is_some() {
                self.turn_drag(-d.0, at, vp);
            } else {
                self.turn_dx = 0.0;
                if d.1.abs() > 24.0 && self.turn.is_none() {
                    self.turn_by(if d.1 > 0.0 { 1 } else { -1 });
                }
            }
            return;
        }
        self.glide = None;
        self.fling = None;
        self.scroll.0 += d.0;
        self.scroll.1 += d.1;
        self.clamp(vp);
        self.indicator_until = Some(Instant::now() + Duration::from_millis(1200));
    }

    pub fn pinch(&mut self, zoom: f32, centroid: (f32, f32), delta: (f32, f32), vp: (f32, f32)) {
        if self.mode == ViewMode::Spread {
            return; // the spread is always fitted
        }
        self.pinching_at = Some(Instant::now());
        self.hl_popup = None;
        self.stop();
        self.scroll.0 -= delta.0;
        self.scroll.1 -= delta.1;
        self.set_zoom(zoom, centroid, vp);
    }

    /// Advance glides and momentum to `now`. True while still moving (the
    /// caller asks for another vsync frame).
    pub fn animate(&mut self, vp: (f32, f32)) -> bool {
        if self.mode == ViewMode::Spread {
            self.pending_jump = None;
            if self.index % 2 == 1 {
                self.index -= 1;
            }
            let l = self.layout(vp);
            self.leaf_size = (l.w, l.h);
            let Some(t) = self.turn.as_mut() else { return false };
            let Some(a) = t.anim else { return false };
            let k = (a.t0.elapsed().as_secs_f32() * 1000.0 / a.ms).min(1.0);
            let e = if a.lift {
                if k < 0.5 { 4.0 * k * k * k } else { 1.0 - (-2.0 * k + 2.0).powi(3) / 2.0 }
            } else {
                1.0 - (1.0 - k).powi(3)
            };
            let mut f = (a.from.0 + (a.to.0 - a.from.0) * e, a.from.1 + (a.to.1 - a.from.1) * e);
            if a.lift {
                f.1 -= l.h * 0.22 * (std::f32::consts::PI * k).sin();
            }
            t.finger = crate::curl::constrain(t.origin, f, l.h);
            if k >= 1.0 {
                let (dir, done) = (t.dir, a.complete);
                self.turn = None;
                if done {
                    if let Some(i) = self.spread_target(dir) {
                        self.index = i;
                    }
                }
                self.indicator_until = Some(Instant::now() + Duration::from_millis(1200));
                return false;
            }
            return true;
        }
        if let Some(i) = self.pending_jump.take() {
            self.jump_to(i, vp);
        }
        let pitch = self.layout(vp).h + GAP;
        if self.pitch > 0.0 && (pitch - self.pitch).abs() > 0.25 {
            self.scroll.1 *= pitch / self.pitch;
            if let Some(g) = self.glide.as_mut() {
                g.from.1 *= pitch / self.pitch;
                g.to.1 *= pitch / self.pitch;
            }
            self.clamp(vp);
        }
        self.pitch = pitch;
        let mut moving = false;
        if let Some(g) = &self.glide {
            let t = (g.t0.elapsed().as_secs_f32() * 1000.0 / g.ms).min(1.0);
            let e = 1.0 - (1.0 - t).powi(3);
            self.scroll = (g.from.0 + (g.to.0 - g.from.0) * e, g.from.1 + (g.to.1 - g.from.1) * e);
            if t >= 1.0 {
                self.glide = None;
            } else {
                moving = true;
            }
            self.indicator_until = Some(Instant::now() + Duration::from_millis(1200));
        }
        if let Some((v, last)) = self.fling {
            let dt = last.elapsed().as_secs_f32().min(0.05);
            let before = self.scroll;
            self.scroll.0 += v.0 * dt;
            self.scroll.1 += v.1 * dt;
            self.clamp(vp);
            let decay = (-2.4 * dt).exp();
            let v = (v.0 * decay, v.1 * decay);
            let stuck = (self.scroll.0 - before.0).abs() < 0.01 && (self.scroll.1 - before.1).abs() < 0.01 && dt > 0.0;
            if v.0.hypot(v.1) < 12.0 || stuck {
                self.fling = None;
            } else {
                self.fling = Some((v, Instant::now()));
                moving = true;
            }
            self.indicator_until = Some(Instant::now() + Duration::from_millis(1200));
        }
        moving
    }

    /// Long-press on the page: offer to remove a highlight under the finger.
    pub fn hold(&mut self, at: (f32, f32), vp: (f32, f32)) -> bool {
        let l = self.layout(vp);
        let Some((i, px_, py_)) = l.hit(at.0, at.1) else { return false };
        let key = self.seq[i].ink_key();
        let Some(m) = self.pages.get(&key) else { return false };
        let hit = m.base.highlights.iter().find(|s| s.points.iter().any(|p| (p.x - px_).hypot(p.y - py_) <= p.r + 14.0));
        if let Some(s) = hit {
            let b = s.bounds();
            self.hl_popup = Some((key, s.id, (l.page_x(i) + b[2] * l.s + 22.0, l.page_y(i) + b[1] * l.s - 4.0)));
            return true;
        }
        false
    }

    /// Timers; returns whether a repaint is needed.
    pub fn tick(&mut self) -> bool {
        let mut dirty = false;
        if self.pinching_at.is_some_and(|t| t.elapsed() > Duration::from_millis(160)) {
            self.pinching_at = None;
            dirty = true;
        }
        for m in self.pages.values_mut() {
            let done: Vec<u64> = m.animating.iter().filter(|(_, t)| t.elapsed().as_secs_f32() > DRAW_IN).map(|(id, _)| *id).collect();
            for id in done {
                m.animating.remove(&id);
                m.full_dirty = true;
                dirty = true;
            }
        }
        if self.indicator_until.is_some_and(|t| Instant::now() > t) {
            self.indicator_until = None;
            dirty = true;
        }
        dirty
    }

    pub fn animating_ink(&self) -> bool {
        self.pages.values().any(|m| !m.animating.is_empty())
    }

    // ---------------------------------------------------------------- ink

    pub fn ink_begin(&mut self, at: (f32, f32), pressure: f32, eraser: bool, vp: (f32, f32), window: &mut Window) {
        self.hl_popup = None;
        if self.turn.is_some() {
            return;
        }
        self.stop();
        let l = self.layout(vp);
        let Some((page, x, y)) = l.hit(at.0, at.1) else { return };
        let key = self.seq[page].ink_key();
        let tool = if eraser { Tool::Eraser } else { self.tool };
        let width = self.width;
        let Some(m) = self.pages.get_mut(&key).filter(|m| m.ready) else { return };
        let before = m.snap();
        let live = match tool {
            Tool::Pen => Some(Live::new(x, y, pressure, width)),
            Tool::Highlighter => Some(Live::new(x, y, 0.5, Width::Medium)),
            Tool::Eraser => None,
        };
        self.live = Some(LiveInk { key, page, tool, live, last: (x, y), changed: false, before });
        if tool == Tool::Eraser {
            self.erase_to(x, y, window);
        }
    }

    pub fn ink_move(&mut self, at: (f32, f32), pressure: f32, vp: (f32, f32), window: &mut Window) {
        let l = self.layout(vp);
        let Some(li) = self.live.as_mut() else { return };
        // the stroke stays on the page it started on
        let (x, y) = ((at.0 - l.page_x(li.page)) / l.s, (at.1 - l.page_y(li.page)) / l.s);
        match li.tool {
            Tool::Pen => li.live.as_mut().unwrap().push(x, y, pressure, l.s),
            Tool::Highlighter => li.live.as_mut().unwrap().push(x, y, 0.5, l.s),
            Tool::Eraser => self.erase_to(x, y, window),
        }
    }

    /// Rub from the last eraser position to (x, y), in nib-sized steps.
    fn erase_to(&mut self, x: f32, y: f32, window: &mut Window) {
        let Some(li) = self.live.as_mut() else { return };
        let (x0, y0) = li.last;
        li.last = (x, y);
        let key = li.key.clone();
        let Some(m) = self.pages.get_mut(&key) else { return };
        let steps = (((x - x0).hypot(y - y0)) / (ERASER_R * 0.5)).ceil().max(1.0) as usize;
        let offsets = |text: &str, size_: f32| -> Vec<f32> {
            let run = TextRun { len: text.len(), font: gpui::font(SERIF), color: gpui::black(), background_color: None, underline: None, strikethrough: None };
            let line = window.text_system().shape_line(SharedString::from(text.to_owned()), px(size_), &[run], None);
            let mut v: Vec<f32> = text.char_indices().map(|(i, _)| f32::from(line.x_for_index(i))).collect();
            v.push(f32::from(line.width));
            v
        };
        let mut changed = false;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let (ex, ey) = (x0 + (x - x0) * t, y0 + (y - y0) * t);
            let mut next = m.base.next_stroke.max(1);
            if let Some(b) = ink::erase_split(&mut m.base.strokes, ex, ey, ERASER_R, &mut next) {
                m.dirty(b);
                changed = true;
            }
            if let Some(b) = ink::erase_split(&mut m.base.highlights, ex, ey, ERASER_R, &mut next) {
                m.dirty(b);
                changed = true;
            }
            m.base.next_stroke = next;
            let (ids, b) = ink::erase_patches(&mut m.base, ex, ey, ERASER_R, &offsets);
            if let Some(b) = b {
                m.dirty(b);
            }
            if !ids.is_empty() {
                changed = true;
                m.patch_dirty.extend(ids);
            }
        }
        if changed {
            m.edit_gen += 1;
            li.changed = true;
        }
    }

    /// Pen up: commit the stroke. Returns the page key if it changed.
    pub fn ink_end(&mut self, _window: &mut Window) -> Option<String> {
        let li = self.live.take()?;
        let m = self.pages.get_mut(&li.key)?;
        match li.tool {
            Tool::Pen | Tool::Highlighter => {
                let live = li.live?;
                let id = m.next_id();
                let mut stroke = live.finish(id);
                let wash = li.tool == Tool::Highlighter;
                if wash {
                    stroke.gray = HL_GRAY;
                    for p in &mut stroke.points {
                        p.r = HL_R;
                    }
                }
                m.history.checkpoint(li.before);
                if let Some(r) = m.raster.as_mut() {
                    r.add_mark(Mark { stroke: &stroke, rgb: if wash { 0xb4b4b4 } else { ink::ink_rgb(0) }, wash });
                }
                if wash {
                    m.base.highlights.push(stroke);
                } else {
                    m.base.strokes.push(stroke);
                }
                m.edit_gen += 1;
            }
            Tool::Eraser => {
                if !li.changed {
                    return None;
                }
                m.history.checkpoint(li.before);
            }
        }
        Some(li.key)
    }

    pub fn ink_cancel(&mut self) {
        if let Some(li) = self.live.take() {
            if li.changed {
                if let Some(m) = self.pages.get_mut(&li.key) {
                    (m.base.strokes, m.base.highlights) = li.before;
                    m.full_dirty = true;
                    m.edit_gen += 1;
                }
            }
        }
    }

    fn undo_redo(&mut self, redo: bool) -> Option<String> {
        let key = self.key();
        let m = self.pages.get_mut(&key)?;
        let cur = m.snap();
        let snap = if redo { m.history.redo(cur) } else { m.history.undo(cur) }?;
        (m.base.strokes, m.base.highlights) = snap;
        m.full_dirty = true;
        m.edit_gen += 1;
        Some(key)
    }

    fn can_undo(&self) -> (bool, bool) {
        self.pages.get(&self.key()).map_or((false, false), |m| (m.history.can_undo(), m.history.can_redo()))
    }
}

// ================================================================ Papier

impl Papier {
    fn dv(&mut self) -> Option<&mut DocView> {
        match &mut self.screen {
            Screen::Doc(dv) => Some(dv),
            Screen::Home => None,
        }
    }

    pub fn doc_opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_pages(cx);
        self.start_pi(cx);
        let _ = window;
    }

    /// Load the current page and its neighbours; forget far-away rasters.
    fn ensure_pages(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let i = dv.index;
        let wanted: Vec<(String, SeqEntry)> = (i.saturating_sub(2)..=(i + 3).min(dv.seq.len() - 1)).map(|j| (dv.seq[j].ink_key(), dv.seq[j])).collect();
        let keep: HashSet<String> = wanted.iter().map(|(k, _)| k.clone()).collect();
        let mut retired = Vec::new();
        let far: HashSet<String> = (i.saturating_sub(4)..=(i + 5).min(dv.seq.len() - 1)).map(|j| dv.seq[j].ink_key()).collect();
        for (k, m) in dv.pages.iter_mut() {
            if !keep.contains(k) && !far.contains(k) {
                if let Some(img) = m.raster.take().and_then(|r| r.image) {
                    retired.push(img);
                }
            }
        }
        dv.pages.retain(|k, m| far.contains(k) || m.sync == Sync::Dirty || m.sync == Sync::Saving);
        let missing: Vec<(String, SeqEntry)> = wanted.into_iter().filter(|(k, _)| !dv.pages.contains_key(k)).collect();
        for (k, e) in &missing {
            dv.pages.insert(k.clone(), PageModel::new(*e));
        }
        self.retired.borrow_mut().extend(retired);
        for (k, _) in missing {
            self.load_page(k, false, cx);
        }
    }

    /// Fetch a page's ink: local pending beats a stale mirror; pi patches
    /// are always the server's; offline falls back to the last copy seen.
    fn load_page(&mut self, key: String, reload: bool, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let (doc_id, version) = (dv.doc.id.clone(), dv.doc.version.clone());
        let client = self.client.clone();
        let k = key.clone();
        let work = bg(cx, move || {
            let remote = client.fetch_ink(&doc_id, &k);
            if let Ok(Some(p)) = &remote {
                store::save_offline_ink(&client.root, &doc_id, &k, p);
            }
            let remote = match remote {
                Ok(r) => Ok(r),
                Err(e) => store::load_offline_ink(&client.root, &doc_id, &k).map(Some).ok_or(e),
            };
            match store::pending_ink(&doc_id, &k, &version) {
                Some(mut local) => {
                    if let Ok(Some(r)) = &remote {
                        local.patches = r.patches.clone();
                        local.next_patch = local.next_patch.max(r.next_patch);
                    }
                    Ok(local)
                }
                None => remote.map(Option::unwrap_or_default),
            }
        });
        let doc_id = self.dv().map(|d| d.doc.id.clone()).unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(dv) = this.dv().filter(|d| d.doc.id == doc_id) else { return };
                let busy_here = dv.live.as_ref().is_some_and(|l| l.key == key);
                let Some(m) = dv.pages.get_mut(&key) else { return };
                if reload && (busy_here || !matches!(m.sync, Sync::Clean | Sync::Saved)) {
                    return; // the user is mid-edit here: their copy wins, it syncs next
                }
                match result {
                    Ok(page) => {
                        if page != m.base || !m.ready {
                            m.base = page;
                            m.full_dirty = true;
                            m.edit_gen += 1;
                        }
                        m.ready = true;
                        if m.sync == Sync::Loading {
                            m.sync = Sync::Clean;
                        }
                    }
                    Err(e) => {
                        m.ready = true; // a blank page you can still write on
                        m.sync = Sync::Error(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The doc's version moved (web viewer, tablet, iPad, cloud pi): pages
    /// nobody is editing here reload from the merged truth.
    pub fn reload_clean_pages(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let keys: Vec<String> = dv.pages.iter().filter(|(_, m)| matches!(m.sync, Sync::Clean | Sync::Saved)).map(|(k, _)| k.clone()).collect();
        for k in keys {
            self.load_page(k, true, cx);
        }
        self.ensure_pages(cx);
    }

    /// An edit landed on `key`: schedule the save after papier's writing
    /// pause, and push any rubbed pi patches.
    pub fn page_edited(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let doc_id = dv.doc.id.clone();
        let Some(m) = dv.pages.get_mut(key) else { return };
        m.sync = Sync::Dirty;
        m.save_gen += 1;
        let generation = m.save_gen;
        let patches: Vec<(u64, Option<crate::model::InkPatch>)> =
            m.patch_dirty.drain().map(|id| (id, m.base.patches.iter().find(|p| p.id == id).cloned())).collect();
        let next_stroke = m.base.next_stroke;
        if !patches.is_empty() {
            let client = self.client.clone();
            let k = key.to_owned();
            let d = doc_id.clone();
            bg(cx, move || {
                for (id, patch) in patches {
                    let _ = match patch {
                        Some(p) => client.replace_patch(&d, &k, &p, next_stroke),
                        None => client.erase_patch(&d, &k, id),
                    };
                }
            })
            .detach();
        }
        let key = key.to_owned();
        cx.spawn(async move |this, cx| {
            Timer::after(SAVE_PAUSE).await;
            let _ = this.update(cx, |this, cx| {
                let current = this.dv().filter(|d| d.doc.id == doc_id).and_then(|d| d.pages.get(&key)).map(|m| m.save_gen);
                if current == Some(generation) {
                    this.flush_page(&key, cx);
                }
            });
        })
        .detach();
    }

    fn flush_page(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let (doc_id, version) = (dv.doc.id.clone(), dv.doc.version.clone());
        let page_no = dv.seq.iter().position(|e| e.ink_key() == key).map(|i| i + 1).unwrap_or(1);
        let auto = dv.pi.mode == "auto";
        let Some(m) = dv.pages.get_mut(key) else { return };
        if m.sync != Sync::Dirty {
            return;
        }
        m.sync = Sync::Saving;
        let generation = m.save_gen;
        let page = m.base.clone();
        let client = self.client.clone();
        let (d, k) = (doc_id.clone(), key.to_owned());
        // Persist locally inside the background job itself, so a save that
        // outlives the document view (closing, quitting) still lands.
        let readonly = std::env::var_os("PAPIER_READONLY").is_some();
        let work = bg(cx, move || match if readonly { Ok(()) } else { client.post_ink(&d, &k, &page) } {
            Ok(()) if readonly => Ok(()),
            Ok(()) => {
                store::remember_pending(&d, &k, &page, &version, false);
                if auto {
                    let _ = client.pi(&format!("pause?{}&page={page_no}", pi_query(&d)), true);
                }
                Ok(())
            }
            Err(e) => {
                store::remember_pending(&d, &k, &page, &version, true);
                Err(e.to_string())
            }
        });
        let key = key.to_owned();
        cx.spawn(async move |this, cx| {
            let r = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(m) = this.dv().filter(|d| d.doc.id == doc_id).and_then(|d| d.pages.get_mut(&key)) else { return };
                if m.save_gen == generation {
                    m.sync = match r {
                        Ok(()) => Sync::Saved,
                        Err(e) => Sync::Error(format!("Offline — kept on this laptop ({e})")),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn flush_all(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let keys: Vec<String> = dv.pages.iter().filter(|(_, m)| m.sync == Sync::Dirty).map(|(k, _)| k.clone()).collect();
        for k in keys {
            self.flush_page(&k, cx);
        }
    }

    /// The current page moved (scrolling, a glide, pi's goto): remember
    /// it, tell pi, save what was being written, load what's coming.
    pub fn index_changed(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        dv.hl_popup = None;
        let (id, index) = (dv.doc.id.clone(), dv.index);
        let current = dv.key();
        let dirty: Vec<String> = dv.pages.iter().filter(|(k, m)| m.sync == Sync::Dirty && **k != current).map(|(k, _)| k.clone()).collect();
        self.settings.set(&format!("pos-{id}"), serde_json::json!(index));
        for k in dirty {
            self.flush_page(&k, cx);
        }
        self.ensure_pages(cx);
        self.pi_call(&format!("page?{}&page={}", pi_query(&id), index + 1), cx);
    }

    pub fn goto_page(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let vp = self.viewport;
        let Some(dv) = self.dv() else { return };
        if index >= dv.seq.len() {
            return;
        }
        dv.glide_to_page(index, vp);
        cx.notify();
    }

    fn turn(&mut self, delta: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        if dv.mode == ViewMode::Spread {
            dv.turn_by(delta.signum() as i32);
            cx.notify();
            return;
        }
        // from the page you're on — or the one a glide is heading for
        let target = (dv.index as i64 + delta).clamp(0, dv.seq.len() as i64 - 1);
        self.goto_page(target as usize, window, cx);
    }

    /// A fresh note page after this one, announced to the cloud.
    fn add_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let next = dv.seq.iter().filter_map(|e| if let SeqEntry::Note(n) = e { Some(*n) } else { None }).max().unwrap_or(0) + 1;
        dv.seq.insert(dv.index + 1, SeqEntry::Note(next));
        let state = DocState { next_note: next + 1, pos: dv.index + 1, seq: dv.seq.clone() };
        let id = dv.doc.id.clone();
        let i = dv.index + 1;
        let client = self.client.clone();
        let readonly = std::env::var_os("PAPIER_READONLY").is_some();
        let work = bg(cx, move || if readonly { Ok(()) } else { client.post_state(&id, &state) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = work.await {
                let _ = this.update(cx, |this, cx| {
                    this.toast(format!("Page added here; sync failed: {e}"));
                    cx.notify();
                });
            }
        })
        .detach();
        self.ensure_pages(cx);
        self.goto_page(i, window, cx);
    }

    fn undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        let Some(key) = self.dv().and_then(|dv| dv.undo_redo(redo)) else { return };
        self.page_edited(&key, cx);
    }

    fn remove_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let Some((key, id, _)) = dv.hl_popup.take() else { return };
        let Some(m) = dv.pages.get_mut(&key) else { return };
        let before = m.snap();
        let n = m.base.highlights.len();
        m.base.highlights.retain(|s| s.id != id);
        if m.base.highlights.len() != n {
            m.history.checkpoint(before);
            m.full_dirty = true;
            m.edit_gen += 1;
            self.page_edited(&key, cx);
        }
    }

    fn set_tool(&mut self, tool: Tool) {
        let mut save_width = None;
        if let Some(dv) = self.dv() {
            if dv.tool == tool && tool == Tool::Pen {
                // second tap on the armed pen: cycle its width
                let i = Width::ALL.iter().position(|w| *w == dv.width).unwrap_or(1);
                dv.width = Width::ALL[(i + 1) % 3];
                save_width = Some(dv.width.key());
            }
            dv.tool = tool;
        }
        if let Some(w) = save_width {
            self.settings.set("width", serde_json::json!(w));
        }
    }

    /// Vertical scrolling <-> two pages turned like a book.
    fn toggle_view(&mut self) {
        let vp = self.viewport;
        let Some(dv) = self.dv() else { return };
        let next = if dv.mode == ViewMode::Scroll { ViewMode::Spread } else { ViewMode::Scroll };
        dv.set_mode(next, vp);
        self.transition_view();
        self.settings.set("view_mode", serde_json::json!(if next == ViewMode::Spread { "spread" } else { "scroll" }));
    }

    fn toggle_toolbar(&mut self) {
        let vp = self.viewport;
        let Some(dv) = self.dv() else { return };
        dv.toolbar_open = !dv.toolbar_open;
        let open = dv.toolbar_open;
        let page = dv.index;
        dv.jump_to(page, vp);
        self.settings.set("toolbar", serde_json::json!(open));
    }

    // ------------------------------------------------------------------ pi

    fn pi_call(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(id) = self.dv().map(|d| d.doc.id.clone()) else { return };
        let client = self.client.clone();
        let path = path.to_owned();
        let work = bg(cx, move || client.pi(&path, true).ok());
        cx.spawn(async move |this, cx| {
            if let Some(st) = work.await {
                let _ = this.update(cx, |this, cx| {
                    this.pi_apply(&id, st, cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Open the remote pi session and poll its events: half a second while
    /// pi works (generated ink feels live), a second and a half when idle.
    fn start_pi(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let id = dv.doc.id.clone();
        let want = dv.pi.mode.clone();
        let q = pi_query(&id);
        self.pi_call(&format!("open?{q}"), cx);
        self.pi_call(&format!("mode?{q}&mode={want}"), cx);
        let index = self.dv().map(|d| d.index + 1).unwrap_or(1);
        self.pi_call(&format!("page?{q}&page={index}"), cx);
        let task = cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some((client, since, busy))) = this.update(cx, |this, _| {
                    let client = this.client.clone();
                    this.dv().filter(|d| d.doc.id == id).map(|d| (client, d.pi.since, d.pi.busy))
                }) else {
                    break;
                };
                let path = format!("events?{}&since={since}", pi_query(&id));
                let st = bg_async(cx, move || client.pi(&path, false).ok()).await;
                if let Some(st) = st {
                    let id = id.clone();
                    if this.update(cx, |this, cx| {
                        this.pi_apply(&id, st, cx);
                        cx.notify();
                    })
                    .is_err()
                    {
                        break;
                    }
                }
                Timer::after(Duration::from_millis(if busy { 500 } else { 1500 })).await;
            }
        });
        if let Some(dv) = self.dv() {
            dv.pi._task = Some(task);
        }
    }

    fn pi_apply(&mut self, id: &str, st: PiState, cx: &mut Context<Self>) {
        let Some(dv) = self.dv().filter(|d| d.doc.id == id) else { return };
        if let Some(e) = st.epoch {
            if e != dv.pi.epoch {
                // the service restarted: its event ids started over
                dv.pi.epoch = e;
                dv.pi.since = 0;
            }
        }
        dv.pi.busy = st.busy;
        if !st.mode.is_empty() {
            dv.pi.mode = st.mode.clone();
        }
        if !st.font.is_empty() {
            dv.pi.font = st.font.clone();
        }
        let mut refresh: Vec<usize> = Vec::new();
        let mut goto = None;
        let mut seq_changed = false;
        let mut notices = Vec::new();
        for ev in &st.events {
            dv.pi.since = dv.pi.since.max(ev.id);
            match ev.kind.as_str() {
                "patch" => refresh.extend(ev.page.filter(|p| *p >= 1).map(|p| p as usize - 1)),
                "goto" => goto = ev.page.filter(|p| *p >= 1).map(|p| p as usize - 1),
                "seq" => seq_changed = true,
                "notice" => notices.extend(ev.text.clone().filter(|t| !t.is_empty())),
                "turn" => {
                    dv.pi.busy = ev.state.as_deref() == Some("start");
                    if ev.state.as_deref() == Some("end") {
                        refresh.push(dv.index);
                    }
                }
                _ => {}
            }
        }
        refresh.sort_unstable();
        refresh.dedup();
        let keys: Vec<String> = refresh.into_iter().filter_map(|i| dv.seq.get(i).map(|e| e.ink_key())).collect();
        for n in notices {
            self.toast(n);
        }
        for k in keys {
            self.refresh_patches(k, cx);
        }
        if seq_changed {
            self.refresh(cx);
        }
        if let Some(i) = goto {
            let vp = self.viewport;
            if let Some(dv) = self.dv() {
                if i < dv.seq.len() && i != dv.index {
                    dv.glide_to_page(i, vp);
                }
            }
        }
    }

    /// pi drew or erased: adopt the server's patches (the user's strokes
    /// stay local truth) and animate the new ones in.
    fn refresh_patches(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        if !dv.pages.contains_key(&key) {
            return;
        }
        let doc_id = dv.doc.id.clone();
        let client = self.client.clone();
        let (d, k) = (doc_id.clone(), key.clone());
        let work = bg(cx, move || client.fetch_ink(&d, &k).ok().flatten());
        cx.spawn(async move |this, cx| {
            let Some(remote) = work.await else { return };
            let _ = this.update(cx, |this, cx| {
                let Some(m) = this.dv().filter(|d| d.doc.id == doc_id).and_then(|d| d.pages.get_mut(&key)) else { return };
                let known: HashSet<u64> = m.base.patches.iter().map(|p| p.id).collect();
                if remote.patches == m.base.patches {
                    return;
                }
                for p in &remote.patches {
                    if !known.contains(&p.id) {
                        m.animating.insert(p.id, Instant::now());
                    }
                }
                m.base.patches = remote.patches;
                m.base.next_patch = m.base.next_patch.max(remote.next_patch);
                m.base.next_stroke = m.base.next_stroke.max(remote.next_stroke);
                m.full_dirty = true;
                m.edit_gen += 1;
                cx.notify();
            });
        })
        .detach();
    }

    fn pi_toggle_mode(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let next = if dv.pi.mode == "auto" { "quiet" } else { "auto" };
        dv.pi.mode = next.into();
        let id = dv.doc.id.clone();
        self.settings.set(&format!("pimode-{id}"), serde_json::json!(next));
        self.pi_call(&format!("mode?{}&mode={next}", pi_query(&id)), cx);
        self.toast(if next == "auto" { "Pi is on — it answers when you pause" } else { "Pi is quiet" });
    }

    fn pi_nudge(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        dv.pi.busy = true;
        let (id, page) = (dv.doc.id.clone(), dv.index + 1);
        let key = dv.key();
        // the page pi reads must include what was just written
        if dv.pages.get(&key).is_some_and(|m| m.sync == Sync::Dirty) {
            self.flush_page(&key, cx);
        }
        self.pi_call(&format!("nudge?{}&page={page}", pi_query(&id)), cx);
    }

    fn pi_cycle_font(&mut self, cx: &mut Context<Self>) {
        let Some(dv) = self.dv() else { return };
        let order = ["serif", "script", "sans", "garamond"];
        let next = order[(order.iter().position(|f| *f == dv.pi.font).unwrap_or(0) + 1) % order.len()];
        dv.pi.font = next.into();
        let id = dv.doc.id.clone();
        self.pi_call(&format!("font?{}&font={next}", pi_query(&id)), cx);
    }

    // ------------------------------------------------------------- keyboard

    pub fn doc_key(&mut self, key: &str, ctrl: bool, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        let vp = self.viewport;
        match (ctrl, key) {
            (true, "z") if shift => self.undo(true, cx),
            (true, "z") => self.undo(false, cx),
            (true, "y") => self.undo(true, cx),
            (false, "escape") => {
                if self.dv().is_some_and(|d| d.hl_popup.is_some()) {
                    if let Some(dv) = self.dv() {
                        dv.hl_popup = None;
                    }
                } else {
                    self.go_home(cx)
                }
            }
            (false, "left") => self.turn(-1, window, cx),
            (false, "right") => self.turn(1, window, cx),
            (false, "up" | "down" | "pageup" | "pagedown" | "space") => {
                let step = match key {
                    "up" => -110.0,
                    "down" => 110.0,
                    "pageup" => -(vp.1 * 0.85),
                    _ => vp.1 * 0.85,
                };
                if let Some(dv) = self.dv() {
                    dv.glide_by(if shift && key == "space" { -step } else { step }, vp);
                }
            }
            (false, "home") => self.goto_page(0, window, cx),
            (false, "end") => {
                let last = self.dv().map(|d| d.seq.len() - 1).unwrap_or(0);
                self.goto_page(last, window, cx)
            }
            (false, "p") => self.set_tool(Tool::Pen),
            (false, "e") => self.set_tool(Tool::Eraser),
            (false, "h") => self.set_tool(Tool::Highlighter),
            (false, "f") => {
                if let Some(dv) = self.dv() {
                    dv.finger_draws = !dv.finger_draws;
                }
            }
            (false, "t") => self.toggle_toolbar(),
            (false, "v") => self.toggle_view(),
            (false, "g") => self.dialog = Some(Dialog::GoTo { digits: String::new() }),
            (false, "n") => self.add_page(window, cx),
            (false, ".") => self.pi_nudge(cx),
            (false, "m") => self.pi_toggle_mode(cx),
            (false, "0") => self.set_doc_zoom(None),
            (false, "w") => {
                if let Some(dv) = self.dv() {
                    dv.toggle_fit(vp);
                }
            }
            (_, "=" | "+") | (false, "-") => {
                let z = self.dv().map(|d| if key == "-" { d.zoom / 1.2 } else { d.zoom * 1.2 });
                self.set_doc_zoom(z);
            }
            _ => {}
        }
    }

    /// Zoom about the window centre (None = reading width) and remember it.
    fn set_doc_zoom(&mut self, z: Option<f32>) {
        let vp = self.viewport;
        let Some(dv) = self.dv() else { return };
        match z {
            Some(z) => dv.set_zoom(z, (vp.0 / 2.0, vp.1 / 2.0), vp),
            None => {
                let page = dv.index;
                dv.zoom = 1.0;
                dv.jump_to(page, vp);
            }
        }
        let z = dv.zoom;
        self.settings.set("zoom", serde_json::json!(z));
    }

    // --------------------------------------------------------------- render

    /// A page's current image, kept up to date without blocking the frame:
    /// full renders (first sight, zoom, pi patches, undo) run on a
    /// background thread while the previous image — scaled if need be —
    /// stays on screen; a pen stroke or eraser rub patches the raster in
    /// place, which is cheap. A build that finishes after the page changed
    /// again is discarded and redone.
    fn page_image(&mut self, index: usize, l: &Layout, sf: f32, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let paper = paper(&self.settings.paper);
        let dark = crate::theme::dark().then(|| crate::theme::dark_paper(&self.settings.paper));
        let (key, print_url, fallback) = {
            let Screen::Doc(dv) = &self.screen else { return None };
            let e = *dv.seq.get(index)?;
            let (url, fallback) = match e {
                SeqEntry::Pdf(p) => (
                    Some(self.client.page_url(&dv.doc, p)),
                    dv.doc.has_source.then(|| (self.client.preview_url(&dv.doc, p), dv.doc.meta.crop.clone(), dv.doc.meta.margins.clone())),
                ),
                SeqEntry::Note(_) => (None, None),
            };
            (e.ink_key(), url, fallback)
        };
        let print = print_url.and_then(|u| self.print(&u, fallback, cx));
        let pinching = self.dv()?.pinching_at.is_some();
        let k = (l.h * sf).min(MAX_RASTER) / l.h;
        let (w, h) = ((l.w * k).round() as u32, (l.h * k).round() as u32);
        let page_scale = w as f32 / self.dv()?.doc.page_w();
        let doc_id = self.dv()?.doc.id.clone();
        let mut retired = Vec::new();
        let dv = self.dv()?;
        let m = dv.pages.get_mut(&key).filter(|m| m.ready)?;
        let stale = m.raster.as_ref().is_none_or(|r| !r.same_setup(w, h, paper, &print, dark));
        if (stale || m.full_dirty) && !m.building && !(pinching && m.raster.is_some()) {
            m.building = true;
            let generation = m.edit_gen;
            let base = m.base.clone();
            let skip: HashSet<u64> = m.animating.keys().copied().collect();
            let job = bg(cx, move || {
                let mut r = PageRaster::new(w, h, page_scale, paper, print, dark)?;
                r.render_all(marks(&base, &skip).into_iter());
                r.flush();
                Some(r)
            });
            let key = key.clone();
            cx.spawn(async move |this, cx| {
                let raster = job.await;
                let _ = this.update(cx, |this, cx| {
                    let Some(m) = this.dv().filter(|d| d.doc.id == doc_id).and_then(|d| d.pages.get_mut(&key)) else { return };
                    m.building = false;
                    if m.edit_gen != generation {
                        cx.notify(); // edited meanwhile: the next frame rebuilds
                        return;
                    }
                    if let Some(r) = raster {
                        let old = m.raster.replace(r).and_then(|o| o.image);
                        m.full_dirty = false;
                        m.dirty_rect = None;
                        this.retired.borrow_mut().extend(old);
                    }
                    cx.notify();
                });
            })
            .detach();
        } else if !stale && !m.full_dirty {
            if let (Some(r), Some(b)) = (m.raster.as_mut(), m.dirty_rect.take()) {
                let pad = ERASER_R;
                r.render_page_rect([b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad], marks(&m.base, &HashSet::new()).into_iter());
            }
        }
        if let Some(r) = m.raster.as_mut() {
            if let Some(old) = r.flush() {
                retired.push(old);
            }
        }
        let img = m.raster.as_ref().and_then(|r| r.image.clone());
        self.retired.borrow_mut().extend(retired);
        img
    }

    pub fn render_doc(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let vp = self.viewport;
        let sf = window.scale_factor();
        let Some(dv) = self.dv() else { return div().into_any_element() };
        if dv.animate(vp) || dv.animating_ink() {
            window.request_animation_frame();
        }
        let l = dv.layout(vp);
        if dv.mode == ViewMode::Scroll {
            dv.index = l.current();
        }
        if dv.reported != Some(dv.index) {
            dv.reported = Some(dv.index);
            self.index_changed(cx);
        }
        let Some(dv) = self.dv() else { return div().into_any_element() };
        let visible = l.visible();
        // spread mode warms the neighbouring spreads so a turn never waits
        let ahead = if l.spread.is_some() {
            visible.start.saturating_sub(2)..(visible.start + 4).min(dv.seq.len())
        } else {
            visible.start.saturating_sub(1)..(visible.end + 2).min(dv.seq.len())
        };
        let turn = dv.turn;

        // Every visible page, plus the next ones warmed in the background.
        let mut images: Vec<(usize, Option<Arc<RenderImage>>)> = Vec::new();
        let mut all: HashMap<usize, Arc<RenderImage>> = HashMap::new();
        for i in ahead.clone() {
            let img = self.page_image(i, &l, sf, cx);
            if let Some(img) = &img {
                all.insert(i, img.clone());
            }
            if visible.contains(&i) {
                images.push((i, img));
            }
        }
        // the turning leaf: what lies under it, and the curl itself
        let turn_paint = match (turn, l.spread) {
            (Some(t), Some((left, spine))) => {
                let page = |i: i64| (i >= 0 && (i as usize) < l.n).then(|| all.get(&(i as usize)).cloned()).flatten();
                let l0 = left as i64;
                let (base_l, base_r, front, back) =
                    if t.dir > 0 { (page(l0), page(l0 + 3), page(l0 + 1), page(l0 + 2)) } else { (page(l0 - 2), page(l0 + 1), page(l0), page(l0 - 1)) };
                let (fs, bs) = (front.map(crate::curl::Src::new), back.map(crate::curl::Src::new));
                let [pr, pg, pb] = if crate::theme::dark() { crate::theme::dark_paper(&self.settings.paper) } else { paper(&self.settings.paper) };
                let curl = crate::curl::Curl {
                    w: l.w,
                    h: l.h,
                    forward: t.dir > 0,
                    origin: t.origin,
                    finger: t.finger,
                    front: fs.as_ref(),
                    back: bs.as_ref(),
                    paper: [pb, pg, pr, 255],
                    dark: crate::theme::dark(),
                };
                let (ow, oh) = ((2.0 * l.w).round() as u32, l.h.round() as u32);
                let img = image::RgbaImage::from_raw(ow, oh, curl.render())
                    .map(|buf| Arc::new(RenderImage::new(vec![image::Frame::new(buf)])));
                let has_right = left + 1 < l.n || t.dir > 0 && left + 3 < l.n;
                Some(TurnPaint { spine, y: l.top, w: l.w, h: l.h, base_l, base_r, curl: img, has_right })
            }
            _ => None,
        };
        if let Some(dv) = self.dv() {
            let fresh = turn_paint.as_ref().and_then(|tp| tp.curl.clone());
            if let Some(old) = std::mem::replace(&mut dv.curl_img, fresh) {
                self.retired.borrow_mut().push(old);
            }
        }
        let spread = l.spread;

        let pen = (self.pen.hover, self.pen.eraser);
        let Some(dv) = self.dv() else { return div().into_any_element() };
        let sync = dv.pages.get(&dv.key()).map(|m| m.sync.clone()).unwrap_or(Sync::Loading);
        let loading: Vec<usize> = visible.clone().filter(|i| !dv.pages.get(&dv.seq[*i].ink_key()).is_some_and(|m| m.ready)).collect();

        // live stroke, as a screen-space polygon on its page
        let live_poly: Option<(Vec<(f32, f32)>, u32, f32)> = dv.live.as_ref().and_then(|li| {
            let lv = li.live.as_ref()?;
            let (pts, rgb, a) = if li.tool == Tool::Highlighter {
                (lv.points.iter().map(|p| crate::model::InkPoint { r: HL_R, ..*p }).collect::<Vec<_>>(), 0x9a9a9a, 0.45)
            } else {
                (lv.points.clone(), ink::ink_rgb(0), 1.0)
            };
            let (pxx, py) = (l.page_x(li.page), l.page_y(li.page));
            let poly = ink::outline(&pts, 0.5 / l.s);
            Some((poly.into_iter().map(|(x, y)| (pxx + x * l.s, py + y * l.s)).collect(), rgb, a))
        });

        // pi's typeset text, and patches still drawing themselves in
        let mut texts: Vec<(String, f32, f32, f32, f32)> = Vec::new();
        let mut drawing_in: Vec<Vec<(f32, f32)>> = Vec::new();
        for i in visible.clone() {
            let Some(m) = dv.pages.get(&dv.seq[i].ink_key()) else { continue };
            let (pxx, py) = (l.page_x(i), l.page_y(i));
            for p in &m.base.patches {
                let progress = m.animating.get(&p.id).map(|t| (t.elapsed().as_secs_f32() / DRAW_IN).min(1.0));
                let alpha = progress.map_or(1.0, |pr| ((pr - 0.15) / 0.5).clamp(0.0, 1.0));
                if alpha > 0.0 {
                    for t in &p.texts {
                        texts.push((t.text.clone(), pxx + t.x * l.s, py + t.y * l.s, t.size * l.s, alpha));
                    }
                }
                if let Some(pr) = progress {
                    let total = p.strokes.len().max(1) as f32;
                    for (j, s) in p.strokes.iter().enumerate() {
                        let frac = ((pr - j as f32 / total) * total).clamp(0.0, 1.0);
                        if frac <= 0.0 {
                            continue;
                        }
                        let n = ((s.points.len() as f32 * frac).ceil() as usize).clamp(1, s.points.len());
                        let poly = ink::outline(&s.points[..n], 0.5 / l.s);
                        drawing_in.push(poly.into_iter().map(|(x, y)| (pxx + x * l.s, py + y * l.s)).collect());
                    }
                }
            }
        }
        let eraser_ring = match (pen.0, dv.tool == Tool::Eraser || pen.1) {
            (Some(h), true) => Some((h, ERASER_R * l.s)),
            _ => None,
        };
        let hover_dot = pen.0.filter(|_| eraser_ring.is_none() && dv.tool != Tool::Eraser);
        let paper_rgb = paper_u32(&self.settings.paper);
        let page = canvas(
            |_, _, _| {},
            move |_, _, window, cx| {
                if let Some(tp) = &turn_paint {
                    tp.paint(window, paper_rgb);
                    return; // overlays (live ink, pi text) wait for the leaf to land
                }
                for (i, img) in &images {
                    let (x, y) = (l.page_x(*i), l.page_y(*i));
                    sheet(window, x, y, l.w, l.h, img.as_ref(), paper_rgb);
                }
                if let Some((left, spine)) = spread {
                    gutter(window, spine, l.top, l.w, l.h, left + 1 < l.n);
                }
                for poly in &drawing_in {
                    fill(window, poly, PI_BLUE, 1.0);
                }
                for (text, x, y, sz, a) in &texts {
                    let font = gpui::font(SERIF);
                    let color = ca(PI_BLUE, *a);
                    let run = TextRun { len: text.len(), font: font.clone(), color: color.into(), background_color: None, underline: None, strikethrough: None };
                    let ts = window.text_system();
                    let id = ts.resolve_font(&font);
                    let ascent = f32::from(ts.ascent(id, px(*sz)));
                    let descent = f32::from(ts.descent(id, px(*sz))).abs();
                    let line = ts.shape_line(SharedString::from(text.clone()), px(*sz), &[run], None);
                    let _ = line.paint(point(px(*x), px(*y - ascent)), px(ascent + descent), window, cx);
                }
                if let Some((poly, rgb, a)) = &live_poly {
                    fill(window, poly, *rgb, *a);
                }
                if let Some(((x, y), r)) = eraser_ring {
                    ring(window, x, y, r, 0x555555);
                }
                if let Some((x, y)) = hover_dot {
                    fill(window, &ink::circle(x, y, 1.6, 10), 0x777777, 0.8);
                }
            },
        )
        .absolute()
        .size_full();

        let dv = self.dv().unwrap();
        let indicator = dv.indicator_until.is_some().then(|| match l.spread {
            Some((left, _)) if left + 1 < dv.seq.len() => format!("{}–{} / {}", left + 1, left + 2, dv.seq.len()),
            _ => format!("{} / {}", dv.index + 1, dv.seq.len()),
        });
        let popup = dv.hl_popup.as_ref().map(|(_, _, p)| *p);
        let strip_w = if dv.toolbar_open { STRIP_W } else { 0.0 };
        let busy = dv.pi.busy;
        let title = dv.doc.title().to_owned();
        let counter = format!("{} / {}", dv.index + 1, dv.seq.len());

        let mut root = div()
            .id("doc")
            .size_full()
            .relative()
            .bg(c(DESK))
            .overflow_hidden()
            .when(self.pen.in_range, |d| d.cursor(gpui::CursorStyle::None))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    let at = (f32::from(e.position.x), f32::from(e.position.y));
                    if this.on_chrome(at.0, at.1) {
                        return;
                    }
                    this.mouse_ink = true;
                    this.ink_begin(at, 0.55, false, window, cx);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                let at = (f32::from(e.position.x), f32::from(e.position.y));
                if this.mouse_ink {
                    this.ink_move(at, 0.55, window, cx);
                    cx.notify();
                } else if at.1 <= 3.0 && this.peek_until.is_none() {
                    this.peek_until = Some(Instant::now() + Duration::from_secs(4));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if std::mem::take(&mut this.mouse_ink) {
                        this.ink_end(window, cx);
                        cx.notify();
                    }
                }),
            )
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                // the touchpad's lift-off carries no motion: it starts the
                // momentum (see Papier::on_pad_scroll) and must not stop it
                if matches!(e.touch_phase, gpui::TouchPhase::Ended) {
                    return;
                }
                let vp = this.viewport;
                let at = (f32::from(e.position.x), f32::from(e.position.y));
                let ctrl = e.modifiers.control;
                if let Some(dv) = this.dv() {
                    match e.delta {
                        // touchpads and our finger emulation: exact pixels
                        gpui::ScrollDelta::Pixels(d) if !ctrl => dv.wheel((-f32::from(d.x), -f32::from(d.y)), at, vp),
                        // a mouse wheel's notches glide
                        gpui::ScrollDelta::Lines(d) if !ctrl => dv.glide_by(-d.y * 90.0, vp),
                        _ => {
                            let d = e.delta.pixel_delta(px(40.0));
                            let z = dv.zoom * (1.0 + f32::from(d.y) / 400.0);
                            dv.set_zoom(z, at, vp);
                        }
                    }
                }
                cx.notify();
            }))
            .child(page);

        // book pages the server has no image for at all (no raster, no source PDF)
        let missing: Vec<usize> = {
            let Screen::Doc(dv) = &self.screen else { unreachable!() };
            visible
                .clone()
                .filter(|i| match dv.seq[*i] {
                    SeqEntry::Pdf(p) => matches!(self.prints.get(&self.client.page_url(&dv.doc, p)), Some(crate::app::Load::Failed)),
                    SeqEntry::Note(_) => false,
                })
                .collect()
        };
        for i in missing {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.page_x(i)))
                    .top(px(l.page_y(i) + 24.0))
                    .w(px(l.w))
                    .flex()
                    .justify_center()
                    .text_color(c(GRAY))
                    .text_size(px(14.0))
                    .child("This page isn't on the server"),
            );
        }
        for i in loading {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.page_x(i)))
                    .top(px(l.page_y(i) + l.h.min(vp.1) / 2.0 - 10.0))
                    .w(px(l.w))
                    .flex()
                    .justify_center()
                    .text_color(c(GRAY))
                    .text_size(px(15.0))
                    .child("…"),
            );
        }
        if let Some(text) = indicator {
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(14.0))
                    .right(px(strip_w + 16.0))
                    .px(px(8.0))
                    .py(px(2.0))
                    .bg(ca(DESK, 0.85))
                    .text_size(px(17.0))
                    .text_color(c(0x696969))
                    .child(text),
            );
        }
        if let Some((x, y)) = popup {
            root = root.child(
                div()
                    .id("hl-remove")
                    .absolute()
                    .left(px(x - 21.0))
                    .top(px(y - 21.0))
                    .size(px(42.0))
                    .bg(c(WHITE))
                    .border_2()
                    .border_color(c(BLACK))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.remove_highlight(cx);
                        cx.notify();
                    }))
                    .child(svg().path(icon("close")).size(px(20.0)).text_color(c(BLACK))),
            );
        }
        if busy {
            // pi's working dot, just left of the strip's top
            root = root.child(div().absolute().top(px(14.0)).right(px(strip_w + 14.0)).size(px(9.0)).rounded_full().bg(c(BLACK)));
        }
        root = root.child(self.toolbar(cx));
        if self.peek_until.is_some() {
            root = root.child(self.status_bar(Some((title, counter, sync)), cx));
        }
        root.into_any_element()
    }

    /// The reMarkable right-edge toolbar: a white strip of thin black
    /// glyphs, the armed tool inverted; collapses to its round toggle.
    fn toolbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let vh = self.viewport.1;
        let Some(dv) = self.dv() else { return div().into_any_element() };
        let open = dv.toolbar_open;
        let toggle = div()
            .id("tb-toggle")
            .size(px(40.0))
            .rounded_full()
            .border_2()
            .border_color(c(BLACK))
            .bg(c(WHITE))
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(c(WASH)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_toolbar();
                cx.notify();
            }))
            .child(svg().path(icon(if open { "close" } else { "menu" })).size(px(18.0)).text_color(c(BLACK)));
        if !open {
            return div().absolute().top(px(10.0)).right(px(10.0)).child(toggle).into_any_element();
        }
        let (can_undo, can_redo) = dv.can_undo();
        let tool = dv.tool;
        let width = dv.width;
        let finger = dv.finger_draws;
        let pi_auto = dv.pi.mode == "auto";
        let font = dv.pi.font.clone();
        let first = dv.index == 0;
        let last = dv.index + 1 >= dv.seq.len();
        let zoomed = dv.zoom < 0.99;
        let spread = dv.mode == ViewMode::Spread;

        // 16 buttons + 5 rules must fit the window height
        let btn = ((vh - 20.0 - 40.0 - 5.0 * 11.0) / 16.0).clamp(28.0, TOOL_BTN);
        let b = |id: &'static str, glyph: AnyElement, active: bool, enabled: bool| {
            div()
                .id(id)
                .size(px(btn))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .when(active, |d| d.bg(c(BLACK)).text_color(c(WHITE)))
                .when(!active, |d| d.text_color(c(if enabled { BLACK } else { LIGHT })).hover(|s| s.bg(c(WASH))))
                .child(glyph)
        };
        let ic = |name: &str, active: bool, enabled: bool| -> AnyElement {
            svg().path(icon(name)).size(px(21.0)).text_color(c(if active { WHITE } else if enabled { BLACK } else { LIGHT })).into_any_element()
        };
        let rule = || div().w(px(26.0)).h(px(1.0)).my(px(5.0)).bg(c(LIGHT));
        let width_dots = |active: bool| {
            div().flex().gap(px(2.0)).mt(px(2.0)).children(Width::ALL.iter().map(|w| {
                div().size(px(3.0)).rounded_full().bg(c(if *w == width { if active { WHITE } else { BLACK } } else { GRAY }))
            }))
        };

        let pen = b(
            "tb-pen",
            div().flex().flex_col().items_center().child(ic("pen", tool == Tool::Pen, true)).child(width_dots(tool == Tool::Pen)).into_any_element(),
            tool == Tool::Pen,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.set_tool(Tool::Pen);
            cx.notify();
        }));
        let eraser = b("tb-eraser", ic("eraser", tool == Tool::Eraser, true), tool == Tool::Eraser, true).on_click(cx.listener(|this, _, _, cx| {
            this.set_tool(Tool::Eraser);
            cx.notify();
        }));
        let hl = b("tb-hl", ic("highlighter", tool == Tool::Highlighter, true), tool == Tool::Highlighter, true).on_click(cx.listener(|this, _, _, cx| {
            this.set_tool(Tool::Highlighter);
            cx.notify();
        }));
        let fing = b("tb-finger", ic("finger", finger, true), finger, true).on_click(cx.listener(|this, _, _, cx| {
            if let Some(dv) = this.dv() {
                dv.finger_draws = !dv.finger_draws;
                let on = dv.finger_draws;
                this.toast(if on { "Finger draws — swipe the margin, or use two fingers, to move" } else { "Finger turns pages" });
            }
            cx.notify();
        }));
        let undo = b("tb-undo", ic("undo", false, can_undo), false, can_undo).on_click(cx.listener(|this, _, _, cx| {
            this.undo(false, cx);
            cx.notify();
        }));
        let redo = b("tb-redo", ic("redo", false, can_redo), false, can_redo).on_click(cx.listener(|this, _, _, cx| {
            this.undo(true, cx);
            cx.notify();
        }));
        let pi_mode = b("tb-pi", pi_glyph(pi_auto).into_any_element(), pi_auto, true).on_click(cx.listener(|this, _, _, cx| {
            this.pi_toggle_mode(cx);
            cx.notify();
        }));
        let nudge = b(
            "tb-nudge",
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(svg().path(icon("up")).size(px(15.0)).text_color(c(BLACK)))
                .child(div().font_family(MONO).text_size(px(7.5)).line_height(px(8.0)).child("NUDGE"))
                .into_any_element(),
            false,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.pi_nudge(cx);
            cx.notify();
        }));
        let font_label: String = font.chars().take(2).collect::<String>();
        let font_btn = b(
            "tb-font",
            div().text_size(px(16.0)).font_weight(gpui::FontWeight::SEMIBOLD).child(capitalize(&font_label)).into_any_element(),
            false,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.pi_cycle_font(cx);
            cx.notify();
        }));
        let prev = b("tb-prev", ic("up", false, !first), false, !first).on_click(cx.listener(|this, _, window, cx| {
            this.turn(-1, window, cx);
            cx.notify();
        }));
        let goto = b("tb-goto", ic("hash", false, true), false, true).on_click(cx.listener(|this, _, _, cx| {
            this.dialog = Some(Dialog::GoTo { digits: String::new() });
            cx.notify();
        }));
        let next = b("tb-next", ic("down", false, !last), false, !last).on_click(cx.listener(|this, _, window, cx| {
            this.turn(1, window, cx);
            cx.notify();
        }));
        let add = b("tb-add", ic("add-page", false, true), false, true).on_click(cx.listener(|this, _, window, cx| {
            this.add_page(window, cx);
            cx.notify();
        }));
        let view = b("tb-view", ic(if spread { "scroll" } else { "spread" }, false, true), false, true).on_click(cx.listener(|this, _, _, cx| {
            this.toggle_view();
            cx.notify();
        }));
        let fit = b("tb-fit", ic("fit", zoomed && !spread, !spread), zoomed && !spread, !spread).on_click(cx.listener(|this, _, _, cx| {
            let vp = this.viewport;
            if let Some(dv) = this.dv() {
                dv.toggle_fit(vp);
                let z = dv.zoom;
                this.settings.set("zoom", serde_json::json!(z));
            }
            cx.notify();
        }));
        let home = b("tb-home", ic("home", false, true), false, true).on_click(cx.listener(|this, _, _, cx| this.go_home(cx)));

        div()
            .absolute()
            .top_0()
            .right_0()
            .h_full()
            .w(px(STRIP_W))
            .bg(c(WHITE))
            .border_l_1()
            .border_color(c(LIGHT))
            .flex()
            .flex_col()
            .items_center()
            .pt(px(8.0))
            .pb(px(8.0))
            .child(toggle)
            .child(div().h(px(8.0)))
            .child(pen)
            .child(eraser)
            .child(hl)
            .child(fing)
            .child(rule())
            .child(undo)
            .child(redo)
            .child(rule())
            .child(pi_mode)
            .child(nudge)
            .child(font_btn)
            .child(rule())
            .child(prev)
            .child(goto)
            .child(next)
            .child(add)
            .child(rule())
            .child(view)
            .child(fit)
            .child(div().flex_1())
            .child(home)
            .into_any_element()
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// The tablet's pixel Pi: a blocky P with an i tucked into its lower-right.
fn pi_glyph(active: bool) -> impl IntoElement {
    const ROWS: [[bool; 4]; 5] =
        [[true, true, true, false], [true, false, true, false], [true, true, false, true], [true, false, false, true], [true, false, false, true]];
    let cell = 4.0;
    let color = c(if active { WHITE } else { BLACK });
    div().flex().flex_col().children(ROWS.iter().map(move |row| {
        div().flex().children(row.iter().map(move |on| div().size(px(cell)).when(*on, |d| d.bg(color))))
    }))
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<gpui::Pixels> {
    Bounds { origin: point(px(x), px(y)), size: size(px(w.max(0.0)), px(h)) }
}

/// One sheet of paper: edge, paper, its page image (stretched to `w` when
/// the leaf is foreshortened).
fn sheet(window: &mut Window, x: f32, y: f32, w: f32, h: f32, img: Option<&Arc<RenderImage>>, paper_rgb: u32) {
    if w < 0.5 {
        return;
    }
    window.paint_quad(quad(rect(x - 1.0, y - 1.0, w + 2.0, h + 2.0), px(0.0), c(PAGE_EDGE), px(0.0), c(PAGE_EDGE), Default::default()));
    window.paint_quad(quad(rect(x, y, w, h), px(0.0), c(paper_rgb), px(0.0), c(paper_rgb), Default::default()));
    if let Some(img) = img {
        let _ = window.paint_image(rect(x, y, w, h), Corners::default(), img.clone(), 0, false);
    }
}

/// A horizontal shade: `a0` alpha at the left edge fading to `a1` at the right.
fn shade(window: &mut Window, x: f32, y: f32, w: f32, h: f32, a0: f32, a1: f32) {
    if w < 0.5 || (a0 <= 0.001 && a1 <= 0.001) {
        return;
    }
    let bg = gpui::linear_gradient(
        90.0,
        gpui::linear_color_stop(gpui::Hsla::from(gpui::rgba((a0.clamp(0.0, 1.0) * 255.0) as u32)), 0.0),
        gpui::linear_color_stop(gpui::Hsla::from(gpui::rgba((a1.clamp(0.0, 1.0) * 255.0) as u32)), 1.0),
    );
    window.paint_quad(quad(rect(x, y, w, h), px(0.0), bg, px(0.0), gpui::transparent_black(), Default::default()));
}

/// The book's gutter: paper curving into the spine on both pages.
fn gutter(window: &mut Window, spine: f32, y: f32, w: f32, h: f32, right_page: bool) {
    let g = (w * 0.07).min(36.0);
    let a = if crate::theme::dark() { 0.32 } else { 0.13 };
    shade(window, spine - g, y, g, h, 0.0, a);
    if right_page {
        shade(window, spine, y, g, h, a, 0.0);
    }
}

/// A leaf mid-turn: the pages it uncovers, then the curl (rendered in
/// curl.rs) over the whole spread.
struct TurnPaint {
    spine: f32,
    y: f32,
    w: f32,
    h: f32,
    base_l: Option<Arc<RenderImage>>,
    base_r: Option<Arc<RenderImage>>,
    curl: Option<Arc<RenderImage>>,
    has_right: bool,
}

impl TurnPaint {
    fn paint(&self, window: &mut Window, paper_rgb: u32) {
        let (spine, y, w, h) = (self.spine, self.y, self.w, self.h);
        sheet(window, spine - w, y, w, h, self.base_l.as_ref(), paper_rgb);
        if self.has_right {
            sheet(window, spine, y, w, h, self.base_r.as_ref(), paper_rgb);
        }
        gutter(window, spine, y, w, h, self.has_right);
        if let Some(img) = &self.curl {
            let _ = window.paint_image(rect(spine - w, y, 2.0 * w, h), Corners::default(), img.clone(), 0, false);
        }
    }
}

fn fill(window: &mut Window, poly: &[(f32, f32)], rgb: u32, alpha: f32) {
    if poly.len() < 3 {
        return;
    }
    // non-zero, like the raster: where a stroke crosses itself the
    // overlap stays ink (even-odd would punch a hole in it)
    let mut pb = PathBuilder::fill().with_style(gpui::PathStyle::Fill(gpui::FillOptions::default().with_fill_rule(gpui::FillRule::NonZero)));
    pb.move_to(point(px(poly[0].0), px(poly[0].1)));
    for &(x, y) in &poly[1..] {
        pb.line_to(point(px(x), px(y)));
    }
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, ca(rgb, alpha));
    }
}

fn ring(window: &mut Window, x: f32, y: f32, r: f32, rgb: u32) {
    let pts = ink::circle(x, y, r, 40);
    let mut pb = PathBuilder::stroke(px(1.2));
    pb.move_to(point(px(pts[0].0), px(pts[0].1)));
    for &(px_, py_) in &pts[1..] {
        pb.line_to(point(px(px_), px(py_)));
    }
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, c(rgb));
    }
}

fn bg_async<T: Send + 'static>(cx: &mut gpui::AsyncApp, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
    cx.background_executor().spawn(async move { work() })
}
