# Remarkable

Projects built around a reMarkable 2 tablet — syncing it, drawing on it, and
talking to a Raspberry Pi from it. Each directory is an independent project
with its own README.

| Project | What it is |
| --- | --- |
| [`sync/`](sync/) | Tablet → server pipeline: rsync backup, JPG/PDF export, LLM activity digest + web viewer at [remarkable.exe.xyz](https://remarkable.exe.xyz/updates/) |
| [`sample-app/`](sample-app/) | Minimal AppLoad doodle-pad app in C — hackable template with direct Wacom input |
| [`sample-app-rust/`](sample-app-rust/) | The Rust twin of `sample-app`, kept at feature parity |
| [`pi/`](pi/) | Running a headless Raspberry Pi from the reMarkable 2 |
| [`pi-collab/`](pi-collab/) | Handwriting chat app: write on the tablet, pi answers via RPC (code/markdown/SVG rendered in-place) |
| [`agent/`](agent/) | RBot — standalone Flue agent |

## Conventions

- Deploys are plain `ssh`/`scp` shell scripts, one per target machine.
- `ssh remarkable` is the tablet (LAN alias in `~/.ssh/config`; IP changes
  occasionally), `ssh exedev@remarkable.exe.xyz` is the server VM.
- See [`sync/ARCHITECTURE.md`](sync/ARCHITECTURE.md) for the full sync-stack
  architecture.
