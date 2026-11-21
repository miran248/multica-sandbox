# multica-sandbox

One sandbox namespace per epic, on the machine that runs the agent, outside
Multica. The `msbx` binary gives every epic its own Compose project, its own
service state, its own loopback ports and its own host names, so two epics can
run Postgres on `5432` at the same time and both stay reachable from the host.

```
pg.mir42.signals.msbx.test     →  127.0.0.1:23100   signals-mir42-pg
web.mir42.signals.msbx.test    →  127.0.0.1:23101   signals-mir42-web
pg.mir42.hours.msbx.test       →  127.0.0.1:23116   hours-mir42-pg
```

`<role>.<epic>.<project>.msbx.test` is the name; `<project>-<epic>-<role>` is
the container. The name resolves to `127.0.0.1` and is a network alias inside
the epic. Names stay, host ports move: the port comes from the epic's block and
is written into the env (`MSBX_<ROLE>_PORT`), never hard-coded on the host.

## Status

**Spec proposed.** The contract is
[docs/specs/0001-per-epic-sandbox.md](docs/specs/0001-per-epic-sandbox.md); the
decision record is [ADR 0001](docs/adr/0001-per-epic-sandbox.md). The current
`doctor` and `init` still measure the Apple `container` driver from the
superseded stage-1 spike; the Docker driver replaces them with the stage-2
delivery.

## What it will manage

- **One namespace per epic** — Compose project `<project>-<epic>`, containers
  `<project>-<epic>-<role>`, created on `up`, stopped on `down`, destroyed on
  `rm`, swept by `gc`.
- **Names, not fixed ports** — `<role>.<epic>.<project>.msbx.test` served by
  the `msbx dns` resolver; a stable loopback port per published role from the
  epic's block.
- **Env and secrets by name** — the project's `.multica/compose.yaml` `x-msbx`
  block lists names; the store holds values outside every checkout; `msbx`
  renders per-role env files, mounts `/run/msbx/secrets/<name>` (0600), and the
  wrapper exports the same values to the harness.
- **Portable definitions** — the engine is here; each project repository owns
  its own manifest and volumes.

## Build

```sh
cargo build
cargo test
cargo run -- doctor
cargo run -- init
```

`msbx init` is the first command on a fresh host and is idempotent: it installs
the `msbx.test` resolver, runs C1–C7 against a synthetic two-epic project and
prints a `PASS`/`FAIL`/`SKIP` table with the observed values. `--check` changes
nothing; `--keep` leaves the check project in place. It needs Docker running and
one sudo prompt for the resolver.
