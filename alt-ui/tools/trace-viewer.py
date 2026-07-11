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
import datetime
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
    last_page_no = None    # which page that image shows (from "page N of M" text)
    blank_pages = set()    # pages created by canvas_insert_note, not yet drawn on
    turn = 0
    nav = []
    pause_at = None        # datetime of the current pause (user msg) for latency
    page_re = re.compile(r"page (\d+) of \d+")

    def parse_ts(e):
        try:
            return datetime.datetime.fromisoformat((e.get("timestamp") or "").replace("Z", "+00:00"))
        except ValueError:
            return None

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
            pause_at = parse_ts(e)
            anchor = f"t{turn}"
            nav.append(f"<a href='#{anchor}'>#{turn} {ts}</a>")
            out.append(f"<div class='turnhdr' id='{anchor}'>— pause #{turn} · {ts} —</div>")
            out.append("<div class='card user'><div class='role'>page → pi</div>")
            for b in content_blocks(msg.get("content")):
                if b.get("type") == "text":
                    pm = page_re.search(b.get("text", ""))
                    if pm:
                        last_page_no = int(pm.group(1))
                    out.append(f"<div class='text'>{esc(b['text'])}</div>")
                elif b.get("type") == "image":
                    uri = f"data:{b.get('mimeType', 'image/png')};base64,{b.get('data', '')}"
                    last_page_img = uri
                    out.append(f"<img class='pageimg' src='{uri}'>")
            out.append("</div>")

        elif role == "assistant":
            lat = ""
            t = parse_ts(e)
            if pause_at and t:
                lat = f"<b>+{(t - pause_at).total_seconds():.1f}s</b> · "
            out.append(f"<div class='card asst'><div class='role'>pi · {lat}{ts} {fmt_usage(msg)}</div>")
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
                    if name in ("reader_draw", "canvas_draw"):
                        out.append("<span class='badge drew'>DREW on page "
                                   f"{esc(args.get('page', '(current)'))}</span>")
                        # only composite over the page image if the draw targets
                        # the page that image actually shows — a draw on a freshly
                        # inserted note page renders on blank instead
                        tgt = args.get("page")
                        bg, cap = last_page_img, "where pi drew (red)"
                        if tgt is not None and tgt in blank_pages:
                            bg, cap = None, f"drawn on fresh blank page {esc(tgt)}"
                            blank_pages.discard(tgt)
                        elif tgt is not None and last_page_no is not None and tgt != last_page_no:
                            bg, cap = None, f"drawn on page {esc(tgt)} (image above shows p.{last_page_no})"
                        out.append(overlay_html(bg, args.get("svg", ""),
                                                "what pi saw", cap))
                        out.append(f"<details><summary>svg source</summary>"
                                   f"<pre>{esc(args.get('svg', ''))}</pre></details>")
                    elif name in ("reader_erase", "canvas_erase"):
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
                    t = b["text"]
                    im = re.search(r"inserted as page (\d+)", t)
                    if im:
                        blank_pages.add(int(im.group(1)))
                    sm = re.search(r"Now showing page (\d+) of", t)
                    if sm:
                        last_page_no = int(sm.group(1))
                    out.append(f"<div class='text'>{esc(t[:3000])}</div>")
                elif b.get("type") == "image":
                    uri = f"data:{b.get('mimeType', 'image/png')};base64,{b.get('data', '')}"
                    last_page_img = uri  # a view result is what pi now sees
                    out.append(f"<img class='pageimg' src='{uri}'>")
            out.append("</div>")

        elif role == "custom":
            out.append(f"<div class='card meta'><div class='role'>"
                       f"{esc(msg.get('customType', 'custom'))}</div></div>")

    return nav


def render_metrics(path, out):
    """Per-turn table from paper-metrics.jsonl: latency, real tokens, payload."""
    try:
        recs = [json.loads(ln) for ln in open(path) if ln.strip()]
    except (OSError, ValueError):
        return
    turns = [r for r in recs if r.get("t") == "turn"]
    tools = [r for r in recs if r.get("t") == "tool"]
    if not turns and not tools:
        return

    rows = []
    tot_cost = tot_in = tot_out = 0
    for r in turns:
        lat = f"{r['latMs'] / 1000:.1f}s" if r.get("latMs") is not None else "?"
        cost = r.get("cost")
        tot_cost += cost or 0
        tot_in += (r.get("in") or 0) + (r.get("cacheR") or 0)
        tot_out += r.get("out") or 0
        rows.append(
            f"<tr><td>{esc((r.get('ts') or '')[11:19])}</td><td><b>{lat}</b></td>"
            f"<td>{r.get('in', 0):,}</td><td>{r.get('cacheR', 0):,}</td>"
            f"<td>{r.get('out', 0):,}</td>"
            f"<td>{(r.get('sentBytes') or 0) / 1e6:.1f}MB / {r.get('sentImgs', 0)}img</td>"
            f"<td>{'$%.4f' % cost if cost is not None else '—'}</td>"
            f"<td>{esc(r.get('stop', ''))}</td></tr>")

    tool_stats = {}
    for r in tools:
        s = tool_stats.setdefault(r.get("name", "?"), [0, 0, 0])  # n, total ms, max ms
        s[0] += 1
        if r.get("ms") is not None:
            s[1] += r["ms"]
            s[2] = max(s[2], r["ms"])
    tool_rows = "".join(
        f"<tr><td>{esc(n)}</td><td>{s[0]}</td><td>{s[1] / max(s[0], 1) / 1000:.1f}s</td>"
        f"<td>{s[2] / 1000:.1f}s</td></tr>"
        for n, s in sorted(tool_stats.items(), key=lambda kv: -kv[1][1]))

    out.append(
        "<div class='card meta'><div class='role'>device metrics — per pi turn</div>"
        f"<div class='usage'>{len(turns)} turns · {tot_in:,} tok in (incl. cache) · "
        f"{tot_out:,} tok out · ${tot_cost:.4f}</div>"
        "<table style='border-collapse:collapse;font-size:12px;width:100%'>"
        "<tr style='text-align:left;opacity:.6'><th>time</th><th>latency</th><th>in</th>"
        "<th>cache</th><th>out</th><th>payload sent</th><th>cost</th><th>stop</th></tr>"
        f"{''.join(rows)}</table>"
        + (f"<details><summary>tool timings</summary><table style='border-collapse:collapse;font-size:12px'>"
           f"<tr style='text-align:left;opacity:.6'><th>tool</th><th>calls</th><th>avg</th><th>max</th></tr>"
           f"{tool_rows}</table></details>" if tool_rows else "")
        + "</div>")


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
    metrics_path = None
    if "--metrics" in args:
        i = args.index("--metrics")
        metrics_path = args[i + 1]
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
    if metrics_path:
        render_metrics(metrics_path, body)
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
