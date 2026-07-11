//! rm2fb client: the takeover display backend (ported from the riddle app).
//!
//! The rM2's e-ink engine moved into libqsgepaper.so on OS 3.20+, and
//! timower/rM2-stuff's rm2fb server hosts it standalone: dlopen the vendor
//! plugin, redirect its framebuffer allocation into shared memory, drive the
//! panel directly. We are a plain client of that server:
//!
//!   - framebuffer: shm /swtfb.01 — 1404x1872 RGB565 (a grayscale back
//!     buffer follows it; we don't touch that)
//!   - updates: SOCK_STREAM unix socket /var/run/rm2fb.sock; each update is
//!     a raw 32-byte UpdateParams (NOTE: y1 before x1, inclusive coords),
//!     acked with one bool byte — synchronous and cheap
//!
//! The server must be running and xochitl stopped; scripts/takeover.sh owns
//! that dance and always restores xochitl on exit. This is what buys the
//! low-latency ink: no Qt loop between our pen samples and the panel, and
//! per-update waveform choice (DU for strokes) instead of a global mode.

use crate::fb::{Framebuffer, SCREEN_H, SCREEN_W};
use std::io;
use std::os::fd::RawFd;

const SHM_PATH: &str = "/dev/shm/swtfb.01\0";
const SOCK_PATH: &str = "/var/run/rm2fb.sock";

/// Waveforms in the ioctl convention; the server maps them onto the vendor
/// engine's internal table when this flag is set.
const IOCTL_WAVEFORM_FLAG: i32 = 0xf000;
const WAVEFORM_DU: i32 = 1; // 1-bit, fastest — live ink
const WAVEFORM_GC16: i32 = 2; // 16-level, flashing — full refresh
const WAVEFORM_GL16: i32 = 3; // 16-level, no flash — text

/// flags: bit 2 = priority (the server maps it to xochitl's pen mode),
/// bit 0 = full refresh.
const FLAG_PRIORITY: i32 = 4;
const FLAG_FULL: i32 = 1;

pub struct Rm2fbClient {
    sock: RawFd,
}

impl Rm2fbClient {
    /// Connect to a running rm2fb server and map the shared framebuffer.
    pub fn connect() -> io::Result<(Self, Framebuffer)> {
        let sock = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if sock < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (i, b) in SOCK_PATH.bytes().enumerate() {
            addr.sun_path[i] = b as libc::c_char;
        }
        let rc = unsafe {
            libc::connect(
                sock,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(sock) };
            return Err(io::Error::new(
                e.kind(),
                format!("rm2fb server socket {SOCK_PATH}: {e} (is rm2fb_server running?)"),
            ));
        }

        let shm_len = (SCREEN_W * SCREEN_H) as usize * 2;
        let shm_fd = unsafe { libc::open(SHM_PATH.as_ptr() as *const libc::c_char, libc::O_RDWR) };
        if shm_fd < 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(sock) };
            return Err(io::Error::new(e.kind(), format!("shm {SHM_PATH}: {e}")));
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                shm_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                shm_fd,
                0,
            )
        };
        unsafe { libc::close(shm_fd) };
        if ptr == libc::MAP_FAILED {
            let e = io::Error::last_os_error();
            unsafe { libc::close(sock) };
            return Err(e);
        }

        let client = Self { sock };
        // Init check: a degenerate rect makes the server ack without updating.
        client.send_params(0, 0, 0, 0, 0, 0)?;
        println!("notebook: rm2fb server answered init check");
        Ok((client, Framebuffer::from_raw(ptr as *mut u16, shm_len / 2)))
    }

    /// Push a region to the panel. Blocks until the server acks — cheap and
    /// synchronous. `fast` selects the 1-bit DU waveform (live ink); slow is
    /// the flash-free 16-level GL16 (antialiased text).
    pub fn update(&self, x: i32, y: i32, w: i32, h: i32, fast: bool) -> io::Result<()> {
        let (waveform, flags) = if fast {
            (WAVEFORM_DU, FLAG_PRIORITY)
        } else {
            (WAVEFORM_GL16, 0)
        };
        self.send_clamped(x, y, w, h, waveform, flags)
    }

    /// Full 16-level GL16 — smooth, flash-free render for typeset reading
    /// views. A *partial* update approximates greys with a fast, speckled
    /// waveform (antialiased text turns to salt-and-pepper); a *full* update
    /// drives every pixel with the real 16-level LUT, so fine greys render
    /// smooth, and GL16 has no black clearing phase so nothing flashes.
    /// Ghost buildup is still reset by the periodic GC16 full_refresh.
    /// (Ported from alt-ui/Paper.)
    pub fn gl16_full(&self, x: i32, y: i32, w: i32, h: i32) -> io::Result<()> {
        self.send_clamped(x, y, w, h, WAVEFORM_GL16, FLAG_FULL)
    }

    /// Flashing clear of the whole panel (ghost removal).
    pub fn full_refresh(&self) -> io::Result<()> {
        self.send_clamped(0, 0, SCREEN_W, SCREEN_H, WAVEFORM_GC16, FLAG_FULL)
    }

    fn send_clamped(&self, x: i32, y: i32, w: i32, h: i32, waveform: i32, flags: i32) -> io::Result<()> {
        let mut x1 = x.clamp(0, SCREEN_W - 1);
        let y1 = y.clamp(0, SCREEN_H - 1);
        let mut x2 = (x + w - 1).clamp(x1, SCREEN_W - 1);
        let y2 = (y + h - 1).clamp(y1, SCREEN_H - 1);
        // A 1x1 rect would read as the server's init-check sentinel; widen it.
        if x1 == x2 && y1 == y2 {
            if x2 < SCREEN_W - 1 {
                x2 += 1;
            } else {
                x1 -= 1;
            }
        }
        self.send_params(x1, y1, x2, y2, waveform, flags)
    }

    /// Raw UpdateParams write + 1-byte ack read.
    fn send_params(&self, x1: i32, y1: i32, x2: i32, y2: i32, waveform: i32, flags: i32) -> io::Result<()> {
        let mut msg = [0u8; 32];
        // struct UpdateParams { int y1, x1, y2, x2, flags, waveform;
        //                       float temperatureOverride; int extraMode; }
        msg[0..4].copy_from_slice(&y1.to_le_bytes());
        msg[4..8].copy_from_slice(&x1.to_le_bytes());
        msg[8..12].copy_from_slice(&y2.to_le_bytes());
        msg[12..16].copy_from_slice(&x2.to_le_bytes());
        msg[16..20].copy_from_slice(&flags.to_le_bytes());
        msg[20..24].copy_from_slice(&(waveform | IOCTL_WAVEFORM_FLAG).to_le_bytes());
        msg[24..28].copy_from_slice(&0f32.to_le_bytes());
        msg[28..32].copy_from_slice(&0i32.to_le_bytes());
        write_all(self.sock, &msg)?;

        let mut ack = [0u8; 1];
        let n = unsafe { libc::read(self.sock, ack.as_mut_ptr() as *mut libc::c_void, 1) };
        if n != 1 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "rm2fb server closed (no update ack)",
            ));
        }
        Ok(())
    }
}

impl Drop for Rm2fbClient {
    fn drop(&mut self) {
        unsafe { libc::close(self.sock) };
    }
}

fn write_all(fd: RawFd, buf: &[u8]) -> io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe {
            libc::write(fd, buf[off..].as_ptr() as *const libc::c_void, buf.len() - off)
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        off += n as usize;
    }
    Ok(())
}
