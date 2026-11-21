# multica-sandbox

One harness container per run and one sandbox box per scope, on the machine
that runs the agent, outside Multica. `msbx-run` starts a scope's services and
runs Pi in a per-run container from a pinned image; `msbx` owns the box —
Compose project, service state, loopback ports and host names — so two sibling
tickets run Postgres on `5432` at the same time and neither sees the other's
box.

```
run    →  msbx-run  →  msbx up  →  docker run pi  →  down on the last lease
scope  →  <project>-<scope>            containers <project>-<scope>-<role>
name   →  <role>.<scope>.<project>.msbx.test  →  127.0.0.1:<assigned port>
```

A scope is a ticket (`mir-42`) or a chat session (`chat-1a2b3c4d`).
`<role>.<scope>.<project>.msbx.test` is the name; `<project>-<scope>-<role>` is
the container. Names stay, host ports move: the port comes from the scope's
block and is written into the env (`MSBX_<ROLE>_PORT`), never hard-coded on the
host. The harness reaches a published role by the same pair inside the scope's
Compose network.

## Status

**Stage 3 on `spec/mir-270`.** The contract is
[docs/specs/0002-container-harness.md](docs/specs/0002-container-harness.md);
the decision record is [ADR 0002](docs/adr/0002-container-harness.md). The box is
keyed by scope and is state only ([MIR-272](https://multica.ai/miran248/issues/MIR-272)),
and `msbx-run` is now the container shim: four identical-path mounts, a lease,
the scope network, the agent dir, the pinned digest, discovery from the pin and
C8 ([MIR-273](https://multica.ai/miran248/issues/MIR-273)). The image is stage 4,
verified here against a fixture pin; the release promotes the branch into
`main`.

## What it will manage

- **One container per run** — Pi runs from the pinned image with the task
  workdir, the task config root, the provider session dir and the agent dir
  mounted at their identical absolute paths. No host `$HOME`, no SSH key, no
  Docker socket, no ambient checkout, no inherited environment beyond
  `MULTICA_*` and the store layers.
- **One box per scope** — Compose project `<project>-<scope>`, containers
  `<project>-<scope>-<role>`, created on `up`, stopped when the last lease
  leaves, destroyed by `rm` or by the final-state sweep. Sibling tickets get
  separate boxes, so a database is never shared; a chat box is removed after two
  idle days.
- **State only, not code** — the box has services, volumes, ports and names.
  The app process runs in the harness container. `msbx` refuses a bind of the
  checkout root or an ancestor of it, and a service `build:` inside or containing
  the checkout.
- **Names, not fixed ports** — `<role>.<scope>.<project>.msbx.test` served by
  the `msbx dns` resolver, with a stable loopback port per published role from
  the scope's block.
- **Three value layers** — the host store, the project store and the
  harness-only store, outside every checkout. The manifest lists names; the
  `host-allow` allowlist decides which host names a repository may take; the
  model credential and GitHub token never enter a service envelope.
- **One image** — built locally and pinned by digest: Pi, the pinned packages,
  the baseline agent dir, git, gh, the Linux Multica CLI, jq, ripgrep, socat and
  the workspace's language toolchains. No registry and no per-project images.

## How a run works

1. The daemon launches `msbx-run` with the harness argv.
2. The shim resolves the scope: `--scope`/`MSBX_SCOPE`, the daemon task context
   (`issue_id` → the ticket key, `chat_session_id` → `chat-<8>`), or the
   worktree branch. There is no parent lookup and no silent default.
3. It seeds the agent dir on the agent's first run, takes a lease, runs
   `msbx up --lease` for the scope's box and passes the rendered
   `env/harness.env` file.
4. It runs Pi in one container with stdin, stdout and stderr attached, exactly
   four mounts, the host UID/GID, the scope's Compose network, a loopback
   forwarder to the daemon port, the in-image CLI and `gh auth setup-git` when a
   token is present.
5. When the container exits it propagates the status and releases the lease.
   The last lease to leave stops a lease-managed box; the volumes, envelope and
   port block stay. A killed shim's container and lease are swept by the next
   `up` or `gc`.

`--version`, `--probe`, `--list-models` and the Pi RPC discovery args never
start a box: `--version` answers from the pin, the others run in a throwaway
container from the pinned digest.

## Host setup

`msbx init` is the first command on a fresh host and is idempotent. On macOS 15
it writes `/etc/resolver/msbx.test` (`nameserver 127.0.0.1`, `port 5335`) and a
LaunchAgent `test.msbx.dns` running `msbx dns`. On Linux it installs
`/etc/systemd/resolved.conf.d/msbx.conf` and a `systemd --user` unit. When the
process cannot prompt for `sudo`, `init` prints the one privileged command it
needs and fails C1 with it.

It then runs C1–C8 against two synthetic scopes: both run Postgres on `5432` on
distinct loopback ports, the host reaches both, the dotted names answer
`127.0.0.1`, a peer reaches `pg` by service name and alias, the scopes cannot
reach each other, and an env value and a secret file are visible without the
secret entering the checkout. C8 runs a container from the pinned image with
the four mounts, checks the host UID/GID on a bind-mounted write and inside the
container, the in-image CLI, the published alias and the agent-dir seed; it
records a skip until stage 4 pins the image. Any `FAIL` exits 1, any `SKIP`
exits 2. `--check` installs nothing.

## State and ports

The allocator records a 16-port block per scope in
`~/.local/state/msbx/state.json`. The block is reused unchanged for the same
scope and released by `msbx gc` when the ticket is final or after 14 idle days
(2 for a chat). Allocation skips a block
whose ports are already bound on `127.0.0.1` and fails loudly when none is free.

`up` also records the scope's key, its checkout, the manifest and the role →
host-port mapping it applied. The block never moves while the box exists:
changing `x-msbx.publish` re-derives the mapping inside the same block,
recreates the affected containers and prints the old and new ports; a second
`up` with an unchanged manifest changes nothing.

## Lifecycle

`msbx up` and its siblings work on one scope at a time. The scope is
`--scope`/`MSBX_SCOPE`, else the ticket or chat resolved by the shim. The
project comes from `x-msbx.project`; `--project`/`MSBX_PROJECT` may name it but
must agree with the manifest. The checkout must be under `$HOME`, and
`~/projects` is refused.

```sh
msbx up --scope mir-42 [--lease <task>] [--file .multica/compose.yaml]
msbx status [--scope mir-42] [--json]
msbx down --scope mir-42
msbx rm --scope mir-42 --yes
```

`up` validates the manifest first, so an unknown key or a role named in
`x-msbx.publish` that is not a service stops before any container starts. It
then renders the override to
`~/.local/state/msbx/<project>/<scope>/override.yaml` (outside the checkout,
regenerated every run) and runs
`docker compose -p <project>-<scope> --project-directory <checkout> -f
.multica/compose.yaml -f <override> up -d --wait`. The override gives every
service its `container_name`, the alias
`<role>.<scope>.<project>.msbx.test`, the `com.msbx.*` labels, and each
published role its `127.0.0.1:<assigned port>:<container port>` mapping; the
scope's default network is `<project>-<scope>`, so two scopes never share one.

`down` stops the containers and keeps the volumes, the envelope and the block.
`rm` runs `docker compose down -v --remove-orphans` and frees the block; a
non-interactive `rm` requires `--yes`. Both are no-ops for an absent box, and
both act on the Compose project name alone. `status` shows each box's services,
health and ports (`--json` for scripts), marking a tunnelled role as
`tunnel:<via>`. `gc --dry-run` lists what it would remove: a ticket whose issue
is done, cancelled, archived or gone; a chat box idle for 2 days; a ticket box
idle for 14 days, and sweeps stale leases and harness containers whose lease is
stale or gone. `up` makes the same stale-lease and own-scope orphan sweep.

## Environment and credentials

The store is outside every checkout. The manifest lists names; `msbx` resolves
them from the host store and the project store, with the project value winning,
and renders three contexts: per-role files and `env/harness.env` with the
container ports, and `env/host.env` with the assigned loopback ports. A host
name reaches a project only when `host-allow` lists it. The harness-only store
(`~/.config/msbx/harness/env`) carries the model credential, `GH_TOKEN` and the
git identity; it is never rendered into a service envelope. `MULTICA_*`, `PI_*`,
`HOME`, `PATH` and `HOSTNAME` are refused at `up`. `msbx env export` prints
`env/host.env`, the host-shell view; the container ports are only for the
services and the harness.

## The image

`images/` holds the one image: a pinned Debian slim base, Pi standalone, the
pinned Pi packages in a read-only tree, the baseline agent dir with role
overlays, the Linux Multica CLI, and Node with pnpm, Rust with rustfmt and
clippy, Go and Python 3. `images/manifest.json` is the only version source:
the Dockerfile pins the base and toolchain images by digest, the Debian
packages and every download by exact version and checksum, and asserts each
one at build time.

`msbx image build` builds `msbx-pi:<pi>-<n>` from this repository;
`msbx image pin` resolves the tag, refuses a tag that moved since the build,
and records `{image, digest, pi, versions, packages}`; `msbx image show`
prints it. The shim names the image by digest and refuses an absent pin or a
moved tag. A tag is never reused: the build counter counts local tags, the
last build and the pin.

The container installs nothing at runtime. The agent dir
(`$XDG_STATE_HOME/msbx/agents/<agent-id>/pi`) is seeded once from the image
baseline plus a role overlay and is never the host's `~/.pi`; a baseline
change is a manual reset (remove the dir and run again). Pi's credentials come
from the harness store, never from the image. See [images/README.md](images/README.md)
for the contents, the pin, the entrypoint and the reset.

## Runtime registration

The runtime profile is one custom profile with `--runtime-type pi` and
`command_name` = `msbx-run`, no fixed args. The catalog's selector is
provider-agnostic and requires exactly one online runtime; it asserts the
provider family and the model id Pi lists, not a literal model string. The
cutover builds and pins the image, registers the profile, binds the five agents,
proves one real ticket, then removes the DSH profile and host install. Rollback
is the previous image digest and an archived DSH profile, kept until the first
green release on `main`.

## Commands

```sh
msbx up [--scope S] [--file F]  # start or reconcile the scope box
msbx down [--scope S]                          # stop it, keep its volumes
msbx rm [--scope S] [--yes]                    # destroy it, free its block
msbx status [--scope S] [--json]               # services and ports
msbx env ls|export [--scope S]                 # names, or exports for a host shell
msbx gc [--dry-run]                            # final-state and idle sweep
msbx doctor                                    # describe the Docker host and the pin
msbx init [--check] [--keep]                   # install the resolver, run C1-C8
msbx image build|pin|show                      # the harness image
msbx dns [--listen ADDR]                       # the msbx.test resolver (127.0.0.1:5335)
msbx-run [harness args...]                     # the container shim
```

## The contract

- **One box per scope, one container per run** — the box is the scope's state,
  the container is the run's code. Sibling tickets share no box.
- **Names, not fixed ports** — `<role>.<scope>.<project>.msbx.test` is the host
  name and the in-network alias; each published role gets a stable loopback
  port from the scope's block.
- **Env and secrets by name** — the manifest lists names; the stores hold
  values outside every checkout; services get their files and secret mounts,
  the harness gets the whole envelope, and neither sees a value in argv.
- **One image, one runtime** — Pi from a digest-pinned local image; one online
  runtime; no harness on the host.
- **Portable definitions** — the engine is here; each project repository owns
  its own manifest and volumes.

## Build

```sh
cargo build          # builds msbx and msbx-run
cargo test
cargo run -- doctor
cargo run -- init
msbx image build && msbx image pin
```

`msbx init` is idempotent and needs Docker running; the resolver install is the
one `sudo` prompt. C8 runs against the pinned harness image and records a skip until
`msbx image build && msbx image pin` has run. A lifecycle run needs the
`msbx dns` unit installed (or a `msbx dns` started for the session) for the
dotted names to resolve on the host; the harness container joins the scope
network instead. Install both binaries with `cargo install --path .` before
pointing a runtime profile at `msbx-run`.
