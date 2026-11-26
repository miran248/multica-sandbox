#!/usr/bin/env bash
# P1 — idempotent bootstrap of the machine-scoped kind cluster.
#
#   ./bootstrap.sh          create/verify the cluster, ingress, and images
#   ./bootstrap.sh --check  verify only; change nothing
#
# The cluster's shape is fixed at creation: API port, the two extraMounts, the
# ingress port mappings. This script owns `kind create cluster` so a hand-made
# cluster cannot drift from the expectation; it fails loudly instead.

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CHECK=0
[ "${1:-}" = "--check" ] && CHECK=1

require kind kubectl docker
[ -d "$WORKDIR_ROOT" ] || die "workdir root does not exist: $WORKDIR_ROOT"
[ -d "$STATE_ROOT" ] || die "state root does not exist: $STATE_ROOT (msbx owns it; the prototype does not create it)"

say "cluster=$CLUSTER api=127.0.0.1:$API_PORT workdir=$WORKDIR_ROOT state=$STATE_ROOT"

# --- preflight -------------------------------------------------------------
if ! "$DOCKER_BIN" info >/dev/null 2>&1; then
  die "Docker is not reachable; start Docker Desktop first"
fi

if [ "$CHECK" -eq 0 ]; then
  # Docker Desktop file sharing: the host path must be visible inside a
  # container, otherwise the same path inside the kind node is an empty mount.
  for root in "$WORKDIR_ROOT" "$STATE_ROOT"; do
    if ! "$DOCKER_BIN" run --rm -v "$root:$root" alpine:3.20 test -d "$root" >/dev/null 2>&1; then
      die "Docker Desktop does not share $root; add it in Settings -> Resources -> File sharing"
    fi
  done
  say "preflight: Docker reachable, both roots shareable"
fi

# --- cluster ---------------------------------------------------------------
render_config="$GENERATED/kind-config.yaml"
mkdir -p "$GENERATED"
{
  sed -e "s|__CLUSTER__|$CLUSTER|g" \
      -e "s|__API_PORT__|$API_PORT|g" \
      -e "s|__INGRESS_HTTP_PORT__|$INGRESS_HTTP_PORT|g" \
      -e "s|__INGRESS_HTTPS_PORT__|$INGRESS_HTTPS_PORT|g" \
      -e "s|__WORKDIR_ROOT__|$WORKDIR_ROOT|g" \
      -e "s|__STATE_ROOT__|$STATE_ROOT|g" \
      "$PROTO_DIR/config/kind.yaml.tmpl"
} > "$render_config"

if cluster_exists; then
  say "cluster exists; reusing (creation-time shape is immutable)"
else
  [ "$CHECK" -eq 0 ] || die "cluster $CLUSTER does not exist"
  if lsof -nP -iTCP:"$API_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    die "API port $API_PORT is already in use; set MSBX_KIND_API_PORT to a free port"
  fi
  start=$(now_ms)
  "$KIND_BIN" create cluster --config "$render_config" --kubeconfig "$KUBECONFIG" --wait 120s
  say "cluster created in $(( $(now_ms) - start )) ms"
fi

# Export the kubeconfig into the prototype even when the cluster already
# existed, so kubectl never needs to write ~/.kube.
"$KIND_BIN" export kubeconfig --name "$CLUSTER" --kubeconfig "$KUBECONFIG"
"$KUBECTL_BIN" wait --for=condition=Ready "node/$(node_container)" --timeout=180s >/dev/null

# --- verify the creation-time decisions survived ---------------------------
server="$("$KUBECTL_BIN" config view --minify -o jsonpath='{.clusters[0].cluster.server}')"
[ "$server" = "https://127.0.0.1:$API_PORT" ] || die "API server is $server, expected https://127.0.0.1:$API_PORT"

for root in "$WORKDIR_ROOT" "$STATE_ROOT"; do
  "$DOCKER_BIN" exec "$(node_container)" test -d "$root" \
    || die "extraMount missing in node: $root (the cluster must be re-created)"
  "$DOCKER_BIN" exec "$(node_container)" sh -c "test -w '$root'" \
    || die "extraMount is not writable in node: $root"
done
say "cluster check: API pinned, both extraMounts present and writable"

"$KUBECTL_BIN" get nodes -o wide

# --- ingress-nginx ---------------------------------------------------------
# One published window: the ingress controller answers every *.msbx.test name
# through hostPort 80 -> host 8080. HTTP only; TCP roles use a forward.
if [ "$CHECK" -eq 0 ]; then
  if ! "$KUBECTL_BIN" get namespace ingress-nginx >/dev/null 2>&1; then
    start=$(now_ms)
    "$KUBECTL_BIN" apply -f "https://raw.githubusercontent.com/kubernetes/ingress-nginx/${INGRESS_NGINX_VERSION}/deploy/static/provider/kind/deploy.yaml"
    say "ingress-nginx manifests applied after $(( $(now_ms) - start )) ms"
  fi
  "$KUBECTL_BIN" -n ingress-nginx wait --for=condition=complete job/ingress-nginx-admission-create --timeout=180s >/dev/null 2>&1 || true
  "$KUBECTL_BIN" -n ingress-nginx wait --for=condition=complete job/ingress-nginx-admission-patch --timeout=180s >/dev/null 2>&1 || true
  "$KUBECTL_BIN" -n ingress-nginx rollout status deploy/ingress-nginx-controller --timeout=240s
  say "ingress-nginx ready on host 127.0.0.1:$INGRESS_HTTP_PORT -> node :80"
fi

# --- workload images (no registry) ----------------------------------------
# `kind load docker-image` is the intended path, but kind v0.33 / containerd 2.3
# fails it on this machine: it imports with --all-platforms and the multi-arch
# index references blobs Docker never pulled. The equivalent no-registry path is
# `docker save | ctr -n k8s.io images import` without --all-platforms. Both are
# recorded, because "images load without a registry" is the measurement.
load_image() { # image -> prints kind-load|ctr-import, 0 on success
  local image="$1"
  if "$KIND_BIN" load docker-image --name "$CLUSTER" "$image" >/dev/null 2>&1; then
    printf 'kind-load'
    return 0
  fi
  "$DOCKER_BIN" save "$image" \
    | "$DOCKER_BIN" exec -i "$(node_container)" ctr -n k8s.io images import --digests - >/dev/null 2>&1 \
    || return 1
  printf 'ctr-import'
}

if [ "$CHECK" -eq 0 ]; then
  start=$(now_ms)
  "$DOCKER_BIN" build -t "kindproto/web:dev" "$PROTO_DIR/images/web" >/dev/null
  "$DOCKER_BIN" build -t "kindproto/harness:dev" "$PROTO_DIR/images/harness" >/dev/null
  build_ms=$(( $(now_ms) - start ))

  : > "$GENERATED/image-load.txt"
  for image in kindproto/web:dev kindproto/harness:dev postgres:16-alpine nginx:alpine; do
    start=$(now_ms)
    method="$(load_image "$image")" || die "image could not be loaded into the node without a registry: $image"
    printf '%s\t%s\t%s\n' "$image" "$method" "$(( $(now_ms) - start ))" | tee -a "$GENERATED/image-load.txt"
  done

  printf 'images: build=%sms (never pushed; loaded from the local daemon)\n' "$build_ms"
  for image in kindproto/web:dev kindproto/harness:dev postgres:16-alpine nginx:alpine; do
    "$DOCKER_BIN" exec "$(node_container)" crictl images 2>/dev/null | grep -q "${image%%:*}" \
      || die "image not present in the node after load: $image"
  done
  say "images present in node; pods run with imagePullPolicy: Never"
fi

say "bootstrap complete"
