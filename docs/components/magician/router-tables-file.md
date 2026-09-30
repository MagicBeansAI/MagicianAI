# Router tables live beside the config, not inside it

`llm.router.profiles` and `llm.router.operation_mapping` are stored in
`llm-router.yaml`, a sibling of `magician-config.yaml`, and spliced in when the
config is loaded.

They are about two-thirds of the combined text and the parts edited most often,
so they live in their own file.

## The pair travels together

A config and its tables are one document that happens to live in two files.
Anything that copies, packages, or seeds one must do the same for the other:

| Surface | File |
| --- | --- |
| Runtime root | `$MAGICIAN_ROOT_DIR/llm-router.yaml` |
| Repository seed | `llm-router.yaml` |

There is no separate template pair: packaging and the Dockerfile ship the
repo-root pair.

## Why the join is textual

Fifteen profiles alias `*local_generation_model`, whose anchor is defined in
`runtime:` of the main config. **YAML anchors do not cross documents**, so
merging two parsed documents cannot resolve them. `splice_router_tables_into`
joins the *text* before any parse, which keeps it one document and requires no
change to either file.

The tables are resolved as a sibling of whichever config is being loaded, never
through an independent search order, so a runtime-root config can never pair
with the repository's tables.

## Absence is fatal, inline is accepted

`llm_pricing.json` fails open because it overlays a built-in rate table. There
are **no built-in profiles**, and `profiles` carries `#[serde(default)]` — so a
missing tables file would parse silently into an empty router and surface much
later as per-operation failures. It is therefore a hard error.

The one exception: a config that already carries a table inline is taken as
complete. That covers a spliced document written to disk (what test fixtures
seed into temp directories), a pre-split config, and a deliberately
self-contained one. Either `profiles` or `operation_mapping` counts.

## Reading the config from code

**Never read `magician-config.yaml` directly** if you care about routing. The
file alone has no profiles and no operation mapping, and because both default to
empty, that parses cleanly rather than failing — the failure appears far away,
as an empty router.

| Language | Use |
| --- | --- |
| Rust, runtime | `load_magician_config_from_path` (splices) |
| Rust, tests/packaging | `shipped_repo_config_yaml()` / `shipped_template_config_yaml()` |
| Python | `scripts/magician_config_text.py` → `read_config_text(path)` |
| Shell / Ruby | `python3 scripts/magician_config_text.py <config>` → stdout |

The Python reader mirrors the Rust splice deliberately rather than
reimplementing a merge. It differs in one respect: it drops the tables file's
column-0 comment header. That header is valid YAML anywhere and `serde_yaml`
ignores it, but several scripts parse this config by indentation rather than
with a YAML library, and a column-0 line inside a nested mapping reads to them
as a dedent out of `llm.router`. Comments documenting individual entries are
indented with them and are preserved.

## Writers must not put the tables back

`profiles` and `operation_mapping` are `#[serde(skip_serializing)]`.

Two settings writers — `privacy_processing_settings` and
`workspace_storage_settings` — save by deserializing the whole config, replacing
one section, and re-serializing. Without the skip, the first such save would
write the spliced tables back into `magician-config.yaml` and undo the split,
leaving two diverging copies. Skipping means no config writer can do that,
whether or not it remembers to.

Editing the tables themselves is a text edit on `llm-router.yaml`. A
re-serialize would drop its comments and resolve `*local_generation_model` into
fifteen literals, silently unlinking them from `runtime.selected`.

## Related

- [Chat profile routing](chat-profile-routing.md) — how a profile is chosen
- [LLM routing overrides](llm-routing-overrides.md) — per-operation overrides,
  which live in the install-level store and never in the config
