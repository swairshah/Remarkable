---
title: Papier for iPad
description: Architecture, build instructions, and VM-backed Pi interaction tracing for the Papier iOS app.
tags:
  - papier
  - ios
  - developer-tools
---
# Papier for iPad

The papier UX on the iPad, talking to the same cloud as the tablet app
(remarkable.exe.xyz). Marks are Apple-Pencil *pencil* textured; the page
files round-trip libreink-page's exact ink JSON.

- Reads: `/papier/api/library` manifest + versioned page PNGs / ink JSON,
  identical to the web viewer.
- Writes: `POST /papier/api/ink|state|notebook` — everything lands in the
  VM's *inbound* tree; the reMarkable's next wake pulls it (per-file
  last-writer-wins), so iPad marks appear on the tablet the next time you
  use it.
- Reachability: the VM over the tailnet at `http://<remarkable-vm>:8000`
  (set in Settings). The exe.dev HTTPS front door has browser auth, so the
  app uses the tailnet path.
- Existing tablet ink is loaded INTO the PencilKit canvas as pencil
  strokes (papier heals foreign stroke ids), pi's patches render read-only
  in pi blue, exactly like the web viewer.

## Build

`xcodegen generate && open Papier.xcodeproj` (`project.yml` is the source of truth). The UI test drives a real draw-and-sync pass against the cloud via `ssh -L 18000:127.0.0.1:8000 exedev@remarkable.exe.xyz`.

## Pi interaction traces

The developer `Makefile` can pull the iOS → VM → Pi history and open the shared HTML timeline with notebook images, model responses, canvas tool calls, latency/token/cost metrics, Pi stderr, and the VM service journal:

```bash
# From the Remarkable repository root — all active document sessions
make -C papier-ios trace

# One document only
make -C papier-ios trace DOC=ipad-sync-test
```

The generated viewer is `papier-ios/build/trace.html`. Override the SSH destination with `VM=user@host` when needed. For live logs:

```bash
make -C papier-ios log
make -C papier-ios log-pi DOC=ipad-sync-test
```
