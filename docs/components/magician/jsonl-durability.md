# JSONL durability — the torn-tail contract

Module: `magician/src/magician_v2/jsonl.rs`. Consumed by every append-only store
in `magician_v2` (share links, scheduling, run state, claim manifests, data
rooms, obligations, commitments, approval envelopes, outcome learning, outward
assertions). Counterparties, the suppression register, and the data-room access
store are typed against the same torn-tail contract, and all three are on a
live path:

- **The suppression register** *is* a dispatch gate — gate 2 of the outward gate.
  `outward_gate::contact_refusal` builds `SuppressionRegister::global` and screens
  every recipient of every outward act `execute_action_inner` classifies, under
  capture as well as live. Its fail-closed read is the reason this contract
  exists: `is_suppressed` returning `Err` means DO NOT SEND.
- **The data-room access store** is written on the HTTP reader path. Every
  successful presentation of a share link writes an `AccessEvent` through
  `AccessStore::record_access` before a document is served
  (`magician-api/src/data_room_reader_api.rs`), and the routes are mounted at the
  app root in `magician-bin/src/main.rs`.
- **Counterparties** back that same reader surface: `CounterpartyStore::audience_for`
  resolves who may be admitted, through `data_room_reader_api::CounterpartyAudiences`,
  constructed in `magician-bin/src/main.rs`. `chat::inbound_authority`
  (`engagement_lane_in_process`) also consults it from chat session routing
  (`get_active_session_handler` / `new_session_handler` via
  `inbound_engagement_outcome`, `magician-api/src/chat_api.rs`), but it is still
  not on the outward dispatch path.

Every store keeps its state as an append-only log and folds it on read. Two
failure semantics are decided here, in one place, so no store can drift:

## Absent is the only error that means "empty"

The original pattern mapped **every** read failure to an empty store. That fails
open: a revocation against an unreadable log reports success while revoking
nothing; a one-per-identity guard passes vacuously; a disk fault gets recorded as
a refusal. `read_log_if_present` maps only `NotFound` to `None` and propagates
everything else.

## Both halves of the torn-tail rule

A completed append writes `line + \n` in one call, so a crash mid-append leaves
exactly one torn line, at the tail, **unterminated**. Two consequences, both
required:

**Read half** — `parse_log_lines` tolerates an unparseable final line *only when
the log does not end in a newline*. A terminated line that fails to parse was an
acknowledged write: dropping it would silently un-record a settlement or a
revocation, so it is corruption and the fold refuses — as it does for any
interior failure, since reading past one would resurrect whatever it ended.

**Write half** — `append_log_line` heals a torn tail before writing. Without
this the read half is unsound: a plain `O_APPEND` after a crash fuses the new
record onto the fragment, silently un-recording the operation just acknowledged,
and the next append turns the fused line into interior corruption that bricks
the log permanently. Healing truncates the fragment — honouring "a torn append
never happened" — then appends. The heal path only runs after a crash, so it is
never on the hot path.

Every store's write ordering (record-before-act, index-before-row,
debit-before-act) is what makes "the torn operation never happened" the
fail-closed reading in all of them.

## Races converge instead of bricking

Append-only files offer no compare-and-append, and the stores are deliberately
unlocked. Where two writers can race, the **fold arbitrates**: a racing
duplicate head for the same act is skipped (outward assertions' `prepare`),
first-wins rules decide terminal transitions (submissions, settlements), and
identical replays resume. The residual windows are documented at each site
rather than discovered.

## Outside the contract

Other subsystems' JSONL writers (`realtime_events`, `learning`,
`agent_update_journal`, `api_mining`, `analytics`) append directly and do not
use this contract. The helper is `pub(crate)`.
