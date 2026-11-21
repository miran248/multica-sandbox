# multica-sandbox

Routing only — agent behavior comes from the workspace agent contract.

- Knowledge index: [docs/README.md](docs/README.md)
- Vocabulary: [GLOSSARY.md](GLOSSARY.md); decisions: [docs/adr/](docs/adr/); approved specs: [docs/specs/](docs/specs/)
- Verification: `cargo test`, `cargo run -- doctor`, and `cargo run -- init` with Docker on macOS 15 or Linux
- This repository owns the engine. A project's stack lives in its own repository at `.multica/compose.yaml` plus the `x-msbx` block.
