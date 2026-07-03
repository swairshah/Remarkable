//! A local, UI-side transcript so reopening the app shows past messages when
//! you scroll up — independent of pi's own session logs.
//!
//! pi's `--continue` restores pi's *memory*, but its session files store the
//! handwriting as PNGs (which we'd have to decode) and don't map cleanly to
//! our on-screen entries. So we keep our own append-only JSONL: one line per
//! sent message (the ink) and per completed reply (pi's text). The ink is a
//! strict black/white bitmap, so it's stored bit-packed (1 bit/pixel) and
//! base64'd — a few KB per message.
//!
//! Lives OUTSIDE the pi session dir so pi's `--continue` never mistakes it
//! for a session. $PI_COLLAB_HISTORY overrides the path (used by preview).

use crate::conv::{Entry, GrayImg};
use crate::png;
use serde_json::{json, Value};
use std::io::Write;

/// How many trailing entries to load — bounds memory and scroll length on a
/// long-running history.
const MAX_LOAD: usize = 80;

fn path() -> String {
    if let Ok(p) = std::env::var("PI_COLLAB_HISTORY") {
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/pi-collab/ui-history.jsonl")
}

/* the snapshot ink is pure black/white (see snapshot_ink), so 1 bit/pixel */
fn pack_bits(g: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; g.len().div_ceil(8)];
    for (i, &v) in g.iter().enumerate() {
        if v < 128 {
            out[i / 8] |= 1 << (i % 8); /* 1 = inked */
        }
    }
    out
}

fn unpack_bits(bits: &[u8], n: usize) -> Vec<u8> {
    let mut out = vec![255u8; n];
    for (i, o) in out.iter_mut().enumerate() {
        if bits.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0) {
            *o = 0;
        }
    }
    out
}

fn append(v: &Value) {
    let p = path();
    if let Some(dir) = std::path::Path::new(&p).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{v}");
    }
}

pub fn append_you(img: &GrayImg) {
    append(&json!({
        "k": "you", "w": img.w, "h": img.h,
        "b": png::base64(&pack_bits(&img.px)),
    }));
}

pub fn append_pi(text: &str) {
    append(&json!({ "k": "pi", "t": text }));
}

/// Load the transcript as entries (most recent MAX_LOAD). Empty if none.
pub fn load() -> Vec<Entry> {
    let data = match std::fs::read_to_string(path()) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut all: Vec<Entry> = Vec::new();
    for line in data.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v["k"].as_str() {
            Some("you") => {
                let w = v["w"].as_i64().unwrap_or(0) as i32;
                let h = v["h"].as_i64().unwrap_or(0) as i32;
                if let (true, Some(b64)) = (w > 0 && h > 0, v["b"].as_str()) {
                    let px = unpack_bits(&png::base64_decode(b64), (w * h) as usize);
                    all.push(Entry::You(GrayImg { w, h, px }));
                }
            }
            Some("pi") => {
                if let Some(t) = v["t"].as_str() {
                    all.push(Entry::Pi(t.to_string()));
                }
            }
            _ => {}
        }
    }
    let n = all.len();
    if n > MAX_LOAD {
        all.drain(0..n - MAX_LOAD);
    }
    all
}
