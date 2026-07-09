// alt-ui inbound upload service (node, no deps), on 127.0.0.1:8093.
// nginx proxies POST /alt-ui/upload here. A dropped PDF is rendered into a
// book bundle under alt-ui-inbound/docs/<slug>/; the web viewer lists that
// dir alongside the tablet mirror (tagged "pending"), and once the tablet's
// reverse-rsync pull is live it moves inbound docs onto the device.
//
// Inbound is a SEPARATE dir from the mirror on purpose: the tablet's
// outbound rsync uses --delete, so writing straight into the mirror would
// be wiped on the next device push. New docs get fresh ids => no conflicts.
'use strict';
const http = require('http');
const fs = require('fs');
const path = require('path');
const { execFile } = require('child_process');

const INBOX = '/home/exedev/remarkable-backup/alt-ui-inbound';
const INCOMING = path.join(INBOX, 'incoming');
const DOCS = path.join(INBOX, 'docs');
fs.mkdirSync(INCOMING, { recursive: true });
fs.mkdirSync(DOCS, { recursive: true });

const MAX_BYTES = 200 * 1024 * 1024;

function slugify(name) {
  return name.toLowerCase().replace(/\.pdf$/i, '')
    .replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'doc';
}

function freeDir(slug) {
  let out = path.join(DOCS, slug), n = 1;
  while (fs.existsSync(out)) { n++; out = path.join(DOCS, `${slug}-${n}`); }
  return out;
}

http.createServer((req, res) => {
  if (req.method === 'GET' && req.url === '/health') {
    res.writeHead(200); res.end('ok'); return;
  }
  if (req.method !== 'POST' || req.url !== '/upload') {
    res.writeHead(404); res.end('not found'); return;
  }
  const name = String(req.headers['x-filename'] || 'upload.pdf').slice(0, 200);
  if (!/\.pdf$/i.test(name)) {
    res.writeHead(415); res.end('only PDF uploads are supported right now'); return;
  }
  const chunks = []; let size = 0, aborted = false;
  req.on('data', (c) => {
    size += c.length;
    if (size > MAX_BYTES) { aborted = true; res.writeHead(413); res.end('file too large'); req.destroy(); return; }
    chunks.push(c);
  });
  req.on('end', () => {
    if (aborted) return;
    const buf = Buffer.concat(chunks);
    const tmp = path.join(INCOMING, `${Date.now()}-${name.replace(/[^\w.\-]/g, '_')}`);
    fs.writeFileSync(tmp, buf);
    const out = freeDir(slugify(name));
    const title = name.replace(/\.pdf$/i, '');
    execFile('/home/exedev/bin/alt-ui-render.sh', [tmp, out, title],
      { timeout: 180000 }, (err, so, se) => {
        fs.unlink(tmp, () => {});
        if (err) {
          console.error('render failed:', se || err.message);
          res.writeHead(500); res.end('render failed: ' + (se || err.message)); return;
        }
        console.log('rendered', out);
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true, id: path.basename(out) }));
      });
  });
}).listen(8093, '127.0.0.1', () => console.log('alt-ui-upload listening on 127.0.0.1:8093'));
