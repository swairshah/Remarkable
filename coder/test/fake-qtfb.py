#!/usr/bin/env python3
"""Coder's `make preview` scenario (no tablet).

The qtfb protocol server lives in libreink/tools/preview/qtfb_host.py —
this file is only the script: launch the app with a FAKE pi
(CODER_PI_BIN), scribble a "clone request" on the notes pad, wait for
the pause trigger -> the fake pi registers the micrograd project over
the tool socket, flips the tablet to it, draws the overview diagram on
page 1 and a subsystem detail on an appended page 2 -> screenshots.
Then flip between pages (persistence + re-render), open the sidebar
(NOTES + MICROGRAD rows), switch back to notes, and read the project's
SUMMARY.md through the DOCS browser.

The container has no Wacom device, so the app falls back to AppLoad pen
events — which is exactly what we script here."""
import os
import sys
import time

# shared protocol core (see the docstring)
sys.path.insert(0, os.environ.get(
    "LIBREINK_PREVIEW",
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 "..", "..", "..", "libreink", "tools", "preview")))
from qtfb_host import *  # noqa: E402,F403


class CoderHarness(Harness):
    def launch(self, **env_extra):
        base = dict(CODER_PI_BIN=os.path.join(self.here, "fake-pi.py"),
                    CODER_DATA_DIR="/tmp/coder-data",
                    CODER_SOCK="/tmp/coder.sock",
                    CODER_VM="echo fake-vm",
                    CODER_PI_STALL="3600")
        base.update(env_extra)
        return super().launch(**base)


def seed_empty_project():
    """A registered-but-never-opened repo (as if cloned from the desktop):
    exercises the blank-page welcome header + the auto-offer open event."""
    d = "/tmp/coder-data/projects/emptyrepo"
    os.makedirs(os.path.join(d, "pages"), exist_ok=True)
    with open(os.path.join(d, "meta.json"), "w") as f:
        f.write('{"name": "emptyrepo", "url": "https://github.com/example/emptyrepo",'
                ' "branch": "main", "summary": "a tiny demo repo - one lib, one cli"}')


def main():
    app_bin, out_png = sys.argv[1], sys.argv[2]
    h = CoderHarness(app_bin)
    seed_empty_project()
    try:
        s = h.launch()
        time.sleep(1.5)  # first paint done (notes pad), pi spawned

        # handwrite the "clone request" on the notes pad (a squiggle —
        # the fake pi doesn't read it, it just acts on the pause)
        s.squiggle(180, 400, n=70, dx=13, amp=22)
        s.squiggle(180, 520, n=40, dx=11, amp=18)
        write_png(out_png.replace(".png", "-notes-ink.png"))

        # pause: idle trigger (2.8s) -> fake pi thinks (1s), registers the
        # project, flips the tablet to it, draws overview + detail pages
        s.drain(3.5)
        write_png(out_png.replace(".png", "-thinking.png"))  # the working dot
        s.drain(30.0)  # goto flash + ghost-hand animation of the overview
        write_png(out_png)  # micrograd page 1: the architecture overview

        # swipe to page 2: the appended subsystem detail
        time.sleep(1.7)  # let palm rejection lapse
        s.swipe(1150, 190)
        s.drain(6.0)
        write_png(out_png.replace(".png", "-page2.png"))

        # and back: page 1 re-rendered entirely from the saved vector model
        s.swipe(190, 1150)
        s.drain(8.0)
        write_png(out_png.replace(".png", "-back.png"))

        # sidebar: NOTES + MICROGRAD rows, DOCS below
        s.pen_tap(40, 40)
        s.drain(1.5)
        write_png(out_png.replace(".png", "-sidebar.png"))

        # tap NOTES: back to the scratch pad (its ink survived)
        s.pen_tap(180, 160)
        s.drain(6.0)
        write_png(out_png.replace(".png", "-notes.png"))

        # sidebar again -> DOCS (row 4: NOTES, MICROGRAD, EMPTYREPO, INSTRUCTIONS, DOCS)
        s.pen_tap(40, 40)
        s.drain(1.5)
        s.pen_tap(180, 130 + 4 * 68 + 30)
        s.drain(2.0)
        s.pen_tap(400, 160)  # the micrograd summary
        s.drain(2.5)
        write_png(out_png.replace(".png", "-docs.png"))

        # leave the reader (swipe right -> list, swipe left -> close) ...
        s.swipe(190, 1150)
        s.drain(1.5)
        s.swipe(1150, 190)
        s.drain(6.0)
        # ... and open the never-touched EMPTYREPO from the sidebar: the
        # welcome header paints AND the blank page auto-offers to pi (the
        # open event) -> fake pi draws a compact overview onto it
        s.pen_tap(40, 40)
        s.drain(1.5)
        s.pen_tap(180, 130 + 2 * 68 + 30)  # EMPTYREPO row
        s.drain(0.8)
        write_png(out_png.replace(".png", "-blank-open.png"))  # header + "pi is reading"
        s.drain(14.0)
        write_png(out_png.replace(".png", "-blank-drawn.png"))  # the drawn overview
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
