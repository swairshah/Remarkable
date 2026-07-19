'use strict';

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

test('render jobs return immediately, report progress, and deliver atomically', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-render-job-'));
  const backup = path.join(root, 'backup');
  const renderer = path.join(root, 'fake-render.js');
  const port = await freePort();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  writeJson(path.join(backup, 'papier', 'docs', 'book', 'meta.json'), { title: 'Book', pages: 2, w: 1404, h: 1872 });
  fs.mkdirSync(path.join(backup, 'papier-sources'), { recursive: true });
  fs.writeFileSync(path.join(backup, 'papier-sources', 'book.pdf'), '%PDF-test');
  fs.writeFileSync(renderer, `#!/usr/bin/env node
const fs=require('fs'),path=require('path');
const [src,out,title,x0,y0,x1,y1]=process.argv.slice(2);
fs.mkdirSync(path.join(out,'pages'),{recursive:true});
fs.mkdirSync(path.join(out,'text'),{recursive:true});
console.error('mkbook: 1/2 pages');
setTimeout(()=>{
  fs.writeFileSync(path.join(out,'pages','0001.png'),'page1');
  fs.writeFileSync(path.join(out,'pages','0002.png'),'page2');
  fs.writeFileSync(path.join(out,'text','0001.json'),'{}');
  fs.writeFileSync(path.join(out,'text','0002.json'),'{}');
  fs.writeFileSync(path.join(out,'thumb.png'),'cover');
  fs.writeFileSync(path.join(out,'state.json'),JSON.stringify({seq:[{p:0},{p:1}]}));
  fs.writeFileSync(path.join(out,'meta.json'),JSON.stringify({title,pages:2,w:1404,h:1872,crop:[+x0,+y0,+x1,+y1]}));
  console.error('mkbook: 2/2 pages');
},75);
`);
  fs.chmodSync(renderer, 0o755);

  const service = spawn(process.execPath, [path.resolve(__dirname, '../bin/papier-upload.js')], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_RENDER: renderer, PAPIER_PORT: String(port) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let serviceErr = '';
  service.stderr.on('data', (chunk) => { serviceErr += chunk.toString(); });
  t.after(() => service.kill('SIGTERM'));
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('service start timeout')), 3000);
    service.once('exit', (code) => reject(new Error('service exited ' + code)));
    service.stdout.on('data', (chunk) => {
      if (chunk.toString().includes(`127.0.0.1:${port}`)) { clearTimeout(timer); resolve(); }
    });
  });

  const submittedAt = Date.now();
  const submitted = await fetch(`http://127.0.0.1:${port}/render-job`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: 'book', crop: [0.1, 0.1, 0.9, 0.9] }),
  });
  const job = await submitted.json();
  assert.equal(submitted.status, 202);
  assert.equal(job.ok, true);
  assert.ok(Date.now() - submittedAt < 500, 'submission should not wait for the renderer');

  let status;
  for (let i = 0; i < 200; i++) {
    status = await fetch(`http://127.0.0.1:${port}/render-status?job=${job.job}`).then((r) => r.json());
    if (status.status === 'done' || status.status === 'failed') break;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.equal(status.status, 'done', serviceErr);
  assert.equal(status.page, 2);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(backup, 'papier-inbound', 'docs', 'book', 'meta.json'))).crop,
    [0.1, 0.1, 0.9, 0.9]);
  assert.equal(fs.readFileSync(path.join(backup, 'papier-inbound', 'docs', 'book', 'thumb.png'), 'utf8'), 'cover');
});
