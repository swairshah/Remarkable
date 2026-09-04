'use strict';

const assert = require('node:assert/strict');
const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const renderScript = path.resolve(__dirname, '../bin/papier-publish-render.py');
const siteScript = path.resolve(__dirname, '../bin/papier-publish-site.py');
const publishScript = path.resolve(__dirname, '../bin/papier-publish.sh');
const saveScript = path.resolve(__dirname, '../bin/papier-publish-save.sh');
const uploadService = path.resolve(__dirname, '../bin/papier-upload.js');
const websiteUi = path.resolve(__dirname, '../web/website/index.html');
const sharedNav = path.resolve(__dirname, '../../../../sync/server/web/nav.js');
const PY = process.env.PAPIER_PY || 'python3';
const hasPillow = () => spawnSync(PY, ['-c', 'import PIL'], { encoding: 'utf8' }).status === 0;
const hasPandoc = () => spawnSync('pandoc', ['--version']).status === 0;
const hasGit = () => spawnSync('git', ['--version']).status === 0;

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value));
}
function executable(file, source) {
  fs.writeFileSync(file, source);
  fs.chmodSync(file, 0o755);
}
function stroke(id, y, x0 = 200, x1 = 600, width = 25, g = 0) {
  const p = [];
  for (let x = x0; x <= x1; x += 20) p.push(x * 10, y * 10, width);
  return { i: id, g, p };
}
function pixel(png, x, y) {
  const r = spawnSync(PY, ['-c', `
from PIL import Image; import sys
im = Image.open(sys.argv[1]).convert("RGB"); print(*im.getpixel((int(sys.argv[2]), int(sys.argv[3]))))`, png, String(x), String(y)], { encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  return r.stdout.trim().split(' ').map(Number);
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => server.listen(0, '127.0.0.1', resolve).once('error', reject));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function waitForService(service, port, stderr) {
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`service start timeout: ${stderr()}`)), 3000);
    service.once('exit', (code) => reject(new Error(`service exited ${code}: ${stderr()}`)));
    service.stdout.on('data', (chunk) => {
      if (chunk.toString().includes(`127.0.0.1:${port}`)) { clearTimeout(timer); resolve(); }
    });
  });
}

test('render script diffs strokes by content and colours grey/black/red', (t) => {
  if (!hasPillow()) return t.skip('Pillow is not installed');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-publish-render-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const prev = path.join(root, 'prev.json');
  const cur = path.join(root, 'cur.json');
  writeJson(prev, { strokes: [stroke(1, 500), stroke(2, 900)] });
  writeJson(cur, { strokes: [stroke(7, 500), stroke(8, 700)], patches: [{ texts: [{ t: 'hello', x: 3000, y: 3000, s: 320, g: 1 }] }] });

  const summaryOnly = spawnSync(PY, [renderScript, '-', cur, '--prev', prev], { encoding: 'utf8' });
  assert.equal(summaryOnly.status, 0, summaryOnly.stderr);
  assert.deepEqual(JSON.parse(summaryOnly.stdout), { added: 2, removed: 1, unchanged: 1, strokes: 3 });

  const out = path.join(root, 'diff.png');
  const rendered = spawnSync(PY, [renderScript, out, cur, '--prev', prev, '--scale', '0.5'], { encoding: 'utf8' });
  assert.equal(rendered.status, 0, rendered.stderr);
  assert.deepEqual(pixel(out, 200, 250), [178, 178, 178]);
  assert.deepEqual(pixel(out, 200, 350), [24, 24, 24]);
  assert.deepEqual(pixel(out, 200, 450), [214, 44, 44]);
  assert.deepEqual(pixel(out, 50, 50), [255, 255, 255]);

  const clean = path.join(root, 'clean.png');
  assert.equal(spawnSync(PY, [renderScript, clean, cur, '--clean'], { encoding: 'utf8' }).status, 0);
  assert.deepEqual(pixel(clean, 200, 250), [24, 24, 24]);
  assert.deepEqual(pixel(clean, 200, 450), [255, 255, 255]);
});

test('site builder makes swair.dev root the post index and copies only chosen assets', (t) => {
  if (!hasPandoc()) return t.skip('pandoc is not installed');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-publish-site-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const repo = path.join(root, 'repo'), out = path.join(root, 'out');
  const postDir = path.join(repo, 'posts', 'thoughts-and-notes');
  fs.mkdirSync(path.join(postDir, 'assets'), { recursive: true });
  fs.writeFileSync(path.join(postDir, 'post.md'), '---\ntitle: "Thoughts & Notes"\n---\n\nFirst *para* with $x^2$.\n\n$$x^2 + y^2 = z^2$$\n\n```python\ndef hello(name):\n    return f"Hi, {name}"\n```\n\n<aside>\n\nA useful side note.\n\n</aside>\n\n![Flow](assets/flow.svg)\n');
  writeJson(path.join(postDir, 'meta.json'), { source: 'writings', published: '2026-09-01T10:00:00Z', updated: '2026-09-02T10:00:00Z' });
  fs.writeFileSync(path.join(postDir, 'assets', 'flow.svg'), '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h10"/></svg>');
  fs.mkdirSync(path.join(repo, 'posts', 'stale'), { recursive: true });

  const result = spawnSync(PY, [siteScript, repo, out], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), { posts: 1 });
  const index = fs.readFileSync(path.join(out, 'index.html'), 'utf8');
  assert.match(index, /Swair Shah \/ Blog/);
  assert.match(index, /href="\/posts\/thoughts-and-notes\/">Thoughts &amp; Notes<\/a>/);
  assert.doesNotMatch(index, />Notebook<|\/notebook\//);
  assert.match(index, /href="\/writing\.css"/);
  assert.match(index, /M21 12\.79A9 9 0 1 1 11\.21 3/);
  assert.match(index, /<circle cx="12" cy="12" r="5"\/>/);
  const post = fs.readFileSync(path.join(out, 'posts', 'thoughts-and-notes', 'index.html'), 'utf8');
  assert.match(post, /<math display="inline"/);
  assert.match(post, /<math display="block"/);
  assert.match(post, /class="sourceCode python"/);
  assert.match(post, /family=Google\+Sans\+Code/);
  assert.match(post, /src="assets\/flow\.svg"/);
  assert.match(post, /<aside>\s*<p>A useful side note\.<\/p>\s*<\/aside>/);
  assert.doesNotMatch(post, /Handwritten pages/);
  const writingCss = fs.readFileSync(path.join(out, 'writing.css'), 'utf8');
  assert.match(writingCss, /:root \{ font-size: 20px/);
  assert.match(writingCss, /\.theme-toggle \{[^}]*border: 0;[^}]*opacity: 0\.6/);
  assert.match(writingCss, /--font-family-code: "Google Sans Code"/);
  assert.match(writingCss, /math\[display="block"\]/);
  assert.match(writingCss, /\.post-body aside/);
  assert.match(writingCss, /position-anchor: --papier-sidenote/);
  assert.ok(fs.existsSync(path.join(out, 'posts', 'thoughts-and-notes', 'assets', 'flow.svg')));
  assert.ok(fs.existsSync(path.join(out, 'writing.css')));
  assert.ok(!fs.existsSync(path.join(out, 'posts', 'stale')));
});

test('website save script publishes the exact editor revision and preserves post metadata', (t) => {
  if (!hasPandoc()) return t.skip('pandoc is not installed');
  if (!hasGit()) return t.skip('git is not installed');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-website-save-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const backup = path.join(root, 'backup');
  const repo = path.join(backup, 'papier-publish', 'site');
  const postDir = path.join(repo, 'posts', 'field-notes');
  fs.mkdirSync(path.join(postDir, 'assets'), { recursive: true });
  fs.writeFileSync(path.join(postDir, 'post.md'), '---\ntitle: "Field Notes"\ndescription: kept\n---\n\nOriginal body.\n');
  writeJson(path.join(postDir, 'meta.json'), {
    id: 'field-notes', source: 'writings', title: 'Field Notes',
    published: '2026-09-01T10:00:00Z', updated: '2026-09-02T10:00:00Z', pages: [1, 3],
  });
  fs.writeFileSync(path.join(postDir, 'assets', 'diagram.svg'), '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h10"/></svg>');
  assert.equal(spawnSync('git', ['init', '-q', repo]).status, 0);
  assert.equal(spawnSync('git', ['-C', repo, '-c', 'user.name=test', '-c', 'user.email=test@example.com', 'add', '-A']).status, 0);
  assert.equal(spawnSync('git', ['-C', repo, '-c', 'user.name=test', '-c', 'user.email=test@example.com', 'commit', '-qm', 'init']).status, 0);

  const env = {
    ...process.env, HOME: root, PAPIER_BACKUP: backup, PAPIER_PY: PY,
    PUBLISH_NO_PUSH: '1', PUBLISH_SITE_URL: 'https://example.test',
  };
  const run = (name, markdown) => {
    const job = path.join(root, name); fs.mkdirSync(job, { recursive: true });
    fs.writeFileSync(path.join(job, 'slug.txt'), 'field-notes\n');
    fs.writeFileSync(path.join(job, 'post.md'), markdown);
    const result = spawnSync('/bin/bash', [saveScript, job], { encoding: 'utf8', env });
    return { job, ...result };
  };
  const edited = '---\ntitle: "Better Field Notes"\ndescription: kept\n---\n\nThe exact **edited** body.\n';
  const first = run('job1', edited);
  assert.equal(first.status, 0, first.stderr);
  assert.equal(fs.readFileSync(path.join(first.job, 'outcome.txt'), 'utf8'), 'published');
  assert.equal(fs.readFileSync(path.join(first.job, 'url.txt'), 'utf8'), 'https://example.test/posts/field-notes/');
  assert.equal(fs.readFileSync(path.join(postDir, 'post.md'), 'utf8'), edited);
  assert.ok(fs.existsSync(path.join(postDir, 'assets', 'diagram.svg')));
  const meta = JSON.parse(fs.readFileSync(path.join(postDir, 'meta.json'), 'utf8'));
  assert.equal(meta.title, 'Better Field Notes');
  assert.equal(meta.description, undefined);
  assert.equal(meta.source, 'writings');
  assert.equal(meta.published, '2026-09-01T10:00:00Z');
  assert.deepEqual(meta.pages, [1, 3]);
  assert.notEqual(meta.updated, '2026-09-02T10:00:00Z');
  assert.match(fs.readFileSync(path.join(backup, 'papier-publish', 'out', 'posts', 'field-notes', 'index.html'), 'utf8'), /The exact <strong>edited<\/strong> body/);
  assert.equal(Number(spawnSync('git', ['-C', repo, 'rev-list', '--count', 'HEAD'], { encoding: 'utf8' }).stdout.trim()), 2);

  const second = run('job2', edited);
  assert.equal(second.status, 0, second.stderr);
  assert.equal(fs.readFileSync(path.join(second.job, 'outcome.txt'), 'utf8'), 'unchanged');
  assert.equal(Number(spawnSync('git', ['-C', repo, 'rev-list', '--count', 'HEAD'], { encoding: 'utf8' }).stdout.trim()), 2);
});

test('website editor is linked in the shared header and supports preview plus save', () => {
  const ui = fs.readFileSync(websiteUi, 'utf8');
  const nav = fs.readFileSync(sharedNav, 'utf8');
  assert.match(nav, /\['website', '\/website\/'\]/);
  assert.match(ui, /id="title"/);
  assert.match(ui, /id="body"/);
  assert.match(ui, /id="preview"/);
  assert.match(ui, /website-preview/);
  assert.match(ui, /website-save/);
  assert.match(ui, /X-Papier-Editor/);
  assert.match(ui, /aria-controls="post-panel"/);
  assert.match(ui, /sidebar-hidden/);
  assert.match(ui, /e\.key\.toLowerCase\(\) === 'b'/);
  assert.match(ui, /metaKey \|\| e\.ctrlKey/);
  assert.equal((ui.match(/<\/body>/g) || []).length, 1, 'nginx nav injection must only match the real closing body');
});

test('publish script lets the agent create/update/delete posts and preserves notebook snapshots', (t) => {
  if (!hasPillow()) return t.skip('Pillow is not installed');
  if (!hasPandoc()) return t.skip('pandoc is not installed');
  if (!hasGit()) return t.skip('git is not installed');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-publish-script-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const backup = path.join(root, 'backup');
  const mirror = path.join(backup, 'papier', 'docs', 'nb-test');
  const inbound = path.join(backup, 'papier-inbound', 'docs', 'nb-test');
  writeJson(path.join(mirror, 'meta.json'), { v: 1, kind: 'notebook', title: 'Writings', folder: '' });
  writeJson(path.join(mirror, 'state.json'), { seq: [{ n: 1 }, { n: 2 }, { n: 3 }] });
  writeJson(path.join(mirror, 'ink', 'note-0001.json'), { strokes: [stroke(1, 500), stroke(2, 600)] });
  writeJson(path.join(mirror, 'ink', 'note-0002.json'), { strokes: [stroke(3, 500)] });
  writeJson(path.join(inbound, 'ink', 'note-0002.json'), { strokes: [stroke(3, 500), stroke(4, 700)] });

  const argsLog = path.join(root, 'pi-args.txt');
  const promptCopy = path.join(root, 'last-prompt.md');
  const fakePi = path.join(root, 'fake-pi.sh');
  executable(fakePi, `#!/bin/bash
set -e
printf '%s\\n' "$@" > "${argsLog}"
cp "$(printf '%s' "$3" | sed 's/^@//')" "${promptCopy}"
n=$(( $(cat "${root}/runs" 2>/dev/null || echo 0) + 1 )); echo "$n" > "${root}/runs"
mkdir -p posts/field-notes/assets
if [ ! -f posts/field-notes/post.md ]; then
  printf -- '%s\\n' '---' 'title: "Field Notes"' '---' '' 'First topic.' '![Flow](assets/flow.svg)' > posts/field-notes/post.md
  printf '%s\\n' '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h10"/></svg>' > posts/field-notes/assets/flow.svg
  action=create
else
  printf '\\nTranscribed run %s.\\n' "$n" >> posts/field-notes/post.md
  action=update
fi
if [ "$n" = 1 ] || [ "$n" = 2 ]; then pages='[1,2]'; else pages='[3]'; fi
printf '{"changes":[{"slug":"field-notes","action":"%s","pages":%s}]}\\n' "$action" "$pages" > decision.json
echo PUBLISHED
`);
  const env = {
    ...process.env, HOME: root, PAPIER_BACKUP: backup, PI_BIN: fakePi, PAPIER_PY: PY,
    PUBLISH_NO_PUSH: '1', PUBLISH_SITE_URL: 'https://example.test', PAPIER_PUBLISH_DOC_ID: 'nb-test',
  };
  const run = (name, mode) => {
    const job = path.join(root, name);
    fs.mkdirSync(job, { recursive: true });
    fs.writeFileSync(path.join(job, 'doc.txt'), 'nb-test\n');
    if (mode) fs.writeFileSync(path.join(job, 'mode.txt'), mode + '\n');
    const result = spawnSync('/bin/bash', [publishScript, job], { encoding: 'utf8', env });
    return { job, ...result, outcome: fs.existsSync(path.join(job, 'outcome.txt')) ? fs.readFileSync(path.join(job, 'outcome.txt'), 'utf8') : null };
  };
  const repo = path.join(backup, 'papier-publish', 'site');
  const out = path.join(backup, 'papier-publish', 'out');
  const commits = () => Number(spawnSync('git', ['-C', repo, 'rev-list', '--count', 'HEAD'], { encoding: 'utf8' }).stdout.trim());

  const first = run('job1');
  assert.equal(first.status, 0, first.stderr);
  assert.equal(first.outcome, 'published');
  assert.equal(fs.readFileSync(path.join(first.job, 'url.txt'), 'utf8'), 'https://example.test/');
  const args = fs.readFileSync(argsLog, 'utf8').trim().split('\n');
  assert.deepEqual(args.slice(0, 3), ['-p', '--no-session', '@' + path.join(first.job, 'prompt.md')]);
  assert.equal(args.length, 5);
  const prompt = fs.readFileSync(promptCopy, 'utf8');
  assert.match(prompt, /prefer a new post instead of mixing unrelated ideas/);
  assert.match(prompt, /create posts\/<slug>\/assets\/<name>\.svg/);
  assert.match(prompt, /crop and clean the relevant region/);
  assert.match(prompt, /Use a side note when the handwriting is a nonessential margin annotation/);
  assert.match(prompt, /<aside>/);
  assert.match(prompt, /display LaTeX as `\$\$\.\.\.\$\$`/);
  assert.match(prompt, /fenced\s+code blocks with a language identifier/);
  assert.match(prompt, /Treat any handwritten `\[do: instruction\]`/);
  assert.match(prompt, /Fulfill it at that exact point in the surrounding text/);
  assert.match(prompt, /never leave the `\[do: \.\.\.\]` text/);
  assert.match(prompt, /code block for an HTTP server/);
  assert.match(prompt, /SVG image of an owl/);
  assert.ok(fs.existsSync(path.join(repo, 'posts', 'field-notes', 'assets', 'flow.svg')));
  assert.ok(!fs.existsSync(path.join(repo, 'posts', 'field-notes', 'pages')));
  assert.ok(fs.existsSync(path.join(repo, 'sources', 'nb-test', 'snapshot', 'note-0001.json')));
  const meta1 = JSON.parse(fs.readFileSync(path.join(repo, 'posts', 'field-notes', 'meta.json'), 'utf8'));
  assert.equal(meta1.source, 'nb-test');
  assert.deepEqual(meta1.pages, [1, 2]);
  assert.match(fs.readFileSync(path.join(out, 'index.html'), 'utf8'), /posts\/field-notes/);
  assert.match(fs.readFileSync(path.join(out, 'posts', 'field-notes', 'index.html'), 'utf8'), /assets\/flow\.svg/);
  assert.equal(commits(), 2);

  fs.rmSync(argsLog);
  const same = run('job2');
  assert.equal(same.status, 0, same.stderr);
  assert.equal(same.outcome, 'unchanged');
  assert.ok(!fs.existsSync(argsLog));
  assert.equal(commits(), 2);

  writeJson(path.join(mirror, 'ink', 'note-0001.json'), { strokes: [stroke(1, 500), stroke(2, 600), stroke(9, 800)] });
  writeJson(path.join(inbound, 'ink', 'note-0002.json'), { strokes: [stroke(3, 500)] });
  const second = run('job3');
  assert.equal(second.status, 0, second.stderr);
  assert.match(fs.readFileSync(path.join(repo, 'posts', 'field-notes', 'post.md'), 'utf8'), /Transcribed run 2/);
  assert.equal(commits(), 3);

  writeJson(path.join(mirror, 'state.json'), { seq: [{ n: 1 }, { n: 3 }] });
  const deletedPage = run('job4');
  assert.equal(deletedPage.status, 0, deletedPage.stderr);
  assert.match(fs.readFileSync(promptCopy, 'utf8'), /page 3 .*DELETED/);
  assert.ok(!fs.existsSync(path.join(repo, 'sources', 'nb-test', 'snapshot', 'note-0002.json')));
  assert.equal(commits(), 4);

  writeJson(path.join(mirror, 'ink', 'note-0001.json'), { strokes: [stroke(1, 500), stroke(2, 600), stroke(9, 800), stroke(10, 900)] });
  executable(fakePi, `#!/bin/sh
printf -- '%s\\n' '---' 'title: "Field Notes"' '---' > posts/field-notes/post.md
printf '%s\\n' '{"changes":[{"slug":"field-notes","action":"update","pages":[1]}]}' > decision.json
`);
  const guarded = run('job5');
  assert.notEqual(guarded.status, 0);
  assert.match(guarded.stderr, /shrank by more than half/);
  assert.equal(commits(), 4);

  executable(fakePi, '#!/bin/sh\nexit 42\n');
  const agentFailed = run('job6');
  assert.notEqual(agentFailed.status, 0);
  assert.match(agentFailed.stderr, /agent failed \(exit 42\)/);
  assert.equal(commits(), 4);

  // A site-build failure rolls back both edited posts and the source snapshot,
  // so the same notebook diff can be retried safely.
  executable(fakePi, `#!/bin/sh
printf '\\nThis must roll back.\\n' >> posts/field-notes/post.md
printf '%s\\n' '{"changes":[{"slug":"field-notes","action":"update","pages":[1]}]}' > decision.json
`);
  const failSite = path.join(root, 'fail-site.sh');
  executable(failSite, '#!/bin/sh\nexit 9\n');
  env.PUBLISH_SITE = failSite;
  const buildFailed = run('job7');
  delete env.PUBLISH_SITE;
  assert.notEqual(buildFailed.status, 0);
  assert.doesNotMatch(fs.readFileSync(path.join(repo, 'posts', 'field-notes', 'post.md'), 'utf8'), /must roll back/);
  assert.equal(JSON.parse(fs.readFileSync(path.join(repo, 'sources', 'nb-test', 'snapshot', 'note-0001.json'))).strokes.length, 3);
  assert.equal(spawnSync('git', ['-C', repo, 'status', '--porcelain'], { encoding: 'utf8' }).stdout, '');
  assert.equal(commits(), 4);

  executable(fakePi, `#!/bin/sh
printf '%s\\n' '{"changes":[{"slug":"field-notes","action":"delete","pages":[]}]}' > decision.json
`);
  const postDeleted = run('job8');
  assert.equal(postDeleted.status, 0, postDeleted.stderr);
  assert.ok(!fs.existsSync(path.join(repo, 'posts', 'field-notes')));
  assert.equal(commits(), 5);

  const removed = run('job9', 'remove');
  assert.equal(removed.status, 0, removed.stderr);
  assert.ok(!fs.existsSync(path.join(repo, 'sources', 'nb-test')));
  assert.match(fs.readFileSync(path.join(out, 'index.html'), 'utf8'), /Nothing published yet/);
  assert.equal(commits(), 6);
});

test('publish API restricts publishing to Writings and reports the root URL', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-publish-api-'));
  const backup = path.join(root, 'backup');
  const publisher = path.join(root, 'fake-publish.js');
  const port = await freePort();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  writeJson(path.join(backup, 'papier', 'docs', 'writings', 'meta.json'), { kind: 'notebook', title: 'Writings' });
  writeJson(path.join(backup, 'papier', 'docs', 'other', 'meta.json'), { kind: 'notebook', title: 'Other' });

  executable(publisher, `#!/usr/bin/env node
const fs = require('fs'), path = require('path');
const job = process.argv[2], backup = '${backup}';
const mode = fs.readFileSync(path.join(job, 'mode.txt'), 'utf8').trim();
const post = path.join(backup, 'papier-publish', 'site', 'posts', 'first-topic');
if (mode === 'remove') fs.rmSync(post, { recursive: true, force: true });
else { fs.mkdirSync(post, { recursive: true }); fs.writeFileSync(path.join(post, 'meta.json'), JSON.stringify({ source: 'writings', title: 'First Topic', updated: '2026-09-03T00:00:00Z' })); }
fs.writeFileSync(path.join(job, 'outcome.txt'), mode === 'remove' ? 'removed' : 'published');
fs.writeFileSync(path.join(job, 'url.txt'), 'https://example.test/');
fs.writeFileSync(path.join(job, 'status.txt'), 'done');
`);
  const service = spawn(process.execPath, [uploadService], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_PUBLISH: publisher, PAPIER_PUBLISH_URL: 'https://example.test', PAPIER_AUTO_PUBLISH: '0', PAPIER_PORT: String(port) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let serviceErr = '';
  service.stderr.on('data', (chunk) => { serviceErr += chunk.toString(); });
  t.after(() => service.kill('SIGTERM'));
  await waitForService(service, port, () => serviceErr);
  const api = `http://127.0.0.1:${port}`;
  const submit = async (body) => {
    const response = await fetch(`${api}/publish`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    return { response, json: await response.json() };
  };
  const settle = async (job) => {
    for (let i = 0; i < 200; i++) {
      const status = await fetch(`${api}/publish-status?job=${job}`).then((r) => r.json());
      if (status.status !== 'running') return status;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    throw new Error('publish job did not settle');
  };

  assert.equal((await submit({ id: 'other' })).response.status, 404);
  assert.deepEqual(await fetch(`${api}/publish-info?id=writings`).then((r) => r.json()), { ok: true, id: 'writings', published: false, url: null });
  const started = await submit({ id: 'writings' });
  const done = await settle(started.json.job);
  assert.equal(done.status, 'done', serviceErr);
  const info = await fetch(`${api}/publish-info?id=writings`).then((r) => r.json());
  assert.equal(info.published, true);
  assert.equal(info.url, 'https://example.test/');
  assert.equal(info.title, '1 post');
  const removal = await submit({ id: 'writings', remove: true });
  assert.equal((await settle(removal.json.job)).outcome, 'removed');
});

test('website API previews Markdown, rejects stale edits, and publishes a save', async (t) => {
  if (!hasPandoc()) return t.skip('pandoc is not installed');
  if (!hasGit()) return t.skip('git is not installed');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-website-api-'));
  const backup = path.join(root, 'backup');
  const repo = path.join(backup, 'papier-publish', 'site');
  const postDir = path.join(repo, 'posts', 'first-topic');
  const port = await freePort();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(postDir, { recursive: true });
  fs.writeFileSync(path.join(postDir, 'post.md'), '---\ntitle: "First Topic"\nsummary: preserve me\n---\n\nOriginal.\n');
  writeJson(path.join(postDir, 'meta.json'), {
    id: 'first-topic', source: 'writings', title: 'First Topic',
    published: '2026-09-01T00:00:00Z', updated: '2026-09-02T00:00:00Z', pages: [1],
  });
  assert.equal(spawnSync('git', ['init', '-q', repo]).status, 0);
  assert.equal(spawnSync('git', ['-C', repo, '-c', 'user.name=test', '-c', 'user.email=test@example.com', 'add', '-A']).status, 0);
  assert.equal(spawnSync('git', ['-C', repo, '-c', 'user.name=test', '-c', 'user.email=test@example.com', 'commit', '-qm', 'init']).status, 0);

  const service = spawn(process.execPath, [uploadService], {
    env: {
      ...process.env, HOME: root, PAPIER_BACKUP: backup, PAPIER_PORT: String(port),
      PAPIER_AUTO_PUBLISH: '0', PAPIER_PUBLISH_SAVE: saveScript,
      PAPIER_PUBLISH_URL: 'https://example.test', PUBLISH_SITE_URL: 'https://example.test',
      PUBLISH_SITE: siteScript, PUBLISH_NO_PUSH: '1', PAPIER_PY: PY,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let serviceErr = '';
  service.stderr.on('data', (chunk) => { serviceErr += chunk.toString(); });
  t.after(() => service.kill('SIGTERM'));
  await waitForService(service, port, () => serviceErr);
  const api = `http://127.0.0.1:${port}`;
  const headers = { 'Content-Type': 'application/json', 'X-Papier-Editor': '1' };

  const list = await fetch(`${api}/website-posts`).then((r) => r.json());
  assert.deepEqual(list.posts.map((p) => [p.slug, p.title]), [['first-topic', 'First Topic']]);
  const opened = await fetch(`${api}/website-post?slug=first-topic`).then((r) => r.json());
  assert.equal(opened.body, 'Original.\n');
  assert.match(opened.revision, /^[a-f0-9]{64}$/);
  assert.equal(opened.url, 'https://example.test/posts/first-topic/');

  const forbidden = await fetch(`${api}/website-preview`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ body: '**hello**' }),
  });
  assert.equal(forbidden.status, 403);
  const preview = await fetch(`${api}/website-preview`, {
    method: 'POST', headers, body: JSON.stringify({ body: '**hello**' }),
  }).then((r) => r.json());
  assert.match(preview.html, /<strong>hello<\/strong>/);

  const startedResponse = await fetch(`${api}/website-save`, {
    method: 'POST', headers,
    body: JSON.stringify({ slug: 'first-topic', title: 'Edited Topic', body: 'Saved from the **website**.\n', revision: opened.revision }),
  });
  assert.equal(startedResponse.status, 202);
  const started = await startedResponse.json();
  let done;
  for (let i = 0; i < 200; i++) {
    done = await fetch(`${api}/website-save-status?job=${started.job}`).then((r) => r.json());
    if (done.status !== 'running') break;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.equal(done.status, 'done', serviceErr + '\n' + JSON.stringify(done));
  assert.equal(done.outcome, 'published');
  assert.equal(done.url, 'https://example.test/posts/first-topic/');
  assert.match(done.revision, /^[a-f0-9]{64}$/);
  assert.equal(done.title, 'Edited Topic');
  assert.ok(done.postUpdated);

  const saved = await fetch(`${api}/website-post?slug=first-topic`).then((r) => r.json());
  assert.equal(saved.title, 'Edited Topic');
  assert.equal(saved.body, 'Saved from the **website**.\n');
  assert.match(fs.readFileSync(path.join(postDir, 'post.md'), 'utf8'), /summary: preserve me/);
  assert.match(fs.readFileSync(path.join(backup, 'papier-publish', 'out', 'posts', 'first-topic', 'index.html'), 'utf8'), /Saved from the <strong>website<\/strong>/);

  const stale = await fetch(`${api}/website-save`, {
    method: 'POST', headers,
    body: JSON.stringify({ slug: 'first-topic', title: 'Stale', body: 'No.', revision: opened.revision }),
  });
  assert.equal(stale.status, 409);
  assert.match((await stale.json()).error, /changed since you opened/);
});

test('Writings changes auto-publish after idle; other notebooks do not', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-auto-publish-'));
  const backup = path.join(root, 'backup');
  const publisher = path.join(root, 'fake-publish.js');
  const runs = path.join(root, 'runs.txt');
  const port = await freePort();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const id of ['writings', 'other']) {
    writeJson(path.join(backup, 'papier', 'docs', id, 'meta.json'), { kind: 'notebook', title: id });
    writeJson(path.join(backup, 'papier', 'docs', id, 'state.json'), { seq: [{ n: 1 }] });
    writeJson(path.join(backup, 'papier', 'docs', id, 'ink', 'note-0001.json'), { strokes: [] });
  }
  executable(publisher, `#!/usr/bin/env node
const fs = require('fs'), path = require('path'); const job = process.argv[2];
let n = 0; try { n = Number(fs.readFileSync('${runs}', 'utf8')) || 0; } catch (_) {}
fs.writeFileSync('${runs}', String(n + 1)); fs.writeFileSync(path.join(job, 'outcome.txt'), 'unchanged'); fs.writeFileSync(path.join(job, 'url.txt'), 'https://example.test/');
`);
  const service = spawn(process.execPath, [uploadService], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_PUBLISH: publisher, PAPIER_PUBLISH_IDLE_MS: '60', PAPIER_PUBLISH_SCAN_MS: '20', PAPIER_PORT: String(port) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let serviceErr = '';
  service.stderr.on('data', (chunk) => { serviceErr += chunk.toString(); });
  t.after(() => service.kill('SIGTERM'));
  await waitForService(service, port, () => serviceErr);
  const waitForRuns = async (want) => {
    for (let i = 0; i < 100; i++) {
      let got = 0; try { got = Number(fs.readFileSync(runs, 'utf8')) || 0; } catch (_) {}
      if (got >= want) return;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    throw new Error(`wanted ${want} auto-publishes: ${serviceErr}`);
  };
  await waitForRuns(1);
  writeJson(path.join(backup, 'papier', 'docs', 'writings', 'ink', 'note-0001.json'), { strokes: [stroke(1, 500)] });
  await waitForRuns(2);
  writeJson(path.join(backup, 'papier', 'docs', 'other', 'ink', 'note-0001.json'), { strokes: [stroke(2, 600)] });
  await new Promise((resolve) => setTimeout(resolve, 200));
  assert.equal(Number(fs.readFileSync(runs, 'utf8')), 2);
});
