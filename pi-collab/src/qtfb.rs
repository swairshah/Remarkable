//! The AppLoad/qtfb wire protocol, in Rust.
//!
//! Modeled on the official clients in rm-appload
//! (<https://github.com/asivery/rm-appload>, `backends/qtfb-clients/`,
//! GPL-3.0). The upstream Rust crate only covers pixel output; this module
//! adds the input side (touch/pen/keyboard events) and the refresh-mode
//! messages, matching the C++ client's `common.h` byte-for-byte.
//!
//! How it works, in one paragraph: AppLoad (inside xochitl) listens on a
//! unix SEQPACKET socket at /tmp/qtfb.sock. It launches your app with the
//! QTFB_KEY env var set. You connect, send an INITIALIZE message with that
//! key and a pixel format, and get back the name of a POSIX shared-memory
//! object. mmap() it: that buffer IS your screen (1404x1872 RGB565 for the
//! rM2 format). Write pixels, send UPDATE to blit a region to the e-ink.
//! Input events arrive on the same socket. That's the entire API.
//!
//! `connect()` hands you two values — a `Framebuffer` (the mapped pixels)
//! and a `Socket` (messages in/out) — so you can mutate pixels while still
//! calling socket methods without fighting the borrow checker.

use std::io;
use std::mem::{size_of, zeroed};
use std::os::unix::io::RawFd;

pub const RM2_WIDTH: i32 = 1404;
pub const RM2_HEIGHT: i32 = 1872;

const SOCKET_PATH: &str = "/tmp/qtfb.sock";

/* message.type values — client -> server */
const MESSAGE_INITIALIZE: u8 = 0;
const MESSAGE_UPDATE: u8 = 1;
const MESSAGE_TERMINATE: u8 = 3;
const MESSAGE_SET_REFRESH_MODE: u8 = 5;
const MESSAGE_REQUEST_FULL_REFRESH: u8 = 6;
/* server -> client */
const MESSAGE_USERINPUT: u8 = 4;

/// Pixel format: 1404x1872, 16bpp RGB565 — what a reMarkable 2 wants.
/// (The RMPP formats from upstream are omitted; add them if you target it.)
const FBFMT_RM2FB: u8 = 0;

const UPDATE_ALL: i32 = 0;
const UPDATE_PARTIAL: i32 = 1;

/// E-ink waveform hints for `Socket::set_refresh_mode`. This is THE latency
/// lever: UltraFast/Fast are the low-latency near-binary waveforms (more
/// ghosting) — right for inking. Ui (the default) is a quality waveform
/// that takes hundreds of ms per refresh. Content: prettiest, slowest.
#[derive(Clone, Copy)]
#[allow(dead_code)] /* pi-collab only uses UltraFast; the rest are API */
pub enum RefreshMode {
    UltraFast = 0,
    Fast = 1,
    Animate = 2,
    Content = 3,
    Ui = 4,
}

/* UserInput.input_type values */
const INPUT_TOUCH_PRESS: i32 = 0x10;
const INPUT_TOUCH_RELEASE: i32 = 0x11;
const INPUT_TOUCH_UPDATE: i32 = 0x12;
const INPUT_PEN_PRESS: i32 = 0x20;
const INPUT_PEN_RELEASE: i32 = 0x21;
const INPUT_PEN_UPDATE: i32 = 0x22;
const INPUT_VKB_PRESS: i32 = 0x40; /* AppLoad's on-screen keyboard */
const INPUT_VKB_RELEASE: i32 = 0x41;

/* ---- wire structs -------------------------------------------------------
 * These must match the C ABI of the server exactly; hence #[repr(C)] and
 * the unions. Sizes are asserted below (32-bit targets only: size_t/usize
 * is 4 bytes on the device, 8 on your dev machine). */

#[repr(C)]
#[derive(Clone, Copy)]
struct InitContents {
    framebuffer_key: i32,
    framebuffer_type: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InitResponseContents {
    shm_key_defined: i32,
    shm_size: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UpdateContents {
    update_type: i32, /* UPDATE_ALL or UPDATE_PARTIAL */
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserInputContents {
    input_type: i32, /* INPUT_* */
    dev_id: i32,     /* finger slot for multitouch */
    x: i32,
    y: i32,          /* framebuffer coordinates */
    d: i32,          /* pen: pressure, keyboard: keycode */
}

#[repr(C)]
union ClientContents {
    init: InitContents,
    update: UpdateContents,
    refresh_mode: i32,
    none: (),
}

#[repr(C)]
struct ClientMessage {
    msg_type: u8,
    contents: ClientContents,
}

#[repr(C)]
union ServerContents {
    init: InitResponseContents,
    user_input: UserInputContents,
}

#[repr(C)]
struct ServerMessage {
    msg_type: u8,
    contents: ServerContents,
}

#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(size_of::<ClientMessage>() == 24);
    assert!(size_of::<ServerMessage>() == 24);
};

/* ---- events -------------------------------------------------------------
 * The raw input packets, decoded into something you can match on. */

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Press,
    Move,
    Release,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] /* pressure etc. are for your app, unused by the sample */
pub enum Event {
    /// A finger. `id` is the multitouch slot — several can be down at once.
    Touch { id: i32, phase: Phase, x: i32, y: i32 },
    /// The pen. `pressure` is only meaningful during Press/Move.
    Pen { phase: Phase, x: i32, y: i32, pressure: i32 },
    /// AppLoad's on-screen keyboard, if you enable it. `code` is ASCII-ish.
    Key { code: i32, pressed: bool },
    /// A signal (e.g. SIGTERM) interrupted the read — check your run flag.
    Interrupted,
    /// AppLoad closed our window; time to exit.
    Closed,
    /// Something this sample doesn't decode (rM1 hardware buttons, etc.).
    Other,
}

/* ---- the two halves of a connection ------------------------------------- */

/// The mmap'ed shared-memory framebuffer. Drawing = writing to `pixels()`.
/// Drawing primitives live in draw.rs.
pub struct Framebuffer {
    ptr: *mut u16,
    len: usize, /* in pixels */
    /* vertical clip band enforced by the draw.rs primitives */
    pub(crate) clip_y0: i32,
    pub(crate) clip_y1: i32,
}

impl Framebuffer {
    pub fn pixels(&mut self) -> &mut [u16] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len * 2) };
    }
}

/// The message channel to AppLoad: refresh requests out, input events in.
pub struct Socket {
    fd: RawFd,
}

impl Socket {
    /// For poll()ing alongside other fds (see the main event loop).
    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    fn send_msg(&self, msg: &ClientMessage) -> io::Result<()> {
        let n = unsafe {
            libc::send(
                self.fd,
                msg as *const _ as *const libc::c_void,
                size_of::<ClientMessage>(),
                0,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Blit the whole framebuffer. Slow and flashy on e-ink; use sparingly.
    pub fn update_all(&self) -> io::Result<()> {
        self.send_msg(&ClientMessage {
            msg_type: MESSAGE_UPDATE,
            contents: ClientContents {
                update: UpdateContents { update_type: UPDATE_ALL, x: 0, y: 0, w: 0, h: 0 },
            },
        })
    }

    /// Blit one region — the workhorse; small regions refresh fast.
    pub fn update_region(&self, x: i32, y: i32, w: i32, h: i32) -> io::Result<()> {
        self.send_msg(&ClientMessage {
            msg_type: MESSAGE_UPDATE,
            contents: ClientContents {
                update: UpdateContents { update_type: UPDATE_PARTIAL, x, y, w, h },
            },
        })
    }

    /// The full deghosting flash (the black/white blink e-readers do).
    pub fn request_full_refresh(&self) -> io::Result<()> {
        self.send_msg(&ClientMessage {
            msg_type: MESSAGE_REQUEST_FULL_REFRESH,
            contents: ClientContents { none: () },
        })
    }

    pub fn set_refresh_mode(&self, mode: RefreshMode) -> io::Result<()> {
        self.send_msg(&ClientMessage {
            msg_type: MESSAGE_SET_REFRESH_MODE,
            contents: ClientContents { refresh_mode: mode as i32 },
        })
    }

    /// Block until the next event. SEQPACKET preserves message boundaries,
    /// so one recv() is one message — no framing to worry about.
    /// (The sample's main loop poll()s and uses try_next_event instead,
    /// but this is the right call for a simpler single-source app.)
    #[allow(dead_code)]
    pub fn next_event(&self) -> Event {
        /* flags = 0 blocks, so recv_event only returns None on EAGAIN,
         * which can't happen here */
        self.recv_event(0).unwrap_or(Event::Closed)
    }

    /// Like `next_event`, but returns None immediately if nothing is queued.
    /// Used to drain the input backlog before spending an e-ink refresh.
    pub fn try_next_event(&self) -> Option<Event> {
        self.recv_event(libc::MSG_DONTWAIT)
    }

    fn recv_event(&self, flags: libc::c_int) -> Option<Event> {
        let mut msg: ServerMessage = unsafe { zeroed() };
        let n = unsafe {
            libc::recv(
                self.fd,
                &mut msg as *mut _ as *mut libc::c_void,
                size_of::<ServerMessage>(),
                flags,
            )
        };
        if n < 0 {
            return match io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => Some(Event::Interrupted),
                Some(libc::EAGAIN) => None, /* nothing queued (MSG_DONTWAIT) */
                _ => Some(Event::Closed),
            };
        }
        if n == 0 {
            return Some(Event::Closed);
        }
        Some(Self::decode(&msg))
    }

    fn decode(msg: &ServerMessage) -> Event {
        if msg.msg_type != MESSAGE_USERINPUT {
            return Event::Other;
        }
        let i = unsafe { msg.contents.user_input };
        match i.input_type {
            INPUT_TOUCH_PRESS => Event::Touch { id: i.dev_id, phase: Phase::Press, x: i.x, y: i.y },
            INPUT_TOUCH_UPDATE => Event::Touch { id: i.dev_id, phase: Phase::Move, x: i.x, y: i.y },
            INPUT_TOUCH_RELEASE => {
                Event::Touch { id: i.dev_id, phase: Phase::Release, x: i.x, y: i.y }
            }
            INPUT_PEN_PRESS => Event::Pen { phase: Phase::Press, x: i.x, y: i.y, pressure: i.d },
            INPUT_PEN_UPDATE => Event::Pen { phase: Phase::Move, x: i.x, y: i.y, pressure: i.d },
            INPUT_PEN_RELEASE => Event::Pen { phase: Phase::Release, x: i.x, y: i.y, pressure: 0 },
            INPUT_VKB_PRESS => Event::Key { code: i.d, pressed: true },
            INPUT_VKB_RELEASE => Event::Key { code: i.d, pressed: false },
            _ => Event::Other,
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        /* polite goodbye so AppLoad tears the window down immediately */
        let _ = self.send_msg(&ClientMessage {
            msg_type: MESSAGE_TERMINATE,
            contents: ClientContents { none: () },
        });
        unsafe { libc::close(self.fd) };
    }
}

/* ---- setup ---------------------------------------------------------------- */

fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg)
}

/// Connect to AppLoad and claim our framebuffer. Reads QTFB_KEY from the
/// environment (AppLoad sets it when it spawns us; a shell doesn't).
pub fn connect() -> io::Result<(Framebuffer, Socket)> {
    let key: i32 = std::env::var("QTFB_KEY")
        .map_err(|_| err("QTFB_KEY not set — this app must be launched by AppLoad"))?
        .parse()
        .map_err(|_| err("QTFB_KEY is not a number"))?;

    /* 1. connect to /tmp/qtfb.sock (SEQPACKET: datagram-like, but reliable
       and connection-oriented — std has no wrapper for it, hence libc) */
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let sock = Socket { fd }; /* from here on, Drop cleans up on any error */

    let mut addr: libc::sockaddr_un = unsafe { zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, b) in SOCKET_PATH.bytes().enumerate() {
        addr.sun_path[i] = b as libc::c_char;
    }
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    /* 2. handshake: claim our key, ask for the rM2 pixel format */
    sock.send_msg(&ClientMessage {
        msg_type: MESSAGE_INITIALIZE,
        contents: ClientContents {
            init: InitContents { framebuffer_key: key, framebuffer_type: FBFMT_RM2FB },
        },
    })?;

    let mut resp: ServerMessage = unsafe { zeroed() };
    let n = unsafe {
        libc::recv(fd, &mut resp as *mut _ as *mut libc::c_void, size_of::<ServerMessage>(), 0)
    };
    if n < 1 {
        return Err(err("no init response from qtfb server"));
    }
    let init = unsafe { resp.contents.init };

    /* 3. map the shared memory the server just described */
    let expected = (RM2_WIDTH * RM2_HEIGHT * 2) as usize;
    if init.shm_size < expected {
        return Err(err("shm smaller than a full rM2 framebuffer"));
    }
    let name = std::ffi::CString::new(format!("/qtfb_{}", init.shm_key_defined)).unwrap();
    let shm_fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR, 0) };
    if shm_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            init.shm_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            shm_fd,
            0,
        )
    };
    unsafe { libc::close(shm_fd) }; /* the mapping keeps the shm alive */
    if ptr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }

    let fb = Framebuffer {
        ptr: ptr as *mut u16,
        len: init.shm_size / 2,
        clip_y0: 0,
        clip_y1: RM2_HEIGHT,
    };
    Ok((fb, sock))
}
