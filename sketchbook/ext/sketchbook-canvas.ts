/**
 * Canvas tools for pi inside the sketchbook app (reMarkable 2).
 *
 * The app (a Rust takeover binary owning the e-ink panel) spawns pi with
 * `-e <this file>` and SKETCHBOOK_SOCK pointing at its unix tool socket.
 * These tools speak one JSON object per line over that socket:
 *
 *   sketchbook_generate {region, prompt?, edit_raster?, dest, ...} -> gen + place
 *   sketchbook_draw   {svg, page?}              -> SVG -> pen strokes -> patch id
 *   sketchbook_erase  {id, page?}               -> remove a patch
 *   sketchbook_view   {page?}                   -> fresh half-scale PNG of a spread
 *   sketchbook_goto   {page}                    -> flip the tablet to a page
 *
 * sketchbook_generate is the star: pi is the ART DIRECTOR — it picks a
 * page region to crop (which may contain handwritten instructions the
 * model reads natively), optionally an existing raster to edit in place,
 * composes the text prompt, and picks the destination rect. The extension
 * ferries: crop -> Gemini image model -> decode (PNG via node:zlib, JPEG
 * via vendored jpeg-js) -> autocontrast -> raw grayscale -> place.
 *
 * When SKETCHBOOK_SOCK is absent (a normal pi session), nothing registers.
 */

import * as net from "node:net";
import * as zlib from "node:zlib";
import { createRequire } from "node:module";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

/* jpeg-js's decoder (vendored, BSD) — Gemini image models answer JPEG */
const jpegDecode = createRequire(import.meta.url)("./jpeg-decode.cjs") as (
  data: Buffer,
  opts?: { useTArray?: boolean; formatAsRGBA?: boolean; maxMemoryUsageInMB?: number },
) => { width: number; height: number; data: Uint8Array };

/* NOTE: `||` not `??` — takeover.sh passes these as empty strings when unset */
const SOCK = process.env.SKETCHBOOK_SOCK || "";
const IMG_MODEL = process.env.SKETCHBOOK_IMG_MODEL || "gemini-3.1-flash-image";
const API_KEY = process.env.GEMINI_API_KEY || process.env.GOOGLE_AI_STUDIO_API_KEY || "";

const MONO_SUFFIX =
  "Unless the instruction says otherwise, render as a refined artist's pencil " +
  "drawing built from VISIBLE INDIVIDUAL GRAPHITE STROKES: grainy pencil " +
  "texture with the tooth of the paper showing through, energetic hatching " +
  "and cross-hatching with slightly broken stroke edges, darker pressed " +
  "accents — never smooth airbrushed gradients. Always pure monochrome on " +
  "plain white paper (this is an e-ink screen): no color, no frame, no text, " +
  "no signature.";

function call(cmd: Record<string, unknown>, timeoutMs = 30000): Promise<any> {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(SOCK);
    let buf = "";
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error("the sketchbook app did not answer on " + SOCK));
    }, timeoutMs);
    sock.on("connect", () => sock.write(JSON.stringify(cmd) + "\n"));
    sock.on("data", (d) => {
      buf += d.toString("utf8");
      const nl = buf.indexOf("\n");
      if (nl >= 0) {
        clearTimeout(timer);
        sock.end();
        try {
          resolve(JSON.parse(buf.slice(0, nl)));
        } catch (e) {
          reject(e);
        }
      }
    });
    sock.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
  });
}

function textResult(text: string, isError = false) {
  return { content: [{ type: "text" as const, text }], isError };
}

/* ---- minimal PNG -> 8-bit grayscale decoder (no deps) -------------------- */

function paeth(a: number, b: number, c: number): number {
  const p = a + b - c;
  const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}

/** Decode an 8-bit PNG (gray / gray+alpha / RGB / RGBA, non-interlaced)
 *  to { w, h, gray } — alpha composited over white, RGB -> luma. */
function pngToGray(png: Buffer): { w: number; h: number; gray: Buffer } {
  if (png.readUInt32BE(0) !== 0x89504e47) throw new Error("not a PNG");
  let pos = 8;
  let w = 0, h = 0, depth = 0, color = 0, interlace = 0;
  const idat: Buffer[] = [];
  while (pos < png.length) {
    const len = png.readUInt32BE(pos);
    const type = png.toString("ascii", pos + 4, pos + 8);
    const data = png.subarray(pos + 8, pos + 8 + len);
    if (type === "IHDR") {
      w = data.readUInt32BE(0);
      h = data.readUInt32BE(4);
      depth = data[8];
      color = data[9];
      interlace = data[12];
    } else if (type === "IDAT") {
      idat.push(data);
    } else if (type === "IEND") {
      break;
    }
    pos += 12 + len;
  }
  if (depth !== 8) throw new Error(`unsupported bit depth ${depth}`);
  if (interlace !== 0) throw new Error("interlaced PNG unsupported");
  const ch = color === 0 ? 1 : color === 4 ? 2 : color === 2 ? 3 : color === 6 ? 4 : 0;
  if (!ch) throw new Error(`unsupported color type ${color}`);

  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = w * ch;
  const out = Buffer.alloc(w * h);
  const prev = Buffer.alloc(stride);
  const cur = Buffer.alloc(stride);
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)];
    raw.copy(cur, 0, y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let i = 0; i < stride; i++) {
      const a = i >= ch ? cur[i - ch] : 0;
      const b = prev[i];
      const c = i >= ch ? prev[i - ch] : 0;
      if (f === 1) cur[i] = (cur[i] + a) & 0xff;
      else if (f === 2) cur[i] = (cur[i] + b) & 0xff;
      else if (f === 3) cur[i] = (cur[i] + ((a + b) >> 1)) & 0xff;
      else if (f === 4) cur[i] = (cur[i] + paeth(a, b, c)) & 0xff;
    }
    for (let x = 0; x < w; x++) {
      const o = x * ch;
      let g: number, alpha = 255;
      if (ch <= 2) {
        g = cur[o];
        if (ch === 2) alpha = cur[o + 1];
      } else {
        g = Math.round(0.299 * cur[o] + 0.587 * cur[o + 1] + 0.114 * cur[o + 2]);
        if (ch === 4) alpha = cur[o + 3];
      }
      if (alpha < 255) g = Math.round((g * alpha + 255 * (255 - alpha)) / 255);
      out[y * w + x] = g;
    }
    cur.copy(prev);
  }
  return { w, h, gray: out };
}

/** Decode whatever image Gemini returned (PNG or JPEG) to 8-bit grayscale. */
function imageToGray(buf: Buffer): { w: number; h: number; gray: Buffer } {
  if (buf.length > 8 && buf.readUInt32BE(0) === 0x89504e47) return pngToGray(buf);
  if (buf.length > 2 && buf[0] === 0xff && buf[1] === 0xd8) {
    const img = jpegDecode(buf, { useTArray: true, formatAsRGBA: true, maxMemoryUsageInMB: 256 });
    const gray = Buffer.alloc(img.width * img.height);
    for (let i = 0; i < gray.length; i++) {
      const o = i * 4;
      gray[i] = Math.round(
        0.299 * img.data[o] + 0.587 * img.data[o + 1] + 0.114 * img.data[o + 2],
      );
    }
    return { w: img.width, h: img.height, gray };
  }
  throw new Error("model returned an unrecognized image format");
}

/** Downscale gray to fit maxSide (2x2 box averaging per halving step,
 *  bilinear-free but plenty for a final on-page size of ~600px). The
 *  model returns 1-2K images; shipping those raw over the socket costs
 *  megabytes of base64 that the app immediately shrinks anyway. */
function downscaleGray(gray: Buffer, w: number, h: number, maxSide: number): { w: number; h: number; gray: Buffer } {
  while (Math.max(w, h) > maxSide) {
    const nw = Math.max(1, w >> 1);
    const nh = Math.max(1, h >> 1);
    const out = Buffer.alloc(nw * nh);
    for (let y = 0; y < nh; y++) {
      const y0 = y * 2, y1 = Math.min(y * 2 + 1, h - 1);
      for (let x = 0; x < nw; x++) {
        const x0 = x * 2, x1 = Math.min(x * 2 + 1, w - 1);
        out[y * nw + x] =
          (gray[y0 * w + x0] + gray[y0 * w + x1] + gray[y1 * w + x0] + gray[y1 * w + x1] + 2) >> 2;
      }
    }
    gray = out;
    w = nw;
    h = nh;
  }
  return { w, h, gray };
}

/** Normalize tones for e-ink: stretch so the paper reads as true white
 *  (models return ~245-250 backgrounds, which would render as visible
 *  gray wash on the panel) and the darkest marks as true black. */
function autocontrast(gray: Buffer): Buffer {
  const hist = new Array(256).fill(0);
  for (const g of gray) hist[g]++;
  const total = gray.length;
  let lo = 0, hi = 255, acc = 0;
  for (let v = 255; v >= 0; v--) {
    acc += hist[v];
    if (acc > total * 0.01) { hi = v; break; } /* 1% highlight cutoff */
  }
  acc = 0;
  for (let v = 0; v < 256; v++) {
    acc += hist[v];
    if (acc > total * 0.002) { lo = v; break; } /* 0.2% shadow cutoff */
  }
  if (hi <= lo) return gray;
  const out = Buffer.alloc(gray.length);
  for (let i = 0; i < gray.length; i++) {
    let v = Math.max(0, Math.min(255, Math.round(((gray[i] - lo) * 255) / (hi - lo))));
    if (v >= 238) v = 255; /* clip near-whites: kills watermark texture on paper */
    out[i] = v;
  }
  return out;
}

/* ---- Gemini image generation --------------------------------------------- */

async function callImageModel(parts: unknown[]): Promise<Buffer> {
  const body = {
    contents: [{ parts }],
    generationConfig: { responseModalities: ["IMAGE"] },
  };
  const url =
    `https://generativelanguage.googleapis.com/v1beta/models/${IMG_MODEL}` +
    `:generateContent?key=${API_KEY}`;
  const resp = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(120000),
  });
  if (!resp.ok) throw new Error(`gemini ${resp.status}: ${(await resp.text()).slice(0, 300)}`);
  const data: any = await resp.json();
  for (const cand of data.candidates ?? []) {
    for (const part of cand.content?.parts ?? []) {
      const inline = part.inlineData ?? part.inline_data;
      if (inline?.data) return Buffer.from(inline.data, "base64");
    }
  }
  throw new Error("gemini returned no image: " + JSON.stringify(data).slice(0, 300));
}

/** One generation call: pi's prompt + the page crop (+ optionally an
 *  existing raster as the base image to edit in place). */
function generateImage(
  prompt: string,
  cropPngB64: string | null,
  baseRasterB64: string | null,
): Promise<Buffer> {
  const parts: unknown[] = [{ text: prompt }];
  if (baseRasterB64) {
    parts.push({ inline_data: { mime_type: "image/png", data: baseRasterB64 } });
  }
  if (cropPngB64) {
    parts.push({ inline_data: { mime_type: "image/png", data: cropPngB64 } });
  }
  return callImageModel(parts);
}

/* ---- tools ----------------------------------------------------------------- */

export default function (pi: ExtensionAPI) {
  if (!SOCK) return; // not running inside the sketchbook app

  pi.registerTool({
    name: "sketchbook_generate",
    label: "sketchbook: generate onto the page",
    description:
      "Ship part of the page to the image model and place the result back on the page. " +
      "You are the art director: `region` [x0,y0,x1,y1] is the page crop the model SEES " +
      "(frame it deliberately — the sketch alone, or sketch plus handwritten notes " +
      "inside the crop: the model reads text in images and follows it); `prompt` is " +
      "what you tell the model (describe literally what the sketch depicts; or point at " +
      "the handwriting: 'follow the handwritten instructions in the image'); " +
      "`edit_raster` (an output id) makes the model UPDATE that existing image in " +
      "place instead of drawing fresh — use for any tweak to something you already " +
      "made; `dest` [x0,y0,x1,y1] is where the output lands (aspect-fit, centered — " +
      "use free space, never the user's ink; when editing, use the old rect and pass " +
      "`replace` with the same id). Default look is grainy graphite pencil unless your " +
      "prompt says otherwise. Returns the new raster id. Takes ~10-30s.",
    parameters: {
      type: "object",
      properties: {
        region: {
          type: "array",
          items: { type: "number" },
          description: "Page crop [x0,y0,x1,y1] the model sees (omit only when editing a raster with no new context)",
        },
        prompt: {
          type: "string",
          description: "Your instruction to the image model",
        },
        edit_raster: {
          type: "number",
          description: "Existing output id to edit in place (model gets it as the base image)",
        },
        dest: {
          type: "array",
          items: { type: "number" },
          description: "Destination rect [x0,y0,x1,y1] on the page",
        },
        replace: {
          type: "number",
          description: "Output id to remove when the new one lands (usually = edit_raster)",
        },
        include_rasters: {
          type: "boolean",
          description: "Include your existing outputs in the region crop (default true)",
        },
        page: {
          type: "number",
          description: "1-based page number; omit for the page on screen",
        },
      },
      required: ["prompt", "dest"],
    },
    async execute(_id: string, params: any) {
      if (!API_KEY) return textResult("generate failed: GEMINI_API_KEY is not set on the device", true);
      if (!params.region && params.edit_raster == null) {
        return textResult("generate failed: pass `region` (a page crop) and/or `edit_raster`", true);
      }
      try {
        /* the base image, when editing one of our outputs in place */
        let baseB64: string | null = null;
        if (params.edit_raster != null) {
          const prev = await call({ cmd: "raster_get", id: params.edit_raster, page: params.page });
          if (!prev.ok) return textResult(`generate failed: ${prev.error}`, true);
          baseB64 = prev.png_base64;
        }
        /* the page crop the model sees */
        let cropB64: string | null = null;
        if (params.region) {
          const c = await call({
            cmd: "crop",
            rect: params.region,
            rasters: params.include_rasters !== false,
            page: params.page,
          });
          if (!c.ok) return textResult(`generate failed: ${c.error}`, true);
          cropB64 = c.png_base64;
        }

        let prompt = params.prompt;
        if (baseB64) {
          prompt =
            "The first image is a drawing you made earlier; UPDATE THAT IMAGE, " +
            "changing nothing except what the instruction asks — same subject, " +
            "same composition, same strokes elsewhere." +
            (cropB64
              ? " The second image is the relevant part of the artist's page for " +
                "reference; it may contain annotation marks or handwritten notes — " +
                "follow them, but never copy handwriting or annotation marks into " +
                "the drawing."
              : "") +
            `\nInstruction: ${prompt}\n` +
            MONO_SUFFIX;
        } else {
          prompt =
            "The attached image is from a page of an artist's sketchbook on an " +
            "e-ink tablet (rough stylus ink; it may include handwritten notes — " +
            "read and follow them, but never copy handwriting or annotation " +
            `marks into your drawing).\n${prompt}\n` +
            MONO_SUFFIX;
        }

        const img = await generateImage(prompt, cropB64, baseB64);
        const decoded = imageToGray(img);
        /* the app aspect-fits into dest (≤ page size) — 1400px is already
         * more than any destination rect can use */
        const { w, h, gray } = downscaleGray(decoded.gray, decoded.w, decoded.h, 1400);
        const normalized = autocontrast(gray);
        const r = await call(
          {
            cmd: "place",
            page: params.page,
            w,
            h,
            raw_base64: normalized.toString("base64"),
            rect: params.dest,
            replace: params.replace,
          },
          60000,
        );
        if (!r.ok) return textResult(`generate failed: ${r.error}`, true);
        const [px, py, pw, ph] = r.placed ?? [];
        return textResult(
          `Placed output #${r.id} (${pw}x${ph} at ${px},${py}) on page ${r.page}. ` +
            `Edit it later with edit_raster:${r.id} (+ replace:${r.id}); ` +
            `sketchbook_view to check the page. The user's rubber can wipe it.`,
        );
      } catch (e: any) {
        return textResult(`generate failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "sketchbook_draw",
    label: "sketchbook: draw on the page",
    description:
      "Draw a small annotation as freeform pen ink (black on the page; shown gray in the " +
      "images you receive so you can tell your ink from the user's). Takes an SVG whose " +
      "coordinate space IS the page: 1404 wide x 1872 tall, y down (omit viewBox or use " +
      'viewBox="0 0 1404 1872"); the panel divider is at x=702 — keep annotations in the ' +
      "left panel near the user's ink, never over their sketch. Supported: rect, line, " +
      "circle, ellipse, polyline, polygon, path (M L H V C S Q T Z), and <text> (one line " +
      "each; x,y = baseline; font-family \"script\" | \"serif\" | \"sans\"). Use " +
      'fill="none" except for tiny solid bits. Returns a patch id — keep it if you may ' +
      "want to erase this later.",
    parameters: {
      type: "object",
      properties: {
        svg: { type: "string", description: "The SVG to draw, in page coordinates" },
        page: {
          type: "number",
          description: "1-based page number; omit for the page currently on screen",
        },
      },
      required: ["svg"],
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "draw", svg: params.svg, page: params.page });
        if (!r.ok) return textResult(`draw failed: ${r.error}`, true);
        const bbox = r.bbox ? ` bbox (${r.bbox[0]},${r.bbox[1]})-(${r.bbox[2]},${r.bbox[3]})` : "";
        const notes = r.notes?.length ? ` NOTE: ${r.notes.join("; ")}.` : "";
        return textResult(
          `Drawn on page ${r.page} as patch #${r.id}${bbox}. ` +
            `Use sketchbook_erase with id ${r.id} to remove it.${notes}`,
        );
      } catch (e: any) {
        return textResult(`draw failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "sketchbook_erase_ink",
    label: "sketchbook: erase the user's handwriting",
    description:
      "Remove the USER'S handwritten strokes that lie fully inside a rect — for cleaning " +
      "up an instruction they wrote to you AFTER you have acted on it (e.g. 'darker' " +
      "scribbled on your output, now applied). Only strokes entirely inside the rect are " +
      "removed, so frame the handwriting tightly. NEVER use this on their drawing, and " +
      "only erase notes that were clearly addressed to you and are now done. When in " +
      "doubt, leave their ink alone — they can rub it out themselves.",
    parameters: {
      type: "object",
      properties: {
        rect: {
          type: "array",
          items: { type: "number" },
          description: "Tight page rect [x0,y0,x1,y1] around the handwriting to remove",
        },
        page: {
          type: "number",
          description: "1-based page number; omit for the page currently on screen",
        },
      },
      required: ["rect"],
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "erase_ink", rect: params.rect, page: params.page });
        return r.ok
          ? textResult(`Removed ${r.removed} handwritten strokes on page ${r.page}.`)
          : textResult(`erase_ink failed: ${r.error}`, true);
      } catch (e: any) {
        return textResult(`erase_ink failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "sketchbook_erase",
    label: "sketchbook: erase one of your patches",
    description:
      "Erase a patch you previously drew with sketchbook_draw, by its id. The user's own " +
      "ink underneath is untouched (the page re-renders from vectors).",
    parameters: {
      type: "object",
      properties: {
        id: { type: "number", description: "The patch id sketchbook_draw returned" },
        page: {
          type: "number",
          description: "1-based page number; omit for the page currently on screen",
        },
      },
      required: ["id"],
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "erase", id: params.id, page: params.page });
        return r.ok
          ? textResult(`Patch ${params.id} erased.`)
          : textResult(`erase failed: ${r.error}`, true);
      } catch (e: any) {
        return textResult(`erase failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "sketchbook_goto",
    label: "sketchbook: turn to a page",
    description:
      "Turn the sketchbook on the user's screen to a given page (1-based). Use it " +
      "sparingly: when the user asks, or to show them a render on another page. Refused " +
      "while the user is actively drawing or inside a menu.",
    parameters: {
      type: "object",
      properties: {
        page: { type: "number", description: "1-based page number to show" },
      },
      required: ["page"],
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "goto", page: params.page });
        return r.ok
          ? textResult(`Now showing page ${r.page} of ${r.page_count}. ${r.layout ?? ""}`)
          : textResult(`goto failed: ${r.error}`, true);
      } catch (e: any) {
        return textResult(`goto failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "sketchbook_view",
    label: "sketchbook: look at a spread",
    description:
      "Returns a fresh image of a sketchbook spread (half scale: multiply image " +
      "coordinates by 2 to get page coordinates; the divider sits at image x=351 — left " +
      "of it the user's sketch, right of it your render) plus the list of your ink " +
      "patches. Use it to check the result after a render, or to read another spread.",
    parameters: {
      type: "object",
      properties: {
        page: {
          type: "number",
          description: "1-based page number; omit for the spread currently on screen",
        },
      },
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "view", page: params.page }, 30000);
        if (!r.ok) return textResult(`view failed: ${r.error}`, true);
        const patches =
          (r.patches ?? [])
            .map((p: any) => `#${p.id}${p.bbox ? ` at (${p.bbox[0]},${p.bbox[1]})-(${p.bbox[2]},${p.bbox[3]})` : ""}`)
            .join(", ") || "none";
        return {
          content: [
            {
              type: "text" as const,
              text:
                `Spread ${r.page} of ${r.page_count} (${r.page_width}x${r.page_height} page, ` +
                `image at 1/${r.image_scale} scale). Your ink patches: ${patches}.`,
            },
            { type: "image" as const, data: r.png_base64, mimeType: "image/png" },
          ],
        };
      } catch (e: any) {
        return textResult(`view failed: ${e.message}`, true);
      }
    },
  });
}
