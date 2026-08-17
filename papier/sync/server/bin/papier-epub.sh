#!/usr/bin/env bash
# papier-epub.sh <article.md> <output.epub> <title> [cover]
#
# Kindle-friendly EPUB 3 renderer shared by the Send to Kindle path. This is
# the proven conversion used by enrich_clippings_agentic.py and the Papier
# Compose EPUB output: MathML instead of per-formula WebTeX images, Reader
# fonts when available, and removal of embedded TeX annotations that weak
# EPUB readers sometimes display beside the equation.
set -euo pipefail

ARTICLE="${1:?usage: papier-epub.sh <article.md> <output.epub> <title> [cover]}"
OUT="${2:?usage: papier-epub.sh <article.md> <output.epub> <title> [cover]}"
TITLE="${3:?usage: papier-epub.sh <article.md> <output.epub> <title> [cover]}"
COVER="${4:-}"

[ -s "$ARTICLE" ] || { echo "papier-epub: missing markdown: $ARTICLE" >&2; exit 1; }
command -v pandoc >/dev/null || { echo "papier-epub: pandoc is not installed" >&2; exit 1; }

BUILD="$(mktemp -d)"
trap 'rm -rf "$BUILD"' EXIT
CSS="$BUILD/epub.css"
FONT_BUILD="$BUILD/epub-fonts"
BODY_FONT="serif"
FONT_CSS=""
FONT_ARGS=()

READER_FONT_DIR="${READER_FONT_DIR:-$HOME/.local/share/fonts}"
if [ ! -f "$READER_FONT_DIR/Reader-Regular.ttf" ] && [ -f "$HOME/Library/Fonts/Reader-Regular.ttf" ]; then
  READER_FONT_DIR="$HOME/Library/Fonts"
fi

if [ -f "$READER_FONT_DIR/Reader-Regular.ttf" ]; then
  mkdir -p "$FONT_BUILD"
  BODY_FONT='"Reader", serif'
  while IFS='|' read -r file weight style; do
    [ -f "$READER_FONT_DIR/$file" ] || continue
    cp "$READER_FONT_DIR/$file" "$FONT_BUILD/$file"
    FONT_ARGS+=(--epub-embed-font "$FONT_BUILD/$file")
    FONT_CSS+="@font-face { font-family: \"Reader\"; src: url(\"../fonts/$file\") format(\"truetype\"); font-weight: $weight; font-style: $style; }"$'\n'
  done <<'FONTS'
Reader-Light.ttf|300|normal
Reader-LightItalic.ttf|300|italic
Reader-Regular.ttf|400|normal
Reader-Italic.ttf|400|italic
Reader-Medium.ttf|500 600|normal
Reader-MediumItalic.ttf|500 600|italic
Reader-Bold.ttf|700 900|normal
Reader-BoldItalic.ttf|700 900|italic
FONTS
fi

cat > "$CSS" <<CSS
/* kindle-reader-fonts-faux-350-v6 */
/* papier-white-page-v1 */
$FONT_CSS
html, body {
  background: #fff !important;
  color: #000;
}
body {
  font-family: $BODY_FONT;
  font-weight: 300;
  line-height: 1.45;
  text-shadow: 0 0 0.35px currentColor;
  -webkit-text-stroke: 0.18px currentColor;
}
p, li, blockquote, td, th { font-weight: 300; }
strong, b { font-weight: 500; text-shadow: none; -webkit-text-stroke: 0; }
h1, h2, h3, h4 {
  font-family: $BODY_FONT;
  font-weight: 500;
  line-height: 1.2;
  page-break-after: avoid;
  break-after: avoid;
}
img { max-width: 100%; height: auto; }
code { font-family: monospace; font-size: 0.82em; line-height: 1.2; }
pre {
  font-family: monospace;
  font-size: 0.76em;
  line-height: 1.18;
  margin: 0.65em 0;
  padding: 0.45em 0.55em;
  overflow-x: auto;
}
pre code { font-size: 1em; line-height: inherit; white-space: pre-wrap; }
table { border-collapse: collapse; width: 100%; }
th, td { border-bottom: 1px solid #ddd; padding: 0.3em 0.45em; vertical-align: top; }
math[display="block"] { display: block; text-align: center; margin: 1em 0; overflow-x: auto; }
math[display="inline"] { display: inline math; }
div.aside, div.concept, div.difficult, div.note {
  margin: 0.9em 0 0.9em 0.8em;
  padding: 0.4em 0.7em;
  border-left: 2px solid #b0a89a;
  background: rgba(0, 0, 0, 0.04);
  font-size: 0.92em;
}
div.aside strong, div.concept strong, div.difficult strong, div.note strong { font-weight: 500; }
CSS

mkdir -p "$(dirname "$OUT")"
(
  cd "$(dirname "$ARTICLE")"
  PANDOC_ARGS=(
    "$(basename "$ARTICLE")"
    --from markdown+smart+yaml_metadata_block+header_attributes+fenced_divs+bracketed_spans+tex_math_dollars
    --to epub3
    --standalone
    --toc
    --mathml
    --css "$CSS"
    --metadata "title=$TITLE"
  )
  if [ "${#FONT_ARGS[@]}" -gt 0 ]; then PANDOC_ARGS+=("${FONT_ARGS[@]}"); fi
  if [ -n "$COVER" ] && [ -s "$COVER" ]; then PANDOC_ARGS+=(--epub-cover-image="$COVER"); fi
  PANDOC_ARGS+=(-o "$OUT")
  pandoc "${PANDOC_ARGS[@]}"
)

python3 - "$OUT" <<'PY'
import re
import sys
import zipfile
from pathlib import Path

epub = Path(sys.argv[1])
tmp = epub.with_suffix(".tmp.epub")
annotation = re.compile(rb"<annotation\b[^>]*>.*?</annotation>", flags=re.S)
with zipfile.ZipFile(epub, "r") as source, zipfile.ZipFile(tmp, "w") as target:
    names = {info.filename for info in source.infolist()}
    if "mimetype" in names:
        target.writestr("mimetype", source.read("mimetype"), compress_type=zipfile.ZIP_STORED)
    for info in source.infolist():
        if info.filename == "mimetype":
            continue
        data = source.read(info.filename)
        if info.filename.endswith((".xhtml", ".html")):
            data = annotation.sub(b"", data)
        target.writestr(info.filename, data, compress_type=zipfile.ZIP_DEFLATED)
tmp.replace(epub)
PY

[ -s "$OUT" ] || { echo "papier-epub: renderer produced no EPUB" >&2; exit 1; }
