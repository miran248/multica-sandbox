# Index

- Vocabulary: [GLOSSARY.md](../GLOSSARY.md)
- Decisions: [docs/adr/](adr/); approved specs: [docs/specs/](specs/)
- Verification: `cargo test`; host report: `cargo run -- doctor`; first-run setup
  and checks: `cargo run -- init`; resolver: `cargo run -- dns`
- The engine lives in this repository. Each project's stack is declared in its
  own repository at `.multica/compose.yaml` plus the `x-msbx` block.
- Contract: [specs/0001-per-epic-sandbox.md](specs/0001-per-epic-sandbox.md).
