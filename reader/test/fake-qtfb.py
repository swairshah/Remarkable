#!/usr/bin/env python3
"""Reader's `make preview` scenario (no tablet).

The qtfb protocol server lives in libreink/tools/preview/qtfb_host.py —
this file is only the script: launch the app with a FAKE pi
(READER_PI_BIN) and the testbook bundle (READER_BOOKS), open the book
from the home screen, scribble (the pause trigger), let the fake pi
underline / margin-note / insert a note page, and flip through
everything. Screenshots along the way.

The container has no Wacom device, so the app falls back to AppLoad pen
events — which is exactly what we script here."""
import os
import subprocess
import sys
import time

# shared protocol core (see the docstring)
sys.path.insert(0, os.environ.get(
    "LIBREINK_PREVIEW",
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 "..", "..", "..", "libreink", "tools", "preview")))
from qtfb_host import *  # noqa: E402,F403


class ReaderHarness(Harness):
    def launch(self, **env_extra):
        base = dict(READER_PI_BIN=os.path.join(self.here, "fake-pi.py"),
                    READER_BOOKS="/tmp/rd-books",
                    READER_SOCK="/tmp/rd.sock")
        base.update(env_extra)
        return super().launch(**base)


def main():
    app_bin, out_png = sys.argv[1], sys.argv[2]
    h = ReaderHarness(app_bin)

    # a fresh copy of the testbook: state.json/ink from earlier runs is gone
    books_src = os.path.join(h.here, "..", "build", "testbook", "books")
    subprocess.run(["rm", "-rf", "/tmp/rd-books"], check=False)
    copy_tree(books_src, "/tmp/rd-books")

    try:
        s = h.launch()
        time.sleep(1.5)  # first paint done, pi spawned -> the home screen
        write_png(out_png.replace(".png", "-home.png"))

        # tap the first (only) book row -> decode page 1, GC16 paint
        s.pen_tap(400, 210)
        s.drain(3.0)
        write_png(out_png.replace(".png", "-opened.png"))

        # handwrite a squiggle in the bottom margin ("the user's ink")
        s.squiggle(220, 1740, n=50, dx=9, amp=24)

        # pause: idle trigger (2.8s) -> fake pi thinks (1s), then underlines,
        # margin-notes, inserts a note page and writes it, views
        s.drain(4.2)
        write_png(out_png.replace(".png", "-thinking.png"))  # the working dot
        s.drain(10.0)
        write_png(out_png)  # p.1: underline + margin note, animated in

        # flip forward: the inserted NOTE page, drawn straight from the model
        time.sleep(1.7)  # let palm rejection lapse
        s.swipe(1150, 190)
        s.drain(2.5)
        write_png(out_png.replace(".png", "-note.png"))

        # flip forward again: printed page 2 (raster decode on flip)
        s.swipe(1150, 190)
        s.drain(2.5)
        write_png(out_png.replace(".png", "-p2.png"))

        # flip back twice: p.1 re-rendered entirely from raster + saved vectors
        s.swipe(190, 1150)
        s.drain(1.5)
        s.swipe(190, 1150)
        s.drain(2.5)
        write_png(out_png.replace(".png", "-back.png"))
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
