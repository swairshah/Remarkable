#!/usr/bin/env bash
# papier-kindle.sh <doc-id> [--format auto|epub|pdf] — send a Papier doc to
# a Kindle via Amazon's Send-to-Kindle email gateway and the Resend API.
#
# Called by papier-upload.js (POST /papier/api/kindle) and by hand / make
# kindle from the Mac. What gets sent, by preference:
#
#   epub  — if the doc came from ✦ Compose, its markdown source is still in
#           papier-compose/<job>/work/article.md; papier-epub.sh turns it into
#           a reflowable EPUB 3 using the same MathML + Reader-font conversion
#           as the proven Clippings pipeline.
#   pdf   — otherwise the retained source PDF (papier-sources/<id>.pdf), or
#           a PDF derived from the bundle (same papier-make-pdf.py path the
#           PDF.js viewer uses). A clean title sheet is prepended to the
#           emailed copy only. Sent fixed-layout, no "Convert" subject —
#           Amazon's PDF reflow mangles papers.
#
# --format epub errors if no markdown source exists (no lossy PDF->EPUB);
# --format pdf skips the markdown even for compose docs.
#
# Ink never leaves the tablet/web reader; the Kindle copy is the clean doc.
#
# Config (~/.papier-kindle.env, see papier/sync/KINDLE.md):
#   KINDLE_TO             your @kindle.com address           (required)
#   KINDLE_FROM           verified + Amazon-approved sender  (required)
#   RESEND_API_KEY        Resend API key                      (required)
set -euo pipefail

BACKUP="${PAPIER_BACKUP:-$HOME/remarkable-backup}"
MIRROR="$BACKUP/papier/docs"
INBOUND="$BACKUP/papier-inbound/docs"
SOURCES="$BACKUP/papier-sources"
COMPOSE="$BACKUP/papier-compose"
DERIVED="$BACKUP/papier-derived-pdf"
MAKE_PDF_PY="${PAPIER_MAKE_PDF:-$HOME/bin/papier-make-pdf.py}"
VENV_PY="${PAPIER_PY:-$HOME/papier-venv/bin/python3}"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
EPUB_RENDERER="${PAPIER_EPUB:-$SCRIPT_DIR/papier-epub.sh}"
[ -f "$EPUB_RENDERER" ] || EPUB_RENDERER="$HOME/bin/papier-epub.sh"
COVER_RENDERER="${PAPIER_KINDLE_COVER:-$SCRIPT_DIR/papier-kindle-cover.py}"
[ -f "$COVER_RENDERER" ] || COVER_RENDERER="$HOME/bin/papier-kindle-cover.py"

[ -f "$HOME/.env" ] && { set +u; set -a; . "$HOME/.env"; set +a; set -u; }
[ -f "$HOME/.papier-kindle.env" ] && { set -a; . "$HOME/.papier-kindle.env"; set +a; }

die() { echo "papier-kindle: $*" >&2; exit 1; }

DOC="${1:-}"; shift || true
FORMAT=auto
while [ $# -gt 0 ]; do
  case "$1" in
    --format) FORMAT="${2:?}"; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ -n "$DOC" ] || die "usage: papier-kindle.sh <doc-id> [--format auto|epub|pdf]"
printf '%s' "$DOC" | grep -Eq '^[a-z0-9][a-z0-9_-]{0,100}$' || die "bad doc id"
case "$FORMAT" in auto|epub|pdf) ;; *) die "bad --format (auto|epub|pdf)" ;; esac
: "${KINDLE_TO:?set KINDLE_TO in ~/.papier-kindle.env (your @kindle.com address)}"
: "${KINDLE_FROM:?set KINDLE_FROM in ~/.papier-kindle.env (a verified, approved sender)}"
: "${RESEND_API_KEY:?set RESEND_API_KEY in ~/.papier-kindle.env or ~/.env}"

DOCDIR=""
for d in "$INBOUND/$DOC" "$MIRROR/$DOC"; do
  [ -f "$d/meta.json" ] && { DOCDIR="$d"; break; }
done
[ -n "$DOCDIR" ] || die "unknown doc: $DOC"

TITLE="$(python3 - "$DOCDIR/meta.json" <<'PY'
import json, sys
try: print(json.load(open(sys.argv[1])).get("title") or "")
except Exception: print("")
PY
)"
[ -n "$TITLE" ] || TITLE="$DOC"

# compose docs keep their markdown source: find the job whose result is DOC
ARTICLE=""
if [ "$FORMAT" != "pdf" ] && [ -d "$COMPOSE" ]; then
  hit="$(grep -lsF "\"docId\":\"$DOC\"" "$COMPOSE"/*/result.json 2>/dev/null | head -n1 || true)"
  if [ -n "$hit" ] && [ -s "$(dirname "$hit")/work/article.md" ]; then
    ARTICLE="$(dirname "$hit")/work/article.md"
  fi
fi
[ "$FORMAT" = epub ] && [ -z "$ARTICLE" ] && \
  die "no markdown source for $DOC — only ✦ Compose docs can become EPUBs (send as pdf instead)"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
SAFE_TITLE="$(printf '%s' "$TITLE" | tr -c 'A-Za-z0-9 ._-' '-' | sed 's/--*/-/g' | cut -c1-120)"

if [ -n "$ARTICLE" ]; then
  [ -f "$EPUB_RENDERER" ] || die "EPUB renderer not found: $EPUB_RENDERER"
  OUT="$TMP/$SAFE_TITLE.epub"
  COVER=""
  [ -f "$DOCDIR/thumb.png" ] && COVER="$DOCDIR/thumb.png"
  /bin/bash "$EPUB_RENDERER" "$ARTICLE" "$OUT" "$TITLE" "$COVER" || die "pandoc epub build failed"
  MIME=application/epub+zip
else
  MIME=application/pdf
  OUT="$TMP/$SAFE_TITLE.pdf"
  RAW_PDF="$TMP/source.pdf"
  if [ -f "$SOURCES/$DOC.pdf" ]; then
    cp "$SOURCES/$DOC.pdf" "$RAW_PDF"
  else
    cached="$(ls -t "$DERIVED/$DOC"-*.pdf 2>/dev/null | head -n1 || true)"
    if [ -n "$cached" ]; then
      cp "$cached" "$RAW_PDF"
    else
      [ -x "$VENV_PY" ] || die "no source PDF and no papier venv to derive one"
      "$VENV_PY" "$MAKE_PDF_PY" "$DOCDIR" "$RAW_PDF" "$TITLE" >&2 || die "derived pdf build failed"
    fi
  fi
  [ -x "$VENV_PY" ] || die "papier venv not found (needed for the Kindle title page)"
  [ -f "$COVER_RENDERER" ] || die "Kindle title-page renderer not found: $COVER_RENDERER"
  "$VENV_PY" "$COVER_RENDERER" "$RAW_PDF" "$OUT" "$TITLE" >&2 || \
    die "Kindle title-page build failed"
fi

BYTES=$(stat -c%s "$OUT" 2>/dev/null || stat -f%z "$OUT")
BASE64_BYTES=$(( ((BYTES + 2) / 3) * 4 ))
[ "$BASE64_BYTES" -le $((40 * 1024 * 1024)) ] || \
  die "$(basename "$OUT") becomes $((BASE64_BYTES / 1024 / 1024))MB after Base64 — over Resend's 40MB email limit"

command -v curl >/dev/null || die "curl not installed on the VM"
PAYLOAD="$TMP/resend.json"
RESPONSE="$TMP/resend-response.json"
python3 - "$OUT" "$MIME" "$TITLE" "$KINDLE_FROM" "$KINDLE_TO" > "$PAYLOAD" <<'PY'
import base64
import json
import sys
from pathlib import Path

out, mime, title, sender, to = sys.argv[1:6]
path = Path(out)
json.dump({
    "from": sender,
    "to": [to],
    "subject": title,
    "text": "Sent from Papier.",
    "attachments": [{
        "filename": path.name,
        "content": base64.b64encode(path.read_bytes()).decode("ascii"),
        "content_type": mime,
    }],
}, sys.stdout, separators=(",", ":"))
PY

RESEND_API_URL="${RESEND_API_URL:-https://api.resend.com/emails}"
if ! HTTP_STATUS="$(curl -sS -o "$RESPONSE" -w '%{http_code}' \
    -X POST "$RESEND_API_URL" \
    -H "Authorization: Bearer $RESEND_API_KEY" \
    -H 'Content-Type: application/json' \
    --data-binary "@$PAYLOAD")"; then
  die "Resend API request failed"
fi
case "$HTTP_STATUS" in
  2??) ;;
  *) die "Resend API returned HTTP $HTTP_STATUS: $(tr '\n' ' ' < "$RESPONSE" | head -c 500)" ;;
esac

EMAIL_ID="$(python3 - "$RESPONSE" <<'PY'
import json
import sys
try:
    print(json.load(open(sys.argv[1], encoding="utf-8")).get("id") or "")
except Exception:
    print("")
PY
)"
[ -n "$EMAIL_ID" ] || die "Resend API response had no email id"

echo "sent: $(basename "$OUT") ($((BYTES / 1024))KB) -> $KINDLE_TO (Resend $EMAIL_ID)"
