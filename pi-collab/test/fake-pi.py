#!/usr/bin/env python3
"""A stand-in for `pi --mode rpc`, for `make preview` (no API key needed).

Speaks just enough of pi's JSONL protocol for pi-collab to exercise its
whole pipeline: it reads prompt commands on stdin and, for each, streams
back agent_start -> a few text_delta chunks -> agent_end, exactly as pi
would. It ignores the attached image (a real pi would read it)."""
import json
import sys
import time

REPLY = """here is how drain_pen works.

# the pipeline
it reads evdev frames from the wacom digitizer and maps them to the screen.

- reads ABS_X / ABS_Y / pressure
- tracks the tip's touch state
- emits one frame per EV_SYN

```rust
fn map(wx: i32, wy: i32) -> (i32, i32) {
    let sx = wy * FB_W / WACOM_Y_MAX;
    let sy = FB_H - wx * FB_H / WACOM_X_MAX;
    (sx, sy)
}
```

and a quick sketch of the coordinate boxes:

```svg
<svg viewBox="0 0 200 120">
  <rect x="10" y="10" width="80" height="60" fill="none" stroke="black"/>
  <line x1="90" y1="40" x2="150" y2="40" stroke="black"/>
  <polygon points="150,30 170,40 150,50" fill="black"/>
  <circle cx="40" cy="90" r="15" fill="none" stroke="black"/>
</svg>
```

want me to walk through the transform in detail?"""


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        cmd = json.loads(line)
    except ValueError:
        continue
    if cmd.get("type") != "prompt":
        # acknowledge anything else so the client isn't left waiting
        emit({"type": "response", "command": cmd.get("type"), "success": True})
        continue

    emit({"type": "response", "command": "prompt", "success": True})
    emit({"type": "agent_start"})
    # a tool notice, to exercise the Note rendering path
    emit({"type": "tool_execution_start", "toolName": "read",
          "args": {"path": "src/pen.rs"}})
    time.sleep(1.5)  # "thinking" window, so the harness can catch the dot
    # stream the reply in fixed-size character chunks, preserving exact text
    # (whitespace/newlines matter for the code + svg blocks)
    for i in range(0, len(REPLY), 14):
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": REPLY[i:i + 14]}})
        time.sleep(0.03)
    emit({"type": "agent_end", "messages": []})
