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
episodes retain their existing text fields and neutralize memory-boundary tags
before indexing or rendering. Index schema v11 forces older
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
