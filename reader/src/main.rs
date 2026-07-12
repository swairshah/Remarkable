//! reader — a PDF reader with a resident reading companion, on the
//! reMarkable 2.
//!
//! Books are PDFs pre-rendered on the desk side (tools/mkbook.py pushes
//! page rasters + extracted text with word boxes); the tablet flips pages,
//! takes pen scribbles as a vector ink overlay, and hands each pause to a
//! background pi agent that reads along: it can underline a phrase
//! (resolved against the real word boxes — no pixel guessing), write in
//! the margins in plotter-font ink, insert a blank NOTE PAGE after the
//! current page for longer thoughts, read any page's text, or stay silent.
//!
//! Built on notebook's takeover stack (which is collab's): rm2fb +
//! per-update waveforms, raw Wacom / touch / power input, unix-socket
//! tools into pi. Page rasters are dithered to pure black/white by mkbook
//! so the pen's 1-bit DU waveform stays safe everywhere.
//!
//! Module map:
//!   fb/draw/display/qtfb/rm2fb   the display stack (from collab)
//!   pen/touch/power              raw input (from collab)
//!   ink.rs      the ink overlay: user strokes + AI patches, vector-first
//!   book.rs     book bundles: rasters, text, note pages, underlining
//!   png_dec.rs  PNG (gray8) + inflate, dependency-free
//!   svg_ink.rs  pi's SVG -> pen strokes (bezier flattening, Hershey text)
//!   hershey.rs  the single-stroke plotter font
//!   ipc.rs      unix-socket server for pi's reader_* tools
//!   pi_rpc.rs   the pi child process (JSONL RPC)
//!   png.rs      grayscale PNG encoder + base64 (page snapshots)

/* The pixel substrate now lives in the shared libreink-core crate
 * (../../libreink); re-exported here so crate::fb etc. keep resolving.
 * APP is this app's identity in those crates: log prefix + env-var prefix. */
pub const APP: libreink_core::app::AppId =
    libreink_core::app::AppId { name: "reader", env_prefix: "READER" };
pub use libreink_core::{draw, fb, font, png, png_dec};
pub use libreink_display::{display, qtfb, rm2fb};
pub use libreink_input::{palm, pen, power, touch};
pub use libreink_hershey as hershey;
pub use libreink_svg as svg_ink;
pub use libreink_pi::ipc;
pub use libreink_text as text;
pub use libreink_page as ink;

mod book;
mod import;
mod xochitl;
mod pi_rpc;

use book::{Book, Entry};
use display::{Display, Wave};
use draw::{text_width, BLACK, WHITE};
use fb::{Framebuffer, SCREEN_H as FB_H, SCREEN_W as FB_W};
use ink::{Page, Pt, Rect, Stroke};
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

/* page turns render gently (full GL16 over print — smooth greys, no flash;
 * partial GC16 over pure-ink note pages), inherited from Paper; a flashing
 * deghost every Nth turn clears the residue the gentle passes leave */
const FLIP_DEGHOST_EVERY: u32 = 8;

/// How long a writing pause must last before the page goes to pi.
const IDLE_DELAY: Duration = Duration::from_millis(2800);

/// AI ink animation: one flush per tick, `ANIM_BUDGET` px of path per tick.
const ANIM_TICK: Duration = Duration::from_millis(28);
const ANIM_BUDGET: f32 = 48.0;

/// Page snapshots for pi are half scale (702x936).
const SNAP_DIV: i32 = 2;

const ERASER_R: f32 = 22.0;

/* takeover exit: X CLOSE pinned at the bottom of the sidebar / home */
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
    Books,      /* back to the shelf */
    First,
    Last,
    Active,     /* where the last writing/drawing happened */
    GoTo,       /* opens the number pad */
    InsertNote, /* blank note page after the current one */
    Agent,
    FontSize,   /* [-] 100% [+]: zoom on pi's text */
    Refresh,
}
const SB_ROWS: [SbRow; 9] = [
    SbRow::Books,
    SbRow::First,
    SbRow::Last,
    SbRow::Active,
    SbRow::GoTo,
    SbRow::InsertNote,
    SbRow::Agent,
    SbRow::FontSize,
    SbRow::Refresh,
];

/* pi text zoom: multiplies every font-size pi writes (persisted) */
const TEXT_SCALE_MIN: f32 = 0.6;
const TEXT_SCALE_MAX: f32 = 1.8;
const TEXT_SCALE_STEP: f32 = 0.1;

fn settings_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/reader/settings.json")
}

fn load_settings() -> (f32, String) {
    let v = std::fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    let scale = v["text_scale"]
        .as_f64()
        .map(|x| (x as f32).clamp(TEXT_SCALE_MIN, TEXT_SCALE_MAX))
        .unwrap_or(1.0);
    let last = v["last_book"].as_str().unwrap_or("").to_string();
    (scale, last)
}

fn save_settings(scale: f32, last_book: &str) {
    let p = settings_path();
    if let Some(dir) = std::path::Path::new(&p).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let doc = json!({ "text_scale": scale, "last_book": last_book });
    let _ = std::fs::write(&p, serde_json::to_vec(&doc).unwrap_or_default());
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

/* the home screen (book list) */
const HOME_LIST_Y0: i32 = 170;
const HOME_ROW_H: i32 = 128;
const HOME_ROWS: usize = ((FB_H - 40 - HOME_LIST_Y0) / HOME_ROW_H) as usize;

/* the TABLET LIBRARY (import) view: header button + row grid */
const IMP_BTN_X: i32 = FB_W - 300;
const IMP_BTN_Y: i32 = 20;
const IMP_BTN_W: i32 = 240;
const IMP_BTN_H: i32 = 64;
const IMP_ROW_H: i32 = 100;
const IMP_ROWS: usize = ((FB_H - 40 - HOME_LIST_Y0) / IMP_ROW_H) as usize;

/* hold a finger on a book row this long -> the delete confirmation */
const HOLD_MS: u128 = 650;
const DEL_W: i32 = 960;
const DEL_H: i32 = 400;
const DEL_BTN_W: i32 = 380;
const DEL_BTN_H: i32 = 96;

/* the AGENT.md page (reachable from the sidebar) */
const AGENT_TEXT_X: i32 = 70;
const AGENT_TEXT_Y0: i32 = 120;
const AGENT_TEXT_PX: f32 = 36.0;
const AGENT_TEXT_W: i32 = FB_W - 2 * AGENT_TEXT_X;

fn agent_md_path() -> String {
    if let Ok(p) = std::env::var("READER_AGENT_MD") {
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/reader/AGENT.md")
}

/// The standing-instructions page: the file rendered as text, plus the
/// user's not-yet-applied handwritten annotations (never persisted as ink —
/// pi consumes them into the file, then the page re-renders clean).
struct AgentPage {
    ink: Page,     /* annotation strokes only */
    changed: bool, /* unsent annotations exist */
    waiting: bool, /* pi is applying them; reload on End */
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
    std::env::var("READER_SOCK").unwrap_or_else(|_| "/tmp/reader-ctl.sock".into())
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
    page: usize, /* seq index */
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
    ipc: Option<IpcServer>,

    /* the open book, or None = the home screen (book shelf) */
    book: Option<Book>,
    shelf: Vec<book::BookInfo>,
    shelf_top: usize, /* first visible shelf row (vertical swipes page it) */
    confirm_delete: Option<usize>, /* shelf index awaiting DELETE/CANCEL */

    /* on-device import (mutool): the TABLET LIBRARY view + render queue */
    lib_list: Option<(Vec<xochitl::XDoc>, usize)>, /* (docs, top row) */
    importer: Option<import::Importer>,
    import_queue: VecDeque<import::Job>,
    import_shown: (usize, usize),   /* progress last painted */

    ink_flush: Duration,

    /* pen */
    cur_stroke: Option<Stroke>,
    ink_dirty: Option<Rect>,
    last_ink_flush: Instant,
    palm: palm::PalmGuard,
    flips_since_flash: u32, /* gentle turns; flash every FLIP_DEGHOST_EVERY */     /* any pen sign of life (incl. hover) */
    last_contact: Option<Instant>, /* actual glass contact */
    contact_changed: bool,         /* this contact wrote or erased something */

    /* pause trigger */
    page_changed: bool,
    idle_at: Option<Instant>,

    /* pi */
    streaming: bool,
    reply_buf: String,

    /* AI ink animation */
    anim: VecDeque<AnimStroke>,
    anim_dirty: Option<Rect>,
    anim_settle: Option<Rect>, /* union animated; GL16-refined when done */
    last_anim: Instant,

    /* touch gestures */
    touch_start: Option<(i32, i32)>,
    touch_t0: Option<Instant>, /* when the current touch began (long-press) */
    touch_last: (i32, i32),

    /* transient chrome */
    indicator_until: Option<Instant>,
    working: bool,

    /* the AGENT.md page, when open (replaces the book view) */
    agent_page: Option<AgentPage>,

    /* the sidebar, when showing */
    sidebar: Option<Sb>,

    /* the seq index that saw the most recent ink activity (user or pi) */
    last_activity_page: usize,

    /* zoom applied to every font-size pi writes (sidebar [-]/[+]) */
    text_scale: f32,

    /* pending deghost flash after rubber erasing (DU-erase leaves ghosts) */
    deghost_at: Option<Instant>,

    /* pen ink lands via DU; on grayscale page rasters a GL16 pass over the
     * fresh strokes ~0.8s after the pen settles heals the print around
     * them (and smooths the strokes) */
    ink_settle: Option<Rect>,
    ink_settle_at: Option<Instant>,
}

impl App {
    fn on_home(&self) -> bool {
        self.book.is_none()
    }

    /* -- small chrome (drawn over the page, re-rendered away later) -- */

    fn indicator_rect(&self) -> Rect {
        Rect { x0: FB_W / 2 - 160, y0: FB_H - 56, x1: FB_W / 2 + 160, y1: FB_H - 10 }
    }

    fn show_page_indicator(&mut self) {
        let Some(b) = &self.book else { return };
        let label = format!("{} / {}  ·  {}", b.current + 1, b.count(), b.label(b.current));
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
        self.render_page_region(r);
    }

    /// Repaint a region of whatever "document" view is underneath the
    /// chrome (book page or home list). Over a grayscale page raster the
    /// 16-level waveform keeps the print clean; plain ink pages take DU.
    fn render_page_region(&mut self, r: Rect) {
        self.render_page_region_wave(r, None);
    }

    /// Same, with a forced waveform — the rubber path passes Wave::Ink
    /// because GL16 is far too slow mid-scrub (the post-erase deghost
    /// flash heals the print afterwards).
    fn render_page_region_wave(&mut self, r: Rect, wave: Option<Wave>) {
        let r = r.clamp_screen();
        let had_gray = match &self.book {
            Some(b) => b.render_region(&mut self.fb, r),
            None => {
                /* home: cheap and correct — repaint the whole list */
                self.render_home(false);
                return;
            }
        };
        let w = wave.unwrap_or(if had_gray { Wave::Text } else { Wave::Ink });
        self.disp.update(r.x0, r.y0, r.w(), r.h(), w);
    }

    fn draw_working_dot(&mut self) {
        let r = DOT_RECT;
        let (cx, cy) = ((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2);
        self.fb.disc(cx, cy, 8, draw::GRAY);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
    }

    fn clear_working_dot(&mut self) {
        let r = DOT_RECT;
        if self.agent_page.is_some() || self.on_home() {
            /* blank margin on these views */
            self.fb.fill_rect(r.x0, r.y0, r.w(), r.h(), WHITE);
            self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
            return;
        }
        self.render_page_region(r);
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

    /// The X CLOSE bar (bottom of the sidebar and of the home shelf).
    fn close_bar_rect() -> (i32, i32, i32, i32) {
        let cy = FB_H - SB_CLOSE_H - SB_CLOSE_MARGIN;
        (SB_CLOSE_MARGIN, cy, SB_W - 2 * SB_CLOSE_MARGIN, SB_CLOSE_H)
    }

    fn draw_close_bar(&mut self) {
        let (x, y, w, h) = Self::close_bar_rect();
        self.fb.fill_rect(x, y, w, h, BLACK);
        let label = "X CLOSE";
        self.fb.text(
            x + (w - text_width(label, 3)) / 2,
            y + (h - 21) / 2,
            label,
            3,
            WHITE,
        );
    }

    /* -- the home screen (book shelf) -- */

    fn go_home(&mut self) {
        if let Some(b) = self.book.as_mut() {
            b.save_all();
        }
        self.book = None;
        self.sidebar = None;
        self.agent_page = None;
        self.anim.clear();
        self.anim_dirty = None;
        self.anim_settle = None;
        self.cur_stroke = None;
        self.idle_at = None;
        self.page_changed = false;
        self.indicator_until = None;
        self.ink_settle = None;
        self.ink_settle_at = None;
        self.confirm_delete = None;
        self.lib_list = None;
        save_settings(self.text_scale, "");
        self.shelf = book::scan();
        self.shelf_top = 0;
        self.render_home(true);
        println!("reader: home ({} books)", self.shelf.len());
    }

    fn render_home(&mut self, flash: bool) {
        self.fb.fill_rect(0, 0, FB_W, FB_H, WHITE);
        text::draw_line(&mut self.fb, 70, 40, text::Face::Heading, 52.0, "READER");
        let sub = if self.shelf.len() > HOME_ROWS {
            format!(
                "books {}-{} of {} - swipe up/down - tap opens, hold deletes",
                self.shelf_top + 1,
                (self.shelf_top + HOME_ROWS).min(self.shelf.len()),
                self.shelf.len()
            )
        } else {
            "tap a book to open it - hold to delete".to_string()
        };
        self.fb.text(72, 108, &sub, 2, draw::GRAY);
        self.fb.fill_rect(0, 140, FB_W, 2, BLACK);
        self.draw_corner_button("IMPORT +");
        self.draw_import_status();
        self.draw_close_bar();
        if self.shelf.is_empty() {
            text::draw_line(
                &mut self.fb,
                70,
                HOME_LIST_Y0 + 30,
                text::Face::Body,
                34.0,
                "No books yet. From your computer:",
            );
            text::draw_line(
                &mut self.fb,
                70,
                HOME_LIST_Y0 + 90,
                text::Face::Body,
                30.0,
                "make book FILE=paper.pdf HOST=root@<tablet-ip>",
            );
        }
        let shelf_rows: Vec<(String, String)> = self
            .shelf
            .iter()
            .skip(self.shelf_top)
            .take(HOME_ROWS)
            .map(|b| {
                let meta = if b.pos > 0 {
                    format!("{} pages  -  at page {} of {}", b.pages, b.pos + 1, b.seq_len)
                } else {
                    format!("{} pages", b.pages)
                };
                (b.title.clone(), meta)
            })
            .collect();
        for (i, (title, meta)) in shelf_rows.iter().enumerate() {
            let y = HOME_LIST_Y0 + i as i32 * HOME_ROW_H;
            let mut t = title.clone();
            while text::width(text::Face::Heading, 42.0, &t) > FB_W - 140 && t.chars().count() > 4 {
                t = t.chars().take(t.chars().count() - 4).collect();
                t.push('.');
                t.push('.');
            }
            text::draw_line(&mut self.fb, 70, y, text::Face::Heading, 42.0, &t);
            self.fb.text(70, y + 64, meta, 2, draw::GRAY);
            self.fb.fill_rect(70, y + HOME_ROW_H - 16, FB_W - 140, 1, draw::LIGHT);
        }
        if self.working {
            let (cx, cy) = ((DOT_RECT.x0 + DOT_RECT.x1) / 2, (DOT_RECT.y0 + DOT_RECT.y1) / 2);
            self.fb.disc(cx, cy, 8, draw::GRAY);
        }
        if flash {
            self.disp.full_refresh();
        } else {
            self.disp.update(0, 0, FB_W, FB_H, Wave::Text);
        }
        if self.confirm_delete.is_some() {
            self.render_delete_confirm(); /* incidental repaints keep it up */
        }
    }

    /// Which shelf row (absolute index) sits under a home-screen press.
    fn home_row_at(&self, y: i32) -> Option<usize> {
        let row = (y - HOME_LIST_Y0) / HOME_ROW_H;
        if row < 0 || row as usize >= HOME_ROWS {
            return None;
        }
        let idx = self.shelf_top + row as usize;
        (idx < self.shelf.len()).then_some(idx)
    }

    /// A press on the home screen (pen press or finger tap).
    fn home_press(&mut self, x: i32, y: i32) {
        if in_rect(x, y, IMP_BTN_X, IMP_BTN_Y, IMP_BTN_W, IMP_BTN_H) {
            self.open_lib_list();
            return;
        }
        let (cx, cy, cw, ch) = Self::close_bar_rect();
        if in_rect(x, y, cx, cy, cw, ch) {
            println!("reader: close (home)");
            RUNNING.store(false, Ordering::Relaxed);
            return;
        }
        if let Some(idx) = self.home_row_at(y) {
            let slug = self.shelf[idx].slug.clone();
            self.open_book(&slug);
        }
    }

    /// A long finger press on a book row: ask before deleting.
    fn home_hold(&mut self, _x: i32, y: i32) {
        if let Some(idx) = self.home_row_at(y) {
            self.confirm_delete = Some(idx);
            self.render_delete_confirm();
        }
    }

    fn delete_rects(&self) -> (Rect, Rect, Rect) {
        let x0 = (FB_W - DEL_W) / 2;
        let y0 = (FB_H - DEL_H) / 2;
        let boxr = Rect { x0, y0, x1: x0 + DEL_W - 1, y1: y0 + DEL_H - 1 };
        let by = y0 + DEL_H - DEL_BTN_H - 40;
        let cancel = Rect { x0: x0 + 60, y0: by, x1: x0 + 60 + DEL_BTN_W - 1, y1: by + DEL_BTN_H - 1 };
        let del = Rect {
            x0: x0 + DEL_W - 60 - DEL_BTN_W,
            y0: by,
            x1: x0 + DEL_W - 60 - 1,
            y1: by + DEL_BTN_H - 1,
        };
        (boxr, cancel, del)
    }

    fn render_delete_confirm(&mut self) {
        let Some(idx) = self.confirm_delete else { return };
        let Some(info) = self.shelf.get(idx) else { return };
        let mut title = info.title.clone();
        let (boxr, cancel, del) = self.delete_rects();
        self.fb.fill_rect(boxr.x0, boxr.y0, boxr.w(), boxr.h(), WHITE);
        self.fb.rect_outline(boxr.x0, boxr.y0, boxr.w(), boxr.h(), 4, BLACK);
        while text::width(text::Face::Heading, 40.0, &title) > DEL_W - 120 && title.chars().count() > 4 {
            title = title.chars().take(title.chars().count() - 4).collect();
            title.push('.');
            title.push('.');
        }
        text::draw_line(&mut self.fb, boxr.x0 + 60, boxr.y0 + 44, text::Face::Heading, 40.0, &title);
        self.fb.text(
            boxr.x0 + 60,
            boxr.y0 + 130,
            "delete this book from the reader?",
            3,
            BLACK,
        );
        self.fb.text(
            boxr.x0 + 60,
            boxr.y0 + 176,
            "removes its pages, your ink, pi's notes and the",
            2,
            draw::GRAY,
        );
        self.fb.text(boxr.x0 + 60, boxr.y0 + 206, "reading position - for good.", 2, draw::GRAY);
        self.fb.rect_outline(cancel.x0, cancel.y0, cancel.w(), cancel.h(), 3, BLACK);
        self.fb.text(
            cancel.x0 + (cancel.w() - text_width("CANCEL", 3)) / 2,
            cancel.y0 + (cancel.h() - 21) / 2,
            "CANCEL",
            3,
            BLACK,
        );
        self.fb.fill_rect(del.x0, del.y0, del.w(), del.h(), BLACK);
        self.fb.text(
            del.x0 + (del.w() - text_width("DELETE", 3)) / 2,
            del.y0 + (del.h() - 21) / 2,
            "DELETE",
            3,
            WHITE,
        );
        self.disp.update(boxr.x0, boxr.y0, boxr.w(), boxr.h(), Wave::Ink);
    }

    /// A press while the delete confirmation is up. Always consumes it —
    /// only a hit on DELETE deletes; CANCEL and everywhere else dismiss.
    fn delete_confirm_press(&mut self, x: i32, y: i32) {
        let (_boxr, _cancel, del) = self.delete_rects();
        if in_rect(x, y, del.x0, del.y0, del.w(), del.h()) {
            if let Some(idx) = self.confirm_delete.take() {
                if let Some(info) = self.shelf.get(idx) {
                    let slug = info.slug.clone();
                    let dir = format!("{}/{}", book::books_dir(), slug);
                    match std::fs::remove_dir_all(&dir) {
                        Ok(()) => println!("reader: deleted book '{slug}'"),
                        Err(e) => println!("reader: delete '{slug}': {e}"),
                    }
                }
                self.shelf = book::scan();
                let max_top = self.shelf.len().saturating_sub(HOME_ROWS);
                self.shelf_top = self.shelf_top.min(max_top);
            }
            self.render_home(true); /* flash: the dialog + row must ghost away */
            return;
        }
        /* CANCEL or anywhere else: dismiss */
        self.confirm_delete = None;
        self.render_home(false);
    }

    /// Vertical swipe on the shelf: page through the book list.
    fn shelf_scroll(&mut self, up: bool) {
        let max_top = self.shelf.len().saturating_sub(HOME_ROWS);
        let new_top = if up {
            (self.shelf_top + HOME_ROWS).min(max_top)
        } else {
            self.shelf_top.saturating_sub(HOME_ROWS)
        };
        if new_top != self.shelf_top {
            self.shelf_top = new_top;
            self.render_home(false);
        }
    }

    /* -- the TABLET LIBRARY view: import from xochitl, on the device -- */

    fn open_lib_list(&mut self) {
        self.lib_list = Some((xochitl::scan_pdfs(), 0));
        self.render_lib_list(false);
    }

    fn close_lib_list(&mut self) {
        self.lib_list = None;
        self.render_home(false);
    }

    /// Is this xochitl doc already a book / being imported / queued?
    fn import_state_of(&self, name: &str) -> Option<&'static str> {
        let slug = xochitl::slugify(name);
        if self.shelf.iter().any(|b| b.slug == slug) {
            return Some("on the shelf");
        }
        if self.importer.as_ref().is_some_and(|i| i.job.slug == slug) {
            return Some("importing...");
        }
        if self.import_queue.iter().any(|j| j.slug == slug) {
            return Some("queued");
        }
        None
    }

    fn render_lib_list(&mut self, flash: bool) {
        let Some((docs, top)) = self.lib_list.as_ref() else { return };
        let (top, total) = (*top, docs.len());
        let rows: Vec<(String, String)> = docs
            .iter()
            .skip(top)
            .take(IMP_ROWS)
            .map(|d| {
                let mark = self
                    .import_state_of(&d.name)
                    .map(|m| format!("  -  {m}"))
                    .unwrap_or_default();
                let mb = (d.kb as f32 / 1024.0).max(0.1);
                (d.name.clone(), format!("{mb:.1} MB{mark}"))
            })
            .collect();
        self.fb.fill_rect(0, 0, FB_W, FB_H, WHITE);
        text::draw_line(&mut self.fb, 70, 40, text::Face::Heading, 52.0, "TABLET LIBRARY");
        let sub = if import::available() {
            format!(
                "pdfs {}-{} of {} on this tablet - tap to import - swipe up/down",
                top + 1,
                (top + IMP_ROWS).min(total),
                total
            )
        } else {
            "mutool missing - run: make deploy-mutool from your computer".to_string()
        };
        self.fb.text(72, 108, &sub, 2, draw::GRAY);
        self.fb.fill_rect(0, 140, FB_W, 2, BLACK);
        self.draw_corner_button("< SHELF");
        for (i, (name, meta)) in rows.iter().enumerate() {
            let y = HOME_LIST_Y0 + i as i32 * IMP_ROW_H;
            let mut t = name.clone();
            while text::width(text::Face::Body, 36.0, &t) > FB_W - 140 && t.chars().count() > 4 {
                t = t.chars().take(t.chars().count() - 4).collect();
                t.push('.');
                t.push('.');
            }
            text::draw_line(&mut self.fb, 70, y, text::Face::Body, 36.0, &t);
            self.fb.text(70, y + 52, meta, 2, draw::GRAY);
            self.fb.fill_rect(70, y + IMP_ROW_H - 14, FB_W - 140, 1, draw::LIGHT);
        }
        self.draw_import_status();
        if flash {
            self.disp.full_refresh();
        } else {
            self.disp.update(0, 0, FB_W, FB_H, Wave::Text);
        }
    }

    fn lib_list_press(&mut self, x: i32, y: i32) {
        if in_rect(x, y, IMP_BTN_X, IMP_BTN_Y, IMP_BTN_W, IMP_BTN_H) {
            self.close_lib_list();
            return;
        }
        let row = (y - HOME_LIST_Y0) / IMP_ROW_H;
        if row < 0 || row as usize >= IMP_ROWS {
            return;
        }
        let Some((docs, top)) = self.lib_list.as_ref() else { return };
        let idx = *top + row as usize;
        let Some(doc) = docs.get(idx) else { return };
        if self.import_state_of(&doc.name).is_some() {
            return; /* already there / in flight */
        }
        if !import::available() {
            println!("reader: import refused — mutool not installed");
            return;
        }
        let job = import::Job {
            uuid: doc.uuid.clone(),
            title: doc.name.trim_end_matches(".pdf").to_string(),
            slug: xochitl::slugify(&doc.name),
        };
        self.enqueue_import(job);
        self.render_lib_list(false); /* row now shows queued/importing */
    }

    fn lib_list_scroll(&mut self, up: bool) {
        let Some((docs, top)) = self.lib_list.as_mut() else { return };
        let max_top = docs.len().saturating_sub(IMP_ROWS);
        let new_top = if up { (*top + IMP_ROWS).min(max_top) } else { top.saturating_sub(IMP_ROWS) };
        if new_top != *top {
            *top = new_top;
            self.render_lib_list(false);
        }
    }

    fn enqueue_import(&mut self, job: import::Job) {
        println!("reader: import queued: '{}' -> {}", job.title, job.slug);
        if self.importer.is_none() {
            match import::Importer::start(job, &book::books_dir()) {
                Ok(imp) => self.importer = Some(imp),
                Err(e) => println!("reader: import start failed: {e}"),
            }
        } else {
            self.import_queue.push_back(job);
        }
    }

    /// Drive the active import forward; start the next queued one when it
    /// finishes. Called every loop iteration (cheap when idle).
    fn tick_import(&mut self) {
        let Some(imp) = self.importer.as_mut() else { return };
        match imp.poll() {
            import::Tick::Working(done, total) => {
                /* repaint the status line at most every ~2s of progress */
                if (done != self.import_shown.0 || total != self.import_shown.1)
                    && done % 5 == 0
                    && self.sidebar.is_none()
                    && self.agent_page.is_none()
                {
                    self.import_shown = (done, total);
                    if self.on_home() {
                        self.paint_import_status_band();
                    }
                }
            }
            import::Tick::Finished(slug) => {
                println!("reader: import finished: {slug}");
                self.importer = None;
                self.import_shown = (0, 0);
                self.shelf = book::scan();
                if let Some(job) = self.import_queue.pop_front() {
                    match import::Importer::start(job, &book::books_dir()) {
                        Ok(imp) => self.importer = Some(imp),
                        Err(e) => println!("reader: import start failed: {e}"),
                    }
                }
                if self.on_home() && self.sidebar.is_none() && self.agent_page.is_none() {
                    if self.lib_list.is_some() {
                        self.render_lib_list(false);
                    } else {
                        self.render_home(false);
                    }
                }
            }
            import::Tick::Failed(msg) => {
                println!("reader: {msg}");
                self.importer = None;
                self.import_shown = (0, 0);
                if let Some(job) = self.import_queue.pop_front() {
                    match import::Importer::start(job, &book::books_dir()) {
                        Ok(imp) => self.importer = Some(imp),
                        Err(e) => println!("reader: import start failed: {e}"),
                    }
                }
            }
        }
    }

    /// One line of import progress, drawn just under the header rule on
    /// the home/library views.
    fn draw_import_status(&mut self) {
        let line = match (&self.importer, self.import_queue.len()) {
            (Some(imp), q) => {
                let (done, total) = self.import_shown;
                let more = if q > 0 { format!(" (+{q} queued)") } else { String::new() };
                let mut t = imp.job.title.clone();
                t.truncate(40);
                format!("importing '{}' - page {}/{}{}", t, done.max(1), total.max(1), more)
            }
            (None, 0) => return,
            (None, q) => format!("{q} imports queued"),
        };
        self.fb.text(72, 146, &line, 2, BLACK);
    }

    fn paint_import_status_band(&mut self) {
        self.fb.fill_rect(0, 144, FB_W, 24, WHITE);
        self.draw_import_status();
        self.disp.update(0, 144, FB_W, 24, Wave::Ink);
    }

    /// The corner button on home/library views (IMPORT + / < SHELF).
    fn draw_corner_button(&mut self, label: &str) {
        self.fb.rect_outline(IMP_BTN_X, IMP_BTN_Y, IMP_BTN_W, IMP_BTN_H, 3, BLACK);
        self.fb.text(
            IMP_BTN_X + (IMP_BTN_W - text_width(label, 3)) / 2,
            IMP_BTN_Y + (IMP_BTN_H - 21) / 2,
            label,
            3,
            BLACK,
        );
    }

    fn open_book(&mut self, slug: &str) {
        match Book::open(slug) {
            Some(b) => {
                println!("reader: opened '{}' ({} pdf pages, {} entries, at {})",
                    b.title, b.pdf_pages, b.count(), b.current + 1);
                self.last_activity_page = b.current;
                self.book = Some(b);
                save_settings(self.text_scale, slug);
                self.page_changed = false;
                self.idle_at = None;
                self.render_book_full(true); /* clean GC16 on open */
                self.show_page_indicator();
            }
            None => println!("reader: could not open book '{slug}'"),
        }
    }

    /// Full repaint of the current book page + chrome. `flash` = the GC16
    /// deghost blink; page turns instead ease over with GL16 (print pages —
    /// full-frame so the greys stay smooth) or partial GC16 (pure-ink note
    /// pages), exactly like Paper.
    fn render_book_full(&mut self, flash: bool) {
        self.ink_settle = None; /* the whole-page repaint supersedes any settle */
        self.ink_settle_at = None;
        let has_raster = match &self.book {
            Some(b) => b.render_region(
                &mut self.fb,
                Rect { x0: 0, y0: 0, x1: FB_W - 1, y1: FB_H - 1 },
            ),
            None => false,
        };
        if flash {
            self.disp.full_refresh();
        } else {
            let wave = if has_raster { Wave::Print } else { Wave::Page };
            self.disp.update(0, 0, FB_W, FB_H, wave);
        }
        self.working = false; /* the repaint wiped the dot; redraw if busy */
        if self.streaming {
            self.set_working(true);
        }
        self.draw_menu_icon();
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
        let Some(b) = &self.book else { return };
        let (cur, count, active, title) =
            (b.current, b.count(), self.last_activity_page, b.title.clone());
        self.fb.fill_rect(0, 0, SB_W, FB_H, WHITE);
        self.fb.fill_rect(SB_W - 3, 0, 3, FB_H, BLACK);
        self.fb.text(28, 24, "READER", 4, BLACK);
        let mut sub = title;
        sub.truncate(26);
        self.fb.text(28, 62, &format!("{sub} - {} / {count}", cur + 1), 2, draw::GRAY);
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
                let on_page = |p: usize| cur == p && self.agent_page.is_none();
                let (label, current) = match row {
                    SbRow::Books => ("ALL BOOKS".to_string(), false),
                    SbRow::First => ("FIRST PAGE".to_string(), on_page(0)),
                    SbRow::Last => (format!("LAST PAGE ({count})"), on_page(count - 1)),
                    SbRow::Active => (format!("ACTIVE PAGE ({})", active + 1), false),
                    SbRow::GoTo => ("GO TO PAGE...".to_string(), false),
                    SbRow::InsertNote => ("+ NOTE PAGE HERE".to_string(), false),
                    SbRow::Agent => ("INSTRUCTIONS".to_string(), self.agent_page.is_some()),
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
            self.draw_close_bar();
        }
        self.disp.update(0, 0, SB_W, FB_H, Wave::Ink);
    }

    /// Hide the panel and repaint what it covered.
    fn hide_sidebar(&mut self) {
        self.sidebar = None;
        if self.agent_page.is_some() {
            self.render_agent_page(false);
            return;
        }
        let r = Rect { x0: 0, y0: 0, x1: SB_W - 1, y1: FB_H - 1 };
        self.render_page_region(r);
        self.draw_menu_icon();
        self.restore_chrome_over(r);
    }

    /// Leave any menu view and land on seq index `p` (0-based).
    fn jump_to_page(&mut self, p: usize) {
        self.sidebar = None;
        if self.agent_page.is_some() {
            self.agent_page = None;
        }
        let Some(b) = self.book.as_mut() else { return };
        let p = p.min(b.count() - 1);
        let delta = p as i32 - b.current as i32;
        if delta != 0 {
            self.flip(delta);
        } else {
            self.render_book_full(true);
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
        let (cx, cy, cw, ch) = Self::close_bar_rect();
        if in_rect(x, y, cx, cy, cw, ch) {
            println!("reader: close (sidebar)");
            RUNNING.store(false, Ordering::Relaxed);
            return;
        }
        let idx = (y - SB_LIST_Y0) / SB_ROW_H;
        let Some(row) = (idx >= 0).then(|| SB_ROWS.get(idx as usize)).flatten().copied() else {
            return; /* header / dead space: keep the panel up */
        };
        let count = self.book.as_ref().map_or(1, |b| b.count());
        match row {
            SbRow::Books => self.go_home(),
            SbRow::First => self.jump_to_page(0),
            SbRow::Last => self.jump_to_page(count - 1),
            SbRow::Active => self.jump_to_page(self.last_activity_page),
            SbRow::GoTo => {
                if let Some(sb) = self.sidebar.as_mut() {
                    sb.numpad = true;
                    sb.entry.clear();
                }
                self.paint_sidebar();
            }
            SbRow::InsertNote => {
                let target = self.book.as_mut().map(|b| {
                    let at = b.insert_note(b.current);
                    println!("reader: note page inserted at {}", at + 1);
                    at
                });
                if let Some(at) = target {
                    self.jump_to_page(at);
                }
            }
            SbRow::Agent => {
                self.sidebar = None;
                if self.agent_page.is_none() {
                    self.open_agent_page();
                } else {
                    self.render_agent_page(false);
                }
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
                let last = self.book.as_ref().map(|b| b.slug.clone()).unwrap_or_default();
                save_settings(self.text_scale, &last);
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

    /// The small hamburger mark in the corner (book pages only).
    fn draw_menu_icon(&mut self) {
        if self.on_home() || self.agent_page.is_some() || self.sidebar.is_some() {
            return;
        }
        for i in 0..3 {
            self.fb.fill_rect(18, 22 + i * 10, 30, 4, draw::GRAY);
        }
        self.disp.update(14, 14, 44, 44, Wave::Ink);
    }

    /* -- the AGENT.md page -- */

    fn open_agent_page(&mut self) {
        self.anim.clear();
        self.anim_settle = None; /* model strokes reappear via render_full later */
        self.anim_dirty = None;
        self.cur_stroke = None;
        self.idle_at = None;
        self.indicator_until = None; /* the full repaint below wipes it */
        self.agent_page = Some(AgentPage { ink: Page::default(), changed: false, waiting: false });
        self.render_agent_page(true);
        println!("reader: AGENT.md page opened");
    }

    fn close_agent_page(&mut self) {
        self.agent_page = None;
        self.cur_stroke = None;
        if self.on_home() {
            self.render_home(true);
            return;
        }
        self.render_book_full(true);
        /* a pending page change resumes its pause countdown */
        if self.page_changed {
            self.idle_at = Some(Instant::now() + IDLE_DELAY);
        }
        self.show_page_indicator();
        println!("reader: AGENT.md page closed");
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
            self.disp.update(0, 0, FB_W, FB_H, Wave::Text);
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
             reader_draw for this."
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
                println!("reader: AGENT.md annotations sent to pi");
            }
            Err(e) => println!("reader: agent feedback send failed: {e}"),
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
        /* on the AGENT.md page: forward returns to the book, further back
         * does nothing */
        if self.agent_page.is_some() {
            if delta > 0 {
                self.close_agent_page();
            }
            return;
        }
        if self.on_home() {
            return;
        }
        /* pending animation strokes are already in the model; the full
         * repaint below shows them instantly on whatever page has them */
        self.anim.clear();
        self.anim_settle = None;
        self.anim_dirty = None;
        let flipped = self.book.as_mut().map(|b| b.flip(delta)).unwrap_or(false);
        if !flipped {
            self.show_page_indicator(); /* at the edge: just show where we are */
            return;
        }
        self.cur_stroke = None;
        self.page_changed = false;
        self.idle_at = None;
        self.flips_since_flash += 1;
        let flash = self.flips_since_flash >= FLIP_DEGHOST_EVERY;
        if flash {
            self.flips_since_flash = 0;
        }
        self.render_book_full(flash);
        self.show_page_indicator();
        if let Some(b) = &self.book {
            println!("reader: page {} / {} ({})", b.current + 1, b.count(), b.label(b.current));
        }
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
        if self.on_home() {
            if phase == PenPhase::Press {
                if self.confirm_delete.is_some() {
                    self.delete_confirm_press(x, y);
                } else if self.lib_list.is_some() {
                    self.lib_list_press(x, y);
                } else {
                    self.home_press(x, y);
                }
            }
            return;
        }
        if phase == PenPhase::Press && x < MENU_HOT && y < MENU_HOT {
            self.show_sidebar();
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
    /// the book page's overlay, or the AGENT.md page's annotation layer.
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
                if let Some(b) = self.book.as_mut() {
                    b.page.strokes.push(s);
                    b.page.dirty = true;
                    self.contact_changed = true;
                    self.last_activity_page = b.current;
                }
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
                p
            }
        };
        ink::stamp_segment(&mut self.fb, prev, p, ink::USER_GRAY);
        self.mark_ink_dirty(prev, p);
        /* writing over print: remember where, heal with GL16 once settled */
        if self.agent_page.is_none() && self.book.as_ref().is_some_and(|b| b.has_raster()) {
            let r = Rect {
                x0: (prev.x.min(p.x) - 6.0) as i32,
                y0: (prev.y.min(p.y) - 6.0) as i32,
                x1: (prev.x.max(p.x) + 6.0).ceil() as i32,
                y1: (prev.y.max(p.y) + 6.0).ceil() as i32,
            };
            self.ink_settle = Some(match self.ink_settle {
                None => r,
                Some(s) => s.union(r),
            });
            self.ink_settle_at = Some(Instant::now() + Duration::from_millis(800));
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
        let Some(b) = self.book.as_mut() else { return };
        if let Some((gone, _)) = b.page.erase_at(x, y, ERASER_R) {
            self.contact_changed = true;
            self.last_activity_page = b.current;
            /* DU-erased black ink ghosts badly; flash once the scrubbing
             * settles */
            self.deghost_at = Some(Instant::now() + Duration::from_millis(1100));
            /* un-animated strokes in the region must appear now that we
             * repaint from the model; drop their pacing entries */
            let cur = b.current;
            let mut region = gone;
            self.anim.retain(|a| {
                let hit = a.page == cur
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
            self.render_page_region_wave(r, Some(Wave::Ink));
            self.restore_chrome_over(r);
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
        /* the delete confirmation swallows every touch */
        if self.on_home() && self.confirm_delete.is_some() {
            if phase == Phase::Press {
                self.delete_confirm_press(x, y);
            }
            return;
        }
        match phase {
            Phase::Press => {
                if !self.on_home() && x < MENU_HOT && y < MENU_HOT {
                    self.show_sidebar();
                    return;
                }
                self.touch_start = Some((x, y));
                self.touch_t0 = Some(Instant::now());
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
                    if self.on_home() && self.lib_list.is_some() {
                        /* the TABLET LIBRARY view: scroll or pick */
                        if dy.abs() >= FLIP_DX && dx.abs() <= FLIP_DY_MAX {
                            self.lib_list_scroll(dy < 0);
                        } else if dx.abs() < 40 && dy.abs() < 40 {
                            self.lib_list_press(sx, sy);
                        } else if dx.abs() >= FLIP_DX {
                            self.close_lib_list(); /* horizontal swipe = back */
                        }
                    } else if dx.abs() >= FLIP_DX && dy.abs() <= FLIP_DY_MAX {
                        /* swipe left = next page (turning forward) */
                        self.flip(if dx < 0 { 1 } else { -1 });
                    } else if self.on_home() && dy.abs() >= FLIP_DX && dx.abs() <= FLIP_DY_MAX {
                        /* vertical swipe on the shelf: page the book list */
                        self.shelf_scroll(dy < 0);
                    } else if self.on_home() && dx.abs() < 40 && dy.abs() < 40 {
                        /* a finger tap opens a book; a long hold offers to
                         * delete it */
                        let held = self
                            .touch_t0
                            .take()
                            .is_some_and(|t| t.elapsed().as_millis() >= HOLD_MS);
                        if held {
                            self.home_hold(sx, sy);
                        } else {
                            self.home_press(sx, sy);
                        }
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
        if !self.page_changed {
            return;
        }
        let Some(b) = self.book.as_mut() else { return };
        if b.page.is_empty() {
            return;
        }
        if self.pi.is_none() {
            return;
        }
        b.save_all();
        let (w, h, gray) = b.snapshot(SNAP_DIV);
        let patches = patch_summary(&b.page);
        let layout = layout_hints(b, self.text_scale);
        let entry = b.entry(b.current);
        let kind = match entry {
            Some(Entry::Pdf(p)) => format!("printed page {} of the PDF", p + 1),
            _ => "a blank note page".into(),
        };
        let text = match entry {
            Some(Entry::Pdf(p)) => {
                let mut t = b.page_text(p);
                if t.len() > 4500 {
                    let mut cut = 4500;
                    while cut > 0 && !t.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    t.truncate(cut);
                    t.push_str("\n[...]");
                }
                if t.trim().is_empty() {
                    "(no extractable text on this page)".into()
                } else {
                    t
                }
            }
            _ => "(note page — handwriting only)".into(),
        };
        let msg = format!(
            "\"{}\" — page {} of {} ({}). The attached image is the page as \
             the user sees it (half scale), with everyone's ink. The user \
             just paused writing. Extracted text of this page:\n---\n{}\n---\n\
             Your existing patches here: {}. Measured layout (page \
             coordinates — trust these numbers): {} \
             Respond with your reader_* tools only if it genuinely helps; \
             otherwise reply `pass`.",
            b.title,
            b.current + 1,
            b.count(),
            kind,
            text,
            patches,
            layout,
        );
        let streaming = self.streaming;
        let page_no = b.current + 1;
        let Some(pi) = self.pi.as_mut() else { return };
        match pi.send_image_message(&gray, w as u32, h as u32, &msg, streaming) {
            Ok(()) => {
                self.page_changed = false;
                self.streaming = true;
                self.set_working(true);
                println!("reader: page {page_no} sent to pi");
            }
            Err(e) => println!("reader: send failed: {e}"),
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
            PiEvent::Notice(n) => println!("reader: pi {n}"),
            PiEvent::End => {
                self.streaming = false;
                self.set_working(false);
                let t: String = self.reply_buf.trim().chars().take(300).collect();
                if !t.is_empty() {
                    println!("reader: pi said: {t}");
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
                println!("reader: pi exited: {reason}");
            }
        }
    }

    /* -- the tool socket -- */

    fn handle_ipc_request(&mut self, req: &Value) -> Value {
        match req["cmd"].as_str().unwrap_or("") {
            "view" => self.ipc_view(req),
            "draw" => self.ipc_draw(req),
            "erase" => self.ipc_erase(req),
            "goto" => self.ipc_goto(req),
            "underline" => self.ipc_underline(req),
            "insert_note" => self.ipc_insert_note(req),
            "page_text" => self.ipc_page_text(req),
            other => json!({ "ok": false, "error": format!("unknown cmd '{other}'") }),
        }
    }

    /// 1-based page param; None/0 = the page on screen.
    fn req_page(&self, req: &Value) -> usize {
        let cur = self.book.as_ref().map_or(0, |b| b.current);
        match req["page"].as_u64() {
            Some(p) if p >= 1 => p as usize - 1,
            _ => cur,
        }
    }

    fn no_book() -> Value {
        json!({ "ok": false, "error": "no book is open (the user is on the book shelf)" })
    }

    fn ipc_view(&mut self, req: &Value) -> Value {
        let idx = self.req_page(req);
        let Some(b) = self.book.as_ref() else { return Self::no_book() };
        if idx >= b.count() {
            return json!({ "ok": false, "error": format!("no page {} (book has {})", idx + 1, b.count()) });
        }
        let (w, h, gray, patches) = if idx == b.current {
            let (w, h, gray) = b.snapshot(SNAP_DIV);
            (w, h, gray, patch_list(&b.page))
        } else {
            match b.snapshot_of(idx, SNAP_DIV) {
                Some((w, h, gray, ink)) => (w, h, gray, patch_list(&ink)),
                None => return json!({ "ok": false, "error": "page unreadable" }),
            }
        };
        let png = png::encode_gray(w as u32, h as u32, &gray);
        json!({
            "ok": true,
            "page": idx + 1,
            "page_count": b.count(),
            "label": b.label(idx),
            "page_width": FB_W,
            "page_height": FB_H,
            "image_scale": SNAP_DIV,
            "png_base64": png::base64(&png),
            "patches": patches,
        })
    }

    /// Add a ready-made patch (parsed SVG or underline strokes) to page
    /// `idx`, animating when that page is on screen. Returns (id, bbox,
    /// on_screen) or an error Value.
    fn add_patch_at(&mut self, idx: usize, strokes: Vec<Stroke>) -> Result<(u64, Option<Rect>, bool), Value> {
        let Some(b) = self.book.as_mut() else { return Err(Self::no_book()) };
        if idx >= b.count() {
            return Err(json!({ "ok": false, "error": format!("no page {} (book has {})", idx + 1, b.count()) }));
        }
        if idx == b.current {
            let id = b.page.add_patch(strokes, Vec::new());
            let patch = b.page.patches.last().unwrap();
            let bbox = ink::patch_bbox(patch).map(|bb| bb.clamp_screen());
            /* queue the ghost-hand animation — unless another view owns the
             * screen right now (the strokes appear on return, via the full
             * repaint from the model) */
            let animate = self.agent_page.is_none() && self.sidebar.is_none();
            let mut queued: Vec<AnimStroke> = Vec::new();
            for s in patch.strokes.iter().filter(|_| animate) {
                if let Some(bb) = ink::stroke_bbox(s) {
                    queued.push(AnimStroke {
                        page: idx,
                        patch: id,
                        gray: s.gray,
                        remaining: s.pts.iter().copied().collect(),
                        last: None,
                        bbox: bb.clamp_screen(),
                    });
                }
            }
            b.save_all();
            self.anim.extend(queued);
            self.last_activity_page = idx;
            Ok((id, bbox, true))
        } else {
            let Some(e) = b.entry(idx) else {
                return Err(json!({ "ok": false, "error": "no such page" }));
            };
            let mut p = b.load_ink(e);
            let id = p.add_patch(strokes, Vec::new());
            let bbox = ink::patch_bbox(p.patches.last().unwrap()).map(|bb| bb.clamp_screen());
            let path = match e {
                Entry::Pdf(n) => format!("{}/ink/pdf-{:04}.json", b.dir, n + 1),
                Entry::Note(n) => format!("{}/ink/note-{:04}.json", b.dir, n),
            };
            if let Err(err) = p.save(&path) {
                return Err(json!({ "ok": false, "error": format!("save: {err}") }));
            }
            Ok((id, bbox, false))
        }
    }

    fn ipc_draw(&mut self, req: &Value) -> Value {
        let Some(svg) = req["svg"].as_str() else {
            return json!({ "ok": false, "error": "missing 'svg'" });
        };
        /* _texts: typeset Garamond runs — this app draws plotter strokes only */
        let (strokes, _texts, notes) =
            match svg_ink::parse(svg, self.text_scale, svg_ink::PiFont::from(hershey::default_face(APP))) {
            Ok(v) => v,
            Err(e) => return json!({ "ok": false, "error": e }),
        };
        for n in &notes {
            println!("reader: draw note: {n}");
        }
        let idx = self.req_page(req);
        let n_strokes = strokes.len();
        match self.add_patch_at(idx, strokes) {
            Ok((id, bbox, on_screen)) => {
                println!("reader: patch #{id} on page {} ({n_strokes} strokes)", idx + 1);
                let layout = self.book.as_ref().map(|b| {
                    if on_screen {
                        layout_hints(b, self.text_scale)
                    } else {
                        String::new()
                    }
                });
                json!({
                    "ok": true, "id": id, "page": idx + 1,
                    "bbox": bbox.map(|b| json!([b.x0, b.y0, b.x1, b.y1])).unwrap_or(json!(null)),
                    "layout": layout.unwrap_or_default(),
                    "notes": notes,
                })
            }
            Err(e) => e,
        }
    }

    fn ipc_underline(&mut self, req: &Value) -> Value {
        let Some(phrase) = req["phrase"].as_str().filter(|p| !p.trim().is_empty()) else {
            return json!({ "ok": false, "error": "missing 'phrase'" });
        };
        let nth = req["occurrence"].as_u64().unwrap_or(1).max(1) as usize;
        let idx = self.req_page(req);
        let Some(b) = self.book.as_ref() else { return Self::no_book() };
        if idx >= b.count() {
            return json!({ "ok": false, "error": format!("no page {} (book has {})", idx + 1, b.count()) });
        }
        let Some(Entry::Pdf(p)) = b.entry(idx) else {
            return json!({ "ok": false, "error": "that page is a note page — nothing printed to underline" });
        };
        let words = b.words(p);
        if words.is_empty() {
            return json!({ "ok": false, "error": "no word geometry for this page" });
        }
        let (picked, total) = book::find_phrase(&words, phrase, nth);
        let Some(picked) = picked else {
            let err = if total == 0 {
                format!(
                    "phrase not found on page {} — quote it exactly as it appears \
                     (matching ignores case and punctuation)",
                    idx + 1
                )
            } else {
                format!("only {total} occurrence(s) on page {}", idx + 1)
            };
            return json!({ "ok": false, "error": err, "matches": total });
        };
        let strokes = book::underline_strokes(&words, &picked);
        match self.add_patch_at(idx, strokes) {
            Ok((id, bbox, _)) => {
                println!("reader: underlined '{phrase}' on page {} (#{id})", idx + 1);
                json!({
                    "ok": true, "id": id, "page": idx + 1, "matches": total,
                    "bbox": bbox.map(|b| json!([b.x0, b.y0, b.x1, b.y1])).unwrap_or(json!(null)),
                })
            }
            Err(e) => e,
        }
    }

    fn ipc_erase(&mut self, req: &Value) -> Value {
        let Some(id) = req["id"].as_u64() else {
            return json!({ "ok": false, "error": "missing 'id'" });
        };
        let idx = self.req_page(req);
        let Some(b) = self.book.as_mut() else { return Self::no_book() };
        if idx == b.current {
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
            match b.page.remove_patch(id) {
                Some(bb) => {
                    b.save_all();
                    if self.agent_page.is_none() && self.sidebar.is_none() {
                        let r = region.map_or(bb, |r| r.union(bb)).pad(4).clamp_screen();
                        self.render_page_region(r);
                        self.restore_chrome_over(r);
                    }
                    json!({ "ok": true })
                }
                None => json!({ "ok": false, "error": format!("no patch {id} on page {}", idx + 1) }),
            }
        } else {
            if idx >= b.count() {
                return json!({ "ok": false, "error": format!("no page {} (book has {})", idx + 1, b.count()) });
            }
            let Some(e) = b.entry(idx) else {
                return json!({ "ok": false, "error": "no such page" });
            };
            let mut p = b.load_ink(e);
            match p.remove_patch(id) {
                Some(_) => {
                    let path = match e {
                        Entry::Pdf(n) => format!("{}/ink/pdf-{:04}.json", b.dir, n + 1),
                        Entry::Note(n) => format!("{}/ink/note-{:04}.json", b.dir, n),
                    };
                    if let Err(err) = p.save(&path) {
                        return json!({ "ok": false, "error": format!("save: {err}") });
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
        let Some(b) = self.book.as_ref() else { return Self::no_book() };
        if idx >= b.count() {
            return json!({ "ok": false, "error": format!("no page {} (book has {})", p, b.count()) });
        }
        if self.agent_page.is_some() || self.sidebar.is_some() {
            return json!({ "ok": false, "error": "the user is in a menu/instructions view; not turning the page" });
        }
        if self.cur_stroke.is_some()
            || self.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
        {
            return json!({ "ok": false, "error": "the user is writing right now; try again shortly" });
        }
        let cur = b.current;
        if idx != cur {
            self.flip(idx as i32 - cur as i32);
        }
        println!("reader: pi turned to page {}", idx + 1);
        let b = self.book.as_ref().unwrap();
        json!({
            "ok": true, "page": idx + 1, "page_count": b.count(), "label": b.label(idx),
            "layout": layout_hints(b, self.text_scale),
        })
    }

    fn ipc_insert_note(&mut self, req: &Value) -> Value {
        let Some(b) = self.book.as_mut() else { return Self::no_book() };
        let after = match req["after_page"].as_u64() {
            Some(p) if p >= 1 && (p as usize) <= b.count() => p as usize - 1,
            Some(_) => {
                return json!({ "ok": false, "error": format!("after_page out of range (book has {})", b.count()) })
            }
            None => b.current,
        };
        let at = b.insert_note(after);
        let count = b.count();
        println!("reader: pi inserted note page at {}", at + 1);
        /* the indicator (page x of y) changed; refresh it if visible */
        if self.indicator_until.is_some() && self.sidebar.is_none() && self.agent_page.is_none() {
            self.show_page_indicator();
        }
        json!({
            "ok": true, "page": at + 1, "page_count": count,
            "note": "a blank note page now exists there; draw on it with reader_draw {page: N}",
        })
    }

    fn ipc_page_text(&mut self, req: &Value) -> Value {
        let Some(b) = self.book.as_ref() else { return Self::no_book() };
        let from = match req["from"].as_u64() {
            Some(p) if p >= 1 && (p as usize) <= b.count() => p as usize - 1,
            _ => return json!({ "ok": false, "error": format!("missing/invalid 'from' (book has {} pages)", b.count()) }),
        };
        let to = match req["to"].as_u64() {
            Some(p) if p >= 1 => (p as usize - 1).min(b.count() - 1),
            _ => from,
        };
        if to < from {
            return json!({ "ok": false, "error": "'to' before 'from'" });
        }
        let to = to.min(from + 7); /* at most 8 pages per call */
        let mut out = String::new();
        for i in from..=to {
            match b.entry(i) {
                Some(Entry::Pdf(p)) => {
                    let mut t = b.page_text(p);
                    if t.len() > 6000 {
                        let mut cut = 6000;
                        while cut > 0 && !t.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        t.truncate(cut);
                        t.push_str("\n[...]");
                    }
                    out.push_str(&format!("--- page {} (p.{}) ---\n{}\n", i + 1, p + 1, t.trim_end()));
                }
                Some(Entry::Note(_)) => {
                    out.push_str(&format!("--- page {} (note page: handwritten ink only — use reader_view to see it) ---\n", i + 1));
                }
                None => {}
            }
        }
        json!({ "ok": true, "from": from + 1, "to": to + 1, "page_count": b.count(), "text": out })
    }

    /* -- AI ink animation -- */

    fn anim_tick(&mut self) {
        /* never fight the writer: hold while the pen is on/near the glass;
         * also hold while another view (sidebar, AGENT.md) owns the screen */
        if self.cur_stroke.is_some()
            || self.sidebar.is_some()
            || self.agent_page.is_some()
            || self.on_home()
            || self.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(350))
        {
            self.last_anim = Instant::now();
            return;
        }
        let cur = self.book.as_ref().map_or(0, |b| b.current);
        let mut budget = ANIM_BUDGET;
        while budget > 0.0 {
            let Some(a) = self.anim.front_mut() else { break };
            if a.page != cur {
                self.anim.pop_front(); /* already in the model; visible on flip */
                continue;
            }
            let Some(next) = a.remaining.pop_front() else {
                self.anim.pop_front();
                continue;
            };
            let from = a.last.unwrap_or(next);
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
         * wrote smooths the DU-rough stroke edges */
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
        let msg = "reader sleeps";
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

/// Measured page geometry for the pause message. On a printed page the
/// interesting space is the MARGINS (from the real word boxes); on a note
/// page it is the free bands between ink rows. All in page coordinates, so
/// placement is arithmetic for pi rather than eyeballing.
fn layout_hints(b: &Book, text_scale: f32) -> String {
    let mut s = match b.entry(b.current) {
        Some(Entry::Pdf(p)) => {
            let words = b.words(p);
            if words.is_empty() {
                String::from("No printed-text geometry on this page. ")
            } else {
                let tx0 = words.iter().map(|w| w.x0).min().unwrap();
                let tx1 = words.iter().map(|w| w.x1).max().unwrap();
                let ty0 = words.iter().map(|w| w.y0).min().unwrap();
                let ty1 = words.iter().map(|w| w.y1).max().unwrap();
                let mut hs: Vec<i32> = words.iter().map(|w| (w.y1 - w.y0).clamp(8, 80)).collect();
                hs.sort_unstable();
                let lh = hs[hs.len() / 2];
                let (left, right) = (tx0, FB_W - tx1);
                let (top, bottom) = (ty0, FB_H - ty1);
                let fs = ((lh * 9) / 10).clamp(24, 42);
                let best = if right >= left && right >= 100 {
                    format!("the RIGHT margin (x{}-{}) is your writing zone", tx1 + 12, FB_W - 10)
                } else if left > right && left >= 100 {
                    format!("the LEFT margin (x10-{}) is your writing zone", tx0 - 12)
                } else if bottom >= 140 {
                    format!("margins are narrow — use the BOTTOM strip (y{}-{})", ty1 + 20, FB_H - 16)
                } else {
                    "margins are narrow — prefer reader_underline + a note page".to_string()
                };
                format!(
                    "Printed block x{tx0}-{tx1}, y{ty0}-{ty1}; print line-height ~{lh}px. \
                     Margins: left {left}px, right {right}px, top {top}px, bottom {bottom}px; \
                     {best}. Write margin notes at font-size ~{fs} and KEEP LINES SHORT — \
                     a line is ~0.6*font-size px per character and must fit the margin. "
                )
            }
        }
        _ => String::new(),
    };

    let bands = b.page.ink_bands();
    if bands.is_empty() {
        if s.is_empty() {
            s = "The page is blank — the full 1404x1872 canvas is yours.".into();
        } else {
            s.push_str("No ink on this page yet.");
        }
    } else {
        let mut rows: Vec<String> = bands
            .iter()
            .map(|band| {
                format!(
                    "y{}-{} (x{}-{}{})",
                    band.y0,
                    band.y1,
                    band.x0,
                    band.x1,
                    if band.user { "" } else { ", yours" }
                )
            })
            .collect();
        if rows.len() > 12 {
            let extra = rows.len() - 11;
            rows.truncate(11);
            rows.push(format!("and {extra} more"));
        }
        s.push_str(&format!("Ink rows: {}.", rows.join(", ")));
        /* free bands only matter on note pages (whole page is writable) */
        if matches!(b.entry(b.current), Some(Entry::Note(_))) {
            let mut free: Vec<String> = Vec::new();
            if bands[0].y0 > 130 {
                free.push(format!("y0-{} (top)", bands[0].y0 - 24));
            }
            for w in bands.windows(2) {
                let (a, bb) = (&w[0], &w[1]);
                if bb.y0 - a.y1 >= 96 {
                    free.push(format!("y{}-{}", a.y1 + 24, bb.y0 - 24));
                }
            }
            let last = bands.last().unwrap();
            if last.y1 < FB_H - 130 {
                free.push(format!("y{}-{} (bottom)", last.y1 + 24, FB_H - 24));
            }
            if !free.is_empty() {
                s.push_str(&format!(" Free bands (full width): {}.", free.join(", ")));
            }
            if let Some(lh) = b.page.user_line_height() {
                let fs = (lh * 9 / 10).clamp(30, 90);
                s.push_str(&format!(
                    " The user's handwriting rows are ~{lh}px tall: write at font-size ~{fs} \
                     with ~{}px between your baselines.",
                    fs * 3 / 2
                ));
            }
        }
    }
    if (text_scale - 1.0).abs() > 0.01 {
        s.push_str(&format!(
            " NOTE: the user zooms your text to {}% — every font-size you write renders \
             that much {}; budget widths and baseline spacing accordingly.",
            (text_scale * 100.0).round() as i32,
            if text_scale > 1.0 { "larger" } else { "smaller" },
        ));
    }
    s
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
    println!("reader: sleeping");
    if let Some(b) = app.book.as_mut() {
        b.save_all();
    }
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
            println!("reader: suspend never happened ({attempts} tries); waking the page");
            break;
        }
        println!("reader: suspend aborted (EPD discharge timer), retrying");
    }
    println!("reader: waking");
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

/// Headless import (`reader --import-cli <name-fragment>`): run the same
/// on-device pipeline synchronously, printing progress — for testing over
/// ssh and for scripted imports, no takeover needed.
fn import_cli(frag: &str) -> std::process::ExitCode {
    let needle = frag.to_lowercase();
    let Some(doc) = xochitl::scan_pdfs().into_iter().find(|d| d.name.to_lowercase().contains(&needle))
    else {
        eprintln!("reader: no xochitl pdf matching '{frag}'");
        return std::process::ExitCode::FAILURE;
    };
    println!("reader: importing '{}' ({} KB)", doc.name, doc.kb);
    let job = import::Job {
        uuid: doc.uuid.clone(),
        title: doc.name.trim_end_matches(".pdf").to_string(),
        slug: xochitl::slugify(&doc.name),
    };
    let mut imp = match import::Importer::start(job, &book::books_dir()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("reader: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut last = usize::MAX;
    loop {
        match imp.poll() {
            import::Tick::Working(done, total) => {
                if done != last && done % 10 == 0 {
                    println!("reader: page {done}/{total}");
                    last = done;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            import::Tick::Finished(slug) => {
                println!("reader: done -> {slug}");
                return std::process::ExitCode::SUCCESS;
            }
            import::Tick::Failed(msg) => {
                eprintln!("reader: {msg}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--import-cli") {
        let Some(frag) = args.get(2) else {
            eprintln!("usage: reader --import-cli <name fragment>");
            return std::process::ExitCode::FAILURE;
        };
        return import_cli(frag);
    }
    let (disp, fb) = match Display::open(APP) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("reader: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let takeover = disp.is_takeover();
    println!(
        "reader: up, fb={FB_W}x{FB_H} ({})",
        if takeover { "takeover/rm2fb" } else { "windowed/qtfb" }
    );
    install_signal_handlers();

    let sock = sock_path();
    let ipc = IpcServer::open(APP, &sock)
        .map_err(|e| eprintln!("reader: tool socket: {e} — pi gets no tools"))
        .ok();
    let pi = match pi_rpc::spawn(&sock) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("reader: could not start pi: {e}");
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
            .map_err(|e| eprintln!("reader: no touch device ({e}) — page flips disabled"))
            .ok()
    } else {
        None
    };
    let mut powerdev = if takeover {
        power::PowerButton::open(APP)
            .map_err(|e| eprintln!("reader: no power button ({e})"))
            .ok()
    } else {
        None
    };
    let mut power_grace = Instant::now();

    /* Idle auto-suspend (takeover only — windowed mode leaves it to
     * xochitl). Stock xochitl sleeps after ~10 min idle; we took the power
     * button, so we owe the battery the same courtesy. Tunable via
     * READER_AUTO_SLEEP_MIN (minutes), 0 disables. */
    let auto_sleep_min: u64 = std::env::var("READER_AUTO_SLEEP_MIN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let auto_sleep = (powerdev.is_some() && auto_sleep_min > 0)
        .then(|| Duration::from_secs(auto_sleep_min * 60));
    let mut last_activity = Instant::now();

    let (text_scale, last_book) = load_settings();
    let now = Instant::now();
    let mut app = App {
        fb,
        disp,
        pi,
        ipc,
        book: None,
        shelf: book::scan(),
        shelf_top: 0,
        confirm_delete: None,
        lib_list: None,
        importer: None,
        import_queue: VecDeque::new(),
        import_shown: (0, 0),
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
        anim: VecDeque::new(),
        anim_dirty: None,
        anim_settle: None,
        last_anim: now,
        touch_start: None,
        touch_t0: None,
        touch_last: (0, 0),

        indicator_until: None,
        working: false,
        agent_page: None,
        sidebar: None,
        last_activity_page: 0,
        text_scale,
        deghost_at: None,
        ink_settle: None,
        ink_settle_at: None,
    };

    /* first paint: resume the last book, else the shelf */
    if !last_book.is_empty() {
        app.open_book(&last_book);
    }
    if app.book.is_none() {
        app.render_home(true);
    }

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
                /* no 5-finger quit (a writing palm reads as 5+ contacts) —
                 * the top-edge swipe -> CLOSE is the exit */
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
        app.tick_import();
        app.maybe_send_page();
        if app.indicator_until.is_some_and(|at| Instant::now() >= at) {
            app.clear_page_indicator();
        }
        /* pen-ink settle: GL16 over fresh strokes on a page raster, once
         * the pen has been quiet a beat — heals DU-crunched print */
        if let Some(at) = app.ink_settle_at {
            if Instant::now() >= at {
                if app.cur_stroke.is_some()
                    || app.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(500))
                {
                    app.ink_settle_at = Some(Instant::now() + Duration::from_millis(400));
                } else {
                    app.ink_settle_at = None;
                    if let Some(r) = app.ink_settle.take() {
                        if app.sidebar.is_none() && app.agent_page.is_none() && !app.on_home() {
                            let r = r.pad(4).clamp_screen();
                            app.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Text);
                        }
                    }
                }
            }
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
                    if app.sidebar.is_none() && app.agent_page.is_none() && !app.on_home() {
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
                println!("reader: idle {auto_sleep_min}min -> auto-sleep");
                sleep_cycle(&mut app, p, &mut pen, &mut touchdev);
                power_grace = Instant::now() + Duration::from_secs(3);
                last_activity = Instant::now();
            }
        }
    }

    println!("reader: exiting");
    if let Some(b) = app.book.as_mut() {
        b.save_all();
    }
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
    if let Some(at) = app.ink_settle_at {
        soonest(at.saturating_duration_since(Instant::now()));
    }
    if app.importer.is_some() {
        /* mutool children finish without an fd to poll: wake regularly */
        soonest(Duration::from_millis(120));
    }
    match t {
        Some(d) => (d.as_millis() as i32).max(0),
        None => -1,
    }
}
