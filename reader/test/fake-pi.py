#!/usr/bin/env python3
"""A stand-in for `pi --mode rpc`, for `make preview` (no tablet, no API).

Speaks just enough of pi's JSONL protocol AND exercises the reader's tool
socket the way the real extension would: on the first page it receives, it
reads the book text, underlines a printed phrase, writes a margin note,
inserts a note page and writes a longer note there (with math), then views
the page — covering page_text, underline, draw (current + other page),
insert_note, view, and the animation path. Later pages get a `pass`."""
import json
import os
import socket
import sys
import time

MARGIN_SVG = """<svg viewBox="0 0 1404 1872">
  <text x="1030" y="300" font-size="30">key idea:</text>
  <text x="1030" y="352" font-size="30">no backlight,</text>
  <text x="1030" y="404" font-size="30">just mirrors</text>
  <text x="1030" y="470" font-size="28">* see note -&gt;</text>
</svg>"""

NOTE_SVG = """<svg viewBox="0 0 1404 1872">
  <text x="120" y="180" font-size="52" font-family="serif">Why e-ink reads like paper</text>
  <line x1="120" y1="215" x2="1280" y2="215" stroke="black"/>
  <text x="120" y="330" font-size="42">Reflectance, not emission: the page is lit by the room.</text>
  <text x="120" y="410" font-size="42">Contrast comes from pigment, roughly R_w/R_b \\approx 10.</text>
  <text x="120" y="490" font-size="42">Update energy E \\propto \\Delta V^{2} - static images are free.</text>
  <path d="M 120 600 C 320 540, 520 660, 720 600 S 1020 660, 1220 600" fill="none" stroke="black"/>
  <text x="120" y="700" font-size="36" font-family="script">pi</text>
</svg>"""


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def tool_call(cmd):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(os.environ["READER_SOCK"])
    s.sendall((json.dumps(cmd) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        d = s.recv(1 << 20)
        if not d:
            break
        buf += d
    s.close()
    return json.loads(buf.split(b"\n", 1)[0].decode())


responded = False
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        cmd = json.loads(line)
    except ValueError:
        continue
    if cmd.get("type") != "prompt":
        emit({"type": "response", "command": cmd.get("type"), "success": True})
        continue

    emit({"type": "response", "command": "prompt", "success": True})
    emit({"type": "agent_start"})
    time.sleep(1.0)  # "thinking" window: the harness can catch the dot

    if not responded:
        responded = True
        emit({"type": "tool_execution_start", "toolName": "reader_page_text", "args": {}})
        t = tool_call({"cmd": "page_text", "from": 1, "to": 2})
        print(f"fake-pi: page_text -> ok={t.get('ok')} {len(t.get('text', ''))} chars", file=sys.stderr)

        emit({"type": "tool_execution_start", "toolName": "reader_underline",
              "args": {"phrase": "reflect ambient light"}})
        u = tool_call({"cmd": "underline", "phrase": "reflect ambient light"})
        print(f"fake-pi: underline -> {u}", file=sys.stderr)

        emit({"type": "tool_execution_start", "toolName": "reader_draw", "args": {}})
        m = tool_call({"cmd": "draw", "svg": MARGIN_SVG})
        print(f"fake-pi: margin draw -> ok={m.get('ok')} id={m.get('id')} notes={m.get('notes')}",
              file=sys.stderr)

        emit({"type": "tool_execution_start", "toolName": "reader_insert_note", "args": {}})
        n = tool_call({"cmd": "insert_note"})
        print(f"fake-pi: insert_note -> {n}", file=sys.stderr)

        note_page = n.get("page")
        emit({"type": "tool_execution_start", "toolName": "reader_draw",
              "args": {"page": note_page}})
        d = tool_call({"cmd": "draw", "svg": NOTE_SVG, "page": note_page})
        print(f"fake-pi: note draw -> ok={d.get('ok')} id={d.get('id')} notes={d.get('notes')}",
              file=sys.stderr)

        v = tool_call({"cmd": "view"})
        print(
            f"fake-pi: view -> page {v.get('page')}/{v.get('page_count')} ({v.get('label')}), "
            f"{len(v.get('patches', []))} patches, png {len(v.get('png_base64', ''))}b64",
            file=sys.stderr,
        )
        # a bad underline must fail cleanly, not draw garbage
        bad = tool_call({"cmd": "underline", "phrase": "no such words here at all"})
        print(f"fake-pi: bad underline -> {bad}", file=sys.stderr)
    else:
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": "pass"}})

    emit({"type": "agent_end", "messages": []})
