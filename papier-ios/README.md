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

## OTA releases — the install page

Installable builds are self-hosted on the VM and installed straight from
Safari on the iPad — no TestFlight, no cable. One command does the whole
ritual:

```bash
./tools/release.sh                # bump patch (1.1.20 -> 1.1.21), build +1
VERSION=1.2.0 ./tools/release.sh  # explicit marketing version
```

Then open **https://remarkable-vm.tail31aa5e.ts.net/papier-install/** on
the iPad and tap Install.

### How it works

iOS installs ad-hoc apps through an `itms-services://` link that points at
a **manifest plist**, which in turn points at the **IPA**. Both must be
served over valid HTTPS — the VM's Tailscale MagicDNS certificate
(`remarkable-vm.tail31aa5e.ts.net`) satisfies Apple, which is why the
install page lives on the tailnet hostname rather than the raw `:8000`
HTTP port the app itself talks to.

The moving parts, all automated by `tools/release.sh`:

1. **Version bump** in `project.yml` (`MARKETING_VERSION` +
   `CURRENT_PROJECT_VERSION`) — iOS only treats a build as an update when
   the version moves — then `xcodegen generate`.
2. **Archive**: `xcodebuild archive` with automatic signing (team in
   `project.yml`).
3. **Export**: `xcodebuild -exportArchive` with
   `build/export-options.plist` — method `debugging` = development
   signing, so the build runs on the team's registered devices only, and
   the iPad must trust the developer certificate once (Settings > General
   > VPN & Device Management).
4. **Manifest**: `ota/manifest-<ver>-<build>.plist` — bundle id, version,
   and the absolute HTTPS URL of the IPA.
5. **Install page**: `ota/index.html` regenerated with the
   `itms-services://?action=download-manifest&url=<manifest>` button.
6. **Upload**: `scp` of the versioned IPA + manifest + page to
   `exedev@remarkable.exe.xyz:~/papier-install/`, which the VM's web
   server exposes at `/papier-install/`.
7. **Verify**: curl the page, manifest, and IPA over HTTPS.

Every release keeps its own manifest + IPA (`Papier-1.1.20-32.ipa`, …) so
older builds stay installable by URL. After a release, commit
`project.yml` (auto-saved), the new `ota/` artifacts, and `build/`
(convention: the shipped archive is tracked). Overrides: `VM=`,
`BASE_URL=`, `INSTALL_DIR=` env vars.

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
