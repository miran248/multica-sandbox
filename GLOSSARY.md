# Glossary

Terms this repository uses in one specific sense.

- **Epic box** — the Compose project, containers, volumes, loopback ports and
  host names that belong to one epic. A namespace, not a virtual machine.
- **Namespace** — the flat prefix shared by an epic's container and Compose
  project names (`signals-mir42-*`). It keeps epics from colliding.
- **Role** — what a service does in an epic (`pg`, `pg-analytics`, `web`). A
  second instance is a second role, never a second container port.
- **Alias** — the dotted host name a role answers to
  (`pg.mir42.signals.msbx.test`). Aliases, not container names, carry the
  hierarchy; the same string is the host name and the in-network alias.
- **Zone** — `msbx.test`, the only local DNS domain the tool maintains, served
  by the `msbx dns` resolver on `127.0.0.1:5335`.
- **Block** — the 16 consecutive loopback ports an epic owns. `publish` order
  maps roles onto it; `gc` releases it.
- **Manifest** — `<repo>/.multica/compose.yaml`: a standard Compose file plus
  the `x-msbx` block. `docker compose` reads it unchanged.
- **Store** — the per-project values outside every checkout
  (`$XDG_CONFIG_HOME/msbx/projects/<project>/`, default
  `~/.config/msbx/projects/<project>/`): `env` (`NAME=value` lines) and
  `secrets/<NAME>` (one value per file). The manifest lists names, never values.
- **Envelope** — the rendered, per-epic material under
  `$XDG_STATE_HOME/msbx/<project>/<epic>/`, mode 0600: `env/<role>.env`,
  `env/harness.env` (the harness envelope) and `secrets/<NAME>`. Materialized,
  never mounted through the checkout.
- **Token** — a `{{host:<role>}}`, `{{port:<role>}}`, `{{epic}}` or `{{project}}`
  placeholder in a store value. `msbx` substitutes it per epic at render time;
  an unknown token is an error.
- **Override** — the generated Compose file that carries container names,
  aliases, loopback ports, env files and tunnel sidecars. Regenerated on every
  `up`; the only place those decisions live.
- **Driver** — the backend that creates and addresses containers. Docker
  Compose is the v1 driver; no other driver is implemented.
- **Init** — `msbx init`: the idempotent first-run setup on a fresh host. It
  installs the `msbx.test` resolver and the `msbx dns` unit, then runs the
  C1–C7 checks ([MIR-178]). `--check` measures without changing the host.
- **State** — `~/.local/state/msbx/state.json`: the port blocks and epic
  bookkeeping `msbx` owns on this host. Written atomically under a lock and
  outside every checkout; `gc` releases idle blocks from it.

[MIR-178]: https://multica.ai/miran248/issues/MIR-178
