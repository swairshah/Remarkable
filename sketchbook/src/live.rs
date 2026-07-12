//! LIVE streaming: mirror pen + AI ink to the web viewer in real time.
//!
//! Toggled from the sidebar (off at launch — it costs a persistent ssh
//! connection, so it only runs when asked). When on, the app spawns the
//! transport command and writes one JSON event per line into its stdin:
//!
//!   {"t":"hi","page":N}            stream start (current page, 1-based)
//!   {"t":"s","id":K,"p":[x,y,r..]} user ink points   (coords x10)
//!   {"t":"rub","p":[x,y,..]}       rubber path       (coords x10)
//!   {"t":"ai","id":K,"p":[..]}     pi ink as the ghost hand draws it
//!   {"t":"page","n":N}             page turn
//!   {"t":"st","s":"think|draw|idle"}  pi status
//!
//! Transport: $SKETCHBOOK_LIVE_CMD (preview harness), or ssh with the tablet's
//! sync key to the VM's ingest script, which feeds sketchbook-live-relay and
//! comes out of https://remarkable.exe.xyz/sketchbook/ as an SSE overlay.
//!
//! Same discipline as the tool socket: the pipe fd is nonblocking, events
//! queue in a bounded buffer flushed from the main loop, and a stalled or
//! dead connection drops the buffer and schedules a reconnect — the pen
//! must never wait for the network.

use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

const BUF_CAP: usize = 256 * 1024;
const RETRY_AFTER: Duration = Duration::from_secs(5);

fn default_cmd() -> String {
    "exec /usr/bin/ssh -y -i /home/root/.ssh/id_sync_dropbear_ed25519 \
     exedev@remarkable.exe.xyz /home/exedev/bin/sketchbook-live-ingest.sh"
        .into()
}

/// One in-progress point run: consecutive same-kind points coalesce into a
/// single event per flush, so a fast pen doesn't emit one line per sample.
struct Pend {
    tag: &'static str,
    id: u64,
    pts: Vec<i64>,
}

pub struct Live {
    pub enabled: bool,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    buf: Vec<u8>,
    pend: Option<Pend>,
    retry_at: Option<Instant>,
    stroke_id: u64,
    ai_id: u64,
}

impl Live {
    pub fn new() -> Live {
        Live {
            enabled: false,
            child: None,
            stdin: None,
            buf: Vec::new(),
            pend: None,
            retry_at: None,
            stroke_id: 0,
            ai_id: 0,
        }
    }

    /// Sidebar toggle. Returns the new state.
    pub fn toggle(&mut self, page: usize) -> bool {
        if self.enabled {
            self.push_line(r#"{"t":"bye"}"#.into());
            let _ = self.flush();
            self.disconnect();
            self.enabled = false;
            self.retry_at = None;
            println!("sketchbook: live stream off");
        } else {
            self.enabled = true;
            self.connect(page);
            println!("sketchbook: live stream on");
        }
        self.enabled
    }

    fn connect(&mut self, page: usize) {
        self.disconnect();
        let cmd = std::env::var("SKETCHBOOK_LIVE_CMD").unwrap_or_else(|_| default_cmd());
        match Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                let stdin = child.stdin.take();
                if let Some(si) = &stdin {
                    unsafe {
                        let fd = si.as_raw_fd();
                        let fl = libc::fcntl(fd, libc::F_GETFL, 0);
                        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
                    }
                }
                self.child = Some(child);
                self.stdin = stdin;
                self.buf.clear();
                self.pend = None;
                self.push_line(format!(r#"{{"t":"hi","page":{}}}"#, page + 1));
                println!("sketchbook: live stream connecting");
            }
            Err(e) => {
                println!("sketchbook: live spawn failed: {e}");
                self.retry_at = Some(Instant::now() + RETRY_AFTER);
            }
        }
    }

    fn disconnect(&mut self) {
        self.stdin = None; /* closes the pipe: remote sees EOF */
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.buf.clear();
        self.pend = None;
    }

    /* -- event intake (all no-ops while off) -- */

    pub fn pen_break(&mut self) {
        self.stroke_id += 1;
    }

    pub fn pen(&mut self, x: f32, y: f32, r: f32) {
        if self.enabled {
            self.point("s", self.stroke_id, x, y, Some(r));
        }
    }

    pub fn rub(&mut self, x: f32, y: f32) {
        if self.enabled {
            self.point("rub", 0, x, y, None);
        }
    }

    pub fn ai_break(&mut self) {
        self.ai_id += 1;
    }

    pub fn ai(&mut self, x: f32, y: f32, r: f32) {
        if self.enabled {
            self.point("ai", self.ai_id, x, y, Some(r));
        }
    }

    pub fn page(&mut self, page: usize) {
        if self.enabled {
            self.flush_pend();
            self.push_line(format!(r#"{{"t":"page","n":{}}}"#, page + 1));
        }
    }

    pub fn status(&mut self, s: &str) {
        if self.enabled {
            self.flush_pend();
            self.push_line(format!(r#"{{"t":"st","s":"{s}"}}"#));
        }
    }

    fn point(&mut self, tag: &'static str, id: u64, x: f32, y: f32, r: Option<f32>) {
        let same = self.pend.as_ref().is_some_and(|p| p.tag == tag && p.id == id);
        if !same {
            self.flush_pend();
            self.pend = Some(Pend { tag, id, pts: Vec::with_capacity(24) });
        }
        if let Some(p) = self.pend.as_mut() {
            p.pts.push((x * 10.0).round() as i64);
            p.pts.push((y * 10.0).round() as i64);
            if let Some(r) = r {
                p.pts.push((r * 10.0).round() as i64);
            }
        }
    }

    fn flush_pend(&mut self) {
        let Some(p) = self.pend.take() else { return };
        if p.pts.is_empty() {
            return;
        }
        let pts: Vec<String> = p.pts.iter().map(|v| v.to_string()).collect();
        self.push_line(format!(
            r#"{{"t":"{}","id":{},"p":[{}]}}"#,
            p.tag,
            p.id,
            pts.join(",")
        ));
    }

    fn push_line(&mut self, line: String) {
        if self.stdin.is_none() && self.retry_at.is_none() && !self.enabled {
            return;
        }
        if self.buf.len() + line.len() + 1 > BUF_CAP {
            /* the pipe has been stuck for a long while: drop and reconnect
             * rather than hoard memory — the mirror reconciles later */
            println!("sketchbook: live buffer overflow, reconnecting");
            self.disconnect();
            self.retry_at = Some(Instant::now() + RETRY_AFTER);
            return;
        }
        self.buf.extend_from_slice(line.as_bytes());
        self.buf.push(b'\n');
    }

    /// Main-loop housekeeping: batch flush, dead-child reap, reconnect.
    pub fn tick(&mut self, page: usize) {
        if !self.enabled {
            return;
        }
        /* transport died? (ssh dropped, relay restarted) */
        if let Some(c) = self.child.as_mut() {
            if c.try_wait().ok().flatten().is_some() {
                println!("sketchbook: live transport exited; retrying soon");
                self.disconnect();
                self.retry_at = Some(Instant::now() + RETRY_AFTER);
            }
        }
        if self.child.is_none() {
            if self.retry_at.is_some_and(|t| Instant::now() >= t) {
                self.retry_at = None;
                self.connect(page);
            }
            return;
        }
        self.flush_pend();
        if let Err(e) = self.flush() {
            println!("sketchbook: live write failed ({e}); retrying soon");
            self.disconnect();
            self.retry_at = Some(Instant::now() + RETRY_AFTER);
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let Some(si) = self.stdin.as_mut() else { return Ok(()) };
        while !self.buf.is_empty() {
            match si.write(&self.buf) {
                Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "pipe closed")),
                Ok(n) => {
                    self.buf.drain(..n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.disconnect();
    }
}
