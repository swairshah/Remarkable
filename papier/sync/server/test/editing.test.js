'use strict';

// The web editor's document-level writes: rename/move, page insert, delete.
// Like the ink write-back, every one of them lands in the INBOUND tree — the
// mirror belongs to the tablet, whose push runs --delete. A delete therefore
// leaves a TOMBSTONE the tablet consumes on its next pull.

const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const { buildLibrary } = require('../bin/papier-library');

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value));
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => server.listen(0, '127.0.0.1', resolve).once('error', reject));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function startService(t, backup) {
  const port = await freePort();
  const service = spawn(process.execPath, [path.resolve(__dirname, '../bin/papier-upload.js')], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_PORT: String(port) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  t.after(() => service.kill('SIGKILL'));
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('service start timeout')), 3000);
    service.once('exit', (code) => reject(new Error('service exited ' + code)));
    service.stdout.on('data', (chunk) => {
      if (chunk.toString().includes(`127.0.0.1:${port}`)) { clearTimeout(timer); resolve(); }
    });
  });
  return `http://127.0.0.1:${port}`;
}

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-editing-'));
  const backup = path.join(root, 'backup');
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'meta.json'),
    { created: 1, folder: '', kind: 'notebook', title: 'NB', v: 1 });
  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'state.json'), { next_note: 3, pos: 0, seq: [{ n: 1 }, { n: 2 }] });
  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'ink', 'note-0001.json'),
    { v: 1, next_patch: 1, next_stroke: 1, strokes: [], patches: [] });
  return { root, backup, inbound: path.join(backup, 'papier-inbound') };
}

const post = (base, path_, body) => fetch(base + path_, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: body === undefined ? undefined : JSON.stringify(body),
});

test('doc-meta renames/re-files into the overlay and validates', async (t) => {
  const f = fixture(t);
  const base = await startService(t, f.backup);

  let r = await post(base, '/doc-meta?id=nb', { title: 'Field Notes', folder: 'Research' });
  assert.equal(r.status, 200);
  const meta = JSON.parse(fs.readFileSync(path.join(f.inbound, 'docs', 'nb', 'meta.json')));
  assert.equal(meta.title, 'Field Notes');
  assert.equal(meta.folder, 'Research');
  assert.equal(meta.kind, 'notebook');          // untouched fields survive
  assert.equal(meta.created, 1);
  // the mirror is left alone; the tablet adopts the overlay copy on its pull
  assert.equal(JSON.parse(fs.readFileSync(path.join(f.backup, 'papier', 'docs', 'nb', 'meta.json'))).title, 'NB');

  // a later folder-only edit builds on the overlay copy, not the mirror
  r = await post(base, '/doc-meta?id=nb', { folder: '' });
  assert.equal(r.status, 200);
  const back = JSON.parse(fs.readFileSync(path.join(f.inbound, 'docs', 'nb', 'meta.json')));
  assert.equal(back.title, 'Field Notes');
  assert.equal(back.folder, '');

  assert.equal((await post(base, '/doc-meta?id=nb', { title: '   ' })).status, 400);
  assert.equal((await post(base, '/doc-meta?id=nb', { folder: 'a/b' })).status, 400);
  assert.equal((await post(base, '/doc-meta?id=nope', { title: 'x' })).status, 404);
  assert.equal((await post(base, '/doc-meta?id=../evil', { title: 'x' })).status, 400);
});

test('page-add inserts a note page and writes state + blank ink', async (t) => {
  const f = fixture(t);
  const base = await startService(t, f.backup);

  let r = await post(base, '/page-add?id=nb', { after: 0 });
  assert.equal(r.status, 200);
  let body = await r.json();
  assert.equal(body.index, 1);
  assert.equal(body.key, 'note-0003');
  assert.deepEqual(body.state.seq, [{ n: 1 }, { n: 3 }, { n: 2 }]);
  assert.equal(body.state.next_note, 4);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(f.inbound, 'docs', 'nb', 'state.json'))).seq, body.state.seq);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(f.inbound, 'docs', 'nb', 'ink', 'note-0003.json'))).strokes, []);

  // no `after` appends; the overlay state (not the stale mirror) is the base
  r = await post(base, '/page-add?id=nb', {});
  body = await r.json();
  assert.deepEqual(body.state.seq, [{ n: 1 }, { n: 3 }, { n: 2 }, { n: 4 }]);

  // the new pages are visible to the library right away
  const doc = buildLibrary({ mirror: path.join(f.backup, 'papier'), inbound: f.inbound })
    .docs.find((d) => d.id === 'nb');
  assert.equal(doc.seq.length, 4);
  assert.ok(doc.inkPending.includes('note-0003'));

  assert.equal((await post(base, '/page-add?id=nope', {})).status, 404);
});

test('doc-delete tombstones for the tablet and clears the overlay + derivatives', async (t) => {
  const f = fixture(t);
  const base = await startService(t, f.backup);

  // a web-drawn page and a retained source PDF, as a real doc would have
  await post(base, '/page-add?id=nb', {});
  fs.writeFileSync(path.join(f.backup, 'papier-sources', 'nb.pdf'), '%PDF-fake');
  fs.mkdirSync(path.join(f.backup, 'papier-previews', 'nb'), { recursive: true });
  fs.writeFileSync(path.join(f.backup, 'papier-previews', 'nb', '0.png'), 'png');
  fs.writeFileSync(path.join(f.backup, 'papier-covers', 'data-nb-abc.webp'), 'webp');
  fs.writeFileSync(path.join(f.backup, 'papier-derived-pdf', 'nb-abc.pdf'), '%PDF-fake');

  const r = await post(base, '/doc-delete?id=nb');
  assert.equal(r.status, 200);
  const tomb = JSON.parse(fs.readFileSync(path.join(f.inbound, 'tombstones', 'nb.json')));
  assert.equal(tomb.id, 'nb');
  assert.ok(tomb.deleted > 0);
  assert.equal(fs.existsSync(path.join(f.inbound, 'docs', 'nb')), false);
  assert.equal(fs.existsSync(path.join(f.backup, 'papier-sources', 'nb.pdf')), false);
  assert.equal(fs.existsSync(path.join(f.backup, 'papier-previews', 'nb')), false);
  assert.equal(fs.existsSync(path.join(f.backup, 'papier-covers', 'data-nb-abc.webp')), false);
  assert.equal(fs.existsSync(path.join(f.backup, 'papier-derived-pdf', 'nb-abc.pdf')), false);
  // the mirror copy stays: only the tablet may prune it (its push is --delete)
  assert.ok(fs.existsSync(path.join(f.backup, 'papier', 'docs', 'nb', 'meta.json')));

  // and the manifest hides it immediately
  const library = await (await fetch(base + '/library')).json();
  assert.deepEqual(library.docs.map((d) => d.id), []);

  // deleting again is idempotent (the mirror copy is still there until the
  // tablet prunes it); an id that never existed is a 404
  assert.equal((await post(base, '/doc-delete?id=nb')).status, 200);
  assert.equal((await post(base, '/doc-delete?id=ghost')).status, 404);

  // a write already in flight when the delete landed must not resurrect it
  const page = { v: 1, next_patch: 1, next_stroke: 2, strokes: [{ i: 1, g: 0, p: [10, 10, 24] }], patches: [] };
  assert.equal((await post(base, '/ink?id=nb&file=note-0001.json', page)).status, 404);
  assert.equal((await post(base, '/page-add?id=nb', {})).status, 404);
  assert.equal((await post(base, '/doc-meta?id=nb', { title: 'zombie' })).status, 404);
  assert.equal(fs.existsSync(path.join(f.inbound, 'docs', 'nb')), false);
});

test('a web ink write to a fresh page keeps the tablet mirror untouched', async (t) => {
  const f = fixture(t);
  const base = await startService(t, f.backup);
  const page = { v: 1, next_patch: 1, next_stroke: 2, strokes: [{ i: 1, g: 0, p: [100, 100, 24, 200, 200, 24] }], patches: [] };
  const r = await post(base, '/ink?id=nb&file=note-0002.json', page);
  assert.equal(r.status, 200);
  assert.equal(JSON.parse(fs.readFileSync(path.join(f.inbound, 'docs', 'nb', 'ink', 'note-0002.json'))).strokes.length, 1);
  assert.equal(fs.existsSync(path.join(f.backup, 'papier', 'docs', 'nb', 'ink', 'note-0002.json')), false);
  // and the read path merges: /ink serves the overlay copy
  const merged = await (await fetch(base + '/ink?id=nb&file=note-0002.json')).json();
  assert.equal(merged.strokes.length, 1);
});
