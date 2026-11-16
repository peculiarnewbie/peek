// `/changelog/`: what changed in each version. The text is plain HTML (pages/changelog.html).
import { createFileRoute } from '@tanstack/solid-router';
import html from '../pages/changelog.html?raw';
import css from '../pages/changelog.css?url';
import { pageHead } from '../seo';

export const Route = createFileRoute('/changelog')({
  head: () => {
    const head = pageHead({
      path: '/changelog/', title: 'peek changelog',
      description: 'What changed in each version of peek, a small, fast remote desktop over Tailscale.',
    });
    return { ...head, links: [...head.links, { rel: 'stylesheet', href: css }] };
  },
  component: () => <main innerHTML={html} />,
});
