#!/usr/bin/env python3
"""Build a searchable PDF from an alt-ui book bundle.

    alt-ui-make-pdf.py <doc-dir> <out.pdf> [title]

Used by the Paper upload service when a book has NO retained source PDF
(desk-rendered books, books that predate source retention): the full
PDF.js viewer needs *a* PDF, so we derive one from the bundle itself —
pages/NNNN.png as page images plus an INVISIBLE text layer placed at the
word boxes mkbook.py recorded in text/NNNN.json. Search, text selection
and copy work in the viewer even though the pages are rasters.

Runs in the alt-ui venv (pymupdf), same as mkbook.py.
"""
import glob
import json
import os
import sys

import fitz  # pymupdf

W, H = 1404, 1872


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    docdir, out = sys.argv[1], sys.argv[2]
    title = sys.argv[3] if len(sys.argv) > 3 else ""

    pages = sorted(glob.glob(os.path.join(docdir, "pages", "[0-9]" * 4 + ".png")))
    if not pages:
        print(f"alt-ui-make-pdf: no pages in {docdir}", file=sys.stderr)
        return 1

    doc = fitz.open()
    for i, png in enumerate(pages):
        page = doc.new_page(width=W, height=H)
        page.insert_image(fitz.Rect(0, 0, W, H), filename=png)
        try:
            with open(os.path.join(docdir, "text", f"{i + 1:04}.json")) as f:
                words = json.load(f).get("words", [])
        except Exception:
            words = []
        for entry in words:
            if len(entry) < 5:
                continue
            x0, y0, x1, y1, w = entry[0], entry[1], entry[2], entry[3], str(entry[4])
            box_h = max(y1 - y0, 4.0)
            try:
                # render_mode=3: invisible text. Baseline sits ~18% above the
                # box bottom, size ~85% of box height — close enough that
                # selection highlights line up with the printed word.
                page.insert_text(
                    (x0, y1 - box_h * 0.18), w,
                    fontsize=box_h * 0.85, render_mode=3,
                )
            except Exception:
                pass  # odd glyphs must never sink the whole build
        if (i + 1) % 50 == 0 or i + 1 == len(pages):
            print(f"make-pdf: {i + 1}/{len(pages)} pages", file=sys.stderr)

    if title:
        doc.set_metadata({"title": title})
    doc.save(out, deflate=True, garbage=3)
    print(f"make-pdf: {len(pages)} pages -> {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
