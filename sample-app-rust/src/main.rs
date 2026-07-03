//! sample-app-rs — a minimal AppLoad app for the reMarkable 2, in Rust.
//!
//! Same doodle pad as ../sample-app (the C version), same on-screen UI:
//! draw with the pen (its eraser end erases), fingers draw too when the
//! pen is away, CLEAR wipes the canvas, EXIT quits, tapping the title
//! cycles the e-ink refresh mode. Read this file top to bottom and you've
//! seen everything an AppLoad app does.
//!
//! Module map:  qtfb.rs  — the AppLoad wire protocol (socket + shm + events)
//!              pen.rs   — direct Wacom digitizer input (low-latency ink)
//!              draw.rs  — pixel/rect/disc/text primitives
//!              font.rs  — a tiny 5x7 bitmap font
//!
//! Ink comes from TWO sources. The pen is read straight from the
//! digitizer hardware when possible (see pen.rs for why: AppLoad's
//! forwarding stalls during e-ink refreshes), while touch — and the pen
//! too, as a fallback — arrives as AppLoad messages over the qtfb socket.
//!
//! The app is launched BY AppLoad (tap the icon in the xochitl sidebar
//! menu); from a shell there is no QTFB_KEY and no qtfb server, so it just
//! prints an error. println!() output lands in xochitl's journal —
//! `make log` tails it.

mod draw;
mod font;
mod pen;
mod qtfb;

use draw::{text_width, BLACK, GRAY, WHITE};
use pen::{Pen, PenPhase};
use qtfb::{
    Event, Framebuffer, Phase, RefreshMode, Socket, RM2_HEIGHT as FB_H, RM2_WIDTH as FB_W,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/* ---- UI layout (all in framebuffer pixels) ------------------------------ */

const HEADER_H: i32 = 140; /* title bar height */
const FOOTER_H: i32 = 80; /* hint line at the bottom */
const BTN_W: i32 = 260;
const BTN_H: i32 = 88;
const BTN_Y: i32 = 26;
const BTN_EXIT_X: i32 = FB_W - 24 - BTN_W;
const BTN_CLEAR_X: i32 = BTN_EXIT_X - 24 - BTN_W;

/* the drawable canvas is everything between header and footer */
const CANVAS_Y0: i32 = HEADER_H + 4;
const CANVAS_Y1: i32 = FB_H - FOOTER_H;

const BRUSH_R: i32 = 4; /* finger stroke half-width */
const ERASER_R: i32 = 20; /* the Marker's tail erases with a wide brush */

/* At most one e-ink update per FLUSH_EVERY while inking: refreshes have a
 * fixed per-update cost through AppLoad's Qt pipeline, so 200 tiny ones a
 * second are slower than ~80 slightly larger ones. Tune 10-30ms to taste. */
const FLUSH_EVERY: Duration = Duration::from_millis(12);

/* Palm rejection: while the pen is on (or hovering near) the screen, the
 * touchscreen mostly reports the writing hand — ignore touch, buttons
 * included, until the pen has been away this long. */
const PEN_TIMEOUT: Duration = Duration::from_millis(1500);

/* E-ink waveforms cycled by tapping the title; which one is fastest
 * varies between OS versions, so feel-test on your device. */
const MODES: [(RefreshMode, &str); 5] = [
    (RefreshMode::UltraFast, "UFAST"),
    (RefreshMode::Fast, "FAST"),
    (RefreshMode::Animate, "ANIM"),
    (RefreshMode::Content, "CONTENT"),
    (RefreshMode::Ui, "UI"),
];

/// Flipped by the signal handler; the event loop checks it every pass.
/// Installed with SA_RESTART off so a signal actually interrupts a
/// blocking poll() instead of silently restarting it.
static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn on_signal(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as *const () as usize; /* sa_flags stays 0: no SA_RESTART */
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        /* no SIGPIPE handling needed: Rust's runtime already ignores it,
         * so a send() to a closed window is an Err, not a crash */
    }
}

/* ---- brushes -------------------------------------------------------------- */

#[derive(Clone, Copy)]
struct Brush {
    r: i32,
    color: u16,
}

/// Fingers — and the pen when it arrives via AppLoad, which only ever
/// reports pressure as 0 or 100 — get the plain fixed brush.
const FINGER_BRUSH: Brush = Brush { r: BRUSH_R, color: BLACK };

/// The directly-read pen gets real pressure (0..4095) and an eraser end.
fn pen_brush(p: &Pen) -> Brush {
    if p.rubber {
        Brush { r: ERASER_R, color: WHITE }
    } else if p.pressure > 0 {
        Brush { r: 2 + p.pressure * 5 / 4096, color: BLACK }
    } else {
        FINGER_BRUSH
    }
}

/* ---- the scene ----------------------------------------------------------- */

fn draw_button(fb: &mut Framebuffer, x: i32, label: &str) {
    fb.fill_rect(x, BTN_Y, BTN_W, BTN_H, WHITE);
    fb.rect_outline(x, BTN_Y, BTN_W, BTN_H, 4, BLACK);
    fb.text(
        x + (BTN_W - text_width(label, 4)) / 2,
        BTN_Y + (BTN_H - 7 * 4) / 2,
        label,
        4,
        BLACK,
    );
}

fn draw_scene(fb: &mut Framebuffer, sock: &Socket) {
    fb.fill_rect(0, 0, FB_W, FB_H, WHITE);

    /* header: title + buttons + separator line */
    fb.text(32, (HEADER_H - 7 * 6) / 2, "SAMPLE APP RS", 6, BLACK);
    draw_button(fb, BTN_CLEAR_X, "CLEAR");
    draw_button(fb, BTN_EXIT_X, "EXIT");
    fb.fill_rect(0, HEADER_H, FB_W, 4, BLACK);

    /* footer hint */
    fb.fill_rect(0, FB_H - FOOTER_H - 4, FB_W, 2, GRAY);
    fb.text(32, FB_H - FOOTER_H + 20, "DRAW WITH PEN OR FINGER", 3, GRAY);

    let _ = sock.update_all();
}

fn clear_canvas(fb: &mut Framebuffer, sock: &Socket) {
    fb.fill_rect(0, CANVAS_Y0, FB_W, CANVAS_Y1 - CANVAS_Y0, WHITE);
    let _ = sock.update_all();
    /* deghost — leftover strokes would shadow through otherwise */
    let _ = sock.request_full_refresh();
}

fn cycle_refresh_mode(fb: &mut Framebuffer, sock: &Socket, idx: &mut usize) {
    *idx = (*idx + 1) % MODES.len();
    let (mode, name) = MODES[*idx];
    let _ = sock.set_refresh_mode(mode);
    /* show the active mode at the bottom-right */
    let x = FB_W - 32 - text_width("MODE:CONTENT", 3);
    let y = FB_H - FOOTER_H + 20;
    fb.fill_rect(x, y, FB_W - x, 7 * 3, WHITE);
    fb.text(x, y, &format!("MODE:{name}"), 3, GRAY);
    let _ = sock.update_region(x, y, FB_W - x, 7 * 3);
    println!("sample-app-rs: refresh mode -> {name}");
}

/* ---- strokes --------------------------------------------------------------
 * We remember the previous point per input source and stamp a line of brush
 * discs from it to the new point. Touch can have several fingers down at
 * once (each gets a slot); the pen is its own source and gets the last slot.
 *
 * Refreshes are BATCHED: input events come far faster than e-ink can
 * refresh, so drawing only grows `dirty`, and the main loop flushes ONE
 * update covering all of it — at most one per FLUSH_EVERY. */

const SLOTS: usize = 16;
const PEN_SLOT: usize = SLOTS - 1;

struct Strokes {
    last: [Option<(i32, i32)>; SLOTS],
    dirty: Option<(i32, i32, i32, i32)>, /* x0, y0, x1, y1 — not yet refreshed */
}

impl Strokes {
    fn new() -> Self {
        Strokes { last: [None; SLOTS], dirty: None }
    }

    fn freeze_touch(&mut self) {
        /* pen-down: whatever the palm was drawing must stop growing */
        for s in self.last[..PEN_SLOT].iter_mut() {
            *s = None;
        }
    }

    fn stroke_to(&mut self, fb: &mut Framebuffer, slot: usize, x: i32, y: i32, b: Brush) {
        let (x0, y0) = self.last[slot].unwrap_or((x, y));
        let (dx, dy) = (x - x0, y - y0);
        let steps = dx.abs().max(dy.abs()) + 1;

        for i in 0..=steps {
            let sx = x0 + dx * i / steps;
            let sy = y0 + dy * i / steps;
            /* clamp the stamp into the canvas so strokes can't paint the UI */
            if sy - b.r >= CANVAS_Y0 && sy + b.r < CANVAS_Y1 {
                fb.disc(sx, sy, b.r, b.color);
            }
        }
        self.last[slot] = Some((x, y));

        /* grow the dirty box by this segment (padded by the brush radius) */
        let (sx0, sy0) = (x0.min(x) - b.r, y0.min(y) - b.r);
        let (sx1, sy1) = (x0.max(x) + b.r, y0.max(y) + b.r);
        self.dirty = Some(match self.dirty {
            None => (sx0, sy0, sx1, sy1),
            Some((dx0, dy0, dx1, dy1)) => {
                (dx0.min(sx0), dy0.min(sy0), dx1.max(sx1), dy1.max(sy1))
            }
        });
    }

    fn flush(&mut self, sock: &Socket) {
        if let Some((x0, y0, x1, y1)) = self.dirty.take() {
            let _ = sock.update_region(x0.max(0), y0.max(0), x1 - x0 + 1, y1 - y0 + 1);
        }
    }
}

fn in_rect(x: i32, y: i32, rx: i32, ry: i32, rw: i32, rh: i32) -> bool {
    x >= rx && x < rx + rw && y >= ry && y < ry + rh
}

/// A press from EITHER source (AppLoad message or the digitizer itself):
/// buttons and the title's mode-cycler first, otherwise begin a stroke.
fn pointer_press(
    fb: &mut Framebuffer,
    sock: &Socket,
    strokes: &mut Strokes,
    mode_idx: &mut usize,
    slot: usize,
    x: i32,
    y: i32,
    brush: Brush,
) {
    if in_rect(x, y, BTN_EXIT_X, BTN_Y, BTN_W, BTN_H) {
        RUNNING.store(false, Ordering::Relaxed);
    } else if in_rect(x, y, BTN_CLEAR_X, BTN_Y, BTN_W, BTN_H) {
        strokes.dirty = None; /* pending strokes just got wiped anyway */
        clear_canvas(fb, sock);
    } else if in_rect(x, y, 0, 0, 560, HEADER_H) {
        cycle_refresh_mode(fb, sock, mode_idx);
    } else {
        strokes.last[slot] = None; /* start fresh: a dot, not a jump */
        strokes.stroke_to(fb, slot, x, y, brush);
    }
}

/* ---- main ------------------------------------------------------------------ */

fn main() -> std::process::ExitCode {
    let (mut fb, sock) = match qtfb::connect() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("sample-app-rs: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("sample-app-rs: up, fb={FB_W}x{FB_H}");

    install_signal_handlers();
    draw_scene(&mut fb, &sock);
    /* ink with the low-latency waveform; tap the title to cycle others */
    let _ = sock.set_refresh_mode(RefreshMode::UltraFast);

    let mut pen = Pen::open();
    /* true while the digitizer inks the pen; turns off in windowed mode */
    let mut direct_pen = pen.is_some();

    let mut strokes = Strokes::new();
    let mut mode_idx = 0usize; /* position in MODES; starts at UltraFast */
    let mut last_flush = Instant::now();
    let mut last_pen: Option<Instant> = None;

    'main: while RUNNING.load(Ordering::Relaxed) {
        /* Wait for a qtfb message or pen hardware input; while strokes are
         * pending the timeout is the flush deadline, so ink hits the
         * screen at most FLUSH_EVERY late. */
        let timeout: i32 = if strokes.dirty.is_some() {
            FLUSH_EVERY.saturating_sub(last_flush.elapsed()).as_millis() as i32
        } else {
            -1 /* idle: sleep until input arrives */
        };
        let mut pfds = [
            libc::pollfd { fd: sock.raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd {
                fd: pen.as_ref().map_or(-1, |p| p.raw_fd()), /* -1: poll skips it */
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(pfds.as_mut_ptr(), 2, timeout) } < 0 {
            continue; /* EINTR: a signal fired; RUNNING decides */
        }

        /* -- pen hardware: lowest-latency ink, pressure, eraser -- */
        if pfds[1].revents & libc::POLLIN != 0 {
            if let Some(p) = pen.as_mut() {
                let seen = p.drain(|p, phase| {
                    if !direct_pen {
                        return; /* windowed: AppLoad's events ink instead */
                    }
                    match phase {
                        PenPhase::Press => {
                            strokes.freeze_touch();
                            pointer_press(
                                &mut fb, &sock, &mut strokes, &mut mode_idx,
                                PEN_SLOT, p.sx, p.sy, pen_brush(p),
                            );
                        }
                        PenPhase::Move if strokes.last[PEN_SLOT].is_some() => {
                            strokes.stroke_to(&mut fb, PEN_SLOT, p.sx, p.sy, pen_brush(p));
                        }
                        PenPhase::Move => {}
                        PenPhase::Release => strokes.last[PEN_SLOT] = None,
                    }
                });
                if seen {
                    last_pen = Some(Instant::now()); /* hover counts too */
                }
            }
        }

        /* -- qtfb socket: touch input, pen fallback, window lifecycle -- */
        if pfds[0].revents & libc::POLLIN != 0 {
            loop {
                let Some(event) = sock.try_next_event() else { break };

                /* palm rejection + windowed-mode detection */
                match event {
                    Event::Closed => break 'main, /* AppLoad closed our window */
                    Event::Interrupted => continue,
                    Event::Pen { phase, x, y, .. } => {
                        if phase == Phase::Press {
                            strokes.freeze_touch();
                        }
                        last_pen = Some(Instant::now());
                        if direct_pen {
                            /* The digitizer inks the pen; AppLoad's delayed
                             * copies only serve as a sanity check. Their
                             * coords are WINDOW-relative — disagreement
                             * means we're windowed (long-press launch) and
                             * the screen mapping is wrong: fall back. */
                            let mismatch = pen.as_ref().is_some_and(|p| {
                                (p.sx != 0 || p.sy != 0)
                                    && (x - p.sx).abs() + (y - p.sy).abs() > 150
                            });
                            if phase == Phase::Press && mismatch {
                                println!("sample-app-rs: windowed? falling back to AppLoad pen");
                                direct_pen = false;
                            } else {
                                continue;
                            }
                        }
                    }
                    Event::Touch { .. }
                        if last_pen.is_some_and(|t| t.elapsed() < PEN_TIMEOUT) =>
                    {
                        continue; /* that "touch" is a palm */
                    }
                    _ => {}
                }

                /* normalize pen and touch into (slot, phase, x, y) */
                let (slot, phase, x, y) = match event {
                    Event::Touch { id, phase, x, y } => {
                        (id.rem_euclid(PEN_SLOT as i32) as usize, phase, x, y)
                    }
                    Event::Pen { phase, x, y, .. } => (PEN_SLOT, phase, x, y),
                    Event::Key { code, pressed: true } => {
                        println!("sample-app-rs: key {code:#x}");
                        continue;
                    }
                    _ => continue,
                };

                match phase {
                    Phase::Press => pointer_press(
                        &mut fb, &sock, &mut strokes, &mut mode_idx, slot, x, y,
                        FINGER_BRUSH,
                    ),
                    /* only extend strokes that began on the canvas — a drag
                     * that started on a button stays inert */
                    Phase::Move if strokes.last[slot].is_some() => {
                        strokes.stroke_to(&mut fb, slot, x, y, FINGER_BRUSH);
                    }
                    Phase::Move => {}
                    Phase::Release => strokes.last[slot] = None,
                }
            }
        }

        if strokes.dirty.is_some() && last_flush.elapsed() >= FLUSH_EVERY {
            strokes.flush(&sock);
            last_flush = Instant::now();
        }
    }

    println!("sample-app-rs: exiting");
    /* Drop on `sock` sends MESSAGE_TERMINATE; Drop on `fb` munmaps */
    std::process::ExitCode::SUCCESS
}
