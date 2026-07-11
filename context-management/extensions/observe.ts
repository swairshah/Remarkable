/**
 * observe.ts — a CONTEXT-USAGE METER for the compression sandbox. Not a compressor.
 *
 * Two signals, so you can iterate on compress.ts and watch the effect:
 *
 *   1) DRY estimate (free, every LLM call) — hooks `context`, which hands us the
 *      exact (possibly compressed) message array that's about to be sent. We
 *      count tokens: text ≈ chars/4, images by RESOLUTION (Anthropic downsizes
 *      to ~1.15 MP then ≈ pixels/750, so a full page ≈ 1,533 tok, a 350px
 *      thumbnail ≈ 220 tok). This responds to your tiering — shrink an image
 *      and the number drops. Base64 *length* is NOT token cost; don't use bytes.
 *
 *   2) LIVE ground truth (only on real calls) — hooks `message_end` and prints
 *      the provider's actual billed usage (input / cacheRead / output / $).
 *
 * Load AFTER compress.ts so the estimate reflects the compressed payload:
 *   pi --fork <session> -ne -e extensions/compress.ts -e extensions/observe.ts ...
 *
 * Env: PI_OBSERVE_TAG=<label>   PI_OBSERVE_ABORT=1 (cancel turn after the dry
 * estimate → measure for free, no model call).
 */
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const REPORT_DIR = fileURLToPath(new URL("../reports", import.meta.url));
const TAG = process.env.PI_OBSERVE_TAG ?? "baseline";
const ABORT = process.env.PI_OBSERVE_ABORT === "1";

const FULL_PAGE = 1404 * 1872; // fallback if dims can't be read
const MAX_PX = 1_150_000; // Anthropic resizes larger images down to ~1.15 MP
const estTextTokens = (s: string) => Math.max(0, Math.ceil(s.length / 4));

/** Read (w,h) from a base64 PNG (IHDR) or JPEG (SOF marker) prefix. */
function imageDims(b64: string): { w: number; h: number } | null {
  try {
    const buf = Buffer.from(b64.slice(0, 8192), "base64");
    if (buf[0] === 0x89 && buf[1] === 0x50) return { w: buf.readUInt32BE(16), h: buf.readUInt32BE(20) };
    if (buf[0] === 0xff && buf[1] === 0xd8) {
      let i = 2;
      while (i < buf.length - 9) {
        if (buf[i] !== 0xff) { i++; continue; }
        const m = buf[i + 1];
        if (m >= 0xc0 && m <= 0xcf && m !== 0xc4 && m !== 0xc8 && m !== 0xcc)
          return { h: buf.readUInt16BE(i + 5), w: buf.readUInt16BE(i + 7) };
        i += 2 + buf.readUInt16BE(i + 2);
      }
    }
  } catch { /* fall through */ }
  return null;
}

function imageTokens(b64: string): number {
  const d = imageDims(b64);
  const px = d ? d.w * d.h : FULL_PAGE;
  return Math.ceil(Math.min(px, MAX_PX) / 750);
}

function blockTokens(b: any): number {
  if (typeof b === "string") return estTextTokens(b);
  if (b?.type === "image") return imageTokens(b.data ?? "");
  if (b?.type === "text") return estTextTokens(b.text ?? "");
  return estTextTokens(JSON.stringify(b ?? {}));
}

const k = (n: number) => (n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n));

export default function (pi: ExtensionAPI) {
  mkdirSync(REPORT_DIR, { recursive: true });
  let call = 0;

  // ---- (1) DRY: token estimate of the (compressed) payload, per LLM call ----
  pi.on("context", async (event: any, ctx: any) => {
    call++;
    const messages: any[] = event.messages ?? [];
    let textTok = 0, imgTok = 0, images = 0, bytes = 0;

    for (const m of messages) {
      const blocks = typeof m.content === "string" ? [m.content] : (m.content ?? []);
      for (const b of blocks) {
        const t = blockTokens(b);
        if (b?.type === "image") { imgTok += t; images++; bytes += b.data?.length ?? 0; }
        else textTok += t;
        if (typeof b === "string") bytes += Buffer.byteLength(b, "utf8");
        else if (b?.type === "text") bytes += Buffer.byteLength(b.text ?? "", "utf8");
      }
    }
    const estTok = textTok + imgTok;
    const win = ctx.getContextUsage?.()?.contextWindow ?? 200_000;
    const pct = win ? (100 * estTok / win) : 0;

    const line =
      `\x1b[1m[observe:${TAG}] call#${call}\x1b[0m  ` +
      `${messages.length} msg · ${(bytes / 1e6).toFixed(1)}MB · ${images} img\n` +
      `  \x1b[36m~${k(estTok)} tok\x1b[0m (text ${k(textTok)} + img ${k(imgTok)})` +
      `  →  \x1b[33m${pct.toFixed(1)}%\x1b[0m of ${k(win)} window`;
    process.stderr.write("\n" + line + "\n");

    const path = `${REPORT_DIR}/${TAG}-call-${String(call).padStart(2, "0")}.json`;
    writeFileSync(path, JSON.stringify(
      { tag: TAG, call, messages: messages.length, bytes, images, estTok, textTok, imgTok, window: win, pct }, null, 2));
    appendFileSync(`${REPORT_DIR}/${TAG}-summary.log`,
      `call#${call}\t${messages.length}msg\t${(bytes / 1e6).toFixed(1)}MB\t~${k(estTok)}tok\t${pct.toFixed(1)}%win\timg=${images}\n`);

    if (ABORT && typeof ctx.abort === "function") {
      process.stderr.write(`[observe:${TAG}] abort → measured for free (no model call)\n`);
      ctx.abort();
    }
    return undefined; // pure observation
  });

  // ---- (2) LIVE: the provider's REAL billed usage, per assistant message -----
  pi.on("message_end", async (event: any) => {
    const u = event?.message?.usage;
    if (event?.message?.role !== "assistant" || !u) return;
    if (!u.input && !u.output && !u.cacheRead) return; // aborted/empty turn (dry runs)
    process.stderr.write(
      `\x1b[32m[observe:${TAG}] REAL usage — in ${k(u.input)} · cacheRead ${k(u.cacheRead)} · ` +
      `out ${k(u.output)} · $${(u.cost?.total ?? 0).toFixed(4)}\x1b[0m\n`);
    appendFileSync(`${REPORT_DIR}/${TAG}-realusage.log`,
      `in=${u.input}\tcacheRead=${u.cacheRead}\tout=${u.output}\ttotal=${u.totalTokens}\t$${(u.cost?.total ?? 0).toFixed(4)}\n`);
    return undefined;
  });
}
