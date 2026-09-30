# Magician SKILL.md spec

We adopt **AgentSkills v1** (https://agentskills.io/specification) verbatim.
Our extensions live exclusively in `metadata.magician.*` per the spec's
free-form metadata convention.

**Authors**: practical walkthrough at
[`skills-authoring.md`](skills-authoring.md). For installing a
third-party skill into a magician deployment, see
[`skills-quickstart.md`](skills-quickstart.md). This document is the
formal schema + runtime contract; the other two are how-tos.

Skillshub packs can be published as app-platform content via
`metadata.magician.app_publication` (exemplar: `youtube-search` +
`youtube-search/app/`). See
[Skill-pack publication metadata](#skill-pack-publication-metadata-plan-24).
`higgsfield` wraps a provider CLI through typed argv into an isolated
scope HOME. `brainstorm-facilitation` is the procedure skill used by Loom
and Live Thinking Map ([`skillshub/CHANGELOG.md`](../../../skillshub/CHANGELOG.md)).

## Required frontmatter (per spec)

- `name` — 1-64 chars, lowercase + hyphens, must match parent dir name.
- `description` — 1-1024 chars. What the skill does + when to use it.

## Optional standard frontmatter (we honour all of them)

- `license` — display in Forge.
- `compatibility` — free-text. Display verbatim. Do not parse.
- `metadata` — free-form. Our extensions go here.
- `allowed-tools` — space-separated; scopes which other tools/skills the
  LLM may call while this skill is active (experimental in spec).

## Optional skill folder layout

Per spec:
- `scripts/` — executable code. Each becomes an ephemeral tool when this
  skill is active. Tool name = `<skill>__<script-stem>`. Languages
  inferred from extension (`.sh`/`.bash`/`.py`/`.js`/`.mjs`).
- `references/`, `assets/` — on-demand context.

Magician extensions to the layout (NOT in the spec, but treated as
recognized dirs/files by our loader):
- `bin/` — pre-built binaries. The loader prepends every active skill's
  `bin/` to `PATH` so one skill's script can call another skill's bin.
  The browser dispatcher resolves `agent-browser` workspace-first
  (`<storage_root>/scopes/<principal>/<workspace>/skills/browser/bin/agent-browser`),
  then through `tool-runtime-config.yaml` extras roots
  (`<extra>/skills/browser/bin/agent-browser`). `make skills-install-scope`
  populates it from the patched fork at `skillshub/browser/_vendor/v<version>/`;
  gitignored native artifacts are rebuilt by `make setup-agent-browser`. The
  runtime never executes `node_modules/.bin/agent-browser`; the mirrored binary
  resolves version-matched `skills/` and `skill-data/` from the sibling pinned
  npm package. `make verify-agent-browser` checks this in a temp mirror
  (`skills get core --full`); the container build runs the same check, and
  first boot materializes the default scope's browser skill before use.

  Protocol/artifact adapters in `bin/` are the provider client itself: the
  runtime sends validated canonical JSON on bounded stdin. A skill with an
  existing native CLI (Metabase) declares typed actions in `SKILL.md` and
  calls that CLI directly. Optional and `at_least_one` secret bindings are
  omitted when absent, never materialized as empty env values. Governed
  dispatch accepts only the exact runtime-declared, regular, non-symlink
  executable from the loader-owned resolved skill directory.
- `config/` — optional setup/compatibility templates. Runtime credentials
  are selected through the scope/profile Auth Broker and are materialized
  only into exact bindings declared by the governed contract. A private
  legacy `config/.env` may temporarily supply a missing profile-free record
  under a bounded 1 MiB-capped, identity-revalidated read; it is not a
  second model-facing auth route. Canonical denial or unavailability never
  falls through. Keys enter only their declared child environments and
  cannot be provided as model input.
- `tool_schema.yaml` — legacy compatibility input for external/scoped
  packages that have not migrated. Built-in tool packages use the governed
  contract in `SKILL.md`. Selected schema-only public-contract baselines
  remain under `magician/tests/fixtures/tool_runtime_legacy_contracts/` and
  cannot be discovered or dispatched (background:
  Phase 7 migration ledger).
- `metadata.magician.runtime_contract`, `runtime_actions`, and
  `runtime_catalog` — the governed replacement authored in `SKILL.md`.
  The loader validates the contract, compiles the finite typed actions, and
  projects the result into the existing capability registry. A skill
  directory may contain the governed package or `tool_schema.yaml`, never
  both; dual active sources fail closed. A SKILL.md with neither source
  remains a plain steering skill.

  Direct CLI, canonical-JSON adapter, CLI-owned session, browser/native,
  QR/device, static-secret, and official-SDK MCP packages all compile from
  that single governed source. Secret references materialize only after
  route authorization from the principal/workspace vault.

  Protocol/artifact packages use `tool-runtime.typed-action-overrides.v2`
  with `input_delivery: canonical_json_stdin`. `runtime_actions` is the sole
  public parameter/default/type contract. The runtime removes raw stdin from
  the model schema, applies defaults once, and sends a deterministic bounded
  object to the executable. The executable must not redeclare flags with
  `argparse`/`sys.argv`. Native CLI packages such as the six Google Workspace
  skills retain v1 inert argv and have no skill-owned wrapper.

  A selectable-profile package may publish one bounded model-facing alias
  under `runtime_catalog.profile_parameter`. For example, Calendar exposes
  the canonical runtime `profile` selection as `account` with the finite
  aliases `business`, `personal`, and `work`. This metadata controls catalog
  presentation only: the Auth Broker profile registry remains authoritative
  for profile existence, readiness, identity, revision, and credential
  placement. Fixed and implicit profiles cannot publish an alias, and an
  authored action parameter cannot collide with it.

  `implementation.cwd` is interpolated against the call's arguments at spawn
  time, so `cwd: '{working_dir}'` lets each call's `working_dir` drive the
  subprocess working directory.

  **Available template vars in `implementation.command` / `cwd` /
  `arg_mappings`:**

  | Var | Resolves to | Use case |
  |---|---|---|
  | `{scope_capabilities_root}` | `<scope>/` (scope root) | scope-local file paths (bot env-files, gws auth, …) |
  | `{scope_capability_auth_root}` | `<scope>/auth/` | OAuth tokens / credential dumps |
  | `{scope_capability_workdir_root}` | `<scope>/workdirs/` | scratch dirs (e.g. WhatsApp `WU_HOME`) |
  | `{skill_runtime_root}` | resolved per call: `<scope>/skills/<skill>/` if scope-installed, else `<system>/skills/<skill>/` | this skill's own runtime tree (for sibling-script `command:` lookups). Useful for `python3` shell wrappers and skill-bundled binaries: `command: ['{skill_runtime_root}/bin/metabase-pp-cli']` |

  Installed CLIs (gws, tgcli, wu, OfficeCLI, and similar tools) are resolved
  through the governed reviewed-executable authority. Skill-owned `bin/`
  entries are admitted only when declared by the runtime package; the
  coordinator does not expose every active skill's binaries through one
  ambient PATH.

  Tool-call argument names also substitute (e.g. `{working_dir}`,
  `{prompt}`) — those resolve at spawn time against the call's arguments,
  while context vars resolve once at dispatcher construction.

## Magician extensions (`metadata.magician.*`)

Two structural blocks. **Presence determines the runtime route.** A
skill is either a personality-mode (if `personality:` is present) or a
procedure (everything else).

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

The skill does NOT appear in the agent's main tool catalog. Its
`name + description` populate the `switch_personality` tool's mode
list. `switch_personality(mode="witty")` writes the content into the
agent's `personality_profile` memory tier.

**Personality is additive** — the agent's base persona (from its
agent-definition YAML at
`magician_data_v3/system/agent_templates/agents/<name>/definition.agent.yaml`)
is unchanged. Personality is an overlay layer.
`lookup_personality_mode` keeps underscore-to-kebab aliases so existing
agent definitions (`default_personality: true_friend`) resolve against
kebab-named skills (`name: true-friend`).

### 2. No block → procedure (default)

Just standard frontmatter. No magician-specific fields needed. This is
what every third-party skill from agentskills.io / OpenClaw / Hermes /
git URLs is — they install and run as procedures unchanged.

A procedure with no `scripts/` and no `bin/` is a *plain* skill — pure
instruction body. Activation = inject body as steering message, register
zero ephemeral tools, no execution. Plain skills are outer-loop only.

A procedure with `scripts/` and/or `bin/` is a *toolskill*. Activation =
inject body + register each script as `<skill>__<stem>` ephemeral tool +
prepend `bin/` to `PATH`. Toolskills span outer loop (LLM picks) and
inner loop (script execution).

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

### `requires.bins` for a governed runtime contract

`requires.cua` is independent of `requires.host_gateway`: a Windows/Linux
CuaDriver provider enables computer use without enabling Mac-only automation.
If both are true, both providers are required. Availability is probed at backend
startup; setup and actual GUI permissions are covered by [CUA setup](../scripts/cua-setup.md).

A governed skill repeats `bins` inside `metadata.magician.runtime_contract.requires`,
and there the list has two distinct jobs.

**The entry point** is the process the runtime execs. It is bound as execution
authority, copied into the private single-file snapshot, and revalidated before
launch. It must resolve, or the call fails.

**PATH companions** are every other name in the list. The governed child runs
with a cleared environment whose `PATH` is assembled only from the directories
that resolve this list. A companion is never snapshotted and never bound as
authority: the child execs it in place. Declaring an optional engine as a
companion does not make that engine a precondition for the rest of the skill.

**`entrypoint` is required as soon as `bins` holds more than one name.**
Without it `validate_requirements` (`manifest_validation.rs`) answers
`AmbiguousExecutable` and `load_pack_defs_from_skills_dir` logs and **drops the
pack entirely** (the tool then answers `unknown inner-loop pack`). `bins` is a
set, so order carries no meaning.

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

Every skill that needs a key (`runtime_contract.auth.kind: secrets`) should
declare the exact HTTPS hosts its code contacts. When an app (an untrusted
package) runs the skill in its jail, an owner-ticked key reaches only the
hosts the skill declares. A skill that declares none makes the owner pick a
host for each key at install, which is guesswork for the owner.

```yaml
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.example.com
      # or, for several hosts (never both fields):
      # destinations:
      # - api.example.com
      # - uploads.example.com
```

Put the block first under `metadata.magician`.

- **Format.** Use one `destination`, or a `destinations` list of at most 16
  hosts. Never use both.
- **Hosts.** Each host is a lowercase DNS name. IP addresses, ports, schemes,
  paths and wildcards are refused. Traffic is HTTPS on port 443 only.
- **Strict parsing.** An unknown field or another `schema_version` refuses
  the skill.
- **Several hosts cost the key its domain lock.** With more than one host,
  the vault grant carries no domain, so a domain-restricted vault key is
  refused. Declare only hosts the code really needs.

**What to list.** List the API base URLs, plus any upload or download CDN the
code itself fetches from. To find them:

- Read the code: the skill's `bin/` adapter, plus the base URL in any
  third-party CLI or SDK it calls (under `skillshub/node_modules` or
  `skillshub/.venv`). The jail gives the child a clean environment, so the
  library default applies, not an operator override.
- Or run the skill once in an app and read the broker receipt's `contacted`
  list.

**When a host cannot be declared.** Declare only fixed hosts, and never
guess. If the host is dynamic, leave it out: the owner then picks the host at
install. Examples are a user-configured base URL (such as `metabase`'s
`METABASE_BASE_URL`) or a URL taken from input.

The trust model, the broker, receipts and in-place skills are covered in
[`app-os-jail-egress.md`](app-os-jail-egress.md).

**Arbitrary sites (`*`).** A skill that contacts arbitrary sites by design (a
web fetcher) may declare `destinations: ["*"]`, and `*` must be the sole
entry. Such a tool is an any-host tool: in an app it reaches the network only
under the app's "any public host" grant (which the app's manifest must ask
for, with every authority ceiling allowing it), and its key only with the
owner's explicit per-key "allow this key to be sent to any site" tick.

**Where a key can go.** With declared hosts, a key reaches the declared hosts
the app is granted. With none declared, the owner picks one or more of the
app's named hosts per (app, tool, key), or opts that key in to "any site";
the choice is the vault scope and the only hosts the tool reaches while the
key is injected.

### `runtime_canary` cost ceilings

`expect.max_cost_microunits` requires `expect.max_cost_commodity`, and the
commodity must match the one the package declares at
`content_source.output.cost.commodity`. Packages price in different things on
purpose — exa reports `usd`, tavily reports `tavily_credit` because the
provider returns no money figure and the value of a credit belongs to the
operator's plan — and the product's retrieval budgets are keyed by commodity
for the same reason. A bare number is not a ceiling: copied onto a package
with another commodity it would be read in the wrong unit.

### Skill-pack publication metadata (plan 2.4)

A skillshub pack becomes platform-publishable app content by declaring one
optional, bounded block inside `metadata.magician`:

```yaml
metadata:
  magician:
    app_publication:
      display_name: YouTube Search   # 1-320 bytes after trim; required
```

The block lives in the same `metadata.magician` mapping as the governed
runtime contract, parsed through `parse_skill_magician_extension`. No sidecar
file and no second manifest format exist.

- **Presence is the publication marker.** Absent = today's behavior.
- **`display_name` is the only field** (1–320 bytes after trim). Identity,
  description, and version come from required frontmatter. Unknown fields
  are rejected (`deny_unknown_fields`).
- **Fail-closed at app-catalog snapshot** (`tool_skill_descriptor` in
  `magician_v2/apps/primitive_catalog.rs`): a present-but-malformed block
  quarantines the pack (`invalid_tool_skill`). Ordinary agent discovery and
  dispatch of the same pack are unaffected.
- **Publication grants nothing by itself.** The pack still passes reviewed
  ToolSkill admission (typed USR contract, typed actions, no raw argv
  passthrough, no shell bins, `expose.apps` not explicitly denied) before an
  app manifest can lock it in `dependencies.tools`.

A first-party wrapper may sit at `skillshub/<name>/app/`. Discovery scans
only `<skills-root>/<name>/SKILL.md` one level deep and skips
`skill_type: app`. Exemplar: `skillshub/youtube-search/` plus
`skillshub/youtube-search/app/` (`youtube-search-coverage`). See
[`app-primitive-catalog.md`](app-primitive-catalog.md).

## Discovery & resolution

Per-name, per-layer (scope wins on collision):

1. `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<name>/` — live workspace (canonical; defaults under `~/MagicianNotes`)
2. `$MAGICIAN_ROOT_DIR/system/skills/<name>/` — back-compat fallback (empty in normal operation; there is no system-shared tier)

Whole-folder shadowing: workspace's `<name>/` wins entirely over
system's. No merging within a name. `<name>` matches both the parent
directory name and the SKILL.md `name` field (validator enforces).

The git-of-record source is `skillshub/`. The runtime tree is populated
by `make -C skillshub install-scope SCOPE=<principal>/<workspace>` —
`install_skill_layer.py` creates source-file symlinks back to
each `skillshub/<name>/*` source file, so editing source is live at
the runtime path without re-installing. Runtime-owned config is stored outside
the swappable package at `<skills>/.skill-state/<name>/config`; the package's
relative `config` link keeps that state relocatable while preserving secrets
and provider-owned OAuth/session data across atomic rematerialization.

A present higher-precedence `SKILL.md` claims its directory name before bounded
parsing. If it is malformed, oversized, or unreadable, that one name fails
closed while valid siblings still load; a lower package of the same name is
not silently reactivated.

Root Makefile targets: `skills-list`, `skills-list-source`, `skills-validate`,
`skills-install-scope SCOPE=<principal>/<workspace>`. `scripts/magician-skills`
is a thin pass-through to the same skillshub Makefile / `list_skills.py`.

## Per-agent allowlist

The list of skills available to an agent during a session is sourced
from that agent's existing definition YAML (`tools:` field at
`magician_data_v3/system/agent_templates/agents/<name>/definition.agent.yaml`),
which the runtime resolves against loaded skills:

- Procedure skills in the allowlist appear in the catalog (LLM can pick).
- Personality-mode skills in the allowlist are valid args to
  `switch_personality`.
- Skills not in the allowlist are loaded by the runtime (so other agents
  with them allowlisted can use them) but invisible to this agent.

**Default policy is deny.** If the agent's `tools:` list is missing or
empty, the agent has no skills available. This is intentional — many
agents with different personas means default-allow erodes LLM reliability
and bloats context.

`activate_skill` is durable: each chat session carries an optional
`active_procedure_skill` slot (single-active, replace-on-set, idempotent
on re-activation). While set, the skill's body is re-injected into the
system prompt every turn as `## Active Playbook: <name>`. `deactivate_skill`
clears the slot. The allowlist is enforced by exact skill name.

## Governed child environment

Governed execution starts from a clean bounded environment. The runtime supplies only
contract-declared context, exact credential bindings, reviewed executable discovery,
and the minimum platform variables needed by the selected execution family. It never
hands a tool a generic vault or ledger bearer token. CLI-owned profiles may receive a
validated `HOME`; ordinary protocol adapters cannot discover ambient home-directory
credentials. Browser cookies, OS permissions, and official-SDK OAuth sessions remain
with their declared native/session owner.

The portable baseline also carries `SSL_CERT_FILE` from a runtime-owned
CA-bundle list (never inherited from the parent). OS-owned stores that only
root can replace are tried before package-manager prefixes; the first readable
match wins. A hermetic contract that needs TLS must opt into the portable
baseline. A host with no readable bundle degrades to the interpreter default
and logs one warning so the fallback is distinguishable from a bad certificate.

Closed skills also receive no `MAGICIAN_ROOT_DIR`, `MAGICIAN_STORAGE_PATH`,
or object-store/database credentials. The launcher writes a versioned
`SubprocessStorageEnvelope` under `workdirs/skill-working/{call_id}/`.
Inputs are materialized read-only into that lease; declared output slots
become canonical only after publication through an owning Task 10 object
repository. A skill file existing on disk is not a durable result.
Operator batches that still need the live runtime root import
`skillshub/scripts/runtime_root_shim.py`. New direct env resolution
outside that shim fails `scripts/skillshub_runtime_root_linter.py` in
the docs-guard gate.

## Validation

The `skillshub/scripts/validate_skill_md.py` CLI checks required frontmatter,
AgentSkills `name` rules, parent-directory match, and `description` ≤ 1024
chars. Optional `metadata.magician.content_source` / `content_reader` /
`observe_source` must use schema version 1 with required
adapter/capability/output mappings and the governed runtime/action catalog in
the same `SKILL.md`. Rust registration performs full deny-unknown validation;
`content_reader` additionally bounds HTTP/cache/quality and requires a local
deterministic extraction action. `observe_source` declares source/profile
projection only and does not acquire executable authority. Legacy sibling
`content_source.yaml` / `content_reader.yaml` / `observe_source.yaml` files
are rejected rather than merged.

Run before installing: `make -C skillshub validate` (or `make skills-validate`).

## Catalog sources

Three sources for capability-pack definitions, in priority order:

1. **`<storage_root>/scopes/<principal>/<workspace>/skills/<skill>/SKILL.md`
   governed runtime package** — all built-in externalizable capabilities (awk, jq,
   gmail, csvkit, browser, …). Materialized from the matching `skillshub/`
   directory. A legacy `tool_schema.yaml` is accepted only for unmigrated external
   packages, and the two catalog sources are mutually exclusive per skill;
   `compiled_providers::load_pack_defs_from_skills_dir` compiles the governed
   package when present and otherwise loads the legacy schema. Description and
   guide always come from `SKILL.md`.
2. **Embedded compiled pack defs** — built into the binary via
   `include_str!`. The 122 internal capabilities that wrap in-process
   Rust providers (`create_agent`, `treasurer`, `list_agents`,
   `notify_owner`, `task_state`, `read_trace`, …). Their schemas are
   part of the magician binary, not runtime data. The YAML source
   files live at
   `magician/src/magician_v2/execution/embedded_pack_defs/<name>.yaml`
   and are pulled in by `compiled_providers::embedded_compiled_pack_source_table`
   (parsed by `embedded_compiled_pack_defs`). Add a new entry to that table when introducing a new compiled
   provider. Parse failures are panics — a malformed embedded YAML is
   a build error, not a runtime condition.
3. **`<storage_root>/system/capability_templates/packs/<name>.yaml`** —
   **removed**. The loader no longer reads a disk pack directory: every
   pack is either skill-authored or compiled into the binary.

Earlier sources win on collision. Same precedence at scope-level via
`CapabilityWorkspaceManager::load_pack_defs_for_scope`.

New tools author the runtime/auth contract and typed actions directly in `SKILL.md`.

### HTTP catalog and install

- `POST /api/magician/v2/skills/install` (admin via `X-Magician-Setup-Token`).
  Body: `{ "source": "path:/abs/path | skillshub:<name>", "target":
  {"system": true, "workspaces": ["principal/workspace"]} }` — at least one
  of `target.system` or `target.workspaces`. SkillLoader validates before
  any copy (bad frontmatter / name-dir mismatch / missing `SKILL.md` → 422).
  `SkillLoader::discover` caches each `SKILL.md` parse by path, size and
  mtime and re-parses only changed files; it sits on hot request paths (the
  chat composer's reference catalog calls it several times per request).
  Bundled materialization is atomic per target (process lock + no-gap
  directory exchange). Provider state is outside both trees.
- `GET /api/magician/v2/skills` returns installed skills plus the 115
  compiled built-ins. SKILL.md: `kind` ∈ {`procedure`, `personality-mode`},
  `layer` ∈ {`system`, `workspace`}. Compiled: `kind: "compiled"`,
  `layer: "built-in"`. Workspace shadows system; SKILL.md shadows built-ins.
  Unauthenticated.
- Forge `/skills` calls `POST /skills/install`, `/skills/{name}/promote`,
  `/skills/{name}/demote`, `/skills/{name}/allow-for-agent`. Promote/Demote
  hidden for `built-in`. Allow-for-agent does line-based YAML editing of the
  agent-definition `tools:` block.

## Dispatch model

Every outer-loop capability tool call dispatches through inner-loop; there is
no procedure-skill single-shot fast path. The executor's
`ExecutableAction::Pack` arm flows into `dispatch_flat_action` /
`primitive_dispatch::dispatch::dispatch_primitive`.

Routing inside that dispatch:

1. **`browser`** → bespoke `BrowserDispatcher` via the
   `BROWSER_PACK_NAME` branch in `dispatch_primitive`. Owns
   `agent-browser` CLI session lifetime, owned-tabs header rendering,
   and Yutori N1.5 translation. Browser is a skill in catalog terms
   (`skillshub/browser/SKILL.md`); only the dispatcher path stays bespoke.
2. **Provider-backed inner loop** (compiled providers exposed as
   inner-loop, e.g. `read_trace`, `duckdb`, `files`, `http`) →
   `dispatch_compiled_provider_primitive`. The pack YAML
   declares `Primitive { provider_name: Some(_), .. }`; dispatch routes
   each primitive to the existing in-process Rust provider.
3. **Governed runtime package** (all built-in tool skills) → the validated
   `SKILL.md` contract lowers typed input into inert argv, canonical JSON stdin, or an
   official-SDK MCP call and executes through the coordinator. Authorization
   precedes credential/session access; executable/cwd authority, cancellation, bounds,
   redaction, durable settlement, and audit are shared. Direct CLIs have no skill-owned
   plumbing wrapper.

**Compiled providers** (`create_agent`, `treasurer`, `list_agents`,
`notify_owner`, … — packs with `implementation.type: compiled`)
continue to dispatch through the pack-runtime path.

### Deterministic application reuse

Governed skill execution is exposed internally through
`ScopedDeterministicCapabilityInvoker`. This is an extraction of the existing primitive
subprocess path, not a second runner: agent-selected tool calls retain the same scoped env,
sandbox, timeout, spend gate, output/failure behavior, and learning record. Application
services that already know the capability and arguments, such as user-defined feeds and
recurring monitors, can invoke the same skill without an additional LLM selection turn.

A skill intended for deterministic content discovery adds
`metadata.magician.content_source` to its `SKILL.md`; no provider-specific
Rust registration is required. The generic adapter validates the declared
action against that skill's governed contract and invokes the native action
through the same execution path. Retrieval appends installed actions to the
rung declared by their own metadata; central configuration is an optional
ordering override.

A local full-content extraction skill may add
`metadata.magician.content_reader`. The generic static reader performs
secure bounded HTTP and scoped conditional caching in Rust, writes the
response to a scoped scratch file, and invokes the declared action. It
cannot opt out of network validation, change candidate
identity/privacy/provenance, or send page content to a remote extractor.

Output modes: `mapped` (RFC 6901 JSON pointers) and `canonical_v1` (the
script emits provider-neutral items; Rust retains policy, scope, bounds,
identity, privacy, canonicalization, provenance, cost, and cursor).
Governed example: `semantic-websearch-via-exa/SKILL.md`.

## Management surface (web + API)

The `/skills` page is the unified management surface, backed by:

- `GET /api/magician/v2/skills/catalog` — every skill in the system
  (skillshub source, extras roots, scope-only `path:` installs, embedded
  compiled packs) with the authoring taxonomy
  (`tool` / `procedure` / `personality` / `compiled`) and the install
  footprint per scope (`installed_scopes`; scope install is filesystem
  presence under `scopes/<p>/<w>/skills/`).
- `POST /api/magician/v2/skills/{name}/uninstall` (setup token) — remove
  from scopes. Default removal preserves the skill's `config/`,
  `auth/` and `.skill-state/` (an inert skeleton the loader skips), so a
  reinstall restores credentials; `purge: true` deletes everything behind a
  preview→confirm round-trip.
- `GET|POST /api/magician/v2/runtime/env` (setup token for writes) — the
  runtime `.env` / `.env.development` value layer, write-only (never echoes
  values), with lockout-risk keys requiring an explicit acknowledge. This is
  the UI replacement for interactive use of the env-writing setup scripts.
- `GET|POST /api/magician/v2/skills/catalog/{name}/env` (setup token for
  writes) — a per-skill `config/.env` inside a scope: key status from the
  manifest's `requires.env` plus the skill's `config/.env.example`, values
  write-only.

The `path:` / `skillshub:` schemes are the only install sources.
