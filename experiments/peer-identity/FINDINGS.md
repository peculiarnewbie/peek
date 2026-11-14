# What identity reaches peek over Tailscale

13 November 2026. The first step of roadmap slice 4: before designing peer
access, check what a host can learn about who is calling, on each path.

Tailscale 1.104.5. The host was desktop-denny (Windows 11). Callers were
desktop-denny itself and cachy (Linux), on the same tailnet and owned by the
same user. The probe is `probe.ts` beside this file: it reports a request's
remote address, its Tailscale and forwarding headers, and what
`tailscale whois` says about the remote address.

## Results

| | Through Tailscale Serve | Direct to the tailnet address |
|---|---|---|
| Remote address the backend sees | `127.0.0.1`: Serve connects | the caller's tailnet address (`100.64.38.83` for cachy) |
| User | `Tailscale-User-Login`, `-Name`, `-Profile-Pic` headers | `whois` on the address |
| Machine | not in a header; `whois` on `X-Forwarded-For` gives it | `whois` on the address: node name and stable ID |
| Forged by the caller | no: Serve overwrote a forged `Tailscale-User-Login` and `X-Forwarded-For` | no: the address comes from WireGuard |
| Forged by a local process | **yes**: connecting to the backend's loopback port directly, any header can be set | no |
| Grants | `--accept-app-caps` exists on `tailscale serve`; not tried | `whois` returns `CapMap`; empty, since the policy grants no app capabilities |

From cachy, through Serve: `x-forwarded-for: 100.64.38.83`,
`tailscale-user-login: curiousarmand@gmail.com`. Directly: remote
`100.64.38.83`, and `whois` gave node `cachy.tail41e8ef.ts.net`, stable ID
`nYvLsQFAB921CNTRL`, user `curiousarmand@gmail.com`, no tags.

Other observations:

- A machine shared in from another tailnet (`agia-ms-7c95.tail287830.ts.net`)
  is tagged `tag:share`, and `whois` gives its user as `tagged-devices`. A
  tagged machine has no person behind it; identity is the machine and its tags.
- `tailscale whois` worked from an unelevated, medium-integrity shell. It was
  not tried from the SYSTEM service.
- On Windows, a request from a machine to its own tailnet address does not
  reach Serve on a port another program already holds. Port 8443 was held by
  HTTP.sys (`System`), and the call reached that instead, with a self-signed
  certificate. On a free port (10443) `tailscaled` listens on the tailnet
  address, and the call reached Serve. Testing Serve from the same machine
  needs a port nothing else uses.

## Not tried

- A caller owned by another user, or a tagged caller, through Serve. Tailscale
  documents that Serve sends no user headers for tagged machines.
- Grants: a policy with an `app` capability for peek, read through `CapMap`
  directly and through Serve's `--accept-app-caps`.
- Calling the local Tailscale client from the SYSTEM service, and from Linux
  and macOS hosts.
- Peek's own `--no-token` direct path; the probe stood in for it.
