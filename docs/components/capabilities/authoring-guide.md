# Capability Pack Authoring Guide

> **Audience:** humans and AI agents adding a callable capability.
>
> **Prerequisites:** none — this guide is self-contained. Catalog of shipped
> packs: [`./README.md`](./README.md). Formal `SKILL.md` schema:
> [`../magician/skills-spec.md`](../magician/skills-spec.md). Practical
> skill walkthrough (plain / personality / ephemeral scripts):
> [`../magician/skills-authoring.md`](../magician/skills-authoring.md).
>
> **Conventions:** `MUST`, `SHOULD`, `MAY`. Code blocks are copy-pasteable.

## Quick TL;DR

A capability pack is the registry record the LLM can call. There are two
authoring lanes:

| Lane | Source of truth | When to pick |
|---|---|---|
| Governed skill | `skillshub/<name>/SKILL.md` with `metadata.magician.runtime_contract` | Default. CLI, HTTP adapter, MCP, browser, GWS, csvkit, media, … |
| Compiled pack | YAML under `magician/src/magician_v2/execution/embedded_pack_defs/` plus a Rust `CapabilityProvider` | Built-in magician tools (`create_agent`, `task_state`, `files`, …) |

Do **not** drop YAML under `capability_templates/packs/` or
`<scope>/capabilities/packs/`; the runtime never loads them.

Fork `skillshub/jq/SKILL.md` (single CLI action, no auth) or
`skillshub/arxiv-search/SKILL.md` (canonical-JSON stdin). Place the
file at `skillshub/<name>/SKILL.md` (`name:` MUST match the directory),
`make -C skillshub validate && make -C skillshub install-scope SCOPE=anonymous/default NAMES=<name>`,
restart magician, then allow-list the name on the agent (`tools:` or
`POST /api/magician/v2/skills/<name>/allow-for-agent`). Trusted agents
with `tools: []` already see every registered tool. There is no pack
file-watcher and no `/capabilities/reload`.

## File layout

Loader: `ArtifactV2Capabilities::load_pack_defs_for_scope`. Earlier wins
on name collision.

| Priority | Location | What it is |
|---|---|---|
| 1 | `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<name>/` | Live scope copy. `make -C skillshub install-scope` symlinks here from `skillshub/`. |
| 2 | Each `tool-runtime-config.yaml` extra root's `skills/` (`extra_skills_dirs()`) | Operator overlay. Skipped when a same-named scope skill already loaded. |
| 3 | `embedded_compiled_pack_defs()` | YAML compiled into the magician binary. |

Each skill directory MUST contain **exactly one** executable catalog
source:

- Governed package: `SKILL.md` containing `runtime_contract:` /
  `runtime_actions:` (`parse_skill_runtime_package`).
- Legacy: `tool_schema.yaml` as a `CapabilityPackDefinition`. Do not
  author new ones. Fixtures live under
  `magician/tests/fixtures/tool_runtime_legacy_contracts/`.

A directory with **both** is skipped (fail-closed). A `SKILL.md` with
neither is a plain steering skill, not a pack.

`load_pack_defs_from_skills_dir` fills `description` from SKILL.md
frontmatter and `guide` from the markdown body (leading `# Heading`
stripped). Compiled YAML carries both fields itself.

## Choosing an `implementation:` type

`ImplementationType` (`capability.rs`) is a tagged enum
(`rename_all = "snake_case"`). Authors of governed skills never write
this block — `project_runtime_package_to_pack` always emits
`type: primitive` with a `GovernedRuntimeImplementation`.

| `type` | Who writes it | Live use |
|---|---|---|
| `primitive` | Projection of a governed `SKILL.md` (or leftover `tool_schema.yaml`) | Every governed tool skill. Projection sets `provider_name: None` and dispatches through the governed coordinator. |
| `compiled` | Embedded YAML | Built-in providers. `provider_name` MUST be a row in `COMPILED_PROVIDERS`. |
| `composite` | Enum still deserializes `{ steps: [{ tool, parameters }] }` | No shipped pack. Do not add new ones. |
| `command` | Enum still deserializes `{ program, fixed_args, arg_mappings, … }` | No shipped pack. Direct `Command::new` (no shell). Do not add new ones. |

There is no `inner_loop` or `java_script` variant. `type: inner_loop`
does not deserialize.

The flat loop (`docs/components/magician/execution/FLAT_LOOP.md`)
exposes each primitive as a leaf `<pack>__<action>`. Pack-level
`to_tool_definition()` hides parameters for most `primitive` packs
(browser is the session-parameter exception).

External CLI / adapter / MCP → governed `SKILL.md`. Magician-internal
Rust tool → `compiled` + provider.

## Governed skill (`SKILL.md`)

AgentSkills frontmatter plus Magician extensions under
`metadata.magician.*`. `name` is 1–64 chars, lowercase + hyphens, and
MUST equal the parent directory.

### `runtime_contract` (`tool-runtime.skill-runtime.v1`)

Required. `deny_unknown_fields`. Shape:

```yaml
runtime_contract:
  schema_version: tool-runtime.skill-runtime.v1
  requires:
    bins: [jq]            # reviewed executables
    entrypoint: jq        # required when bins has more than one name
  runtime:
    protocol: cli         # or mcp (stdio / streamable_http)
    command_prefix: []
    interaction: batch    # or pty
    stdin: {mode: denied, sensitivity: public}  # denied | optional | required
    working_directory: {mode: workspace}        # denied | workspace | output_root
    limits: {timeout_secs: 30, stdout_bytes: 10485760, stderr_bytes: 2097152}
  auth:
    kind: none            # none | secrets | cli_profile | browser_profile | …
    requirement: none     # none | required | at_least_one | conditional
  policy_floor:
    approval: ordinary
    resource_scopes: [workspace]
```

Pick the executable boundary before writing YAML:

- Existing CLI → reviewed binary + v1/v2 argv mappings. No skill-owned
  shell wrapper.
- HTTP-only provider → bounded client +
  `tool-runtime.typed-action-overrides.v2` with
  `input_delivery: canonical_json_stdin`.
- MCP → `protocol: mcp`; official SDK owns discovery/session.
- Browser / native → specialized controller; credentials stay in
  browser/OS authority.

### `runtime_actions`

Required for CLI packages. Schema
`tool-runtime.typed-action-overrides.v1` (inert argv, GWS-style) or
`.v2` (per-action `executable`, `input_delivery: canonical_json_stdin`).

Each action becomes one `native_action_schemas` entry after projection
(`skip_tool_name: true`, `argv` = `fixed_args`). Mapping types:
`positional`, `flag`, `bool_flag`, `repeated_flag`, `passthrough`,
`json_flag`, `literal` (trusted tokens), `split_positional`,
`runtime_control` (session knobs, not argv). Action-level `fixed_args`
prepend the CLI prefix.

Reserved runtime parameter names: `profile`, `stdin`, `working_dir`,
`timeout_secs`. Do not collide.

Single-action packages re-project that action's parameters at pack
level. Multi-action packages expose a pack-level `command` string plus
per-action leaves.

### `runtime_canary` (`tool-runtime.canary.v1`)

Every governed tool skill MUST declare either a live probe or an
exemption. `make test-tool-skills-live` runs the probe through the
production coordinator.

```yaml
runtime_canary:
  schema_version: tool-runtime.canary.v1
  cost_tier: free          # or cheap / expensive
  action: run
  input: { query: "transformer architecture", limit: 3 }
  fixtures: {}             # workspace-relative files materialized before the call
  expect:
    stdout_contains: "usage"   # or min_items + items_pointer + error_pointer
    max_latency_ms: 30000
# or: exempt: { reason: "read-only actions still require live operator OAuth" }
```

### `runtime_catalog`

Projected onto `ExecutionMetadata`: `categories`,
`composition_category`, `expose_timeout_control` /
`timeout_default_secs` (capped by the contract ceiling),
`profile_parameter` (e.g. Gmail `account`), `spend` (`committed` /
`metered` / `counted`), `chat_inline_adapter` (exact known value
`tutor_screen_draw` only).

### `auth:` lifecycle

Governed packages declare auth on the **contract**, not a pack-level
`auth:` block. Projection (`project_compatibility_auth`) fills
`CapabilityAuthConfig` (`required`, `setup_command`, `check_command`,
`reauth_command`, `error_patterns`) so `ensure_auth` /
`maybe_reauth_and_retry` still run. Google Workspace packs get a fixed
error-pattern list.

Do not put OAuth client secrets in SKILL.md. Bindings go through
`auth.secret_bindings` / `injections` and the scope vault.
`install-scope` is the only supported way to materialize a skill into
`<scope>/skills/`.

## Compiled packs

Use this lane only when the work is a Rust provider inside magician.

1. Implement `CapabilityProvider` (or a `GenericCompiledProvider`
   handler).
2. Add `("<provider_name>", deferred)` to `COMPILED_PROVIDERS` in
   `compiled_providers.rs`. `deferred = true` for handler-registered
   tools that bind after `set_compiled_handlers`.
3. Write `magician/src/magician_v2/execution/embedded_pack_defs/<name>.yaml`.
4. Add `include_str!(...)` to `embedded_compiled_pack_source_table()`.
5. Wire the provider in `build_compiled_registry`.
6. Rebuild magician. A compiled pack whose `provider_name` is missing
   from `COMPILED_PROVIDERS` is pruned at boot.

Shape: see `embedded_pack_defs/system_status.yaml`. `name` SHOULD match
`provider_name` and the file stem.

### Top-level YAML field reference

`CapabilityPackDefinition` keys: required `name`, `implementation`;
optional `description`, `version`, `guide`, `parameters`,
`native_action_schemas`, `execution`, `auth`, `reliability`,
`result_projection`. Unknown keys are ignored (no
`deny_unknown_fields`). `status` / `replaced_by` / `retire_after` are
not on the struct.

`yaml_parsing_test.rs` still has a `PACK_KEYS` allowlist for leftover
YAML under the decommissioned `capability_templates` / scope
`capabilities` roots, and loads every `skillshub/*/SKILL.md` through
`load_pack_defs_from_skills_dir`.

## Parameter authoring

Applies to compiled YAML `parameters:` and to leftover
`tool_schema.yaml`. Governed skills declare parameters on
`runtime_actions` instead; the projector builds `ParameterDef`s.

After deserialize, `ParameterDef.schema` is always populated and is
what both catalogs emit.

| If the parameter is… | Use |
|---|---|
| Plain string / integer / number / boolean | `param_type: string` (etc.) |
| String from a fixed set | `param_type: string` + `enum_values: [...]` |
| Arrays, objects, pattern, format, oneOf, ranges | `schema:` JSON Schema block |

```yaml
- name: query
  required: true
  param_type: string
  description: "Search query."
  aliases: [q]
- name: evidence_refs
  required: true
  schema: { type: array, items: { type: string }, minItems: 1 }
```

`default:` is a YAML string. If both shorthand and `schema:` are
present, `schema:` wins.

## `description:` vs `guide:`

| Field | Audience | Source (skill) | Source (compiled) |
|---|---|---|---|
| `description` | Outer catalog every turn. Tight: what + when + not-for. | SKILL.md frontmatter (`description`, 1–1024 chars) | YAML `description` |
| `guide` | Dispatched usage. Capability, output, limits, examples. | SKILL.md body, leading `# Heading` stripped | YAML `guide:` |

Description template:

> "<verb-first action>. Pick when <intent>. Not for <neighbor>."

Shipped example (`skillshub/image-generation/SKILL.md`): generate/edit
still images via Gemini; not video; not Imgflip memes.

## `execution:` metadata

Compiled YAML (skills use `runtime_catalog` instead). Fields:
`requires_browser_session`, `default_timeout_secs`, `categories`,
`composition_category`, `sandbox` (`shell` / `file` / `none`),
`spend`, optional `chat_inline_adapter: tutor_screen_draw`.

`spend.type`: `committed` (cost from a parameter), `metered`
(estimate + optional cap), `counted` (flat per call). `commodity` is a
ledger name. Lowering wraps the action in a spend gate when present.

## Multi-action packs

Governed: list actions under `runtime_actions.actions`. Each key is the
leaf name (`gmail__triage`).

Compiled / leftover YAML may still declare `native_action_schemas:`
(description, `parameters`, `required`, `parameter_overrides`,
`arg_mappings`, `argv`, `suffix_args`, `timeout_secs`,
`skip_tool_name`). Per-action `parameter_overrides` win over pack-level
`parameters`. `skip_tool_name: true` omits the primitive name from
argv. Leaf `reliability` does not inherit the pack.

## Hot reload

There is no hot reload and no `POST /api/magician/v2/capabilities/reload`.
Skill YAML, SKILL.md, and
legacy `tool_schema.yaml` changes take effect on **magician restart**.
Compiled Rust + embedded YAML also require rebuild + restart.

Catalog read: `GET /api/magician/v2/skills` (scope skills plus
`kind: compiled`, `layer: built-in`). There is no
`/api/magician/v2/capabilities/packs` route.

## Naming conventions

Skills (AgentSkills slugs):

- Kebab-case directory and `name:` (`image-generation`,
  `news-search-via-tavily`). Underscore aliases are not kept.
- Capability-focused, not vendor-branded. `_via_<vendor>` for API
  wrappers; `_with_<model>` for in-the-loop model wrappers.

Compiled packs keep the historical snake_case tool names
(`create_agent`, `system_status`). Match the embedded file stem.

## Local testing checklist

1. `python3 skillshub/scripts/validate_skill_md.py skillshub/<name>/` and `make -C skillshub validate`.
2. `make test-capabilities` (loads skillshub via `load_pack_defs_from_skills_dir`).
3. `make test-tool-skills` (hermetic; no provider calls).
4. Compiled: `make check-all` plus a provider unit test.
5. `make -C skillshub install-scope SCOPE=anonymous/default NAMES=<name>`, restart magician.
6. `GET /api/magician/v2/skills` — name + description; compiled packs are `kind: compiled`.
7. Agent `tools:` contains the name (unless trusted `tools: []`).
8. Smoke via chat or `POST /api/magician/v2/skills/{name}/run` (debug path, not agent dispatch).

## Common mistakes

| Symptom | Cause | Fix |
|---|---|---|
| Pack never appears | Edited `capability_templates/packs` or `<scope>/capabilities/packs` | Author `skillshub/<name>/SKILL.md` (or an embedded compiled YAML) and `install-scope` |
| Skill skipped at load | Both `runtime_contract` and `tool_schema.yaml` present | Keep one catalog source |
| `unknown inner-loop pack` | Contract failed validation (often `bins` > 1 without `entrypoint`) | Fix `runtime_contract.requires` |
| LLM calls `{}` | Empty `properties` in the emitted schema | Declare `runtime_actions` parameters (or compiled `parameters` / `schema:`) |
| Compiled pack pruned every boot | `provider_name` not in `COMPILED_PROVIDERS` | Add the row and wire `build_compiled_registry` |
| Changes ignored | Expected hot reload | Restart magician; rebuild for compiled Rust |
| Agent never calls it | Name missing from `tools:` | Allow-list the kebab/snake name as registered |
| YAML `type: inner_loop` / `java_script` | Removed variants | Governed skill → projected `primitive`; JS → compiled or CLI |

## Worked examples

| Pattern | Reference |
|---|---|
| Compiled, scalar params | `magician/src/magician_v2/execution/embedded_pack_defs/system_status.yaml` |
| Compiled, array + free-form object | `…/embedded_pack_defs/create_agent.yaml` |
| Compiled, `task_state` | `…/embedded_pack_defs/task_state.yaml` |
| CLI argv skill (v2 mappings) | `skillshub/jq/SKILL.md`, `skillshub/csvkit/SKILL.md` |
| Canonical-JSON stdin adapter | `skillshub/arxiv-search/SKILL.md`, `skillshub/image-generation/SKILL.md` |
| CLI profile auth (GWS) | `skillshub/gmail/SKILL.md` |
| Browser / native | `skillshub/browser/SKILL.md` |
| `description` + body-as-guide | `skillshub/image-generation/SKILL.md` |

Fork the closest shipped skill. Keep `runtime_contract` / canary /
catalog patterns; change `name`, `description`, body, and actions.

## Agent authoring checklist

1. Unused kebab-case `skillshub/<name>/` (or unused `COMPILED_PROVIDERS` snake_case name).
2. Lane: governed skill vs compiled provider.
3. `description` (what + when + not-for) and body / `guide`.
4. Actions + parameters (`runtime_actions` or YAML `parameters` / `schema:`).
5. Skills: `runtime_canary` (probe or exemption); `auth` on the contract iff needed.
6. Skills: validate + `install-scope`. Compiled: YAML + `COMPILED_PROVIDERS` + provider + rebuild.
7. Restart magician. Confirm `GET /api/magician/v2/skills`. Allow-list `tools:`.
8. Smoke-test. One-liner in `docs/components/capabilities/README.md`.

## Out of scope for this guide

- **SKILL.md formal schema** — [`../magician/skills-spec.md`](../magician/skills-spec.md).
- **Plain / personality / ephemeral-script skills** — [`../magician/skills-authoring.md`](../magician/skills-authoring.md).
- **Registry internals** — `magician/src/magician_v2/execution/capability.rs`,
  [`../runtime-core/runtime-core.md`](../runtime-core/runtime-core.md).
- **Flat-loop dispatch** — [`../magician/execution/FLAT_LOOP.md`](../magician/execution/FLAT_LOOP.md).
- **AI CLI delegation (retired)** —
  `../../archive/components/capabilities/ai-cli-delegation.md`.
  Agent coding uses `run_coding_task`.
