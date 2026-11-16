// The hosted site's header and the introduction a browser with no machines
// sees. Rendered on the server too, when the site is built (site/), so it
// must not touch the browser: no imports from hosted.ts or the screen.
import { createSignal } from 'solid-js';
import type { JSX } from '@solidjs/web';
import { Icon } from './Icons';

export const GITHUB = 'https://github.com/peculiarnewbie/peek';

/** Shell commands, one per line, with one button that copies them all. */
export function Commands(props: { lines: string[] }) {
  const [copied, setCopied] = createSignal(false);
  let timer: number | undefined;
  const copy = () => {
    navigator.clipboard.writeText(props.lines.join('\n')).then(() => {
      setCopied(true);
      clearTimeout(timer);
      timer = window.setTimeout(() => setCopied(false), 1200);
    }, () => {});
  };
  return (
    <div class="cmds">
      <code>{props.lines.join('\n')}</code>
      <button type="button" class="cmd-copy" onClick={copy} aria-label="Copy commands" title="Copy">
        <Icon name={copied() ? 'check' : 'copy'} />
      </button>
    </div>
  );
}

export function Header(props: { children?: JSX.Element }) {
  return (
    <header class="dhead">
      <h1><img src="/icon.svg" alt="" width="28" height="28" />peek</h1>
      <nav class="dnav">
        {props.children}
        <a class="tb" href="/docs/">Docs</a>
        <a class="tb" href="/changelog/">Changelog</a>
        <a class="tb" href={GITHUB}>GitHub</a>
      </nav>
    </header>
  );
}

export function Intro() {
  return (
    <section class="intro">
      <p class="hint lead">peek is a small, fast remote desktop, tailored for short sessions.</p>
      <p class="hint lead">It also lets a machine show its vitals, sleep and wake, share folders, and run{' '}
        <a href="/docs/#apps">plugins</a>: programs on the machine that show up in your browser through peek.</p>
      <p class="hint lead">Run it on a computer (Windows, or Linux on Wayland) on your{' '}
        <a href="https://tailscale.com">Tailscale</a> network and pair this browser with it once. Its screen
        is then a click away, with mouse, keyboard and clipboard.</p>
      <p class="hint lead">This site only serves the page: your browser connects straight to each machine
        over Tailscale.</p>
      <Commands lines={['npx @peculiarnewbie/peek pair']} />
      <dl class="figures">
        <div><dt>140 ms</dt><dd>to a whole full-screen frame over Tailscale, from idle</dd></div>
        <div><dt>22 ms</dt><dd>the same on the machine itself</dd></div>
        <div><dt>0.02%</dt><dd>of a CPU core while nobody looks</dd></div>
        <div><dt>18 MB</dt><dd>of memory, in a 4.6 MB program</dd></div>
      </dl>
      <a class="figures-how" href="/docs/#numbers">How these were measured</a>
    </section>
  );
}

export const Foot = () => (
  <p class="hint small foot">The list of machines and their keys stay in this browser. Nothing about them is
    stored on this site.</p>
);
