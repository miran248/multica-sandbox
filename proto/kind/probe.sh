#!/usr/bin/env bash
# P3 — the measurements. Run after bootstrap.sh and after `scope.sh up` on both
# scopes. Writes a TSV to .generated/measurements.tsv and prints it.
#
# Numbers are facts about this machine on this run; the doc cites them as such.

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

require kind kubectl docker
R="$GENERATED/measurements.tsv"
mkdir -p "$GENERATED"
: > "$R"

record() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" | tee -a "$R"; }
obs() { record "$1" OBSERVED "$2"; }
pass() { record "$1" PASS "$2"; }
fail() { record "$1" FAIL "$2"; }

ns_a="$(ns_for scope-a)"
ns_b="$(ns_for scope-b)"

tcp_probe() { # host port -> open|closed
  python3 - "$1" "$2" <<'PY'
import socket, sys
host, port = sys.argv[1], int(sys.argv[2])
s = socket.socket(); s.settimeout(3)
try:
    s.connect((host, port)); print("open")
except Exception as e:
    print("closed: %s" % e)
finally:
    s.close()
PY
}

wait_marker() { # namespace scope -> 0 when psql in that namespace answers
  local ns="$1" want="$2"
  for _ in $(seq 1 60); do
    [ "$(hquery "$ns" 'select scope from marker limit 1' || true)" = "$want" ] && return 0
    sleep 1
  done
  return 1
}

hquery() { # namespace sql
  "$KUBECTL_BIN" -n "$1" exec deploy/harness -- sh -c 'psql "$DATABASE_URL" -Atc "$1"' _ "$2" 2>/dev/null | tr -d '\r'
}

# --- environment facts -----------------------------------------------------
record env INFO "kind=$("$KIND_BIN" version) docker=$("$DOCKER_BIN" version --format '{{.Server.Version}}') node=$("$KUBECTL_BIN" get node -o jsonpath='{.items[0].status.nodeInfo.kubeletVersion}')"
record env INFO "api_server=$("$KUBECTL_BIN" config view --minify -o jsonpath='{.clusters[0].cluster.server}') ingress=http://127.0.0.1:$INGRESS_HTTP_PORT"

# --- M1: two scopes, each reaches its own pg by service name ----------------
a="$(hquery "$ns_a" 'select scope from marker limit 1' || true)"
b="$(hquery "$ns_b" 'select scope from marker limit 1' || true)"
if [ "$a" = "scope-a" ] && [ "$b" = "scope-b" ]; then
  pass M1 "harness in $ns_a -> pg:5432 returned '$a'; harness in $ns_b -> pg:5432 returned '$b' (same role name, canonical port, no port block)"
else
  fail M1 "expected scope-a/scope-b, got '$a'/'$b'"
fi

# --- M2a: host reaches web by name through the ingress ---------------------
if body="$(curl -fsS -H "Host: web.scope-a.kindproto.msbx.test" "http://127.0.0.1:$INGRESS_HTTP_PORT/" 2>&1)" && printf '%s' "$body" | grep -q 'scope-a'; then
  pass M2a "curl with Host: web.scope-a.kindproto.msbx.test -> 127.0.0.1:$INGRESS_HTTP_PORT served scope-a (virtual host, one listener)"
else
  fail M2a "ingress did not serve web.scope-a.kindproto.msbx.test on :$INGRESS_HTTP_PORT: ${body:-no body}"
fi

# --- M2b: host reaches pg over a forward ----------------------------------
pf_log="$GENERATED/port-forward.log"
"$KUBECTL_BIN" -n "$ns_a" port-forward svc/pg 15432:5432 >"$pf_log" 2>&1 &
pf_pid=$!
forward_open=closed
for _ in $(seq 1 30); do
  if [ "$(tcp_probe 127.0.0.1 15432)" = "open" ]; then forward_open=open; break; fi
  sleep 1
done
if psql_bin="$(command -v psql)"; then
  if out="$("$psql_bin" 'postgres://postgres:proto-pg-a@127.0.0.1:15432/app?sslmode=disable' -Atc 'select scope from marker limit 1' 2>&1)" && [ "$out" = "scope-a" ]; then
    pass M2b "host psql -> 127.0.0.1:15432 (kubectl port-forward svc/pg) queried '$out'"
  else
    fail M2b "forward socket $forward_open but psql failed: $out"
  fi
else
  [ "$forward_open" = open ] && pass M2b "host TCP 127.0.0.1:15432 open via kubectl port-forward svc/pg (no local psql; TCP only)" \
    || fail M2b "forward socket not open; see $pf_log"
fi

# --- M3: does the forward survive a pod restart unattended? ---------------
old_uid="$("$KUBECTL_BIN" -n "$ns_a" get pod pg-0 -o jsonpath='{.metadata.uid}')"
"$KUBECTL_BIN" -n "$ns_a" delete pod pg-0 --wait=false >/dev/null
new_uid="$old_uid"
for _ in $(seq 1 120); do
  new_uid="$("$KUBECTL_BIN" -n "$ns_a" get pod pg-0 -o jsonpath='{.metadata.uid}' 2>/dev/null || true)"
  [ -n "$new_uid" ] && [ "$new_uid" != "$old_uid" ] && break
  sleep 1
done
"$KUBECTL_BIN" -n "$ns_a" wait --for=condition=Ready pod/pg-0 --timeout=180s >/dev/null
sleep 2
alive=no
kill -0 "$pf_pid" 2>/dev/null && alive=yes
after="$(tcp_probe 127.0.0.1 15432)"
query=""
if [ -n "${psql_bin:-}" ]; then
  query="$("$psql_bin" 'postgres://postgres:proto-pg-a@127.0.0.1:15432/app?sslmode=disable' -Atc 'select scope from marker limit 1' 2>&1 || true)"
fi
if [ "$alive" = yes ] && [ "$after" = open ] && { [ -z "${psql_bin:-}" ] || [ "$query" = scope-a ]; }; then
  pass M3 "deleting pg-0 (uid replaced) left the forward process alive and still usable (socket=$after query='${query:-n/a}')"
else
  fail M3 "after pg-0 was replaced (old=$old_uid new=$new_uid): process_alive=$alive socket=$after query='${query:-n/a}' (see $pf_log)"
fi
kill "$pf_pid" 2>/dev/null || true
wait "$pf_pid" 2>/dev/null || true

# --- M4: images load without a registry -----------------------------------
# `kind load docker-image` refuses multi-arch registry images on this
# combination (kind v0.33, containerd 2.3, --all-platforms). It is attempted
# first and the fallback is the equivalent stream without --all-platforms.
: > "$GENERATED/image-load.tsv"
kind_batch=ok
if ! "$KIND_BIN" load docker-image --name "$CLUSTER" \
      kindproto/web:dev kindproto/harness:dev postgres:16-alpine nginx:alpine >/dev/null 2>&1; then
  kind_batch=failed
fi
for image in kindproto/web:dev kindproto/harness:dev postgres:16-alpine nginx:alpine; do
  start=$(now_ms)
  if "$DOCKER_BIN" save "$image" \
      | "$DOCKER_BIN" exec -i "$(node_container)" ctr -n k8s.io images import --digests - >/dev/null 2>&1; then
    printf '%s\tctr-import\t%s\n' "$image" "$(( $(now_ms) - start ))" | tee -a "$GENERATED/image-load.tsv"
  else
    printf '%s\tFAILED\t%s\n' "$image" "$(( $(now_ms) - start ))" | tee -a "$GENERATED/image-load.tsv"
  fi
done
in_node="$("$DOCKER_BIN" exec "$(node_container)" crictl images 2>/dev/null | grep -cE 'kindproto/(web|harness)|postgres.*16-alpine|nginx.*alpine' || true)"
pull_policy="$("$KUBECTL_BIN" -n "$ns_a" get deploy web -o jsonpath='{.spec.template.spec.containers[0].imagePullPolicy}')"
if [ "$in_node" -ge 4 ] && [ "$pull_policy" = Never ]; then
  pass M4 "kind load docker-image batch=$kind_batch; docker save | ctr images import loaded all 4 without a registry; pods run imagePullPolicy=$pull_policy (a pull cannot happen)"
else
  fail M4 "images_in_node=$in_node imagePullPolicy=$pull_policy kind_batch=$kind_batch"
fi

# --- M5: data survives down/up and a node restart -------------------------
hquery "$ns_a" "delete from probe where note='before-down'" >/dev/null
hquery "$ns_a" "insert into probe (note) values ('before-down')" >/dev/null
"$PROTO_DIR/scope.sh" down scope-a >/dev/null
up_out="$("$PROTO_DIR/scope.sh" up scope-a)"
warm_ms="$(printf '%s' "$up_out" | sed -n 's/^up_to_first_query_ms=//p')"
count="$(hquery "$ns_a" "select count(*) from probe where note='before-down'" || true)"
[ "$count" = "1" ] && pass M5a "row written before \`down\` still present after \`down\`+\`up\` (warm up_to_first_query_ms=$warm_ms)" \
  || fail M5a "row count after down/up = '$count'"

start=$(now_ms)
"$DOCKER_BIN" stop "$(node_container)" >/dev/null
"$DOCKER_BIN" start "$(node_container)" >/dev/null
# The API server accepts connections before its bootstrap RBAC policy is
# loaded; the first `kubectl` call can return 403 for a moment. Retry the
# readiness gate instead of treating that window as a failed restart.
nodes_ok=no
for _ in $(seq 1 120); do
  if "$KUBECTL_BIN" get nodes >/dev/null 2>&1; then nodes_ok=yes; break; fi
  sleep 1
done
[ "$nodes_ok" = yes ] || fail M5b "API server did not become authorized within 120s after node restart"
"$KUBECTL_BIN" wait --for=condition=Ready "node/$(node_container)" --timeout=240s >/dev/null
"$KUBECTL_BIN" -n "$ns_a" rollout status statefulset/pg --timeout=240s >/dev/null
"$KUBECTL_BIN" -n "$ns_a" rollout status deployment/harness --timeout=240s >/dev/null
wait_marker "$ns_a" scope-a || true
restart_ms=$(( $(now_ms) - start ))
count="$(hquery "$ns_a" "select count(*) from probe where note='before-down'" || true)"
q="$(hquery "$ns_a" 'select scope from marker limit 1' || true)"
[ "$count" = "1" ] && [ "$q" = "scope-a" ] \
  && pass M5b "after node stop/start (${restart_ms} ms to Ready) data and scope marker intact" \
  || fail M5b "after node restart count='$count' marker='$q' restart_ms=$restart_ms"

# --- M6: workdir identity and the host daemon port ------------------------
wd="$GENERATED/workdir-write"
mkdir -p "$wd"
chmod 755 "$wd"
# Host writes one file; the pod must see it at the same absolute path.
printf 'host-can-write\n' > "$wd/from-host.txt"
pod_read="$("$KUBECTL_BIN" -n "$ns_a" exec deploy/harness -- cat "$wd/from-host.txt" 2>/dev/null || true)"
pod_uid="$("$KUBECTL_BIN" -n "$ns_a" exec deploy/harness -- id -u 2>/dev/null || true)"
host_uid="$(stat -f '%u' "$wd")"
pod_write_fail=""
if ! "$KUBECTL_BIN" -n "$ns_a" exec deploy/harness -- sh -c "echo pod > '$wd/from-pod.txt'" 2>/dev/null; then
  pod_write_fail="mode-755"
  chmod 777 "$wd"
  "$KUBECTL_BIN" -n "$ns_a" exec deploy/harness -- sh -c "echo pod > '$wd/from-pod.txt'" 2>/dev/null || pod_write_fail="failed"
fi
host_seen="$(cat "$wd/from-pod.txt" 2>/dev/null || true)"
host_owner="$(stat -f '%Su:%Sg %Sp' "$wd/from-pod.txt" 2>/dev/null || echo missing)"
if [ "$pod_read" = "host-can-write" ] && [ "$host_seen" = "pod" ]; then
  pass M6a "identical absolute path $wd: host file read in pod; pod file (pod uid $pod_uid) read on host as $host_owner; dir was ${pod_write_fail:-writable at 755}"
else
  fail M6a "workdir identity: pod_read='$pod_read' host_seen='$host_seen' pod_uid=$pod_uid host_uid=$host_uid write_fail=$pod_write_fail"
fi

# The daemon listens on 127.0.0.1:19681 (pid 9304 at bootstrap time). A pod is
# a different network namespace; host.docker.internal is the only route back.
ip="$(docker_host_ip || true)"
daemon_probe="$("$KUBECTL_BIN" -n "$ns_a" exec deploy/harness -- sh -c 'command -v nc >/dev/null && nc -z -w 3 host.docker.internal 19681 && echo open || echo closed' 2>/dev/null || echo closed)"
python3 -m http.server 19777 --bind 127.0.0.1 >/dev/null 2>&1 &
loop_pid=$!
python3 -m http.server 19778 --bind 0.0.0.0 >/dev/null 2>&1 &
wild_pid=$!
sleep 1
loop_probe="$("$KUBECTL_BIN" -n "$ns_a" exec deploy/harness -- sh -c 'nc -z -w 3 host.docker.internal 19777 && echo open || echo closed' 2>/dev/null || echo closed)"
wild_probe="$("$KUBECTL_BIN" -n "$ns_a" exec deploy/harness -- sh -c 'nc -z -w 3 host.docker.internal 19778 && echo open || echo closed' 2>/dev/null || echo closed)"
kill "$loop_pid" "$wild_pid" 2>/dev/null || true
if [ "$daemon_probe" = open ]; then
  pass M6b "pod -> host.docker.internal:$ip reaches the Multica daemon on 127.0.0.1:19681"
else
  fail M6b "pod -> host.docker.internal ($ip) cannot reach the daemon on 127.0.0.1:19681 (loopback-bound listener: $loop_probe; wildcard-bound listener: $wild_probe)"
fi

# --- observation: cross-scope reach on the flat local network --------------
cross="$("$KUBECTL_BIN" -n "$ns_a" exec deploy/harness -- sh -c 'psql "postgres://postgres:proto-pg-b@pg.kindproto-scope-b:5432/app?sslmode=disable" -Atc "select scope from marker limit 1"' 2>/dev/null | tr -d '\r' || true)"
if [ "$cross" = "scope-b" ]; then
  obs X1 "harness in $ns_a reached pg.$ns_b:5432 across namespaces and read '$cross' (flat pod network; C6 is not a local guarantee)"
else
  obs X1 "cross-namespace reach from $ns_a to $ns_b did not succeed ('$cross')"
fi

say "measurements written to $R"
cat "$R"
