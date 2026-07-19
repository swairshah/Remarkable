'use strict';

// papier-ios write-back endpoints: POST /ink, /state, /notebook all land in
// the INBOUND tree (never the mirror) so the tablet's add-only pull applies
// them with per-file last-writer-wins.

const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

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

test('ink/state/notebook write-back lands in inbound with validation', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-writeback-'));
  const backup = path.join(root, 'backup');
  const port = await freePort();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  // one mirrored doc (as if the tablet pushed it)
  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'meta.json'),
    { created: 1, folder: '', kind: 'notebook', title: 'NB', v: 1 });
  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'state.json'),
    { next_note: 2, pos: 0, seq: [{ n: 1 }] });

  const service = spawn(process.execPath, [path.resolve(__dirname, '../bin/papier-upload.js')], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_PORT: String(port) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  t.after(() => service.kill('SIGTERM'));
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('service start timeout')), 3000);
    service.once('exit', (code) => reject(new Error('service exited ' + code)));
    service.stdout.on('data', (chunk) => {
      if (chunk.toString().includes(`127.0.0.1:${port}`)) { clearTimeout(timer); resolve(); }
    });
  });
  const base = `http://127.0.0.1:${port}`;

  // ink write to a mirrored doc -> inbound/docs/nb/ink/note-0001.json
  const page = { v: 1, next_patch: 1, next_stroke: 3,
    strokes: [{ i: 1, g: 0, p: [100, 200, 20, 110, 210, 20] }], patches: [] };
  let r = await fetch(`${base}/ink?id=nb&file=note-0001.json`, { method: 'POST', body: JSON.stringify(page) });
  assert.equal(r.status, 200);
  const written = JSON.parse(fs.readFileSync(
    path.join(backup, 'papier-inbound', 'docs', 'nb', 'ink', 'note-0001.json')));
  assert.equal(written.strokes.length, 1);

  // validation: bad filename, unknown doc, malformed page
  r = await fetch(`${base}/ink?id=nb&file=../evil.json`, { method: 'POST', body: JSON.stringify(page) });
  assert.equal(r.status, 400);
  r = await fetch(`${base}/ink?id=nope&file=note-0001.json`, { method: 'POST', body: JSON.stringify(page) });
  assert.equal(r.status, 404);
  r = await fetch(`${base}/ink?id=nb&file=note-0001.json`, { method: 'POST', body: '{"v":1}' });
  assert.equal(r.status, 400);

  // state write (page appended on the iPad)
  const state = { next_note: 3, pos: 1, seq: [{ n: 1 }, { n: 2 }] };
  r = await fetch(`${base}/state?id=nb`, { method: 'POST', body: JSON.stringify(state) });
  assert.equal(r.status, 200);
  assert.deepEqual(
    JSON.parse(fs.readFileSync(path.join(backup, 'papier-inbound', 'docs', 'nb', 'state.json'))).seq,
    state.seq);
  r = await fetch(`${base}/state?id=nb`, { method: 'POST', body: JSON.stringify({ seq: [{ bogus: true }] }) });
  assert.equal(r.status, 400);

  // notebook create -> fresh inbound bundle with one blank page
  r = await fetch(`${base}/notebook`, { method: 'POST', body: JSON.stringify({ title: 'Sketch Pad' }) });
  assert.equal(r.status, 200);
  const { id } = await r.json();
  assert.equal(id, 'sketch-pad');
  const nbDir = path.join(backup, 'papier-inbound', 'docs', id);
  assert.equal(JSON.parse(fs.readFileSync(path.join(nbDir, 'meta.json'))).kind, 'notebook');
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(nbDir, 'state.json'))).seq, [{ n: 1 }]);
  assert.equal(JSON.parse(fs.readFileSync(path.join(nbDir, 'ink', 'note-0001.json'))).strokes.length, 0);

  // id collision -> de-collided fresh id
  r = await fetch(`${base}/notebook`, { method: 'POST', body: JSON.stringify({ title: 'Sketch Pad' }) });
  assert.equal((await r.json()).id, 'sketch-pad-2');
});
