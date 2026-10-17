#!/usr/bin/env python3
"""Generate a minimal illustrated Papier cover and prepend it to a PDF.

Commands:
    papier-compose-cover.py generate TITLE BRIEF_FILE COVER_PNG COVER_PDF
    papier-compose-cover.py prepend COVER_PDF BODY_PDF OUTPUT_PDF

The generated cover contains only the artwork and the article title. API keys
are read from the environment and are never written to the job directory.
"""

from __future__ import annotations

import base64
import json
import os
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import NoReturn

from PIL import Image, ImageDraw, ImageEnhance, ImageFont, ImageOps

WIDTH = 1570
HEIGHT = 2100
PDF_DPI = 254
DEFAULT_MODEL = "recraft/recraft-v4.1-flash"
DEFAULT_API_URL = "https://openrouter.ai/api/v1/images"

STYLE_PROMPT = """Vertical 3:4 editorial cover illustration for a monochrome e-ink reader. Pure black and white, with a sophisticated pixelated newsprint aesthetic: bold 1-bit shapes, chunky ordered dithering, coarse halftone dots, dry ink, photocopy grain, and sharp negative space. One oversized visual metaphor must dominate the composition, supported by at most three smaller elements. Keep the scene calm and immediately legible; no busy collage and no decorative clutter. Reserve the upper 30 percent as a deep near-black, visually quiet field for an externally typeset title. The lower image may become lighter and more detailed. High contrast, premium independent technology-magazine art direction, designed to survive grayscale e-ink. Generate artwork only: absolutely no words, letters, numbers, pseudo-writing, logos, frame, border, or mockup."""


def fail(message: str) -> NoReturn:
    print(f"papier-compose-cover: {message}", file=sys.stderr)
    raise SystemExit(1)


def find_font() -> Path | None:
    configured = os.environ.get("PAPIER_PDF_FONT_DIR")
    roots = [Path(configured).expanduser()] if configured else []
    roots.extend(
        [
            Path.home() / ".local/share/fonts",
            Path.home() / "Library/Fonts",
            Path("/System/Library/Fonts/Supplemental"),
            Path("/usr/share/fonts/truetype/dejavu"),
            Path("/usr/share/fonts/truetype/dejavu"),
        ]
    )
    names = ["DejaVuSans-Bold.ttf", "Arial Bold.ttf", "Reader-Bold.ttf", "Reader-Medium.ttf"]
    for name in names:
        for root in roots:
            candidate = root / name
            if candidate.is_file():
                return candidate
    return None


def text_width(draw: ImageDraw.ImageDraw, text: str, font: ImageFont.FreeTypeFont) -> int:
    box = draw.textbbox((0, 0), text, font=font)
    return box[2] - box[0]


def wrap_title(
    draw: ImageDraw.ImageDraw,
    title: str,
    font: ImageFont.FreeTypeFont,
    max_width: int,
) -> list[str]:
    words = title.upper().split()
    if not words:
        return ["UNTITLED"]
    lines: list[str] = []
    current = words[0]
    for word in words[1:]:
        candidate = f"{current} {word}"
        if text_width(draw, candidate, font) <= max_width:
            current = candidate
        else:
            lines.append(current)
            current = word
    lines.append(current)
    return lines


def fit_title(
    draw: ImageDraw.ImageDraw, title: str, font_path: Path | None, max_width: int
) -> tuple[ImageFont.FreeTypeFont | ImageFont.ImageFont, list[str]]:
    if font_path is None:
        font = ImageFont.load_default(size=80)
        return font, wrap_title(draw, title, font, max_width)
    for size in range(154, 75, -4):
        font = ImageFont.truetype(str(font_path), size=size)
        lines = wrap_title(draw, title, font, max_width)
        if len(lines) <= 4 and all(text_width(draw, line, font) <= max_width for line in lines):
            return font, lines
    font = ImageFont.truetype(str(font_path), size=72)
    return font, wrap_title(draw, title, font, max_width)


def read_brief(path: Path, title: str) -> str:
    try:
        brief = " ".join(path.read_text(encoding="utf-8").split()).strip()
    except OSError:
        brief = ""
    if not brief:
        brief = (
            f"Create one concrete visual metaphor for the article titled {title!r}. "
            "Use a single dominant object and a small human-scale counterpoint."
        )
    return brief[:1800]


def request_art(prompt: str) -> tuple[bytes, dict[str, object]]:
    fixture = os.environ.get("PAPIER_COVER_SOURCE_IMAGE")
    if fixture:
        return Path(fixture).expanduser().read_bytes(), {"fixture": str(fixture)}

    key = os.environ.get("OPENROUTER_API_KEY", "").strip()
    if not key:
        fail("OPENROUTER_API_KEY is not set")
    model = os.environ.get("PAPIER_COVER_MODEL", DEFAULT_MODEL)
    api_url = os.environ.get("PAPIER_COVER_API_URL", DEFAULT_API_URL)
    payload = json.dumps(
        {"model": model, "prompt": prompt, "n": 1, "aspect_ratio": "3:4"}
    ).encode("utf-8")
    headers = {
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
        "HTTP-Referer": "https://remarkable.exe.xyz/papier/",
        "X-Title": "Papier composed article cover",
    }
    last_error = "unknown error"
    for attempt in range(3):
        try:
            request = urllib.request.Request(
                api_url, data=payload, headers=headers, method="POST"
            )
            with urllib.request.urlopen(request, timeout=180) as response:
                result = json.load(response)
            data = (result.get("data") or [{}])[0]
            encoded = data.get("b64_json")
            if not encoded:
                raise RuntimeError(f"image response has no b64_json: {list(data)}")
            return base64.b64decode(encoded), {
                "model": model,
                "usage": result.get("usage"),
                "media_type": data.get("media_type"),
            }
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", "replace")[:500]
            last_error = f"HTTP {error.code}: {detail}"
        except Exception as error:  # network and malformed-response retries
            last_error = str(error)
        if attempt < 2:
            time.sleep(3 * (attempt + 1))
    fail(f"image generation failed after 3 attempts: {last_error}")


def crop_cover(image: Image.Image) -> Image.Image:
    image = ImageOps.exif_transpose(image).convert("L")
    source_ratio = image.width / image.height
    target_ratio = WIDTH / HEIGHT
    if source_ratio > target_ratio:
        crop_width = int(image.height * target_ratio)
        left = (image.width - crop_width) // 2
        image = image.crop((left, 0, left + crop_width, image.height))
    else:
        crop_height = int(image.width / target_ratio)
        top = (image.height - crop_height) // 2
        image = image.crop((0, top, image.width, top + crop_height))
    image = image.resize((WIDTH, HEIGHT), Image.Resampling.LANCZOS)
    image = ImageOps.autocontrast(image, cutoff=1)
    return ImageEnhance.Contrast(image).enhance(1.16)


def add_title(art: Image.Image, title: str) -> Image.Image:
    page = art.convert("RGBA")
    # The prompt requests a dark title field. Reinforce it softly so titles stay
    # readable even when a provider drifts without adding any cover furniture.
    overlay = Image.new("RGBA", page.size, (0, 0, 0, 0))
    pixels = overlay.load()
    fade_end = int(HEIGHT * 0.36)
    solid_end = int(HEIGHT * 0.20)
    for y in range(fade_end):
        if y <= solid_end:
            alpha = 190
        else:
            alpha = int(190 * (1 - (y - solid_end) / (fade_end - solid_end)))
        for x in range(WIDTH):
            pixels[x, y] = (0, 0, 0, max(0, alpha))
    page = Image.alpha_composite(page, overlay)

    draw = ImageDraw.Draw(page)
    side = 105
    top = 115
    max_width = WIDTH - side * 2
    font, lines = fit_title(draw, title, find_font(), max_width)
    if isinstance(font, ImageFont.FreeTypeFont):
        ascent, descent = font.getmetrics()
        line_height = int((ascent + descent) * 0.90)
    else:
        line_height = 86
    y = top
    for line in lines:
        draw.text((side + 3, y + 4), line, font=font, fill=(0, 0, 0, 180))
        draw.text((side, y), line, font=font, fill=(255, 255, 255, 255))
        y += line_height
    return page.convert("L")


def atomic_save_image(image: Image.Image, path: Path, **kwargs: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    suffix = path.suffix or ".tmp"
    with tempfile.NamedTemporaryFile(dir=path.parent, suffix=suffix, delete=False) as tmp:
        temporary = Path(tmp.name)
    try:
        image.save(temporary, **kwargs)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def generate(title: str, brief_path: Path, png_path: Path, pdf_path: Path) -> None:
    brief = read_brief(brief_path, title)
    prompt = f"{STYLE_PROMPT}\n\nArticle-specific visual metaphor: {brief}"
    art_bytes, response_meta = request_art(prompt)
    try:
        from io import BytesIO

        with Image.open(BytesIO(art_bytes)) as source:
            cover = add_title(crop_cover(source), title)
    except Exception as error:
        fail(f"could not decode generated artwork: {error}")

    atomic_save_image(cover, png_path, format="PNG", optimize=True)
    atomic_save_image(
        cover.convert("RGB"), pdf_path, format="PDF", resolution=PDF_DPI
    )
    meta_path = png_path.with_name("cover-meta.json")
    meta_path.write_text(
        json.dumps(
            {
                "title": title,
                "brief": brief,
                "prompt": prompt,
                **response_meta,
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"papier-compose-cover: generated {png_path.name}", file=sys.stderr)


def prepend(cover_path: Path, body_path: Path, output_path: Path) -> None:
    try:
        import pymupdf as fitz
    except ImportError:
        try:
            import fitz  # type: ignore[no-redef]
        except ImportError:
            fail("PyMuPDF is required to prepend a cover")

    cover = fitz.open(cover_path)
    body = fitz.open(body_path)
    if cover.page_count != 1:
        fail("cover PDF must contain exactly one page")
    if body.page_count < 1:
        fail("article PDF has no pages")
    output = fitz.open()
    output.insert_pdf(cover)
    output.insert_pdf(body)
    toc = body.get_toc()
    for item in toc:
        if len(item) >= 3 and item[2] > 0:
            item[2] += 1
    if toc:
        output.set_toc(toc)
    metadata = dict(body.metadata or {})
    metadata.update({"creator": "Papier", "title": metadata.get("title") or "Papier article"})
    output.set_metadata(metadata)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = output_path.with_name(f".{output_path.stem}.tmp.pdf")
    try:
        output.save(temporary, garbage=4, deflate=True)
        with fitz.open(temporary) as check:
            if check.page_count != body.page_count + 1:
                fail("cover prepend verification failed")
        os.replace(temporary, output_path)
    finally:
        temporary.unlink(missing_ok=True)
        output.close()
        body.close()
        cover.close()
    print(f"papier-compose-cover: prepended cover to {output_path.name}", file=sys.stderr)


def main() -> None:
    if len(sys.argv) < 2:
        fail("expected generate or prepend command")
    command = sys.argv[1]
    if command == "generate" and len(sys.argv) == 6:
        generate(sys.argv[2].strip() or "Untitled", Path(sys.argv[3]), Path(sys.argv[4]), Path(sys.argv[5]))
        return
    if command == "prepend" and len(sys.argv) == 5:
        prepend(Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]))
        return
    fail(
        "usage: papier-compose-cover.py generate TITLE BRIEF COVER.png COVER.pdf "
        "or papier-compose-cover.py prepend COVER.pdf BODY.pdf OUTPUT.pdf"
    )


if __name__ == "__main__":
    main()
