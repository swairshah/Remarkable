'use strict';

// Regression: a page added on the iPad writes state.json to the INBOUND
// overlay. The library manifest must serve that overlay state for mirrored
// docs — otherwise the new page vanishes when the doc is reopened.

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const { buildLibrary } = require('../bin/papier-library');

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value));
}

test('overlay state.json wins over the stale mirror copy', (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-overlay-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const mirror = path.join(root, 'mirror');
  const inbound = path.join(root, 'inbound');

  // the tablet's (stale) copy: one page
  writeJson(path.join(mirror, 'docs', 'nb', 'meta.json'),
    { created: 1, folder: '', kind: 'notebook', title: 'NB', v: 1 });
  writeJson(path.join(mirror, 'docs', 'nb', 'state.json'),
    { next_note: 2, pos: 0, seq: [{ n: 1 }] });
  writeJson(path.join(mirror, 'docs', 'nb', 'ink', 'note-0001.json'),
    { v: 1, next_patch: 1, next_stroke: 1, strokes: [], patches: [] });

  // the iPad added page 2: newer state + its ink live in the overlay
  writeJson(path.join(inbound, 'docs', 'nb', 'state.json'),
    { next_note: 3, pos: 1, seq: [{ n: 1 }, { n: 2 }] });
  writeJson(path.join(inbound, 'docs', 'nb', 'ink', 'note-0002.json'),
    { v: 1, next_patch: 1, next_stroke: 2, strokes: [{ i: 1, g: 0, p: [10, 10, 20] }], patches: [] });

  const lib = buildLibrary({ mirror, inbound });
  const doc = lib.docs.find((d) => d.id === 'nb');
  assert.ok(doc, 'doc listed');
  assert.equal(doc.pending, false, 'still the mirrored doc');
  assert.deepEqual(doc.seq, [{ n: 1 }, { n: 2 }], 'overlay seq served — added page survives reopen');
  assert.deepEqual(doc.ink, ['note-0001', 'note-0002'], 'ink keys merged from both trees');

  // and the doc version must move when overlay ink changes (cache-busting).
  // Real writers are atomic (tmp + rename), which advances the dir mtime.
  const v1 = doc.version;
  const inkFile = path.join(inbound, 'docs', 'nb', 'ink', 'note-0002.json');
  const future = new Date(Date.now() + 5000);
  fs.writeFileSync(inkFile + '.tmp', fs.readFileSync(inkFile));
  fs.renameSync(inkFile + '.tmp', inkFile);
  fs.utimesSync(path.dirname(inkFile), future, future);
  const v2 = buildLibrary({ mirror, inbound }).docs.find((d) => d.id === 'nb').version;
  assert.notEqual(v1, v2, 'version bumps on overlay ink change');
});
