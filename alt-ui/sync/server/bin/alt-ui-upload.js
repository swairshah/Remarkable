// Paper inbound + margin-editor service (node, no deps), on 127.0.0.1:8093.
// nginx proxies /paper/upload and /paper/api/* here.
//
// Endpoints:
//   POST /upload            (X-Filename)  drop a PDF -> render a book bundle
//                                         into alt-ui-inbound/docs/<slug>/ and
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
//
// Inbound is a SEPARATE dir from the mirror on purpose: the tablet's outbound
// rsync uses --delete, so writing straight into the mirror would be wiped on
// the next device push. Re-renders reuse the doc's id, so the tablet's pull
// updates the pages in place and keeps ink/state.
'use strict';
const http = require('http');
const fs = require('fs');
const path = require('path');
const { execFile } = require('child_process');
const { URL } = require('url');

const BACKUP = '/home/exedev/remarkable-backup';
const INBOX = path.join(BACKUP, 'alt-ui-inbound');
const INCOMING = path.join(INBOX, 'incoming');
const DOCS = path.join(INBOX, 'docs');                 // inbound bundles (delivery)
const MIRROR = path.join(BACKUP, 'alt-ui', 'docs');    // tablet mirror (read meta)
const SOURCES = path.join(BACKUP, 'alt-ui-sources');   // retained PDFs: <id>.pdf
const PREVIEWS = path.join(BACKUP, 'alt-ui-previews'); // cached raw pages: <id>/<n>.png
const RENDER = '/home/exedev/bin/alt-ui-render.sh';
const PREVIEW_PY = '/home/exedev/bin/alt-ui-preview-page.py';
const PY = '/home/exedev/alt-ui-venv/bin/python3';
[INCOMING, DOCS, SOURCES, PREVIEWS].forEach((d) => fs.mkdirSync(d, { recursive: true }));

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

/* ---- POST /render {id, margins:[l,t,r,b]} ---------------------------- */
function handleRender(req, res) {
  readBody(req, res, (buf) => {
    let body; try { body = JSON.parse(buf.toString('utf8')); } catch (_) { return json(res, 400, { ok: false, error: 'bad json' }); }
    const id = safeId(body.id);
    const c = body.crop;
    if (!id) return json(res, 400, { ok: false, error: 'bad id' });
    if (!Array.isArray(c) || c.length !== 4 || !c.every((v) => Number.isFinite(v) && v >= 0 && v <= 1)
        || c[2] - c[0] < 0.05 || c[3] - c[1] < 0.05)
      return json(res, 400, { ok: false, error: 'crop must be 4 fractions 0..1 (min 5% each way)' });
    const src = sourcePdf(id);
    if (!fs.existsSync(src)) return json(res, 404, { ok: false, error: 'no source PDF — attach one first' });

    const existing = readMeta(id) || {};
    const title = existing.title || id;
    const orig = existing.orig_crop || existing.crop || DEFAULT_CROP;
    const ca = c.map((v) => String(v));
    const tmpOut = path.join(INCOMING, `render-${id}-${Date.now()}`);
    execFile(RENDER, [src, tmpOut, title, ca[0], ca[1], ca[2], ca[3]], { timeout: 240000 }, (err, so, se) => {
      if (err) { fs.rmSync(tmpOut, { recursive: true, force: true }); console.error('re-render failed', se || err.message); return json(res, 500, { ok: false, error: String(se || err.message) }); }
      try {
        const dest = path.join(DOCS, id);
        fs.mkdirSync(dest, { recursive: true });
        const rendered = JSON.parse(fs.readFileSync(path.join(tmpOut, 'meta.json')));
        // start from the doc's live meta so title/folder/kind the tablet set
        // survive; only overlay the render outputs + crop.
        const meta = { ...existing, pages: rendered.pages, w: rendered.w, h: rendered.h,
                       crop: rendered.crop, orig_crop: orig };
        delete meta.margins;
        fs.writeFileSync(path.join(dest, 'meta.json'), JSON.stringify(meta));
        copyDir(path.join(tmpOut, 'pages'), path.join(dest, 'pages'));
        copyDir(path.join(tmpOut, 'text'), path.join(dest, 'text'));
        // a doc not yet on the tablet (pending) needs a state.json; one already
        // on the tablet keeps its own (don't clobber reading position / notes).
        if (!fs.existsSync(path.join(dest, 'state.json')) && fs.existsSync(path.join(tmpOut, 'state.json')))
          fs.copyFileSync(path.join(tmpOut, 'state.json'), path.join(dest, 'state.json'));
        fs.rmSync(tmpOut, { recursive: true, force: true });
      } catch (e) { fs.rmSync(tmpOut, { recursive: true, force: true }); console.error('deliver failed', e); return json(res, 500, { ok: false, error: String(e) }); }
      console.log('re-rendered', id, 'crop', ca.join(','), '-> inbound');
      json(res, 200, { ok: true, crop: c });
    });
  });
}

/* ---- router ----------------------------------------------------------- */
http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  const p = u.pathname;
  if (req.method === 'GET' && p === '/health') { res.writeHead(200); res.end('ok'); return; }
  if (req.method === 'GET' && p === '/source-status') return handleSourceStatus(res, u.searchParams.get('id'));
  if (req.method === 'GET' && p === '/preview') return handlePreview(res, u.searchParams.get('id'), parseInt(u.searchParams.get('page'), 10));
  if (req.method === 'POST' && p === '/upload') return handleUpload(req, res);
  if (req.method === 'POST' && p === '/attach') return handleAttach(req, res);
  if (req.method === 'POST' && p === '/render') return handleRender(req, res);
  res.writeHead(404); res.end('not found');
}).listen(8093, '127.0.0.1', () => console.log('paper upload/editor service on 127.0.0.1:8093'));
