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
<svg viewBox="0 0 260 90">
  <rect x="10" y="25" width="90" height="40" fill="none" stroke="black"/>
  <text x="55" y="50" text-anchor="middle" font-size="13">digitizer</text>
  <line x1="100" y1="45" x2="150" y2="45" stroke="black"/>
  <polygon points="150,38 168,45 150,52" fill="black"/>
  <rect x="168" y="25" width="82" height="40" fill="none" stroke="black"/>
  <text x="209" y="50" text-anchor="middle" font-size="13">screen</text>
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
