// Optional headless Chrome integration for bench/plugins.ts --browser.
// Uses an isolated browser profile; never automates the user's browser.
import { strict as assert } from 'node:assert';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname, resolve } from 'node:path';

export async function verifyBrowser(host: { base: string; token: string }, restart: () => Promise<void>, screenPlugin = false) {
  const profile = await mkdtemp(join(tmpdir(), 'peek-browser-'));
  assert.equal(dirname(profile), resolve(tmpdir()));
  const chrome = process.env.PEEK_TEST_CHROME || (process.platform === 'win32' ? 'C:/Program Files/Google/Chrome/Application/chrome.exe' : 'google-chrome');
  const browser = Bun.spawn([chrome, '--headless', '--no-first-run', '--no-default-browser-check', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], { stdout: 'ignore', stderr: 'ignore' });
  let ws: WebSocket | undefined;
  async function until<T>(fn: () => Promise<T>, label: string): Promise<T> {
    for (let i = 0; i < 200; i++) {
      try { const value = await fn(); if (value) return value; } catch {}
      await Bun.sleep(50);
    }
    throw new Error(`Browser check timed out: ${label}`);
  }
  try {
    const port = await until(async () => (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0], 'debugger');
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json() as any[];
    ws = new WebSocket(targets.find(t => t.type === 'page').webSocketDebuggerUrl);
    await new Promise<void>((resolve, reject) => { ws!.onopen = () => resolve(); ws!.onerror = () => reject(new Error('debugger unavailable')); });
    let next = 0;
    const pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
    const errors: string[] = [];
    let genericScreenSocket = false;
    ws.onmessage = e => {
      const m = JSON.parse(e.data as string);
      if (m.method === 'Runtime.exceptionThrown') errors.push(m.params.exceptionDetails.text);
      if (m.method === 'Network.webSocketCreated' && m.params.url.includes('/api/frame/screen-experiment/ws?') && new URL(m.params.url).searchParams.has('ticket')) genericScreenSocket = true;
      if (m.id) {
        const p = pending.get(m.id); pending.delete(m.id);
        if (m.error) p?.reject(new Error(m.error.message)); else p?.resolve(m.result);
      }
    };
    const call = (method: string, params: object = {}) => new Promise<any>((resolve, reject) => {
      const id = ++next; pending.set(id, { resolve, reject }); ws!.send(JSON.stringify({ id, method, params }));
    });
    async function evaluate(expression: string) {
      const response = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
      if (response.exceptionDetails) throw new Error(response.exceptionDetails.text);
      return response.result.value;
    }
    await call('Runtime.enable');
    await call('Network.enable');
    await call('Page.enable');
    await call('Emulation.setFocusEmulationEnabled', { enabled: true });
    await call('Page.addScriptToEvaluateOnNewDocument', { source: "localStorage.setItem('peek-pause','0')" });
    await call('Page.navigate', { url: `${host.base}/#${host.token}` });
    await until(() => evaluate("document.querySelector('#screen')?.width > 16"), 'built-in screen renders').catch(async error => { console.log('Browser diagnostics:', await evaluate("({message:document.querySelector('#msg')?.textContent,sleep:document.querySelector('#sleep')?.className,canvas:document.querySelector('#screen')?.width,ready:document.readyState})"), errors); throw error; });
    await evaluate("document.querySelector('#b-stats').click()");
    await until(() => evaluate("[...document.querySelectorAll('#stats button')].some(b=>b.textContent==='notes')"), 'plugin page navigation');
    await evaluate("[...document.querySelectorAll('#stats button')].find(b=>b.textContent==='notes').click()");
    await until(() => evaluate("!!document.querySelector('#frame iframe')?.contentDocument?.querySelector('#note')"), 'notes frame');
    // Restart the host while the same browser tab and frame are open. Its old
    // ticket is now invalid; the parent must acquire and load a new one.
    await restart();
    await until(() => evaluate("document.querySelector('#frame iframe')?.contentDocument?.body.textContent.includes('Notes')"), 'frame after host restart');
    // The new frame must actually have a live, authorized socket, not merely
    // public HTML. The example exposes its status in #status.
    await until(() => evaluate("document.querySelector('#frame iframe')?.contentDocument?.querySelector('#status')?.textContent?.toLowerCase().includes('live')"), 'renewed page socket');
    await evaluate("document.querySelector('#frame-head button').click()");
    await until(() => evaluate("!document.querySelector('#frame iframe')"), 'frame closes');
    if (screenPlugin) {
      await evaluate("[...document.querySelectorAll('#stats button')].find(b=>b.textContent==='screen-experiment').click()");
      await until(() => evaluate("document.querySelector('#frame iframe')?.contentDocument?.querySelector('#screen')?.width > 16"), 'screen plugin renders');
      assert.ok(genericScreenSocket, 'screen plugin bypassed its generic socket');
      await evaluate("document.querySelector('#frame iframe').contentDocument.querySelector('#b-sleep').click()");
      await until(() => evaluate("document.querySelector('#frame iframe')?.contentDocument?.querySelector('#sleep')?.classList.contains('show')"), 'screen plugin pauses');
      await evaluate("document.querySelector('#frame iframe').contentDocument.querySelector('#b-sleep').click()");
      await until(() => evaluate("!document.querySelector('#frame iframe')?.contentDocument?.querySelector('#sleep')?.classList.contains('show')"), 'screen plugin resumes');
      console.log('PASS: experimental screen page renders through the generic ticketed socket and its pause/resume controls work');
    }
    assert.deepEqual(errors, []);
    console.log('PASS: headless Chrome renders built-in screen, opens a plugin frame, renews authorization after host restart, and closes the frame');
  } finally {
    ws?.close(); browser.kill(); await browser.exited;
    for (let i = 0; ; i++) {
      try { await rm(profile, {recursive:true,force:true}); break; }
      catch (error) { if (i===30) throw error; await Bun.sleep(100); }
    }
  }
}
