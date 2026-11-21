# Glossary

Terms this repository uses in one specific sense. The vocabulary changed with
[spec 0002](docs/specs/0002-container-harness.md): the box's unit is the
**scope**, not the epic, and the harness runs in a per-run **container**.

- **Scope** — the unit a box belongs to: a ticket (`mir-42`) or a chat session
  (`chat-1a2b3c4d`). Replaces *epic*. Two sibling tickets are two scopes.
- **Ticket box**, **chat box** — the Compose project, containers, volumes,
  loopback ports and host names that belong to one scope. State only: services,
  never code. A namespace, not a virtual machine.
- **Harness container** — the per-run container that runs Pi with the run's
  workdir, config root, session dir and agent dir mounted at their identical
  host paths. It is removed when the run ends and runs nothing long-lived.
- **Lease** — one run's claim on its box, recorded under
  `$XDG_STATE_HOME/msbx/leases/`. The last lease to leave stops the box; its
  volumes stay. A stale lease is one whose host process is gone.
- **Agent dir** — the persistent per-agent Pi home
  (`$XDG_STATE_HOME/msbx/agents/<agent-id>/pi`, mounted at `/home/agent/.pi`),
  seeded once from the image baseline plus a role overlay. Never the host's
  `~/.pi`.
- **Image** — the one locally built, digest-pinned harness image: Pi, the
  pinned packages, the baseline agent dir, git, gh, the Linux Multica CLI, jq,
  ripgrep, socat and the workspace's language toolchains.
- **Namespace** — the flat prefix shared by a scope's container and Compose
  project names (`signals-mir42-*`). It keeps scopes from colliding.
- **Role** — what a service does in a scope (`pg`, `pg-analytics`, `web`). A
  second instance is a second role, never a second container port.
- **Alias** — the dotted host name a role answers to
  (`pg.mir42.signals.msbx.test`). Aliases, not container names, carry the
  hierarchy; the same string is the host name, the in-network alias and, for
  the harness, the service reached on the scope's Compose network.
- **Zone** — `msbx.test`, the only local DNS domain the tool maintains, served
  by the `msbx dns` resolver on `127.0.0.1:5335`.
- **Block** — the 16 consecutive loopback ports a scope owns. `publish` order
  maps roles onto it; `gc` releases it.
- **Manifest** — `<repo>/.multica/compose.yaml`: a standard Compose file plus
  the `x-msbx` block. `docker compose` reads it unchanged.
- **Host store**, **project store**, **harness store** — the three value
  layers outside every checkout. The host store
  (`$XDG_CONFIG_HOME/msbx/host.env`) sits below the project store
  (`$XDG_CONFIG_HOME/msbx/projects/<project>/`); a project may take a host name
  only when `host-allow` lists it. The harness store
  (`$XDG_CONFIG_HOME/msbx/harness/env`) carries the model credential and the
  GitHub token and never enters a service envelope.
- **Envelope** — the rendered, per-scope material under
  `$XDG_STATE_HOME/msbx/<project>/<scope>/`, mode 0600: `env/<role>.env`,
  `env/harness.env` (what the harness container receives), `secrets/<NAME>` and
  `override.yaml`. Materialized, never mounted through the checkout.
- **Token** — a `{{host:<role>}}`, `{{port:<role>}}`, `{{scope}}` or
  `{{project}}` placeholder in a store value. `msbx` substitutes it per scope
  at render time; an unknown token is an error. `{{epic}}` is refused and names
  `{{scope}}`.
- **Override** — the generated Compose file that carries container names,
  aliases, loopback ports, env files and tunnel sidecars. Regenerated on every
  `up`; the only place those decisions live.
- **Driver** — the backend that creates and addresses containers. Docker
  Compose is the v1 driver; no other driver is implemented.
- **Init** — `msbx init`: the idempotent first-run setup on a fresh host. It
  installs the `msbx.test` resolver and the `msbx dns` unit, then runs the
  C1–C8 checks ([MIR-178]). `--check` measures without changing the host.
- **State** — `~/.local/state/msbx/state.json`: the port blocks, scopes and
  lease bookkeeping `msbx` owns on this host. Written atomically under a lock
  and outside every checkout; `gc` releases idle blocks from it.

[MIR-178]: https://multica.ai/miran248/issues/MIR-178
