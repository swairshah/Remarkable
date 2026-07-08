#!/usr/bin/env python3
"""A stand-in for `pi --mode rpc`, for `make preview` (no tablet, no API).

Speaks just enough of pi's JSONL protocol AND exercises the notebook's tool
socket the way the real extension would: on the first page it receives, it
draws two patches over NOTEBOOK_SOCK (a circle, then a text+curve+arrow
annotation), views the page, then erases the first patch — covering draw,
animate, view, erase, and the erase-while-animating path. Later pages get
a `pass`."""
import json
import os
import socket
import sys
import time

CIRCLE_SVG = """<svg viewBox="0 0 1404 1872">
  <circle cx="480" cy="430" r="90" fill="none" stroke="black" stroke-width="3"/>
</svg>"""

NOTE_SVG = """<svg viewBox="0 0 1404 1872">
  <text x="150" y="1000" font-size="46" font-family="script">looks like a sine wave</text>
  <path d="M 150 1080 C 250 1020, 350 1140, 450 1080 S 650 1140, 750 1080" fill="none" stroke="black"/>
  <line x1="480" y1="960" x2="420" y2="560" stroke="black"/>
  <polygon points="412,548 432,560 416,574" fill="black"/>
  <rect x="880" y="920" width="360" height="220" fill="none" stroke="black"/>
  <text x="1060" y="1000" font-size="30" text-anchor="middle">f(x) = sin x</text>
  <text x="1060" y="1060" font-size="30" text-anchor="middle" font-family="sans">period 2 pi</text>
  <text x="700" y="1400" font-size="40">this long sentence would have sailed straight off the right edge of the panel without the auto-wrap</text>
  <text x="150" y="1660" font-size="42">loss \\approx E + a\\cdot N^{-\\alpha} + b\\cdot D^{-\\beta}</text>
</svg>"""


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def tool_call(cmd):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(os.environ["NOTEBOOK_SOCK"])
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
        emit({"type": "tool_execution_start", "toolName": "notebook_draw", "args": {}})
        a = tool_call({"cmd": "draw", "svg": CIRCLE_SVG})
        print(f"fake-pi: draw A -> {a}", file=sys.stderr)
        b = tool_call({"cmd": "draw", "svg": NOTE_SVG})
        print(f"fake-pi: draw B -> {b}", file=sys.stderr)
        v = tool_call({"cmd": "view"})
        print(
            f"fake-pi: view -> page {v.get('page')}/{v.get('page_count')}, "
            f"{len(v.get('patches', []))} patches, png {len(v.get('png_base64', ''))}b64",
            file=sys.stderr,
        )
        emit({"type": "tool_execution_start", "toolName": "notebook_erase",
              "args": {"id": a.get("id")}})
        e = tool_call({"cmd": "erase", "id": a.get("id")})
        print(f"fake-pi: erase A -> {e}", file=sys.stderr)
    else:
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": "pass"}})

    emit({"type": "agent_end", "messages": []})
