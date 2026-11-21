# syntax=docker/dockerfile:1.7
#
# The one harness image. `msbx image build` supplies every version and digest
# from images/manifest.json; nothing in this file floats. The only network
# access is at build time, and the final image installs nothing at run time.

# Every image reference is a global arg: an ARG between FROMs belongs to the
# preceding stage and cannot be used by the next FROM.
ARG NODE_IMAGE
ARG RUST_IMAGE
ARG GO_IMAGE
ARG BASE_IMAGE

FROM ${NODE_IMAGE} AS node

FROM ${RUST_IMAGE} AS rust
# The upstream Rust image ships rustc and cargo; the workspace's checks need
# rustfmt and clippy, so add those components to the same pinned toolchain.
RUN rustup component add rustfmt clippy

FROM ${GO_IMAGE} AS go

FROM ${BASE_IMAGE}

ARG TARGETARCH
ARG BASE_IMAGE
ARG NODE_VERSION
ARG PNPM_VERSION
ARG RUST_VERSION
ARG GO_VERSION
ARG PYTHON_VERSION
ARG APT_CA_CERTIFICATES
ARG APT_GIT
ARG APT_GH
ARG APT_JQ
ARG APT_RIPGREP
ARG APT_SOCAT
ARG APT_BUILD_ESSENTIAL
ARG APT_PYTHON3
ARG APT_LIBSTDCXX6
ARG APT_CURL
ARG PI_VERSION
ARG PI_PACKAGES_JSON
ARG PI_URL_AMD64
ARG PI_SHA256_AMD64
ARG PI_URL_ARM64
ARG PI_SHA256_ARM64
ARG MULTICA_VERSION
ARG MULTICA_URL_AMD64
ARG MULTICA_SHA256_AMD64
ARG MULTICA_URL_ARM64
ARG MULTICA_SHA256_ARM64

ENV LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    HOME=/home/agent \
    PATH=/usr/local/cargo/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    GOROOT=/usr/local/go \
    GOTOOLCHAIN=local \
    PI_PACKAGE_DIR=/opt/msbx/pi \
    PI_SKIP_VERSION_CHECK=1

# The Debian packages are pinned to the versions in images/manifest.json. apt
# fails the build when the mirror moves, which is the intended loud failure: a
# new base means a new manifest.
RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        "ca-certificates=${APT_CA_CERTIFICATES}" \
        "git=${APT_GIT}" \
        "gh=${APT_GH}" \
        "jq=${APT_JQ}" \
        "ripgrep=${APT_RIPGREP}" \
        "socat=${APT_SOCAT}" \
        "build-essential=${APT_BUILD_ESSENTIAL}" \
        "python3=${APT_PYTHON3}" \
        "libstdc++6=${APT_LIBSTDCXX6}" \
        "curl=${APT_CURL}"; \
    rm -rf /var/lib/apt/lists/*

# The workspace's language toolchains, each from its pinned upstream image.
COPY --from=node /usr/local/bin/ /usr/local/bin/
COPY --from=node /usr/local/lib/node_modules/ /usr/local/lib/node_modules/
COPY --from=node /usr/local/include/node/ /usr/local/include/node/
COPY --from=rust /usr/local/rustup/ /usr/local/rustup/
COPY --from=rust /usr/local/cargo/ /usr/local/cargo/
COPY --from=go /usr/local/go/ /usr/local/go/

# Pi standalone, verified against the manifest checksum for the target arch.
RUN set -eux; \
    case "$TARGETARCH" in \
        amd64) url="$PI_URL_AMD64"; sha="$PI_SHA256_AMD64" ;; \
        arm64) url="$PI_URL_ARM64"; sha="$PI_SHA256_ARM64" ;; \
        *) echo "msbx: unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "$url" -o /tmp/pi.tar.gz; \
    echo "$sha  /tmp/pi.tar.gz" | sha256sum -c -; \
    mkdir -p /opt/msbx/pi; \
    tar -xzf /tmp/pi.tar.gz -C /opt/msbx/pi --strip-components=1; \
    test -x /opt/msbx/pi/pi; \
    ln -s /opt/msbx/pi/pi /usr/local/bin/pi; \
    rm /tmp/pi.tar.gz

# The pinned Pi packages, installed once at build time into a read-only tree.
# They are loaded by path from the baseline settings, never installed at run
# time. `--legacy-peer-deps` keeps Pi itself out of the tree: the extension is
# loaded by the running Pi, which supplies the peer modules.
RUN set -eux; \
    printf '%s' "$PI_PACKAGES_JSON" > /tmp/pi-packages.json; \
    mkdir -p /opt/msbx/pi-packages; \
    jq -r '.[] | [.name, .version, .url, .sha256] | @tsv' /tmp/pi-packages.json | \
    while IFS="$(printf '\t')" read -r name version url sha; do \
        curl -fsSL "$url" -o "/tmp/package.tgz"; \
        echo "$sha  /tmp/package.tgz" | sha256sum -c -; \
        npm install --prefix /opt/msbx/pi-packages --omit=dev --legacy-peer-deps \
            --ignore-scripts --no-audit --no-fund "/tmp/package.tgz"; \
        node -e 'const p = require("/opt/msbx/pi-packages/node_modules/" + process.argv[1] + "/package.json"); if (p.version !== process.argv[2]) process.exit(1)' \
            "$name" "$version"; \
        rm /tmp/package.tgz; \
    done; \
    rm -rf /root/.npm

# pnpm for the workspace's Node projects.
RUN set -eux; \
    npm install -g --ignore-scripts --no-audit --no-fund "pnpm@${PNPM_VERSION}"; \
    test "$(pnpm --version)" = "$PNPM_VERSION"; \
    rm -rf /root/.npm

# The Linux Multica CLI at the workspace's version.
RUN set -eux; \
    case "$TARGETARCH" in \
        amd64) url="$MULTICA_URL_AMD64"; sha="$MULTICA_SHA256_AMD64" ;; \
        arm64) url="$MULTICA_URL_ARM64"; sha="$MULTICA_SHA256_ARM64" ;; \
        *) echo "msbx: unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "$url" -o /tmp/multica.tar.gz; \
    echo "$sha  /tmp/multica.tar.gz" | sha256sum -c -; \
    mkdir -p /tmp/multica; \
    tar -xzf /tmp/multica.tar.gz -C /tmp/multica multica; \
    install -m 0755 /tmp/multica/multica /usr/local/bin/multica; \
    rm -rf /tmp/multica /tmp/multica.tar.gz; \
    multica --version | grep -F "$MULTICA_VERSION"

# The baseline agent dir and role overlays. The shim copies this once into
# $XDG_STATE_HOME/msbx/agents/<agent-id>/pi, then overlays the role dir.
COPY agent/ /opt/msbx/agent/
# The seed runs as the host UID, so the baseline must be world-readable even
# when the checkout that built it had a tighter umask.
RUN chmod -R a+rX /opt/msbx/agent

COPY entrypoint.sh /usr/local/bin/msbx-entrypoint
RUN chmod 0755 /usr/local/bin/msbx-entrypoint \
    && mkdir -p /home/agent \
    && chmod 1777 /home/agent

# Pi's assets and packages are read-only: a run cannot install or update.
RUN chmod -R a-w /opt/msbx/pi /opt/msbx/pi-packages

# Every version is asserted against the manifest, not assumed.
RUN set -eux; \
    test "$(pi --version)" = "$PI_VERSION"; \
    test "$(node --version)" = "v$NODE_VERSION"; \
    rustc --version | grep -F "rustc $RUST_VERSION "; \
    cargo --version | grep -F "cargo $RUST_VERSION "; \
    rustfmt --version; \
    cargo clippy --version; \
    go version | grep -F "go$GO_VERSION "; \
    python3 --version | grep -F "$PYTHON_VERSION"; \
    git --version; \
    gh --version; \
    jq --version; \
    rg --version; \
    socat -V; \
    test -f /opt/msbx/agent/agent/settings.json; \
    test -d /opt/msbx/pi-packages/node_modules/pi-subagents

ARG BUILT_AT

LABEL org.opencontainers.image.title="msbx-pi" \
      org.opencontainers.image.description="The Multica harness image: Pi, the pinned packages and the workspace toolchains" \
      com.msbx.pi.version="$PI_VERSION" \
      com.msbx.pi.packages="$PI_PACKAGES_JSON" \
      com.msbx.multica.version="$MULTICA_VERSION" \
      com.msbx.node.version="$NODE_VERSION" \
      com.msbx.pnpm.version="$PNPM_VERSION" \
      com.msbx.rust.version="$RUST_VERSION" \
      com.msbx.go.version="$GO_VERSION" \
      com.msbx.python.version="$PYTHON_VERSION" \
      com.msbx.base="$BASE_IMAGE" \
      com.msbx.built_at="$BUILT_AT"

WORKDIR /home/agent
ENTRYPOINT ["/usr/local/bin/msbx-entrypoint"]
