// `/docs/`: how to set up and use peek. The text is plain HTML (pages/docs.html).
import { createFileRoute } from '@tanstack/solid-router';
import { onSettled } from 'solid-js';
import html from '../pages/docs.html?raw';
import css from '../pages/docs.css?url';
import { pageHead } from '../seo';

export const Route = createFileRoute('/docs')({
  head: () => {
    const head = pageHead({
      path: '/docs/', title: 'peek docs',
      description: 'How to set up peek: a remote desktop for your own machines over Tailscale that connects instantly.',
      preview: 'Set up peek: a small, fast remote desktop for your own machines over Tailscale, tailored for short sessions.',
    });
    return { ...head, links: [...head.links, { rel: 'stylesheet', href: css }] };
  },
  component: Docs,
});

// One copy button per command block; it copies every line, without trailing # comments.
const ICON = {
  copy: '<svg viewBox="0 0 24 24"><rect x="8.5" y="8.5" width="11" height="11" rx="2" /><path d="M15.5 8.5V6.5a2 2 0 0 0-2-2h-7a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h2" /></svg>',
  check: '<svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg>',
};

function Docs() {
  let main!: HTMLElement;
  onSettled(() => {
    for (const pre of main.querySelectorAll('pre')) {
      const text = (pre.textContent ?? '').split('\n').map(l => l.replace(/\s+#.*$/, '').trimEnd()).filter(Boolean).join('\n');
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.title = 'Copy';
      btn.setAttribute('aria-label', 'Copy commands');
      btn.innerHTML = ICON.copy;
      let timer: ReturnType<typeof setTimeout> | undefined;
      btn.onclick = () => navigator.clipboard.writeText(text).then(() => {
        btn.innerHTML = ICON.check;
        clearTimeout(timer);
        timer = setTimeout(() => btn.innerHTML = ICON.copy, 1200);
      }, () => {});
      pre.append(btn);
    }
  });
  return <main ref={main} innerHTML={html} />;
}
