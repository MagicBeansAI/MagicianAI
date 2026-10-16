# Trusted-store integrity (unforgeable agent-writable records)

Makes the security-bearing, agent-writable store records **unforgeable**, so an
operator-approved file write cannot be redirected by a malicious or
prompt-injected agent. Design:
`docs/archive/plans/2026-07-10-trusted-store-integrity-design.md`.

The in-process `TrustAuthority` is bound to the **transaction**,
**code-change-proposal** and **pause-state** stores. Pause-state binding gates
only **elevation-bearing** resumes (those that restore approval rules or an
owner authority chain).

Because transaction/proposal apply paths bind `apply_root` to the in-process
authority, `config::external_writes_enabled()` defaults **ON**;
`MAGICIAN_ALLOW_EXTERNAL_WRITES` = `0`/`false`/`no`/`off` is the kill-switch.
The always-on native-file runtime-store deny-fence closes the other door
(native `write_file`/`edit_file` into the stores).

Module: `magician/src/magician_v2/execution/trusted_store/`, re-exported from
`execution::trusted_store`. The same directory holds `durable_attenuation.rs`,
`durable_launch.rs` and `durable_routing.rs` (HMAC-sealed sidecars for plane
attenuation, accepted runtime launch and execution-local LLM routing),
documented in [plane.md](plane.md); they are not the `TrustAuthority` path.

## Threat model in one line

The **shell/agent can write files on disk** but **cannot reach magician's
process memory**. The trusted copy of a record's security-bearing fields
(`content_hash`, `apply_root`, …) lives in process memory, and the decision
site takes the decision from there — never from the on-disk record.

## Pieces

- **`TrustAuthority`** (`authority.rs`) — per-boot, in-process
  `RwLock<HashMap<TrustedRecordKey, AuthorityEntry>>`, populated **only** when
  magician stages a record (`record_stage`).
  - `TrustedRecordKey` = `{ StoreKind, principal, workspace, id }`,
    `StoreKind ∈ { Transaction, CodeChangeProposal, PauseState }`.
  - `AuthorityEntry` = `{ content_hash: blake3::Hash, apply_root: Option<PathBuf>,
    target_paths: Vec<PathBuf> }`.
  - `get` is the decision-time lookup: **`None` ⇒ fail closed (re-ask)**. The
    map is empty after restart by design. `forget` drops an entry when its
    record goes terminal.
  - `install_process_authority` returns the canonical `Arc`
    (`OnceLock::get_or_init`); `AgentApiServices` stores that return value so
    the struct field and `process_authority()` are the same object.
- **`BootKey`** (`boot_key.rs`) — RESERVED, **not on the live security path**:
  a per-process, memory-only keyed-BLAKE3 key (redacted `Debug`). No disk MAC
  sidecar is used — an attacker who can write the record can delete a MAC;
  the memory-anchored `content_hash` is stronger.

## How the guarantee works

At the decision site a caller requires **both**, from process memory:
1. a **`TrustAuthority::get` hit** — magician staged this exact record this
   boot; and
2. **`entry.content_hash == record.content_hash()`** recomputed on the loaded
   on-disk record — its security-bearing content was not edited after staging.

Either miss ⇒ fail closed / re-ask. `apply_root` and authorization fields are
read from the authority, never from the JSON.

## Transaction apply binding

- **Stage** (`compiled_handlers/staged_file_edit.rs` `stage_edits`): records an
  entry for **every** transaction (the decide site fails closed on a miss).
  `content_hash` = BLAKE3 over the serialized `edits`; `apply_root` =
  `scoped.apply_root` (`None` for in-workspace writes); `target_paths` = each
  edit's destination.
- **Decide** (`magician-api/src/web_api.rs` `respond_hitl_handler`,
  `diff_approval` → `apply` → `Transaction`): apply root from
  `trust_authority.get(key)`. Miss ⇒ `diff_approval_not_in_authority`. On a hit,
  re-load the same record `apply_transaction` applies and compare
  `content_hash()`; mismatch ⇒ `diff_approval_content_tampered` (entry not
  forgotten). Then the root passes canonicalize + `is_dir`, and
  `apply_transaction`'s containment guard still rejects `..`/symlink escape.
  Entry is `forget`-ten on apply and reject.
- `resolve_transaction_apply_root` / `resolve_code_change_proposal_apply_root`
  (on-disk resolution) are dead code kept as reference.

## Proposal apply binding

Same shape as transactions, same process-global authority.

- **Stage** (`file_edit/proposal.rs` `stage_patch_with_apply_root`) — the
  single choke point every proposal stage site funnels through. Key
  `{ CodeChangeProposal, scope.principal, scope.workspace, proposal.id }`;
  `content_hash` = BLAKE3 over the serialized `patch`; `apply_root` =
  `proposal.apply_root`; `target_paths` = `proposal.touched_files`.
- **Decide** (`respond_hitl_handler`, `diff_approval` → `apply` → `Proposal`):
  apply root from the authority, never `proposal.apply_root`; miss ⇒
  `diff_approval_not_in_authority`; hash mismatch ⇒
  `diff_approval_content_tampered` (not forgotten). `apply_proposal`'s
  `assert_resolved_inside_workspace` still rejects escapes. Forgotten on apply
  and reject.
- **Key match.** Stage key scope comes from the `TransactionScope`; decide key
  from `request_scope`, and the store is rooted at `scope_root(request_scope)`,
  so a loadable proposal always matches — a miss means "never staged this boot".

### Agent-callable apply binding (`apply_code_proposal`)

The `apply_code_proposal` compiled handler (an agent applying its own staged
proposal) is bound identically: key uses the **runtime-injected**
`__principal`/`__workspace`, never the on-disk `proposal.scope`; miss ⇒
`apply_code_proposal_not_in_authority`; mismatch ⇒
`apply_code_proposal_content_tampered`; forgotten on successful apply; absent
authority fails closed.

## Pause-state elevation binding

The forge vector here is **agent re-authorization**:
`restore_context_from_pause` (executor.rs) rebuilds `trust_level`,
`trust_policies_path`, `approval_rules`, owner snapshot
(`active_owner_agent_id`, `owner_stack`) and `spend_token_ids` from the on-disk
pause JSON. So the entry uses `apply_root: None`, `target_paths: []` and puts
all security in `content_hash`.

- **Discriminator** — `AgenticPauseState::is_elevation()` =
  `!approval_rules.is_empty() || !owner_stack.is_empty() ||
  active_owner_agent_id.is_some()`. Only elevation pauses are bound; benign
  pauses (max-iterations, budget, user-input, manual) resume as before.
- **Authorization hash** — `authorization_hash()` = BLAKE3 over
  `serde_json::to_vec(&(trust_level, trust_policies_path, approval_rules,
  active_owner_agent_id, owner_stack, spend_token_ids))`, nested maps
  canonicalized via `to_value` (sorted keys). `loop_protective_state` is hashed
  after one serde round trip of that field only (round-tripping the whole pause
  overflows the execution worker's 2 MiB stack in debug).
- **Stage** — `FullPauseStore::store` is the single choke point. For
  elevation pauses it records `{ PauseState, principal, workspace,
  storage_key() }` with `content_hash` = **the hash the envelope was sealed
  with**, computed by `persist_to_disk` over the body *as read back from disk*
  (`PersistedPauseAuthority::authorization_hash`). Invariant: stage and resume
  hash the same bytes; hashing the in-memory state refused same-boot pauses
  whenever a field was lossy through serde. App-disclosure and placement-retry
  pauses hash `app_pause_continuation_bytes`; a memory-only store uses the
  in-memory hash.
- **Decide** — three resume surfaces gate before `restore_context_from_pause`:
  HTTP **resume** and **continue** (`verify_elevation_pause_authority`, 400
  `pause_authorization_not_in_authority`) and
  `execute_agentically_resume_exact` (from `/executions/{id}/resume`, terminal
  `Failed`). Miss or hash mismatch both refuse.
- **Fail closed drops the pause** — refuse branches do not re-store it (a
  re-store would re-mint the entry and let a retry auto-heal). Scope/input-type
  mismatches still re-store.
- `forget` once the resume consumes the pause; a re-pause re-mints.
- **Restart** ⇒ pending elevation pauses fail closed until re-approved; they are
  not re-queued at boot.

## Restart re-queue

Pending file-write approvals are benign, operator-visible diffs, so boot
re-anchors them instead of failing them.

- **Where** — `run_agent_startup_hydration` (`magician-api/src/web_api.rs`),
  per-scope loop, after paused-agent-state hydration.
- **Enumeration** — `TransactionStore::list_pending()` and file-edit
  `CodeChangeProposalStore::list_pending()` (not the agent-definition
  `ProposalStore`): best-effort scan of `<scope>/transactions/` /
  `<scope>/code_change_proposals/`, `Pending` only, skipping missing dirs and
  malformed files. Roots via
  `artifact_v2_service().workspace().scope_root(principal, workspace)`, the
  same derivation as stage/decide.
- **Key + entry are byte-for-byte the stage sites'.** A record tampered after
  the re-anchor still fails the decide-site hash check.
- Re-record only: applies nothing, emits no HITL events, idempotent. Elevation
  pauses excluded. Never aborts hydration.

## Tests

Unit tests live beside the code: `trusted_store/authority.rs`,
`trusted_store/boot_key.rs`, `file_edit/transaction.rs`,
`file_edit/proposal.rs`, `agentic/types.rs` (`AgenticPauseState`). An
end-to-end HTTP stage→approve→apply regression through `respond_hitl_handler`
does not exist yet (`TODO(trusted-store)` markers in those files).
