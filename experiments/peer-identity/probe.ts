// Reports what a request brings that could identify its caller: the address
// it came from, any headers Tailscale Serve added, and what the local
// Tailscale client says about that address.
//
//   bun experiments/peer-identity/probe.ts [port] [host]
//
// Behind Serve:  bun probe.ts 5291            (then: tailscale serve --bg --https=8443 http://127.0.0.1:5291)
// Direct path:   bun probe.ts 5292 100.x.y.z  (this machine's tailnet address)
const port = Number(process.argv[2] ?? 5291);
const hostname = process.argv[3] ?? '127.0.0.1';

async function whois(ip: string) {
  const p = Bun.spawn(['tailscale', 'whois', '--json', ip], { stdout: 'pipe', stderr: 'pipe' });
  const [code, out, err] = await Promise.all([p.exited, new Response(p.stdout).text(), new Response(p.stderr).text()]);
  if (code !== 0) return { error: err.trim() || `exit ${code}` };
  const d = JSON.parse(out);
  return {
    node: d.Node?.Name,
    nodeId: d.Node?.StableID,
    tags: d.Node?.Tags ?? [],
    user: d.UserProfile?.LoginName,
    capMap: d.CapMap ?? null,
  };
}

Bun.serve({
  port,
  hostname,
  async fetch(req, server) {
    const remote = server.requestIP(req);
    const headers = Object.fromEntries([...req.headers].filter(([k]) =>
      k.startsWith('tailscale-') || k.startsWith('x-forwarded') || k === 'host' || k === 'forwarded'));
    const report = {
      at: new Date().toISOString(),
      url: req.url,
      remote,
      headers,
      whois: remote ? await whois(remote.address) : null,
    };
    console.log(JSON.stringify(report));
    return Response.json(report);
  },
});
console.log(`probe on http://${hostname}:${port}`);
