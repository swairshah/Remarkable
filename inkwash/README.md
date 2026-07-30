# inkwash — pen-and-ink with living water, on the reMarkable 2

A native AppLoad port of [johnowhitaker/inkwash](https://github.com/johnowhitaker/inkwash)
(a single-file WebGL2 fluid-painting app): the **pen** lays down crisp dark
ink, **water** makes the paper wet, and wherever the paper is wet the ink
lifts, flows, bleeds and blends — then dries back into the page.

On the tablet:

| tool | does |
| --- | --- |
| pen tip | ink — real Wacom pressure shapes the line |
| Marker tail (eraser end) | water brush |
| finger | water brush too (once the pen has been away ~1.5s) |
| FIX button | flash-dry: settle every wash into the paper, quality refresh |
| BLEED button | cycle how eagerly pigment spreads in water (LO/MED/HI) |
| CLEAR / EXIT | what they say |
| tap the title | cycle the live e-ink waveform (latency vs quality) |

**Prerequisite:** the xovi + AppLoad stack from `../pi/pi-appload/install.sh`,
same as [`../sample-app/`](../sample-app/) — this project only adds an app
to it. All the plumbing (qtfb shared framebuffer, direct digitizer reads,
palm rejection, update batching) is inherited from sample-app; read that
first if this is your first AppLoad app.

## Quick start

```sh
make            # cross-compile (docker, ghcr.io/toltec-dev/base image)
make deploy     # push to the tablet
make preview    # no tablet: scripted paint session under qemu -> build/preview*.png
```

## How the effect is done without a GPU

The original runs a Navier-Stokes-ish GPU sim (velocity, pressure
projection, wetness, pigment — five shader passes per frame). An i.MX7 has
no usable GPU, and e-ink can't show 60fps anyway, so the port re-thinks the
pipeline (all in `src/main.c`):

- **Quarter-resolution fluid state.** Wetness, velocity and pigment live on
  a 351x468 float grid (25 steps/s, only while something is wet, only inside
  the bounding box of wet paper). Bleeding is a soft low-frequency
  phenomenon; it doesn't need 1404x1872. The pressure-projection step is
  dropped — brush-driven velocity plus wet-confined diffusion carries the
  look.
- **Full-resolution linework.** Pen strokes go into their own 1404x1872
  layer, so lines stay razor sharp. Wet cells gradually dissolve the line
  pixels under them into mobile pigment — which is exactly how a real ink
  line re-wets. Dry paper never blurs.
- **Watercolor edges for free.** Pigment diffuses only where the paper is
  wet; as the water evaporates the wet region shrinks and strands pigment
  at its boundary — the classic dark-edged wash, no special casing.
- **Two-stage e-ink rendering.** While water is live, regions repaint
  through the fast near-binary waveform, dithered to pure B/W — washes read
  as halftone stipple, latency stays low. When the last water evaporates
  (or FIX is tapped), the whole wash area re-renders in true 16-level gray
  through the slow quality waveform: the drawing visibly "dries into the
  page". Grayscale comes from an exp() absorption LUT (Beer-Lambert, like
  the original's `paper * exp(-absorbance)`) with hashed paper grain.

## Files

```
src/main.c                  the whole app: sim, rendering, input, UI
src/qtfb.h                  the AppLoad/qtfb wire protocol (from sample-app)
src/font5x7.h               tiny bitmap font (from sample-app)
app/external.manifest.json  tells AppLoad what to launch
app/icon.png                the launcher icon
test/fake-qtfb.py           fake server: scripted paint session, no tablet
Makefile                    build / preview / deploy / log / uninstall
```

## Preview without the tablet

`make preview` runs the real arm binary under qemu against
`test/fake-qtfb.py`, which scripts a session — pen draws an apple-ish
outline, a finger sweeps water across it, the ink bleeds for a few seconds,
FIX dries it — and screenshots the framebuffer at each stage:

- `build/preview-1-lines.png` — crisp linework
- `build/preview-2-wet.png` — water down, ink bleeding (stippled live view)
- `build/preview-3-fixed.png` — dried into 16-gray

## Knobs worth turning (top of main.c)

- `DRY_TAU` — seconds for water to evaporate (default 10; the web app's
  `dry` slider)
- `BLEED_LEVELS` — what the BLEED button cycles through (`flow`+`bleed`)
- `REWET_RATE` — how fast water eats crisp linework
- `SIM_MS` — sim cadence; 40ms is comfortable for the i.MX7
- `WET_SHEEN` — how dark wet blank paper renders (feedback while painting)

## Troubleshooting

Same story as [`../sample-app/`](../sample-app/#troubleshooting): icon
missing → `make restart-ui`; app dies instantly → `make log` while tapping
the icon; UI broken → `ssh root@10.11.99.1 /home/root/xovi/stock`.
