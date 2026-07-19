#!/usr/bin/env bash
# papier-compose.sh <jobdir> — agentic document creation for Papier.
#
# Called by papier-upload.js (POST /papier/api/compose). The job dir holds:
#   instructions.md   what the user asked for (links, topic, guidance)
#   status.txt        phase string, polled by the viewer
#   work/             the agent's working directory (article.md + assets/)
#   out/article.pdf   the final typeset PDF (rendered by notes-md2pdf.sh)
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
typeset PDF for an e-ink tablet, so the markdown must be clean and
self-contained.

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
sed -i 's/\\hdots/\\dots/g' "$WORK/article.md"

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

status "typesetting the PDF"
"$MD2PDF" "$WORK/article.md" "$OUT/article.pdf" "$TITLE" >&2

status "done writing"
echo "[compose] ok: $OUT/article.pdf ($TITLE)" >&2
