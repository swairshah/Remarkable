//! pi-collab — handwrite to a background pi agent, watch its replies stream
//! back, on the reMarkable 2. An AppLoad app.
//!
//! The screen is three bands:
//!   - a header (title + status),
//!   - a scrollable conversation viewport (your ink + pi's text),
//!   - an input strip at the bottom: a writing canvas plus SEND / CLEAR.
//!
//! You write in the canvas with the pen; SEND snapshots the ink, drops it
//! into the log, and hands it to pi (running headless via `pi --mode rpc`)
//! as a PNG. pi's reply streams in as text. Drag the viewport with a finger
//! to scroll; it auto-follows the bottom while pi is typing unless you've
//! scrolled away.
//!
//! Module map:
//!   qtfb.rs   AppLoad protocol (framebuffer + input) — from sample-app-rust
//!   pen.rs    direct Wacom digitizer input           — from sample-app-rust
//!   draw.rs   framebuffer primitives (clip, text, blit)
//!   font.rs   full-ASCII 5x7 bitmap font
//!   conv.rs   the conversation model + its layout/rendering
//!   png.rs    tiny grayscale PNG encoder + base64
//!   pi_rpc.rs the pi child process and its JSONL protocol

mod conv;
mod draw;
mod font;
mod history;
mod md;
mod pen;
mod pi_rpc;
mod png;
mod qtfb;
mod svg;
mod text;

use conv::{total_height, Entry, GrayImg};
use draw::{text_width, BLACK, GRAY, WHITE};
use pen::{Pen, PenPhase};
use pi_rpc::{Pi, PiEvent};
use qtfb::{Event, Framebuffer, Phase, RefreshMode, Socket, RM2_HEIGHT as FB_H, RM2_WIDTH as FB_W};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/* ---- layout (framebuffer pixels) ---------------------------------------- */

const HEADER_H: i32 = 92;
const INPUT_H: i32 = 460;
const INPUT_Y0: i32 = FB_H - INPUT_H; /* 1412 */
const CTRL_H: i32 = 76; /* control bar atop the input strip */
const CANVAS_Y0: i32 = INPUT_Y0 + CTRL_H; /* 1488 */
const CANVAS_Y1: i32 = FB_H; /* 1872 */
const VIEW_Y0: i32 = HEADER_H;
const VIEW_Y1: i32 = INPUT_Y0;

/* SEND / CLEAR buttons, right-aligned in the control bar */
const BTN_H: i32 = 60;
const BTN_Y: i32 = INPUT_Y0 + 8;
const SEND_W: i32 = 220;
const SEND_X: i32 = FB_W - 24 - SEND_W;
const CLEAR_W: i32 = 180;
const CLEAR_X: i32 = SEND_X - 16 - CLEAR_W;

/* the pi wordmark, as an 8x8 pixel-art bitmap (each bit = one cell, MSB is
 * the leftmost column) — traced from the pi.dev icon in ../pi/pi-appload */
const LOGO: [u8; 8] = [
    0b11111100,
    0b11111100,
    0b11001100,
    0b11001100,
    0b11110011,
    0b11110011,
    0b11000011,
    0b11000011,
];
const LOGO_CELL: i32 = 7; /* -> a 56x56 mark */

/* header buttons: font A- / A+ and a manual REFRESH (deghost) */
const HB_Y: i32 = 20;
const HB_H: i32 = 52;
const FDN_X: i32 = 130;
const FUP_X: i32 = FDN_X + 66;
const AZ_W: i32 = 60; /* the A-/A+ buttons */
const REFRESH_X: i32 = FUP_X + AZ_W + 22;
const REFRESH_W: i32 = 200;

/* pi body-text size in pixels; A- / A+ step it within these bounds */
const PI_PX_DEFAULT: i32 = 34;
const PI_PX_MIN: i32 = 22;
const PI_PX_MAX: i32 = 58;
const PI_PX_STEP: i32 = 4;

/* nib sizes (base stroke radius): small / medium / large. Pressure adds a
 * little on top; large ~ matches the old fixed nib at its max. */
const NIB_BASE: [i32; 3] = [1, 3, 5];
const ERASER_R: i32 = 22;

/* nib selector buttons, in the control bar between the hint and CLEAR */
const NIB_W: i32 = 58;
const NIB_H: i32 = 60;
const NIB0_X: i32 = 28;
const NIB_Y: i32 = INPUT_Y0 + 8;
const fn nib_x(i: i32) -> i32 {
    NIB0_X + i * (NIB_W + 10)
}

/* refresh cadences: ink strokes want minimum latency; the conversation
 * viewport is a big region, so its redraws are throttled harder */
const INK_FLUSH: Duration = Duration::from_millis(12);
const VIEW_FLUSH: Duration = Duration::from_millis(110);
const PEN_TIMEOUT: Duration = Duration::from_millis(1500);

/* Partial e-ink updates leave ghosting; a full-panel refresh (the deghost
 * flash) clears it but is disruptive, so we never do it mid-interaction —
 * only once things settle. This is the "not too much" delay after the last
 * scroll / streamed reply before a single cleanup flash. */
const DEGHOST_DELAY: Duration = Duration::from_millis(700);

/* snapshot target: downscale ink to at most this before storing/sending */
const SNAP_W: i32 = 1000;
const SNAP_H: i32 = 460;

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

/* ---- app state ----------------------------------------------------------- */

struct App {
    fb: Framebuffer,
    sock: Socket,
    pi: Option<Pi>,

    entries: Vec<Entry>,
    scroll: i32,
    stuck: bool, /* auto-follow the bottom as content grows */
    live_pi: Option<usize>, /* index of the assistant entry being streamed */
    streaming: bool, /* pi is mid-reply (between agent_start and agent_end) */
    reply_buf: String, /* full text of the current reply, for history */
    status: String,
    pi_px: i32, /* body text size in pixels for pi's replies (A- / A+) */
    nib: usize, /* pen nib size, index into NIB_BASE (S/M/L) */
    deghost_at: Option<Instant>, /* when to do the next cleanup flash */
    wave: i32, /* current e-ink waveform mode (RefreshMode as i32) */
    scroll_pending: bool, /* scroll moved during a drag; repaint on release */

    /* pen writing in the canvas */
    pen_last: Option<(i32, i32)>,
    ink_dirty: Option<(i32, i32, i32, i32)>,
    last_ink_flush: Instant,
    last_pen: Option<Instant>,

    /* finger scroll drag */
    drag_last: Option<i32>,

    view_dirty: bool,
    last_view_flush: Instant,
}

impl App {
    fn max_scroll(&self) -> i32 {
        (total_height(&self.entries, self.pi_px) - (VIEW_Y1 - VIEW_Y0)).max(0)
    }

    /* -- painting -- */

    fn draw_header(&mut self) {
        self.fb.fill_rect(0, 0, FB_W, HEADER_H, WHITE);
        self.draw_logo(28, (HEADER_H - 8 * LOGO_CELL) / 2);
        self.hdr_button(FDN_X, AZ_W, "A-");
        self.hdr_button(FUP_X, AZ_W, "A+");
        self.hdr_button(REFRESH_X, REFRESH_W, "REFRESH");
        /* top-right: a filled dot while pi is working (thinking / typing),
         * otherwise a short status word only for notable states (errors) */
        if self.streaming {
            self.fb.disc(FB_W - 40, HEADER_H / 2 - 1, 9, BLACK);
        } else if !self.status.is_empty() {
            let s = self.status.clone();
            self.fb.text(FB_W - 28 - text_width(&s, 3), 30, &s, 3, GRAY);
        }
        self.fb.fill_rect(0, HEADER_H - 2, FB_W, 2, BLACK);
        let _ = self.sock.update_region(0, 0, FB_W, HEADER_H);
    }

    /// Paint the pi mark: each set bit is a filled cell.
    fn draw_logo(&mut self, x: i32, y: i32) {
        for (r, row) in LOGO.iter().enumerate() {
            for c in 0..8 {
                if (row >> (7 - c)) & 1 == 1 {
                    self.fb.fill_rect(
                        x + c * LOGO_CELL,
                        y + r as i32 * LOGO_CELL,
                        LOGO_CELL,
                        LOGO_CELL,
                        BLACK,
                    );
                }
            }
        }
    }

    fn hdr_button(&mut self, x: i32, w: i32, label: &str) {
        self.fb.fill_rect(x, HB_Y, w, HB_H, WHITE);
        self.fb.rect_outline(x, HB_Y, w, HB_H, 2, BLACK);
        self.fb
            .text(x + (w - text_width(label, 3)) / 2, HB_Y + (HB_H - 21) / 2, label, 3, BLACK);
    }

    fn set_status(&mut self, s: &str) {
        if self.status != s {
            self.status = s.to_string();
            self.draw_header();
        }
    }

    /// Draw the input strip: control bar (hint + buttons) and, when
    /// `clear_canvas` is set, a blank writing area with a baseline.
    fn draw_input_strip(&mut self, clear_canvas: bool) {
        self.fb.fill_rect(0, INPUT_Y0, FB_W, CTRL_H, WHITE);
        self.fb.fill_rect(0, INPUT_Y0, FB_W, 2, BLACK); /* divider */
        self.paint_nib_buttons();
        self.draw_button(CLEAR_X, "CLEAR");
        self.draw_button(SEND_X, "SEND");
        if clear_canvas {
            self.fb.fill_rect(0, CANVAS_Y0, FB_W, CANVAS_Y1 - CANVAS_Y0, WHITE);
        }
        let _ = self.sock.update_region(0, INPUT_Y0, FB_W, INPUT_H);
    }

    fn draw_button(&mut self, x: i32, label: &str) {
        let w = if label == "SEND" { SEND_W } else { CLEAR_W };
        self.fb.fill_rect(x, BTN_Y, w, BTN_H, WHITE);
        self.fb.rect_outline(x, BTN_Y, w, BTN_H, 3, BLACK);
        self.fb
            .text(x + (w - text_width(label, 3)) / 2, BTN_Y + (BTN_H - 21) / 2, label, 3, BLACK);
    }

    /// The three nib buttons, each showing a dot sized like its stroke; the
    /// selected one is inverted (black fill, white dot).
    fn paint_nib_buttons(&mut self) {
        for i in 0..3usize {
            let x = nib_x(i as i32);
            let sel = i == self.nib;
            let (bg, fg) = if sel { (BLACK, WHITE) } else { (WHITE, BLACK) };
            self.fb.fill_rect(x, NIB_Y, NIB_W, NIB_H, bg);
            self.fb.rect_outline(x, NIB_Y, NIB_W, NIB_H, 2, BLACK);
            /* dot radius grows S<M<L so the sizes read at a glance */
            let r = 3 + i as i32 * 3;
            self.fb.disc(x + NIB_W / 2, NIB_Y + NIB_H / 2, r, fg);
        }
    }

    fn set_nib(&mut self, i: usize) {
        if i != self.nib {
            self.nib = i;
            self.paint_nib_buttons();
            let _ = self
                .sock
                .update_region(NIB0_X, NIB_Y, nib_x(2) + NIB_W - NIB0_X, NIB_H);
        }
    }

    fn redraw_view(&mut self) {
        /* antialiased text wants the quality waveform, not the near-binary
         * UFAST one we ink with; switch just for the viewport blit */
        self.redraw_view_wave(RefreshMode::Ui);
    }

    /// Repaint the viewport with a specific e-ink waveform. Scrolling uses
    /// the fast, flash-free UltraFast one; streaming/settled text uses the
    /// quality one so the antialiasing is clean.
    fn redraw_view_wave(&mut self, wave: RefreshMode) {
        self.use_wave(wave);
        conv::draw(&mut self.fb, &self.entries, VIEW_Y0, VIEW_Y1, self.scroll, self.pi_px);
        let _ = self.sock.update_region(0, VIEW_Y0, FB_W, VIEW_Y1 - VIEW_Y0);
        self.view_dirty = false;
        self.last_view_flush = Instant::now();
    }

    /// Switch the e-ink waveform mode, but only when it actually changes.
    fn use_wave(&mut self, m: RefreshMode) {
        if self.wave != m as i32 {
            self.wave = m as i32;
            let _ = self.sock.set_refresh_mode(m);
        }
    }

    /* -- buttons, font size, deghosting -- */

    /// Handle a press on any button (works for pen or finger). Returns true
    /// if a button was hit, so callers skip their drawing/scrolling logic.
    fn handle_button(&mut self, x: i32, y: i32) -> bool {
        if in_rect(x, y, SEND_X, BTN_Y, SEND_W, BTN_H) {
            self.send_message();
        } else if in_rect(x, y, CLEAR_X, BTN_Y, CLEAR_W, BTN_H) {
            self.draw_input_strip(true);
        } else if in_rect(x, y, FDN_X, HB_Y, AZ_W, HB_H) {
            self.change_scale(-1);
        } else if in_rect(x, y, FUP_X, HB_Y, AZ_W, HB_H) {
            self.change_scale(1);
        } else if in_rect(x, y, REFRESH_X, HB_Y, REFRESH_W, HB_H) {
            self.deghost_now();
        } else if y >= NIB_Y && y < NIB_Y + NIB_H && x >= NIB0_X && x < nib_x(2) + NIB_W {
            /* which of the three nib buttons (ignoring the inter-button gaps) */
            let i = ((x - NIB0_X) / (NIB_W + 10)).clamp(0, 2) as usize;
            if in_rect(x, y, nib_x(i as i32), NIB_Y, NIB_W, NIB_H) {
                self.set_nib(i);
            }
        } else {
            return false;
        }
        true
    }

    fn change_scale(&mut self, delta: i32) {
        let new = (self.pi_px + delta * PI_PX_STEP).clamp(PI_PX_MIN, PI_PX_MAX);
        if new == self.pi_px {
            return;
        }
        self.pi_px = new;
        /* keep the view pinned to the bottom if it was; else clamp */
        self.scroll = if self.stuck { self.max_scroll() } else { self.scroll.min(self.max_scroll()) };
        self.redraw_view();
        /* the reflowed text sits over the old text's ghost: flash it away,
         * but on a delay so rapid A+/A- taps coalesce into one flash */
        self.schedule_deghost();
    }

    /// Arrange a single cleanup flash once things settle (coalescing).
    fn schedule_deghost(&mut self) {
        self.deghost_at = Some(Instant::now() + DEGHOST_DELAY);
    }

    /// Full-panel refresh: repaint from the current framebuffer and ask for
    /// the deghosting waveform. Clears accumulated partial-update residue.
    fn deghost_now(&mut self) {
        self.deghost_at = None;
        let _ = self.sock.update_all();
        let _ = self.sock.request_full_refresh();
        self.last_view_flush = Instant::now();
    }

    /// Auto-follow the bottom if we're stuck there, then mark the viewport
    /// for a (throttled) repaint.
    fn content_changed(&mut self) {
        if self.stuck {
            self.scroll = self.max_scroll();
        }
        self.view_dirty = true;
    }

    /* -- pen writing -- */

    fn ink_at(&mut self, x: i32, y: i32, r: i32, color: u16) {
        if y - r >= CANVAS_Y0 && y + r < CANVAS_Y1 {
            self.fb.disc(x, y, r, color);
            let (x0, y0, x1, y1) = (x - r, y - r, x + r, y + r);
            self.ink_dirty = Some(match self.ink_dirty {
                None => (x0, y0, x1, y1),
                Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
            });
        }
    }

    fn pen_stroke(&mut self, x: i32, y: i32, r: i32, color: u16) {
        let (x0, y0) = self.pen_last.unwrap_or((x, y));
        let steps = (x - x0).abs().max((y - y0).abs()) + 1;
        for i in 0..=steps {
            self.ink_at(x0 + (x - x0) * i / steps, y0 + (y - y0) * i / steps, r, color);
        }
        self.pen_last = Some((x, y));
    }

    /// A pen frame (from the digitizer or, as fallback, AppLoad). The pen
    /// writes only inside the canvas; a press elsewhere hits buttons.
    fn pen_point(&mut self, phase: PenPhase, x: i32, y: i32, pressure: i32, rubber: bool) {
        self.last_pen = Some(Instant::now());
        match phase {
            PenPhase::Press => {
                if self.handle_button(x, y) {
                    /* a button took the press */
                } else if y >= CANVAS_Y0 {
                    self.pen_last = None;
                    let (r, c) = brush(self.nib, pressure, rubber);
                    self.pen_stroke(x, y, r, c);
                }
            }
            PenPhase::Move if self.pen_last.is_some() => {
                let (r, c) = brush(self.nib, pressure, rubber);
                self.pen_stroke(x, y, r, c);
            }
            PenPhase::Move => {}
            PenPhase::Release => self.pen_last = None,
        }
    }

    /* -- finger: scroll + buttons -- */

    fn touch(&mut self, phase: Phase, x: i32, y: i32) {
        /* palm rejection: ignore touch while the pen is around */
        if self.last_pen.is_some_and(|t| t.elapsed() < PEN_TIMEOUT) {
            return;
        }
        match phase {
            Phase::Press => {
                if self.handle_button(x, y) {
                    /* a button took the tap */
                } else if y >= VIEW_Y0 && y < VIEW_Y1 {
                    self.drag_last = Some(y); /* begin a scroll drag */
                }
            }
            Phase::Move => {
                if let Some(prev) = self.drag_last {
                    /* finger up (y shrinks) reveals newer content below.
                     * We move the scroll position but do NOT repaint the
                     * e-ink mid-drag — repainting every frame smears the
                     * panel. The view is redrawn once, on release. */
                    let new = (self.scroll + (prev - y)).clamp(0, self.max_scroll());
                    if new != self.scroll {
                        self.scroll = new;
                        self.stuck = self.scroll >= self.max_scroll() - 4;
                        self.scroll_pending = true;
                    }
                    self.drag_last = Some(y);
                }
            }
            Phase::Release => {
                if self.drag_last.take().is_some() && self.scroll_pending {
                    /* the drag ended: paint the final position ONCE with the
                     * fast flash-free waveform, and do NOT deghost. Scrolling
                     * stays flicker-free; any residue is cleared on demand
                     * with the REFRESH button. */
                    self.scroll_pending = false;
                    self.redraw_view_wave(RefreshMode::UltraFast);
                }
            }
        }
    }

    /* -- send: snapshot ink -> log + pi -- */

    fn send_message(&mut self) {
        let Some(img) = self.snapshot_ink() else {
            self.set_status("nothing written");
            return;
        };
        let streaming = self.streaming;
        if let Some(pi) = self.pi.as_mut() {
            let png_gray: Vec<u8> = img.px.clone();
            if let Err(e) = pi.send_ink(&png_gray, img.w as u32, img.h as u32, streaming) {
                self.entries.push(Entry::Note(format!("[send failed: {e}]")));
            }
        }
        history::append_you(&img); /* persist for scrollback next launch */
        self.entries.push(Entry::You(img));
        self.live_pi = None; /* pi's reply starts a fresh bubble */
        self.stuck = true;
        self.scroll = self.max_scroll();
        if self.pi.is_some() {
            self.streaming = true; /* show the working dot until pi replies */
            self.status.clear();
        }
        self.draw_header();
        self.redraw_view();
        self.draw_input_strip(true); /* wipe the canvas for the next message */
    }

    /// Read the canvas out of the framebuffer, crop to the ink's bounding
    /// box, downscale to <= SNAP_W x SNAP_H, return it as grayscale (0/255).
    fn snapshot_ink(&mut self) -> Option<GrayImg> {
        let px = self.fb.pixels();
        let dark = |x: i32, y: i32| -> bool {
            let v = px[(y * FB_W + x) as usize];
            ((v >> 5) & 0x3F) < 32 /* green channel < half => inked */
        };
        let (mut x0, mut y0, mut x1, mut y1) = (FB_W, CANVAS_Y1, 0, CANVAS_Y0);
        for y in CANVAS_Y0..CANVAS_Y1 {
            for x in 0..FB_W {
                if dark(x, y) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        if x1 < x0 {
            return None; /* blank canvas */
        }
        /* pad, clamp */
        let pad = 12;
        x0 = (x0 - pad).max(0);
        y0 = (y0 - pad).max(CANVAS_Y0);
        x1 = (x1 + pad).min(FB_W - 1);
        y1 = (y1 + pad).min(CANVAS_Y1 - 1);
        let (sw, sh) = (x1 - x0 + 1, y1 - y0 + 1);
        let factor = 1.max((sw + SNAP_W - 1) / SNAP_W).max((sh + SNAP_H - 1) / SNAP_H);
        let (ow, oh) = (sw / factor, sh / factor);
        let mut out = vec![255u8; (ow * oh) as usize];
        /* nearest-with-ink: a block is black if ANY source pixel is inked,
         * so thin strokes survive downscaling */
        for oy in 0..oh {
            for ox in 0..ow {
                let mut inked = false;
                'blk: for by in 0..factor {
                    for bx in 0..factor {
                        if dark(x0 + ox * factor + bx, y0 + oy * factor + by) {
                            inked = true;
                            break 'blk;
                        }
                    }
                }
                if inked {
                    out[(oy * ow + ox) as usize] = 0;
                }
            }
        }
        Some(GrayImg { w: ow, h: oh, px: out })
    }

    /* -- pi events -- */

    fn handle_pi(&mut self, ev: PiEvent) {
        match ev {
            PiEvent::Start => {
                self.streaming = true;
                self.reply_buf.clear();
                self.status.clear();
                self.draw_header(); /* show the working dot */
            }
            PiEvent::Delta(d) => {
                let idx = match self.live_pi {
                    Some(i) => i,
                    None => {
                        self.entries.push(Entry::Pi(String::new()));
                        self.live_pi = Some(self.entries.len() - 1);
                        self.entries.len() - 1
                    }
                };
                if let Entry::Pi(t) = &mut self.entries[idx] {
                    t.push_str(&d);
                }
                self.reply_buf.push_str(&d); /* accumulate for history */
                self.content_changed();
            }
            PiEvent::Notice(n) => {
                self.entries.push(Entry::Note(n));
                self.live_pi = None; /* text after a note starts a new bubble */
                self.content_changed();
            }
            PiEvent::End => {
                self.streaming = false;
                self.live_pi = None;
                if !self.reply_buf.trim().is_empty() {
                    history::append_pi(&self.reply_buf); /* persist the reply */
                }
                self.status.clear();
                self.draw_header(); /* clear the working dot */
                self.content_changed();
                /* a streamed reply is many partial updates; deghost once */
                self.schedule_deghost();
            }
            PiEvent::Died(reason) => {
                self.streaming = false;
                self.pi = None;
                self.entries.push(Entry::Note(format!("[pi exited: {reason}]")));
                self.set_status("pi gone");
                self.content_changed();
            }
        }
    }
}

/// Brush radius + color for a pen frame. The eraser paints white and wide;
/// the tip inks black, its width the selected nib's base plus a little from
/// real pressure (0..4095).
fn brush(nib: usize, pressure: i32, rubber: bool) -> (i32, u16) {
    if rubber {
        (ERASER_R, WHITE)
    } else {
        (NIB_BASE[nib] + pressure * 2 / 4096, BLACK)
    }
}

/* ---- main ---------------------------------------------------------------- */

fn main() -> std::process::ExitCode {
    let (fb, sock) = match qtfb::connect() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("pi-collab: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("pi-collab: up, fb={FB_W}x{FB_H}");
    install_signal_handlers();
    let _ = sock.set_refresh_mode(RefreshMode::UltraFast);

    let pi = match Pi::spawn() {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("pi-collab: could not start pi: {e}");
            None
        }
    };
    let mut pen = Pen::open();
    /* the digitizer is authoritative whenever it's present: it carries
     * pressure and the eraser, which AppLoad's pen events don't. We only
     * ink via AppLoad when there's no digitizer at all (e.g. the preview
     * harness). pi-collab is meant to run fullscreen — see the note below on
     * why we don't try to auto-detect windowed mode. */
    let direct_pen = pen.is_some();

    /* reload the past conversation so scrolling up shows history */
    let mut entries = history::load();
    if entries.is_empty() {
        entries.push(Entry::Note(
            "write a message below and tap send. pi is listening.".into(),
        ));
    } else {
        /* ASCII only — the Note bitmap font has no em-dash glyph */
        entries.push(Entry::Note("---------- new ----------".into()));
    }

    let now = Instant::now();
    let mut app = App {
        fb,
        sock,
        pi,
        entries,
        scroll: 0,
        stuck: true,
        live_pi: None,
        streaming: false,
        reply_buf: String::new(),
        status: String::new(),
        pi_px: PI_PX_DEFAULT,
        nib: 2, /* largest by default — the nib we had before */
        deghost_at: None,
        wave: RefreshMode::UltraFast as i32,
        scroll_pending: false,
        pen_last: None,
        ink_dirty: None,
        last_ink_flush: now,
        last_pen: None,
        drag_last: None,
        view_dirty: false,
        last_view_flush: now,
    };
    if app.pi.is_none() {
        app.entries
            .push(Entry::Note("[pi did not start — check the journal]".into()));
    }

    /* first paint */
    app.fb.fill_rect(0, 0, FB_W, FB_H, WHITE);
    app.draw_header();
    app.redraw_view();
    app.draw_input_strip(true);
    let _ = app.sock.update_all();
    let _ = app.sock.request_full_refresh();

    while RUNNING.load(Ordering::Relaxed) {
        /* poll the qtfb socket, the pen hardware, and pi's stdout; the
         * timeout is whichever pending flush comes due first */
        let timeout = next_timeout(&app);
        let mut pfds = [
            libc::pollfd { fd: app.sock.raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd {
                fd: pen.as_ref().map_or(-1, |p| p.raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: app.pi.as_ref().map_or(-1, |p| p.raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(pfds.as_mut_ptr(), 3, timeout) } < 0 {
            continue; /* EINTR */
        }

        /* -- pen hardware -- */
        if pfds[1].revents & libc::POLLIN != 0 {
            if let Some(p) = pen.as_mut() {
                let mut frames = Vec::new();
                let seen = p.drain(|p, phase| {
                    frames.push((phase, p.sx, p.sy, p.pressure, p.rubber));
                });
                if seen {
                    app.last_pen = Some(Instant::now());
                }
                if direct_pen {
                    for (phase, x, y, pr, rub) in frames {
                        app.pen_point(phase, x, y, pr, rub);
                    }
                }
            }
        }

        /* -- qtfb socket: touch, AppLoad pen fallback, window lifecycle -- */
        if pfds[0].revents & libc::POLLIN != 0 {
            while let Some(event) = app.sock.try_next_event() {
                match event {
                    Event::Closed => { RUNNING.store(false, Ordering::Relaxed); break; }
                    Event::Interrupted => continue,
                    Event::Touch { phase, x, y, .. } => app.touch(phase, x, y),
                    Event::Pen { phase, x, y, .. } => {
                        app.last_pen = Some(Instant::now());
                        /* With a digitizer present, AppLoad's pen events are a
                         * lower-fidelity mirror of what we already read from
                         * the hardware (no pressure, no eraser) — ignore them.
                         * We deliberately do NOT auto-switch to them for
                         * "windowed mode": the old coordinate-mismatch test
                         * false-fired on a stale digitizer sample during a
                         * fast pen-down and permanently dropped pressure + the
                         * eraser. pi-collab is fullscreen; only when there's no
                         * digitizer at all do we ink from AppLoad. */
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

        /* -- due flushes -- */
        if app.ink_dirty.is_some() && app.last_ink_flush.elapsed() >= INK_FLUSH {
            let (x0, y0, x1, y1) = app.ink_dirty.take().unwrap();
            app.use_wave(RefreshMode::UltraFast); /* pen ink wants low latency */
            let _ = app
                .sock
                .update_region(x0.max(0), y0.max(0), x1 - x0 + 1, y1 - y0 + 1);
            app.last_ink_flush = Instant::now();
        }
        /* don't repaint the viewport mid-drag — scrolling defers to release */
        if app.view_dirty && app.drag_last.is_none() && app.last_view_flush.elapsed() >= VIEW_FLUSH {
            app.redraw_view();
        }
        /* deghost only once everything else has settled — never mid-stroke,
         * mid-scroll, or with a pending viewport repaint */
        if let Some(at) = app.deghost_at {
            if Instant::now() >= at
                && !app.view_dirty
                && app.ink_dirty.is_none()
                && app.drag_last.is_none()
            {
                app.deghost_now();
            }
        }
    }

    println!("pi-collab: exiting");
    std::process::ExitCode::SUCCESS
}

/// Milliseconds until the next pending flush is due (-1 = sleep until input).
fn next_timeout(app: &App) -> i32 {
    let mut t: Option<Duration> = None;
    let mut soonest = |d: Duration| {
        t = Some(t.map_or(d, |cur| cur.min(d)));
    };
    if app.ink_dirty.is_some() {
        soonest(INK_FLUSH.saturating_sub(app.last_ink_flush.elapsed()));
    }
    if app.view_dirty {
        soonest(VIEW_FLUSH.saturating_sub(app.last_view_flush.elapsed()));
    }
    if let Some(at) = app.deghost_at {
        soonest(at.saturating_duration_since(Instant::now()));
    }
    match t {
        Some(d) => d.as_millis() as i32,
        None => -1,
    }
}
