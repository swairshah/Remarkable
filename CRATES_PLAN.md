# Crate extraction plan (libreink-*)

Design for pulling the shared code out of alt-ui / notebook / reader into a
Cargo workspace of library crates. Written 2026-07-11 after a full cross-app
diff; the numbers below come from that audit.

## Why

Each app duplicates ~4,800–5,400 lines across 17+ modules (~15k lines total).
Drift is already real: alt-ui's stock-quality page-turn waveforms, the GC16
temperature fix, the qtfb msync preview fix, grey text, and Garamond typeset
output never flowed back to notebook/reader. Nowhere do two apps solve the
same problem differently — the divergence is renames, alt-ui evolving ahead,
and app policy embedded in shared files. So extraction is adoption of alt-ui's
versions, not a merge.

## Prior art (checked 2026-07-11)

- `remarkable_lines` (crates.io): parses xochitl's proprietary `.rm` file
  format. No overlap — our ink is our own JSON. Possibly useful later for
  importing stock notebooks.
- `libremarkable` (0.7.0, MIT, maintained): overlaps only our display+input
  layer, and with a different architecture — rM1-first mxcfb ioctls, rM2 via
  the rm2fb *compat shim*, no AppLoad/qtfb concept, no refresh-strategy
  opinions, heavier deps. Decision: do not build on it, do not name ours
  "libremarkable2". Our platform crates are the modern-OS (3.20+),
  AppLoad-aware, takeover-native alternative.
- asivery's rm-appload qtfb crate: pixel output only; ours adds input events
  and refresh modes (already noted in qtfb.rs).

## Divergence audit summary

| Bucket | Modules | Reconciliation |
|---|---|---|
| A: byte-identical | fb, font, hershey_data, png (+png_dec in alt/reader) | move verbatim (~1,166 lines) |
| B: app-name strings only | touch, pen, power, ipc (4–8 lines each) | one `AppId` config (env prefix + log tag) |
| C: alt-ui strict superset | display (Wave::Page/Print), rm2fb (gc16_partial + 25°C temp pin, gl16_full), qtfb (msync), draw (blend_toward), text (grey levels, fallbacks), svg_ink (PiFont/Garamond), hershey (Face picker) | adopt alt-ui's copy; nb/rd inherit upgrades |
| D: structural | ink.rs — alt-ui superset (TextRun, Owner/OwnedStroke undo+move); notebook's `Notebook` container is app model, moves out; reader is minimal subset. pi_rpc.rs — transport identical; ~90% of the 277-line diff is SYSTEM_PROMPT/AGENT.md/library prose + config (extension path, session dir, env names); tiny deltas (alt kill(), nb send_page()) | ink: take superset, evict Notebook; pi: extract transport, parameterize prompts/config |

## The crates

The crates live in their own repo — `~/work/projects/libreink` (sibling of
this one), a Cargo workspace with `crates/*` — because they will be published
to crates.io and pushed to GitHub as a standalone project. The apps here use
relative path deps (`../../libreink/crates/...`) during development and will
switch to crates.io versions + a `[patch]` section once published. Dual
MIT/Apache-2.0; every libreink-* name was free on crates.io as of 2026-07-11.
Naming: `libreink-*` — CONFIRMED by user 2026-07-11 (rmx and slate were
vetoed; the name carries libremarkable's libre pun onto the medium). The
page-model crate is `libreink-page` and the SVG crate `libreink-svg`, so no
crate stutters as libreink-ink.
Structure decision: 7 focused crates, no facade (add one later if it hurts).

| Crate | From | Contents | Deps |
|---|---|---|---|
| `libreink-core` | fb, draw, font, png, png_dec | Framebuffer (RGB565), clip, fills/blends incl. blend_toward, 5×7 chrome font, PNG enc/dec, `AppId` config seam | libc |
| `libreink-display` | display, qtfb, rm2fb (alt-ui's) | `Wave` {Ink,Text,Page,Print}, backend trait update/update_all/full_refresh, runtime qtfb-vs-rm2fb selection, native swtfb client w/ temp pinning, qtfb client w/ input + msync | libreink-core, libc |
| `libreink-input` | touch, pen, power (kb later) | multitouch slots + flips + 5-finger quit, Wacom + EVIOCGRAB, grabbed power button + suspend | libreink-core, libc |
| `libreink-hershey` | hershey, hershey_data (alt-ui's) | stroke fonts, glyph→polylines, Face picker API | — |
| `libreink-text` | text (alt-ui's) | fontdue typesetting, glyph cache, grey-level draw, fallback chains; embedded EB Garamond + Google Sans Code as defaults, caller-suppliable fonts | libreink-core, fontdue |
| `libreink-page` | ink (alt-ui superset) | Pt/Stroke/Patch/TextRun/Rect/Band, darkest-wins stamping, re-render-from-vectors, JSON persistence, undo/move machinery (unconditional) | libreink-core, serde_json, libreink-text (feature `typeset`) |
| `libreink-svg` | svg_ink (alt-ui's) | SVG string → strokes + text runs in page coords: viewBox remap, path flatten, fill hatching, Hershey text, Garamond TextRuns. Zero device deps | libreink-page, libreink-hershey, libreink-text |
| `libreink-pi` | pi_rpc minus prose, ipc | `Pi` child (spawn `pi --mode rpc`, JSONL, drain/translate/PiEvent, image msgs, kill) + nonblocking unix tool socket. `PiConfig`: system prompt, extension path, session dir, socket env, AGENT.md bootstrap | serde_json, libc |

(That's 8 rows; libreink-pi is the +1 "independent" crate.)

Stays in apps: main.rs, prompts, tool semantics, UI chrome (home/doc/store/
toolbar/statusbar, library/live/md_view, book/import/xochitl), notebook's
`Notebook` page container, alt-ui's kb/select/undo.

Shared but non-Rust (alongside, later): TS helper for pi extensions (the 3
canvas.ts files are near-copies), consolidated preview harness
(fake-qtfb.py / fake-pi.py / preview.Dockerfile) as the workspace-level
integration gate, deploy scripts.

## Dependency shape

```
libreink-core ────────┬─────────────────┬────────────────┐
                      │                 │                │
             libreink-display    libreink-input    libreink-text ──┐
                                                         │         │
libreink-hershey ─────────────┐                   libreink-page ───┤
                              │                          │         │
                              └────────── libreink-svg ─────────────┘
libreink-pi  (independent)
```

Device-free test island: libreink-core, libreink-hershey, libreink-text, libreink-page,
libreink-svg — plain `cargo test` on the host.

## Testing strategy

1. Pure `cargo test`: golden-image snapshots for libreink-svg (SVG fixture →
   strokes → stamp to in-memory Framebuffer → own PNG encoder → compare
   golden; write actual+diff on mismatch). Fixtures from real pi traces
   (math, code, mixed fonts, underlines, arrows, fills, odd viewBoxes).
   Property tests: viewBox stays in page bounds, no NaN, fills bounded.
2. Protocol conformance: in-test fake servers for swtfb (32-byte struct +
   ack) and qtfb (SEQPACKET + shm); assert exact byte sequences.
3. Whole-app: the existing QEMU preview harness, consolidated; unchanged
   screenshots are the proof each migration step preserved behavior.
4. On-device: one small `libreink-demo` exerciser (waveform gallery, input echo,
   svg-ink render of a fixture) deployed via the existing ssh flow — for
   what a simulator can't show (waveform look, latency, panel quirks).

## Migration order

1. Workspace + move Bucket A verbatim (zero risk, proves plumbing).
   **DONE 2026-07-11**: libreink-core (fb/draw/font/png/png_dec, alt-ui's
   copies) extracted; all three apps build against it and pass their QEMU
   previews. One API change: `Framebuffer::clip()` accessor replaces direct
   field reads. `make test-host` in alt-ui also runs the crate suite.
2. Bucket B behind `AppId`. **DONE 2026-07-12**: `AppId` lives in
   libreink-core (`app.rs`); libreink-input = touch/pen/power, constructors
   take `AppId`; each app declares `pub const APP: AppId` in main.rs.
   `Phase` moved from qtfb.rs to libreink-core `event.rs` (input and display
   both need it; qtfb re-exports it so app paths didn't change).
3. Bucket C: adopt alt-ui's display/rm2fb/qtfb/draw/text/svg_ink/hershey;
   preview-screenshot pass per app. Riskiest single item: notebook/reader
   have never run the new waveforms → on-device demo check here.
   **display/qtfb/rm2fb DONE 2026-07-12** (libreink-display, alt-ui's
   canonical copies): notebook/reader gained Wave::Page/Print, temp-pinned
   GC16, gl16_full, and the qtfb msync fix — available but not yet used by
   their code paths, so previews stayed pixel-identical. On-device check of
   the new waveforms on nb/rd still pending.
   **hershey/text/svg DONE 2026-07-12**: Pt/Stroke/TextRun/grays moved to
   core::geom (nb/rd Stroke gained the id field, left 0); libreink-hershey
   (data.rs private, default_face takes AppId — nb's runtime FACE_OVERRIDE
   global replaced by main.rs-owned pi_font state and an explicit parse
   param); libreink-text (fonts embedded in the crate); libreink-svg is
   alt-ui's parse(src, scale, default_font) -> (strokes, texts, notes) —
   nb/rd adapted, dropping texts (no typeset rendering there yet). Six
   geometry-invariant tests in libreink-svg (hatching, viewBox remap,
   Garamond runs, math glyphs). All previews pixel-identical.
4. Bucket D: libreink-page (superset, evict Notebook), libreink-pi (transport only).
   **libreink-pi DONE 2026-07-12**: Pi (spawn/JSONL/PiEvent/kill) + ipc
   (tool socket) extracted with PiConfig{app,name,session_dir,system_prompt};
   standing_instructions() helper for AGENT.md bootstrap. Apps keep shim
   pi_rpc.rs files: SYSTEM_PROMPT + prompt composition + session_dir;
   notebook keeps send_page as a SendPage trait. Bin-override env
   standardized to {PREFIX}_PI_BIN (nb/rd fake-qtfb.py updated). All
   previews pass. THE EXTRACTION IS COMPLETE — all 8 crates exist.
   **libreink-page DONE 2026-07-12**: alt-ui's ink.rs verbatim (superset with
   TextRun/undo/ids). Notebook keeps a shim ink.rs: the `Notebook` container
   + a `RenderExt` trait (fill-white + stamp_region, the old render_region/
   render_full API). nb/rd call sites: add_patch gained a texts arg
   (Vec::new()), erase_at returns the undo tuple (they drop it). Format
   compat verified: crate load() heals id-less nb/rd page files
   (unwrap_or(0) + max-id healing), save() adds i/next_stroke/texts keys.
   Flip-away/flip-back preview round-trips the crate's save/load pixel-
   identically. Only libreink-pi remains.
5. Consolidate preview harness + TS extension helper.

## Publishing posture

Internal-only until stable. Community-worthy later: libreink-display (only
qtfb + native-swtfb client anywhere) and libreink-svg (agent SVG → e-ink
plotter strokes). Check crates.io name availability before publishing; if
libreink-svg goes standalone, geometry types may move to libreink-core then.
