# Magician SKILL.md spec

We adopt **AgentSkills v1** (https://agentskills.io/specification) verbatim,
extending only `metadata.magician.*`. This is the schema and runtime contract; how-tos:
[`skills-authoring.md`](skills-authoring.md) (writing) and
[`skills-quickstart.md`](skills-quickstart.md) (installing third-party skills).

Packs publish as app content via `metadata.magician.app_publication`
([below](#skill-pack-publication-metadata)). `higgsfield` wraps a provider CLI through typed argv into an isolated scope
HOME; `brainstorm-facilitation` is the procedure skill used by Loom and Live
Thinking Map ([`skillshub/CHANGELOG.md`](../../../skillshub/CHANGELOG.md)).

Remote MCP supports explicit `auth.kind: none` / `requirement: none` contracts
without a bearer token or Auth Broker. They retain scoped audit, local catalog
policy, approval and resource checks; failed OAuth connections never fall back
to anonymous access. Endpoint choices remain reviewed manifest aliases.

`mcp_bindings.public_profile` declares a bounded literal public JSON document and
its native URL argument pointer. The runtime publishes an immutable copy under
its own public HTTPS origin and binds that URL before approvals; local-only
installs require remote-access setup. Only those content-addressed profiles are
public, with no environment interpolation or private file serving.

`mcp_bindings.tools` can declare native `commerce_quote` producers and
`commerce_checkout` consumers. Quotes are short-lived, single-use and bound to
scope, endpoint, credential session, policy, cart, selection and exact total;
host-owned atomic state and account file leases preserve this across worker
handoff and restart. Missing/expired state fails closed. Zero-price checkout
requires an explicit `server_enforces_zero_price` binding for a reviewed provider
contract that atomically enforces zero at placement. Payment continuation tools
do not initiate another purchase or reserve the amount again.

## Required frontmatter (per spec)

- `name`: 1-64 chars, lowercase + hyphens, must match the parent dir name.
- `description`: 1-1024 chars; what the skill does and when to use it.

## Optional standard frontmatter (we honour all of them)

- `license`: displayed in Forge.
- `compatibility`: free text, displayed verbatim, never parsed.
- `metadata`: free-form; our extensions go here.
- `allowed-tools`: space-separated tools/skills callable while active
  (experimental in the spec).

## Optional skill folder layout

| path | origin | contract |
| --- | --- | --- |
| `scripts/` | spec | each file is an ephemeral tool `<skill>__<script-stem>` while active; language from extension (`.sh`/`.bash`/`.py`/`.js`/`.mjs`) |
| `references/`, `assets/` | spec | on-demand context |
| `bin/` | Magician | pre-built binaries; every active skill's `bin/` is prepended to `PATH` |
| `config/` | Magician | optional setup/compatibility templates (credentials: below) |
| `tool_schema.yaml` | Magician, legacy | unmigrated external/scoped packages only; built-ins use the governed contract |
| `metadata.magician.runtime_contract`, `runtime_actions`, `runtime_catalog` | Magician | the governed contract in `SKILL.md` |

**`bin/` rules.**

- `agent-browser` resolves workspace-first
  (`<storage_root>/scopes/<principal>/<workspace>/skills/browser/bin/agent-browser`),
  then via `tool-runtime-config.yaml` extras roots
  (`<extra>/skills/browser/bin/agent-browser`); under a data root it is a
  relative link into `<data root>/system/skill-bin/`
  ([scope layer self-repair](#scope-layer-self-repair)), and lookup never
  canonicalizes into that store. It comes from the patched fork at
  `skillshub/browser/_vendor/v<version>/` (`make skills-install-scope`;
  `make setup-agent-browser` rebuilds), never `node_modules/.bin/agent-browser`,
  and finds `skills/` / `skill-data/` in the sibling pinned npm package;
  `make verify-agent-browser` (temp mirror and a materialized scope tree) and the
  container build check that (`skills get core --full`, `AGENT_BROWSER_SKILLS_DIR`
  blank); first boot materializes the default scope's browser skill before use.
- `bin/` adapters are the provider client (canonical JSON on bounded stdin); a
  native-CLI skill calls its CLI via typed actions. Absent optional /
  `at_least_one` secret bindings are omitted, never set empty. Governed dispatch
  runs only the exact declared, regular, non-symlink executable from the
  loader-owned skill directory.
- Skill code is shared and read-only: governed children get
  `PYTHONDONTWRITEBYTECODE=1` (`GOVERNED_FIXED_ENVIRONMENT`) so no `__pycache__`
  lands beside `bin/`; the app jail points `PYTHONPYCACHEPREFIX` at its private workdir instead.

**`config/` and credentials.** Credentials come from the scope/profile Auth
Broker, materialized only into the contract's exact bindings and declared child
environments, never model input. A private legacy `config/.env` may fill a
missing profile-free record (1 MiB cap, identity-revalidated); canonical denial
or unavailability never falls through to it.

**Output capture** accepts declared files in the admitted task tree or the
workspace's real `workdirs/` (governed `workspace_path` skills;
`images[].path` / `outputs[].path` become task artifacts), never private scope
state, other workspaces or symlink escapes.

**Legacy vs governed.** The loader validates the governed contract and compiles
its typed actions into the capability registry. A directory holds it or
`tool_schema.yaml`, never both (fail closed); neither makes a plain steering
skill. Baselines in `magician/tests/fixtures/tool_runtime_legacy_contracts/` are
never discovered or dispatched (Phase 7 migration ledger).

### Governed packages

Every package family (direct CLI, canonical-JSON adapter, CLI-owned session,
browser/native, QR/device, static-secret, official-SDK MCP) compiles from this
source; secrets materialize from the workspace vault only after route authorization.

- Protocol/artifact packages use `tool-runtime.typed-action-overrides.v2`,
  `input_delivery: canonical_json_stdin`: `runtime_actions` is the only
  parameter/default/type contract, raw stdin is removed from the model schema,
  defaults apply once (a bounded deterministic object is sent), and the executable must
  not redeclare flags (`argparse`/`sys.argv`). Native CLI packages (the six
  Google Workspace skills) keep v1 inert argv, no wrapper.
- A selectable-profile package may alias `profile` via
  `runtime_catalog.profile_parameter` (Calendar: `account`, with `business`,
  `personal`, `work`). Presentation only; the Auth Broker profile registry is
  authoritative. Not for fixed/implicit profiles; no action parameter may collide.
- `implementation.cwd` is interpolated per call (`cwd: '{working_dir}'`).

Template vars in `implementation.command` / `cwd` / `arg_mappings`:

| Var | Resolves to | Use case |
|---|---|---|
| `{scope_capabilities_root}` | `<scope>/` | scope-local files (bot env-files, gws auth, …) |
| `{scope_capability_auth_root}` | `<scope>/auth/` | OAuth tokens / credential dumps |
| `{scope_capability_workdir_root}` | `<scope>/workdirs/` | scratch dirs (e.g. WhatsApp `WU_HOME`) |
| `{skill_runtime_root}` | per call: `<scope>/skills/<skill>/` if scope-installed, else `<system>/skills/<skill>/` | sibling scripts, bundled binaries: `command: ['{skill_runtime_root}/bin/document-to-markdown']` |

Argument names (`{working_dir}`, `{prompt}`) substitute at spawn; context vars
resolve once at dispatcher construction.

Installed CLIs (gws, tgcli, wu, OfficeCLI, …) resolve through the reviewed-executable
authority; skill `bin/` entries only when declared (no ambient PATH of all skills' binaries).

## Magician extensions (`metadata.magician.*`)

**Presence determines the route**: `personality:` makes a personality mode;
otherwise a procedure.

### 1. `metadata.magician.personality` → personality-mode

```yaml
metadata:
  magician:
    personality:
      active_mode: witty
      voice: |
        ...
      expression_bias: ...
      suppression_rules: ...
      expression_triggers: ...
```

Not in the tool catalog: `name + description` feed the `switch_personality`
mode list; `switch_personality(mode="witty")` writes the content into the
`personality_profile` memory tier. **Personality is additive**, an overlay on
the base persona
(`magician_data_v3/system/agent_templates/agents/<name>/definition.agent.yaml`).
`lookup_personality_mode` keeps underscore-to-kebab aliases
(`default_personality: true_friend` → `name: true-friend`).

### 2. No block → procedure (default)

Every third-party skill (agentskills.io, OpenClaw, Hermes, git URLs) runs as a
procedure unchanged.

- No `scripts/` or `bin/`: a *plain* skill; activation injects the body as a
  steering message, registers no tools (outer loop only).
- With `scripts/` and/or `bin/`: a *toolskill*; activation also registers each
  script as `<skill>__<stem>` and prepends `bin/` to `PATH` (outer and inner loop).

### Optional `metadata.magician` fields shared across all kinds

```yaml
metadata:
  magician:
    requires:
      bins: ["agent-browser"]        # external binaries needed
      scripts:
        runtime: "python>=3.11"      # script runtime (informational)
      env: ["FOO_API_KEY"]           # env vars expected
      host_gateway: false           # requires Mac host automation when true
      cua: false                    # local/relayed CUA on any desktop OS when true
    install_hint:                    # surfaced in Forge if requires unmet
      brew: "..."
      cargo: "..."
      npm: "..."
      docs: "..."
```

`requires.cua` is independent of `requires.host_gateway` (Windows/Linux
CuaDriver needs no Mac automation); both true needs both. Probed at startup;
see [CUA setup](../scripts/cua-setup.md).

### `requires.bins` for a governed runtime contract

Inside `metadata.magician.runtime_contract.requires`, `bins` is a set (order
irrelevant) with two roles:

- **Entry point**: the process the runtime execs; bound as execution authority,
  copied into the private single-file snapshot and revalidated before launch.
  It must resolve or the call fails. When it resolves to a JavaScript launcher
  in an installed npm package (including through the skill's own `bin/` link),
  the package's native `node_modules/.bin_real/<name>` or platform binary is
  snapshotted instead, since a launcher copied alone loses the modules beside it.
- **PATH companions**: every other name. The cleared child `PATH` holds only
  the directories resolving this list; companions run in place, are never
  snapshotted or bound, and an optional one is no precondition for the skill.

**`entrypoint` is required once `bins` holds more than one name**; otherwise
`validate_requirements` (`manifest_validation.rs` in the external
`tool-runtime-core` crate) returns `ManifestValidationErrorCode::AmbiguousExecutable` and `load_pack_defs_from_skills_dir` **drops the pack**
(calls answer `unknown inner-loop pack`).

```yaml
    runtime_contract:
      requires:
        bins:
        - youtube-search      # the adapter this package ships
        - python3             # PATH companion: the interpreter
        - yt-dlp              # PATH companion: the search backend
        entrypoint: youtube-search
```

### `app_egress`: the hosts a keyed skill contacts

A keyed skill (`runtime_contract.auth.kind: secrets`) should declare the exact
HTTPS hosts it contacts: when an untrusted app runs it in the brokered-egress
jail, an owner-ticked key reaches only those hosts (otherwise the owner picks
per key at install). Put the block first under `metadata.magician`:

```yaml
metadata:
  magician:
    app_egress:
      schema_version: 1
      destination: api.example.com
      # or (never both): destinations: [api.example.com, uploads.example.com]
```

- One `destination` or ≤ 16 `destinations`; lowercase DNS names, HTTPS on 443
  only (no IPs, ports, schemes, paths, wildcards). Unknown fields or another
  `schema_version` refuse the skill. Full rules:
  [`app-os-jail-egress.md`](app-os-jail-egress.md).
- **Several hosts cost the key its domain lock**: the vault grant carries no
  domain, so a domain-restricted key is refused. List only needed hosts.
- **What to list**: API base URLs and CDNs, read from the `bin/` adapter and
  its CLI/SDK defaults (`skillshub/node_modules`, `skillshub/.venv`; the jail's
  clean env means library defaults apply, not operator overrides), or from the
  broker receipt's `contacted` list after one app run.
- **Dynamic hosts** (a configured instance origin, URLs from input) are
  left out; the owner picks at install.
- **`destinations: ["*"]`** (sole entry), for web fetchers: network only under
  the app's "any public host" grant (requested in the manifest, allowed by every
  ceiling); the key only with the owner's per-key "any site" tick.
- **Where a key can go**: declared hosts the app is granted; else the hosts the
  owner picks per (app, tool, key), or "any site". That choice is the vault
  scope and the only reachable hosts while the key is injected.

### `runtime_canary` cost ceilings

`expect.max_cost_microunits` requires `expect.max_cost_commodity` matching the
package's `content_source.output.cost.commodity` (exa `usd`, tavily
`tavily_credit`), since budgets are keyed by commodity.

Deep research workloads allow up to 2,100 seconds, while their canary probes
retain a separate 600-second limit. Adapters return partial findings or an
OpenAI `response_id` for later collection when a run stops early.

### Skill-pack publication metadata

```yaml
metadata:
  magician:
    app_publication:
      display_name: YouTube Search   # 1-320 bytes after trim; required
```

Parsed by `parse_skill_magician_extension` (no sidecar or second format).

- **Presence is the publication marker.** `display_name` is the only field
  (identity, description, version come from frontmatter); unknown fields are
  rejected (`deny_unknown_fields`).
- **Fail-closed at app-catalog snapshot** (`tool_skill_descriptor`,
  `magician_v2/apps/primitive_catalog.rs`): a malformed block quarantines the
  pack (`invalid_tool_skill`); agent discovery and dispatch are unaffected.
- **Publication grants nothing.** ToolSkill admission (typed USR contract and
  actions, no raw argv passthrough, no shell bins, `expose.apps` not denied)
  still gates an app manifest locking it in `dependencies.tools`.

A first-party wrapper may sit at `skillshub/<name>/app/` (discovery scans only
`<skills-root>/<name>/SKILL.md`, one level deep, and skips `skill_type: app`). Exemplar:
`skillshub/youtube-search/` + `skillshub/youtube-search/app/`
(`youtube-search-coverage`); see [`app-primitive-catalog.md`](app-primitive-catalog.md).

## Discovery & resolution

Per name, scope wins on collision:

1. `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<name>/`: the live workspace (canonical; defaults under `~/MagicianNotes`)
2. `$MAGICIAN_ROOT_DIR/system/skills/<name>/`: back-compat fallback (empty in normal operation; no system-shared tier)

Shadowing is whole-directory (no merging). A present higher-precedence
`SKILL.md` claims its name before parsing: malformed, oversized or unreadable,
that name fails closed (siblings load; no lower package is reactivated).

`skillshub/` is the git-of-record source;
`make -C skillshub install-scope SCOPE=<principal>/<workspace>` runs
`install_skill_layer.py`, symlinking each `skillshub/<name>/*` file (edits are
live). Runtime config lives at `<skills>/.skill-state/<name>/config` behind a
relative `config` link, so secrets and provider OAuth/session data survive
atomic rematerialization.

Root Makefile targets: `skills-list`, `skills-list-source`, `skills-validate`,
`skills-install-scope SCOPE=<principal>/<workspace>`, `skills-check-scopes`
(read-only), `skills-link-scopes` (offline repair of all scopes);
`scripts/magician-skills` wraps the same Makefile / `list_skills.py`.

### Scope layer self-repair

`CapabilityWorkspaceManager::materialize_scope` runs
`skills::scope_layer::reconcile_scope_skills_layer` once per scope per process
(boot via `ensure_seeded_for_existing_scopes`, else first touch; callers wait
for that first reconcile); a change
invalidates the scope's pack-catalog cache and the CLI pack-dir index. Against
`<repo or package root>/skillshub` it:

- installs missing skills (`install_skill_layer.py` layout, per-skill `flock` in
  `<skills>/.skill-install-locks/`) unless marked removed: uninstall (UI/API or
  `uninstall_skill_layer.py`) writes `<skills>/.skill-state/<name>/removed`, any
  install clears it;
- re-points dangling absolute links at the running skillshub and links newly
  added source files;
- copies pinned native executables to `<data root>/system/skill-bin/<sha256>/`,
  linking `bin/<name>` relatively (`skillshub/scripts/pinned_bin_store.py`), so
  a cleaned checkout keeps agent-browser, OfficeCLI and
  document-to-markdown.

Offline, never builds; a link with no source left logs a warning naming
`make skills-check-scopes` / `make skills-link-scopes`. Scope-owned skills
(regular `SKILL.md`) are never touched; skipped when `MAGICIAN_SKILLSHUB_ROOT`
is set (mounted host trees resolve through `path_rewrite`).

A skill with `render.yaml` gets a setup-rendered per-scope manifest
(`skillshub/scripts/skill_render.py`, from the scope's `config/.env`): the
reconciler never links its placeholder `SKILL.md` or `render.yaml`, leaves a
`SKILL.md` marked `.skill-render.json` as setup wrote it (other files still
installed and topped up), and a runtime-created
workspace gets it on the next `setup-env` or `link-scopes`
([per-scope manifests](../../../skillshub/README.md#per-scope-manifests-rendered-not-linked)).

`AgentBrowserSession::resolve_cli_path_for_scope` reports a dangling link with
the fix (`make setup-agent-browser`, which rebuilds the fork and relinks every
scope). Tests: `make test-scope-skill-layer` (new scope, cleaned/moved checkout,
moved data root, scope-owned copy).

## Per-agent allowlist

`tools:` in the agent's `definition.agent.yaml` selects, by exact name, which
procedure skills are in its catalog and which personality modes
`switch_personality` accepts; unlisted skills stay loaded for other agents but
invisible to this one. **Default policy is deny** (missing or empty: no skills).

`activate_skill` sets the session's single `active_procedure_skill` slot
(replace-on-set, idempotent), re-injecting the body every turn as
`## Active Playbook: <name>`; `deactivate_skill` clears it.

## Governed child environment

Governed children get only declared context, exact credential bindings,
reviewed executable discovery and minimal platform variables, never a vault or
ledger bearer. CLI-owned profiles may get a validated `HOME`; protocol adapters
cannot reach home credentials. Cookies, OS permissions and SDK OAuth sessions
stay with their declared owner.

`SSL_CERT_FILE` comes from a runtime-owned CA-bundle list (never inherited; OS
stores before package-manager prefixes; first readable wins). Hermetic contracts
needing TLS opt into the portable baseline; no readable bundle falls back to the
interpreter default with one warning.

Closed skills get no `MAGICIAN_ROOT_DIR`, `MAGICIAN_STORAGE_PATH` or store
credentials; they work in a versioned `SubprocessStorageEnvelope` under
`workdirs/skill-working/{call_id}/` (read-only inputs; outputs count only once
published through an owning object repository; see
[storage abstraction](storage-abstraction.md)). Operator batches needing the
live root import `skillshub/scripts/runtime_root_shim.py`; other direct env
resolution fails `scripts/skillshub_runtime_root_linter.py` (docs-guard gate).

## Validation

`skillshub/scripts/validate_skill_md.py` checks frontmatter, `name` rules, the
directory match and `description` ≤ 1024. `metadata.magician.content_source` /
`content_reader` / `observe_source` need schema version 1, their mappings and
the governed action catalog in the same `SKILL.md` (Rust validates
deny-unknown); `content_reader` also bounds HTTP/cache/quality and needs a local
deterministic extraction action; `observe_source` has no executable authority.
Sibling `content_source.yaml` / `content_reader.yaml` / `observe_source.yaml`
files are rejected.

Run before installing: `make -C skillshub validate` (or `make skills-validate`).

## Catalog sources

Capability-pack definitions, earlier wins (per scope via
`CapabilityWorkspaceManager::load_pack_defs_for_scope`):

1. **`<storage_root>/scopes/<principal>/<workspace>/skills/<skill>/SKILL.md`**:
   every built-in externalizable capability (awk, jq, gmail, csvkit, browser,
   …); `compiled_providers::load_pack_defs_from_skills_dir` compiles the
   governed package, else legacy `tool_schema.yaml`. Description and guide
   always come from `SKILL.md`.
2. **Embedded compiled pack defs** (`include_str!`) for in-process Rust
   providers (`create_agent`, `treasurer`, `list_agents`, `notify_owner`,
   `task_state`, `read_trace`, …), from
   `magician/src/magician_v2/execution/embedded_pack_defs/<name>.yaml` via
   `compiled_providers::embedded_compiled_pack_source_table` (parsed by
   `embedded_compiled_pack_defs`); register new compiled providers there. A
   malformed embedded YAML panics (a build error).
3. **`<storage_root>/system/capability_templates/packs/<name>.yaml`**: **removed**.

### HTTP catalog and install

- `POST /api/magician/v2/skills/install` (admin, `X-Magician-Setup-Token`):
  `{ "source": "path:/abs/path | skillshub:<name>", "target":
  {"workspaces": ["principal/workspace"]} }`. `target.workspaces` needs one
  scope or more; `target.system` is retired. SkillLoader validates first (bad
  frontmatter, name-dir mismatch, missing `SKILL.md` → 422); `SkillLoader::discover`
  caches parses by path, size and mtime. `skillshub:<name>` installs linked
  (`scope_layer::install_linked_skill`, atomic no-gap exchange under a process
  lock; replacing a copy moves its real `config/` files into
  `.skill-state/<name>/config` and keeps `auth/`); `path:<abs>` copies
  (scope-owned). Both clear the removal mark; provider state is outside both trees.
- `GET /api/magician/v2/skills`: installed skills plus compiled built-ins.
  SKILL.md rows: `kind` ∈ {`procedure`, `personality-mode`}, `layer` ∈
  {`system`, `workspace`}; compiled: `kind: "compiled"`, `layer: "built-in"`.
  Workspace shadows system; SKILL.md shadows built-ins. Read-only; workspace
  from the verified bearer (or local `anonymous/default` open mode).
- Forge `/skills` calls `POST /skills/install`, `/skills/{name}/uninstall` and
  `/skills/{name}/allow-for-agent` (line-based YAML edit of the agent's `tools:`).
  `/skills/{name}/promote` and `/skills/{name}/demote` are not registered.

## Dispatch model

Every outer-loop capability call goes through the inner loop (no
procedure-skill single-shot fast path):
`ExecutableAction::Pack` → `dispatch_flat_action` /
`primitive_dispatch::dispatch::dispatch_primitive`, which routes:

1. **`browser`** (a catalog skill, `skillshub/browser/SKILL.md`) → bespoke
   `BrowserDispatcher` (`BROWSER_PACK_NAME`): `agent-browser` session lifetime,
   owned-tabs header, Yutori N1.5 translation.
2. **Provider-backed inner loop** (`read_trace`, `duckdb`, `files`, `http`) →
   `dispatch_compiled_provider_primitive`; pack YAML declares
   `Primitive { provider_name: Some(_), .. }`.
3. **Governed runtime package** (all built-in tool skills): typed input lowers
   to inert argv, canonical JSON stdin or an official-SDK MCP call through the
   coordinator; authorization precedes credential access, and authority,
   cancellation, bounds, redaction, settlement and audit are shared.

**Compiled providers** (`implementation.type: compiled`: `create_agent`,
`treasurer`, `list_agents`, `notify_owner`, …) use the pack-runtime path.

### Deterministic application reuse

`ScopedDeterministicCapabilityInvoker` gives services that already know the
capability and arguments (user feeds, recurring monitors) the same primitive
subprocess path (scoped env, sandbox, timeout, spend gate, learning record)
without an LLM turn.

- `metadata.magician.content_source`: content discovery with no
  provider-specific Rust; the action is validated against the governed contract
  and retrieval appends it to its declared rung (central config only reorders).
- `metadata.magician.content_reader`: local full-content extraction. Rust does
  bounded HTTP, scoped caching and the scratch file; the action cannot skip
  network validation, change identity/privacy/provenance, or send content to a
  remote extractor.

Output modes: `mapped` (RFC 6901 JSON pointers) and `canonical_v1`
(provider-neutral items; Rust keeps policy, scope, bounds, identity, privacy,
canonicalization, provenance, cost and cursor). Example: `semantic-websearch-via-exa/SKILL.md`.

## Management surface (web + API)

The `/skills` page is backed by:

- `GET /api/magician/v2/skills/catalog`: every skill (skillshub, extras roots,
  `path:` installs, embedded compiled packs) with taxonomy (`tool` /
  `procedure` / `personality` / `compiled`) and `installed_scopes` (under
  `scopes/<p>/<w>/skills/`). A folder the
  loader refuses (malformed frontmatter, name/folder mismatch, oversized, or a
  governed package failing `compiled_providers::governed_runtime_package_rejection`,
  same check and reason as the loader) is a `kind: "invalid"` row with the reason in `invalid`, never omitted; app
  packages are left out. `copied_scopes` lists plain copies of a skillshub skill
  (no repair, no live edits), badged "copy" with Re-link.
- `POST /api/magician/v2/skills/install`: [above](#http-catalog-and-install).
- `POST /api/magician/v2/skills/{name}/uninstall` (setup token): keeps
  `config/`, `auth/` and `.skill-state/` (an inert skeleton the loader skips) so reinstall
  restores credentials; `purge: true` deletes `<skills>/.skill-state/<name>`
  after a preview→confirm round-trip. Both write the removal mark, so boot
  repair does not reinstall it.
- `GET|PUT /api/magician/v2/runtime/env` (setup token for writes): runtime
  `.env` / `.env.development` values, write-only; lockout-risk keys need an
  explicit acknowledge.
- `GET|POST /api/magician/v2/skills/catalog/{name}/env` (setup token for
  writes): a scope's per-skill `config/.env`; key status from `requires.env`
  plus `config/.env.example`; values write-only.

`path:` and `skillshub:` are the only install sources.
