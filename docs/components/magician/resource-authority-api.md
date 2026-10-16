# Resource Authority Admin API

The resource-authority admin API exposes the billing / spend-control plane:
ceilings, capability tokens, the spend ledger, reservations, freeze/unfreeze,
and period closes. It is the operator-facing surface for the
`resource_authority` layer (spend caps and financial audit), not a general
data API. Magician `0.7.3` / Magician API `0.3.5`.

- Handlers: `magician-api/src/resource_authority_api.rs`
- Routes: `configure_resource_authority_routes`, mounted under the v2 API
  scope at `/api/magician/v2/resource-authority/*`.

Ceiling list/upsert `reserved_in_period` is live in-flight spend from
`active_reservations` (stacked `batch_id` counted once), not the period-windowed
reserved-journal net. A hold that started before midnight still occupies today's
remaining budget. Upsert clones the row under the write guard, then samples the
ledger without holding `system_ceilings.write`, so a concurrent delete cannot
panic the handler and cannot invert `admit`'s ledger-then-ceilings lock order.

`POST /resource-authority/reservations` calls `spend_session::admit` fail-closed
(same writer as chat dispatch and MCP checkout) and `SpendHold::persist` before
drop so commit/rollback is a recovery handoff. Commit or rollback of any stacked
member settles the whole `batch_id` group.

## Authentication

Every endpoint is gated by `ApiAuth::extract`, which reads the
`RESOURCE_AUTHORITY_API_KEY` environment variable:

- **Unset** → the API runs in open ("anonymous") mode. Intended for local
  development only.
- **Set** → each request must present the key, either as
  `Authorization: Bearer <key>` or as a `?token=<key>` query parameter. The key
  is compared in constant time; a missing or mismatched key returns `401`.

This key gates **only** the resource-authority admin API. It is not an
authentication layer for the general magician API — do not treat it as one.

## Uniform enforcement

All resource-authority endpoints — reads and mutations alike — call
`ApiAuth::extract` before any work, so the billing plane cannot be read or
mutated without the key when it is configured. That includes secondary
mutations (`delete_ceiling`, `revoke_token`, `commit_reservation`,
`rollback_reservation`) and every financial read (`list_ceilings`,
`list_tokens`, `get_token`, `query_ledger`, `ledger_balances`, `ledger_audit`,
`list_reservations`, `freeze_status`, `list_period_closes`).

When adding a new resource-authority handler, extract the key first:

```rust
let _auth = match ApiAuth::extract(&http_req) {
    Ok(a) => a,
    Err(resp) => return resp,
};
```

## Spend journal durability and damage handling

The per-scope spend journal lives at
`scopes/{principal}/{workspace}/resource_authority/resource_ledger.jsonl` and is
replayed into a `ResourceLedger` on first touch of the scope
(`resource_authority/scoped_authority.rs::load_for_scope_blocking`).

### Writers

- `persistence::atomic_write` (used by `save_journal`, `persist_state`, and
  `TokenStore::save`) writes a sibling tmpfile, `sync_all`s it, renames, then
  fsyncs the parent directory.
- `persistence::append_journal` serialises the whole batch into one buffer, does a
  single `write_all`, then **`sync_all`s the file** before returning. A spend
  record that returns `Ok` is on disk. The fsync is a hard error here (unlike
  `atomic_write`'s best-effort warning) because the entire contract of the call is
  durability of a financial record. The parent-directory fsync stays best-effort —
  it only matters when the call created the file.

### Readers: trailing vs interior damage

`persistence::load_journal_from_bytes` classifies damage instead of failing the
whole replay, and the two cases are deliberately **not** treated the same.

- **Unterminated trailing fragment → recovered.** Only the newline-terminated
  prefix was ever committed (the writer fsyncs after the newline), so bytes past
  the last `\n` are a torn append no reader was entitled to see. They are dropped
  and reported as `JournalReplay::torn_tail_bytes`. This is the same
  committed-prefix rule the LLM trace journal uses
  (`analytics/llm_trace_journal.rs::complete_jsonl_prefix_len`), not a new scheme.
- **Unparseable line inside the committed prefix → hard error**
  (`PersistenceError::CorruptFile`). Those bytes *were* committed. Skipping them
  would silently understate spend — a reserve with no commit, or a whole agent's
  expense missing — and the gate would then hand out budget that has already been
  consumed. Understating spend is the expensive direction, so the reader refuses
  to guess.

A trailing line that *is* newline-terminated but will not parse counts as interior
damage, not a torn tail.

### What the caller does with an unreplayable journal

It must never fall back to an empty ledger: to the gate that reads as "this scope
has spent nothing", so the whole budget would become available again. The failure
is recorded on `ScopedAuthorityBundle::ledger_load_failure` and the
bundle is **fail-closed** in two places:

1. `system_freeze` is engaged **in memory** with the failure as its reason.
   `reserve_spend` checks the freeze before it looks at any token or balance, so
   every gated dispatch into the scope is rejected. The freeze is never written to
   `system_freeze.json`, so a repaired journal clears it on the next restart rather
   than leaving a scope permanently frozen by a transient read error.
2. `ScopedAuthorityBundle::persist_state` **skips the ledger write**. That write is
   a full-file atomic overwrite, so persisting the empty in-memory stand-in would
   destroy the real history. The damaged file is left exactly as it is for repair
   and forensics. The token store is still written — it has its own load path.

A missing journal file is the normal cold-start shape for a new scope and is not a
failure.
