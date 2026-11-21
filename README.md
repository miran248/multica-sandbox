# multica-sandbox

One sandbox namespace per epic, on the machine that runs the agent, outside
Multica. The `msbx` binary gives every epic its own Compose project, its own
service state, its own loopback ports and its own host names, so two epics can
run Postgres on `5432` at the same time and both stay reachable from the host.

```
pg.mir42.signals.msbx.test     →  127.0.0.1:23000   signals-mir42-pg
web.mir42.signals.msbx.test    →  127.0.0.1:23001   signals-mir42-web
pg.mir42.hours.msbx.test       →  127.0.0.1:23016   hours-mir42-pg
```

`<role>.<epic>.<project>.msbx.test` is the name; `<project>-<epic>-<role>` is
the container. The name resolves to `127.0.0.1` and is a network alias inside
the epic. Names stay, host ports move: the port comes from the epic's block and
is written into the env (`MSBX_<ROLE>_PORT`), never hard-coded on the host.

## Status

**Stage 6 delivered.** On top of the Docker foundation (`doctor`, `init`, the
`msbx dns` resolver and its macOS/Linux installation, the `x-msbx` manifest
reader and the per-epic port-block allocator) the Compose lifecycle is in
place: `up`, `down`, `rm`, `status` and `gc` drive one Compose project per epic,
and the generated override decides container names, dotted network aliases and
loopback port mappings. The env store renders per-role and harness envelopes
outside the checkout, and `msbx-run` wraps the agent launch so a task starts its
epic's box and the harness sees the exported values. `x-msbx.tunnels` turns a
remote service into a generated sidecar on the epic network — addressed as
`pg:5432` by a peer, with no host port — over `ssh` or `cloudflared`. Two epics
run the same Postgres stack at once. The release into `main` is the remaining
stage of [MIR-178](https://multica.ai/miran248/issues/MIR-178).

## Install

```sh
brew install miran248/tap/msbx   # macOS, once the tap exists
cargo install --path .           # before the tap, or without Homebrew
msbx init                        # once after install
```

Both binaries — `msbx` and `msbx-run` — are installed. Run `msbx init` once
after install; "Host setup" below describes what it installs and checks. The
Homebrew install is a symlink under `$(brew --prefix)/bin`, which
`brew upgrade` re-points, so the installed unit keeps working; if `msbx` was
run from a versioned Cellar path, re-run `msbx init` after an upgrade to
rewrite the unit.

## Host setup

`msbx init` is the first command on a fresh host and is idempotent. On macOS 15
it writes `/etc/resolver/msbx.test` (`nameserver 127.0.0.1`, `port 5335`) and a
LaunchAgent `test.msbx.dns` running `msbx dns`. On Linux it installs
`/etc/systemd/resolved.conf.d/msbx.conf` and a `systemd --user` unit. When the
process cannot prompt for `sudo`, `init` prints the one privileged command it
needs and fails C1 with it.

It then runs C1–C7 against a synthetic two-epic project: both epics run
Postgres on `5432` on distinct loopback ports, the host reaches both, the dotted
names answer `127.0.0.1`, a peer reaches `pg` by service name and alias, the
epics cannot reach each other, and an env value and a secret file are visible
without the secret entering the checkout. Any `FAIL` exits 1, any `SKIP` exits
2.

`msbx init --check` installs nothing: it still runs the container checks, then
removes them and its temporary state. C1 records the resolver and the unit;
when the unit is absent (as in CI), C2–C7 still run against an ephemeral
`msbx dns`, so the table measures the contract even where the privileged
install is impossible. CI runs it on `ubuntu-latest` and records the table in
the job summary. The installed unit runs the path of the binary that ran
`init`, so run it from the installed location (or re-run `init` after moving
`msbx`).

## State and ports

The allocator records a 16-port block per epic in
`~/.local/state/msbx/state.json`. The block is reused unchanged for the same
epic and released by `msbx gc` after 14 idle days. Allocation skips a block whose
ports are already bound on `127.0.0.1` and fails loudly when none is free.

`up` also records the epic's keys, its checkout and the role → host-port mapping
it applied. The block itself never moves while the box exists: changing
`x-msbx.publish` re-derives the mapping inside the same block, recreates the
affected containers and prints the old and new ports, and a second `up` with an
unchanged manifest changes no container and no port. `rm` frees the block
immediately; `gc` frees it when the box has been idle for 14 days.

## Lifecycle

`msbx up` and its siblings work on one epic at a time. The epic is `--epic`
(lowercased, so `MIR-42` is `mir-42`) or `MSBX_EPIC`; the wrapper supplies it for
a task. The project comes from `x-msbx.project`; `--project`/`MSBX_PROJECT` may
name it but must agree with the manifest. The checkout must be under `$HOME`, and
`~/projects` is refused.

```sh
msbx up --epic mir-42 [--file .multica/compose.yaml]
msbx status [--epic mir-42] [--json]
msbx down --epic mir-42
msbx rm --epic mir-42 --yes
```

`up` validates the manifest first, so an unknown key or a role named in
`x-msbx.publish` that is not a service stops before any container starts. It then
renders the override to
`~/.local/state/msbx/<project>/<epic>/override.yaml` (outside the checkout,
regenerated every run) and runs
`docker compose -p <project>-<epic> --project-directory <checkout> -f
.multica/compose.yaml -f <override> up -d --wait`. The override gives every
service its `container_name`, the alias
`<role>.<epic>.<project>.msbx.test`, the `com.msbx.*` labels, and each published
role its `127.0.0.1:<assigned port>:<container port>` mapping; the epic's default
network is named `<project>-<epic>`, so two epics never share one.

`down` stops the containers and keeps the volumes, the envelope and the block.
`rm` runs `docker compose down -v --remove-orphans` and frees the block; a
non-interactive `rm` requires `--yes`. `down` and `rm` are no-ops for an absent
box, and both act on the Compose project name alone, so they work after the
checkout is gone. `status` shows each recorded box's services, health and
assigned ports (`--json` for scripts), marking a tunnelled role as
`tunnel:<via>`. `gc --dry-run` lists boxes idle for 14 days without changing
anything; `gc` stops them, removes their volumes and releases their blocks.

## Remote services

`x-msbx.tunnels` declares a role that is served by a tunnel sidecar instead of a
Compose service: `host` is the remote service, `port` is its port and the port
the sidecar listens on, and `via` is `ssh` or `cloudflared`. `msbx up` generates
the sidecar on the epic's network with the role's dotted alias, so a peer reaches
it at `pg:5432` and `pg.<epic>.<project>.msbx.test:5432` exactly as it reaches a
local `pg`. Nothing is published on the host unless `x-msbx.publish` lists the
role, and the sidecar gets the env and secret names the role declares — the
transport's credentials. See [docs/tunnels.md](docs/tunnels.md) for the two
transport contracts and their limits.

## Agent launch

The Multica runtime profile launches `msbx-run` instead of the harness. In a
checkout with `.multica/compose.yaml` it resolves the epic (`MSBX_EPIC`, else the
task's checkout branch or the daemon task context → the issue → its parent epic,
cached per task), runs `msbx up`, exports the rendered envelope into the harness
process, and execs the real harness with the original arguments and stdin. Any
other checkout is passed through untouched, and `--probe` / `--list-models`
launches never start a box. A failed sandbox step fails the launch;
`MSBX_RUN_SOFT=1` prints the failure and passes the prompt through. The profile
change and the rollback step are in [docs/wrapper.md](docs/wrapper.md).

## Commands

```sh
msbx up [--epic E] [--project P] [--file F]   # start or reconcile the epic box
msbx down [--epic E] [--project P]            # stop it, keep its volumes
msbx rm [--epic E] [--project P] [--yes]      # destroy it, free its block
msbx status [--epic E] [--project P] [--json] # services, health and ports
msbx gc [--dry-run]                           # remove boxes idle for 14 days
msbx doctor                                   # describe the Docker host
msbx init [--check] [--keep]                  # install the resolver, run C1-C7
msbx dns [--listen ADDR]                      # the msbx.test resolver (127.0.0.1:5335)
msbx-run [harness args...]                    # the profile's wrapper: up, export, exec
```

## The contract

- **One namespace per epic** — Compose project `<project>-<epic>`, containers
  `<project>-<epic>-<role>`, created on `up`, stopped on `down`, destroyed on
  `rm`, swept by `gc`.
- **Names, not fixed ports** — `<role>.<epic>.<project>.msbx.test` is both the
  host name and the in-network alias; each published role gets a stable loopback
  port from the epic's block and the dotted name resolves to `127.0.0.1`.
- **Env and secrets by name** — the project's `.multica/compose.yaml` `x-msbx`
  block lists names; the store holds values outside every checkout, and `msbx`
  renders per-role env files and mounts `/run/msbx/secrets/<name>` (0600).
  `msbx-run` exports the harness envelope into the agent's process.
- **Remote is local-shaped** — `x-msbx.tunnels` generates an in-epic sidecar
  (`ssh` or `cloudflared`) that answers to the role's name and port; there is no
  host port forwarding.
- **Portable definitions** — the engine is here; each project repository owns
  its own manifest and volumes.

## Build

```sh
cargo build          # builds msbx and msbx-run
cargo test
cargo run -- doctor
cargo run -- init
```

`msbx init` is idempotent and needs Docker running; the resolver install is the
one `sudo` prompt. `--check` installs nothing while still running the C1–C7
container checks, and `--keep` leaves those containers in place for inspection.
A lifecycle run needs the `msbx dns` unit installed (or a `msbx dns` started for
the session) for the dotted names to resolve on the host; the port mapping
itself works without it. Install both binaries as in [Install](#install) before
pointing a runtime profile at `msbx-run`.
