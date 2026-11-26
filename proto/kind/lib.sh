#!/usr/bin/env bash
# Shared paths and helpers for the kind substrate prototype.
#
# This is throwaway prototype code: nothing under proto/ is meant to merge.
# Every absolute path here is machine-local and is embedded into the generated
# cluster config and manifests, which is the point of the prototype (P1/P2).

set -euo pipefail

PROTO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$PROTO_DIR/../.." && pwd)"

KIND_BIN="${KIND_BIN:-$(command -v kind || true)}"
KUBECTL_BIN="${KUBECTL_BIN:-$(command -v kubectl || true)}"
DOCKER_BIN="${DOCKER_BIN:-$(command -v docker || true)}"

CLUSTER="${MSBX_KIND_CLUSTER:-msbx}"
API_PORT="${MSBX_KIND_API_PORT:-6445}"

# The root the sandbox's task workdirs live under. msbx mounts this whole root
# into the kind node so a pod can use the task workdir at its identical path.
WORKDIR_ROOT="${MSBX_WORKDIR_ROOT:-$HOME/multica_workspaces_desktop-api.multica.ai}"
if [ ! -d "$WORKDIR_ROOT" ]; then
  WORKDIR_ROOT="$HOME/multica_workspaces"
fi

STATE_ROOT="${MSBX_STATE_ROOT:-${XDG_STATE_HOME:-$HOME/.local/state}/msbx}"
PROJECT="${MSBX_PROJECT:-kindproto}"

INGRESS_HTTP_PORT="${MSBX_INGRESS_HTTP_PORT:-8080}"
INGRESS_HTTPS_PORT="${MSBX_INGRESS_HTTPS_PORT:-8443}"
INGRESS_NGINX_VERSION="${INGRESS_NGINX_VERSION:-controller-v1.12.1}"

SCOPES=(scope-a scope-b)
GENERATED="$PROTO_DIR/.generated"

# Keep the cluster's kubeconfig inside the prototype, not in ~/.kube: the
# prototype must not mutate host tooling state beyond the cluster and the msbx
# state dir it already owns.
export KUBECONFIG="${MSBX_KUBECONFIG:-$GENERATED/kubeconfig}"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
say() { printf '==> %s\n' "$*"; }

require() {
  local missing=0 name path
  for name in "$@"; do
    case "$name" in
      kind) path="$KIND_BIN" ;;
      kubectl) path="$KUBECTL_BIN" ;;
      docker) path="$DOCKER_BIN" ;;
      *) path="$(command -v "$name" || true)" ;;
    esac
    if [ -z "$path" ] || [ ! -x "$path" ]; then
      printf 'missing: %s\n' "$name" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || die "install the tools above first (KIND_BIN=... may point at a downloaded binary)"
}

ctx() { printf 'kind-%s' "$CLUSTER"; }

ns_for() { printf '%s-%s' "$PROJECT" "$1"; }
scope_dir() { printf '%s/%s/%s' "$STATE_ROOT" "$PROJECT" "$1"; }
scope_domain() { printf '%s.%s.msbx.test' "$1" "$PROJECT"; }
role_fqdn() { printf '%s.%s.%s.msbx.test' "$1" "$2" "$PROJECT"; }

# Milliseconds, portable enough for macOS.
now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

cluster_exists() {
  "$KIND_BIN" get clusters 2>/dev/null | grep -qx "$CLUSTER"
}

node_container() { printf '%s-control-plane' "$CLUSTER"; }

docker_host_ip() {
  "$DOCKER_BIN" exec "$(node_container)" getent ahostsv4 host.docker.internal 2>/dev/null | awk 'NR==1 {print $1}'
}
