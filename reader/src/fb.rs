//! The shared framebuffer: 1404x1872 RGB565, whichever backend it maps to.
//!
//! Both display backends hand us a shared-memory region in the same pixel
//! format — qtfb's /qtfb_<key> shm (windowed) and the rm2fb server's
//! /swtfb.01 shm (takeover) — so one Framebuffer type plus the draw.rs
//! primitives serve both. Pushing pixels to the panel is the backend's job
//! (display.rs); this type only owns the mapping and the clip band.

pub const SCREEN_W: i32 = 1404;
pub const SCREEN_H: i32 = 1872;

/// The mmap'ed shared-memory framebuffer. Drawing = writing to `pixels()`;
/// primitives live in draw.rs.
pub struct Framebuffer {
    ptr: *mut u16,
    len: usize, /* in pixels */
    /* vertical clip band enforced by the draw.rs primitives */
    pub(crate) clip_y0: i32,
    pub(crate) clip_y1: i32,
}

impl Framebuffer {
    /// Wrap a mapped region. `len` is in PIXELS; the mapping must cover at
    /// least a full screen. Takes ownership: Drop munmaps.
    pub fn from_raw(ptr: *mut u16, len: usize) -> Self {
        Self { ptr, len, clip_y0: 0, clip_y1: SCREEN_H }
    }

    pub fn pixels(&mut self) -> &mut [u16] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// Snapshot a screen-band's raw pixels (save-under for the sleep page).
    pub fn copy_band(&mut self, y0: i32, y1: i32) -> Vec<u16> {
        let (a, b) = (y0.clamp(0, SCREEN_H) as usize, y1.clamp(0, SCREEN_H) as usize);
        self.pixels()[a * SCREEN_W as usize..b * SCREEN_W as usize].to_vec()
    }

    /// Put back pixels captured by `copy_band` at the same geometry.
    pub fn paste_band(&mut self, y0: i32, saved: &[u16]) {
        let a = y0.clamp(0, SCREEN_H) as usize * SCREEN_W as usize;
        self.pixels()[a..a + saved.len()].copy_from_slice(saved);
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len * 2) };
    }
}
