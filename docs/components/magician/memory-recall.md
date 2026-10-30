# Conversational memory recall

Ordinary admitted chat turns retain complete redacted user and assistant source
text in native episode memory candidates. `text_excerpt` and observations retain
the 4,000-character compatibility view; `source_text`, source role/identity and
recording time preserve later evidence for indexing and consolidation. Legacy
episodes remain readable with their available excerpt and an explicit fidelity
marker. Null, blank or malformed complete-source fields fall back to the retained
excerpt. Missing legacy content is never synthesized.

Episode candidate composition is shared by the index and runtime. It prefers
complete conversation sources, labels their speakers and excludes operational
summaries/coordinates when conversational evidence exists. Non-conversational
episodes neutralize memory-boundary tags and compose from outcome, title and a
bounded task intent, never the raw task description (an episode is memory of
what happened, not a copy of another run's instructions).

- **`task_intent`** (≤480 chars, recorded at terminal): the user's request
  inside a `<<<…USER_PROMPT` / `<<<…USER_REQUEST` fence, else the
  description's user-facing lead paragraph, else the title.
- **Recalled fields** of a non-conversation episode: outcome, title, intent,
  plus remaining work, a distinct `strategy_summary`, starting context and
  observations when each passes the prose gate. The outcome is the verdict
  (`outcome_kind`) + intent + the run's final output (user-audience excerpt
  first, ≤600 chars); `outcome_summary` (≤2,000 chars) stands in only without
  an output, only when it passes the gate and does not replay the task brief
  (two or more substantive non-request description lines).
- **Prose gate** (`is_user_facing_prose` / `user_facing_prose`, structural): a
  line opening with `{` or holding `{"`/`[{`, a `<<<` fence or a bracketed
  runtime marker (`[Execution resumed …]`, `[Delegation] …`) makes its
  paragraph machine text; text whose every line is a bare identifier/key
  (`hitl.requested`, `progress_channel:<uuid>`) is not prose. Runtime
  bookkeeping and the raw description never enter recall (the description
  stays on the record for provenance).
- **Write and read apply one rule**: `episode_outcome_statement` at record
  time, `recalled_episode_outcome` / `recalled_episode_strategy` on every
  stored record at read time. All recall paths use it: indexed episode text
  behind "### Episodes" (a non-conversation episode renders whole, so no
  "Adjacent source passage" expansion), `prompt_pipeline` episode sources, the
  `task_progress`/archive/strategy tier writers, and the consolidator's inputs
  (episode projection, quality-classifier view, `source_text`), which carry
  `task_intent` instead of `task_description`.
- **Tier items** keep only the user-facing prose each string leads with (none
  left: not recalled) and render as one line, `label (as of <date>; valid
  until <date>; unresolved conflict): value`, never as JSON; ids,
  supersession, provenance, evidence, rationale, confidence and containment
  labels never render (provenance travels as `prompt_provenance` metadata).
  Non-episode items render whole or shortened from the end within their lane
  share and a 1,000-char cap (`bounded_item_text`), never as a mid-item
  relevance window.
- **Near-duplicate tier items** in one lane and tier (`memory_similarity` over
  candidates' `claim_key` / `claim_text`) render once: the most-confirmed copy
  (then newest), prefixed `(N similar)`. Storage folding lives in
  [memory lifecycle](memory-lifecycle.md).
- **Bounds.** All shortening goes through `magician_vector_index::text_bounds`:
  `bound_text` (sentence end, else word boundary, then `…`) for outcomes,
  results, intents, tier items and passage fallbacks; `word_aligned_window` for
  query windows and index chunks; `bound_section` (`bounded_memory_section`)
  for section caps, keeping whole entries, marking `…(more memory omitted)` and
  keeping the closing tag. Text without spaces is cut at the budget.
  `repair_dangling_cut` repairs, at read time only, a stored legacy cut (a word
  glued to `...`, or a ≤3-word stub after the last sentence in text ≥160
  chars) to that boundary plus `…`; records are not rewritten. A
  non-conversation episode's recalled text is capped at 3,000 chars
  (`MAX_EPISODE_RECALL_CHARS`); changed text hashes re-index episodes via
  `source_hashes_changed`.

Index schema v11 forces older
derived vectors to rebuild through the normal maintainer. Dense embeddings use
speaker-labeled passage text with boundaries supplied by the source producer; lexical search retains exact identifiers and scalar columns retain
scope/source addresses.

The completed implementation is recorded in the
archived memory recall plan.
Performance qualification is tracked in the
active benchmark plan.
Run focused provider-free checks with `make test-memory-recall`.

Automatic prompt discovery includes native episodes through the canonical agent
candidate producer. Hybrid fusion ranks physical passages before taking the
best hit per parent; source revision and engagement labels bind both scores and
passage locations. Lexical degradation also selects a source passage. Scoped
Lance filters apply before bounded retrieval, then source hydration checks the
current record again. The agent episode lane allows three entries and 7,800
characters under the unchanged 12,000-character agent section cap.
Indexed passage replacement applies only to native episodes in automatic
injection; other memory types retain their compact projections and provenance
rendering.

Selected hits include bounded adjacent passages from the same episode (minus
text already in the hit), so nearby owner corrections around assistant
suggestions survive. Speaker labels always come from producer-owned source
spans, even when clipped or when the source contains role-like text. Ranges
that cross a source boundary split by producer span. Final packing clips each
source body separately and reserves its full speaker label and context header;
a small budget drops context rather than attribution.
Selected episodes may also include up to two adjacent turns under the same genuine
runtime session, owner, workspace, agent and engagement. Automatic context uses
spare packing capacity, capped at 700 characters per anchor; separate sessions
retain their own candidate seats. Explicit recall returns bounded contextual
passages with source IDs, revisions and offsets. These bounds can still miss
remote evidence within a long conversation; they do not make recall exhaustive.

`memory_episode_rerank` is optional and **unbound by default**: the repo-root
`llm-router.yaml` names a candidate profile in a commented-out mapping. Binding
it requires paired quality, latency and cost qualification, which the candidate
has not passed. The runtime offers
at most 24 eligible episode passages, validates a complete permutation of
server-issued IDs, pins the routed provider, imposes a three-second timeout and
preserves baseline order on failure. Source mutations invalidate its results.
Public-surface redaction applies to both the offered passage and its adjacent
context before either enters the optional reranker's model input.

Consolidation and quality classification receive complete sanitized
`conversation_sources`; compatibility excerpts remain supplementary. Logical
adapters split oversized sources with Unicode offsets, source roles and stable
root IDs. Exact supporting quotes retain bounded surrounding context for
speaker, condition and correction review (`memory_lifecycle_pair_v2`). Policies
that require qualification records must qualify this projection version; v1
records do not qualify v2. Existing explicit operator overrides still apply.
When grounding conversational sources, assistant and uncited proposals cannot
acquire owner-statement authority through extraction, including through stated
source aliases such as `correction` or `owner_confirmed`. Re-grounding an exact
quote retains its verified surrounding attribution and correction context.
Distinct verified citations from the same turn survive grounding and merging
but count as one independent observation; spans use the earliest recording time
per root, so replayed citations cannot stretch the qualification window. A newly retained citation marks the item for review;
replaying an unchanged citation preserves the existing decision.
Existing trust ceilings,
confirmation, independent-source counting and correction lifecycle still apply.
Recording times label capture time; occurrence dates stated or quoted inside
source text remain source evidence.

`POST /api/magician/v2/chat/memory/sources/repair` assesses retained sources with
`{"apply":false,"limit":1000}`. `apply:true` repairs only unambiguous sanitized
messages from the matching scoped session and reports missing/ambiguous sources.
Continue bounded batches with the returned `next_after_episode_id` as
`after_episode_id`; this also permits resuming an interrupted scan.
The session lifecycle fence and an episode compare-and-write protect interrupted
or repeated repair and concurrent forget. Repairs retain excerpts, episode IDs
and all non-source fields, journal index changes and refresh prompt snapshots.
No missing episode is recreated. Repair can fill null or blank source placeholders
while preserving existing source identities and other populated metadata. Unsupported source values or
conflicting speaker metadata remain unrecoverable.
Rebuild through the existing index maintainer;
`make memory-index-prewarm MEMORY_INDEX_REBUILD_FLAGS=--force` explicitly rebuilds
an assessed scope when needed. Scope parameters must identify the intended
workspace; never purge memory to migrate it.

The isolated runtime probe is `make build-memory-recall-eval`. Run its
`memory_recall_live_eval` example with `--config`, `--root`, `--principal`,
`--workspace`, `--cases` and a fresh `--output` file. The copied root must contain
a `.memory-recall-eval` marker and differ from the live config directory. Optional
`--repair true` exercises retained-source repair; `--answer-profile` and
`--rerank-profile` measure configured models without changing live bindings.
Expected evidence in the case file is evaluated only after product retrieval.
Reports contain private fixture evidence and must remain on the local eval
surface.

## User-knowledge maintenance and the knowledge lock

`memory/users/knowledge.json` is one multi-megabyte document per scope, written
under an exclusive file lock with a 10 s acquisition deadline. Its lifecycle
pass (`memory_lifecycle::runtime::pass`: quarantine, legacy normalisation,
expiry, owner-answer reconciliation, one bounded review) belongs to the scope:

- The 5-minute consolidation sweep runs it once per scope per tick
  (`MemoryConsolidator::run_user_knowledge_pass`, scopes from
  `user_knowledge_pass_scopes`) before the agent sweeps. A per-agent batch sweep
  runs only that agent's tier contradiction sweep
  (`run_agent_tier_contradiction_sweep`). `run_contradiction_sweep_for_agent`
  still does both for single-agent callers; the analytics maintenance endpoint
  runs the user pass once and the tier sweep per agent. The resurfacing worker
  also runs the pass; the sweep remains the fallback when it is off.
- At most `MEMORY_SWEEP_AGENT_CONCURRENCY` (4) agent sweeps run at once, for
  shared file-I/O contention (LLM back-pressure is the dispatch queue's job). A
  long tick delays the next one; missed ticks are skipped, never overlapped.
- The pass applies maintenance to an unlocked copy and takes the lock only when
  something changes, re-applying it to the document under the lock. It checks
  the hourly review budget (`memory.user_knowledge_bounds.review.max_per_hour`,
  default `MAX_REVIEWS_PER_HOUR` = 6) and the per-item attempt cooldown on the
  unlocked copy before the reservation lock, which re-checks both. Records
  that used up `review.max_attempts_per_record` tries are set aside in a
  maintenance write; queue order and set-aside are in
  [memory lifecycle](memory-lifecycle.md#scheduling-and-bounds).
- A busy lock is not a failure: the user pass reports
  `user.knowledge:lock_busy` as a skipped target, and an agent's tier sweep
  reports `memory_contradiction_sweep:lock_busy` as a skipped rule while its
  batch outcome (reports, drained marking) is kept. Lock timeouts from the pass
  keep their typed `FileLockTimeout`, so `is_memory_file_lock_timeout` matches
  them.
- A write (`update_user_knowledge`) diffs the preference history against the
  document it loaded instead of re-reading it, fingerprints the generation from
  the bytes it wrote, and parses, copies and serialises the document on the
  blocking pool.
