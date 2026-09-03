'use strict';

const assert = require('node:assert/strict');
const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const composeScript = path.resolve(__dirname, '../bin/papier-compose.sh');
const uploadService = path.resolve(__dirname, '../bin/papier-upload.js');

function executable(file, source) {
  fs.writeFileSync(file, source);
  fs.chmodSync(file, 0o755);
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => server.listen(0, '127.0.0.1', resolve).once('error', reject));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function waitForJob(port, job) {
  let status;
  for (let i = 0; i < 200; i++) {
    status = await fetch(`http://127.0.0.1:${port}/compose-status?job=${job}`).then((r) => r.json());
    if (status.status === 'done' || status.status === 'failed') return status;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error(`compose job did not settle: ${JSON.stringify(status)}`);
}

test('compose renderer defaults to PDF and creates a Clippings-style EPUB 3', (t) => {
  if (spawnSync('pandoc', ['--version']).status !== 0) return t.skip('pandoc is not installed');
  if (spawnSync('unzip', ['-v']).status !== 0) return t.skip('unzip is not installed');

  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-compose-script-'));
  const fakePi = path.join(root, 'fake-pi.sh');
  const fakePdf = path.join(root, 'fake-pdf.sh');
  const emptyFonts = path.join(root, 'fonts');
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(emptyFonts);

  executable(fakePi, `#!/bin/sh
cat > article.md <<'EOF'
---
title: "Archive Test"
---

## A section

Inline math $x^2 + y^2 = z^2$ and **strong text**.
EOF
printf '%s\n' 'Archive Test'
`);
  executable(fakePdf, `#!/bin/sh
printf '%s\n' '%PDF-1.4 test' > "$2"
`);

  const run = (job, formats) => {
    fs.mkdirSync(path.join(job, 'work'), { recursive: true });
    fs.mkdirSync(path.join(job, 'out'), { recursive: true });
    fs.writeFileSync(path.join(job, 'instructions.md'), 'Write a test article.\n');
    if (formats) fs.writeFileSync(path.join(job, 'formats.txt'), formats.join('\n') + '\n');
    return spawnSync('/bin/bash', [composeScript, job], {
      encoding: 'utf8',
      env: { ...process.env, HOME: root, PI_BIN: fakePi, MD2PDF: fakePdf, READER_FONT_DIR: emptyFonts },
    });
  };

  const defaultJob = path.join(root, 'default-job');
  const defaultResult = run(defaultJob);
  assert.equal(defaultResult.status, 0, defaultResult.stderr);
  assert.equal(fs.readFileSync(path.join(defaultJob, 'formats.txt'), 'utf8'), 'pdf\n');
  assert.ok(fs.existsSync(path.join(defaultJob, 'out', 'article.pdf')));
  assert.ok(!fs.existsSync(path.join(defaultJob, 'out', 'article.epub')));

  const epubJob = path.join(root, 'epub-job');
  const epubResult = run(epubJob, ['epub']);
  assert.equal(epubResult.status, 0, epubResult.stderr);
  assert.ok(!fs.existsSync(path.join(epubJob, 'out', 'article.pdf')));
  const epub = path.join(epubJob, 'out', 'article.epub');
  assert.ok(fs.statSync(epub).size > 0);

  const entries = spawnSync('unzip', ['-Z1', epub], { encoding: 'utf8' });
  assert.equal(entries.status, 0, entries.stderr);
  const names = entries.stdout.trim().split('\n');
  const xhtml = names.filter((name) => name.endsWith('.xhtml') && !name.endsWith('nav.xhtml'));
  const css = names.find((name) => name.endsWith('.css'));
  assert.ok(xhtml.length, entries.stdout);
  assert.ok(css, entries.stdout);
  assert.equal(spawnSync('unzip', ['-p', epub, 'mimetype'], { encoding: 'utf8' }).stdout, 'application/epub+zip');
  const content = xhtml.map((name) => spawnSync('unzip', ['-p', epub, name], { encoding: 'utf8' }).stdout).join('\n');
  assert.match(content, /<math/);
  assert.doesNotMatch(content, /<annotation\b/);
  const stylesheet = spawnSync('unzip', ['-p', epub, css], { encoding: 'utf8' }).stdout;
  assert.match(stylesheet, /kindle-reader-fonts-faux-350-v6/);
  assert.match(stylesheet, /papier-white-page-v1/);
  assert.match(stylesheet, /background:\s*#fff\s*!important/);
});

test('compose API validates formats, publishes EPUB downloads, and only syncs selected PDFs', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-compose-api-'));
  const backup = path.join(root, 'backup');
  const composer = path.join(root, 'fake-compose.js');
  const renderer = path.join(root, 'fake-render.js');
  const port = await freePort();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  executable(composer, `#!/usr/bin/env node
const fs = require('fs'), path = require('path');
const job = process.argv[2];
const formats = fs.readFileSync(path.join(job, 'formats.txt'), 'utf8').trim().split(/\\s+/);
console.error('\\u001b[36m[compose] researching sources\\u001b[0m');
fs.writeFileSync(path.join(job, 'title.txt'), 'Format Test');
for (const format of formats) fs.writeFileSync(path.join(job, 'out', 'article.' + format), format === 'pdf' ? '%PDF-test' : 'PK-epub-test');
fs.writeFileSync(path.join(job, 'status.txt'), 'done writing');
`);
  executable(renderer, `#!/usr/bin/env node
const fs = require('fs'), path = require('path');
const [src, out, title] = process.argv.slice(2);
fs.mkdirSync(path.join(out, 'pages'), { recursive: true });
fs.mkdirSync(path.join(out, 'text'), { recursive: true });
fs.writeFileSync(path.join(out, 'pages', '0001.png'), 'page');
fs.writeFileSync(path.join(out, 'text', '0001.json'), '{}');
fs.writeFileSync(path.join(out, 'thumb.png'), 'cover');
fs.writeFileSync(path.join(out, 'state.json'), JSON.stringify({ seq: [{ p: 0 }] }));
fs.writeFileSync(path.join(out, 'meta.json'), JSON.stringify({ title, kind: 'book', pages: 1, w: 1404, h: 1872 }));
`);

  const service = spawn(process.execPath, [uploadService], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_COMPOSE: composer, PAPIER_RENDER: renderer, PAPIER_PORT: String(port) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let serviceErr = '';
  service.stderr.on('data', (chunk) => { serviceErr += chunk.toString(); });
  t.after(() => service.kill('SIGTERM'));
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('service start timeout')), 3000);
    service.once('exit', (code) => reject(new Error(`service exited ${code}: ${serviceErr}`)));
    service.stdout.on('data', (chunk) => {
      if (chunk.toString().includes(`127.0.0.1:${port}`)) { clearTimeout(timer); resolve(); }
    });
  });

  const submit = async (body) => {
    const response = await fetch(`http://127.0.0.1:${port}/compose`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    });
    return { response, json: await response.json() };
  };

  const invalid = await submit({ instructions: 'No output', formats: [] });
  assert.equal(invalid.response.status, 400);
  assert.match(invalid.json.error, /select at least one/);

  const epubSubmission = await submit({ instructions: 'EPUB please', formats: ['epub'] });
  assert.equal(epubSubmission.response.status, 202);
  const epubStatus = await waitForJob(port, epubSubmission.json.job);
  assert.equal(epubStatus.status, 'done', serviceErr);
  assert.equal(epubStatus.trace, undefined);
  assert.deepEqual(epubStatus.formats, ['epub']);
  assert.deepEqual(epubStatus.outputs, ['epub']);
  assert.equal(epubStatus.docId, undefined);
  assert.ok(!fs.existsSync(path.join(backup, 'papier-inbound', 'docs', 'format-test')));

  const download = await fetch(`http://127.0.0.1:${port}/compose-download?job=${epubSubmission.json.job}&format=epub`);
  assert.equal(download.status, 200);
  assert.equal(download.headers.get('content-type'), 'application/epub+zip');
  assert.match(download.headers.get('content-disposition'), /Format Test\.epub/);
  assert.equal(await download.text(), 'PK-epub-test');

  const tracedResponse = await fetch(`http://127.0.0.1:${port}/compose-status?job=${epubSubmission.json.job}&trace=1`);
  const traced = await tracedResponse.json();
  assert.equal(tracedResponse.headers.get('cache-control'), 'private, no-store');
  assert.match(traced.trace, /\[compose\] starting/);
  assert.match(traced.trace, /\[compose\] researching sources/);
  assert.doesNotMatch(traced.trace, /\u001b/);

  const defaultSubmission = await submit({ instructions: 'Default output' });
  assert.equal(defaultSubmission.response.status, 202);
  const defaultStatus = await waitForJob(port, defaultSubmission.json.job);
  assert.equal(defaultStatus.status, 'done', serviceErr);
  assert.deepEqual(defaultStatus.formats, ['pdf']);
  assert.deepEqual(defaultStatus.outputs, ['pdf']);
  assert.equal(defaultStatus.docId, 'format-test');
  assert.ok(fs.existsSync(path.join(backup, 'papier-inbound', 'docs', 'format-test', 'meta.json')));

  const bothSubmission = await submit({ instructions: 'Both please', formats: ['pdf', 'epub'] });
  assert.equal(bothSubmission.response.status, 202);
  const bothStatus = await waitForJob(port, bothSubmission.json.job);
  assert.equal(bothStatus.status, 'done', serviceErr);
  assert.deepEqual(bothStatus.outputs, ['pdf', 'epub']);
  assert.equal(bothStatus.docId, 'format-test-2');
});
