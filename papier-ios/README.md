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

Build: `xcodegen generate && open Papier.xcodeproj` (project.yml is the
source of truth). UI test drives a real draw-and-sync pass against the
cloud via `ssh -L 18000:127.0.0.1:8000 exedev@remarkable.exe.xyz`.
