# Index

- Vocabulary: [GLOSSARY.md](../GLOSSARY.md)
- Decisions: [docs/adr/](adr/); approved specs: [docs/specs/](specs/)
- Verification: `cargo test`; host report: `cargo run -- doctor`; first-run setup
  and checks: `cargo run -- init`; resolver: `cargo run -- dns`; image:
  `msbx image build && msbx image pin`
- The engine lives in this repository. Each project's stack is declared in its
  own repository at `.multica/compose.yaml` plus the `x-msbx` block.
- Contract: [specs/0002-container-harness.md](specs/0002-container-harness.md)
  (the per-run harness container and the per-scope box). The per-epic contract
  it supersedes is [specs/0001-per-epic-sandbox.md](specs/0001-per-epic-sandbox.md).
- Env and secrets: [env.md](env.md) — the three value layers, the render
  pipeline, the harness envelope and its stated limits.
- Harness wrapper: [wrapper.md](wrapper.md) — `msbx-run`, scope resolution, the
  container shim, probe pass-through, profile wiring and rollback.
- Remote services: [tunnels.md](tunnels.md) — the `x-msbx.tunnels` sidecar, the
  `ssh` and `cloudflared` transports, their credentials and stated limits.

The per-epic pages above record the MIR-178 delivery. [Spec
0002](specs/0002-container-harness.md) renames epic to scope, adds the host and
harness value layers, and moves the harness into a per-run container; the
delivery stages rewrite the pages that state otherwise.
