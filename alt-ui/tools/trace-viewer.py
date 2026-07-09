#!/usr/bin/env python3
"""Render reader's pi session traces (.jsonl) into one self-contained HTML.

The debugging view this exists for: every reader_draw call is shown as an
OVERLAY — pi's SVG drawn in red on top of the page image it had just been
shown — so placement mistakes ("wrote over my ink", "missed the empty
space") are visible at a glance. Everything else (thinking, text, tool
results, usage/cost) is on the same timeline.

Usage:
    tools/trace-viewer.py SESSION.jsonl [more.jsonl ...] -o out.html
    tools/trace-viewer.py DIR -o out.html      # all *.jsonl in DIR, newest first
    ... --log reader.log                     # include the device app log

No dependencies. Open the output in any browser.
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
.pass  { background: #2f7d4f22; color: #2f7d4f; border: 1px solid #2f7d4f55; }
.drew  { background: #b0323222; color: #b03232; border: 1px solid #b0323255; }
.err   { background: #b0323233; color: #b03232; }
.usage { font-size: 11px; opacity: .55; }
pre { background: #f0ede6; border-radius: 6px; padding: 8px 10px;
      overflow-x: auto; font-size: 12px; }
details > summary { cursor: pointer; opacity: .7; font-size: 12.5px;
                    margin: 4px 0; }
.duo { display: flex; gap: 14px; flex-wrap: wrap; align-items: flex-start; }
.imgbox { position: relative; width: 420px; max-width: 100%;
          border: 1px solid #ccc; border-radius: 4px; background: #fff;
          flex: 0 0 auto; }
.imgbox img { display: block; width: 100%; }
.imgbox svg.ovl { position: absolute; inset: 0; width: 100%; height: 100%; }
.imgbox .cap { position: absolute; left: 6px; top: 6px; font-size: 10.5px;
               background: #000a; color: #fff; border-radius: 4px;
               padding: 1px 7px; }
svg.ovl * { stroke: #e02020 !important; stroke-width: 3px; }
svg.ovl [fill]:not([fill=\"none\"]):not([fill=\"transparent\"]) { fill: #e02020cc !important; }
svg.ovl text { fill: #e02020 !important; stroke: none !important;
               font-family: cursive, sans-serif; }
.thinking { font-style: italic; opacity: .75; white-space: pre-wrap; }
.text { white-space: pre-wrap; }
.pageimg { max-width: 420px; width: 100%; border: 1px solid #ccc;
           border-radius: 4px; background: #fff; }
.turnhdr { margin: 26px 0 4px; font-size: 13px; opacity: .6; }
hr.sess { margin: 40px 0; border: none; border-top: 3px double #bbb; }
"""

SVG_OPEN = re.compile(r"<svg\b[^>]*>", re.I | re.S)
VIEWBOX = re.compile(r'viewBox\s*=\s*["\']([^"\']+)["\']', re.I)


def esc(s):
    return html.escape(str(s), quote=True)


def svg_inner(src):
    """Return (viewBox, inner elements) of pi's SVG, tolerant of no wrapper."""
    vb = "0 0 1404 1872"
    m = SVG_OPEN.search(src)
    if m:
        vm = VIEWBOX.search(m.group(0))
        if vm:
            vb = vm.group(1)
        inner = src[m.end():]
        end = inner.rfind("</svg>")
        if end >= 0:
            inner = inner[:end]
    else:
        inner = src
    return vb, inner


def overlay_html(page_img_datauri, svg_src, cap_left, cap_right):
    vb, inner = svg_inner(svg_src)
    left = f"""
    <div class="imgbox"><img src="{page_img_datauri}"><span class="cap">{esc(cap_left)}</span></div>
    """ if page_img_datauri else ""
    right = f"""
    <div class="imgbox">
      {f'<img src="{page_img_datauri}">' if page_img_datauri
       else f'<svg viewBox="{esc(vb)}" style="display:block;width:100%"></svg>'}
      <svg class="ovl" viewBox="{esc(vb)}" preserveAspectRatio="none"><g fill="none">{inner}</g></svg>
      <span class="cap">{esc(cap_right)}</span>
    </div>"""
    return f'<div class="duo">{left}{right}</div>'


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

    last_page_img = None   # data URI of the most recent page image pi was shown
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
            out.append(f"<div class='turnhdr' id='{anchor}'>— pause #{turn} · {ts} —</div>")
            out.append("<div class='card user'><div class='role'>page → pi</div>")
            for b in content_blocks(msg.get("content")):
                if b.get("type") == "text":
                    out.append(f"<div class='text'>{esc(b['text'])}</div>")
                elif b.get("type") == "image":
                    uri = f"data:{b.get('mimeType', 'image/png')};base64,{b.get('data', '')}"
                    last_page_img = uri
                    out.append(f"<img class='pageimg' src='{uri}'>")
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
                    txt = b.get("text", "")
                    if txt.strip().lower() == "pass":
                        out.append("<span class='badge pass'>PASS — stayed silent</span>")
                    elif txt.strip():
                        out.append(f"<div class='text'>{esc(txt)}</div>")
                elif t == "toolCall":
                    name = b.get("name", "?")
                    args = b.get("arguments") or {}
                    if name == "reader_draw":
                        out.append("<span class='badge drew'>DREW on page "
                                   f"{esc(args.get('page', '(current)'))}</span>")
                        out.append(overlay_html(last_page_img, args.get("svg", ""),
                                                "what pi saw", "where pi drew (red)"))
                        out.append(f"<details><summary>svg source</summary>"
                                   f"<pre>{esc(args.get('svg', ''))}</pre></details>")
                    elif name == "reader_erase":
                        out.append(f"<span class='badge drew'>ERASE patch #{esc(args.get('id'))}"
                                   f" page {esc(args.get('page', '(current)'))}</span>")
                    else:
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
                    last_page_img = uri  # a view result is what pi now sees
                    out.append(f"<img class='pageimg' src='{uri}'>")
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
        if "sent to pi" in ln or "patch #" in ln:
            cls = " style='font-weight:600'"
        elif any(k in ln for k in ("error", "failed", "Died", "exited", "note:")):
            cls = " style='color:#b03232'"
        marked.append(f"<span{cls}>{esc(ln)}</span>")
    out.append(
        "<div class='card meta'><div class='role'>device log — /tmp/reader.log "
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

    doc = (f"<!doctype html><meta charset='utf-8'><title>reader · pi traces</title>"
           f"<style>{CSS}</style>"
           f"<div class='nav'>{''.join(all_nav)}</div>"
           f"<div class='wrap'>{''.join(body)}</div>")
    with open(out_path, "w") as f:
        f.write(doc)
    sz = os.path.getsize(out_path)
    print(f"trace-viewer: wrote {out_path} ({sz / 1e6:.1f} MB, {len(files)} session(s))")


if __name__ == "__main__":
    main()
