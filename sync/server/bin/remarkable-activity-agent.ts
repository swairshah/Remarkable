#!/usr/bin/env node
import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";

type Cli = {
  prompt: string;
  model: string;
  sourceDir: string;
  stateDir: string;
  outputHtml: string;
  envFile: string;
  rerender: boolean;
};

type Doc = {
  uuid: string;
  name: string;
  type: string;
  lastModified: string;
  lastOpened: string;
  lastOpenedPage: number | null;
  bookmCount: number;
  bookmHash: string;
  highlightsCount: number;
  highlightsSig: string;
  rmLatestMtime: number;
  pageOrder: string[];
  rmPages: Record<string, { m: number; s: number }>;
};

type State = { generatedAt: string; docs: Record<string, Doc> };

type PageRef = { id: string; label: string };

type Change = {
  uuid: string;
  name: string;
  type: string;
  lastModified: string;
  bits: string[];
  pages?: { changed: PageRef[]; added: PageRef[]; removed: PageRef[] };
};

type HistoryEntry = {
  at: string;
  totalChanges: number;
  shownChanges: number;
  summary?: string;
  changes: Change[];
};

const MAX_VISIBLE_CHANGES = 10;
const MAX_HISTORY_RUNS = 20;
const NOTEBOOK_IMAGE_LIMIT = 6;

function parseArgs(argv: string[]): Cli {
  const cli: Cli = {
    prompt:
      "Summarize reMarkable activity since last sync. Focus on reading progress, highlights, bookmarks, and notebook writing changes.",
    model: "gpt-5.5",
    sourceDir: path.join(homedir(), "remarkable-backup", "xochitl"),
    stateDir: path.join(homedir(), "remarkable-exports", "activity-agent"),
    outputHtml: path.join(homedir(), "notes", "updates", "index.html"),
    envFile: path.join(homedir(), ".env"),
    rerender: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const n = argv[i + 1];
    if ((a === "-p" || a === "--prompt") && n) {
      cli.prompt = n;
      i++;
    } else if ((a === "-m" || a === "--model") && n) {
      cli.model = n;
      i++;
    } else if (a === "--source-dir" && n) {
      cli.sourceDir = n;
      i++;
    } else if (a === "--state-dir" && n) {
      cli.stateDir = n;
      i++;
    } else if (a === "--output-html" && n) {
      cli.outputHtml = n;
      i++;
    } else if (a === "--env-file" && n) {
      cli.envFile = n;
      i++;
    } else if (a === "--rerender") {
      cli.rerender = true;
    }
  }

  return cli;
}

async function exists(p: string): Promise<boolean> {
  try {
    await fs.access(p);
    return true;
  } catch {
    return false;
  }
}

async function readJson<T>(p: string): Promise<T | null> {
  try {
    return JSON.parse(await fs.readFile(p, "utf8")) as T;
  } catch {
    return null;
  }
}

async function hashFile(p: string): Promise<string> {
  try {
    const b = await fs.readFile(p);
    return createHash("sha256").update(b).digest("hex");
  } catch {
    return "";
  }
}

async function loadEnv(file: string): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  if (!(await exists(file))) return out;
  const txt = await fs.readFile(file, "utf8");
  for (const raw of txt.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const i = line.indexOf("=");
    if (i <= 0) continue;
    out[line.slice(0, i).trim()] = line.slice(i + 1).trim().replace(/^['"]|['"]$/g, "");
  }
  return out;
}

async function buildState(base: string): Promise<State> {
  const docs: Record<string, Doc> = {};
  const entries = await fs.readdir(base, { withFileTypes: true });

  for (const e of entries) {
    if (!e.isFile() || !e.name.endsWith(".metadata")) continue;
    const uuid = e.name.slice(0, -9);
    const md = await readJson<Record<string, unknown>>(path.join(base, e.name));
    if (!md) continue;

    const bookmPath = path.join(base, `${uuid}.bookm`);
    const bookm = (await readJson<Record<string, unknown>>(bookmPath)) || {};

    const hlDir = path.join(base, `${uuid}.highlights`);
    let highlightsCount = 0;
    let highlightsSig = "";
    if (await exists(hlDir)) {
      const hFiles = (await fs.readdir(hlDir, { withFileTypes: true }))
        .filter((x) => x.isFile() && x.name.endsWith(".json"))
        .map((x) => path.join(hlDir, x.name))
        .sort();
      highlightsCount = hFiles.length;
      if (hFiles.length) {
        const sigParts: string[] = [];
        for (const f of hFiles) {
          const st = await fs.stat(f);
          sigParts.push(`${path.basename(f)}:${Math.trunc(st.mtimeMs)}:${st.size}`);
        }
        highlightsSig = createHash("sha256").update(sigParts.join("|"), "utf8").digest("hex");
      }
    }

    const content = await readJson<{ cPages?: { pages?: Array<{ id?: string }> } }>(
      path.join(base, `${uuid}.content`)
    );
    const pageOrder = (content?.cPages?.pages ?? []).map((p) => p.id ?? "").filter(Boolean);

    const rmDir = path.join(base, uuid);
    let rmLatestMtime = 0;
    const rmPages: Record<string, { m: number; s: number }> = {};
    if (await exists(rmDir)) {
      const rmFiles = await fs.readdir(rmDir, { withFileTypes: true });
      for (const rf of rmFiles) {
        if (!rf.isFile() || !rf.name.endsWith(".rm")) continue;
        const st = await fs.stat(path.join(rmDir, rf.name));
        rmLatestMtime = Math.max(rmLatestMtime, Math.trunc(st.mtimeMs));
        rmPages[rf.name.slice(0, -3)] = { m: Math.trunc(st.mtimeMs), s: st.size };
      }
    }

    docs[uuid] = {
      uuid,
      name: String(md.visibleName ?? ""),
      type: String(md.type ?? ""),
      lastModified: String(md.lastModified ?? "0"),
      lastOpened: String(md.lastOpened ?? "0"),
      lastOpenedPage:
        md.lastOpenedPage === undefined || md.lastOpenedPage === null
          ? null
          : Number(md.lastOpenedPage),
      bookmCount: Object.keys(bookm).length,
      bookmHash: (await exists(bookmPath)) ? await hashFile(bookmPath) : "",
      highlightsCount,
      highlightsSig,
      rmLatestMtime,
      pageOrder,
      rmPages,
    };
  }

  return { generatedAt: new Date().toISOString(), docs };
}

function diff(prev: State, cur: State): Change[] {
  const changes: Change[] = [];

  for (const [uuid, d] of Object.entries(cur.docs)) {
    const p = prev.docs[uuid];
    if (!p) {
      // Ignore first-seen docs to avoid backup/noise floods.
      continue;
    }

    const bits: string[] = [];
    if (p.lastOpenedPage !== d.lastOpenedPage) bits.push(`page ${String(p.lastOpenedPage)} -> ${String(d.lastOpenedPage)}`);
    if (p.lastOpened !== d.lastOpened) bits.push("opened");
    if (p.lastModified !== d.lastModified) bits.push("modified");
    if (p.bookmHash !== d.bookmHash) bits.push(`bookmarks ${p.bookmCount} -> ${d.bookmCount}`);
    if (p.highlightsSig !== d.highlightsSig) bits.push(`highlights ${p.highlightsCount} -> ${d.highlightsCount}`);

    // Per-page handwriting diff (falls back to the coarse signal for
    // states written before rmPages existed).
    let pages: Change["pages"];
    if (p.rmPages && Object.keys(p.rmPages).length + Object.keys(d.rmPages ?? {}).length > 0) {
      const label = (pid: string): string => {
        const idx = (d.pageOrder ?? []).indexOf(pid);
        if (idx >= 0) return `p${idx + 1}`;
        const pidx = (p.pageOrder ?? []).indexOf(pid);
        return pidx >= 0 ? `p${pidx + 1}` : pid.slice(0, 8);
      };
      const ref = (pid: string): PageRef => ({ id: pid, label: label(pid) });
      const changed: PageRef[] = [];
      const added: PageRef[] = [];
      const removed: PageRef[] = [];
      for (const [pid, curPage] of Object.entries(d.rmPages ?? {})) {
        const prevPage = p.rmPages[pid];
        if (!prevPage) added.push(ref(pid));
        else if (prevPage.m !== curPage.m || prevPage.s !== curPage.s) changed.push(ref(pid));
      }
      for (const pid of Object.keys(p.rmPages)) {
        if (!(d.rmPages ?? {})[pid]) removed.push(ref(pid));
      }
      if (changed.length) bits.push(`handwriting edited: ${changed.map((x) => x.label).join(", ")}`);
      if (added.length) bits.push(`pages added: ${added.map((x) => x.label).join(", ")}`);
      if (removed.length) bits.push(`pages removed: ${removed.map((x) => x.label).join(", ")}`);
      if (changed.length || added.length || removed.length) pages = { changed, added, removed };
    } else if (p.rmLatestMtime !== d.rmLatestMtime) {
      bits.push("handwriting changed");
    }

    // A lone lastOpened bump is xochitl housekeeping (e.g. the tablet waking
    // with a book on screen), not user activity — suppress it.
    if (bits.length === 1 && bits[0] === "opened") continue;

    if (bits.length) changes.push({ uuid, name: d.name || uuid, type: d.type, lastModified: d.lastModified, bits, ...(pages ? { pages } : {}) });
  }

  changes.sort((a, b) => Number(b.lastModified || 0) - Number(a.lastModified || 0));
  return changes;
}

// Copy thumbnails of edited/added pages into stateDir/changed-pages/<run>/
// so downstream consumers (e.g. Shelley) can see what was actually written.
async function copyChangedPageImages(cli: Cli, changes: Change[], runStamp: string): Promise<void> {
  const destRoot = path.join(cli.stateDir, "changed-pages");
  const stamp = runStamp.replace(/[:.]/g, "-");
  let copied = false;

  for (const c of changes) {
    if (!c.pages) continue;
    const thumbs = path.join(cli.sourceDir, `${c.uuid}.thumbnails`);
    if (!(await exists(thumbs))) continue;
    const slug = (c.name || c.uuid).replace(/[^a-zA-Z0-9_-]+/g, "_").slice(0, 40);
    for (const pg of [...c.pages.changed, ...c.pages.added]) {
      const src = path.join(thumbs, `${pg.id}.png`);
      if (!(await exists(src))) continue;
      const destDir = path.join(destRoot, stamp);
      await fs.mkdir(destDir, { recursive: true });
      await fs.copyFile(src, path.join(destDir, `${slug}-${pg.label}-${pg.id.slice(0, 8)}.png`));
      copied = true;
    }
  }

  if (!copied) return;
  const dirs = (await fs.readdir(destRoot, { withFileTypes: true }))
    .filter((d) => d.isDirectory())
    .map((d) => d.name)
    .sort();
  for (const old of dirs.slice(0, Math.max(0, dirs.length - 15))) {
    await fs.rm(path.join(destRoot, old), { recursive: true, force: true });
  }
}

async function collectNotebookImages(sourceDir: string, changes: Change[]): Promise<string[]> {
  const notebookChange = changes.find(
    (c) => c.name.trim().toLowerCase() === "notebook" && c.bits.some((b) => b.toLowerCase().includes("handwriting changed"))
  );

  if (!notebookChange) return [];

  const thumbsDir = path.join(sourceDir, `${notebookChange.uuid}.thumbnails`);
  if (!(await exists(thumbsDir))) return [];

  const items = await fs.readdir(thumbsDir, { withFileTypes: true });
  const files = items
    .filter((x) => x.isFile() && /\.(png|jpg|jpeg|webp)$/i.test(x.name))
    .map((x) => x.name);

  const withTimes: Array<{ name: string; mtime: number }> = [];
  for (const name of files) {
    const st = await fs.stat(path.join(thumbsDir, name));
    withTimes.push({ name, mtime: st.mtimeMs });
  }

  withTimes.sort((a, b) => b.mtime - a.mtime);
  const selected = withTimes.slice(0, NOTEBOOK_IMAGE_LIMIT);

  const dataUrls: string[] = [];
  for (const f of selected) {
    const full = path.join(thumbsDir, f.name);
    const buf = await fs.readFile(full);
    const ext = path.extname(f.name).toLowerCase();
    const mime = ext === ".png" ? "image/png" : ext === ".webp" ? "image/webp" : "image/jpeg";
    dataUrls.push(`data:${mime};base64,${buf.toString("base64")}`);
  }

  return dataUrls;
}

// Extract assistant text from a Responses API SSE stream. The exe.dev
// ChatGPT-subscription source requires store:false + stream:true, so we
// always read SSE and concatenate output_text deltas.
function parseResponsesSSE(body: string): string {
  const deltas: string[] = [];
  let doneText = "";
  for (const raw of body.split(/\r?\n/)) {
    if (!raw.startsWith("data: ")) continue;
    const payload = raw.slice(6).trim();
    if (!payload || payload === "[DONE]") continue;
    try {
      const ev = JSON.parse(payload) as {
        type?: string;
        delta?: string;
        part?: { type?: string; text?: string };
      };
      if (ev.type === "response.output_text.delta" && typeof ev.delta === "string") {
        deltas.push(ev.delta);
      } else if (ev.type === "response.content_part.done" && ev.part?.type === "output_text" && ev.part.text) {
        doneText += (doneText ? "\n" : "") + ev.part.text;
      }
    } catch {
      // ignore malformed SSE lines
    }
  }
  return (deltas.length ? deltas.join("") : doneText).trim();
}

async function summarizeWithLLM(cli: Cli, changes: Change[]): Promise<string> {
  const env = await loadEnv(cli.envFile);

  const system = "You are a concise personal activity summarizer. Return accurate markdown only.";
  const notebookImages = await collectNotebookImages(cli.sourceDir, changes);

  const userText = `${cli.prompt}\n\nChanges JSON:\n${JSON.stringify(changes, null, 2)}\n\n${
    notebookImages.length
      ? `Notebook context images attached: ${notebookImages.length} recent page previews from the Notebook document. Use them to infer what writing changed.`
      : ""
  }\n\nFormat:\n## Activity Summary\n## Reading\n## Writing\n## Highlights & Bookmarks\n## Next Actions`;

  const fallback = (label: string): string =>
    `${label}\n\n${changes.map((c) => `- ${c.name}: ${c.bits.join(", ")}`).join("\n")}`;

  // model "openrouter/<vendor>/<model>" -> OpenRouter chat completions.
  // Anything else -> exe.dev LLM integration (Responses API), which routes
  // gpt-* to the connected ChatGPT subscription. No API key needed in-VM.
  if (cli.model.startsWith("openrouter/")) {
    const key = process.env.OPENROUTER_API_KEY || env.OPENROUTER_API_KEY;
    if (!key) return fallback("OPENROUTER_API_KEY missing; fallback summary:");

    const userContent =
      notebookImages.length > 0
        ? [
            { type: "text", text: userText },
            ...notebookImages.map((url) => ({ type: "image_url", image_url: { url } })),
          ]
        : userText;

    const res = await fetch("https://openrouter.ai/api/v1/chat/completions", {
      method: "POST",
      headers: {
        Authorization: `Bearer ${key}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        model: cli.model.slice("openrouter/".length),
        temperature: 0.2,
        messages: [
          { role: "system", content: system },
          { role: "user", content: userContent },
        ],
      }),
    });

    if (!res.ok) {
      const err = await res.text();
      return `${fallback(`LLM call failed (${res.status}).`)}\n\nError: ${err}`;
    }

    const json = (await res.json()) as { choices?: Array<{ message?: { content?: string } }> };
    return json.choices?.[0]?.message?.content?.trim() || "(empty summary)";
  }

  const base = (process.env.EXE_LLM_BASE || env.EXE_LLM_BASE || "https://llm.int.exe.xyz").replace(/\/$/, "");
  const inputContent: Array<Record<string, unknown>> = [{ type: "input_text", text: userText }];
  for (const url of notebookImages) inputContent.push({ type: "input_image", image_url: url });

  const res = await fetch(`${base}/v1/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: cli.model,
      store: false,
      stream: true,
      instructions: system,
      input: [{ role: "user", content: inputContent }],
    }),
  });

  if (!res.ok) {
    const err = await res.text();
    return `${fallback(`LLM call failed (${res.status}).`)}\n\nError: ${err}`;
  }

  const text = parseResponsesSSE(await res.text());
  return text || "(empty summary)";
}

function esc(s: string): string {
  return s.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}

function humanTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString("en-US", {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

async function readHistory(historyPath: string): Promise<HistoryEntry[]> {
  if (!(await exists(historyPath))) return [];
  const txt = await fs.readFile(historyPath, "utf8");
  const lines = txt.split(/\r?\n/).filter(Boolean);
  const parsed: HistoryEntry[] = [];
  for (let i = lines.length - 1; i >= 0; i--) {
    try {
      const row = JSON.parse(lines[i]) as HistoryEntry;
      if (row && row.at && Array.isArray(row.changes)) parsed.push(row);
    } catch {
      // ignore malformed lines
    }
    if (parsed.length >= MAX_HISTORY_RUNS) break;
  }
  return parsed;
}

function renderHtml(summary: string, changes: Change[], history: HistoryEntry[]): string {
  const nowHuman = humanTime(new Date().toISOString());
  const historyItems = history
    .map((h, i) => {
      const firstLine = (h.summary || "").split("\n").find((x) => x.trim()) || "(no summary stored)";
      return `<li class="hist-item${i === 0 ? " active" : ""}" data-idx="${i}"><div class="hist-time">${esc(humanTime(h.at))}</div><div class="hist-meta">${h.shownChanges}/${h.totalChanges} changes</div><div class="hist-text">${esc(firstLine)}</div></li>`;
    })
    .join("\n");
  // Embedded for client-side browsing of past runs; <-escape so a
  // summary containing "</script>" can't break out of the script tag.
  const historyJson = JSON.stringify(history).replace(/</g, "\\u003c");

  return `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1.0"/>
<title>activity</title>
<link rel="preconnect" href="https://fonts.googleapis.com" />
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
<link href="https://fonts.googleapis.com/css2?family=Google+Sans+Code:wght@400;500;600&display=swap" rel="stylesheet" />
<style>
*,*::before,*::after{margin:0;padding:0;box-sizing:border-box}
:root{--bg:rgb(15,17,21);--bg2:rgb(26,29,35);--border:rgba(107,114,128,.2);--text:rgb(201,204,209);--bright:rgb(229,231,235);--muted:rgb(107,114,128);--accent:rgb(245,158,11);--serif:'Iowan Old Style','Palatino Linotype',Palatino,'Book Antiqua',Georgia,serif;--mono:'Google Sans Code',ui-monospace,'SF Mono',Menlo,Consolas,monospace}
/* Light mode — the site nav's toggle sets <html data-theme="light"> (see /nav.js) */
html[data-theme=light]{--bg:rgb(250,250,249);--bg2:rgb(239,238,234);--border:rgba(20,24,32,.14);--text:rgb(64,68,74);--bright:rgb(22,24,28);--muted:rgb(110,115,123);--accent:rgb(199,106,6)}
body{background:var(--bg);color:var(--text);font-family:var(--serif);font-size:17px;line-height:1.6;min-height:100vh}
pre,code{font-family:var(--mono)}
nav{padding:20px 24px;display:flex;justify-content:space-between;align-items:center}.nav-left{display:flex;align-items:center;gap:14px}.brand{color:var(--bright);text-decoration:none;font-weight:500}.brand:hover{color:var(--accent)}
.layout{display:flex;height:calc(100vh - 100px)} /* 70px own nav + ~30px injected site bar (/nav.js) */
#sidebar{width:340px;max-width:80vw;border-right:1px solid var(--border);padding:16px 14px;overflow:auto;transition:width .2s ease,padding .2s ease}
#sidebar.collapsed{width:0;padding:0;border-right:none;overflow:hidden}
.side-header{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px}
.side-title{color:var(--bright);font-size:14px}
.toggle{background:var(--bg2);border:1px solid var(--border);color:var(--muted);border-radius:6px;padding:6px 10px;cursor:pointer;font-family:inherit;font-size:16px;line-height:1}
.toggle:hover{color:var(--bright)}
#tablet-seen{color:var(--muted);font-size:12px;font-family:var(--mono);display:none}
#tablet-seen.stale{color:rgb(239,68,68);border:1px solid rgba(239,68,68,.4);border-radius:999px;padding:2px 10px}
.hist-list{list-style:none}.hist-item{padding:10px 8px;border-bottom:1px solid var(--border);cursor:pointer;border-radius:6px}.hist-item:hover{background:var(--bg2)}.hist-item.active{background:var(--bg2)}.hist-item.active .hist-time{color:var(--accent)}.hist-time{color:var(--bright);font-size:12px}.hist-meta{color:var(--muted);font-size:11px}.hist-text{color:var(--text);font-size:12px;margin-top:4px}
main{flex:1;max-width:860px;padding:24px 32px 56px;overflow-y:auto}h1{color:var(--bright);font-size:22px;font-weight:500;margin-bottom:8px;clear:both}.meta{color:var(--muted);font-size:13px;margin-bottom:24px}
h2{color:var(--bright);font-size:18px;font-weight:500;margin:24px 0 12px}pre{background:var(--bg2);border:1px solid var(--border);border-radius:8px;padding:16px;white-space:pre-wrap;overflow:auto;font-size:14px}
ul{list-style:none}
.item{padding:14px 0;border-bottom:1px solid var(--border)}.item:last-child{border-bottom:none}.title{color:var(--bright)}
code{background:var(--bg2);border:1px solid var(--border);padding:2px 6px;border-radius:4px;color:var(--muted);font-size:12px}.bits{margin-top:8px;padding-left:16px}.bits li{list-style:disc;color:var(--muted)}
@media (max-width: 900px){
  nav{padding:14px 18px}
  #sidebar{position:fixed;top:64px;left:0;bottom:0;background:var(--bg);z-index:10}
  main{padding:16px 18px 40px}
}
</style>
</head><body>
<nav><div class="nav-left"><button class="toggle" onclick="toggleSidebar()" title="Toggle history">&#9776;</button><a class="brand" href="/updates/">reMarkable diffs</a><span id="tablet-seen"></span></div></nav>
<div class="layout">
<aside id="sidebar" class="collapsed">
  <div class="side-header"><div class="side-title">previous summaries</div></div>
  <ul class="hist-list">${historyItems || "<li class='hist-item'><div class='hist-text'>No previous summaries yet.</div></li>"}</ul>
</aside>
<main>
  <h1>reMarkable activity</h1><p class="meta" id="meta">updated ${esc(nowHuman)}</p>
  <h2>agent summary</h2><pre id="summary">${esc(summary)}</pre>
  <h2>detected changes</h2>
  <ul id="changes">${changes.map((c) => `<li class="item"><div class="title">${esc(c.name)} <code>${esc(c.uuid)}</code></div><ul class="bits">${c.bits.map((b) => `<li>${esc(b)}</li>`).join("")}</ul></li>`).join("\n")}</ul>
</main>
</div>
<script>
function toggleSidebar(){document.getElementById('sidebar').classList.toggle('collapsed')}
const HISTORY = ${historyJson};
function fmtTime(iso){
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  return d.toLocaleString('en-US',{year:'numeric',month:'short',day:'2-digit',hour:'2-digit',minute:'2-digit'});
}
function renderEntry(i){
  const h = HISTORY[i];
  if (!h) return;
  document.getElementById('summary').textContent = h.summary || '(no summary stored)';
  document.getElementById('meta').textContent =
    (i === 0 ? 'latest · ' : '') + fmtTime(h.at) + ' · ' + h.shownChanges + '/' + h.totalChanges + ' changes';
  const ul = document.getElementById('changes');
  ul.replaceChildren();
  for (const c of (h.changes || [])) {
    const li = document.createElement('li'); li.className = 'item';
    const title = document.createElement('div'); title.className = 'title';
    title.textContent = (c.name || c.uuid) + ' ';
    const code = document.createElement('code'); code.textContent = c.uuid;
    title.appendChild(code);
    const bits = document.createElement('ul'); bits.className = 'bits';
    for (const b of (c.bits || [])) {
      const bi = document.createElement('li'); bi.textContent = b; bits.appendChild(bi);
    }
    li.appendChild(title); li.appendChild(bits); ul.appendChild(li);
  }
  document.querySelectorAll('.hist-item').forEach((el) =>
    el.classList.toggle('active', Number(el.dataset.idx) === i));
}
document.querySelectorAll('.hist-item[data-idx]').forEach((el) =>
  el.addEventListener('click', () => renderEntry(Number(el.dataset.idx))));
// Tablet heartbeat. last-sync (same dir) is touched by every real tablet
// contact: each push hook run + the daily notes pull. Staleness is computed
// HERE, client-side, from the file's Last-Modified — the digest only
// re-renders when the tablet syncs, so a wedged tablet freezes this page
// and a server-rendered warning could never appear.
const STALE_HOURS = 26; // healthy worst case: one notes pull per day + slack
fetch('last-sync', {method:'HEAD', cache:'no-store'}).then((r) => {
  if (!r.ok) return;
  const lm = r.headers.get('Last-Modified');
  const ageMs = Date.now() - new Date(lm).getTime();
  if (!lm || isNaN(ageMs)) return;
  const el = document.getElementById('tablet-seen');
  const h = ageMs / 3600000;
  const label = h < 1 ? Math.max(1, Math.round(ageMs / 60000)) + 'm'
              : h < 48 ? Math.round(h) + 'h'
              : (h / 24).toFixed(1) + 'd';
  el.style.display = 'inline-block';
  if (h > STALE_HOURS) {
    el.classList.add('stale');
    el.textContent = '⚠ tablet silent ' + label + ' — sync may be wedged';
  } else {
    el.textContent = 'tablet seen ' + label + ' ago';
  }
}).catch(() => {});
</script>
</body></html>`;
}

async function main() {
  const cli = parseArgs(process.argv.slice(2));
  await fs.mkdir(cli.stateDir, { recursive: true });

  const statePath = path.join(cli.stateDir, "last-state.json");
  const latestPath = path.join(cli.stateDir, "latest.md");
  const historyPath = path.join(cli.stateDir, "history.jsonl");

  if (cli.rerender) {
    // Re-render the page from stored state (design changes) without
    // diffing, calling the LLM, or touching state/history.
    const summary = (await exists(latestPath))
      ? (await fs.readFile(latestPath, "utf8")).trim()
      : "(no summary published yet)";
    const history = await readHistory(historyPath);
    const changes = history[0]?.changes ?? [];
    await fs.mkdir(path.dirname(cli.outputHtml), { recursive: true });
    await fs.writeFile(cli.outputHtml, renderHtml(summary, changes, history), "utf8");
    console.log(`Re-rendered ${cli.outputHtml} from stored state.`);
    return;
  }

  const prev = await readJson<State>(statePath);
  const cur = await buildState(cli.sourceDir);
  await fs.writeFile(statePath, JSON.stringify(cur, null, 2), "utf8");

  if (!prev) {
    const msg = "Baseline created. Next run will produce diffs.";
    await fs.writeFile(latestPath, msg + "\n", "utf8");
    console.log(msg);
    return;
  }

  const changes = diff(prev, cur);
  if (!changes.length) {
    console.log("No changes since last sync. Nothing published.");
    return;
  }

  // Machine-readable diff feed + page images for downstream consumers
  // (persisted before the LLM call so they survive an API failure).
  const runStamp = new Date().toISOString();
  await fs.appendFile(
    path.join(cli.stateDir, "diffs.jsonl"),
    JSON.stringify({ at: runStamp, changes }) + "\n",
    "utf8"
  );
  await copyChangedPageImages(cli, changes, runStamp);

  const visibleChanges = changes.slice(0, MAX_VISIBLE_CHANGES);
  const summary = await summarizeWithLLM(cli, visibleChanges);
  await fs.writeFile(latestPath, summary + "\n", "utf8");
  await fs.appendFile(
    historyPath,
    JSON.stringify({
      at: new Date().toISOString(),
      totalChanges: changes.length,
      shownChanges: visibleChanges.length,
      summary,
      changes: visibleChanges,
    }) + "\n",
    "utf8"
  );

  const history = await readHistory(historyPath);

  await fs.mkdir(path.dirname(cli.outputHtml), { recursive: true });
  await fs.writeFile(cli.outputHtml, renderHtml(summary, visibleChanges, history), "utf8");
  // Marker consumed by remarkable-post-sync.sh to decide whether to ping Shelley.
  await fs.writeFile(path.join(cli.stateDir, "last-published"), runStamp + "\n", "utf8");

  console.log(`Published ${visibleChanges.length}/${changes.length} change(s) to ${cli.outputHtml}`);
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : String(err));
  process.exit(1);
});
