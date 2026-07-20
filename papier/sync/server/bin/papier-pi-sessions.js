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
// The tablet's context solution, reused verbatim: transcribe (image->text
// swap + page dedup + recall_page, so sessions never bloat with page PNGs)
// and per-turn metrics.
const TRANSCRIBE_EXT = process.env.PAPIER_TRANSCRIBE_EXT || '/home/exedev/bin/papier-transcribe.ts';
const METRICS_EXT = process.env.PAPIER_METRICS_EXT || '/home/exedev/bin/papier-metrics.ts';
const PROMPT_MD = process.env.PAPIER_PI_PROMPT || '/home/exedev/bin/papier-cloud-prompt.md';
const PI_BIN = process.env.PI_BIN || 'pi';
const PI_PROVIDER = process.env.PAPIER_PI_PROVIDER || 'openai-codex';
// Use the exact user-selected Codex model. `:low` made the cloud companion
// noticeably shallower than the tablet agent.
const PI_MODEL = process.env.PAPIER_PI_MODEL || 'gpt-5.6-luna';
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
    // The font picker is authoritative. Models often copy an old explicit
    // font-family="script" from session history, which otherwise overrides GA.
    // Removing it makes cloud-canvas apply PAPIER_CLOUD_FONT consistently.
    if (cmd.cmd === 'draw' && typeof injected.svg === 'string') {
      injected.svg = injected.svg.replace(/\s+font-family\s*=\s*(["'])[^"']*\1/gi, '');
    }
    if (injected.page == null && cmd.cmd !== 'insert_note' && cmd.cmd !== 'page_text') {
      injected.page = s.page;
    }
    if (cmd.cmd === 'insert_note' && injected.after_page == null) injected.after_page = s.page;
    const r = await canvasCall(s, injected);
    if (r && r.ok) {
      s.turnActivity = true;
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

  async function pauseMessage(s, page, kind, view) {
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
    const patches = Array.isArray(view && view.patches) && view.patches.length
      ? view.patches.map((p) => `#${p.id} at ${JSON.stringify(p.bbox)}`).join(', ')
      : 'none';
    const layout = view && view.layout ? view.layout : 'Use the visible free space in the page image.';
    return (
      `${lead} They are on page ${page} of ${seq.length} of "${meta.title || s.id}" — ${pageKind}. ` +
      `The attached image is the current page at half scale (multiply image coordinates by 2 ` +
      `for page coordinates); your earlier ink appears gray.${text}\n\n` +
      `Your existing patches here: ${patches}. Measured layout (page coordinates — trust these numbers): ${layout}`
    );
  }

  // RESIDENT pi, exactly like the tablet: one `pi --mode rpc` child per
  // session, JSONL over stdin/stdout (libreink-pi's protocol). Staying
  // alive between turns is what lets papier-transcribe's post-answer fork
  // land — the image→text compression that keeps long sessions fast.
  function ensurePi(s) {
    if (s.pi && s.pi.exitCode === null) return s.pi;
    const resumed = fs.existsSync(s.sessionDir)
      && fs.readdirSync(s.sessionDir).some((f) => f.endsWith('.jsonl'));
    const args = [
      '--mode', 'rpc',
      '--session-dir', s.sessionDir,
      '--name', 'papier-cloud',
      '--append-system-prompt', PROMPT_MD,
      '--no-extensions', '-e', CANVAS_EXT,
    ];
    // Continued sessions pin their birth model; force the configured one
    // (the user's Codex subscription) even on resume.
    // :low thinking — margin notes, not dissertations; keeps turns snappy.
    args.push('--provider', PI_PROVIDER, '--model', PI_MODEL);
    // Context hooks run in extension order. Transcription must compact page
    // images before metrics observes the payload, matching the tablet path.
    if (fs.existsSync(TRANSCRIBE_EXT)) args.push('-e', TRANSCRIBE_EXT);
    if (fs.existsSync(METRICS_EXT)) args.push('-e', METRICS_EXT);
    if (resumed) args.push('--continue');

    const child = spawn(PI_BIN, args, {
      cwd: s.workDir,
      env: {
        ...process.env, ...dotEnv(),
        PAPIER_SOCK: s.sock,
        PAPIER_TRANSCRIBE_STORE: path.join(PI_HOME, 'transcriptions.json'),
        PAPIER_METRICS: path.join(PI_HOME, s.id, 'metrics.jsonl'),
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    const errLog = path.join(PI_HOME, s.id, 'turn-stderr.log');
    child.stderr.on('data', (d) => {
      try {
        fs.appendFileSync(errLog, d);
        if (fs.statSync(errLog).size > 200_000) fs.truncateSync(errLog, 0);
      } catch (_) {}
    });
    let buf = '';
    child.stdout.on('data', (d) => {
      buf += d.toString('utf8');
      let nl;
      while ((nl = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (!line) continue;
        try { handleRpc(s, JSON.parse(line)); } catch (_) {}
      }
    });
    child.on('exit', (code) => {
      if (s.pi === child) s.pi = null;
      if (s.busy) {
        s.busy = false;
        clearTimeout(s.watchdog);
        emit(s, { type: 'notice', text: `[pi exited (${code}); next turn restarts it]` });
        emit(s, { type: 'turn', state: 'end' });
      }
    });
    s.pi = child;
    return child;
  }

  function handleRpc(s, v) {
    switch (v.type) {
      case 'agent_start':
        s.busy = true;
        s.turnText = '';
        s.turnActivity = false;
        emit(s, { type: 'turn', state: 'start' });
        break;
      case 'message_update': {
        const ev = v.assistantMessageEvent || {};
        if (ev.type === 'text_delta' && typeof ev.delta === 'string') s.turnText += ev.delta;
        break;
      }
      case 'extension_ui_request':
        // no keyboard in headless mode — dismiss dialogs like the tablet
        if (['select', 'confirm', 'input', 'editor'].includes(v.method)) {
          try { s.pi.stdin.write(JSON.stringify({ type: 'extension_ui_response', id: v.id, cancelled: true }) + '\n'); } catch (_) {}
        } else if (v.method === 'notify') {
          // extension warnings (e.g. transcribe auth problems) — keep them
          try {
            fs.appendFileSync(path.join(PI_HOME, s.id, 'turn-stderr.log'),
              `[notify] ${v.message || ''}\n`);
          } catch (_) {}
        }
        break;
      case 'response':
        if (v.success === false) emit(s, { type: 'notice', text: `[pi error: ${v.error || '?'}]` });
        break;
      case 'agent_end': {
        s.busy = false;
        clearTimeout(s.watchdog);
        const said = (s.turnText || '').trim();
        if (said && said.toLowerCase() !== 'pass' && said.length < 600) {
          emit(s, { type: 'notice', text: said });
        }
        // A dead model fails SILENTLY (empty answer, no tools). Say so —
        // silence must be pi's choice, never an outage.
        if (!said && !s.turnActivity) {
          emit(s, { type: 'notice', text: '[pi returned nothing — possible model outage; check /pi/health]' });
        }
        emit(s, { type: 'turn', state: 'end' });
        const p = s.pending;
        s.pending = null;
        if (p) runTurn(s, p.page, p.kind);
        break;
      }
      default: break;
    }
  }

  async function runTurn(s, page, kind) {
    if (s.busy) {
      // User intent beats background automation. Previously a save-triggered
      // pause could overwrite a queued NUDGE, making the sparkle button look
      // unreliable until it was tapped 2–3 times.
      if (kind === 'nudge' || !s.pending) s.pending = { page, kind };
      return;
    }
    s.page = page;
    try {
      const view = await canvasCall(s, { cmd: 'view', page });
      const msg = await pauseMessage(s, page, kind, view);
      const child = ensurePi(s);
      const prompt = { type: 'prompt', message: msg };
      if (view && view.ok) {
        prompt.images = [{ type: 'image', data: view.png_base64, mimeType: 'image/png' }];
      }
      child.stdin.write(JSON.stringify(prompt) + '\n');
      s.busy = true; // agent_start confirms; watchdog covers a wedged child
      clearTimeout(s.watchdog);
      s.watchdog = setTimeout(() => {
        if (s.busy && s.pi) { try { s.pi.kill('SIGKILL'); } catch (_) {} }
      }, TURN_TIMEOUT_MS);
      s.watchdog.unref?.();
    } catch (e) {
      emit(s, { type: 'notice', text: `pi error: ${String(e.message || e)}` });
    }
  }

  /* ---- sessions --------------------------------------------------------- */

  function ensure(id) {
    let s = sessions.get(id);
    if (s) { s.lastUsed = Date.now(); return s; }
    const key = id.replace(/[^a-z0-9-]/g, '').slice(0, 40) || 'doc';
    const prefsFile = path.join(PI_HOME, id, 'settings.json');
    const prefs = readJson(prefsFile) || {};
    s = {
      id, key,
      sock: path.join(os.tmpdir(), `papier-pi-${key}.sock`),
      sessionDir: path.join(PI_HOME, id, 'sessions'),
      workDir: path.join(PI_HOME, id, 'work'),
      events: [], nextEvent: 0,
      epoch: Date.now(),   // clients reset their event cursor when this moves
      busy: false, pending: null, pi: null, turnText: '', turnActivity: false, watchdog: null,
      mode: prefs.mode === 'quiet' ? 'quiet' : 'auto',
      font: FONTS.includes(prefs.font) ? prefs.font : 'serif',
      prefsFile, page: 1,
      canvas: null, canvasQueue: [],
      lastUsed: Date.now(),
    };
    fs.mkdirSync(s.sessionDir, { recursive: true });
    fs.mkdirSync(s.workDir, { recursive: true });
    startSocket(s);
    startCanvas(s);
    sessions.set(id, s);
    // Give the Unix socket one tick to bind, then resume pi before the user
    // asks for it. Events polling after a service restart also triggers this.
    const warm = setTimeout(() => {
      if (sessions.get(id) === s && !s.pi) ensurePi(s);
    }, 100);
    warm.unref?.();
    return s;
  }

  function savePrefs(s) {
    try {
      fs.mkdirSync(path.dirname(s.prefsFile), { recursive: true });
      const tmp = s.prefsFile + '.tmp';
      fs.writeFileSync(tmp, JSON.stringify({ mode: s.mode, font: s.font }));
      fs.renameSync(tmp, s.prefsFile);
    } catch (_) {}
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
    return { ok: true, busy: s.busy, mode: s.mode, font: s.font, epoch: s.epoch };
  }

  // GET /pi/health — a REAL 1-token model call (cached 10 min): a dead
  // model becomes visible instantly instead of masquerading as silence.
  let healthCache = null;
  function handleHealth(res) {
    if (healthCache && Date.now() - healthCache.at < 600_000) {
      json(res, 200, healthCache.body);
      return;
    }
    const child = spawn(PI_BIN, ['-p', '--no-session', '--provider', PI_PROVIDER,
      '--model', PI_MODEL, 'reply with exactly: ok'], {
      cwd: os.tmpdir(), env: { ...process.env, ...dotEnv() }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let out = '';
    child.stdout.on('data', (d) => { out += d; });
    const t = setTimeout(() => { try { child.kill('SIGKILL'); } catch (_) {} }, 90_000);
    child.on('exit', () => {
      clearTimeout(t);
      const ok = out.trim().toLowerCase().includes('ok');
      healthCache = { at: Date.now(), body: { ok, provider: PI_PROVIDER, model: PI_MODEL, output: out.trim().slice(0, 100) } };
      json(res, ok ? 200 : 503, healthCache.body);
    });
  }

  // Returns true when the request was handled.
  function handle(req, res, p, u) {
    if (!p.startsWith('/pi/')) return false;
    if (req.method === 'GET' && p === '/pi/health') { handleHealth(res); return true; }
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
        // Hide cold-start cost while the user is reading/writing. Previously
        // the first nudge paid ~14s to spawn + resume pi; warm turns are ~4s.
        ensurePi(s);
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
        if (m === 'auto' || m === 'quiet') { s.mode = m; savePrefs(s); }
        json(res, 200, state(s));
        return true;
      }
      case '/pi/font': {
        const f = u.searchParams.get('font');
        if (FONTS.includes(f)) { s.font = f; savePrefs(s); startCanvas(s); }
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
