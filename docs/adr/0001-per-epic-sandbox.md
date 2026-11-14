# 0001 — One sandbox namespace per epic on Apple containers

Status: accepted for v1. The driver choice is confirmed or replaced by the
stage-1 spike (MIR-179); everything else here does not depend on that result.

## Context

Multica runs every task on the host, with the host's filesystem, environment
and network. Environment variables are per agent, not per project, so an agent
working across projects has to be told where each project's tokens live. Two
epics cannot run the same service stack at once: a second Postgres on `5432`
collides, and the current workaround — a hand-maintained dnsmasq zone, a static
loopback IP per project and a Caddy sidecar — needs a human for every new epic.

## Decision

**One namespace of containers per epic, on the host, outside Multica.** The
tool is `multica-sandbox`, a Rust binary in this repository.

1. **The harness stays on the host.** `dsh`, model calls and the Multica task
   boundary are not moved into the box. The box is a tool target, reached by
   `multica-sandbox exec <epic> -- <cmd>`, with a host-side `env --` escape
   hatch for tools that must run where `gh`, `kubectl` and `terraform` run.
2. **The unit is the epic**, not the task and not the issue: separate state,
   separate names and separate lifecycle, so epics run in parallel.
3. **The substrate is Apple's `container` CLI.** One lightweight VM per
   container gives every container a host-reachable address, so two epics can
   each answer on `5432` without port publishing. `container machine` is ruled
   out: it has no `--network`, `--dns`, `-p` or alias support, so it cannot
   carry a name. Docker Compose is ruled out: one shared VM needs the loopback
   alias pool this design retires.
4. **Names carry the hierarchy.** `<role>.<epic>.<project>.msbx.test`, one exact
   alias per role. Container names are flat and reversed:
   `signals-mir42-pg`. No L4 or L7 router in v1.
5. **`msbx.test` is the only zone**, owned by Apple's embedded DNS through one
   `sudo container system dns create msbx.test`. dnsmasq, `/etc/resolver/test`
   and the `hours.test` / `signals.test` / `hermes.test` zones retire with it.
6. **Secrets are materialized, not referenced.** A project's manifest declares
   names; the host resolves values into `/run/msbx/env` and
   `/run/msbx/secrets/<name>` (0600); every exec sources them. Values never
   enter the agent process. The store is `file` today and `age` behind the same
   schema later.
7. **The home mount is rw for v1**, with `~/projects` off limits and `up`
   refusing a workdir outside `$HOME`. Materialized secrets mean a later
   read-only or no-mount driver is a flag, not a rewrite.
8. **Lifecycle is lazy**: first `up` creates, idle-stop after 2h, volumes
   retained 14 days, `down` stops, `rm` destroys, `gc` sweeps.
9. **Portability is definitions plus exported volumes.** No host-bound state.

## Consequences

- The box protects against side effects (builds, tests, services), not against
  model intent: the agent's own host shell remains trusted. This is stated as a
  convention, not a boundary.
- `container machine` remains unavailable until it can express names; if Apple
  adds network control to machines, the driver decision is revisited.
- The spec is written after the spike, so the reproduction of its assertions is
  part of the record rather than an assumption.
