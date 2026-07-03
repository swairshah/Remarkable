//! Direct pen input from the Wacom digitizer.
//!
//! Why bypass AppLoad for the pen: AppLoad forwards input through
//! xochitl's Qt loop, which stalls while the e-ink refreshes — measured
//! on-device, pen positions arrive from it in bursts up to ~50ms apart.
//! Reading /dev/input directly (we run as root) gives ~1ms latency at
//! hardware rate, real pressure (0..4095 — AppLoad only forwards 0/100),
//! and the eraser end of the Marker (which AppLoad's protocol lacks).
//!
//! The digitizer maps to the whole SCREEN, so coordinates are only right
//! when the app runs fullscreen (the default). main.rs detects windowed
//! mode by comparing AppLoad's own pen events against ours and falls back.

use std::os::unix::io::RawFd;

/// Kernel input_event for 32-bit ABIs — defined ourselves because the
/// header's struct grows to 24 bytes under musl's 64-bit time_t while the
/// kernel keeps writing 16-byte records.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInputEvent {
    sec: u32,
    usec: u32,
    kind: u16, /* EV_* ("type" in the kernel struct) */
    code: u16,
    value: i32,
}

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const BTN_TOOL_PEN: u16 = 0x140; /* pen tip enters/leaves hover range */
const BTN_TOOL_RUBBER: u16 = 0x141; /* eraser end (Marker tail) does */
const BTN_TOUCH: u16 = 0x14a; /* either end touches/leaves the glass */
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_PRESSURE: u16 = 24;

/* _IOC(_IOC_READ, 'E', 0x06, len) spelled out (generic/arm layout).
 * musl declares ioctl's request as a plain int, hence the wrapping cast. */
const fn eviocgname(len: u32) -> libc::c_int {
    (2u32 << 30 | len << 16 | (b'E' as u32) << 8 | 0x06) as libc::c_int
}

/* rM2 Wacom geometry (from rM2-stuff's rMlib): ABS_X runs 0..20967 along
 * the LONG edge, ABS_Y 0..15725 along the short edge, rotated vs the
 * screen:  screen_x = wy * W / 15725   screen_y = H - wx * H / 20967 */
const WACOM_X_MAX: i32 = 20967;
const WACOM_Y_MAX: i32 = 15725;

#[derive(Clone, Copy, PartialEq)]
pub enum PenPhase {
    Press,
    Move,
    Release,
}

pub struct Pen {
    fd: RawFd,
    wx: i32,
    wy: i32,
    touching: bool,
    was_touching: bool,
    /// raw pressure, 0..4095
    pub pressure: i32,
    /// true while the Marker's eraser end faces the glass
    pub rubber: bool,
    /// latest mapped screen coordinates
    pub sx: i32,
    pub sy: i32,
}

impl Pen {
    /// Scan /dev/input for the Wacom digitizer. None off-device (e.g. in
    /// the preview harness) — the caller then inks via AppLoad events.
    pub fn open() -> Option<Pen> {
        for i in 0..8 {
            let path = std::ffi::CString::new(format!("/dev/input/event{i}")).unwrap();
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                continue;
            }
            let mut name = [0u8; 64];
            let n = unsafe { libc::ioctl(fd, eviocgname(64), name.as_mut_ptr()) };
            let name = String::from_utf8_lossy(&name[..n.max(0) as usize]);
            if n > 0 && name.contains("Wacom") {
                println!("sample-app-rs: direct pen input from /dev/input/event{i} ({})",
                         name.trim_end_matches('\0'));
                return Some(Pen {
                    fd,
                    wx: 0,
                    wy: 0,
                    touching: false,
                    was_touching: false,
                    pressure: 0,
                    rubber: false,
                    sx: 0,
                    sy: 0,
                });
            }
            unsafe { libc::close(fd) };
        }
        println!("sample-app-rs: no Wacom device, inking via AppLoad events");
        None
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Read everything queued. The digitizer streams ABS_* values followed
    /// by an EV_SYN "frame complete" marker; `on_frame` fires once per
    /// frame while touching. Returns true if the pen showed any sign of
    /// life (including hover) — main.rs feeds that to palm rejection.
    pub fn drain(&mut self, mut on_frame: impl FnMut(&Pen, PenPhase)) -> bool {
        let mut seen = false;
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
                return seen;
            }
            seen = true;
            for ev in &evs[..n as usize / std::mem::size_of::<RawInputEvent>()] {
                match (ev.kind, ev.code) {
                    (EV_ABS, ABS_X) => self.wx = ev.value,
                    (EV_ABS, ABS_Y) => self.wy = ev.value,
                    (EV_ABS, ABS_PRESSURE) => self.pressure = ev.value,
                    (EV_KEY, BTN_TOOL_RUBBER) => self.rubber = ev.value != 0,
                    (EV_KEY, BTN_TOUCH) => self.touching = ev.value != 0,
                    (EV_SYN, _) => {
                        self.sx = self.wy * crate::qtfb::RM2_WIDTH / WACOM_Y_MAX;
                        self.sy =
                            crate::qtfb::RM2_HEIGHT - self.wx * crate::qtfb::RM2_HEIGHT / WACOM_X_MAX;
                        let phase = match (self.was_touching, self.touching) {
                            (false, true) => Some(PenPhase::Press),
                            (true, true) => Some(PenPhase::Move),
                            (true, false) => Some(PenPhase::Release),
                            (false, false) => None, /* hovering */
                        };
                        self.was_touching = self.touching;
                        if let Some(p) = phase {
                            on_frame(self, p);
                        }
                    }
                    (EV_KEY, BTN_TOOL_PEN) => {} /* proximity: `seen` covers it */
                    _ => {}
                }
            }
        }
    }
}

impl Drop for Pen {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
