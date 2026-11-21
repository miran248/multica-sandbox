# 0003 — release-please owns the version; cargo-dist owns the artifacts

Status: accepted when the spec pull request for
[spec 0003](../specs/0003-release.md) merges into `spec/mir-282`.
Epic: [MIR-282](https://multica.ai/miran248/issues/MIR-282).

## Context

`msbx` has no tag and no release; the documented install builds from source
with Cargo. The harness image pin and the container cutover's rollback each
need a named binary version. `multica-workspace` has no version at all, so the
catalog cannot be pinned by a released identifier. The macOS install and
upgrade path should not require a Rust toolchain, and a hand-tagged release
cannot be the cutover's version source.

The repository already uses Conventional Commits. Both the sandbox and the
workspace are GitHub repositories; the sandbox is Rust, the workspace is a
private Node catalog repository with a `version.txt`-shaped version and no
package to publish.

## Decision

**release-please owns the version, changelog, tag and draft release;
cargo-dist owns the artifacts and the Homebrew formula.** The spec is
[0003-release](../specs/0003-release.md); the durable decisions are:

1. **Two repositories, one chain.** release-please runs on `main` in
   `miran248/multica-sandbox` (`rust`) and `miran248/multica-workspace`
   (`simple` with `version.txt` and `initial-version: 0.1.0`). Both tag
   `vX.Y.Z` with `include-component-in-tag: false`. No other process writes a
   version or a tag.
2. **The tag is data, not a command.** A merged release PR is the release act.
   The first release is `v0.1.0`; the sandbox manifest starts empty so rust's
   initial version applies, and the workspace supplies `0.1.0` because simple
   defaults to `1.0.0`. Pre-1.0, a breaking change bumps the minor
   (`bump-minor-pre-major`), so `1.0.0` is deliberate.
3. **The draft handshake is explicit.** release-please creates the tag ref and
   a draft release (`draft: true`, `force-tag-creation: true`, because GitHub
   does not tag a draft until it is published). cargo-dist runs with
   `create-release = false`: it assumes the draft exists, uploads the
   artifacts to it and undrafts it. One release is created, by one owner.
4. **cargo-dist owns the artifacts.** A pinned cargo-dist builds
   `aarch64-apple-darwin` on `macos-15` and `x86_64-apple-darwin` on
   `macos-15-intel`, publishes the archives, checksums and source tarball to
   the GitHub release, and pushes the generated `Formula/msbx.rb` to
   `miran248/homebrew-tap`. The formula is a prebuilt-binary formula that
   installs `msbx` and `msbx-run`; a tap needs no bottle for it.
5. **The token is provisioning, not design.** A PAT or a GitHub App with
   contents and pull-request write on both repositories and contents write on
   the tap. `GITHUB_TOKEN` is insufficient: a tag it creates does not start
   the cargo-dist workflow.
6. **The install is the tap.** `brew install miran248/tap/msbx`;
   `cargo install --path .` until the tap exists. `msbx init` runs after
   install and records the Homebrew `bin` symlink path, which survives
   `brew upgrade`; re-running `init` is the recovery for a moved binary.
7. **The cutover is a release, not a tag.** The container cutover is released
   through the same pipeline after the pipeline is on `main`; it is never
   hand-tagged. A release's rollback pair is the previous tag plus its image
   digest.
8. **Published tags are immutable.** A bad release is reverted and fixed by
   the next release. Tags and releases are never moved or deleted, and the tap
   formula is never hand-edited.

## Rejected alternatives

- **cargo-dist creating the release itself** (`create-release = true`). It
  would make dist a second release owner beside release-please, duplicate the
  release decision that the tag already carries, and replace the curated
  changelog with dist's generated body. The draft handshake keeps one owner
  for the release and one owner for the artifacts, and it is the documented
  integration point for an external release creator.
- **release-plz or cargo-release instead of release-please.** release-plz is
  Rust-only, so the workspace repository would need a second tool and a second
  model. cargo-release is a local command: it has no pull request, no changelog
  policy and no tag event, so it would need a workflow wrapper with the same
  token problem and less review. release-please is the GitHub-native release-PR
  model, supports both `rust` and `simple` with one manifest shape, and keeps
  the version decision reviewable in a PR.
- **A cask instead of a formula.** A cask installs an app bundle or a binary
  into an application directory; it does not put two command-line binaries on
  `PATH` for a terminal user, and cargo-dist only generates formulae. A cask
  would be a second hand-maintained artifact whose upgrade path diverges from
  the formula. A formula is the CLI package model and `brew upgrade` already
  handles it.
- **Publishing without a tap.** The core `homebrew-core` tap does not accept
  third-party prebuilt binaries, and asking a user to download a tarball and
  place it on `PATH` gives no upgrade path and no checksum policy. A tap is one
  repository, one generated formula and one token; Homebrew treats it as a
  first-class install source without registering anything.
- **A single repository for both versions.** The catalog and the engine release
  on their own commit histories and have different consumers; a shared tag
  would couple a catalog edit to a binary release. Two repositories, two tags,
  one workflow shape.
- **Linux targets now.** They would need a Linux build host and a Linux
  consumer; the sandbox runs on macOS today and the tap's Linux path is
  untested. The configuration leaves room for the target when a host exists.
- **A bottle or a source-build formula.** A source build would require a Rust
  toolchain on the installing machine, which is the problem the tap solves; a
  bottle would add a second build-and-publish pipeline for no gain over the
  prebuilt archive the formula already verifies by checksum.

## Consequences

- Every release is two steps: a release PR reviewed and merged, then a
  tag-triggered workflow. A failed workflow after the tag exists is recovered
  by re-running it, never by re-tagging.
- A release token with broad permissions is a standing credential; it lives in
  repository secrets only, is never written into a checkout or a log, and is
  rotated by secret update.
- Homebrew serves one version. Rollback is a manual older-archive install or a
  revert plus the next release, not a tap operation.
- The tap repository and the token are the human's provisioning steps; the
  release is otherwise agent-operated.
- The formula and the generated workflow are coupled to the pinned cargo-dist
  version; upgrading it is a pipeline change with a regenerated workflow.
- macOS-only releases leave Linux on `cargo install` until a Linux host and
  target are added.
