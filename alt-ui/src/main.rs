//! Paper — a pixel-faithful replication of the stock reMarkable UI with
//! pi (the AI agent) integrated, on the reMarkable 2.
//!
//! One unified takeover app: a xochitl-like home grid opens notebooks
//! (blank vector pages, pi as co-writer) and books (pre-rendered PDF
//! bundles, pi as margin companion). Status bar, right-edge toolbar,
//! lasso selection, undo/redo.
//!
//! Built on reader's takeover stack (which is notebook's, which is
//! collab's): rm2fb + per-update waveforms, raw Wacom / touch / power
//! input, unix-socket tools into pi.
//!
//! Module map:
//!   fb/draw/display/qtfb/rm2fb   the display stack (from collab)
//!   pen/touch/power              raw input (from collab)
//!   ink.rs      the ink overlay: user strokes + AI patches, vector-first
//!   doc.rs      the unified document: book bundles + growing notebooks
//!   store.rs    the document store: scan, folders, import
//!   home.rs     the home grid; thumbs.rs its thumbnail cache
//!   statusbar.rs the top bar: clock, wifi, battery
//!   kb.rs       a minimal on-screen keyboard (rename / new folder)
//!   png_dec.rs  PNG (gray8) + inflate, dependency-free
//!   svg_ink.rs  pi's SVG -> pen strokes (bezier flattening, Hershey text)
//!   hershey.rs  the single-stroke plotter font
//!   ipc.rs      unix-socket server for pi's canvas_* tools
//!   pi_rpc.rs   the pi child process (JSONL RPC)
//!   png.rs      grayscale PNG encoder + base64 (page snapshots)
//!
//! Views are an explicit `Screen` enum — Home (the grid) or Doc (a
//! document being edited). Doc-scoped state lives INSIDE Screen::Doc, so
//! closing a document drops all of it by construction. Modal dialogs are
//! one `Option<Dialog>` overlay that swallows input while open.

mod display;
#[allow(dead_code)] /* library module from collab; not all used */
mod draw;
#[allow(dead_code)] /* words/snapshot/underline wire in with pi (M5) */
mod doc;
mod fb;
mod font;
#[allow(dead_code)] /* wired in with pi (M5) */
mod hershey;
mod hershey_data;
mod home;
#[allow(dead_code)] /* selection APIs wire in with the lasso (M4) */
mod icons;
#[allow(dead_code)] /* patches/snapshot wire in with pi (M5), bands too */
mod ink;
#[allow(dead_code)] /* wired in with pi (M5) */
mod ipc;
mod kb;
mod pen;
#[allow(dead_code)] /* wired in with pi (M5) */
mod pi_rpc;
#[allow(dead_code)] /* library module from collab; not all used */
mod png;
mod png_dec;
mod power;
mod qtfb;
mod rm2fb;
mod select;
mod statusbar;
mod store;
#[allow(dead_code)] /* wired in with pi (M5) */
mod svg_ink;
#[allow(dead_code)] /* library module from collab; not all used */
mod text;
mod thumbs;
mod toolbar;
mod touch;
#[allow(dead_code)] /* MoveStrokes wires in with the lasso (M4) */
mod undo;

use display::{Display, Wave};
use doc::{Doc, Entry};
use draw::{text_width, BLACK, GRAY, WHITE};
use fb::{Framebuffer, SCREEN_H as FB_H, SCREEN_W as FB_W};
use home::HomeView;
use svg_ink::PiFont;
use ink::{Page, Pt, Rect, Stroke};
use ipc::IpcServer;
use kb::{Kb, KbAction};
use pen::{Pen, PenPhase};
use pi_rpc::{Pi, PiEvent};
use qtfb::{Event, Phase};
use serde_json::{json, Value};
use std::collections::VecDeque;
use statusbar::{SysStatus, STATUS_H};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use toolbar::{TbAction, Tool};
use undo::EditOp;

/* ---- tuning -------------------------------------------------------------- */

const INK_FLUSH_QTFB: Duration = Duration::from_millis(12);
const INK_FLUSH_TAKEOVER: Duration = Duration::from_millis(8);
const PEN_TIMEOUT: Duration = Duration::from_millis(1500); /* palm rejection */

const ERASER_R: f32 = 22.0;

/// How long a writing pause must last before the page goes to pi.
const IDLE_DELAY: Duration = Duration::from_millis(2800);

/// AI ink animation: one flush per tick, `ANIM_BUDGET` px of path per tick.
const ANIM_TICK: Duration = Duration::from_millis(28);
const ANIM_BUDGET: f32 = 48.0;

/// Page snapshots for pi are half scale (702x936).
const SNAP_DIV: i32 = 2;

/* the pi working dot sits just left of the toolbar strip */
const DOT_RECT: Rect =
    Rect { x0: toolbar::TB_X0 - 40, y0: 8, x1: toolbar::TB_X0 - 14, y1: 34 };

/* the top-edge swipe reveals the top bar (CLOSE / MY FILES / status) */
const EDGE_Y: i32 = 16;
const SWIPE_DIST: i32 = 90;
const BAR_H: i32 = 90;
const CLOSE_X0: i32 = 16;
const CLOSE_Y0: i32 = 12;
const CLOSE_BTN_W: i32 = 190;
const CLOSE_BTN_H: i32 = 64;
const FILES_X0: i32 = 226;
const FILES_BTN_W: i32 = 260;
const BAR_TTL: Duration = Duration::from_secs(4);

/* page-flip / scroll gesture: mostly-straight finger travel */
const FLIP_DX: i32 = 260;
const FLIP_DY_MAX: i32 = 240;

/* long-press on a home cell */
const HOLD_MS: u128 = 650;

/* transient page-number indicator after a flip */
const INDICATOR_TTL: Duration = Duration::from_millis(1400);

/* status bar refresh cadence while visible */
const STATUS_POLL: Duration = Duration::from_secs(20);

/* re-scan the docs dir on the home screen this often, so a doc a background
 * sync pulled in appears without navigating away and back */
const HOME_RESCAN: Duration = Duration::from_secs(8);

/* the minimum gap between app-triggered background web syncs */
const MIN_SYNC_GAP: Duration = Duration::from_secs(10);

/* page turns render gently (GL16); a GC16 deghost flash every Nth turn
 * clears the faint residue GL16 leaves behind */
const FLIP_DEGHOST_EVERY: u32 = 8;

/* modal dialogs */
const DLG_W: i32 = 760;
const DLG_ROW_H: i32 = 96;

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

/// Brush radius for a pen frame: a fixed medium nib plus a little from
/// real pressure (0..4095).
fn brush_r(pressure: i32) -> f32 {
    2.0 + pressure as f32 / 4096.0 * 1.6
}

/* ---- settings ------------------------------------------------------------- */

fn settings_path() -> String {
    format!("{}/settings.json", store::data_dir())
}

fn load_settings() -> (String, home::Sort, Tool, PiFont) {
    match store::read_json(&settings_path()) {
        Some(v) => (
            v["last_doc"].as_str().unwrap_or("").to_string(),
            match v["sort"].as_str() {
                Some("title") => home::Sort::Title,
                _ => home::Sort::Opened,
            },
            match v["tool"].as_str() {
                Some("eraser") => Tool::Eraser,
                Some("lasso") => Tool::Lasso,
                _ => Tool::Pen,
            },
            v["pi_font"]
                .as_str()
                .and_then(PiFont::from_key)
                .unwrap_or(PiFont::Serif),
        ),
        None => (String::new(), home::Sort::Opened, Tool::Pen, PiFont::Serif),
    }
}

fn save_settings(last_doc: &str, sort: home::Sort, tool: Tool, pi_font: PiFont) {
    let p = settings_path();
    if let Some(dir) = std::path::Path::new(&p).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let doc = json!({
        "last_doc": last_doc,
        "sort": match sort { home::Sort::Title => "title", home::Sort::Opened => "opened" },
        "tool": match tool { Tool::Pen => "pen", Tool::Eraser => "eraser", Tool::Lasso => "lasso" },
        "pi_font": pi_font.key(),
    });
    let _ = std::fs::write(&p, serde_json::to_vec(&doc).unwrap_or_default());
}

fn sock_path() -> String {
    std::env::var("PAPER_SOCK").unwrap_or_else(|_| "/tmp/paper-ctl.sock".into())
}

/* ---- AI ink animation ------------------------------------------------------ */

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

/* ---- screens -------------------------------------------------------------- */

/// A document being edited. Everything scoped to the open document lives
/// here; dropping the screen drops the state.
struct DocView {
    doc: Doc,

    /* editing */
    tool: Tool,
    tb_open: bool,
    undo: undo::PerPage,
    pending_erase: Vec<ink::OwnedStroke>, /* rubber batch, one op per contact */
    lasso: Option<select::Lasso>,
    selection: Option<select::Selection>,
    sel_chrome: Option<Rect>, /* where the dashed box is PAINTED right now */
    last_drag_paint: Instant,

    /* live pen ink */
    cur_stroke: Option<Stroke>,
    ink_dirty: Option<Rect>,
    contact_changed: bool,
    page_changed: bool,

    /* the pause trigger */
    idle_at: Option<Instant>,

    /* AI ink animation */
    anim: VecDeque<AnimStroke>,
    anim_dirty: Option<Rect>,
    anim_settle: Option<Rect>, /* union animated; GL16-refined when done */

    /* transient chrome + heal timers */
    indicator_until: Option<Instant>,
    deghost_at: Option<Instant>,
    ink_settle: Option<Rect>,
    ink_settle_at: Option<Instant>,
}

impl DocView {
    fn new(doc: Doc, tool: Tool) -> Self {
        DocView {
            doc,
            tool,
            tb_open: false,
            undo: undo::PerPage::default(),
            pending_erase: Vec::new(),
            lasso: None,
            selection: None,
            sel_chrome: None,
            last_drag_paint: Instant::now(),
            cur_stroke: None,
            ink_dirty: None,
            contact_changed: false,
            page_changed: false,
            idle_at: None,
            anim: VecDeque::new(),
            anim_dirty: None,
            anim_settle: None,
            indicator_until: None,
            deghost_at: None,
            ink_settle: None,
            ink_settle_at: None,
        }
    }
}

enum Screen {
    Home(HomeView),
    Doc(DocView),
}

/// Modal overlays: exactly one at a time, swallowing all input. The idx
/// is the cell index in the home grid the dialog acts on.
enum Dialog {
    DocMenu { idx: usize },
    ConfirmDelete { idx: usize },
    Rename { idx: usize, kb: Kb },
    MoveTo { idx: usize },
    NewFolder { idx: usize, kb: Kb },
    GoTo { entry: String }, /* the page-jump numpad (doc screen) */
    FontPick,               /* pick pi's handwriting face (doc screen) */
}

/* ---- the app ------------------------------------------------------------- */

struct App {
    fb: Framebuffer,
    disp: Display,
    takeover: bool,
    screen: Screen,
    dialog: Option<Dialog>,

    /* pi + its tool socket */
    pi: Option<Pi>,
    ipc: Option<IpcServer>,
    streaming: bool,
    reply_buf: String,
    working: bool,
    last_anim: Instant,
    last_contact: Option<Instant>, /* actual glass contact */
    pi_alive_at: Instant,          /* last sign of life mid-turn */
    pi_respawn_at: Option<Instant>,
    pi_stall: Duration,

    status: SysStatus,
    status_poll_at: Instant,
    home_rescan_at: Instant,
    last_sync: Instant,
    sort: home::Sort,
    tool: Tool, /* remembered across documents, persisted */
    pi_font: PiFont, /* pi's default writing font, persisted */
    flips_since_flash: u32, /* partial-GC16 turns; GC16 flash every FLIP_DEGHOST_EVERY */

    ink_flush: Duration,
    last_ink_flush: Instant,

    /// Cut strokes, normalized to their bbox origin (paste comes later).
    clipboard: Option<Vec<Stroke>>,

    /* cross-cutting input state */
    last_pen: Option<Instant>,
    touch_start: Option<(i32, i32)>,
    touch_t0: Option<Instant>,
    touch_last: (i32, i32),
    swipe_from: Option<i32>,
    bar_until: Option<Instant>,
}

impl App {
    /* -- home ---------------------------------------------------------- */

    fn go_home(&mut self, folder: Option<String>) {
        let edited = matches!(&self.screen, Screen::Doc(_));
        if let Screen::Doc(dv) = &mut self.screen {
            dv.doc.save_all();
        }
        save_settings("", self.sort, self.tool, self.pi_font);
        self.screen = Screen::Home(HomeView::build(folder, self.sort));
        self.render_home(true);
        if edited {
            self.trigger_sync(); /* push this doc's edits to the web */
        }
    }

    /// Cheap periodic re-scan of the docs dir while on the home screen: if
    /// a background sync pulled in (or removed) a doc, rebuild the grid.
    fn home_rescan(&mut self) {
        self.home_rescan_at = Instant::now() + HOME_RESCAN;
        let (folder, cur_sig) = match &self.screen {
            Screen::Home(hv) if self.dialog.is_none() => (hv.folder.clone(), hv.sig.clone()),
            _ => return,
        };
        let nv = HomeView::build(folder, self.sort);
        if nv.sig != cur_sig {
            println!("paper: docs changed on disk — refreshing home");
            self.screen = Screen::Home(nv);
            self.render_home(false);
        }
    }

    fn rebuild_home(&mut self) {
        if let Screen::Home(hv) = &self.screen {
            let folder = hv.folder.clone();
            let top = hv.top_row;
            let mut nv = HomeView::build(folder, self.sort);
            nv.top_row = top.min(nv.cells.len().div_ceil(home::COLS as usize)
                .saturating_sub(1) / home::ROWS as usize * home::ROWS as usize);
            self.screen = Screen::Home(nv);
        }
        /* a home mutation (delete / rename / move / new) — push it to the
         * web now instead of waiting for the periodic timer */
        self.trigger_sync();
    }

    /// Kick a background web sync (fire-and-forget). Only on the device; the
    /// periodic timer is the backstop. Debounced by MIN_SYNC_GAP so a burst
    /// of edits doesn't spawn a pile of rsyncs.
    fn trigger_sync(&mut self) {
        if !self.takeover {
            return;
        }
        if self.last_sync.elapsed() < MIN_SYNC_GAP {
            return;
        }
        self.last_sync = Instant::now();
        let _ = std::process::Command::new("/home/root/bin/alt-ui-sync.sh")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    fn render_home(&mut self, flash: bool) {
        let Screen::Home(hv) = &self.screen else { return };
        statusbar::render(&mut self.fb, &self.status);
        hv.render(&mut self.fb, self.sort);
        if let Some(d) = &self.dialog {
            render_dialog(&mut self.fb, d, &self.screen);
        }
        if flash {
            self.disp.full_refresh();
        } else {
            self.disp.update(0, 0, FB_W, FB_H, Wave::Text);
        }
    }

    /// One lazily generated thumbnail per idle pass.
    fn thumb_tick(&mut self) {
        let Screen::Home(hv) = &mut self.screen else { return };
        if self.dialog.is_some() {
            return; /* don't repaint cells under a dialog */
        }
        let Some(i) = hv.generate_one_thumb() else { return };
        if let Some((x, y)) = hv.cell_rect(i) {
            hv.render_cell(&mut self.fb, i);
            self.disp.update(x, y, home::CELL_W, home::CELL_H, Wave::Text);
        }
    }

    fn home_tap(&mut self, x: i32, y: i32) {
        let Screen::Home(hv) = &self.screen else { return };
        /* header buttons */
        let by = STATUS_H + 40;
        let nx = FB_W - 48 - home::NEW_BTN_W;
        let sx = nx - 24 - home::SORT_BTN_W;
        if in_rect(x, y, nx, by, home::NEW_BTN_W, home::HDR_BTN_H) {
            let folder = hv.folder.clone().unwrap_or_default();
            let n = store::scan().iter().filter(|d| d.kind == doc::DocKind::Notebook).count();
            if let Some(id) = store::create_notebook(&format!("Notebook {}", n + 1), &folder) {
                self.open_doc(&id);
            }
            return;
        }
        if in_rect(x, y, sx, by, home::SORT_BTN_W, home::HDR_BTN_H) {
            self.sort = match self.sort {
                home::Sort::Opened => home::Sort::Title,
                home::Sort::Title => home::Sort::Opened,
            };
            save_settings("", self.sort, self.tool, self.pi_font);
            self.rebuild_home();
            self.render_home(false);
            return;
        }
        /* breadcrumb: tap the "< folder" title to go back up */
        if hv.folder.is_some() && y < home::GRID_Y0 && x < FB_W / 2 {
            self.go_home(None);
            return;
        }
        match hv.cell_at(x, y).map(|i| (i, &hv.cells[i])) {
            Some((_, home::Cell::Folder(f))) => {
                let f = f.clone();
                self.screen = Screen::Home(HomeView::build(Some(f), self.sort));
                self.render_home(false);
            }
            Some((_, home::Cell::Doc(d))) => {
                let id = d.id.clone();
                self.open_doc(&id);
            }
            None => {}
        }
    }

    fn home_hold(&mut self, x: i32, y: i32) {
        let Screen::Home(hv) = &self.screen else { return };
        if let Some(i) = hv.cell_at(x, y) {
            if matches!(hv.cells[i], home::Cell::Doc(_)) {
                self.dialog = Some(Dialog::DocMenu { idx: i });
                self.render_home(false);
            }
        }
    }

    /* -- documents ------------------------------------------------------ */

    fn open_doc(&mut self, id: &str) -> bool {
        match Doc::open(id) {
            Some(d) => {
                println!(
                    "paper: opened '{}' ({} pdf pages, {} entries, at {})",
                    d.title,
                    d.pdf_pages,
                    d.count(),
                    d.current + 1
                );
                save_settings(id, self.sort, self.tool, self.pi_font);
                self.dialog = None;
                self.screen = Screen::Doc(DocView::new(d, self.tool));
                self.flips_since_flash = 0;
                self.render_doc_full(true); /* clean GC16 on open */
                self.show_page_indicator();
                true
            }
            None => {
                println!("paper: could not open doc '{id}'");
                false
            }
        }
    }

    /// Repaint the whole document page, matching the stock app's page turn.
    ///
    /// `flash=true` is the GC16 deghost flash (clean, but the whole-page black
    /// blink) — used to open a doc and periodically to reset ghost buildup.
    ///
    /// `flash=false` is the everyday turn, and the waveform depends on what's
    /// on the page:
    ///
    ///   - a PDF raster (fine antialiased print) turns with GL16+FULL
    ///     (Wave::Print): a FULL update drives every pixel with the real
    ///     16-level LUT so the grey letterforms and rules render smooth — a
    ///     partial pass approximates greys with a fast, speckled waveform and
    ///     shreds fine print to salt-and-pepper. GL16 (not GC16) means no
    ///     clearing phase, so the page eases over without a flash. The raster
    ///     is contrast-lifted to true black on load (doc.rs boost_contrast) so
    ///     the body text reads as bright as stock.
    ///   - a notebook page (bold vector ink) turns with partial GC16
    ///     (Wave::Page): the hard rail drive keeps thick strokes crisp, and
    ///     there are no fine greys for it to speckle.
    ///
    /// Ghosting is reset by a GC16 flash every FLIP_DEGHOST_EVERY turns. The
    /// toolbar is drawn into the frame (blit_toolbar, no separate push) so it
    /// repaints in step with the page instead of lagging a frame behind.
    fn render_doc_full(&mut self, flash: bool) {
        let has_raster = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            dv.ink_settle = None; /* the whole-page repaint below supersedes any settle */
            dv.ink_settle_at = None;
            dv.doc.render_full(&mut self.fb);
            dv.doc.has_raster()
        };
        self.blit_toolbar();
        if flash {
            self.disp.full_refresh();
        } else {
            let wave = if has_raster { Wave::Print } else { Wave::Page };
            self.disp.update(0, 0, FB_W, FB_H, wave);
        }
    }

    /// Repaint a region of the document from the model. Over a grayscale
    /// page raster the 16-level waveform keeps the print clean; plain ink
    /// pages take DU. The rubber path forces Wave::Ink (GL16 is far too
    /// slow mid-scrub; the post-erase deghost flash heals the print).
    fn render_doc_region_wave(&mut self, r: Rect, wave: Option<Wave>) {
        match &self.screen {
            Screen::Doc(dv) => {
                let r = r.clamp_screen();
                let had_gray = dv.doc.render_region(&mut self.fb, r);
                let w = wave.unwrap_or(if had_gray { Wave::Text } else { Wave::Ink });
                self.disp.update(r.x0, r.y0, r.w(), r.h(), w);
            }
            Screen::Home(_) => self.render_home(false),
        }
    }

    fn render_doc_region(&mut self, r: Rect) {
        self.render_doc_region_wave(r, None);
    }

    /// Chrome that must survive a content repaint of `r`: the revealed top
    /// bar and the toolbar (the M4 selection overlay hooks in here too).
    fn restore_chrome_over(&mut self, r: Rect) {
        if self.bar_until.is_some() && r.y0 < BAR_H {
            self.paint_top_bar();
        }
        if let Screen::Doc(dv) = &self.screen {
            let t = toolbar::tb_rect(dv.tb_open);
            if r.x1 >= t.x0 && r.x0 <= t.x1 && r.y1 >= t.y0 && r.y0 <= t.y1 {
                self.paint_toolbar();
            }
        }
        let sel_hit = {
            let Screen::Doc(dv) = &self.screen else { return };
            dv.selection.as_ref().map(|s| s.chrome_rect()).is_some_and(|c| {
                r.x1 >= c.x0 && r.x0 <= c.x1 && r.y1 >= c.y0 && r.y0 <= c.y1
            })
        };
        if sel_hit {
            self.repaint_selection_chrome();
        }
    }

    /* -- page indicator -- */

    fn indicator_rect(&self) -> Rect {
        Rect { x0: FB_W / 2 - 160, y0: FB_H - 56, x1: FB_W / 2 + 160, y1: FB_H - 10 }
    }

    fn show_page_indicator(&mut self) {
        let Screen::Doc(dv) = &mut self.screen else { return };
        let label = dv.doc.label(dv.doc.current);
        let label = if label.is_empty() {
            format!("{} / {}", dv.doc.current + 1, dv.doc.count())
        } else {
            format!("{} / {}  -  {}", dv.doc.current + 1, dv.doc.count(), label)
        };
        dv.indicator_until = Some(Instant::now() + INDICATOR_TTL);
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
    }

    fn clear_page_indicator(&mut self) {
        if let Screen::Doc(dv) = &mut self.screen {
            dv.indicator_until = None;
        }
        let r = self.indicator_rect();
        self.render_doc_region(r);
        self.restore_chrome_over(r);
    }

    /* -- page turning -- */

    fn flip(&mut self, delta: i32) {
        self.cancel_lasso();
        self.dismiss_selection();
        let Screen::Doc(dv) = &mut self.screen else { return };
        let flipped = dv.doc.flip(delta);
        if !flipped {
            self.show_page_indicator(); /* at the edge: just show where we are */
            return;
        }
        dv.cur_stroke = None;
        dv.page_changed = false;
        dv.deghost_at = None;
        dv.idle_at = None;
        /* pending animation strokes are already in the model; the full
         * repaint below shows them instantly on whatever page has them */
        dv.anim.clear();
        dv.anim_dirty = None;
        dv.anim_settle = None;
        println!(
            "paper: page {} / {} {}",
            dv.doc.current + 1,
            dv.doc.count(),
            dv.doc.label(dv.doc.current)
        );
        self.flips_since_flash += 1;
        let flash = self.flips_since_flash >= FLIP_DEGHOST_EVERY;
        if flash {
            self.flips_since_flash = 0;
        }
        self.render_doc_full(flash);
        self.show_page_indicator();
    }

    /// Jump straight to a 1-based page (the numpad's OK).
    fn jump_to(&mut self, page1: usize) {
        self.cancel_lasso();
        self.dismiss_selection();
        let Screen::Doc(dv) = &mut self.screen else { return };
        let target = page1.saturating_sub(1).min(dv.doc.count().saturating_sub(1));
        if target != dv.doc.current {
            dv.doc.goto(target);
            dv.cur_stroke = None;
            dv.page_changed = false;
            dv.deghost_at = None;
            dv.idle_at = None;
            dv.anim.clear();
            dv.anim_dirty = None;
            dv.anim_settle = None;
        }
        self.flips_since_flash += 1;
        let flash = self.flips_since_flash >= FLIP_DEGHOST_EVERY;
        if flash {
            self.flips_since_flash = 0;
        }
        self.render_doc_full(flash);
        self.show_page_indicator();
    }

    /* -- pen -- */

    fn route_pen(&mut self, phase: PenPhase, x: i32, y: i32, pressure: i32, rubber: bool) {
        self.last_pen = Some(Instant::now());
        if self.dialog.is_some() {
            if phase == PenPhase::Press {
                self.dialog_press(x, y);
            }
            return;
        }
        if self.bar_until.is_some() && phase == PenPhase::Press && y < BAR_H {
            self.top_bar_press(x, y);
            return;
        }
        match &self.screen {
            Screen::Home(_) => {
                if phase == PenPhase::Press {
                    self.home_tap(x, y);
                }
            }
            Screen::Doc(_) => self.doc_pen(phase, x, y, pressure, rubber),
        }
    }

    fn doc_pen(&mut self, phase: PenPhase, x: i32, y: i32, pressure: i32, rubber: bool) {
        /* the toolbar swallows presses only — a stroke that started on the
         * canvas may finish under it (chrome repaints over the ink) */
        if phase == PenPhase::Press {
            let Screen::Doc(dv) = &self.screen else { return };
            if dv.cur_stroke.is_none() {
                if let Some(a) = toolbar::hit(x, y, dv.tb_open) {
                    self.toolbar_action(a);
                    return;
                }
            }
        }
        let tool = match &self.screen {
            Screen::Doc(dv) => dv.tool,
            _ => return,
        };
        match phase {
            PenPhase::Press | PenPhase::Move => {
                self.last_contact = Some(Instant::now());
                if phase == PenPhase::Press && (rubber || tool != Tool::Lasso) {
                    /* editing again: hold the pause trigger. Lassoing is NOT
                     * an edit — it must not cancel a pending co-writer pause */
                    if let Screen::Doc(dv) = &mut self.screen {
                        dv.idle_at = None;
                    }
                }
                if rubber || tool == Tool::Eraser {
                    /* the hardware rubber always erases, whatever the tool;
                     * commit any open stroke and drop any selection first
                     * so what's on the glass is what's in the model */
                    self.commit_open_stroke();
                    if phase == PenPhase::Press {
                        self.cancel_lasso();
                        self.dismiss_selection();
                    }
                    self.erase_pass(x as f32, y as f32);
                } else if tool == Tool::Pen {
                    self.ink_pass(phase, x, y, pressure);
                } else {
                    self.lasso_pen(phase, x, y);
                }
            }
            PenPhase::Release => {
                self.last_contact = Some(Instant::now());
                if tool == Tool::Lasso {
                    self.lasso_pen(phase, x, y);
                }
                self.commit_open_stroke();
                self.flush_erase_op();
                if let Screen::Doc(dv) = &mut self.screen {
                    if dv.contact_changed {
                        dv.contact_changed = false;
                        dv.idle_at = Some(Instant::now() + IDLE_DELAY);
                    }
                }
            }
        }
    }

    /* -- the pi working dot -- */

    fn draw_working_dot(&mut self) {
        let r = DOT_RECT;
        let (cx, cy) = ((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2);
        self.fb.disc(cx, cy, 8, GRAY);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
    }

    fn clear_working_dot(&mut self) {
        let r = DOT_RECT;
        match &self.screen {
            Screen::Doc(_) => {
                self.render_doc_region_wave(r, Some(Wave::Ink));
                self.restore_chrome_over(r);
            }
            Screen::Home(_) => {
                self.fb.fill_rect(r.x0, r.y0, r.w(), r.h(), WHITE);
                self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
            }
        }
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

    /* -- the lasso tool -- */

    fn lasso_pen(&mut self, phase: PenPhase, x: i32, y: i32) {
        match phase {
            PenPhase::Press => {
                /* a press on the chip bar acts; inside the box drags;
                 * anywhere else dismisses and starts a fresh loop */
                let action = {
                    let Screen::Doc(dv) = &mut self.screen else { return };
                    match &mut dv.selection {
                        Some(sel) => match sel.chip_at(x, y) {
                            Some(select::Chip::Delete) => 1,
                            Some(select::Chip::Cut) => 2,
                            None if sel.contains(x, y) => {
                                sel.drag_from = Some((x, y));
                                3
                            }
                            None => 0,
                        },
                        None => 0,
                    }
                };
                match action {
                    1 => self.selection_delete(false),
                    2 => self.selection_delete(true),
                    3 => {}
                    _ => {
                        self.dismiss_selection();
                        self.flush_anim(); /* select what's IN the model, visibly */
                        if let Screen::Doc(dv) = &mut self.screen {
                            dv.lasso = Some(select::Lasso::new(x as f32, y as f32));
                        }
                    }
                }
            }
            PenPhase::Move => {
                let Screen::Doc(dv) = &mut self.screen else { return };
                if dv.selection.as_ref().is_some_and(|s| s.drag_from.is_some()) {
                    self.drag_move(x, y);
                } else if let Some(l) = &mut dv.lasso {
                    if let Some(prev) = l.extend(x as f32, y as f32) {
                        let seg = select::draw_trail_segment(&mut self.fb, prev, (x as f32, y as f32));
                        dv.ink_dirty = Some(match dv.ink_dirty {
                            None => seg,
                            Some(d) => d.union(seg),
                        });
                    }
                }
            }
            PenPhase::Release => {
                let dragging = {
                    let Screen::Doc(dv) = &self.screen else { return };
                    dv.selection.as_ref().is_some_and(|s| s.drag_from.is_some())
                };
                if dragging {
                    self.drag_commit();
                } else {
                    self.close_lasso();
                }
            }
        }
    }

    /// Live drag: the logical offset always tracks the pen; only the
    /// dashed chrome repaints, throttled. Ink stays put until release.
    fn drag_move(&mut self, x: i32, y: i32) {
        let paint = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let Some(sel) = &mut dv.selection else { return };
            let Some((fx, fy)) = sel.drag_from else { return };
            let (dx, dy) = sel.clamp_offset(x - fx, y - fy);
            if (dx, dy) == (sel.dx, sel.dy) {
                return;
            }
            sel.dx = dx;
            sel.dy = dy;
            if dv.last_drag_paint.elapsed() < Duration::from_millis(30) {
                false
            } else {
                dv.last_drag_paint = Instant::now();
                true
            }
        };
        if paint {
            self.repaint_selection_chrome();
        }
    }

    /// Release after a drag: one vector translate + one model re-render.
    fn drag_commit(&mut self) {
        let dirty = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let Some(sel) = &mut dv.selection else { return };
            let (dx, dy) = (sel.dx, sel.dy);
            sel.drag_from = None;
            if (dx, dy) == (0, 0) {
                None
            } else {
                let dirty = dv.doc.page.translate_strokes(&sel.refs, dx as f32, dy as f32);
                sel.bbox = Rect {
                    x0: sel.bbox.x0 + dx,
                    y0: sel.bbox.y0 + dy,
                    x1: sel.bbox.x1 + dx,
                    y1: sel.bbox.y1 + dy,
                };
                sel.dx = 0;
                sel.dy = 0;
                let refs = sel.refs.clone();
                let key = dv.doc.cur_ink_path();
                dv.undo.stack(&key).push(EditOp::MoveStrokes {
                    refs,
                    dx: dx as f32,
                    dy: dy as f32,
                });
                dv.page_changed = true;
                dv.deghost_at = Some(Instant::now() + Duration::from_millis(1200));
                dirty
            }
        };
        if let Some(d) = dirty {
            let r = d.pad(4).clamp_screen();
            self.render_doc_region(r);
            self.restore_chrome_over(r);
        }
        self.repaint_selection_chrome();
        self.paint_toolbar(); /* undo became available */
        self.rearm_pause();
    }

    /// Close the loop: erase the trail, select what's inside.
    fn close_lasso(&mut self) {
        let (trail, refs, bbox) = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let Some(l) = dv.lasso.take() else { return };
            let trail = l.bbox().pad(6);
            if !l.viable() {
                (trail, Vec::new(), None)
            } else {
                let (refs, bbox) = select::select_strokes(&dv.doc.page, &l.pts);
                (trail, refs, bbox)
            }
        };
        let r = trail.clamp_screen();
        self.render_doc_region_wave(r, Some(Wave::Ink));
        self.restore_chrome_over(r);
        if let (false, Some(b)) = (refs.is_empty(), bbox) {
            if let Screen::Doc(dv) = &mut self.screen {
                dv.selection = Some(select::Selection::new(refs, b));
            }
            self.repaint_selection_chrome();
        }
    }

    fn cancel_lasso(&mut self) {
        let trail = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            dv.lasso.take().map(|l| l.bbox().pad(6))
        };
        if let Some(t) = trail {
            let r = t.clamp_screen();
            self.render_doc_region_wave(r, Some(Wave::Ink));
            self.restore_chrome_over(r);
        }
    }

    /// Repaint the selection chrome at its CURRENT logical spot: re-render
    /// the model under wherever it was last painted, draw it anew, push
    /// one update over the union.
    fn repaint_selection_chrome(&mut self) {
        let (old, new) = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            (dv.sel_chrome.take(), dv.selection.as_ref().map(|s| s.chrome_rect()))
        };
        let mut span: Option<Rect> = None;
        if let Some(o) = old {
            let o = o.clamp_screen();
            /* re-render the model under the old chrome (thin strips would
             * be cheaper; the union is simpler and still small) */
            if let Screen::Doc(dv) = &self.screen {
                dv.doc.render_region(&mut self.fb, o);
            }
            span = Some(o);
        }
        if let Some(n) = new {
            let n = n.clamp_screen();
            if let Screen::Doc(dv) = &self.screen {
                if let Some(sel) = &dv.selection {
                    select::draw_selection(&mut self.fb, sel);
                }
            }
            span = Some(span.map_or(n, |s| s.union(n)));
        }
        if let Screen::Doc(dv) = &mut self.screen {
            dv.sel_chrome = new;
        }
        if let Some(s) = span {
            self.disp.update(s.x0, s.y0, s.w(), s.h(), Wave::Ink);
        }
    }

    fn dismiss_selection(&mut self) {
        let chrome = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let painted = dv.sel_chrome.take();
            dv.selection.take().map(|s| painted.unwrap_or_else(|| s.chrome_rect()))
        };
        if let Some(c) = chrome {
            let r = c.clamp_screen();
            self.render_doc_region_wave(r, Some(Wave::Ink));
            self.restore_chrome_over(r);
            self.rearm_pause();
        }
    }

    /// After a selection is dismissed/committed, let a still-pending page
    /// change reach pi (the lasso interaction suppressed it meanwhile).
    fn rearm_pause(&mut self) {
        if let Screen::Doc(dv) = &mut self.screen {
            if dv.page_changed && dv.idle_at.is_none() {
                dv.idle_at = Some(Instant::now() + IDLE_DELAY);
            }
        }
    }

    /// The chip bar: DELETE drops the selected strokes; CUT also stashes
    /// them (normalized) in the clipboard.
    fn selection_delete(&mut self, cut: bool) {
        let gone = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let Some(sel) = dv.selection.take() else { return };
            let chrome = dv.sel_chrome.take().unwrap_or_else(|| sel.chrome_rect());
            let (lifted, gone) = dv.doc.page.remove_strokes_by_ids(&sel.refs);
            if cut && !lifted.is_empty() {
                let origin = gone.unwrap_or(sel.bbox);
                self.clipboard = Some(
                    lifted
                        .iter()
                        .map(|o| {
                            let mut s = o.stroke.clone();
                            for p in &mut s.pts {
                                p.x -= origin.x0 as f32;
                                p.y -= origin.y0 as f32;
                            }
                            s
                        })
                        .collect(),
                );
            }
            if !lifted.is_empty() {
                let key = dv.doc.cur_ink_path();
                dv.undo.stack(&key).push(EditOp::erased(lifted));
                dv.page_changed = true;
                dv.deghost_at = Some(Instant::now() + Duration::from_millis(1200));
            }
            gone.map_or(chrome, |g| g.union(chrome))
        };
        let r = gone.pad(4).clamp_screen();
        self.render_doc_region_wave(r, Some(Wave::Ink));
        self.restore_chrome_over(r);
        self.paint_toolbar();
        self.rearm_pause();
    }

    /// Land the in-progress stroke in the page model (one undoable op).
    fn commit_open_stroke(&mut self) {
        let Screen::Doc(dv) = &mut self.screen else { return };
        let Some(s) = dv.cur_stroke.take() else { return };
        if s.pts.is_empty() {
            return;
        }
        let id = dv.doc.page.push_stroke(s);
        dv.contact_changed = true;
        dv.page_changed = true;
        let key = dv.doc.cur_ink_path();
        dv.undo.stack(&key).push(EditOp::AddStroke { id, stroke: None });
        self.paint_toolbar();
    }

    /// One rubber contact = one undoable op (accumulated across the scrub).
    fn flush_erase_op(&mut self) {
        let Screen::Doc(dv) = &mut self.screen else { return };
        if dv.pending_erase.is_empty() {
            return;
        }
        let lifted = std::mem::take(&mut dv.pending_erase);
        let key = dv.doc.cur_ink_path();
        dv.undo.stack(&key).push(EditOp::erased(lifted));
        self.paint_toolbar();
    }

    fn ink_pass(&mut self, phase: PenPhase, x: i32, y: i32, pressure: i32) {
        let Screen::Doc(dv) = &mut self.screen else { return };
        let p = Pt { x: x as f32, y: y as f32, r: brush_r(pressure) };
        let prev = match (&mut dv.cur_stroke, phase) {
            (Some(s), PenPhase::Move) => {
                let prev = *s.pts.last().unwrap();
                s.pts.push(p);
                prev
            }
            _ => {
                /* Press, or Move with no open stroke (e.g. after an erase) */
                dv.cur_stroke = Some(Stroke { id: 0, pts: vec![p], gray: ink::USER_GRAY });
                p
            }
        };
        ink::stamp_segment(&mut self.fb, prev, p, ink::USER_GRAY);
        let seg = Rect {
            x0: (prev.x.min(p.x) - prev.r.max(p.r)) as i32,
            y0: (prev.y.min(p.y) - prev.r.max(p.r)) as i32,
            x1: (prev.x.max(p.x) + prev.r.max(p.r)).ceil() as i32,
            y1: (prev.y.max(p.y) + prev.r.max(p.r)).ceil() as i32,
        };
        dv.ink_dirty = Some(match dv.ink_dirty {
            None => seg,
            Some(d) => d.union(seg),
        });
        /* writing over print: remember where, heal with GL16 once settled */
        if dv.doc.has_raster() {
            let r = seg.pad(6);
            dv.ink_settle = Some(match dv.ink_settle {
                None => r,
                Some(s) => s.union(r),
            });
            dv.ink_settle_at = Some(Instant::now() + Duration::from_millis(800));
        }
    }

    fn erase_pass(&mut self, x: f32, y: f32) {
        /* a Garamond run is typeset, not stroke geometry — the rubber
         * removes its whole patch (one undoable ErasePatch), like pi's own
         * erase */
        let text_patch = {
            let Screen::Doc(dv) = &self.screen else { return };
            dv.doc.page.text_patch_at(x, y, ERASER_R)
        };
        if let Some(id) = text_patch {
            let bb = {
                let Screen::Doc(dv) = &mut self.screen else { return };
                let Some((body, bb)) = dv.doc.page.take_patch(id) else { return };
                let key = dv.doc.cur_ink_path();
                dv.undo.stack(&key).push(EditOp::ErasePatch { id, body: Some(body) });
                dv.contact_changed = true;
                dv.page_changed = true;
                dv.deghost_at = Some(Instant::now() + Duration::from_millis(1100));
                dv.anim.retain(|a| a.patch != id);
                bb
            };
            let r = bb.pad(4).clamp_screen();
            self.render_doc_region_wave(r, Some(Wave::Ink));
            self.restore_chrome_over(r);
            self.paint_toolbar();
            return;
        }
        let gone = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let Some((gone, lifted)) = dv.doc.page.erase_at(x, y, ERASER_R) else { return };
            dv.pending_erase.extend(lifted);
            dv.contact_changed = true;
            dv.page_changed = true;
            /* DU-erased black ink ghosts badly; flash once the scrubbing
             * settles */
            dv.deghost_at = Some(Instant::now() + Duration::from_millis(1100));
            /* un-animated strokes in the region must appear now that we
             * repaint from the model; drop their pacing entries */
            let cur = dv.doc.current;
            let mut region = gone;
            dv.anim.retain(|a| {
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
            region
        };
        let r = gone.pad(4).clamp_screen();
        self.render_doc_region_wave(r, Some(Wave::Ink));
        self.restore_chrome_over(r);
    }

    /// Flush every queued animation stroke for the current page into view
    /// (the model already holds them) — selection must see all the ink.
    fn flush_anim(&mut self) {
        let region = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let cur = dv.doc.current;
            let mut region: Option<Rect> = None;
            dv.anim.retain(|a| {
                if a.page == cur {
                    region = Some(region.map_or(a.bbox, |r| r.union(a.bbox)));
                    false
                } else {
                    true
                }
            });
            dv.anim_dirty = None;
            dv.anim_settle = None;
            region
        };
        if let Some(r) = region {
            let r = r.pad(4).clamp_screen();
            self.render_doc_region(r);
            self.restore_chrome_over(r);
        }
    }

    /* -- the toolbar -- */

    /// Draw the toolbar into the framebuffer WITHOUT pushing it to the panel.
    /// Used to keep the toolbar present in a full-screen frame (page turns)
    /// so it doesn't flash white while the page around it repaints.
    fn blit_toolbar(&mut self) {
        let Screen::Doc(dv) = &self.screen else { return };
        let key = dv.doc.cur_ink_path();
        let (cu, cr) = dv
            .undo
            .peek(&key)
            .map_or((false, false), |s| (s.can_undo(), s.can_redo()));
        toolbar::paint(
            &mut self.fb,
            dv.tb_open,
            dv.tool,
            cu,
            cr,
            (dv.doc.current + 1, dv.doc.count()),
        );
    }

    fn paint_toolbar(&mut self) {
        self.blit_toolbar();
        let Screen::Doc(dv) = &self.screen else { return };
        let r = toolbar::tb_rect(dv.tb_open);
        self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
    }

    fn toolbar_action(&mut self, a: TbAction) {
        match a {
            TbAction::Swallow => {}
            TbAction::Toggle => {
                let was_open = {
                    let Screen::Doc(dv) = &mut self.screen else { return };
                    let was = dv.tb_open;
                    dv.tb_open = !was;
                    was
                };
                if was_open {
                    /* collapsing: re-render the strip area from the model */
                    let r = toolbar::tb_rect(true);
                    self.render_doc_region(r);
                }
                self.paint_toolbar();
            }
            TbAction::Tool(t) => {
                self.cancel_lasso();
                self.dismiss_selection();
                if let Screen::Doc(dv) = &mut self.screen {
                    dv.tool = t;
                }
                self.tool = t;
                save_settings(&self.doc_id(), self.sort, t, self.pi_font);
                self.paint_toolbar();
            }
            TbAction::Undo => self.undo_action(false),
            TbAction::Redo => self.undo_action(true),
            TbAction::PagePrev => self.flip(-1),
            TbAction::PageNext => self.flip(1),
            TbAction::GoTo => {
                self.dialog = Some(Dialog::GoTo { entry: String::new() });
                self.render_goto_dialog();
            }
            TbAction::Font => {
                self.dialog = Some(Dialog::FontPick);
                self.render_font_dialog();
            }
            TbAction::Home => {
                self.go_home(None);
            }
        }
    }

    fn doc_id(&self) -> String {
        match &self.screen {
            Screen::Doc(dv) => dv.doc.id.clone(),
            Screen::Home(_) => String::new(),
        }
    }

    fn undo_action(&mut self, redo: bool) {
        /* history rewinds under a selection would strand its refs */
        self.cancel_lasso();
        self.dismiss_selection();
        let dirty = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let key = dv.doc.cur_ink_path();
            let stack = dv.undo.stack(&key);
            let dirty = if redo { stack.redo(&mut dv.doc.page) } else { stack.undo(&mut dv.doc.page) };
            if dirty.is_some() {
                dv.doc.page.dirty = true;
                dv.page_changed = true;
            }
            dirty
        };
        /* repaint the affected region if there was one — but ALWAYS refresh
         * the toolbar so the undo/redo enabled state tracks the stacks even
         * when an op produced no visible change */
        if let Some(d) = dirty {
            let r = d.pad(4).clamp_screen();
            self.render_doc_region(r);
            self.restore_chrome_over(r);
        }
        self.paint_toolbar();
    }

    /* -- touch: taps, holds, flips, scrolls, the top bar -- */

    fn route_touch(&mut self, phase: Phase, x: i32, y: i32) {
        if self.last_pen.is_some_and(|t| t.elapsed() < PEN_TIMEOUT) {
            return; /* palm rejection */
        }
        if self.dialog.is_some() {
            if phase == Phase::Press {
                self.dialog_press(x, y);
            }
            return;
        }
        match phase {
            Phase::Press => {
                if self.bar_until.is_some() && y < BAR_H {
                    self.top_bar_press(x, y);
                    return;
                }
                if y <= EDGE_Y {
                    self.swipe_from = Some(y);
                    return;
                }
                /* finger taps work the toolbar too */
                if let Screen::Doc(dv) = &self.screen {
                    if let Some(a) = toolbar::hit(x, y, dv.tb_open) {
                        self.toolbar_action(a);
                        return;
                    }
                }
                self.touch_start = Some((x, y));
                self.touch_t0 = Some(Instant::now());
                self.touch_last = (x, y);
            }
            Phase::Move => {
                if let Some(sy) = self.swipe_from {
                    if y - sy >= SWIPE_DIST {
                        self.swipe_from = None;
                        self.show_top_bar();
                    }
                    return;
                }
                if self.touch_start.is_some() {
                    self.touch_last = (x, y);
                }
            }
            Phase::Release => {
                self.swipe_from = None;
                let Some((sx, sy)) = self.touch_start.take() else { return };
                let held = self.touch_t0.take().map_or(0, |t| t.elapsed().as_millis());
                let (dx, dy) = (self.touch_last.0 - sx, self.touch_last.1 - sy);
                match &self.screen {
                    Screen::Doc(_) => {
                        if dx.abs() >= FLIP_DX && dy.abs() <= FLIP_DY_MAX {
                            /* swipe left = next page (turning forward) */
                            self.flip(if dx < 0 { 1 } else { -1 });
                        }
                    }
                    Screen::Home(_) => {
                        if dy.abs() >= FLIP_DX && dx.abs() <= FLIP_DY_MAX {
                            let Screen::Home(hv) = &mut self.screen else { return };
                            if hv.scroll(dy < 0) {
                                self.render_home(false);
                            }
                        } else if dx.abs() < 40 && dy.abs() < 40 {
                            if held >= HOLD_MS {
                                self.home_hold(sx, sy);
                            } else {
                                self.home_tap(sx, sy);
                            }
                        }
                    }
                }
            }
        }
    }

    /* -- the top bar (revealed by the top-edge swipe) -- */

    fn paint_top_bar(&mut self) {
        self.fb.fill_rect(0, 0, FB_W, BAR_H, WHITE);
        self.fb.fill_rect(0, BAR_H - 2, FB_W, 2, BLACK);
        /* CLOSE */
        self.fb.fill_rect(CLOSE_X0, CLOSE_Y0, CLOSE_BTN_W, CLOSE_BTN_H, BLACK);
        let label = "X CLOSE";
        self.fb.text(
            CLOSE_X0 + (CLOSE_BTN_W - text_width(label, 3)) / 2,
            CLOSE_Y0 + (CLOSE_BTN_H - 21) / 2,
            label,
            3,
            WHITE,
        );
        /* MY FILES (only useful inside a doc) + title */
        if let Screen::Doc(dv) = &self.screen {
            self.fb.rect_outline(FILES_X0, CLOSE_Y0, FILES_BTN_W, CLOSE_BTN_H, 2, BLACK);
            let fl = "MY FILES";
            self.fb.text(
                FILES_X0 + (FILES_BTN_W - text_width(fl, 3)) / 2,
                CLOSE_Y0 + (CLOSE_BTN_H - 21) / 2,
                fl,
                3,
                BLACK,
            );
            let mut t = dv.doc.title.clone();
            while text::width(text::Face::Body, 32.0, &t) > FB_W - 800 && t.chars().count() > 4 {
                t = t.chars().take(t.chars().count() - 4).collect();
                t.push_str("..");
            }
            text::draw_line(&mut self.fb, FILES_X0 + FILES_BTN_W + 32, 26, text::Face::Body, 32.0, &t);
        }
        /* status cluster, right: clock + battery */
        let clock = format!("{:02}:{:02}", self.status.hm.0, self.status.hm.1);
        let pct = if self.status.batt_pct >= 0 {
            format!("{}%", self.status.batt_pct)
        } else {
            "--".into()
        };
        let s = format!("{clock}   {pct}");
        let w = text::width(text::Face::Body, 30.0, &s);
        text::draw_line(&mut self.fb, FB_W - 32 - w, 28, text::Face::Body, 30.0, &s);
        self.disp.update(0, 0, FB_W, BAR_H, Wave::Ink);
    }

    fn show_top_bar(&mut self) {
        let _ = self.status.refresh();
        self.bar_until = Some(Instant::now() + BAR_TTL);
        self.paint_top_bar();
    }

    fn dismiss_top_bar(&mut self) {
        self.bar_until = None;
        let r = Rect { x0: 0, y0: 0, x1: FB_W - 1, y1: BAR_H - 1 };
        match &self.screen {
            Screen::Doc(_) => self.render_doc_region(r),
            Screen::Home(_) => self.render_home(false),
        }
    }

    fn top_bar_press(&mut self, x: i32, y: i32) {
        if in_rect(x, y, CLOSE_X0, CLOSE_Y0, CLOSE_BTN_W, CLOSE_BTN_H) {
            println!("paper: close button");
            RUNNING.store(false, Ordering::Relaxed);
            return;
        }
        if matches!(self.screen, Screen::Doc(_))
            && in_rect(x, y, FILES_X0, CLOSE_Y0, FILES_BTN_W, CLOSE_BTN_H)
        {
            self.bar_until = None;
            self.go_home(None);
            return;
        }
        self.dismiss_top_bar();
    }

    /* -- dialogs -- */

    fn dialog_press(&mut self, x: i32, y: i32) {
        let Some(dialog) = self.dialog.take() else { return };
        match dialog {
            Dialog::DocMenu { idx } => {
                let rows = ["RENAME", "MOVE TO FOLDER", "DUPLICATE", "DELETE", "CANCEL"];
                match dialog_row_at(rows.len(), x, y) {
                    Some(0) => {
                        let title = self.cell_doc_title(idx);
                        self.dialog = Some(Dialog::Rename { idx, kb: Kb::new("Rename", title) });
                    }
                    Some(1) => self.dialog = Some(Dialog::MoveTo { idx }),
                    Some(2) => {
                        if let Some(id) = self.cell_doc_id(idx) {
                            store::duplicate(&id);
                        }
                        self.rebuild_home();
                    }
                    Some(3) => self.dialog = Some(Dialog::ConfirmDelete { idx }),
                    _ => {}
                }
            }
            Dialog::ConfirmDelete { idx } => {
                let rows = ["DELETE", "CANCEL"];
                if let Some(0) = dialog_row_at(rows.len(), x, y) {
                    if let Some(id) = self.cell_doc_id(idx) {
                        store::delete(&id);
                        println!("paper: deleted '{id}'");
                    }
                }
                self.rebuild_home();
            }
            Dialog::Rename { idx, mut kb } => match kb.press(x, y) {
                KbAction::Ok(t) => {
                    if let Some(id) = self.cell_doc_id(idx) {
                        if !t.is_empty() {
                            store::rename(&id, &t);
                        }
                    }
                    self.rebuild_home();
                }
                KbAction::Cancel => {}
                KbAction::Edited => self.dialog = Some(Dialog::Rename { idx, kb }),
            },
            Dialog::MoveTo { idx } => {
                let folders = store::folders();
                let mut rows: Vec<String> = vec!["( my files )".into()];
                rows.extend(folders.iter().cloned());
                rows.push("NEW FOLDER ...".into());
                rows.push("CANCEL".into());
                match dialog_row_at(rows.len(), x, y) {
                    Some(0) => {
                        if let Some(id) = self.cell_doc_id(idx) {
                            store::set_folder(&id, "");
                        }
                        self.rebuild_home();
                    }
                    Some(i) if i <= folders.len() => {
                        if let Some(id) = self.cell_doc_id(idx) {
                            store::set_folder(&id, &folders[i - 1]);
                        }
                        self.rebuild_home();
                    }
                    Some(i) if i == folders.len() + 1 => {
                        self.dialog =
                            Some(Dialog::NewFolder { idx, kb: Kb::new("New folder", String::new()) });
                    }
                    _ => {}
                }
            }
            Dialog::NewFolder { idx, mut kb } => match kb.press(x, y) {
                KbAction::Ok(t) => {
                    if !t.is_empty() {
                        store::add_folder(&t);
                        if let Some(id) = self.cell_doc_id(idx) {
                            store::set_folder(&id, &t);
                        }
                    }
                    self.rebuild_home();
                }
                KbAction::Cancel => {}
                KbAction::Edited => self.dialog = Some(Dialog::NewFolder { idx, kb }),
            },
            Dialog::GoTo { mut entry } => {
                match np_press(x, y) {
                    NpAction::Digit(d) => {
                        if entry.len() < 4 {
                            entry.push(d);
                        }
                        self.dialog = Some(Dialog::GoTo { entry });
                        self.render_goto_dialog();
                    }
                    NpAction::Del => {
                        entry.pop();
                        self.dialog = Some(Dialog::GoTo { entry });
                        self.render_goto_dialog();
                    }
                    NpAction::Ok => {
                        if let Ok(n) = entry.parse::<usize>() {
                            if n >= 1 {
                                self.jump_to(n);
                                return; /* full repaint covered everything */
                            }
                        }
                        self.dismiss_doc_dialog();
                    }
                    NpAction::Swallow => {
                        self.dialog = Some(Dialog::GoTo { entry });
                    }
                    NpAction::Outside => self.dismiss_doc_dialog(),
                }
                return; /* doc-screen dialog: no home repaint */
            }
            Dialog::FontPick => {
                self.font_dialog_press(x, y);
                return; /* doc-screen dialog: no home repaint */
            }
        }
        self.render_home(false);
    }

    fn dismiss_doc_dialog(&mut self) {
        self.dialog = None;
        let r = np_rect().pad(8);
        self.render_doc_region(r);
        self.restore_chrome_over(r);
    }

    /* -- pi handwriting font picker -- */

    fn render_font_dialog(&mut self) {
        let cur = self.pi_font;
        let mut rows: Vec<String> = PiFont::ALL
            .iter()
            .map(|f| {
                let mark = if *f == cur { " *" } else { "" };
                format!("{}{}", f.label(), mark)
            })
            .collect();
        rows.push("CANCEL".into());
        draw_dialog_rows(&mut self.fb, "pi handwriting", &rows);
        let r = dialog_rect(rows.len());
        self.disp.update(r.x0 - 6, r.y0 - 6, r.w() + 12, r.h() + 12, Wave::Ink);
    }

    fn font_dialog_press(&mut self, x: i32, y: i32) {
        let n = PiFont::ALL.len() + 1;
        match dialog_row_at(n, x, y) {
            Some(i) if i < PiFont::ALL.len() => {
                self.pi_font = PiFont::ALL[i];
                save_settings(&self.doc_id(), self.sort, self.tool, self.pi_font);
                println!("paper: pi font -> {}", self.pi_font.key());
                self.dialog = None;
                self.dismiss_font_dialog(n);
            }
            _ => {
                self.dialog = None;
                self.dismiss_font_dialog(n);
            }
        }
    }

    fn dismiss_font_dialog(&mut self, nrows: usize) {
        let r = dialog_rect(nrows).pad(10);
        self.render_doc_region(r);
        self.restore_chrome_over(r);
    }

    fn render_goto_dialog(&mut self) {
        let Some(Dialog::GoTo { entry }) = &self.dialog else { return };
        let entry = entry.clone();
        let r = np_rect();
        self.fb.fill_rect(r.x0 - 6, r.y0 - 6, r.w() + 12, r.h() + 12, GRAY);
        self.fb.fill_rect(r.x0, r.y0, r.w(), r.h(), WHITE);
        self.fb.rect_outline(r.x0, r.y0, r.w(), r.h(), 3, BLACK);
        let shown = if entry.is_empty() { "go to page _".to_string() } else { format!("go to page {entry}_") };
        self.fb.text(r.x0 + 28, r.y0 + 34, &shown, 3, BLACK);
        for (i, label) in NP_KEYS.iter().enumerate() {
            let (bx, by) = np_btn_xy(i);
            self.fb.rect_outline(bx, by, NP_BTN_W, NP_BTN_H, 2, BLACK);
            self.fb.text(
                bx + (NP_BTN_W - text_width(label, 3)) / 2,
                by + (NP_BTN_H - 21) / 2,
                label,
                3,
                BLACK,
            );
        }
        self.disp.update(r.x0 - 6, r.y0 - 6, r.w() + 12, r.h() + 12, Wave::Ink);
    }

    fn cell_doc_id(&self, idx: usize) -> Option<String> {
        let Screen::Home(hv) = &self.screen else { return None };
        match hv.cells.get(idx) {
            Some(home::Cell::Doc(d)) => Some(d.id.clone()),
            _ => None,
        }
    }

    fn cell_doc_title(&self, idx: usize) -> String {
        let Screen::Home(hv) = &self.screen else { return String::new() };
        match hv.cells.get(idx) {
            Some(home::Cell::Doc(d)) => d.title.clone(),
            _ => String::new(),
        }
    }

    /* -- status bar upkeep -- */

    fn status_bar_visible(&self) -> bool {
        matches!(self.screen, Screen::Home(_)) || self.bar_until.is_some()
    }

    fn status_tick(&mut self) {
        self.status_poll_at = Instant::now() + STATUS_POLL;
        if !self.status_bar_visible() {
            return;
        }
        if !self.status.refresh() {
            return;
        }
        match &self.screen {
            Screen::Home(_) if self.dialog.is_none() => {
                statusbar::render(&mut self.fb, &self.status);
                self.disp.update(0, 0, FB_W, STATUS_H, Wave::Text);
            }
            Screen::Doc(_) if self.bar_until.is_some() => self.paint_top_bar(),
            _ => {}
        }
    }

    /* -- the pause trigger: hand the page to pi -- */

    fn maybe_send_page(&mut self) {
        let due = {
            let Screen::Doc(dv) = &self.screen else { return };
            /* a live selection/lasso means the user is mid-manipulation */
            if dv.lasso.is_some() || dv.selection.is_some() || dv.cur_stroke.is_some() {
                return;
            }
            match dv.idle_at {
                Some(at) => Instant::now() >= at,
                None => return,
            }
        };
        if !due {
            return;
        }
        /* still touching the glass? push the deadline out a beat */
        if self.last_contact.is_some_and(|t| t.elapsed() < IDLE_DELAY) {
            if let Screen::Doc(dv) = &mut self.screen {
                dv.idle_at = Some(Instant::now() + Duration::from_millis(300));
            }
            return;
        }
        if self.pi.is_none() {
            if let Screen::Doc(dv) = &mut self.screen {
                dv.idle_at = None;
            }
            return;
        }
        let (msg, gray, w, h, page_no) = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            dv.idle_at = None;
            if !dv.page_changed || dv.doc.page.is_empty() {
                return;
            }
            dv.doc.save_all();
            let (w, h, gray) = dv.doc.snapshot(SNAP_DIV);
            let patches = patch_summary(&dv.doc.page);
            let layout = layout_hints(&dv.doc);
            let entry = dv.doc.entry(dv.doc.current);
            let kind = match (dv.doc.kind, entry) {
                (doc::DocKind::Notebook, _) => "a notebook page".to_string(),
                (_, Some(Entry::Pdf(p))) => format!("printed page {} of the PDF", p + 1),
                _ => "a blank note page in a book".into(),
            };
            let text = match (dv.doc.kind, entry) {
                (doc::DocKind::Book, Some(Entry::Pdf(p))) => {
                    let mut t = dv.doc.page_text(p);
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
                _ => "(handwriting only)".into(),
            };
            let msg = format!(
                "\"{}\" — page {} of {} ({}). The attached image is the page \
                 as the user sees it (half scale), with everyone's ink. The \
                 user just paused writing. Extracted text of this page:\n---\n\
                 {}\n---\nYour existing patches here: {}. Measured layout \
                 (page coordinates — trust these numbers): {} Respond with \
                 your canvas_* tools only if it genuinely helps; otherwise \
                 reply `pass`.",
                dv.doc.title,
                dv.doc.current + 1,
                dv.doc.count(),
                kind,
                text,
                patches,
                layout,
            );
            dv.page_changed = false;
            (msg, gray, w, h, dv.doc.current + 1)
        };
        let streaming = self.streaming;
        let Some(pi) = self.pi.as_mut() else { return };
        match pi.send_image_message(&gray, w as u32, h as u32, &msg, streaming) {
            Ok(()) => {
                self.streaming = true;
                self.pi_alive_at = Instant::now();
                self.set_working(true);
                println!("paper: page {page_no} sent to pi");
            }
            Err(e) => println!("paper: send failed: {e}"),
        }
    }

    /* -- pi events -- */

    fn handle_pi(&mut self, ev: PiEvent) {
        self.pi_alive_at = Instant::now();
        match ev {
            PiEvent::Start => {
                self.streaming = true;
                self.reply_buf.clear();
                self.set_working(true);
            }
            PiEvent::Delta(d) => self.reply_buf.push_str(&d),
            PiEvent::Notice(n) => println!("paper: pi {n}"),
            PiEvent::End => {
                self.streaming = false;
                self.set_working(false);
                let t: String = self.reply_buf.trim().chars().take(300).collect();
                if !t.is_empty() {
                    println!("paper: pi said: {t}");
                }
                self.reply_buf.clear();
            }
            PiEvent::Died(reason) => {
                self.streaming = false;
                self.pi = None;
                self.set_working(false);
                self.pi_respawn_at = Some(Instant::now() + Duration::from_secs(5));
                println!("paper: pi exited: {reason}");
            }
        }
    }

    /// Wedged or dead pi: kill + respawn with --continue (the fixed session
    /// dir makes it pick the conversation back up).
    fn check_pi_health(&mut self) {
        if self.streaming
            && self.pi.is_some()
            && self.pi_alive_at.elapsed() >= self.pi_stall
        {
            println!(
                "paper: pi silent for {}s mid-turn; respawning",
                self.pi_alive_at.elapsed().as_secs()
            );
            if let Some(mut pi) = self.pi.take() {
                pi.kill();
            }
            self.streaming = false;
            self.set_working(false);
            /* re-arm the pause so the swallowed page re-sends */
            if let Screen::Doc(dv) = &mut self.screen {
                dv.page_changed = true;
                dv.idle_at = Some(Instant::now() + Duration::from_millis(400));
            }
            self.pi_respawn_at = Some(Instant::now());
        }
        if self.pi.is_none() && self.pi_respawn_at.is_some_and(|at| Instant::now() >= at) {
            self.pi_respawn_at = None;
            match Pi::spawn(&sock_path()) {
                Ok(p) => {
                    self.pi = Some(p);
                    self.pi_alive_at = Instant::now();
                    println!("paper: pi respawned");
                }
                Err(e) => {
                    println!("paper: pi respawn failed: {e}");
                    self.pi_respawn_at = Some(Instant::now() + Duration::from_secs(15));
                }
            }
        }
    }

    /* -- the tool socket -- */

    fn handle_ipc_request(&mut self, req: &Value) -> Value {
        self.pi_alive_at = Instant::now();
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
        let cur = match &self.screen {
            Screen::Doc(dv) => dv.doc.current,
            _ => 0,
        };
        match req["page"].as_u64() {
            Some(p) if p >= 1 => p as usize - 1,
            _ => cur,
        }
    }

    fn no_doc() -> Value {
        json!({ "ok": false, "error": "no document is open (the user is on the home screen)" })
    }

    fn ipc_view(&mut self, req: &Value) -> Value {
        let idx = self.req_page(req);
        let Screen::Doc(dv) = &self.screen else { return Self::no_doc() };
        let d = &dv.doc;
        if idx >= d.count() {
            return json!({ "ok": false, "error": format!("no page {} (document has {})", idx + 1, d.count()) });
        }
        let (w, h, gray, patches) = if idx == d.current {
            let (w, h, gray) = d.snapshot(SNAP_DIV);
            (w, h, gray, patch_list(&d.page))
        } else {
            match d.snapshot_of(idx, SNAP_DIV) {
                Some((w, h, gray, ink)) => (w, h, gray, patch_list(&ink)),
                None => return json!({ "ok": false, "error": "page unreadable" }),
            }
        };
        let png = png::encode_gray(w as u32, h as u32, &gray);
        json!({
            "ok": true,
            "page": idx + 1,
            "page_count": d.count(),
            "label": d.label(idx),
            "page_width": FB_W,
            "page_height": FB_H,
            "image_scale": SNAP_DIV,
            "png_base64": png::base64(&png),
            "patches": patches,
        })
    }

    /// Add a ready-made patch to page `idx`, animating when that page is on
    /// screen. AI patches are undoable ops like everything else.
    fn add_patch_at(&mut self, idx: usize, strokes: Vec<Stroke>, texts: Vec<ink::TextRun>) -> Result<(u64, Option<Rect>, bool), Value> {
        let has_text = !texts.is_empty();
        let Screen::Doc(dv) = &mut self.screen else { return Err(Self::no_doc()) };
        let d = &mut dv.doc;
        if idx >= d.count() {
            return Err(json!({ "ok": false, "error": format!("no page {} (document has {})", idx + 1, d.count()) }));
        }
        if idx == d.current {
            let id = d.page.add_patch(strokes, texts);
            let patch = d.page.patches.last().unwrap();
            let bbox = ink::patch_bbox(patch).map(|bb| bb.clamp_screen());
            /* queue the ghost-hand animation — unless a dialog owns the
             * screen (the strokes appear on its close, via repaint) */
            let animate = self.dialog.is_none();
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
            d.save_all();
            let key = d.cur_ink_path();
            dv.undo.stack(&key).push_keep_redo(EditOp::AddPatch { id, body: None });
            dv.anim.extend(queued);
            /* typeset runs don't stroke-animate — render their region now so
             * the Garamond appears immediately (with the deghost settle) */
            if has_text && self.dialog.is_none() {
                if let Some(bb) = bbox {
                    let r = bb.pad(6).clamp_screen();
                    self.render_doc_region(r);
                    self.restore_chrome_over(r);
                }
            }
            Ok((id, bbox, true))
        } else {
            let Some(e) = d.entry(idx) else {
                return Err(json!({ "ok": false, "error": "no such page" }));
            };
            let mut p = d.load_ink(e);
            let id = p.add_patch(strokes, texts);
            let bbox = ink::patch_bbox(p.patches.last().unwrap()).map(|bb| bb.clamp_screen());
            let path = d.ink_path_of(e);
            if let Err(err) = p.save(&path) {
                return Err(json!({ "ok": false, "error": format!("save: {err}") }));
            }
            dv.undo.drop_page(&path); /* our ids for that page went stale */
            Ok((id, bbox, false))
        }
    }

    fn ipc_draw(&mut self, req: &Value) -> Value {
        let Some(svg) = req["svg"].as_str() else {
            return json!({ "ok": false, "error": "missing 'svg'" });
        };
        let (strokes, texts, notes) = match svg_ink::parse(svg, 1.0, self.pi_font) {
            Ok(v) => v,
            Err(e) => return json!({ "ok": false, "error": e }),
        };
        for n in &notes {
            println!("paper: draw note: {n}");
        }
        let idx = self.req_page(req);
        let n_strokes = strokes.len();
        match self.add_patch_at(idx, strokes, texts) {
            Ok((id, bbox, on_screen)) => {
                println!("paper: patch #{id} on page {} ({n_strokes} strokes)", idx + 1);
                self.paint_toolbar(); /* undo became available */
                let layout = match &self.screen {
                    Screen::Doc(dv) if on_screen => layout_hints(&dv.doc),
                    _ => String::new(),
                };
                json!({
                    "ok": true, "id": id, "page": idx + 1,
                    "bbox": bbox.map(|b| json!([b.x0, b.y0, b.x1, b.y1])).unwrap_or(json!(null)),
                    "layout": layout,
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
        let (strokes, total) = {
            let Screen::Doc(dv) = &self.screen else { return Self::no_doc() };
            let d = &dv.doc;
            if idx >= d.count() {
                return json!({ "ok": false, "error": format!("no page {} (document has {})", idx + 1, d.count()) });
            }
            let Some(Entry::Pdf(p)) = d.entry(idx) else {
                return json!({ "ok": false, "error": "nothing printed on that page to underline" });
            };
            let words = d.words(p);
            if words.is_empty() {
                return json!({ "ok": false, "error": "no word geometry for this page" });
            }
            let (picked, total) = doc::find_phrase(&words, phrase, nth);
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
            (doc::underline_strokes(&words, &picked), total)
        };
        match self.add_patch_at(idx, strokes, Vec::new()) {
            Ok((id, bbox, _)) => {
                println!("paper: underlined '{phrase}' on page {} (#{id})", idx + 1);
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
        let repaint = {
            let Screen::Doc(dv) = &mut self.screen else { return Self::no_doc() };
            let d = &mut dv.doc;
            if idx == d.current {
                /* drop any still-animating strokes of this patch */
                let mut region: Option<Rect> = None;
                dv.anim.retain(|a| {
                    if a.patch == id && a.page == idx {
                        region = Some(region.map_or(a.bbox, |r| r.union(a.bbox)));
                        false
                    } else {
                        true
                    }
                });
                match d.page.take_patch(id) {
                    Some((body, bb)) => {
                        let key = d.cur_ink_path();
                        dv.undo.stack(&key).push_keep_redo(EditOp::ErasePatch { id, body: Some(body) });
                        d.save_all();
                        /* a live selection may reference the vanished patch */
                        if let Some(sel) = &mut dv.selection {
                            sel.refs.retain(|&(o, _)| o != ink::Owner::Patch(id));
                        }
                        Some(region.map_or(bb, |r| r.union(bb)).pad(4).clamp_screen())
                    }
                    None => {
                        return json!({ "ok": false, "error": format!("no patch {id} on page {}", idx + 1) })
                    }
                }
            } else {
                if idx >= d.count() {
                    return json!({ "ok": false, "error": format!("no page {} (document has {})", idx + 1, d.count()) });
                }
                let Some(e) = d.entry(idx) else {
                    return json!({ "ok": false, "error": "no such page" });
                };
                let mut p = d.load_ink(e);
                match p.remove_patch(id) {
                    Some(_) => {
                        let path = d.ink_path_of(e);
                        if let Err(err) = p.save(&path) {
                            return json!({ "ok": false, "error": format!("save: {err}") });
                        }
                        dv.undo.drop_page(&path);
                        None
                    }
                    None => {
                        return json!({ "ok": false, "error": format!("no patch {id} on page {}", idx + 1) })
                    }
                }
            }
        };
        if let Some(r) = repaint {
            if self.dialog.is_none() {
                self.render_doc_region(r);
                self.restore_chrome_over(r);
            }
            self.paint_toolbar();
        }
        json!({ "ok": true })
    }

    /// pi turns the page. Refused while the user is writing, selecting, or
    /// in a dialog — yanking the page out from under them would be rude.
    fn ipc_goto(&mut self, req: &Value) -> Value {
        let Some(p) = req["page"].as_u64().filter(|&p| p >= 1) else {
            return json!({ "ok": false, "error": "missing/invalid 'page' (1-based)" });
        };
        let idx = p as usize - 1;
        {
            let Screen::Doc(dv) = &self.screen else { return Self::no_doc() };
            if idx >= dv.doc.count() {
                return json!({ "ok": false, "error": format!("no page {} (document has {})", p, dv.doc.count()) });
            }
            if self.dialog.is_some() {
                return json!({ "ok": false, "error": "the user is in a menu; not turning the page" });
            }
            if dv.cur_stroke.is_some()
                || dv.lasso.is_some()
                || dv.selection.is_some()
                || self.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
            {
                return json!({ "ok": false, "error": "the user is writing right now; try again shortly" });
            }
        }
        self.jump_to(idx + 1);
        println!("paper: pi turned to page {}", idx + 1);
        let Screen::Doc(dv) = &self.screen else { return Self::no_doc() };
        json!({
            "ok": true, "page": idx + 1, "page_count": dv.doc.count(), "label": dv.doc.label(idx),
            "layout": layout_hints(&dv.doc),
        })
    }

    fn ipc_insert_note(&mut self, req: &Value) -> Value {
        let (at, count) = {
            let Screen::Doc(dv) = &mut self.screen else { return Self::no_doc() };
            let d = &mut dv.doc;
            let after = match req["after_page"].as_u64() {
                Some(p) if p >= 1 && (p as usize) <= d.count() => p as usize - 1,
                Some(_) => {
                    return json!({ "ok": false, "error": format!("after_page out of range (document has {})", d.count()) })
                }
                None => d.current,
            };
            let at = d.insert_note(after);
            (at, d.count())
        };
        println!("paper: pi inserted note page at {}", at + 1);
        let indicator = {
            let Screen::Doc(dv) = &self.screen else { return Self::no_doc() };
            dv.indicator_until.is_some()
        };
        if indicator && self.dialog.is_none() {
            self.show_page_indicator();
        }
        self.paint_toolbar(); /* the page count in the strip changed */
        json!({
            "ok": true, "page": at + 1, "page_count": count,
            "note": "a blank note page now exists there; draw on it with canvas_draw {page: N}",
        })
    }

    fn ipc_page_text(&mut self, req: &Value) -> Value {
        let Screen::Doc(dv) = &self.screen else { return Self::no_doc() };
        let d = &dv.doc;
        let from = match req["from"].as_u64() {
            Some(p) if p >= 1 && (p as usize) <= d.count() => p as usize - 1,
            _ => return json!({ "ok": false, "error": format!("missing/invalid 'from' (document has {} pages)", d.count()) }),
        };
        let to = match req["to"].as_u64() {
            Some(p) if p >= 1 => (p as usize - 1).min(d.count() - 1),
            _ => from,
        };
        if to < from {
            return json!({ "ok": false, "error": "'to' before 'from'" });
        }
        let to = to.min(from + 7); /* at most 8 pages per call */
        let mut out = String::new();
        for i in from..=to {
            match d.entry(i) {
                Some(Entry::Pdf(p)) => {
                    let mut t = d.page_text(p);
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
                    out.push_str(&format!("--- page {} (handwritten ink only — use canvas_view to see it) ---\n", i + 1));
                }
                None => {}
            }
        }
        json!({ "ok": true, "from": from + 1, "to": to + 1, "page_count": d.count(), "text": out })
    }

    /* -- AI ink animation -- */

    fn anim_tick(&mut self) {
        /* never fight the writer: hold while the pen is on/near the glass;
         * hold under dialogs and live selections too */
        let hold = {
            let Screen::Doc(dv) = &self.screen else { return };
            dv.cur_stroke.is_some()
                || dv.lasso.is_some()
                || dv.selection.is_some()
                || self.dialog.is_some()
                || self.last_contact.is_some_and(|t| t.elapsed() < Duration::from_millis(350))
        };
        if hold {
            self.last_anim = Instant::now();
            return;
        }
        {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let cur = dv.doc.current;
            let mut budget = ANIM_BUDGET;
            while budget > 0.0 {
                let Some(a) = dv.anim.front_mut() else { break };
                if a.page != cur {
                    dv.anim.pop_front(); /* already in the model; visible on flip */
                    continue;
                }
                let Some(next) = a.remaining.pop_front() else {
                    dv.anim.pop_front();
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
                dv.anim_dirty = Some(match dv.anim_dirty {
                    None => seg,
                    Some(d) => d.union(seg),
                });
                dv.anim_settle = Some(match dv.anim_settle {
                    None => seg,
                    Some(d) => d.union(seg),
                });
                budget -= (next.x - from.x).hypot(next.y - from.y).max(1.5);
                a.last = Some(next);
            }
        }
        let (dirty, settle) = {
            let Screen::Doc(dv) = &mut self.screen else { return };
            let dirty = dv.anim_dirty.take();
            let settle = if dv.anim.is_empty() { dv.anim_settle.take() } else { None };
            (dirty, settle)
        };
        if let Some(r) = dirty {
            let r = r.clamp_screen();
            /* black ink now: the same crisp low-latency waveform as the pen */
            self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
            self.restore_chrome_over(r);
        }
        /* the ghost hand finished: one 16-level pass over everything it
         * wrote smooths the DU-rough stroke edges */
        if let Some(r) = settle {
            let r = r.pad(4).clamp_screen();
            self.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Text);
        }
        self.last_anim = Instant::now();
    }

    /* -- sleep (takeover only) -- */

    fn show_sleep_page(&mut self) -> Vec<u16> {
        let saved = self.fb.copy_band(0, FB_H);
        self.fb.fill_rect(0, 0, FB_W, FB_H, WHITE);
        let msg = "Paper sleeps";
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

/* ---- pi message helpers ------------------------------------------------------ */

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

/// Measured layout for the pause message: printed-page margins on book
/// pages, ink rows + free bands everywhere (the numbers pi must trust).
fn layout_hints(d: &Doc) -> String {
    let mut s = match d.entry(d.current) {
        Some(Entry::Pdf(p)) => {
            let words = d.words(p);
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
                    "margins are narrow — prefer canvas_underline + a note page".to_string()
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

    let bands = d.page.ink_bands();
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
        /* free bands matter wherever the whole page is writable */
        if !matches!(d.entry(d.current), Some(Entry::Pdf(_))) {
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
            if let Some(lh) = d.page.user_line_height() {
                let fs = (lh * 9 / 10).clamp(30, 90);
                s.push_str(&format!(
                    " The user's handwriting rows are ~{lh}px tall: write at font-size ~{fs} \
                     with ~{}px between your baselines.",
                    fs * 3 / 2
                ));
            }
        }
    }
    s
}

/* ---- the go-to-page numpad -------------------------------------------------- */

const NP_BTN_W: i32 = 170;
const NP_BTN_H: i32 = 110;
const NP_GAP: i32 = 12;
const NP_KEYS: [&str; 12] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "DEL", "0", "GO"];

fn np_rect() -> Rect {
    let w = 3 * NP_BTN_W + 2 * NP_GAP + 56;
    let h = 90 + 4 * (NP_BTN_H + NP_GAP) + 28;
    let x0 = (FB_W - w) / 2;
    let y0 = (FB_H - h) / 2;
    Rect { x0, y0, x1: x0 + w - 1, y1: y0 + h - 1 }
}

fn np_btn_xy(i: usize) -> (i32, i32) {
    let r = np_rect();
    let (col, row) = ((i % 3) as i32, (i / 3) as i32);
    (r.x0 + 28 + col * (NP_BTN_W + NP_GAP), r.y0 + 90 + row * (NP_BTN_H + NP_GAP))
}

enum NpAction {
    Digit(char),
    Del,
    Ok,
    Swallow,
    Outside,
}

fn np_press(x: i32, y: i32) -> NpAction {
    let r = np_rect();
    if !in_rect(x, y, r.x0, r.y0, r.w(), r.h()) {
        return NpAction::Outside;
    }
    for (i, label) in NP_KEYS.iter().enumerate() {
        let (bx, by) = np_btn_xy(i);
        if in_rect(x, y, bx, by, NP_BTN_W, NP_BTN_H) {
            return match *label {
                "DEL" => NpAction::Del,
                "GO" => NpAction::Ok,
                d => NpAction::Digit(d.chars().next().unwrap()),
            };
        }
    }
    NpAction::Swallow
}

/* ---- dialog rendering (free fns: fb + data only) --------------------------- */

fn dialog_rect(nrows: usize) -> Rect {
    let h = TITLE_PAD + nrows as i32 * DLG_ROW_H + 24;
    let x0 = (FB_W - DLG_W) / 2;
    let y0 = (FB_H - h) / 2;
    Rect { x0, y0, x1: x0 + DLG_W - 1, y1: y0 + h - 1 }
}

const TITLE_PAD: i32 = 84;

fn dialog_row_at(nrows: usize, x: i32, y: i32) -> Option<usize> {
    let r = dialog_rect(nrows);
    if !in_rect(x, y, r.x0, r.y0, r.w(), r.h()) {
        return None; /* outside = dismiss (caller drops the dialog) */
    }
    let i = (y - r.y0 - TITLE_PAD) / DLG_ROW_H;
    (i >= 0 && (i as usize) < nrows).then_some(i as usize)
}

fn render_dialog(fb: &mut Framebuffer, d: &Dialog, screen: &Screen) {
    let title_of = |idx: usize| -> String {
        if let Screen::Home(hv) = screen {
            if let Some(home::Cell::Doc(doc)) = hv.cells.get(idx) {
                return doc.title.clone();
            }
        }
        String::new()
    };
    match d {
        Dialog::Rename { kb, .. } | Dialog::NewFolder { kb, .. } => {
            kb.render(fb);
            return;
        }
        Dialog::DocMenu { idx } => {
            let rows = ["RENAME", "MOVE TO FOLDER", "DUPLICATE", "DELETE", "CANCEL"];
            draw_dialog_rows(fb, &title_of(*idx), &rows.map(String::from));
        }
        Dialog::ConfirmDelete { idx } => {
            let t = format!("Delete \"{}\"?", title_of(*idx));
            draw_dialog_rows(fb, &t, &["DELETE".to_string(), "CANCEL".to_string()]);
        }
        Dialog::MoveTo { idx } => {
            let folders = store::folders();
            let mut rows: Vec<String> = vec!["( my files )".into()];
            rows.extend(folders);
            rows.push("NEW FOLDER ...".into());
            rows.push("CANCEL".into());
            let t = format!("Move \"{}\" to:", title_of(*idx));
            draw_dialog_rows(fb, &t, &rows);
        }
        Dialog::GoTo { .. } | Dialog::FontPick => {
            /* doc-screen dialogs; rendered by render_goto_dialog / render_font_dialog */
        }
    }
}

fn draw_dialog_rows(fb: &mut Framebuffer, title: &str, rows: &[String]) {
    let r = dialog_rect(rows.len());
    fb.fill_rect(r.x0 - 6, r.y0 - 6, r.w() + 12, r.h() + 12, GRAY);
    fb.fill_rect(r.x0, r.y0, r.w(), r.h(), WHITE);
    fb.rect_outline(r.x0, r.y0, r.w(), r.h(), 3, BLACK);
    let mut t = title.to_string();
    while text::width(text::Face::Body, 34.0, &t) > DLG_W - 60 && t.chars().count() > 4 {
        t = t.chars().take(t.chars().count() - 4).collect();
        t.push_str("..");
    }
    text::draw_line(fb, r.x0 + 30, r.y0 + 24, text::Face::Body, 34.0, &t);
    for (i, label) in rows.iter().enumerate() {
        let y = r.y0 + TITLE_PAD + i as i32 * DLG_ROW_H;
        fb.fill_rect(r.x0 + 24, y, DLG_W - 48, 1, draw::LIGHT);
        fb.text(
            r.x0 + (DLG_W - text_width(label, 3)) / 2,
            y + (DLG_ROW_H - 21) / 2,
            label,
            3,
            BLACK,
        );
    }
}

/* ---- sleep ----------------------------------------------------------------- */

fn sleep_cycle(
    app: &mut App,
    p: &mut power::PowerButton,
    pen: &mut Option<Pen>,
    touchdev: &mut Option<touch::TouchDevice>,
) {
    println!("paper: sleeping (power button)");
    if let Screen::Doc(dv) = &mut app.screen {
        dv.doc.save_all();
    }
    let saved = app.show_sleep_page();
    app.disp.full_refresh();
    std::thread::sleep(Duration::from_millis(800));
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
            println!("paper: suspend never happened ({attempts} tries); waking the page");
            break;
        }
        println!("paper: suspend aborted (EPD discharge timer), retrying");
    }
    println!("paper: waking");
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
    /* device-side legacy adoption, dry-runnable via `make adopt` */
    if std::env::var("PAPER_IMPORT_ONLY").is_ok() {
        let (b, p) = store::import_legacy();
        println!("paper: import done ({b} books, {p} notebook pages)");
        return std::process::ExitCode::SUCCESS;
    }

    let (disp, fb) = match Display::open() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("paper: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let takeover = disp.is_takeover();
    println!(
        "paper: up, fb={FB_W}x{FB_H} ({})",
        if takeover { "takeover/rm2fb" } else { "windowed/qtfb" }
    );
    install_signal_handlers();

    store::import_legacy();

    let mut pen = Pen::open();
    let direct_pen = pen.is_some();
    if takeover {
        if let Some(p) = pen.as_ref() {
            p.grab();
        }
    }
    let mut touchdev = if takeover {
        touch::TouchDevice::open()
            .map_err(|e| eprintln!("paper: no touch device ({e}) — page flips disabled"))
            .ok()
    } else {
        None
    };
    let mut powerdev = if takeover {
        power::PowerButton::open()
            .map_err(|e| eprintln!("paper: no power button ({e})"))
            .ok()
    } else {
        None
    };
    let mut power_grace = Instant::now();

    let sock = sock_path();
    let ipc = IpcServer::open(&sock)
        .map_err(|e| eprintln!("paper: tool socket: {e} — pi gets no tools"))
        .ok();
    let pi = match Pi::spawn(&sock) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("paper: could not start pi: {e}");
            None
        }
    };
    let pi_stall = std::env::var("PAPER_PI_STALL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(Duration::from_secs(180), Duration::from_secs);

    let (last_doc, sort, tool, pi_font) = load_settings();
    let now = Instant::now();
    let mut app = App {
        fb,
        disp,
        takeover,
        screen: Screen::Home(HomeView::build(None, sort)),
        dialog: None,
        pi,
        ipc,
        streaming: false,
        reply_buf: String::new(),
        working: false,
        last_anim: now,
        last_contact: None,
        pi_alive_at: now,
        pi_respawn_at: None,
        pi_stall,
        status: SysStatus::new(),
        status_poll_at: now + STATUS_POLL,
        home_rescan_at: now + HOME_RESCAN,
        last_sync: now.checked_sub(MIN_SYNC_GAP).unwrap_or(now), /* allow an immediate first sync */
        sort,
        tool,
        pi_font,
        flips_since_flash: 0,
        ink_flush: if takeover { INK_FLUSH_TAKEOVER } else { INK_FLUSH_QTFB },
        last_ink_flush: now,
        clipboard: None,
        last_pen: None,
        touch_start: None,
        touch_t0: None,
        touch_last: (0, 0),
        swipe_from: None,
        bar_until: None,
    };
    let _ = app.takeover;

    /* first paint: PAPER_OPEN (harness), else resume the last doc, else home */
    let open_id = std::env::var("PAPER_OPEN").ok().unwrap_or(last_doc);
    if open_id.is_empty() || !app.open_doc(&open_id) {
        app.render_home(true);
    }

    while RUNNING.load(Ordering::Relaxed) {
        let timeout = next_timeout(&app);
        let mut pfds: Vec<libc::pollfd> = vec![
            libc::pollfd { fd: app.disp.raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: pen.as_ref().map_or(-1, |p| p.raw_fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: touchdev.as_ref().map_or(-1, |t| t.raw_fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: powerdev.as_ref().map_or(-1, |p| p.raw_fd()), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: app.pi.as_ref().map_or(-1, |p| p.raw_fd()), events: libc::POLLIN, revents: 0 },
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
        if pfds[3].revents & libc::POLLIN != 0 {
            if let Some(p) = powerdev.as_mut() {
                if p.drain_pressed() && Instant::now() >= power_grace {
                    sleep_cycle(&mut app, p, &mut pen, &mut touchdev);
                    power_grace = Instant::now() + Duration::from_secs(3);
                }
            }
        }

        /* -- pen -- */
        if pfds[1].revents & libc::POLLIN != 0 {
            if let Some(p) = pen.as_mut() {
                let mut frames = Vec::new();
                p.drain(|p, phase| {
                    frames.push((phase, p.sx, p.sy, p.pressure, p.rubber));
                });
                if direct_pen {
                    for (phase, x, y, pr, rub) in frames {
                        app.route_pen(phase, x, y, pr, rub);
                    }
                }
            }
        }

        /* -- raw touch (takeover) -- */
        if pfds[2].revents & libc::POLLIN != 0 {
            if let Some(t) = touchdev.as_mut() {
                /* no 5-finger quit (a writing palm reads as 5+ contacts) —
                 * the top-edge swipe -> CLOSE is the exit */
                let (evs, _quit) = t.drain();
                for e in evs {
                    app.route_touch(e.phase, e.x, e.y);
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
                    Event::Touch { phase, x, y, .. } => app.route_touch(phase, x, y),
                    Event::Pen { phase, x, y, .. } => {
                        app.last_pen = Some(Instant::now());
                        if !direct_pen {
                            let ph = match phase {
                                Phase::Press => PenPhase::Press,
                                Phase::Move => PenPhase::Move,
                                Phase::Release => PenPhase::Release,
                            };
                            app.route_pen(ph, x, y, 0, false);
                        }
                    }
                    Event::Key { .. } | Event::Other => {}
                }
            }
        }

        /* -- pi stdout -- */
        if pfds[4].revents & libc::POLLIN != 0 {
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
        let mut flushed: Option<Rect> = None;
        if let Screen::Doc(dv) = &mut app.screen {
            if dv.ink_dirty.is_some() && app.last_ink_flush.elapsed() >= app.ink_flush {
                let r = dv.ink_dirty.take().unwrap().clamp_screen();
                app.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Ink);
                app.last_ink_flush = Instant::now();
                flushed = Some(r);
            }
        }
        if let Some(r) = flushed {
            /* a stroke may run under the toolbar: keep the chrome on top */
            app.restore_chrome_over(r);
        }
        if let Screen::Doc(dv) = &app.screen {
            if dv.indicator_until.is_some_and(|at| Instant::now() >= at) {
                app.clear_page_indicator();
            }
        }
        /* pen-ink settle: GL16 over fresh strokes on a page raster, once
         * the pen has been quiet a beat — heals DU-crunched print */
        if let Screen::Doc(dv) = &mut app.screen {
            if let Some(at) = dv.ink_settle_at {
                if Instant::now() >= at {
                    if dv.cur_stroke.is_some()
                        || app.last_pen.is_some_and(|t| t.elapsed() < Duration::from_millis(500))
                    {
                        dv.ink_settle_at = Some(Instant::now() + Duration::from_millis(400));
                    } else {
                        dv.ink_settle_at = None;
                        if let Some(r) = dv.ink_settle.take() {
                            let r = r.pad(4).clamp_screen();
                            app.disp.update(r.x0, r.y0, r.w(), r.h(), Wave::Text);
                        }
                    }
                }
            }
        }
        /* post-erase deghost: only once the pen has settled */
        if let Screen::Doc(dv) = &mut app.screen {
            if let Some(at) = dv.deghost_at {
                if Instant::now() >= at {
                    if dv.cur_stroke.is_some()
                        || app.last_pen.is_some_and(|t| t.elapsed() < Duration::from_millis(700))
                    {
                        dv.deghost_at = Some(Instant::now() + Duration::from_millis(600));
                    } else {
                        dv.deghost_at = None;
                        app.disp.full_refresh();
                    }
                }
            }
        }
        if app.bar_until.is_some_and(|at| Instant::now() >= at) {
            app.dismiss_top_bar();
        }
        if Instant::now() >= app.status_poll_at {
            app.status_tick();
        }
        if matches!(&app.screen, Screen::Home(_)) && app.dialog.is_none()
            && Instant::now() >= app.home_rescan_at
        {
            app.home_rescan();
        }
        let anim_due = matches!(&app.screen, Screen::Doc(dv) if !dv.anim.is_empty())
            && app.last_anim.elapsed() >= ANIM_TICK;
        if anim_due {
            app.anim_tick();
        }
        app.maybe_send_page();
        app.check_pi_health();
        app.thumb_tick();
    }

    println!("paper: exiting");
    if let Screen::Doc(dv) = &mut app.screen {
        dv.doc.save_all();
    }
    std::process::ExitCode::SUCCESS
}

/// Milliseconds until the next pending flush/tick is due (-1 = block).
fn next_timeout(app: &App) -> i32 {
    let mut t: Option<Duration> = None;
    let mut soonest = |d: Duration| {
        t = Some(t.map_or(d, |cur| cur.min(d)));
    };
    match &app.screen {
        Screen::Doc(dv) => {
            if dv.ink_dirty.is_some() {
                soonest(app.ink_flush.saturating_sub(app.last_ink_flush.elapsed()));
            }
            if !dv.anim.is_empty() {
                soonest(ANIM_TICK.saturating_sub(app.last_anim.elapsed()));
            }
            for at in [dv.indicator_until, dv.deghost_at, dv.ink_settle_at, dv.idle_at]
                .into_iter()
                .flatten()
            {
                soonest(at.saturating_duration_since(Instant::now()));
            }
        }
        Screen::Home(hv) => {
            if !hv.pending.is_empty() && app.dialog.is_none() {
                soonest(Duration::from_millis(5)); /* lazy thumbnails */
            }
            if app.dialog.is_none() {
                soonest(app.home_rescan_at.saturating_duration_since(Instant::now()));
            }
        }
    }
    if app.streaming {
        soonest(Duration::from_secs(5)); /* keep the watchdog breathing */
    }
    if let Some(at) = app.pi_respawn_at {
        soonest(at.saturating_duration_since(Instant::now()));
    }
    if let Some(at) = app.bar_until {
        soonest(at.saturating_duration_since(Instant::now()));
    }
    if app.status_bar_visible() {
        soonest(app.status_poll_at.saturating_duration_since(Instant::now()));
    }
    match t {
        Some(d) => (d.as_millis() as i32).max(0),
        None => -1,
    }
}
