#!/bin/bash
# Render a dropped PDF into an alt-ui book bundle on the VM.
#   alt-ui-render.sh <pdf-path> <out-docs-dir> <title> [ml mt mr mb]
# mkbook.py (pymupdf + numpy) runs in a venv so it doesn't fight Ubuntu's
# externally-managed python. Writes meta.json/pages/text (mkbook) plus the
# state.json the reader/viewer need (a sequence over all PDF pages).
#
# Margins (device px, per side) are optional; omitted -> mkbook's default 40px
# border. The web margin editor passes explicit values on re-render.
set -euo pipefail

PDF="$1"; OUT="$2"; TITLE="${3:-Untitled}"
VENV="/home/exedev/alt-ui-venv"
PY="$VENV/bin/python3"
MKBOOK="/home/exedev/bin/mkbook.py"

MARGS=()
if [ "$#" -ge 7 ]; then
  MARGS=(--margin-left "$4" --margin-top "$5" --margin-right "$6" --margin-bottom "$7")
fi

"$PY" "$MKBOOK" "$PDF" -o "$OUT" --title "$TITLE" "${MARGS[@]}"

# synthesize state.json: seq = every PDF page, 1-based positions
PAGES="$("$PY" -c "import json;print(json.load(open('$OUT/meta.json'))['pages'])")"
"$PY" - "$OUT" "$PAGES" <<'PYEOF'
import json, sys
out, pages = sys.argv[1], int(sys.argv[2])
json.dump({"v": 1, "seq": [{"p": i} for i in range(pages)], "next_note": 1, "pos": 0},
          open(f"{out}/state.json", "w"))
PYEOF
echo "rendered $PAGES pages -> $OUT"
