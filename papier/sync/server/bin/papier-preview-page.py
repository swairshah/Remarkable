#!/usr/bin/env python3
"""Render ONE raw PDF page (edge-to-edge, no added margins) to a PNG, for the
web margin editor's live preview.

    papier-preview-page.py <pdf> <page-index-0based> <out.png>

The browser fits this raw image into the draggable margin box exactly the way
mkbook fits the page into the margin box (scale-to-fit, centred), so dragging
the box gives a faithful live preview without a server round-trip per drag.
Runs in the VM venv (pymupdf). Grayscale, long edge ~1400px.
"""
import sys
import fitz  # pymupdf

TARGET = 1400.0  # long-edge pixels — plenty for a preview


def main() -> int:
    pdf, page, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    doc = fitz.open(pdf)
    if page < 0 or page >= doc.page_count:
        print(f"page {page} out of range (0..{doc.page_count - 1})", file=sys.stderr)
        return 1
    p = doc[page]
    r = p.rect
    k = TARGET / max(r.width, r.height)
    pix = p.get_pixmap(matrix=fitz.Matrix(k, k), colorspace=fitz.csGRAY, alpha=False)
    pix.save(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
