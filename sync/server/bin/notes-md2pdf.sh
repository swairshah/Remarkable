#!/usr/bin/env bash
# notes-md2pdf.sh — render one markdown note to a reMarkable-ready PDF.
#
# Server-side port of the local Clippings md2pdf.sh with the --rm2 preset
# baked in: 157x210mm pages (reMarkable 2 aspect), large e-ink typography,
# pure white paper, Reader/EB Garamond body + Google Sans Code mono, native
# MathML (no MathJax → no Chrome shrink-to-fit), and a post-layout JS pass
# that scales down any display math / table / pre that would overflow the
# page width.
#
# Usage:
#   notes-md2pdf.sh input.md output.pdf [fallback-title]
#
# The fallback title is used only when the markdown has no YAML `title:`.
#
# Env:
#   CHROME       chrome binary (auto-detected otherwise)
#   FONT_DIR     dir holding Reader-*.ttf / GoogleSansCode-*.ttf / EBGaramond-*.ttf
#                (default: ~/.local/share/fonts, then ~/Library/Fonts)
#   KEEP_HTML=1  keep the intermediate HTML next to the PDF
#   MD2PDF_COLOR=1  keep the colored palette (red accents, warm grays)
#                instead of the default pure-black e-ink text
#   RM_BODY_PT, RM_LINE_H, RM_PAGE_MARGIN, RM_TITLE_PT, RM_H2_PT, RM_H3_PT,
#   RM_H4_PT, RM_DROPCAP_EM, RM_P_GAP, RM_TITLE_GAP, RM_H_GAP_TOP
#                per-render typography tuning (same knobs as md2pdf.sh --rm2)
#
# Deps: pandoc, Chrome/Chromium.

set -euo pipefail

MD="${1:?usage: notes-md2pdf.sh input.md output.pdf [title]}"
PDF="${2:?usage: notes-md2pdf.sh input.md output.pdf [title]}"
FALLBACK_TITLE="${3:-$(basename "${MD%.md}")}"

find_chrome() {
  if [[ -n "${CHROME:-}" ]]; then printf '%s' "$CHROME"; return; fi
  local c
  for c in google-chrome google-chrome-stable chromium chromium-browser; do
    if command -v "$c" >/dev/null 2>&1; then command -v "$c"; return; fi
  done
  local mac="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
  if [[ -x "$mac" ]]; then printf '%s' "$mac"; return; fi
  printf ''
}

CHROME_BIN="$(find_chrome)"
[[ -n "$CHROME_BIN" ]] || { echo "notes-md2pdf: no chrome/chromium found (set CHROME=)" >&2; exit 1; }
command -v pandoc >/dev/null 2>&1 || { echo "notes-md2pdf: pandoc not found" >&2; exit 1; }

# Chrome's sandbox needs unprivileged user namespaces, which Ubuntu 24.04
# restricts; we render only our own generated HTML, so dropping it is fine.
CHROME_SANDBOX_FLAGS=()
[[ "$(uname)" == "Linux" ]] && CHROME_SANDBOX_FLAGS=(--no-sandbox)

if [[ -z "${FONT_DIR:-}" ]]; then
  if [[ -f "$HOME/.local/share/fonts/Reader-Regular.ttf" ]]; then
    FONT_DIR="$HOME/.local/share/fonts"
  else
    FONT_DIR="$HOME/Library/Fonts"
  fi
fi

HEADER="$(mktemp -t notes-md2pdf.XXXXXX).html"
trap 'rm -f "$HEADER"' EXIT

write_header() {
  # rm2 preset: e-ink pure white paper, true-black text. Warm ink (#1a1a1a),
  # red accent (#8a2a2a), and mid-gray muted (#6b6b6b) all dither to
  # washed-out gray on the reMarkable's monochrome panel, so text colors are
  # forced to black / near-black; hierarchy comes from size, weight, and
  # italics. Set MD2PDF_COLOR=1 to keep the colored palette (for PDFs meant
  # for color screens rather than the tablet).
  local paper="#ffffff" ink="#000000" muted="#3a3a3a" rule="#bbbbbb" accent="#000000" code_bg="#f1f1f1"
  if [[ "${MD2PDF_COLOR:-0}" == 1 ]]; then
    ink="#1a1a1a"; muted="#6b6b6b"; rule="#cfcfcf"; accent="#8a2a2a"
  fi
  # reMarkable 2: 1404x1872 px @ 226dpi → roughly 157x210mm screen.
  local page_size="157mm 210mm"
  local page_margin="${RM_PAGE_MARGIN:-10mm 12mm 11mm 12mm}"
  local body_pt="${RM_BODY_PT:-13pt}"
  local line_h="${RM_LINE_H:-1.32}"
  local title_pt="${RM_TITLE_PT:-20pt}"
  local h2_pt="${RM_H2_PT:-15.5pt}"
  local h3_pt="${RM_H3_PT:-13.5pt}"
  local h4_pt="${RM_H4_PT:-9.5pt}"
  local dropcap_em="${RM_DROPCAP_EM:-2.8em}"
  local p_align="left"
  local p_gap="${RM_P_GAP:-0.5em}"
  local title_gap="${RM_TITLE_GAP:-16pt}"
  local h_gap_top="${RM_H_GAP_TOP:-1.2em}"

  local serif_stack='"EB Garamond","Iowan Old Style","Hoefler Text",Georgia,serif'
  local reader_font_css=""
  if [[ -f "$FONT_DIR/Reader-Regular.ttf" ]]; then
    serif_stack='"Reader","EB Garamond","Iowan Old Style","Hoefler Text",Georgia,serif'
    reader_font_css=$(cat <<CSS
  @font-face { font-family: "Reader"; src: url("file://$FONT_DIR/Reader-Light.ttf") format("truetype"); font-weight: 300; font-style: normal; font-display: swap; }
  @font-face { font-family: "Reader"; src: url("file://$FONT_DIR/Reader-LightItalic.ttf") format("truetype"); font-weight: 300; font-style: italic; font-display: swap; }
  @font-face { font-family: "Reader"; src: url("file://$FONT_DIR/Reader-Regular.ttf") format("truetype"); font-weight: 400; font-style: normal; font-display: swap; }
  @font-face { font-family: "Reader"; src: url("file://$FONT_DIR/Reader-Italic.ttf") format("truetype"); font-weight: 400; font-style: italic; font-display: swap; }
  @font-face { font-family: "Reader"; src: url("file://$FONT_DIR/Reader-Medium.ttf") format("truetype"); font-weight: 500 600; font-style: normal; font-display: swap; }
  @font-face { font-family: "Reader"; src: url("file://$FONT_DIR/Reader-MediumItalic.ttf") format("truetype"); font-weight: 500 600; font-style: italic; font-display: swap; }
  @font-face { font-family: "Reader"; src: url("file://$FONT_DIR/Reader-Bold.ttf") format("truetype"); font-weight: 700 900; font-style: normal; font-display: swap; }
  @font-face { font-family: "Reader"; src: url("file://$FONT_DIR/Reader-BoldItalic.ttf") format("truetype"); font-weight: 700 900; font-style: italic; font-display: swap; }
CSS
)
  fi

  local garamond_font_css=""
  if [[ -f "$FONT_DIR/EBGaramond-Regular.ttf" ]]; then
    garamond_font_css=$(cat <<CSS
  @font-face { font-family: "EB Garamond"; src: url("file://$FONT_DIR/EBGaramond-Regular.ttf") format("truetype"); font-weight: 400; font-style: normal; font-display: swap; }
  @font-face { font-family: "EB Garamond"; src: url("file://$FONT_DIR/EBGaramond-Italic.ttf") format("truetype"); font-weight: 400; font-style: italic; font-display: swap; }
  @font-face { font-family: "EB Garamond"; src: url("file://$FONT_DIR/EBGaramond-Medium.ttf") format("truetype"); font-weight: 500; font-style: normal; font-display: swap; }
  @font-face { font-family: "EB Garamond"; src: url("file://$FONT_DIR/EBGaramond-SemiBold.ttf") format("truetype"); font-weight: 600; font-style: normal; font-display: swap; }
  @font-face { font-family: "EB Garamond"; src: url("file://$FONT_DIR/EBGaramond-Bold.ttf") format("truetype"); font-weight: 700; font-style: normal; font-display: swap; }
  @font-face { font-family: "EB Garamond"; src: url("file://$FONT_DIR/EBGaramond-BoldItalic.ttf") format("truetype"); font-weight: 700; font-style: italic; font-display: swap; }
CSS
)
  fi

  local mono_stack='"JetBrains Mono","SF Mono",Menlo,Consolas,monospace'
  local code_font_css=""
  if [[ -f "$FONT_DIR/GoogleSansCode-Regular.ttf" ]]; then
    mono_stack='"Google Sans Code","JetBrains Mono","SF Mono",Menlo,Consolas,monospace'
    code_font_css=$(cat <<CSS
  @font-face { font-family: "Google Sans Code"; src: url("file://$FONT_DIR/GoogleSansCode-Light.ttf") format("truetype"); font-weight: 300; font-style: normal; font-display: swap; }
  @font-face { font-family: "Google Sans Code"; src: url("file://$FONT_DIR/GoogleSansCode-Regular.ttf") format("truetype"); font-weight: 400; font-style: normal; font-display: swap; }
  @font-face { font-family: "Google Sans Code"; src: url("file://$FONT_DIR/GoogleSansCode-Italic.ttf") format("truetype"); font-weight: 400; font-style: italic; font-display: swap; }
  @font-face { font-family: "Google Sans Code"; src: url("file://$FONT_DIR/GoogleSansCode-Medium.ttf") format("truetype"); font-weight: 500; font-style: normal; font-display: swap; }
  @font-face { font-family: "Google Sans Code"; src: url("file://$FONT_DIR/GoogleSansCode-SemiBold.ttf") format("truetype"); font-weight: 600; font-style: normal; font-display: swap; }
  @font-face { font-family: "Google Sans Code"; src: url("file://$FONT_DIR/GoogleSansCode-Bold.ttf") format("truetype"); font-weight: 700; font-style: normal; font-display: swap; }
CSS
)
  fi

  cat > "$HEADER" <<HTMLHEAD
<style>
$reader_font_css
$garamond_font_css
$code_font_css
  @page { size: $page_size; margin: $page_margin; }

  :root {
    --ink:    $ink;
    --muted:  $muted;
    --rule:   $rule;
    --accent: $accent;
    --paper:  $paper;
    --code-bg: $code_bg;
    --serif:  $serif_stack;
    --sans:   "Inter","SF Pro Text","Helvetica Neue",Arial,sans-serif;
    --mono:   $mono_stack;
  }

  html, body { background: var(--paper); color: var(--ink); }
  /* Pandoc's standalone template ships a screen-oriented body with
     padding: 50px and max-width: 36em, neither of which it resets in
     @media print. When math/code/tables overflow that body box, Chrome's
     headless --print-to-pdf shrinks the whole page (~2/3) to fit. We reset
     padding/max-width explicitly so layout matches the @page geometry. */
  body {
    font-family: var(--serif);
    font-size: $body_pt;
    line-height: $line_h;
    font-feature-settings: "liga","kern","onum";
    text-rendering: optimizeLegibility;
    margin: 0 !important;
    padding: 0 !important;
    max-width: none !important;
    -webkit-print-color-adjust: exact;
    print-color-adjust: exact;
  }
  /* Prevent any single overflowing element from triggering Chrome's
     shrink-to-fit. Chrome's headless --print-to-pdf scales the entire
     document down if any descendant overflows the @page width, so we
     constrain math, code, tables, and images explicitly. */
  html { overflow-x: hidden; }
  body { overflow-x: hidden; }
  .math.display, math[display="block"], mjx-container, pre, table, img, figure {
    max-width: 100%;
  }
  .math.display {
    display: block;
    overflow: hidden;
    font-size: 0.92em;
  }
  math[display="block"] {
    display: block;
    max-width: 100%;
    overflow: hidden;
    /* Display math rendered as MathML can run wide on math-heavy pages
       (long aligned chains with stacked subscripts/superscripts). Dropping
       the font to ~83% of body keeps these from clipping on the right
       margin while staying readable on reMarkable. Inline math is left at
       100% so the body still reads naturally. */
    font-size: 0.83em;
  }
  math {
    max-width: 100%;
  }
  pre, table {
    overflow-x: hidden;
    word-break: break-word;
  }

  /* Title block produced by pandoc --standalone */
  header#title-block-header { margin: 0 0 $title_gap 0; padding-bottom: 10pt; border-bottom: 1.5pt solid var(--rule); }
  h1.title {
    font-size: $title_pt;
    line-height: 1.08;
    font-weight: 600;
    color: var(--accent);
    margin: 0;
    letter-spacing: -0.01em;
    font-feature-settings: "liga","kern","dlig";
  }
  /* Hide the author/date lines under the title block — the cover only shows
     title and (optionally) subtitle. */
  p.author, p.date { display: none; }

  h1, h2, h3, h4 {
    font-family: var(--serif);
    font-weight: 600;
    color: var(--ink);
    line-height: 1.2;
    margin-top: $h_gap_top;
    margin-bottom: 0.4em;
    page-break-after: avoid;
    break-after: avoid;
  }
  /* The pandoc title block carries the title at $title_pt. Without an
     explicit font-size, body H1s default to the UA's ~2em which dwarfs
     the title block. We bring section H1s to the same scale as H2 so the
     title block always reads as the document's title. */
  h1:not(.title) { font-size: $h2_pt; color: var(--accent); }
  h2 { font-size: $h2_pt; color: var(--accent); }
  h3 { font-size: $h3_pt; font-style: italic; font-weight: 500; color: var(--ink); }
  h4 { font-family: var(--sans); font-size: $h4_pt; text-transform: uppercase; letter-spacing: 0.12em; font-weight: 600; color: var(--muted); }

  /* Asides: smaller, indented, with a soft side rule. Emitted via Pandoc
     fenced divs: ::: aside ... :::  (also accept .concept, .difficult, .note). */
  div.aside, div.concept, div.difficult, div.note {
    margin: 1em 0 1em 1.2em;
    padding: 0.5em 0.9em;
    border-left: 2pt solid var(--rule);
    background: rgba(0, 0, 0, 0.025);
    font-size: 0.88em;
    color: var(--muted);
    page-break-inside: avoid; break-inside: avoid;
  }
  div.aside > p, div.concept > p, div.difficult > p, div.note > p { margin-bottom: 0.4em; }
  div.aside strong, div.concept strong, div.difficult strong, div.note strong { color: var(--ink); font-weight: 600; }
  div.aside em, div.concept em, div.difficult em, div.note em { color: var(--ink); }

  p { margin: 0 0 $p_gap 0; text-align: $p_align; hyphens: auto; -webkit-hyphens: auto; orphans: 3; widows: 3; }

  /* Drop cap on the first body paragraph */
  body > p:first-of-type::first-letter {
    font-family: var(--serif);
    font-size: $dropcap_em;
    line-height: 0.85;
    float: left;
    padding: 0.05em 0.1em 0 0;
    margin-top: 0.05em;
    color: var(--accent);
    font-weight: 600;
  }

  a { color: var(--accent); text-decoration: underline; text-underline-offset: 2px; text-decoration-thickness: 0.5pt; }

  blockquote {
    margin: 1em 0;
    padding: 0.2em 0 0.2em 1em;
    border-left: 3pt solid var(--accent);
    font-style: italic;
    color: var(--ink);
  }

  ul, ol { padding-left: 1.4em; margin: 0 0 1em 0; }
  li { margin-bottom: 0.25em; }

  hr {
    border: none;
    text-align: center;
    margin: 2em 0;
    overflow: visible;
  }
  hr::after {
    content: "\\2756  \\2756  \\2756";
    color: var(--rule);
    letter-spacing: 0.6em;
    font-size: 10pt;
  }

  img { max-width: 100%; display: block; margin: 1.2em auto; border-radius: 2pt; page-break-inside: avoid; break-inside: avoid; }

  code { font-family: var(--mono); font-size: 0.9em; background: var(--code-bg); padding: 0.05em 0.3em; border-radius: 2pt; }
  pre, div.sourceCode pre, div.sourceCode {
    font-family: var(--mono); font-size: 9.7pt;
    background: var(--code-bg);
    padding: 10pt; border-radius: 3pt;
    line-height: 1.4;
    /* Wrap aggressively so individual lines never exceed the column. */
    overflow-wrap: anywhere; word-break: break-word; white-space: pre-wrap;
    page-break-inside: avoid; break-inside: avoid;
    color: var(--ink);
  }
  pre code, div.sourceCode code { background: none; padding: 0; color: inherit; }
  /* Hide pandoc's line-number anchors that show up with syntax highlighting. */
  pre a[aria-hidden="true"], pre a[href^="#cb"] { display: none; }

  table { border-collapse: collapse; margin: 1em 0; width: 100%; font-size: 10.5pt; table-layout: auto; }
  th, td { padding: 6pt 8pt; border-bottom: 0.5pt solid var(--rule); text-align: left; vertical-align: top; overflow-wrap: anywhere; word-break: break-word; }
  th { font-weight: 600; border-bottom: 1pt solid var(--ink); font-family: var(--sans); font-size: 9.5pt; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); }
  /* URLs and other long unbreakable tokens inside table cells should wrap
     rather than overflow the column. */
  td a, td code { overflow-wrap: anywhere; word-break: break-all; }

  strong { font-weight: 600; }
  em     { font-style: italic; }

  figure { margin: 1.2em 0; }
  figcaption { font-family: var(--sans); font-size: 9.5pt; color: var(--muted); text-align: center; font-style: italic; margin-top: 0.4em; }

  .yaml-frontmatter { display: none; }
</style>
<script>
  // Auto-shrink display math (and other block elements) that overflow the
  // page width. Chrome's headless --print-to-pdf would normally shrink the
  // entire page when content overflows; we prevent that via overflow:hidden,
  // which then CLIPS wide content. Here we walk display-math containers and
  // wide tables/pre blocks after layout, and apply a transform: scale that
  // makes each overflowing element fit its parent's width.
  document.addEventListener('DOMContentLoaded', function () {
    // Wait briefly to let MathML/font layout settle.
    setTimeout(function () {
      // Measure the natural unconstrained width of a math/table/pre element.
      // Chrome's MathML implementation reports a CLIPPED width when the
      // parent has "overflow: hidden", so we measure the inner mtable/mrow,
      // falling back to a clone in a hidden unconstrained wrapper.
      function measureNaturalWidth(el) {
        var inner = el.querySelector('mtable, mrow, semantics') || el;
        var rect = inner.getBoundingClientRect();
        var w = rect.width;
        if (w > 0) return w;
        var measurer = document.createElement('div');
        measurer.style.cssText =
          'position:absolute;left:-99999px;top:0;visibility:hidden;' +
          'width:auto;white-space:nowrap;overflow:visible;';
        var clone = el.cloneNode(true);
        clone.style.cssText = 'width:auto;max-width:none;overflow:visible;transform:none;';
        measurer.appendChild(clone);
        document.body.appendChild(measurer);
        var width = clone.getBoundingClientRect().width;
        document.body.removeChild(measurer);
        return width;
      }

      var sels = [
        'math[display="block"]',
        'span.math.display',
        'mjx-container[display="true"]',
        'pre',
        'table',
      ];
      sels.forEach(function (sel) {
        document.querySelectorAll(sel).forEach(function (el) {
          var prev = el.style.overflow;
          el.style.overflow = 'visible';
          var parent = el.parentElement || document.body;
          var available = parent.getBoundingClientRect().width;
          var natural = measureNaturalWidth(el);
          el.style.overflow = prev;
          if (natural > available + 1 && available > 0) {
            var scale = available / natural;
            // Don't shrink below 0.45 — at that point readability suffers
            // more than wrapping or letting it overflow gracefully.
            if (scale < 0.45) scale = 0.45;
            el.style.transformOrigin = 'left top';
            el.style.transform = 'scale(' + scale.toFixed(3) + ')';
            // After scaling, the element's bounding box height is unchanged,
            // but its visible height shrinks. Compensate so siblings don't
            // overlap by setting an explicit height/width on the host.
            var h = el.getBoundingClientRect().height * scale;
            el.style.height = h.toFixed(2) + 'px';
            el.style.width = (natural * scale).toFixed(2) + 'px';
            el.style.display = 'block';
            el.style.marginRight = 'auto';
            var spareLeft = (available - natural * scale) / 2;
            if (spareLeft > 0) {
              el.style.marginLeft = spareLeft.toFixed(2) + 'px';
            }
          }
        });
      });
    }, 200);
  });
</script>
HTMLHEAD
}

write_header

md_dir="$(cd "$(dirname "$MD")" && pwd)"
# Keep the temporary HTML beside the Markdown, not in /tmp. Chrome resolves
# relative image paths (e.g. assets/foo.webp) relative to the HTML file.
html_base="$(mktemp "$md_dir/.notes-md2pdf.XXXXXX")"
html_tmp="$html_base.html"
localized_md="$html_base.localized.md"
cleanup_tmp() { rm -f "$html_base" "$html_tmp" "$localized_md"; }
trap 'cleanup_tmp; rm -f "$HEADER"' EXIT
rm -f "$html_base"

# Localize remote Markdown images before pandoc. Headless Chrome may fail to
# hotlink images (403, network hiccups, referrer checks), which produces ugly
# broken-image icons in PDFs. Render from a temporary Markdown copy with
# downloaded assets beside the source file.
render_md="$MD"
if command -v python3 >/dev/null 2>&1; then
  python3 - "$MD" "$md_dir" "$localized_md" <<'PYLOCALIMG' || true
from pathlib import Path
import hashlib
import mimetypes
import re
import sys
import urllib.parse
import urllib.request

src = Path(sys.argv[1])
md_dir = Path(sys.argv[2])
out = Path(sys.argv[3])
text = src.read_text(encoding="utf-8", errors="ignore")
assets = md_dir / "assets"
changed = False

image_url = re.compile(r"https?://[^\s)\"']+?\.(?:png|jpe?g|gif|webp|svg)(?:\?[^\s)\"']*)?", re.I)

def localize(url: str) -> str:
    global changed
    parsed = urllib.parse.urlparse(url)
    basename = Path(parsed.path).name or "image"
    stem = Path(basename).stem or "image"
    suffix = Path(basename).suffix or mimetypes.guess_extension("image/png") or ".img"
    digest = hashlib.sha1(url.encode("utf-8")).hexdigest()[:10]
    filename = f"{stem}-{digest}{suffix}"
    dest = assets / filename
    rel = Path("assets") / filename
    if not dest.exists() or dest.stat().st_size == 0:
        assets.mkdir(parents=True, exist_ok=True)
        req = urllib.request.Request(
            url,
            headers={
                "User-Agent": "Mozilla/5.0",
                "Referer": f"{parsed.scheme}://{parsed.netloc}/",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                data = r.read()
            if data:
                dest.write_bytes(data)
        except Exception:
            return url
    if dest.exists() and dest.stat().st_size > 0:
        changed = True
        return rel.as_posix()
    return url

text = image_url.sub(lambda m: localize(m.group(0)), text)

if changed:
    out.write_text(text, encoding="utf-8")
else:
    out.write_text(src.read_text(encoding="utf-8", errors="ignore"), encoding="utf-8")
PYLOCALIMG
  [[ -s "$localized_md" ]] && render_md="$localized_md"
fi

# Only inject a title when the markdown doesn't carry one in YAML frontmatter
# (pandoc: --metadata overrides in-file metadata, so we must not always pass it).
TITLE_ARGS=()
head -n 20 "$MD" | grep -qE '^title[[:space:]]*:' || TITLE_ARGS=(--metadata title="$FALLBACK_TITLE")

# MathML (native Chrome rendering) instead of MathJax: MathJax CHTML renders
# display equations at natural width, which on math-heavy documents exceeds
# the @page width and triggers Chrome's whole-page shrink-to-fit. Native
# MathML respects CSS font-size and overflow.
pandoc "$render_md" \
  --from markdown+smart+yaml_metadata_block+fenced_divs+bracketed_spans+header_attributes+tex_math_dollars \
  --to html5 \
  --standalone \
  --mathml \
  --no-highlight \
  ${TITLE_ARGS[@]+"${TITLE_ARGS[@]}"} \
  --include-in-header="$HEADER" \
  -o "$html_tmp"

"$CHROME_BIN" \
  --headless=new \
  ${CHROME_SANDBOX_FLAGS[@]+"${CHROME_SANDBOX_FLAGS[@]}"} \
  --disable-gpu \
  --no-pdf-header-footer \
  --hide-scrollbars \
  --virtual-time-budget=20000 \
  --run-all-compositor-stages-before-draw \
  --print-to-pdf="$PDF" \
  "file://$html_tmp" 2>/dev/null

[[ "${KEEP_HTML:-0}" == 1 ]] && cp "$html_tmp" "${PDF%.pdf}.html"

printf '  ✓  %s\n' "$PDF"
