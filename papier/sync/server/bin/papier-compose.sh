#!/usr/bin/env bash
# papier-compose.sh <jobdir> — agentic document creation for Papier.
#
# Called by papier-upload.js (POST /papier/api/compose). The job dir holds:
#   instructions.md   what the user asked for (links, topic, guidance)
#   status.txt        phase string, polled by the viewer
#   work/             the agent's working directory (article.md + assets/)
#   formats.txt       selected outputs, one of pdf/epub per line (default pdf)
#   out/article.pdf   optional fixed-layout PDF (notes-md2pdf.sh)
#   out/article.epub  optional reflowable EPUB 3 (Pandoc + MathML)
#   title.txt         resolved document title
#
# The writing style/pipeline is a port of the local Clippings enrichment
# flow (~/Documents/Notes/Clippings/enrich_clippings_agentic.py) minus the
# per-link reference appendix and the quiz: one pi agent researches the
# links/topic, writes ONE self-contained teaching article, localizes images,
# keeps math/code portable, and the md is typeset with the same reMarkable
# preset (notes-md2pdf.sh = md2pdf.sh --rm2 port).
set -euo pipefail

JOB="${1:?usage: papier-compose.sh <jobdir>}"
WORK="$JOB/work"
OUT="$JOB/out"
MD2PDF="${MD2PDF:-$HOME/bin/notes-md2pdf.sh}"
PI_BIN="${PI_BIN:-pi}"
FORMATS_FILE="$JOB/formats.txt"
WANT_PDF=false
WANT_EPUB=false

if [ ! -s "$FORMATS_FILE" ]; then printf '%s\n' pdf > "$FORMATS_FILE"; fi
while IFS= read -r format; do
  case "$format" in
    pdf) WANT_PDF=true ;;
    epub) WANT_EPUB=true ;;
    '') ;;
    *) echo "[compose] bad output format: $format" >&2; exit 1 ;;
  esac
done < "$FORMATS_FILE"
if ! $WANT_PDF && ! $WANT_EPUB; then
  echo "[compose] no output formats selected" >&2
  exit 1
fi

status() { printf '%s' "$1" > "$JOB/status.txt"; echo "[compose] $1" >&2; }

# The service runs under systemd without a login env; pick up API keys.
if [ -f "$HOME/.env" ]; then set -a; . "$HOME/.env"; set +a; fi

mkdir -p "$WORK/assets" "$OUT"

status "researching sources"

# The prompt is assembled from QUOTED heredocs with the instructions file
# cat'd between them: user text must never pass through shell expansion
# (a $(...) in pasted instructions would otherwise execute here).
{
cat <<'PROMPT'
You are a document-composition agent for Papier (a reMarkable tablet
library). The user wants a new reading document created from the
instructions below. Your final deliverable is ONE markdown file at
article.md in the current directory. A separate renderer turns it into a
typeset the selected PDF and/or EPUB outputs, so the markdown must be
clean, portable, and self-contained.

--- USER INSTRUCTIONS ---
PROMPT
cat "$JOB/instructions.md"
cat <<'PROMPT'
--- END USER INSTRUCTIONS ---

## Step 1 — Gather the sources

- If the instructions contain links, fetch each one and read it properly
  (curl -L with a browser User-Agent; for arXiv prefer
  https://arxiv.org/abs/ID and https://ar5iv.labs.arxiv.org/html/ID).
- If a fetch fails, SELF-RESCUE: retry, then search for an authoritative
  substitute (author's blog, official docs, Wikipedia, a good survey).
  Never give up after one failed fetch, and NEVER write meta-commentary
  about fetch failures into the article.
- If the instructions are a topic rather than links, research it from
  authoritative sources.
- Extract the core claims, key equations, key numbers/results, and every
  concept a smart reader without this background would trip on.

## Step 2 — Write the article (article.md)

One long-form, genuinely readable teaching article that covers the
material thoroughly. Requirements:

- Start with YAML frontmatter:
  ---
  title: "The Title"
  ---
  Then go STRAIGHT into the body with ## sections. Do NOT repeat the
  title as a "# ..." heading — the renderer typesets the frontmatter
  title itself, so a body H1 would print it twice.
- Write in the voice of a careful, slightly informal textbook: confident,
  precise, concrete. No marketing tone, no filler, no "as an AI".
- Carry a concrete worked example through the article where the material
  allows — small real numbers the reader can trace by hand.
- Put short asides near first use for genuinely difficult concepts.
- State prerequisites specifically ("basic linear algebra: column space,
  projections, Frobenius norm" — not "some math background").
- Do not invent specific facts (numbers, dates, theorem names,
  citations). Mark anything inferred rather than read as "(inferred)".
- Math: $...$ inline, $$...$$ display. Use \dots (never \hdots). The
  renderer converts math with Pandoc's MathML backend — use only
  standard LaTeX it supports. NO color or styling macros (\colorbox,
  \fcolorbox, \textcolor, \color, \bbox, \cancel, \bm), no
  \newcommand. The output is a grayscale e-ink screen: express emphasis
  structurally (\mathbf, \boldsymbol, \underbrace, \text{...} labels),
  never with color. When quoting math from a source that uses color,
  strip the color and keep the mathematics.
- Code in fenced blocks with language tags.
- Images: download any image you want to include into assets/ (curl with
  a Referer of the page it came from) and reference it as
  ![caption](assets/file.png). NEVER hotlink remote images. Skip images
  that fail to download.
- No raw HTML. No hyperlink-only "see also" dumps. Keep external links
  sparse and inline as plain [text](url) — the reader is offline.
- Exercises: if the document includes exercises or practice problems
  (because the user asked, or they genuinely fit), give EACH exercise
  its OWN PAGE — the reader works them in ink directly on the tablet,
  so the rest of the page must stay blank as writing room. Start every
  exercise by emitting exactly this fenced div on its own lines,
  immediately before the exercise heading:

  ::: {.page-break}
  :::

  Then a short heading ("### Exercise 3") and ONLY the problem
  statement. Never put two exercises on one page, and never let prose
  continue after an exercise statement. If you include solutions, put
  them in a final "Solutions" section that also starts with a
  page-break div (solutions may share pages with each other).
- End with a short "Sources" section listing what you actually read
  (title + URL, one line each).
- Length: substantial but not bloated. Match the depth the instructions
  ask for; default to a thorough read of roughly 2000-5000 words.

## Step 3 — Verify

- Re-read article.md top to bottom. Fix broken math delimiters, unclosed
  code fences, and any reference to an image that is not in assets/.
- Confirm the YAML title matches the article.

Work in the current directory. Write article.md there. When done, reply
with exactly one line: the final title of the article.
PROMPT
} > "$JOB/prompt.md"

cd "$WORK"

# Headless pi run; stdout's last line is the title (best effort).
set +e
AGENT_OUT="$("$PI_BIN" -p --no-session "@$JOB/prompt.md" 2>"$JOB/agent.stderr.log")"
AGENT_RC=$?
set -e
printf '%s\n' "$AGENT_OUT" > "$JOB/agent.stdout.log"

if [ ! -s "$WORK/article.md" ]; then
  status "failed: agent produced no article.md (exit $AGENT_RC)"
  echo "agent exit $AGENT_RC and no article.md" >&2
  exit 1
fi

# Portable-math normalization (same fix enrich_clippings applies).
sed 's/\\hdots/\\dots/g' "$WORK/article.md" > "$WORK/article.md.tmp"
mv "$WORK/article.md.tmp" "$WORK/article.md"

# ---- review pass: catch what the typesetter cannot render ---------------
# Pandoc's MathML converter rejects some LaTeX (\colorbox, \textcolor, ...)
# and leaves it as RAW TeX TEXT in the PDF. Detect those — plus referenced
# image files that don't exist — and send the agent back to repair
# article.md, up to two fix rounds before shipping anyway.
collect_render_problems() {
  local problems missing=""
  problems="$(pandoc "$WORK/article.md" \
      --from markdown+smart+tex_math_dollars --to html5 --mathml -o /dev/null 2>&1 \
    | grep -A3 'Could not convert TeX math' | head -120 || true)"
  while IFS= read -r img; do
    [ -z "$img" ] && continue
    [ -f "$WORK/$img" ] || missing="$missing  $img"$'\n'
  done < <(grep -o 'assets/[A-Za-z0-9._/-]*' "$WORK/article.md" 2>/dev/null | sort -u)
  if [ -n "$missing" ]; then
    problems="$problems"$'\n'"Referenced image files that do not exist on disk (remove or fix these references):"$'\n'"$missing"
  fi
  printf '%s' "$problems" | sed '/^[[:space:]]*$/d'
}

for PASS in 1 2 3; do
  PROBLEMS="$(collect_render_problems)"
  [ -z "$PROBLEMS" ] && { echo "[compose] review clean (pass $PASS)" >&2; break; }
  if [ "$PASS" = 3 ]; then
    echo "[compose] unresolved rendering problems after 2 fix passes — shipping anyway:" >&2
    printf '%s\n' "$PROBLEMS" >&2
    break
  fi
  status "fixing typesetting issues (pass $PASS)"
  printf '%s\n' "$PROBLEMS" > "$JOB/render-problems.txt"
  {
  cat <<'FIX'
You previously wrote article.md in the current directory (it is there
now). The PDF typesetter reported problems that would appear as raw
LaTeX text or broken images in the final document. Fix article.md IN
PLACE, changing as little else as possible.

Rules:
- The renderer converts math with Pandoc's MathML backend. It does NOT
  support color/styling macros: \colorbox, \fcolorbox, \textcolor,
  \color, \bbox, \style, \class, \cancel, \bm, \hdots. Rewrite any such
  math in plain supported LaTeX (\mathbf, \boldsymbol, \underbrace,
  \text{...} labels). The document is for a grayscale e-ink screen —
  express emphasis structurally, never with color.
- For every "Could not convert TeX math" snippet below, find it in
  article.md and rewrite it so it parses as standard LaTeX.
- For every missing image file, remove the image reference or replace
  it with a short text description.
- Do not add new content and do not restructure the article.

--- PROBLEMS REPORTED BY THE RENDERER ---
FIX
  cat "$JOB/render-problems.txt"
  cat <<'FIX'
--- END PROBLEMS ---

Edit article.md now. When done, reply with one line: FIXED.
FIX
  } > "$JOB/fix-prompt.md"
  set +e
  "$PI_BIN" -p --no-session "@$JOB/fix-prompt.md" >> "$JOB/agent.stdout.log" 2>>"$JOB/agent.stderr.log"
  set -e
done

# Resolve the title: YAML title -> first heading -> agent's last line.
TITLE="$(awk -F': *' '/^title:/ { sub(/^title: */, ""); gsub(/^"|"$/, ""); print; exit }' "$WORK/article.md" || true)"
[ -n "$TITLE" ] || TITLE="$(grep -m1 '^# ' "$WORK/article.md" | sed 's/^# *//' || true)"
[ -n "$TITLE" ] || TITLE="$(printf '%s' "$AGENT_OUT" | tail -n1 | head -c 160)"
[ -n "$TITLE" ] || TITLE="Composed document"
printf '%s' "$TITLE" > "$JOB/title.txt"

if $WANT_PDF; then
  status "typesetting the PDF"
  "$MD2PDF" "$WORK/article.md" "$OUT/article.pdf" "$TITLE" >&2
fi

if $WANT_EPUB; then
  status "typesetting the EPUB"
  EPUB_CSS="$OUT/epub.css"
  EPUB_FONT_DIR="$OUT/epub-fonts"
  READER_FONT_DIR="${READER_FONT_DIR:-$HOME/.local/share/fonts}"
  EPUB_BODY_FONT="serif"
  EPUB_FONT_CSS=""
  EPUB_FONT_ARGS=()

  if [ -f "$READER_FONT_DIR/Reader-Regular.ttf" ]; then
    mkdir -p "$EPUB_FONT_DIR"
    EPUB_BODY_FONT='"Reader", serif'
    while IFS='|' read -r file weight style; do
      [ -f "$READER_FONT_DIR/$file" ] || continue
      cp "$READER_FONT_DIR/$file" "$EPUB_FONT_DIR/$file"
      EPUB_FONT_ARGS+=(--epub-embed-font "$EPUB_FONT_DIR/$file")
      EPUB_FONT_CSS+="@font-face { font-family: \"Reader\"; src: url(\"../fonts/$file\") format(\"truetype\"); font-weight: $weight; font-style: $style; }"$'\n'
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

  cat > "$EPUB_CSS" <<CSS
/* kindle-reader-fonts-faux-350-v6 */
/* papier-white-page-v1 */
$EPUB_FONT_CSS
html, body {
  background: #fff !important;
  color: #000;
}
body {
  font-family: $EPUB_BODY_FONT;
  font-weight: 300;
  line-height: 1.45;
  text-shadow: 0 0 0.35px currentColor;
  -webkit-text-stroke: 0.18px currentColor;
}
p, li, blockquote, td, th { font-weight: 300; }
strong, b { font-weight: 500; text-shadow: none; -webkit-text-stroke: 0; }
h1, h2, h3, h4 {
  font-family: $EPUB_BODY_FONT;
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

  (
    cd "$WORK"
    PANDOC_ARGS=(
      article.md
      --from markdown+smart+yaml_metadata_block+header_attributes+fenced_divs+bracketed_spans+tex_math_dollars
      --to epub3
      --standalone
      --toc
      --mathml
      --css "$EPUB_CSS"
      --metadata "title=$TITLE"
    )
    if [ "${#EPUB_FONT_ARGS[@]}" -gt 0 ]; then PANDOC_ARGS+=("${EPUB_FONT_ARGS[@]}"); fi
    PANDOC_ARGS+=(-o "$OUT/article.epub")
    pandoc "${PANDOC_ARGS[@]}"
  ) >&2

  # Weak EPUB readers sometimes show Pandoc's embedded TeX source beside
  # MathML. Mirror the Clippings renderer: remove those annotations while
  # keeping the required mimetype member first and uncompressed.
  python3 - "$OUT/article.epub" <<'PY'
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
fi

status "done writing"
OUTPUTS=""
$WANT_PDF && OUTPUTS="$OUTPUTS $OUT/article.pdf"
$WANT_EPUB && OUTPUTS="$OUTPUTS $OUT/article.epub"
echo "[compose] ok:$OUTPUTS ($TITLE)" >&2
