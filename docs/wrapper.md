# The harness wrapper

Status: re-keyed to the scope by
[MIR-272](https://multica.ai/miran248/issues/MIR-272), stage 2 of
[MIR-270](https://multica.ai/miran248/issues/MIR-270); originally delivered by
[MIR-261](https://multica.ai/miran248/issues/MIR-261). The wrapper still runs
the harness on the host in this stage. Stage 3 turns it into the container shim:
it will run Pi in the pinned image with the task mounts instead of exec'ing a
host harness.

`msbx-run` is the command the Multica runtime profile launches instead of the
harness. It decides which scope the task is about, starts that scope's box,
exports the host context and then replaces itself with the real harness, keeping
its arguments, stdin, stdout, stderr and exit status.

```
profile command  →  msbx-run  →  msbx up  →  msbx env export  →  exec dsh "$@"
```

## The launch

1. **Harness** — `MSBX_HARNESS`, else `dsh` on `PATH`. A harness that resolves to
   `msbx-run` itself is refused, so a misconfigured profile cannot recurse.
2. **Probe launches** — an argument equal to `--probe` or `--list-models` is a
   harness question, not a task. The wrapper resolves no scope, starts no box
   and never calls Docker; it execs the harness immediately.
3. **No manifest** — a checkout with no `.multica/compose.yaml`, or a Compose
   file without an `x-msbx` block, is passed through untouched.
4. **Manifest checkout** — the wrapper resolves the scope, reads
   `x-msbx.project` from the manifest, runs `msbx up --scope S --project P` and
   then `msbx env export --scope S --project P`. The exported names are added to
   the harness's environment and the harness is exec'd. `up`'s summary is
   captured, never written to the harness stdout.

`msbx` itself is `MSBX_BIN`, else the `msbx` binary next to `msbx-run`, else
`msbx` on `PATH`. The Multica CLI is `MULTICA_CLI_PATH` (the daemon injects it
for a task), else `multica` on `PATH`.

`msbx env export` prints `env/host.env`: shell-safe `export NAME='value'` lines
with the scope's assigned loopback ports, so the host harness reaches a
published role. The wrapper parses exactly those lines and applies the pairs to
the harness process; it does not hand a store value to a shell and it prints no
value. The envelope files remain the source of truth, so a probe and a task see
the same contract.

## Scope resolution

The scope is, in order:

1. `--scope` from the CLI caller, else `MSBX_SCOPE` (the wrapper has no flag, so it reads the variable the daemon injects). It is normalized
   (`MIR-42` → `mir-42`) and refused with a loud error when it is not a valid
   key. `MSBX_EPIC` is retired: setting it is an error naming `MSBX_SCOPE`, never
   a silent alias.
2. The daemon's task-context marker, `.multica/daemon_task_context.json`, beside
   or above the checkout. It names the issue (`issue_id`) when the checkout is on
   the task's own `agent/<role>/<task-slot>` branch — the branch the daemon
   creates — rather than a branch named after the issue; or the chat session
   (`chat_session_id`), whose scope is `chat-` plus its first eight characters.
   Both present means the issue wins.
3. The checkout branch, when it matches `agent/<role>/mir-<n>[-<slug>]`. The
   branch names the *issue* and the issue is the scope: `mir-<n>`. There is no
   parent lookup, so retries, follow-ups and stages of one ticket share its box
   and sibling tickets never do.

Resolution failures are loud and name the next step: a failed
`multica issue get`, an identifier that is not a valid scope key, a detached HEAD
with no task-context marker, or a checkout whose branch and marker name no scope
at all. There is no silent default and no parent lookup. The result is not
cached; the branch and marker are cheap to read, and the issue lookup is one
`multica issue get`.

## Failure modes

| Situation | Default | `MSBX_RUN_SOFT=1` |
| --- | --- | --- |
| No manifest | pass through | pass through |
| `--probe` / `--list-models` | pass through | pass through |
| Scope cannot be resolved | exit 1, harness not started | warning, harness runs without the envelope |
| `msbx up` fails | exit 1, harness not started | warning, harness runs without the envelope |
| `msbx env export` fails | exit 1, harness not started | warning, harness runs without the envelope |
| Harness cannot be launched | exit 1 | exit 1 |

`MSBX_RUN_SOFT=1` exists so a broken sandbox degrades to the previous behaviour
instead of blocking the task. It is an explicit opt-in, not a default.

## Profile wiring

Build and install both binaries:

```sh
cargo install --path .
command -v msbx msbx-run
```

In the Multica runtime profile, set the harness command (`command_name`) to
`msbx-run` and set `MSBX_HARNESS` to the absolute path of the harness the
profile launched before. Keep the daemon-injected `MULTICA_CLI_PATH` — it is
what the resolver calls. The wrapper is quiet on success; a failure is written
to stderr with the scope or the command that failed.

Rollback is the same edit in reverse:

1. Set the profile's `command_name` back to the harness path.
2. Remove `MSBX_HARNESS` from the profile environment.

Boxes stay up through a rollback; the wrapper owns no Multica state. Stop or
destroy them with `msbx down --scope S` or `msbx rm --scope S --yes`.

## Limits

- `--probe` and `--list-models` intentionally skip scope resolution. They prove
  the harness starts, not that the task's box is healthy.
- The wrapper does not install the resolver, the `msbx dns` unit or the store.
  `msbx init` and the project's own store remain separate steps.
- `msbx-run` is not a security boundary: in this stage the harness runs on the
  host with the host environment plus the exported values, the same trust domain
  as before. Stage 3's container is the isolation boundary.
- A repository without `.multica/compose.yaml` — this engine repository included
  — is unaffected. The wrapper becomes visible only when a project declares a
  manifest.
- The task-context marker is daemon-owned and read-only. A runtime that does not
  write one needs `MSBX_SCOPE` (or an `agent/*/mir-<n>-*` checkout branch); the
  wrapper fails loudly rather than guessing a scope.
