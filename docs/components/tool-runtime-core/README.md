# Tool Runtime Core

`tool-runtime-core` `0.1.82` is the provider-neutral skill runtime: one
`SKILL.md` package compiles to a typed catalog, a scoped Auth Broker and a
governed child process. Magician dispatches a tool that retains a
`SkillRuntimePackage` through
`magician/src/magician_v2/execution/primitive_dispatch/governed_runtime.rs`,
never falling back to `CliTemplateDispatcher` (kept for packs without a runtime
package).

A package is a `skillshub/<id>/` child whose `SKILL.md` contains
`runtime_contract:`. Dual governed + `tool_schema.yaml` sources fail closed;
`skill_type: facade` is not executable; procedure, agent and personality skills
are not tools.

## Canonical References

Source: [MagicRun](https://github.com/MagicBeansAI/MagicRun), pinned at the root
to `e9a4fdc4` (`0.1.82`: an unbounded `string_array` typed action derives bounds
inside the 64 KiB argv budget). Every consumer, `magicvault-effect` included,
inherits the pin, so one `tool-runtime-core` resolves. Magician owns
authorizers, credential resolvers, audit sinks and dispatch; there is no second
executor. Product-owned attestations cover the backends' `source_bytes`, so a
backend change rotates source-bound approvals, the OS-jail runtime digest and
any OS-jail lock. Tests: `make test-magicrun` (the product `--workspace` run
skips external dependencies).

Jail capabilities Magician relies on ([Process jail](#process-jail),
[app-os-jail-egress](../magician/app-os-jail-egress.md)):

- macOS non-jailed batch children use `posix_spawn`; the declared-artifact
  collector fails closed outside Unix; the library compiles on Windows.
- No jailed command inherits host descriptors: jail launches, the in-jail
  helper, the forwarder's child and unjailed Linux batch children set
  close-on-exec above fd 2 (bar the Linux exec-status channel) via
  `close_range`, `/proc/self/fd` or macOS `proc_pidinfo(PROC_PIDLISTFDS)` (8 KiB
  stack buffer, scan fallback).
- Linux requires the root-owned in-jail helper (else `JailHelperUnavailable`):
  it sets `RLIMIT_NPROC` in the user namespace and reports pre-exec refusal on
  an unforgeable channel. Directory identity adds birth time off overlay
  filesystems. `max_tasks`: default 256, max 1024.
- `stage_input_file`: no-follow, quota-checked write into the private workdir;
  the app jail stages read-only file inputs through it.
- `GovernedJailInterpreter` / `with_interpreter`: only the host's pinned,
  digest-checked Python (`-I -S -B <script>`), so reviewed single-file Python
  skills run in the app jail; workdir files are never mapped executable.
- `strict_app_with_brokered_egress` (opt-in): sole network is one host-owned
  HTTP CONNECT broker (macOS `localhost:<port>` IPv4 TCP; Linux caller-owned
  unix socket via `magicrun-jail-egress-forwarder`). `apps/os_jail.rs` starts
  one per call when reviewed source declares one HTTPS host
  (`metadata.magician.app_egress`); other app tools stay no-network.
- `GovernedProcessJail::with_exec_roots` runs an installed program in place:
  read/execute declared roots minus excluded subpaths, write only the workdir
  (`stage_input_file` returns `in/<name>`). Errors: `InvalidExecRoots`,
  `ExecRootsRefused` (not dispatched), `JailTeardownIncomplete` (macOS cannot
  prove all jailed processes dead → post-dispatch `ProcessFailed`, settled
  effect-uncertain, never success). Magician calls `sweep_stale_jail_members()`
  once at boot.

The app jail's use of these is specified in
[app-os-jail-egress](../magician/app-os-jail-egress.md#in-place-skills-inplaceskill-app_in_place_skill_v1):
`AppOsJailArtifactKind::InPlaceSkill` roots derive from the skill package and
its
runtimes; the exec-roots profile identity is in the lock. Checked before
`GovernedJailExecRoots`: data, home and home dot-directories are forbidden;
runtime roots under home must be recognised versioned layouts (never home or a
direct child); `etc`/`var` are excluded from prefix roots; a key file rides a
private credential directory as one more exec root (MagicRun has no read-only
one). Exact `python3` scripts run pinned `-s -B`. Fixed `requires.environment`
reaches the child only via `provide_fixed` after MagicRun validates the exact
effective contract at call time, so the loader/interpreter injection denylist
(`DYLD_*`, `LD_*`, `NODE_OPTIONS`, `PYTHONPATH`, ...) applies and launch strips
`DYLD_*`/`LD_*` again. A supported static-secret skill (environment injection
only, declared egress host, owner-ticked grant) gets a
`new_with_minimum_present`
plan built as the governed profile-free path builds it, resolved by
`ScopedCredentialMaterialAdapter` (egress host as grant domain); others run
`CredentialPreparationPlan::unauthenticated`. Every action lowers through one
`lower_reviewed_action`; app catalog and jail compiler share one contract with
working-directory controls removed (the child runs in a private empty
directory). An explicit `ordinary` approval adds no authority; other approvals,
grants and scopes block admission.

- [Tool Runtime Changelog](https://github.com/MagicBeansAI/MagicRun/blob/c65fbbaac46a70f8a1247307fc7daa5ded018275/tool-runtime-core/CHANGELOG.md)
- Phase 0: [Source Inventory](phase0-source-inventory.md), [Auth And Ownership Classification](phase0-classification.md), [Credential-Free Replay Fixtures](phase0-replay-fixtures.md)
- [SKILL.md spec](../magician/skills-spec.md), [Skill authoring](../magician/skills-authoring.md), [MCP client](../magician-mcp-client/README.md), [Workspace Architecture](../../ARCHITECTURE_V2.md), [Quick Start Config Notes](../../quickstart.md)
- Archived: commerce/MCP ownership,
  commerce policy fixtures,
  SDK/Auth Broker handoff,
  offline baseline,
  live-agentic baseline,
  Phase 7 ledger,
  runtime plan

## Runtime pipeline

```
SKILL.md
  → parse_skill_runtime_package
  → validate_skill_runtime_contract
  → synthesize_runtime_catalog  and/or  compile_typed_action_overrides
  → profile selection + sealed credential preparation
  → authorization (policy floor, grants, scopes, Resource Authority)
  → materialize credentials into a clean child environment
  → GovernedExecutionCoordinator (batch or PTY, optional process jail)
  → exact-value redaction + metadata-only audit
```

Modules: `tool-runtime-core/src/`. Magician owns product adapters (secret store,
lifecycle process owner, audit journal, workspace-path broker);
`magician-mcp-client` owns official-SDK MCP transport.

## Vocabulary

`tool_runtime_core::manifest` is the contract at
`metadata.magician.runtime_contract` (schema `tool-runtime.skill-runtime.v1`,
deny-unknown-fields), beside `skill_type`, `user_invocable`, `install_hint` and
`expose`. `metadata.magician.expose` is audience only, never authority: parser
default `agents: true`, `apps: false`; app admission of curated, reviewed and
compiled sources reads an omitted `expose` as `apps: true`, refusing only
explicit `expose.apps: false`.

- protocol: CLI (`batch` / `pty`) or MCP (stdio / Streamable HTTP)
- auth kind: `none`, `secrets`, `cli_profile`, `oauth_session`,
  `browser_profile`, `native_permission`, `delegated_credential`
- auth requirement: `none`, `optional`, `required`, `at_least_one`
- profile selection: `none`, `selectable`, `fixed`, `implicit`
- storage: `none`, `scoped_directory`, `cli_owned`, `browser_profile`,
  `operating_system`, `ephemeral_grant`
- plus reference-only secret bindings, typed injections, exact-token lifecycle
  hooks, registry-owned expected identity, and a policy floor (approval,
  grants, resource scopes, Resource Authority)

Sets serialize deterministically; argv and path-token sequences keep order.

```yaml
metadata:
  magician:
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [jq]
      runtime:
        protocol: cli
        command_prefix: []
```

`requires.bins` names executables; `requires.entrypoint` picks one when several
are required; `requires.environment` is a finite public child environment
(credentials use `auth.injections`).

## Parser

`parse_skill_runtime_package` returns a typed package (contract, optional typed
actions, non-authoritative catalog hints), `None` for a valid legacy skill, or a
stable `ManifestParseErrorCode`. `parse_skill_runtime_contract` returns only the
contract; `parse_skill_magician_extension<T>` reads a deny-unknown block under
`metadata.magician`. Limits: source 1 MiB, frontmatter 512 KiB, YAML depth
checked before deserialization, no anchors/aliases/custom tags, tree depth 64,
20,000 nodes. Diagnostics carry fixed text and line/column, never authored
scalars. Unsupported schema versions are diagnosed before v1 deserialization.
The loader owns filesystem, symlink and package-root checks.

Catalog hints (`runtime_catalog`: categories, composition category,
`profile_parameter` / `mcp_endpoint_parameter`, timeout and working-directory
control names, spend metadata) are presentation only and grant no profile, path
or spend authority. A selectable-profile package may declare one
`runtime_catalog.profile_parameter` with a portable name and finite alias set
(not on fixed/implicit profiles; a default must be in the set); Magician maps
the alias back to `profile` before selection.

## Validator

`validate_skill_runtime_contract` yields a borrowed
`ValidatedSkillRuntimeContract` whose private field stops downstream code from
accepting a merely deserialized contract. Deterministic, fail-fast,
nonrecursive; it enforces:

- one exact CLI executable; stdio MCP executable checks; remote MCP declares no
  local executable or local injection;
- bounded fixed argv, no inline shell/interpreter source;
- Streamable HTTP over HTTPS or loopback HTTP without URL credentials or
  fragments; MCP allow/deny may overlap (deny wins);
- all seven auth kinds' requirement/provider/profile/storage compatibility,
  reference-only secrets, expected-identity selectors, fully consumed sources,
  collision-free targets, lifecycle ownership, ceilings, stdin and policy
  references; canonical traversal-free relative paths; process-control names
  (`PATH`, `HOME`, loader hooks, language runtime options) never targets;
- `runtime.limits.memory_bytes` 64 MiB–8 GiB, CLI-batch only (PTY/MCP fail
  closed); stream and timeout ceilings non-zero, ≤ 256 MiB / 24 h; lifecycle
  timeout ≤ 15 minutes;
- `ConfigDirectory` from `ProfileAuthRoot`, or `Secret` only for
  `provider: minimax` + `name: MMX_CONFIG_DIR`; non-Unix rejects `ScopedFile`
  and `ConfigDirectory`.

## Catalog synthesis

`synthesize_runtime_catalog` accepts only the validation proof. Secret
references, injection bindings, lifecycle argv and identity selectors never
reach the model schema.

- **CLI**: one `<skill>.run` action (closed JSON Schema, separate non-secret
  execution binding) requiring exact `args`; `profile` only for selectable
  profiles (required without a default); stdin only when declared;
  working-directory authority and timeout ceilings included. Executable,
  prefix, auth roots, environment and policy are never model-controlled.
- **MCP**: a skill-owned discovery seed (remote tools not yet known) keeping
  transport, limits, allow/deny and local namespace, with ceilings of 512 tools,
  256-byte names, 8 KiB descriptions, 256 KiB / depth 32 / 10,000-node schemas,
  8 MiB catalog. Local names are exactly `<skill namespace>.<remote name>`, no
  slugging.

## Typed action overrides

`compile_typed_action_overrides` (`tool-runtime.typed-action-overrides.v1` or
`.v2`) replaces the generic `args` action with ≤ 512 typed CLI actions (MCP
refinement waits for bounded SDK discovery).

- Parameters: nonrecursive scalars, string arrays, bounded opaque JSON objects,
  `workspace_path`. v1 maps each to positional, flag, boolean-flag,
  repeated-flag, passthrough, JSON-flag or literal, proving worst-case argv fits
  the byte ceilings. Reserved controls (`profile`, `stdin`, `working_dir`,
  `timeout_secs`) cannot be replaced; action timeouts only lower the manifest's.
- v2 package-wide `input_delivery: canonical_json_stdin` (HTTP-only adapters):
  no argv, raw stdin hidden, every parameter validated, trusted defaults
  applied, one deterministic object on the required bounded
  stdin; JSON `null` is `InvalidParameter`.
- `argument_rules`: bounded, non-overlapping final-argv prefix denials or
  next-token allowlists, checked before authorization; they never add, rewrite
  or interpolate tokens (Higgsfield: deny `auth`, limit `workspace` to
  `list`/`status`; no Higgsfield-specific runtime branch).
- `workspace_path` is a type, not authority: the action carries the `workspace`
  resource scope, and create-file/create-directory also need
  `delegated_workspace_write`. Values are an exact broker-owned read file or
  normalized relatives under the scope's `workdirs/`; absolute paths, `..`,
  non-normal components, symlinks, missing read files and existing create
  targets fail before argv lowering.
- `runtime_control` mappings join validation, defaults and catalog projection
  but emit no argv or replay mapping; the product controller must consume
  them before dispatch.
- Policy is monotonic (per-action policy only adds approvals, grants, scopes,
  authority references). Prefixes overlapping Auth Broker lifecycle paths fail
  closed; interpreter-backed manifests fix a script or module before model
  arguments.

## Profile registry and selection

- **`credential_profiles`**: principal, workspace, provider, alias, optional MCP
  OAuth resource/issuer; endpoints HTTPS or loopback HTTP without userinfo,
  query or fragment. Snapshots are scope-exact: ≤ 256 profiles, unique keys,
  ≤ 1 enabled default per provider binding.
- **`scoped_paths`**: capability under one trusted scope-container root
  (principal/workspace, fixed `auth` dir, one profile directory bound to a key).
  Relative roots, traversal, aliases, symlinks, cross-filesystem mounts, missing
  targets and oversized scans fail closed; ancestry owner-accessible, not
  group/other writable; auth and profile roots owner-only.
  Paths only via `revalidated_path`; `ScopedPath` is not publicly constructible
  or serializable.
- **`credential_profile_store`**: metadata in `.credential-profiles.v1.json`
  under an owner-private auth root, written under a bounded `flock` with a
  revision check and atomic rename; reads never create it; failure after rename
  is `commit_state_unknown`. Legacy directories report `unknown` or `missing`;
  unsafe ones fail closed. Magician owns the Google Workspace YAML projection
  and `gws-<alias>` naming; the core accepts only opaque compatibility
  references.
- **`profile_selection`**: the one model input (a selectable alias) → a
  scope/provider/binding-exact decision; precedence request, contract default,
  registry default; a missing or disabled alias fails closed. Fixed rejects
  model aliases; none/implicit reject selection and skip the registry. Public
  selection requires a `CredentialProfileRegistry`.
- Readiness (ready, verification required, authentication required, in
  progress, blocked) is separate from selection and keeps the exact `AuthState`;
  legacy directory presence stays `unknown` / verification-required.

## Credential preparation and materialization

- **`credential_preparation`**: selection proof + strategy + ≤ 64 typed slots →
  sorted, deduped plan (1 MiB per value, 4 MiB total); invalid or cross-scope
  material is rejected before the consumer runs; one resolver batch call. Values
  sit in exact-size sealed storage (non-`Clone`, non-`Debug`, non-serializable,
  borrowed only by the injection layer), zeroized on every exit including
  unwind.
- **`CredentialSecretReference`** (≤ 512 bytes, no absolute paths or traversal)
  names a Magician SecretStore entry. `ScopedCredentialMaterialAdapter` binds
  exact profile/provider/scope/route; duplicate bindings share one decision,
  grant, redemption and charge. Delegated grants pin principal, workspace,
  provider, agent, tool, action, optional domain and expiry (≤ 64 per lock). A
  private per-skill `.env` fills only missing vault references (vault wins,
  ≤ 1 MiB, unsafe permissions fail closed); canonical record presence stays
  authoritative in a vault outage.
- **`CredentialInjectionPlan::compile`**: environment, stdin, scoped-file and
  config-directory targets, never argv; every secret consumed exactly; children
  start from a finite name-only baseline. Case-insensitive collisions, duplicate
  stdin and remote Streamable HTTP injection fail closed; remote MCP OAuth needs
  the resource URL to equal the endpoint.
- **`ChildEnvironmentValues`**: baseline names only; UTF-8, NUL-free, ≤ 64 KiB
  each, 256 KiB total; stdin bounded binary; both zeroized when the callback
  returns or unwinds.
- **Exact-value redaction** of raw output bytes (longest match first, one-byte
  sentinel) covers credential values and physical paths, not encoded, hashed,
  truncated or reordered secrets; against a hostile binary the controls are the
  sandbox, executable identity, network policy and authorization. Persistence
  redaction scans ≤ 16 records (8 MiB, 1 MiB each) as one stream, catching
  cross-record splits.
- **`CredentialScratchAuthority`**: one private container per scope; digest
  session names, `0700` dirs, create-once `0600` files; profile directories
  projected, never copied. Cleanup is retried and runs on success, cancellation
  and unwind; recovery refuses while calls are active, follows no symlinks, and
  stops at 256 sessions, 1,024 entries, 32 levels.
- `ProfileAuthRoot` `ConfigDirectory` entries stay directory handles; only
  non-profile sources inject a directory path (MiniMax `MMX_CONFIG_DIR`).

## Credential lifecycle

- **`credential_lifecycle`**: one CLI auth operation (`status`, `login`,
  `logout`, `refresh`) from the fixed executable and broker-owned hook argv,
  never the model prefix; the profile must match key, provider, binding,
  selection mode and fixed alias; disabled profiles allow only status and
  logout; plans are not serializable.
- **`credential_lifecycle_observation`**: each status hook declares an exit-code
  mapping or conditional JSON rules (equality, presence, absence, string-set);
  only a ready rule may rest solely on an expected-identity selector.
  `json_pointer` is exact; `json_pointer_ascii_case_insensitive` is opt-in.
  Outcomes include `identity_mismatch` and `provider_unverified`. Login/refresh
  success needs a fresh status; logout invalidates verified status.
- **`credential_status_cache`**: in-memory `ready` (identity `matched` or
  `not_declared`) bound to plan, process epoch, policy and profile revisions,
  auth-directory identity and expiry (256 entries, 5-minute TTL); a consume-once
  ticket stops a stale probe restoring readiness. Hits never authorize.
- **`credential_lifecycle_coordinator`**: one lease per profile key (or implicit
  scope/provider/binding); login verifies the cache epoch and invalidates
  readiness. States: running, cancelling, recovery-required, or pending
  (browser-callback, device-code, OTP, QR, operator-required), holding no URLs,
  codes or QR bytes. Caps: 256 leases and epochs; deadline ≤ 15 min (default
  5); idle ≤ 5 min. Success returns only `fresh_status_required`.
- **`credential_lifecycle_execution`**: seals a plan to the contract,
  auth-directory identity, clean environment and the executable resolved from
  its bounded absolute `PATH` (≤ 256 MiB copied, SHA-256 checked). Magician runs
  it with no shell, an empty environment, bounded pipes or a clean PTY for
  interactive login (e.g. GWS), a process group per child, ≤ 16 lifecycles.

Only an implicit CLI-profile contract with `cli_owned` storage gets `HOME`;
pack-level `interaction: pty` there fails closed (agents use `run_coding_task`,
operators Developer Mode PTY). Higgsfield is the batch CLI-owned example.

## Strategy adapters and MCP catalog

- **`strategy_adapter`**: MCP, browser-profile, native-permission and
  delegated-credential contracts share `AuthState`, `AuthKind`,
  `AuthRequirement`, `ApprovalClass`. Optional/conditional auth needs a
  per-invocation demand; protocol and auth ownership are independent; unknown
  post-dispatch delivery is never auto-retried;
  `ready` never authorizes without a fresh adapter check.
- **`mcp_catalog_policy`**: local policy fixed before any remote descriptor is
  published; entries add risk, approvals, grants, scopes and Resource Authority,
  never subtract the skill floor. Tools without an entry are `unclassified` and
  need `conditional_external_side_effect` approval, whatever remote annotations
  say.
- **`mcp_catalog_projection`**: five actions per official-SDK MCP package
  (`status`, `auth_start`, `list_tools`, `clear_auth`, `call_tool`); live
  schemas stay SDK discovery data; default timeout 90 s.
- **`browser_profile_adapter`**: opaque authority bound to profile identity,
  session class and controller session revision; cookies, storage, CDP
  credentials and profile contents stay out. Prepare, then revalidate.
- **`native_permission_adapter`**: Accessibility, Screen Recording, Microphone,
  Notifications or Automation from a metadata-only observation; a serialized
  `ready` is never authority; the native owner re-observes at use.

## Governed execution

- **`governed_execution`**: admits one invocation against the CLI contract and a
  trusted resource policy; the intent is move-only, non-debuggable, not launch
  authority; shell metacharacters stay inert.
- **`governed_execution_authority`**: resolves the fixed binary in a clean
  baseline, holds it open and copies it to a private snapshot (≤ 256 MiB;
  provenance length + SHA-256). Cwd capabilities are canonical owner-bound
  directories whose ≤ 64 components are symlink-free, same-device,
  owner-controlled and not group/world-writable; PATH symlinks are followed only
  to bind exact regular-file bytes. Self-relative wrappers needing companion
  files require a declared executable-bundle authority.
- **`governed_batch_process`**: execs the snapshot without a shell, with fixed
  prefix, admitted tokens, only the sealed child environment, bounded stdin,
  descriptor-bound cwd and a new process group. Caps: 32 processes, 512 MiB
  streams / 640 MiB retained results; 256 KiB helper stacks. POSIX
  `waitid(..., WNOWAIT)` pins the leader's group while leftovers are killed.
- **`governed_pty_process`**: separate sealed capability for
  `interaction: pty`, no fallback either way; `portable-pty` lacks pre-exec
  `fchdir`, so cwd is revalidated just before spawn. The bridge sees only
  redacted bytes.
- **`governed_execution_result`**: collects only predeclared relative files via
  `openat` (undeclared files ignored); links, special files, unsafe parents,
  owner/filesystem drift, concurrent mutation and size/depth/count overflow fail
  closed. Stdout, stderr and artifacts stay zeroizing until one
  length-preserving redaction pass over all of them as one stream; a
  credential in any artifact rejects the set; non-success terminals yield
  `partial`.
- **`governed_execution_coordinator`**: authorization before auth. The
  authorizer sees call identity, inert argv, cwd, stdin digest, policy floor,
  contract and credential-plan digests; evidence must bind the exact request and
  meet every approval, grant, scope and Resource Authority before any profile
  filesystem authority or resolver. Same-thread re-entry fails; batch/PTY entry
  points reject an interaction mismatch before authorization; every terminal
  path commits one metadata-only audit receipt.

Known limits: a descendant calling `setsid` escapes the process group (needs the
jail); a blocking authorizer, resolver or audit sink blocks its thread; a
hostile rename in the final PTY spawn interval is the jail's job.

## Process jail

`governed_process_jail` (`tool-runtime.governed-process-jail.v1`) is fail-closed
OS containment for non-interactive children: a move-only launch profile wrapping
only the exact executable snapshot and a private invocation directory it owns.
It does not resolve executables, lower input, prepare credentials, authorize or
retain output. Missing containment is a pre-dispatch refusal, never
pass-through.

- Launchers: macOS `/usr/bin/sandbox-exec`, Linux `bwrap`; absolute, root-owned,
  no group/other write on any ancestor.
- Hard maxima: 256 files, 16 MiB per file, 64 MiB total, 16 processes, 64 open
  files, 300 s wall and CPU, 1 GiB memory. Defaults: 30 s wall/CPU, 512 MiB.
  Profile text ≤ 32 KiB.
- Audit lists guarantees, not a "sandboxed" boolean: no direct network, ambient
  environment or host writes; private workdir; exact snapshot; resource
  ceilings. `process_ceiling` is true only on Linux (PID/user namespace plus
  `RLIMIT_NPROC`); macOS reports a sampled watchdog.
- macOS is deny-default, allowing only `(allow file-read-data (literal "/"))`
  for libc startup: no traversal, Mach services, network or extra executable
  authority. HOME/PATH/TMPDIR/TMP/TEMP point at the workdir; `SSL_CERT_FILE` is
  removed.
- The sampler reaps the child before enforcing sampled limits, so a fast exit
  keeps its real status; hard kernel limits apply from exec.
- The invocation cwd must be the jail-owned directory; the jail holds the
  `TempDir` through execution and removes it on every path.

## Memory limits

Batch CLI `runtime.limits.memory_bytes` (64 MiB–8 GiB) is bound into
authorization, enforced before exec where possible, and reserved against an
8 GiB process-wide budget. `memory_limit_enforcement()` reports the mechanism;
`process_memory_limits_are_enforceable()` is true when either applies.

| Mechanism | Where | How the ceiling is held |
| --- | --- | --- |
| `KernelAddressSpace` | `RLIMIT_AS` is a real resource | `setrlimit(RLIMIT_AS, …)` in `pre_exec`; the kernel refuses the allocation. |
| `ParentFootprintWatchdog` | Darwin | Samples the group's `ri_phys_footprint` every `POLL_INTERVAL`; kills it on breach. |
| `None` | neither | Contract refused at construction. |

The watchdog detects rather than bounds: a child may overshoot for one interval
and spikes between samples go unseen. (On Darwin `RLIMIT_AS`/`RLIMIT_RSS` are
one resource that rejects finite values with `EINVAL`; `RLIMIT_DATA` bounds only
`brk`, which `mmap` arenas bypass.) A breach is
`GovernedExecutionTerminal::MemoryLimitExceeded` →
`CredentialExecutionFailure::ProcessMemoryExceeded`, distinct from timeout.

## Governed kill versus inner provider budget

A credentialed HTTP adapter's wall-clock kill (`runtime.limits.timeout_secs`)
must outlast its inner provider deadlines, or its timeout/retry/fallback never
runs; socket deadlines bound each operation, not the total. Invariant:
`governed kill >= inner worst case + margin` (5 s for the table below;
`reddit-search` declares 54 s + 6 s = 60 s), stated beside `timeout_secs` and as
`INNER_WORST_CASE_SECS` / `GOVERNED_KILL_MARGIN_SECS` /
`GOVERNED_KILL_CEILING_SECS` in each adapter, checked for the six adapters below by
`phase7_migration.rs::credentialed_adapter_governed_kill_outlasts_its_inner_provider_budget`
and per-skill `test_governed_kill_outlasts_the_inner_provider_budget`.

| skill | inner provider budget | governed kill |
| --- | --- | --- |
| `websearch-via-claude` | 300 s (60 s × 5 pause-turn) | 305 s |
| `meme-generation-via-imgflip` | 25 s (10 s catalog + 15 s caption) | 30 s |
| `websearch-via-openai` | 60 s | 65 s |
| `semantic-websearch-via-exa` | 60 s | 65 s |
| `news-search-via-tavily` | 30 s | 35 s |
| `gif-search-via-klipy` | 15 s | 20 s |

## Magician product adapter

`governed_runtime.rs` dispatches retained packages; the compatibility
`Primitive` catalog stays model-facing.

- Google Workspace (calendar, gmail, sheets and presto-prefixed twins) runs
  through the coordinator with fresh profile status and expected-identity
  checks before authorization-before-auth; environment templates use the public
  alias (e.g. `{account}`).
- Static/multi-secret contracts take a profile-free branch through the same
  coordinator. Optional auth (`github-search`) injects `GITHUB_TOKEN` only when
  present; `at_least_one` (`video-generation-via-veo`) takes `VEO31_API_KEY` or
  `GEMINI_API_KEY`. Direct HTTP adapters use bounded in-process HTTPS, keep
  credentials out of argv and deny redirects.
- A nonzero CLI result's stderr
  `{"ok":false,"error":{"code":"...","message":"..."}}` becomes a typed failure
  when ≤ 16 KiB, `ok` is false, code matches `[a-z0-9_]{1,64}` and message is
  1–8 KiB without control characters; otherwise an opaque step failure.
- Schema-only fixtures:
  `magician/tests/fixtures/tool_runtime_legacy_contracts/`, outside every
  runtime skill root.

## Canary vocabulary

`canary` (`tool-runtime.canary.v1`): one cheap read-only invocation with
expectations (exit-zero alone is refused), or an exemption with a reason
(messaging, checkout, operator GUI); `every_tool_skill_declares_a_canary`
fails on a missing block. `max_cost_microunits` must name `max_cost_commodity`
(e.g. Exa `usd`, Tavily `tavily_credit`). Tiers: `free`, `cheap`, `expensive`;
the default live lane runs free and cheap.

- `make test-tool-skills-live`, narrowed by `TOOL_CANARY_TIER` /
  `TOOL_CANARY_SKILLS`, e.g.
  `make TOOL_CANARY_TIER=free TOOL_CANARY_SKILLS=web-via-tinyfish test-tool-skills-live`.
  Unknown or empty selections fail; a missing credential is SKIPPED, never PASS.
- `make test-tool-skills`: provider-free contract suite. Magician-owned
  `tool_skill_contract` and `phase7_migration` run in `make test-phase5g-local`;
  MagicRun keeps its own tests.

## Source inventory and generation

Three generators build the checked-in Phase 0 chain without copying credential
or environment values, command bodies, defaults, skill prose or adapter
contents. Each takes `generate` or `check` (keep `check` green).

An active tool is a direct `skillshub/` child with exactly one catalog source:
governed `runtime_contract` (optional `runtime_actions`) in `SKILL.md`, or
legacy `tool_schema.yaml`; both is `duplicate_active_catalog_sources`. Google
Workspace is atomic: all six packages and exactly 98 actions
(`EXPECTED_GOOGLE_WORKSPACE_ACTIONS`).

```bash
D=data/tool-runtime-inventory R=docs/components/tool-runtime-core
make magicrun-inventory ARGS="generate --skill-root skillshub --source-label skillshub \
  --json $D/phase0-source-inventory-v1.json --report $R/phase0-source-inventory.md"
make magicrun-classification ARGS="generate --inventory $D/phase0-source-inventory-v1.json \
  --classification $D/phase0-classification-v1.yaml \
  --json $D/phase0-classification-v1.json --report $R/phase0-classification.md"
make magicrun-replay ARGS="generate --skill-root skillshub \
  --inventory $D/phase0-source-inventory-v1.json \
  --classification-manifest $D/phase0-classification-v1.yaml \
  --classification $D/phase0-classification-v1.json \
  --json $D/phase0-replay-fixtures-v1.json --report $R/phase0-replay-fixtures.md"
```

The hand-authored `data/tool-runtime-inventory/phase0-classification-v1.yaml`
lists every skill id and `(skill, adapter path)` pair exactly: `work-modules` is
a `skill_adapter`; `web-via-tinyfish` is `external_cli` with no adapters (its
`bin/node` and `bin/tinyfish` are host-local symlinks the scanner skips). Replay
emits one credential-free fixture per product action.
`tool-runtime-core/tests/phase1_qualification.rs` locks the compiled-catalog
corpus by SHA-256; `scripts/eval-tool-runtime-phase0-agentic-live.py` rebuilds
provider schemas from `skillshub` and records first actions without dispatching.
Migration-only artifacts: `docs/archive/components/tool-runtime-core/`.

## Design Notes

- `RegistryService.tools` is `RwLock<HashMap>`, so `register_tool()` /
  `unregister_tool()` work through `Arc` without `&mut self`.
- Trusted computing boundary: crate-internal code can copy a borrowed byte
  slice before a sealed callback returns; the type system does not sandbox
  Magician.
