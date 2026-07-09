//! Display backends, selected at runtime (the riddle pattern): if QTFB_KEY
//! is set we're a windowed AppLoad app talking to qtfb inside xochitl;
//! otherwise we assume full takeover and connect to a running rm2fb server
//! (xochitl stopped — scripts/takeover.sh owns that).
//!
//! Both backends expose the same three operations on the shared RGB565
//! framebuffer (fb.rs), each taking the waveform intent per call:
//!
//!   update(rect, Wave)  — Ink: lowest-latency near-binary waveform
//!                         (qtfb UltraFast / rm2fb DU+priority); right for
//!                         pen strokes, chrome, and scroll repaints.
//!                         Text: quality flash-free waveform (qtfb Ui /
//!                         rm2fb GL16); right for antialiased text.
//!   update_all()        — blit the whole framebuffer, no flash
//!   full_refresh()      — the flashing deghost pass
//!
//! qtfb has no per-update waveform, only a sticky mode — the Qtfb arm
//! caches it and re-sends only on change, which is exactly what the old
//! use_wave() did in main.rs.

use crate::fb::{Framebuffer, SCREEN_H, SCREEN_W};
use crate::qtfb::{self, Event, RefreshMode, Socket};
use crate::rm2fb::Rm2fbClient;
use std::cell::Cell;
use std::io;
use std::os::unix::io::RawFd;

#[derive(Clone, Copy, PartialEq)]
pub enum Wave {
    /// Low latency, near-binary (pen ink, buttons, scroll repaints).
    Ink,
    /// Flash-free quality (antialiased text on a clear area). Right when the
    /// region underneath is already white — status bar, home thumbnails, an
    /// ink-settle over untouched print.
    Text,
    /// Partial GC16 (no FULL flag): drives only the changed pixels through the
    /// rails. Crisp for bold vector ink (notebook page turns) — but it
    /// speckles fine antialiased greys, so it's NOT used for print. See
    /// render_doc_full.
    Page,
    /// Full-page flash-free 16-level (GL16 + FULL) for PRINT (PDF) page turns:
    /// every pixel driven with the real 16-level LUT, so antialiased print
    /// stays smooth (a partial pass speckles it), and no clearing phase so the
    /// page eases over without a flash. See render_doc_full.
    Print,
}

pub enum Display {
    Qtfb { sock: Socket, mode: Cell<i32> },
    Rm2fb(Rm2fbClient),
}

impl Display {
    pub fn open() -> io::Result<(Self, Framebuffer)> {
        if std::env::var("QTFB_KEY").is_ok() {
            let (fb, sock) = qtfb::connect()?;
            let _ = sock.set_refresh_mode(RefreshMode::UltraFast);
            let disp = Display::Qtfb { sock, mode: Cell::new(RefreshMode::UltraFast as i32) };
            return Ok((disp, fb));
        }
        let (client, fb) = Rm2fbClient::connect()?;
        Ok((Display::Rm2fb(client), fb))
    }

    /// Whether we own the whole panel (xochitl stopped): input devices and
    /// the power button are ours to read, and there's no window to lose.
    pub fn is_takeover(&self) -> bool {
        matches!(self, Display::Rm2fb(_))
    }

    /// The qtfb socket fd for poll(), or -1 in takeover (poll ignores it).
    pub fn raw_fd(&self) -> RawFd {
        match self {
            Display::Qtfb { sock, .. } => sock.raw_fd(),
            Display::Rm2fb(_) => -1,
        }
    }

    /// Push one region to the panel with the given waveform intent.
    pub fn update(&self, x: i32, y: i32, w: i32, h: i32, wave: Wave) {
        match self {
            Display::Qtfb { sock, mode } => {
                /* qtfb is preview-only and has no per-pixel-clearing partial
                 * mode; Page renders like Text there (the real distinction
                 * only exists on the device's e-ink engine). */
                let m = match wave {
                    Wave::Ink => RefreshMode::UltraFast,
                    Wave::Text | Wave::Page | Wave::Print => RefreshMode::Ui,
                };
                if mode.get() != m as i32 {
                    mode.set(m as i32);
                    let _ = sock.set_refresh_mode(m);
                }
                let _ = sock.update_region(x, y, w, h);
            }
            Display::Rm2fb(c) => match wave {
                Wave::Ink => {
                    let _ = c.update(x, y, w, h, true);
                }
                Wave::Text => {
                    let _ = c.update(x, y, w, h, false);
                }
                Wave::Page => {
                    let _ = c.gc16_partial(x, y, w, h);
                }
                Wave::Print => {
                    let _ = c.gl16_full(x, y, w, h);
                }
            },
        }
    }

    /// Blit the whole framebuffer without the deghosting flash.
    #[allow(dead_code)] /* API — full repaints currently prefer full_refresh */
    pub fn update_all(&self) {
        match self {
            Display::Qtfb { sock, .. } => {
                let _ = sock.update_all();
            }
            Display::Rm2fb(c) => {
                let _ = c.update(0, 0, SCREEN_W, SCREEN_H, false);
            }
        }
    }

    /// Repaint from the current framebuffer with the flashing deghost
    /// waveform. Clears accumulated partial-update residue.
    pub fn full_refresh(&self) {
        match self {
            Display::Qtfb { sock, .. } => {
                let _ = sock.update_all();
                let _ = sock.request_full_refresh();
            }
            Display::Rm2fb(c) => {
                let _ = c.full_refresh();
            }
        }
    }

    /// Drain one queued window-system event (touch/pen/lifecycle). Takeover
    /// has no window system — input comes from the raw devices instead.
    pub fn try_next_event(&self) -> Option<Event> {
        match self {
            Display::Qtfb { sock, .. } => sock.try_next_event(),
            Display::Rm2fb(_) => None,
        }
    }
}
