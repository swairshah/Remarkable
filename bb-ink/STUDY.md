# Source study and design choices

## bb as the remote runtime

The [bb repository](https://github.com/get-bb/bb) describes the client/server IDE. Local inspection found a running 0.42.1 server and plugin SDK 0.4.47. The bridge uses the installed SDK's typed APIs and official fake-host test harness. It does not read bb's private database or scrape the desktop UI.

A bb plugin provides a server entry point, scoped storage, HTTP routes, agent tools, agent configuration, events, and CLI commands. A frontend entry point is optional. `bb ink` needs only the server because its display is the native tablet. Existing bb threads retain their provider, project, and permissions.

Studied local plugins:

| Plugin | Relevant pattern | Applied here |
| --- | --- | --- |
| Handsfree | Server-side project/thread discovery, thread creation and sending through the SDK; tool dispatch and plugin CLI discovery | The tablet can start or resume work through bb while presenting a much smaller interface |
| Semdiff | A semantic outline separates meaningful changes from raw patch volume; server storage retains analysis | The review view presents a small number of change cards. Cards are authored by the selected agent; this implementation does not invoke Semdiff itself |
| BB Office | Agent status as spatial, glanceable information; an exclusive `experimental_threadList` provider | Compact activity and attention markers. The bridge leaves the desktop sidebar provider alone |
| thread-hover-status / bb-custom-sidebar | Conversation outlines, latest previews, grouped threads, and environment Git status | A lightweight thread picker with provider, branch, status, and pending-attention indication |

Sources were the installed Handsfree cache under `~/.bb/plugins/cache/git/github.com/swairshah/bb-handsfree/3368c36006271df7ca134a14e5f3341bbafffe01`, Semdiff under `~/work/projects/raising-abstractions/repositories/bb/bb-diff-plugins/bb-plugin-semdiff`, BB Office under `~/.bb/plugins/cache/git/github.com/ymichael/bb-plugins/6df043f21c406195f412b67012f2b9ceab0a6434/plugins/bb-office`, and `~/.bb/plugin-sources/bb-custom-sidebar`.

## Existing tablet apps

Collab demonstrates a native pen canvas around a Pi process using JSONL RPC. Coder and Sketchbook use the shared libreink foundation for tablet display, input, text, and agent interactions. Papier extends this into a persistent working page with idle observations and quiet behavior. Their ARMv7 musl and AppLoad/rm2fb deployment patterns are the basis for this client.

The decisive Papier references are `papier/src/main.rs` (`IDLE_DELAY` and `maybe_send_page`) and the sibling `libreink/crates/libreink-pi/src/lib.rs` (`send_image_message`). Papier waits 2800 ms, checks that the page changed, avoids sending during pen and selection activity, honors Quiet, and uses `streamingBehavior: followUp` when Pi is active. Its transport sends a page snapshot with patch/layout context.

bb ink uses that timing and collaboration model, with an explicit user-ink delta and before/after region image. The full page becomes occasional context. The bb equivalent of the Pi follow-up is `threads.send` with `mode: queue-if-active`.

## Visual direction

The supplied `/tmp/bb-claude-sketch.mp4` shows sparse paper, handwritten prompts, short replies, small tool blocks, and drawings. The contact sheet in `references/video-contact-sheet.png` preserves the inspected frames. This informed the generous margins, black-and-white type, diagram-first display, and absence of animated token output.

There are two layers: persistent user writing and a structured agent board. The agent can revise its board without erasing handwriting. Updates wait while the user is writing. Local ink changes are measured against the last acknowledged user layer, so an agent redraw cannot cause an observation loop.

The revised drawing surface uses the same SVG-to-ink converter as Papier. The model supplies independent sections with local coordinates, including handwriting, curves, charts, annotations, and code. [The layout module](src/layout.rs) reserves user stroke bounds plus a margin; [the compositor](src/scene.rs) places and clips complete sections in remaining space. Overflow has separate read-only reply pages. Existing grid responses are preserved as text, while new agent tools accept the freeform SVG schema in [protocol.ts](bb-plugin-ink/protocol.ts). Selected-region editing, synchronized multi-device canvases, and on-tablet permission answers remain outside the implemented scope.

See [[bb-ink/README]] for setup, verification, and current limits.
