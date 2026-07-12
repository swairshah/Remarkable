/**
 * Canvas tools for pi inside the sketchbook app (reMarkable 2).
 *
 * The app (a Rust takeover binary owning the e-ink panel) spawns pi with
 * `-e <this file>` and SKETCHBOOK_SOCK pointing at its unix tool socket.
 * These tools speak one JSON object per line over that socket:
 *
 *   sketchbook_render {subject, style?, page?}  -> gen + place on the right panel
 *   sketchbook_draw   {svg, page?}              -> SVG -> pen strokes -> patch id
 *   sketchbook_erase  {id, page?}               -> remove a patch
 *   sketchbook_view   {page?}                   -> fresh half-scale PNG of a spread
 *   sketchbook_goto   {page}                    -> flip the tablet to a page
 *
 * sketchbook_render is the star: it asks the app for the sketch (the left
 * panel's ink, cropped to its bounding box), sends it to a Gemini image
 * model with a prompt tuned for monochrome pencil rendition, decodes the
 * returned PNG right here (node:zlib inflate + scanline unfilter — no npm
 * deps), and hands the app raw grayscale to place on the right panel.
 *
 * When SKETCHBOOK_SOCK is absent (a normal pi session), nothing registers.
 */

import * as net from "node:net";
import * as zlib from "node:zlib";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const SOCK = process.env.SKETCHBOOK_SOCK ?? "";
const IMG_MODEL = process.env.SKETCHBOOK_IMG_MODEL ?? "gemini-3.1-flash-image";
const API_KEY = process.env.GEMINI_API_KEY ?? process.env.GOOGLE_AI_STUDIO_API_KEY ?? "";

const DEFAULT_STYLE =
  "a refined, confident artist's pencil sketch: pure monochrome graphite on white " +
  "paper, clean confident linework, graphite shading, hatching and soft smudged " +
  "tones, plain white background, no color, no paper texture, no frame, no text";

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

/* ---- Gemini image generation --------------------------------------------- */

async function generate(sketchPngB64: string, subject: string, style: string): Promise<Buffer> {
  const prompt =
    "This is a rough freehand sketch drawn with a stylus on an e-ink tablet. " +
    `It depicts: ${subject}.\n` +
    "Redraw it, keeping the same composition, pose, framing and personality — it " +
    "must clearly read as a polished version of THIS drawing, not a different " +
    `picture. Render it as ${style}. ` +
    "The subject fills the frame the same way the sketch does.";
  const body = {
    contents: [{
      parts: [
        { text: prompt },
        { inline_data: { mime_type: "image/png", data: sketchPngB64 } },
      ],
    }],
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

/* ---- tools ----------------------------------------------------------------- */

export default function (pi: ExtensionAPI) {
  if (!SOCK) return; // not running inside the sketchbook app

  pi.registerTool({
    name: "sketchbook_render",
    label: "sketchbook: render the sketch",
    description:
      "Turn the user's rough sketch (the LEFT panel of the current spread) into a polished " +
      "rendition and place it on the RIGHT panel. Captures the sketch automatically; you " +
      "supply `subject` — a one-line literal description of what the sketch depicts (be " +
      "specific about pose, orientation, expression: the image model uses it to " +
      "disambiguate rough strokes). Optional `style` replaces the default graphite-pencil " +
      "look — use it only when the user asked for a style in writing. Replaces any " +
      "previous render on the page. Takes ~10-30s.",
    parameters: {
      type: "object",
      properties: {
        subject: {
          type: "string",
          description: "What the sketch depicts, one careful line",
        },
        style: {
          type: "string",
          description: "Optional style override (only when the user asked)",
        },
        page: {
          type: "number",
          description: "1-based page number; omit for the spread on screen",
        },
      },
      required: ["subject"],
    },
    async execute(_id: string, params: any) {
      if (!API_KEY) return textResult("render failed: GEMINI_API_KEY is not set on the device", true);
      try {
        const s = await call({ cmd: "sketch", page: params.page });
        if (!s.ok) return textResult(`render failed: ${s.error}`, true);
        const png = await generate(
          s.png_base64,
          params.subject,
          params.style?.trim() || DEFAULT_STYLE,
        );
        const { w, h, gray } = pngToGray(png);
        const r = await call(
          {
            cmd: "render",
            page: params.page,
            w,
            h,
            raw_base64: gray.toString("base64"),
          },
          60000,
        );
        if (!r.ok) return textResult(`render failed: ${r.error}`, true);
        const [px, py, pw, ph] = r.placed ?? [];
        return textResult(
          `Rendered "${params.subject}" onto page ${r.page}'s right panel ` +
            `(${pw}x${ph} at ${px},${py}). The user's rubber can wipe it; a new ` +
            `sketchbook_render replaces it.`,
        );
      } catch (e: any) {
        return textResult(`render failed: ${e.message}`, true);
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
