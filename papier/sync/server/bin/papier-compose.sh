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
# flow (~/Documents/Notes/Clippings/enrich_clippings_agentic.py). URL-only
# requests use a source-aware mode: authored articles and blog posts keep their
# prose, while technical papers and books become pedagogical editions. Explicit
# fidelity or transformation instructions still override that default. In every
# mode the agent localizes images, keeps math/code portable, and the md is
# typeset with the same reMarkable preset (notes-md2pdf.sh = md2pdf.sh --rm2 port).
set -euo pipefail

JOB="${1:?usage: papier-compose.sh <jobdir>}"
WORK="$JOB/work"
OUT="$JOB/out"
MD2PDF="${MD2PDF:-$HOME/bin/notes-md2pdf.sh}"
PI_BIN="${PI_BIN:-pi}"
COVER_TOOL="${PAPIER_COVER_TOOL:-$HOME/bin/papier-compose-cover.py}"
PAPIER_PY="${PAPIER_PY:-$HOME/papier-venv/bin/python3}"
COVER_REQUIRED="${PAPIER_COVER_REQUIRED:-0}"
COVER_PNG="$WORK/cover.png"
COVER_PDF="$WORK/cover.pdf"
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

EXHAUSTIVE=false
if grep -Eqi 'full(est)?([[:space:]-]+(page|paper|book|article|post))?|complete[[:space:]-]+(paper|book|article|post|edition)|all[[:space:]]+(sections|chapters|entries)|every[[:space:]]+chapter|each[[:space:]]+entry|do[[:space:]]+not[[:space:]]+summarize' "$JOB/instructions.md"; then
  EXHAUSTIVE=true
fi

# Explicit fidelity language wins even if it mentions words such as "rephrase"
# in a negative instruction. A bare URL uses source-aware mode: the agent keeps
# authored article/blog prose, but may simplify and augment papers and books.
SOURCE_PRESERVING=false
SOURCE_AWARE=false
URL_COUNT="$( { grep -Eo 'https?://[^[:space:]<>()]+' "$JOB/instructions.md" 2>/dev/null || true; } | wc -l | tr -d ' ')"
PRESERVE_RE='do[[:space:]]+not[[:space:]]+(rewrite|rephrase|summari[sz]e)|without[[:space:]]+(rewriting|rephrasing|summari[sz]ing)|verbatim|as[-[:space:]]+is|format(ting)?[[:space:]]+only|reading[[:space:]-]+edition|preserve[[:space:]]+(the[[:space:]]+)?(wording|prose|text)'
TRANSFORM_RE='summari[sz]e|rewrite|rephrase|write[-[:space:]]*up|write[[:space:]]+(an?|the)[[:space:]]+(new[[:space:]]+)?(article|essay|guide|tutorial)|create[[:space:]]+(an?|the)[[:space:]]+(article|essay|guide|tutorial)|explain|compare|contrast|synthesi[sz]e|analy[sz]e|critique|expand|enrich|adapt|turn.+into'
if grep -Eqi "$PRESERVE_RE" "$JOB/instructions.md"; then
  SOURCE_PRESERVING=true
elif [ "${URL_COUNT:-0}" -gt 0 ] && ! grep -Eqi "$TRANSFORM_RE" "$JOB/instructions.md"; then
  SOURCE_AWARE=true
fi
if $SOURCE_PRESERVING; then EXHAUSTIVE=true; fi

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
instructions below. Your deliverables are article.md and cover-brief.txt
in the current directory. A separate renderer turns the article into the
selected PDF and/or EPUB outputs, so the markdown must be clean, portable,
and self-contained.

--- USER INSTRUCTIONS ---
PROMPT
cat "$JOB/instructions.md"
if $SOURCE_PRESERVING; then
cat <<'PROMPT'
--- COMPOSITION MODE: SOURCE-PRESERVING READING EDITION ---
PROMPT
elif $SOURCE_AWARE; then
cat <<'PROMPT'
--- COMPOSITION MODE: SOURCE-AWARE READING EDITION ---
PROMPT
else
cat <<'PROMPT'
--- COMPOSITION MODE: TRANSFORMATIVE ARTICLE ---
PROMPT
fi
cat <<'PROMPT'
--- END USER INSTRUCTIONS ---

## Step 1 — Gather the sources

- If the instructions contain links, fetch each one and read it properly
  (curl -L with a browser User-Agent; for arXiv prefer
  https://arxiv.org/abs/ID and https://ar5iv.labs.arxiv.org/html/ID).
- If a linked page is a table of contents, index, or landing page, follow the
  linked chapters or entries needed to satisfy the request. Do not mistake the
  index text for the full source.
- Preserve explanatory figures and diagrams while gathering sources. Inspect
  source Markdown or the upstream repository when a site's static HTML exposes
  an empty figure that is filled by client-side JavaScript.
- If a fetch fails, SELF-RESCUE and retry through the publisher's canonical
  HTML, RSS/API, print view, or upstream repository. In SOURCE-PRESERVING mode,
  never substitute another author or source and never invent missing passages.
  In TRANSFORMATIVE mode, an authoritative substitute is acceptable. Never
  write fetch-failure meta-commentary into the article.
- If the instructions are a topic rather than links, research it from
  authoritative sources.
- In TRANSFORMATIVE mode, extract the core claims, equations, results, and
  difficult concepts. In SOURCE-PRESERVING mode, extract the complete main
  article body and its original structure instead of reducing it to claims.
- In SOURCE-AWARE mode, classify each source by its dominant reading experience
  before writing. Extract a prose-led source's complete authored body and
  structure. Only for genuinely dense technical-reference material, extract
  the full set of claims, arguments, equations, examples, code, results, and
  qualifications needed for a faithful pedagogical edition.

## Step 2 — Write the article (article.md)

Produce the document required by the selected composition mode.

### SOURCE-PRESERVING READING EDITION rules

- This is faithful transcription and formatting, not authorship. Preserve the
  author's wording sentence-for-sentence and paragraph-for-paragraph. Do not
  paraphrase, condense, expand, fact-check, correct, modernize, or improve the
  prose—even when you think a different explanation would be clearer.
- Preserve the original title, subtitle, author names, publication date,
  heading order and wording, paragraphs, quotations, lists, footnotes/endnotes,
  captions, tables, equations, and code. Convert their presentation to clean
  Markdown, but do not change their content.
- Begin at the first substantive reader-facing content, such as the abstract,
  deck, or opening paragraph. Omit front matter that exists only for publication
  administration: author affiliations and contact addresses, copyright/copying
  or reprint permission notices, journal or conference identifiers,
  submission/acceptance metadata, price lines, and publisher boilerplate. Keep
  author names and the original publication date in a compact byline when
  useful. Do not treat an abstract, foreword, preface, epigraph, dedication,
  contents list, or substantive author's note as boilerplate.
- Remove only website chrome and non-article UI: navigation, share/like/comment
  controls, subscription prompts, cookie banners, repeated publication
  footers, related-post modules, and advertisements. A sponsor passage inside
  the authored post remains unless it is clearly injected site chrome.
- Do not add a new introduction, conclusion, prerequisites, worked example,
  transition, summary, or Sources section. Do not merge, rename, reorder, or
  synthesize sections. You MAY add a concise `::: aside` immediately after a
  genuinely dense passage to explain prerequisite context, terminology, an
  equation, or a difficult inference. Begin it with **Reader's note.** Keep the
  original passage unchanged, make the addition clearly editorial, and never
  use notes to summarize, fact-check, argue with, or repeatedly interrupt the
  author.
- For multiple posts, keep the URLs in the user's order. Give each post a
  clear boundary using its original title, subtitle, byline, and date, then
  preserve its complete body. Do not blend several authors' posts into one
  continuous narrative.
- Preserve meaningful hyperlinks inline. Localize every substantive source
  image in its original position and retain the original caption/alt text.
- Before finishing, compare article.md against every source from beginning to
  end. Every main-body block must be represented once and in the same order.

### SOURCE-AWARE READING EDITION rules

- First classify each source by its dominant reading experience, not merely by
  its URL, file format, venue, or label such as "paper" or "book." Prose-led
  sources include authored essays, blog posts, newsletters, interviews,
  magazine articles, narrative nonfiction, and historical, economic,
  humanities, survey, argumentative, or essayistic academic papers and books.
- Treat a source as technical-reference material only when its primary value is
  formal technical instruction or analysis and understanding it depends on
  dense equations, algorithms, code, formal definitions, specifications, or
  procedures. Being academic, peer-reviewed, long, or published as a PDF does
  NOT by itself justify rewriting it.
- For prose-led sources, follow the SOURCE-PRESERVING rules above: retain the
  author's wording, voice, paragraph order, headings, examples, and complete
  body. Add only occasional, clearly labeled Reader's note asides where a dense
  passage genuinely benefits from explanation; never replace that passage.
- For technical-reference material, create a pedagogical edition. You may
  simplify sentences, reorganize exposition, introduce prerequisites, carry a
  worked example, and add explanations or connective tissue. Preserve the
  source's claims, results, equations, algorithms, code, important examples,
  limitations, and citations faithfully. Never turn a precise technical claim
  into a looser or stronger one.
- When a prose-led source contains a few dense technical passages, preserve the
  surrounding prose and those passages verbatim, then add sparse, clearly
  labeled Reader's note asides. Do not transform the whole work merely because
  technical material appears inside it.
- When the request asks for full or complete coverage, preserve the source's
  full technical scope even if you reorganize it for clarity. Do not collapse a
  paper or book into a short summary.
- Treat mixed source sets per source. Do not rewrite an essay merely because a
  linked formal paper is technical, and do not transcribe a dense instructional
  source merely because a linked post is prose-led. If classification is
  genuinely ambiguous, favor preserving the author's prose and add clearly
  labeled explanations.

### TRANSFORMATIVE ARTICLE rules

- Write one long-form, genuinely readable teaching article in the voice of a
  careful, slightly informal textbook: confident, precise, and concrete.
- Carry a concrete worked example through the article where the material
  allows, and state prerequisites specifically.
- Put short asides near first use for genuinely difficult concepts.
- End with a short "Sources" section listing what you actually read.
- Match the depth the instructions ask for. When the user has not requested
  exhaustive coverage, default to roughly 2000–5000 words. Exhaustive jobs
  may be much longer.

### Shared requirements

- Treat requests such as "the full page", "all sections", "every chapter",
  "each entry", or "do not summarize" as EXHAUSTIVE. Preserve the source order
  and give every requested chapter or entry its own descriptive section. Carry
  over its substantive explanations, examples, equations, and important
  qualifications. Do not merge entries into a thematic summary, and do not
  stop at a normal article length merely to stay concise. Before finishing,
  compare the article's section list against the source table of contents and
  add anything missing.

- Start with YAML frontmatter:
  ---
  title: "The Title"
  ---
  Put a compact author/date byline in an optional YAML `subtitle:` field,
  rather than as the first body paragraph; otherwise the renderer's opening
  drop cap can incorrectly consume the author's initial.
  Then go STRAIGHT into the body with ## sections. Do NOT repeat the
  title as a "# ..." heading — the renderer typesets the frontmatter
  title itself, so a body H1 would print it twice.
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
- Images: for ordinary jobs, include the source images that materially improve
  understanding. For EXHAUSTIVE jobs, preserve EVERY unique substantive source
  figure or diagram, in source order, at its first relevant section. Deduplicate
  identical figures, but do not replace them with prose. Download them into
  assets/ and reference them as ![caption](assets/file.svg) (PNG/JPEG/WebP are
  also supported). NEVER hotlink remote images.
- Some sites use custom image schemes. For the Modal GPU Glossary, resolve
  themed-image://NAME.svg to
  https://modal-cdn.com/gpu-glossary/light-NAME.svg, then download the light
  SVG into assets/ for the white e-ink page. Do not discard an image merely
  because its original reference is not an http(s) URL.
- For an EXHAUSTIVE job, write figure-manifest.tsv with one tab-separated line
  per unique source figure:
    included<TAB>original source reference<TAB>assets/local-file.ext<TAB>caption
  Use `skipped` instead of `included` only after retries genuinely fail, with
  the reason in the last field. If the complete source truly contains no
  substantive figures, write one `none` line and explain how you verified it.
- No raw HTML. No hyperlink-only "see also" dumps. Links must be inline as
  plain [text](url) so the reader remains useful offline.
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
- In SOURCE-PRESERVING mode, include exercises only if they occur in the source
  and keep their original wording. In TRANSFORMATIVE mode, exercises may be
  added only when requested or when they genuinely fit.

## Step 3 — Write the cover brief (cover-brief.txt)

Write one plain-text paragraph of 35–70 words describing a single concrete
visual metaphor for this article. Use one dominant object or scene and at
most three supporting elements. Favor a surprising human-scale metaphor
over generic robots, brains, circuit boards, or glowing AI imagery. Describe
only the subject and composition—not an art style. Do not request lettering,
a title, numbers, logos, frames, or a mockup; Papier adds the real title.

## Step 4 — Verify

- Re-read article.md top to bottom. Fix broken math delimiters, unclosed
  code fences, and any reference to an image that is not in assets/.
- For an exhaustive job, compare figure-manifest.tsv with the source and with
  article.md. Every included asset must exist and appear in the article; every
  substantive source figure must be accounted for.
- Confirm the YAML title matches the article.
- Confirm cover-brief.txt is one paragraph and contains no instructions to
  draw words, lettering, numbers, logos, frames, or mockups.

Work in the current directory. Write article.md and cover-brief.txt there, plus
figure-manifest.tsv when the request is exhaustive.
When done, reply with exactly one line: the final title of the article.
PROMPT
} > "$JOB/prompt.md"

cd "$WORK"

# Headless pi run; stdout's last line is the title (best effort).
set +e
"$PI_BIN" -p --no-session "@$JOB/prompt.md" 2>&1 >"$JOB/agent.stdout.log" \
  | tee -a "$JOB/agent.stderr.log" >&2
AGENT_RC=${PIPESTATUS[0]}
set -e
AGENT_OUT="$(cat "$JOB/agent.stdout.log")"

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

collect_figure_problems() {
  $EXHAUSTIVE || return 0
  local manifest="$WORK/figure-manifest.tsv" problems="" rows=0
  if [ ! -s "$manifest" ]; then
    printf '%s\n' "Exhaustive source-figure coverage is unaudited: create figure-manifest.tsv and include every substantive source figure."
    return 0
  fi
  while IFS=$'\t' read -r disposition source asset detail; do
    [ -z "$disposition" ] && continue
    case "$disposition" in
      included)
        rows=$((rows + 1))
        if [[ "$asset" != assets/* ]]; then
          problems+="Manifest entry must use a local assets/ path: $source -> $asset"$'\n'
        elif [ ! -f "$WORK/$asset" ]; then
          problems+="Manifest asset does not exist: $source -> $asset"$'\n'
        elif ! grep -Fq "$asset" "$WORK/article.md"; then
          problems+="Manifest asset is not referenced by article.md: $source -> $asset"$'\n'
        fi
        ;;
      skipped|none) rows=$((rows + 1)) ;;
      *) problems+="Bad figure-manifest.tsv disposition '$disposition' for: $source"$'\n' ;;
    esac
  done < "$manifest"
  [ "$rows" -gt 0 ] || problems+="figure-manifest.tsv has no figure records."$'\n'
  printf '%s' "$problems" | sed '/^[[:space:]]*$/d'
}

for PASS in 1 2 3; do
  PROBLEMS="$(collect_render_problems)"
  FIGURE_PROBLEMS="$(collect_figure_problems)"
  if [ -n "$FIGURE_PROBLEMS" ]; then
    PROBLEMS="${PROBLEMS:+$PROBLEMS$'\n'}$FIGURE_PROBLEMS"
  fi
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
- For ordinary missing images, repair the download or remove the reference.
  For any problem mentioning exhaustive source-figure coverage or
  figure-manifest.tsv, DO NOT replace the figure with prose: download and
  include the required local asset, update article.md, and create or correct
  figure-manifest.tsv.
- Do not restructure the article.

--- PROBLEMS REPORTED BY THE RENDERER ---
FIX
  cat "$JOB/render-problems.txt"
  cat <<'FIX'
--- END PROBLEMS ---

Edit article.md now. When done, reply with one line: FIXED.
FIX
  } > "$JOB/fix-prompt.md"
  set +e
  "$PI_BIN" -p --no-session "@$JOB/fix-prompt.md" 2>&1 >>"$JOB/agent.stdout.log" \
    | tee -a "$JOB/agent.stderr.log" >&2
  set -e
done

FINAL_FIGURE_PROBLEMS="$(collect_figure_problems)"
if [ -n "$FINAL_FIGURE_PROBLEMS" ]; then
  status "failed: exhaustive source-figure coverage incomplete"
  printf '%s\n' "$FINAL_FIGURE_PROBLEMS" >&2
  exit 1
fi

# Resolve the title: YAML title -> first heading -> agent's last line.
TITLE="$(awk -F': *' '/^title:/ { sub(/^title: */, ""); gsub(/^"|"$/, ""); print; exit }' "$WORK/article.md" || true)"
[ -n "$TITLE" ] || TITLE="$(grep -m1 '^# ' "$WORK/article.md" | sed 's/^# *//' || true)"
[ -n "$TITLE" ] || TITLE="$(printf '%s' "$AGENT_OUT" | tail -n1 | head -c 160)"
[ -n "$TITLE" ] || TITLE="Composed document"
printf '%s' "$TITLE" > "$JOB/title.txt"

# Generate one image-led cover page. The title is typeset locally so it stays
# sharp and correct; the image model never draws lettering. Cover failures are
# non-fatal by default so an external image outage cannot discard the article.
COVER_READY=false
if [ ! -x "$PAPIER_PY" ]; then PAPIER_PY="$(command -v python3 || true)"; fi
if [ -f "$COVER_TOOL" ] && [ -n "$PAPIER_PY" ] \
   && { [ -n "${OPENROUTER_API_KEY:-}" ] || [ -n "${PAPIER_COVER_SOURCE_IMAGE:-}" ]; }; then
  status "creating cover art"
  if "$PAPIER_PY" "$COVER_TOOL" generate "$TITLE" "$WORK/cover-brief.txt" "$COVER_PNG" "$COVER_PDF" >&2; then
    COVER_READY=true
  else
    echo "[compose] cover generation failed; continuing without a cover" >&2
    if [ "$COVER_REQUIRED" = 1 ]; then
      status "failed: cover generation failed"
      exit 1
    fi
  fi
else
  echo "[compose] cover generator or OpenRouter key unavailable; continuing without a cover" >&2
fi

if $WANT_PDF; then
  status "typesetting the PDF"
  BODY_PDF="$OUT/article.body.pdf"
  "$MD2PDF" "$WORK/article.md" "$BODY_PDF" "$TITLE" >&2
  if $COVER_READY; then
    if "$PAPIER_PY" "$COVER_TOOL" prepend "$COVER_PDF" "$BODY_PDF" "$OUT/article.pdf" >&2; then
      rm -f "$BODY_PDF"
    else
      echo "[compose] cover prepend failed; shipping the article without a cover" >&2
      mv "$BODY_PDF" "$OUT/article.pdf"
      if [ "$COVER_REQUIRED" = 1 ]; then
        status "failed: cover prepend failed"
        exit 1
      fi
    fi
  else
    mv "$BODY_PDF" "$OUT/article.pdf"
  fi
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
    if $COVER_READY && [ -s "$COVER_PNG" ]; then PANDOC_ARGS+=(--epub-cover-image "$COVER_PNG"); fi
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
