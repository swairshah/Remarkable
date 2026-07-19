/**
 * papier-metrics.ts — per-turn pi telemetry on the device.
 *
 * Answers "what took how long, and how much context was sent" for every pi
 * call. Loaded alongside papier-canvas.ts via PAPIER_EXT (colon-separated).
 *
 * Writes one JSON line per event to $PAPIER_METRICS
 * (default ~/.local/share/papier/metrics.jsonl):
 *   {t:"ctx",  ts, call, msgs, bytes, imgs, estTok, textTok, imgTok}   payload of each LLM call
 *   {t:"turn", ts, latMs, model, stop, in, cacheR, cacheW, out, cost}  real usage + wall latency
 *   {t:"tool", ts, name, ms, isError}                                  tool execution timing
 * Also prints a one-line summary per turn to stderr → /tmp/papier.log.
 *
 * Pulled to the desk by `make trace` and rendered as a table in trace.html.
 */
import { appendFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const OUT =
  process.env.PAPIER_METRICS ??
  `${process.env.HOME || "/home/root"}/.local/share/papier/metrics.jsonl`;

const MAX_PX = 1_150_000; // Anthropic resizes larger images to ~1.15 MP; tokens ≈ px/750
const estText = (s: string) => Math.ceil((s?.length ?? 0) / 4);

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
const imgTokens = (b64: string) => {
  const d = imageDims(b64);
  return Math.ceil(Math.min(d ? d.w * d.h : MAX_PX, MAX_PX) / 750);
};

function emit(rec: Record<string, unknown>) {
  try {
    mkdirSync(dirname(OUT), { recursive: true });
    appendFileSync(OUT, JSON.stringify({ ts: new Date().toISOString(), ...rec }) + "\n");
  } catch { /* never break the session over telemetry */ }
}

const k = (n: number) => (n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n ?? 0));

export default function (pi: ExtensionAPI) {
  let call = 0;
  let turnStart = 0;
  let lastPayload = { bytes: 0, imgs: 0, estTok: 0 };
  const toolStart = new Map<string, number>();

  pi.on("turn_start", () => { turnStart = Date.now(); });

  pi.on("context", async (event: any) => {
    call++;
    let bytes = 0, imgs = 0, textTok = 0, imgTok = 0;
    for (const m of event.messages ?? []) {
      const blocks = typeof m.content === "string" ? [m.content] : (m.content ?? []);
      for (const b of blocks) {
        if (typeof b === "string") { bytes += b.length; textTok += estText(b); }
        else if (b?.type === "image") { bytes += b.data?.length ?? 0; imgs++; imgTok += imgTokens(b.data ?? ""); }
        else if (b?.type === "text") { bytes += b.text?.length ?? 0; textTok += estText(b.text); }
        else { textTok += estText(JSON.stringify(b ?? {})); }
      }
    }
    lastPayload = { bytes, imgs, estTok: textTok + imgTok };
    emit({ t: "ctx", call, msgs: (event.messages ?? []).length, bytes, imgs, estTok: textTok + imgTok, textTok, imgTok });
    return undefined; // observe only
  });

  pi.on("message_end", async (event: any) => {
    const m = event?.message;
    if (m?.role !== "assistant") return;
    const u = m.usage;
    if (!u || (!u.input && !u.output && !u.cacheRead)) return; // aborted/empty
    const latMs = turnStart ? Date.now() - turnStart : null;
    emit({
      t: "turn", latMs, model: m.model, stop: m.stopReason,
      in: u.input, cacheR: u.cacheRead, cacheW: u.cacheWrite, out: u.output,
      cost: u.cost?.total ?? null,
      sentBytes: lastPayload.bytes, sentImgs: lastPayload.imgs, sentEstTok: lastPayload.estTok,
    });
    process.stderr.write(
      `pi-metrics: turn ${latMs != null ? (latMs / 1000).toFixed(1) + "s" : "?"} · ` +
      `in ${k(u.input)} cache ${k(u.cacheRead)} out ${k(u.output)} · ` +
      `sent ${(lastPayload.bytes / 1e6).toFixed(1)}MB/${lastPayload.imgs}img\n`);
    return undefined;
  });

  pi.on("tool_execution_start", (event: any) => {
    toolStart.set(event.toolCallId ?? event.toolCall?.id ?? "?", Date.now());
  });
  pi.on("tool_execution_end", (event: any) => {
    const id = event.toolCallId ?? event.toolCall?.id ?? "?";
    const t0 = toolStart.get(id);
    toolStart.delete(id);
    emit({
      t: "tool", name: event.toolName ?? event.toolCall?.name ?? "?",
      ms: t0 ? Date.now() - t0 : null, isError: !!event.result?.isError,
    });
  });
}
