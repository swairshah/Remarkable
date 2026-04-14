#!/usr/bin/env node
var __create = Object.create;
var __getProtoOf = Object.getPrototypeOf;
var __defProp = Object.defineProperty;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __toESM = (mod, isNodeMode, target) => {
  target = mod != null ? __create(__getProtoOf(mod)) : {};
  const to = isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target;
  for (let key of __getOwnPropNames(mod))
    if (!__hasOwnProp.call(to, key))
      __defProp(to, key, {
        get: () => mod[key],
        enumerable: true
      });
  return to;
};

// server/bin/remarkable-activity-agent.ts
var import_node_crypto = require("node:crypto");
var import_node_fs = require("node:fs");
var import_node_path = __toESM(require("node:path"));
var MAX_VISIBLE_CHANGES = 10;
var MAX_HISTORY_RUNS = 20;
var NOTEBOOK_IMAGE_LIMIT = 6;
function parseArgs(argv) {
  const cli = {
    prompt: "Summarize reMarkable activity since last sync. Focus on reading progress, highlights, bookmarks, and notebook writing changes.",
    model: "anthropic/claude-sonnet-4-6",
    sourceDir: "/home/swair/remarkable-backup/xochitl",
    stateDir: "/home/swair/remarkable-exports/activity-agent",
    outputHtml: "/home/swair/remarkable-exports/activity-agent/index.html",
    envFile: "/home/swair/.env"
  };
  for (let i = 0;i < argv.length; i++) {
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
    }
  }
  return cli;
}
async function exists(p) {
  try {
    await import_node_fs.promises.access(p);
    return true;
  } catch {
    return false;
  }
}
async function readJson(p) {
  try {
    return JSON.parse(await import_node_fs.promises.readFile(p, "utf8"));
  } catch {
    return null;
  }
}
async function hashFile(p) {
  try {
    const b = await import_node_fs.promises.readFile(p);
    return import_node_crypto.createHash("sha256").update(b).digest("hex");
  } catch {
    return "";
  }
}
async function loadEnv(file) {
  const out = {};
  if (!await exists(file))
    return out;
  const txt = await import_node_fs.promises.readFile(file, "utf8");
  for (const raw of txt.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#"))
      continue;
    const i = line.indexOf("=");
    if (i <= 0)
      continue;
    out[line.slice(0, i).trim()] = line.slice(i + 1).trim().replace(/^['"]|['"]$/g, "");
  }
  return out;
}
async function buildState(base) {
  const docs = {};
  const entries = await import_node_fs.promises.readdir(base, { withFileTypes: true });
  for (const e of entries) {
    if (!e.isFile() || !e.name.endsWith(".metadata"))
      continue;
    const uuid = e.name.slice(0, -9);
    const md = await readJson(import_node_path.default.join(base, e.name));
    if (!md)
      continue;
    const bookmPath = import_node_path.default.join(base, `${uuid}.bookm`);
    const bookm = await readJson(bookmPath) || {};
    const hlDir = import_node_path.default.join(base, `${uuid}.highlights`);
    let highlightsCount = 0;
    let highlightsSig = "";
    if (await exists(hlDir)) {
      const hFiles = (await import_node_fs.promises.readdir(hlDir, { withFileTypes: true })).filter((x) => x.isFile() && x.name.endsWith(".json")).map((x) => import_node_path.default.join(hlDir, x.name)).sort();
      highlightsCount = hFiles.length;
      if (hFiles.length) {
        const sigParts = [];
        for (const f of hFiles) {
          const st = await import_node_fs.promises.stat(f);
          sigParts.push(`${import_node_path.default.basename(f)}:${Math.trunc(st.mtimeMs)}:${st.size}`);
        }
        highlightsSig = import_node_crypto.createHash("sha256").update(sigParts.join("|"), "utf8").digest("hex");
      }
    }
    const rmDir = import_node_path.default.join(base, uuid);
    let rmLatestMtime = 0;
    if (await exists(rmDir)) {
      const rmFiles = await import_node_fs.promises.readdir(rmDir, { withFileTypes: true });
      for (const rf of rmFiles) {
        if (!rf.isFile() || !rf.name.endsWith(".rm"))
          continue;
        const st = await import_node_fs.promises.stat(import_node_path.default.join(rmDir, rf.name));
        rmLatestMtime = Math.max(rmLatestMtime, Math.trunc(st.mtimeMs));
      }
    }
    docs[uuid] = {
      uuid,
      name: String(md.visibleName ?? ""),
      type: String(md.type ?? ""),
      lastModified: String(md.lastModified ?? "0"),
      lastOpened: String(md.lastOpened ?? "0"),
      lastOpenedPage: md.lastOpenedPage === undefined || md.lastOpenedPage === null ? null : Number(md.lastOpenedPage),
      bookmCount: Object.keys(bookm).length,
      bookmHash: await exists(bookmPath) ? await hashFile(bookmPath) : "",
      highlightsCount,
      highlightsSig,
      rmLatestMtime
    };
  }
  return { generatedAt: new Date().toISOString(), docs };
}
function diff(prev, cur) {
  const changes = [];
  for (const [uuid, d] of Object.entries(cur.docs)) {
    const p = prev.docs[uuid];
    if (!p) {
      continue;
    }
    const bits = [];
    if (p.lastOpenedPage !== d.lastOpenedPage)
      bits.push(`page ${String(p.lastOpenedPage)} -> ${String(d.lastOpenedPage)}`);
    if (p.lastOpened !== d.lastOpened)
      bits.push("opened");
    if (p.lastModified !== d.lastModified)
      bits.push("modified");
    if (p.bookmHash !== d.bookmHash)
      bits.push(`bookmarks ${p.bookmCount} -> ${d.bookmCount}`);
    if (p.highlightsSig !== d.highlightsSig)
      bits.push(`highlights ${p.highlightsCount} -> ${d.highlightsCount}`);
    if (p.rmLatestMtime !== d.rmLatestMtime)
      bits.push("handwriting changed");
    if (bits.length)
      changes.push({ uuid, name: d.name || uuid, type: d.type, lastModified: d.lastModified, bits });
  }
  changes.sort((a, b) => Number(b.lastModified || 0) - Number(a.lastModified || 0));
  return changes;
}
async function collectNotebookImages(sourceDir, changes) {
  const notebookChange = changes.find((c) => c.name.trim().toLowerCase() === "notebook" && c.bits.some((b) => b.toLowerCase().includes("handwriting changed")));
  if (!notebookChange)
    return [];
  const thumbsDir = import_node_path.default.join(sourceDir, `${notebookChange.uuid}.thumbnails`);
  if (!await exists(thumbsDir))
    return [];
  const items = await import_node_fs.promises.readdir(thumbsDir, { withFileTypes: true });
  const files = items.filter((x) => x.isFile() && /\.(png|jpg|jpeg|webp)$/i.test(x.name)).map((x) => x.name);
  const withTimes = [];
  for (const name of files) {
    const st = await import_node_fs.promises.stat(import_node_path.default.join(thumbsDir, name));
    withTimes.push({ name, mtime: st.mtimeMs });
  }
  withTimes.sort((a, b) => b.mtime - a.mtime);
  const selected = withTimes.slice(0, NOTEBOOK_IMAGE_LIMIT);
  const dataUrls = [];
  for (const f of selected) {
    const full = import_node_path.default.join(thumbsDir, f.name);
    const buf = await import_node_fs.promises.readFile(full);
    const ext = import_node_path.default.extname(f.name).toLowerCase();
    const mime = ext === ".png" ? "image/png" : ext === ".webp" ? "image/webp" : "image/jpeg";
    dataUrls.push(`data:${mime};base64,${buf.toString("base64")}`);
  }
  return dataUrls;
}
async function summarizeWithLLM(cli, changes) {
  const env = await loadEnv(cli.envFile);
  const key = process.env.OPENROUTER_API_KEY || env.OPENROUTER_API_KEY;
  if (!key) {
    return ["OPENROUTER_API_KEY missing; fallback summary:", ...changes.map((c) => `- ${c.name}: ${c.bits.join(", ")}`)].join(`
`);
  }
  const system = "You are a concise personal activity summarizer. Return accurate markdown only.";
  const notebookImages = await collectNotebookImages(cli.sourceDir, changes);
  const userText = `${cli.prompt}

Changes JSON:
${JSON.stringify(changes, null, 2)}

${notebookImages.length ? `Notebook context images attached: ${notebookImages.length} recent page previews from the Notebook document. Use them to infer what writing changed.` : ""}

Format:
## Activity Summary
## Reading
## Writing
## Highlights & Bookmarks
## Next Actions`;
  const userContent = notebookImages.length > 0 ? [
    { type: "text", text: userText },
    ...notebookImages.map((url) => ({ type: "image_url", image_url: { url } }))
  ] : userText;
  const res = await fetch("https://openrouter.ai/api/v1/chat/completions", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${key}`,
      "Content-Type": "application/json"
    },
    body: JSON.stringify({
      model: cli.model,
      temperature: 0.2,
      messages: [
        { role: "system", content: system },
        { role: "user", content: userContent }
      ]
    })
  });
  if (!res.ok) {
    const err = await res.text();
    return `LLM call failed (${res.status}).

${changes.map((c) => `- ${c.name}: ${c.bits.join(", ")}`).join(`
`)}

Error: ${err}`;
  }
  const json = await res.json();
  return json.choices?.[0]?.message?.content?.trim() || "(empty summary)";
}
function esc(s) {
  return s.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}
function humanTime(iso) {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime()))
    return iso;
  return d.toLocaleString("en-US", {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit"
  });
}
async function readHistory(historyPath) {
  if (!await exists(historyPath))
    return [];
  const txt = await import_node_fs.promises.readFile(historyPath, "utf8");
  const lines = txt.split(/\r?\n/).filter(Boolean);
  const parsed = [];
  for (let i = lines.length - 1;i >= 0; i--) {
    try {
      const row = JSON.parse(lines[i]);
      if (row && row.at && Array.isArray(row.changes))
        parsed.push(row);
    } catch {}
    if (parsed.length >= MAX_HISTORY_RUNS)
      break;
  }
  return parsed;
}
function renderHtml(summary, changes, history) {
  const nowHuman = humanTime(new Date().toISOString());
  const historyItems = history.map((h) => {
    const firstLine = (h.summary || "").split(`
`).find((x) => x.trim()) || "(no summary stored)";
    return `<li class="hist-item"><div class="hist-time">${esc(humanTime(h.at))}</div><div class="hist-meta">${h.shownChanges}/${h.totalChanges} changes</div><div class="hist-text">${esc(firstLine)}</div></li>`;
  }).join(`
`);
  return `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1.0"/>
<title>activity</title>
<link rel="preconnect" href="https://fonts.googleapis.com" />
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;600&display=swap" rel="stylesheet" />
<style>
*,*::before,*::after{margin:0;padding:0;box-sizing:border-box}
:root{--bg:rgb(15,17,21);--bg2:rgb(26,29,35);--border:rgba(107,114,128,.2);--text:rgb(201,204,209);--bright:rgb(229,231,235);--muted:rgb(107,114,128);--accent:rgb(245,158,11)}
body{background:var(--bg);color:var(--text);font-family:'JetBrains Mono',monospace;font-size:15px;line-height:1.6;min-height:100vh}
nav{padding:20px 24px;display:flex;justify-content:space-between;align-items:center}.brand{color:var(--bright);text-decoration:none;font-weight:500}.brand:hover{color:var(--accent)}
.layout{display:flex;min-height:calc(100vh - 70px)}
#sidebar{width:340px;max-width:80vw;border-right:1px solid var(--border);padding:16px 14px;overflow:auto;transition:width .2s ease,padding .2s ease}
#sidebar.collapsed{width:0;padding:0;border-right:none;overflow:hidden}
.side-header{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px}
.side-title{color:var(--bright);font-size:14px}
.toggle{background:var(--bg2);border:1px solid var(--border);color:var(--muted);border-radius:6px;padding:6px 10px;cursor:pointer;font-family:inherit;font-size:13px;line-height:1}
main .toggle{display:inline-flex;align-items:center;margin-bottom:16px}
.hist-list{list-style:none}.hist-item{padding:10px 2px;border-bottom:1px solid var(--border)}.hist-time{color:var(--bright);font-size:12px}.hist-meta{color:var(--muted);font-size:11px}.hist-text{color:var(--text);font-size:12px;margin-top:4px}
main{flex:1;max-width:860px;padding:24px 32px 56px}h1{color:var(--bright);font-size:22px;font-weight:500;margin-bottom:8px;clear:both}.meta{color:var(--muted);font-size:13px;margin-bottom:24px}
h2{color:var(--bright);font-size:18px;font-weight:500;margin:24px 0 12px}pre{background:var(--bg2);border:1px solid var(--border);border-radius:8px;padding:16px;white-space:pre-wrap;overflow:auto}
ul{list-style:none}
.item{padding:14px 0;border-bottom:1px solid var(--border)}.item:last-child{border-bottom:none}.title{color:var(--bright)}
code{background:var(--bg2);border:1px solid var(--border);padding:2px 6px;border-radius:4px;color:var(--muted);font-size:12px}.bits{margin-top:8px;padding-left:16px}.bits li{list-style:disc;color:var(--muted)}
@media (max-width: 900px){
  nav{padding:14px 18px}
  #sidebar{position:fixed;top:64px;left:0;bottom:0;background:var(--bg);z-index:10}
  main{padding:16px 18px 40px}
  main .toggle{display:block;width:auto;margin:0 0 20px;padding:8px 12px}
}
</style>
</head><body>
<nav><a class="brand" href="/">swair.dev</a></nav>
<div class="layout">
<aside id="sidebar" class="collapsed">
  <div class="side-header"><div class="side-title">previous summaries</div><button class="toggle" onclick="toggleSidebar()">hide</button></div>
  <ul class="hist-list">${historyItems || "<li class='hist-item'><div class='hist-text'>No previous summaries yet.</div></li>"}</ul>
</aside>
<main>
  <button class="toggle" onclick="toggleSidebar()" style="margin-bottom:12px">history</button>
  <h1>reMarkable activity</h1><p class="meta">updated ${esc(nowHuman)}</p>
  <h2>agent summary</h2><pre>${esc(summary)}</pre>
  <h2>detected changes (latest ${MAX_VISIBLE_CHANGES})</h2>
  <ul>${changes.map((c) => `<li class="item"><div class="title">${esc(c.name)} <code>${esc(c.uuid)}</code></div><ul class="bits">${c.bits.map((b) => `<li>${esc(b)}</li>`).join("")}</ul></li>`).join(`
`)}</ul>
</main>
</div>
<script>
function toggleSidebar(){document.getElementById('sidebar').classList.toggle('collapsed')}
</script>
</body></html>`;
}
async function main() {
  const cli = parseArgs(process.argv.slice(2));
  await import_node_fs.promises.mkdir(cli.stateDir, { recursive: true });
  const statePath = import_node_path.default.join(cli.stateDir, "last-state.json");
  const latestPath = import_node_path.default.join(cli.stateDir, "latest.md");
  const historyPath = import_node_path.default.join(cli.stateDir, "history.jsonl");
  const prev = await readJson(statePath);
  const cur = await buildState(cli.sourceDir);
  await import_node_fs.promises.writeFile(statePath, JSON.stringify(cur, null, 2), "utf8");
  if (!prev) {
    const msg = "Baseline created. Next run will produce diffs.";
    await import_node_fs.promises.writeFile(latestPath, msg + `
`, "utf8");
    console.log(msg);
    return;
  }
  const changes = diff(prev, cur);
  if (!changes.length) {
    console.log("No changes since last sync. Nothing published.");
    return;
  }
  const visibleChanges = changes.slice(0, MAX_VISIBLE_CHANGES);
  const summary = await summarizeWithLLM(cli, visibleChanges);
  await import_node_fs.promises.writeFile(latestPath, summary + `
`, "utf8");
  await import_node_fs.promises.appendFile(historyPath, JSON.stringify({
    at: new Date().toISOString(),
    totalChanges: changes.length,
    shownChanges: visibleChanges.length,
    summary,
    changes: visibleChanges
  }) + `
`, "utf8");
  const history = await readHistory(historyPath);
  await import_node_fs.promises.mkdir(import_node_path.default.dirname(cli.outputHtml), { recursive: true });
  await import_node_fs.promises.writeFile(cli.outputHtml, renderHtml(summary, visibleChanges, history), "utf8");
  console.log(`Published ${visibleChanges.length}/${changes.length} change(s) to ${cli.outputHtml}`);
}
main().catch((err) => {
  console.error(err instanceof Error ? err.message : String(err));
  process.exit(1);
});
