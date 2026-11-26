#!/usr/bin/env bash
# P2 — one namespace per scope, applied with kubectl, ArgoCD-shaped.
#
#   ./scope.sh up <scope>     apply, wait, print up-to-first-query
#   ./scope.sh down <scope>   scale to zero; namespace, secrets and data stay
#   ./scope.sh rm <scope>     delete the namespace and its hostPath data
#   ./scope.sh status <scope> pods, services, ingress

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

cmd="${1:-}"
scope="${2:-}"
[ -n "$cmd" ] && [ -n "$scope" ] || die "usage: scope.sh up|down|rm|status <scope>"
printf '%s\n' "${SCOPES[@]}" | grep -qx "$scope" || die "unknown scope: $scope"

ns="$(ns_for "$scope")"
overlay="$PROTO_DIR/manifests/overlays/$scope"

query_from_harness() {
  "$KUBECTL_BIN" -n "$ns" exec deploy/harness -- sh -c 'psql "$DATABASE_URL" -Atc "$1"' _ "$1" 2>/dev/null | tr -d '\r'
}

case "$cmd" in
  up)
    "$PROTO_DIR/render.sh" "$scope"
    start=$(now_ms)
    "$KUBECTL_BIN" apply -k "$overlay"
    "$KUBECTL_BIN" -n "$ns" rollout status statefulset/pg --timeout=180s
    "$KUBECTL_BIN" -n "$ns" rollout status deployment/web --timeout=120s
    "$KUBECTL_BIN" -n "$ns" rollout status deployment/harness --timeout=120s

    marker=""
    for _ in $(seq 1 60); do
      marker="$(query_from_harness 'select scope from marker limit 1' || true)"
      [ "$marker" = "$scope" ] && break
      sleep 1
    done
    elapsed=$(( $(now_ms) - start ))
    [ "$marker" = "$scope" ] || die "first query did not return '$scope' (got '$marker') after ${elapsed} ms"
    printf 'up_to_first_query_ms=%s scope=%s\n' "$elapsed" "$scope"
    ;;

  down)
    "$KUBECTL_BIN" -n "$ns" scale statefulset/pg deployment/web deployment/harness --replicas=0
    "$KUBECTL_BIN" -n "$ns" wait --for=delete pod -l app=pg --timeout=60s >/dev/null 2>&1 || true
    printf 'down scope=%s (namespace, secrets and %s kept)\n' "$scope" "$(scope_dir "$scope")/pgdata"
    ;;

  rm)
    "$KUBECTL_BIN" delete namespace "$ns" --wait=true --timeout=120s >/dev/null 2>&1 || true
    # hostPath data outlives the namespace by construction: deleting the
    # namespace does not touch the host directory. rm must name it explicitly.
    rm -rf "$(scope_dir "$scope")"
    printf 'rm scope=%s (namespace and host state removed)\n' "$scope"
    ;;

  status)
    "$KUBECTL_BIN" -n "$ns" get pods,svc,ingress -o wide || true
    ;;

  *) die "unknown command: $cmd" ;;
esac
