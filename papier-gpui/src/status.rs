//! The status bar's facts: clock, wifi bars, battery (papier's statusbar.rs,
//! reading the laptop's sysfs instead of the tablet's).

use std::time::Instant;

#[derive(Clone, Debug)]
pub struct SysStatus {
    pub hm: (i32, i32),
    /// 0..=3, -1 = no link.
    pub wifi: i8,
    /// 0..=100, -1 = no battery.
    pub battery: i32,
    pub charging: bool,
    at: Instant,
}

impl SysStatus {
    pub fn read() -> SysStatus {
        let (battery, charging) = battery();
        SysStatus { hm: clock_hm(), wifi: wifi_bars(), battery, charging, at: Instant::now() }
    }

    /// Refresh at most every few seconds; true if anything visible changed.
    pub fn refresh(&mut self) -> bool {
        if self.at.elapsed().as_secs() < 5 {
            return false;
        }
        let next = SysStatus::read();
        let changed = next.hm != self.hm || next.wifi != self.wifi || next.battery != self.battery || next.charging != self.charging;
        *self = next;
        changed
    }

    pub fn clock(&self) -> String {
        format!("{:02}:{:02}", self.hm.0, self.hm.1)
    }
}

fn read_trim(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn battery() -> (i32, bool) {
    let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else { return (-1, false) };
    for e in rd.flatten() {
        let base = e.path().to_string_lossy().to_string();
        if read_trim(&format!("{base}/type")).as_deref() != Some("Battery") {
            continue;
        }
        if let Some(cap) = read_trim(&format!("{base}/capacity")).and_then(|s| s.parse().ok()) {
            let status = read_trim(&format!("{base}/status")).unwrap_or_default();
            return (cap, status == "Charging" || status == "Full");
        }
    }
    (-1, false)
}

fn wifi_bars() -> i8 {
    // Signal quality from the wireless extensions when the driver reports
    // it; otherwise a connected wireless link shows full bars.
    let s = std::fs::read_to_string("/proc/net/wireless").unwrap_or_default();
    for line in s.lines().skip(2) {
        let mut parts = line.split_whitespace();
        let Some(iface) = parts.next() else { continue };
        let q: f32 = parts.nth(1).map(|t| t.trim_end_matches('.')).and_then(|t| t.parse().ok()).unwrap_or(0.0);
        if read_trim(&format!("/sys/class/net/{}/operstate", iface.trim_end_matches(':'))).as_deref() == Some("up") {
            return if q >= 45.0 { 3 } else if q >= 30.0 { 2 } else if q >= 15.0 { 1 } else { 0 };
        }
    }
    let Ok(rd) = std::fs::read_dir("/sys/class/net") else { return -1 };
    for e in rd.flatten() {
        let wireless = e.path().join("wireless").exists() || e.path().join("phy80211").exists();
        if wireless && read_trim(&format!("{}/operstate", e.path().display())).as_deref() == Some("up") {
            return 3;
        }
    }
    -1
}

fn clock_hm() -> (i32, i32) {
    // SAFETY: localtime_r writes into the zeroed tm we own.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_hour, tm.tm_min)
    }
}

/// "Today", "Yesterday", "N days ago" from a unix-ms stamp.
pub fn rel_date(ms: Option<f64>) -> String {
    let Some(ms) = ms.filter(|m| *m > 0.0) else { return "new".into() };
    let now = crate::store::now_ms() as f64;
    let days = ((now - ms) / 86_400_000.0).max(0.0) as u64;
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        n => format!("{n} days ago"),
    }
}

pub fn ago(ms: u64) -> String {
    let s = crate::store::now_ms().saturating_sub(ms) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}
