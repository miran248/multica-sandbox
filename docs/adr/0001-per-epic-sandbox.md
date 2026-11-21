# 0001 — One sandbox namespace per epic on Docker Compose

Status: accepted. Revision 2 (2026-11-21) replaced the Apple `container`
substrate of revision 1 with Docker Compose after the stage-1 init run showed
the Apple driver needs macOS 26, which this Mac does not run and Linux cannot
run at all. Rejected alternatives are recorded under "Substrate revision".

## Context

Multica runs every task on the host, with the host's filesystem, environment
and network. Environment variables are per agent, not per project, so an agent
working across projects has to be told where each project's values live. Two
epics cannot run the same service stack at once: a second Postgres on `5432`
collides, and the workaround — a hand-maintained dnsmasq zone, a static loopback
IP per project and a Caddy sidecar — needs a human for every new epic.

## Decision

**One namespace of containers per epic, on the host, outside Multica.** The tool
is `msbx`, the Rust binary built from this repository.

1. **The harness stays on the host.** `dsh`, model calls and the Multica task
   boundary are not moved into the box. The box is a set of services the host
   reaches by name and port.
2. **The unit is the epic**, not the task and not the issue: separate Compose
   project, separate names, separate lifecycle, so epics run in parallel.
3. **The substrate is Docker Engine + Compose v2.** Docker Desktop is the macOS
   reference, Docker Engine the Linux one. The project's stack is
   `<repo>/.multica/compose.yaml` plus one `x-msbx` extension block.
4. **Names stay, host ports move.** `<role>.<epic>.<project>.msbx.test` resolves
   to `127.0.0.1` from the host and is a network alias inside the epic. Each
   epic owns a block of 16 loopback ports; the port is in the env
   (`MSBX_<ROLE>_PORT`). No ingress, no alias helper, no canonical host port.
5. **`msbx.test` is the only zone**, served by the `msbx dns` resolver on
   `127.0.0.1:5335` and installed through `/etc/resolver/msbx.test` (macOS) or
   a `systemd-resolved` drop-in (Linux). dnsmasq and the `hours.test` /
   `signals.test` / `hermes.test` zones retire with it.
6. **Flat container names, dotted aliases.** `<project>-<epic>-<role>`; the
   dotted alias is used verbatim on the network and as the host name.
7. **Env and secrets are rendered, not referenced.** The store lives outside
   every checkout; the manifest lists names; `msbx` renders per-role env files
   and mounts secret files at `/run/msbx/secrets/<name>` (0600). The harness
   gets the same values through the wrapper. Values never enter the checkout,
   the manifest or `msbx` output; `docker inspect` remains a stated exposure.
   The store is `file` today and `age` behind the same paths later.
8. **The workdir is bind-mounted rw for v1**, with `~/projects` off limits and
   `up` refusing a workdir outside `$HOME`.
9. **Lifecycle is lazy**: first `up` creates, `down` stops, `rm` destroys with
   volumes, `gc` sweeps epics idle for 14 days. No background daemon in v1.
10. **Portability is definitions plus exported volumes.** No host-bound state.
11. **Remote services are in-epic tunnel sidecars**, addressed like a local
    role. Nothing is forwarded on the host.
12. **The epic key comes from the task context**: `MSBX_EPIC` when set, else the
    checkout's `agent/*/mir-<n>-*` branch → the issue → its parent epic.

## Substrate revision

Revision 1 chose Apple's `container` CLI for per-container VM addresses and
embedded DNS. It failed on reach, not on merit:

- Apple `container` needs macOS 26 and Apple silicon, and has no Linux path.
  This host runs macOS 15.8.1; the stage-1 init run could never record C1.
- At the time of writing it also lacks Compose support and restart policy and
  runs only a minority of real Compose files unmodified.
- Docker Sandboxes (`sbx`) was rejected: one microVM per *agent*, not a service
  host, and the agent would have to move inside for the feature to pay off.
- Podman was rejected as the v1 driver: it is a compatibility target and should
  run the same Compose files, but its macOS networking lives in a separate VM
  and its Compose provider is not the reference. Nothing in the contract
  depends on Docker-only behaviour, so podman can follow.
- Plain Docker Compose needs host ports published; the accepted trade is a
  stable assigned port in the env while the name stays the same everywhere.

## Consequences

- A service is reached by a name plus the port from its env, not by a canonical
  port. Code that hard-codes `5432` on the host is wrong by construction and
  says so at the first connection.
- The box protects against port collisions and service drift, not against model
  intent: the agent's own host shell remains trusted. This is a convention, not
  a boundary.
- Docker Desktop's VM is shared by every epic; isolation is by Compose project,
  network and port block, not by hypervisor.
- The spec is written before the init run in revision 2: Docker's behaviour is
  documented, so `msbx init` records evidence instead of deciding the driver.
