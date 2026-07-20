'use strict';

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

const fakeCanvas = `
const fs=require('fs'), readline=require('readline');
let next=2, count=1;
const log=process.env.SAFETY_LOG;
readline.createInterface({input:process.stdin}).on('line',line=>{
 const c=JSON.parse(line); fs.appendFileSync(log,'canvas '+JSON.stringify(c)+'\\n');
 let r={ok:true};
 if(c.cmd==='view') r={ok:true,png_base64:'',patches:[],layout:''};
 if(c.cmd==='insert_note') r={ok:true,page:++count,page_count:count,note:next++};
 if(c.cmd==='remove_empty_note') r={ok:true,page:count,page_count:--count,note:c.note};
 process.stdout.write(JSON.stringify(r)+'\\n');
});
`;

const fakePi = `
const fs=require('fs'), net=require('net'), readline=require('readline');
const sock=process.env.PAPIER_SOCK, out=process.env.SAFETY_RESULT;
function call(cmd){return new Promise((resolve,reject)=>{const c=net.createConnection(sock,()=>c.write(JSON.stringify(cmd)+'\\n'));let b='';c.on('data',d=>{b+=d;if(b.includes('\\n')){c.end();resolve(JSON.parse(b));}});c.on('error',reject);});}
readline.createInterface({input:process.stdin}).on('line',async line=>{
 const x=JSON.parse(line); if(x.type!=='prompt') return;
 process.stdout.write(JSON.stringify({type:'agent_start'})+'\\n');
 const first=await call({cmd:'insert_note',after_page:1});
 const second=await call({cmd:'insert_note',after_page:2});
 fs.writeFileSync(out,JSON.stringify({first,second}));
 process.stdout.write(JSON.stringify({type:'agent_end'})+'\\n');
});
`;

test('a turn must draw an inserted page before adding another; blank is rolled back', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'papier-pi-safety-'));
  const backup = path.join(root, 'backup');
  const bin = path.join(root, 'bin'); fs.mkdirSync(bin);
  const pi = path.join(bin, 'pi'); const canvas = path.join(bin, 'canvas');
  const log = path.join(root, 'safety.log'); const result = path.join(root, 'result.json');
  executable(pi, fakePi); executable(canvas, fakeCanvas);
  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'meta.json'),
    { kind: 'notebook', title: 'NB', w: 1404, h: 1872 });
  writeJson(path.join(backup, 'papier', 'docs', 'nb', 'state.json'),
    { next_note: 2, pos: 0, seq: [{ n: 1 }] });
  const port = await freePort();
  const service = spawn(process.execPath, [path.resolve(__dirname, '../bin/papier-upload.js')], {
    env: { ...process.env, PAPIER_BACKUP: backup, PAPIER_PORT: String(port),
      PAPIER_CANVAS_BIN: canvas, PI_BIN: pi, PAPIER_PI_HOME: path.join(root, 'pi-home'),
      SAFETY_LOG: log, SAFETY_RESULT: result }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  t.after(() => { service.kill('SIGTERM'); fs.rmSync(root, { recursive: true, force: true }); });
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('service start timeout')), 3000);
    service.once('exit', (code) => reject(new Error('service exited '+code)));
    service.stdout.on('data', d => { if(d.toString().includes('127.0.0.1:'+port)){clearTimeout(timer);resolve();} });
  });
  await fetch(`http://127.0.0.1:${port}/pi/nudge?id=nb&page=1`, { method: 'POST' });
  const deadline = Date.now()+3000;
  while ((!fs.existsSync(result) || !fs.existsSync(log) || !fs.readFileSync(log,'utf8').includes('remove_empty_note')) && Date.now()<deadline) {
    await new Promise(r=>setTimeout(r,25));
  }
  const got=JSON.parse(fs.readFileSync(result));
  assert.equal(got.first.ok,true);
  assert.equal(got.second.ok,false);
  assert.match(got.second.error,/draw on newly inserted page 2/);
  assert.match(fs.readFileSync(log,'utf8'),/"cmd":"remove_empty_note","note":2/);
  const events=await (await fetch(`http://127.0.0.1:${port}/pi/events?id=nb&since=0`)).json();
  assert.ok(events.events.some(e=>e.type==='notice' && /removed 1 unfinished blank page/.test(e.text)));
});
