#!/usr/bin/env bash
# alt-ui-compose.sh <jobdir> — agentic document creation for Paper.
#
# Called by alt-ui-upload.js (POST /paper/api/compose). The job dir holds:
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

JOB="${1:?usage: alt-ui-compose.sh <jobdir>}"
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
You are a document-composition agent for Paper (a reMarkable tablet
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
  renderer uses native MathML — stick to standard LaTeX.
- Code in fenced blocks with language tags.
- Images: download any image you want to include into assets/ (curl with
  a Referer of the page it came from) and reference it as
  ![caption](assets/file.png). NEVER hotlink remote images. Skip images
  that fail to download.
- No raw HTML. No hyperlink-only "see also" dumps. Keep external links
  sparse and inline as plain [text](url) — the reader is offline.
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
