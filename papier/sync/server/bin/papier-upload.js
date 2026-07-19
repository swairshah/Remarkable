// Papier inbound + margin-editor service (node, no deps), on 127.0.0.1:8093.
// nginx proxies /papier/upload and /papier/api/* here.
//
// Endpoints:
//   POST /upload            (X-Filename)  drop a PDF -> render a book bundle
//                                         into papier-inbound/docs/<slug>/ and
//                                         RETAIN the source PDF for editing.
//   GET  /source-status?id= whether a source PDF is on file + current/original
//                           margins (so the viewer shows the editor or an
//                           "attach PDF" prompt).
//   GET  /preview?id=&page= a RAW page render (edge-to-edge, no margins), for
//                           the browser's live box preview. Cached.
//   POST /attach   (X-Doc-Id) attach a source PDF to an existing book (lets
//                           desk-rendered books use the editor).
//   POST /render   {id,margins:[l,t,r,b]}  re-render with new margins and ship
//                           pages/text/meta to the tablet via inbound (NOT
//                           state.json/ink, so annotations survive).
//   GET  /source-pdf?id=&v= stream the retained source PDF (immutable when
//                           versioned) — the viewer renders it with PDF.js.
//   POST /compose  {instructions,title?}  agentic doc creation: a pi agent on
//                           this VM researches the instructions/links, writes
//                           a typeset markdown article, renders it with
//                           notes-md2pdf.sh, and the result enters the normal
//                           upload path (book bundle -> inbound -> tablet).
//   GET  /compose-status?job=  phase/progress of a compose job.
//
// Inbound is a SEPARATE dir from the mirror on purpose: the tablet's outbound
// rsync uses --delete, so writing straight into the mirror would be wiped on
// the next device push. Re-renders reuse the doc's id, so the tablet's pull
// updates the pages in place and keeps ink/state.
'use strict';
const http = require('http');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { execFile, spawn } = require('child_process');
const { URL } = require('url');
const { serializedLibrary } = require('./papier-library');
const { createPiSessions } = require('./papier-pi-sessions');

const BACKUP = process.env.PAPIER_BACKUP || '/home/exedev/remarkable-backup';
const INBOX = path.join(BACKUP, 'papier-inbound');
const INCOMING = path.join(INBOX, 'incoming');
const DOCS = path.join(INBOX, 'docs');                 // inbound bundles (delivery)
const MIRROR = path.join(BACKUP, 'papier', 'docs');    // tablet mirror (read meta)
const SOURCES = path.join(BACKUP, 'papier-sources');   // retained PDFs: <id>.pdf
const PREVIEWS = path.join(BACKUP, 'papier-previews'); // cached raw pages: <id>/<n>.png
const COVERS = path.join(BACKUP, 'papier-covers');     // small web covers, keyed by source+doc version
const COMPOSE = path.join(BACKUP, 'papier-compose');   // compose job workdirs
const DERIVED = path.join(BACKUP, 'papier-derived-pdf'); // PDFs built from bundles (no retained source)
const RENDER = process.env.PAPIER_RENDER || '/home/exedev/bin/papier-render.sh';
const MAKE_PDF_PY = '/home/exedev/bin/papier-make-pdf.py';
const COMPOSE_SH = process.env.PAPIER_COMPOSE || '/home/exedev/bin/papier-compose.sh';
const PREVIEW_PY = '/home/exedev/bin/papier-preview-page.py';
const PY = '/home/exedev/papier-venv/bin/python3';
const PORT = Number(process.env.PAPIER_PORT || 8093);
[INCOMING, DOCS, SOURCES, PREVIEWS, COVERS, COMPOSE, DERIVED].forEach((d) => fs.mkdirSync(d, { recursive: true }));

const piSessions = createPiSessions({ mirrorDocs: MIRROR, inboundDocs: DOCS });

const MAX_BYTES = 200 * 1024 * 1024;
const DEFAULT_CROP = [0, 0, 1, 1];   // whole page (fractions 0..1)

/* ---- helpers ---------------------------------------------------------- */
function slugify(name) {
  return name.toLowerCase().replace(/\.pdf$/i, '')
    .replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'doc';
}
function freeDir(slug) {
  let out = path.join(DOCS, slug), n = 1;
  while (fs.existsSync(out)) { n++; out = path.join(DOCS, `${slug}-${n}`); }
  return out;
}
// doc ids are slugs; this both validates and blocks path traversal
function safeId(id) { return typeof id === 'string' && /^[a-z0-9][a-z0-9_-]{0,100}$/.test(id) ? id : null; }
function sourcePdf(id) { return path.join(SOURCES, id + '.pdf'); }
function json(res, code, obj) { res.writeHead(code, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(obj)); }

function handleLibrary(req, res) {
  const started = process.hrtime.bigint();
  const result = serializedLibrary({
    mirror: path.join(BACKUP, 'papier'),
    inbound: path.join(BACKUP, 'papier-inbound'),
    sources: SOURCES,
  });
  const durationMs = Number(process.hrtime.bigint() - started) / 1e6;
  const headers = {
    'Cache-Control': 'no-cache',
    'ETag': result.etag,
    'X-Papier-Generation': result.library.generation,
    'Server-Timing': `library;dur=${durationMs.toFixed(1)}`,
  };
  if (req.headers['if-none-match'] === result.etag) {
    res.writeHead(304, headers); res.end(); return;
  }
  res.writeHead(200, { ...headers, 'Content-Type': 'application/json', 'Content-Length': result.body.length });
  res.end(result.body);
}

function handleCover(res, source, id, version) {
  id = safeId(id);
  if (!id || !/^(?:data|inbound)$/.test(source || '') || !/^[a-z0-9]+$/.test(version || '')) {
    res.writeHead(400); res.end('bad cover request'); return;
  }
  const root = source === 'data' ? path.join(BACKUP, 'papier') : path.join(BACKUP, 'papier-inbound');
  const docDir = path.join(root, 'docs', id);
  const thumb = path.join(docDir, 'thumb.png');
  let original = fs.existsSync(thumb) ? thumb : null;
  if (!original) {
    const state = (() => { try { return JSON.parse(fs.readFileSync(path.join(docDir, 'state.json'))); } catch (_) { return null; } })();
    const first = state && Array.isArray(state.seq) && state.seq[0];
    const page = first && first.p != null ? first.p + 1 : 1;
    const candidate = path.join(docDir, 'pages', String(page).padStart(4, '0') + '.png');
    if (fs.existsSync(candidate)) original = candidate;
  }
  if (!original) { res.writeHead(404); res.end('no cover source'); return; }

  const cached = path.join(COVERS, `${source}-${id}-${version}.webp`);
  const serve = (file, type) => fs.readFile(file, (err, data) => {
    if (err) { res.writeHead(500); res.end('cover read failed'); return; }
    res.writeHead(200, {
      'Content-Type': type,
      'Content-Length': data.length,
      'Cache-Control': 'private, max-age=31536000, immutable',
    });
    res.end(data);
  });
  if (fs.existsSync(cached)) return serve(cached, 'image/webp');
  execFile('convert', [original, '-resize', '280x373', '-strip', '-quality', '78', '-define', 'webp:method=6', cached],
    { timeout: 30000 }, (err) => err ? serve(original, 'image/png') : serve(cached, 'image/webp'));
}

// A doc's meta from wherever it currently lives (pending inbound, then mirror).
function readMeta(id) {
  for (const base of [path.join(DOCS, id), path.join(MIRROR, id)]) {
    try { return JSON.parse(fs.readFileSync(path.join(base, 'meta.json'))); } catch (_) {}
  }
  return null;
}
function copyDir(src, dst) {
  fs.mkdirSync(dst, { recursive: true });
  for (const f of fs.readdirSync(src)) fs.copyFileSync(path.join(src, f), path.join(dst, f));
}
function readBody(req, res, cb) {
  const chunks = []; let size = 0, aborted = false;
  req.on('data', (c) => {
    size += c.length;
    if (size > MAX_BYTES) { aborted = true; res.writeHead(413); res.end('file too large'); req.destroy(); return; }
    chunks.push(c);
  });
  req.on('end', () => { if (!aborted) cb(Buffer.concat(chunks)); });
}
// stamp orig_crop (once) so "reset to original" always knows the starting crop
function setOrigCrop(docDir) {
  try {
    const mp = path.join(docDir, 'meta.json');
    const meta = JSON.parse(fs.readFileSync(mp));
    if (!meta.orig_crop) {
      meta.orig_crop = meta.crop || DEFAULT_CROP;
      fs.writeFileSync(mp, JSON.stringify(meta));
    }
  } catch (_) {}
}

/* ---- papier-ios write-back (iPad ink -> inbound -> tablet pull) -------- */
// Every write lands in the INBOUND tree, never the mirror: the tablet's
// next pull rsyncs it into ~/.local/share/papier/docs/<id>/ (per-file
// last-writer-wins, the "editable later" hook from SYNC_PLAN.md), and the
// push after that folds it back into the mirror for every viewer.

function docExists(id) {
  return fs.existsSync(path.join(BACKUP, 'papier', 'docs', id, 'meta.json'))
      || fs.existsSync(path.join(DOCS, id, 'meta.json'));
}

function writeAtomically(dst, data) {
  const tmp = dst + '.tmp';
  fs.writeFileSync(tmp, data);
  fs.renameSync(tmp, dst);
}

// The freshest copy of a page ink file: the inbound overlay (iPad + pi
// writes) when present, else the tablet mirror.
function effectiveInkPath(id, file) {
  const overlay = path.join(DOCS, id, 'ink', file);
  if (fs.existsSync(overlay)) return overlay;
  return path.join(MIRROR, id, 'ink', file);
}

// GET /ink?id=&file= — the merged-truth read the iPad uses.
function handleInkRead(res, id, file) {
  id = safeId(id);
  if (!id || !/^(?:pdf|note)-\d{4}\.json$/.test(file || '')) return json(res, 400, { ok: false, error: 'bad request' });
  const p = effectiveInkPath(id, file);
  if (!fs.existsSync(p)) { res.writeHead(404); res.end('no ink'); return; }
  res.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-cache' });
  fs.createReadStream(p).pipe(res);
}

// POST /ink?id=<doc>&file=note-0001.json — body is the FULL page ink file
// (v/next_patch/next_stroke/strokes/patches, libreink-page schema). Papier
// heals foreign stroke ids on load, so the iPad may number strokes freely.
//
// Ownership split: the poster (iPad) is the authority on USER strokes; the
// SERVER is the authority on pi's patches (cloud pi may have drawn/erased
// since the client loaded the page), so the current file's patches replace
// the posted ones.
function handleInkWrite(req, res, id, file) {
  id = safeId(id);
  if (!id) return json(res, 400, { ok: false, error: 'bad id' });
  if (!/^(?:pdf|note)-\d{4}\.json$/.test(file || '')) return json(res, 400, { ok: false, error: 'bad ink filename' });
  if (!docExists(id)) return json(res, 404, { ok: false, error: 'unknown doc' });
  readBody(req, res, (buf) => {
    let page;
    try { page = JSON.parse(buf.toString('utf8')); } catch (_) { return json(res, 400, { ok: false, error: 'not JSON' }); }
    if (!page || page.v !== 1 || !Array.isArray(page.strokes) || !Array.isArray(page.patches))
      return json(res, 400, { ok: false, error: 'not a page ink file' });
    try {
      const current = JSON.parse(fs.readFileSync(effectiveInkPath(id, file), 'utf8'));
      if (current && Array.isArray(current.patches)) {
        page.patches = current.patches;
        page.next_patch = Math.max(page.next_patch || 1, current.next_patch || 1);
      }
    } catch (_) { /* no existing file — nothing to merge */ }
    try {
      const dir = path.join(DOCS, id, 'ink');
      fs.mkdirSync(dir, { recursive: true });
      writeAtomically(path.join(dir, file), JSON.stringify(page));
    } catch (e) { return json(res, 500, { ok: false, error: String(e) }); }
    console.log('ink write', id, file, `${page.strokes.length} strokes`);
    json(res, 200, { ok: true });
  });
}

// POST /state?id=<doc> — body is a full state.json (seq/pos/next_note).
// Used by the iPad only after it appends a note page to a document.
function handleStateWrite(req, res, id) {
  id = safeId(id);
  if (!id) return json(res, 400, { ok: false, error: 'bad id' });
  if (!docExists(id)) return json(res, 404, { ok: false, error: 'unknown doc' });
  readBody(req, res, (buf) => {
    let state;
    try { state = JSON.parse(buf.toString('utf8')); } catch (_) { return json(res, 400, { ok: false, error: 'not JSON' }); }
    if (!state || !Array.isArray(state.seq) || !state.seq.every((e) => e && (Number.isInteger(e.p) || Number.isInteger(e.n))))
      return json(res, 400, { ok: false, error: 'not a state file' });
    try {
      fs.mkdirSync(path.join(DOCS, id), { recursive: true });
      writeAtomically(path.join(DOCS, id, 'state.json'), JSON.stringify(state));
    } catch (e) { return json(res, 500, { ok: false, error: String(e) }); }
    console.log('state write', id, `${state.seq.length} entries`);
    json(res, 200, { ok: true });
  });
}

// POST /patch-erase?id=&file=&patch=N — the user rubbed out one of pi's
// patches (tablet parity: the user may erase ANY ink). Removes it from the
// effective ink file and writes the result to inbound.
function handlePatchErase(res, id, file, patchId) {
  id = safeId(id);
  const pid = Number(patchId);
  if (!id || !/^(?:pdf|note)-\d{4}\.json$/.test(file || '') || !Number.isInteger(pid))
    return json(res, 400, { ok: false, error: 'bad request' });
  let page;
  try { page = JSON.parse(fs.readFileSync(effectiveInkPath(id, file), 'utf8')); }
  catch (_) { return json(res, 404, { ok: false, error: 'no ink file' }); }
  const before = (page.patches || []).length;
  page.patches = (page.patches || []).filter((p) => p.id !== pid);
  if (page.patches.length === before) return json(res, 404, { ok: false, error: 'no such patch' });
  try {
    const dir = path.join(DOCS, id, 'ink');
    fs.mkdirSync(dir, { recursive: true });
    writeAtomically(path.join(dir, file), JSON.stringify(page));
  } catch (e) { return json(res, 500, { ok: false, error: String(e) }); }
  console.log('patch erase', id, file, '#' + pid);
  json(res, 200, { ok: true });
}

// POST /notebook — body {title} -> a fresh notebook bundle in inbound with
// one blank page. Fresh id (slug de-collided), so it never clobbers a doc.
function handleNotebookCreate(req, res) {
  readBody(req, res, (buf) => {
    let body;
    try { body = JSON.parse(buf.toString('utf8') || '{}'); } catch (_) { body = {}; }
    const title = (typeof body.title === 'string' && body.title.trim())
      ? body.title.trim().slice(0, 120) : 'Notebook (iPad)';
    const dir = freeDir(slugify(title) || 'notebook');
    const id = path.basename(dir);
    try {
      fs.mkdirSync(path.join(dir, 'ink'), { recursive: true });
      writeAtomically(path.join(dir, 'meta.json'), JSON.stringify({
        created: Math.floor(Date.now() / 1000), folder: '', kind: 'notebook', title, v: 1,
      }));
      writeAtomically(path.join(dir, 'state.json'), JSON.stringify({ next_note: 2, pos: 0, seq: [{ n: 1 }] }));
      writeAtomically(path.join(dir, 'ink', 'note-0001.json'), JSON.stringify({
        v: 1, next_patch: 1, next_stroke: 1, strokes: [], patches: [],
      }));
    } catch (e) { return json(res, 500, { ok: false, error: String(e) }); }
    console.log('created notebook', id, `"${title}"`);
    json(res, 200, { ok: true, id });
  });
}

/* ---- POST /upload ----------------------------------------------------- */
function handleUpload(req, res) {
  const name = String(req.headers['x-filename'] || 'upload.pdf').slice(0, 200);
  if (!/\.pdf$/i.test(name)) { res.writeHead(415); res.end('only PDF uploads are supported right now'); return; }
  readBody(req, res, (buf) => {
    const tmp = path.join(INCOMING, `${Date.now()}-${name.replace(/[^\w.\-]/g, '_')}`);
    fs.writeFileSync(tmp, buf);
    const out = freeDir(slugify(name));
    const id = path.basename(out);
    const title = name.replace(/\.pdf$/i, '');
    execFile(RENDER, [tmp, out, title, '0', '0', '1', '1'], { timeout: 180000 }, (err, so, se) => {
      if (err) { fs.unlink(tmp, () => {}); console.error('render failed:', se || err.message); res.writeHead(500); res.end('render failed: ' + (se || err.message)); return; }
      try { fs.copyFileSync(tmp, sourcePdf(id)); } catch (e) { console.error('source save failed', e); }
      fs.unlink(tmp, () => {});
      setOrigCrop(out);
      console.log('rendered', out, '(source retained)');
      json(res, 200, { ok: true, id });
    });
  });
}

/* ---- GET /source-status?id= ------------------------------------------ */
function handleSourceStatus(res, id) {
  if (!safeId(id)) return json(res, 400, { ok: false, error: 'bad id' });
  const meta = readMeta(id) || {};
  json(res, 200, {
    ok: true,
    hasSource: fs.existsSync(sourcePdf(id)),
    pages: meta.pages || 0,
    crop: meta.crop || DEFAULT_CROP,
    orig: meta.orig_crop || meta.crop || DEFAULT_CROP,
    title: meta.title || id,
  });
}

/* ---- GET /source-pdf?id=&v= ------------------------------------------ */
// One request for the whole document: the viewer renders it locally with
// PDF.js instead of fetching one full-page PNG per page. When ?v= (source
// mtime token) is present the response is immutable in the browser.
//
// Books WITHOUT a retained source (desk-rendered, pre-retention uploads)
// still get the full viewer: a PDF is DERIVED from the bundle itself
// (pages/*.png + an invisible text layer from text/*.json word boxes,
// so search/selection work), cached in DERIVED keyed by doc version.
const derivedBuilds = new Map();   // id -> Promise<pdfPath>, de-dupes concurrent requests

function docDirOf(id) {
  for (const base of [path.join(DOCS, id), path.join(MIRROR, id)]) {
    if (fs.existsSync(path.join(base, 'meta.json'))) return base;
  }
  return null;
}
function docVersionToken(docDir) {
  const mt = (f) => { try { return fs.statSync(f).mtimeMs; } catch (_) { return 0; } };
  const v = Math.max(mt(path.join(docDir, 'meta.json')), mt(path.join(docDir, 'pages')), mt(docDir));
  return Math.trunc(v).toString(36);
}
function buildDerivedPdf(id, docDir) {
  if (derivedBuilds.has(id)) return derivedBuilds.get(id);
  const out = path.join(DERIVED, `${id}-${docVersionToken(docDir)}.pdf`);
  if (fs.existsSync(out)) return Promise.resolve(out);
  const meta = readMeta(id) || {};
  const p = new Promise((resolve, reject) => {
    const tmp = out + '.tmp';
    execFile(PY, [MAKE_PDF_PY, docDir, tmp, meta.title || id], { timeout: 300000 }, (err, so, se) => {
      if (err) { fs.rm(tmp, { force: true }, () => {}); reject(new Error(String(se || err.message).slice(0, 400))); return; }
      // stale cache housekeeping: older versions of this doc
      for (const f of fs.readdirSync(DERIVED)) {
        if (f.startsWith(id + '-') && f.endsWith('.pdf') && path.join(DERIVED, f) !== out) fs.rm(path.join(DERIVED, f), { force: true }, () => {});
      }
      fs.renameSync(tmp, out);
      resolve(out);
    });
  });
  derivedBuilds.set(id, p);
  // .finally() derives a NEW promise; without the .catch a failed build
  // becomes an unhandled rejection and kills the whole service.
  p.finally(() => derivedBuilds.delete(id)).catch(() => {});
  return p;
}
function streamPdf(req, res, file, immutable) {
  fs.stat(file, (err, st) => {
    if (err) { res.writeHead(404); res.end('no pdf'); return; }
    const headers = {
      'Content-Type': 'application/pdf',
      'Content-Length': st.size,
      'Cache-Control': immutable ? 'private, max-age=31536000, immutable' : 'no-cache',
    };
    if (req.method === 'HEAD') { res.writeHead(200, headers); res.end(); return; }
    res.writeHead(200, headers);
    fs.createReadStream(file).pipe(res);
  });
}
function handleSourcePdf(req, res, id, v) {
  if (!safeId(id)) { res.writeHead(400); res.end('bad id'); return; }
  const immutable = /^[a-z0-9]+$/.test(v || '');
  const src = sourcePdf(id);
  if (fs.existsSync(src)) return streamPdf(req, res, src, immutable);
  const docDir = docDirOf(id);
  if (!docDir) { res.writeHead(404); res.end('unknown doc'); return; }
  buildDerivedPdf(id, docDir)
    .then((file) => streamPdf(req, res, file, immutable))
    .catch((err) => { console.error('derived pdf failed for', id, err.message); res.writeHead(500); res.end('pdf build failed'); });
}

/* ---- GET /preview?id=&page= (raw page, cached) ----------------------- */
function handlePreview(res, id, page) {
  if (!safeId(id) || !(page >= 0)) { res.writeHead(400); res.end('bad request'); return; }
  const src = sourcePdf(id);
  if (!fs.existsSync(src)) { res.writeHead(404); res.end('no source pdf'); return; }
  const dir = path.join(PREVIEWS, id);
  const out = path.join(dir, page + '.png');
  const serve = () => fs.readFile(out, (e, data) => {
    if (e) { res.writeHead(500); res.end('preview read failed'); return; }
    res.writeHead(200, { 'Content-Type': 'image/png', 'Cache-Control': 'no-cache' });
    res.end(data);
  });
  if (fs.existsSync(out)) return serve();
  fs.mkdirSync(dir, { recursive: true });
  execFile(PY, [PREVIEW_PY, src, String(page), out], { timeout: 60000 }, (err, so, se) => {
    if (err) { console.error('preview render failed', se || err.message); res.writeHead(500); res.end('preview render failed'); return; }
    serve();
  });
}

/* ---- POST /attach (X-Doc-Id) ----------------------------------------- */
function handleAttach(req, res) {
  const id = safeId(req.headers['x-doc-id']);
  if (!id) return json(res, 400, { ok: false, error: 'bad or missing X-Doc-Id' });
  if (!readMeta(id)) return json(res, 404, { ok: false, error: 'unknown doc' });
  readBody(req, res, (buf) => {
    if (buf.slice(0, 5).toString('latin1') !== '%PDF-') return json(res, 415, { ok: false, error: 'not a PDF' });
    try {
      fs.writeFileSync(sourcePdf(id), buf);
      fs.rmSync(path.join(PREVIEWS, id), { recursive: true, force: true }); // the raw pages changed
    } catch (e) { return json(res, 500, { ok: false, error: String(e) }); }
    console.log('attached source pdf for', id);
    json(res, 200, { ok: true });
  });
}

/* ---- full-book crop rendering ---------------------------------------- */
function renderSpec(body) {
  const id = safeId(body && body.id);
  const crop = body && body.crop;
  if (!id) return { status: 400, error: 'bad id' };
  if (!Array.isArray(crop) || crop.length !== 4 || !crop.every((v) => Number.isFinite(v) && v >= 0 && v <= 1)
      || crop[2] - crop[0] < 0.05 || crop[3] - crop[1] < 0.05)
    return { status: 400, error: 'crop must be 4 fractions 0..1 (min 5% each way)' };
  const src = sourcePdf(id);
  if (!fs.existsSync(src)) return { status: 404, error: 'no source PDF — attach one first' };
  const existing = readMeta(id) || {};
  return {
    id, crop, src, existing,
    title: existing.title || id,
    orig: existing.orig_crop || existing.crop || DEFAULT_CROP,
    total: existing.pages || 0,
  };
}

function deliverRender(spec, tmpOut) {
  const dest = path.join(DOCS, spec.id);
  fs.mkdirSync(dest, { recursive: true });
  const rendered = JSON.parse(fs.readFileSync(path.join(tmpOut, 'meta.json')));
  const meta = { ...spec.existing, pages: rendered.pages, w: rendered.w, h: rendered.h,
                 crop: rendered.crop, orig_crop: spec.orig };
  delete meta.margins;
  fs.writeFileSync(path.join(dest, 'meta.json'), JSON.stringify(meta));
  copyDir(path.join(tmpOut, 'pages'), path.join(dest, 'pages'));
  copyDir(path.join(tmpOut, 'text'), path.join(dest, 'text'));
  if (fs.existsSync(path.join(tmpOut, 'thumb.png')))
    fs.copyFileSync(path.join(tmpOut, 'thumb.png'), path.join(dest, 'thumb.png'));
  if (!fs.existsSync(path.join(dest, 'state.json')) && fs.existsSync(path.join(tmpOut, 'state.json')))
    fs.copyFileSync(path.join(tmpOut, 'state.json'), path.join(dest, 'state.json'));
  fs.rmSync(tmpOut, { recursive: true, force: true });
  return rendered.pages;
}

function runRender(spec, onProgress = () => {}) {
  const ca = spec.crop.map(String);
  const tmpOut = path.join(INCOMING, `render-${spec.id}-${Date.now()}`);
  return new Promise((resolve, reject) => {
    let stderr = '', settled = false;
    const child = spawn(RENDER, [spec.src, tmpOut, spec.title, ca[0], ca[1], ca[2], ca[3]]);
    child.stdout.resume();
    const timer = setTimeout(() => { child.kill('SIGKILL'); }, 240000);
    child.stderr.on('data', (chunk) => {
      stderr = (stderr + chunk.toString()).slice(-16000);
      for (const match of stderr.matchAll(/mkbook: (\d+)\/(\d+) pages/g)) onProgress(Number(match[1]), Number(match[2]));
    });
    child.on('error', (err) => {
      if (settled) return; settled = true; clearTimeout(timer);
      fs.rmSync(tmpOut, { recursive: true, force: true }); reject(err);
    });
    child.on('close', (code, signal) => {
      if (settled) return; settled = true; clearTimeout(timer);
      if (code !== 0) {
        fs.rmSync(tmpOut, { recursive: true, force: true });
        reject(new Error(stderr || `render exited ${code == null ? signal : code}`)); return;
      }
      try {
        const pages = deliverRender(spec, tmpOut);
        console.log('re-rendered', spec.id, 'crop', ca.join(','), '-> inbound');
        resolve({ pages });
      } catch (err) {
        fs.rmSync(tmpOut, { recursive: true, force: true }); reject(err);
      }
    });
  });
}

function readRenderSpec(req, res, cb) {
  readBody(req, res, (buf) => {
    let body; try { body = JSON.parse(buf.toString('utf8')); } catch (_) { json(res, 400, { ok: false, error: 'bad json' }); return; }
    const spec = renderSpec(body);
    if (spec.error) { json(res, spec.status, { ok: false, error: spec.error }); return; }
    cb(spec);
  });
}

// Compatibility endpoint: waits for the full render before responding.
function handleRender(req, res) {
  readRenderSpec(req, res, (spec) => runRender(spec)
    .then(() => json(res, 200, { ok: true, crop: spec.crop }))
    .catch((err) => { console.error('re-render failed', err); json(res, 500, { ok: false, error: String(err.message || err) }); }));
}

const renderJobs = new Map();
function pruneRenderJobs() {
  const cutoff = Date.now() - 3600000;
  for (const [id, job] of renderJobs) if (job.updated < cutoff) renderJobs.delete(id);
}
function handleRenderJob(req, res) {
  readRenderSpec(req, res, (spec) => {
    pruneRenderJobs();
    const id = crypto.randomBytes(10).toString('hex');
    const job = { id, ok: true, status: 'queued', page: 0, total: spec.total, updated: Date.now() };
    renderJobs.set(id, job);
    json(res, 202, { ok: true, job: id });
    setImmediate(() => {
      Object.assign(job, { status: 'rendering', updated: Date.now() });
      runRender(spec, (page, total) => Object.assign(job, { page, total, updated: Date.now() }))
        .then(({ pages }) => Object.assign(job, { status: 'done', page: pages, total: pages, crop: spec.crop, updated: Date.now() }))
        .catch((err) => { console.error('render job failed', err); Object.assign(job, { status: 'failed', error: String(err.message || err), updated: Date.now() }); });
    });
  });
}
function handleRenderStatus(res, id) {
  if (!/^[a-f0-9]{20}$/.test(id || '') || !renderJobs.has(id)) return json(res, 404, { ok: false, error: 'unknown render job' });
  json(res, 200, renderJobs.get(id));
}

// Pre-warm derived PDFs (serially, low priority) so the first "PDF viewer"
// click on a big desk-rendered book doesn't wait minutes on the build — a
// 383-page book takes ~3 minutes. Runs at startup and every 6h to cover
// books that arrive via sync.
async function warmDerivedPdfs() {
  const ids = new Set();
  for (const root of [MIRROR, DOCS]) {
    try { for (const e of fs.readdirSync(root)) ids.add(e); } catch (_) {}
  }
  for (const id of ids) {
    if (!safeId(id) || fs.existsSync(sourcePdf(id))) continue;
    const docDir = docDirOf(id);
    if (!docDir) continue;
    const meta = readMeta(id);
    if (!meta || !(meta.pages > 0)) continue;
    if (fs.existsSync(path.join(DERIVED, `${id}-${docVersionToken(docDir)}.pdf`))) continue;
    try { await buildDerivedPdf(id, docDir); console.log('pre-warmed derived pdf for', id); }
    catch (err) { console.error('derived pre-warm failed for', id, err.message); }
  }
}
setTimeout(warmDerivedPdfs, 15000);
setInterval(warmDerivedPdfs, 6 * 3600000);

/* ---- agentic compose --------------------------------------------------- */
// POST /compose {instructions, title?} -> {ok, job}. A detached-ish pipeline:
//   1. job dir under papier-compose/<job>/ with instructions.md
//   2. papier-compose.sh runs a headless pi agent (research + write + render
//      via notes-md2pdf.sh) -> <job>/out/article.pdf + <job>/title.txt,
//      updating <job>/status.txt as it goes
//   3. on success the PDF takes the exact upload path: book bundle into
//      inbound docs/, source retained for the crop editor + PDF.js viewer.
// Result is also persisted to <job>/result.json so status survives restarts.
const composeJobs = new Map();
const COMPOSE_TIMEOUT = 40 * 60000;

function composeDir(id) { return path.join(COMPOSE, id); }
function readComposePhase(id) {
  try { return fs.readFileSync(path.join(composeDir(id), 'status.txt'), 'utf8').trim().slice(0, 200); } catch (_) { return null; }
}
function persistComposeResult(id, job) {
  try { fs.writeFileSync(path.join(composeDir(id), 'result.json'), JSON.stringify(job)); } catch (_) {}
}

function finishCompose(job, dir) {
  const pdf = path.join(dir, 'out', 'article.pdf');
  if (!fs.existsSync(pdf)) {
    Object.assign(job, { status: 'failed', error: 'agent produced no PDF', updated: Date.now() });
    persistComposeResult(job.id, job); return;
  }
  let title = job.title || '';
  try { title = (fs.readFileSync(path.join(dir, 'title.txt'), 'utf8').trim() || title); } catch (_) {}
  title = title || 'Composed document';
  const out = freeDir(slugify(title + '.pdf'));
  const docId = path.basename(out);
  Object.assign(job, { status: 'rendering', phase: 'rendering pages for the tablet', updated: Date.now() });
  execFile(RENDER, [pdf, out, title, '0', '0', '1', '1'], { timeout: 240000 }, (err, so, se) => {
    if (err) {
      Object.assign(job, { status: 'failed', error: 'book render failed: ' + String(se || err.message).slice(0, 500), updated: Date.now() });
      persistComposeResult(job.id, job); return;
    }
    try { fs.copyFileSync(pdf, sourcePdf(docId)); } catch (e) { console.error('compose source save failed', e); }
    setOrigCrop(out);
    console.log('composed', docId, '->', out);
    Object.assign(job, { status: 'done', docId, title, updated: Date.now() });
    persistComposeResult(job.id, job);
  });
}

function handleCompose(req, res) {
  readBody(req, res, (buf) => {
    let body; try { body = JSON.parse(buf.toString('utf8')); } catch (_) { return json(res, 400, { ok: false, error: 'bad json' }); }
    const instructions = String(body && body.instructions || '').trim();
    const title = String(body && body.title || '').trim().slice(0, 160);
    if (!instructions) return json(res, 400, { ok: false, error: 'instructions are required' });
    if (instructions.length > 100000) return json(res, 413, { ok: false, error: 'instructions too long' });

    const id = crypto.randomBytes(8).toString('hex');
    const dir = composeDir(id);
    fs.mkdirSync(path.join(dir, 'work'), { recursive: true });
    fs.mkdirSync(path.join(dir, 'out'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'instructions.md'),
      (title ? `Preferred title: ${title}\n\n` : '') + instructions + '\n');
    fs.writeFileSync(path.join(dir, 'status.txt'), 'starting');

    const job = { id, ok: true, status: 'running', phase: 'starting', title, updated: Date.now() };
    composeJobs.set(id, job);
    json(res, 202, { ok: true, job: id });

    // Run a per-job COPY of the script: bash reads scripts from disk
    // incrementally, so a deploy overwriting the shared one mid-run would
    // corrupt a job that is hours into its work (learned the hard way).
    const script = path.join(dir, 'compose.sh');
    try { fs.copyFileSync(COMPOSE_SH, script); fs.chmodSync(script, 0o755); } catch (err) {
      Object.assign(job, { status: 'failed', error: 'compose script unavailable: ' + err.message, updated: Date.now() });
      persistComposeResult(id, job); return;
    }
    const child = spawn(script, [dir], { stdio: ['ignore', 'pipe', 'pipe'] });
    let tail = '';
    child.stdout.resume();
    child.stderr.on('data', (c) => { tail = (tail + c.toString()).slice(-8000); });
    const timer = setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, COMPOSE_TIMEOUT);
    child.on('error', (err) => {
      clearTimeout(timer);
      Object.assign(job, { status: 'failed', error: String(err.message || err), updated: Date.now() });
      persistComposeResult(id, job);
    });
    child.on('close', (code, signal) => {
      clearTimeout(timer);
      if (job.status === 'failed') return;
      if (code !== 0) {
        const reason = signal === 'SIGKILL' ? 'timed out' : `agent exited ${code == null ? signal : code}`;
        console.error('compose failed:', reason, tail.slice(-1000));
        Object.assign(job, { status: 'failed', error: reason, updated: Date.now() });
        persistComposeResult(id, job); return;
      }
      finishCompose(job, dir);
    });
  });
}

function handleComposeStatus(res, id) {
  if (!/^[a-f0-9]{16}$/.test(id || '')) return json(res, 400, { ok: false, error: 'bad job id' });
  let job = composeJobs.get(id);
  if (!job) {   // service restarted mid-job or long after: fall back to disk
    try { job = JSON.parse(fs.readFileSync(path.join(composeDir(id), 'result.json'), 'utf8')); } catch (_) {}
    if (!job) {
      if (fs.existsSync(composeDir(id))) return json(res, 200, { ok: true, id, status: 'failed', error: 'service restarted during compose' });
      return json(res, 404, { ok: false, error: 'unknown compose job' });
    }
  }
  const phase = job.status === 'running' ? (readComposePhase(id) || job.phase) : job.phase;
  json(res, 200, { ...job, phase });
}

/* ---- router ----------------------------------------------------------- */
http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  const p = u.pathname;
  if (req.method === 'GET' && p === '/health') { res.writeHead(200); res.end('ok'); return; }
  if (req.method === 'GET' && p === '/library') return handleLibrary(req, res);
  if (req.method === 'GET' && p === '/cover') return handleCover(res, u.searchParams.get('source'), u.searchParams.get('id'), u.searchParams.get('v'));
  if (req.method === 'GET' && p === '/source-status') return handleSourceStatus(res, u.searchParams.get('id'));
  if (req.method === 'GET' && p === '/preview') return handlePreview(res, u.searchParams.get('id'), parseInt(u.searchParams.get('page'), 10));
  if ((req.method === 'GET' || req.method === 'HEAD') && p === '/source-pdf') return handleSourcePdf(req, res, u.searchParams.get('id'), u.searchParams.get('v'));
  if (req.method === 'POST' && p === '/compose') return handleCompose(req, res);
  if (req.method === 'GET' && p === '/compose-status') return handleComposeStatus(res, u.searchParams.get('job'));
  if (piSessions.handle(req, res, p, u)) return;
  if (req.method === 'GET' && p === '/ink') return handleInkRead(res, u.searchParams.get('id'), u.searchParams.get('file'));
  if (req.method === 'POST' && p === '/ink') return handleInkWrite(req, res, u.searchParams.get('id'), u.searchParams.get('file'));
  if (req.method === 'POST' && p === '/state') return handleStateWrite(req, res, u.searchParams.get('id'));
  if (req.method === 'POST' && p === '/notebook') return handleNotebookCreate(req, res);
  if (req.method === 'POST' && p === '/patch-erase') return handlePatchErase(res, u.searchParams.get('id'), u.searchParams.get('file'), u.searchParams.get('patch'));
  if (req.method === 'POST' && p === '/upload') return handleUpload(req, res);
  if (req.method === 'POST' && p === '/attach') return handleAttach(req, res);
  if (req.method === 'POST' && p === '/render') return handleRender(req, res);
  if (req.method === 'POST' && p === '/render-job') return handleRenderJob(req, res);
  if (req.method === 'GET' && p === '/render-status') return handleRenderStatus(res, u.searchParams.get('job'));
  res.writeHead(404); res.end('not found');
}).listen(PORT, '127.0.0.1', () => console.log(`papier upload/editor service on 127.0.0.1:${PORT}`));
