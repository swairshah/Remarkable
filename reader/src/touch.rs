//! Raw touch input for takeover mode (the cyttsp5 "pt_mt" panel on the rM2).
//!
//! In windowed mode AppLoad forwards finger events over the qtfb socket; in
//! takeover xochitl is stopped, so we read the multitouch device ourselves
//! (type-B slot protocol) and hand main.rs the same Phase/x/y shape it
//! already consumes for scrolling and button taps.
//!
//! Coordinates: the rM2 panel reports pixel-scale values with only the Y
//! axis inverted — screen_y = H - raw_y, X passes through — per
//! timower/rM2-stuff's rMlib (the library the rm2fb server is built from).
//! Ranges are still read from EVIOCGABS and the flips are overridable with
//! READER_TOUCH_FLIP=none|x|y|xy in case another panel revision differs.
//!
//! Only the FIRST finger down drives scroll/taps; extra fingers are counted
//! but ignored — except five at once, the takeover quit gesture (from the
//! riddle app; with xochitl stopped there's no other way out).

use crate::fb::{SCREEN_H, SCREEN_W};
use crate::pen::{RawInputEvent, EVIOCGRAB};
use crate::qtfb::Phase;
use std::io;
use std::os::unix::io::RawFd;

const EV_SYN: u16 = 0;
const EV_ABS: u16 = 3;
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TRACKING_ID: u16 = 57;
const MAX_SLOTS: usize = 16;

/* EVIOCGABS(code) = _IOR('E', 0x40 + code, struct input_absinfo) */
const fn eviocgabs(code: u16) -> libc::c_int {
    (2u32 << 30 | 24 << 16 | (b'E' as u32) << 8 | (0x40 + code as u32)) as libc::c_int
}

#[repr(C)]
#[derive(Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

pub struct TouchEvent {
    pub phase: Phase,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Default)]
struct Slot {
    active: bool,
    x: i32,
    y: i32,
}

pub struct TouchDevice {
    fd: RawFd,
    slots: [Slot; MAX_SLOTS],
    cur: usize,
    /// The slot whose finger drives scroll/taps, and its last emitted spot.
    primary: Option<usize>,
    last_emit: (i32, i32),
    max_x: i32,
    max_y: i32,
    flip_x: bool,
    flip_y: bool,
}

impl TouchDevice {
    /// Find and grab the touch panel.
    pub fn open() -> io::Result<Self> {
        for i in 0..8 {
            let name = std::fs::read_to_string(format!("/sys/class/input/event{i}/device/name"))
                .unwrap_or_default()
                .to_lowercase();
            if !name.contains("pt_mt") && !name.contains("cyttsp5") && !name.contains("touch") {
                continue;
            }
            let path = std::ffi::CString::new(format!("/dev/input/event{i}")).unwrap();
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            unsafe { libc::ioctl(fd, EVIOCGRAB as _, 1i32) };

            let axis_max = |code: u16, fallback: i32| -> i32 {
                let mut info = AbsInfo::default();
                let rc = unsafe { libc::ioctl(fd, eviocgabs(code) as _, &mut info) };
                if rc == 0 && info.maximum > 0 { info.maximum } else { fallback }
            };
            let max_x = axis_max(ABS_MT_POSITION_X, SCREEN_W - 1);
            let max_y = axis_max(ABS_MT_POSITION_Y, SCREEN_H - 1);

            let flip = std::env::var("READER_TOUCH_FLIP").unwrap_or_else(|_| "y".into());
            let (flip_x, flip_y) = (flip.contains('x'), flip.contains('y'));
            println!(
                "reader: touch /dev/input/event{i} ({}x{}, flip {})",
                max_x, max_y, if flip == "none" { "none" } else { &flip }
            );
            return Ok(Self {
                fd,
                slots: [Slot::default(); MAX_SLOTS],
                cur: 0,
                primary: None,
                last_emit: (0, 0),
                max_x,
                max_y,
                flip_x,
                flip_y,
            });
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no touch device"))
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    fn to_screen(&self, s: Slot) -> (i32, i32) {
        let rx = if self.flip_x { self.max_x - s.x } else { s.x };
        let ry = if self.flip_y { self.max_y - s.y } else { s.y };
        (
            (rx * (SCREEN_W - 1) / self.max_x).clamp(0, SCREEN_W - 1),
            (ry * (SCREEN_H - 1) / self.max_y).clamp(0, SCREEN_H - 1),
        )
    }

    /// Drain queued kernel events into per-frame TouchEvents for the primary
    /// finger. Returns (events, quit) — quit latches when 5+ fingers touch.
    pub fn drain(&mut self) -> (Vec<TouchEvent>, bool) {
        let mut out = Vec::new();
        let mut quit = false;
        let mut evs = [RawInputEvent::default(); 64];
        loop {
            let n = unsafe {
                libc::read(
                    self.fd,
                    evs.as_mut_ptr() as *mut libc::c_void,
                    std::mem::size_of_val(&evs),
                )
            };
            if n <= 0 {
                break;
            }
            for ev in &evs[..n as usize / std::mem::size_of::<RawInputEvent>()] {
                match (ev.kind, ev.code) {
                    (EV_ABS, ABS_MT_SLOT) => {
                        self.cur = (ev.value.max(0) as usize).min(MAX_SLOTS - 1);
                    }
                    (EV_ABS, ABS_MT_TRACKING_ID) => {
                        self.slots[self.cur].active = ev.value != -1;
                        if self.slots.iter().filter(|s| s.active).count() >= 5 {
                            quit = true;
                        }
                    }
                    (EV_ABS, ABS_MT_POSITION_X) => self.slots[self.cur].x = ev.value,
                    (EV_ABS, ABS_MT_POSITION_Y) => self.slots[self.cur].y = ev.value,
                    (EV_SYN, _) => self.end_frame(&mut out),
                    _ => {}
                }
            }
        }
        (out, quit)
    }

    /// One SYN_REPORT: reconcile the primary finger against slot state.
    fn end_frame(&mut self, out: &mut Vec<TouchEvent>) {
        match self.primary {
            None => {
                if let Some(i) = self.slots.iter().position(|s| s.active) {
                    self.primary = Some(i);
                    let (x, y) = self.to_screen(self.slots[i]);
                    self.last_emit = (x, y);
                    out.push(TouchEvent { phase: Phase::Press, x, y });
                }
            }
            Some(i) if !self.slots[i].active => {
                self.primary = None; /* a remaining finger may claim next frame */
                let (x, y) = self.last_emit;
                out.push(TouchEvent { phase: Phase::Release, x, y });
            }
            Some(i) => {
                let (x, y) = self.to_screen(self.slots[i]);
                if (x, y) != self.last_emit {
                    self.last_emit = (x, y);
                    out.push(TouchEvent { phase: Phase::Move, x, y });
                }
            }
        }
    }
}

impl Drop for TouchDevice {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.fd, EVIOCGRAB as _, 0i32);
            libc::close(self.fd);
        }
    }
}
