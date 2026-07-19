'use strict';

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

function readJson(file) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch (_) { return null; }
}

function mtimeMs(file) {
  try { return fs.statSync(file).mtimeMs; } catch (_) { return 0; }
}

function versionOf(files) {
  return Math.trunc(Math.max(0, ...files.map(mtimeMs))).toString(36);
}

function inkKey(entry) {
  if (entry && entry.p != null) return `pdf-${String(entry.p + 1).padStart(4, '0')}`;
  if (entry && entry.n != null) return `note-${String(entry.n).padStart(4, '0')}`;
  return null;
}

function readInkKeys(docDir) {
  try {
    return fs.readdirSync(path.join(docDir, 'ink'))
      .filter((name) => /^(?:pdf|note)-\d{4}\.json$/.test(name))
      .map((name) => name.slice(0, -5))
      .sort();
  } catch (_) {
    return [];
  }
}

function readSource(root, base, pending, coverEndpoint, sourcesDir, overlayRoot = null) {
  const docsDir = path.join(root, 'docs');
  let entries;
  try { entries = fs.readdirSync(docsDir, { withFileTypes: true }); } catch (_) { return []; }

  const docs = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const id = entry.name;
    const docDir = path.join(docsDir, id);
    const metaPath = path.join(docDir, 'meta.json');
    const statePath = path.join(docDir, 'state.json');
    const meta = readJson(metaPath);
    if (!meta) continue;

    const kind = meta.kind === 'notebook' ? 'notebook' : (meta.pages > 0 ? 'book' : null);
    if (!kind) continue;

    // The inbound overlay is the freshest truth for state (iPad page-adds,
    // pi note inserts) and holds new pages' ink until the tablet pulls.
    // Without this, a page added on the iPad VANISHES on reopen: its seq
    // entry only exists in the overlay's state.json.
    const overlayDir = overlayRoot ? path.join(overlayRoot, 'docs', id) : null;
    const overlayState = overlayDir ? path.join(overlayDir, 'state.json') : null;
    const effectiveStatePath = overlayState && fs.existsSync(overlayState) ? overlayState : statePath;
    const state = readJson(effectiveStatePath) || {};
    const seq = Array.isArray(state.seq) ? state.seq : [];
    const first = seq[0] || (kind === 'book' ? { p: 0 } : { n: 1 });
    const thumbPath = path.join(docDir, 'thumb.png');
    const pagePath = first.p != null
      ? path.join(docDir, 'pages', `${String(first.p + 1).padStart(4, '0')}.png`)
      : null;
    const coverFile = fs.existsSync(thumbPath) ? thumbPath : pagePath;
    const coverVersion = versionOf([coverFile]);
    const cover = coverFile && fs.existsSync(coverFile)
      ? `${coverEndpoint}?source=${pending ? 'inbound' : 'data'}&id=${encodeURIComponent(id)}&v=${coverVersion}`
      : null;
    const sourcePdf = sourcesDir ? path.join(sourcesDir, id + '.pdf') : null;
    const srcMtime = sourcePdf ? mtimeMs(sourcePdf) : 0;
    const inkDir = path.join(docDir, 'ink');
    const pagesDir = path.join(docDir, 'pages');
    const versionFiles = [docDir, metaPath, effectiveStatePath, thumbPath, inkDir, pagesDir];
    if (overlayDir) versionFiles.push(overlayDir, path.join(overlayDir, 'ink'));

    docs.push({
      id,
      base,
      pending,
      meta: { ...meta, kind },
      mtime: Math.max(mtimeMs(metaPath), mtimeMs(effectiveStatePath), mtimeMs(docDir)),
      version: versionOf(versionFiles),
      cover,
      coverVersion,
      seq,
      ink: [...new Set([...readInkKeys(docDir), ...(overlayDir ? readInkKeys(overlayDir) : [])])].sort(),
      hasSource: srcMtime > 0,
      srcVersion: srcMtime > 0 ? Math.trunc(srcMtime).toString(36) : null,
    });
  }
  return docs;
}

function buildLibrary({ mirror, inbound, sources = null, dataBase = '/papier/data/', inboundBase = '/papier/inbound/', coverEndpoint = '/papier/api/cover' }) {
  const mirrored = readSource(mirror, dataBase, false, coverEndpoint, sources, inbound);
  const pending = readSource(inbound, inboundBase, true, coverEndpoint, sources);
  const have = new Set(mirrored.map((doc) => doc.id));
  const docs = mirrored.concat(pending.filter((doc) => !have.has(doc.id)));

  const foldersFile = path.join(mirror, 'folders.json');
  const folderDoc = readJson(foldersFile);
  const folders = Array.isArray(folderDoc && folderDoc.folders) ? folderDoc.folders.slice() : [];
  for (const doc of docs) {
    if (doc.meta.folder && !folders.includes(doc.meta.folder)) folders.push(doc.meta.folder);
  }
  folders.sort();
  docs.sort((a, b) => b.mtime - a.mtime);

  const stable = JSON.stringify({ docs, folders });
  const generation = crypto.createHash('sha1').update(stable).digest('hex').slice(0, 16);
  return { v: 1, generation, docs, folders };
}

function serializedLibrary(options) {
  const library = buildLibrary(options);
  const body = Buffer.from(JSON.stringify(library));
  const etag = `"${crypto.createHash('sha1').update(body).digest('hex')}"`;
  return { library, body, etag };
}

module.exports = { buildLibrary, inkKey, serializedLibrary };
