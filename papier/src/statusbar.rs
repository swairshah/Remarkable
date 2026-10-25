//! The top status bar: clock on the left, wifi + battery on the right.
//! Persistent on the home screen; inside documents it only appears on the
//! top-edge-swipe reveal bar (main.rs) so it never fights the pen.
//!
//! All readers are plain file reads — no fork/exec on the poll loop.
//! Battery comes from /sys/class/power_supply (the node name differs by
//! board; globbed once at startup). Wifi is "connected" when wlan0's
//! operstate is up; bars come from /proc/net/wireless when the driver
//! fills it in (this firmware's brcmfmac often leaves it empty, so a
//! live link without a quality reading shows full bars). wpa_cli is
//! deliberately NOT used here — it can hang exactly when wpa_supplicant
//! is wedged. PAPIER_FAKE_SYS=1 pins deterministic values for the preview
//! harness.
//!
//! The clock is local time via localtime_r, so it needs TZ: the device
//! ships as UTC, and takeover.sh exports TZ from the data dir's
//! `timezone` file (written by `make deploy` from the Mac's zone).

use crate::draw::{BLACK, LIGHT, WHITE};
use crate::fb::{Framebuffer, SCREEN_W};
use crate::text;

pub const STATUS_H: i32 = 56;

pub struct SysStatus {
    cap_path: Option<String>,
    status_path: Option<String>,
    fake: bool,

    pub batt_pct: i32,
    pub charging: bool,
    pub wifi_bars: i8, /* -1 = off/absent, else 0..=3 */
    pub hm: (i32, i32),
}

impl SysStatus {
    pub fn new() -> Self {
        let fake = std::env::var("PAPIER_FAKE_SYS").is_ok();
        let (cap_path, status_path) = if fake { (None, None) } else { find_battery() };
        let mut s = SysStatus {
            cap_path,
            status_path,
            fake,
            batt_pct: -1,
            charging: false,
            wifi_bars: -1,
            hm: (0, 0),
        };
        s.refresh();
        s
    }

    /// Re-read everything; true if anything user-visible changed.
    pub fn refresh(&mut self) -> bool {
        let (pct, charging, wifi, hm) = if self.fake {
            (87, false, 3, (14, 32))
        } else {
            (
                self.cap_path
                    .as_deref()
                    .and_then(|p| read_trim(p)?.parse::<i32>().ok())
                    .unwrap_or(-1),
                self.status_path
                    .as_deref()
                    .and_then(read_trim)
                    .is_some_and(|s| s == "Charging"),
                read_wifi_bars(),
                clock_hm(),
            )
        };
        let changed = (pct, charging, wifi, hm)
            != (self.batt_pct, self.charging, self.wifi_bars, self.hm);
        self.batt_pct = pct;
        self.charging = charging;
        self.wifi_bars = wifi;
        self.hm = hm;
        changed
    }
}

fn read_trim(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// Find the battery node once: any power_supply entry with a capacity file.
fn find_battery() -> (Option<String>, Option<String>) {
    let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else {
        return (None, None);
    };
    for e in rd.flatten() {
        let base = e.path().to_string_lossy().to_string();
        let cap = format!("{base}/capacity");
        if std::path::Path::new(&cap).exists() {
            return (Some(cap), Some(format!("{base}/status")));
        }
    }
    (None, None)
}

/// Wifi state -> 0..=3 bars, -1 = not connected (no link on wlan0).
fn read_wifi_bars() -> i8 {
    let up = read_trim("/sys/class/net/wlan0/operstate").is_some_and(|s| s == "up");
    if !up {
        return -1;
    }
    let s = std::fs::read_to_string("/proc/net/wireless").unwrap_or_default();
    for line in s.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("wlan0:") {
            let q: f32 = rest
                .split_whitespace()
                .nth(1)
                .map(|t| t.trim_end_matches('.'))
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0);
            /* iwlib scale: 0..70 */
            return if q >= 45.0 {
                3
            } else if q >= 30.0 {
                2
            } else if q >= 15.0 {
                1
            } else {
                0
            };
        }
    }
    3 /* link up, no quality reading */
}

fn clock_hm() -> (i32, i32) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_hour, tm.tm_min)
    }
}

/* ---- rendering ------------------------------------------------------------ */

/// Paint the bar into `fb` at y=0..STATUS_H (caller pushes the region).
pub fn render(fb: &mut Framebuffer, st: &SysStatus) {
    fb.fill_rect(0, 0, SCREEN_W, STATUS_H, WHITE);
    fb.fill_rect(0, STATUS_H - 2, SCREEN_W, 2, BLACK);

    /* clock, left */
    let clock = format!("{:02}:{:02}", st.hm.0, st.hm.1);
    text::draw_line(fb, 24, 12, text::Face::Body, 30.0, &clock);

    /* battery, rightmost: outline + fill + nub + percent */
    let (bw, bh) = (46, 22);
    let bx = SCREEN_W - 24 - bw;
    let by = (STATUS_H - 2 - bh) / 2;
    fb.rect_outline(bx, by, bw, bh, 2, BLACK);
    fb.fill_rect(bx + bw, by + bh / 2 - 4, 4, 8, BLACK); /* the nub */
    if st.batt_pct >= 0 {
        let inner = ((bw - 8) * st.batt_pct.min(100)) / 100;
        fb.fill_rect(bx + 4, by + 4, inner, bh - 8, BLACK);
    }
    let pct = if st.batt_pct >= 0 { format!("{}%", st.batt_pct) } else { "--".into() };
    let pw = text::width(text::Face::Body, 26.0, &pct);
    text::draw_line(fb, bx - 12 - pw, 14, text::Face::Body, 26.0, &pct);
    if st.charging {
        /* a small bolt: two stacked triangles-ish, kept simple */
        fb.fill_rect(bx + bw / 2 - 2, by - 6, 4, 4, BLACK);
    }

    /* wifi, left of the battery text */
    let wx = bx - 12 - pw - 28;
    wifi_glyph(fb, wx, STATUS_H / 2 + 12, st.wifi_bars);
}

/// The wifi fan: a dot plus three arcs opening upward from (cx, base_y).
/// `bars` arcs are black (0..=3); -1 = disconnected: the fan goes light
/// gray and a black slash strikes it out. Spans ~44x34 px.
pub fn wifi_glyph(fb: &mut Framebuffer, cx: i32, base_y: i32, bars: i8) {
    let off = bars < 0;
    fb.disc(cx, base_y - 3, 4, if off { LIGHT } else { BLACK });
    for i in 1..=3i32 {
        let r = 9 * i + 1;
        let c = if !off && bars >= i as i8 { BLACK } else { LIGHT };
        let (a0, a1) = (-2.36f32, -0.78f32); /* -135deg .. -45deg */
        let mut last: Option<(i32, i32)> = None;
        for k in 0..=16 {
            let a = a0 + (a1 - a0) * k as f32 / 16.0;
            let p = (cx + (r as f32 * a.cos()).round() as i32, base_y - 3 + (r as f32 * a.sin()).round() as i32);
            if let Some(l) = last {
                fb.stroke_segment(l.0, l.1, p.0, p.1, 1, c);
            }
            last = Some(p);
        }
    }
    if off {
        fb.stroke_segment(cx - 18, base_y - 32, cx + 18, base_y + 2, 1, BLACK);
    }
}
