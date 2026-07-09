#!/usr/bin/env python3
"""Capture reference screenshots of the REAL xochitl UI (reSnap technique).

The rM2 has no readable /dev/fb0 content — the live framebuffer sits in
xochitl's own heap. We find the mapping that FOLLOWS the /dev/fb0 line in
/proc/<pid>/maps, dd 1404*1872*2 bytes of RGB565 from /proc/<pid>/mem, and
convert to a PNG locally. Read-only: it never touches xochitl.

Usage: python3 tools/xsnap.py root@<ip> -o build/ref [--name home]
Then drive the tablet by hand between captures.
"""
import argparse
import struct
import subprocess
import sys
import zlib

W, H = 1404, 1872


def ssh(host, cmd):
    return subprocess.run(["ssh", "-o", "ConnectTimeout=8", host, cmd],
                          capture_output=True)


def find_fb(host):
    """(pid, address) of the framebuffer region in xochitl's memory.

    xochitl has more than one pid; only one maps /dev/fb0. On this rM2
    firmware the anonymous frame buffer does not reliably follow that line
    (libcrypt can), so we try, in order: the mapping right after /dev/fb0,
    then the /dev/fb0 mapping itself, then the largest anonymous rw-p region
    of exactly-a-frame size. Callers can also pass --addr to override once
    the right region is known for a firmware.
    """
    pids = ssh(host, "pgrep xochitl").stdout.decode().split()
    for pid in pids:
        maps = ssh(host, f"cat /proc/{pid}/maps").stdout.decode()
        lines = maps.splitlines()
        cands = []
        for i, line in enumerate(lines):
            if "/dev/fb0" in line:
                if i + 1 < len(lines):
                    cands.append(int(lines[i + 1].split("-", 1)[0], 16))
                cands.append(int(line.split("-", 1)[0], 16))  # fb0 itself
        need = W * H * 2
        best = None
        for line in lines:
            parts = line.split()
            if len(parts) >= 2 and parts[1].startswith("rw-p") and len(parts) < 6:
                a, b = (int(x, 16) for x in parts[0].split("-"))
                if b - a >= need and (best is None or b - a < best[1]):
                    best = (a, b - a)
        if cands:
            if best:
                cands.append(best[0])
            return pid, cands
    sys.exit("no xochitl pid maps /dev/fb0 (is the tablet awake on the UI?)")


def grab(host, pid, addr, out):
    n = W * H * 2
    # page-aligned block read (fb regions end in 000); bs=1 would be 5M
    # syscalls. Read whole 4K pages covering the frame, then trim.
    PAGE = 4096
    if addr % PAGE != 0:
        print(f"xsnap: warn: addr 0x{addr:x} not page-aligned, skipping")
        return
    blocks = (n + PAGE - 1) // PAGE
    raw = ssh(host, f"dd if=/proc/{pid}/mem bs={PAGE} skip={addr // PAGE} "
                    f"count={blocks} 2>/dev/null | base64").stdout
    import base64
    data = base64.b64decode(raw)[:n]
    if len(data) < n:
        print(f"xsnap: short read at 0x{addr:x} ({len(data)}/{n}) — not this region")
        return
    px = memoryview(data).cast("H")
    gray = bytes(((v >> 5) & 0x3F) * 255 // 63 for v in px[:W * H])

    def chunk(tag, d):
        c = tag + d
        return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c))

    ihdr = struct.pack(">IIBBBBB", W, H, 8, 0, 0, 0, 0)
    rows = b"".join(b"\x00" + gray[y * W:(y + 1) * W] for y in range(H))
    with open(out, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) +
                chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
    print(f"xsnap: wrote {out}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("host")
    ap.add_argument("-o", "--out", default="build/ref")
    ap.add_argument("--name", default="xochitl")
    ap.add_argument("--addr", help="hex fb address override for this firmware")
    a = ap.parse_args()
    import os
    os.makedirs(a.out, exist_ok=True)
    if a.addr:
        pid = ssh(a.host, "pgrep xochitl").stdout.decode().split()[0]
        cands = [int(a.addr, 16)]
    else:
        pid, cands = find_fb(a.host)
    print(f"xsnap: xochitl pid {pid}, {len(cands)} candidate fb address(es)")
    # write each candidate to a distinct file; the user keeps the one that
    # actually shows the UI (this firmware's layout, recorded via --addr next time)
    for i, addr in enumerate(cands):
        suffix = "" if len(cands) == 1 else f"-c{i}@{addr:x}"
        grab(a.host, pid, addr, f"{a.out}/{a.name}{suffix}.png")


if __name__ == "__main__":
    main()
