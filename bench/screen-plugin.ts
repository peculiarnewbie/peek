// Compare the built-in screen and the opt-in plugin on an isolated host.
// Build the plugin first: node experiments/screen-plugin/build.mjs
// bun bench/screen-plugin.ts [runs=20]
// Captures the desktop, but never injects input or reads clipboard contents.
import { strict as assert } from 'node:assert';
import { mkdtemp, readFile, rm, cp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join, dirname } from 'node:path';

const root = resolve(import.meta.dir, '..');
const binary = join(root, 'target/release', process.platform === 'win32' ? 'peek.exe' : 'peek');
const config = await mkdtemp(join(tmpdir(), 'peek-screen-spike-'));
assert.equal(dirname(config), resolve(tmpdir()));
const reservation = Bun.serve({ port: 0, fetch: () => new Response() });
const port = reservation.port!; reservation.stop(true);
const base = `http://127.0.0.1:${port}`;
const env = { ...process.env, PEEK_CONFIG_DIR: config };
const host = Bun.spawn([binary, '--bind', '127.0.0.1', '--port', String(port), '--web', 'none', '--no-clipboard'], { env, stdout: 'ignore', stderr: 'ignore' });
const sockets: WebSocket[] = [];
const cold = process.argv.includes('--cold');
const runs = Number(process.argv.slice(2).find(a=>!a.startsWith('--')) || 20);
assert.ok(Number.isInteger(runs) && runs > 0 && runs <= 200);
async function waitFor(fn: () => Promise<boolean>) {
  for (let i = 0; i < 200; i++) { try { if (await fn()) return; } catch {} await Bun.sleep(50); }
  throw new Error('host/provider not ready');
}
async function once(plugin: boolean, token: string) {
  let endpoint: string;
  if (plugin) {
    const response = await fetch(`${base}/api/frame/screen-experiment`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ token }) });
    assert.equal(response.status, 200);
    const { ticket } = await response.json() as any;
    endpoint = `${base.replace('http', 'ws')}/api/frame/screen-experiment/ws?ticket=${ticket}`;
  } else {
    endpoint = `${base.replace('http', 'ws')}/ws?${new URLSearchParams({v:'4',token,q:'med',fps:'60',lossless:'1',vis:'1'})}`;
  }
  return new Promise<{ firstFrameMs: number; hostMs: number; bytes: number; width: number; height: number }>((resolve, reject) => {
    const start = performance.now();
    const ws = new WebSocket(endpoint, { headers: { Origin: base } } as any);
    sockets.push(ws); ws.binaryType = 'arraybuffer';
    const timeout = setTimeout(() => { ws.close(); reject(new Error(`${plugin ? 'plugin' : 'built-in'}: no frame within 10 s; needs an interactive desktop`)); }, 10000);
    ws.onopen = () => {
      if (plugin) {
        ws.send(JSON.stringify({t:'cfg',q:'med',scale:1,mon:0,fps:60,soft:false,lossless:true}));
        ws.send(JSON.stringify({t:'vis',v:true}));
      }
    };
    ws.onmessage = e => {
      if (typeof e.data === 'string') return;
      const frame = new DataView(e.data as ArrayBuffer);
      clearTimeout(timeout);
      ws.send(JSON.stringify({t:'ack',s:frame.getUint32(1,true)}));
      ws.close();
      resolve({firstFrameMs:performance.now()-start,hostMs:frame.getUint32(11,true)/1000,bytes:frame.byteLength,width:frame.getUint16(5,true),height:frame.getUint16(7,true)});
    };
    ws.onclose = e => { clearTimeout(timeout); if (e.code !== 1000) reject(new Error(`closed: ${e.code}`)); };
  });
}
async function exerciseStream(token: string) {
  const { ticket } = await (await fetch(`${base}/api/frame/screen-experiment`, {
    method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({token}),
  })).json() as any;
  await new Promise<void>((resolve,reject) => {
    const ws = new WebSocket(`${base.replace('http','ws')}/api/frame/screen-experiment/ws?ticket=${ticket}`, {headers:{Origin:base}} as any);
    sockets.push(ws); ws.binaryType='arraybuffer';
    let frames=0, paused=false, pong=false;
    const timeout=setTimeout(()=>{ws.close();reject(new Error('stream exercise timed out'));},10000);
    ws.onopen=()=>ws.send(JSON.stringify({t:'vis',v:true}));
    ws.onmessage=e=>{
      if(typeof e.data==='string') { if(JSON.parse(e.data).t==='pong') pong=true; return; }
      frames++;
      const dv=new DataView(e.data as ArrayBuffer);
      ws.send(JSON.stringify({t:'ack',s:dv.getUint32(1,true)}));
      if(frames<60) ws.send(JSON.stringify({t:'cfg',q:frames%2?'low':'high'}));
      else if(!paused) {
        paused=true; ws.send(JSON.stringify({t:'vis',v:false}));
        setTimeout(()=>{
          const settled=frames;
          ws.send(JSON.stringify({t:'ping',n:7}));
          setTimeout(()=>{
            clearTimeout(timeout); ws.close();
            try {assert.equal(frames,settled,'paused stream sent more frames');assert.ok(pong,'timing probe failed');resolve();}
            catch(error){reject(error);}
          },300);
        },300);
      }
    };
  });
  console.log('PASS: 60 acknowledged screen updates through the plugin channel, visibility pause, and timing probe');
}
try {
  await waitFor(async () => (await fetch(`${base}/.well-known/peek`)).ok);
  const token = (await readFile(join(config, 'token'), 'utf8')).trim();
  const source = join(config, 'source');
  await cp(join(root, 'experiments/screen-plugin/package'), source, {recursive:true});
  const manifest = JSON.parse(await readFile(join(source, 'peek-plugin.json'), 'utf8'));
  manifest.run.push('--no-clipboard');
  await writeFile(join(source, 'peek-plugin.json'), JSON.stringify(manifest));
  const install = Bun.spawn([binary, '--port', String(port), 'plugin', 'add', source], {env,stdout:'ignore',stderr:'pipe'});
  const err = await new Response(install.stderr).text();
  assert.equal(await install.exited, 0, err);
  await waitFor(async () => (await (await fetch(`${base}/.well-known/peek`)).json() as any).providers.some((p:any)=>p.name==='screen-experiment' && p.frame));
  const result: Record<string, Awaited<ReturnType<typeof once>>[]> = { builtin: [], plugin: [] };
  // Warm both independent capture engines before alternating their samples.
  if (!cold) { await once(false, token); await once(true, token); }
  for (let i=0;i<runs;i++) {
    for (const plugin of i % 2 ? [true,false] : [false,true]) {
      if (cold) await Bun.sleep(5500);
      result[plugin?'plugin':'builtin'].push(await once(plugin,token));
      await Bun.sleep(40);
    }
  }
  const percentile = (values:number[],p:number) => [...values].sort((a,b)=>a-b)[Math.min(values.length-1,Math.floor(values.length*p))];
  const summary = Object.fromEntries(Object.entries(result).map(([key,values])=>[key,{
    samples:values.length,
    medianFirstFrameMs:percentile(values.map(v=>v.firstFrameMs),.5),
    p90FirstFrameMs:percentile(values.map(v=>v.firstFrameMs),.9),
    medianHostHeaderMs:percentile(values.map(v=>v.hostMs),.5),
    medianBytes:percentile(values.map(v=>v.bytes),.5),
    resolution:`${values[0].width}x${values[0].height}`,
  }]));
  console.log(JSON.stringify({mode:`${cold?'cold (5.5 s idle before each connection)':'warm alternating'}; desktop capture; loopback; excludes ticket HTTP and browser decode`,summary},null,2));
  await exerciseStream(token);
  console.log('Host header timing excludes some IPC/socket queue cost; first-frame timing includes it. No input, lock-screen, sign-out, or browser behavior is verified here.');
} finally {
  for (const ws of sockets) ws.close();
  host.kill(); await host.exited;
  for (let attempt = 0; ; attempt++) {
    try { await rm(config,{recursive:true,force:true}); break; }
    catch (error) { if (attempt === 30) throw error; await Bun.sleep(100); }
  }
}
