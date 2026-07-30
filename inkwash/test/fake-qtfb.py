#!/usr/bin/env python3
"""A fake AppLoad/qtfb server, so inkwash can be exercised without a tablet.

Usage (inside the linux container `make preview` builds):

    python3 test/fake-qtfb.py build/inkwash build/preview

It plays the server side of the protocol in src/qtfb.h: listens on
/tmp/qtfb.sock, backs the framebuffer with a file in /dev/shm, launches the
app under qemu-arm-static, then scripts a painting session: pen linework,
water strokes across it (as touch — the finger IS the water brush), a few
seconds for the ink to bleed, then FIX and EXIT. Three PNGs come out:

    <prefix>-1-lines.png   crisp pen linework
    <prefix>-2-wet.png     water down, ink bleeding (stippled live render)
    <prefix>-3-fixed.png   after FIX: the wash dried into 16-level gray
"""
import math
import os
import socket
import struct
import subprocess
import sys
import time
import zlib

W, H = 1404, 1872
SHM_KEY = 7  # arbitrary; the app just formats it into "/qtfb_<key>"
SHM_PATH = f"/dev/shm/qtfb_{SHM_KEY}"
SOCK_PATH = "/tmp/qtfb.sock"

# input types, mirroring qtfb.h
TOUCH_PRESS, TOUCH_RELEASE, TOUCH_UPDATE = 0x10, 0x11, 0x12
PEN_PRESS, PEN_RELEASE, PEN_UPDATE = 0x20, 0x21, 0x22
MESSAGE_USERINPUT = 4

# button centers, mirroring the layout constants in src/main.c
FIX_XY = (860 + 75, 26 + 44)
EXIT_XY = (1230 + 75, 26 + 44)


def write_png(path, gray):
    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))
    ihdr = struct.pack(">IIBBBBB", W, H, 8, 0, 0, 0, 0)  # 8-bit grayscale
    rows = b"".join(b"\x00" + gray[y * W:(y + 1) * W] for y in range(H))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) +
                chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
    print(f"fake-qtfb: wrote {path}")


def snap():
    raw = memoryview(open(SHM_PATH, "rb").read()).cast("H")
    return bytes(((v >> 5) & 0x3F) * 255 // 63 for v in raw)


def main():
    app_bin, prefix = sys.argv[1], sys.argv[2]

    with open(SHM_PATH, "wb") as f:
        f.truncate(W * H * 2)  # RGB565
    if os.path.exists(SOCK_PATH):
        os.remove(SOCK_PATH)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    srv.bind(SOCK_PATH)
    srv.listen(1)

    env = dict(os.environ, QTFB_KEY="12345")
    app = subprocess.Popen(["qemu-arm-static", app_bin], env=env)

    conn, _ = srv.accept()
    init = conn.recv(64)
    key, fbtype = struct.unpack_from("<iB", init, 4)
    assert init[0] == 0 and key == 12345, (init[0], key)
    print(f"fake-qtfb: app connected, key={key} fbFormat={fbtype}")
    # init response: u8 type=0, pad, i32 shmKey, u32 shmSize (size_t on arm32)
    conn.send(struct.pack("<B3xiI12x", 0, SHM_KEY, W * H * 2))
    conn.setblocking(False)

    def send_input(itype, dev, x, y, d=0):
        conn.send(struct.pack("<B3xiiiii", MESSAGE_USERINPUT, itype, dev, x, y, d))

    def drain(seconds):
        """Sleep while draining the app's update messages (the sim keeps
        ticking and flushing regions the whole time)."""
        end = time.time() + seconds
        while time.time() < end:
            try:
                while conn.recv(64):
                    pass
            except BlockingIOError:
                time.sleep(0.02)
            except (ConnectionResetError, OSError):
                return

    drain(1.0)  # let the app draw its scene

    # pen: an apple-ish closed curve plus a stem, center canvas
    cx, cy, r = 700, 950, 260
    send_input(PEN_PRESS, 0, cx + r, cy, d=2600)
    for i in range(1, 80):
        a = i / 79 * 2 * math.pi
        rr = r * (1 + 0.08 * math.sin(3 * a))
        send_input(PEN_UPDATE, 0, int(cx + rr * math.cos(a)),
                   int(cy + rr * math.sin(a)), d=2600)
        if i % 10 == 0:
            drain(0.03)
    send_input(PEN_RELEASE, 0, cx + r, cy)
    drain(0.2)
    send_input(PEN_PRESS, 0, cx, cy - r, d=1800)
    for i in range(1, 20):
        send_input(PEN_UPDATE, 0, cx + i * 2, cy - r - i * 6, d=1800)
    send_input(PEN_RELEASE, 0, cx + 40, cy - r - 120)

    drain(1.8)  # palm rejection: touch is ignored for 1.5s after pen use
    write_png(prefix + "-1-lines.png", snap())

    # finger: water strokes across the right half of the shape
    for sweep in range(3):
        y = cy - 160 + sweep * 160
        send_input(TOUCH_PRESS, 0, cx - 60, y)
        for i in range(1, 25):
            send_input(TOUCH_UPDATE, 0, cx - 60 + i * 22,
                       y + int(20 * math.sin(i / 3)))
            if i % 5 == 0:
                drain(0.05)
        send_input(TOUCH_RELEASE, 0, cx - 60 + 24 * 22, y)
        drain(0.3)

    drain(4.0)  # let the ink bleed and flow
    write_png(prefix + "-2-wet.png", snap())

    # FIX (with the pen: touch is still palm-rejected near pen activity)
    send_input(PEN_PRESS, 0, *FIX_XY)
    send_input(PEN_RELEASE, 0, *FIX_XY)
    drain(1.5)
    write_png(prefix + "-3-fixed.png", snap())

    send_input(PEN_PRESS, 0, *EXIT_XY)
    send_input(PEN_RELEASE, 0, *EXIT_XY)
    drain(1.0)
    rc = app.wait(timeout=5)
    print(f"fake-qtfb: app exited rc={rc}")
    assert rc == 0, f"app exited with rc={rc}"


if __name__ == "__main__":
    main()
