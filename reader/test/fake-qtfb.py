#!/usr/bin/env python3
"""Fake AppLoad/qtfb server for reader's `make preview` (no tablet).

Plays the server side of the qtfb protocol (see src/qtfb.rs): backs the
framebuffer with /dev/shm, launches the app under qemu with a FAKE pi
(test/fake-pi.py) wired in via READER_PI_BIN and the testbook bundle via
READER_BOOKS, then scripts a session: open the book from the home screen,
scribble (the pause trigger), let the fake pi underline / margin-note /
insert a note page, and flip through everything. Screenshots along the way.

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


def copy_tree(src, dst):
    """python3-minimal has no shutil; this is all we need of copytree."""
    os.makedirs(dst, exist_ok=True)
    for name in os.listdir(src):
        s, d = os.path.join(src, name), os.path.join(dst, name)
        if os.path.isdir(s):
            copy_tree(s, d)
        else:
            with open(s, "rb") as fi, open(d, "wb") as fo:
                fo.write(fi.read())

W, H = 1404, 1872
SHM_KEY = 7

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
    # a fresh copy of the testbook: state.json/ink from earlier runs is gone
    books_src = os.path.join(here, "..", "build", "testbook", "books")
    subprocess.run(["rm", "-rf", "/tmp/rd-books"], check=False)
    copy_tree(books_src, "/tmp/rd-books")

    env = dict(os.environ,
               QTFB_KEY="12345",
               READER_PI_BIN=os.path.join(here, "fake-pi.py"),
               READER_BOOKS="/tmp/rd-books",
               READER_SOCK="/tmp/rd.sock",
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

    def swipe(x0, x1, y=900):
        step = (x1 - x0) // 11
        touch(TOUCH_PRESS, x0, y)
        for i in range(1, 12):
            touch(TOUCH_UPDATE, x0 + i * step, y)
        touch(TOUCH_RELEASE, x1, y)

    time.sleep(1.5)  # first paint done, pi spawned -> the home screen
    write_png(out_png.replace(".png", "-home.png"))

    # tap the first (only) book row -> decode page 1, GC16 paint
    pen(PEN_PRESS, 400, 210)
    pen(PEN_RELEASE, 400, 210)
    drain(3.0)
    write_png(out_png.replace(".png", "-opened.png"))

    # handwrite a squiggle in the bottom margin (this is "the user's ink")
    pen(PEN_PRESS, 220, 1740)
    for i in range(1, 50):
        pen(PEN_UPDATE, 220 + i * 9, 1740 + int(24 * math.sin(i / 3)))
    pen(PEN_RELEASE, 220 + 50 * 9, 1740)

    # pause: idle trigger (2.8s) -> fake pi thinks (1s), then underlines,
    # margin-notes, inserts a note page and writes it, views
    drain(4.2)
    write_png(out_png.replace(".png", "-thinking.png"))  # the working dot
    drain(10.0)
    write_png(out_png)  # p.1: underline + margin note, animated in

    # flip forward: the inserted NOTE page, drawn straight from the model
    time.sleep(1.7)  # let palm rejection lapse
    swipe(1150, 190)
    drain(2.5)
    write_png(out_png.replace(".png", "-note.png"))

    # flip forward again: printed page 2 (raster decode on flip)
    swipe(1150, 190)
    drain(2.5)
    write_png(out_png.replace(".png", "-p2.png"))

    # flip back twice: p.1 re-rendered entirely from raster + saved vectors
    swipe(190, 1150)
    drain(1.5)
    swipe(190, 1150)
    drain(2.5)
    write_png(out_png.replace(".png", "-back.png"))

    app.terminate()
    try:
        app.wait(timeout=3)
    except subprocess.TimeoutExpired:
        app.kill()


if __name__ == "__main__":
    main()
