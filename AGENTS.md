# multica-sandbox

Routing only — agent behavior comes from the workspace agent contract.

- Knowledge index: [docs/README.md](docs/README.md)
- Vocabulary: [GLOSSARY.md](GLOSSARY.md); decisions: [docs/adr/](docs/adr/); approved specs: [docs/specs/](docs/specs/)
- Contract: [spec 0002](docs/specs/0002-container-harness.md) — one harness container per run, one state-only box per scope (ticket or chat). The vocabulary is **scope**, not epic; `{{scope}}`, not `{{epic}}`.
- Verification: `cargo test`, `cargo run -- doctor`, and `cargo run -- init` with Docker on macOS 15 or Linux; `msbx image build && msbx image pin` for the harness image.
- This repository owns the engine and the harness image. A project's stack lives in its own repository at `.multica/compose.yaml` plus the `x-msbx` block.
