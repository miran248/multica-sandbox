# multica-sandbox

One sandbox namespace per epic, on the machine that runs the agent, outside
Multica. `multica-sandbox` gives every epic its own containers, its own service
state and its own host names, so two epics can run Postgres on `5432` at the
same time and both stay reachable from the host.

```
pg.signals-mir42.msbx.test     →  signals-mir42-pg     :5432
web.signals-mir42.msbx.test    →  signals-mir42-web    :3000
pg.hours-mir42.msbx.test       →  hours-mir42-pg       :5432
```

The container name is the reverse of the name it answers to. The dotted name is
a container alias; the flat name is the container. A role keeps its native port:
a name is never a remapped port.

## Status

**Pre-spike.** This repository exists to answer one question with evidence: does
Apple's `container` give a host-reachable address and a `msbx.test` name to
every container without publishing ports? The [stage-1 spike][spike] decides it;
the spec follows the measurement. Only `doctor` and `spike` are implemented.

[spike]: https://github.com/miran248/multica-sandbox/issues

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
```

The spike needs macOS 26 and `container system start`; it prints the one
privileged command it does not run.

## Knowledge

- [docs/README.md](docs/README.md) — index
- [GLOSSARY.md](GLOSSARY.md) — vocabulary
- [docs/adr/](docs/adr/) — decisions
- [docs/specs/](docs/specs/) — approved specs
