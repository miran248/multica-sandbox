# The harness image

One image, built locally from this directory, pinned by digest. It serves all
five seats (`general`, `planner`, `implementer`, `reviewer`, `release`) and
carries Pi, the pinned Pi packages, the baseline agent dir, the Linux Multica
CLI and the workspace's language toolchains. No registry, no per-project image,
no pull, and nothing is installed at run time.

```
images/
  Dockerfile        the one build; every version arrives as a build arg
  manifest.json     the only version source (digests, versions, checksums)
  entrypoint.sh     daemon forwarder, gh auth setup-git, exec pi
  agent/            copied to /opt/msbx/agent in the image
    agent/          Pi's global config dir (~/.pi/agent)
    roles/<role>/   overlay copied over the baseline for that seat
```

## Build and pin

```sh
msbx image build     # docker buildx build --load -> msbx-pi:<pi>-<n>
msbx image pin       # resolve the tag, write $XDG_STATE_HOME/msbx/image.json
msbx image show      # print the pin and every recorded version
```

`msbx image build` reads `manifest.json`, validates that every image and
download is pinned, passes every version to the Dockerfile as a build arg, and
runs `docker buildx build --load`. The Dockerfile asserts each version and
labels the image with it. A build records its candidate tag and digest in
`$XDG_STATE_HOME/msbx/image-build.json`; `pin` promotes it only when the tag
still resolves to the same digest.

A tag is never reused: the build counter counts local tags, the last build and
the current pin. A tag that resolves to a different digest than the build
recorded is refused, and so is repinning a tag a pin already names.
`msbx-run` executes `tag@sha256:…`, so a moved tag cannot change a run.

The manifest pins:

- the Debian `bookworm-slim` base and the Node, Rust and Go images by digest;
- the exact Debian package versions (`ca-certificates`, git, gh, jq, ripgrep,
  socat, build-essential, python3, libstdc++6 and the build-only curl);
- Pi's standalone release, the Linux Multica CLI and every Pi package with a
  sha256 checksum for each architecture.

Updating a version means editing `manifest.json`: the Dockerfile has no
versions of its own. The build is reproducible per manifest; when the Debian
mirror moves a pinned package the build fails loudly and the manifest is
updated deliberately.

The Debian base and the three toolchain images are `linux/amd64` and
`linux/arm64` manifests, and the downloads are chosen from `TARGETARCH`. Build
on the architecture the runs use; the default is the host's.

## What the image carries

| Path | Contents |
| --- | --- |
| `/opt/msbx/pi` | Pi standalone, the package assets `PI_PACKAGE_DIR` names, read-only |
| `/opt/msbx/pi-packages` | the pinned Pi packages and their dependencies, read-only, loaded by path |
| `/opt/msbx/agent` | the baseline agent dir and the role overlays |
| `/usr/local/bin/pi` | symlink to the pinned Pi |
| `/usr/local/bin/multica` | the Linux Multica CLI at the workspace's version |
| `/usr/local/bin/msbx-entrypoint` | the entrypoint |

Pi's package manager never writes to `/opt/msbx/pi-packages`; the baseline
loads the package by absolute path, so a run cannot update it. `PI_PACKAGE_DIR`
names Pi's own installation (Pi uses it to find its themes and docs); the Pi
packages live beside it in the read-only `/opt/msbx/pi-packages`. Both are
root-owned and mode-immutable to the run.

The entrypoint:

1. starts a `socat` forwarder from `127.0.0.1:$MULTICA_DAEMON_PORT` to
   `host.docker.internal:$MULTICA_DAEMON_PORT` when the daemon set the port, so
   the in-image CLI's hard-coded loopback reaches the daemon;
2. runs `gh auth setup-git` when `GH_TOKEN` is present, so HTTPS remotes push;
3. `exec`s Pi with the daemon's argv verbatim.

## Agent dir: seeded once, reset manually

The shim seeds `$XDG_STATE_HOME/msbx/agents/<agent-id>/pi` from
`/opt/msbx/agent/` and then overlays `/opt/msbx/agent/roles/<agent-name>/`
(agent name from `MULTICA_AGENT_NAME`, sanitized), and writes `.msbx-seed` with
the image digest and the agent name. It seeds only when the dir is absent, so a
second run finds it and copies nothing.

A baseline change is a **manual reset**:

```sh
rm -rf "$XDG_STATE_HOME/msbx/agents/<agent-id>/pi"
```

The next run seeds the new baseline and overlay. Sessions do not live in the
agent dir; the daemon passes `--session` in its own store, mounted at the
identical path.

The baseline carries Pi's settings (`defaultProjectTrust: always`,
`quietStartup`, telemetry off, the pinned package path) and a `handoff` prompt
template. It carries no model id: the daemon passes `--model`, and a model
change is a catalog or agent edit, not an image rebuild. Credentials arrive
through the harness store (`$XDG_CONFIG_HOME/msbx/harness/env`, mode 0600):
`DEEPSEEK_API_KEY`, `GH_TOKEN` and the git identity. The image carries none.

## What the image does not do

- No registry push or pull; it is built and loaded locally.
- No per-project image and no toolbox catalog (terraform, kubectl, wrangler,
  hcloud, scw are absent).
- No runtime install: every package and toolchain version is in the image
  manifest, and Pi's version check is off.
- No host mount except the four the shim adds (workdir, config root, session
  dir, agent dir). There is no `$HOME`, `~/.ssh`, `~/.config` or Docker socket
  beyond them.
