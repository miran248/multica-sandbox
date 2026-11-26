# kind substrate prototype — measurement report

Status: **experiment complete; prototype branch only.** Branch `proto/kind`,
never merged, no ticket, no epic, no labels. This report is P4 of the packet;
everything it describes lives under [`proto/kind/`](../../proto/kind/).

Question: can a local kind cluster replace the Compose substrate for one epic's
services — keeping role names, canonical ports, cluster-local service discovery,
the envelope, and host access — without the costs Compose avoids?

Host of record: macOS 15 (Darwin arm64), Docker Desktop 28.2.2 (14 CPU, 15.6 GiB
VM), kind v0.33.0, node `kindest/node:v1.37.0`, containerd 2.3.4, kubectl 1.37.
Numbers below are from one clean `./probe.sh` run unless noted; they are facts
about this machine, not guarantees.

## Verdict

**Go**, conditional on four changes that are now known rather than assumed.
None of the pre-committed kill criteria fired; one measurement changes the shape
of the epic (the forward supervisor) and one changes the bootstrap (image load).

| Pre-committed kill criterion | Outcome |
| --- | --- |
| First `up` > ~90 s | Not hit — cold 4.6–5.8 s, warm 3.2 s, reconcile 1.1–1.6 s |
| An image can be loaded only through a registry | Not hit — `docker save \| ctr images import` loaded all four in 0.4–2.3 s each |
| Workdir identity needs per-task cluster reconfiguration | Not hit — the workdir root is mounted once, at its real absolute path |
| Non-TLS TCP host reach needs a per-scope cluster | Not hit — one cluster, one forward per role |
| Forwards cannot be re-established without a human | **Hit as built** — a raw `kubectl port-forward` dies with its pod; a supervisor is mandatory |

## What was built

| Piece | Delivered |
| --- | --- |
| P1 | `proto/kind/config/kind.yaml.tmpl` + `bootstrap.sh`: pinned API port `6445`, `extraMounts` of the workdir root and the msbx state root at identical absolute paths, one ingress window (host 8080 → node 80), ingress-nginx, image load, and a `--check` mode that changes nothing. Re-running it is safe; the cluster is reused and verified. |
| P2 | `render.sh`, `scope.sh`, `manifests/base` + per-scope overlays. One namespace per scope, Postgres on a hostPath data dir, a web role behind the shared ingress, a Pi stand-in pod that runs `psql`, one Secret per role from the rendered envelope, `/run/msbx/secrets/API_TOKEN` as a projected volume. |
| P3 | `probe.sh`: the six measurements, cold/warm time-to-first-query, cross-scope observation. Raw output in `.generated/measurements.tsv`. |
| P4 | This document. |

`scope.sh up` is the prototype's `up`: `kubectl apply -k` + rollout + first
query. `down` is scale-to-zero (namespace, Secrets and data stay). `rm` deletes
the namespace **and** the host data directory, because deleting a namespace does
not touch a hostPath.

## Results

| # | Measurement | Result | Evidence |
| --- | --- | --- | --- |
| M1 | Two scopes, each reaches its own `pg` by service name on 5432 | PASS | `psql -h pg` returned `scope-a` in `kindproto-scope-a` and `scope-b` in `kindproto-scope-b`; same role name, canonical port, no port block, no aliases |
| M2a | Host reaches `web` by name through the ingress | PASS | `curl -H 'Host: web.scope-a.kindproto.msbx.test' http://127.0.0.1:8080/` served scope-a; two scopes share the one listener |
| M2b | Host reaches `pg` over a forward | PASS | host `psql` → `127.0.0.1:15432` (`kubectl port-forward svc/pg`) queried `scope-a` |
| M3 | The forward survives a pod replacement unattended | **FAIL** | `kubectl port-forward` exits with `error: lost connection to pod` when `pg-0` is replaced; before it exits the local socket can still appear open while connections fail. A human or a supervisor must re-establish it |
| M4 | Images load without a registry | PASS | `kind load docker-image` fails on this combination; `docker save \| ctr -n k8s.io images import` loaded web 402 ms, harness 1463 ms, postgres 1327 ms, nginx 581 ms; pods run `imagePullPolicy: Never` |
| M5a | Data survives `down`/`up` | PASS | a row written before `down` was present after `up`; warm up-to-first-query 3156 ms |
| M5b | Data survives a node stop/start | PASS | `docker stop`/`start` of the node: 50.2 s from stop to the first successful query, marker and row intact |
| M6a | A pod writes the task workdir at its identical absolute path | PASS | host file read in the pod; pod-written file read on the host as `miran:staff -rw-r--r--`; the harness stub ran as uid 0, the host dir was 0755 |
| M6b | A pod reaches the Multica daemon's port | PASS | `nc host.docker.internal 19681` open; `host.docker.internal` resolved IPv4 `192.168.65.254`; the daemon listens on `127.0.0.1:19681` |
| X1 | Cross-scope reach on the flat local network | OBSERVED | a harness in scope-a reached `pg.kindproto-scope-b:5432` and read `scope-b`. C6 is not a local guarantee (D11) |

### Timing

| Phase | Measured |
| --- | --- |
| `kind create cluster` (includes node image pull) | 72.7 s; control-plane ready 18 s after start |
| ingress-nginx manifests applied / controller ready | 1.1 s / not separately instrumented |
| Image build, both local images | 7.8 s |
| Load per image | 0.4–2.3 s |
| Cold `up` → first query (fresh namespace, empty data dir, `initdb`) | 4599 ms (scope-a), 5836 ms (scope-b) |
| Warm `down` → `up` → first query | 3156 ms |
| Reconcile `up` on a running scope | 1108–1588 ms |
| Node stop → first query | 50.2 s |
| First query from a warm cluster, existing pod | 1.6 s |

The per-ticket number the agent pays is the 3–6 s band, not the 72 s cluster
creation: the cluster is machine-scoped and created once by `msbx init`.

## Findings that change the epic

1. **A forward supervisor is a stage, not a detail.** M3 fails exactly as the
   kill criterion feared, and the failure is worse than an exit: the local
   listener can still accept a connection for a moment while forwarding to a
   dead sandbox. The supervisor must health-check with a real query or TCP
   request, not just watch a PID. Re-establishing is cheap (restart the
   forward), so this does not kill the substrate; it makes the supervisor the
   first host-reach deliverable.

2. **`kind load docker-image` is not the image path.** On kind v0.33 /
   containerd 2.3 it imports with `--all-platforms` and fails on multi-arch
   registry images (`content digest ... not found`); the same import without
   `--all-platforms` works. `bootstrap.sh` owns the fallback: try `kind load`,
   then `docker save | ctr -n k8s.io images import --digests -`. The epic's
   `init` must do the same, or the no-registry property silently becomes a
   registry dependency.

3. **Port 80 is not available on this Mac.** Binding `127.0.0.1:80` as the
   unprivileged user is `EACCES`, and `0.0.0.0:80` is held by Docker Desktop.
   The ingress therefore lives on host 8080, so HTTP names carry `:8080`. The
   dotted name survives; the "no port" part of today's alias contract cannot,
   unless `msbx init` installs a privileged port helper.

4. **The ingress is an addon `msbx init` must own.** kind ships no ingress
   controller; this prototype pins ingress-nginx `controller-v1.12.1` and pulls
   its two images from `registry.k8s.io` at bootstrap. That is a bootstrap-time
   registry dependency, distinct from the workload images, and should be stated
   as such.

5. **Ownership works here for a Docker Desktop reason, not a Kubernetes one.**
   The Postgres `fix-owner` init container's `chown -R 70:70` succeeded through
   the mount (init log `fix-owner=chown`), and the host sees the data dir owned
   by the host user, mode 0700. Docker Desktop maps container uids onto the host
   user; a Linux host would create real uid 70 files and the agent could not
   read them. The epic must define the uid strategy explicitly (match the
   invoking uid, or a per-scope data dir owned by it), and the Linux CI cannot
   use this measurement as evidence.

6. **The daemon is reachable from a pod — over IPv4 `host.docker.internal`.**
   The node resolves `host.docker.internal` to IPv6 first; a `hostAliases` entry
   that takes the first answer is a silent failure. Discovery must prefer
   `getent ahostsv4`. With that, a pod reaches `127.0.0.1:19681` and no daemon
   rebinding is needed.

7. **Storage semantics are cleaner than Compose, with one trap.** `down` is
   scale-to-zero and keeps data; `rm` is a namespace delete **plus** an explicit
   host-directory delete; `kind delete cluster` destroys every scope at once
   because the state root is shared. `doctor` and `gc` must say this out loud.

## Host reach: the two branches

Both branches keep the name `<role>.<epic>.<project>.msbx.test` resolving to
`127.0.0.1`; both need the forward supervisor for non-TLS TCP roles.

**Branch A — keep today's contract.** A 16-port block per scope and one
supervised forward per published role. No ingress, no vocabulary change, HTTP
and TCP both go through forwards. Cost: one supervised process per role per
scope (two for a web + database stack), and the block stays as-is.

**Branch B — ingress for HTTP, forwards for TCP.** One shared ingress on a
single host port (8080 here) virtual-hosts every scope's HTTP names; the port
block shrinks to the non-TLS TCP roles. Cost: an ingress addon in `init`, a port
in HTTP names, and TLS/annotations deferred; benefit: one listener for every
scope's web role and no forward for HTTP at all.

**Recommendation: B**, on the evidence. M2a is clean, HTTP is the common case,
and B removes the most fragile process (the web forward) from the everyday path.
A is the fallback if the epic wants zero new cluster addons.

## What the epic would have to change from the packet

| Assumption in the packet | What the prototype found |
| --- | --- |
| `msbx init` owns `kind create cluster` | Confirmed; also owns the ingress addon, the `ctr` import fallback, and IPv4 gateway discovery |
| `extraMounts` at identical paths | Confirmed, but the real workdir root is `~/multica_workspaces_desktop-api.multica.ai`; creation-time mounts mean changing roots re-creates the cluster and loses every scope's data |
| hostPath data | Works, but `rm` must delete the host dir and `kind delete cluster` is a global data loss; the epic must revisit storage if the experiment passes |
| `.multica/kind/` replaces the manifest | The kustomize base/overlay shape applied cleanly and is ArgoCD-shaped; the only machine-local pieces are the hostPath patch and the gateway, both generated per render |
| Envelope → env_file | Envelope → one Secret per role works; `/run/msbx/secrets/<NAME>` survives as a projected volume; the store stays the source and the checked-in envelope is a stand-in |
| Pi as a pod in the scope namespace | Works: cluster DNS, canonical ports, hostPath workdir and daemon reach all verified; no block and no alias needed inside the cluster |

## Reproduce

```sh
cd proto/kind
export KIND_BIN=/path/to/kind
./bootstrap.sh
./scope.sh up scope-a && ./scope.sh up scope-b
./probe.sh
```

Cleanup: `./scope.sh rm scope-a`, `./scope.sh rm scope-b`,
`kind delete cluster --name msbx`. Nothing in this branch is meant to merge;
the epic, if it is created, is a new sibling of MIR-270 and starts from a
rewrite of this material, not from the branch.
