# `proto/kind` — throwaway kind-substrate prototype

Never merged. The branch exists to answer one question with measurements: **can
a local kind cluster replace the Compose substrate for one epic's services?**

It is not the epic, not `msbx` support for kind, and not a driver interface.
The implementation epic exists only if the report in
[`docs/proto/kind.md`](../../docs/proto/kind.md) says go.

## Shape

| Piece | What it is |
| --- | --- |
| `config/kind.yaml.tmpl` | The creation-time contract: pinned API port, one ingress window, `extraMounts` of the workdir root and the msbx state root at their identical absolute paths. |
| `bootstrap.sh` | P1. Idempotent cluster + ingress + image load, with checks that fail loudly when the cluster is missing, mis-mounted or on the wrong API port. `--check` changes nothing. |
| `render.sh` | Materializes one scope's envelope under `~/.local/state/msbx/<project>/<scope>/` (0600) and turns it into one Secret per role plus the machine-local path patch. Checked-in envelope values stand in for the store. |
| `scope.sh` | P2. `up`, `down` (scale to zero, keep data), `rm` (namespace **and** host data), `status`. One namespace per scope, `kubectl apply -k`, no ArgoCD. |
| `manifests/base` | Postgres on a hostPath data dir, a static web role behind the shared ingress, and a Pi stand-in pod that runs `psql` and sees the task workdir at its real path. |
| `manifests/overlays/<scope>` | Namespace, ingress host, initdb seed, document root, and the generated Secret/patch inputs. |
| `probe.sh` | P3. The six measurements plus cold/warm time-to-first-query and the cross-scope observation. Writes `.generated/measurements.tsv`. |
| `docs/proto/kind.md` | P4. The measured table, failure modes, go/no-go and the host-reach branches. |

## Run

```sh
export KIND_BIN=/path/to/kind          # only if kind is not on PATH
./bootstrap.sh
./scope.sh up scope-a
./scope.sh up scope-b
./probe.sh
./scope.sh status scope-a
```

Cleanup: `./scope.sh rm scope-a`, `./scope.sh rm scope-b`, then
`kind delete cluster --name msbx`. Deleting the cluster deletes every scope's
hostPath data, because the state root is shared.

## Deviation from the epic contract

The prototype keeps the contract's *observable* shape — role names, canonical
ports, cluster DNS, the envelope and `/run/msbx/secrets/<NAME>` — and replaces
only the pieces the epic would rewrite anyway: Compose becomes kustomize,
`env_file` becomes a Secret, `docker compose stop` becomes scale-to-zero. The
checked-in `envelope/` directory stands in for the store; values are fake.
