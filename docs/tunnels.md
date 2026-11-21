# Remote services: in-epic tunnels

Status: delivered by [MIR-262](https://multica.ai/miran248/issues/MIR-262),
stage 6 of [MIR-178](https://multica.ai/miran248/issues/MIR-178). A remote
service is a tunnel sidecar inside the epic's network, addressed exactly like a
local role: one role name, one container port, the same dotted alias, no host
port forwarding.

Amended by [spec 0002](specs/0002-container-harness.md): the network and names
belong to a scope, not an epic, and the harness container joins that network.
The transports and their limits are unchanged.

## The manifest

```yaml
x-msbx:
  project: signals
  env:
    pg: [MSBX_TUNNEL_SSH_USER]
  secrets:
    pg: [MSBX_TUNNEL_SSH_KEY]
  tunnels:
    pg:
      host: db.internal.example.com  # the remote service
      port: 5432                     # its port = the sidecar's container port
      via: ssh                       # ssh | cloudflared
services:
  web:
    image: signals-web
```

- A tunnelled role is **not** a `services` entry: `x-msbx.tunnels.<role>` and a
  service `<role>` in the same file is an error. `msbx` generates the sidecar.
- `port` is both the remote service port and the port the sidecar listens on
  inside the epic, so `pg:5432` works the same whether `pg` is local or remote.
- `x-msbx.tunnels` is the only place a remote service is declared. `publish` may
  list the role (nothing forces it): it then gets a loopback port from the
  epic's block like any other published role, and `publish.<role>` must equal
  the tunnel port. Without `publish` the role has **no host port** at all.

`msbx up` generates, on the epic's network:

```
<project>-<epic>-<role>                       signals-mir-42-pg
<role>.<epic>.<project>.msbx.test             pg.mir-42.signals.msbx.test
```

The sidecar carries the role's dotted network alias and the `com.msbx.*` labels
plus `com.msbx.tunnel=<via>`. A peer reaches it by service name (`pg`) and by
alias. `x-msbx.env` and `x-msbx.secrets` declared for the role are rendered into
the sidecar exactly as they are for a local service, which is how a transport
gets its credentials.

## Transport: `ssh`

`ssh -N -L 0.0.0.0:<port>:<host>:<port>` from an `alpine:3.20` sidecar (it
installs `openssh-client` at start). The role's envelope supplies:

| Name | Kind | Required | Meaning |
| --- | --- | --- | --- |
| `MSBX_TUNNEL_SSH_USER` | env | yes | the login on the ssh endpoint; without it the sidecar exits rather than guessing |
| `MSBX_TUNNEL_SSH_HOST` | env | no | the ssh endpoint, when it is not the remote service host (default: the tunnel `host`) |
| `MSBX_TUNNEL_SSH_PORT` | env | no | the ssh endpoint port (default `22`) |
| `MSBX_TUNNEL_SSH_KEY` | secret | no | a private key, **base64 in one line** (see below) |
| `MSBX_TUNNEL_SSH_KNOWN_HOSTS` | secret | no | a known-hosts file, base64 in one line |

The store keeps one line per secret (`docs/env.md`), so a multi-line key enters
it base64-encoded; the sidecar decodes it to a `0600` file before `ssh` uses it:

```sh
base64 < ~/.ssh/id_ed25519 | tr -d '\n' > \
  ~/.config/msbx/projects/signals/secrets/MSBX_TUNNEL_SSH_KEY
```

Without a known-hosts secret the sidecar trusts the endpoint's key on first
connect (`StrictHostKeyChecking=accept-new`). Declare the known-hosts secret to
pin it.

A failed login or an unreachable ssh endpoint exits the sidecar: `ssh` carries
`ConnectTimeout=10`, `ExitOnForwardFailure=yes` and keepalives, so `msbx up`
fails its `--wait` instead of hanging and `msbx status` shows the exited
container. The sidecar's healthcheck is a TCP connect to its own listening port,
so it reports `unhealthy` while `ssh` is not listening (starting, or gone). When
`ssh` is up but the far end cannot reach the remote service, each client
connection is closed rather than queued: the failure is visible at the client
and in the sidecar's log, not a silent hang.

## Transport: `cloudflared`

`cloudflared access tcp --hostname <host> --url 0.0.0.0:<port>` from
`cloudflare/cloudflared:2026.11.1`. The Access service token is declared like any
other env value, because `cloudflared` reads these names itself:

```yaml
  env:
    pg: [TUNNEL_SERVICE_TOKEN_ID, TUNNEL_SERVICE_TOKEN_SECRET]
```

`cloudflared access tcp` is a lazy proxy: it binds the local port immediately
and dials the edge per client connection. An unreachable edge therefore appears
as a failed connection at the client and an error in the container's log, not as
a hang; a bad token or configuration exits the container. The official image is
distroless, so the sidecar has no shell and no local probe; its health signal is
the container state (`msbx status` reports `state`, `health` and `exit_code`).

## Status

`msbx status` marks a role served by a tunnel:

```
signals-mir-42  signals/mir-42  block 23000..=23015  idle 4m
  pg           tunnel:ssh  running  healthy  127.0.0.1:23000 → 5432  signals-mir-42-pg
  web          local       running  healthy  -                       signals-mir-42-web
```

`--json` carries `"tunnel": {"via": "ssh", "upstream": "db.internal.example.com:5432"}`
on the role and omits it for a local service. `up` records the generated tunnels
in the state file, so `status` still marks them after the checkout is gone.

## Limits

- The ssh sidecar installs its client from the Alpine repositories at container
  start; the host must reach them. `cloudflared` ships its own binary.
- A key is stored base64 because a store secret is one line. An encrypted key is
  not unlocked, so use a passphrase-free key.
- `accept-new` trusts the endpoint's host key on first connect; declare
  `MSBX_TUNNEL_SSH_KNOWN_HOSTS` where that matters.
- A tunnel is routed through the transport: its liveness is the transport's.
  `cloudflared access` cannot report edge reachability while idle (see above).
- `publish` on a tunnelled role exposes the sidecar's loopback port; it does not
  expose the remote service directly and nothing binds on the host otherwise.
