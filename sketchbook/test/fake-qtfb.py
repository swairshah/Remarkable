#!/usr/bin/env python3
"""Sketchbook's `make preview` scenario (no tablet).

The qtfb protocol server lives in libreink/tools/preview/qtfb_host.py —
this file is only the script: launch the app with a FAKE pi
(SKETCHBOOK_PI_BIN), handwrite, wait for the pause trigger -> the fake pi
draws two patches over the tool socket and erases one -> screenshots.
Then flip to page 2 and back (persistence + full re-render), and open
the sidebar -> LIBRARY -> the markdown reader.

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

SAMPLE_MD = """---
title: "Scaling laws, distilled"
source: "https://example.com/post"
---

# Scaling laws, distilled

Training loss $L$ falls **predictably** with model size $N$, data $D$ and
compute $C$ — a power law, i.e. a straight line on a log-log plot (see
[Kaplan et al. 2020](https://arxiv.org/abs/2001.08361)).

::: aside
**Rule of thumb:** $C \\\\approx 6ND$ — the `6` covers forward + backward.
:::

Key regimes:

1. Noiseless data, unique solution: error goes as $D^{-1}$.
2. Noisy data: error goes as $D^{-1/2}$, learning is harder.
- Chinchilla: scale $N$ and $D$ together, about 20 tokens per parameter.

$$
L(N, D) = E + A N^{-a} + B D^{-b}
$$

```python
def loss(n, d):
    return E + A * n**-0.34 + B * d**-0.28
```

| Symbol | Meaning |
| --- | --- |
| N | parameters |
| D | tokens |

---

That is the whole idea.
"""



class SketchbookHarness(Harness):
    def launch(self, **env_extra):
        base = dict(SKETCHBOOK_PI_BIN=os.path.join(self.here, "fake-pi.py"),
                    SKETCHBOOK_DATA_DIR="/tmp/nb-pages",
                    SKETCHBOOK_LIBRARY="/tmp/nb-lib",
                    SKETCHBOOK_SOCK="/tmp/nb.sock")
        base.update(env_extra)
        return super().launch(**base)


def main():
    app_bin, out_png = sys.argv[1], sys.argv[2]
    h = SketchbookHarness(app_bin)
    os.makedirs("/tmp/nb-lib", exist_ok=True)
    with open("/tmp/nb-lib/scaling-laws-notes.md", "w") as f:
        f.write(SAMPLE_MD)
    try:
        s = h.launch()
        time.sleep(1.5)  # first paint done, pi spawned

        # sketch a rough circle-ish blob in the LEFT panel ("the sketch");
        # x stays under PANEL_W=702 — the right panel is the render's
        s.pen(PEN_PRESS, 350 + 180, 700)
        for i in range(1, 60):
            a = i / 60.0 * 2 * math.pi
            s.pen(PEN_UPDATE, 350 + int(180 * math.cos(a)), 700 + int(220 * math.sin(a)))
        s.pen(PEN_RELEASE, 350 + 180, 700)

        # pause: idle trigger (2.8s) -> fake pi thinks (1s), draws two
        # patches, views, erases the circle mid-animation
        s.drain(4.2)
        write_png(out_png.replace(".png", "-thinking.png"))  # the working dot
        s.drain(14.0)
        write_png(out_png)  # user ink + the AI note (circle erased again)

        # flip forward: a fresh page 2 (flip gesture + new-page path)
        time.sleep(1.7)  # let palm rejection lapse
        s.swipe(1150, 190)
        s.drain(6.0)
        write_png(out_png.replace(".png", "-page2.png"))

        # flip back: page 1 re-rendered entirely from the saved vector model
        s.swipe(190, 1150)
        s.drain(10.0)
        write_png(out_png.replace(".png", "-back.png"))

        # sidebar -> LIBRARY -> first item: the markdown reader
        s.pen_tap(40, 40)               # corner tap: sidebar
        s.drain(1.0)
        write_png(out_png.replace(".png", "-sidebar.png"))
        s.pen_tap(180, 130 + 5 * 68 + 30)   # LIBRARY row
        s.drain(1.5)
        s.pen_tap(400, 160)             # first item
        s.drain(2.0)
        write_png(out_png.replace(".png", "-library.png"))
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
