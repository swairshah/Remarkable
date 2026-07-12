#!/usr/bin/env python3
"""sketchbook render pipeline: rough sketch PNG -> Gemini image gen -> e-ink ready PNG.

Usage: render.py <sketch.png> <out.png> [--model MODEL] [--hint "it's a cat"]

Sends the sketch to a Gemini image model with a prompt tuned for
monochrome pencil rendering, then post-processes the result for the
reMarkable's GC16 waveform: grayscale, white background, 16 gray levels.
"""
import argparse
import base64
import io
import json
import os
import sys
import urllib.request

from PIL import Image, ImageOps

DEFAULT_MODEL = "gemini-3.1-flash-image"

PROMPT = """This is a rough freehand sketch drawn with a stylus on an e-ink tablet.
Redraw it as a refined, confident artist's pencil sketch of the same subject,
keeping the same composition, pose, framing and personality — it must clearly be
a polished version of THIS drawing, not a different picture.

Style requirements (strict):
- Pure monochrome graphite pencil on white paper. No color at all.
- Clean confident linework, graphite shading, hatching and soft smudged tones.
- Plain white background, no paper texture, no frame, no signature, no text.
- Subject fills the frame the same way the sketch does.
"""


def call_gemini(sketch_png: bytes, model: str, hint: str | None, api_key: str) -> bytes:
    prompt = PROMPT
    if hint:
        prompt += f"\nThe artist says the sketch is: {hint}\n"
    body = {
        "contents": [{
            "parts": [
                {"text": prompt},
                {"inline_data": {
                    "mime_type": "image/png",
                    "data": base64.b64encode(sketch_png).decode(),
                }},
            ],
        }],
        "generationConfig": {"responseModalities": ["IMAGE"]},
    }
    url = (f"https://generativelanguage.googleapis.com/v1beta/models/"
           f"{model}:generateContent?key={api_key}")
    req = urllib.request.Request(
        url, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = json.load(resp)
    for cand in data.get("candidates", []):
        for part in cand.get("content", {}).get("parts", []):
            inline = part.get("inlineData") or part.get("inline_data")
            if inline and inline.get("data"):
                return base64.b64decode(inline["data"])
    raise RuntimeError(f"no image in response: {json.dumps(data)[:500]}")


def eink_post(png: bytes, size: tuple[int, int] | None) -> Image.Image:
    """Grayscale, white-normalized, quantized to 16 levels for GC16."""
    img = Image.open(io.BytesIO(png)).convert("L")
    # normalize: brightest tone becomes pure white (models often return ~250 bg)
    img = ImageOps.autocontrast(img, cutoff=(0, 1))
    if size:
        img = ImageOps.contain(img, size, Image.LANCZOS)
        canvas = Image.new("L", size, 255)
        canvas.paste(img, ((size[0] - img.width) // 2,
                           (size[1] - img.height) // 2))
        img = canvas
    # quantize to 16 gray levels (what GC16 can actually show)
    img = img.point(lambda v: min(255, (v // 17) * 17))
    return img


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("sketch")
    ap.add_argument("out")
    ap.add_argument("--model", default=os.environ.get("SKETCHBOOK_MODEL", DEFAULT_MODEL))
    ap.add_argument("--hint", default=None, help="optional subject hint for the prompt")
    ap.add_argument("--size", default=None, help="WxH to fit output into (e.g. 702x936)")
    args = ap.parse_args()

    api_key = os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_AI_STUDIO_API_KEY")
    if not api_key:
        print("GEMINI_API_KEY not set", file=sys.stderr)
        return 2

    sketch = open(args.sketch, "rb").read()
    size = None
    if args.size:
        w, h = args.size.lower().split("x")
        size = (int(w), int(h))
    else:
        size = Image.open(args.sketch).size  # match the sketch panel

    png = call_gemini(sketch, args.model, args.hint, api_key)
    img = eink_post(png, size)
    img.save(args.out)
    print(f"wrote {args.out} ({img.width}x{img.height})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
