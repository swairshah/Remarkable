'use strict';

const assert = require('node:assert/strict');
const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const kindleScript = path.resolve(__dirname, '../bin/papier-kindle.sh');
const kindleCoverScript = path.resolve(__dirname, '../bin/papier-kindle-cover.py');

function runScript(args, env) {
  return new Promise((resolve) => {
    const child = spawn('/bin/bash', [kindleScript, ...args], {
      env: { ...process.env, ...env },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk.toString(); });
    child.stderr.on('data', (chunk) => { stderr += chunk.toString(); });
    child.on('close', (code, signal) => resolve({ code, signal, stdout, stderr }));
  });
}

test('Kindle sender posts one Base64 attachment to the Resend API', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-kindle-resend-'));
  const home = path.join(root, 'home');
  const backup = path.join(root, 'backup');
  const doc = path.join(backup, 'papier', 'docs', 'book');
  fs.mkdirSync(doc, { recursive: true });
  fs.mkdirSync(path.join(backup, 'papier-sources'), { recursive: true });
  fs.mkdirSync(home);
  fs.writeFileSync(path.join(doc, 'meta.json'), JSON.stringify({ title: 'Resend Test', kind: 'book', pages: 1 }));
  fs.writeFileSync(path.join(backup, 'papier-sources', 'book.pdf'), '%PDF-test');
  const fakeCover = path.join(root, 'fake-cover.py');
  const fakePython = path.join(root, 'fake-python');
  fs.writeFileSync(fakeCover, '# test double\n');
  fs.writeFileSync(fakePython, '#!/bin/sh\ncp "$2" "$3"\n', { mode: 0o755 });
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  const requests = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (chunk) => chunks.push(chunk));
    req.on('end', () => {
      requests.push({ url: req.url, headers: req.headers, body: Buffer.concat(chunks).toString('utf8') });
      if (req.url === '/fail') {
        res.writeHead(422, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ message: 'sender domain is not verified' }));
      } else {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ id: 'email_123' }));
      }
    });
  });
  await new Promise((resolve, reject) => server.listen(0, '127.0.0.1', resolve).once('error', reject));
  t.after(() => server.close());
  const api = `http://127.0.0.1:${server.address().port}`;
  const env = {
    HOME: home,
    PAPIER_BACKUP: backup,
    RESEND_API_URL: `${api}/emails`,
    RESEND_API_KEY: 're_test_secret',
    KINDLE_FROM: 'Papier <kindle@kindle.swair.io>',
    KINDLE_TO: 'reader@kindle.com',
    PAPIER_PY: fakePython,
    PAPIER_KINDLE_COVER: fakeCover,
  };

  const sent = await runScript(['book', '--format', 'pdf'], env);
  assert.equal(sent.code, 0, sent.stderr);
  assert.match(sent.stdout, /sent: Resend Test\.pdf/);
  assert.match(sent.stdout, /Resend email_123/);
  assert.doesNotMatch(sent.stdout + sent.stderr, /re_test_secret/);
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, '/emails');
  assert.equal(requests[0].headers.authorization, 'Bearer re_test_secret');
  assert.equal(requests[0].headers['content-type'], 'application/json');

  const payload = JSON.parse(requests[0].body);
  assert.equal(payload.from, env.KINDLE_FROM);
  assert.deepEqual(payload.to, [env.KINDLE_TO]);
  assert.equal(payload.subject, 'Resend Test');
  assert.equal(payload.text, 'Sent from Papier.');
  assert.equal(payload.attachments.length, 1);
  assert.equal(payload.attachments[0].filename, 'Resend Test.pdf');
  assert.equal(payload.attachments[0].content_type, 'application/pdf');
  assert.equal(Buffer.from(payload.attachments[0].content, 'base64').toString('utf8'), '%PDF-test');

  const failed = await runScript(['book', '--format', 'pdf'], { ...env, RESEND_API_URL: `${api}/fail` });
  assert.equal(failed.code, 1);
  assert.match(failed.stderr, /Resend API returned HTTP 422/);
  assert.match(failed.stderr, /sender domain is not verified/);
});

test('Kindle PDF helper prepends a title sheet without changing the original pages', (t) => {
  const python = process.env.PAPIER_TEST_PY || 'python3';
  if (spawnSync(python, ['-c', 'import pymupdf, reportlab']).status !== 0) {
    return t.skip('PyMuPDF and ReportLab are not installed');
  }

  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-kindle-cover-'));
  const source = path.join(root, 'source.pdf');
  const output = path.join(root, 'kindle.pdf');
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  const create = spawnSync(python, ['-c', [
    'import pymupdf, sys',
    'd = pymupdf.open()',
    'p = d.new_page(width=444.96, height=594.96)',
    'p.insert_text((60, 90), "Original first page")',
    'd.save(sys.argv[1])',
  ].join('; '), source], { encoding: 'utf8' });
  assert.equal(create.status, 0, create.stderr);

  const render = spawnSync(python, [kindleCoverScript, source, output, 'A Better Kindle Title'], {
    encoding: 'utf8',
    env: { ...process.env, PAPIER_PDF_FONT_DIR: path.join(root, 'no-fonts') },
  });
  assert.equal(render.status, 0, render.stderr);

  const inspect = spawnSync(python, ['-c', [
    'import pymupdf, json, sys',
    'd = pymupdf.open(sys.argv[1])',
    'print(json.dumps({"pages": d.page_count, "cover": d[0].get_text(), "first": d[1].get_text(), "title": d.metadata.get("title")}))',
  ].join('; '), output], { encoding: 'utf8' });
  assert.equal(inspect.status, 0, inspect.stderr);
  const result = JSON.parse(inspect.stdout);
  assert.equal(result.pages, 2);
  assert.ok(result.cover.replace(/\s/g, '').includes('PAPIER'));
  assert.match(result.cover, /A Better Kindle Title/);
  assert.match(result.first, /Original first page/);
  assert.equal(result.title, 'A Better Kindle Title');
});

test('Kindle sender renders Compose markdown with Clippings-style MathML EPUB CSS', async (t) => {
  if (spawnSync('pandoc', ['--version']).status !== 0) return t.skip('pandoc is not installed');
  if (spawnSync('unzip', ['-v']).status !== 0) return t.skip('unzip is not installed');

  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-kindle-epub-'));
  const home = path.join(root, 'home');
  const backup = path.join(root, 'backup');
  const docId = 'compose-book';
  const doc = path.join(backup, 'papier', 'docs', docId);
  const job = path.join(backup, 'papier-compose', 'job');
  const emptyFonts = path.join(root, 'fonts');
  fs.mkdirSync(doc, { recursive: true });
  fs.mkdirSync(path.join(job, 'work'), { recursive: true });
  fs.mkdirSync(home);
  fs.mkdirSync(emptyFonts);
  fs.writeFileSync(path.join(doc, 'meta.json'), JSON.stringify({ title: 'Kindle Math Test', kind: 'book', pages: 1 }));
  fs.writeFileSync(path.join(job, 'result.json'), JSON.stringify({ docId }));
  fs.writeFileSync(path.join(job, 'work', 'article.md'), `---
title: "Kindle Math Test"
---

## Equations

Inline math $f : \\Gamma \\to \\Gamma$ stays on the text baseline.

$$g \\circ f = \\mathrm{id}$$
`);
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  const requests = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (chunk) => chunks.push(chunk));
    req.on('end', () => {
      requests.push(JSON.parse(Buffer.concat(chunks).toString('utf8')));
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ id: 'email_epub_123' }));
    });
  });
  await new Promise((resolve, reject) => server.listen(0, '127.0.0.1', resolve).once('error', reject));
  t.after(() => server.close());

  const sent = await runScript([docId, '--format', 'epub'], {
    HOME: home,
    PAPIER_BACKUP: backup,
    READER_FONT_DIR: emptyFonts,
    RESEND_API_URL: `http://127.0.0.1:${server.address().port}/emails`,
    RESEND_API_KEY: 're_test_secret',
    KINDLE_FROM: 'Papier <kindle@kindle.swair.io>',
    KINDLE_TO: 'reader@kindle.com',
  });
  assert.equal(sent.code, 0, sent.stderr);
  assert.match(sent.stdout, /sent: Kindle Math Test\.epub/);
  assert.equal(requests.length, 1);

  const attachment = requests[0].attachments[0];
  assert.equal(attachment.filename, 'Kindle Math Test.epub');
  assert.equal(attachment.content_type, 'application/epub+zip');
  const epub = path.join(root, 'kindle-math.epub');
  fs.writeFileSync(epub, Buffer.from(attachment.content, 'base64'));
  assert.equal(spawnSync('unzip', ['-p', epub, 'mimetype'], { encoding: 'utf8' }).stdout, 'application/epub+zip');

  const entries = spawnSync('unzip', ['-Z1', epub], { encoding: 'utf8' });
  assert.equal(entries.status, 0, entries.stderr);
  const names = entries.stdout.trim().split('\n');
  const xhtml = names.filter((name) => name.endsWith('.xhtml') && !name.endsWith('nav.xhtml'));
  const css = names.find((name) => name.endsWith('.css'));
  assert.ok(xhtml.length, entries.stdout);
  assert.ok(css, entries.stdout);
  assert.equal(names.filter((name) => /EPUB\/media\/.*\.png$/.test(name)).length, 0, entries.stdout);

  const content = xhtml.map((name) => spawnSync('unzip', ['-p', epub, name], { encoding: 'utf8' }).stdout).join('\n');
  assert.match(content, /<math/);
  assert.doesNotMatch(content, /<annotation\b/);
  assert.doesNotMatch(content, /<img[^>]+alt="(?:f|g|\\Gamma)/);

  const stylesheet = spawnSync('unzip', ['-p', epub, css], { encoding: 'utf8' }).stdout;
  assert.match(stylesheet, /kindle-reader-fonts-faux-350-v6/);
  assert.match(stylesheet, /papier-white-page-v1/);
  assert.match(stylesheet, /background:\s*#fff\s*!important/);
});
