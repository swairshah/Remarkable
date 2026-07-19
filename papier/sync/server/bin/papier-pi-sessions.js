// papier-pi-sessions.js — remote pi for papier-ios.
//
// One session per open document. The iPad reports pauses/nudges over
// HTTP; each turn is a one-shot `pi -p --continue` run on the VM whose
// canvas tools (ext/papier-canvas.ts, UNCHANGED from the tablet) come
// back over a per-session unix socket speaking the tablet's exact tool
// protocol. Tool commands execute in the papier-cloud-canvas binary —
// the same libreink code the tablet runs — against the inbound overlay,
// so pi's ink flows to every device through the ordinary sync.
//
//   POST /pi/open?id=      -> ensure session {mode, font, busy}
//   POST /pi/page?id=&page= -> the iPad's current page (goto targeting)
//   POST /pi/pause?id=&page= -> "the user paused here" turn (auto mode)
//   POST /pi/nudge?id=&page= -> explicit poke (always allowed)
//   POST /pi/mode?id=&mode=auto|quiet
//   POST /pi/font?id=&font=serif|script|sans|garamond
//   GET  /pi/events?id=&since=N -> {busy, mode, font, events:[{id,type,...}]}
//
// Events: turn (start/end), patch {page}, seq {}, goto {page}, notice {text}.

'use strict';
const fs = require('fs');
const net = require('net');
const os = require('os');
const path = require('path');
const { spawn } = require('child_process');

const CANVAS_BIN = process.env.PAPIER_CANVAS_BIN || '/home/exedev/bin/papier-cloud-canvas';
const CANVAS_EXT = process.env.PAPIER_CANVAS_EXT || '/home/exedev/bin/papier-canvas.ts';
const PROMPT_MD = process.env.PAPIER_PI_PROMPT || '/home/exedev/bin/papier-cloud-prompt.md';
const PI_BIN = process.env.PI_BIN || 'pi';
const PI_HOME = process.env.PAPIER_PI_HOME || path.join(os.homedir(), 'papier-pi');
const TURN_TIMEOUT_MS = 240_000;
const IDLE_MS = 30 * 60_000;
const FONTS = ['serif', 'script', 'sans', 'garamond'];

// The service runs under systemd without a login env; pi's API keys live
// in ~/.env (same convention as papier-compose.sh).
function dotEnv() {
  const out = {};
  try {
    for (const line of fs.readFileSync(path.join(os.homedir(), '.env'), 'utf8').split('\n')) {
      const m = line.match(/^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=(.*)$/);
      if (m) out[m[1]] = m[2].replace(/^["']|["']$/g, '');
    }
  } catch (_) {}
  return out;
}

function createPiSessions({ mirrorDocs, inboundDocs }) {
  const sessions = new Map(); // docId -> session

  function readJson(file) {
    try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch (_) { return null; }
  }
  function docFile(id, rel) {
    const o = path.join(inboundDocs, id, rel);
    if (fs.existsSync(o)) return o;
    return path.join(mirrorDocs, id, rel);
  }
  function docExists(id) {
    return fs.existsSync(path.join(mirrorDocs, id, 'meta.json'))
        || fs.existsSync(path.join(inboundDocs, id, 'meta.json'));
  }
  function docState(id) {
    const meta = readJson(docFile(id, 'meta.json')) || {};
    const st = readJson(docFile(id, 'state.json')) || {};
    let seq = Array.isArray(st.seq) && st.seq.length ? st.seq : null;
    if (!seq) {
      seq = meta.kind === 'notebook'
        ? [{ n: 1 }]
        : Array.from({ length: meta.pages || 0 }, (_, p) => ({ p }));
    }
    return { meta, seq };
  }

  /* ---- the cloud-canvas child (one per session, line-oriented) -------- */

  function startCanvas(s) {
    if (s.canvas) { try { s.canvas.kill(); } catch (_) {} }
    s.canvas = spawn(CANVAS_BIN, [], {
      env: {
        ...process.env,
        PAPIER_CLOUD_MIRROR: path.join(mirrorDocs, s.id),
        PAPIER_CLOUD_OVERLAY: path.join(inboundDocs, s.id),
        PAPIER_CLOUD_FONT: s.font,
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    s.canvasQueue = [];
    let buf = '';
    s.canvas.stdout.on('data', (d) => {
      buf += d.toString('utf8');
      let nl;
      while ((nl = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, nl);
        buf = buf.slice(nl + 1);
        const next = s.canvasQueue.shift();
        if (next) {
          try { next.resolve(JSON.parse(line)); } catch (e) { next.reject(e); }
        }
      }
    });
    s.canvas.on('exit', () => {
      for (const w of s.canvasQueue.splice(0)) w.reject(new Error('canvas exited'));
      s.canvas = null;
    });
  }

  function canvasCall(s, cmd) {
    if (!s.canvas) startCanvas(s);
    return new Promise((resolve, reject) => {
      s.canvasQueue.push({ resolve, reject });
      s.canvas.stdin.write(JSON.stringify(cmd) + '\n');
      setTimeout(() => {
        const i = s.canvasQueue.findIndex((w) => w.resolve === resolve);
        if (i >= 0) { s.canvasQueue.splice(i, 1); reject(new Error('canvas timeout')); }
      }, 30_000).unref?.();
    });
  }

  /* ---- events ---------------------------------------------------------- */

  function emit(s, ev) {
    ev.id = ++s.nextEvent;
    s.events.push(ev);
    if (s.events.length > 200) s.events.splice(0, s.events.length - 200);
  }

  /* ---- the per-session tool socket (tablet protocol) ------------------- */

  async function handleTool(s, cmd) {
    s.lastUsed = Date.now();
    const { seq } = docState(s.id);
    if (cmd.cmd === 'goto') {
      const p = Number(cmd.page);
      if (!Number.isInteger(p) || p < 1 || p > seq.length) {
        return { ok: false, error: `no page ${cmd.page} (document has ${seq.length})` };
      }
      emit(s, { type: 'goto', page: p });
      const e = seq[p - 1];
      return { ok: true, page: p, page_count: seq.length,
               label: e.p != null ? `p.${e.p + 1}` : 'note', layout: '' };
    }
    // tools without an explicit page act on the iPad's current page
    const injected = { ...cmd };
    if (injected.page == null && cmd.cmd !== 'insert_note' && cmd.cmd !== 'page_text') {
      injected.page = s.page;
    }
    if (cmd.cmd === 'insert_note' && injected.after_page == null) injected.after_page = s.page;
    const r = await canvasCall(s, injected);
    if (r && r.ok) {
      if (cmd.cmd === 'draw' || cmd.cmd === 'underline' || cmd.cmd === 'erase') {
        emit(s, { type: 'patch', page: r.page || injected.page });
      }
      if (cmd.cmd === 'insert_note') emit(s, { type: 'seq', page: r.page });
    }
    return r;
  }

  function startSocket(s) {
    try { fs.unlinkSync(s.sock); } catch (_) {}
    s.server = net.createServer((conn) => {
      let buf = '';
      conn.on('data', async (d) => {
        buf += d.toString('utf8');
        const nl = buf.indexOf('\n');
        if (nl < 0) return;
        const line = buf.slice(0, nl);
        buf = '';
        let resp;
        try {
          resp = await handleTool(s, JSON.parse(line));
        } catch (e) {
          resp = { ok: false, error: String(e.message || e) };
        }
        try { conn.write(JSON.stringify(resp) + '\n'); } catch (_) {}
      });
      conn.on('error', () => {});
    });
    s.server.listen(s.sock);
  }

  /* ---- pi turns --------------------------------------------------------- */

  async function pauseMessage(s, page, kind) {
    const { meta, seq } = docState(s.id);
    const e = seq[page - 1] || {};
    const pageKind = e.p != null ? `a printed BOOK page (p.${e.p + 1} of the PDF)` : 'a NOTEBOOK page';
    let text = '';
    if (e.p != null) {
      const t = await canvasCall(s, { cmd: 'page_text', from: page, to: page }).catch(() => null);
      if (t && t.ok) text = `\n\nExtracted text of this page:\n${t.text}`;
    }
    const lead = kind === 'nudge'
      ? 'The user NUDGED you — they want you to engage with this page now.'
      : 'The user paused writing.';
    return (
      `${lead} They are on page ${page} of ${seq.length} of "${meta.title || s.id}" — ${pageKind}. ` +
      `The attached image is the current page at half scale (multiply image coordinates by 2 ` +
      `for page coordinates); your earlier ink appears gray.${text}`
    );
  }

  async function runTurn(s, page, kind) {
    if (s.busy) { s.pending = { page, kind }; return; }
    s.busy = true;
    s.page = page;
    emit(s, { type: 'turn', state: 'start' });
    try {
      const view = await canvasCall(s, { cmd: 'view', page });
      const img = path.join(os.tmpdir(), `papier-pi-${s.key}.png`);
      if (view && view.ok) fs.writeFileSync(img, Buffer.from(view.png_base64, 'base64'));
      const msg = await pauseMessage(s, page, kind);

      const args = [
        '-p', '--continue',
        '--session-dir', s.sessionDir,
        '--no-extensions', '-e', CANVAS_EXT,
        '--append-system-prompt', PROMPT_MD,
      ];
      if (view && view.ok) args.push('@' + img);
      args.push(msg);

      const child = spawn(PI_BIN, args, {
        cwd: s.workDir,
        env: { ...process.env, ...dotEnv(), PAPIER_SOCK: s.sock },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      s.pi = child;
      let out = '';
      child.stdout.on('data', (d) => { out += d.toString('utf8'); if (out.length > 20000) out = out.slice(-10000); });
      child.stderr.on('data', () => {});
      const killer = setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, TURN_TIMEOUT_MS);
      await new Promise((resolve) => child.once('exit', resolve));
      clearTimeout(killer);
      s.pi = null;
      const said = out.trim();
      if (said && said.toLowerCase() !== 'pass' && said.length < 600) {
        emit(s, { type: 'notice', text: said });
      }
    } catch (e) {
      emit(s, { type: 'notice', text: `pi error: ${String(e.message || e)}` });
    } finally {
      s.busy = false;
      emit(s, { type: 'turn', state: 'end' });
      const p = s.pending;
      s.pending = null;
      if (p) runTurn(s, p.page, p.kind);
    }
  }

  /* ---- sessions --------------------------------------------------------- */

  function ensure(id) {
    let s = sessions.get(id);
    if (s) { s.lastUsed = Date.now(); return s; }
    const key = id.replace(/[^a-z0-9-]/g, '').slice(0, 40) || 'doc';
    s = {
      id, key,
      sock: path.join(os.tmpdir(), `papier-pi-${key}.sock`),
      sessionDir: path.join(PI_HOME, id, 'sessions'),
      workDir: path.join(PI_HOME, id, 'work'),
      events: [], nextEvent: 0,
      busy: false, pending: null, pi: null,
      mode: 'auto', font: 'serif', page: 1,
      canvas: null, canvasQueue: [],
      lastUsed: Date.now(),
    };
    fs.mkdirSync(s.sessionDir, { recursive: true });
    fs.mkdirSync(s.workDir, { recursive: true });
    startSocket(s);
    startCanvas(s);
    sessions.set(id, s);
    return s;
  }

  function drop(s) {
    try { s.server?.close(); } catch (_) {}
    try { fs.unlinkSync(s.sock); } catch (_) {}
    try { s.canvas?.kill(); } catch (_) {}
    try { s.pi?.kill('SIGKILL'); } catch (_) {}
    sessions.delete(s.id);
  }

  setInterval(() => {
    for (const s of sessions.values()) {
      if (!s.busy && Date.now() - s.lastUsed > IDLE_MS) drop(s);
    }
  }, 60_000).unref?.();

  /* ---- HTTP surface ------------------------------------------------------ */

  function json(res, code, obj) {
    res.writeHead(code, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(obj));
  }

  function state(s) {
    return { ok: true, busy: s.busy, mode: s.mode, font: s.font };
  }

  // Returns true when the request was handled.
  function handle(req, res, p, u) {
    if (!p.startsWith('/pi/')) return false;
    const id = u.searchParams.get('id') || '';
    if (!/^[a-z0-9][a-z0-9_-]{0,100}$/.test(id) || !docExists(id)) {
      json(res, 404, { ok: false, error: 'unknown doc' });
      return true;
    }
    const s = ensure(id);
    const page = Math.max(1, parseInt(u.searchParams.get('page'), 10) || s.page);

    if (req.method === 'GET' && p === '/pi/events') {
      const since = parseInt(u.searchParams.get('since'), 10) || 0;
      json(res, 200, { ...state(s), events: s.events.filter((e) => e.id > since) });
      return true;
    }
    if (req.method !== 'POST') { json(res, 405, { ok: false, error: 'POST required' }); return true; }

    switch (p) {
      case '/pi/open':
        json(res, 200, state(s));
        return true;
      case '/pi/page':
        s.page = page;
        json(res, 200, state(s));
        return true;
      case '/pi/pause':
        if (s.mode === 'quiet') { json(res, 200, { ...state(s), skipped: 'quiet' }); return true; }
        runTurn(s, page, 'pause');
        json(res, 200, state(s));
        return true;
      case '/pi/nudge':
        runTurn(s, page, 'nudge');
        json(res, 200, state(s));
        return true;
      case '/pi/mode': {
        const m = u.searchParams.get('mode');
        if (m === 'auto' || m === 'quiet') s.mode = m;
        json(res, 200, state(s));
        return true;
      }
      case '/pi/font': {
        const f = u.searchParams.get('font');
        if (FONTS.includes(f)) { s.font = f; startCanvas(s); }
        json(res, 200, state(s));
        return true;
      }
      default:
        json(res, 404, { ok: false, error: 'unknown pi endpoint' });
        return true;
    }
  }

  return { handle };
}

module.exports = { createPiSessions };
