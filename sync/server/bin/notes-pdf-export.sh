#!/usr/bin/env bash
# notes-pdf-export.sh — render Shelley's daily notes posts to reMarkable PDFs.
#
# Scans ~/notes/notes/YYYY-MM-DD/index.md (the markdown twin Shelley writes
# alongside each HTML post), renders any whose content changed since the
# last run via notes-md2pdf.sh into ~/remarkable-exports/notes-pdf/, and
# maintains manifest.sha256 — the file the tablet's pull script reads to
# decide what to import.
#
# Called at the end of remarkable-post-sync.sh on every sync; cheap when
# nothing changed (one sha256 per post, no chrome run).
#
# Env: NOTES_DIR, NOTES_PDF_OUT

set -euo pipefail

NOTES_DIR="${NOTES_DIR:-$HOME/notes/notes}"
OUT_DIR="${NOTES_PDF_OUT:-$HOME/remarkable-exports/notes-pdf}"
STATE="$OUT_DIR/.rendered.state"   # lines: "<date> <sha256-of-md>"
RENDERER="$HOME/bin/notes-md2pdf.sh"

mkdir -p "$OUT_DIR"
touch "$STATE"

[[ -d "$NOTES_DIR" ]] || exit 0

changed=0
shopt -s nullglob
for md in "$NOTES_DIR"/*/index.md; do
  date_dir="$(basename "$(dirname "$md")")"
  [[ "$date_dir" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || continue
  hash="$(sha256sum "$md" | awk '{print $1}')"
  prev="$(awk -v d="$date_dir" '$1==d{print $2}' "$STATE")"
  if [[ "$hash" == "$prev" && -f "$OUT_DIR/$date_dir.pdf" ]]; then
    continue
  fi
  echo "[$(date -Is)] render $date_dir"
  if "$RENDERER" "$md" "$OUT_DIR/$date_dir.pdf" "Notes · $date_dir"; then
    grep -v "^$date_dir " "$STATE" > "$STATE.tmp" || true
    echo "$date_dir $hash" >> "$STATE.tmp"
    mv "$STATE.tmp" "$STATE"
    changed=1
  else
    echo "[$(date -Is)] render FAILED for $date_dir (leaving state unchanged)"
  fi
done

# Manifest: sha256sum format ("<hash>  <name>.pdf"), consumed by the tablet.
if [[ "$changed" == 1 || ! -f "$OUT_DIR/manifest.sha256" ]]; then
  (
    cd "$OUT_DIR"
    pdfs=(*.pdf)
    if [[ ${#pdfs[@]} -gt 0 ]]; then
      sha256sum "${pdfs[@]}" > manifest.sha256
    else
      : > manifest.sha256
    fi
  )
  echo "[$(date -Is)] manifest updated"
fi
