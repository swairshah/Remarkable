//! The pi child process: spawn `pi --mode rpc`, speak its JSONL protocol.
//!
//! pi runs headless in the background for the app's whole life. We send
//! `prompt` commands (handwriting PNGs as base64 image attachments) on its
//! stdin and stream events off its stdout, which the main loop polls like
//! any other fd. Protocol reference: the rpc.md doc shipped inside the
//! @mariozechner/pi-coding-agent package.
//!
//! A fixed --session-id makes the whole collaboration ONE pi session that
//! survives app restarts. pi's stderr is inherited, so its complaints land
//! in xochitl's journal next to ours (`make log`).

use crate::png;
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::io::{AsRawFd, RawFd};
use std::process::{Child, ChildStdin, Command, Stdio};

/// Where collab keeps its own conversation history — a dedicated data
/// dir, NOT the cluttered /home/root. `$COLLAB_SESSION_DIR` overrides it
/// (the preview harness points it at a scratch dir).
fn session_dir() -> String {
    if let Ok(d) = std::env::var("COLLAB_SESSION_DIR") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/collab/sessions")
}

/// What the UI cares about, distilled from the event stream.
pub enum PiEvent {
    /// A chunk of assistant text — append to the current reply.
    Delta(String),
    /// Agent started working on a prompt.
    Start,
    /// Agent finished (reply complete, tools done).
    End,
    /// A one-line notice for the log (tool runs, retries, errors).
    Notice(String),
    /// The pi process is gone; the message is a human-readable reason.
    Died(String),
}

pub struct Pi {
    child: Child,
    stdin: ChildStdin,
    stdout_fd: RawFd,
    buf: Vec<u8>,
}

impl Pi {
    /// Spawn pi in RPC mode. COLLAB_BIN overrides the binary (the
    /// preview harness points it at a fake); the default is where
    /// pi-harness installs it on the tablet.
    ///
    /// Sessions live in a dedicated dir (see `session_dir`), one timestamped
    /// JSONL file per conversation. If a prior session exists we `--continue`
    /// it, so pi keeps its memory across app restarts; either way pi is told
    /// where the dated history lives so it can read older ones on request.
    pub fn spawn() -> std::io::Result<Pi> {
        let bin = std::env::var("COLLAB_BIN")
            .unwrap_or_else(|_| "/home/root/bin/pi".into());
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
        let dir = session_dir();
        let _ = std::fs::create_dir_all(&dir);
        let resumed = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            })
            .unwrap_or(false);

        let sys = format!(
            "Your past conversations with this user are saved as timestamped \
             JSONL session files in {dir}. If the user refers to an earlier \
             session or a previous day, you may read those files with your \
             tools to recall what was discussed."
        );
        let mut args = vec![
            "--mode".to_string(),
            "rpc".into(),
            "--session-dir".into(),
            dir.clone(),
            "--name".into(),
            "collab".into(),
            "--append-system-prompt".into(),
            sys,
        ];
        if resumed {
            args.push("--continue".into()); /* resume the latest session */
        }

        let mut child = Command::new(&bin)
            .args(&args)
            .current_dir(&home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) /* -> xochitl journal */
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout_fd = child.stdout.as_ref().unwrap().as_raw_fd();
        /* stdout is drained non-blockingly from the main poll loop */
        unsafe {
            let fl = libc::fcntl(stdout_fd, libc::F_GETFL, 0);
            libc::fcntl(stdout_fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        println!(
            "collab: spawned {bin} (session-dir {dir}, {})",
            if resumed { "continued" } else { "fresh" }
        );
        Ok(Pi { child, stdin, stdout_fd, buf: Vec::new() })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.stdout_fd
    }

    fn send(&mut self, v: &Value) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        self.stdin.write_all(&line)
    }

    /// Send the handwritten message. When pi is mid-reply (`streaming`),
    /// it must be queued with `followUp` — delivered after pi finishes;
    /// when idle, a plain prompt starts a run immediately (specifying a
    /// streamingBehavior then would queue it against a run that never comes).
    pub fn send_ink(
        &mut self,
        gray: &[u8],
        w: u32,
        h: u32,
        streaming: bool,
    ) -> std::io::Result<()> {
        let png = png::encode_gray(w, h, gray);
        let mut cmd = json!({
            "type": "prompt",
            "message": "The attached image is a message the user just handwrote to you \
                        on a reMarkable tablet. Read it and respond directly, concisely. \
                        Your reply is rendered on an e-ink screen that DOES format \
                        markdown: use headings (#), bullet lists (-), and fenced code \
                        blocks (```lang) where they help. You may also include a simple \
                        SVG diagram in a ```svg fenced block — it will be drawn. For \
                        diagrams: use a viewBox; draw boxes/nodes UNFILLED \
                        (fill=\"none\" stroke=\"black\"), never solid-filled, so their \
                        labels stay readable; put every label in a <text> element with \
                        text-anchor=\"middle\" positioned at the box center, font-size \
                        around 12-16; use <line>/<polyline> for connectors and small \
                        solid <polygon> arrowheads. Stick to rect, line, circle, \
                        ellipse, polyline, polygon, path (straight segments), and text. \
                        Avoid tables, raster images, and very wide code lines.",
            "images": [{
                "type": "image",
                "data": png::base64(&png),
                "mimeType": "image/png",
            }],
        });
        if streaming {
            cmd["streamingBehavior"] = json!("followUp");
        }
        self.send(&cmd)
    }

    /// Auto-dismiss extension dialogs so a headless question can't wedge
    /// the agent (we have no keyboard to answer with).
    fn dismiss_dialog(&mut self, id: &Value) {
        let _ = self.send(&json!({
            "type": "extension_ui_response", "id": id, "cancelled": true,
        }));
    }

    /// Drain whatever pi has written and distill it into UI events.
    pub fn drain(&mut self) -> Vec<PiEvent> {
        let mut out = Vec::new();
        loop {
            let mut chunk = [0u8; 16384];
            let n = unsafe {
                libc::read(self.stdout_fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len())
            };
            if n > 0 {
                self.buf.extend_from_slice(&chunk[..n as usize]);
                continue;
            }
            if n == 0 {
                let status = self
                    .child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map(|s| format!("exit {}", s.code().unwrap_or(-1)))
                    .unwrap_or_else(|| "stdout closed".into());
                out.push(PiEvent::Died(status));
            }
            break; /* n < 0: EAGAIN (drained) or a real error; stop either way */
        }

        /* strict JSONL: split on \n only (per pi's rpc.md) */
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = &line[..line.len() - 1];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<Value>(line) {
                Ok(v) => self.translate(&v, &mut out),
                Err(e) => out.push(PiEvent::Notice(format!("bad rpc json: {e}"))),
            }
        }
        out
    }

    fn translate(&mut self, v: &Value, out: &mut Vec<PiEvent>) {
        match v["type"].as_str().unwrap_or("") {
            "agent_start" => out.push(PiEvent::Start),
            "agent_end" => out.push(PiEvent::End),
            "message_update" => {
                let ev = &v["assistantMessageEvent"];
                if ev["type"] == "text_delta" {
                    if let Some(d) = ev["delta"].as_str() {
                        out.push(PiEvent::Delta(d.to_string()));
                    }
                }
            }
            "tool_execution_start" => {
                let name = v["toolName"].as_str().unwrap_or("tool");
                /* show the most informative small arg we can find */
                let arg = ["command", "path", "file_path", "pattern"]
                    .iter()
                    .find_map(|k| v["args"][k].as_str())
                    .unwrap_or("");
                let mut line = format!("[{name}] {arg}");
                line.truncate(120);
                out.push(PiEvent::Notice(line));
            }
            "auto_retry_start" => {
                out.push(PiEvent::Notice("[retrying after transient error]".into()));
            }
            "extension_error" => {
                let e = v["error"].as_str().unwrap_or("?");
                out.push(PiEvent::Notice(format!("[extension error: {e}]")));
            }
            "extension_ui_request" => {
                let method = v["method"].as_str().unwrap_or("");
                if matches!(method, "select" | "confirm" | "input" | "editor") {
                    self.dismiss_dialog(&v["id"]);
                    let title = v["title"].as_str().unwrap_or(method);
                    out.push(PiEvent::Notice(format!("[dismissed dialog: {title}]")));
                } else if method == "notify" {
                    let m = v["message"].as_str().unwrap_or("");
                    out.push(PiEvent::Notice(format!("[{m}]")));
                }
            }
            "response" => {
                if v["success"] == false {
                    let e = v["error"].as_str().unwrap_or("unknown error");
                    out.push(PiEvent::Notice(format!("[pi error: {e}]")));
                }
            }
            _ => {} /* turn_*, message_start/end, queue_update, compaction_* */
        }
    }
}

impl Drop for Pi {
    fn drop(&mut self) {
        /* the session file keeps the conversation; the process can go */
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
