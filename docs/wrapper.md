# The harness wrapper

Status: delivered by [MIR-261](https://multica.ai/miran248/issues/MIR-261),
stage 5 of [MIR-178](https://multica.ai/miran248/issues/MIR-178).

`msbx-run` is the command the Multica runtime profile launches instead of the
harness. It still runs the harness on the host: it only decides which epic the
task belongs to, starts that epic's box, exports the rendered envelope and then
replaces itself with the real harness, keeping its arguments, stdin, stdout,
stderr and exit status.

```
profile command  →  msbx-run  →  msbx up  →  msbx env export  →  exec dsh "$@"
```

## The launch

1. **Harness** — `MSBX_HARNESS`, else `dsh` on `PATH`. A harness that resolves to
   `msbx-run` itself is refused, so a misconfigured profile cannot recurse.
2. **Probe launches** — an argument equal to `--probe` or `--list-models` is a
   harness question, not a task. The wrapper resolves no epic, starts no box and
   never calls Docker; it execs the harness immediately.
3. **No manifest** — a checkout without `.multica/compose.yaml` is passed
   through untouched.
4. **Manifest checkout** — the wrapper resolves the epic, reads
   `x-msbx.project` from the manifest, runs `msbx up --epic E --project P` and
   then `msbx env export --epic E --project P`. The exported names are added to
   the harness's environment and the harness is exec'd. `up`'s summary is
   captured, never written to the harness stdout.

`msbx` itself is `MSBX_BIN`, else the `msbx` binary next to `msbx-run`, else
`msbx` on `PATH`. The Multica CLI is `MULTICA_CLI_PATH` (the daemon injects it
for a task), else `multica` on `PATH`.

`msbx env export` prints shell-safe `export NAME='value'` lines. The wrapper
parses exactly those lines and applies the pairs to the harness process; it does
not hand a store value to a shell and it prints no value. The envelope files
remain the source of truth, so a probe and a task see the same contract.

## Epic resolution

The epic is, in order:

1. `MSBX_EPIC` when set. It is normalized (`MIR-42` → `mir-42`) and refused with
   a loud error when it is not a valid key.
2. The checkout branch, when it matches `agent/<role>/mir-<n>[-<slug>]`. The
   branch names the *issue*; the wrapper asks the Multica CLI for issue
   `mir-<n>`, then for its parent. The parent's identifier is the epic key
   (`MIR-178` → `mir-178`). A task on the epic itself has no parent, so the
   issue's own identifier is the epic.
3. The daemon's task-context marker, `.multica/daemon_task_context.json`, beside
   or above the checkout. It carries the issue id when the checkout is on the
   task's own `agent/<role>/<task-slot>` branch — the branch the daemon creates
   — rather than a branch named after the issue. The same parent lookup turns
   the issue id into the epic key.

The resolved result is cached per task at
`$XDG_STATE_HOME/msbx/.wrapper/<task-id>.epic` (`MULTICA_TASK_ID`; the branch
name stands in when the task id is absent). The cache is best effort: a
read-only state directory costs one Multica lookup, never a failed launch. The
cache is only written for a branch- or task-context-derived resolution, never
for `MSBX_EPIC`.

Resolution failures are loud and name the next step — a failed
`multica issue get`, an identifier that is not a valid epic key, a detached HEAD
with no task-context marker, or a checkout whose branch and marker name no issue
at all. There is no silent default epic.

## Failure modes

| Situation | Default | `MSBX_RUN_SOFT=1` |
| --- | --- | --- |
| No manifest | pass through | pass through |
| `--probe` / `--list-models` | pass through | pass through |
| Epic cannot be resolved | exit 1, harness not started | warning, harness runs without the envelope |
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
to stderr with the epic or the command that failed.

Rollback is the same edit in reverse:

1. Set the profile's `command_name` back to the harness path.
2. Remove `MSBX_HARNESS` from the profile environment.

Boxes stay up through a rollback; the wrapper owns no Multica state. Stop or
destroy them with `msbx down --epic E` or `msbx rm --epic E --yes`.

## Limits

- `--probe` and `--list-models` intentionally skip epic resolution. They prove
  the harness starts, not that the task's box is healthy.
- The wrapper does not install the resolver, the `msbx dns` unit or the store.
  `msbx init` and the project's own store remain separate steps.
- `msbx-run` is not a security boundary: the harness runs on the host with the
  host environment plus the exported values, the same trust domain as before.
- A repository without `.multica/compose.yaml` — this engine repository included
  — is unaffected. The wrapper becomes visible only when a project declares a
  manifest.
- The task-context marker is daemon-owned and read-only. A runtime that does not
  write one needs `MSBX_EPIC` (or an `agent/*/mir-<n>-*` checkout branch); the
  wrapper fails loudly rather than guessing an epic.
