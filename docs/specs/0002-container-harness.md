# 0002 — Container harness and per-scope sandbox

Status: proposed. Approved when its pull request merges into `spec/mir-270`.
Epic: [MIR-270](https://multica.ai/miran248/issues/MIR-270).
integration: `spec/mir-270`. Supersedes decisions 1, 2 and 12 of
[ADR 0001](../adr/0001-per-epic-sandbox.md); [ADR 0002](../adr/0002-container-harness.md)
records the decision and the rejected alternatives.

## Problem

Every task runs its harness on the host, with the host's filesystem,
environment and agent config, and the box is keyed by the epic. Four costs
follow.

- **Prompt content is ambient.** The harness loads `~/.agents`, `~/.claude`,
  `~/.codex` and friends, so a run's prompt carries skills and rules nobody
  bound; what a seat sees cannot be controlled from Multica.
- **The harness is host infrastructure.** DSH needs Node, Python and pnpm on
  the host (about 310 MB under `~/.dsh/dsh-runtimes/`), plus a per-machine
  wrapper. A harness change is a host change, and every project shares one
  runtime.
- **Projects are not separated.** Every checkout and every project's values
  live under one user. ADR 0001 isolates services, not the agent.
- **Boxes collide across tickets.** The box is per epic, so sibling tickets
  promoted in one stage share one database and one `rm` stops another task's
  services.

## Outcome

A Multica task starts one harness container for the run and, where the
checkout carries a manifest, one state box for the scope. The container carries
Pi, the pinned packages, the in-image CLI and one run's mounts; it is gone when
the task ends. The box carries only services, volumes, ports and names, and two
sibling tickets of one project run the same stack at once without sharing a
database. Chat runs get the same container and, where a manifest exists, the
same box, keyed by the chat session. One locally built image serves all five
seats, and exactly one runtime is online.

```
run:    msbx-run → scope + lease → msbx up → docker run pi → msbx stop on last lease
scope:  <project>-<scope>   containers <project>-<scope>-<role>
name:   <role>.<scope>.<project>.msbx.test
```

## Non-goals

- A per-project image, a registry, a toolbox catalog, or live code in a service
  (`build:` from the checkout). Terraform, kubectl, wrangler, hcloud and scw
  wait for a consumer.
- Per-epic boxes, a host-side harness, OMP, Windows and WSL.
- Changing the Docker Compose driver, the `x-msbx` schema, the `msbx dns`
  resolver, the port-block layout, tunnels, or `/run/msbx/secrets`.
- A daemon change in the Multica product. The shim absorbs the container.

## Vocabulary

[GLOSSARY.md](../../GLOSSARY.md) is binding. A **scope** is a ticket
(`mir-42`) or a chat session (`chat-1a2b3c4d`). A **ticket box** or **chat box**
is that scope's Compose project. A **harness container** is the per-run
container that runs Pi. A **lease** is a run's claim on its box. The **agent
dir** is the persistent per-agent Pi home. The **image** is the pinned harness
image. The **run** is one daemon task; **scope** is the box's unit, **run** is
the container's.

## Scope keys and names

The key is `[a-z0-9][a-z0-9-]{0,47}` and comes from, in order:

1. `--scope` or `MSBX_SCOPE`, normalized (`MIR-42` → `mir-42`); an invalid key
   is a loud error.
2. The task-context marker `.multica/daemon_task_context.json`, beside or above
   the workdir: `issue_id` → `multica issue get <uuid>` → the issue's identifier
   lowercased; otherwise `chat_session_id` → `chat-` plus its first eight
   characters. Both present means the issue wins.
3. The worktree branch, when it matches `agent/<role>/mir-<n>[-<slug>]` →
   `mir-<n>`. The branch names the issue; the issue is the scope. The parent
   lookup is deleted.
4. Anything else is an error. There is no silent default.

Names and tokens:

- Name: `<role>.<scope>.<project>.msbx.test`. It resolves to `127.0.0.1` on
  the host through `msbx dns` and is a network alias in the scope's Compose
  network, so the same string works from both sides.
- Container: `<project>-<scope>-<role>`; Compose project and network
  `<project>-<scope>`. A harness container is
  `<project>-<scope>-harness-<task8>`.
- Block: 16 consecutive `127.0.0.1` ports from 23000 upward, one block per
  scope. The first `up` allocates; the block is stable for the scope's life and
  is released by `rm`/`gc`.
- `MSBX_<ROLE>_HOST` is the alias. `MSBX_<ROLE>_PORT` is the role's port in
  the render's context: the assigned loopback port in `env/host.env` (what
  `msbx env export` prints for a host shell), and the role's container port in
  `env/harness.env` and `env/<role>.env` (a service or the container on the
  scope network, the value `publish` or `tunnels` maps). Each pair is
  self-consistent for its side; code that uses the pair works on the host and
  in the harness, and code that hard-codes a port is wrong by construction.
- `{{scope}}` replaces `{{epic}}`. `{{epic}}` is an error naming `{{scope}}`,
  never a silent alias. `{{host:<role>}}` is the alias in both contexts.
  `{{port:<role>}}` renders per context too: the role's container port in the
  service and harness contexts, because every consumer there is on the scope
  network, and the assigned loopback port in the host context, because that is
  what a host shell can reach. It was one value while the harness ran on the
  host. `{{port:…}}` of an unpublished role is still an error.
- `--epic`, `MSBX_EPIC` and the `Epic box` vocabulary are removed; each fails
  or reads as scope.

## The harness container

`msbx-run` is the runtime profile's command and runs on the host. It resolves
the scope and the project, acquires the lease, starts or reconciles the box,
renders the harness envelope, seeds the agent dir if needed, and runs Pi in one
container with stdin, stdout and stderr attached. It propagates the harness's
exit status, releases the lease, and removes the container with `--rm`.
Nothing is long-lived.

**Mounts.** The container mounts exactly four host paths, each at its identical
absolute path:

| Path | Source | Mode |
| --- | --- | --- |
| the task workdir | the daemon's run workdir (the shim's cwd) | rw |
| `MULTICA_TASK_CONFIG_ROOT` | the daemon's per-task config root | rw, when set |
| the session dir | `dirname(--session)` from the run's argv | rw, when `--session` is present |
| `/home/agent/.pi` | the agent dir (§ Agent dir and repository trust) | rw |

The first three are the only **task-scoped** host paths. Path identity is the
contract: a host path in a session argument, an env value or a daemon request
resolves to the same file inside the container. It is also what makes Pi's
resume work, because Pi re-anchors a resumed run to the working directory
recorded in the session file and refuses when it is gone. The agent dir is the
only other host path the container sees. There is no `$HOME`, no `~/.ssh`, no
`~/.config`, no Docker socket, and no ambient checkout.

**Environment.** The host environment is not inherited. Only the `MULTICA_*`
names the daemon injected pass through, with `MULTICA_CLI_PATH` rewritten to the
in-image CLI (`/usr/local/bin/multica`), plus the rendered store layers (§
Environment). `HOME=/home/agent`, `PATH` and `PI_*` are set by the image.
Values reach the container through the mode-0600 `env/harness.env` file, never
argv.

**Daemon reach.** The in-image Multica CLI hard-codes `127.0.0.1` for the
daemon's `/repo/checkout` and `/environment` endpoints. Inside the container
that is the container itself, so the image's entrypoint runs a loopback
forwarder from `127.0.0.1:$MULTICA_DAEMON_PORT` to
`host.docker.internal:$MULTICA_DAEMON_PORT`; the shim adds
`host.docker.internal:host-gateway` on Linux (Docker Desktop provides it on
macOS). The CLI is unchanged and works because the path it sends is the
identical workdir path, which the daemon writes on the host and the container
sees through the bind.

**Network.** The container joins the scope's Compose network, so a published
role's alias resolves to its service and `MSBX_<ROLE>_HOST` works directly. A
run whose project has no manifest, or whose box has no services, runs on the
default bridge with no service pairs. The container does not publish ports.

**Identity.** The container runs as the host UID/GID so files written through
the binds keep their owner, with `HOME=/home/agent` and `--init` for signal
handling. `/home/agent` itself is ephemeral and writable by the run's UID (mode
1777 in the image); only `/home/agent/.pi` persists. The entrypoint starts the
forwarder, runs `gh auth setup-git` when `GH_TOKEN` is present so HTTPS remotes
push, and execs Pi with the run's argv verbatim.

## The box is state only

The box is the scope's state: services, volumes, networks, ports and names. It
is the deps-only box — databases, caches, queues and the like. Code is
run-scoped: an application process runs in the harness container and reaches
the box by name and port.

- `up` reads `.multica/compose.yaml` and `.env` from the checkout at start
  time, and the generated override adds only names, aliases, ports, env files,
  secret mounts and tunnel sidecars. It never bind-mounts the checkout root.
- `msbx` refuses a project volume whose source resolves to the checkout root:
  mounting the tree into a service makes code scope-scoped and lets one
  ticket's edit reach a sibling's box. A subpath fixture (`./init.sql`) is
  allowed.
- `msbx` refuses a service `build:` whose context resolves inside the checkout.
  A live-code service is out of scope in v1; the app runs in the harness.
- The volume sources, images and ports are read from the manifest at every
  `up`. The box's volumes and port block persist across runs; the manifest
  follows the last run that called `up`.

## Lifecycle: lease, stop, final state, backstop

**Lease.** The shim writes a lease per run at
`$XDG_STATE_HOME/msbx/leases/<project>/<scope>/<task>.json` holding the host
PID, start time and container name. It writes the lease before starting the
box and releases it after the container exits, on a signal, or when the launch
fails. A lease is stale when its PID is gone or it is older than 24 hours; the
next `up` or `gc` drops it.

- `msbx up --lease <task>` sets `lease_managed` on the box. When the last lease
  is released and the box is `lease_managed`, the shim runs `msbx down`: the
  containers stop, the volumes, envelope and block stay. `up` again starts
  them.
- A manual `msbx up` with no lease leaves `lease_managed` false and the box up;
  a manual `down` stops it. Sibling runs of one scope share the box; sibling
  scopes never do.

**Final state.** A ticket scope is final when `multica issue get` reports
`status_category` `done` or `cancelled`, when `archived_at` is set, or when the
issue is gone. `cancelled` is terminal, not a backstop case: an abandoned epic
must not keep its boxes for 14 days. `msbx gc` removes the box, volumes,
envelope, leases and block; `up` makes the same check for its own scope before
it starts. A chat scope is final when it is idle for two days.

**Backstop.** A ticket box idle for 14 days is removed by `gc`. Idle means no
lease and no `up`, `down` or `env` activity. The sweep also removes harness
containers labelled `com.msbx.harness=true` whose lease is stale. There is no
background daemon: the full sweep runs when `gc` runs, and `up` sweeps its own
scope and its own orphans. `gc --dry-run` lists what it would remove.

## Environment: host, project, harness

Three stores, all outside every checkout and mode 0600:

| Layer | Path | Reaches |
| --- | --- | --- |
| host | `$XDG_CONFIG_HOME/msbx/host.env`, and `host-allow` beside it | services and harness, by allowlist |
| project | `$XDG_CONFIG_HOME/msbx/projects/<project>/env` and `secrets/<NAME>` | services and harness |
| harness | `$XDG_CONFIG_HOME/msbx/harness/env` | harness only |

- The project value wins over the host value for the same name. In the harness,
  the harness value wins over both.
- A project may use a host name only when `host-allow` lists it (one name per
  line, `#` comments). The manifest is repo-controlled, so without the
  allowlist a repository could name and exfiltrate any host value. A manifest
  name found only in the host store and not allowed is an error naming the name
  and the allowlist file.
- The harness layer is not manifest-driven and never enters a service envelope.
  It carries the model credential, `GH_TOKEN`, and the git author/committer
  identity; it is how the container gets credentials without `~/.config`,
  `~/.ssh` or `~/.gitconfig`.
- Reserved names are refused at `up` and at harness render, naming the name,
  the layer and the role: `MULTICA_*`, `PI_*`, `HOME`, `PATH`, `HOSTNAME`. A
  declared name that collides with a generated `MSBX_<ROLE>_HOST`/`_PORT` is an
  error, as today. A role named `harness` in `x-msbx.env` is still refused.
- `msbx up` renders three contexts per-scope under
  `$XDG_STATE_HOME/msbx/<project>/<scope>/`: `env/<role>.env` for a service
  (container ports), `env/harness.env` for the harness container (container
  ports), and `env/host.env` for a host shell (assigned loopback ports), plus
  `secrets/<NAME>` and `override.yaml`. Files an earlier render produced and
  this one does not are removed.
- The harness container receives every declared env value and every declared
  secret's value as an environment variable, because it cannot see the
  services' `/run/msbx/secrets/` mounts and its mount set is closed. The service
  envelope keeps the read-only secret mounts and `<NAME>_FILE`.
- `msbx env export` prints the host context, `env/host.env`: shell-safe
  exports with the assigned loopback ports, the same names as
  `env/harness.env`, and `<NAME>_FILE` for a declared secret pointing at the
  rendered `secrets/<NAME>` (the host can read it; the container gets the value
  instead). It never prints `env/harness.env`, whose container ports are
  unreachable from a host shell. The shim does not export store values into its
  own process, because the harness runs in the container.

## Agent dir and repository trust

The agent dir is `$XDG_STATE_HOME/msbx/agents/<agent-id>/pi`, mode 0700, and is
mounted at `/home/agent/.pi`. It is per agent, persistent across runs and
scopes, and never the host's real `~/.pi`.

- **Seed once.** When the dir is absent, the shim copies the image's baseline
  `/opt/msbx/agent/` into it, then overlays
  `/opt/msbx/agent/roles/<agent-name>/` when present, where the agent name is
  `MULTICA_AGENT_NAME` sanitized. It writes `.msbx-seed` with the image digest
  and the agent name. The baseline is never re-merged; a baseline change is a
  manual reset (remove the dir; the next run seeds it).
- The baseline carries Pi's settings, provider/model config, the
  `pi-subagents` extension and prompt templates. It carries no workspace skill
  bundles: the daemon materialises those per run.
- **Sessions.** The run's `--session` path lives in the daemon's own session
  store, is passed by the daemon as an absolute path, and is mounted at the
  identical path. The agent dir never holds sessions. A probe run with no
  `--session` mounts none.
- **Repository trust.** Project trust is `always` in the baseline, so the
  checked-in `.pi/` of the mounted worktree loads (skills, prompt templates,
  extensions, settings), and `AGENTS.md` loads as today. The daemon writes
  managed skills to `<workdir>/.pi/skills/<name>/` and passes each as
  `--skill`; that directory is part of the workdir mount, so the container sees
  them unchanged.

## The launch contract and the CLI

No Multica daemon change is in scope. The daemon resolves the profile's
`command_name` (`msbx-run`) on PATH, discovers the version with `--version`,
discovers models with `--list-models` and the Pi RPC probe, and launches the run
with the Pi backend's normal argv (`-p --mode json --session … --model …
--thinking … --skill …`). The shim answers each without the daemon knowing a
container exists:

- `--version` prints the pinned Pi version from the image pin, without Docker.
- `--probe`, `--list-models` and the RPC discovery args (`--mode rpc
  --no-session --no-skills --no-prompt-templates --no-context-files`) run in a
  throwaway container from the pinned digest: no scope, no box, no mounts, no
  lease, a throwaway home. `--list-models` output is cached by image digest
  under `$XDG_STATE_HOME/msbx/cache/`. The exit status is the container's.
- Any other argv is a task run and follows the container contract above.

The `msbx` CLI is re-keyed from epic to scope throughout: state, port blocks,
envelope paths, names, `gc`, `doctor`, `env`, docs and tests. `--scope` and
`MSBX_SCOPE` replace `--epic` and `MSBX_EPIC`.

| Verb | Purpose |
| --- | --- |
| `doctor` | Docker, Compose, resolver, image pin, mount roots, stale leases and orphan containers. Changes nothing. |
| `init [--check] [--keep]` | Install the resolver, run C1–C8 against synthetic scopes, print the table. |
| `up [--scope S] [--lease TASK] [--project P] [--file F]` | Start or reconcile the scope box. |
| `down [--scope S]` | Stop the scope box, keep state. |
| `rm [--scope S] [--yes]` | Destroy the scope box, volumes, envelope and block. |
| `status [--scope S] [--json]` | Boxes, services, health, leases, ports. |
| `env ls\|export [--scope S]` | Names, or the host-context exports (`env/host.env`, assigned ports). |
| `gc [--dry-run]` | Final-state removal, idle backstop, stale leases, orphan harness containers. |
| `image build\|pin\|show` | Build the image, record its digest, print the pin. |
| `dns [--listen ADDR]` | The resolver daemon behind the installed unit. |
| `msbx-run [harness args…]` | The container shim. |

## The image

One image, built locally in this repository, pinned by digest. No registry, no
per-project images, no pull.

- **Contents.** A pinned Debian slim base; Pi at the pinned version; the pinned
  packages (`pi-subagents` and the rest) in a read-only `PI_PACKAGE_DIR`; the
  baseline agent dir and role overlays; git, gh, jq, ripgrep, socat and
  ca-certificates; the Linux `multica` CLI at the workspace's version; the
  entrypoint; and the language toolchains the workspace's repositories verify
  with — Node with pnpm, Rust with rustfmt and clippy, Go matching the product
  module, and Python 3 — each version pinned in the image manifest. Pi is
  installed standalone, so it needs none of them at run time; they are there
  for the workspace's projects. Terraform, kubectl, wrangler, hcloud and scw
  are not included.
- **Pinning.** `msbx image build` builds and tags `msbx-pi:<pi>-<n>`.
  `msbx image pin` resolves the tag and writes `{image, digest, built_at}` to
  `$XDG_STATE_HOME/msbx/image.json`. The shim names the image by digest; an
  absent pin is an error with the fix, and a tag that no longer resolves to the
  pinned digest is refused. `msbx image show` prints the pin.
- **No runtime install.** The container installs nothing: every package,
  extension and toolchain version is in the image manifest. `PI_PACKAGE_DIR`
  is read-only and Pi's package manager never writes there.
- **Credentials.** The image carries no credential. The harness store supplies
  the model provider key and `GH_TOKEN`; the entrypoint sets git to use the
  token for HTTPS remotes.

## Runtime registration and the selector

Cutover registers one custom runtime profile:

```sh
multica runtime profile create --display-name Pi --runtime-type pi \
  --command-name msbx-run
```

The profile carries no fixed args. The daemon registers the Pi runtime when
`msbx-run --version` answers; `multica runtime profile set-path` pins the
binary per machine when PATH is not enough.

Exactly one runtime is online. The catalog's selector stops hard-coding `dsh`:
the provider enum gains `pi`, the `flash-first-v1` guard asserts the configured
provider family and requires exactly one online runtime, and the defaults name
the model by the id the pinned image lists at cutover. The catalog does not
hard-code `deepseek-official/deepseek-flash`; a model change is a catalog or
agent edit, not an image rebuild. The DSH profile and its runtime are removed
in the same cutover, so "the provider" is unambiguous.

## Cutover and rollback

The cutover is the release child and is the human's one irreversible step. Order:

1. Build and pin the image; record the digest in the release ticket.
2. Register the Pi profile and confirm `--version`, `--list-models` and one
   probe launch answer from the image.
3. Apply the provider-agnostic catalog and bind all five agents to the Pi
   runtime.
4. Run one real ticket end to end in a box: services up, Pi in the container,
   the daemon CLI works, a published role is reachable by name and port, the
   workdir keeps its owner, and a follow-up resumes the session.
5. Remove the DSH profile and the host install (`~/.dsh`, the `dsh` wrapper and
   `~/.dsh/dsh-runtimes`), leaving one online runtime.
6. Merge `spec/mir-270` into `main` as one squash.

Rollback material is kept until the first green release on `main`: the previous
image digest and an archive of the old DSH profile and host install outside
every checkout, at `$XDG_CONFIG_HOME/msbx/rollback/`. Before DSH removal,
rollback is re-binding the agents to the DSH profile. After removal, it is
restoring the archive, creating the profile and re-binding. The archive is
deleted once `main` is green; after that the release is the rollback.

## Verification

Run and record, not assume:

- **Pi skill bundles.** The daemon materialises managed skills at
  `<workdir>/.pi/skills/<name>/` and passes each path as `--skill`; the image
  sets no `--no-skills` for a task run. C8 records a run with one managed skill
  visible to Pi and the receipt path the daemon reports.
- **The `deepseek/*` model id.** `msbx-run --list-models` against the pinned
  image prints Pi's catalog; the exact DeepSeek family id Pi lists at cutover
  (with its provider prefix) is recorded on the release ticket and used in the
  catalog. Nothing in this repository hard-codes the old
  `deepseek-official/deepseek-flash` literal.
- **Bind-mount ownership.** A file written from inside the container under the
  workdir appears owned by the host user: on Linux the numeric UID/GID is
  preserved, on macOS Docker Desktop the file is mapped to the host user. C8
  records both the host UID and the container's `id -u`, and the same file's
  writer/owner on each platform.

The existing checks stay: `cargo fmt --check`, `cargo clippy -- -D warnings`,
`cargo test`, and CI on `ubuntu-latest`. `init` re-keys C1–C7 from epic to
scope and gains C8, the harness container check (mounts, ownership, in-image
CLI, network alias, agent-dir seed), which is stage 3.

## Proposed breakdown

| Stage | Ticket | Delivers |
| --- | --- | --- |
| 1 | [MIR-271](https://multica.ai/miran248/issues/MIR-271) | This spec, ADR 0002 and the vocabulary updates; closes on merge into `spec/mir-270`. |
| 2 | [MIR-272](https://multica.ai/miran248/issues/MIR-272) | Re-key `msbx` to scope; the state-only box refusals; the host, project and harness stores, the allowlist and reserved names; the final-state `gc` (`done`/`cancelled`), the 14-day backstop and the 2-day chat sweep. |
| 3 | [MIR-273](https://multica.ai/miran248/issues/MIR-273) | `msbx-run` as the container shim: mounts, path identity, leases, the daemon forwarder, the agent dir, probes and orphan sweep. |
| 4 | [MIR-274](https://multica.ai/miran248/issues/MIR-274) | The pinned image: Pi, packages, baseline agent dir, CLI, toolchains; `msbx image build\|pin`. One real ticket run. |
| 5 | [MIR-275](https://multica.ai/miran248/issues/MIR-275) | The provider-agnostic catalog selector in `multica-workspace`; `pi` accepted; exactly one online runtime. |
| 6 | [MIR-276](https://multica.ai/miran248/issues/MIR-276) | Cutover and DSH removal, then `spec/mir-270` → `main` as one squash. |

## Risks

- Docker becomes required for chat as well as tickets; a stopped Docker fails
  every run. The shim fails loudly, never falls back to a host harness.
- Bind-mount ownership on Linux can fail under root-squashed or SELinux hosts;
  C8 records it rather than promising it. The worktree must stay writable by
  the host user.
- The one-image approach couples every project to one toolchain set; a missing
  runtime is an image change, not a project change.
- Project trust `always` lets a repository's `.pi/` extensions run with the
  run's mounts, token and daemon port. The container has no host home, SSH key
  or Docker socket, so the blast radius is the checkout, the session and the
  task token — the same trust as the agent today, now container-wide.
- The loopback daemon forwarder makes the daemon reachable to every process in
  the run; that is the price of the in-image CLI, and it is scoped to the task
  token.
- Name and port pairs are a start-time snapshot; a manifest edited mid-run is
  not re-read until the next `up`.
- The 14-day backstop only fires when `gc` runs. There is no scheduler in v1;
  `gc` is a command, not a daemon.
- `docker inspect` still exposes the container environment, including
  `MULTICA_TOKEN`: a stated limit, not a promise, as before.
