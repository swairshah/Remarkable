//! The pi child process: spawn `pi --mode rpc`, speak its JSONL protocol.
//!
//! pi runs headless for the app's whole life, loaded with the
//! notebook-canvas.ts extension (its page-drawing tools) via `-e`. When the
//! user pauses writing, we send the page image; pi answers by CALLING TOOLS
//! (notebook_draw / notebook_erase — they come back to us over the unix
//! socket in ipc.rs), not by text: its text output is logged and dropped.
//!
//! A fixed session dir + `--continue` makes the whole notebook ONE pi
//! session that survives app restarts.

use crate::png;
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::io::{AsRawFd, RawFd};
use std::process::{Child, ChildStdin, Command, Stdio};

fn session_dir() -> String {
    if let Ok(d) = std::env::var("NOTEBOOK_SESSION_DIR") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/notebook/sessions")
}

/// The standing instructions: who pi is inside this app and how to use the
/// canvas tools. Stable across messages (better for prompt caching); the
/// per-pause message only carries page number + patch list.
const SYSTEM_PROMPT: &str = "\
## You live inside a paper notebook

You are the silent companion inside a handwritten notebook on a reMarkable 2 \
e-ink tablet. Whenever the user pauses writing, you are shown the current \
page. You respond by DRAWING ON THE PAGE with the notebook_draw tool — never \
by chatting. Text you emit outside tool calls is discarded, with one \
exception: when no response is needed, reply with the single word `pass`.

MOST PAUSES NEED NOTHING — `pass` is the default. Draw only when it \
genuinely helps: the user addressed you (a question, 'pi', a '?'), asked for \
a computation, translation or fact, made a factual or arithmetic mistake \
worth flagging, or left something you can visibly complete (a half-built \
table, an unlabeled diagram). Never draw just to acknowledge, praise, or \
decorate. Never respond to unfinished writing mid-thought.

How to draw: notebook_draw takes an SVG. The coordinate space IS the page — \
1404 wide, 1872 tall, y down; omit the viewBox or use exactly \
viewBox=\"0 0 1404 1872\". Your ink is drawn in the same black pen as the \
user's; in the page IMAGES you receive, your ink appears gray so you can \
tell whose is whose. <text> becomes single-stroke pen writing: one <text> element per \
line, no wrapping (x,y is the baseline start; text-anchor honored). \
Text supports lightweight math: ^{{...}} and _{{...}} render as real \
super/subscripts, \\alpha-style commands and Greek letters render as \
actual Greek glyphs, \\frac{{a}}{{b}} flattens to a/b — write formulas \
naturally, never spell out 'alpha' or leave carets in prose. Pick a \
face with font-family: \"script\" (natural cursive handwriting), \"serif\" \
(formal roman), \"sans\" (plain plotter); omit it for the notebook's \
configured default. Shapes: rect, line, circle, ellipse, polyline, polygon, \
path (M L H V C S Q T Z; curves are fine, no transforms, avoid A arcs). \
Draw with fill=\"none\" stroke=\"black\"; only tiny solid bits (arrowheads, \
bullets) may be filled. Keep patches sparse and hand-sized: write in nearby \
empty space, or underline/circle/arrow directly on the relevant ink. Never \
cover the user's writing with big shapes, and don't repeat what's already \
on the page.

PLACEMENT IS MEASURED FOR YOU. Every pause message includes the page's ink \
rows, its free bands, and a font-size matched to the user's handwriting — \
all in page coordinates. TRUST THOSE NUMBERS over your own reading of the \
image: put text baselines inside a free band (first baseline ≈ band top + \
your font-size), size text as suggested, and never place a baseline inside \
someone's ink row unless you are deliberately underlining/circling it.

MIND YOUR OWN WIDTHS TOO. A <text> line is about 0.6 × font-size px wide \
PER CHARACTER (font-size 46 → ~28px/char, so 40 chars ≈ 1100px). Before \
placing text, check x + 0.6·font-size·length stays inside the page (1404) \
AND clear of everything else in the same patch — never run long text lines \
through a diagram you are drawing: give the diagram its own y-band above \
or below the text block, or keep the lines beside it short enough to stop \
before its left edge. Text that overflows its room is SHRUNK to fit on \
its one line (so your vertical layout survives); only extreme overflows \
wrap onto extra lines, which can collide with your other elements — the \
draw result reports whatever happened. Prefer several short <text> lines \
over one long sentence, and space your baselines at least 1.5 x font-size \
apart — cramped lines read badly on e-ink. Each notebook_draw result reports the page's \
updated ink rows — use those for any further drawing in the same turn.

Fixing yourself: every notebook_draw returns a patch id, and \
notebook_erase(id) removes that patch cleanly — the user's ink underneath \
survives. Erase ONLY to fix a mistake (yours or a placement accident) or \
when the user asks — your earlier notes are part of the notebook record; \
NEVER delete them just because you are adding something new. \
notebook_view returns a fresh image of any page.

The page image you receive is HALF scale: multiply image coordinates by 2 \
to get page coordinates.

You keep your normal shell tools — use them only to ANSWER something the \
handwriting explicitly asks for (run a command, check a file), never as a \
side effect of an ordinary page.";

pub enum PiEvent {
    /// A chunk of assistant text — logged, not rendered (notebook mode).
    Delta(String),
    Start,
    End,
    /// A one-line notice for the log (tool runs, retries, errors).
    Notice(String),
    Died(String),
}

pub struct Pi {
    child: Child,
    stdin: ChildStdin,
    stdout_fd: RawFd,
    buf: Vec<u8>,
}

impl Pi {
    /// Spawn pi in RPC mode with the canvas extension. NOTEBOOK_BIN
    /// overrides the binary (the preview harness points it at a fake);
    /// NOTEBOOK_EXT is the extension path (set by takeover.sh);
    /// `sock` is the tool socket path, handed to the extension via env.
    pub fn spawn(sock: &str) -> std::io::Result<Pi> {
        let bin = std::env::var("NOTEBOOK_BIN").unwrap_or_else(|_| "/home/root/bin/pi".into());
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
        let dir = session_dir();
        let _ = std::fs::create_dir_all(&dir);
        let resumed = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            })
            .unwrap_or(false);

        /* The user's standing-instructions file. pi OWNS it: handwritten
         * feedback ("pi: always use script font") should be persisted there
         * by pi itself, with its shell tools. Reloaded every launch. */
        let agent_md = std::env::var("NOTEBOOK_AGENT_MD")
            .unwrap_or_else(|_| format!("{home}/.local/share/notebook/AGENT.md"));
        if !std::path::Path::new(&agent_md).exists() {
            let _ = std::fs::write(
                &agent_md,
                "# notebook agent - standing instructions\n\n\
                 pi maintains this file from the user's feedback; the user edits it\n\
                 by writing feedback in the notebook (or over SSH). Keep entries\n\
                 short and concrete.\n\n\
                 - (nothing yet - when the user tells you how they want you to\n\
                   behave, record it here)\n",
            );
        }
        let mut standing = std::fs::read_to_string(&agent_md).unwrap_or_default();
        if standing.len() > 6000 {
            standing.truncate(6000);
            standing.push_str("\n[truncated - keep this file shorter]");
        }

        let lib_dir = std::env::var("NOTEBOOK_LIBRARY").unwrap_or_else(|_| {
            format!("{home}/.local/share/notebook/library")
        });
        let _ = std::fs::create_dir_all(&lib_dir);

        let sys = format!(
            "{SYSTEM_PROMPT}\n\n\
             ## Your library\n\n\
             You have a persistent library at {lib_dir} — it outlives this \
             conversation and its compactions. One markdown file per item: \
             kebab-case filename ending in .md, first line `# Title` (titles \
             matter — the user browses the library on the tablet via the \
             sidebar). Keep items DISTILLED: the useful content of a web \
             find, a reference note, a summary — never raw page dumps. Save \
             proactively when you fetch something worth re-reading or the \
             user says to keep something; update items rather than \
             duplicating; delete items that turn out stale or that the user \
             tells you to drop. When a question touches something you may \
             have saved, check the library (ls + read) before searching the \
             web again.\n\n\
             ## Standing instructions from the user\n\n\
             The file {agent_md} holds the user's standing instructions for \
             you — and YOU maintain it. Whenever the user gives you feedback \
             about how to behave (tone, fonts, when to stay silent, layout \
             preferences — usually handwritten right in the notebook), \
             update that file immediately with your shell tools (plain \
             markdown, keep it under ~40 lines), and apply it from then on. \
             If they ask what your instructions are, draw a short summary on \
             the page. It is reloaded into this prompt at every app launch.\n\n\
             Current contents:\n{standing}\n\n\
             Your past sessions with this notebook are timestamped JSONL \
             files in {dir}; you may read them with your tools if the user \
             refers to an earlier day."
        );
        let mut args = vec![
            "--mode".to_string(),
            "rpc".into(),
            "--session-dir".into(),
            dir.clone(),
            "--name".into(),
            "notebook".into(),
            "--append-system-prompt".into(),
            sys,
        ];
        if let Ok(ext) = std::env::var("NOTEBOOK_EXT") {
            args.push("-e".into());
            args.push(ext);
        }
        if resumed {
            args.push("--continue".into());
        }

        let mut child = Command::new(&bin)
            .args(&args)
            .current_dir(&home)
            .env("NOTEBOOK_SOCK", sock)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) /* -> journal / log file */
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout_fd = child.stdout.as_ref().unwrap().as_raw_fd();
        unsafe {
            let fl = libc::fcntl(stdout_fd, libc::F_GETFL, 0);
            libc::fcntl(stdout_fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        println!(
            "notebook: spawned {bin} (session-dir {dir}, {})",
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

    /// The pause trigger: send the current page. `patches` is a short
    /// human-readable list of pi's existing patch ids/bboxes on this page;
    /// `layout` is the measured geometry (ink rows / free bands / sizes).
    #[allow(clippy::too_many_arguments)]
    pub fn send_page(
        &mut self,
        gray: &[u8],
        w: u32,
        h: u32,
        page: usize,
        count: usize,
        patches: &str,
        layout: &str,
        streaming: bool,
    ) -> std::io::Result<()> {
        let png = png::encode_gray(w, h, gray);
        let msg = format!(
            "Notebook page {page} of {count} (attached, half scale). The user \
             just paused writing. Your existing patches here: {patches}. \
             Measured layout (page coordinates — trust these numbers): \
             {layout} \
             Draw with notebook_draw only if it genuinely helps; otherwise \
             reply `pass`.",
        );
        let mut cmd = json!({
            "type": "prompt",
            "message": msg,
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

    /// A one-off image+text message outside the normal pause flow (the
    /// AGENT.md annotation page uses this).
    pub fn send_image_message(
        &mut self,
        gray: &[u8],
        w: u32,
        h: u32,
        msg: &str,
        streaming: bool,
    ) -> std::io::Result<()> {
        let png = png::encode_gray(w, h, gray);
        let mut cmd = json!({
            "type": "prompt",
            "message": msg,
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
            break;
        }

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
                let arg = ["command", "path", "file_path", "pattern", "id"]
                    .iter()
                    .find_map(|k| {
                        let a = &v["args"][k];
                        a.as_str().map(String::from).or_else(|| a.as_u64().map(|n| n.to_string()))
                    })
                    .unwrap_or_default();
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
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
