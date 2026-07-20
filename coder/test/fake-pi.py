#!/usr/bin/env python3
"""A stand-in for `pi --mode rpc`, for `make preview` (no tablet, no API).

Speaks just enough of pi's JSONL protocol AND exercises coder's tool
socket the way the real extension would. On the first page it receives
(the notes pad, where the user scribbled a clone request) it plays a
whole coder session:

  coder_projects            -> list the sidebar
  register a project        -> meta.json + SUMMARY.md via plain file IO
                               (the real pi does this with shell tools)
  coder_goto {project}      -> flip the tablet to it
  coder_draw (overview)     -> page 1: title, summary lines, architecture
                               boxes + arrows
  coder_draw (page 2)       -> appends a subsystem detail page
  coder_view                -> read back page 1
  coder_erase               -> remove a scratch patch (exercises erase)

Later pages get a `pass`."""
import json
import os
import socket
import sys
import time

DATA = os.environ.get("CODER_DATA_DIR", "/tmp/coder-data")

OVERVIEW_SVG = """<svg viewBox="0 0 1404 1872">
  <text x="90" y="150" font-size="64" font-family="serif">micrograd</text>
  <line x1="90" y1="180" x2="560" y2="180" stroke="black"/>
  <text x="90" y="250" font-size="38">tiny scalar autograd engine + neural net library</text>
  <text x="90" y="310" font-size="38">python, ~250 lines, no dependencies</text>
  <text x="90" y="370" font-size="38">entry: engine.py (Value), nn.py (MLP)</text>

  <rect x="180" y="520" width="380" height="170" fill="none" stroke="black"/>
  <text x="370" y="590" font-size="40" text-anchor="middle">engine.py</text>
  <text x="370" y="650" font-size="30" text-anchor="middle" font-family="sans">Value: data, grad</text>

  <rect x="820" y="520" width="380" height="170" fill="none" stroke="black"/>
  <text x="1010" y="590" font-size="40" text-anchor="middle">nn.py</text>
  <text x="1010" y="650" font-size="30" text-anchor="middle" font-family="sans">Neuron - Layer - MLP</text>

  <line x1="820" y1="605" x2="580" y2="605" stroke="black"/>
  <polygon points="568,605 588,595 588,615" fill="black"/>
  <text x="620" y="580" font-size="26" font-family="sans">builds on</text>

  <rect x="500" y="880" width="380" height="150" fill="none" stroke="black"/>
  <text x="690" y="945" font-size="40" text-anchor="middle">backward()</text>
  <text x="690" y="1000" font-size="28" text-anchor="middle" font-family="sans">topo sort + chain rule</text>
  <line x1="370" y1="690" x2="620" y2="880" stroke="black"/>
  <polygon points="628,890 610,878 622,866" fill="black"/>

  <text x="90" y="1180" font-size="34" font-family="script">ask me about any box - or sketch a change</text>
</svg>"""

DETAIL_SVG = """<svg viewBox="0 0 1404 1872">
  <text x="90" y="140" font-size="52" font-family="serif">Value: one scalar node</text>
  <rect x="120" y="240" width="500" height="300" fill="none" stroke="black"/>
  <text x="370" y="300" font-size="36" text-anchor="middle">Value</text>
  <line x1="140" y1="330" x2="600" y2="330" stroke="black"/>
  <text x="150" y="380" font-size="30" font-family="sans">data: float</text>
  <text x="150" y="430" font-size="30" font-family="sans">grad: float = 0</text>
  <text x="150" y="480" font-size="30" font-family="sans">_backward: closure</text>

  <rect x="820" y="240" width="440" height="200" fill="none" stroke="black"/>
  <text x="1040" y="310" font-size="34" text-anchor="middle">_prev: set(Value)</text>
  <text x="1040" y="370" font-size="28" text-anchor="middle" font-family="sans">the DAG edges</text>
  <line x1="620" y1="340" x2="820" y2="340" stroke="black"/>
  <polygon points="832,340 812,330 812,350" fill="black"/>

  <text x="120" y="680" font-size="36">backward pass:</text>
  <text x="160" y="750" font-size="32" font-family="sans">1. topo-sort the DAG from the loss</text>
  <text x="160" y="810" font-size="32" font-family="sans">2. loss.grad = 1</text>
  <text x="160" y="870" font-size="32" font-family="sans">3. walk reversed, call _backward each</text>
  <path d="M 130 920 C 300 980, 900 980, 1150 920" fill="none" stroke="black"/>
  <polygon points="1160,915 1140,912 1146,930" fill="black"/>
</svg>"""

SCRATCH_SVG = """<svg viewBox="0 0 1404 1872">
  <text x="90" y="1400" font-size="30" font-family="sans">scratch patch (will be erased)</text>
</svg>"""

# what the OPEN EVENT gets: a compact overview drawn on the current page
OPEN_SVG = """<svg viewBox="0 0 1404 1872">
  <text x="90" y="150" font-size="60" font-family="serif">emptyrepo</text>
  <line x1="90" y1="180" x2="540" y2="180" stroke="black"/>
  <text x="90" y="250" font-size="36">a tiny demo repo - one lib, one cli</text>
  <rect x="220" y="420" width="360" height="150" fill="none" stroke="black"/>
  <text x="400" y="505" font-size="38" text-anchor="middle">lib/</text>
  <rect x="820" y="420" width="360" height="150" fill="none" stroke="black"/>
  <text x="1000" y="505" font-size="38" text-anchor="middle">cli/</text>
  <line x1="820" y1="495" x2="600" y2="495" stroke="black"/>
  <polygon points="588,495 608,485 608,505" fill="black"/>
</svg>"""

SUMMARY_MD = """# micrograd

A tiny scalar-valued autograd engine and a neural net library on top of it.

## Layout

- `engine.py` - the `Value` class: data, grad, operator overloads, `backward()`
- `nn.py` - `Neuron`, `Layer`, `MLP` built from `Value` scalars
- `test/` - sanity checks against pytorch

## Notes

Backward pass is a topological sort of the computation DAG followed by
chain-rule closures in reverse order. About 250 lines total.
"""


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def tool_call(cmd):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(os.environ["CODER_SOCK"])
    s.sendall((json.dumps(cmd) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        d = s.recv(1 << 20)
        if not d:
            break
        buf += d
    s.close()
    return json.loads(buf.split(b"\n", 1)[0].decode())


def register_project():
    """What the real pi does with shell tools after `git clone` on the VM."""
    d = os.path.join(DATA, "projects", "micrograd")
    os.makedirs(os.path.join(d, "pages"), exist_ok=True)
    with open(os.path.join(d, "meta.json"), "w") as f:
        json.dump({
            "name": "micrograd",
            "url": "https://github.com/karpathy/micrograd",
            "branch": "master",
            "summary": "tiny autograd engine",
        }, f)
    with open(os.path.join(d, "SUMMARY.md"), "w") as f:
        f.write(SUMMARY_MD)


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

    if "OPEN" in json.dumps(cmd):
        # the open event: "read the repo" for a while FIRST — long enough
        # for the scenario to flip the screen to another project — then
        # draw with no project param: the app must route the ink to the
        # TURN's project (the race that once painted pi's diagram on tau)
        time.sleep(4.0)
        emit({"type": "tool_execution_start", "toolName": "coder_draw", "args": {}})
        r = tool_call({"cmd": "draw", "svg": OPEN_SVG})
        print(f"fake-pi: open-event overview -> ok={r.get('ok')} id={r.get('id')} "
              f"project={r.get('project')} note={r.get('note')}", file=sys.stderr)
        emit({"type": "agent_end", "messages": []})
        continue

    if not responded:
        responded = True
        emit({"type": "tool_execution_start", "toolName": "coder_projects", "args": {}})
        r = tool_call({"cmd": "projects"})
        print(f"fake-pi: projects -> {r}", file=sys.stderr)

        # "git clone on the VM" happened here; register the project
        register_project()

        emit({"type": "tool_execution_start", "toolName": "coder_goto",
              "args": {"project": "micrograd"}})
        r = tool_call({"cmd": "goto", "project": "micrograd"})
        print(f"fake-pi: goto -> {r}", file=sys.stderr)

        emit({"type": "tool_execution_start", "toolName": "coder_draw", "args": {}})
        r = tool_call({"cmd": "draw", "svg": OVERVIEW_SVG})
        print(f"fake-pi: overview -> ok={r.get('ok')} id={r.get('id')}", file=sys.stderr)

        r = tool_call({"cmd": "draw", "svg": DETAIL_SVG, "page": 2})
        print(f"fake-pi: detail p2 -> ok={r.get('ok')} appended={r.get('appended')} "
              f"count={r.get('page_count')}", file=sys.stderr)

        r = tool_call({"cmd": "draw", "svg": SCRATCH_SVG})
        scratch = r.get("id")
        v = tool_call({"cmd": "view"})
        print(
            f"fake-pi: view -> page {v.get('page')}/{v.get('page_count')}, "
            f"{len(v.get('patches', []))} patches, png {len(v.get('png_base64', ''))}b64",
            file=sys.stderr,
        )
        time.sleep(2.0)  # let the overview animation run a beat
        emit({"type": "tool_execution_start", "toolName": "coder_erase",
              "args": {"id": scratch}})
        r = tool_call({"cmd": "erase", "id": scratch})
        print(f"fake-pi: erase scratch -> {r}", file=sys.stderr)
    else:
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": "pass"}})

    emit({"type": "agent_end", "messages": []})
