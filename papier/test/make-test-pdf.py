#!/usr/bin/env python3
"""A tiny synthetic 2-page PDF for `make preview` / `make testbook`.

Letter-sized pages with a deliberately wide RIGHT margin (so the fake pi
has room for margin notes) and a known phrase for the underline test:
"reflect ambient light" on page 1.
"""
import sys

import fitz

PAGE1 = """Electronic Paper

Electrophoretic displays form images by moving charged pigment
particles through a fluid under an electric field. Unlike emissive
screens, they reflect ambient light, which is why reading one feels
close to reading print on paper.

Each waveform trades speed against fidelity. A direct update drives
pixels straight between black and white and completes quickly, at the
cost of edge artifacts. Higher-fidelity waveforms step through many
voltage phases to settle precise gray levels, and a full refresh
flashes the panel to erase ghosting entirely.

The panel holds its image with no power at all; energy is spent only
on change. This is the property that makes week-long battery life
ordinary for such devices.
"""

PAGE2 = """Waveforms in Practice

Handwriting demands the fast path: ink must appear within tens of
milliseconds of the nib touching glass, so pen strokes ride the direct
update everywhere.

Page turns can afford the opposite trade. A full flash costs almost
half a second, but it wipes the accumulated ghosts of the previous
page and leaves crisp print behind.

The practical compromise for mixed content is to dither grays down to
a binary image ahead of time: the panel then never needs a gray-aware
waveform for ordinary reading, and every interaction stays on the
fast path.
"""


def main() -> int:
    out = sys.argv[1]
    doc = fitz.open()
    for body in (PAGE1, PAGE2):
        page = doc.new_page(width=612, height=792)  # letter
        head, rest = body.split("\n", 1)
        page.insert_textbox(fitz.Rect(60, 56, 440, 90), head, fontsize=18, fontname="tibo")
        page.insert_textbox(fitz.Rect(60, 100, 440, 740), rest.strip(), fontsize=12.5,
                            fontname="tiro", lineheight=1.45)
    doc.save(out)
    print(f"make-test-pdf: wrote {out} ({doc.page_count} pages)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
