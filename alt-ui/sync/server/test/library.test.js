'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const { buildLibrary, inkKey, serializedLibrary } = require('../bin/alt-ui-library');

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value));
}

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'paper-library-'));
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
  assert.equal(library.docs.find((doc) => doc.id === 'book').cover, '/paper/api/cover?source=data&id=book&v=' + library.docs.find((doc) => doc.id === 'book').coverVersion);
  assert.deepEqual(library.docs.find((doc) => doc.id === 'book').ink, ['pdf-0002']);
  assert.deepEqual(library.folders, ['Research', 'Writing']);
  assert.match(library.generation, /^[a-f0-9]{16}$/);
});

test('serializedLibrary is stable until the filesystem changes', (t) => {
  const f = fixture();
  t.after(() => fs.rmSync(f.root, { recursive: true, force: true }));
  const a = serializedLibrary({ mirror: f.mirror, inbound: f.inbound });
  const b = serializedLibrary({ mirror: f.mirror, inbound: f.inbound });
  assert.equal(a.etag, b.etag);
  assert.deepEqual(a.body, b.body);
});

test('inkKey follows Paper page naming', () => {
  assert.equal(inkKey({ p: 0 }), 'pdf-0001');
  assert.equal(inkKey({ n: 7 }), 'note-0007');
  assert.equal(inkKey(null), null);
});
