#!/usr/bin/env python3
"""Build a reader book bundle from a PDF — runs on the desk side, not the
tablet (the device never parses PDFs).

    uv run --with pymupdf --with numpy python3 tools/mkbook.py in.pdf \
        -o build/books/my-paper [--title "My Paper"]

Produces:
    OUT/meta.json        { title, pages, w, h }
    OUT/pages/0001.png   1404x1872 8-bit gray PNG, 1-BIT CONTENT (dithered)
    OUT/text/0001.json   { text, words: [[x0,y0,x1,y1,"word"], ...] }

Pages are scaled to fit 1404x1872 preserving aspect and centered on white
(minus --margin px of guaranteed border on every side — margin-note room);
word boxes are transformed into the same device pixel space, so underlining
on the tablet is pure geometry.

Pages stay GRAYSCALE (antialiased, like xochitl) — the app writes pen ink
with the 1-bit DU waveform and heals the surrounding print with a GL16
settle pass after the pen lifts. --dither forces 1-bit output (Bayer 8x8)
if you ever want the old look.
"""
import argparse
import json
import os
import re
import struct
import sys
import zlib

import fitz  # pymupdf
import numpy as np

W, H = 1404, 1872

BAYER8 = np.array([
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
], dtype=np.float32)


def dither_1bit(img: np.ndarray) -> np.ndarray:
    """Bayer 8x8 ordered dither to {0, 255}. Near-black and near-white pass
    through untouched at any matrix phase, so print stays sharp."""
    h, w = img.shape
    thresh = (np.tile(BAYER8, (h // 8 + 1, w // 8 + 1))[:h, :w] + 0.5) * (255.0 / 64.0)
    return np.where(img.astype(np.float32) > thresh, 255, 0).astype(np.uint8)


def write_png_gray(path: str, img: np.ndarray) -> None:
    h, w = img.shape
    raw = np.zeros((h, w + 1), dtype=np.uint8)
    raw[:, 1:] = img  # filter byte 0 per row

    def chunk(tag: bytes, data: bytes) -> bytes:
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", zlib.compress(raw.tobytes(), 6)))
        f.write(chunk(b"IEND", b""))


CROP_BORDER = 30  # device px of breathing room around a cropped page


def build_book(pdf: str, out: str, title: str | None = None,
               dither: bool = False, margin: int = 40,
               margins: tuple | None = None,
               crop: tuple | None = None) -> str:
    """Render `pdf` into a bundle at `out`; returns the resolved title.

    Two placement modes:
      `crop` = (cx0, cy0, cx1, cy1) fractions 0..1 of each source page to
        KEEP; that region is scaled UP to fill the screen (with a small
        border), so cropping a PDF's wasteful margins makes the text bigger.
        This is what the web crop editor sends. Applied to every page.
      `margins` = (left, top, right, bottom) white border in device px — the
        whole page is scaled to fit inside (the CLI default). Asymmetric
        margins shift the page like the stock app's page adjustment."""
    doc = fitz.open(pdf)
    n = doc.page_count
    if n == 0:
        raise ValueError("empty PDF")

    if crop is not None:
        cx0, cy0, cx1, cy1 = (min(max(float(v), 0.0), 1.0) for v in crop)
        if cx1 - cx0 < 0.05 or cy1 - cy0 < 0.05:
            raise ValueError(f"crop region {crop} too small")
    else:
        ml, mt, mr, mb = ((margin, margin, margin, margin) if margins is None else
                          tuple(margin if v is None else v for v in margins))
        ml, mt, mr, mb = (max(0, min(int(v), 1000)) for v in (ml, mt, mr, mb))
        box_w, box_h = W - ml - mr, H - mt - mb
        if box_w < 400 or box_h < 500:
            raise ValueError(f"margins leave a {box_w}x{box_h} page box — too small")

    title = (title or "").strip() or ((doc.metadata or {}).get("title") or "").strip()
    title = title or re.sub(r"[-_]+", " ", os.path.splitext(os.path.basename(pdf))[0]).strip()

    os.makedirs(os.path.join(out, "pages"), exist_ok=True)
    os.makedirs(os.path.join(out, "text"), exist_ok=True)

    total_bytes = 0
    for i in range(n):
        page = doc[i]
        rect = page.rect
        if crop is not None:
            # keep the crop region; scale it up to fill the screen (minus border)
            clip = fitz.Rect(rect.x0 + cx0 * rect.width, rect.y0 + cy0 * rect.height,
                             rect.x0 + cx1 * rect.width, rect.y0 + cy1 * rect.height)
            k = min((W - 2 * CROP_BORDER) / clip.width, (H - 2 * CROP_BORDER) / clip.height)
            pix = page.get_pixmap(matrix=fitz.Matrix(k, k), colorspace=fitz.csGRAY, alpha=False, clip=clip)
            orig_x, orig_y = clip.x0, clip.y0   # page-pt that maps to the pixmap's top-left
            drop_outside = True
        else:
            k = min(box_w / rect.width, box_h / rect.height)
            pix = page.get_pixmap(matrix=fitz.Matrix(k, k), colorspace=fitz.csGRAY, alpha=False)
            orig_x, orig_y = 0.0, 0.0
            drop_outside = False
        img = np.frombuffer(pix.samples, dtype=np.uint8).reshape(pix.height, pix.stride)[:, :pix.width]
        if crop is not None:
            ox = (W - pix.width) // 2
            oy = (H - pix.height) // 2
        else:
            # center the residual slack WITHIN the margin box (asymmetric shift)
            ox = ml + (box_w - pix.width) // 2
            oy = mt + (box_h - pix.height) // 2
        canvas = np.full((H, W), 255, dtype=np.uint8)
        canvas[oy:oy + pix.height, ox:ox + pix.width] = img
        if dither:
            canvas = dither_1bit(canvas)

        png_path = os.path.join(out, "pages", f"{i + 1:04}.png")
        write_png_gray(png_path, canvas)
        total_bytes += os.path.getsize(png_path)

        words = []
        for (x0, y0, x1, y1, w, *_rest) in page.get_text("words"):
            nx0, ny0 = (x0 - orig_x) * k + ox, (y0 - orig_y) * k + oy
            nx1, ny1 = (x1 - orig_x) * k + ox, (y1 - orig_y) * k + oy
            if drop_outside and (nx1 < ox or nx0 > ox + pix.width or ny1 < oy or ny0 > oy + pix.height):
                continue  # word cropped away
            words.append([int(nx0), int(ny0), int(nx1 + 0.5), int(ny1 + 0.5), w])
        text = page.get_text().strip()
        with open(os.path.join(out, "text", f"{i + 1:04}.json"), "w") as f:
            json.dump({"text": text, "words": words}, f, ensure_ascii=False)

        if (i + 1) % 20 == 0 or i + 1 == n:
            print(f"mkbook: {i + 1}/{n} pages", file=sys.stderr)

    meta = {"title": title, "pages": n, "w": W, "h": H}
    if crop is not None:
        meta["crop"] = [cx0, cy0, cx1, cy1]
    else:
        meta["margins"] = [ml, mt, mr, mb]
    with open(os.path.join(out, "meta.json"), "w") as f:
        json.dump(meta, f, ensure_ascii=False)

    print(f"mkbook: '{title}' — {n} pages, {total_bytes // 1024} KB of rasters -> {out}")
    return title


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("pdf")
    ap.add_argument("-o", "--out", required=True, help="bundle directory to write")
    ap.add_argument("--title", help="book title (default: PDF metadata, then filename)")
    ap.add_argument("--margin", type=int, default=40,
                    help="guaranteed white border in device px (default 40; "
                         "80-120 gives real margin-note room)")
    ap.add_argument("--margin-left", type=int)
    ap.add_argument("--margin-top", type=int)
    ap.add_argument("--margin-right", type=int,
                    help="per-side overrides; a big --margin-right shifts the "
                         "page left and leaves a writing gutter (like moving "
                         "the page in the stock app)")
    ap.add_argument("--margin-bottom", type=int)
    ap.add_argument("--dither", action="store_true",
                    help="force 1-bit pages (Bayer); default keeps grayscale")
    ap.add_argument("--crop", nargs=4, type=float, metavar=("CX0", "CY0", "CX1", "CY1"),
                    help="keep this fraction (0..1) of each page and scale it up to "
                         "fill the screen — crops a PDF's margins to enlarge text")
    args = ap.parse_args()
    try:
        build_book(args.pdf, args.out, args.title, dither=args.dither, margin=args.margin,
                   margins=(args.margin_left, args.margin_top,
                            args.margin_right, args.margin_bottom),
                   crop=(tuple(args.crop) if args.crop else None))
    except ValueError as e:
        print(f"mkbook: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
