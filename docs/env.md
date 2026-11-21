# Environment and secrets

Status: re-keyed to the scope by
[MIR-272](https://multica.ai/miran248/issues/MIR-272), stage 2 of
[MIR-270](https://multica.ai/miran248/issues/MIR-270); originally delivered by
[MIR-260](https://multica.ai/miran248/issues/MIR-260). The manifest lists names;
`msbx` resolves the values outside every checkout from three layers, renders
them per scope in two contexts, and injects them into the scope's services and
into the harness container.

## The layers

```
$XDG_CONFIG_HOME/msbx/host.env                          host values, 0600
$XDG_CONFIG_HOME/msbx/host-allow                        names a project may take
$XDG_CONFIG_HOME/msbx/projects/<project>/env            project values, 0600
$XDG_CONFIG_HOME/msbx/projects/<project>/secrets/<NAME> project secret files, 0600
$XDG_CONFIG_HOME/msbx/harness/env                       harness values, 0600
```

`XDG_CONFIG_HOME` defaults to `~/.config`. None of these paths is inside a
checkout.

- **Project** wins over **host** for the same name. A manifest name that exists
  only in `host.env` resolves only when `host-allow` lists it, one name per line
  (`#` comments); otherwise `up` refuses and names the name, `host.env` and the
  allowlist file. The manifest is repo-controlled, so without the allowlist a
  repository could name and exfiltrate any host value.
- **Harness** is not manifest-driven. It carries the model credential,
  `GH_TOKEN` and the git identity, wins over both other layers in the harness
  context, and never enters a service envelope. It is how the container gets
  credentials without `~/.config`, `~/.ssh` or `~/.gitconfig`.
- `env` is one `NAME=value` per line. Blank lines and lines whose first
  non-blank character is `#` are skipped; the value is everything after the
  first `=`, verbatim. A malformed line, a duplicate name or a name that is not
  `[A-Za-z_][A-Za-z0-9_]*` is an error that quotes the line number, never its
  content.
- `secrets/<NAME>` holds one value. One trailing newline is a file convention
  and is dropped; a newline anywhere else is an error.
- Names are identifiers because each one becomes an environment variable, a file
  name under `secrets/` and a mount path.

`msbx up` reads only the names the manifest lists. It resolves them before it
allocates a port block or starts a container: a listed name with no value, a
disallowed host name or a reserved name stops `up` and names the block, the
role and the name. Unlisted project entries are ignored, so a project can keep
values for other stages of its work.

## Reserved names

`MULTICA_*`, `PI_*`, `HOME`, `PATH` and `HOSTNAME` are refused at `up` and at
harness render, naming the name, the layer and the role. The harness sets them,
and a store value carrying one would shadow or break the launch. A declared name
that collides with a generated `MSBX_<ROLE>_HOST`/`_PORT` or `<NAME>_FILE` is an
error too. `harness` is reserved as an `x-msbx.env` role name: its file would be
`env/harness.env`, the harness envelope itself.

## Render: two contexts

Every `up` rewrites the scope's envelope under
`$XDG_STATE_HOME/msbx/<project>/<scope>/` (default `~/.local/state/...`),
outside every checkout, directories 0700 and files 0600:

| Path | Context | Content |
| --- | --- | --- |
| `env/<role>.env` | service | the role's `x-msbx.env` names, container ports |
| `env/harness.env` | harness | the harness envelope, container ports |
| `env/host.env` | host | shell exports, assigned loopback ports |
| `secrets/<NAME>` | service | a rendered secret, one value and a trailing newline |
| `override.yaml` | service | the generated Compose override |

The service and harness contexts carry the **container** ports, because every
consumer there is on the scope's Compose network. The host context carries the
**assigned loopback** ports, because that is what a host shell can reach, so a
pair is self-consistent for its side: code that uses `MSBX_<ROLE>_HOST` and
`MSBX_<ROLE>_PORT` works on the host and in the harness, and code that
hard-codes a port is wrong by construction.

The override gives each role with env entries an `env_file` pointing at
`env/<role>.env`, mounts each of its secrets read-only at
`/run/msbx/secrets/<NAME>`, and adds `<NAME>_FILE` to the role's environment.
The harness receives every declared env value and every declared secret's value
as an environment variable: it cannot see the services' secret mounts and its
mount set is closed. Values reach Compose through files, never through argv.
Files an earlier render produced and this one does not are removed.

`up` also passes the `MSBX_<ROLE>_HOST`/`_PORT` pair in the service context to
the `docker compose` process, so a project's Compose file may interpolate them
into a service; store values are never placed there.

## Tokens

A store value may carry `{{host:<role>}}`, `{{port:<role>}}`, `{{scope}}` and
`{{project}}`; `msbx` substitutes them per scope at render time. This is
host-side substitution, not Compose interpolation, so the result is literal.

- `{{host:<role>}}` is `<role>.<scope>.<project>.msbx.test` in both contexts;
  the role may be any service or tunnelled role.
- `{{port:<role>}}` renders the role's container port in the service and harness
  contexts and its assigned loopback port in the host context. It is an error
  for a role that is not in `x-msbx.publish`.
- `{{scope}}` is the key. `{{epic}}` is retired and is an error naming
  `{{scope}}`, never a silent alias.
- An unknown token, an unclosed `{{`, or a role that is not in the manifest is
  an error naming the env and the role it belongs to — never the value, and
  never the token's text, which may itself be a fragment of a value.

## The envelopes

`env/harness.env` is what the shim exports to the harness container:
`MSBX_<ROLE>_HOST` and `MSBX_<ROLE>_PORT` for every published role (container
ports), every value listed under any `x-msbx.env` role, every declared secret's
value under its own name, and every name in the harness store. `<NAME>_FILE`
does not appear here: the container gets the value instead.

`env/host.env` is shell-safe `export NAME='value'` lines for a host shell: the
same names with the assigned loopback ports, `<NAME>_FILE` for every declared
secret pointing at its rendered file under the scope's `secrets/` (the host can
read it), and the harness store's values. `msbx env export` prints this file
verbatim and comes with a caveat: it prints values, so evaluate it, do not log
it.

```sh
msbx env ls --scope mir-42       # harness names only, one per line
msbx env export --scope mir-42   # env/host.env, the loopback-port exports
```

Both read what the last `up` rendered for one recorded scope and refuse when
there is none. `env` activity counts against the idle backstop.

## Invariants and limits

- A value appears in no checkout, no manifest, no `msbx` stdout or stderr, no
  log line `msbx` writes, and no Docker argv. It exists in the layer, the
  rendered envelope (0600) and the harness process environment by design.
- The harness store is excluded from every service envelope, but `env/host.env`
  and `msbx env export` print it, because both are the host user's own shell
  view. It is still never written to a checkout or a container argv.
- `docker inspect` exposes a container's environment. That is a stated limit,
  not a promise: the values are in the environment because the service needs
  them.
- The harness process receives the values as environment variables, so every
  child it starts inherits them — the same trust boundary as the agent itself.
- Changing a value means changing its layer and running `msbx up` again; `up`
  re-renders the envelope and Compose reconciles the affected containers.
- A tunnelled role declares `x-msbx.env` and `x-msbx.secrets` like any other
  role: they are injected into its generated tunnel sidecar, which is how the
  transport gets its credentials. The secret names and the transports are in
  [tunnels.md](tunnels.md).
