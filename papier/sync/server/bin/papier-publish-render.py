#!/usr/bin/env python3
"""Render a Papier notebook page (ink JSON) to a PNG, optionally as a DIFF
against a previously published copy of the same page.

    papier-publish-render.py <out.png|-> <current.json> [--prev <prev.json>]
                             [--scale 0.5] [--clean]

Diff mode (--prev): strokes are matched by CONTENT (their point array), not
by stroke id — the iPad heals foreign ids, so ids are not stable across
devices. Colour code, which the publish prompt explains to the model:

    grey   unchanged  (already published)
    black  added      (new since the last publish)
    red    removed    (erased since the last publish)
    blue   pi's own ink / typeset text (context, not the user's words)

--clean draws every stroke in its final colour (user black, pi blue) for the
handwritten-page figures on the site. A missing/empty current file renders a
blank page (so a deleted page diffs as "everything removed").

Prints one JSON line — {"added": n, "removed": n, "unchanged": n,
"strokes": n} — to stdout. With "-" as the output path nothing is drawn, only
the summary is printed (cheap change detection).
"""
import hashlib
import json
import sys

PAGE_W, PAGE_H = 1404, 1872
GREY = (178, 178, 178)
BLACK = (24, 24, 24)
RED = (214, 44, 44)
BLUE = (36, 87, 197)
BLUE_OLD = (176, 190, 214)
BG = (255, 255, 255)


def load(path):
    if not path:
        return {}
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f) or {}
    except (OSError, ValueError):
        return {}


def items(doc):
    """Flatten a page into (key, kind, payload) triples.

    kind: 'stroke' | 'text'; the key is a content hash so the same ink on
    two devices (different ids) matches, and a re-drawn stroke counts as
    removed + added."""
    out = []
    for s in doc.get("strokes") or []:
        pts = s.get("p") or []
        if len(pts) < 3:
            continue
        ai = bool(s.get("g", 0))
        key = "s:" + hashlib.sha1(("%d:%s" % (ai, ",".join(str(int(v)) for v in pts))).encode()).hexdigest()
        out.append((key, "stroke", (pts, ai)))
    for p in doc.get("patches") or []:
        for s in p.get("strokes") or []:
            pts = s.get("p") or []
            if len(pts) < 3:
                continue
            key = "s:" + hashlib.sha1(("1:%s" % ",".join(str(int(v)) for v in pts)).encode()).hexdigest()
            out.append((key, "stroke", (pts, True)))
        for t in p.get("texts") or []:
            txt = str(t.get("t", ""))
            if not txt:
                continue
            ai = bool(t.get("g", 0))
            key = "t:" + hashlib.sha1(("%d:%s:%s:%s:%s" % (ai, txt, t.get("x"), t.get("y"), t.get("s"))).encode()).hexdigest()
            out.append((key, "text", (txt, float(t.get("x", 0)), float(t.get("y", 0)), float(t.get("s", 320)), ai)))
    return out


def main(argv):
    if len(argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    out_path, cur_path = argv[1], argv[2]
    prev_path, scale, clean = None, 0.5, False
    i = 3
    while i < len(argv):
        a = argv[i]
        if a == "--prev":
            prev_path = argv[i + 1]; i += 2
        elif a == "--scale":
            scale = float(argv[i + 1]); i += 2
        elif a == "--clean":
            clean = True; i += 1
        else:
            print("unknown argument: " + a, file=sys.stderr)
            return 2

    cur = items(load(cur_path))
    prev = items(load(prev_path)) if prev_path else []
    cur_keys = {k for k, _, _ in cur}
    prev_keys = {k for k, _, _ in prev}
    added = [it for it in cur if it[0] not in prev_keys]
    removed = [it for it in prev if it[0] not in cur_keys]
    unchanged = [it for it in cur if it[0] in prev_keys]
    summary = {"added": len(added), "removed": len(removed), "unchanged": len(unchanged), "strokes": len(cur)}

    if out_path != "-":
        from PIL import Image, ImageDraw, ImageFont

        w, h = int(round(PAGE_W * scale)), int(round(PAGE_H * scale))
        img = Image.new("RGB", (w, h), BG)
        draw = ImageDraw.Draw(img)

        def colour(ai, status):
            if clean:
                return BLUE if ai else BLACK
            if status == "removed":
                return RED
            if ai:
                return BLUE if status == "added" else BLUE_OLD
            return BLACK if status == "added" else GREY

        def draw_stroke(pts, fill):
            if len(pts) == 3:
                x, y, r = pts[0] / 10 * scale, pts[1] / 10 * scale, max(pts[2] / 10 * scale, 1)
                draw.ellipse([x - r, y - r, x + r, y + r], fill=fill)
                return
            width = max(int(round(pts[2] / 10 * 2 * scale)), 1)
            xy = [(pts[j] / 10 * scale, pts[j + 1] / 10 * scale) for j in range(0, len(pts) - 2, 3)]
            draw.line(xy, fill=fill, width=width, joint="curve")

        def draw_text(txt, x, y, size, fill):
            px = max(int(round(size / 10 * scale)), 6)
            try:
                font = ImageFont.load_default(size=px)
            except TypeError:  # very old Pillow: fixed-size bitmap font
                font = ImageFont.load_default()
            try:
                draw.text((x / 10 * scale, y / 10 * scale), txt, fill=fill, font=font, anchor="ls")
            except (ValueError, TypeError):
                draw.text((x / 10 * scale, y / 10 * scale - px), txt, fill=fill, font=font)

        # removed ink underneath, unchanged next, added on top: the new
        # strokes must stay legible where they overlap old ones.
        layers = [] if clean else [(it, "removed") for it in removed]
        layers += [(it, "unchanged") for it in unchanged] + [(it, "added") for it in added]
        for (key, kind, payload), status in layers:
            if kind == "stroke":
                pts, ai = payload
                draw_stroke(pts, colour(ai, status))
            else:
                txt, x, y, size, ai = payload
                draw_text(txt, x, y, size, colour(ai, status))
        img.save(out_path, "PNG", optimize=True)

    print(json.dumps(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
