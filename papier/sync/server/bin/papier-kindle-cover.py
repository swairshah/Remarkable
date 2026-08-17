#!/usr/bin/env python3
"""Prepend a quiet title sheet to the PDF copy emailed to Kindle."""

from __future__ import annotations

import os
import sys
from io import BytesIO
from pathlib import Path

import pymupdf as fitz
from reportlab.lib.colors import HexColor
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas


def fail(message: str) -> "NoReturn":
    print(f"papier-kindle-cover: {message}", file=sys.stderr)
    raise SystemExit(1)


def find_font(filename: str) -> Path | None:
    configured = os.environ.get("PAPIER_PDF_FONT_DIR")
    roots = [Path(configured).expanduser()] if configured else []
    roots.extend(
        [
            Path.home() / ".local/share/fonts",
            Path.home() / "Library/Fonts",
        ]
    )
    for root in roots:
        candidate = root / filename
        if candidate.is_file():
            return candidate
    return None


def register_font(name: str, filename: str, fallback: str) -> str:
    path = find_font(filename)
    if path is None:
        return fallback
    try:
        pdfmetrics.registerFont(TTFont(name, str(path)))
        return name
    except Exception:
        return fallback


def wrap_title(title: str, font: str, size: float, max_width: float) -> list[str]:
    words = title.split()
    if not words:
        return ["Untitled"]
    lines: list[str] = []
    current = words[0]
    for word in words[1:]:
        candidate = f"{current} {word}"
        if pdfmetrics.stringWidth(candidate, font, size) <= max_width:
            current = candidate
        else:
            lines.append(current)
            current = word
    lines.append(current)
    return lines


def fit_title(title: str, font: str, width: float) -> tuple[float, list[str]]:
    for size in range(34, 21, -1):
        lines = wrap_title(title, font, size, width)
        if len(lines) <= 5:
            return float(size), lines
    return 21.0, wrap_title(title, font, 21.0, width)


def make_cover(title: str, width: float, height: float) -> bytes:
    regular = register_font("PapierGaramond", "EBGaramond-Regular.ttf", "Times-Roman")
    semibold = register_font("PapierGaramondSemibold", "EBGaramond-SemiBold.ttf", "Times-Bold")

    stream = BytesIO()
    sheet = canvas.Canvas(stream, pagesize=(width, height), pageCompression=1)
    sheet.setTitle(title)
    sheet.setSubject("Kindle reading copy")
    sheet.setCreator("Papier")
    sheet.setFillColor(HexColor("#FFFFFF"))
    sheet.rect(0, 0, width, height, fill=1, stroke=0)

    side = max(42.0, width * 0.12)
    top = height - max(50.0, height * 0.105)

    sheet.setFillColor(HexColor("#5B5B56"))
    sheet.saveState()
    mark = sheet.beginText(side, top)
    mark.setFont(semibold, 9.5)
    mark.setCharSpace(2.2)
    mark.textOut("PAPIER")
    sheet.drawText(mark)
    sheet.restoreState()

    sheet.setStrokeColor(HexColor("#C9C9C2"))
    sheet.setLineWidth(0.65)
    sheet.line(side, top - 17, width - side, top - 17)

    title_width = width - (side * 2)
    title_size, lines = fit_title(title, semibold, title_width)
    leading = title_size * 1.08
    block_height = leading * len(lines)
    block_center = height * 0.59
    baseline = block_center + (block_height / 2) - title_size

    sheet.setFillColor(HexColor("#1D1D1B"))
    sheet.setFont(semibold, title_size)
    for line in lines:
        sheet.drawCentredString(width / 2, baseline, line)
        baseline -= leading

    caption_y = max(height * 0.18, baseline - 42)
    sheet.setFillColor(HexColor("#777770"))
    sheet.setFont(regular, 10.5)
    sheet.drawCentredString(width / 2, caption_y, "A reading copy from Papier")

    sheet.showPage()
    sheet.save()
    return stream.getvalue()


def prepend_cover(source_path: Path, output_path: Path, title: str) -> None:
    source = fitz.open(source_path)
    if source.page_count < 1:
        fail("source PDF has no pages")

    page_rect = source[0].rect
    cover_bytes = make_cover(title, page_rect.width, page_rect.height)
    cover = fitz.open(stream=cover_bytes, filetype="pdf")
    output = fitz.open()
    output.insert_pdf(cover)
    output.insert_pdf(source)

    toc = source.get_toc()
    for item in toc:
        if len(item) >= 3 and item[2] > 0:
            item[2] += 1
    if toc:
        output.set_toc(toc)

    metadata = dict(source.metadata or {})
    metadata.update({"title": title, "subject": "Kindle reading copy", "creator": "Papier"})
    output.set_metadata(metadata)

    output_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = output_path.with_name(f".{output_path.stem}.tmp.pdf")
    try:
        output.save(temporary, garbage=4, deflate=True)
        with fitz.open(temporary) as check:
            if check.page_count != source.page_count + 1:
                fail("title page verification failed")
        os.replace(temporary, output_path)
    finally:
        temporary.unlink(missing_ok=True)
        output.close()
        cover.close()
        source.close()


def main() -> None:
    if len(sys.argv) != 4:
        fail("usage: papier-kindle-cover.py <source.pdf> <output.pdf> <title>")
    source = Path(sys.argv[1])
    output = Path(sys.argv[2])
    if not source.is_file():
        fail(f"source PDF not found: {source}")
    prepend_cover(source, output, sys.argv[3].strip() or source.stem)


if __name__ == "__main__":
    main()
