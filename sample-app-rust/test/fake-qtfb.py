#!/usr/bin/env python3
"""A fake AppLoad/qtfb server, so the app can be exercised without a tablet.

Usage (inside the linux container `make preview` builds):

    python3 test/fake-qtfb.py build/sample-app build/preview.png

It plays the server side of the protocol in src/qtfb.h: listens on
/tmp/qtfb.sock, backs the framebuffer with a file in /dev/shm (which is
where shm_open() lands on Linux), launches the app under qemu-arm-static,
scripts some touch/pen input, taps EXIT, and finally writes the framebuffer
out as a grayscale PNG. If the layout or font code regresses, you see it
in build/preview.png instead of on the device.
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


def main():
    app_bin, out_png = sys.argv[1], sys.argv[2]

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

    def send_input(itype, dev, x, y, d=0):
        conn.send(struct.pack("<B3xiiiii", MESSAGE_USERINPUT, itype, dev, x, y, d))

    time.sleep(1.0)  # let the app draw its scene

    # finger: a sine-wave scribble across the canvas
    send_input(TOUCH_PRESS, 0, 100, 900)
    for i in range(1, 120):
        send_input(TOUCH_UPDATE, 0, 100 + i * 10, 900 + int(300 * math.sin(i / 8)))
    send_input(TOUCH_RELEASE, 0, 1290, 900)
    # pen: a short diagonal stroke
    send_input(PEN_PRESS, 0, 300, 1400)
    for i in range(1, 40):
        send_input(PEN_UPDATE, 0, 300 + i * 8, 1400 + i * 5, d=2000)
    send_input(PEN_RELEASE, 0, 612, 1595)

    # drain the update messages the app sends back while it draws
    conn.settimeout(2.0)
    try:
        while conn.recv(64):
            pass
    except socket.timeout:
        pass

    # tap EXIT (top-right button) and wait for a clean exit — with the PEN:
    # after pen activity the app ignores touch for a while (palm rejection),
    # and pen taps on buttons must work anyway
    send_input(PEN_PRESS, 0, 1250, 70)
    send_input(PEN_RELEASE, 0, 1250, 70)
    try:
        while conn.recv(64):
            pass
    except (socket.timeout, ConnectionResetError):
        pass
    rc = app.wait(timeout=5)
    print(f"fake-qtfb: app exited rc={rc}")

    # framebuffer -> grayscale PNG (green channel is enough for B/W content)
    raw = memoryview(open(SHM_PATH, "rb").read()).cast("H")
    gray = bytes(((v >> 5) & 0x3F) * 255 // 63 for v in raw)

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    ihdr = struct.pack(">IIBBBBB", W, H, 8, 0, 0, 0, 0)  # 8-bit grayscale
    rows = b"".join(b"\x00" + gray[y * W:(y + 1) * W] for y in range(H))
    with open(out_png, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) +
                chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
    print(f"fake-qtfb: wrote {out_png}")
    assert rc == 0, f"app exited with rc={rc}"


if __name__ == "__main__":
    main()
