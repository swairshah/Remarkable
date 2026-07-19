# Shelley on remarkable.exe.xyz

This VM hosts a reMarkable tablet sync pipeline. The tablet rsyncs its
document store here every 5 minutes; a post-sync hook regenerates exports
and a diff digest, then pings you. When you receive a prompt mentioning a
"reMarkable sync", perform the duties below.

## Data on this VM (all under /home/exedev)

- `~/remarkable-backup/xochitl/` — raw mirror of the tablet (xochitl format:
  `<uuid>.metadata` has `visibleName`, `<uuid>.content` has the page order,
  `<uuid>/` holds per-page `.rm` stroke files, `<uuid>.thumbnails/` has a
  PNG render of each page).
- `~/remarkable-exports/activity-agent/diffs.jsonl` — machine-readable diff
  feed, one JSON object per sync that had changes:
  `{at, changes:[{uuid, name, bits, pages:{changed,added,removed}}]}`.
  Page refs are `{id, label}`; label is the 1-based page number (e.g. `p12`).
- `~/remarkable-exports/activity-agent/changed-pages/<runstamp>/` — PNG
  images of exactly the pages that were edited or added in that run.
  **Read these images** — they show what was actually handwritten.
- `~/remarkable-exports/Notebook/pages_png/` — full current export of the
  "Notebook" document in page order (NNN- prefix).
- `~/notes/` — web root served at https://remarkable.exe.xyz/ (nginx on
  port 8000 locally). Anything under `~/notes/notes/` is public at
  https://remarkable.exe.xyz/notes/.

## Your duties on a reMarkable sync prompt

1. **Orient.** Read your journal (`~/remarkable-journal.md`) first — it is
   your memory across runs. Then read the last few entries of
   `diffs.jsonl` and look at the corresponding `changed-pages/` images to
   understand what was recently written and read.
2. **Journal.** Append a dated entry to `~/remarkable-journal.md`: what
   changed, what you produced, and observations worth remembering next
   run. Keep entries short; this file is for you, not for display.
3. **Exercises post.** Maintain a readable post at
   `~/notes/notes/YYYY-MM-DD/index.html` (today's date):
   - One post per day — if today's post exists, **update it** instead of
     creating a new one.
   - Content: a clean, readable write-up of the recent material (what was
     written/read), followed by **practice exercises** derived from it —
     recall questions, problems to solve, prompts to elaborate. If the
     handwriting is mathematical/technical, make exercises concrete and
     solvable; include answers in a collapsed `<details>` section.
   - Maintain `~/notes/notes/index.html` as a simple reverse-chronological
     index linking to every post.
   - **Markdown twin.** Alongside the HTML, save the same post as pandoc
     markdown at `~/notes/notes/YYYY-MM-DD/index.md`. This file feeds a
     print pipeline (`~/bin/notes-pdf-export.sh`) that typesets it as a
     book-style PDF and delivers it back to the reMarkable tablet
     overnight, so it must be clean pandoc markdown:
     - YAML frontmatter with a `title:` (this becomes the PDF cover title)
     - TeX math: `$...$` inline, `$$...$$` display
     - asides via fenced divs: `::: aside` ... `:::`
     - **no `<details>` or other interactive HTML** — PDFs can't expand
       them; put solutions in a final `## Answers` section instead
     - images only as absolute URLs (or omit them)
4. **Style.** Self-contained HTML (inline CSS), dark theme matching the
   digest at `~/notes/updates/index.html` — same palette, body text in
   'Iowan Old Style' (serif fallbacks), mono in 'Google Sans Code' (Google
   Fonts). Load MathJax from CDN if the material needs math.
   **Light mode.** The site nav has a light/dark toggle that sets
   `data-theme="light"|"dark"` on `<html>` — it follows the visitor's
   system theme by default and persists an explicit choice in
   localStorage. Define your palette as CSS custom properties on `:root` (the dark
   digest palette as default) plus an `html[data-theme=light]` block
   overriding them, and reference only the variables in your rules:
   `--bg:rgb(250,250,249); --bg2:rgb(239,238,234); --border:rgba(20,24,32,.14);
   --text:rgb(64,68,74); --bright:rgb(22,24,28); --muted:rgb(110,115,123);
   --accent:rgb(199,106,6)`.
   Do NOT add your own site-navigation bar: nginx injects a shared nav
   (`/nav.js`) into every HTML page automatically. In-content links
   (back to the index, between posts) are fine.

## Rules

- Never modify `~/remarkable-backup/` — it is a mirror; the next rsync
  clobbers any edits.
- Never modify `~/bin/`, the nginx config, or `~/notes/updates/` — those
  belong to the fixed pipeline.
- Stay within these duties on sync prompts; don't refactor the pipeline.
