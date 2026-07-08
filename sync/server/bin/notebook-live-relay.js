#!/usr/bin/env node
/*
 * notebook live relay — bridges the tablet's stroke stream to browsers.
 *
 *   tablet (notebook app, LIVE toggled on)
 *     └─ ssh exedev@remarkable.exe.xyz notebook-live-ingest.sh
 *          └─ raw TCP JSONL -> 127.0.0.1:8092  (INGEST, localhost only)
 *   browsers
 *     └─ GET /notebook/live/events  (nginx -> 127.0.0.1:8091/events, SSE)
 *
 * The relay is deliberately dumb: it fans every ingested line out to every
 * SSE client verbatim. The only state it keeps is (a) whether a tablet is
 * currently connected ("live"), (b) the current page, and (c) a replay
 * buffer of events since the last page change so a browser that connects
 * mid-scribble still sees the strokes drawn so far.
 *
 * Runs as a systemd service (notebook-live-relay.service). No deps.
 */
'use strict';

const http = require('http');
const net = require('net');

const SSE_PORT = 8091;
const INGEST_PORT = 8092;
const REPLAY_MAX = 4000;   /* events kept for late joiners (current page) */

let clients = [];          /* SSE responses */
let ingests = 0;           /* connected tablet pipes */
let replay = [];           /* events since last page change */
let curPage = null;

function broadcast(line) {
  const msg = 'data: ' + line + '\n\n';
  clients = clients.filter((res) => {
    try { res.write(msg); return true; } catch (e) { return false; }
  });
}

function track(line) {
  let ev;
  try { ev = JSON.parse(line); } catch (e) { return; }
  if (ev.t === 'page' || ev.t === 'hi') {
    if (ev.t === 'page') curPage = ev.n;
    else if (typeof ev.page === 'number') curPage = ev.page;
    replay = [];
  } else if (ev.t === 's' || ev.t === 'ai' || ev.t === 'rub' || ev.t === 'st') {
    replay.push(line);
    if (replay.length > REPLAY_MAX) replay.splice(0, replay.length - REPLAY_MAX);
  }
}

function liveState() {
  return JSON.stringify({ t: 'live', on: ingests > 0, page: curPage });
}

/* ---- ingest: raw TCP JSONL from the tablet's ssh pipe ---- */
const ingest = net.createServer((sock) => {
  ingests++;
  console.log(`ingest connected (${ingests})`);
  broadcast(liveState());
  let buf = '';
  sock.on('data', (d) => {
    buf += d.toString('utf8');
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim();
      buf = buf.slice(i + 1);
      if (!line) continue;
      track(line);
      broadcast(line);
    }
  });
  const drop = () => {
    if (!sock._dropped) {
      sock._dropped = true;
      ingests = Math.max(0, ingests - 1);
      console.log(`ingest closed (${ingests})`);
      broadcast(liveState());
    }
  };
  sock.on('close', drop);
  sock.on('error', drop);
});
ingest.listen(INGEST_PORT, '127.0.0.1');

/* ---- SSE out to browsers ---- */
const sse = http.createServer((req, res) => {
  if (req.url === '/health') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ clients: clients.length, live: ingests > 0, page: curPage, replay: replay.length }));
    return;
  }
  if (req.url !== '/events') {
    res.writeHead(404);
    res.end();
    return;
  }
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache',
    'Connection': 'keep-alive',
    'X-Accel-Buffering': 'no',
  });
  res.write('retry: 3000\n\n');
  res.write('data: ' + liveState() + '\n\n');
  for (const line of replay) res.write('data: ' + line + '\n\n');
  clients.push(res);
  req.on('close', () => { clients = clients.filter((c) => c !== res); });
});
sse.listen(SSE_PORT, '127.0.0.1');

setInterval(() => {
  clients = clients.filter((res) => {
    try { res.write(': ping\n\n'); return true; } catch (e) { return false; }
  });
}, 15000);

console.log(`notebook-live-relay: sse :${SSE_PORT}, ingest :${INGEST_PORT}`);
