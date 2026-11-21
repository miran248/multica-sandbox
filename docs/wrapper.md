# The harness container shim

Status: delivered by [MIR-273](https://multica.ai/miran248/issues/MIR-273),
stage 3 of [MIR-270](https://multica.ai/miran248/issues/MIR-270). The wrapper
from [MIR-261](https://multica.ai/miran248/issues/MIR-261) now runs Pi in a
per-run container. The shim names whatever digest the pin records; the image
itself is built and pinned in stage 4 ([images/README.md](../images/README.md)).

`msbx-run` is the command the Multica runtime profile launches instead of the
harness. It resolves the scope and the project, takes a lease, starts or
reconciles the scope's box, renders the harness envelope, seeds the agent dir,
and runs Pi in one container with stdin, stdout and stderr attached. It
propagates the container's exit status, releases the lease and removes the
container with `--rm`; the last lease of a lease-managed box stops the box.

```
profile command  →  msbx-run  →  lease + msbx up  →  docker run <pin> pi "$@"
                                 →  release, down on the last lease
```

## The launch

1. **Discovery** — `--version` prints the pinned Pi version from
   `$XDG_STATE_HOME/msbx/image.json` and never needs Docker. `--probe`,
   `--list-models` and the RPC discovery args
   (`--mode rpc --no-session …`) run in a throwaway container from the pinned
   digest: no scope, box, lease or mount, and the harness store as the only
   environment file. `--list-models` output is cached by digest under
   `$XDG_STATE_HOME/msbx/cache/`.
2. **Scope** — `MSBX_SCOPE` (refused when invalid), the daemon task context
   (`issue_id`, else `chat_session_id`), or the worktree branch
   `agent/*/mir-<n>-*`. There is no parent lookup and no silent default.
3. **Project** — `x-msbx.project` in the manifest, else `MSBX_PROJECT`, else a
   key derived from the checkout directory. A `MSBX_PROJECT` that disagrees
   with the manifest is refused.
4. **Run id** — `MULTICA_TASK_ID`, else `MULTICA_RUN_ID`, else `run-<pid>`. The
   lease file and the container name (`<project>-<scope>-harness-<task8>`) use
   it.
5. **Agent dir** — `$XDG_STATE_HOME/msbx/agents/<agent-id>/pi`, seeded once
   from the image's `/opt/msbx/agent/` plus `roles/<agent-name>/` and marked by
   `.msbx-seed` with the digest and the agent name. The seed is staged and
   renamed, so a failed copy is not committed. A baseline change is a manual
   reset: remove the dir and the next run seeds it.
6. **Lease and box** — the lease is written before `msbx up --lease <task>`.
   The box renders `env/harness.env`; a manifest with services or tunnels adds
   the scope's Compose network. A checkout without a manifest runs on the
   default bridge with no service pairs and no box.
7. **Container** — `docker run --rm -i --init` with the four mounts, the host
   UID/GID, the harness env file, the daemon's `MULTICA_*` names and the image
   digest, then Pi's argv verbatim.
8. **Exit** — the container's status becomes the shim's. The lease is released;
   when it was the last live lease and `up` marked the box lease-managed, the
   shim runs `msbx down` (volumes, envelope and block stay). A SIGINT, SIGTERM
   or SIGHUP removes the container, releases the lease, runs the same last-lease
   check and exits 130.

## The four mounts

Each task-scoped path is mounted at its identical absolute host path:

| Path | Mode |
| --- | --- |
| the task workdir (the shim's cwd) | rw |
| `MULTICA_TASK_CONFIG_ROOT`, when set | rw |
| the parent directory of the argv's `--session`, when present | rw |
| the agent dir, at `/home/agent/.pi` | rw |

There is no `$HOME`, no `~/.ssh`, no `~/.config`, no Docker socket and no
ambient checkout. `MULTICA_TASK_CONFIG_ROOT` and the session directory must
exist before the box starts; a missing one is a loud error. `--session` names a
file in the daemon's session store, so the path identity is what Pi's resume
needs.

## What the container sees

The host environment is not inherited. The shim passes only the daemon's
`MULTICA_*` names, by name rather than by value, and rewrites
`MULTICA_CLI_PATH` to `/usr/local/bin/multica`; the values therefore never
appear in Docker's argv. `HOME`, `PATH` and `PI_*` come from the image.
`env/harness.env` is passed as `--env-file` (mode 0600) and carries the
container ports, every declared env value, every declared secret's value and
the harness store.

On Linux the shim adds `host.docker.internal:host-gateway`; Docker Desktop
provides it on macOS. The image's entrypoint forwards
`127.0.0.1:$MULTICA_DAEMON_PORT` to the host, so the in-image CLI's hard-coded
loopback endpoints work. The container joins the scope's Compose network, so
`MSBX_<ROLE>_HOST`/`_PORT` and the dotted alias reach a published role
directly.

## Leases and sweeps

The lease is
`$XDG_STATE_HOME/msbx/leases/<project>/<scope>/<task>.json`: host PID, start
time, container name. It is stale when the PID is gone or it is older than 24
hours. `msbx up` drops its scope's stale leases and removes that scope's orphan
harness containers; `msbx gc` sweeps every stale lease and every harness
container labelled `com.msbx.harness=true` whose lease is stale or gone. A shim
killed before it can release is therefore cleaned up: its container by the
label, its lease by the PID check. `gc --dry-run` lists both without changing
anything.

The lease write and the last-lease check take the same lock beside the leases.
A run that starts as the previous one exits either records its lease before the
check (and keeps the box) or waits for the stop and starts the box again; two
shims exiting together cannot both leave it up, or both stop it.

## Failure modes

| Situation | Result |
| --- | --- |
| Scope cannot be resolved | exit 1, no box and no container |
| Image pin missing or malformed | exit 1, names `msbx image build` / `msbx image pin` |
| Seed fails | exit 1, no agent dir, no lease |
| `msbx up` fails | exit 1, lease released, no container |
| Container cannot launch | exit 1, lease released, box stopped if last |
| Container exits non-zero | that status is the shim's |
| SIGINT, SIGTERM or SIGHUP | container removed, lease released, last-lease check, exit 130 |
| Shim is SIGKILLed | stale lease and orphan container are swept by the next `up`/`gc` |

There is no host-harness fallback and no soft mode: a stopped Docker fails the
run loudly. `MSBX_RUN_SOFT` is retired.

## Profile wiring

Build and install both binaries:

```sh
cargo install --path .
command -v msbx msbx-run
```

In the Multica runtime profile, set `command_name` to `msbx-run` and keep the
daemon-injected `MULTICA_CLI_PATH`; the shim rewrites it. No `MSBX_HARNESS` is
read. The profile has no fixed args. The daemon discovers the version with
`--version` and the models with `--list-models`/the RPC probe.

Rollback is the profile edit in reverse: point `command_name` back at the host
harness and remove the shim from `PATH`. Boxes and leases stay; `msbx down
--scope S` stops a box, `msbx rm --scope S --yes` destroys it.

## Limits

- The shim is not a security boundary against the image or the workspace
  repository: project trust `always` loads the mounted `.pi/` and `AGENTS.md`.
  The blast radius is the checkout, the session, the task token and the daemon
  port — no host home, SSH key or Docker socket.
- SIGINT, SIGTERM and SIGHUP are handled; a SIGKILL has no cleanup hook, so its
  stale lease and orphan container are left to the next `up` or `gc`. There is
  no background supervisor in v1.
- `--version` records no image build time, and `--probe` reports the image's
  answer, not the box's health.
- Probes read the harness store only; a project store value is not available to
  a probe because no scope is resolved.
- The task-context marker is daemon-owned and read-only. A runtime that does
  not write one needs `MSBX_SCOPE` or an `agent/*/mir-<n>-*` checkout branch.
