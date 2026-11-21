# 0002 — The harness runs in a per-run container; the box is per scope

Status: accepted when the spec pull request for
[spec 0002](../specs/0002-container-harness.md) merges into `spec/mir-270`.
Epic: [MIR-270](https://multica.ai/miran248/issues/MIR-270). Supersedes
decisions 1, 2 and 12 of [ADR 0001](0001-per-epic-sandbox.md); its substrate,
naming, port, env, tunnel and lifecycle decisions stand.

## Context

ADR 0001 put the sandbox around services and left the harness on the host. That
was the smaller change, and it isolates ports and service state. It does not
isolate the agent: the harness loads the host's `~/.agents`, `~/.claude`,
`~/.codex` and `~/.pi`, so a prompt carries skills and rules nobody bound; the
harness itself is host infrastructure (DSH plus a 310 MB Node/Python/pnpm
runtime and a per-machine wrapper), so a harness change is a host change and
every project shares one runtime. Every checkout and every project's values
live under one user, and project separation is a convention.

The box's unit is also wrong for the way the Factory promotes work. An epic's
sibling tickets are promoted in one stage, so they run at the same time and
share one box: one database, one `rm`, one port block. Retries, follow-ups and
stage moves of the same ticket should share state; sibling tickets should not.

The product now supports the pieces the move needs: a custom runtime profile
whose command is run by the daemon, a Pi compatibility target, an in-image
Linux CLI, and a per-task context marker. A per-run container can therefore be
introduced without a daemon change.

## Decision

**The harness runs in a per-run container, and the box is keyed by scope.**
The runtime profile's command becomes `msbx-run`, the shim. The shim resolves
the scope, starts the scope's state box, runs Pi in one container from a pinned
image with the run's mounts and env, and stops the box when the last lease
leaves. All five seats use it; no harness runs on the host; Docker becomes a
hard dependency for chat as well as tickets. The spec is
[0002-container-harness](../specs/0002-container-harness.md); the decisions
below are the durable record.

1. **The harness container is per run.** One container per daemon task, with
   the task workdir, the task config root, the provider session dir and the
   agent dir mounted at their identical absolute paths. Nothing else: no
   `$HOME`, no SSH key, no Docker socket, no ambient checkout, no inherited
   host environment beyond `MULTICA_*` and the store layers. Path identity is
   what makes resume and the in-container CLI work.
2. **The unit is the scope.** A scope is a ticket (`mir-42`) or a chat session
   (`chat-<8>`), resolved from `--scope`/`MSBX_SCOPE`, the daemon task context,
   or the worktree branch. The parent lookup is deleted. Retries, follow-ups
   and stages of one ticket share a box; parallel sibling tickets do not.
3. **Code is run-scoped, state is scope-scoped.** The box is deps-only:
   services, volumes, ports, names. `up` never bind-mounts the checkout root,
   refuses a volume whose source is that root, refuses a `build:` inside the
   checkout, and runs the app process in the harness container instead.
4. **The agent dir is persistent.** Pi's `~/.pi` is backed by a per-agent
   directory seeded once from the image baseline plus a role overlay. It is
   persistent across runs and scopes and is never the host's real `~/.pi`. The
   repository's checked-in `.pi/` loads from the mounted worktree with project
   trust `always`; `AGENTS.md` loads as today.
5. **Lifecycle is lease-driven.** `up` records a lease per run; the last lease
   to leave stops the box and keeps its volumes. `rm` happens when the ticket
   is final (`status_category` done/closed, archived, or gone), at `up` for its
   own scope and in `gc`; a 14-day idle backstop covers a ticket that never
   gets there, and a chat box is removed after two days. No background daemon;
   `gc` sweeps stale leases and orphaned harness containers too.
6. **Env is layered and allowlisted.** A host store sits below the project
   store; the project value wins. A project may use a host name only when
   `host-allow` lists it, because the manifest is repo-controlled. A
   harness-only store carries the model credential and the GitHub token and
   never enters a service envelope. `MULTICA_*`, `PI_*`, `HOME`, `PATH` and
   `HOSTNAME` are reserved.
7. **Pi replaces DSH.** `pi-subagents`, pinned in the image, closes the
   subagent and background-workflow gap; Multica treats Pi as a first-class
   target (`--runtime-type pi`). DSH's profile and host install are removed
   after a proven cutover, leaving one online runtime.
8. **One image, built locally, pinned by digest.** Pi, the pinned packages, the
   baseline agent dir, git, gh, the Linux CLI, jq, ripgrep, socat and the
   workspace's language toolchains; no registry, no per-project images, no
   toolbox catalog. The shim names it by digest; a moved tag is refused.
9. **Probes are throwaway containers.** `--version` answers from the pin;
   `--probe`, `--list-models` and the RPC discovery args run from the pinned
   digest with no scope, box, mounts or lease, cached by digest.
10. **Registration is one custom profile.** `--runtime-type pi`, command
    `msbx-run`, no fixed args. The catalog's selector stops hard-coding `dsh`,
    asserts the provider family, requires exactly one online runtime, and names
    the model by the id the image lists — not a literal model string.
11. **The cutover is one human step.** Build and pin, register, bind the five
    agents, prove one real ticket, remove DSH, merge `spec/mir-270` into
    `main`. The old image digest and DSH archive are kept until the first green
    release and are the rollback.

## Rejected alternatives

- **OMP.** Its only edge over Pi was bundling, and it imports every other
  agent's config on first run, which is the ambient-prompt problem the epic is
  fixing. It also splits the runtime family the daemon already treats as one.
- **Per-epic boxes.** The box must follow the ticket, not the epic: sibling
  tickets promoted together would otherwise share a database and a `rm`, which
  is the collision this ADR removes. A per-epic box cannot be scoped by a lease
  per run without becoming a per-ticket box.
- **A host-side harness.** Keeping DSH or Pi on the host keeps the ambient
  config, the host runtime and the shared project values. A wrapper can filter
  env and skills, but it cannot remove the host filesystem, the host agent
  config or the host install, and it does not isolate projects. The container
  is the smallest unit that removes all four.
- **Per-project images.** One image per project would isolate toolchains but
  multiply build, pin and rollback by project, and it makes the harness an
  image-per-repo concern. The one image plus pinned toolchains is enough for
  v1; the toolbox catalog is deferred, not forgotten.
- **A host port published on all interfaces.** It would let the harness reach
  the box without joining its network, but it breaks the loopback-only port
  contract and exposes the services on the LAN. The container joins the scope
  network instead, and the host keeps `127.0.0.1` ports.
- **A daemon change.** The daemon already launches an arbitrary profile
  command, discovers its version and models, passes a session path and
  materialises skills. The shim absorbs the container, so the cutover needs no
  product release.

## Consequences

- Every run needs Docker, including chat. A stopped Docker fails the launch
  loudly; the shim never falls back to a host harness.
- A project's toolchain comes from the image, not the host, so a missing
  runtime is an image change. One image serves every project and every seat.
- The harness sees only its mounts and the `MULTICA_*` environment; credentials
  that used to come from the host (`~/.ssh`, `~/.config`, `~/.gitconfig`,
  agent custom env) now come from the harness store or do not exist. HTTPS with
  `GH_TOKEN` replaces SSH for pushes.
- Project trust `always` lets a repository's `.pi/` code run with the run's
  mounts and token. The container has no host home, key or socket, so the blast
  radius is the checkout, the session and the task token.
- `done` and final-state removal are only as fresh as the last `up` or `gc`;
  there is no scheduler in v1.
- The spec is approved before the implementation: the spec PR's merge approves
  the breakdown, and the release is the one step with no automated fallback.
