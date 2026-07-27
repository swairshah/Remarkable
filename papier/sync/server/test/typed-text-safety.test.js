'use strict';

// The user's typed text (top-level `texts`) is newer than libreink-page's
// Page model, so papier-cloud-canvas — which loads and re-saves the whole
// page file through that model — silently drops the field when pi draws.
// papier-pi-sessions snapshots the runs around every mutating canvas call
// and puts them back. This test stands in a fake canvas that does exactly
// what the current Rust does: rewrite the page without `texts`.
//
// When libreink-page learns the field the restore becomes a no-op and this
// test still passes (the written file already has the runs).

const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

function executable(file, body) {
  fs.writeFileSync(file, '#!/usr/bin/env node\n' + body);
  fs.chmodSync(file, 0o755);
}
function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value));
}
async function freePort() {
  const s = net.createServer();
  await new Promise((resolve) => s.listen(0, '127.0.0.1', resolve));
  const port = s.address().port;
  await new Promise((resolve) => s.close(resolve));
  return port;
}

// draws a patch into the inbound ink file the way the Rust binary does:
// load -> mutate -> save, through a model that knows nothing about `texts`
const fakeCanvas = `
const fs=require('fs'), path=require('path'), readline=require('readline');
const inkFile=process.env.TT_INK;
readline.createInterface({input:process.stdin}).on('line',line=>{
 const c=JSON.parse(line);
 let r={ok:true};
 if(c.cmd==='view') r={ok:true,png_base64:'',patches:[],layout:''};
 if(c.cmd==='draw'){
   let page={v:1,next_patch:1,next_stroke:1,strokes:[],patches:[]};
   try{ const cur=JSON.parse(fs.readFileSync(inkFile,'utf8'));
        page={v:1,next_patch:cur.next_patch||1,next_stroke:cur.next_stroke||1,
              strokes:cur.strokes||[],patches:cur.patches||[]}; }catch(_){}
   page.patches.push({id:page.next_patch++,strokes:[{i:99,g:110,p:[10,10,20,20,20,20]}],texts:[]});
   fs.mkdirSync(path.dirname(inkFile),{recursive:true});
   fs.writeFileSync(inkFile,JSON.stringify(page));   // note: no \`texts\`
   r={ok:true,page:c.page,bbox:[0,0,10,10]};
 }
 process.stdout.write(JSON.stringify(r)+'\\n');
});
`;

const fakePi = `
const fs=require('fs'), net=require('net'), readline=require('readline');
const sock=process.env.PAPIER_SOCK, out=process.env.TT_RESULT;
function call(cmd){return new Promise((resolve,reject)=>{const c=net.createConnection(sock,()=>c.write(JSON.stringify(cmd)+'\\n'));let b='';c.on('data',d=>{b+=d;if(b.includes('\\n')){c.end();resolve(JSON.parse(b));}});c.on('error',reject);});}
readline.createInterface({input:process.stdin}).on('line',async line=>{
 const x=JSON.parse(line); if(x.type!=='prompt') return;
 process.stdout.write(JSON.stringify({type:'agent_start'})+'\\n');
 const drew=await call({cmd:'draw',page:1,svg:'<svg/>'});
 fs.writeFileSync(out,JSON.stringify({drew}));
 process.stdout.write(JSON.stringify({type:'agent_end'})+'\\n');
});
`;

test('a cloud-pi draw cannot drop the user typed text layer', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-typed-'));
  const backup = path.join(root, 'backup');
  const bin = path.join(root, 'bin'); fs.mkdirSync(bin);
  const pi = path.join(bin, 'pi'); const canvas = path.join(bin, 'canvas');
  const result = path.join(root, 'result.json');
  executable(pi, fakePi); executable(canvas, fakeCanvas);
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'meta.json'),
    { kind: 'notebook', title: 'NB', w: 1404, h: 1872 });
  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'state.json'),
    { next_note: 2, pos: 0, seq: [{ n: 1 }] });
  // the page as the web left it: a stroke and a typed run
  const inbound = path.join(backup, 'papier-inbound', 'docs', 'nb', 'ink', 'note-0001.json');
  writeJson(inbound, {
    v: 1, next_patch: 1, next_stroke: 2,
    strokes: [{ i: 1, g: 0, p: [100, 100, 24, 200, 200, 24] }],
    texts: [{ x: 1200, y: 8000, s: 400, g: 0, t: 'typed on the web' }],
    patches: [],
  });

  const port = await freePort();
  const service = spawn(process.execPath, [path.resolve(__dirname, '../bin/papier-upload.js')], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_PORT: String(port),
      PAPIER_CANVAS_BIN: canvas, PI_BIN: pi, PAPIER_PI_HOME: path.join(root, 'pi-home'),
      TT_INK: inbound, TT_RESULT: result },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  t.after(() => service.kill('SIGKILL'));
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('service start timeout')), 3000);
    service.once('exit', (code) => reject(new Error('service exited ' + code)));
    service.stdout.on('data', (d) => { if (d.toString().includes('127.0.0.1:' + port)) { clearTimeout(timer); resolve(); } });
  });

  await fetch(`http://127.0.0.1:${port}/pi/nudge?id=nb&page=1`, { method: 'POST' });
  const deadline = Date.now() + 5000;
  while (!fs.existsSync(result) && Date.now() < deadline) await new Promise((r) => setTimeout(r, 25));
  assert.ok(fs.existsSync(result), 'the fake pi never drew');
  // give the restore its tick
  await new Promise((r) => setTimeout(r, 150));

  const page = JSON.parse(fs.readFileSync(inbound, 'utf8'));
  assert.equal(page.patches.length, 1, 'pi\'s patch should be there');
  assert.deepEqual(page.texts, [{ x: 1200, y: 8000, s: 400, g: 0, t: 'typed on the web' }]);
  assert.equal(page.strokes.length, 1);
});
