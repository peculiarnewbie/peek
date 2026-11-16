// Reading what is typed or scanned to pair with a machine. Plain functions,
// so the hosted site's home can use them as it is built, too.

/** A host and code from a pairing link (`peek pair` prints one), if it is one. */
export function parseLink(text: string): { host: string; code: string; name?: string } | null {
  try {
    const url = new URL(text.trim());
    const host = url.searchParams.get('host');
    const code = new URLSearchParams(url.hash.slice(1)).get('code');
    if (host && code) return { host, code, name: url.searchParams.get('name') ?? undefined };
  } catch {}
  return null;
}

/** A machine as typed: no scheme, no path, no trailing dot. */
export const cleanHost = (text: string) =>
  text.trim().replace(/^https?:\/\//i, '').replace(/\/.*$/, '').replace(/\.$/, '').toLowerCase();
