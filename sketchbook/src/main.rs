//! sketchbook — a paper sketchbook that writes back, on the reMarkable 2.
//!
//! The whole screen is a page. You write on it with the pen, flip pages
//! with a finger swipe, erase with the marker's rubber end. When you pause,
//! the page is photographed to a background pi agent, which may respond by
//! DRAWING on the page — freeform gray ink (text in a plotter font,
//! sketches, arrows, underlines), animated in stroke by stroke like a ghost
//! hand — or by staying silent. Its drawings are tracked as patches it can
//! later erase or replace via its tools (see ipc.rs / sketchbook-canvas.ts).
//!
//! Built on collab's takeover stack: rm2fb + per-update waveforms (DU for
//! the pen, GL16 for gray AI ink, GC16 flash on page turns), raw Wacom /
//! touch / power-button input. The windowed qtfb backend remains for the
//! no-tablet preview harness.
//!
//! Module map:
//!   fb/draw/display/qtfb/rm2fb   the display stack (from collab)
//!   pen/touch/power              raw input (from collab)
//!   ink.rs      the page model: user strokes + AI patches, vector-first
//!   svg_ink.rs  pi's SVG -> pen strokes (bezier flattening, Hershey text)
//!   hershey.rs  the single-stroke plotter font
//!   ipc.rs      unix-socket server for pi's sketchbook_* tools
//!   pi_rpc.rs   the pi child process (JSONL RPC)
//!   png.rs      grayscale PNG encoder + base64 (page snapshots)

/* The pixel substrate now lives in the shared libreink-core crate
 * (../../libreink); re-exported here so crate::fb etc. keep resolving.
 * APP is this app's identity in those crates: log prefix + env-var prefix. */
pub const APP: libreink_core::app::AppId =
    libreink_core::app::AppId { name: "sketchbook", env_prefix: "SKETCHBOOK" };
pub use libreink_core::{draw, fb, font, png, toolbar};
pub use libreink_display::{display, qtfb, rm2fb};
pub use libreink_input::{palm, pen, power, touch};
pub use libreink_hershey as hershey;
pub use libreink_svg as svg_ink;
pub use libreink_pi::ipc;
pub use libreink_text as text;

mod ink;
mod library;
mod live;
mod md_view;
mod pi_rpc;

use display::{Display, Wave};
use draw::{text_width, BLACK, WHITE};
use fb::{Framebuffer, SCREEN_H as FB_H, SCREEN_W as FB_W};
use ink::{Sketchbook, Page, Pt, Rect, RenderExt, Stroke};
use ipc::IpcServer;
use pen::{Pen, PenPhase};
use pi_rpc::{Pi, PiEvent, SendPage};
use qtfb::{Event, Phase};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/* ---- tuning -------------------------------------------------------------- */

const INK_FLUSH_QTFB: Duration = Duration::from_millis(12);
const INK_FLUSH_TAKEOVER: Duration = Duration::from_millis(8);
const PEN_TIMEOUT: Duration = Duration::from_millis(1500); /* palm rejection */

/* page turns render as partial GC16 (crisp for bold ink, no flash — the
 * stock-app feel, inherited from Paper); a flashing deghost every Nth turn
 * clears accumulated partial-update residue */
const FLIP_DEGHOST_EVERY: u32 = 8;

/// How long a writing pause must last before the page goes to pi.
const IDLE_DELAY: Duration = Duration::from_millis(2800);

/// AI ink animation: one flush per tick, `ANIM_BUDGET` px of path per tick.
const ANIM_TICK: Duration = Duration::from_millis(28);
const ANIM_BUDGET: f32 = 48.0;

/// Page snapshots for pi are half scale (702x936).
const SNAP_DIV: i32 = 2;

/// Raster edit crossfade: old → new over a few 16-level frames instead of
/// blinking into existence.
const FADE_STEPS: u32 = 4;
const FADE_STEP: Duration = Duration::from_millis(320);

/* the right-edge toolbar (stock-reMarkable style): item ids */
const TB_SELECT: u32 = 1;
const TB_GENERATE: u32 = 2;
const TB_QUIET: u32 = 3;
const TB_REFRESH: u32 = 4;
const TB_ERASER: u32 = 5;
const TB_UNDO: u32 = 6;
const TB_REDO: u32 = 7;

/// Undo depth (each entry clones the page's vectors; raster buffers are
/// cloned only when the op touched them).
const UNDO_CAP: usize = 16;

/// What the marker's rubber end does.
#[derive(Clone, Copy, PartialEq)]
enum EraserMode {
    /// Whole strokes vanish at a touch (the quick-sheets default).
    Object,
    /// Only what the rubber actually covers: strokes are SPLIT at the rub
    /// (the vector model keeps the remainders), raster pixels go white.
    Pixel,
    /// Circle a region with the rubber; on lift everything inside goes.
    Region,
}

impl EraserMode {
    fn key(self) -> &'static str {
        match self {
            EraserMode::Object => "object",
            EraserMode::Pixel => "pixel",
            EraserMode::Region => "region",
        }
    }
    fn from_key(s: &str) -> EraserMode {
        match s {
            "pixel" => EraserMode::Pixel,
            "region" => EraserMode::Region,
            _ => EraserMode::Object,
        }
    }
    fn next(self) -> EraserMode {
        match self {
            EraserMode::Object => EraserMode::Pixel,
            EraserMode::Pixel => EraserMode::Region,
            EraserMode::Region => EraserMode::Object,
        }
    }
    fn icon(self) -> toolbar::Icon {
        match self {
            EraserMode::Object => toolbar::Icon::Eraser,
            EraserMode::Pixel => toolbar::Icon::EraserPixel,
            EraserMode::Region => toolbar::Icon::EraserRegion,
        }
    }
    fn label(self) -> &'static str {
        match self {
            EraserMode::Object => "ERASE",
            EraserMode::Pixel => "PIXEL",
            EraserMode::Region => "REGION",
        }
    }
}

/// Lasso selection: majority of a stroke's points must fall inside the
/// loop for the stroke to join the selection.
const SEL_INSIDE: f32 = 0.6;
/// Drag preview repaint throttle.
const DRAG_TICK: Duration = Duration::from_millis(70);

/// One undo step: the page's vector content, plus the rasters when the
/// recorded operation touched them (None = "rasters were not changed by
/// this op", so undoing it must not roll them back).
struct UndoState {
    strokes: Vec<Stroke>,
    patches: Vec<ink::Patch>,
    rasters: Option<Vec<ink::RasterPatch>>,
}

const ERASER_R: f32 = 22.0;

/* pi watchdog: total silence (no stdout, no tool calls) this long while a
 * turn is in flight means the run is wedged — restart pi (--continue keeps
 * the session) and re-arm the pause so the page gets re-sent.
 * SKETCHBOOK_PI_STALL (seconds) overrides, mainly for the preview harness. */
fn pi_stall() -> Duration {
    Duration::from_secs(
        std::env::var("SKETCHBOOK_PI_STALL").ok().and_then(|v| v.parse().ok()).unwrap_or(180),
    )
}
const PI_RESPAWN_DELAY: Duration = Duration::from_secs(5);

/* takeover exit: the X CLOSE button pinned at the sidebar's bottom */
const SB_CLOSE_H: i32 = 64;
const SB_CLOSE_MARGIN: i32 = 24;

/* page-flip gesture: mostly-horizontal finger travel */
const FLIP_DX: i32 = 260;
const FLIP_DY_MAX: i32 = 240;

/* transient page-number indicator after a flip */
const INDICATOR_TTL: Duration = Duration::from_millis(1400);

/* the pi working dot, top-right corner */
const DOT_RECT: Rect = Rect { x0: FB_W - 34, y0: 8, x1: FB_W - 8, y1: 34 };

/* the sidebar (xochitl-style): tap the top-left corner to toggle */
const MENU_HOT: i32 = 90; /* corner tap target, px square */
const SB_W: i32 = 380;
const SB_ROW_H: i32 = 68;
const SB_LIST_Y0: i32 = 130;

#[derive(Clone, Copy, PartialEq)]
enum SbRow {
    First,
    Last,
    Active,   /* where the last writing/drawing happened */
    GoTo,     /* opens the number pad */
    Agent,
    Library,  /* pi's saved material */
    Live,     /* stream strokes to the web viewer */
    Quiet,    /* quiet mode: pauses send NOTHING to pi (write in peace) */
    PiFont,   /* pi's handwriting face: serif / script / sans (cycles) */
    FontSize, /* [-] 100% [+]: zoom on pi's text */
    Refresh,
}
const SB_ROWS: [SbRow; 11] = [
    SbRow::First,
    SbRow::Last,
    SbRow::Active,
    SbRow::GoTo,
    SbRow::Agent,
    SbRow::Library,
    SbRow::Live,
    SbRow::Quiet,
    SbRow::PiFont,
    SbRow::FontSize,
    SbRow::Refresh,
];

/* pi text zoom: multiplies every font-size pi writes (persisted) */
const TEXT_SCALE_MIN: f32 = 0.6;
const TEXT_SCALE_MAX: f32 = 1.8;
const TEXT_SCALE_STEP: f32 = 0.1;

fn settings_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/sketchbook/settings.json")
}

/// (text_scale, quiet, pi_font) from settings.json; all optional in the
/// file. pi_font None = no override (fall back to $SKETCHBOOK_FONT).
fn load_settings() -> (f32, bool, Option<hershey::Face>, EraserMode) {
    let v = std::fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let scale = v
        .as_ref()
        .and_then(|v| v["text_scale"].as_f64())
        .map(|v| (v as f32).clamp(TEXT_SCALE_MIN, TEXT_SCALE_MAX))
        .unwrap_or(1.0);
    let quiet = v.as_ref().and_then(|v| v["quiet"].as_bool()).unwrap_or(false);
    let font = v
        .as_ref()
        .and_then(|v| v["pi_font"].as_str())
        .and_then(hershey::face_from_name);
    let eraser = EraserMode::from_key(
        v.as_ref().and_then(|v| v["eraser"].as_str()).unwrap_or("object"),
    );
    (scale, quiet, font, eraser)
}

fn save_settings(scale: f32, quiet: bool, font: hershey::Face, eraser: EraserMode) {
    let p = settings_path();
    if let Some(dir) = std::path::Path::new(&p).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        &p,
        serde_json::to_vec(&json!({
            "text_scale": scale,
            "quiet": quiet,
            "pi_font": font.key(),
            "eraser": eraser.key(),
        }))
        .unwrap_or_default(),
    );
}

/* the go-to-page number pad, inside the sidebar */
const NP_Y0: i32 = SB_LIST_Y0 + 84; /* grid top (display box sits above) */
const NP_BTN_W: i32 = 104;
const NP_BTN_H: i32 = 84;
const NP_GAP: i32 = 10;
const NP_X0: i32 = 24;

struct Sb {
    numpad: bool,
    entry: String, /* typed digits */
}

/* the AGENT.md page (also reachable by swiping right from page 1) */
const AGENT_TEXT_X: i32 = 70;
const AGENT_TEXT_Y0: i32 = 120;
const AGENT_TEXT_PX: f32 = 36.0;
const AGENT_TEXT_W: i32 = FB_W - 2 * AGENT_TEXT_X;

fn agent_md_path() -> String {
    if let Ok(p) = std::env::var("SKETCHBOOK_AGENT_MD") {
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/sketchbook/AGENT.md")
}

/// The standing-instructions page: the file rendered as text, plus the
/// user's not-yet-applied handwritten annotations (never persisted as ink —
/// pi consumes them into the file, then the page re-renders clean).
struct AgentPage {
    ink: Page,     /* annotation strokes only */
    changed: bool, /* unsent annotations exist */
    waiting: bool, /* pi is applying them; reload on End */
}

/* the library browser: list of items, or one item paginated */
const LIB_LIST_Y0: i32 = 130;
const LIB_ROW_H: i32 = 112;

enum LibView {
    List { items: Vec<library::LibItem> },
    Item { title: String, lines: Vec<md_view::RLine>, pages: Vec<(usize, usize)>, page: usize },
}

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn on_signal(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as *const () as usize; /* no SA_RESTART */
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
}

fn in_rect(x: i32, y: i32, rx: i32, ry: i32, rw: i32, rh: i32) -> bool {
    x >= rx && x < rx + rw && y >= ry && y < ry + rh
}

fn in_rect_r(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1
}

fn offset_rect(r: Rect, dx: i32, dy: i32) -> Rect {
    Rect { x0: r.x0 + dx, y0: r.y0 + dy, x1: r.x1 + dx, y1: r.y1 + dy }
}

fn lasso_bbox(pts: &[(f32, f32)]) -> Option<Rect> {
    if pts.is_empty() {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    Some(Rect { x0: x0 as i32 - 3, y0: y0 as i32 - 3, x1: x1 as i32 + 3, y1: y1 as i32 + 3 })
}

/// Split every stroke the rubber disc touches into the fragments that
/// survive OUTSIDE the disc — the pixel eraser's core. Returns true when
/// anything changed; `dirty` accumulates the affected area.
fn split_strokes(list: &mut Vec<Stroke>, x: f32, y: f32, r: f32, dirty: &mut Option<Rect>) -> bool {
    let inside = |p: &Pt| {
        let (dx, dy) = (p.x - x, p.y - y);
        let rr = r + p.r;
        dx * dx + dy * dy <= rr * rr
    };
    let mut changed = false;
    let mut out: Vec<Stroke> = Vec::with_capacity(list.len());
    for s in list.drain(..) {
        if !s.pts.iter().any(inside) {
            out.push(s);
            continue;
        }
        changed = true;
        if let Some(b) = ink::stroke_bbox(&s) {
            *dirty = Some(dirty.map_or(b, |d| d.union(b)));
        }
        let mut run: Vec<Pt> = Vec::new();
        for p in &s.pts {
            if inside(p) {
                if !run.is_empty() {
                    out.push(Stroke { id: s.id, gray: s.gray, pts: std::mem::take(&mut run) });
                }
            } else {
                run.push(*p);
            }
        }
        if !run.is_empty() {
            out.push(Stroke { id: s.id, gray: s.gray, pts: run });
        }
    }
    *list = out;
    changed
}

/// Ray-cast point-in-polygon (the lasso loop, implicitly closed).
fn point_in_poly(pts: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    let n = pts.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn sock_path() -> String {
    std::env::var("SKETCHBOOK_SOCK").unwrap_or_else(|_| "/tmp/sketchbook-ctl.sock".into())
}

/// Brush radius for a pen frame: a fixed medium nib plus a little from
/// real pressure (0..4095).
fn brush_r(pressure: i32) -> f32 {
    2.0 + pressure as f32 / 4096.0 * 1.6
}

/* ---- AI ink animation ---------------------------------------------------- */

/// One queued stroke of a patch being "hand-drawn" onto the panel. The
/// stroke is ALREADY in the page model (a flip repaints it instantly);
/// this queue only paces its appearance.
struct AnimStroke {
    page: usize,
    patch: u64,
    gray: u8,
    remaining: VecDeque<Pt>,
    last: Option<Pt>,
    bbox: Rect,
}

/// What the lasso caught: indices into the page model (valid until any
/// other mutation — every IPC mutation cancels the selection) plus raster
/// ids, and the union bbox the dashed marquee is drawn around.
#[derive(Clone)]
struct Selection {
    strokes: Vec<usize>,
    patch_strokes: Vec<(usize, usize)>,
    rasters: Vec<u64>,
    bbox: Rect,
}

/// The select tool's state machine (armed by the toolbar's lasso item).
enum Sel {
    Armed,
    Lasso { pts: Vec<(f32, f32)> },
    Have { sel: Selection },
    Drag {
        sel: Selection,
        sx: i32,
        sy: i32,
        lx: i32,
        ly: i32,
        drawn: Option<Rect>,
        /// Save-under: the fb band beneath the preview marquee — erasing
        /// it is a memcpy, not a model re-render (the drag stays snappy).
        saved: Option<(i32, Vec<u16>)>,
        last_tick: Instant,
    },
}

/// A raster edit mid-crossfade: region-sized gray composites of the old
/// and new states, blended step by step with the 16-level waveform.
struct RasterFade {
    page: usize,
    region: Rect,
    from: Vec<u8>,
    to: Vec<u8>,
    step: u32, /* next step to paint, 1..=FADE_STEPS */
    next_at: Instant,
}

/* ---- app ------------------------------------------------------------------ */

struct App {
    fb: Framebuffer,
    disp: Display,
    pi: Option<Pi>,
    nb: Sketchbook,
    ipc: Option<IpcServer>,

    ink_flush: Duration,

    /* pen */
    cur_stroke: Option<Stroke>,
    ink_dirty: Option<Rect>,
    last_ink_flush: Instant,
    palm: palm::PalmGuard,
    flips_since_flash: u32, /* partial-GC16 turns; flash every FLIP_DEGHOST_EVERY */     /* any pen sign of life (incl. hover) */
    last_contact: Option<Instant>, /* actual glass contact */
    contact_changed: bool,         /* this contact wrote or erased something */

    /* pause trigger */
    page_changed: bool,
    idle_at: Option<Instant>,

    /* pi */
    streaming: bool,
    reply_buf: String,
    sock: String,
    /* liveness: watchdog while a turn is in flight, respawn after death.
     * a wedged run (hung API call, stuck tool) used to leave the app in
     * "thinking" forever — every new pause queued as followUp behind it */
    pi_alive_at: Option<Instant>,
    pi_respawn_at: Option<Instant>,
    pi_stall: Duration,

    /* raster edit crossfade */
    fade: Option<RasterFade>,

    /* the right-edge toolbar + the lasso select tool it arms */
    tb: toolbar::EdgeToolbar,
    sel: Option<Sel>,
    pen_chrome: bool, /* this pen contact began on the toolbar: swallow it */

    /* the rubber's mode + the region-eraser's in-flight loop */
    eraser: EraserMode,
    rub_loop: Option<Vec<(f32, f32)>>,

    /* undo/redo: current-page states; a pending checkpoint is committed
     * only when the rubber contact actually changes something */
    undo: Vec<UndoState>,
    redo: Vec<UndoState>,
    pending_undo: Option<UndoState>,

    /* AI ink animation */
    anim: VecDeque<AnimStroke>,
    anim_dirty: Option<Rect>,
    anim_settle: Option<Rect>, /* union animated; GL16-refined when done */
    last_anim: Instant,

    /* touch gestures */
    touch_start: Option<(i32, i32)>,
    touch_last: (i32, i32),

    /* quiet mode: pauses send nothing to pi (sidebar toggle, persisted) */
    quiet: bool,
    /* pi's default handwriting face (sidebar toggle, persisted) */
    pi_font: hershey::Face,

    /* transient chrome */
    indicator_until: Option<Instant>,
    working: bool,

    /* the AGENT.md page, when open (replaces the sketchbook view) */
    agent_page: Option<AgentPage>,

    /* the sidebar, when showing */
    sidebar: Option<Sb>,

    /* the page that saw the most recent ink activity (user or pi) */
    last_activity_page: usize,

    /* zoom applied to every font-size pi writes (sidebar [-]/[+]) */
    text_scale: f32,

    /* the library browser, when open (read-only view) */
    lib_view: Option<LibView>,

    /* pending deghost flash after rubber erasing (DU-erase leaves ghosts) */
    deghost_at: Option<Instant>,

    /* LIVE web stream (sidebar toggle; off at launch) */
    live: live::Live,
}

impl App {
    /* -- small chrome (drawn over the page, re-rendered away later) -- */

    fn indicator_rect(&self) -> Rect {
        Rect { x0: FB_W / 2 - 120, y0: FB_H - 56, x1: FB_W / 2 + 120, y1: FB_H - 10 }
    }

    fn show_page_indicator(&mut self) {
        let label = format!("{} / {}", self.nb.current + 1, self.nb.count);
        let r = self.indicator_rect();
        self.fb.fill_rect(r.x0, r.y0, r.w(), r.h(), WHITE);
        self.fb.rect_outline(r.x0, r.y0, r.w(), r.h(), 2, BLACK);
        self.fb.text(
            FB_W / 2 - text_width(&label, 3) / 2,
            r.y0 + (r.h() - 21) / 2,
            &label,
            3,
            BLACK,
        );
        self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
        self.indicator_until = Some(Instant::now() + INDICATOR_TTL);
    }

    fn clear_page_indicator(&mut self) {
        self.indicator_until = None;
        let r = self.indicator_rect();
        let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
    }

    fn draw_working_dot(&mut self) {
        let r = DOT_RECT;
        let (cx, cy) = ((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2);
        self.fb.disc(cx, cy, 8, draw::GRAY);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
    }

    fn clear_working_dot(&mut self) {
        let r = DOT_RECT;
        if self.agent_page.is_some() || self.lib_view.is_some() {
            /* the dot sits in blank margin on these text views */
            self.fb.fill_rect(r.x0, r.y0, r.w(), r.h(), WHITE);
            self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
            return;
        }
        let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
    }

    fn set_working(&mut self, on: bool) {
        if self.working != on {
            self.working = on;
            if on {
                self.draw_working_dot();
            } else {
                self.clear_working_dot();
            }
        }
    }

    /* -- the sidebar -- */

    fn show_sidebar(&mut self) {
        self.drop_selection(); /* menus repaint the whole screen */
        self.sidebar = Some(Sb { numpad: false, entry: String::new() });
        self.cur_stroke = None;
        self.paint_sidebar();
    }

    fn paint_sidebar(&mut self) {
        let Some(sb) = &self.sidebar else { return };
        let (numpad, entry) = (sb.numpad, sb.entry.clone());
        self.fb.fill_rect(0, 0, SB_W, FB_H, WHITE);
        self.fb.fill_rect(SB_W - 3, 0, 3, FB_H, BLACK);
        self.fb.text(28, 24, "SKETCHBOOK", 4, BLACK);
        let sub = format!("page {} of {}", self.nb.current + 1, self.nb.count);
        self.fb.text(28, 62, &sub, 2, draw::GRAY);
        self.fb.fill_rect(0, 92, SB_W - 3, 2, BLACK);

        if numpad {
            /* entry display */
            self.fb.rect_outline(NP_X0, SB_LIST_Y0, SB_W - 2 * NP_X0, 64, 3, BLACK);
            let shown = format!("GO TO: {entry}_");
            self.fb.text(NP_X0 + 14, SB_LIST_Y0 + 22, &shown, 3, BLACK);
            /* keys */
            let keys = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "<", "0", "OK"];
            for (i, k) in keys.iter().enumerate() {
                let (col, row) = (i as i32 % 3, i as i32 / 3);
                let (x, y) = (NP_X0 + col * (NP_BTN_W + NP_GAP), NP_Y0 + row * (NP_BTN_H + NP_GAP));
                self.fb.rect_outline(x, y, NP_BTN_W, NP_BTN_H, 2, BLACK);
                self.fb.text(
                    x + (NP_BTN_W - text_width(k, 4)) / 2,
                    y + (NP_BTN_H - 28) / 2,
                    k,
                    4,
                    BLACK,
                );
            }
            let cy = NP_Y0 + 4 * (NP_BTN_H + NP_GAP);
            self.fb.rect_outline(NP_X0, cy, SB_W - 2 * NP_X0, 64, 2, BLACK);
            self.fb.text(
                (SB_W - text_width("CANCEL", 3)) / 2,
                cy + 22,
                "CANCEL",
                3,
                BLACK,
            );
        } else {
            for (i, row) in SB_ROWS.iter().enumerate() {
                let y = SB_LIST_Y0 + i as i32 * SB_ROW_H;
                let on_page = |p: usize| self.nb.current == p && self.agent_page.is_none();
                let (label, current) = match row {
                    SbRow::First => ("FIRST PAGE".to_string(), on_page(0)),
                    SbRow::Last => (
                        format!("LAST PAGE ({})", self.nb.count),
                        on_page(self.nb.count - 1),
                    ),
                    SbRow::Active => (
                        format!("ACTIVE PAGE ({})", self.last_activity_page + 1),
                        false,
                    ),
                    SbRow::GoTo => ("GO TO PAGE...".to_string(), false),
                    SbRow::Agent => ("INSTRUCTIONS".to_string(), self.agent_page.is_some()),
                    SbRow::Library => (
                        format!("LIBRARY ({})", library::scan().len()),
                        self.lib_view.is_some(),
                    ),
                    SbRow::Live => (
                        format!("LIVE STREAM: {}", if self.live.enabled { "ON" } else { "OFF" }),
                        self.live.enabled,
                    ),
                    SbRow::Quiet => (
                        format!("PI: {}", if self.quiet { "QUIET" } else { "AUTO" }),
                        self.quiet,
                    ),
                    SbRow::PiFont => (
                        format!(
                            "PI FONT: {}",
                            self.pi_font.key().to_uppercase()
                        ),
                        false,
                    ),
                    SbRow::FontSize => (
                        format!("-   PI TEXT {:3}%   +", (self.text_scale * 100.0).round() as i32),
                        false,
                    ),
                    SbRow::Refresh => ("REFRESH SCREEN".to_string(), false),
                };
                if matches!(row, SbRow::Agent) {
                    self.fb.fill_rect(24, y - 8, SB_W - 48, 2, draw::LIGHT);
                }
                if current {
                    self.fb.fill_rect(12, y, SB_W - 27, SB_ROW_H - 8, BLACK);
                    self.fb.text(36, y + (SB_ROW_H - 8 - 21) / 2, &label, 3, WHITE);
                } else {
                    self.fb.text(36, y + (SB_ROW_H - 8 - 21) / 2, &label, 3, BLACK);
                }
            }
            /* X CLOSE, pinned at the bottom: the takeover exit */
            let cy = FB_H - SB_CLOSE_H - SB_CLOSE_MARGIN;
            self.fb.fill_rect(SB_CLOSE_MARGIN, cy, SB_W - 2 * SB_CLOSE_MARGIN, SB_CLOSE_H, BLACK);
            let label = "X CLOSE";
            self.fb.text(
                (SB_W - text_width(label, 3)) / 2,
                cy + (SB_CLOSE_H - 21) / 2,
                label,
                3,
                WHITE,
            );
        }
        self.disp.update(0, 0, SB_W, FB_H, Wave::Ink);
    }

    /// Hide the panel and repaint what it covered.
    fn hide_sidebar(&mut self) {
        self.sidebar = None;
        if self.lib_view.is_some() {
            self.render_library(false);
            return;
        }
        if self.agent_page.is_some() {
            self.render_agent_page(false);
            return;
        }
        let r = Rect { x0: 0, y0: 0, x1: SB_W - 1, y1: FB_H - 1 };
        let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
        self.draw_menu_icon();
        self.restore_chrome_over(r);
    }

    /// Leave any menu view and land on sketchbook page `p` (0-based).
    fn jump_to_page(&mut self, p: usize) {
        self.sidebar = None;
        self.lib_view = None;
        if self.agent_page.is_some() {
            self.agent_page = None;
        }
        let p = p.min(self.nb.count - 1);
        let delta = p as i32 - self.nb.current as i32;
        if delta != 0 {
            self.flip(delta);
        } else {
            self.nb.page.render_full(&mut self.fb, &self.nb.rasters);
            self.disp.full_refresh();
            self.draw_menu_icon();
            self.draw_toolbar();
            self.show_page_indicator();
        }
    }

    /// A press while the sidebar is showing. Always consumes the press.
    fn sidebar_press(&mut self, x: i32, y: i32) {
        if x >= SB_W {
            self.hide_sidebar();
            return;
        }
        let numpad = self.sidebar.as_ref().is_some_and(|s| s.numpad);
        if numpad {
            self.numpad_press(x, y);
            return;
        }
        /* the X CLOSE bar at the bottom */
        let cy = FB_H - SB_CLOSE_H - SB_CLOSE_MARGIN;
        if in_rect(x, y, SB_CLOSE_MARGIN, cy, SB_W - 2 * SB_CLOSE_MARGIN, SB_CLOSE_H) {
            println!("sketchbook: close (sidebar)");
            RUNNING.store(false, Ordering::Relaxed);
            return;
        }
        let idx = (y - SB_LIST_Y0) / SB_ROW_H;
        let Some(row) = (idx >= 0).then(|| SB_ROWS.get(idx as usize)).flatten().copied() else {
            return; /* header / dead space: keep the panel up */
        };
        match row {
            SbRow::First => self.jump_to_page(0),
            SbRow::Last => self.jump_to_page(self.nb.count - 1),
            SbRow::Active => self.jump_to_page(self.last_activity_page),
            SbRow::GoTo => {
                if let Some(sb) = self.sidebar.as_mut() {
                    sb.numpad = true;
                    sb.entry.clear();
                }
                self.paint_sidebar();
            }
            SbRow::Agent => {
                self.sidebar = None;
                self.lib_view = None;
                if self.agent_page.is_none() {
                    self.open_agent_page();
                } else {
                    self.render_agent_page(false);
                }
            }
            SbRow::Library => {
                self.sidebar = None;
                self.open_library();
            }
            SbRow::Live => {
                self.live.toggle(self.nb.current);
                self.paint_sidebar(); /* stays open: shows the new state */
            }
            SbRow::PiFont => {
                /* sketchbook's toggle order (serif first); parse() takes the
                 * face directly now, so no global override to update */
                const FACE_ORDER: [hershey::Face; 3] =
                    [hershey::Face::Serif, hershey::Face::Script, hershey::Face::Sans];
                let i = FACE_ORDER.iter().position(|f| *f == self.pi_font).unwrap_or(0);
                self.pi_font = FACE_ORDER[(i + 1) % FACE_ORDER.len()];
                save_settings(self.text_scale, self.quiet, self.pi_font, self.eraser);
                println!("sketchbook: pi font -> {}", self.pi_font.key());
                self.paint_sidebar(); /* stays open for repeated taps */
            }
            SbRow::Quiet => {
                self.quiet = !self.quiet;
                save_settings(self.text_scale, self.quiet, self.pi_font, self.eraser);
                println!("sketchbook: pi {}", if self.quiet { "quiet" } else { "auto" });
                if !self.quiet && self.page_changed {
                    /* back to AUTO: offer the accumulated page soon */
                    self.idle_at = Some(Instant::now());
                }
                self.paint_sidebar(); /* stays open: shows the new state */
            }
            SbRow::FontSize => {
                /* left third = smaller, right third = bigger, middle = 100% */
                let new = if x < SB_W / 3 {
                    self.text_scale - TEXT_SCALE_STEP
                } else if x > 2 * SB_W / 3 {
                    self.text_scale + TEXT_SCALE_STEP
                } else {
                    1.0
                };
                let new = (new * 10.0).round() / 10.0;
                self.text_scale = new.clamp(TEXT_SCALE_MIN, TEXT_SCALE_MAX);
                save_settings(self.text_scale, self.quiet, self.pi_font, self.eraser);
                self.paint_sidebar(); /* stays open for repeated taps */
            }
            SbRow::Refresh => {
                self.hide_sidebar();
                self.disp.full_refresh();
            }
        }
    }

    fn numpad_press(&mut self, x: i32, y: i32) {
        /* CANCEL bar */
        let cy = NP_Y0 + 4 * (NP_BTN_H + NP_GAP);
        if in_rect(x, y, NP_X0, cy, SB_W - 2 * NP_X0, 64) {
            if let Some(sb) = self.sidebar.as_mut() {
                sb.numpad = false;
                sb.entry.clear();
            }
            self.paint_sidebar();
            return;
        }
        /* key grid */
        let (col, row) = ((x - NP_X0) / (NP_BTN_W + NP_GAP), (y - NP_Y0) / (NP_BTN_H + NP_GAP));
        if !(0..3).contains(&col) || !(0..4).contains(&row) {
            return;
        }
        if !in_rect(
            x,
            y,
            NP_X0 + col * (NP_BTN_W + NP_GAP),
            NP_Y0 + row * (NP_BTN_H + NP_GAP),
            NP_BTN_W,
            NP_BTN_H,
        ) {
            return; /* in a gap */
        }
        let key = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "<", "0", "OK"]
            [(row * 3 + col) as usize];
        match key {
            "<" => {
                if let Some(sb) = self.sidebar.as_mut() {
                    sb.entry.pop();
                }
                self.paint_sidebar();
            }
            "OK" => {
                let target = self
                    .sidebar
                    .as_ref()
                    .and_then(|s| s.entry.parse::<usize>().ok());
                match target {
                    Some(n) if n >= 1 => self.jump_to_page(n - 1),
                    _ => {} /* empty/garbage: stay on the pad */
                }
            }
            d => {
                if let Some(sb) = self.sidebar.as_mut() {
                    if sb.entry.len() < 4 {
                        sb.entry.push_str(d);
                    }
                }
                self.paint_sidebar();
            }
        }
    }

    /// The small hamburger mark in the corner (sketchbook pages only — the
    /// AGENT.md page's header owns that corner, though the tap still works).
    fn draw_menu_icon(&mut self) {
        if self.agent_page.is_some() || self.sidebar.is_some() || self.lib_view.is_some() {
            return;
        }
        for i in 0..3 {
            self.fb.fill_rect(18, 22 + i * 10, 30, 4, draw::GRAY);
        }
        self.disp.update(14, 14, 44, 44, Wave::Ink);
    }

    /* -- the library browser -- */

    fn open_library(&mut self) {
        self.drop_selection(); /* menus repaint the whole screen */
        self.agent_page = None;
        self.anim.clear();
        self.anim_settle = None;
        self.anim_dirty = None;
        self.cur_stroke = None;
        self.idle_at = None;
        self.indicator_until = None;
        self.lib_view = Some(LibView::List { items: library::scan() });
        self.render_library(true);
        println!("sketchbook: library opened");
    }

    fn close_library(&mut self) {
        self.lib_view = None;
        self.nb.page.render_full(&mut self.fb, &self.nb.rasters);
        self.disp.full_refresh();
        self.draw_menu_icon();
        self.draw_toolbar();
        if self.streaming {
            self.working = false;
            self.set_working(true);
        }
        if self.page_changed {
            self.idle_at = Some(Instant::now() + IDLE_DELAY);
        }
        self.show_page_indicator();
    }

    fn render_library(&mut self, flash: bool) {
        self.fb.fill_rect(0, 0, FB_W, FB_H, WHITE);
        match &self.lib_view {
            Some(LibView::List { items }) => {
                let head = format!("LIBRARY  ({} items)", items.len());
                self.fb.text(24, 18, &head, 3, BLACK);
                self.fb.text(
                    24,
                    52,
                    "pi's saved material - tap to read - swipe left to return",
                    2,
                    draw::GRAY,
                );
                self.fb.fill_rect(0, 84, FB_W, 2, BLACK);
                let rows: Vec<(String, String)> = items
                    .iter()
                    .map(|it| {
                        (it.title.clone(), format!("{}  -  {} KB  -  {}", it.date, it.kb, it.file))
                    })
                    .collect();
                if rows.is_empty() {
                    text::draw_line(
                        &mut self.fb,
                        AGENT_TEXT_X,
                        LIB_LIST_Y0 + 40,
                        text::Face::Body,
                        34.0,
                        "Empty. pi saves distilled web finds and notes here - ask it to keep something.",
                    );
                }
                for (i, (title, meta)) in rows.iter().enumerate() {
                    let y = LIB_LIST_Y0 + i as i32 * LIB_ROW_H;
                    if y + LIB_ROW_H > FB_H - 30 {
                        self.fb.text(AGENT_TEXT_X, y + 8, "[... more items]", 2, draw::GRAY);
                        break;
                    }
                    let mut t = title.clone();
                    while text::width(text::Face::Heading, 38.0, &t) > FB_W - 2 * AGENT_TEXT_X
                        && t.chars().count() > 4
                    {
                        t = t.chars().take(t.chars().count() - 4).collect();
                        t.push('.');
                        t.push('.');
                    }
                    text::draw_line(&mut self.fb, AGENT_TEXT_X, y, text::Face::Heading, 38.0, &t);
                    self.fb.text(AGENT_TEXT_X, y + 58, meta, 2, draw::GRAY);
                    self.fb.fill_rect(
                        AGENT_TEXT_X,
                        y + LIB_ROW_H - 14,
                        FB_W - 2 * AGENT_TEXT_X,
                        1,
                        draw::LIGHT,
                    );
                }
            }
            Some(LibView::Item { title, lines, pages, page }) => {
                let mut t = title.clone();
                while text::width(text::Face::Heading, 36.0, &t) > FB_W - 300 && t.chars().count() > 4 {
                    t = t.chars().take(t.chars().count() - 4).collect();
                    t.push('.');
                    t.push('.');
                }
                text::draw_line(&mut self.fb, 24, 10, text::Face::Heading, 36.0, &t);
                let pager =
                    format!("{} / {}  -  swipe left/right - right at start = back", page + 1, pages.len());
                self.fb.text(24, 60, &pager, 2, draw::GRAY);
                self.fb.fill_rect(0, 88, FB_W, 2, BLACK);
                let (s0, e0) = pages[*page];
                let mut y = 120;
                for l in &lines[s0..e0] {
                    if l.hr {
                        self.fb.fill_rect(AGENT_TEXT_X, y + l.h / 2, FB_W - 2 * AGENT_TEXT_X, 2, draw::LIGHT);
                        y += l.h;
                        continue;
                    }
                    if l.aside {
                        self.fb.fill_rect(AGENT_TEXT_X - 30, y, FB_W - 2 * (AGENT_TEXT_X - 30), l.h, draw::CODE_BG);
                        self.fb.fill_rect(AGENT_TEXT_X - 30, y, 3, l.h, draw::GRAY);
                    }
                    if l.code {
                        self.fb.fill_rect(AGENT_TEXT_X - 12, y, FB_W - 2 * (AGENT_TEXT_X - 12), l.h, draw::CODE_BG);
                    }
                    let mut cx = AGENT_TEXT_X + l.x;
                    if l.center {
                        let w: i32 = l.spans.iter().map(|sp| text::width(sp.face, sp.px, &sp.text)).sum();
                        cx = (FB_W - w) / 2;
                    }
                    for sp in &l.spans {
                        let adv = text::draw_line(&mut self.fb, cx, y + sp.dy, sp.face, sp.px, &sp.text);
                        if sp.underline {
                            self.fb.fill_rect(cx, y + (sp.px * 1.18) as i32, adv, 2, draw::GRAY);
                        }
                        cx += adv;
                    }
                    y += l.h;
                }
            }
            None => {}
        }
        if self.working {
            let (cx, cy) = ((DOT_RECT.x0 + DOT_RECT.x1) / 2, (DOT_RECT.y0 + DOT_RECT.y1) / 2);
            self.fb.disc(cx, cy, 8, draw::GRAY);
        }
        if flash {
            self.disp.full_refresh();
        } else {
            /* full GL16: antialiased reading text stays smooth (a partial
             * pass speckles the greys) and there is no flash */
            self.disp.update(0, 0, FB_W, FB_H, Wave::Print);
        }
    }

    /// A tap inside the library list (pen press or a finger tap).
    fn library_press(&mut self, _x: i32, y: i32) {
        let Some(LibView::List { items }) = &self.lib_view else { return };
        let idx = (y - LIB_LIST_Y0) / LIB_ROW_H;
        if idx < 0 || idx as usize >= items.len() {
            return;
        }
        let it = &items[idx as usize];
        let Some(content) = library::read(&it.file) else { return };
        let (fm_title, lines) = md_view::layout(&content, FB_W - 2 * AGENT_TEXT_X);
        let title = fm_title.unwrap_or_else(|| it.title.clone());
        let pages = md_view::paginate(&lines, FB_H - 120 - 40);
        self.lib_view = Some(LibView::Item { title, lines, pages, page: 0 });
        self.render_library(false);
    }

    /// Swipe navigation inside the library (delta +1 = swipe left).
    fn lib_flip(&mut self, delta: i32) {
        match self.lib_view.as_mut() {
            Some(LibView::List { .. }) => {
                if delta > 0 {
                    self.close_library();
                }
            }
            Some(LibView::Item { page, pages, .. }) => {
                if delta > 0 && *page + 1 < pages.len() {
                    *page += 1;
                    self.render_library(false);
                } else if delta < 0 {
                    if *page > 0 {
                        *page -= 1;
                        self.render_library(false);
                    } else {
                        self.lib_view = Some(LibView::List { items: library::scan() });
                        self.render_library(false);
                    }
                }
            }
            None => {}
        }
    }

    /* -- the AGENT.md page -- */

    fn open_agent_page(&mut self) {
        self.drop_selection(); /* menus repaint the whole screen */
        self.lib_view = None;
        self.anim.clear();
        self.anim_settle = None; /* model strokes reappear via render_full later */
        self.anim_dirty = None;
        self.cur_stroke = None;
        self.idle_at = None;
        self.indicator_until = None; /* the full repaint below wipes it */
        self.agent_page = Some(AgentPage { ink: Page::default(), changed: false, waiting: false });
        self.render_agent_page(true);
        println!("sketchbook: AGENT.md page opened");
    }

    fn close_agent_page(&mut self) {
        self.agent_page = None;
        self.cur_stroke = None;
        self.nb.page.render_full(&mut self.fb, &self.nb.rasters);
        self.disp.full_refresh();
        self.draw_menu_icon();
        self.draw_toolbar();
        if self.streaming {
            self.working = false;
            self.set_working(true);
        }
        /* a pending sketchbook change resumes its pause countdown */
        if self.page_changed {
            self.idle_at = Some(Instant::now() + IDLE_DELAY);
        }
        self.show_page_indicator();
        println!("sketchbook: AGENT.md page closed");
    }

    /// Paint the standing-instructions page: header, the file as text, and
    /// any pending annotation ink. `flash` runs the GC16 page-turn refresh.
    fn render_agent_page(&mut self, flash: bool) {
        self.fb.fill_rect(0, 0, FB_W, FB_H, WHITE);
        self.fb.text(24, 18, "YOUR STANDING INSTRUCTIONS  (AGENT.MD)", 3, BLACK);
        self.fb.text(
            24,
            52,
            "write feedback on this page, pause to apply - swipe left to return",
            2,
            draw::GRAY,
        );
        self.fb.fill_rect(0, 84, FB_W, 2, BLACK);

        let content = std::fs::read_to_string(agent_md_path()).unwrap_or_default();
        let lh = text::line_h(text::Face::Body, AGENT_TEXT_PX);
        let mut y = AGENT_TEXT_Y0;
        'outer: for raw in content.lines() {
            let line = if raw.trim().is_empty() { " " } else { raw };
            for wrapped in text::wrap(text::Face::Body, AGENT_TEXT_PX, AGENT_TEXT_W, line) {
                if y + lh > FB_H - 40 {
                    self.fb.text(AGENT_TEXT_X, y + 8, "[... file continues]", 2, draw::GRAY);
                    break 'outer;
                }
                text::draw_line(&mut self.fb, AGENT_TEXT_X, y, text::Face::Body, AGENT_TEXT_PX, wrapped.trim_end());
                y += lh;
            }
        }
        if let Some(ap) = &self.agent_page {
            for s in &ap.ink.strokes {
                if s.pts.len() == 1 {
                    ink::stamp_segment(&mut self.fb, s.pts[0], s.pts[0], s.gray);
                }
                for w in s.pts.windows(2) {
                    ink::stamp_segment(&mut self.fb, w[0], w[1], s.gray);
                }
            }
        }
        if self.working {
            let (cx, cy) = ((DOT_RECT.x0 + DOT_RECT.x1) / 2, (DOT_RECT.y0 + DOT_RECT.y1) / 2);
            self.fb.disc(cx, cy, 8, draw::GRAY);
        }
        if flash {
            self.disp.full_refresh();
        } else {
            self.disp.update(0, 0, FB_W, FB_H, Wave::Print);
        }
    }

    /// The user paused after annotating the instructions page: ship the
    /// annotated view to pi with orders to rewrite the file to match.
    fn send_agent_feedback(&mut self) {
        let ready = self
            .agent_page
            .as_ref()
            .is_some_and(|ap| ap.changed && !ap.ink.strokes.is_empty());
        if !ready || self.pi.is_none() {
            return;
        }
        let path = agent_md_path();
        let mut content = std::fs::read_to_string(&path).unwrap_or_default();
        if content.len() > 6000 {
            content.truncate(6000);
        }

        /* composite snapshot: annotation ink at half scale, then the file
         * text drawn into the same buffer at half geometry */
        let (w, h, mut gray) = self.agent_page.as_ref().unwrap().ink.snapshot(SNAP_DIV);
        let lh = text::line_h(text::Face::Body, AGENT_TEXT_PX);
        let mut y = AGENT_TEXT_Y0;
        'outer: for raw in content.lines() {
            let line = if raw.trim().is_empty() { " " } else { raw };
            for wrapped in text::wrap(text::Face::Body, AGENT_TEXT_PX, AGENT_TEXT_W, line) {
                if y + lh > FB_H - 40 {
                    break 'outer;
                }
                text::draw_gray(
                    &mut gray,
                    w,
                    h,
                    AGENT_TEXT_X / SNAP_DIV,
                    y / SNAP_DIV,
                    text::Face::Body,
                    AGENT_TEXT_PX / SNAP_DIV as f32,
                    wrapped.trim_end(),
                );
                y += lh;
            }
        }

        let msg = format!(
            "The attached image is your standing-instructions page (the file \
             {path}) as the user sees it, WITH their fresh handwritten \
             annotations. Current file contents:\n```\n{content}\n```\n\
             Interpret the annotations and rewrite {path} with your shell \
             tools to match the user's intent: crossed-out lines are removed, \
             added notes are incorporated (rewrite cleanly, don't transcribe \
             scribbles verbatim), keep it under ~40 lines of markdown. When \
             the file is updated, reply with just `done`. Do NOT call \
             sketchbook_draw for this."
        );
        let streaming = self.streaming;
        let Some(pi) = self.pi.as_mut() else { return };
        match pi.send_image_message(&gray, w as u32, h as u32, &msg, streaming) {
            Ok(()) => {
                if let Some(ap) = self.agent_page.as_mut() {
                    ap.changed = false;
                    ap.waiting = true;
                }
                self.streaming = true;
                self.set_working(true);
                self.pi_alive_at = Some(Instant::now());
                println!("sketchbook: AGENT.md annotations sent to pi");
            }
            Err(e) => println!("sketchbook: agent feedback send failed: {e}"),
        }
    }

    /// Repaint chrome that a region re-render may have wiped.
    fn restore_chrome_over(&mut self, r: Rect) {
        if self.working {
            let d = DOT_RECT;
            if r.x1 >= d.x0 && r.x0 <= d.x1 && r.y1 >= d.y0 && r.y0 <= d.y1 {
                self.draw_working_dot();
            }
        }
        if r.x0 < MENU_HOT && r.y0 < MENU_HOT {
            self.draw_menu_icon();
        }
        let t = self.tb.rect();
        if r.x1 >= t.x0 && r.x0 <= t.x1 && r.y1 >= t.y0 && r.y0 <= t.y1 {
            self.draw_toolbar();
        }
        /* the selection marquee is fb-only chrome too */
        if let Some(Sel::Have { sel }) = &self.sel {
            let b = sel.bbox.pad(8);
            if r.x1 >= b.x0 && r.x0 <= b.x1 && r.y1 >= b.y0 && r.y0 <= b.y1 {
                let b = sel.bbox;
                self.draw_marquee(b);
            }
        }
    }

    /* -- the right-edge toolbar (stock-reMarkable style) -- */

    fn draw_toolbar(&mut self) {
        if self.sidebar.is_some() || self.agent_page.is_some() || self.lib_view.is_some() {
            return;
        }
        self.tb.draw(&mut self.fb);
        let r = self.tb.rect();
        self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
    }

    /// A toolbar press (pen or finger). Returns true when consumed.
    fn toolbar_press(&mut self, x: i32, y: i32) -> bool {
        if self.sidebar.is_some() || self.agent_page.is_some() || self.lib_view.is_some() {
            return false;
        }
        let Some(hit) = self.tb.hit(x, y) else { return false };
        match hit {
            toolbar::Hit::Toggle => {
                let was = self.tb.rect();
                self.tb.open = !self.tb.open;
                if !self.tb.open {
                    /* fold: restore the page under the strip */
                    let had_gray = self.nb.page.render_region(&mut self.fb, was, &self.nb.rasters);
                    self.disp.update(was.x0, was.y0, was.w(), was.h(), if had_gray { Wave::Text } else { Wave::Ink });
                    self.restore_chrome_over(was);
                }
                self.draw_toolbar();
            }
            toolbar::Hit::Item(TB_SELECT) => {
                if self.sel.is_some() {
                    self.cancel_selection();
                } else {
                    self.sel = Some(Sel::Armed);
                    self.set_tb_active(TB_SELECT, true);
                }
            }
            toolbar::Hit::Item(TB_GENERATE) => {
                /* offer the page to pi right now (quiet mode included) */
                if !self.nb.page.is_empty() || !self.nb.rasters.is_empty() {
                    self.page_changed = true;
                    let quiet = self.quiet;
                    self.quiet = false;
                    self.idle_at = Some(Instant::now());
                    self.maybe_send_page();
                    self.quiet = quiet;
                }
            }
            toolbar::Hit::Item(TB_QUIET) => {
                self.quiet = !self.quiet;
                save_settings(self.text_scale, self.quiet, self.pi_font, self.eraser);
                let label = if self.quiet { "OFF" } else { "AUTO" };
                for it in &mut self.tb.items {
                    if it.id == TB_QUIET {
                        it.label = label;
                    }
                }
                self.set_tb_active(TB_QUIET, !self.quiet);
            }
            toolbar::Hit::Item(TB_ERASER) => {
                self.eraser = self.eraser.next();
                save_settings(self.text_scale, self.quiet, self.pi_font, self.eraser);
                let (icon, label, active) =
                    (self.eraser.icon(), self.eraser.label(), self.eraser != EraserMode::Object);
                for it in &mut self.tb.items {
                    if it.id == TB_ERASER {
                        it.icon = icon;
                        it.label = label;
                        it.active = active;
                    }
                }
                self.draw_toolbar();
            }
            toolbar::Hit::Item(TB_UNDO) => self.do_undo(),
            toolbar::Hit::Item(TB_REDO) => self.do_redo(),
            toolbar::Hit::Item(TB_REFRESH) => {
                self.disp.full_refresh();
            }
            toolbar::Hit::Item(_) => {}
        }
        true
    }

    fn set_tb_active(&mut self, id: u32, active: bool) {
        for it in &mut self.tb.items {
            if it.id == id {
                it.active = active;
            }
        }
        self.draw_toolbar();
    }

    /* -- undo / redo -- */

    fn snapshot_state(&self, with_rasters: bool) -> UndoState {
        UndoState {
            strokes: self.nb.page.strokes.clone(),
            patches: self.nb.page.patches.clone(),
            rasters: with_rasters.then(|| self.nb.rasters.clone()),
        }
    }

    /// Record an undo step NOW (the mutation follows immediately).
    fn checkpoint(&mut self, with_rasters: bool) {
        let st = self.snapshot_state(with_rasters);
        self.undo.push(st);
        if self.undo.len() > UNDO_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.pending_undo = None;
    }

    /// Stash a checkpoint that only becomes an undo step if the contact
    /// actually changes something (a dry rubber pass costs nothing).
    fn checkpoint_pending(&mut self, with_rasters: bool) {
        self.pending_undo = Some(self.snapshot_state(with_rasters));
    }

    fn commit_pending_undo(&mut self) {
        if let Some(st) = self.pending_undo.take() {
            self.push_undo(st);
        }
    }

    fn push_undo(&mut self, st: UndoState) {
        self.undo.push(st);
        if self.undo.len() > UNDO_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn do_undo(&mut self) {
        let Some(prev) = self.undo.pop() else { return };
        let cur = self.snapshot_state(prev.rasters.is_some());
        self.redo.push(cur);
        self.apply_state(prev);
        println!("sketchbook: undo ({} left)", self.undo.len());
    }

    fn do_redo(&mut self) {
        let Some(next) = self.redo.pop() else { return };
        let cur = self.snapshot_state(next.rasters.is_some());
        self.undo.push(cur);
        self.apply_state(next);
        println!("sketchbook: redo ({} left)", self.redo.len());
    }

    fn apply_state(&mut self, st: UndoState) {
        self.drop_selection();
        self.rub_loop = None;
        self.pending_undo = None;
        self.anim.clear();
        self.anim_dirty = None;
        self.anim_settle = None;
        self.fade = None;
        self.nb.page.strokes = st.strokes;
        self.nb.page.patches = st.patches;
        let max_patch = self.nb.page.patches.iter().map(|p| p.id + 1).max().unwrap_or(0);
        self.nb.page.next_patch = self.nb.page.next_patch.max(max_patch);
        if let Some(r) = st.rasters {
            let max_raster = r.iter().map(|x| x.id + 1).max().unwrap_or(1);
            self.nb.next_raster = self.nb.next_raster.max(max_raster);
            self.nb.rasters = r;
            self.nb.rasters_dirty = true;
        }
        self.nb.page.dirty = true;
        self.nb.save_current();
        self.page_changed = true;
        if self.sidebar.is_none() && self.agent_page.is_none() && self.lib_view.is_none() {
            self.nb.page.render_full(&mut self.fb, &self.nb.rasters);
            self.disp.update(0, 0, FB_W, FB_H, Wave::Page);
            self.draw_menu_icon();
            self.draw_toolbar();
        }
    }

    /* -- lasso select: circle objects, drag them around -- */

    /// Drop the selection without repainting (a full repaint follows).
    fn drop_selection(&mut self) {
        if self.sel.take().is_some() {
            for it in &mut self.tb.items {
                if it.id == TB_SELECT {
                    it.active = false;
                }
            }
        }
    }

    fn cancel_selection(&mut self) {
        if self.sel.is_none() {
            return;
        }
        let repaint = match self.sel.take() {
            Some(Sel::Lasso { pts }) => lasso_bbox(&pts),
            Some(Sel::Have { sel }) => Some(sel.bbox.pad(8)),
            Some(Sel::Drag { sel, drawn, .. }) => {
                let mut r = sel.bbox.pad(8);
                if let Some(d) = drawn {
                    r = r.union(d.pad(8));
                }
                Some(r)
            }
            _ => None,
        };
        if let Some(r) = repaint {
            let r = r.clamp_screen();
            let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
            self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
            self.restore_chrome_over(r);
        }
        self.set_tb_active(TB_SELECT, false);
    }

    /// Pen events while the select tool is armed. Returns true if consumed.
    fn select_pen(&mut self, phase: PenPhase, x: i32, y: i32) -> bool {
        let Some(state) = self.sel.take() else { return false };
        match (state, phase) {
            /* start: a lasso, or a drag when pressing inside the marquee */
            (Sel::Have { sel }, PenPhase::Press) => {
                if in_rect_r(sel.bbox.pad(14), x, y) {
                    self.sel = Some(Sel::Drag {
                        sel,
                        sx: x,
                        sy: y,
                        lx: x,
                        ly: y,
                        drawn: None,
                        saved: None,
                        last_tick: Instant::now(),
                    });
                } else {
                    /* press outside: drop the marquee, start a fresh lasso */
                    let b = sel.bbox.pad(8).clamp_screen();
                    let had_gray = self.nb.page.render_region(&mut self.fb, b, &self.nb.rasters);
                    self.disp.update(b.x0, b.y0, b.w(), b.h(), if had_gray { Wave::Text } else { Wave::Ink });
                    self.restore_chrome_over(b);
                    self.sel = Some(Sel::Lasso { pts: vec![(x as f32, y as f32)] });
                }
            }
            (Sel::Armed, PenPhase::Press) => {
                self.sel = Some(Sel::Lasso { pts: vec![(x as f32, y as f32)] });
            }
            (Sel::Lasso { mut pts }, PenPhase::Move | PenPhase::Press) => {
                let (px, py) = *pts.last().unwrap();
                let (fx, fy) = (x as f32, y as f32);
                if (fx - px).hypot(fy - py) >= 3.0 {
                    let r = self.fb.stroke_segment(px as i32, py as i32, x, y, 0, draw::BLACK);
                    self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
                    pts.push((fx, fy));
                }
                self.sel = Some(Sel::Lasso { pts });
            }
            (Sel::Lasso { pts }, PenPhase::Release) => {
                self.finish_lasso(pts);
            }
            (Sel::Drag { sel, sx, sy, lx: _, ly: _, drawn, mut saved, mut last_tick }, PenPhase::Move) => {
                let mut new_drawn = drawn;
                if last_tick.elapsed() >= DRAG_TICK {
                    last_tick = Instant::now();
                    let target = offset_rect(sel.bbox, x - sx, y - sy).clamp_screen();
                    if drawn != Some(target) {
                        /* erase the old preview by pasting the saved band
                         * back — a memcpy, no model re-render, no lag */
                        if let (Some(d), Some((y0, band))) = (drawn, saved.take()) {
                            self.fb.paste_band(y0, &band);
                            let d = d.pad(8).clamp_screen();
                            self.disp.update(d.x0, d.y0, d.w(), d.h(), Wave::Ink);
                        }
                        let keep = target.pad(9).clamp_screen();
                        saved = Some((keep.y0, self.fb.copy_band(keep.y0, keep.y1 + 1)));
                        self.draw_marquee(target);
                        new_drawn = Some(target);
                    }
                }
                self.sel = Some(Sel::Drag { sel, sx, sy, lx: x, ly: y, drawn: new_drawn, saved, last_tick });
            }
            (Sel::Drag { sel, sx, sy, lx, ly, drawn, saved, .. }, PenPhase::Release) => {
                let _ = (lx, ly);
                /* restore under the last preview before the real move */
                if let (Some(d), Some((y0, band))) = (drawn, saved) {
                    self.fb.paste_band(y0, &band);
                    let d = d.pad(8).clamp_screen();
                    self.disp.update(d.x0, d.y0, d.w(), d.h(), Wave::Ink);
                }
                self.apply_drag(sel, x - sx, y - sy);
            }
            (s, _) => self.sel = Some(s),
        }
        true
    }

    fn finish_lasso(&mut self, pts: Vec<(f32, f32)>) {
        let trail = lasso_bbox(&pts);
        /* wipe the lasso trail (it is fb-only chrome) */
        if let Some(r) = trail {
            let r = r.clamp_screen();
            let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
            self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
            self.restore_chrome_over(r);
        }
        if pts.len() < 8 {
            self.sel = Some(Sel::Armed);
            return;
        }
        let inside = |px: f32, py: f32| point_in_poly(&pts, px, py);
        let stroke_caught = |s: &Stroke| {
            if s.pts.is_empty() {
                return false;
            }
            let step = (s.pts.len() / 24).max(1);
            let sampled: Vec<&Pt> = s.pts.iter().step_by(step).collect();
            let hit = sampled.iter().filter(|p| inside(p.x, p.y)).count();
            hit as f32 / sampled.len() as f32 >= SEL_INSIDE
        };
        let mut sel = Selection { strokes: Vec::new(), patch_strokes: Vec::new(), rasters: Vec::new(), bbox: Rect { x0: 0, y0: 0, x1: -1, y1: -1 } };
        let mut bbox: Option<Rect> = None;
        let add_bbox = |b: Option<Rect>, acc: &mut Option<Rect>| {
            if let Some(b) = b {
                *acc = Some(acc.map_or(b, |a| a.union(b)));
            }
        };
        for (i, s) in self.nb.page.strokes.iter().enumerate() {
            if stroke_caught(s) {
                sel.strokes.push(i);
                add_bbox(ink::stroke_bbox(s), &mut bbox);
            }
        }
        for (pi_idx, p) in self.nb.page.patches.iter().enumerate() {
            for (si, s) in p.strokes.iter().enumerate() {
                if stroke_caught(s) {
                    sel.patch_strokes.push((pi_idx, si));
                    add_bbox(ink::stroke_bbox(s), &mut bbox);
                }
            }
        }
        for rl in &self.nb.rasters {
            let (cx, cy) = (rl.x0 as f32 + rl.w as f32 / 2.0, rl.y0 as f32 + rl.h as f32 / 2.0);
            if inside(cx, cy) {
                sel.rasters.push(rl.id);
                add_bbox(Some(rl.rect()), &mut bbox);
            }
        }
        match bbox {
            Some(b) if !sel.strokes.is_empty() || !sel.patch_strokes.is_empty() || !sel.rasters.is_empty() => {
                sel.bbox = b;
                self.draw_marquee(b);
                self.sel = Some(Sel::Have { sel });
            }
            _ => {
                self.sel = Some(Sel::Armed);
            }
        }
    }

    fn apply_drag(&mut self, sel: Selection, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            self.draw_marquee(sel.bbox);
            self.sel = Some(Sel::Have { sel });
            return;
        }
        self.checkpoint(!sel.rasters.is_empty());
        let (fdx, fdy) = (dx as f32, dy as f32);
        for &i in &sel.strokes {
            if let Some(s) = self.nb.page.strokes.get_mut(i) {
                for p in &mut s.pts {
                    p.x += fdx;
                    p.y += fdy;
                }
            }
        }
        for &(pi_idx, si) in &sel.patch_strokes {
            if let Some(s) = self.nb.page.patches.get_mut(pi_idx).and_then(|p| p.strokes.get_mut(si)) {
                for p in &mut s.pts {
                    p.x += fdx;
                    p.y += fdy;
                }
            }
        }
        for id in &sel.rasters {
            if let Some(rl) = self.nb.rasters.iter_mut().find(|r| r.id == *id) {
                rl.x0 += dx;
                rl.y0 += dy;
            }
        }
        self.nb.page.dirty = true;
        if !sel.rasters.is_empty() {
            self.nb.rasters_dirty = true;
        }
        self.nb.save_current();
        self.page_changed = true;
        self.last_activity_page = self.nb.current;

        let new_bbox = offset_rect(sel.bbox, dx, dy);
        let r = sel.bbox.union(new_bbox).pad(8).clamp_screen();
        let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
        self.restore_chrome_over(r);

        let moved = Selection { bbox: new_bbox.clamp_screen(), ..sel };
        self.draw_marquee(moved.bbox);
        self.sel = Some(Sel::Have { sel: moved });
        println!("sketchbook: selection moved by ({dx},{dy})");
    }

    /// The dashed selection rectangle (fb-only chrome).
    fn draw_marquee(&mut self, b: Rect) {
        let b = b.pad(6).clamp_screen();
        let dash = 14;
        let gap = 10;
        let mut x = b.x0;
        while x < b.x1 {
            let e = (x + dash).min(b.x1);
            self.fb.stroke_segment(x, b.y0, e, b.y0, 0, draw::BLACK);
            self.fb.stroke_segment(x, b.y1, e, b.y1, 0, draw::BLACK);
            x += dash + gap;
        }
        let mut y = b.y0;
        while y < b.y1 {
            let e = (y + dash).min(b.y1);
            self.fb.stroke_segment(b.x0, y, b.x0, e, 0, draw::BLACK);
            self.fb.stroke_segment(b.x1, y, b.x1, e, 0, draw::BLACK);
            y += dash + gap;
        }
        let r = b.pad(2);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
    }



    /* -- page turning -- */

    fn flip(&mut self, delta: i32) {
        /* the library owns swipes while open (item pages / back) */
        if self.lib_view.is_some() {
            self.lib_flip(delta);
            return;
        }
        /* on the AGENT.md page: forward returns to the sketchbook, further
         * back does nothing */
        if self.agent_page.is_some() {
            if delta > 0 {
                self.close_agent_page();
            }
            return;
        }
        /* swiping back past page 1 opens the standing-instructions page */
        if delta < 0 && self.nb.current == 0 {
            self.open_agent_page();
            return;
        }
        /* pending animation strokes are already in the model; the full
         * repaint below shows them instantly on whatever page has them */
        self.anim.clear();
        self.anim_settle = None;
        self.anim_dirty = None;
        self.drop_selection(); /* selection indices are per-page */
        self.undo.clear(); /* undo history is per page-state */
        self.redo.clear();
        self.pending_undo = None;
        if !self.nb.flip(delta) {
            self.show_page_indicator(); /* at the edge: just show where we are */
            return;
        }
        self.cur_stroke = None;
        self.page_changed = false;
        self.idle_at = None;
        self.nb.page.render_full(&mut self.fb, &self.nb.rasters);
        self.flips_since_flash += 1;
        if self.flips_since_flash >= FLIP_DEGHOST_EVERY {
            self.flips_since_flash = 0;
            self.disp.full_refresh(); /* periodic deghost */
        } else {
            self.disp.update(0, 0, FB_W, FB_H, Wave::Page); /* flash-free turn */
        }
        self.working = false; /* dot was flashed away; redraw if still busy */
        if self.streaming {
            self.set_working(true);
        }
        self.draw_menu_icon();
        self.draw_toolbar();
        self.show_page_indicator();
        self.live.page(self.nb.current);
        println!("sketchbook: page {} / {}", self.nb.current + 1, self.nb.count);
    }

    /* -- pen -- */

    fn pen_point(&mut self, phase: PenPhase, x: i32, y: i32, pressure: i32, rubber: bool) {
        self.palm.arm();
        /* the sidebar swallows the pen entirely: a press picks a row (or
         * dismisses), moves/releases never ink */
        if self.sidebar.is_some() {
            if phase == PenPhase::Press {
                self.sidebar_press(x, y);
            }
            return;
        }
        if phase == PenPhase::Press && x < MENU_HOT && y < MENU_HOT {
            self.show_sidebar();
            return;
        }
        /* the library is read-only: a pen press picks a list row, nothing
         * else inks */
        if self.lib_view.is_some() {
            if phase == PenPhase::Press {
                self.library_press(x, y);
            }
            return;
        }
        /* a contact that began on the toolbar never inks */
        if self.pen_chrome {
            if phase == PenPhase::Release {
                self.pen_chrome = false;
            }
            return;
        }
        if phase == PenPhase::Press && self.toolbar_press(x, y) {
            self.pen_chrome = true;
            return;
        }
        /* the select tool owns the pen while armed (the rubber still
         * erases: flipping the marker cancels the selection first) */
        if self.sel.is_some() && self.agent_page.is_none() {
            if rubber {
                self.cancel_selection();
            } else {
                self.last_contact = Some(Instant::now());
                if self.select_pen(phase, x, y) {
                    return;
                }
            }
        }
        match phase {
            PenPhase::Press | PenPhase::Move => {
                if phase == PenPhase::Press {
                    self.last_contact = Some(Instant::now());
                    self.contact_changed = false;
                    self.idle_at = None; /* writing again: hold the trigger */
                }
                self.last_contact = Some(Instant::now());
                if rubber {
                    /* commit any open stroke before switching to the rubber,
                     * so what's on the glass is what's in the model */
                    self.commit_open_stroke();
                    if phase == PenPhase::Press {
                        /* becomes an undo step only if the rub changes something */
                        self.checkpoint_pending(true);
                    }
                    if self.agent_page.is_none() {
                        match self.eraser {
                            /* raster wipe ONLY on the initial press on a
                             * clean spot: scrubbing strokes off a raster
                             * must never cascade into wiping it */
                            EraserMode::Object => {
                                self.erase_pass(x as f32, y as f32, phase == PenPhase::Press)
                            }
                            EraserMode::Pixel => self.pixel_erase_pass(x as f32, y as f32),
                            EraserMode::Region => self.region_erase_pen(phase, x, y),
                        }
                    } /* no eraser on the AGENT.md page — annotate instead */
                } else {
                    self.ink_pass(phase, x, y, pressure);
                }
            }
            PenPhase::Release => {
                self.last_contact = Some(Instant::now());
                if self.rub_loop.is_some() {
                    self.finish_rub_loop(); /* region eraser: the lift deletes */
                }
                self.pending_undo = None; /* dry rubber contact: no undo step */
                self.commit_open_stroke();
                if self.contact_changed {
                    if self.agent_page.is_none() {
                        self.page_changed = true;
                    }
                    self.idle_at = Some(Instant::now() + IDLE_DELAY);
                }
            }
        }
    }

    /// Land the in-progress stroke in whichever model owns the screen:
    /// the sketchbook page, or the AGENT.md page's annotation layer.
    fn commit_open_stroke(&mut self) {
        let Some(s) = self.cur_stroke.take() else { return };
        if s.pts.is_empty() {
            return;
        }
        match self.agent_page.as_mut() {
            Some(ap) => {
                ap.ink.strokes.push(s);
                ap.changed = true;
                self.contact_changed = true;
            }
            None => {
                self.checkpoint(false); /* one undo step per stroke */
                self.nb.page.strokes.push(s);
                self.nb.page.dirty = true;
                self.contact_changed = true;
                self.last_activity_page = self.nb.current;
            }
        }
    }

    fn ink_pass(&mut self, phase: PenPhase, x: i32, y: i32, pressure: i32) {
        let p = Pt { x: x as f32, y: y as f32, r: brush_r(pressure) };
        let prev = match (&mut self.cur_stroke, phase) {
            (Some(s), PenPhase::Move) => {
                let prev = *s.pts.last().unwrap();
                s.pts.push(p);
                prev
            }
            _ => {
                /* Press, or Move with no open stroke (e.g. after an erase) */
                self.cur_stroke = Some(Stroke { id: 0, pts: vec![p], gray: ink::USER_GRAY });
                self.live.pen_break();
                p
            }
        };
        ink::stamp_segment(&mut self.fb, prev, p, ink::USER_GRAY);
        self.mark_ink_dirty(prev, p);
        if self.agent_page.is_none() {
            self.live.pen(p.x, p.y, p.r);
        }
    }

    fn mark_ink_dirty(&mut self, a: Pt, b: Pt) {
        let r = Rect {
            x0: (a.x.min(b.x) - a.r.max(b.r)) as i32,
            y0: (a.y.min(b.y) - a.r.max(b.r)) as i32,
            x1: (a.x.max(b.x) + a.r.max(b.r)).ceil() as i32,
            y1: (a.y.max(b.y) + a.r.max(b.r)).ceil() as i32,
        };
        self.ink_dirty = Some(match self.ink_dirty {
            None => r,
            Some(d) => d.union(r),
        });
    }

    /// Rubber over a raster (and not over ink): wipe that raster patch.
    fn wipe_raster_at(&mut self, x: i32, y: i32) -> bool {
        let Some(pos) = self.nb.rasters.iter().position(|r| r.contains(x, y)) else {
            return false;
        };
        let rl = self.nb.rasters.remove(pos);
        self.commit_pending_undo();
        let r = rl.rect().pad(2).clamp_screen();
        self.nb.rasters_dirty = true;
        self.nb.save_current();
        self.contact_changed = false; /* wiping a render is not new sketch ink */
        let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
        self.deghost_at = Some(Instant::now() + Duration::from_millis(900));
        self.restore_chrome_over(r);
        println!("sketchbook: raster #{} wiped on page {}", rl.id, self.nb.current + 1);
        true
    }

    /// PIXEL eraser: strokes are SPLIT where the rubber covers them (the
    /// vector model keeps the rest intact) and raster pixels go white.
    fn pixel_erase_pass(&mut self, x: f32, y: f32) {
        let r = ERASER_R;
        let mut dirty: Option<Rect> = None;
        let mut changed = split_strokes(&mut self.nb.page.strokes, x, y, r, &mut dirty);
        for p in &mut self.nb.page.patches {
            changed |= split_strokes(&mut p.strokes, x, y, r, &mut dirty);
        }
        self.nb.page.patches.retain(|p| !p.strokes.is_empty() || !p.texts.is_empty());

        /* raster pixels under the disc go white */
        let disc = Rect {
            x0: (x - r) as i32,
            y0: (y - r) as i32,
            x1: (x + r).ceil() as i32,
            y1: (y + r).ceil() as i32,
        };
        for rl in &mut self.nb.rasters {
            let rr = rl.rect();
            if disc.x1 < rr.x0 || disc.x0 > rr.x1 || disc.y1 < rr.y0 || disc.y0 > rr.y1 {
                continue;
            }
            let mut any = false;
            for py in disc.y0.max(rr.y0)..=disc.y1.min(rr.y1) {
                for px in disc.x0.max(rr.x0)..=disc.x1.min(rr.x1) {
                    let (dx, dy) = (px as f32 - x, py as f32 - y);
                    if dx * dx + dy * dy <= r * r {
                        let i = ((py - rl.y0) * rl.w + (px - rl.x0)) as usize;
                        if rl.gray[i] != 255 {
                            rl.gray[i] = 255;
                            any = true;
                        }
                    }
                }
            }
            if any {
                changed = true;
                self.nb.rasters_dirty = true;
                dirty = Some(dirty.map_or(disc, |d| d.union(disc)));
            }
        }

        if !changed {
            return;
        }
        self.commit_pending_undo();
        self.nb.page.dirty = true;
        self.contact_changed = true;
        self.last_activity_page = self.nb.current;
        self.live.rub(x, y);
        self.deghost_at = Some(Instant::now() + Duration::from_millis(1100));
        let d = dirty.unwrap_or(disc).pad(4).clamp_screen();
        let had_gray = self.nb.page.render_region(&mut self.fb, d, &self.nb.rasters);
        self.disp.update(d.x0, d.y0, d.w(), d.h(), if had_gray { Wave::Text } else { Wave::Ink });
        self.restore_chrome_over(d);
    }

    /// REGION eraser: the rubber draws a loop; collect it (thin trail).
    fn region_erase_pen(&mut self, phase: PenPhase, x: i32, y: i32) {
        match phase {
            PenPhase::Press => {
                self.rub_loop = Some(vec![(x as f32, y as f32)]);
            }
            PenPhase::Move => {
                let Some(pts) = self.rub_loop.as_mut() else {
                    self.rub_loop = Some(vec![(x as f32, y as f32)]);
                    return;
                };
                let (px, py) = *pts.last().unwrap();
                let (fx, fy) = (x as f32, y as f32);
                if (fx - px).hypot(fy - py) >= 3.0 {
                    pts.push((fx, fy));
                    let r = self.fb.stroke_segment(px as i32, py as i32, x, y, 0, draw::BLACK);
                    self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
                }
            }
            PenPhase::Release => self.finish_rub_loop(),
        }
    }

    /// The region-eraser loop closed: delete everything inside it.
    fn finish_rub_loop(&mut self) {
        let Some(pts) = self.rub_loop.take() else { return };
        let trail = lasso_bbox(&pts);
        let mut dirty = trail;
        let mut changed = false;
        if pts.len() >= 8 {
            let caught = |s: &Stroke| {
                if s.pts.is_empty() {
                    return false;
                }
                let step = (s.pts.len() / 24).max(1);
                let sampled: Vec<&Pt> = s.pts.iter().step_by(step).collect();
                let hit = sampled.iter().filter(|p| point_in_poly(&pts, p.x, p.y)).count();
                hit as f32 / sampled.len() as f32 >= SEL_INSIDE
            };
            let add = |b: Option<Rect>, acc: &mut Option<Rect>| {
                if let Some(b) = b {
                    *acc = Some(acc.map_or(b, |a| a.union(b)));
                }
            };
            let before = self.nb.page.strokes.len();
            let mut kept = Vec::with_capacity(before);
            for s in self.nb.page.strokes.drain(..) {
                if caught(&s) {
                    add(ink::stroke_bbox(&s), &mut dirty);
                } else {
                    kept.push(s);
                }
            }
            changed |= kept.len() != before;
            self.nb.page.strokes = kept;
            for p in &mut self.nb.page.patches {
                let n = p.strokes.len();
                let mut kept = Vec::with_capacity(n);
                for s in p.strokes.drain(..) {
                    if caught(&s) {
                        add(ink::stroke_bbox(&s), &mut dirty);
                    } else {
                        kept.push(s);
                    }
                }
                changed |= kept.len() != n;
                p.strokes = kept;
            }
            self.nb.page.patches.retain(|p| !p.strokes.is_empty() || !p.texts.is_empty());
            let nr = self.nb.rasters.len();
            let mut kept_r = Vec::with_capacity(nr);
            for rl in self.nb.rasters.drain(..) {
                let (cx, cy) = (rl.x0 as f32 + rl.w as f32 / 2.0, rl.y0 as f32 + rl.h as f32 / 2.0);
                if point_in_poly(&pts, cx, cy) {
                    add(Some(rl.rect()), &mut dirty);
                } else {
                    kept_r.push(rl);
                }
            }
            if kept_r.len() != nr {
                changed = true;
                self.nb.rasters_dirty = true;
            }
            self.nb.rasters = kept_r;
        }
        if changed {
            self.commit_pending_undo();
            self.nb.page.dirty = true;
            self.contact_changed = true;
            self.last_activity_page = self.nb.current;
            self.nb.save_current();
            self.deghost_at = Some(Instant::now() + Duration::from_millis(1100));
            println!("sketchbook: region erase on page {}", self.nb.current + 1);
        }
        if let Some(d) = dirty {
            let d = d.pad(4).clamp_screen();
            let had_gray = self.nb.page.render_region(&mut self.fb, d, &self.nb.rasters);
            self.disp.update(d.x0, d.y0, d.w(), d.h(), if had_gray { Wave::Text } else { Wave::Ink });
            self.restore_chrome_over(d);
        }
    }

    fn erase_pass(&mut self, x: f32, y: f32, allow_raster_wipe: bool) {
        if let Some((gone, _)) = self.nb.page.erase_at(x, y, ERASER_R) {
            self.commit_pending_undo();
            self.contact_changed = true;
            self.last_activity_page = self.nb.current;
            self.live.rub(x, y);
            /* DU-erased black ink ghosts badly; flash once the scrubbing
             * settles */
            self.deghost_at = Some(Instant::now() + Duration::from_millis(1100));
            /* un-animated strokes in the region must appear now that we
             * repaint from the model; drop their pacing entries */
            let mut region = gone;
            self.anim.retain(|a| {
                let hit = a.page == self.nb.current
                    && a.bbox.x1 >= gone.x0
                    && a.bbox.x0 <= gone.x1
                    && a.bbox.y1 >= gone.y0
                    && a.bbox.y0 <= gone.y1;
                if hit {
                    region = region.union(a.bbox);
                }
                !hit
            });
            let r = region.pad(4).clamp_screen();
            let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
            self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
            self.restore_chrome_over(r);
        } else if allow_raster_wipe {
            /* nothing under the rubber at first contact: maybe the user is
             * wiping one of pi's raster outputs */
            self.wipe_raster_at(x as i32, y as i32);
        }
    }

    /* -- touch: page flips, CLOSE -- */

    fn touch(&mut self, phase: Phase, x: i32, y: i32) {
        if self.palm.within(PEN_TIMEOUT) {
            return; /* palm rejection */
        }
        if self.sidebar.is_some() {
            if phase == Phase::Press {
                self.sidebar_press(x, y);
            }
            return; /* no drags/flips under the panel */
        }
        match phase {
            Phase::Press => {
                if x < MENU_HOT && y < MENU_HOT {
                    self.show_sidebar();
                    return;
                }
                if self.toolbar_press(x, y) {
                    return;
                }
                self.touch_start = Some((x, y));
                self.touch_last = (x, y);
            }
            Phase::Move => {
                if self.touch_start.is_some() {
                    self.touch_last = (x, y);
                }
            }
            Phase::Release => {
                if let Some((sx, sy)) = self.touch_start.take() {
                    let (dx, dy) = (self.touch_last.0 - sx, self.touch_last.1 - sy);
                    if dx.abs() >= FLIP_DX && dy.abs() <= FLIP_DY_MAX {
                        /* swipe left = next page (turning forward) */
                        self.flip(if dx < 0 { 1 } else { -1 });
                    } else if self.lib_view.is_some() && dx.abs() < 40 && dy.abs() < 40 {
                        /* a finger tap picks a library row */
                        self.library_press(sx, sy);
                    }
                }
            }
        }
    }

    /* -- the pause trigger -- */

    fn maybe_send_page(&mut self) {
        let Some(at) = self.idle_at else { return };
        if Instant::now() < at || self.cur_stroke.is_some() {
            return;
        }
        /* still touching the glass? push the deadline out a beat */
        if self.last_contact.is_some_and(|t| t.elapsed() < IDLE_DELAY) {
            self.idle_at = Some(Instant::now() + Duration::from_millis(300));
            return;
        }
        self.idle_at = None;
        if self.agent_page.is_some() {
            self.send_agent_feedback();
            return;
        }
        if self.quiet {
            return; /* quiet mode: write in peace — nothing is sent, nothing costs */
        }
        if !self.page_changed || self.nb.page.is_empty() {
            return;
        }
        let Some(pi) = self.pi.as_mut() else { return };
        self.nb.save_current();
        let (w, h, gray) = ink::snapshot_with_rasters(&self.nb.page, &self.nb.rasters, SNAP_DIV);
        let patches = format!(
            "{}; your raster outputs: {}",
            patch_summary(&self.nb.page),
            raster_summary(&self.nb.rasters),
        );
        let layout = layout_hints(&self.nb.page, self.text_scale);
        let streaming = self.streaming;
        match pi.send_page(
            &gray,
            w as u32,
            h as u32,
            self.nb.current + 1,
            self.nb.count,
            &patches,
            &layout,
            streaming,
        ) {
            Ok(()) => {
                self.page_changed = false;
                self.streaming = true;
                self.set_working(true);
                self.live.status("think");
                self.pi_alive_at = Some(Instant::now());
                println!("sketchbook: page {} sent to pi", self.nb.current + 1);
            }
            Err(e) => println!("sketchbook: send failed: {e}"),
        }
    }

    /* -- pi events -- */

    fn handle_pi(&mut self, ev: PiEvent) {
        match ev {
            PiEvent::Start => {
                self.streaming = true;
                self.reply_buf.clear();
                self.set_working(true);
            }
            PiEvent::Delta(d) => self.reply_buf.push_str(&d),
            PiEvent::Notice(n) => println!("sketchbook: pi {n}"),
            PiEvent::End => {
                self.streaming = false;
                self.set_working(false);
                self.live.status("idle");
                self.pi_alive_at = None;
                let t: String = self.reply_buf.trim().chars().take(300).collect();
                if !t.is_empty() {
                    println!("sketchbook: pi said: {t}");
                }
                self.reply_buf.clear();
                /* annotations applied: show the rewritten file, clean */
                let reload = self.agent_page.as_mut().is_some_and(|ap| {
                    let w = ap.waiting;
                    if w {
                        ap.waiting = false;
                        ap.ink = Page::default();
                    }
                    w
                });
                if reload && self.sidebar.is_none() {
                    self.render_agent_page(false);
                }
            }
            PiEvent::Died(reason) => {
                self.streaming = false;
                self.pi = None;
                self.set_working(false);
                self.live.status("idle");
                self.pi_alive_at = None;
                self.pi_respawn_at = Some(Instant::now() + PI_RESPAWN_DELAY);
                println!("sketchbook: pi exited: {reason}; respawning in {}s", PI_RESPAWN_DELAY.as_secs());
            }
        }
    }

    /* -- the tool socket -- */

    fn handle_ipc_request(&mut self, req: &Value) -> Value {
        if self.streaming {
            self.pi_alive_at = Some(Instant::now()); /* tool call = alive */
        }
        /* mutations invalidate the lasso selection's model indices */
        if matches!(
            req["cmd"].as_str().unwrap_or(""),
            "draw" | "erase" | "place" | "raster_erase" | "erase_ink"
        ) {
            self.cancel_selection();
        }
        match req["cmd"].as_str().unwrap_or("") {
            "view" => self.ipc_view(req),
            "draw" => self.ipc_draw(req),
            "erase" => self.ipc_erase(req),
            "goto" => self.ipc_goto(req),
            "crop" => self.ipc_crop(req),
            "place" => self.ipc_place(req),
            "raster_get" => self.ipc_raster_get(req),
            "raster_erase" => self.ipc_raster_erase(req),
            "erase_ink" => self.ipc_erase_ink(req),
            other => json!({ "ok": false, "error": format!("unknown cmd '{other}'") }),
        }
    }

    /// 1-based page param; None/0 = the page on screen.
    fn req_page(&self, req: &Value) -> usize {
        match req["page"].as_u64() {
            Some(p) if p >= 1 => p as usize - 1,
            _ => self.nb.current,
        }
    }

    fn ipc_view(&mut self, req: &Value) -> Value {
        let idx = self.req_page(req);
        if idx >= self.nb.count {
            return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", idx + 1, self.nb.count) });
        }
        let (snap, patches, rasters) = if idx == self.nb.current {
            (
                ink::snapshot_with_rasters(&self.nb.page, &self.nb.rasters, SNAP_DIV),
                patch_list(&self.nb.page),
                raster_list(&self.nb.rasters),
            )
        } else {
            match Page::load(&self.nb.page_path(idx)) {
                Some(p) => {
                    let rl = ink::load_rasters(&self.nb.render_path(idx));
                    (ink::snapshot_with_rasters(&p, &rl, SNAP_DIV), patch_list(&p), raster_list(&rl))
                }
                None => return json!({ "ok": false, "error": "page file unreadable" }),
            }
        };
        let (w, h, gray) = snap;
        let png = png::encode_gray(w as u32, h as u32, &gray);
        json!({
            "ok": true,
            "page": idx + 1,
            "page_count": self.nb.count,
            "page_width": FB_W,
            "page_height": FB_H,
            "image_scale": SNAP_DIV,
            "png_base64": png::base64(&png),
            "patches": patches,
            "rasters": rasters,
        })
    }

    /// An arbitrary page region as a full-resolution PNG — the agent picks
    /// what ships to the image model (a sketch, a sketch plus handwritten
    /// instructions inside it, an existing raster with annotation marks).
    /// Composites raster patches under the ink unless ink:false/rasters:false.
    fn ipc_crop(&mut self, req: &Value) -> Value {
        let idx = self.req_page(req);
        if idx >= self.nb.count {
            return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", idx + 1, self.nb.count) });
        }
        let Some(rect) = req_rect(req, "rect") else {
            return json!({ "ok": false, "error": "missing 'rect' [x0,y0,x1,y1]" });
        };
        let r = rect.clamp_screen();
        if r.w() < 8 || r.h() < 8 {
            return json!({ "ok": false, "error": "rect is degenerate" });
        }
        let with_ink = req["ink"].as_bool().unwrap_or(true);
        let with_rasters = req["rasters"].as_bool().unwrap_or(true);

        let loaded_page;
        let loaded_rasters;
        let (page, rasters): (&Page, &[ink::RasterPatch]) = if idx == self.nb.current {
            (&self.nb.page, &self.nb.rasters)
        } else {
            match Page::load(&self.nb.page_path(idx)) {
                Some(p) => {
                    loaded_page = p;
                    loaded_rasters = ink::load_rasters(&self.nb.render_path(idx));
                    (&loaded_page, &loaded_rasters)
                }
                None => return json!({ "ok": false, "error": "page file unreadable" }),
            }
        };

        let (w, _h, gray) = if with_ink && with_rasters {
            ink::snapshot_with_rasters(page, rasters, 1)
        } else if with_ink {
            ink::snapshot_with_rasters(page, &[], 1)
        } else {
            ink::snapshot_with_rasters(&Page::default(), rasters, 1)
        };
        let (cw, ch) = (r.w(), r.h());
        let mut crop = vec![255u8; (cw * ch) as usize];
        for y in 0..ch {
            let src = ((r.y0 + y) * w + r.x0) as usize;
            let dst = (y * cw) as usize;
            crop[dst..dst + cw as usize].copy_from_slice(&gray[src..src + cw as usize]);
        }
        let png = png::encode_gray(cw as u32, ch as u32, &crop);
        json!({
            "ok": true,
            "page": idx + 1,
            "png_base64": png::base64(&png),
            "width": cw,
            "height": ch,
            "rect": [r.x0, r.y0, r.x1, r.y1],
        })
    }

    /// Place an agent-generated grayscale raster at an agent-chosen page
    /// rect (aspect-fit inside it, centered). Returns the raster id.
    fn ipc_place(&mut self, req: &Value) -> Value {
        let idx = self.req_page(req);
        if idx >= self.nb.count {
            return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", idx + 1, self.nb.count) });
        }
        let (Some(w), Some(h)) = (req["w"].as_i64(), req["h"].as_i64()) else {
            return json!({ "ok": false, "error": "missing 'w'/'h'" });
        };
        let (w, h) = (w as i32, h as i32);
        let Some(b64) = req["raw_base64"].as_str() else {
            return json!({ "ok": false, "error": "missing 'raw_base64' (raw 8-bit grayscale bytes)" });
        };
        let Some(raw) = ink::base64_decode(b64) else {
            return json!({ "ok": false, "error": "raw_base64 is not valid base64" });
        };
        if raw.len() != (w * h) as usize || w <= 0 || h <= 0 {
            return json!({ "ok": false, "error": format!("raw length {} != w*h {}", raw.len(), w * h) });
        }
        let Some(dest) = req_rect(req, "rect") else {
            return json!({ "ok": false, "error": "missing 'rect' [x0,y0,x1,y1] (destination)" });
        };
        let dest = dest.clamp_screen();
        if dest.w() < 16 || dest.h() < 16 {
            return json!({ "ok": false, "error": "destination rect is too small" });
        }
        let replace = req["replace"].as_u64(); /* optional raster id to replace */

        let scale = (dest.w() as f32 / w as f32).min(dest.h() as f32 / h as f32);
        let (dw, dh) = (((w as f32 * scale) as i32).max(1), ((h as f32 * scale) as i32).max(1));
        /* stored clean; graphite tooth is applied at blit/snapshot time */
        let gray = ink::resize_gray(&raw, w, h, dw, dh);
        /* trim the paper margins: the stored rect hugs the actual drawing,
         * so every later blit walks content, not acres of background */
        let (gray, cw, chh, ox, oy) = ink::content_crop(gray, dw, dh);
        let (px, py) = (dest.x0 + (dest.w() - dw) / 2 + ox, dest.y0 + (dest.h() - dh) / 2 + oy);
        let (dw, dh) = (cw, chh);

        if idx == self.nb.current {
            self.checkpoint(true);
            let mut repaint = Rect { x0: px, y0: py, x1: px + dw - 1, y1: py + dh - 1 };
            let mut old: Option<ink::RasterPatch> = None;
            if let Some(rid) = replace {
                if let Some(pos) = self.nb.rasters.iter().position(|r| r.id == rid) {
                    let o = self.nb.rasters.remove(pos);
                    repaint = repaint.union(o.rect());
                    old = Some(o);
                }
            }
            let id = self.nb.next_raster;
            self.nb.next_raster += 1;
            let new = ink::RasterPatch { id, x0: px, y0: py, w: dw, h: dh, gray };
            /* an EDIT (replace) crossfades old → new instead of blinking;
             * a fresh placement appears in one 16-level pass */
            let can_fade = old.is_some() && self.agent_page.is_none() && self.lib_view.is_none();
            if can_fade {
                let region = repaint.pad(2).clamp_screen();
                let from = ink::raster_composite(&[old.as_ref().unwrap()], region);
                let to = ink::raster_composite(&[&new], region);
                self.fade = Some(RasterFade {
                    page: idx,
                    region,
                    from,
                    to,
                    step: 1,
                    next_at: Instant::now(),
                });
            }
            self.nb.rasters.push(new);
            self.nb.rasters_dirty = true;
            self.nb.save_current();
            if !can_fade {
                self.repaint_raster_rect(repaint.pad(2));
            }
            self.live.status("render");
            self.last_activity_page = idx;
            println!("sketchbook: raster #{id} {dw}x{dh} placed at ({px},{py}) on page {}", idx + 1);
            json!({ "ok": true, "id": id, "page": idx + 1, "placed": [px, py, dw, dh] })
        } else {
            let path = self.nb.render_path(idx);
            let mut rasters = ink::load_rasters(&path);
            if let Some(rid) = replace {
                rasters.retain(|r| r.id != rid);
            }
            let id = rasters.iter().map(|r| r.id + 1).max().unwrap_or(1);
            rasters.push(ink::RasterPatch { id, x0: px, y0: py, w: dw, h: dh, gray });
            if let Err(e) = ink::save_rasters(&path, &rasters) {
                return json!({ "ok": false, "error": format!("save rasters: {e}") });
            }
            self.last_activity_page = idx;
            json!({ "ok": true, "id": id, "page": idx + 1, "placed": [px, py, dw, dh] })
        }
    }

    /// Hand back one raster patch as PNG — the input for an edit-mode
    /// regeneration ("remove the background", "darker").
    fn ipc_raster_get(&mut self, req: &Value) -> Value {
        let idx = self.req_page(req);
        if idx >= self.nb.count {
            return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", idx + 1, self.nb.count) });
        }
        let loaded;
        let rasters: &[ink::RasterPatch] = if idx == self.nb.current {
            &self.nb.rasters
        } else {
            loaded = ink::load_rasters(&self.nb.render_path(idx));
            &loaded
        };
        let rl = match req["id"].as_u64() {
            Some(id) => rasters.iter().find(|r| r.id == id),
            None => rasters.last(), /* no id: the most recent */
        };
        let Some(rl) = rl else {
            return json!({ "ok": false, "error": "no such raster on this page" });
        };
        let png = png::encode_gray(rl.w as u32, rl.h as u32, &rl.gray);
        json!({
            "ok": true,
            "page": idx + 1,
            "id": rl.id,
            "png_base64": png::base64(&png),
            "width": rl.w,
            "height": rl.h,
            "rect": [rl.x0, rl.y0, rl.x0 + rl.w - 1, rl.y0 + rl.h - 1],
        })
    }

    /// Remove one raster patch (the agent fixing itself, or replacing).
    fn ipc_raster_erase(&mut self, req: &Value) -> Value {
        let idx = self.req_page(req);
        if idx >= self.nb.count {
            return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", idx + 1, self.nb.count) });
        }
        let Some(id) = req["id"].as_u64() else {
            return json!({ "ok": false, "error": "missing 'id'" });
        };
        if idx == self.nb.current {
            let Some(pos) = self.nb.rasters.iter().position(|r| r.id == id) else {
                return json!({ "ok": false, "error": format!("no raster #{id} on page {}", idx + 1) });
            };
            self.checkpoint(true);
            let rl = self.nb.rasters.remove(pos);
            self.nb.rasters_dirty = true;
            self.nb.save_current();
            self.repaint_raster_rect(rl.rect().pad(2));
            json!({ "ok": true, "page": idx + 1 })
        } else {
            let path = self.nb.render_path(idx);
            let mut rasters = ink::load_rasters(&path);
            let before = rasters.len();
            rasters.retain(|r| r.id != id);
            if rasters.len() == before {
                return json!({ "ok": false, "error": format!("no raster #{id} on page {}", idx + 1) });
            }
            if let Err(e) = ink::save_rasters(&path, &rasters) {
                return json!({ "ok": false, "error": format!("save rasters: {e}") });
            }
            json!({ "ok": true, "page": idx + 1 })
        }
    }

    /// One crossfade frame: blend old→new composites, stamp strokes over,
    /// push with the 16-level waveform. The deterministic grain field keeps
    /// the tooth stable across frames, so the morph reads as the drawing
    /// changing — not as noise crawling.
    fn fade_tick(&mut self) {
        let Some(mut f) = self.fade.take() else { return };
        if Instant::now() < f.next_at {
            self.fade = Some(f);
            return;
        }
        /* never fight the writer; menus repaint on close anyway */
        if self.cur_stroke.is_some()
            || self.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(350))
        {
            f.next_at = Instant::now() + Duration::from_millis(200);
            self.fade = Some(f);
            return;
        }
        if f.page != self.nb.current || self.agent_page.is_some() || self.lib_view.is_some() {
            return; /* page turned / view changed: model is truth, fade dropped */
        }
        if f.step >= FADE_STEPS {
            /* final frame = the model itself (strokes included) */
            self.repaint_raster_rect(f.region);
            return;
        }
        let t = f.step as f32 / FADE_STEPS as f32;
        let region = f.region;
        let rw = region.w();
        for y in region.y0..=region.y1 {
            let row = ((y - region.y0) * rw) as usize;
            for x in region.x0..=region.x1 {
                let i = row + (x - region.x0) as usize;
                let (fa, fb_) = (f.from[i], f.to[i]);
                if fa == 255 && fb_ == 255 {
                    continue; /* paper on both sides of the fade */
                }
                let a = fa as f32;
                let b = fb_ as f32;
                let v = (a + (b - a) * t).round().clamp(0.0, 255.0) as u8;
                self.fb.px(x, y, ink::grain_565(v, x, y));
            }
        }
        self.nb.page.stamp_region(&mut self.fb, region);
        self.disp.update(region.x0, region.y0, region.w(), region.h(), Wave::Text);
        f.step += 1;
        f.next_at = Instant::now() + FADE_STEP;
        self.fade = Some(f);
    }

    /// Remove USER strokes that lie fully inside a rect — the agent
    /// cleaning up handwritten instructions it has acted on. Fully-inside
    /// keeps a sloppy rect from chopping the user's drawing.
    fn ipc_erase_ink(&mut self, req: &Value) -> Value {
        let idx = self.req_page(req);
        if idx >= self.nb.count {
            return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", idx + 1, self.nb.count) });
        }
        let Some(rect) = req_rect(req, "rect") else {
            return json!({ "ok": false, "error": "missing 'rect' [x0,y0,x1,y1]" });
        };
        let r = rect.clamp_screen();
        let inside = |b: &Rect| b.x0 >= r.x0 && b.x1 <= r.x1 && b.y0 >= r.y0 && b.y1 <= r.y1;

        if idx == self.nb.current {
            let st = self.snapshot_state(false);
            let before = self.nb.page.strokes.len();
            self.nb.page.strokes.retain(|s| !ink::stroke_bbox(s).as_ref().is_some_and(inside));
            let removed = before - self.nb.page.strokes.len();
            if removed == 0 {
                return json!({ "ok": false, "error": "no user strokes lie fully inside that rect" });
            }
            self.push_undo(st);
            self.nb.page.dirty = true;
            self.nb.save_current();
            let rr = r.pad(4).clamp_screen();
            if self.agent_page.is_none() && self.lib_view.is_none() {
                let had_gray = self.nb.page.render_region(&mut self.fb, rr, &self.nb.rasters);
                self.disp.update(rr.x0, rr.y0, rr.w(), rr.h(), if had_gray { Wave::Text } else { Wave::Ink });
                self.deghost_at = Some(Instant::now() + Duration::from_millis(900));
                self.restore_chrome_over(rr);
            }
            println!("sketchbook: erased {removed} user strokes in rect on page {}", idx + 1);
            json!({ "ok": true, "page": idx + 1, "removed": removed })
        } else {
            let path = self.nb.page_path(idx);
            let Some(mut p) = Page::load(&path) else {
                return json!({ "ok": false, "error": "page file unreadable" });
            };
            let before = p.strokes.len();
            p.strokes.retain(|s| !ink::stroke_bbox(s).as_ref().is_some_and(inside));
            let removed = before - p.strokes.len();
            if removed == 0 {
                return json!({ "ok": false, "error": "no user strokes lie fully inside that rect" });
            }
            p.dirty = true;
            if let Err(e) = p.save(&path) {
                return json!({ "ok": false, "error": format!("save: {e}") });
            }
            json!({ "ok": true, "page": idx + 1, "removed": removed })
        }
    }

    /// Repaint a raster-affected region with the 16-level waveform (real
    /// grayscale; DU would posterize it).
    fn repaint_raster_rect(&mut self, r: Rect) {
        if self.agent_page.is_some() || self.lib_view.is_some() {
            return; /* another view owns the screen; appears on return */
        }
        let r = r.clamp_screen();
        self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Text);
        self.restore_chrome_over(r);
    }


    fn ipc_draw(&mut self, req: &Value) -> Value {
        let Some(svg) = req["svg"].as_str() else {
            return json!({ "ok": false, "error": "missing 'svg'" });
        };
        /* _texts: typeset Garamond runs — this app draws plotter strokes only */
        let (strokes, _texts, notes) =
            match svg_ink::parse(svg, self.text_scale, svg_ink::PiFont::from(self.pi_font)) {
            Ok(v) => v,
            Err(e) => return json!({ "ok": false, "error": e }),
        };
        for n in &notes {
            println!("sketchbook: draw note: {n}");
        }
        let idx = self.req_page(req);
        if idx == self.nb.current {
            self.checkpoint(false);
            let id = self.nb.page.add_patch(strokes, Vec::new());
            let patch = self.nb.page.patches.last().unwrap();
            let bbox = ink::patch_bbox(patch).map(|b| b.clamp_screen());
            let n_strokes = patch.strokes.len();
            /* queue the ghost-hand animation — unless another view owns the
             * screen right now (the strokes appear on return, via the full
             * repaint from the model) */
            let animate = self.agent_page.is_none() && self.lib_view.is_none();
            if animate {
                self.live.status("draw");
            }
            for s in patch.strokes.iter().filter(|_| animate) {
                if let Some(bb) = ink::stroke_bbox(s) {
                    self.anim.push_back(AnimStroke {
                        page: idx,
                        patch: id,
                        gray: s.gray,
                        remaining: s.pts.iter().copied().collect(),
                        last: None,
                        bbox: bb.clamp_screen(),
                    });
                }
            }
            self.nb.save_current();
            self.last_activity_page = idx;
            println!("sketchbook: patch #{id} on page {} ({n_strokes} strokes)", idx + 1);
            json!({
                "ok": true, "id": id, "page": idx + 1,
                "bbox": bbox.map(|b| json!([b.x0, b.y0, b.x1, b.y1])).unwrap_or(json!(null)),
                "layout": layout_hints(&self.nb.page, self.text_scale),
                "notes": notes,
            })
        } else {
            /* a page that isn't on screen: mutate its file directly */
            if idx >= self.nb.count {
                return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", idx + 1, self.nb.count) });
            }
            let path = self.nb.page_path(idx);
            let Some(mut p) = Page::load(&path) else {
                return json!({ "ok": false, "error": "page file unreadable" });
            };
            let id = p.add_patch(strokes, Vec::new());
            let bbox = ink::patch_bbox(p.patches.last().unwrap()).map(|b| b.clamp_screen());
            if let Err(e) = p.save(&path) {
                return json!({ "ok": false, "error": format!("save: {e}") });
            }
            json!({
                "ok": true, "id": id, "page": idx + 1,
                "bbox": bbox.map(|b| json!([b.x0, b.y0, b.x1, b.y1])).unwrap_or(json!(null)),
                "layout": layout_hints(&p, self.text_scale),
                "notes": notes,
            })
        }
    }

    fn ipc_erase(&mut self, req: &Value) -> Value {
        let Some(id) = req["id"].as_u64() else {
            return json!({ "ok": false, "error": "missing 'id'" });
        };
        let idx = self.req_page(req);
        if idx == self.nb.current {
            /* drop any still-animating strokes of this patch */
            let mut region: Option<Rect> = None;
            self.anim.retain(|a| {
                if a.patch == id && a.page == idx {
                    region = Some(region.map_or(a.bbox, |r| r.union(a.bbox)));
                    false
                } else {
                    true
                }
            });
            let st = self.snapshot_state(false);
            match self.nb.page.remove_patch(id) {
                Some(b) => {
                    self.push_undo(st);
                    if self.agent_page.is_none() && self.sidebar.is_none() && self.lib_view.is_none() {
                        let r = region.map_or(b, |r| r.union(b)).pad(4).clamp_screen();
                        let had_gray = self.nb.page.render_region(&mut self.fb, r, &self.nb.rasters);
                        self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
                        self.restore_chrome_over(r);
                    }
                    self.nb.save_current();
                    json!({ "ok": true })
                }
                None => json!({ "ok": false, "error": format!("no patch {id} on page {}", idx + 1) }),
            }
        } else {
            if idx >= self.nb.count {
                return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", idx + 1, self.nb.count) });
            }
            let path = self.nb.page_path(idx);
            let Some(mut p) = Page::load(&path) else {
                return json!({ "ok": false, "error": "page file unreadable" });
            };
            match p.remove_patch(id) {
                Some(_) => {
                    if let Err(e) = p.save(&path) {
                        return json!({ "ok": false, "error": format!("save: {e}") });
                    }
                    json!({ "ok": true })
                }
                None => json!({ "ok": false, "error": format!("no patch {id} on page {}", idx + 1) }),
            }
        }
    }

    /// pi turns the page. Refused while the user is writing or in a menu
    /// view — yanking the page out from under them would be rude.
    fn ipc_goto(&mut self, req: &Value) -> Value {
        let Some(p) = req["page"].as_u64().filter(|&p| p >= 1) else {
            return json!({ "ok": false, "error": "missing/invalid 'page' (1-based)" });
        };
        let idx = p as usize - 1;
        if idx >= self.nb.count {
            return json!({ "ok": false, "error": format!("no page {} (sketchbook has {})", p, self.nb.count) });
        }
        if self.agent_page.is_some() || self.sidebar.is_some() || self.lib_view.is_some() {
            return json!({ "ok": false, "error": "the user is in a menu/instructions/library view; not turning the page" });
        }
        if self.cur_stroke.is_some()
            || self.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
        {
            return json!({ "ok": false, "error": "the user is writing right now; try again shortly" });
        }
        if idx != self.nb.current {
            self.flip(idx as i32 - self.nb.current as i32);
        }
        println!("sketchbook: pi turned to page {}", idx + 1);
        json!({
            "ok": true, "page": idx + 1, "page_count": self.nb.count,
            "layout": layout_hints(&self.nb.page, self.text_scale),
        })
    }

    /* -- pi liveness -- */

    /// Watchdog + resurrection: restart a wedged run (process alive but
    /// silent mid-turn for PI_STALL) and respawn a dead process. Both paths
    /// keep the session (`--continue`) and re-arm the pause trigger so the
    /// page that got swallowed is re-sent instead of lost.
    fn check_pi_health(&mut self) {
        /* wedged: a turn is in flight but pi has gone completely silent */
        if self.streaming && self.pi.is_some() {
            if let Some(at) = self.pi_alive_at {
                if at.elapsed() >= self.pi_stall {
                    println!(
                        "sketchbook: pi silent for {}s mid-turn; restarting it",
                        at.elapsed().as_secs()
                    );
                    self.pi = None; /* Drop kills the child */
                    self.streaming = false;
                    self.reply_buf.clear();
                    self.set_working(false);
                    self.live.status("idle");
                    self.pi_alive_at = None;
                    self.pi_respawn_at = Some(Instant::now());
                    /* let the agent page recover too: stop waiting for a
                     * rewrite that will never finish */
                    if let Some(ap) = self.agent_page.as_mut() {
                        ap.waiting = false;
                    }
                }
            }
        }
        /* gone: bring it back (crash earlier, or the restart above) */
        if self.pi.is_none() {
            if let Some(at) = self.pi_respawn_at {
                if Instant::now() >= at {
                    match pi_rpc::spawn(&self.sock) {
                        Ok(p) => {
                            self.pi = Some(p);
                            self.pi_respawn_at = None;
                            println!("sketchbook: pi respawned (session continued)");
                            /* re-send the current page if it has unanswered
                             * ink — the wedged turn swallowed that pause */
                            if !self.nb.page.is_empty() && self.agent_page.is_none() {
                                self.page_changed = true;
                                self.idle_at = Some(Instant::now() + Duration::from_secs(2));
                            }
                        }
                        Err(e) => {
                            println!("sketchbook: pi respawn failed: {e}; retrying in 30s");
                            self.pi_respawn_at = Some(Instant::now() + Duration::from_secs(30));
                        }
                    }
                }
            }
        }
    }

    /* -- AI ink animation -- */

    fn anim_tick(&mut self) {
        /* never fight the writer: hold while the pen is on/near the glass;
         * also hold while another view (sidebar, AGENT.md) owns the screen */
        if self.cur_stroke.is_some()
            || self.sidebar.is_some()
            || self.agent_page.is_some()
            || self.lib_view.is_some()
            || self.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(350))
        {
            self.last_anim = Instant::now();
            return;
        }
        let mut budget = ANIM_BUDGET;
        while budget > 0.0 {
            let Some(a) = self.anim.front_mut() else { break };
            if a.page != self.nb.current {
                self.anim.pop_front(); /* already in the model; visible on flip */
                continue;
            }
            let Some(next) = a.remaining.pop_front() else {
                self.anim.pop_front();
                continue;
            };
            let from = a.last.unwrap_or(next);
            if a.last.is_none() {
                self.live.ai_break();
            }
            self.live.ai(next.x, next.y, next.r);
            ink::stamp_segment(&mut self.fb, from, next, a.gray);
            let seg = Rect {
                x0: (from.x.min(next.x) - 4.0) as i32,
                y0: (from.y.min(next.y) - 4.0) as i32,
                x1: (from.x.max(next.x) + 4.0).ceil() as i32,
                y1: (from.y.max(next.y) + 4.0).ceil() as i32,
            };
            self.anim_dirty = Some(match self.anim_dirty {
                None => seg,
                Some(d) => d.union(seg),
            });
            self.anim_settle = Some(match self.anim_settle {
                None => seg,
                Some(d) => d.union(seg),
            });
            budget -= (next.x - from.x).hypot(next.y - from.y).max(1.5);
            a.last = Some(next);
        }
        if let Some(r) = self.anim_dirty.take() {
            let r = r.clamp_screen();
            /* black ink now: the same crisp low-latency waveform as the pen */
            self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
        }
        /* the ghost hand finished: one 16-level pass over everything it
         * wrote smooths the DU-rough stroke edges (the old GL16 crispness) */
        if self.anim.is_empty() {
            if let Some(r) = self.anim_settle.take() {
                let r = r.pad(4).clamp_screen();
                self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Text);
            }
        }
        self.last_anim = Instant::now();
    }

    /* -- sleep (takeover only) -- */

    fn show_sleep_page(&mut self) -> Vec<u16> {
        let saved = self.fb.copy_band(0, FB_H);
        self.fb.fill_rect(0, 0, FB_W, FB_H, WHITE);
        let msg = "sketchbook sleeps";
        let w = text::width(text::Face::Body, 44.0, msg);
        text::draw_line(&mut self.fb, (FB_W - w) / 2, FB_H / 2 - 60, text::Face::Body, 44.0, msg);
        let hint = "press power to wake";
        let hw = text::width(text::Face::Body, 28.0, hint);
        text::draw_line(&mut self.fb, (FB_W - hw) / 2, FB_H / 2 + 10, text::Face::Body, 28.0, hint);
        saved
    }

    fn restore_sleep_page(&mut self, saved: &[u16]) {
        self.fb.paste_band(0, saved);
    }
}

/// Parse a [x0,y0,x1,y1] array parameter into a Rect.
fn req_rect(req: &Value, key: &str) -> Option<Rect> {
    let a = req[key].as_array()?;
    if a.len() != 4 {
        return None;
    }
    let v: Vec<i32> = a.iter().filter_map(|x| x.as_i64().map(|v| v as i32)).collect();
    if v.len() != 4 {
        return None;
    }
    Some(Rect { x0: v[0].min(v[2]), y0: v[1].min(v[3]), x1: v[0].max(v[2]), y1: v[1].max(v[3]) })
}

fn raster_list(rasters: &[ink::RasterPatch]) -> Value {
    Value::Array(
        rasters
            .iter()
            .map(|r| json!({ "id": r.id, "rect": [r.x0, r.y0, r.x0 + r.w - 1, r.y0 + r.h - 1] }))
            .collect(),
    )
}

/// One line for the pause message: where the agent's rasters sit.
fn raster_summary(rasters: &[ink::RasterPatch]) -> String {
    if rasters.is_empty() {
        return "none".into();
    }
    rasters
        .iter()
        .map(|r| format!("#{} at ({},{})-({},{})", r.id, r.x0, r.y0, r.x0 + r.w - 1, r.y0 + r.h - 1))
        .collect::<Vec<_>>()
        .join(", ")
}

fn patch_list(p: &Page) -> Value {
    Value::Array(
        p.patches
            .iter()
            .map(|pa| {
                let b = ink::patch_bbox(pa).map(|b| b.clamp_screen());
                json!({
                    "id": pa.id,
                    "bbox": b.map(|b| json!([b.x0, b.y0, b.x1, b.y1])).unwrap_or(json!(null)),
                })
            })
            .collect(),
    )
}

/// Measured page geometry for the pause message: where ink already is,
/// where the gaps are, and how big the user's handwriting runs — in page
/// coordinates, so placement is arithmetic for pi rather than eyeballing.
fn layout_hints(p: &Page, text_scale: f32) -> String {
    let bands = p.ink_bands();
    if bands.is_empty() {
        return "The page is blank.".into();
    }
    let mut rows: Vec<String> = bands
        .iter()
        .map(|b| format!("y{}-{} (x{}-{}{})", b.y0, b.y1, b.x0, b.x1, if b.user { "" } else { ", yours" }))
        .collect();
    if rows.len() > 12 {
        let extra = rows.len() - 11;
        rows.truncate(11);
        rows.push(format!("and {extra} more"));
    }

    let mut free: Vec<String> = Vec::new();
    if bands[0].y0 > 130 {
        free.push(format!("y0-{} (top)", bands[0].y0 - 24));
    }
    for w in bands.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if b.y0 - a.y1 >= 96 {
            free.push(format!("y{}-{}", a.y1 + 24, b.y0 - 24));
        }
    }
    let last = bands.last().unwrap();
    if last.y1 < FB_H - 130 {
        free.push(format!("y{}-{} (bottom)", last.y1 + 24, FB_H - 24));
    }
    let free = if free.is_empty() {
        "none — only annotate directly on the ink".to_string()
    } else {
        free.join(", ")
    };

    let size_hint = match p.user_line_height() {
        Some(lh) => {
            let fs = (lh * 9 / 10).clamp(30, 90);
            format!(
                " The user's handwriting rows are ~{lh}px tall: write at font-size ~{fs} \
                 with ~{}px between your baselines.",
                fs * 3 / 2
            )
        }
        None => String::new(),
    };
    let zoom_note = if (text_scale - 1.0).abs() > 0.01 {
        format!(
            " NOTE: the user zooms your text to {}% — every font-size you write renders \
             that much {}; budget widths and baseline spacing accordingly.",
            (text_scale * 100.0).round() as i32,
            if text_scale > 1.0 { "larger" } else { "smaller" },
        )
    } else {
        String::new()
    };
    format!("Ink rows: {}. Free bands (full width): {}.{}{}", rows.join(", "), free, size_hint, zoom_note)
}

fn patch_summary(p: &Page) -> String {
    if p.patches.is_empty() {
        return "none".into();
    }
    p.patches
        .iter()
        .map(|pa| match ink::patch_bbox(pa) {
            Some(b) => format!("#{} at ({},{})-({},{})", pa.id, b.x0, b.y0, b.x1, b.y1),
            None => format!("#{}", pa.id),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The power-button sleep cycle (riddle's dance), same as collab's.
fn sleep_cycle(
    app: &mut App,
    p: &mut power::PowerButton,
    pen: &mut Option<Pen>,
    touchdev: &mut Option<touch::TouchDevice>,
) {
    println!("sketchbook: sleeping");
    app.nb.save_current();
    let saved = app.show_sleep_page();
    app.disp.full_refresh();
    std::thread::sleep(Duration::from_millis(800));
    /* flush local changes to the VM while the sleep page settles — sync is
     * event-driven (edit / sleep / wake), not timer-driven, to keep the
     * radio quiet; bounded so a dead network can't stall sleep */
    power::sync_flush(APP, Duration::from_secs(45));
    let count0 = power::suspend_count();
    let mut attempts = 0;
    'sleeping: loop {
        if p.grabbed {
            let _ = std::process::Command::new("systemctl").arg("suspend").status();
        }
        attempts += 1;
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(6) {
            std::thread::sleep(Duration::from_millis(400));
            if power::suspend_count() > count0 {
                break 'sleeping;
            }
        }
        if attempts >= 8 {
            println!("sketchbook: suspend never happened ({attempts} tries); waking the page");
            break;
        }
        println!("sketchbook: suspend aborted (EPD discharge timer), retrying");
    }
    println!("sketchbook: waking");
    app.restore_sleep_page(&saved);
    app.disp.full_refresh();
    power::wifi_heal(); /* pi needs the network back */
    if let Some(pd) = pen.as_mut() {
        pd.drain(|_, _| {});
    }
    if let Some(td) = touchdev.as_mut() {
        let _ = td.drain();
    }
    p.drain_pressed();
}

/* ---- main ---------------------------------------------------------------- */

fn main() -> std::process::ExitCode {
    let (disp, fb) = match Display::open(APP) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sketchbook: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let takeover = disp.is_takeover();
    println!(
        "sketchbook: up, fb={FB_W}x{FB_H} ({})",
        if takeover { "takeover/rm2fb" } else { "windowed/qtfb" }
    );
    install_signal_handlers();

    let sock = sock_path();
    let ipc = IpcServer::open(APP, &sock)
        .map_err(|e| eprintln!("sketchbook: tool socket: {e} — pi gets no drawing tools"))
        .ok();
    let pi = match pi_rpc::spawn(&sock) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("sketchbook: could not start pi: {e}");
            None
        }
    };

    let mut pen = Pen::open(APP);
    let direct_pen = pen.is_some();
    if takeover {
        if let Some(p) = pen.as_ref() {
            p.grab();
        }
    }
    let mut touchdev = if takeover {
        touch::TouchDevice::open(APP)
            .map_err(|e| eprintln!("sketchbook: no touch device ({e}) — page flips disabled"))
            .ok()
    } else {
        None
    };
    let mut powerdev = if takeover {
        power::PowerButton::open(APP)
            .map_err(|e| eprintln!("sketchbook: no power button ({e})"))
            .ok()
    } else {
        None
    };
    let mut power_grace = Instant::now();

    /* Idle auto-suspend (takeover only — windowed mode leaves it to
     * xochitl). Stock xochitl sleeps after ~10 min idle; we took the power
     * button, so we owe the battery the same courtesy. Tunable via
     * SKETCHBOOK_AUTO_SLEEP_MIN (minutes), 0 disables. */
    let auto_sleep_min: u64 = std::env::var("SKETCHBOOK_AUTO_SLEEP_MIN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let auto_sleep = (powerdev.is_some() && auto_sleep_min > 0)
        .then(|| Duration::from_secs(auto_sleep_min * 60));
    let mut last_activity = Instant::now();

    let now = Instant::now();
    let (text_scale, quiet, saved_font, eraser) = load_settings();
    let pi_font = saved_font.unwrap_or_else(|| hershey::default_face(APP));
    let mut app = App {
        fb,
        disp,
        pi,
        nb: Sketchbook::open(),
        ipc,
        ink_flush: if takeover { INK_FLUSH_TAKEOVER } else { INK_FLUSH_QTFB },
        cur_stroke: None,
        ink_dirty: None,
        last_ink_flush: now,
        palm: palm::PalmGuard::default(),
        flips_since_flash: 0,
        last_contact: None,
        contact_changed: false,
        page_changed: false,
        idle_at: None,
        streaming: false,
        reply_buf: String::new(),
        sock: sock.clone(),
        pi_alive_at: None,
        pi_respawn_at: None,
        pi_stall: pi_stall(),
        anim: VecDeque::new(),
        anim_dirty: None,
        anim_settle: None,
        last_anim: now,
        touch_start: None,
        touch_last: (0, 0),
        indicator_until: None,
        working: false,
        agent_page: None,
        sidebar: None,
        last_activity_page: 0,
        text_scale,
        quiet,
        pi_font,
        lib_view: None,
        fade: None,
        tb: {
            let mut tb = toolbar::EdgeToolbar::new(FB_W - 52, 96);
            tb.items = vec![
                toolbar::Item { id: TB_UNDO, icon: toolbar::Icon::Undo, label: "UNDO", active: false },
                toolbar::Item { id: TB_REDO, icon: toolbar::Icon::Redo, label: "REDO", active: false },
                toolbar::Item { id: TB_SELECT, icon: toolbar::Icon::Lasso, label: "SELECT", active: false },
                toolbar::Item { id: TB_ERASER, icon: eraser.icon(), label: eraser.label(), active: eraser != EraserMode::Object },
                toolbar::Item { id: TB_GENERATE, icon: toolbar::Icon::Squiggle, label: "RENDER", active: false },
                toolbar::Item { id: TB_QUIET, icon: toolbar::Icon::Pi, label: if quiet { "OFF" } else { "AUTO" }, active: !quiet },
                toolbar::Item { id: TB_REFRESH, icon: toolbar::Icon::Refresh, label: "CLEAN", active: false },
            ];
            tb
        },
        sel: None,
        pen_chrome: false,
        eraser,
        rub_loop: None,
        undo: Vec::new(),
        redo: Vec::new(),
        pending_undo: None,
        deghost_at: None,
        live: live::Live::new(),
    };

    /* first paint */
    app.nb.page.render_full(&mut app.fb, &app.nb.rasters);
    app.disp.full_refresh();
    app.tb.items.iter_mut().for_each(|it| {
        if it.id == TB_QUIET {
            it.active = !app.quiet; /* eye open = watching */
        }
    });
    app.draw_menu_icon();
    app.draw_toolbar();
    app.show_page_indicator();

    while RUNNING.load(Ordering::Relaxed) {
        let mut timeout = next_timeout(&app);
        if let Some(limit) = auto_sleep {
            /* wake the poll for the idle deadline — it blocks forever otherwise */
            let ms = limit.saturating_sub(last_activity.elapsed()).as_millis() as i32;
            timeout = if timeout < 0 { ms.max(0) } else { timeout.min(ms.max(0)) };
        }
        let mut pfds: Vec<libc::pollfd> = vec![
            libc::pollfd { fd: app.disp.raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: pen.as_ref().map_or(-1, |p| p.raw_fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: app.pi.as_ref().map_or(-1, |p| p.raw_fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: touchdev.as_ref().map_or(-1, |t| t.raw_fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: powerdev.as_ref().map_or(-1, |p| p.raw_fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: app.ipc.as_ref().map_or(-1, |s| s.listen_fd()), events: libc::POLLIN, revents: 0 },
        ];
        let conn_base = pfds.len();
        if let Some(ipc) = app.ipc.as_ref() {
            for c in &ipc.conns {
                pfds.push(libc::pollfd { fd: c.fd, events: libc::POLLIN, revents: 0 });
            }
        }
        if unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, timeout) } < 0 {
            continue; /* EINTR */
        }

        /* -- power button -- */
        if pfds[4].revents & libc::POLLIN != 0 {
            if let Some(p) = powerdev.as_mut() {
                if p.drain_pressed() && Instant::now() >= power_grace {
                    sleep_cycle(&mut app, p, &mut pen, &mut touchdev);
                    power_grace = Instant::now() + Duration::from_secs(3);
                }
                last_activity = Instant::now();
            }
        }

        /* -- pen -- */
        if pfds[1].revents & libc::POLLIN != 0 {
            if let Some(p) = pen.as_mut() {
                let mut frames = Vec::new();
                let seen = p.drain(|p, phase| {
                    frames.push((phase, p.sx, p.sy, p.pressure, p.rubber));
                });
                if seen {
                    app.palm.arm();
                    last_activity = Instant::now();
                }
                if direct_pen {
                    for (phase, x, y, pr, rub) in frames {
                        app.pen_point(phase, x, y, pr, rub);
                    }
                }
            }
        }

        /* -- raw touch (takeover) -- */
        if pfds[3].revents & libc::POLLIN != 0 {
            if let Some(t) = touchdev.as_mut() {
                /* no 5-finger quit here (a writing palm reads as 5+
                 * contacts) — the top-edge swipe -> CLOSE is the exit */
                let (evs, _quit) = t.drain();
                if !evs.is_empty() {
                    last_activity = Instant::now();
                }
                for e in evs {
                    app.touch(e.phase, e.x, e.y);
                }
            }
        }

        /* -- qtfb socket (windowed preview) -- */
        if pfds[0].revents & libc::POLLIN != 0 {
            while let Some(event) = app.disp.try_next_event() {
                match event {
                    Event::Closed => {
                        RUNNING.store(false, Ordering::Relaxed);
                        break;
                    }
                    Event::Interrupted => continue,
                    Event::Touch { phase, x, y, .. } => app.touch(phase, x, y),
                    Event::Pen { phase, x, y, .. } => {
                        app.palm.arm();
                        if !direct_pen {
                            let ph = match phase {
                                Phase::Press => PenPhase::Press,
                                Phase::Move => PenPhase::Move,
                                Phase::Release => PenPhase::Release,
                            };
                            app.pen_point(ph, x, y, 0, false);
                        }
                    }
                    Event::Key { .. } | Event::Other => {}
                }
            }
        }

        /* -- pi stdout -- */
        if pfds[2].revents & libc::POLLIN != 0 {
            /* any stdout traffic counts as liveness, even event types the
             * translator ignores (reasoning deltas, housekeeping) */
            app.pi_alive_at = Some(Instant::now());
            let events = app.pi.as_mut().map(|p| p.drain()).unwrap_or_default();
            for ev in events {
                app.handle_pi(ev);
            }
        }

        /* -- tool socket -- */
        if pfds[5].revents & libc::POLLIN != 0 {
            if let Some(ipc) = app.ipc.as_mut() {
                ipc.accept();
            }
        }
        for i in conn_base..pfds.len() {
            if pfds[i].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                let fd = pfds[i].fd;
                let reqs = app.ipc.as_mut().map(|s| s.read_conn(fd)).unwrap_or_default();
                for req in reqs {
                    let resp = app.handle_ipc_request(&req);
                    if let Some(ipc) = app.ipc.as_mut() {
                        ipc.reply(fd, &resp);
                    }
                }
            }
        }

        /* -- due work -- */
        if app.ink_dirty.is_some() && app.last_ink_flush.elapsed() >= app.ink_flush {
            let r = app.ink_dirty.take().unwrap().clamp_screen();
            app.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
            app.last_ink_flush = Instant::now();
        }
        if !app.anim.is_empty() && app.last_anim.elapsed() >= ANIM_TICK {
            app.anim_tick();
        }
        app.fade_tick();
        app.maybe_send_page();
        app.live.tick(app.nb.current);
        app.check_pi_health();
        if app.indicator_until.is_some_and(|at| Instant::now() >= at) {
            app.clear_page_indicator();
        }
        /* post-erase deghost: only once the pen has settled, and never
         * under a menu/text view (their close repaints anyway) */
        if let Some(at) = app.deghost_at {
            if Instant::now() >= at {
                if app.cur_stroke.is_some()
                    || app.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(700))
                {
                    app.deghost_at = Some(Instant::now() + Duration::from_millis(600));
                } else {
                    app.deghost_at = None;
                    if app.sidebar.is_none() && app.agent_page.is_none() && app.lib_view.is_none() {
                        app.disp.full_refresh();
                    }
                }
            }
        }
        /* -- idle auto-suspend -- */
        if let (Some(limit), Some(p)) = (auto_sleep, powerdev.as_mut()) {
            /* deferred while pi is mid-turn (streaming/working clear on End,
             * and the stall watchdog kills a wedged turn, so this can't
             * hold the device awake forever) */
            if !app.streaming
                && !app.working
                && last_activity.elapsed() >= limit
                && Instant::now() >= power_grace
            {
                println!("sketchbook: idle {auto_sleep_min}min -> auto-sleep");
                sleep_cycle(&mut app, p, &mut pen, &mut touchdev);
                power_grace = Instant::now() + Duration::from_secs(3);
                last_activity = Instant::now();
            }
        }
    }

    println!("sketchbook: exiting");
    app.nb.save_current();
    std::process::ExitCode::SUCCESS
}

/// Milliseconds until the next pending flush/tick is due (-1 = block).
fn next_timeout(app: &App) -> i32 {
    let mut t: Option<Duration> = None;
    let mut soonest = |d: Duration| {
        t = Some(t.map_or(d, |cur| cur.min(d)));
    };
    if app.ink_dirty.is_some() {
        soonest(app.ink_flush.saturating_sub(app.last_ink_flush.elapsed()));
    }
    if !app.anim.is_empty() {
        soonest(ANIM_TICK.saturating_sub(app.last_anim.elapsed()));
    }
    if let Some(at) = app.idle_at {
        soonest(at.saturating_duration_since(Instant::now()));
    }
    if let Some(at) = app.indicator_until {
        soonest(at.saturating_duration_since(Instant::now()));
    }
    if let Some(at) = app.deghost_at {
        soonest(at.saturating_duration_since(Instant::now()));
    }
    if let Some(f) = &app.fade {
        soonest(f.next_at.saturating_duration_since(Instant::now()));
    }
    if app.live.enabled {
        /* batch flushes + reconnect checks while streaming */
        soonest(Duration::from_millis(500));
    }
    if app.streaming {
        if let Some(at) = app.pi_alive_at {
            soonest((at + app.pi_stall).saturating_duration_since(Instant::now()));
        }
    }
    if let Some(at) = app.pi_respawn_at {
        soonest(at.saturating_duration_since(Instant::now()));
    }
    match t {
        Some(d) => (d.as_millis() as i32).max(0),
        None => -1,
    }
}
