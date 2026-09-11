# bb ink

A native reMarkable client for a bb workspace running on another machine. The tablet is a shared canvas: write, erase, pause, and let the agent draw back. bb owns the projects, coding agents, tools, permissions, and conversations.

## Interaction

A 2.8-second pause after a pen stroke, erasure, undo, or keyboard edit sends an observation automatically. Unchanged pages send nothing. Each observation contains a canvas ID, consecutive revision numbers, added and removed stroke IDs, and a before/after image of the changed region. Typed notes are persistent canvas content too.

The first observation includes an image of the current viewport. Later observations include one when the agent board changes, every eighth observation after the first, or when you tap **Nudge**. Before/after crops cover every document band touched by new ink or erasure, including regions you have scrolled away from. Captions supply document rectangles and viewport offsets. Scrolling alone sends nothing.

Writing remains on the page after acknowledgement. You can keep writing during delivery; only the captured baseline advances, and newer strokes become the next diff. Observations use bb's `queue-if-active`, matching Papier's Pi follow-up behavior. Agent instructions ask it to stay silent on ordinary notes or unfinished thoughts. A response of exactly `pass` is hidden.

**Quiet** pauses automatic sharing. **Nudge** asks the agent to inspect the canvas now, including when Quiet is on. Drag a finger vertically to scroll the same writable document. The pen always writes or erases; pen proximity cancels finger gestures. **More ↓** scrolls farther down a response and **Top ↑** returns to the beginning. Handwriting uses document coordinates and stays anchored when you scroll or reopen a conversation. **Threads** opens projects and recent conversations. The native client supports pen erasing and Undo. The desktop companion supports drawing, typing, and Undo.

## Run locally

Run these commands from this directory. The sibling `../../libreink` checkout supplies the same native display, input, and text crates used by the other tablet apps.

```sh
cd bb-plugin-ink
npm ci
cd ..
make test
make preview
```

The preview opens at [localhost:5188](http://127.0.0.1:5188). It renders the actual Rust tablet UI, overlays browser ink, and reports automatic observations without starting an agent. `make render` also writes map, review, and thread-picker PNGs to `build/`.

```sh
make install-plugin
make live
```

The live companion uses the installed `ink` plugin. Choose a project or an existing thread, then write. Live observations are real prompts to the selected bb agent. The token remains in the companion's server process and is never sent to browser JavaScript.

This machine's bb application is version 0.42.1; its bundled CLI is used by the Makefile. The older `bb` on PATH is 0.37.0. On another server, set `BB` to that server's current CLI when building/installing and `BB_INK_CLI` to its bb JavaScript entry point for the companion and pairing helper. `BB_SERVER_URL` selects the CLI server; `BB_INK_URL` selects the HTTP bridge origin.

## Install on reMarkable 2

Requires the existing Xovi/AppLoad setup and the same rm2fb integration used by Coder. The USB tunnel uses built-in loopback HTTP and requires no curl on the tablet. Direct HTTPS connections still require curl. Build tools require the Rust `armv7-unknown-linux-musleabihf` target. The default target is configured in `.cargo/config.toml`.

```sh
make vendor
make deploy HOST=remarkable
make pair HOST=remarkable
make tunnel HOST=remarkable
```

Leave the tunnel running, then open **bb ink** in AppLoad. The reverse SSH tunnel connects the tablet's loopback port 38886 to bb on the host. The bridge does not require exposing bb publicly. An HTTPS bridge can instead be configured with `BB_INK_URL` in `/home/root/.config/bb-ink/env`. Set `BB_INK_PAUSE_MS` there to change the native pause interval (1000–15000 ms).

Local ink, acknowledgement baselines, pending receipts, and per-thread drafts persist under `/home/root/.local/share/bb-ink`. The private token header is in `/home/root/.config/bb-ink/auth-header`. Network failures preserve writing. Nudge checks the same receipt; an ambiguous server dispatch is never repeated automatically. Inspect the conversation before resolving an unknown delivery in the recovery panel.

The launcher restores xochitl on exit. Use the menu's Exit button or the power button; ten minutes without input also returns to xochitl. Five-contact gestures are ignored because a writing palm can generate them, matching Papier. Finger buttons require a complete tap that was not interrupted by pen proximity. Logs include exit reasons and process status in `/tmp/bb-ink.log`; display logs are in `/tmp/bb-ink-rm2fb.log`. See [pen_contact.rs](src/pen_contact.rs), [tablet.rs](src/tablet.rs), and [takeover.sh](scripts/takeover.sh).

SVG reply text is literal, including underscores, braces, dollar signs, backslashes, and indentation. The default reply face is Inter; `font-family="monospace"` uses Google Sans Code. Garamond and the pen lettering styles remain available. Math uses Unicode or explicitly positioned text rather than implicit TeX conversion. [svg_text.rs](src/svg_text.rs) owns this behavior; [font-options.json](fixtures/font-options.json) is the renderable font specimen. Observation images are prepared in a background worker, preserving writing and erasing that arrive during capture; see [observation.rs](src/observation.rs). Physical palm behavior still requires an on-device writing check.

## Implementation and current limits

- `src/`: Rust framebuffer renderer, pen client, delta calculation, persisted notebook, and background HTTP worker. Display updates skip identical frames and defer board changes while writing.
- `bb-plugin-ink/`: supported bb SDK integration, authenticated HTTP routes, durable receipts, diagram storage, and `ink_read` / `ink_publish` tools. It uses the selected thread's provider, including Pi.
- `web/` and `tools/preview.mjs`: local desktop companion and demo, using the native renderer.
- `references/video-contact-sheet.png`: frames from the supplied sketch video.

The agent draws freeform SVG sections: handwritten lettering, curves, charts, annotated sketches, math, and small code excerpts. Up to 24 sections are supported, each with its own dimensions. The client finds empty space for complete sections and reserves a 24-pixel margin around each user stroke; user coordinates never change. If a section cannot fit on the first screen, it continues below in the same scrolling document. Section positions persist; annotating a section does not trigger reflow. Stable IDs and unchanged or smaller dimensions retain their anchors during revisions. Larger replacement sections move to available space. Placement and viewport clipping are enforced locally by [layout.rs](src/layout.rs) and [scene.rs](src/scene.rs). The document is currently limited to 65,536 pixels vertically. Old grid responses remain readable as plain text. The supported SVG subset is defined in [protocol.ts](bb-plugin-ink/protocol.ts): paths, lines, circles, ellipses, polygons, rectangles, and text with literal local coordinates. It uses Papier's SVG-to-ink renderer for geometry and bb-ink's literal text renderer for labels and code. Scripts, external references, transforms, CSS, and viewBox are rejected. This client does not mirror bb's desktop chrome, run Node on the tablet, or answer bb approval dialogs on-device. Pending interactions are surfaced with a prompt to handle them in bb. Full-context images retained by `ink_read` can lag newer region diffs; their revision is included explicitly. Thread selection keeps a separate local canvas for each conversation, but simultaneous editors of one canvas are not synchronized.

Verified on the host: ARM static build, native renderer, bridge typecheck, Rust and JavaScript tests, authenticated live thread reads, and automatic pause behavior in the demo companion. The `ink` plugin is installed and running in local bb. The app is now installed and paired on the reMarkable. A read-only check executed by the tablet binary through the USB tunnel returned protocol 2, 15 projects, and 40 threads. The freeform update is installed and running on the tablet. Its Wacom input and rm2fb display initialized successfully, and all 25 pre-update saved user strokes were unchanged after restart. Current tests cover protected placement, continuous overflow, persistent anchors, world-coordinate ink rendering, offscreen erasure captures, palm-cancelled scroll gestures, and receipt deduplication. Physical pen latency and refresh quality still need user evaluation. No live agent prompt was dispatched during validation. Run the installed binary with `--check` to verify the connection without taking over the screen.

The [source study](STUDY.md) records the plugin patterns, Papier behavior, and visual reference behind these decisions.
