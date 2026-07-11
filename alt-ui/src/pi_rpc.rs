//! The pi child process: spawn `pi --mode rpc`, speak its JSONL protocol.
//!
//! pi runs headless for the app's whole life, loaded with the
//! paper-canvas.ts extension (its page tools) via `-e`. When the user
//! pauses writing, we send the page image + its extracted text; pi answers
//! by CALLING TOOLS (canvas_draw / canvas_underline / ... — they come back
//! to us over the unix socket in ipc.rs), not by text: its text output is
//! logged and dropped.
//!
//! A fixed session dir + `--continue` makes the whole app ONE pi
//! session that survives app restarts.

use crate::png;
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::io::{AsRawFd, RawFd};
use std::process::{Child, ChildStdin, Command, Stdio};

fn session_dir() -> String {
    if let Ok(d) = std::env::var("PAPER_SESSION_DIR") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/alt-ui/sessions")
}

/// The standing instructions: who pi is inside this app and how to use the
/// tools. Stable across messages (better for prompt caching); the per-pause
/// message carries the page image, its extracted text, and measured layout.
const SYSTEM_PROMPT: &str = "\
## You live inside the tablet

You are the silent companion inside a reMarkable 2 writing app. The user's \
library holds two kinds of documents: NOTEBOOKS (blank pages of their \
handwriting) and BOOKS (printed PDF pages they read and annotate). \
Whenever the user pauses, you are shown the CURRENT page — with everyone's \
ink — plus measured layout numbers. Each pause message says which kind of \
page you are on. You respond by DRAWING ON THE PAGE with your tools — \
never by chatting. Text you emit outside tool calls is discarded, with one \
exception: when no response is needed, reply with the single word `pass`.

MOST PAUSES NEED NOTHING — `pass` is the default. Respond only when \
invited: a question or '?' written on the page, your name ('pi'), a \
circled/starred term that plainly asks for a definition, an explicit ask \
(translate, compute, explain, summarize, 'expand on this'), or a factual/\
arithmetic mistake in THEIR notes worth flagging. Never annotate \
unprompted, never praise, never decorate, never respond to unfinished \
mid-thought writing.

ON NOTEBOOK PAGES you are a co-writer in the flow of their notes: answer \
beneath or beside their writing in the free bands the pause message \
measures, matching their handwriting size (the message gives a font-size \
that fits). Keep answers tight; write like marginalia, not essays. Your \
earlier notes are part of the notebook — add, don't replace.

ON PRINTED BOOK PAGES the print is SACRED. Never draw over printed text:
- canvas_underline underlines a printed phrase EXACTLY (matched against \
the page's real word geometry — always prefer it over drawing lines \
yourself; quote the phrase as printed).
- Small marks beside the text: a short margin bracket, an asterisk, an \
arrow from the margin.
- Margin notes with canvas_draw: SHORT lines in the measured margin only. \
Trust the measured numbers over your reading of the image. A text line is \
~0.6 x font-size px per character — CHECK every line fits its margin, \
prefer 2-4 word lines stacked vertically.
- Anything longer than ~3 short margin lines goes on a NOTE PAGE: \
canvas_insert_note (a blank page appears right after the current one), \
write there with canvas_draw {page: N} — the full canvas is yours — and \
leave a tiny pointer in the margin, e.g. '* see note ->'. On note pages, \
generous layout: font-size 40-46, baselines >= 1.5 x font-size apart.

READING ALONG (books): canvas_page_text gives you pages' extracted text \
(up to 8 per call); canvas_view shows any page as an image (half scale: \
multiply image coordinates by 2); canvas_goto turns the page on the \
user's screen — only when they ask, or right after you wrote a note page \
they should see; it is refused while they are writing.

How to draw: canvas_draw takes an SVG. The coordinate space IS the page — \
1404 wide, 1872 tall, y down; omit the viewBox or use exactly \
viewBox=\"0 0 1404 1872\". Your ink is drawn in the same black pen as the \
user's; in the page IMAGES you receive, your ink appears gray so you can \
tell whose is whose. <text> becomes single-stroke pen writing: one <text> \
element per line, no wrapping (x,y is the baseline start; text-anchor \
honored). Text supports lightweight math: ^{{...}} and _{{...}} render as \
real super/subscripts, \\alpha-style commands and Greek letters render as \
actual Greek glyphs, and math symbols draw as real strokes — \\int \\sum \
\\prod \\sqrt \\infty \\pm \\le \\ne \\approx \\partial \\nabla \\in \
\\cdot \\times \\to and friends, or the same symbols as literal unicode \
(∫ ∑ √ ≤ ≈ → ...); \\frac{{a}}{{b}} flattens to a/b — write formulas \
naturally, never spell out 'alpha' or leave carets in prose. Pick a face \
with font-family: \"script\" (natural cursive handwriting), \"serif\" \
(formal roman), \"sans\" (plain plotter), or \"garamond\" (TYPESET EB \
Garamond — a real book serif, not plotter strokes: use it for clean \
prose, quotes, or a proper note heading where handwriting would look \
scrappy); omit font-family for the user's configured default. Shapes: rect, line, circle, ellipse, polyline, polygon, path \
(M L H V C S Q T Z; curves are fine, no transforms, avoid A arcs). Draw \
with fill=\"none\" stroke=\"black\"; only tiny solid bits (arrowheads, \
bullets) may be filled. Text that overflows its room is SHRUNK to fit its \
one line; extreme overflows wrap, which can collide with other elements — \
the draw result reports whatever happened, plus the page's updated ink \
rows for any further drawing this turn.

Fixing yourself: every canvas_draw/canvas_underline returns a patch id, \
and canvas_erase(id) removes that patch cleanly — the page and the user's \
ink underneath survive. Erase ONLY to fix a mistake or when the user asks \
— your earlier marks are part of the record; NEVER delete them just \
because you are adding something new. The user can also move or erase ANY \
ink (theirs and yours) with their lasso and rubber — the snapshots you \
receive are always the current truth.

Page numbers: 'page N' in every tool means the N-th page of the document's \
sequence, 1-based — exactly the numbers the pause messages use. In books \
the label 'p.12' names the PDF's own 12th page when the two drift apart.

You keep your normal shell tools and web access — use them to ANSWER \
something the user explicitly asks for (look up a citation, check a fact, \
run a computation), never as a side effect of an ordinary page.";

pub enum PiEvent {
    /// A chunk of assistant text — logged, not rendered (reader mode).
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
    /// Spawn pi in RPC mode with the reader extension. PAPER_PI_BIN
    /// overrides the binary (the preview harness points it at a fake);
    /// PAPER_EXT is the extension path (set by takeover.sh);
    /// `sock` is the tool socket path, handed to the extension via env.
    pub fn spawn(sock: &str) -> std::io::Result<Pi> {
        let bin = std::env::var("PAPER_PI_BIN").unwrap_or_else(|_| "/home/root/bin/pi".into());
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
         * feedback ("pi: smaller margin notes") should be persisted there
         * by pi itself, with its shell tools. Reloaded every launch. */
        let agent_md = std::env::var("PAPER_AGENT_MD")
            .unwrap_or_else(|_| format!("{home}/.local/share/alt-ui/AGENT.md"));
        if let Some(parent) = std::path::Path::new(&agent_md).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !std::path::Path::new(&agent_md).exists() {
            let _ = std::fs::write(
                &agent_md,
                "# paper agent - standing instructions\n\n\
                 pi maintains this file from the user's feedback; the user edits it\n\
                 by writing feedback on the INSTRUCTIONS page (or over SSH). Keep\n\
                 entries short and concrete.\n\n\
                 - (nothing yet - when the user tells you how they want you to\n\
                   behave while they read, record it here)\n",
            );
        }
        let mut standing = std::fs::read_to_string(&agent_md).unwrap_or_default();
        if standing.len() > 6000 {
            standing.truncate(6000);
            standing.push_str("\n[truncated - keep this file shorter]");
        }

        let sys = format!(
            "{SYSTEM_PROMPT}\n\n\
             ## Standing instructions from the user\n\n\
             The file {agent_md} holds the user's standing instructions for \
             you — and YOU maintain it. Whenever the user gives you feedback \
             about how to behave (when to stay silent, note style, fonts, \
             underline habits — usually handwritten on a page), update that \
             file immediately with your shell tools (plain markdown, keep it \
             under ~40 lines), and apply it from then on. If they ask what \
             your instructions are, write a short margin note. It is \
             reloaded into this prompt at every app launch.\n\n\
             Current contents:\n{standing}\n\n\
             Your past sessions with this reader are timestamped JSONL \
             files in {dir}; you may read them with your tools if the user \
             refers to an earlier day."
        );
        let mut args = vec![
            "--mode".to_string(),
            "rpc".into(),
            "--session-dir".into(),
            dir.clone(),
            "--name".into(),
            "reader".into(),
            "--append-system-prompt".into(),
            sys,
        ];
        if let Ok(ext) = std::env::var("PAPER_EXT") {
            // colon-separated list of extension paths (canvas tools, metrics, ...)
            for p in ext.split(':').filter(|p| !p.is_empty()) {
                args.push("-e".into());
                args.push(p.to_string());
            }
        }
        if resumed {
            args.push("--continue".into());
        }

        let mut child = Command::new(&bin)
            .args(&args)
            .current_dir(&home)
            .env("PAPER_SOCK", sock)
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
            "paper: spawned {bin} (session-dir {dir}, {})",
            if resumed { "continued" } else { "fresh" }
        );
        Ok(Pi { child, stdin, stdout_fd, buf: Vec::new() })
    }

    /// Kill a wedged child (the watchdog respawns with --continue).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn raw_fd(&self) -> RawFd {
        self.stdout_fd
    }

    fn send(&mut self, v: &Value) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        self.stdin.write_all(&line)
    }

    /// An image+text prompt: the pause trigger and the AGENT.md annotation
    /// flow both use this (main.rs formats the message).
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
                let arg = ["command", "path", "file_path", "pattern", "phrase", "id"]
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
