# Runtime config seeding

`seed_runtime_config_if_missing` (`desktop/src-tauri/src/container/mod.rs`)
materialises a runtime root on first run from files packaged into the app
binary.

## The config is two files

`magician-config.yaml` cannot load without its sibling `llm-router.yaml`, which
holds `llm.router.profiles` and `operation_mapping`. The loader splices the
tables in and treats a missing sibling as fatal — there are no built-in
profiles, so tolerating absence would boot a router with nothing to route to.

So the desktop packages **both**:

| Constant | File seeded |
| --- | --- |
| `PACKAGED_MAGICIAN_CONFIG` | `magician-config.yaml` |
| `PACKAGED_ROUTER_TABLES` | `llm-router.yaml` |

## Seeded independently, on purpose

The tables are seeded whether or not the config itself was written this run.
A runtime root created before the tables were split out has a config and no
sibling; seeding only alongside a *new* config would leave that root unable to
boot after an upgrade. Each file is written only when absent, so an operator's
edits are never overwritten.

The same rule applies to the `make` seeding targets, which copy the repository
seed or the template into a runtime root.

Contract: [router tables](../magician/router-tables-file.md).
