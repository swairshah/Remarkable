#!/usr/bin/env python3
"""A stand-in for `pi --mode rpc`, for `make preview` (no API key needed).

Speaks just enough of pi's JSONL protocol for pi-collab to exercise its
whole pipeline: it reads prompt commands on stdin and, for each, streams
back agent_start -> a few text_delta chunks -> agent_end, exactly as pi
would. It ignores the attached image (a real pi would read it)."""
import json
import sys
import time

REPLY = (
    "i can see your handwriting. you asked how drain_pen works: it reads "
    "evdev frames from the wacom digitizer, tracks the tip's touch state, "
    "and maps raw coordinates onto the screen. want me to walk through the "
    "transform?"
)


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
    # stream the reply a few words at a time
    words = REPLY.split(" ")
    for i in range(0, len(words), 3):
        chunk = " ".join(words[i:i + 3]) + " "
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": chunk}})
        time.sleep(0.05)
    emit({"type": "agent_end", "messages": []})
