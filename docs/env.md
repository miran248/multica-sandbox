# Environment and secrets

Status: delivered by [MIR-260](https://multica.ai/miran248/issues/MIR-260),
stage 4 of [MIR-178](https://multica.ai/miran248/issues/MIR-178). The manifest
lists names; `msbx` resolves the values outside every checkout, renders them per
epic, and injects them into the epic's services and the harness.

## The store

```
$XDG_CONFIG_HOME/msbx/projects/<project>/env             NAME=value lines, 0600
$XDG_CONFIG_HOME/msbx/projects/<project>/secrets/<NAME>  one value per file, 0600
```

`XDG_CONFIG_HOME` defaults to `~/.config`, so the store is
`~/.config/msbx/projects/<project>/`. It belongs to the project, not the epic: a
second epic of the same project reads the same values.

- `env` is one `NAME=value` per line. Blank lines and lines whose first
  non-blank character is `#` are skipped; the value is everything after the
  first `=`, verbatim. A malformed line, a duplicate name or a name that is not
  `[A-Za-z_][A-Za-z0-9_]*` is an error that quotes the line number, never its
  content.
- `secrets/<NAME>` holds the value and nothing else. One trailing newline is a
  file convention and is dropped; a newline anywhere else is an error.
- Names are identifiers because each one becomes an environment variable, a file
  name under `secrets/` and a mount path (`x-msbx.env` and `x-msbx.secrets` in
  the manifest use the same rule).

`msbx up` reads only the names the manifest lists for the epic. It resolves them
before it allocates a port block or starts a container: a listed name with no
value stops `up` and names the block, the role and the name. Unlisted store
entries are ignored, so a project can keep values for other stages of its work.

## Render

Every `up` rewrites the epic's envelope under
`$XDG_STATE_HOME/msbx/<project>/<epic>/` (default `~/.local/state/...`), outside
every checkout, directories 0700 and files 0600:

| Path | Content |
| --- | --- |
| `env/<role>.env` | the role's `x-msbx.env` names, rendered, one `NAME=value` per line |
| `env/harness.env` | the harness envelope, sorted by name |
| `secrets/<NAME>` | a rendered secret, one value and a trailing newline |
| `override.yaml` | the generated Compose override (see the spec) |

The override gives each role with env entries an `env_file` pointing at
`env/<role>.env`, mounts each of its secrets read-only at
`/run/msbx/secrets/<NAME>`, and adds `<NAME>_FILE` to the role's environment.
Values reach Compose through those files, never through argv. Files an earlier
render produced and this one does not are removed.

`harness` is reserved as an `x-msbx.env` role name: its file would be
`env/harness.env`, the harness envelope itself. A manifest that lists env for a
role named `harness` is refused.

## Tokens

A store value may carry `{{host:<role>}}`, `{{port:<role>}}`, `{{epic}}` and
`{{project}}`; `msbx` substitutes them per epic at render time. This is
host-side substitution, not Compose interpolation, so the result is literal.

- `{{host:<role>}}` is `<role>.<epic>.<project>.msbx.test`; the role may be any
  service or tunnelled role.
- `{{port:<role>}}` is the role's assigned loopback port and is an error for a
  role that is not in `x-msbx.publish`.
- An unknown token, an unclosed `{{`, or a role that is not in the manifest is
  an error naming the env and the role it belongs to — never the value, and
  never the token's text, which may itself be a fragment of a value.

## The harness envelope

`env/harness.env` is what the wrapper evaluates and exports to the harness:
`MSBX_<ROLE>_HOST` and `MSBX_<ROLE>_PORT` for every published role, every name
listed under any `x-msbx.env` role with its rendered value, and
`<NAME>_FILE` for every declared secret pointing at its rendered file under the
epic's `secrets/`. Roles are uppercased and `-` becomes `_` in
`MSBX_<ROLE>_*`. A declared name that collides with a generated one is an error.

`up` also passes the `MSBX_<ROLE>_HOST`/`_PORT` pair to the `docker compose`
process, so a project's Compose file may interpolate them into a service; store
values are never placed there.

```sh
msbx env ls --epic mir-42        # names only, one per line
msbx env export --epic mir-42    # export NAME='value' lines for the harness
```

Both read what the last `up` rendered for one recorded epic and refuse when
there is none. `export` is the only command that prints a value; it is meant to
be evaluated (`eval "$(msbx env export --epic E)"`), not logged.

## Invariants and limits

- A value appears in no checkout, no manifest, no `msbx` stdout or stderr, no
  log line `msbx` writes, and no Docker argv. It exists in the store, the
  rendered envelope (0600) and the harness process environment by design.
- `docker inspect` still exposes a container's environment. That is a stated
  limit, not a promise: the values are in the container's environment because
  the service needs them.
- The harness process receives the values as environment variables, so every
  child it starts inherits them — the same trust boundary as the agent itself.
- Changing a value means changing the store and running `msbx up` again; `up`
  re-renders the envelope and Compose reconciles the affected containers.
- A tunnelled role cannot declare `x-msbx.env` or `x-msbx.secrets` yet: its
  sidecar and its env injection land with
  [MIR-262](https://multica.ai/miran248/issues/MIR-262), and a manifest that
  declares them is refused rather than silently ignored.
