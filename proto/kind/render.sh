#!/usr/bin/env bash
# Render one scope: materialize the envelope under the msbx state root, turn it
# into one Secret per role, and write the machine-local patch (hostPath paths,
# host gateway address) the overlay references.
#
# The checked-in envelope under proto/kind/envelope/<scope>/ stands in for the
# store; in the epic the store is the source and this step is msbx's `render`.
# Values never enter the checkout.

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

scope="${1:-}"
[ -n "$scope" ] || die "usage: render.sh <scope>"
printf '%s\n' "${SCOPES[@]}" | grep -qx "$scope" || die "unknown scope: $scope"

src="$PROTO_DIR/envelope/$scope"
dst="$(scope_dir "$scope")"
generated="$PROTO_DIR/manifests/overlays/$scope/.generated"
ns="$(ns_for "$scope")"

[ -d "$src/env" ] || die "envelope missing: $src/env"

mkdir -p "$dst/env" "$dst/secrets" "$dst/pgdata" "$generated"
chmod 700 "$dst"
install -m 600 "$src"/env/*.env "$dst/env/"
if compgen -G "$src/secrets/*" >/dev/null; then
  install -m 600 "$src"/secrets/* "$dst/secrets/"
fi

# One Secret per role: env names from env/<role>.env, plus the secret names the
# role is allowed to see (the x-msbx `secrets` list). Emitted as JSON documents,
# which kustomize reads as YAML.
python3 - "$dst" "$generated/secrets.yaml" "$ns" <<'PYEOF'
import base64, json, os, sys

dst, out, ns = sys.argv[1], sys.argv[2], sys.argv[3]
role_secrets = {"web": ["API_TOKEN"], "harness": ["API_TOKEN"]}
docs = []
for filename in sorted(os.listdir(os.path.join(dst, "env"))):
    if not filename.endswith(".env"):
        continue
    role = filename[:-4]
    data = {}
    with open(os.path.join(dst, "env", filename)) as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            name, _, value = line.partition("=")
            data[name] = base64.b64encode(value.encode()).decode()
    for name in role_secrets.get(role, []):
        path = os.path.join(dst, "secrets", name)
        if not os.path.exists(path):
            sys.exit("secret %s is listed for role %s but absent from the envelope" % (name, role))
        with open(path, "rb") as handle:
            data[name] = base64.b64encode(handle.read().strip()).decode()
    docs.append({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": role, "namespace": ns},
        "type": "Opaque",
        "data": data,
    })
with open(out, "w") as handle:
    handle.write("\n---\n".join(json.dumps(doc) for doc in docs))
    handle.write("\n")
PYEOF
printf 'rendered %s secrets for %s\n' "$scope" "$ns"

# Machine-local patch: the paths cannot be kustomize literals without hardcoding
# this machine, and the Docker host address is discovered at bootstrap.
gateway="$(docker_host_ip || true)"
patch="$generated/runtime-patch.yaml"
{
  cat <<EOF
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: pg
spec:
  template:
    spec:
      volumes:
        - name: data
          hostPath:
            path: $(scope_dir "$scope")/pgdata
            type: DirectoryOrCreate
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: harness
spec:
  template:
    spec:
      containers:
        - name: harness
          volumeMounts:
            - name: workdir
              mountPath: $WORKDIR_ROOT
      volumes:
        - name: workdir
          hostPath:
            path: $WORKDIR_ROOT
            type: Directory
EOF
  if [ -n "$gateway" ]; then
    cat <<EOF
      hostAliases:
        - ip: $gateway
          hostnames:
            - host.docker.internal
EOF
  fi
} > "$patch"

printf 'rendered %s: envelope=%s gateway=%s\n' "$scope" "$dst" "${gateway:-none}"
