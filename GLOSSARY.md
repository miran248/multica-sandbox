# Glossary

Terms this repository uses in one specific sense.

- **Epic box** — the set of containers, volumes and host names that belong to
  one epic. Today it is a namespace of containers, not a virtual machine.
- **Namespace** — the flat suffix shared by an epic's container names
  (`signals-mir42-*`). It keeps epics from colliding.
- **Role** — what a service does in an epic (`pg`, `pg-analytics`, `web`). A
  second instance is a second role, never a second port.
- **Alias** — the dotted host name a container answers to
  (`pg.mir42.signals.msbx.test`). Aliases, not container names, carry the
  hierarchy.
- **Zone** — `msbx.test`, the only local DNS domain the tool maintains, served
  by Apple's embedded DNS.
- **Envelope** — `/run/msbx/env` and `/run/msbx/secrets/`: the rendered,
  per-epic environment inside a container. Materialized, never mounted through.
- **Driver** — the backend that creates and addresses containers. Apple
  `container` is the v1 target; the trait exists so the decision stays
  reversible.
- **Init** — `msbx init`: the first-run checks on a fresh host, one check per
  driver assumption ([MIR-179]). It prints the one privileged command
  (`container system dns create msbx.test`) instead of running it.
