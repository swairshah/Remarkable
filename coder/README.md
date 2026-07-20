# coder — codebases as diagrams, sketch feedback, get PRs; on the reMarkable 2

[sketchbook](../sketchbook/)'s takeover tech reshaped into a **code-reading
studio**. The sidebar lists PROJECTS — git repositories cloned on the
exe.dev VM — and each project is a stack of paper pages. A resident pi
agent explains each repo by **drawing**: an architecture overview on page 1
(boxes for subsystems, arrows for the data flow, terse plotter-font
notes), subsystem zoom-ins on appended pages when you ask. You read,
think and answer with the pen; pi answers with ink. Code never appears on
the page — the page carries the *understanding*.

Sketch a change — circle a box and write "split this", arrow a new cache
into the diagram, write a spec — and pi implements it **on the repo over
ssh**: branch, edit, test, commit, and (where the remote allows) a pull
request via `gh`, then draws a PR card next to your request.

```
 tablet (this app + pi, headless)              exe.dev VM
 ┌──────────────────────────────┐    ssh    ┌─────────────────────┐
 │ sidebar: projects  ◄── scan ─┤           │ ~/coder/<slug>      │
 │ pages: ink + pi's diagrams   │  ───────► │   git clones        │
 │ pause → page photo → pi      │  git/gh/rg│   branches, PRs     │
 │ pi tools: draw/erase/goto/   │           └─────────────────────┘
 │           view/projects      │
 │ registry: ~/.local/share/    │   the repos never touch the tablet;
 │   coder/projects/<slug>/     │   the tablet only holds metadata + ink
 │     meta.json, SUMMARY.md,   │
 │     pages/                   │
 └──────────────────────────────┘
```

## The flow

1. **Clone.** Write `clone github.com/karpathy/micrograd` on the NOTES
   pad (the always-present scratch project). On the pause, pi clones the
   repo into the VM's `~/coder/`, explores it briefly, registers the
   project (meta.json + SUMMARY.md on the tablet), flips the tablet to
   it and draws the overview.
2. **Read.** Page 1 is the map: name, 3-5 facts, an architecture diagram.
   Swipe pages for pi's subsystem details. Sidebar → DOCS reads each
   project's SUMMARY.md typeset (Garamond, paginated).
3. **Ask.** Write a question anywhere — or circle part of a diagram with
   a `?`. pi reads the actual code over ssh (rg, sed, git log) and draws
   the answer: a zoomed diagram, a call flow, a terse note. Crowded page?
   It appends a detail page and leaves a `→ p.N` pointer.
4. **Change.** Sketch/write what you want different. pi branches
   (`coder/<name>`), edits, runs the quick tests, commits, pushes + opens
   a PR where it can (your own repos — `gh` on the VM must be logged in),
   or keeps the branch local for read-only clones. It draws a PR card by
   your request; never a diff.

## The tools pi gets

| tool | does |
|------|------|
| `coder_projects` | the sidebar as data: slug, url, pages, which is on screen |
| `coder_goto {project?, page?}` | flip the tablet to a project/page (refused mid-writing) |
| `coder_draw {svg, page?}` | SVG → pen strokes → patch id; `page = count+1` appends a page |
| `coder_erase {id, page?}` | remove one of its patches (user ink survives) |
| `coder_erase_ink {rect, page?}` | consume a handwritten instruction it has fulfilled |
| `coder_view {page?}` | fresh half-scale PNG of a page |

Everything else — cloning, reading code, editing, PRs — is pi's ordinary
shell over `ssh -y -i ~/.ssh/id_sync_dropbear_ed25519 exedev@remarkable.exe.xyz`
(the tablet's registered sync identity; override with `CODER_VM`).
The project registry lives on the tablet and pi maintains it with plain
file tools; the app only reads it.

## Gestures

| Do this | And |
|---------|-----|
| Write anywhere | Pausing ~3s offers the page to pi |
| Tap the top-left corner | Sidebar: NOTES + projects, INSTRUCTIONS, DOCS, quiet mode |
| Tap a project | Switch to its pages (its ink, its diagrams) |
| Swipe left / right | Next / previous page within the project |
| Tap the ⊙ button (top-right) | Toolbar: undo/redo, lasso select, eraser modes, nudge pi, quiet, restart session |
| Flip the marker, rub | Erase (object / pixel / region modes from the toolbar) |
| Write feedback near pi's diagram | It learns — durable rules land in AGENT.md (sidebar → INSTRUCTIONS) |
| Power button | Sleep + real suspend; wake resumes in place |

## Build & deploy

Prereqs are sketchbook's: xovi + AppLoad (`../pi/pi-appload/install.sh`),
pi on the tablet (`../pi/pi-harness/install.sh`), WiFi on, and
`rustup target add armv7-unknown-linux-musleabihf`.

```sh
make                # cross-compile (cargo + rust-lld)
make preview        # no tablet: qemu + fake pi plays a whole session → build/preview*.png
make fetch-server   # rm2fb_server (timower/rM2-stuff) into vendor/
make deploy         # push binary/scripts/extension/manifest to the device
make log            # tail /tmp/coder.log on the device
make kill           # stop a running session (restores xochitl)
```

One-time on the VM (`ssh exedev@remarkable.exe.xyz`):

```sh
mkdir -p ~/coder     # where the clones live
gh auth login        # only needed for the PR half of the story
```

Then tap **coder** in the AppLoad menu. The NOTES pad is page one of
everything: write a clone request and watch the sidebar grow.

## Data layout (tablet)

```
~/.local/share/coder/
  projects/<slug>/meta.json    {"name","url","branch","summary"}   (pi's)
  projects/<slug>/SUMMARY.md   living notes, read via DOCS         (pi's)
  projects/<slug>/pages/       page-NNNN.json vector ink           (app's)
  AGENT.md                     standing instructions pi learns
  sessions/                    pi session JSONL (survives restarts)
  settings.json                text scale, quiet, font, last project
```

`slug` doubles as the repo directory name on the VM: `~/coder/<slug>`.
