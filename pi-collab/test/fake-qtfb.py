#!/usr/bin/env python3
"""Fake AppLoad/qtfb server for pi-collab's `make preview` (no tablet).

Plays the server side of the qtfb protocol (see src/qtfb.rs): backs the
framebuffer with /dev/shm, launches the app under qemu with a FAKE pi
(test/fake-pi.py) wired in via PI_COLLAB_BIN, scripts some handwriting +
a SEND tap, lets pi's streamed reply render, then screenshots the
framebuffer to a grayscale PNG.

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
SHM_PATH = f"/dev/shm/qtfb_{SHM_KEY}"
SOCK_PATH = "/tmp/qtfb.sock"

PEN_PRESS, PEN_RELEASE, PEN_UPDATE = 0x20, 0x21, 0x22
MESSAGE_USERINPUT = 4

# input-strip + header geometry mirrored from src/main.rs
INPUT_Y0 = H - 460
SEND_X, SEND_W, BTN_Y, BTN_H = W - 24 - 220, 220, INPUT_Y0 + 8, 60
CANVAS_Y0 = INPUT_Y0 + 76
FUP_X, HB_Y, HB_H = 130 + 66, 20, 52  # the A+ header button (FDN_X + 66)
NIB_W, NIB_H, NIB0_X, NIB_Y = 58, 60, 28, INPUT_Y0 + 8  # nib buttons

def nib_center(i):
    return NIB0_X + i * (NIB_W + 10) + NIB_W // 2, NIB_Y + NIB_H // 2


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
    env = dict(os.environ,
               QTFB_KEY="12345",
               PI_COLLAB_BIN=os.path.join(here, "fake-pi.py"),
               HOME="/tmp")
    app = subprocess.Popen(["qemu-arm-static", app_bin], env=env)

    conn, _ = srv.accept()
    init = conn.recv(64)
    assert init[0] == 0, init[0]
    conn.send(struct.pack("<B3xiI12x", 0, SHM_KEY, W * H * 2))

    def pen(itype, x, y, d=0):
        conn.send(struct.pack("<B3xiiiii", MESSAGE_USERINPUT, itype, 0, x, y, d))

    time.sleep(1.5)  # header + input strip drawn, pi spawned

    # pick the small nib (i=0) so the stroke is visibly thin
    nx, ny = nib_center(0)
    pen(PEN_PRESS, nx, ny)
    pen(PEN_RELEASE, nx, ny)
    time.sleep(0.2)

    # handwrite a couple of "words" in the canvas with the pen
    pen(PEN_PRESS, 120, CANVAS_Y0 + 140)
    for i in range(1, 90):
        x = 120 + i * 9
        y = CANVAS_Y0 + 140 + int(70 * math.sin(i / 6))
        pen(PEN_UPDATE, x, y)
    pen(PEN_RELEASE, 120 + 90 * 9, CANVAS_Y0 + 140)
    time.sleep(0.3)

    # tap SEND with the pen (touch would be palm-rejected)
    pen(PEN_PRESS, SEND_X + SEND_W // 2, BTN_Y + BTN_H // 2)
    pen(PEN_RELEASE, SEND_X + SEND_W // 2, BTN_Y + BTN_H // 2)

    def drain(seconds):
        conn.settimeout(seconds)
        end = time.time() + seconds
        try:
            while time.time() < end:
                conn.recv(64)
        except (socket.timeout, OSError):
            pass

    drain(0.9)  # pi is now in its "thinking" window
    write_png(out_png.replace(".png", "-thinking.png"))  # catch the dot

    drain(2.5)  # let pi's reply stream in

    # bump the font up twice to check A+ reflow + the deghost flash
    for _ in range(2):
        pen(PEN_PRESS, FUP_X + 30, HB_Y + HB_H // 2)
        pen(PEN_RELEASE, FUP_X + 30, HB_Y + HB_H // 2)
        drain(0.4)
    drain(1.2)  # let the coalesced deghost settle

    write_png(out_png)

    app.terminate()
    try:
        app.wait(timeout=3)
    except subprocess.TimeoutExpired:
        app.kill()


if __name__ == "__main__":
    main()
