'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const { buildLibrary, inkKey, serializedLibrary } = require('../bin/papier-library');

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value));
}

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-library-'));
  const mirror = path.join(root, 'mirror');
  const inbound = path.join(root, 'inbound');
  writeJson(path.join(mirror, 'folders.json'), { folders: ['Research'] });
  writeJson(path.join(mirror, 'docs', 'book', 'meta.json'), { title: 'Book', pages: 2, folder: 'Research' });
  writeJson(path.join(mirror, 'docs', 'book', 'state.json'), { seq: [{ p: 0 }, { p: 1 }] });
  writeJson(path.join(mirror, 'docs', 'book', 'ink', 'pdf-0002.json'), { strokes: [] });
  fs.writeFileSync(path.join(mirror, 'docs', 'book', 'thumb.png'), 'thumb');
  writeJson(path.join(mirror, 'docs', 'notes', 'meta.json'), { title: 'Notes', kind: 'notebook', folder: 'Writing' });
  writeJson(path.join(mirror, 'docs', 'notes', 'state.json'), { seq: [{ n: 1 }] });
  writeJson(path.join(mirror, 'docs', 'notes', 'ink', 'note-0001.json'), { strokes: [] });
  writeJson(path.join(inbound, 'docs', 'book', 'meta.json'), { title: 'Pending duplicate', pages: 2 });
  writeJson(path.join(inbound, 'docs', 'pending', 'meta.json'), { title: 'Pending', pages: 1 });
  writeJson(path.join(inbound, 'docs', 'pending', 'state.json'), { seq: [{ p: 0 }] });
  fs.mkdirSync(path.join(inbound, 'docs', 'stale-empty'), { recursive: true });
  return { root, mirror, inbound };
}

test('buildLibrary merges sources, ignores stale dirs, and exposes cover/ink metadata', (t) => {
  const f = fixture();
  t.after(() => fs.rmSync(f.root, { recursive: true, force: true }));
  const library = buildLibrary({ mirror: f.mirror, inbound: f.inbound });

  assert.deepEqual(library.docs.map((doc) => doc.id).sort(), ['book', 'notes', 'pending']);
  assert.equal(library.docs.find((doc) => doc.id === 'book').pending, false);
  assert.equal(library.docs.find((doc) => doc.id === 'pending').pending, true);
  assert.equal(library.docs.find((doc) => doc.id === 'book').cover, '/papier/api/cover?source=data&id=book&v=' + library.docs.find((doc) => doc.id === 'book').coverVersion);
  assert.deepEqual(library.docs.find((doc) => doc.id === 'book').ink, ['pdf-0002']);
  assert.deepEqual(library.folders, ['Research', 'Writing']);
  assert.match(library.generation, /^[a-f0-9]{16}$/);
});

test('buildLibrary flags docs whose source PDF is retained', (t) => {
  const f = fixture();
  t.after(() => fs.rmSync(f.root, { recursive: true, force: true }));
  const sources = path.join(f.root, 'sources');
  fs.mkdirSync(sources, { recursive: true });
  fs.writeFileSync(path.join(sources, 'book.pdf'), '%PDF-fake');
  const library = buildLibrary({ mirror: f.mirror, inbound: f.inbound, sources });
  const book = library.docs.find((doc) => doc.id === 'book');
  const pending = library.docs.find((doc) => doc.id === 'pending');
  assert.equal(book.hasSource, true);
  assert.match(book.srcVersion, /^[a-z0-9]+$/);
  assert.equal(pending.hasSource, false);
  assert.equal(pending.srcVersion, null);
});

test('serializedLibrary is stable until the filesystem changes', (t) => {
  const f = fixture();
  t.after(() => fs.rmSync(f.root, { recursive: true, force: true }));
  const a = serializedLibrary({ mirror: f.mirror, inbound: f.inbound });
  const b = serializedLibrary({ mirror: f.mirror, inbound: f.inbound });
  assert.equal(a.etag, b.etag);
  assert.deepEqual(a.body, b.body);
});

test('the inbound overlay wins for meta and flags its own ink pages', (t) => {
  const f = fixture();
  t.after(() => fs.rmSync(f.root, { recursive: true, force: true }));
  // a web rename + a web-drawn page, both living only in the overlay
  writeJson(path.join(f.inbound, 'docs', 'notes', 'meta.json'),
    { title: 'Renamed on the web', kind: 'notebook', folder: 'Research' });
  writeJson(path.join(f.inbound, 'docs', 'notes', 'ink', 'note-0002.json'), { strokes: [{ i: 1, g: 0, p: [] }] });
  const library = buildLibrary({ mirror: f.mirror, inbound: f.inbound });
  const notes = library.docs.find((doc) => doc.id === 'notes');

  assert.equal(notes.pending, false);                 // still the mirrored doc
  assert.equal(notes.meta.title, 'Renamed on the web');
  assert.equal(notes.meta.folder, 'Research');
  assert.deepEqual(notes.ink, ['note-0001', 'note-0002']);
  assert.deepEqual(notes.inkPending, ['note-0002']);  // read this one through /api/ink
  assert.deepEqual(library.docs.find((doc) => doc.id === 'book').inkPending, []);
});

test('tombstoned docs disappear from the library while the mirror still has them', (t) => {
  const f = fixture();
  t.after(() => fs.rmSync(f.root, { recursive: true, force: true }));
  const tombstones = path.join(f.root, 'tombstones');
  fs.mkdirSync(tombstones, { recursive: true });
  fs.writeFileSync(path.join(tombstones, 'book.json'), JSON.stringify({ id: 'book', deleted: 1 }));
  fs.writeFileSync(path.join(tombstones, 'pending.json'), JSON.stringify({ id: 'pending', deleted: 1 }));

  const library = buildLibrary({ mirror: f.mirror, inbound: f.inbound, tombstones });
  assert.deepEqual(library.docs.map((doc) => doc.id), ['notes']);
  // the mirror is the tablet's to prune — the manifest only hides the doc
  assert.ok(fs.existsSync(path.join(f.mirror, 'docs', 'book', 'meta.json')));
});

test('an ink write bumps the doc version so the browser refetches', (t) => {
  const f = fixture();
  t.after(() => fs.rmSync(f.root, { recursive: true, force: true }));
  const before = buildLibrary({ mirror: f.mirror, inbound: f.inbound }).docs.find((d) => d.id === 'notes').version;
  const overlayInk = path.join(f.inbound, 'docs', 'notes', 'ink', 'note-0001.json');
  writeJson(overlayInk, { strokes: [{ i: 1, g: 0, p: [] }] });
  fs.utimesSync(overlayInk, new Date(Date.now() + 60000), new Date(Date.now() + 60000));
  const after = buildLibrary({ mirror: f.mirror, inbound: f.inbound }).docs.find((d) => d.id === 'notes').version;
  assert.notEqual(before, after);
});

test('inkKey follows Papier page naming', () => {
  assert.equal(inkKey({ p: 0 }), 'pdf-0001');
  assert.equal(inkKey({ n: 7 }), 'note-0007');
  assert.equal(inkKey(null), null);
});
