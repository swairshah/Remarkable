#!/usr/bin/env python3
"""Render collab's pi session traces (.jsonl) into one self-contained HTML.

The debugging view this exists for: each exchange shown as the HANDWRITING
image pi received next to the reply it streamed back — so "pi misread my
writing" and "the reply rendered wrong" are visible at a glance. Fenced
```svg blocks in replies are rendered as actual diagrams (with source a
click away), since that's what the app draws on the panel. Thinking, tool
runs, usage/cost sit on the same timeline.

Usage:
    tools/trace-viewer.py SESSION.jsonl [more.jsonl ...] -o out.html
    tools/trace-viewer.py DIR -o out.html      # all *.jsonl in DIR, newest first
    ... --log collab.log                       # include the device app log

No dependencies. Open the output in any browser.
(Sibling of notebook/tools/trace-viewer.py, adapted for a chat app: no
page overlays; replies are markdown-ish text, not notebook_draw calls.)
"""
import html
import json
import os
import re
import sys

CSS = """
:root { color-scheme: light dark; }
* { box-sizing: border-box; }
body { font: 14px/1.45 -apple-system, system-ui, sans-serif; margin: 0;
       background: #f4f3ef; color: #1c1b18; }
@media (prefers-color-scheme: dark) {
  body { background: #191817; color: #e8e6e1; }
  .card { background: #232120 !important; border-color: #3a3835 !important; }
  .nav { background: #232120 !important; border-color: #3a3835 !important; }
  pre { background: #191817 !important; }
  .imgbox { background: #fff; }
}
.wrap { max-width: 1080px; margin: 0 auto; padding: 18px; }
h1 { font-size: 18px; } h2 { font-size: 15px; margin: 6px 0; }
.nav { position: sticky; top: 0; z-index: 5; background: #fffdf8;
       border-bottom: 1px solid #ddd8cf; padding: 8px 18px; overflow-x: auto;
       white-space: nowrap; }
.nav a { margin-right: 10px; text-decoration: none; color: inherit;
         opacity: .75; font-size: 12.5px; }
.nav a:hover { opacity: 1; }
.card { background: #fffdf8; border: 1px solid #ddd8cf; border-radius: 10px;
        padding: 12px 14px; margin: 12px 0; overflow-wrap: anywhere; }
.role { font-size: 11px; letter-spacing: .08em; text-transform: uppercase;
        opacity: .6; margin-bottom: 6px; display: flex; gap: 8px;
        align-items: baseline; flex-wrap: wrap; }
.user  { border-left: 4px solid #4a72b8; }
.asst  { border-left: 4px solid #7a5cc4; }
.tool  { border-left: 4px solid #b8863f; }
.meta  { border-left: 4px solid #999; opacity: .85; }
.badge { display: inline-block; border-radius: 20px; padding: 1px 10px;
         font-size: 11px; font-weight: 600; letter-spacing: .04em; }
.svgb  { background: #2f4d7d22; color: #2f4d7d; border: 1px solid #2f4d7d55; }
.err   { background: #b0323233; color: #b03232; }
.usage { font-size: 11px; opacity: .55; }
pre { background: #f0ede6; border-radius: 6px; padding: 8px 10px;
      overflow-x: auto; font-size: 12px; }
details > summary { cursor: pointer; opacity: .7; font-size: 12.5px;
                    margin: 4px 0; }
.imgbox { width: 420px; max-width: 100%; border: 1px solid #ccc;
          border-radius: 4px; background: #fff; }
.imgbox img, .imgbox svg { display: block; width: 100%; height: auto; }
.thinking { font-style: italic; opacity: .75; white-space: pre-wrap; }
.text { white-space: pre-wrap; }
.inkimg { max-width: 420px; width: 100%; border: 1px solid #ccc;
          border-radius: 4px; background: #fff; }
.turnhdr { margin: 26px 0 4px; font-size: 13px; opacity: .6; }
hr.sess { margin: 40px 0; border: none; border-top: 3px double #bbb; }
"""

SVG_FENCE = re.compile(r"```svg\s*\n(.*?)```", re.S | re.I)


def esc(s):
    return html.escape(str(s), quote=True)


def render_reply_text(txt, out):
    """Reply text with each fenced ```svg block rendered as a real diagram
    (the app draws these on the panel), source behind a toggle."""
    pos = 0
    for m in SVG_FENCE.finditer(txt):
        before = txt[pos:m.start()].strip()
        if before:
            out.append(f"<div class='text'>{esc(before)}</div>")
        src = m.group(1)
        out.append("<span class='badge svgb'>SVG DIAGRAM</span>")
        out.append(f"<div class='imgbox'>{src}</div>")  # trusted local traces
        out.append(f"<details><summary>svg source</summary><pre>{esc(src)}</pre></details>")
        pos = m.end()
    rest = txt[pos:].strip()
    if rest:
        out.append(f"<div class='text'>{esc(rest)}</div>")


def content_blocks(content):
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    return content or []


def fmt_usage(msg):
    u = msg.get("usage") or {}
    cost = (u.get("cost") or {}).get("total")
    bits = []
    if u.get("input") is not None:
        bits.append(f"in {u.get('input', 0)} + cache {u.get('cacheRead', 0)}")
        bits.append(f"out {u.get('output', 0)}")
    if cost is not None:
        bits.append(f"${cost:.4f}")
    model = msg.get("model", "")
    stop = msg.get("stopReason", "")
    lead = " · ".join(x for x in [model, stop] if x)
    tail = " · ".join(bits)
    return f'<span class="usage">{esc(lead)}{" · " if lead and tail else ""}{esc(tail)}</span>'


def render_session(path, out):
    entries = []
    with open(path) as f:
        for ln in f:
            ln = ln.strip()
            if ln:
                try:
                    entries.append(json.loads(ln))
                except ValueError:
                    pass
    header = entries[0] if entries and entries[0].get("type") == "session" else {}
    out.append(f"<h1 id='{esc(os.path.basename(path))}'>session {esc(os.path.basename(path))}</h1>")
    out.append(f"<div class='usage'>cwd {esc(header.get('cwd', '?'))} · {len(entries)} entries</div>")

    turn = 0
    nav = []

    for e in entries:
        if e.get("type") == "compaction":
            out.append(f"<div class='card meta'><div class='role'>compaction</div>"
                       f"<div class='text'>{esc(e.get('summary', ''))[:2000]}</div></div>")
            continue
        if e.get("type") != "message":
            continue
        msg = e.get("message") or {}
        role = msg.get("role")
        ts = (e.get("timestamp") or "")[11:19]

        if role == "user":
            turn += 1
            anchor = f"t{turn}"
            nav.append(f"<a href='#{anchor}'>#{turn} {ts}</a>")
            out.append(f"<div class='turnhdr' id='{anchor}'>— message #{turn} · {ts} —</div>")
            out.append("<div class='card user'><div class='role'>handwriting → pi</div>")
            for b in content_blocks(msg.get("content")):
                if b.get("type") == "image":
                    uri = f"data:{b.get('mimeType', 'image/png')};base64,{b.get('data', '')}"
                    out.append(f"<img class='inkimg' src='{uri}'>")
                elif b.get("type") == "text":
                    # the fixed harness prompt is noise; keep it one click away
                    out.append(f"<details><summary>prompt text</summary>"
                               f"<div class='text'>{esc(b['text'])}</div></details>")
            out.append("</div>")

        elif role == "assistant":
            out.append(f"<div class='card asst'><div class='role'>pi · {ts} {fmt_usage(msg)}</div>")
            if msg.get("stopReason") in ("error", "aborted"):
                out.append(f"<span class='badge err'>{esc(msg.get('stopReason'))}: "
                           f"{esc(msg.get('errorMessage', ''))}</span>")
            for b in content_blocks(msg.get("content")):
                t = b.get("type")
                if t == "thinking":
                    out.append(f"<details><summary>thinking</summary>"
                               f"<div class='thinking'>{esc(b.get('thinking', ''))}</div></details>")
                elif t == "text":
                    if b.get("text", "").strip():
                        render_reply_text(b["text"], out)
                elif t == "toolCall":
                    name = b.get("name", "?")
                    args = b.get("arguments") or {}
                    out.append(f"<details><summary>tool: {esc(name)}</summary>"
                               f"<pre>{esc(json.dumps(args, indent=2)[:4000])}</pre></details>")
            out.append("</div>")

        elif role == "toolResult":
            err = " · ERROR" if msg.get("isError") else ""
            out.append(f"<div class='card tool'><div class='role'>"
                       f"{esc(msg.get('toolName', 'tool'))} result{err} · {ts}</div>")
            for b in content_blocks(msg.get("content")):
                if b.get("type") == "text":
                    out.append(f"<div class='text'>{esc(b['text'][:3000])}</div>")
                elif b.get("type") == "image":
                    uri = f"data:{b.get('mimeType', 'image/png')};base64,{b.get('data', '')}"
                    out.append(f"<img class='inkimg' src='{uri}'>")
            out.append("</div>")

        elif role == "custom":
            out.append(f"<div class='card meta'><div class='role'>"
                       f"{esc(msg.get('customType', 'custom'))}</div></div>")

    return nav


def render_device_log(path, out):
    try:
        lines = open(path, errors="replace").read().splitlines()[-600:]
    except OSError:
        return
    marked = []
    for ln in lines:
        cls = ""
        if "-> pi" in ln or "reply" in ln:
            cls = " style='font-weight:600'"
        elif any(k in ln for k in ("error", "failed", "Died", "exited", "note:")):
            cls = " style='color:#b03232'"
        marked.append(f"<span{cls}>{esc(ln)}</span>")
    out.append(
        "<div class='card meta'><div class='role'>device log — /tmp/collab.log "
        f"(last {len(lines)} lines)</div><details open><summary>toggle</summary>"
        f"<pre style='max-height:340px;overflow:auto'>{chr(10).join(marked)}</pre>"
        "</details></div>")


def main():
    args = [a for a in sys.argv[1:]]
    out_path = "trace.html"
    log_path = None
    if "-o" in args:
        i = args.index("-o")
        out_path = args[i + 1]
        del args[i:i + 2]
    if "--log" in args:
        i = args.index("--log")
        log_path = args[i + 1]
        del args[i:i + 2]
    files = []
    for a in args:
        if os.path.isdir(a):
            files += sorted(
                (os.path.join(a, f) for f in os.listdir(a) if f.endswith(".jsonl")),
                reverse=True)  # newest first (timestamped names)
        else:
            files.append(a)
    if not files:
        sys.exit("no session files given")

    body, all_nav = [], []
    if log_path:
        render_device_log(log_path, body)
    for i, f in enumerate(files):
        if i:
            body.append("<hr class='sess'>")
        all_nav += render_session(f, body)

    doc = (f"<!doctype html><meta charset='utf-8'><title>collab · pi traces</title>"
           f"<style>{CSS}</style>"
           f"<div class='nav'>{''.join(all_nav)}</div>"
           f"<div class='wrap'>{''.join(body)}</div>")
    with open(out_path, "w") as f:
        f.write(doc)
    sz = os.path.getsize(out_path)
    print(f"trace-viewer: wrote {out_path} ({sz / 1e6:.1f} MB, {len(files)} session(s))")


if __name__ == "__main__":
    main()
