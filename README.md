# multica-sandbox

One sandbox namespace per epic, on the machine that runs the agent, outside
Multica. The `msbx` binary gives every epic its own containers, its own service
state and its own host names, so two epics can run Postgres on `5432` at the
same time and both stay reachable from the host.

```
pg.mir42.signals.msbx.test     →  signals-mir42-pg     :5432
web.mir42.signals.msbx.test    →  signals-mir42-web    :3000
pg.mir42.hours.msbx.test       →  hours-mir42-pg       :5432
```

`<role>.<epic>.<project>.msbx.test` is the name; `<project>-<epic>-<role>` is
the container. The container name is the name in reverse, and the dotted name
is a container alias. A role keeps its native port: a name is never a remapped
port.

## Status

**Pre-spec.** This repository exists to answer one question with evidence: does
Apple's `container` give a host-reachable address and a `msbx.test` name to
every container without publishing ports? The [stage-1 init run][init] decides
it; the spec follows the measurement. Only `doctor` and `init` are implemented.

[init]: https://multica.ai/miran248/issues/MIR-179

## What it will manage

- **One namespace per epic** — `<project>-<epic>-<role>` containers, created on
  the first `up`, idle-stopped, destroyed on `rm`.
- **Names, not ports** — `<role>.<epic>.<project>.msbx.test`, served by Apple's
  embedded DNS. No dnsmasq, no loopback alias pool, no Caddy per project.
- **Env and secrets by name** — the project's `.multica/sandbox.toml` declares
  names; the host resolves values into `/run/msbx/env` (0600) and
  `/run/msbx/secrets/<name>` (0600), and every exec sources them. Values never
  enter the agent's process.
- **Portable definitions** — the engine is here; each project repository owns
  its own manifest and volumes.

## Build

```sh
cargo build
cargo test
cargo run -- doctor
cargo run -- init
```

`msbx init` is the first command on a fresh host, and it is idempotent: it
starts the `container` system (installing the kernel on first run), registers
the `msbx.test` resolver with `sudo container system dns create msbx.test`,
then runs C1–C7 and exits non-zero when a check fails or is skipped. `--check`
performs no setup and only measures; `--keep` leaves the check containers in
place. It needs macOS 26 and a terminal for the sudo prompt.
