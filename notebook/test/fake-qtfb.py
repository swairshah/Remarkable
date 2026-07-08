#!/usr/bin/env python3
"""Fake AppLoad/qtfb server for notebook's `make preview` (no tablet).

Plays the server side of the qtfb protocol (see src/qtfb.rs): backs the
framebuffer with /dev/shm, launches the app under qemu with a FAKE pi
(test/fake-pi.py) wired in via NOTEBOOK_BIN, scripts some handwriting,
waits for the pause trigger -> fake pi draws two patches over the tool
socket and erases one -> screenshots. Then flips to page 2 and back to
prove persistence + full re-render.

The container has no Wacom device, so the app falls back to AppLoad pen
events — which is exactly what we script here."""
import math
import os
import socket
import struct
import subprocess
import sys
import time
import zlib

W, H = 1404, 1872
SHM_KEY = 7

SAMPLE_MD = """---
title: "Scaling laws, distilled"
source: "https://example.com/post"
---

# Scaling laws, distilled

Training loss $L$ falls **predictably** with model size $N$, data $D$ and
compute $C$ — a power law, i.e. a straight line on a log-log plot (see
[Kaplan et al. 2020](https://arxiv.org/abs/2001.08361)).

::: aside
**Rule of thumb:** $C \\\\approx 6ND$ — the `6` covers forward + backward.
:::

Key regimes:

1. Noiseless data, unique solution: error goes as $D^{-1}$.
2. Noisy data: error goes as $D^{-1/2}$, learning is harder.
- Chinchilla: scale $N$ and $D$ together, about 20 tokens per parameter.

$$
L(N, D) = E + A N^{-a} + B D^{-b}
$$

```python
def loss(n, d):
    return E + A * n**-0.34 + B * d**-0.28
```

| Symbol | Meaning |
| --- | --- |
| N | parameters |
| D | tokens |

---

That is the whole idea.
"""
SHM_PATH = f"/dev/shm/qtfb_{SHM_KEY}"
SOCK_PATH = "/tmp/qtfb.sock"

PEN_PRESS, PEN_RELEASE, PEN_UPDATE = 0x20, 0x21, 0x22
TOUCH_PRESS, TOUCH_RELEASE, TOUCH_UPDATE = 0x10, 0x11, 0x12
MESSAGE_USERINPUT = 4


def write_png(path):
    raw = memoryview(open(SHM_PATH, "rb").read()).cast("H")
    gray = bytes(((v >> 5) & 0x3F) * 255 // 63 for v in raw)

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    ihdr = struct.pack(">IIBBBBB", W, H, 8, 0, 0, 0, 0)
    rows = b"".join(b"\x00" + gray[y * W:(y + 1) * W] for y in range(H))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) +
                chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
    print(f"fake-qtfb: wrote {path}")


def main():
    app_bin, out_png = sys.argv[1], sys.argv[2]

    with open(SHM_PATH, "wb") as f:
        f.truncate(W * H * 2)
    if os.path.exists(SOCK_PATH):
        os.remove(SOCK_PATH)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    srv.bind(SOCK_PATH)
    srv.listen(1)

    here = os.path.dirname(os.path.abspath(__file__))
    os.makedirs("/tmp/nb-lib", exist_ok=True)
    with open("/tmp/nb-lib/scaling-laws-notes.md", "w") as f:
        f.write(SAMPLE_MD)
    env = dict(os.environ,
               QTFB_KEY="12345",
               NOTEBOOK_BIN=os.path.join(here, "fake-pi.py"),
               NOTEBOOK_DATA_DIR="/tmp/nb-pages",
               NOTEBOOK_LIBRARY="/tmp/nb-lib",
               NOTEBOOK_SOCK="/tmp/nb.sock",
               HOME="/tmp")
    app = subprocess.Popen(["qemu-arm-static", app_bin], env=env)

    conn, _ = srv.accept()
    init = conn.recv(64)
    assert init[0] == 0, init[0]
    conn.send(struct.pack("<B3xiI12x", 0, SHM_KEY, W * H * 2))

    def pen(itype, x, y, d=0):
        conn.send(struct.pack("<B3xiiiii", MESSAGE_USERINPUT, itype, 0, x, y, d))

    def touch(itype, x, y):
        conn.send(struct.pack("<B3xiiiii", MESSAGE_USERINPUT, itype, 0, x, y, 0))

    def drain(seconds):
        conn.settimeout(seconds)
        end = time.time() + seconds
        try:
            while time.time() < end:
                conn.recv(64)
        except (socket.timeout, OSError):
            pass

    time.sleep(1.5)  # first paint done, pi spawned

    # handwrite a sine-ish squiggle mid-page (this is "the user's writing")
    pen(PEN_PRESS, 160, 420)
    for i in range(1, 80):
        pen(PEN_UPDATE, 160 + i * 9, 420 + int(80 * math.sin(i / 5)))
    pen(PEN_RELEASE, 160 + 80 * 9, 420)

    # pause: idle trigger (2.8s) -> fake pi thinks (1s), draws two patches,
    # views, erases the circle mid-animation; let the note finish animating
    drain(4.2)
    write_png(out_png.replace(".png", "-thinking.png"))  # the working dot
    drain(6.0)
    write_png(out_png)  # user ink + the AI note (circle erased again)

    # flip forward: a fresh page 2 (proves the flip gesture + new-page path)
    time.sleep(1.7)  # let palm rejection lapse
    touch(TOUCH_PRESS, 1150, 900)
    for i in range(1, 12):
        touch(TOUCH_UPDATE, 1150 - i * 80, 900)
    touch(TOUCH_RELEASE, 190, 900)
    drain(2.0)
    write_png(out_png.replace(".png", "-page2.png"))

    # flip back: page 1 re-rendered entirely from the saved vector model
    touch(TOUCH_PRESS, 190, 900)
    for i in range(1, 12):
        touch(TOUCH_UPDATE, 190 + i * 80, 900)
    touch(TOUCH_RELEASE, 1150, 900)
    drain(2.0)
    write_png(out_png.replace(".png", "-back.png"))

    # sidebar -> LIBRARY -> first item: the markdown reader
    pen(PEN_PRESS, 40, 40)          # corner tap: sidebar
    pen(PEN_RELEASE, 40, 40)
    drain(1.0)
    pen(PEN_PRESS, 180, 130 + 5 * 68 + 30)   # LIBRARY row
    pen(PEN_RELEASE, 180, 130 + 5 * 68 + 30)
    drain(1.5)
    pen(PEN_PRESS, 400, 160)        # first item
    pen(PEN_RELEASE, 400, 160)
    drain(2.0)
    write_png(out_png.replace(".png", "-library.png"))

    app.terminate()
    try:
        app.wait(timeout=3)
    except subprocess.TimeoutExpired:
        app.kill()


if __name__ == "__main__":
    main()
