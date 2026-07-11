//! notebook — a paper notebook that writes back, on the reMarkable 2.
//!
//! The whole screen is a page. You write on it with the pen, flip pages
//! with a finger swipe, erase with the marker's rubber end. When you pause,
//! the page is photographed to a background pi agent, which may respond by
//! DRAWING on the page — freeform gray ink (text in a plotter font,
//! sketches, arrows, underlines), animated in stroke by stroke like a ghost
//! hand — or by staying silent. Its drawings are tracked as patches it can
//! later erase or replace via its tools (see ipc.rs / notebook-canvas.ts).
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
//!   ipc.rs      unix-socket server for pi's notebook_* tools
//!   pi_rpc.rs   the pi child process (JSONL RPC)
//!   png.rs      grayscale PNG encoder + base64 (page snapshots)

mod display;
#[allow(dead_code)] /* library module from collab; not all used */
mod draw;
mod fb;
mod font;
#[allow(dead_code)] /* small API surface; not every metric is used yet */
mod hershey;
mod hershey_data;
mod ink;
mod ipc;
mod library;
mod live;
mod md_view;
mod pen;
mod pi_rpc;
#[allow(dead_code)] /* library module from collab; not all used */
mod png;
mod power;
mod qtfb;
mod rm2fb;
mod svg_ink;
#[allow(dead_code)] /* library module from collab; not all used */
mod text;
mod touch;

use display::{Display, Wave};
use draw::{text_width, BLACK, WHITE};
use fb::{Framebuffer, SCREEN_H as FB_H, SCREEN_W as FB_W};
use ink::{Notebook, Page, Pt, Rect, Stroke};
use ipc::IpcServer;
use pen::{Pen, PenPhase};
use pi_rpc::{Pi, PiEvent};
use qtfb::{Event, Phase};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/* ---- tuning -------------------------------------------------------------- */

const INK_FLUSH_QTFB: Duration = Duration::from_millis(12);
const INK_FLUSH_TAKEOVER: Duration = Duration::from_millis(8);
const PEN_TIMEOUT: Duration = Duration::from_millis(1500); /* palm rejection */

/// How long a writing pause must last before the page goes to pi.
const IDLE_DELAY: Duration = Duration::from_millis(2800);

/// AI ink animation: one flush per tick, `ANIM_BUDGET` px of path per tick.
const ANIM_TICK: Duration = Duration::from_millis(28);
const ANIM_BUDGET: f32 = 48.0;

/// Page snapshots for pi are half scale (702x936).
const SNAP_DIV: i32 = 2;

const ERASER_R: f32 = 22.0;

/* pi watchdog: total silence (no stdout, no tool calls) this long while a
 * turn is in flight means the run is wedged — restart pi (--continue keeps
 * the session) and re-arm the pause so the page gets re-sent.
 * NOTEBOOK_PI_STALL (seconds) overrides, mainly for the preview harness. */
fn pi_stall() -> Duration {
    Duration::from_secs(
        std::env::var("NOTEBOOK_PI_STALL").ok().and_then(|v| v.parse().ok()).unwrap_or(180),
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
    format!("{home}/.local/share/notebook/settings.json")
}

/// (text_scale, quiet, pi_font) from settings.json; all optional in the
/// file. pi_font None = no override (fall back to $NOTEBOOK_FONT).
fn load_settings() -> (f32, bool, Option<hershey::Face>) {
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
    (scale, quiet, font)
}

fn save_settings(scale: f32, quiet: bool, font: hershey::Face) {
    let p = settings_path();
    if let Some(dir) = std::path::Path::new(&p).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        &p,
        serde_json::to_vec(&json!({
            "text_scale": scale,
            "quiet": quiet,
            "pi_font": hershey::face_name(font),
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
    if let Ok(p) = std::env::var("NOTEBOOK_AGENT_MD") {
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/notebook/AGENT.md")
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

fn sock_path() -> String {
    std::env::var("NOTEBOOK_SOCK").unwrap_or_else(|_| "/tmp/notebook-ctl.sock".into())
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

/* ---- app ------------------------------------------------------------------ */

struct App {
    fb: Framebuffer,
    disp: Display,
    pi: Option<Pi>,
    nb: Notebook,
    ipc: Option<IpcServer>,

    ink_flush: Duration,

    /* pen */
    cur_stroke: Option<Stroke>,
    ink_dirty: Option<Rect>,
    last_ink_flush: Instant,
    last_pen: Option<Instant>,     /* any pen sign of life (incl. hover) */
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

    /* the AGENT.md page, when open (replaces the notebook view) */
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
        let had_gray = self.nb.page.render_region(&mut self.fb, r);
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
        let had_gray = self.nb.page.render_region(&mut self.fb, r);
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
        self.sidebar = Some(Sb { numpad: false, entry: String::new() });
        self.cur_stroke = None;
        self.paint_sidebar();
    }

    fn paint_sidebar(&mut self) {
        let Some(sb) = &self.sidebar else { return };
        let (numpad, entry) = (sb.numpad, sb.entry.clone());
        self.fb.fill_rect(0, 0, SB_W, FB_H, WHITE);
        self.fb.fill_rect(SB_W - 3, 0, 3, FB_H, BLACK);
        self.fb.text(28, 24, "NOTEBOOK", 4, BLACK);
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
                            hershey::face_name(self.pi_font).to_uppercase()
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
        let had_gray = self.nb.page.render_region(&mut self.fb, r);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
        self.draw_menu_icon();
        self.restore_chrome_over(r);
    }

    /// Leave any menu view and land on notebook page `p` (0-based).
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
            self.nb.page.render_full(&mut self.fb);
            self.disp.full_refresh();
            self.draw_menu_icon();
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
            println!("notebook: close (sidebar)");
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
                let i = hershey::FACE_ORDER.iter().position(|f| *f == self.pi_font).unwrap_or(0);
                self.pi_font = hershey::FACE_ORDER[(i + 1) % hershey::FACE_ORDER.len()];
                hershey::set_default_face(self.pi_font);
                save_settings(self.text_scale, self.quiet, self.pi_font);
                println!("notebook: pi font -> {}", hershey::face_name(self.pi_font));
                self.paint_sidebar(); /* stays open for repeated taps */
            }
            SbRow::Quiet => {
                self.quiet = !self.quiet;
                save_settings(self.text_scale, self.quiet, self.pi_font);
                println!("notebook: pi {}", if self.quiet { "quiet" } else { "auto" });
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
                save_settings(self.text_scale, self.quiet, self.pi_font);
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

    /// The small hamburger mark in the corner (notebook pages only — the
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
        self.agent_page = None;
        self.anim.clear();
        self.anim_settle = None;
        self.anim_dirty = None;
        self.cur_stroke = None;
        self.idle_at = None;
        self.indicator_until = None;
        self.lib_view = Some(LibView::List { items: library::scan() });
        self.render_library(true);
        println!("notebook: library opened");
    }

    fn close_library(&mut self) {
        self.lib_view = None;
        self.nb.page.render_full(&mut self.fb);
        self.disp.full_refresh();
        self.draw_menu_icon();
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
        self.lib_view = None;
        self.anim.clear();
        self.anim_settle = None; /* model strokes reappear via render_full later */
        self.anim_dirty = None;
        self.cur_stroke = None;
        self.idle_at = None;
        self.indicator_until = None; /* the full repaint below wipes it */
        self.agent_page = Some(AgentPage { ink: Page::default(), changed: false, waiting: false });
        self.render_agent_page(true);
        println!("notebook: AGENT.md page opened");
    }

    fn close_agent_page(&mut self) {
        self.agent_page = None;
        self.cur_stroke = None;
        self.nb.page.render_full(&mut self.fb);
        self.disp.full_refresh();
        self.draw_menu_icon();
        if self.streaming {
            self.working = false;
            self.set_working(true);
        }
        /* a pending notebook change resumes its pause countdown */
        if self.page_changed {
            self.idle_at = Some(Instant::now() + IDLE_DELAY);
        }
        self.show_page_indicator();
        println!("notebook: AGENT.md page closed");
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
             notebook_draw for this."
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
                println!("notebook: AGENT.md annotations sent to pi");
            }
            Err(e) => println!("notebook: agent feedback send failed: {e}"),
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
    }

    /* -- page turning -- */

    fn flip(&mut self, delta: i32) {
        /* the library owns swipes while open (item pages / back) */
        if self.lib_view.is_some() {
            self.lib_flip(delta);
            return;
        }
        /* on the AGENT.md page: forward returns to the notebook, further
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
        if !self.nb.flip(delta) {
            self.show_page_indicator(); /* at the edge: just show where we are */
            return;
        }
        self.cur_stroke = None;
        self.page_changed = false;
        self.idle_at = None;
        self.nb.page.render_full(&mut self.fb);
        self.disp.full_refresh(); /* the page-turn flash doubles as deghost */
        self.working = false; /* dot was flashed away; redraw if still busy */
        if self.streaming {
            self.set_working(true);
        }
        self.draw_menu_icon();
        self.show_page_indicator();
        self.live.page(self.nb.current);
        println!("notebook: page {} / {}", self.nb.current + 1, self.nb.count);
    }

    /* -- pen -- */

    fn pen_point(&mut self, phase: PenPhase, x: i32, y: i32, pressure: i32, rubber: bool) {
        self.last_pen = Some(Instant::now());
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
                    if self.agent_page.is_none() {
                        self.erase_pass(x as f32, y as f32);
                    } /* no eraser on the AGENT.md page — annotate instead */
                } else {
                    self.ink_pass(phase, x, y, pressure);
                }
            }
            PenPhase::Release => {
                self.last_contact = Some(Instant::now());
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
    /// the notebook page, or the AGENT.md page's annotation layer.
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
                self.cur_stroke = Some(Stroke { pts: vec![p], gray: ink::USER_GRAY });
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

    fn erase_pass(&mut self, x: f32, y: f32) {
        if let Some(gone) = self.nb.page.erase_at(x, y, ERASER_R) {
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
            let had_gray = self.nb.page.render_region(&mut self.fb, r);
            self.disp.update(r.x0, r.y0, r.w(), r.h(), if had_gray { Wave::Text } else { Wave::Ink });
            self.restore_chrome_over(r);
        }
    }

    /* -- touch: page flips, CLOSE -- */

    fn touch(&mut self, phase: Phase, x: i32, y: i32) {
        if self.last_pen.is_some_and(|t| t.elapsed() < PEN_TIMEOUT) {
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
        let (w, h, gray) = self.nb.page.snapshot(SNAP_DIV);
        let patches = patch_summary(&self.nb.page);
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
                println!("notebook: page {} sent to pi", self.nb.current + 1);
            }
            Err(e) => println!("notebook: send failed: {e}"),
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
            PiEvent::Notice(n) => println!("notebook: pi {n}"),
            PiEvent::End => {
                self.streaming = false;
                self.set_working(false);
                self.live.status("idle");
                self.pi_alive_at = None;
                let t: String = self.reply_buf.trim().chars().take(300).collect();
                if !t.is_empty() {
                    println!("notebook: pi said: {t}");
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
                println!("notebook: pi exited: {reason}; respawning in {}s", PI_RESPAWN_DELAY.as_secs());
            }
        }
    }

    /* -- the tool socket -- */

    fn handle_ipc_request(&mut self, req: &Value) -> Value {
        if self.streaming {
            self.pi_alive_at = Some(Instant::now()); /* tool call = alive */
        }
        match req["cmd"].as_str().unwrap_or("") {
            "view" => self.ipc_view(req),
            "draw" => self.ipc_draw(req),
            "erase" => self.ipc_erase(req),
            "goto" => self.ipc_goto(req),
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
            return json!({ "ok": false, "error": format!("no page {} (notebook has {})", idx + 1, self.nb.count) });
        }
        let (snap, patches) = if idx == self.nb.current {
            (self.nb.page.snapshot(SNAP_DIV), patch_list(&self.nb.page))
        } else {
            match Page::load(&self.nb.page_path(idx)) {
                Some(p) => (p.snapshot(SNAP_DIV), patch_list(&p)),
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
        })
    }

    fn ipc_draw(&mut self, req: &Value) -> Value {
        let Some(svg) = req["svg"].as_str() else {
            return json!({ "ok": false, "error": "missing 'svg'" });
        };
        let (strokes, notes) = match svg_ink::parse(svg, self.text_scale) {
            Ok(v) => v,
            Err(e) => return json!({ "ok": false, "error": e }),
        };
        for n in &notes {
            println!("notebook: draw note: {n}");
        }
        let idx = self.req_page(req);
        if idx == self.nb.current {
            let id = self.nb.page.add_patch(strokes);
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
            println!("notebook: patch #{id} on page {} ({n_strokes} strokes)", idx + 1);
            json!({
                "ok": true, "id": id, "page": idx + 1,
                "bbox": bbox.map(|b| json!([b.x0, b.y0, b.x1, b.y1])).unwrap_or(json!(null)),
                "layout": layout_hints(&self.nb.page, self.text_scale),
                "notes": notes,
            })
        } else {
            /* a page that isn't on screen: mutate its file directly */
            if idx >= self.nb.count {
                return json!({ "ok": false, "error": format!("no page {} (notebook has {})", idx + 1, self.nb.count) });
            }
            let path = self.nb.page_path(idx);
            let Some(mut p) = Page::load(&path) else {
                return json!({ "ok": false, "error": "page file unreadable" });
            };
            let id = p.add_patch(strokes);
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
            match self.nb.page.remove_patch(id) {
                Some(b) => {
                    if self.agent_page.is_none() && self.sidebar.is_none() && self.lib_view.is_none() {
                        let r = region.map_or(b, |r| r.union(b)).pad(4).clamp_screen();
                        let had_gray = self.nb.page.render_region(&mut self.fb, r);
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
                return json!({ "ok": false, "error": format!("no page {} (notebook has {})", idx + 1, self.nb.count) });
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
            return json!({ "ok": false, "error": format!("no page {} (notebook has {})", p, self.nb.count) });
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
        println!("notebook: pi turned to page {}", idx + 1);
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
                        "notebook: pi silent for {}s mid-turn; restarting it",
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
                    match Pi::spawn(&self.sock) {
                        Ok(p) => {
                            self.pi = Some(p);
                            self.pi_respawn_at = None;
                            println!("notebook: pi respawned (session continued)");
                            /* re-send the current page if it has unanswered
                             * ink — the wedged turn swallowed that pause */
                            if !self.nb.page.is_empty() && self.agent_page.is_none() {
                                self.page_changed = true;
                                self.idle_at = Some(Instant::now() + Duration::from_secs(2));
                            }
                        }
                        Err(e) => {
                            println!("notebook: pi respawn failed: {e}; retrying in 30s");
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
        let msg = "notebook sleeps";
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
    println!("notebook: sleeping");
    app.nb.save_current();
    let saved = app.show_sleep_page();
    app.disp.full_refresh();
    std::thread::sleep(Duration::from_millis(800));
    /* flush local changes to the VM while the sleep page settles — sync is
     * event-driven (edit / sleep / wake), not timer-driven, to keep the
     * radio quiet; bounded so a dead network can't stall sleep */
    power::sync_flush(Duration::from_secs(45));
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
            println!("notebook: suspend never happened ({attempts} tries); waking the page");
            break;
        }
        println!("notebook: suspend aborted (EPD discharge timer), retrying");
    }
    println!("notebook: waking");
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
    let (disp, fb) = match Display::open() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("notebook: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let takeover = disp.is_takeover();
    println!(
        "notebook: up, fb={FB_W}x{FB_H} ({})",
        if takeover { "takeover/rm2fb" } else { "windowed/qtfb" }
    );
    install_signal_handlers();

    let sock = sock_path();
    let ipc = IpcServer::open(&sock)
        .map_err(|e| eprintln!("notebook: tool socket: {e} — pi gets no drawing tools"))
        .ok();
    let pi = match Pi::spawn(&sock) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("notebook: could not start pi: {e}");
            None
        }
    };

    let mut pen = Pen::open();
    let direct_pen = pen.is_some();
    if takeover {
        if let Some(p) = pen.as_ref() {
            p.grab();
        }
    }
    let mut touchdev = if takeover {
        touch::TouchDevice::open()
            .map_err(|e| eprintln!("notebook: no touch device ({e}) — page flips disabled"))
            .ok()
    } else {
        None
    };
    let mut powerdev = if takeover {
        power::PowerButton::open()
            .map_err(|e| eprintln!("notebook: no power button ({e})"))
            .ok()
    } else {
        None
    };
    let mut power_grace = Instant::now();

    /* Idle auto-suspend (takeover only — windowed mode leaves it to
     * xochitl). Stock xochitl sleeps after ~10 min idle; we took the power
     * button, so we owe the battery the same courtesy. Tunable via
     * NOTEBOOK_AUTO_SLEEP_MIN (minutes), 0 disables. */
    let auto_sleep_min: u64 = std::env::var("NOTEBOOK_AUTO_SLEEP_MIN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let auto_sleep = (powerdev.is_some() && auto_sleep_min > 0)
        .then(|| Duration::from_secs(auto_sleep_min * 60));
    let mut last_activity = Instant::now();

    let now = Instant::now();
    let (text_scale, quiet, saved_font) = load_settings();
    let pi_font = saved_font.unwrap_or_else(hershey::default_face);
    hershey::set_default_face(pi_font);
    let mut app = App {
        fb,
        disp,
        pi,
        nb: Notebook::open(),
        ipc,
        ink_flush: if takeover { INK_FLUSH_TAKEOVER } else { INK_FLUSH_QTFB },
        cur_stroke: None,
        ink_dirty: None,
        last_ink_flush: now,
        last_pen: None,
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
        deghost_at: None,
        live: live::Live::new(),
    };

    /* first paint */
    app.nb.page.render_full(&mut app.fb);
    app.disp.full_refresh();
    app.draw_menu_icon();
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
                    app.last_pen = Some(Instant::now());
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
                        app.last_pen = Some(Instant::now());
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
                println!("notebook: idle {auto_sleep_min}min -> auto-sleep");
                sleep_cycle(&mut app, p, &mut pen, &mut touchdev);
                power_grace = Instant::now() + Duration::from_secs(3);
                last_activity = Instant::now();
            }
        }
    }

    println!("notebook: exiting");
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
