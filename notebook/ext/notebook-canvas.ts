/**
 * Canvas tools for pi inside the notebook app (reMarkable 2).
 *
 * The app (a Rust takeover binary owning the e-ink panel) spawns pi with
 * `-e <this file>` and NOTEBOOK_SOCK pointing at its unix tool socket.
 * These tools speak one JSON object per line over that socket:
 *
 *   notebook_draw   {cmd:"draw",  svg, page?}  -> {ok, id, bbox}
 *   notebook_erase  {cmd:"erase", id,  page?}  -> {ok}
 *   notebook_view   {cmd:"view",  page?}       -> {ok, png_base64, patches,...}
 *
 * The SVG becomes real pen strokes on the page (gray ink, animated in);
 * <text> renders in a single-stroke plotter font. Patches are tracked by
 * id so they can be erased cleanly later.
 *
 * When NOTEBOOK_SOCK is absent (a normal pi session), nothing registers —
 * the tools only exist inside the app.
 */

import * as net from "node:net";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const SOCK = process.env.NOTEBOOK_SOCK ?? "";

function call(cmd: Record<string, unknown>, timeoutMs = 20000): Promise<any> {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(SOCK);
    let buf = "";
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error("the notebook app did not answer on " + SOCK));
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

export default function (pi: ExtensionAPI) {
  if (!SOCK) return; // not running inside the notebook app

  pi.registerTool({
    name: "notebook_draw",
    label: "notebook: draw on the page",
    description:
      "Draw on the user's notebook page as freeform pen ink (black on the page; shown gray " +
      "in the images you receive so you can tell your ink from the user's). Takes an SVG whose " +
      "coordinate space IS the page: 1404 wide x 1872 tall, y down (omit viewBox or use " +
      'viewBox="0 0 1404 1872"). Supported: rect, line, circle, ellipse, polyline, polygon, ' +
      "path (M L H V C S Q T Z), and <text> (one line each; x,y = baseline; size it to match " +
      "the user's handwriting — the pause message suggests a font-size; rendered as single-" +
      "stroke pen writing, font-family \"script\" | \"serif\" | \"sans\" to pick the face). " +
      "Use fill=\"none\" except for tiny " +
      "solid bits like arrowheads. The drawing is animated onto the page stroke by stroke. " +
      "Returns a patch id — keep it if you may want to erase/replace this later.",
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
        const layout = r.layout ? ` Page layout now: ${r.layout}` : "";
        const notes = r.notes?.length
          ? ` NOTE: ${r.notes.join("; ")}. If that broke your layout, erase patch ${r.id} and redraw.`
          : "";
        return {
          content: [{
            type: "text" as const,
            text: `Drawn on page ${r.page} as patch #${r.id}${bbox}. ` +
              `Use notebook_erase with id ${r.id} to remove it.${notes}${layout}`,
          }],
          details: { id: r.id, page: r.page, bbox: r.bbox },
        };
      } catch (e: any) {
        return textResult(`draw failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "notebook_erase",
    label: "notebook: erase one of your patches",
    description:
      "Erase a patch you previously drew with notebook_draw, by its id. The user's own ink " +
      "underneath is untouched (the page re-renders from vectors). Use this to correct or " +
      "replace an earlier note instead of stacking new ink on top.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "number", description: "The patch id notebook_draw returned" },
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
    name: "notebook_goto",
    label: "notebook: turn to a page",
    description:
      "Turn the notebook on the user's screen to a given page (1-based), like flipping there " +
      "for them. Use it sparingly: when the user asks to go somewhere, or when you need to " +
      "show them something you drew on another page. It is refused while the user is " +
      "actively writing or inside a menu. Returns the page's measured layout.",
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
    name: "notebook_view",
    label: "notebook: look at a page",
    description:
      "Returns a fresh image of a notebook page (half scale: multiply image coordinates by 2 " +
      "to get page coordinates) plus the list of your patches on it (ids and bounding boxes). " +
      "Use it to re-check the page after drawing, or to read another page for context — e.g. " +
      "the previous page when the current one continues a draft, list, or question from it.",
    parameters: {
      type: "object",
      properties: {
        page: {
          type: "number",
          description: "1-based page number; omit for the page currently on screen",
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
                `Page ${r.page} of ${r.page_count} (${r.page_width}x${r.page_height} page, ` +
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
}
