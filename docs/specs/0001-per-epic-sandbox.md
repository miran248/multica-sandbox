# 0001 — Per-epic sandbox on Docker Compose

Status: proposed. Approved when its pull request merges into `spec/mir-178`.
Epic: [MIR-178](https://multica.ai/miran248/issues/MIR-178). Supersedes the
Apple `container` substrate of the first revision of
[ADR 0001](../adr/0001-per-epic-sandbox.md).

## Problem

Every Multica task runs on the host with the host's filesystem, environment and
network. Environment variables are per agent, not per project, so an agent
working across projects has to be told where each project's values live. Two
epics cannot run the same service stack at once: a second Postgres on `5432`
collides, and the workaround — a hand-maintained dnsmasq zone, a static loopback
IP per project and a Caddy sidecar — needs a human for every new epic. Code the
agent runs shares the host with everything else.

## Outcome

On the machine that runs the agent, `msbx up` in a project checkout starts that
epic's services as their own Compose project, gives every published role a
stable `127.0.0.1` port and the name
`<role>.<epic>.<project>.msbx.test`, and exports the epic's environment and
secret files to the harness and to the services. Two epics run the same
Postgres image at the same time without colliding. The agent, `dsh` and model
calls stay on the host.

## Non-goals

- Sandboxing the agent, the harness, model calls or the Multica boundary.
- An L4/L7 ingress, canonical host ports, or a loopback-alias helper.
- Multi-user isolation, rootless daemons, or a microVM per epic.
- Apple `container`, `sbx` (Docker Sandboxes), podman as the v1 driver.
- Windows, WSL and UDP.

## Vocabulary

[GLOSSARY.md](../../GLOSSARY.md) is binding. The spec uses *epic box* for the
Compose project, *role* for a Compose service, *alias* for the dotted name and
*envelope* for the rendered env and secret material.

## The contract

`<repo>/.multica/compose.yaml` is a standard Compose file; `docker compose`
reads it unchanged. `x-msbx` is an extension block carrying only what Compose
cannot express:

```yaml
x-msbx:
  project: signals                  # required, [a-z0-9-]+, unique per repository
  publish:                          # role -> container port; order fixes host ports
    pg: 5432
    web: 3000
  env:
    web: [DATABASE_URL, STRIPE_KEY] # names only; values come from the store
  secrets:
    web: [SMTP_PASSWORD]            # files at /run/msbx/secrets/<NAME>
  tunnels:
    pg:                             # a role served by a tunnel, not a container image
      host: db.internal.example.com
      port: 5432
      via: ssh                      # ssh | cloudflared
services:
  pg:
    image: postgres:17
    environment:
      POSTGRES_PASSWORD: ${PG_PASSWORD}
  web:
    build: .
    depends_on: [pg]
```

Rules:

- `project` is required. It must not contain the epic; the epic key is added by
  `msbx`. A file without `x-msbx` is not a sandbox manifest.
- `publish` lists the roles reachable from the host. Inside the network a role
  is reachable on its native container port whether or not it is published.
- `env` and `secrets` list names. A name with no value in the store is an error
  before any container starts.
- A role named in `publish`, `env`, `secrets` or `tunnels` must exist as a
  service, except a tunnelled role, which `msbx` generates.
- `tunnels.<role>` and a service `<role>` in the same file is an error: a role
  is either local or remote, never both.
- Unknown `x-msbx` keys are an error, not a warning. Values are never written
  into this file.

## Names, ports and zones

- Name: `<role>.<epic>.<project>.msbx.test`. It resolves to `127.0.0.1` on the
  host and is a network alias inside the epic's Compose network, so the same
  string works from both sides.
- Container: `<project>-<epic>-<role>`; Compose project `<project>-<epic>`.
  Epic keys are lowercased (`MIR-178` → `mir-178`).
- Host port: the first `up` allocates the epic a block of 16 consecutive
  `127.0.0.1` ports from 23000 upward and records it. The block maps roles in
  `publish` order (`block+0`, `block+1`, …). The port is stable across restarts
  and is released by `gc` after `rm`.
- `msbx` exports `MSBX_<ROLE>_HOST` and `MSBX_<ROLE>_PORT` (role uppercased,
  `-` → `_`) to the harness and to the Compose process. Store values may use
  `{{host:<role>}}`, `{{port:<role>}}`, `{{epic}}` and `{{project}}`; `msbx`
  substitutes them per epic at render time.
- `msbx.test` is the only zone. `msbx dns` answers `A 127.0.0.1` for any name
  under it and `NXDOMAIN` for everything else. There is no dnsmasq, no
  `/etc/hosts` edit and no alias pool.

## DNS installation

`msbx init` installs and `doctor` verifies:

- macOS: `/etc/resolver/msbx.test` containing `nameserver 127.0.0.1` and
  `port 5335`, plus a LaunchAgent (`test.msbx.dns`) running `msbx dns`.
- Linux: `/etc/systemd/resolved.conf.d/msbx.conf` with `DNS=127.0.0.1:5335`
  and `Domains=~msbx.test`, plus a `systemd --user` unit running `msbx dns`.
- `--check` changes nothing. A non-interactive `init` prints the one privileged
  command it needs and fails C1 with that instruction.

## Environment and secrets

The store is outside every checkout:

```
~/.config/msbx/projects/<project>/env              NAME=value lines, 0600
~/.config/msbx/projects/<project>/secrets/<NAME>   one value per file, 0600
```

- `msbx up` renders `env/<role>.env` and `env/harness.env` under
  `~/.local/state/msbx/<project>/<epic>/`, injects them through a generated
  override (`env_file`), and bind-mounts each requested secret read-only at
  `/run/msbx/secrets/<NAME>`. The service also sees `<NAME>_FILE`.
- `msbx env export --epic E` prints shell-safe `export` lines; the wrapper
  evaluates it, so the harness process gets the values. `msbx env ls` prints
  names only.
- A value appears in no checkout, no `x-msbx` block, no `msbx` output and no
  log line `msbx` writes. `docker inspect` still exposes container environment:
  that is a stated limit, not a promise. Values are passed to Compose through
  the generated files, never argv.
- The store is `kind = "file"` in v1; an `age` store lands behind the same
  names and paths.

## Lifecycle

- `up` — validate the manifest, allocate or reuse the port block, render,
  `docker compose -p <project>-<epic> -f <file> -f <override> up -d --wait`,
  record state. Idempotent; a changed `publish` set reallocates and says so.
- `down` — `docker compose stop`; containers, networks and volumes stay.
- `rm` — `docker compose down -v`; the state entry is removed and the block
  freed.
- `status` — per epic: services, health, host ports, last use; `--json` for
  scripts.
- `gc` — remove epics idle for 14 days and release stale blocks; `--dry-run`
  lists what it would remove. There is no background daemon in v1.
- `msbx` refuses a checkout outside `$HOME` and refuses a workdir under
  `~/projects`.

## CLI surface

| Verb | Purpose |
| --- | --- |
| `doctor` | Report Docker, Compose, the resolver, the daemon, leftover zones. Changes nothing. |
| `init [--check] [--keep]` | Install the resolver, run C1–C7 against a synthetic two-epic project, print the table. |
| `up [--epic E] [--project P] [--file F]` | Start or reconcile the epic box. |
| `down [--epic E]` | Stop the epic box, keep state. |
| `rm [--epic E] [--yes]` | Destroy the epic box and its volumes. |
| `status [--epic E] [--json]` | Show the boxes and their ports. |
| `env ls\|export [--epic E]` | List names, or print exports for the harness. |
| `gc [--dry-run]` | Sweep idle boxes and stale blocks. |
| `dns [--listen ADDR]` | The resolver daemon behind the installed unit. |

## Wrapper

The Multica runtime profile's `command_name` becomes a `msbx-run` wrapper,
shipped in this repository. It runs on the host and:

1. Resolves the project: `MSBX_PROJECT`, else `x-msbx.project` in the
   checkout's `.multica/compose.yaml`, else the repository name.
2. Resolves the epic: `MSBX_EPIC`, else the checkout's `agent/*/mir-<n>-*`
   branch → the issue → its parent epic (cached per task). A resolution failure
   is a loud error, not a silent default.
3. If the checkout has a manifest: `msbx up`, then evaluates
   `msbx env export`; otherwise passes through untouched.
4. `--probe` and `--list-models` launches never start a box.
5. A failed `up` fails the launch by default; `MSBX_RUN_SOFT=1` passes the
   prompt through with the failure printed.
6. Executes the real `dsh` with the original arguments and stdin.

## External services

A remote database is an in-epic tunnel, not a host port forward. `msbx`
generates a sidecar on the epic's network that listens on the role's container
port, terminates the tunnel (`ssh -N -L` or `cloudflared access tcp`), and
carries the role's dotted alias. The application's config is identical whether
the role is local or remote, and nothing is published on the host.

## Verification

- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
- `msbx init` runs C1–C7 against a synthetic project and prints
  `PASS`/`FAIL`/`SKIP` with observed values; non-zero exit on any non-PASS.
  - C1 host — Docker Engine ≥ 24, Compose ≥ 2.24, daemon reachable, resolver
    installed.
  - C2 ports — two epics, Postgres on `5432` in both, distinct host ports.
  - C3 reachability — TCP connect from the host to both assigned ports.
  - C4 names — both dotted names resolve to `127.0.0.1` through the resolver.
  - C5 in-epic — a client reaches `pg` by service name and by dotted alias.
  - C6 isolation — epic one cannot resolve epic two's name from inside; the
    observed error is recorded.
  - C7 env — an env value and a secret file are visible inside, and the secret
    is absent from the checkout.
- End-to-end on this host: two epics run the same stack; the wrapper starts a
  box for a real task and the harness sees the exports.
- Limits: Docker Desktop must be running (and is licensed per Docker's terms);
  the daemon cannot be started headlessly without `open -a Docker`; Docker
  Desktop's VM is shared, so this is isolation, not a security boundary.

## Proposed breakdown

| Stage | Ticket | Delivers |
| --- | --- | --- |
| 1 | [MIR-179](https://multica.ai/miran248/issues/MIR-179) | This document and the ADR revision; closes on merge into `spec/mir-178`. |
| 2 | [MIR-258](https://multica.ai/miran248/issues/MIR-258) | `doctor`, `init`, `msbx dns` and its install, manifest parsing, the port-block allocator and state file; C1–C7 pass on macOS 15 and in Linux CI. |
| 3 | [MIR-259](https://multica.ai/miran248/issues/MIR-259) | `up`, `down`, `rm`, `status`, `gc`; override generation (container names, aliases, loopback ports); two epics on the same stack. |
| 4 | [MIR-260](https://multica.ai/miran248/issues/MIR-260) | The store, render paths, `env ls\|export`, secret mounts, `{{…}}` substitution; a service and the harness receive the same values. |
| 5 | [MIR-261](https://multica.ai/miran248/issues/MIR-261) | `msbx-run`, epic resolution from the task branch, profile wiring, probe pass-through. |
| 6 | [MIR-262](https://multica.ai/miran248/issues/MIR-262) | `tunnels` schema and the generated sidecar; an epic service reaches a remote database by role name, no host port. |
| 7 | [MIR-263](https://multica.ai/miran248/issues/MIR-263) | `spec/mir-178` → `main`, remove the Apple spike, align project and repository descriptions. |

## Risks

- Host port blocks can collide with an unrelated local service; allocation must
  fail loudly, never silently choose a port in use.
- macOS resolver files are system state; `doctor` must detect a stale or
  conflicting `msbx.test` entry and a stopped daemon.
- Compose override drift: the generated file is the only place names, ports and
  env files are decided; regenerating it on every `up` is mandatory.
- Docker Desktop licensing for larger organisations; Docker Engine on Linux is
  unaffected.
