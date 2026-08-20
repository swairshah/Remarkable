'use strict';

// The crop editor can drag its box PAST the page edge to ADD margin, so the
// render API takes fractions outside 0..1 (mkbook pads the overhang with
// white). These pin the accepted range: out-of-page rects go through and reach
// the renderer verbatim, rects that would render blank or absurd are refused.

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

// A renderer stub that records the crop it was handed, so we can prove the
// service passes negative fractions through instead of clamping them.
async function startService(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-crop-range-'));
  const backup = path.join(root, 'backup');
  const renderer = path.join(root, 'fake-render.js');
  const argsLog = path.join(root, 'render-args.json');
  const port = await freePort();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  writeJson(path.join(backup, 'papier', 'docs', 'book', 'meta.json'), { title: 'Book', pages: 1, w: 1404, h: 1872 });
  fs.mkdirSync(path.join(backup, 'papier-sources'), { recursive: true });
  fs.writeFileSync(path.join(backup, 'papier-sources', 'book.pdf'), '%PDF-test');
  fs.writeFileSync(renderer, `#!/usr/bin/env node
const fs=require('fs'),path=require('path');
const [src,out,title,x0,y0,x1,y1]=process.argv.slice(2);
fs.writeFileSync(${JSON.stringify(argsLog)}, JSON.stringify([x0,y0,x1,y1]));
fs.mkdirSync(path.join(out,'pages'),{recursive:true});
fs.mkdirSync(path.join(out,'text'),{recursive:true});
fs.writeFileSync(path.join(out,'pages','0001.png'),'page1');
fs.writeFileSync(path.join(out,'text','0001.json'),'{}');
fs.writeFileSync(path.join(out,'meta.json'),JSON.stringify({title,pages:1,w:1404,h:1872,crop:[+x0,+y0,+x1,+y1]}));
console.error('mkbook: 1/1 pages');
`);
  fs.chmodSync(renderer, 0o755);

  const service = spawn(process.execPath, [path.resolve(__dirname, '../bin/papier-upload.js')], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_RENDER: renderer, PAPIER_PORT: String(port) },
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

  const render = (crop) => fetch(`http://127.0.0.1:${port}/render-job`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: 'book', crop }),
  });
  const settle = async (job) => {
    for (let i = 0; i < 200; i++) {
      const st = await fetch(`http://127.0.0.1:${port}/render-status?job=${job}`).then((r) => r.json());
      if (st.status === 'done' || st.status === 'failed') return st;
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    throw new Error('render never settled');
  };
  return { render, settle, backup, argsLog };
}

test('an out-of-page crop is accepted and reaches the renderer unclamped', async (t) => {
  const { render, settle, backup, argsLog } = await startService(t);

  const res = await render([-0.2, -0.1, 1.2, 1.15]);   // margin added on all four sides
  assert.equal(res.status, 202);
  const job = await res.json();
  assert.equal(job.ok, true);
  assert.equal((await settle(job.job)).status, 'done');

  assert.deepEqual(JSON.parse(fs.readFileSync(argsLog, 'utf8')), ['-0.2', '-0.1', '1.2', '1.15'],
    'negative fractions must survive the trip to mkbook');
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(backup, 'papier-inbound', 'docs', 'book', 'meta.json'))).crop,
    [-0.2, -0.1, 1.2, 1.15]);
});

test('crops that would render blank or absurd are refused', async (t) => {
  const { render } = await startService(t);

  const cases = {
    'entirely off the page': [1.2, 0.1, 1.9, 0.9],
    'barely touching the page': [-0.99, 0.1, 0.02, 0.9],
    'past the outer bound': [-1.4, 0, 1, 1],
    'inverted': [0.9, 0.1, 0.2, 0.9],
    'not a number': [0, 0, 'x', 1],
  };
  for (const [why, crop] of Object.entries(cases)) {
    const res = await render(crop);
    assert.equal(res.status, 400, `${why} (${JSON.stringify(crop)}) should be rejected`);
    assert.equal((await res.json()).ok, false);
  }

  // ...while the plain whole-page crop still goes through, as does the widest
  // rect the bounds allow (a full page of margin on every side)
  assert.equal((await render([0, 0, 1, 1])).status, 202);
  assert.equal((await render([-1, -1, 2, 2])).status, 202);
});
