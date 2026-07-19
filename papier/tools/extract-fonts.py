#!/usr/bin/env python3
"""Carve the UI fonts (reMarkableSans etc.) out of /usr/bin/xochitl.

xochitl ships its typefaces as Qt resources embedded in the binary, not as
files on the tablet. We pull the binary, scan for sfnt font blobs (the
0x00010000 / 'OTTO' / 'true' magic + a sane table directory), and write
each out as a .ttf. NEVER commit the results (licensed fonts) — they land
in build/fonts/ and ship only to the user's own device.

Usage: python3 tools/extract-fonts.py root@<ip> -o build/fonts
"""
import argparse
import os
import struct
import subprocess
import sys

MAGICS = (b"\x00\x01\x00\x00", b"OTTO", b"true", b"ttcf")


def name_of(blob):
    """Read the font's name table entry 4 (full name), best-effort."""
    try:
        num_tables = struct.unpack(">H", blob[4:6])[0]
        off = 12
        for _ in range(num_tables):
            tag = blob[off:off + 4]
            toff, tlen = struct.unpack(">II", blob[off + 8:off + 16])
            if tag == b"name":
                nt = blob[toff:toff + tlen]
                count = struct.unpack(">H", nt[2:4])[0]
                soff = struct.unpack(">H", nt[4:6])[0]
                for r in range(count):
                    rec = nt[6 + r * 12:18 + r * 12]
                    nid, ln, o = struct.unpack(">HHH", rec[6:12])
                    if nid == 4:
                        s = nt[soff + o:soff + o + ln]
                        return s.replace(b"\x00", b"").decode("ascii", "ignore")
            off += 16
    except Exception:
        pass
    return None


def table_len(blob, pos):
    """Total byte length of the sfnt at `pos`, or None if not sane."""
    if len(blob) - pos < 12:
        return None
    num_tables = struct.unpack(">H", blob[pos + 4:pos + 6])[0]
    if not (1 <= num_tables <= 60):
        return None
    end = pos + 12 + num_tables * 16
    if end > len(blob):
        return None
    max_end = end - pos
    for i in range(num_tables):
        rec = blob[pos + 12 + i * 16:pos + 28 + i * 16]
        if len(rec) < 16:
            return None
        toff, tlen = struct.unpack(">II", rec[8:16])
        max_end = max(max_end, toff + tlen)
    return max_end if max_end < len(blob) - pos + 4 else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("host")
    ap.add_argument("-o", "--out", default="build/fonts")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)

    blob = subprocess.run(
        ["ssh", "-o", "ConnectTimeout=8", a.host, "cat /usr/bin/xochitl | base64"],
        capture_output=True).stdout
    import base64
    blob = base64.b64decode(blob)
    print(f"extract-fonts: pulled {len(blob) // 1024} KiB xochitl binary")

    found, pos = 0, 0
    while pos < len(blob) - 12:
        if blob[pos:pos + 4] in MAGICS:
            ln = table_len(blob, pos)
            if ln and ln > 4000:
                font = blob[pos:pos + ln]
                nm = name_of(font) or f"font{found}"
                safe = "".join(c for c in nm if c.isalnum() or c in "-_")
                path = f"{a.out}/{safe}.ttf"
                with open(path, "wb") as f:
                    f.write(font)
                print(f"extract-fonts: {path} ({ln // 1024} KiB) '{nm}'")
                found += 1
                pos += ln
                continue
        pos += 1

    if not found:
        sys.exit("no fonts found — the resources may be zlib-compressed "
                 "(rcc); parsing the rcc tree is the fallback (not yet built)")
    print(f"extract-fonts: {found} fonts -> {a.out} (NOT committed)")


if __name__ == "__main__":
    main()
