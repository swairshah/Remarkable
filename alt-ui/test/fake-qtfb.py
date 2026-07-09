#!/usr/bin/env python3
"""Fake AppLoad/qtfb server for Paper's `make preview` (no tablet).

Plays the server side of the qtfb protocol (see src/qtfb.rs): backs the
framebuffer with /dev/shm, launches the app under qemu with a FAKE pi
(test/fake-pi.py) wired in via PAPER_PI_BIN, then scripts a session and
screenshots along the way.

The container has no Wacom device, so the app falls back to AppLoad pen
events — which is exactly what we script here.

PAPER_SCENARIO picks the scripted session (one per milestone):
  m0    white canvas: scribble, top-edge swipe -> CLOSE, tap CLOSE -> exit
  m1    doc model: book open/ink/flip/persist, notebook quick-sheets grow,
        ink persistence across an app restart (three app sessions)
"""
import json
import math
import os
import socket
import struct
import subprocess
import sys
import time
import zlib


def copy_tree(src, dst):
    """python3-minimal has no shutil; this is all we need of copytree."""
    os.makedirs(dst, exist_ok=True)
    for name in os.listdir(src):
        s, d = os.path.join(src, name), os.path.join(dst, name)
        if os.path.isdir(s):
            copy_tree(s, d)
        else:
            with open(s, "rb") as fi, open(d, "wb") as fo:
                fo.write(fi.read())

W, H = 1404, 1872
SHM_KEY = 7

SHM_PATH = f"/dev/shm/qtfb_{SHM_KEY}"
SOCK_PATH = "/tmp/qtfb.sock"
DATA_DIR = "/tmp/au-data"

PEN_PRESS, PEN_RELEASE, PEN_UPDATE = 0x20, 0x21, 0x22
TOUCH_PRESS, TOUCH_RELEASE, TOUCH_UPDATE = 0x10, 0x11, 0x12
MESSAGE_USERINPUT = 4

CLOSE_TAP_X, CLOSE_TAP_Y = 1318, 44  # inside the CLOSE button (now top-RIGHT)


def write_png(path):
    raw = memoryview(open(SHM_PATH, "rb").read()).cast("H")
    gray = bytes(((v >> 5) & 0x3F) * 255 // 63 for v in raw)

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    ihdr = struct.pack(">IIBBBBB", W, H, 8, 0, 0, 0, 0)
    rows = b"".join(b"\x00" + gray[y * W:(y + 1) * W] for y in range(H))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) +
                chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
    print(f"fake-qtfb: wrote {path}")


class Harness:
    """Owns the qtfb server socket; launches app sessions on demand."""

    def __init__(self, app_bin):
        self.app_bin = app_bin
        with open(SHM_PATH, "wb") as f:
            f.truncate(W * H * 2)
        if os.path.exists(SOCK_PATH):
            os.remove(SOCK_PATH)
        self.srv = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        self.srv.bind(SOCK_PATH)
        self.srv.listen(1)
        self.here = os.path.dirname(os.path.abspath(__file__))
        self.launched = []

    def cleanup(self):
        for app in self.launched:
            if app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    app.kill()

    def launch(self, **env_extra):
        env = dict(os.environ,
                   QTFB_KEY="12345",
                   PAPER_PI_BIN=os.path.join(self.here, "fake-pi.py"),
                   PAPER_DATA_DIR=DATA_DIR,
                   PAPER_SOCK="/tmp/au.sock",
                   HOME="/tmp")
        env.pop("PAPER_OPEN", None)
        env.update(env_extra)
        app = subprocess.Popen(["qemu-arm-static", self.app_bin], env=env)
        self.launched.append(app)
        conn, _ = self.srv.accept()
        init = conn.recv(64)
        assert init[0] == 0, init[0]
        conn.send(struct.pack("<B3xiI12x", 0, SHM_KEY, W * H * 2))
        return Session(conn, app)


class Session:
    """One accepted app connection + input/screenshot helpers."""

    def __init__(self, conn, app):
        self.conn = conn
        self.app = app

    def pen(self, itype, x, y, d=0):
        self.conn.send(struct.pack("<B3xiiiii", MESSAGE_USERINPUT, itype, 0, x, y, d))

    def touch(self, itype, x, y):
        self.conn.send(struct.pack("<B3xiiiii", MESSAGE_USERINPUT, itype, 0, x, y, 0))

    def drain(self, seconds):
        self.conn.settimeout(seconds)
        end = time.time() + seconds
        try:
            while time.time() < end:
                self.conn.recv(64)
        except (socket.timeout, OSError):
            pass

    def drain_counting(self, seconds):
        """Drain, counting UPDATE messages and summing their rect areas."""
        self.conn.settimeout(seconds)
        end = time.time() + seconds
        count, area = 0, 0
        try:
            while time.time() < end:
                msg = self.conn.recv(64)
                if len(msg) >= 24 and msg[0] == 1:  # MESSAGE_UPDATE
                    ut, x, y, w, hh = struct.unpack_from("<iiiii", msg, 4)
                    count += 1
                    area += (w * hh) if ut == 1 else (W * H)
        except (socket.timeout, OSError):
            pass
        return count, area

    def swipe(self, x0, x1, y=900):
        step = (x1 - x0) // 11
        self.touch(TOUCH_PRESS, x0, y)
        for i in range(1, 12):
            self.touch(TOUCH_UPDATE, x0 + i * step, y)
        self.touch(TOUCH_RELEASE, x1, y)

    def swipe_down_from_top(self, x=700):
        self.touch(TOUCH_PRESS, x, 4)
        for i in range(1, 12):
            self.touch(TOUCH_UPDATE, x, 4 + i * 14)
        self.touch(TOUCH_RELEASE, x, 4 + 12 * 14)

    def tap(self, x, y):
        self.touch(TOUCH_PRESS, x, y)
        self.touch(TOUCH_RELEASE, x, y)

    def squiggle(self, x0, y0, n=50, dx=9, amp=24):
        self.pen(PEN_PRESS, x0, y0)
        for i in range(1, n):
            self.pen(PEN_UPDATE, x0 + i * dx, y0 + int(amp * math.sin(i / 3)))
        self.pen(PEN_RELEASE, x0 + n * dx, y0)

    def expect_exit(self, why):
        try:
            self.app.wait(timeout=6)
        except subprocess.TimeoutExpired:
            raise AssertionError(f"app did not exit ({why})")
        print(f"fake-qtfb: app exited rc={self.app.returncode} ({why})")
        assert self.app.returncode == 0, f"app exit rc={self.app.returncode}"

    def terminate_clean(self):
        self.app.terminate()  # SIGTERM -> save_all -> clean exit
        self.expect_exit("SIGTERM")


def scenario_m0(h, out_png):
    """Smoke test: empty home renders, top bar reveals, CLOSE exits clean."""
    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    s = h.launch(PAPER_FAKE_SYS="1")
    time.sleep(1.5)  # first paint
    write_png(out_png.replace(".png", "-root.png"))

    s.swipe_down_from_top()
    s.drain(1.0)
    write_png(out_png)  # top bar with CLOSE visible

    s.tap(CLOSE_TAP_X, CLOSE_TAP_Y)
    s.expect_exit("CLOSE tap")


def scenario_m1(h, out_png):
    """Unified doc model: book + notebook + persistence across restart."""
    def shot(tag):
        write_png(out_png.replace(".png", f"-{tag}.png"))

    # fixtures: the mkbook testbook + a fresh notebook
    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    os.makedirs(f"{DATA_DIR}/docs/nb-test", exist_ok=True)
    copy_tree(os.path.join(h.here, "..", "build", "testbook", "docs", "demo-paper"),
              f"{DATA_DIR}/docs/demo-paper")
    with open(f"{DATA_DIR}/docs/nb-test/meta.json", "w") as f:
        json.dump({"v": 1, "kind": "notebook", "title": "Test Notebook"}, f)

    # -- session A: the book -------------------------------------------------
    s = h.launch(PAPER_OPEN="demo-paper")
    s.drain(2.2)  # open + raster decode + GC16
    shot("book-open")

    s.squiggle(220, 1700)  # ink in the bottom margin
    s.drain(1.5)
    shot("book-ink")

    time.sleep(1.7)  # palm rejection lapses
    s.swipe(1150, 190)  # flip forward -> printed page 2
    s.drain(2.2)
    shot("book-p2")

    s.swipe(190, 1150)  # flip back -> page 1, ink re-rendered from vectors
    s.drain(2.2)
    shot("book-back")

    s.swipe_down_from_top()
    s.drain(0.8)
    s.tap(CLOSE_TAP_X, CLOSE_TAP_Y)
    s.expect_exit("CLOSE tap")

    # -- session B: the notebook (quick-sheets growth) -----------------------
    s = h.launch(PAPER_OPEN="nb-test")
    time.sleep(1.5)
    s.squiggle(200, 700, n=60)
    s.drain(1.0)
    shot("nb-p1")

    time.sleep(1.7)
    s.swipe(1150, 190)  # inked page: flipping forward GROWS the notebook
    s.drain(1.8)
    shot("nb-p2")  # blank fresh page (indicator 2/2 may still show)

    s.swipe(1150, 190)  # blank page: must NOT grow (still 2 pages)
    s.drain(1.5)
    s.swipe(190, 1150)  # back to page 1
    s.drain(1.8)
    shot("nb-back")  # page-1 ink persisted

    s.terminate_clean()

    # -- session C: resume via settings.json (no PAPER_OPEN) -----------------
    s = h.launch()
    time.sleep(1.8)
    shot("nb-resume")  # last_doc=nb-test, page 1 ink straight from disk
    s.terminate_clean()

    st = json.load(open(f"{DATA_DIR}/docs/nb-test/state.json"))
    assert len(st["seq"]) == 2, f"notebook should have exactly 2 pages, has {st['seq']}"
    print("fake-qtfb: m1 assertions passed")


def kb_tap(s, ch):
    """Tap one key on the kb.rs keyboard (geometry mirrored from kb.rs)."""
    ROWS = ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"]
    KEY_H, GAP, TITLE_H = 96, 10, 110
    KB_H = 5 * KEY_H + 6 * GAP + TITLE_H
    y0 = H - KB_H - 40
    if ch in ("OK", "CANCEL", "SPACE", "DEL"):
        widths = [280, 520, 240, 280]
        order = ["CANCEL", "SPACE", "DEL", "OK"]
        y = y0 + TITLE_H + 4 * (KEY_H + GAP) + KEY_H // 2
        x = (W - (sum(widths) + GAP * 3)) // 2
        for label, w in zip(order, widths):
            if label == ch:
                s.tap(x + w // 2, y)
                return
            x += w + GAP
        return
    for ri, row in enumerate(ROWS):
        if ch in row:
            kw = (W - 48 - GAP * (len(row) - 1)) // len(row)
            x0 = (W - (kw * len(row) + GAP * (len(row) - 1))) // 2
            i = row.index(ch)
            s.tap(x0 + i * (kw + GAP) + kw // 2,
                  y0 + TITLE_H + ri * (KEY_H + GAP) + KEY_H // 2)
            return
    raise ValueError(ch)


def dialog_row_tap(s, nrows, i):
    """Tap dialog row i (geometry mirrored from main.rs dialog_rect)."""
    DLG_W, DLG_ROW_H, TITLE_PAD = 760, 96, 84
    h = TITLE_PAD + nrows * DLG_ROW_H + 24
    y0 = (H - h) // 2
    s.tap(W // 2, y0 + TITLE_PAD + i * DLG_ROW_H + DLG_ROW_H // 2)


def long_press(s, x, y, ms=900):
    s.touch(TOUCH_PRESS, x, y)
    time.sleep(ms / 1000.0)
    s.touch(TOUCH_RELEASE, x, y)


def write_legacy_fixtures():
    """A reader book bundle + two notebook pages, for import_legacy."""
    here = os.path.dirname(os.path.abspath(__file__))
    subprocess.run(["rm", "-rf", "/tmp/legacy-books", "/tmp/legacy-nb"], check=False)
    copy_tree(os.path.join(here, "..", "build", "testbook", "docs", "demo-paper"),
              "/tmp/legacy-books/demo-paper")
    os.makedirs("/tmp/legacy-nb", exist_ok=True)
    for n in (1, 2):
        pts = []
        for i in range(40):
            pts += [(2000 + i * 200) , (6000 + n * 1000 + int(300 * math.sin(i / 3))), 25]
        with open(f"/tmp/legacy-nb/page-{n:04}.json", "w") as f:
            json.dump({"v": 1, "next_patch": 1, "patches": [],
                       "strokes": [{"g": 0, "p": pts}]}, f)


def scenario_m2(h, out_png):
    """Home grid + status bar + dialogs + folders + legacy import."""
    def shot(tag):
        write_png(out_png.replace(".png", f"-{tag}.png"))

    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    write_legacy_fixtures()
    env = dict(PAPER_FAKE_SYS="1",
               PAPER_LEGACY_READER="/tmp/legacy-books",
               PAPER_LEGACY_NOTEBOOK="/tmp/legacy-nb")

    # -- launch: import runs, home grid renders with lazy thumbs -----------
    s = h.launch(**env)
    s.drain(3.5)
    shot("home")  # status bar + [imported notebook, demo paper] cells

    # -- long-press the imported notebook (cell 0) -> menu -> move to folder
    long_press(s, 234, 480)
    s.drain(0.8)
    shot("menu")
    dialog_row_tap(s, 5, 1)  # MOVE TO FOLDER
    s.drain(0.8)
    dialog_row_tap(s, 3, 1)  # NEW FOLDER ...
    s.drain(0.8)
    shot("kb")
    for ch in "papers":
        kb_tap(s, ch)
        s.drain(0.15)
    kb_tap(s, "OK")
    s.drain(2.0)
    shot("folder-root")  # folder cell + demo paper

    # -- into the folder and back ------------------------------------------
    s.tap(234, 480)  # the folder cell
    s.drain(1.5)
    shot("in-folder")
    s.tap(300, 170)  # breadcrumb "< papers" -> root
    s.drain(1.5)

    # -- new notebook: create, ink, top bar, back to files ------------------
    s.tap(W - 48 - 150, 96 + 32)  # [+ NOTEBOOK]
    s.drain(1.8)
    s.squiggle(300, 800, n=40)
    s.drain(1.0)
    time.sleep(1.7)  # let palm rejection lapse before the finger swipe
    s.swipe_down_from_top()
    s.drain(1.0)
    shot("topbar")
    s.tap(16 + 130, 44)  # MY FILES (now at the left edge, FILES_X0=16)
    s.drain(3.0)
    shot("home2")  # new notebook first, with its ink thumbnail

    # -- delete the new notebook --------------------------------------------
    time.sleep(0.3)
    long_press(s, 702, 480)  # cell 1 = the new notebook (after folder cell 0)
    s.drain(0.8)
    dialog_row_tap(s, 5, 3)  # DELETE
    s.drain(0.8)
    shot("confirm")
    dialog_row_tap(s, 2, 0)  # DELETE, really
    s.drain(1.5)
    shot("deleted")

    s.terminate_clean()

    # -- filesystem assertions ----------------------------------------------
    docs = sorted(os.listdir(f"{DATA_DIR}/docs"))
    assert "demo-paper" in docs, docs
    assert "nb-imported" in docs, docs
    assert len(docs) == 2, f"expected 2 docs after delete, got {docs}"
    meta = json.load(open(f"{DATA_DIR}/docs/nb-imported/meta.json"))
    assert meta["folder"] == "papers", meta
    folders = json.load(open(f"{DATA_DIR}/folders.json"))
    assert "papers" in folders["folders"], folders
    assert os.path.exists(f"{DATA_DIR}/.import-done")
    assert not os.listdir("/tmp/legacy-books"), "legacy books should have moved"
    assert not [f for f in os.listdir("/tmp/legacy-nb")], "legacy pages should have moved"
    assert os.path.exists(f"{DATA_DIR}/docs/nb-imported/thumb.png"), "thumb cache missing"
    print("fake-qtfb: m2 assertions passed")


# toolbar geometry mirror (src/toolbar.rs)
TB_CX = W - 104 // 2  # strip button center x = 1352
TB_TOGGLE = (W - 60, 60)
# button-center y for each toolbar slot (mirrors src/toolbar.rs slot_y):
# TB_TOP=132, TB_BTN_H=96, DIV_H=25, dividers after lasso/redo/next/home.
TB_BTN = {"pen": 180, "eraser": 276, "lasso": 372, "undo": 493, "redo": 589,
          "prev": 710, "goto": 806, "next": 902, "font": 1023, "home": 1119}


def pen_tap(s, x, y):
    s.pen(PEN_PRESS, x, y)
    s.pen(PEN_RELEASE, x, y)


def np_key_center(label):
    """Numpad geometry mirror (src/main.rs np_rect/np_btn_xy)."""
    keys = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "DEL", "0", "GO"]
    i = keys.index(label)
    w, h = 3 * 170 + 2 * 12 + 56, 90 + 4 * (110 + 12) + 28
    x0, y0 = (W - w) // 2, (H - h) // 2
    col, row = i % 3, i // 3
    return (x0 + 28 + col * (170 + 12) + 85, y0 + 90 + row * (110 + 12) + 55)


def scenario_garamond(h, out_png):
    """pi writes in typeset Garamond: renders on device, persists as a text
    run, rubber-erases whole, and undoes."""
    def shot(tag):
        write_png(out_png.replace(".png", f"-{tag}.png"))

    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    os.makedirs(f"{DATA_DIR}/docs/nb/ink", exist_ok=True)
    with open(f"{DATA_DIR}/docs/nb/meta.json", "w") as f:
        json.dump({"v": 1, "kind": "notebook", "title": "Garamond"}, f)
    ink_path = f"{DATA_DIR}/docs/nb/ink/note-0001.json"

    s = h.launch(PAPER_OPEN="nb", PAPER_FAKE_SYS="1", FAKE_PI_SCRIPT="garamond")
    s.drain(1.8)
    s.squiggle(300, 1000, n=30)   # arm the pause
    s.drain(6.0)                  # send + pi draws the Garamond patch + settle
    shot("typeset")

    ink = json.load(open(ink_path))
    texts = [t for p in ink["patches"] for t in p.get("texts", [])]
    assert len(texts) == 2, f"two Garamond runs expected: {texts}"
    assert "Electronic" in texts[0]["t"], texts[0]
    print(f"fake-pi: garamond persisted {len(texts)} runs")

    # dark pixels in the text band (y 460-640, x 180-1120) = the rendered type
    def text_ink():
        raw = memoryview(open(SHM_PATH, "rb").read()).cast("H")
        n = 0
        for y in range(460, 640):
            for x in range(180, 1120):
                v = raw[y * W + x]
                if ((v >> 5) & 0x3F) * 255 // 63 < 100:
                    n += 1
        return n
    rendered = text_ink()
    print(f"fake-qtfb: garamond rendered ink px = {rendered}")
    assert rendered > 2000, f"Garamond should render on device: {rendered}"

    # rubber-erase the first Garamond line (scrub across ~y505)
    pen_tap(s, *TB_TOGGLE)
    s.drain(0.4)
    pen_tap(s, TB_CX, TB_BTN["eraser"])
    s.drain(0.4)
    s.pen(PEN_PRESS, 250, 505)
    for i in range(1, 30):
        s.pen(PEN_UPDATE, 250 + i * 24, 505)
    s.pen(PEN_RELEASE, 250 + 30 * 24, 505)
    s.drain(1.5)
    shot("erased")
    after_erase = text_ink()
    print(f"fake-qtfb: after erase ink px = {after_erase}")
    assert after_erase < rendered * 0.55, f"rubber should remove the Garamond line: {after_erase} vs {rendered}"

    # undo -> Garamond back
    pen_tap(s, TB_CX, TB_BTN["undo"])
    s.drain(1.2)
    shot("undo")
    restored = text_ink()
    print(f"fake-qtfb: after undo ink px = {restored}")
    assert restored > rendered * 0.9, f"undo should restore the Garamond: {restored} vs {rendered}"
    s.terminate_clean()
    print("fake-qtfb: garamond assertions passed")


def scenario_fontflip(h, out_png):
    """Font picker persists pi's face; page flips use gentle GL16 (no GC16
    flash) until the periodic deghost."""
    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    os.makedirs(f"{DATA_DIR}/docs/nb/ink", exist_ok=True)
    with open(f"{DATA_DIR}/docs/nb/meta.json", "w") as f:
        json.dump({"v": 1, "kind": "notebook", "title": "Fonts"}, f)

    s = h.launch(PAPER_OPEN="nb", PAPER_FAKE_SYS="1")
    s.drain(1.5)
    pen_tap(s, *TB_TOGGLE)
    s.drain(0.4)

    # open the font picker (the "Aa" slot) and choose Sans
    pen_tap(s, TB_CX, TB_BTN["font"])
    s.drain(0.8)
    write_png(out_png.replace(".png", "-picker.png"))
    # rows: Script / Serif / Sans / Garamond / CANCEL (5) — mirrors dialog_rect
    DLG_W, DLG_ROW_H, TITLE_PAD = 760, 96, 84
    hgt = TITLE_PAD + 5 * DLG_ROW_H + 24
    y0 = (H - hgt) // 2
    pen_tap(s, W // 2, y0 + TITLE_PAD + 2 * DLG_ROW_H + DLG_ROW_H // 2)  # Sans (row 2)
    s.drain(0.8)
    s.terminate_clean()
    st = json.load(open(f"{DATA_DIR}/settings.json"))
    assert st.get("pi_font") == "sans", f"font should persist as sans: {st}"

    # gentle flips: count GC16 full-refreshes over 4 page turns (expect 0)
    s = h.launch(PAPER_OPEN="nb", PAPER_FAKE_SYS="1")
    s.drain(1.8)
    s.squiggle(200, 700, n=30)   # give page 1 some ink
    s.drain(0.6)
    time.sleep(1.7)
    full_refreshes = 0
    for _ in range(4):
        s.conn.settimeout(2.0)
        s.swipe(1100, 200, y=1500)  # forward (grows notebook), avoid the strip
        end = time.time() + 2.0
        try:
            while time.time() < end:
                msg = s.conn.recv(64)
                if len(msg) >= 8 and msg[0] == 6:  # MESSAGE_REQUEST_FULL_REFRESH
                    full_refreshes += 1
        except (socket.timeout, OSError):
            pass
        time.sleep(1.7)
    print(f"fake-qtfb: fontflip GC16 full-refreshes over 4 flips = {full_refreshes}")
    write_png(out_png)
    assert full_refreshes == 0, f"flips should be gentle (no GC16 flash): {full_refreshes}"
    s.terminate_clean()
    print("fake-qtfb: fontflip assertions passed")


def scenario_redobug(h, out_png):
    """Draw ONE stroke, undo it, check the redo button lights up."""
    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    os.makedirs(f"{DATA_DIR}/docs/nb/ink", exist_ok=True)
    with open(f"{DATA_DIR}/docs/nb/meta.json", "w") as f:
        json.dump({"v": 1, "kind": "notebook", "title": "Redo"}, f)
    s = h.launch(PAPER_OPEN="nb", PAPER_FAKE_SYS="1")
    s.drain(1.5)
    pen_tap(s, *TB_TOGGLE)
    s.drain(0.4)
    s.squiggle(200, 700, n=40)
    s.drain(0.6)
    pen_tap(s, TB_CX, TB_BTN["undo"])
    s.drain(0.6)
    write_png(out_png)

    def darkness(cy):
        raw = memoryview(open(SHM_PATH, "rb").read()).cast("H")
        n = 0
        for y in range(cy - 30, cy + 30):
            for x in range(1315, 1390):
                v = raw[y * W + x]
                if ((v >> 5) & 0x3F) * 255 // 63 < 100:
                    n += 1
        return n
    def buttons(tag=None):
        if tag:
            write_png(out_png.replace(".png", f"-{tag}.png"))
        return darkness(493), darkness(589)

    undo_d, redo_d = buttons("stroke-undo")
    print(f"fake-qtfb: redobug stroke undo-btn={undo_d} redo-btn={redo_d}")
    assert undo_d < 60, f"undo should be greyed (empty stack): {undo_d}"
    assert redo_d > 120, f"redo should be ENABLED after undoing a stroke: {redo_d}"

    # ERASE then undo -> redo must light up
    pen_tap(s, TB_CX, TB_BTN["redo"])  # bring the stroke back
    s.drain(0.4)
    pen_tap(s, TB_CX, TB_BTN["eraser"])
    s.drain(0.3)
    s.pen(PEN_PRESS, 200, 700)
    for i in range(1, 30):
        s.pen(PEN_UPDATE, 200 + i * 12, 700)
    s.pen(PEN_RELEASE, 200 + 30 * 12, 700)
    s.drain(1.0)
    pen_tap(s, TB_CX, TB_BTN["undo"])  # undo the erase
    s.drain(0.8)
    u, r = buttons("erase-undo")
    print(f"fake-qtfb: redobug erase undo-btn={u} redo-btn={r}")
    assert r > 120, f"redo should be ENABLED after undoing an erase: {r}"

    # MOVE then undo -> redo must light up
    pen_tap(s, TB_CX, TB_BTN["lasso"])
    s.drain(0.3)
    lasso_loop(s, 500, 700, 380)
    s.drain(0.8)
    s.pen(PEN_PRESS, 500, 700)
    for i in range(1, 20):
        s.pen(PEN_UPDATE, 500, 700 + i * 20)
    s.pen(PEN_RELEASE, 500, 1080)
    s.drain(1.0)
    pen_tap(s, TB_CX, TB_BTN["undo"])  # undo the move
    s.drain(0.8)
    u, r = buttons("move-undo")
    print(f"fake-qtfb: redobug move undo-btn={u} redo-btn={r}")
    assert r > 120, f"redo should be ENABLED after undoing a move: {r}"

    s.terminate_clean()
    print("fake-qtfb: redobug assertions passed")


def scenario_m3(h, out_png):
    """Toolbar, tools, undo/redo, erase-undo, id persistence, numpad."""
    def shot(tag):
        write_png(out_png.replace(".png", f"-{tag}.png"))

    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    os.makedirs(f"{DATA_DIR}/docs/nb-test", exist_ok=True)
    with open(f"{DATA_DIR}/docs/nb-test/meta.json", "w") as f:
        json.dump({"v": 1, "kind": "notebook", "title": "Undo Lab"}, f)

    s = h.launch(PAPER_OPEN="nb-test", PAPER_FAKE_SYS="1")
    s.drain(1.8)

    pen_tap(s, *TB_TOGGLE)  # expand the toolbar
    s.drain(1.0)
    shot("toolbar")

    s.squiggle(200, 600, n=40)   # stroke A
    s.drain(0.6)
    s.squiggle(200, 900, n=40)   # stroke B
    s.drain(0.8)
    shot("ink2")

    pen_tap(s, TB_CX, TB_BTN["undo"])
    s.drain(0.8)
    shot("undo1")               # B gone, A remains

    pen_tap(s, TB_CX, TB_BTN["redo"])
    s.drain(0.8)
    shot("redo")                # B back

    pen_tap(s, TB_CX, TB_BTN["eraser"])
    s.drain(0.5)
    s.pen(PEN_PRESS, 180, 900)  # scrub along stroke B
    for i in range(1, 30):
        s.pen(PEN_UPDATE, 180 + i * 14, 900)
    s.pen(PEN_RELEASE, 180 + 30 * 14, 900)
    s.drain(1.2)
    shot("erased")              # B gone again (as an ERASE op)

    pen_tap(s, TB_CX, TB_BTN["undo"])
    s.drain(0.8)
    shot("undo-erase")          # B restored by undoing the erase

    # flip away and back: ids + stacks must survive the round trip
    time.sleep(1.7)
    s.swipe(1000, 200, y=1500)  # forward (page grows; avoid the strip rows)
    s.drain(1.8)
    s.swipe(200, 1000, y=1500)  # back to page 1
    s.drain(1.8)
    pen_tap(s, TB_CX, TB_BTN["undo"])  # undoes addStroke(B) after the flip
    s.drain(0.8)
    shot("undo-after-flip")     # only A remains

    # toolbar swallow: pen tool back on, stroke runs UNDER the strip
    pen_tap(s, TB_CX, TB_BTN["pen"])
    s.drain(0.3)
    s.pen(PEN_PRESS, 1000, 1500)
    for i in range(1, 40):
        s.pen(PEN_UPDATE, 1000 + i * 10, 1500 + i)
    s.pen(PEN_RELEASE, 1398, 1540)
    s.drain(1.0)
    shot("swallow")             # ink to the edge, toolbar chrome intact

    # numpad jump to page 2
    pen_tap(s, TB_CX, TB_BTN["goto"])
    s.drain(0.8)
    shot("numpad")
    pen_tap(s, *np_key_center("2"))
    s.drain(0.3)
    pen_tap(s, *np_key_center("GO"))
    s.drain(1.8)
    shot("page2")

    s.terminate_clean()

    ink1 = json.load(open(f"{DATA_DIR}/docs/nb-test/ink/note-0001.json"))
    assert len(ink1["strokes"]) == 2, f"page 1 should hold A + the edge stroke, got {len(ink1['strokes'])}"
    assert all("i" in s and s["i"] > 0 for s in ink1["strokes"]), "stroke ids missing"
    st = json.load(open(f"{DATA_DIR}/docs/nb-test/state.json"))
    assert st["pos"] == 1, f"should end on page 2, got {st}"
    print("fake-qtfb: m3 assertions passed")


def hline(y, x0=300, x1=700, r=25):
    """One horizontal stroke in page-JSON form (coords x10)."""
    pts = []
    for i in range(21):
        x = x0 + (x1 - x0) * i // 20
        pts += [x * 10, y * 10, r]
    return pts


def lasso_loop(s, cx, cy, r, steps=26):
    """Draw a closed-ish loop with the pen (the lasso tool active)."""
    import math as m
    s.pen(PEN_PRESS, int(cx + r), int(cy))
    for i in range(1, steps + 1):
        a = 2 * m.pi * i / steps
        s.pen(PEN_UPDATE, int(cx + r * m.cos(a)), int(cy + r * m.sin(a)))
    s.pen(PEN_RELEASE, int(cx + r), int(cy))


def scenario_m4(h, out_png):
    """Lasso: select user+AI strokes, drag-move, delete/cut, undo/redo."""
    def shot(tag):
        write_png(out_png.replace(".png", f"-{tag}.png"))

    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    os.makedirs(f"{DATA_DIR}/docs/nb-sel/ink", exist_ok=True)
    with open(f"{DATA_DIR}/docs/nb-sel/meta.json", "w") as f:
        json.dump({"v": 1, "kind": "notebook", "title": "Lasso Lab"}, f)
    # two user strokes + one AI patch stroke, stacked around y=500..620
    with open(f"{DATA_DIR}/docs/nb-sel/ink/note-0001.json", "w") as f:
        json.dump({"v": 1, "next_patch": 2, "patches": [
                       {"id": 1, "strokes": [{"g": 110, "p": hline(620)}]}],
                   "strokes": [{"g": 0, "p": hline(500)},
                               {"g": 0, "p": hline(560)}]}, f)

    s = h.launch(PAPER_OPEN="nb-sel", PAPER_FAKE_SYS="1")
    s.drain(1.8)

    pen_tap(s, *TB_TOGGLE)      # expand toolbar
    s.drain(0.6)
    pen_tap(s, TB_CX, TB_BTN["lasso"])
    s.drain(0.5)

    lasso_loop(s, 500, 560, 280)
    s.drain(1.2)
    shot("selected")            # dashed box + DELETE/CUT chips

    # drag from inside the box by (+400, +440); count update messages
    s.pen(PEN_PRESS, 500, 560)
    for i in range(1, 25):
        s.pen(PEN_UPDATE, 500 + i * 16, 560 + int(i * 17.6))
        time.sleep(0.02)
    n_updates, area = s.drain_counting(0.5)
    s.pen(PEN_RELEASE, 900, 1000)
    s.drain(1.5)
    shot("moved")               # ink itself moved; box follows
    assert n_updates < 90, f"drag repainted too often: {n_updates}"
    if n_updates:
        mean = area / n_updates
        assert mean < 900_000, f"drag repaints too large: mean {mean:.0f} px"

    pen_tap(s, TB_CX, TB_BTN["undo"])   # undo the move (selection dismissed)
    s.drain(1.2)
    shot("undo-move")
    pen_tap(s, TB_CX, TB_BTN["redo"])   # move again
    s.drain(1.2)
    shot("redo-move")

    # re-lasso at the moved spot, DELETE via chip, then undo.
    # The ring hugs the STROKES' bbox (moved to x 700..1100, y 940..1060):
    # ring y0 = 940-12 = 928, chips 14+64 above, centered on x=900.
    lasso_loop(s, 900, 1000, 300)
    s.drain(1.2)
    pen_tap(s, 900 - 175 + 85, 928 - 14 - 64 + 32)  # DELETE chip center
    s.drain(1.2)
    shot("deleted")
    pen_tap(s, TB_CX, TB_BTN["undo"])
    s.drain(1.2)
    shot("undo-delete")

    # empty lasso in a blank corner: silent no-op
    lasso_loop(s, 350, 1500, 120)
    s.drain(1.0)
    shot("empty")

    s.terminate_clean()

    ink = json.load(open(f"{DATA_DIR}/docs/nb-sel/ink/note-0001.json"))
    assert len(ink["strokes"]) == 2, f"user strokes: {len(ink['strokes'])}"
    assert len(ink["patches"]) == 1 and ink["patches"][0]["id"] == 1, "patch identity lost"
    # everything sits at the moved position: y (x10) of the first user
    # stroke should be ~ (500+440)*10
    y10 = ink["strokes"][0]["p"][1]
    assert abs(y10 - 9400) < 200, f"moved y: {y10}"
    py10 = ink["patches"][0]["strokes"][0]["p"][1]
    assert abs(py10 - 10600) < 200, f"moved patch y: {py10}"
    print("fake-qtfb: m4 assertions passed")


def scenario_m5_book(h, out_png):
    """pi as margin companion: underline, margin note, inserted note page."""
    def shot(tag):
        write_png(out_png.replace(".png", f"-{tag}.png"))

    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    os.makedirs(f"{DATA_DIR}/docs", exist_ok=True)
    copy_tree(os.path.join(h.here, "..", "build", "testbook", "docs", "demo-paper"),
              f"{DATA_DIR}/docs/demo-paper")

    s = h.launch(PAPER_OPEN="demo-paper", PAPER_FAKE_SYS="1", FAKE_PI_SCRIPT="book")
    s.drain(2.2)

    s.squiggle(220, 1700)      # user ink in the bottom margin
    s.drain(4.2)               # idle 2.8s -> page sent; fake pi thinks 1s
    shot("thinking")           # the working dot
    s.drain(10.0)              # underline + margin note + note page, animated
    shot("reply")

    time.sleep(1.7)
    s.swipe(1150, 190)         # the inserted NOTE page
    s.drain(2.5)
    shot("note")
    s.swipe(190, 1150)         # back
    s.drain(2.5)
    s.terminate_clean()

    st = json.load(open(f"{DATA_DIR}/docs/demo-paper/state.json"))
    assert len(st["seq"]) == 3, f"note page should be inserted: {st['seq']}"
    ink1 = json.load(open(f"{DATA_DIR}/docs/demo-paper/ink/pdf-0001.json"))
    assert len(ink1["patches"]) == 2, f"underline + margin note expected: {len(ink1['patches'])}"
    note = json.load(open(f"{DATA_DIR}/docs/demo-paper/ink/note-0001.json"))
    assert len(note["patches"]) == 1, "note-page draw missing"
    print("fake-qtfb: m5-book assertions passed")


def scenario_m5_nb(h, out_png):
    """pi as co-writer + pause suppression while a selection is active."""
    def shot(tag):
        write_png(out_png.replace(".png", f"-{tag}.png"))

    subprocess.run(["rm", "-rf", DATA_DIR], check=False)
    os.makedirs(f"{DATA_DIR}/docs/nb-pi", exist_ok=True)
    with open(f"{DATA_DIR}/docs/nb-pi/meta.json", "w") as f:
        json.dump({"v": 1, "kind": "notebook", "title": "Co-writer"}, f)

    s = h.launch(PAPER_OPEN="nb-pi", PAPER_FAKE_SYS="1", FAKE_PI_SCRIPT="notebook")
    s.drain(1.8)

    s.squiggle(240, 500, n=45)          # arms the 2.8s pause
    # immediately lasso-select the ink: the pause must NOT fire while the
    # selection is up
    pen_tap(s, *TB_TOGGLE)
    pen_tap(s, TB_CX, TB_BTN["lasso"])
    lasso_loop(s, 440, 500, 220)
    time.sleep(4.5)                     # well past the idle window
    ink_path = f"{DATA_DIR}/docs/nb-pi/ink/note-0001.json"
    assert not os.path.exists(ink_path), "page was sent to pi during a live selection"
    shot("suppressed")

    pen_tap(s, 1000, 1400)              # tap outside: dismiss -> pause may fire
    s.drain(6.0)                        # send + fake pi thinks + draws + anim
    shot("cowriter")
    ink = json.load(open(ink_path))
    assert len(ink["patches"]) == 1, f"co-writer patch missing: {ink.get('patches')}"

    # the AI patch is one undoable op
    pen_tap(s, TB_CX, TB_BTN["undo"])
    s.drain(1.2)
    shot("undo-ai")
    ink = json.load(open(ink_path))
    # undo mutates in memory; force a save via page flip... simpler: redo
    pen_tap(s, TB_CX, TB_BTN["redo"])
    s.drain(1.2)
    shot("redo-ai")

    s.terminate_clean()
    ink = json.load(open(ink_path))
    assert len(ink["patches"]) == 1, "AI patch should survive undo+redo"
    print("fake-qtfb: m5-nb assertions passed")


SCENARIOS = {
    "m0": scenario_m0,
    "m1": scenario_m1,
    "m2": scenario_m2,
    "m3": scenario_m3,
    "m4": scenario_m4,
    "m5-book": scenario_m5_book,
    "m5-nb": scenario_m5_nb,
    "redobug": scenario_redobug,
    "fontflip": scenario_fontflip,
    "garamond": scenario_garamond,
}


def main():
    app_bin, out_png = sys.argv[1], sys.argv[2]
    scenario = SCENARIOS[os.environ.get("PAPER_SCENARIO", "m1")]
    h = Harness(app_bin)
    try:
        scenario(h, out_png)
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
