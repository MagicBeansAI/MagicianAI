# Memory consolidation and lifecycle

Owner-memory writes keep revisions, reconcile related claims and ask about
uncertain conflicts through one shared, provider-independent reviewer.

## Shared structured user-memory path

`save_preference`, user-scope `update_memory_tier`, the web preference endpoint,
learned user promotions and remembered connection clarifications share one
ingress. User extraction leaves conflict questions to the reviewer. Writes keep
revisions rather than replacing an array element because its key matches. A
preference clear retracts matching current entries but keeps their history. The
web endpoint also keeps its episode as provenance; an episode alone is not a
preference write.

A saved revision starts as `memory_lifecycle: pending_review`. Tools confirm
durable storage and schedule review without waiting past their deadline; the
scoped background worker resumes pending work. The reviewer classifies facts,
preferences, goals, instructions and observations, and proposes duplicate,
reinforce, coexist, supersede or `ask_owner` relationships, across keys and
user tiers. It never rewrites the owner's claim into model prose.

Each revision has a stable record ID, an ingress identity and source evidence.
Independent events can reinforce a claim; repeated copies of one root event do
not advance its date or evidence count, and an uncited batch counts as one
source. User promotions ground evidence in real input IDs and literal quotes,
with server dates and root execution identities. Extraction asks for a
self-contained assertion and an exact supporting excerpt; invented excerpts
cannot become evidence. Uncited attribution stores source identity and date,
not event payload. Tool writes discard caller-supplied lifecycle IDs and
evidence counters. Extraction uses JSON Schema and validates records before
persisting. Recognizable legacy descriptive-schema copies are quarantined in
the private audit journal and excluded from current memory; legacy primitive
collections are normalized lazily without losing text.

Every `knowledge.json` write through `persist_user_knowledge`, and every
`forget_memory` deletion, bumps `memory/users/intention_generation.json` under
the same flock. `forget_memory` holds the file's exclusive lock across each
tier-field read-modify-write, so a concurrent save is not lost. Preference
alignment compares this generation to detect owner knowledge that changed after
an assessment. The record stores a BLAKE3 fingerprint of the knowledge bytes,
so a failed bump does not fail the write; it reads back as stale. See
[preference alignment](preference-alignment.md#sources-and-generation-p1).

## Change policy

- **Corrections.** A clear explicit correction can supersede an older claim
  about the same subject, aspect and context. Project and engagement boundaries
  hold even if the model calls two items equivalent.
- **Coexistence.** Different people, attributes, projects or conditions can
  coexist. A temporary exception does not replace a standing preference.
- **Coverage.** Duplicate and reinforce describe a relationship, not
  redundancy. Review declares separately whether an existing memory covers the
  **entire** incoming statement (`incoming_coverage: full|partial|unknown`).
  Except for identical text with matching applicability, only full coverage
  retires the incoming record, and different validity boundaries still block
  the merge. Partial support does not add to the older claim's evidence count.
- **One merge per plan.** A record that repeats several saved copies merges
  into the one the reviewer is surest of (the earliest offered, so closest in
  meaning, on a tie). The other copies the reviewer called `duplicate` fold
  into that record too: their evidence moves to it and they are superseded
  with `supersession_reason: duplicate_of_merged_copy`, keeping their words.
  A `reinforce` copy stays in service, since it may say more. Until
  2026-10-19 such plans were rejected, so a preference saved two or more
  times could never be reviewed and was set aside after three tries. A plan
  that merges and also asks the owner about another claim is still rejected,
  and so is more than one merge while resolving an owner clarification.
  Rewording or spelling out something already implied is not new information.
- **Near-duplicates fold at write.** The shared ingress (`merge_items` /
  `merge_collection`; `near_duplicate_record` over `memory_similarity`) merges an
  incoming near duplicate of a live record into it (re-queued for review)
  instead of saving a sibling. Near duplicate: normalised keys (case,
  separators, plurals, generic words like `preference`) share ≥ 40% of words,
  content words overlap at Jaccard ≥ 0.80, no differing negation, qualifier
  (`except`, `unless`, `only`, `without`, …) or number, and the same
  applicability, kind and trust. Possible refinements or contradictions still
  go to the reviewer.
- **Owner-run near-duplicate fold.** Older stored copies fold only on request:
  `magician memory-fold-user-duplicates [--apply]` (Make targets alike) or
  `POST /api/magician/v2/chat/memory/duplicates/fold` (`apply` defaults to
  false). Per collection the most-confirmed copy (most independent
  observations, then newest) keeps; each near duplicate's evidence merges into
  it and the copy is superseded (`supersession_reason:
  near_duplicate_of_record`, words kept). A dry run writes nothing; a record
  changed since planning is skipped.
- **Inferences.** An inferred pattern needs at least three independent
  observations over two days before it can replace another inference, or
  unsettle one through clarification. Observation count can never silently
  retire an explicit goal, preference or instruction; authority comes from
  provenance, not model confidence. A weak incoming inference stays pending
  even if the model picks `ask_owner`; new independent evidence resumes it.
- **Expiry** follows a recorded validity interval backed by quoted source text.
  Inactivity or rare retrieval does not make a standing goal false.
- **Failure.** Model failure, invalid output or changed source revisions defer
  the change. Short references are bound to the offered record snapshots;
  unknown references fail rather than being matched by similarity. Source IDs
  and referenced revisions are checked before apply.

Review decoding is the same for every provider. Repeated identical JSON members
pass only if their parsed values agree; conflicting duplicates at any depth,
unknown fields, wrong types, trailing content and responses over 64 KiB are
rejected before mutation. Nothing is repaired or retried with a second call.
Missing coverage is `unknown`.

The classifier is domain-independent. The backend enforces identity, authority,
scope and lifecycle; semantic matching and question usefulness are model
judgments measured by the behavioral eval. Full coverage is a judgment, not an
entailment proof. Retired records are not resurrected automatically.

## Clarification and recall

Ambiguous contradictions use the durable `memory_clarification` HITL surface.
The owner can keep the earlier memory, replace it, explain in free text, or
dismiss. These choices affect memory only; they cannot approve or run an
external action.

The review label `ask_owner` requests missing information (the old `clarify`
spelling is still accepted). A concrete, nonempty question of at most 600
characters is required before any part of the plan applies, so an empty
question never yields a generic prompt. A blocked replacement keeps its
separately governed confirmation fallback.

Questions carry their cited source versions and related claims. A stale answer
cannot apply to newer evidence; unrelated writes do not invalidate it. A
free-text answer is reconciled with both cited memories and must address them
before the conflict resolves. If review calls a reaffirmation a duplicate, the
full owner answer becomes the current version, keeping its qualifications;
earlier evidence and validity bounds survive. Decided conflicts stay in the
private lifecycle journal so a restart or replay does not repeat them.
Dismissal keeps the uncertainty; new evidence may reopen it.

Prompt and search candidates exclude pending, superseded, retracted and expired
revisions; unresolved claims are labelled uncertain. Prompt text holds the
claim and its applicability, not its evidence history. Stable record IDs keep
current and pending versions apart in the index, and admitted candidates
resolve cross-tier successor links for history. Temperature and retention are
separate from currency. Attention shadow conditioning also excludes pending and
retired claims, and unresolved claims cannot suppress attention.

## Scheduling and bounds

`memory_lifecycle_review` is mapped to `gpt6luna-responses-toolsany` in the
repo-root `llm-router.yaml` seed and appears in Settings through the dynamic
operation catalog. It is background work with its own route, not a
chat turn's model.

| Bound | Value |
| --- | --- |
| Records offered per pass | 8 (`review.max_offered`), chosen by meaning |
| Incoming claim | 4,000 characters |
| All offered claims | 24,000 characters |
| Whole prompt with metadata | 32,000 characters |
| Review deadline (engine answers plus written verdict) | 120 seconds (`review.deadline_secs`) |
| Calls per scope | 6 per rolling hour (`user_knowledge_bounds.review.max_per_hour`; durable reservation; failures count) |
| Retry wait per record | at least 15 minutes |
| Tries per record revision | 3 (`review.max_attempts_per_record`), then set aside; re-queued after `review.retry_after_days` (14) |

**Unchanged knowledge is parsed once.** Every agent sweep and the resurfacing
worker's minute tick run this pass, and each run used to parse the
multi-megabyte `knowledge.json` up to three times (the maintenance check, the
owner-question reconcile, then the review). The pass's unlocked reads now use
`load_user_knowledge_shared`. It is one process-wide parse per file, reused
while the file's size, mtime and `(dev, inode)` match and its mtime is more than
three seconds old (the learning store's racy-mtime rule). Bytes read while the
stamp moved are not kept, and every write through `AgentMemoryService` drops
the parse. Kept parses are bounded: one unused for five minutes is dropped,
and beyond 16 files or 32 MiB of summed file size the least recently used go
first (a larger file is never kept). The maintenance check only reads it: each detector
(`has_legacy_schema_echoes`, `has_legacy_collections`, `validity_ended`) mirrors
the mutation it stands for, and the mutations still run only under the lock.
The resurfacing worker records the pass under its own `memory_lifecycle` run
kind when it reviewed, applied, expired or failed something; idle ticks are
not recorded, and a repeating failure writes one row per 15 minutes unless
its message changes.

**Which records are offered** (`memory_lifecycle::candidates`): the conflict
being resolved first, then the existing records closest in meaning to the
incoming one (cosine over local embeddings of key and wording, cached per
wording for the process), then shared words; across tiers. If the embedding
model is unavailable the order is by shared words alone. After a restart
the cache is empty and the first ranking embeds every record (about 1,200,
10 to 15 minutes on the local model); one ranking embeds at a time, so the
sweep and the resurfacing worker no longer both do it, and each batch is
cached as it lands, so a pass that fails part way keeps its progress. The decision engine
(Jev only) answers offered records one at a time (`item_concurrency: 1`), so
before 2026-10-18 the 48 records offered by shared words never finished inside
the shared 40-second deadline and no review completed: the queue stood at 584.
The ranking only chooses what the reviewer reads; the verdict is still the
reviewer's.

**Who decides each pair:** the engine's confident answers (every question at
or above its threshold) are locked; the written reviewer decides the rest, or
every pair when the engine settled none (`text::Unqualified::Generate`). Until
2026-10-19 a review waited for the engine to be sure of all eight pairs, which
happened in none of 61 reviews, so none applied. Kev-4B no longer backs this
decision: it called 164 of 165 pairs duplicates.

Queue order: owner sections before agent sections (`workflows`,
`organization`), newest first. Only a verdict the reviewer could not use for
that record (`invalid memory review`) or a plan the backend rejects for it
counts as a try; timeouts, transport errors, an unavailable route and lock
timeouts never do, so a reviewer outage cannot set the queue aside. Each
counted try logs `memory review: a try failed for this record` (WARN) with
the record id, the try number and the reason, never the record's text. A record
that used up its tries is set aside:
`memory_review_needed: false` plus `memory_review_deferred_at` and
`memory_review_deferred_reason: max_attempts`. Its `memory_lifecycle` is not
changed, so it stays in service exactly as it was; new evidence (a new
revision) earns it fresh tries, and after `review.retry_after_days` a
maintenance write queues it again. Try counts live in
`_memory_lifecycle.attempt_counts` (the `attempts` last-try times keep their
shape); entries for records that are gone or no longer waiting are pruned in
the reservation write.

Not queued at all: activity logs (agent-observed sections, including the
nested `user` object holding email/chat evidence and channel patterns and
feedback), and records from before the reviewer existed (neither
`memory_review_needed` nor `memory_review_version`). Those count as reviewed
(owner decision 2026-10-18; the clean-up stamps them `memory_review_version:
0`).

The prompt holds a bounded evidence sample plus separately computed counts and
date span. Questions and expiry reconcile without the model. Unmapping the
operation leaves writes pending; it never makes them current by fallback.

App contribution envelopes keep their own eligibility path. Agent and goal
operational tiers keep their schemas and merge rules; promoting their content
into owner memory enters this lifecycle, but execution state is never turned
into owner preferences automatically. Supersession and pair-review stamps the
consolidator writes on tier items (`memory_lifecycle`, `superseded_*`,
`supersession_*`, `memory_conflict_review*`) are runtime metadata: the merged
value admits them though no `item_schema` declares them, while fresh model
output still may not set them. A stored item that no longer matches its schema
is kept unchanged and logged, so it cannot block later merges; new or changed
items are still validated strictly. The owner-edited profile note stays a
separately approved instruction source, already in connection recall. Profile
add, retract and restore decisions have distinct identities, so replaying an
old approval cannot undo a later retraction. Review never edits that note.

## Size bounds and archive

`knowledge.json` used to grow without limit (6.5 MB, ~130 KB a day), and every
write rewrites the whole file under one lock. `memory.user_knowledge_bounds`
bounds it. Plan: `docs/archive/plans/2026-10-18-user-memory-growth-bounds.md`.

**Nothing is deleted.** Every record that leaves the main file is first
appended to `memory/users/archive/<YYYY-MM>.jsonl` (whole record plus
`archived_at`, `reason`, `from_section`, `replaced_by`); one flush per batch.
If that append fails the write keeps every record. Months older than
`archive.compress_after_months` (3) are rewritten once to `.jsonl.zip`
(verified byte for byte first). Archived intention records are journalled as
retired in the preference history, not forgotten, so their wording is kept.

`mode` (default `observe`):

- `off`: writes behave as before.
- `observe`: on a write, at most hourly per file, log what would leave
  (`user memory bounds observe: would archive`). Nothing changes.
- `enforce`: on every write, after the merge and before the file is written:
  - **Activity logs** (`user.email_evidence`, `user.chat_evidence`,
    `user.channel_patterns`, `user.channel_feedback`) go to
    `memory/users/lanes/<name>.json` with their own lock, not to
    `knowledge.json` (when `lanes.enabled`). One row per key: later slices of
    an email/chat or feedback day are combined into its row (lists merged,
    counts added, notes capped at 12; a slice that loses anything in the
    combining — notes past the cap, a changed value — is archived first as
    `lane_slice_combined`; a replayed pass changes nothing); a
    channel pattern keeps one row per topic with `first_seen_at` and
    `seen_count`, archiving the previous wording. Rows older than
    `lanes.hot_days` (45) or past `lanes.max_rows` are archived. Lane rows carry
    no lifecycle fields and are never reviewed.
  - **One claim stored once** (`fold_inferred_same_key`: `workflows`,
    `organization`, `preferences`; any user-memory section may be listed).
    Among live, unprotected, inferred or untrusted records with the same
    applicability, the newest stays when they share a key — compared by
    letters and digits only, so `draft-only`, `draft_only` and `DraftOnly` are
    one key. Similar wording under different keys is never folded here (one
    changed word can be a different claim); the lifecycle reviewer judges
    meaning. Applicability is the project, engagement, scope, subject, context
    and validity window, so a time-boxed copy is never merged into a standing
    one. The others are archived
    (`restated_by_producer`, `replaced_by` the survivor), whose evidence ids
    (without quotes, at most 32) it keeps. An **exact duplicate** (same
    applicability and same wording; under different keys only with at least
    6 distinct words)
    is merged even when a copy is protected: the owner's strongest copy stays
    (confirmed, then stated, then asked for, then reviewed, then newest) and
    the rest are archived as `merged_duplicate`. Restored, app-governed,
    unresolved and open-question records are never merged. An agent's
    rewording never replaces an owner record: protection is per record.
  - **Section caps** (`max_live`; `workflows` 600, `organization` 250,
    `preferences` 600, `knowledge` 200, `skills` 100, `accounts` 100). Over a
    cap: retired records older than `retired_archive_after_days` (30) first,
    then unprotected inferred records, least used first (temperature overlay
    usage, loaded only when a cap applies; oldest first without it).
  - **Review-queue cap** (`review.max_pending_per_section`, 300): over it, the
    least used unprotected waiting records are archived.
  - The learning promotions audit keeps 20 rows in the main file (200
    otherwise); every new row, and every older row trimmed from the file that
    the archive does not already hold, is appended to
    `archive/learning_promotions.jsonl` before the file is written (no
    trimming if that append fails).

**Protected, never removed by any rule:** owner-stated (`Stated` trust),
owner-confirmed or corrected records, learning records the owner asked for,
records the reviewer made active (`memory_review_version` ≥ 1), records
restored from the archive, app-governed records, and records in, or related
to, an open owner question. Removing other records shifts array positions, so
open questions whose context is current are moved with their records.

Always on, whatever the mode: retention pruning (meeting notes, screen
observations) and records a learning promotion replaces go to the archive
instead of being dropped. The learning bridge no longer trims sections itself.

**Magician's framing is not the owner's words.** A surface that composes the
user turn itself (the contextual-writing helper's instructions, page and
selected text; a tutor continuation) wraps what it wrote in a
`<magician_context>` block (`chat::authored_framing::wrap`). Every path that
turns a user turn into memory or learning input reads
`authored_framing::owner_words`, which drops those blocks: the chat episode
(`chat_user_message_excerpt`, `user_message_excerpt`), memory repair from
stored messages, the preference-alignment owner turn (`OwnerTurn`), and taste
capture transcripts (the owner's side only). An unclosed block drops everything
after it, a routing marker left alone (`@tutor`) counts as no owner words, and
`wrap` neutralises tags inside the text it wraps, so page text cannot close a
block early. Before this,
the helper's fixed "return only the draft text" instruction was learned as an
explicit owner request, four times under four keys.

**Evidence distiller.** The channel bridges' email/chat rows sit under the
nested `user` object (or in lane files under enforce), where the distiller
never looked. It reads them only with `lanes.distill_enabled: true` (default
false: the owner has not yet agreed to email and chat feeding work evidence).

### Runbook: report, clean-up, restore

```bash
make memory-user-report          # sizes, keys, older copies, queue; no contents
make memory-compact-user         # dry run of the one-time clean-up; writes nothing
make memory-compact-user-apply   # apply (owner present; check `pgrep -fl scripts/eval-` first)
magician memory-user-restore --record-id <memory_record_id> [--apply]
make memory-dedupe-user          # dry run: same meaning in different words; writes nothing
```

`memory-dedupe-user` (`--section` repeatable, `--threshold`, default 0.90)
embeds each live record of `preferences`, `workflows`, `organization`,
`knowledge` and `skills` with the local embedding model (background lane) and
groups records with the same applicability when every pair in the group
reaches the threshold (closeness does not chain: A near B and B near C does not
put A with C). A record that is the memory of one observed event (a
`work_evidence` ref, such as one ambient browsing day) never groups with another
event's record. For each group it shows which copy an approved merge
would keep (the exact-duplicate order above) and which would be archived, and
marks groups holding an owner record (`needs owner OK`). It also prints what
the write-time fold archives on the next write. It only reads.

All take `--principal`/`--workspace` (Make: `MEMORY_INDEX_PRINCIPAL`,
`MEMORY_INDEX_WORKSPACE`; default `anonymous`/`default`) and `--json`. The
clean-up moves the nested activity logs to lane files (archiving every
original record first), stamps pre-review records `memory_review_version: 0`,
applies the fold and caps above using the configured values, and trims the
promotions audit to 20 rows. `--apply` takes the knowledge lock (gives up after
60 s), re-reads, backs the file up to
`<scope>/memory_backups/knowledge-before-growth-bounds-<UTC>/knowledge.json`
(hash-checked), writes archive and lane files, then `knowledge.json`, then a
counts-only `growth-bounds-summary.json` next to the backup. A second run
finds nothing to do. To roll back: stop the service and copy the backup back;
lane and archive files are additive and can stay. Restore puts an archived
record back pending review, protected from the bounds; a restored lane row
carries `lane_restored_at`, which exempts it from the lane's window and row
cap (it does not count toward the cap either). It reports `restored: true`
only if the record is there after the write. The archive line stays.
The `--apply` backup is flushed (file and directory) before it is verified.

## Focused qualification

- `make test-memory-lifecycle`: deterministic lifecycle regressions.
- `make build-memory-lifecycle-eval` builds the real-model fixture runner;
  `make test-memory-lifecycle-live-eval` (or **Evals → Lane →
  `test-memory-lifecycle-live-eval` → Run**) runs it. Both lanes are discovered
  from Makefile annotations.
- `make test-memory-lifecycle-evals-integration` checks the wrapper and
  Run/history/report API contracts without provider calls.

Fixtures live in `scripts/fixtures/memory_lifecycle/cases.json`. The runner
uses production ingress, storage, review, rendering and owner-request handling
on synthetic scoped data, and records raw responses, usage, expected and actual
states, recall and replay. Fixtures cover a chat turn through consolidation
then a correction and recall, downstream connection and delivery decisions, and
free-text clarification through the real owner request service with a runtime
teardown and reopen. Recall is warmed first so checks include index refresh.
Scheduler timing and browser or device input are not covered.

The live lane defaults to all 19 journeys (17 cases in `cases.json` plus the
capture and clarification journeys) × 3 repeats; partition
`all|development|validation`, 1–3 repeats. CLI controls:
`MEMORY_LIFECYCLE_EVAL_PROFILES` (comma-separated, up to three),
`MEMORY_LIFECYCLE_EVAL_REPEATS`, `MEMORY_LIFECYCLE_EVAL_PARTITION`. Profiles
come from the Settings runtime catalog; empty uses configured routes. A
selection overrides only `memory_lifecycle_review`, `memory_connection_review`
and `memory_user_promotion` in a private config snapshot; the live config is
untouched. A profile can change provider and generation settings, not just
model.

UI and API runs keep options and status in Evals history; CLI runs keep reports
but no task history. Each report is bound to its run ID under
`coverage/evals/memory-lifecycle/{live,deterministic}/runs/` and links raw
evidence, logs, models, usage and config, fixture and binary hashes. Missing
evidence, build failures, timeouts and failed journeys cannot pass. Spend shows
as **unknown** in the task-ledger Cost tab; router-estimated cost is in the
report.

Related: [memory index](memory-index.md), [memory connections](memory-connections.md),
[taste profile](taste-profile.md).
