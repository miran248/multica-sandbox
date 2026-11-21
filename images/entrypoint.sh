#!/bin/sh
# The harness container entrypoint: start the daemon forwarder, configure git
# for the harness token, then run Pi with the daemon's argv.
#
# The in-image Multica CLI hard-codes 127.0.0.1 for the daemon's endpoints.
# Inside the container that is the container itself, so forward the daemon port
# from the loopback to the host. The shim adds host.docker.internal on Linux;
# Docker Desktop provides it on macOS.
set -eu

if [ -n "${MULTICA_DAEMON_PORT:-}" ]; then
    socat \
        "TCP-LISTEN:${MULTICA_DAEMON_PORT},bind=127.0.0.1,fork,reuseaddr" \
        "TCP:host.docker.internal:${MULTICA_DAEMON_PORT}" &
fi

# HTTPS remotes need a credential helper. `gh` reads GH_TOKEN from the
# environment; the harness store supplied it through env/harness.env.
if [ -n "${GH_TOKEN:-}" ] && command -v gh >/dev/null 2>&1; then
    gh auth setup-git >/dev/null 2>&1 || {
        echo "msbx: gh auth setup-git failed; git HTTPS pushes may not authenticate" >&2
    }
fi

exec pi "$@"
