# Tool Runtime Core

`tool-runtime-core` `0.1.81` is the provider-neutral skill runtime: one
`SKILL.md` package compiles to a typed catalog, a scoped Auth Broker, and a
governed child process. Magician dispatches a tool that retains a
`SkillRuntimePackage` through
`magician/src/magician_v2/execution/primitive_dispatch/governed_runtime.rs`.
Once that governed owner is selected, the call cannot fall back to
`CliTemplateDispatcher`; the template dispatcher remains only for packs
that still lack a runtime package.

A package is a `skillshub/<id>/` child whose `SKILL.md` contains
`runtime_contract:`. Dual governed + `tool_schema.yaml` sources fail closed.
`skill_type: facade` is not executable. Procedure, agent, and personality skills
are not counted as tools.

`work-modules` and `web-via-tinyfish` are in the manifest;
`web-via-tinyfish`'s `bin/node` and `bin/tinyfish` are host-local symlinks, and
the scanner does not inventory symlink bins as adapter files.

## Canonical References

The source lives in [MagicRun](https://github.com/MagicBeansAI/MagicRun). All
Magician consumers (including `magicvault-effect`) inherit the exact root
workspace pin, MagicRun `c65fbba` (`0.1.81`), so only one `tool-runtime-core`
resolves. Magician owns authorizers, credential resolvers, audit sinks and
execution dispatch; there is no second executor. `source_bytes` covers the
backends in product-owned attestations, so a backend change rotates source-bound
approvals and the OS-jail runtime digest (and any installed OS-jail lock).
Portable runtime tests run through `make test-magicrun` (an external
dependency's tests are not selected by the product `--workspace` run).

Jail capabilities Magician relies on (see [Process jail](#process-jail) and
[app-os-jail-egress](../magician/app-os-jail-egress.md)):

- **macOS batch launch** uses native `posix_spawn` for non-jailed children. The
  declared-artifact collector is fail-closed outside Unix; the portable library
  compiles on Windows.
- **Descriptor hygiene**: every jail launch, the in-jail helper and the
  forwarder's child mark all descriptors above 2 close-on-exec (keeping only the
  Linux exec-status channel) via `close_range`, `/proc/self/fd` or macOS
  `proc_pidinfo(PROC_PIDLISTFDS)` (8 KiB stack buffer, scan fallback). Unjailed
  Linux batch children get the same marking. Why: a jailed command must not
  inherit host descriptors.
- **Linux in-jail helper** (root-owned, required: `JailHelperUnavailable`
  otherwise) execs the command and sets `RLIMIT_NPROC` inside the user namespace,
  reporting a pre-exec refusal over a private status channel a jailed command
  cannot fake. Linux directory identity adds birth time off overlay filesystems.
  Jail limits include `max_tasks` (default 256, at most 1024).
- **`stage_input_file`** writes content no-follow and quota-checked into the
  jail's private workdir; the app jail stages read-only file inputs through it.
- **`GovernedJailInterpreter` / `with_interpreter`**: the jail execs only the
  host's pinned, trusted, digest-checked Python (`-I -S -B <script>`), so
  reviewed single-file Python skills run in the app jail. Workdir files cannot be
  mapped executable.
- **`strict_app_with_brokered_egress`**: opt-in jail whose only network is one
  host-owned HTTP CONNECT broker (macOS: IPv4 TCP to `localhost:<port>`; Linux: a
  caller-owned unix socket relayed by `magicrun-jail-egress-forwarder`).
  Magician's app OS jail (`apps/os_jail.rs`) uses it for a skill whose reviewed
  source declares one HTTPS host (`metadata.magician.app_egress`), with a broker
  started for that call; every other app tool keeps the strict no-network
  profile.
- **Declared exec roots** (`GovernedProcessJail::with_exec_roots`) run an
  installed program in place, reading/executing only declared roots less
  excluded subpaths and writing only the private workdir; `stage_input_file`
  returns `in/<name>` in that mode. Errors: `InvalidExecRoots`,
  `ExecRootsRefused` (not dispatched), `JailTeardownIncomplete` (macOS teardown
  could not prove every jailed process dead; the coordinator reports a
  post-dispatch `ProcessFailed`, which the app jail settles as effect-uncertain,
  never success). Magician calls `sweep_stale_jail_members()` once at boot.

Magician's app jail uses exec roots for in-place skills
(`AppOsJailArtifactKind::InPlaceSkill`): roots derive from the skill package and
its runtimes; data and home directories are forbidden; the exec-roots profile
identity is in the lock; exact `python3` scripts run pinned Python `-s -B` (see
[app-os-jail-egress](../magician/app-os-jail-egress.md#in-place-skills-inplaceskill-app_in_place_skill_v1)).
Magician's own checks before `GovernedJailExecRoots`: runtime roots under home
must be recognised versioned layouts (never home or a direct child), `etc`/`var`
are excluded from prefix roots, home dot-directories are forbidden, and a key
file travels in a private credential directory declared as one more exec root
(MagicRun offers no read-only extra root). A contract's fixed
`requires.environment` reaches the child only through `provide_fixed` after
MagicRun validates the exact effective contract at call time, so the loader and
interpreter injection denylist (`DYLD_*`, `LD_*`, `NODE_OPTIONS`, `PYTHONPATH`,
...) applies; the jail strips `DYLD_*`/`LD_*` again at launch.

App-jail credentials: a skill with the supported static-secret contract
(environment injection only, declared egress host, owner-ticked grant) gets a
`new_with_minimum_present` plan built exactly as the governed runtime's
profile-free path builds it, resolved by `ScopedCredentialMaterialAdapter` with
the egress host as the vault grant's domain; other skills run
`CredentialPreparationPlan::unauthenticated`. Every reviewed action lowers
through one `lower_reviewed_action` path. The app catalog and jail compiler share
one narrowed contract: workspace working-directory controls are removed (the
child runs in a private empty directory). An explicit `ordinary` approval
carries no extra authority; other required approvals, grants and resource scopes
still block admission.

- [Tool Runtime Changelog](https://github.com/MagicBeansAI/MagicRun/blob/c65fbbaac46a70f8a1247307fc7daa5ded018275/tool-runtime-core/CHANGELOG.md)
- [Phase 0A Source Inventory](phase0-source-inventory.md)
- [Phase 0B Auth And Ownership Classification](phase0-classification.md)
- [Phase 0C Credential-Free Replay Fixtures](phase0-replay-fixtures.md)
- [SKILL.md spec](../magician/skills-spec.md)
- [Skill authoring](../magician/skills-authoring.md)
- [MCP client](../magician-mcp-client/README.md)
- [Workspace Architecture](../../ARCHITECTURE_V2.md)
- [Quick Start Config Notes](../../quickstart.md)
- Archived: commerce/MCP ownership,
  commerce policy fixtures,
  SDK/Auth Broker handoff,
  offline baseline,
  live-agentic baseline,
  Phase 7 migration ledger,
  universal skill runtime plan

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

Public modules live in `tool-runtime-core/src/`. Magician owns product adapters
(secret store, lifecycle process owner, audit journal, workspace-path broker).
`magician-mcp-client` owns official-SDK MCP transport.

## Vocabulary

`tool_runtime_core::manifest` is the contract nested at
`metadata.magician.runtime_contract`. Schema
`tool-runtime.skill-runtime.v1` is deny-unknown-fields. General Magician
metadata (`skill_type`, `user_invocable`, `install_hint`, `expose`) can coexist.

`metadata.magician.expose` is audience only and grants no execution authority.
Parser default is `agents: true`, `apps: false`. Magician app admission for
curated / reviewed / compiled sources treats an omitted `expose` block as
eligible (`apps: true`); only an explicit `expose.apps: false` is refused.

The contract distinguishes:

- protocol: CLI vs MCP
- CLI interaction: `batch` vs `pty`
- MCP transport: stdio vs Streamable HTTP
- auth kind: `none`, `secrets`, `cli_profile`, `oauth_session`,
  `browser_profile`, `native_permission`, `delegated_credential`
- auth requirement: `none`, `optional`, `required`, `at_least_one`
- profile selection: `none`, `selectable`, `fixed`, `implicit`
- storage: `none`, `scoped_directory`, `cli_owned`, `browser_profile`,
  `operating_system`, `ephemeral_grant`
- reference-only secret bindings and typed injection sources/targets
- CLI-owned scoped storage and exact-token lifecycle hooks
- profile-registry-owned expected identity
- policy floor: approval, grants, resource scopes, Resource Authority

Unordered sets serialize deterministically; argv and path-token sequences
retain order.

Unauthenticated CLI:

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

`requires.bins` names executables only. `requires.entrypoint` picks the process
entry when more than one reviewed binary is required. `requires.environment`
is a finite public child environment; credential material uses `auth.injections`.

## Parser

`parse_skill_runtime_package` takes a complete `SKILL.md` string and returns a
typed package (contract, optional typed actions, non-authoritative catalog
hints), `None` for a valid legacy skill, or a stable `ManifestParseErrorCode`.
`parse_skill_runtime_contract` is the contract-only wrapper;
`parse_skill_magician_extension<T>` deserializes a deny-unknown product block
under `metadata.magician` under the same limits.

Limits: source 1 MiB, frontmatter 512 KiB, YAML depth checked before
deserialization, no anchors/aliases/custom tags, parsed tree depth 64 and 20,000
nodes. Diagnostics are fixed text plus line/column; authored scalars are never
copied. Unsupported schema versions are diagnosed before v1 deserialization.
Filesystem, symlink and package-root ownership belong to the loader.

Catalog hints (`runtime_catalog`) are presentation only (categories, composition
category, optional `profile_parameter` / `mcp_endpoint_parameter`, timeout and
working-directory control names, spend metadata) and never create profile, path
or spend authority. A selectable-profile package may declare one
`runtime_catalog.profile_parameter` with a portable public name and optional
finite alias set (rejected on fixed/implicit profiles; a declared default must
be in a nonempty finite set). Magician translates the alias back to canonical
`profile` before selection.

## Validator

`validate_skill_runtime_contract` turns a parsed v1 contract into a borrowed
`ValidatedSkillRuntimeContract`; the private field means nothing downstream can
accept a merely deserialized contract. Validation is deterministic, fail-fast
and nonrecursive.

- Binds one exact CLI executable; checks stdio MCP executable requirements;
  prohibits local executable declarations for remote MCP.
- Fixed argv is bounded; inline shell/interpreter source is rejected.
- Streamable HTTP accepts HTTPS and loopback HTTP only, with no URL credentials
  or fragments. MCP allow/deny overlap is valid (deny wins).
- Covers all seven auth kinds, requirement/provider/profile/storage
  compatibility, reference-only secret bindings, fully consumed injection
  sources, collision-free targets, lifecycle ownership, expected-identity
  selectors, runtime ceilings, stdin semantics and policy references. Remote MCP
  cannot inject into a local process. Relative paths are canonical and
  traversal-free. Process-control names (`PATH`, `HOME`, loader hooks, language
  runtime options) cannot be injection targets.
- `runtime.limits.memory_bytes` is 64 MiB–8 GiB and CLI-batch only (PTY/MCP
  declaring it fail closed). Stream and timeout ceilings are non-zero and at most
  256 MiB / 24 h. Lifecycle timeout ceiling is 15 minutes.
- `ConfigDirectory` injections accept `ProfileAuthRoot`, or `Secret` only for
  `provider: minimax` + `name: MMX_CONFIG_DIR`. Non-Unix hosts reject
  `ScopedFile` and `ConfigDirectory`.

## Catalog synthesis

`synthesize_runtime_catalog` accepts only the validation proof.

- **CLI** emits exactly one `<skill>.run` action with a closed JSON Schema and a
  separate non-secret execution binding. It always requires exact `args`,
  exposes `profile` only for selectable profiles (required when no default),
  exposes stdin only when declared, and includes working-directory authority
  and timeout ceilings. Executable, prefix, auth roots, environment and policy
  are never model-controlled.
- **MCP** emits a skill-owned discovery seed (remote tools are not yet known)
  retaining transport, limits, allow/deny, local namespace and hard ceilings:
  512 tools, 256-byte names, 8 KiB descriptions, 256 KiB / depth 32 /
  10,000-node schemas, 8 MiB aggregate catalog. Local names are exactly
  `<skill namespace>.<remote name>`, no slugging.

Secret references, injection bindings, lifecycle argv and identity selectors
never enter the model schema.

## Typed action overrides

`compile_typed_action_overrides` takes the validation proof plus a versioned
override set (`tool-runtime.typed-action-overrides.v1` or `.v2`). A CLI override
replaces the generic `args` action with at most 512 typed actions. Typed
overrides are CLI-only; MCP refinement waits for bounded SDK discovery.

- Parameters: nonrecursive strings, integers, numbers, booleans, string arrays,
  bounded opaque JSON objects, and `workspace_path`. Each v1 parameter has one
  mapping: positional, flag, boolean-flag, repeated-flag, passthrough,
  JSON-flag, or literal. The compiler proves worst-case argv fits item and
  combined-byte ceilings. Names cannot replace reserved controls (`profile`,
  `stdin`, `working_dir`, `timeout_secs`). An action timeout may only lower the
  manifest ceiling.
- **v2** adds package-wide `input_delivery: canonical_json_stdin` for HTTP-only
  adapters: no argv mappings, raw stdin removed from the model schema, every
  parameter validated, trusted defaults applied, one deterministic object bound
  to the manifest's required bounded stdin. Explicit JSON `null` is
  `InvalidParameter`.
- **`argument_rules`** deny a finite set of final-argv prefixes or require the
  token after a constrained prefix to be in a finite allowlist. They are
  bounded, non-overlapping, checked against final inert argv before
  authorization, and cannot add, rewrite or interpolate tokens. Higgsfield uses
  them to deny the CLI-owned `auth` namespace and limit `workspace` to
  `list`/`status`; the runtime has no Higgsfield-specific branch.
- **`workspace_path`** is a semantic type, not authority. Such actions always
  carry the `workspace` resource scope; create-file/create-directory also
  require `delegated_workspace_write` approval. Magician binds an exact
  broker-owned read file, or confines direct calls to normalized relative paths
  under the scope's `workdirs/`. Absolute paths, `..`, non-normal components,
  symlink traversal, missing read files and existing create destinations fail
  before argv lowering.
- A **`runtime_control`** mapping participates in validation, defaults and
  catalog projection but emits no argv or replay mapping; the product controller
  must consume it before dispatch.
- **Policy precedence is monotonic**: per-action policy can only add approval
  classes, grants, scopes and authority references (bounded set union). Prefixes
  overlapping Auth Broker lifecycle paths fail closed. Interpreter-backed
  manifests must fix a script or module before model arguments.

## Profile registry and selection

- **`credential_profiles`** — logical identity: principal, workspace, provider,
  alias, optional MCP OAuth resource/issuer binding. Endpoints are canonicalized
  (HTTPS or loopback HTTP; no userinfo, query, fragment). Snapshots are exact to
  one scope, capped at 256 profiles, reject duplicate keys, allow at most one
  default per provider binding; disabled profiles cannot be defaults.
- **`scoped_paths`** — a typed capability beneath one trusted scope-container
  root: exact principal/workspace names, the fixed `auth` directory, and one
  validated profile-directory component bound to a logical key. Relative roots,
  traversal, aliases, symlinks, cross-filesystem mounts, missing targets and
  oversized scans fail closed. Scope ancestry must be owner-accessible and not
  group/other writable; auth and profile roots must be owner-only. Physical
  paths come only through `revalidated_path`; `ScopedPath` is not publicly
  constructible or serializable.
- **`credential_profile_store`** — metadata only, in
  `.credential-profiles.v1.json` beneath a verified owner-private auth root. A
  bounded `flock` serializes writers; each mutation rereads under the lock,
  checks expected revision, writes an owner-only staging file and publishes with
  descriptor-relative atomic rename. Reads never create the store. Failure after
  rename is `commit_state_unknown`. Magician owns the Google Workspace YAML
  projection and `gws-<alias>` naming; the core accepts only opaque
  compatibility references. Verified legacy directories report `unknown`,
  missing ones `missing`, unsafe ones fail closed.
- **`profile_selection`** — turns manifest policy plus the single permitted
  model input (a selectable alias) into a scope/provider/binding-exact decision.
  Precedence: explicit request, contract default, scoped registry default. Once
  any source names an alias, missing or disabled fails closed. Fixed profiles
  reject any model alias; none/implicit reject selection and do not read the
  registry. Public selection must use a `CredentialProfileRegistry`.
- **Readiness** is separate from selection: ready, verification required,
  authentication required, in progress, or blocked, keeping exact `AuthState`.
  Legacy directory presence stays `unknown` / verification-required.

## Credential preparation and materialization

- **`credential_preparation`** converts a selection proof, the auth strategy and
  at most 64 typed material slots into a deterministic plan (sorted, deduped;
  1 MiB per value, 4 MiB per preparation). Missing, empty, undeclared,
  duplicate, oversized, incompatible or cross-scope material is rejected before
  the consumer runs. A resolver gets exactly one batch call.
- Resolved values live in exact-size sealed storage: owner, entries, view and
  sink are non-`Clone`, non-`Debug`, non-serializable; only the crate-internal
  injection layer can borrow bytes; the owner zeroizes on every exit including
  panic unwind.
- **`CredentialSecretReference`** is the metadata-only bridge to Magician's
  scoped SecretStore (≤ 512 bytes; no absolute paths or traversal). Magician's
  `ScopedCredentialMaterialAdapter` binds the plan to exact
  profile/provider/scope/route. Duplicate bindings to one reference share one
  policy decision, grant, redemption and usage charge. Delegated grants carry
  exact principal, workspace, provider, agent, tool, action, optional domain and
  expiry; a batch of at most 64 validates under one lock. A materialized private
  per-skill `.env` can satisfy only missing vault references (vault wins, file
  capped at 1 MiB, unsafe permissions fail closed). Canonical record presence
  stays authoritative during a vault outage.
- **`CredentialInjectionPlan::compile`** produces sorted metadata-only bindings
  for environment, stdin, scoped-file and config-directory targets. There is no
  argv target, and every prepared secret must be consumed exactly. Children start
  from a finite name-only baseline, not the parent environment. Case-insensitive
  collisions, duplicate stdin, and remote Streamable HTTP credential injection
  fail closed. Remote MCP OAuth compiles only when the binding's resource URL
  exactly matches the endpoint.
- **`ChildEnvironmentValues`** accepts values only for the compiled baseline:
  UTF-8, NUL-free, ≤ 64 KiB each and 256 KiB aggregate; stdin is bounded
  binary. Both zeroize when the callback returns or unwinds.
- **Exact-value redaction** runs on raw output bytes, prefers the longest
  overlapping credential, and uses a one-byte sentinel absent from every
  pattern. It is raw-byte containment, not detection of encoded secrets.
  Persistence redaction scans a batch (≤ 16 records, 8 MiB aggregate, 1 MiB per
  record) as one stream so a credential split across records is still caught.
- **`CredentialScratchAuthority`** opens one private container beneath a scope
  capability. Session names are digests; directories are `0700`, secret files
  create-once `0600`. Profile directories are projected, never copied. Cleanup
  is explicit and retried; drop covers success, cancellation and unwind.
  Recovery refuses to run while calls are active, follows no symlinks, and is
  bounded (256 sessions, 1,024 entries, 32 levels).
- For `ProfileAuthRoot` sources, named `ConfigDirectory` entries stay as
  materialized directory handles; only non-profile sources inject directory
  paths into the environment (MiniMax `MMX_CONFIG_DIR`).

## Credential lifecycle

- **`credential_lifecycle`** compiles one declared CLI auth operation
  (`status`, `login`, `logout`, `refresh`) using the fixed executable and
  broker-owned hook argv, not the model-facing prefix. The selected profile must
  match key, provider, binding, selection mode and fixed alias. Disabled
  profiles may be inspected or logged out but not logged in or refreshed. Plans
  are not serializable.
- **`credential_lifecycle_observation`** interprets one bounded result. Status
  hook and observation declaration come together: exact exit-code mapping, or
  JSON rules (equality, presence, absence, string-set), all conditional except a
  ready rule that relies solely on a declared expected-identity selector.
  `json_pointer` compares exactly; `json_pointer_ascii_case_insensitive` must be
  chosen explicitly. Identity mismatch projects `identity_mismatch`; a provider
  that cannot expose identity reports `provider_unverified`. Login/refresh
  success requires a fresh status observation; logout invalidates verified
  status.
- **`credential_status_cache`** keeps only a sanitized ready projection in
  memory, bound to lifecycle plan, process epoch, policy revision, profile
  revision, auth-directory identity and monotonic expiry (256 entries, 5-minute
  TTL). Only `ready` with `matched` or `not_declared` identity is cached. A
  consume-once observation ticket records the mutation generation so an older
  in-flight probe cannot restore readiness. Hits are non-authorizing.
- **`credential_lifecycle_coordinator`** is the single-owner state machine for
  one login plan. One lease per selected profile key (or implicit
  scope/provider/binding); starting login verifies the cache epoch and
  invalidates readiness. The lease projects running, cancelling,
  recovery-required, or typed pending (browser-callback, device-code, OTP, QR,
  operator-required) without owning URLs, codes or QR bytes. Caps: 256 leases
  and epochs; deadline ≤ 15 minutes (default 5); idle tolerance ≤ 5 minutes.
  Success returns only `fresh_status_required`.
- **`credential_lifecycle_execution`** seals one plan to the validated contract,
  auth-directory identity, finite clean environment, and executable resolved
  from that environment's bounded absolute `PATH`. The file handle is retained;
  ≤ 256 MiB is copied and its SHA-256 must match bind-time bytes. Magician's
  process owner uses no shell, an empty environment, bounded pipe readers for
  batch, a clean-environment PTY for interactive login (e.g. GWS), and an
  isolated process group per child, under a process-wide 16-lifecycle ceiling.
- Only an implicit CLI-profile contract with `cli_owned` storage receives
  `HOME`. Pack-level `interaction: pty` on that path fails closed (agents use
  `run_coding_task`, operators use Developer Mode PTY). Higgsfield is the batch
  CLI-owned example.

## Strategy adapters and MCP catalog

- **`strategy_adapter`** projects MCP, browser-profile, native-permission and
  delegated-credential contracts through one transport-free vocabulary
  (`AuthState`, `AuthKind`, `AuthRequirement`, `ApprovalClass`). Protocol and
  auth ownership are independent. Optional/conditional auth needs an explicit
  per-invocation demand. Unknown post-dispatch delivery is never auto-retried.
  `ready` is observational and never authorizes dispatch without a fresh
  adapter-owned check.
- **`mcp_catalog_policy`** compiles a validated MCP skill into a local policy
  boundary before any remote descriptor is published. Local tool entries may add
  description, risk class, approvals, grants, scopes and Resource Authority, but
  cannot subtract the skill-wide floor. An eligible tool without a local entry is
  `unclassified` and requires `conditional_external_side_effect` approval;
  remote read-only/destructive annotations never change that.
- **`mcp_catalog_projection`** exposes five stable product actions per
  official-SDK MCP package: `status`, `auth_start`, `list_tools`, `clear_auth`,
  `call_tool`. Live remote schemas stay SDK discovery data. Default action
  timeout 90 s.
- **`browser_profile_adapter`** binds opaque dispatch authority to a profile
  identity plus browser session class and controller session revision. Cookies,
  storage, CDP credentials and profile contents never enter the contract.
  Dispatch is two-step prepare/revalidate.
- **`native_permission_adapter`** projects Accessibility, Screen Recording,
  Microphone, Notifications or Automation from a metadata-only OS observation.
  A serialized `ready` is never authority; the native owner re-observes at use.

## Governed execution

- **`governed_execution`** admits one invocation against the validated CLI
  contract and a trusted local resource policy. The intent is move-only,
  non-debuggable, and not launch authority. Shell metacharacters stay inert.
- **`governed_execution_authority`** resolves the executable from a supplied
  clean environment baseline and the fixed binary name; the regular file is
  bounded, held open, hashed and copied into a private launch snapshot
  (≤ 256 MiB; provenance is length + SHA-256). Workspace/output-root cwd
  capabilities are owner-bound canonical directories; every component is
  symlink-free, same-device, owner-controlled and not group/world-writable (≤ 64
  components). PATH symlinks may be followed only to bind exact regular-file
  bytes. Self-relative wrappers needing companion files are ineligible without a
  separately declared executable-bundle authority.
- **`governed_batch_process`** launches the snapshot directly (never via shell)
  with the fixed prefix, admitted tokens, empty base environment plus sealed
  child environment, optional bounded stdin and descriptor-bound cwd. Each child
  owns a new process group. Process-wide reservations: 32 concurrent processes,
  512 MiB streams / 640 MiB retained results (RAII). Helper threads use 256 KiB
  stacks. On POSIX, `waitid(..., WNOWAIT)` pins the exited leader's pid/group
  while leftover members are killed, then the leader is reaped.
- **`governed_pty_process`** is a distinct sealed capability for
  `interaction: pty`; neither executor falls back to the other. `portable-pty`
  has no pre-exec `fchdir`, so launch revalidates the retained directory
  descriptor and pathname immediately before spawn. The interaction bridge sees
  only incrementally redacted bytes.
- **`governed_execution_result`** collects only predeclared portable relative
  files through `openat`. Links, special files, unsafe parents, owner/filesystem
  drift, concurrent mutation and size/depth/count overflow fail closed;
  undeclared files are ignored. Stdout, stderr and artifacts stay zeroizing until
  one length-preserving redaction pass over all of them as one stream. A
  credential in an artifact rejects the whole artifact set. Files from a
  non-success terminal are `partial`.
- **`governed_execution_coordinator`** is authorization-before-auth. The
  authorizer sees call identity, inert argv, cwd, stdin presence/digest, policy
  floor, contract digest and credential-plan digest. Evidence must bind the exact
  request and meet every required approval, grant, scope and Resource Authority
  before profile filesystem authority is issued or a resolver called. Same-thread
  re-entry fails. Every terminal path commits one metadata-only audit receipt.
  Batch/PTY entry points reject an interaction mismatch before authorization.

Platform limits stay explicit: a descendant that calls `setsid` leaves the
process group and needs the jail; a blocking authorizer, resolver or audit sink
blocks its thread (Rust cannot preempt it); a hostile rename in the final PTY
spawn interval is the jail's job.

## Process jail

`governed_process_jail` (schema `tool-runtime.governed-process-jail.v1`) is
fail-closed OS containment for non-interactive children. It does not resolve
executables, lower input, prepare credentials, authorize, or retain output. A
jail is a move-only launch profile that wraps only the exact executable snapshot
and the private invocation directory it owns. Missing containment is a
pre-dispatch refusal; there is no pass-through.

- Platforms: macOS `/usr/bin/sandbox-exec`, Linux `bwrap`. Launchers must be
  real absolute root-owned files with no group/other write on any ancestor.
- Hard maxima: 256 files, 16 MiB per file, 64 MiB total, 16 processes, 64 open
  files, 300 s wall and CPU, 1 GiB memory. Defaults: 30 s wall/CPU, 512 MiB.
  Profile text ≤ 32 KiB.
- Audit reports specific guarantees, not a "sandboxed" boolean: direct network,
  ambient environment and host writes denied; private workdir; exact executable
  snapshot; wall/CPU/memory/file/output ceilings. `process_ceiling` is true only
  on Linux (PID/user namespace plus `RLIMIT_NPROC`); macOS reports its sampled
  process watchdog instead.
- The macOS profile is deny-default. Darwin libc must read the root entry once
  at startup, so it permits only `(allow file-read-data (literal "/"))` — no
  traversal, metadata or subtree. No Mach services, network or extra executable
  authority. HOME/PATH/TMPDIR/TMP/TEMP point at the private workdir;
  `SSL_CERT_FILE` is removed.
- The sampler reaps the owned child at the top of every tick before enforcing a
  sampled limit, so a fast exit returns its real status. Hard kernel limits
  apply from exec.
- The invocation cwd must be the jail-owned directory; the jail holds the
  `TempDir` through execution and removes it on every path.

## Memory limits

Batch CLI contracts may declare `runtime.limits.memory_bytes` (64 MiB–8 GiB).
It is part of contract/authorization binding, enforced before exec where
possible, and reserved against an 8 GiB process-wide budget.
`memory_limit_enforcement()` reports the mechanism;
`process_memory_limits_are_enforceable()` is true when either applies. A host
that cannot hold the limit refuses the contract.

| Mechanism | Where | How the ceiling is held |
| --- | --- | --- |
| `KernelAddressSpace` | `RLIMIT_AS` is a real resource | `setrlimit(RLIMIT_AS, …)` in `pre_exec`; the kernel refuses the allocation. |
| `ParentFootprintWatchdog` | Darwin | The executor samples the process group's `ri_phys_footprint` every `POLL_INTERVAL` and kills the group on breach. |
| `None` | neither | Contract refused at construction. |

The watchdog is a detection bound, not an allocation bound: a child may exceed
its ceiling for up to one interval, and a spike between samples is unseen.
Darwin needs it because `RLIMIT_AS`/`RLIMIT_RSS` are one resource there and any
finite value returns `EINVAL`; `RLIMIT_DATA` only bounds `brk`, which `mmap`
arenas bypass. A breach reports
`GovernedExecutionTerminal::MemoryLimitExceeded` →
`CredentialExecutionFailure::ProcessMemoryExceeded`, distinct from timeout.

## Governed kill versus inner provider budget

Credentialed HTTP adapters carry two timeouts that cannot reference each other:
provider deadlines inside the adapter, and the wall-clock kill
`runtime.limits.timeout_secs` in the frontmatter. The kill is a backstop; if it
fires first, the adapter's own timeout/retry/fallback handling becomes
unreachable.

Invariant: `governed kill >= inner worst case + margin` (margin 5 s), written at
both sites (YAML comment above `timeout_secs`; `INNER_WORST_CASE_SECS` /
`GOVERNED_KILL_MARGIN_SECS` / `GOVERNED_KILL_CEILING_SECS` in each adapter) and
checked by
`phase7_migration.rs::credentialed_adapter_governed_kill_outlasts_its_inner_provider_budget`
plus per-skill `test_governed_kill_outlasts_the_inner_provider_budget`.

| skill | inner provider budget | governed kill |
| --- | --- | --- |
| `websearch-via-claude` | 300 s (60 s × 5 pause-turn) | 305 s |
| `meme-generation-via-imgflip` | 25 s (10 s catalog + 15 s caption) | 30 s |
| `websearch-via-openai` | 60 s | 65 s |
| `semantic-websearch-via-exa` | 60 s | 65 s |
| `news-search-via-tavily` | 30 s | 35 s |
| `gif-search-via-klipy` | 15 s | 20 s |

A socket deadline bounds each operation, not total time, so the governed kill is
the only total bound.

## Magician product adapter

`governed_runtime.rs` is the live dispatch path for a retained
`SkillRuntimePackage`; the compatibility `Primitive` catalog stays model-facing,
and `CliTemplateDispatcher` is never constructed as a same-call fallback.

- Google Workspace (calendar, gmail, sheets, and the presto-prefixed twins)
  loads from governed packages: 98 actions through the coordinator, with fresh
  profile status and expected-identity verification before
  authorization-before-auth. Selectable environment templates use the public
  alias (e.g. `{account}`).
- Static/multi-secret contracts take a profile-free branch through the same
  coordinator. Optional auth (`github-search`) injects `GITHUB_TOKEN` only when
  present; `at_least_one` (`video-generation-via-veo`) accepts `VEO31_API_KEY` or
  `GEMINI_API_KEY`. Direct HTTP adapters use bounded in-process HTTPS, keep
  credentials out of argv, and deny redirects.
- Nonzero CLI results may carry `{"ok":false,"error":{"code":"...","message":"..."}}`
  on stderr. The capability invoker keeps it as a typed failure when the
  envelope is ≤ 16 KiB, `ok` is false, code matches `[a-z0-9_]{1,64}`, and
  message is 1–8 KiB without control characters; otherwise it stays an opaque
  step failure.
- Schema-only compatibility fixtures live under
  `magician/tests/fixtures/tool_runtime_legacy_contracts/`, outside every runtime
  skill root.

## Canary vocabulary

`canary` (`tool-runtime.canary.v1`): a skill declares one cheap read-only
invocation and its expectations, or an exemption with a recorded reason
(messaging, checkout, operator GUI). `every_tool_skill_declares_a_canary` fails
on a missing block. A canary asserting only exit-zero is refused. A
`max_cost_microunits` ceiling must name `max_cost_commodity` (e.g. Exa `usd`,
Tavily `tavily_credit`). Cost tiers: `free`, `cheap`, `expensive`; the default
live lane runs free and cheap.

- `make test-tool-skills-live` sweeps the selected tier; narrow with
  `TOOL_CANARY_TIER` / `TOOL_CANARY_SKILLS` (make/env variables), e.g.
  `make TOOL_CANARY_TIER=free TOOL_CANARY_SKILLS=web-via-tinyfish test-tool-skills-live`.
  Unknown or empty selections fail; a skill with an absent credential reports
  SKIPPED, never PASS.
- `make test-tool-skills` runs the provider-free contract suite. The
  `tool_skill_contract` and `phase7_migration` suites are Magician-owned (they
  inspect built-in skills and adapters) and are included in
  `make test-phase5g-local`; MagicRun keeps its own unit and qualification
  tests.

## Source inventory and generation

Three binaries generate the checked-in Phase 0 chain. They never copy
credential values, environment values, command bodies, arbitrary defaults,
skill prose or adapter contents. Set `CARGO_TARGET_DIR` as for other
Cargo-backed targets.

An active tool is a direct `skillshub/` child with exactly one executable
catalog source: a governed `runtime_contract` (optional `runtime_actions`) in
`SKILL.md`, or a legacy `tool_schema.yaml`. Both is
`duplicate_active_catalog_sources`. Google Workspace is an atomic coverage
group: a production catalog must compile all six packages and exactly 98 actions
(`EXPECTED_GOOGLE_WORKSPACE_ACTIONS`).

```bash
make magicrun-inventory ARGS="generate \
  --skill-root skillshub --source-label skillshub \
  --json data/tool-runtime-inventory/phase0-source-inventory-v1.json \
  --report docs/components/tool-runtime-core/phase0-source-inventory.md"
# same ARGS with "check" verifies
```

Classification compiles the hand-authored
`data/tool-runtime-inventory/phase0-classification-v1.yaml` against that JSON;
every skill id and `(skill, adapter path)` pair is listed exactly (`work-modules`
is a `skill_adapter`; `web-via-tinyfish` is `external_cli` with no inventoried
adapters). `check` must stay green after any generate.

```bash
make magicrun-classification ARGS="generate \
  --inventory data/tool-runtime-inventory/phase0-source-inventory-v1.json \
  --classification data/tool-runtime-inventory/phase0-classification-v1.yaml \
  --json data/tool-runtime-inventory/phase0-classification-v1.json \
  --report docs/components/tool-runtime-core/phase0-classification.md"
```

Replay emits one credential-free fixture per stable product action:

```bash
make magicrun-replay ARGS="generate \
  --skill-root skillshub \
  --inventory data/tool-runtime-inventory/phase0-source-inventory-v1.json \
  --classification-manifest data/tool-runtime-inventory/phase0-classification-v1.yaml \
  --classification data/tool-runtime-inventory/phase0-classification-v1.json \
  --json data/tool-runtime-inventory/phase0-replay-fixtures-v1.json \
  --report docs/components/tool-runtime-core/phase0-replay-fixtures.md"
```

The compiled-catalog qualification corpus is locked by SHA-256 in
`tool-runtime-core/tests/phase1_qualification.rs`.
`scripts/eval-tool-runtime-phase0-agentic-live.py` reconstructs provider schemas
from `skillshub` and records first actions without dispatching; migration-only
artifacts live in `docs/archive/components/tool-runtime-core/`.

## Design Notes

- **Interior-mutable registry**: `RegistryService.tools` uses
  `RwLock<HashMap>` so `register_tool()` / `unregister_tool()` work through
  `Arc` without `&mut self`.
- **Exact raw-byte redaction**: contained sequences are credential values and
  physical paths. Encoded, hashed, truncated or reordered secrets are outside the
  matcher; sandbox, fixed executable identity, network policy and authorization
  are the controls against a malicious executable.
- **Trusted computing boundary**: crate-internal execution code can copy a
  borrowed byte slice before a sealed callback returns. The type system does not
  sandbox Magician.
