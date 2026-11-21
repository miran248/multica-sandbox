# Index

- Vocabulary: [GLOSSARY.md](../GLOSSARY.md)
- Decisions: [docs/adr/](adr/); approved specs: [docs/specs/](specs/)
- Verification: `cargo test`; environment: `cargo run -- doctor`; first-run checks: `cargo run -- init`
- The engine lives in this repository. Each project's stack is declared in its
  own repository at `.multica/compose.yaml` plus the `x-msbx` block.
- Contract: [specs/0001-per-epic-sandbox.md](specs/0001-per-epic-sandbox.md).
