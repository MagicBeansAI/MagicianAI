# Runtime Core

`runtime-core` is the shared contract crate for tool discovery and matching
across this workspace. Current crate version: `0.1.8`.

## What It Contains

- `ToolCatalog`: list/get tool metadata. `agent_filtered_tools()` supports
  whitelist/blacklist by tool **name** or **category**, with `tools`
  (whitelist) and `excluded_tools` (blacklist) parameters.
- `ToolMatching`: map user intent to candidate tools.
- `SemanticSearch`: ranked search interface for tool discovery.
- Shared data models such as `ExecutionContext` and `ToolMatch`.

## Magician Edge protocol

`runtime_core::edge` is the versioned wire contract between a remote Magician
service and the desktop running as Magician Edge: client hello and capability
manifest, session lease and heartbeat, capability-generation updates,
short-lived execution grants, bounded typed invocations and results, and
cancellation. Server-assigned session generations fence replaced connections;
grants bind workspace, device, execution epoch, capability generation,
operation, expiry and payload limits. Struct payloads reject unknown fields.

The crate opens no sockets and stores no device credentials. The service owns
session admission and routing; Tauri owns the outbound connector and local
capability dispatcher. Shared constants name the `host.cua`, `browser.cdp` and
`host.imessage` capability contracts. The desktop loopback host gateway stays a
local compatibility adapter, never a remote listener. Contract suite:
`make test-edge-protocol`.

The Magician-side `EdgeSessionRegistry` applies this to live connections with
the lifecycle shape of `DeviceBridgeHub`: exact socket generations cannot
deregister replacements, admission and in-flight work are bounded per scope and
globally, capability rotation fails older calls, and disconnect, revocation or
lease expiry resolves every pending caller. Its `dispatch` entry point mints
request ids, grants, generations, deadlines and payload limits from the live
descriptor; callers supply only scoped execution identity and the typed
payload. Wire and registry suites: `make test-edge-transport`.

`ParameterDefinition` field `schema: serde_json::Value` is the canonical JSON Schema
for a parameter and the LLM-visible source of truth; catalog generators emit it
verbatim. Pack loading fills it from each capability pack's YAML (an explicit
`schema:` block, or synthesized from the legacy `param_type` / `enum_values`
shorthand). `param_type`, `enum_values`, `description` and `default_value`
remain coarse views for runtime coercion and tool matching.

## Tool catalog and `ToolInfo` extensions

`ToolCatalog` (`runtime-core/src/services.rs`) carries `CORE_UTILITY_CATEGORY`,
the shared `core_utility` category for deterministic, low-risk helpers such as
`time_math` that stay visible across department/team allowlists unless an agent
excludes the tool name or one of its categories; `agent_filtered_tools` applies
that rule together with the agent's name/category whitelist and blacklist.

`ToolInfo` (`runtime-core/src/tooling.rs`) carries `providing_agent_id:
Option<String>`, the worker agent that provides a delegated tool. The planner
copies it to `PlanStep.providing_agent_id`, and the step executor uses it for
delegation-boundary validation and step hashes. `StrategyContext::merged_agent_tools()`
stamps delegate tools with `with_providing_agent(agent_id)`.

## Current Usage

- `tool-runtime-core` provides local implementations backed by capability YAML files.
- `magician` consumes these traits directly in-process (no HTTP adapter path).

## V2 Conversation Store

`V2ConversationStore` is the shared execution-keyed storage contract for the
Magician V2 runtime: execution records, turns, slots, delete/list/status are
addressed by `execution_id`, not by task or thread.

Status concurrency is three required methods:
`compare_exchange_execution_status`, `compare_exchange_execution_status_at`,
and `get_execution_status_revision`. The stronger CAS binds the expected status
and a status-only generation, so pause/resume transactions reject Paused-state
ABA while metadata updates cause no status conflicts. Implementations must
compare atomically or fail closed.

`compare_exchange_execution_status_holding_task_lock` is the same CAS for a
caller that already holds the execution's task-scoped cross-process lock. It
is **defaulted** to the ordinary CAS. A store whose ordinary CAS takes that
same task lock must override it: `flock` is per open-file-description, so
re-acquiring on a second descriptor blocks forever. The file-backed Magician
store overrides it; see `docs/components/magician/storage-v2-format.md`.

`add_turn_with_id` lets durable accepted-launch recovery prewrite and replay
one deterministic inbound planning turn. An implementation returns `None` when
it cannot guarantee exact insertion (the default writes nothing). Otherwise it
returns the existing turn only when execution, direction, text and reply-slot
binding all match, and rejects a conflicting reuse of the id. Falling back to
`add_turn` would create a second user message and is outside the contract.

`create_execution_with_work_authority` is a **required** method carrying an
optional `WorkAuthorityGrant` `{ work_kind, work_id, authority_revision }` for
the new execution record. `runtime-core` cannot depend on the crate that owns
work contexts, so it crosses as three named fields; the magician-side store
rebuilds the typed kind and **refuses a token it does not know**. Each
implementor states what it does with the carrier (`FileV2Store` persists it;
test doubles delegate to `create_execution_with_options` and drop it). Since a
store may drop it, **the caller must compare the returned record with what it
asked for**: `MagicianV2Orchestrator` does so on both write paths
(`create_root_execution_under_work`, `create_delegation_execution`) via
`verify_persisted_work_authority`. See
`docs/components/magician/engagements.md`.

`update_execution_entry_mode(execution_id, entry_mode)` persists the execution
entry mode beside the wait-state/status hooks, so Magician can keep apart an
execution entered through the plan-backed task path, explicit direct `none`,
and explicit direct `taskplan`.

## Semantic Search Contract

`SemanticSearch` exposes two methods:

- `search_all_tools_with_scores(query)` — ranked `SemanticMatch` rows.
  Implementations may apply thresholding and should truncate to configured
  backend limits (for example `semantic_search.max_results`).
- `search_categories(category, top_k)` — top matching categories with
  similarity scores.

There is no `semantic_search(query, limit)` method and no `SearchResult` type.

## RuntimeConfig Trait

The `RuntimeConfig` trait exposes environment flags that control runtime behavior:

- `consumer_mode_enabled()` — When `true` (Magician config default), forces AtomicComposition strategy and blocks GuidedSearch at all orchestrator entry points. Implementors: `MagicianRuntimeConfig` (reads from config), `TestRuntimeConfig` (returns `false`). Trait default is `false`.
- `file_sandbox()` — Returns the `FileSandboxConfig` (allowed roots) for file operations. `FileSandboxConfig::augment_with_scopes_root(base_root)` idempotently adds `<base_root>/scopes` to `allowed_roots` — used to grant compiled read handlers (e.g. `read_file`) access to per-scope `durable_artifacts/` under the runtime root.
- `allow_consent_slots()` — Controls whether consent-slot elicitation is permitted (default `false`).
- `on_failure_mode()` — Returns `"ask_user"` (default) or `"fail"`.

## Prompt Types

`runtime-core` defines the shared prompt data model used across crates:

- `Prompt`: Versioned prompt definition with templated content and named variables.
- `PromptVariable`: Variable referenced within a prompt template (name, description, required flag, default, examples).
- `PromptMetadata`: Category, author, tags, and token estimates.
- `PromptCategory` (`runtime-core/src/prompts.rs`): `QueryAnalysis`, `TaskDecomposition`, `ToolMatching`, `ParameterElicitation`, `ResponseGeneration`, `ErrorHandling`, `AtomicComposition`, `Vision`, `General`, `Verification`, `AgenticExecution`, `MemoryConsolidation`, `AutonomousExecution`, `AgentEvolution`, `TaskplanExecution`, `Chat`, `Conversational`, `Learning`, `ApiMining`, `ChannelAssist`, `Social`. `Chat` is text chat, `Conversational` is realtime voice session prompts (modality addendum, speech tags, task-completion announcements; see `docs/components/magician/realtime-media-rails.md`), and `Social` is fleet social-network gating and composition.
- `PromptStore`: Async trait for prompt persistence (get, list, save, delete).

**Rendering:** `Prompt::render(variables)` substitutes `{name}` placeholders with provided values. Variables not in the caller's map fall back to `default_value` when defined; otherwise, `required` variables produce an error. Caller-provided values always override defaults.

**Flexible deserialization:** Both `content` and `variables` fields accept multiple JSON shapes:
- `content`: a single string or an array of strings (joined with `\n`).
- `variables`: an array of full `PromptVariable` objects or plain strings (shorthand — each string becomes a required variable with that name).

## Child process program resolution

`runtime_core::process::resolve_program(program, path_env)` (and the `str`
form `resolve_program_str`) turns a bare program name into the absolute path
of the first executable on `path_env` (the PATH the child will see, or the
process PATH when `None`). A program that already names a location passes
through unchanged. A bare name that is not found comes back as
`<first non-empty PATH dir>/<program>`, so the spawn stays on `posix_spawn` and
fails with an ordinary not-found error. Only with nothing to search (no PATH, or
no non-empty dir) is the bare name returned unchanged.

Why: Rust's `Command::spawn` falls back to `fork`+`exec` when a bare name
needs PATH lookup and the child's `PATH` is touched (`env("PATH", ..)`,
`env_remove("PATH")`, or `env_clear()` alone; std checks `saw_path || clear`).
A forked copy of the multithreaded Magician process can fault in macOS's XPC
atfork handlers before `exec`, leaving the parent blocked on the exec-status
pipe. So every spawn site that overrides `PATH` resolves its program first,
against the PATH the child actually receives. Where a later env layer can carry
`PATH` (a model-supplied bash step env, a bot's `config.env`, a skill's
`config/.env`, a pack or CLI-template `env`, a verification `spec.env`), the
site computes that final PATH before `Command::new`, keeping env order
unchanged. The `AGENTS.md` project defaults state the rule.

## Extension Guidance

When adding a new runtime implementation:

1. Implement the `runtime-core` trait(s).
2. Wire the implementation into service bootstrap (typically `magician`).
3. Add focused tests in the implementing crate.
