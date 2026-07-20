/**
 * Canvas tools for pi inside the Coder app (reMarkable 2).
 *
 * The app (a Rust takeover binary owning the e-ink panel) spawns pi with
 * `-e <this file>` and CODER_SOCK pointing at its unix tool socket.
 * These tools speak one JSON object per line over that socket:
 *
 *   coder_projects  {}                    -> the sidebar: every project
 *   coder_goto      {project?, page?}     -> switch project / turn page
 *   coder_draw      {svg, page?}          -> SVG -> pen strokes -> patch id
 *   coder_erase     {id, page?}           -> remove one of pi's patches
 *   coder_erase_ink {rect, page?}         -> remove user handwriting (consumed instructions)
 *   coder_view      {page?}               -> fresh half-scale PNG of a page
 *
 * Everything else — cloning repos, reading code, editing, PRs — is pi's
 * ordinary shell running ssh to the VM; no special tools needed.
 *
 * When CODER_SOCK is absent (a normal pi session), nothing registers.
 */

import * as net from "node:net";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

/* NOTE: `||` not `??` — takeover.sh passes these as empty strings when unset */
const SOCK = process.env.CODER_SOCK || "";

function call(cmd: Record<string, unknown>, timeoutMs = 30000): Promise<any> {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(SOCK);
    let buf = "";
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error("the coder app did not answer on " + SOCK));
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
  if (!SOCK) return; // not running inside the coder app

  pi.registerTool({
    name: "coder_projects",
    label: "coder: list the projects",
    description:
      "List every project in the sidebar: slug, display name, repo url, one-line " +
      "summary, page count, and which one is on the user's screen right now. Free and " +
      "instant — check it before coder_goto when unsure of a slug.",
    parameters: { type: "object", properties: {} },
    async execute() {
      try {
        const r = await call({ cmd: "projects" });
        if (!r.ok) return textResult(`projects failed: ${r.error}`, true);
        const lines = (r.projects ?? []).map((p: any) => {
          const bits = [
            `${p.slug}${p.on_screen ? " [ON SCREEN]" : ""}`,
            `${p.pages} page${p.pages === 1 ? "" : "s"}`,
          ];
          if (p.url) bits.push(p.url);
          if (p.summary) bits.push(p.summary);
          return "- " + bits.join(" · ");
        });
        return textResult(
          `Projects (current: ${r.current}, page ${r.page}/${r.page_count}):\n` +
            lines.join("\n"),
        );
      } catch (e: any) {
        return textResult(`projects failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "coder_goto",
    label: "coder: show a project / page",
    description:
      "Flip the tablet to a project (by slug) and/or a page (1-based) of it. Use after " +
      "cloning + drawing an overview so the user lands on your work, or when they ask. " +
      "Refused while the user is writing or inside a menu — then just tell them on the " +
      "page you are on (or let them find it in the sidebar). Omit `project` to turn " +
      "pages within the current one. Drawing tools always target the CURRENT project, " +
      "so goto the project first.",
    parameters: {
      type: "object",
      properties: {
        project: { type: "string", description: "Project slug (see coder_projects)" },
        page: { type: "number", description: "1-based page number within the project" },
      },
    },
    async execute(_id: string, params: any) {
      try {
        const r = await call({ cmd: "goto", project: params.project, page: params.page });
        return r.ok
          ? textResult(
              `Now showing project '${r.project}' page ${r.page} of ${r.page_count}. ${r.layout ?? ""}`,
            )
          : textResult(`goto failed: ${r.error}`, true);
      } catch (e: any) {
        return textResult(`goto failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "coder_draw",
    label: "coder: draw on the page",
    description:
      "Draw on the current project's page as freeform pen ink (black on the page; shown " +
      "gray in the images you receive so you can tell your ink from the user's). Takes " +
      "an SVG whose coordinate space IS the page: 1404 wide x 1872 tall, y down (omit " +
      'viewBox or use viewBox="0 0 1404 1872"). Supported: rect, line, circle, ellipse, ' +
      "polyline, polygon, path (M L H V C S Q T Z), and <text> (one line each; x,y = " +
      'baseline; font-family "script" | "serif" | "sans"). Use fill="none" except tiny ' +
      "solid bits (arrowheads). `page` beyond the last one APPENDS a fresh page — build " +
      "multi-page documents that way. Returns a patch id — keep it if you may erase or " +
      "replace this later.",
    parameters: {
      type: "object",
      properties: {
        svg: { type: "string", description: "The SVG to draw, in page coordinates" },
        page: {
          type: "number",
          description:
            "1-based page number; omit for the page on screen; count+1 appends a new page",
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
        const appended = r.appended ? ` (new page appended — the project now has ${r.page_count})` : "";
        return textResult(
          `Drawn on page ${r.page} as patch #${r.id}${bbox}${appended}. ` +
            `Use coder_erase with id ${r.id} to remove it.${notes}`,
        );
      } catch (e: any) {
        return textResult(`draw failed: ${e.message}`, true);
      }
    },
  });

  pi.registerTool({
    name: "coder_erase",
    label: "coder: erase one of your patches",
    description:
      "Erase a patch you previously drew with coder_draw, by its id. The user's own " +
      "ink underneath is untouched (the page re-renders from vectors). Use to replace " +
      "a stale diagram (e.g. after a merged change reshaped the architecture).",
    parameters: {
      type: "object",
      properties: {
        id: { type: "number", description: "The patch id coder_draw returned" },
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
    name: "coder_erase_ink",
    label: "coder: erase the user's handwriting",
    description:
      "Remove the USER'S handwritten strokes that lie fully inside a rect — for " +
      "cleaning up an instruction they wrote to you AFTER you have acted on it (e.g. " +
      "'clone github.com/x/y', now cloned; a change request now shipped as a PR). Only " +
      "strokes entirely inside the rect are removed, so frame the handwriting tightly. " +
      "NEVER use this on their sketches or notes-to-self, and only erase instructions " +
      "that were clearly addressed to you and are now done. When in doubt, leave it.",
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
    name: "coder_view",
    label: "coder: look at a page",
    description:
      "Returns a fresh image of a page of the CURRENT project (half scale: multiply " +
      "image coordinates by 2 to get page coordinates) plus the list of your ink " +
      "patches on it. Use it to check the page before drawing, after drawing, or to " +
      "re-read what the user wrote. For another project's pages, coder_goto there first.",
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
