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

`runtime_core::edge` is the shared versioned wire contract between a remote
Magician service and the desktop running as Magician Edge. It defines the
client hello and capability manifest, accepted session lease and heartbeat,
capability-generation updates, short-lived execution grants, bounded typed
invocations and results, and cancellation. The server assigns session
generations to fence replaced connections; grants bind workspace, device,
execution epoch, capability generation, operation, expiry, and payload limits.
Struct payloads reject unknown fields.

This crate does not open sockets or store device credentials. The service owns
session admission and routing, while Tauri owns the outbound connector and
local capability dispatcher. The shared constants name the initial
`host.cua`, `browser.cdp`, and `host.imessage` capability contracts. The
desktop loopback host gateway remains a local
compatibility adapter and is never exposed as a remote listener. Run the
focused provider-free contract suite with `make test-edge-protocol`.

The Magician-side `EdgeSessionRegistry` applies this vocabulary to live
connections. It reuses the lifecycle shape of `DeviceBridgeHub`: exact socket
generations cannot deregister replacements, per-scope/global admission and
in-flight work are bounded, capability rotation fails older calls, and
disconnect, revocation, or lease expiry resolves every pending caller. Run both
the shared wire and registry suites with `make test-edge-transport`. Its
`dispatch` entry point mints request ids, grants, generations, deadlines and
payload limits from the live descriptor; callers provide only scoped execution
identity and the typed operation payload.

`ParameterDefinition` carries a `schema: serde_json::Value` field that is the
canonical JSON Schema for the parameter's accepted value. Catalog generators
emit it verbatim to LLM tool lists. Upstream pack-loading code populates it
from each capability pack's YAML (either an explicit `schema:` block or a
synthesis from the legacy `param_type` / `enum_values` shorthand). The other
fields (`param_type`, `enum_values`, `description`, `default_value`) remain as
derived/coarse views for runtime parameter coercion and tool matching; the
LLM-visible source of truth is `schema`.

## Tool catalog and `ToolInfo` extensions

`ToolCatalog` (`runtime-core/src/services.rs`) carries `CORE_UTILITY_CATEGORY`,
the shared `core_utility` category for deterministic, low-risk helpers such as
`time_math` that stay visible across department/team allowlists unless an agent
excludes the tool name or one of its categories; `agent_filtered_tools` applies
that rule together with the agent's name/category whitelist and blacklist.

`ToolInfo` (`runtime-core/src/tooling.rs`) carries `providing_agent_id:
Option<String>` — the agent that provides a tool for delegation-aware
planning. When a personal agent delegates steps to a worker, the field is set
to the worker's agent_id; the planner copies it to `PlanStep.providing_agent_id`
and the step executor uses it for delegation-boundary validation and step-hash
computation. `with_providing_agent(agent_id)` is the builder used by
`StrategyContext::merged_agent_tools()` to stamp delegate tools before they
reach planners.

## Current Usage

- `tool-runtime-core` provides local implementations backed by capability YAML files.
- `magician` consumes these traits directly in-process (no HTTP adapter path).

## V2 Conversation Store

`V2ConversationStore` is the shared execution-keyed storage contract for the
Magician V2 runtime: execution records, turns, slots, delete/list/status are
addressed by `execution_id`, not by task or thread.

Status concurrency is three required methods:
`compare_exchange_execution_status`, `compare_exchange_execution_status_at`,
and `get_execution_status_revision`. The stronger CAS binds both the expected
status enum and a status-only generation, so durable multi-record pause/resume
transactions reject Paused-state ABA while ordinary metadata updates do not
manufacture status conflicts. Implementations must provide a real atomic
comparison or fail closed.

`compare_exchange_execution_status_holding_task_lock` is the same CAS for a
caller that already holds the execution's task-scoped cross-process lock. It
is **defaulted** to the ordinary CAS. A store whose ordinary CAS takes that
same task lock must override it: `flock` is per open-file-description, so
re-acquiring on a second descriptor blocks forever. The file-backed Magician
store overrides it; see `docs/components/magician/storage-v2-format.md`.

`add_turn_with_id` is used by durable accepted-launch recovery to prewrite and
replay one deterministic inbound planning turn. An implementation returns
`None` when it cannot guarantee exact insertion; the default performs no
write. A supported implementation must return the same existing turn only when
execution, direction, text, and reply-slot binding all match, and reject a
conflicting reuse of the id. Calling ordinary `add_turn` as a fallback would
create a second random-id user message and is outside the contract.

`create_execution_with_work_authority` is a **required** method carrying an
optional `WorkAuthorityGrant` `{ work_kind, work_id, authority_revision }` to
persist on the new execution record. `runtime-core` cannot depend on the crate
that owns work contexts, so the typed carrier crosses as three named fields;
the magician-side store rebuilds the typed kind and **refuses a token it does
not know**. Every implementor states in its body what it does with the carrier
(`FileV2Store` persists it; test doubles delegate to
`create_execution_with_options` and say they drop it).

Required does not relieve the caller. A store may still legitimately drop the
carrier, so **the caller must compare the record it got back with what it
asked for**. `MagicianV2Orchestrator` does it on both write paths
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
of the first executable file found on `path_env` — the PATH the child will
see, or the process PATH when `None`. A program that already names a location
(absolute, or relative with a separator) passes through unchanged. A bare
name that is not found does NOT stay bare: it comes back as
`<first non-empty PATH dir>/<program>`, a path that names no executable, so
the spawn stays on `posix_spawn` and fails with the ordinary not-found error
instead of forking to repeat the lookup in the child. Only when there is
nothing to search (no PATH at all, or one with no non-empty dir) is the bare
name returned unchanged.

It exists because Rust's `Command::spawn` only uses `posix_spawn` when it can:
a bare name that needs PATH lookup combined with any touch of the child's
`PATH` — `env("PATH", ..)`, `env_remove("PATH")`, or `env_clear()` on its own
(std's check is `saw_path || clear`) — forces `fork`+`exec`, and a forked copy
of the multithreaded Magician process can fault inside macOS's XPC atfork
handlers before it reaches `exec`, leaving the parent blocked on the
exec-status pipe. Every spawn site in the workspace that overrides `PATH`
resolves its program through this helper first, against the PATH the child
actually receives: where a later env layer can carry `PATH` — a model-supplied
bash step env, a bot's `config.env`, a skill's `config/.env`, a pack or
CLI-template `env`, a verification `spec.env` — the site computes that final
PATH before `Command::new` and resolves against it, keeping the env
application order unchanged. The `AGENTS.md` project defaults state the rule.

## Extension Guidance

When adding a new runtime implementation:

1. Implement the `runtime-core` trait(s).
2. Wire the implementation into service bootstrap (typically `magician`).
3. Add focused tests in the implementing crate.
