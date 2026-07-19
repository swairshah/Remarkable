/**
 * Reading-companion tools for pi inside the reader app (reMarkable 2).
 *
 * The app (a Rust takeover binary owning the e-ink panel) spawns pi with
 * `-e <this file>` and PAPIER_SOCK pointing at its unix tool socket.
 * These tools speak one JSON object per line over that socket:
 *
 *   canvas_draw        {cmd:"draw", svg, page?}            -> {ok, id, bbox}
 *   canvas_underline   {cmd:"underline", phrase, page?}    -> {ok, id, matches}
 *   canvas_erase       {cmd:"erase", id, page?}            -> {ok}
 *   canvas_view        {cmd:"view", page?}                 -> {ok, png_base64,..}
 *   canvas_goto        {cmd:"goto", page}                  -> {ok, layout}
 *   canvas_insert_note {cmd:"insert_note", after_page?}    -> {ok, page}
 *   canvas_page_text   {cmd:"page_text", from, to?}        -> {ok, text}
 *
 * "page N" always means the N-th entry of the reading sequence (printed
 * pages + inserted note pages), 1-based — the same numbers the pause
 * messages use.
 *
 * When PAPIER_SOCK is absent (a normal pi session), nothing registers —
 * the tools only exist inside the app.
 */

import * as net from "node:net";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const SOCK = process.env.PAPIER_SOCK ?? "";

function call(cmd: Record<string, unknown>, timeoutMs = 20000): Promise<any> {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(SOCK);
    let buf = "";
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error("the reader app did not answer on " + SOCK));
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

const PAGE_PARAM = {
  type: "number",
  description: "1-based page in the reading sequence; omit for the page on screen",
};

export default function (pi: ExtensionAPI) {
  if (!SOCK) return; // not running inside the reader app

  pi.registerTool({
    name: "canvas_draw",
    label: "papier: draw on a page",
    description:
      "Draw freeform pen ink on a book page (black on the page; shown gray in the images " +
      "you receive so you can tell your ink from the user's). Takes an SVG whose coordinate " +
      "space IS the page: 1404 wide x 1872 tall, y down (omit viewBox or use " +
      'viewBox="0 0 1404 1872"). Supported: rect, line, circle, ellipse, polyline, polygon, ' +
      "path (M L H V C S Q T Z), and <text> (one line each; x,y = baseline; rendered as " +
      "single-stroke pen writing, font-family \"script\" | \"serif\" | \"sans\"). " +
      'Use fill="none" except tiny solid bits like arrowheads. On printed pages write ONLY ' +
      "in the margins (the pause message measures them); note pages are all yours. " +
      "To underline printed text use canvas_underline instead — it is exact. " +
      "Returns a patch id — keep it if you may want to erase/replace this later.",
    parameters: {
      type: "object",
      properties: {
        svg: { type: "string", description: "The SVG to draw, in page coordinates" },
        page: PAGE_PARAM,
      },
      required: ["svg"],
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "draw", svg: params.svg, page: params.page });
        if (!r.ok) return textResult(`draw failed: ${r.error}`, true);
        const bbox = r.bbox ? ` bbox (${r.bbox[0]},${r.bbox[1]})-(${r.bbox[2]},${r.bbox[3]})` : "";
        const layout = r.layout ? ` Page layout now: ${r.layout}` : "";
        const notes = r.notes?.length
          ? ` NOTE: ${r.notes.join("; ")}. If that broke your layout, erase patch ${r.id} and redraw.`
          : "";
        return {
          content: [{
            type: "text" as const,
            text: `Drawn on page ${r.page} as patch #${r.id}${bbox}. ` +
              `Use canvas_erase with id ${r.id} to remove it.${notes}${layout}`,
          }],
          details: { id: r.id, page: r.page, bbox: r.bbox },
        };
      } catch (e: any) {
        return textResult(`draw failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "canvas_underline",
    label: "papier: underline a printed phrase",
    description:
      "Underline a phrase of the PRINTED text with a hand-drawn-looking line. The phrase is " +
      "matched against the page's real word geometry (case- and punctuation-insensitive, " +
      "spans line breaks and end-of-line hyphenation), so the underline lands exactly under " +
      "the words — always prefer this over drawing lines yourself. Quote the phrase as it " +
      "appears in the page text. Returns a patch id (erasable like any patch) and how many " +
      "times the phrase occurs; pass occurrence: 2 to pick the second one, etc.",
    parameters: {
      type: "object",
      properties: {
        phrase: { type: "string", description: "The printed words to underline, quoted exactly" },
        occurrence: {
          type: "number",
          description: "Which occurrence to underline when the phrase repeats (1-based, default 1)",
        },
        page: PAGE_PARAM,
      },
      required: ["phrase"],
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({
          cmd: "underline",
          phrase: params.phrase,
          occurrence: params.occurrence,
          page: params.page,
        });
        if (!r.ok) return textResult(`underline failed: ${r.error}`, true);
        const more = r.matches > 1 ? ` (the phrase occurs ${r.matches}x on this page)` : "";
        return textResult(
          `Underlined on page ${r.page} as patch #${r.id}${more}. ` +
          `Use canvas_erase with id ${r.id} to remove it.`,
        );
      } catch (e: any) {
        return textResult(`underline failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "canvas_erase",
    label: "papier: erase one of your patches",
    description:
      "Erase a patch you previously made (canvas_draw or canvas_underline), by its id. The " +
      "printed page and the user's own ink underneath are untouched. Use this to correct or " +
      "replace an earlier mark instead of stacking new ink on top.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "number", description: "The patch id the drawing tool returned" },
        page: PAGE_PARAM,
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
    name: "canvas_view",
    label: "papier: look at a page",
    description:
      "Returns a fresh image of a book page (half scale: multiply image coordinates by 2 to " +
      "get page coordinates) plus the list of your patches on it (ids and bounding boxes). " +
      "Use it to re-check a page after drawing, or to look at another page's figures — for " +
      "plain text prefer canvas_page_text, it is much cheaper.",
    parameters: {
      type: "object",
      properties: {
        page: PAGE_PARAM,
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
                `Page ${r.page} of ${r.page_count} (${r.label}; ${r.page_width}x${r.page_height} page, ` +
                `image at 1/${r.image_scale} scale). Your patches: ${patches}.`,
            },
            { type: "image" as const, data: r.png_base64, mimeType: "image/png" },
          ],
        };
      } catch (e: any) {
        return textResult(`view failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "canvas_goto",
    label: "papier: turn to a page",
    description:
      "Turn the book on the user's screen to a given page (1-based, reading sequence), like " +
      "flipping there for them. Use it sparingly: when the user asks to go somewhere, or to " +
      "show them a note page you just wrote. It is refused while the user is actively " +
      "writing or inside a menu.",
    parameters: {
      type: "object",
      properties: {
        page: { type: "number", description: "1-based page to show" },
      },
      required: ["page"],
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "goto", page: params.page });
        return r.ok
          ? textResult(`Now showing page ${r.page} of ${r.page_count} (${r.label}). ${r.layout ?? ""}`)
          : textResult(`goto failed: ${r.error}`, true);
      } catch (e: any) {
        return textResult(`goto failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "canvas_insert_note",
    label: "papier: insert a blank note page",
    description:
      "Insert a fresh blank note page into the reading sequence, right after the current " +
      "page (or after after_page). Use it when a response deserves more room than a margin " +
      "allows; then write on it with canvas_draw {page: N} using the returned page number, " +
      "and leave a small pointer in the margin of the printed page (e.g. '* see note ->'). " +
      "The user flips to it like any page.",
    parameters: {
      type: "object",
      properties: {
        after_page: {
          type: "number",
          description: "1-based page to insert after; omit for the current page",
        },
      },
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "insert_note", after_page: params.after_page });
        return r.ok
          ? textResult(
              `Blank note page inserted as page ${r.page} (book now has ${r.page_count} pages). ` +
              `Draw on it with canvas_draw {page: ${r.page}}.`,
            )
          : textResult(`insert_note failed: ${r.error}`, true);
      } catch (e: any) {
        return textResult(`insert_note failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "canvas_page_text",
    label: "papier: read pages as text",
    description:
      "Returns the extracted text of a range of book pages (at most 8 per call) — the cheap " +
      "way to read along: recall earlier context, check definitions, read ahead. Note pages " +
      "in the range are listed but contain only handwriting (use canvas_view to see them).",
    parameters: {
      type: "object",
      properties: {
        from: { type: "number", description: "First page of the range (1-based)" },
        to: { type: "number", description: "Last page of the range; omit for just 'from'" },
      },
      required: ["from"],
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "page_text", from: params.from, to: params.to });
        return r.ok
          ? textResult(`Text of pages ${r.from}-${r.to} (of ${r.page_count}):\n${r.text}`)
          : textResult(`page_text failed: ${r.error}`, true);
      } catch (e: any) {
        return textResult(`page_text failed: ${e.message}`, true);
      }
    },
  });
}
