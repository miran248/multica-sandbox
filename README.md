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

**Stage 2 delivered.** The Docker driver foundation is in place: `doctor`,
`init`, the `msbx dns` resolver and its macOS/Linux installation, the `x-msbx`
manifest reader and the per-epic port-block allocator with its state file. The
Compose lifecycle (`up`, `down`, `rm`, `status`, `env`, tunnels) lands in the
following stages of [MIR-178](https://multica.ai/miran248/issues/MIR-178).

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

## Commands

```sh
msbx doctor                   # describe the Docker host; changes nothing
msbx init [--check] [--keep]  # install the resolver, run C1-C7
msbx dns [--listen ADDR]      # the msbx.test resolver (127.0.0.1:5335)
msbx gc [--dry-run]           # release port blocks idle for 14 days
```

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

`msbx init` is idempotent and needs Docker running; the resolver install is the
one `sudo` prompt. `--check` installs nothing while still running the C1–C7
container checks, and `--keep` leaves those containers in place for
inspection.
