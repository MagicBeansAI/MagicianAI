# Bounded app workflow values and recipe IR

The foundation lives in `magician/src/magician_v2/apps/recipe_ir.rs`. It closes
the data and topology vocabulary recipes lower to V3 with, without a second
workflow runtime or an executable publisher language.

## Supported vertical

The foundation compiles a workflow value schema, validates values against the
exact compiled schema and compiles an immutable recipe identity. It lowers
`query`, `get`, `map`, `validate`, `emit_value`, `sequence`, `parallel`,
`switch` and the terminal `reconcile`, `contextual_round` and
`store_transaction` nodes onto the existing app-workflow and Artifact V3
task/execution/schedule/reducer owners. There is no recipe executor or generic
evaluation surface.

- Parallel branches run concurrently through their own V3 node states and join
  in stable semantic-key order into the declared fixed-length array. Nested
  Parallel is denied so the width ceiling cannot multiply.
- `retry`, `mark_uncertain`, reasoning, general mutation, general tool/action
  dispatch and every deferred node are typed unsupported. Retry needs a
  semantic attempt owner and a retryable-error classifier (V3 lease recovery
  only repeats an identical interrupted read); MarkUncertain cannot produce the
  canonical uncertainty transition and receipt its output contract needs.
- Package-lock admission requires an installed immutable Recipe runner/member
  and binds content, schemas, topology, compiled plan, supported-node set and
  runtime contract; launch and revalidation require the same binding. There is
  no default, generic or fallback Recipe shape.
- Resource cleanup validates historical package and binding hashes against the
  sealed resource tree without requiring the old runtime to match the current
  binary; it grants no runnable lock or dispatch authority and only lets the
  settlement owner finish interrupted work.

Compiled schemas, validated values and compiled recipes are not
deserializable. Wire declarations are data with strict unknown-field
rejection. There is no code string, script, callback or generic extension node.
Contextual rounds carry a bounded flat value-expression table whose
dependencies the compiler checks.

## Workflow schema and value contract

The v1 type algebra is a flat indexed arena, walked iteratively. The schema
must be acyclic with every definition reachable; values must be trees.

Closed type set:

- unit, boolean, signed integer, normalized plain decimal, bounded text and
  markdown, closed enums, normalized RFC3339 timestamp, typed entity
  references and opaque logical references;
- entity-projection, artifact, mutation/external-effect receipt and named
  logical resource references;
- explicit nullable values;
- arrays with mandatory min and max item counts;
- records with unique property names and required/optional fields;
- tagged unions with one exact discriminator and a closed variant set.

Recipe workflow inputs may use a tagged union as root so a `switch` consumes
the discriminator; Auto workflow inputs stay closed record roots. Optional (may
be absent) and nullable (present null) are distinct. Arbitrary recursive JSON
is not a workflow value type.

This is also the manifest workflow-value authority: manifests may carry the
bounded `value_schema` graph for inputs and results; legacy flat scalar-object
declarations keep their serialized identity and are projected
deterministically at admission (never both). Recipe compilation, manifest
validation, launch/result sealing, composition and generated SDK codecs all
compare the same schema references and digests.

| Ceiling | v1 maximum |
| --- | ---: |
| Schema nodes / edges / depth | 256 / 1,024 / 32 |
| Value nodes / edges / depth | 8,000 / 16,000 / 32 |
| Record properties | 64 |
| Tagged-union variants | 32 |
| Array items | 256 |
| Text bytes | 262,144 per declared text type |
| Canonical schema / value bytes | 128 KiB / 512 KiB |
| Provenance / resource references | 256 / 256 |

A value carries its schema reference, handling labels, a payload-minimal
provenance map, and typed logical resource references restricted to
`entity:`, `artifact:` or `receipt:` namespaces. Provider IDs, paths, URLs,
task/execution IDs and bearer material have no field. Resource and provenance
labels may raise classification or narrow processing; the value label cannot
lower schema, source or resource floors. This is structural only; execution
still uses the policy owner for the live label join.

## Deterministic encoding and identities

Schema declaration order is normalized by traversal from the root. Record
fields, variants, resources, provenance and recipe nodes use ordered maps and
sets; array items, sequence steps and retry/topology order carry identity. The
compact v1 JSON encoding streams into a bounded length/hash sink; schema, value
and recipe references bind the versioned canonical digest and length.

The TypeScript SDK exposes `recipeNode` builders only for the admitted kinds
(including `recipeNode.reconcile` and `recipeNode.storeTransaction`) and
`defineRecipeBundle`; they sort maps and validate names, references and
ceilings but are declarations, never authority proofs. Rust package admission
is the canonical compiler. Per-app generation emits the workflow input/result
and action contracts, schema descriptors and codecs from the compiled bytes,
plus form metadata, opaque handle types and the custom-surface bridge
contract. Generated codecs reject recursion, over-deep nodes, overflow,
unknown variants and internal task/execution IDs in place of public handles.

**Query/Get/Map.** Query returns an opaque projection reference that omits the
store record id; the raw locator lives only in a sealed private lifecycle
sidecar. Get resolves that sidecar within the same run, replays the original
one-row query through current authority and requires the exact entity,
revision, content, labels and provenance. The public digest is rejection
evidence, never bearer authority. Map embeds its immutable operation body in
the Recipe, package-lock and plan identity; admission recompiles the bounded
mapping algebra and verifies `mapping_digest`. Resource inputs and executable
expressions are refused.

## Recipe topology contract

The recipe IR is a normalized-name map with one root and exact recipe and node
input/output schema references. Each executable node declares:

- effect class, idempotency and uncertainty posture;
- exact primitive/action/target-app and grant/resource-scope references where
  applicable;
- active time, input/output bytes, cost, tool-call and parallelism ceilings;
- bounded retry state;
- cancellation propagation and acknowledgement timeout;
- provenance join behavior;
- an authoritative, derived or feedback-only output kind.

Authority fields are namespace-checked logical declarations; mapping and
idempotency identities are digests. They are ceilings for later live
intersection, never bearer authority.

Closed core vocabulary: `query`, `get`, `map`, `validate`, `call_tool`,
`invoke_action`, `mutate`, `reconcile`, `contextual_round`, `run_procedure`,
`agent_as_tool`, `sequence`, `parallel`, `switch`, `emit_value`,
`emit_artifact`, `emit_receipt`, `retry`, `mark_uncertain`.

- Sequence schemas chain exactly; Parallel output is a fixed-length array whose
  item type matches every branch; Switch input is a closed tagged union whose
  cases match its variants; `mark_uncertain` requires an exact `status` union
  of `completed` and an external-effect receipt.
- The graph is validated iteratively for missing nodes, cycles, reachability,
  single control ownership, node/edge/depth/fan-out/parallelism limits and
  aggregate resources.
- V1 retry is limited to no-effect and read-only children, even with a
  declared idempotency reference.
- V1 accepts only provenance preservation. Map, validate and reasoning outputs,
  `emit_value`, `emit_receipt` (an exact-schema pass-through) and
  `mark_uncertain` are derived; a control result cannot claim stronger
  authority than a child. A caller-supplied `intersection` enum is refused.
- Follow-on nodes (`bounded_for_each`, event/user waits, approval, timer,
  handoff, contribution) deserialize only as deferred declarations and fail
  with `UnsupportedNode`. No catch-all execution path exists.

## Recovery and evolution

Every recipe pins a positive topology revision, v1 contract version, schema
references and topology digest; revisions > 1 name their predecessor. The only
v1 migration policy is `recompile_required`: recovery reopens the exact
compiled identity and never reinterprets older bytes under newer semantics.

The immutable launch binding seals member bytes, package lock, contract and
topology revision and digest, compiled plan, schema refs, normalized input and
resource ceilings before the deterministic Artifact root exists. The package
lock (version 5) and the lowered plan keep the reviewed runtime contract identity; the build
source digest is reported separately by `app check`, so compatible source
changes do not rewrite locks (see
[Runtime compatibility](app-runtime-compatibility.md)). Unknown identities and
changed schemas, authority, topology, plans or package bytes fail validation.
Recipes not using contextual rounds keep their original node-set binding.

**Node lifecycle.** Artifact V3's execution schedule is the only mutable node
lifecycle authority. Each node has a deterministic step identity and persists
`dispatch_reserved`, `started`, a cross-process owner/claim epoch and bounded
lease, typed input/retry binding, deadlines, output reference and
terminal/skipped/uncertain state under the task write lock.

- Only the claim winner mints the move-only physical permit. The short claim
  lease is separate from the immutable active deadline; an owned heartbeat
  renews only the same live owner/epoch during physical Query work.
- After proven lease expiry and before the active deadline, recovery may adopt
  sealed output from that owner/epoch or retry the same read-only input under
  a new epoch. Output is accepted only when its sealed owner/epoch match the
  current permit, and output and canonical completion must precede both the
  lease and the deadline. Lease loss, lateness or substitution settles
  uncertain without disclosure.
- The recipe sidecar is non-authoritative evidence (plan and input, dispatch
  intent, output bytes, branch-skip evidence, mirrored cancellation request)
  and is never consulted for readiness or terminal disposition.
- The deterministic root and pristine `app_recipe_v1` schedule commit in one
  task-locked V3 transaction before cancellation or a claim can see the root.
  Startup adopts or tombstones the one pre-root reservation, closes
  schedule-to-root gaps, adopts retained outputs before downstream readiness,
  and never re-dispatches settled nodes. Missing schedule events are rebuilt
  from schedule truth with transition identities that include the claim epoch.
  Generic schedule refresh cannot overwrite a recipe schedule.
- Member, lock, grant, topology or schema drift settles every node and the root
  before terminal publication. Terminal projection retries to a fixed point,
  and the launch key is released only after it succeeds.
- The recipe event log has an 8 MiB new-append ceiling; committed event IDs
  stay replayable after exhaustion, new events defer, and canonical reducers
  run first so event repair cannot hold terminal truth open.

**Cancellation** is recorded first in the canonical schedule under the task
lock. An already-settled root wins a late cancel. Otherwise pending and
reserved nodes become cancelled, a started read keeps settlement ownership
through its acknowledgement deadline, and the root is cancelled only after that
resolves. Missed node, aggregate or cancellation-ack deadlines settle
uncertain; failed or ambiguous dispatch is never relabeled `cancelled`. A live
cancel arms a single-flight deadline job after the reducer commit, and startup
rearms it from the sealed schedule. At the due instant started work becomes
uncertain and unstarted work cancelled.

## Deterministic reconciliation

`reconcile` is a single terminal node. The immutable recipe member declares
bounded host-read actions and field mappings to the app's own entities;
workflow `uses` must name exactly those tools and `may_mutate` exactly those
entities. General CallTool and Mutate stay unsupported. Input is a reviewed
record schema equal to the workflow input; optional `input_parameters` binds
named input fields to scalar provider parameters (absent or null keep pinned
defaults). Scope, provider, selectors and targets stay package/runtime owned;
the physical provider checks argument types and bounds before I/O.

- **Sources.** Up to three additional `sources`, each sealing an exact
  primitive/action and document or bounded-page result shape, chosen by
  package-owned name. `when_input_present` runs a source only when its input
  field is non-null; no output can invent another read. Missing enabled reads
  fail before writes; disabled sources leave targets untouched; conditional and
  document sources cannot seed or retire records. Tool-call ceilings count all
  declared sources. The claim/cancellation fence is renewed before each read,
  store page and the terminal transaction.
- **Locking.** The recipe's primitive/action references must equal the selected
  source dependency binding; a stale reference fails publication with an
  instruction to refresh catalog references.
- **Execution.** The Artifact owner resolves the current built-in provider
  witness; the workflow uses the existing parameter proof, locked identity,
  disclosure permit, physical effect owner, resource journal and labeled result
  retention. No model call. Host data comes only through its approved Apps
  binder, never a product API or direct host DB.
- **Planner.** Literals, source scalars with explicit defaults, server
  timestamps, and lossless whole-row JSON via `source_document` under a
  reviewed byte ceiling (oversize fails, never truncates). `allow_null` keeps a
  present null; a missing field needs a fallback. `update_fields` is an
  allow-list (empty = create only). Seeds create missing named records. Reads
  reject duplicate natural keys, malformed data, partial snapshots and overflow
  before writing. Missing rows are patched only when the source reports a null
  next cursor and no truncation; without completeness metadata a page may only
  populate or update non-retiring targets. **Reconciliation never deletes
  records.**
- **Bounds.** Source snapshots ≤ 200 rows. Existing-record scans follow the
  app-store cursor in pages ≤ 100 rows up to the reviewed `max_existing_rows`
  budget; repeated cursors and non-progressing pages fail before writes. Each
  page revalidates revisions, bytes and policies in one registry read
  transaction; page size is fixed (part of cursor identity). Terminal
  reservation uses the mutation owner's write bound.
- **Commit.** The terminal commit owner persists one immutable intent and
  writes atomically with expected revisions and stable create IDs. No-change
  completion is read-only with no fabricated receipt. Output has one fixed
  receipt-summary schema. Interrupted receipt-bound nodes never enter
  identical-read retry: ambiguity is uncertain and the commit/resource journal
  owns recovery.
- **Timestamps.** Root resource admission uses the same boundary timestamp as
  its freshly resolved authority (a later sample is rejected as stale); the
  compiled-effect dispatcher uses the policy resolver's admission timestamp for
  disclosure, then re-resolves authority at the resource boundary; physical
  deadlines resample at final I/O.
- **Result bytes.** Provider transport limits are part of effect identity; an
  80 KiB continuation-result ceiling (within the 256 KiB aggregate) bounds
  retained completion bytes, so a 512 KiB transport allowance does not widen
  retained continuation or grants.
- **Stack.** The recipe root, source-read and reconciliation helpers, entity
  read branches and the shared pre-dispatch effect owner return boxed futures
  at their definitions (boxing only the outer call still builds large inline
  children).

Examples: Town Square `recipes/sync-roster.json`; Memory Learning
`recipes/sync-queue.json` (scalar input bindings, nullable fields,
non-retiring page); Brainstorm `recipes/sync-maps.json` (optional `read_map`
source, full JSON snapshot). `make test-app-reconciliation` exercises the
production planner.

## Native progress and contextual rounds

The shared workflow effect owner separates terminal output from native
progress slots. Each slot has a node/member identity, stable mutation key and
its own reservation attempt generation, so receipt recovery cannot reset
another member or publish task completion. Revision one stays the terminal
origin. Native callers need a current node claim with reviewed internal
mutation authority and the same schema, grant, revision and resource fences.
Intermediate receipts use a host receipt schema, keep reviewed policy floors
and cannot emit terminal contribution ports. Native progress rechecks its claim
under the task guard before persisting an intent and before entity I/O, so a
displaced worker cannot publish after a replacement began recovery.

`contextual_round` binds one paginated host source, a reviewed semantic step,
per-participant and aggregate budgets, fixed App-store context queries,
eligibility rules and mutation templates. Its flat value program does only
bounded deterministic transforms; model values cannot choose entity, record
id, expected revision or query operator, and the shared resume cursor never
depends on a model or participant. Deterministic exclusions buy no model call;
context failures are per-participant. Each permitted participant runs the
reviewed step through the normal dispatcher; output and actual spend are
retained before commit; uncertain physical outcomes are never free or blindly
retried. A receiptless quiet round can complete with sealed outcome evidence;
an unfinished round cannot. Receipts are aggregated only after every
participant is terminal. See
[Behavior operation recipes](app-behavior-recipes.md) for dispatch and spend.

- `context_mode` defaults to `snapshot` (omitted from canonical JSON for lock
  compatibility). `progressive` requires `max_concurrent: 1`: the owner reads
  each participant's personal queries just before dispatch. Shared queries opt
  into `refresh_before_dispatch: true`; after the previous commit the owner
  refreshes that context, rechecks eligibility without a model, and seals the
  dispatch context before the first claim. Plan identity stays fixed; retries
  reuse the sealed view. An unresolved preceding commit blocks later dispatch.
- The query page limit bounds context per request, not total records. Reserved
  context names cannot shadow the participant or round.

## Deterministic App-store transactions

The root-only `store_transaction` node uses the existing workflow and entity
owners for reviewed mechanical reads and writes. Its fixed source map uses
exact locked host-read primitives (no provider, action or scope choice from
data). Its program binds scalar input or earlier context into fixed own-store
query comparisons and prepares create/update/delete for declared entities;
updates and deletes carry the read revision. Live grants, disclosure, resource
admission, schema validation and the atomic mutation receipt stay
authoritative.

- Reviewed `source_parameters` evaluate before host I/O and may replace only
  declared public scalar parameters (such as limit clamping).
- Own-store `in` predicates bind bounded scalar arrays (look up returned keys,
  not scan). `query_when` skips absent lookups: false yields an empty window;
  unknown or non-boolean is refused.
- Each query is either a bounded window or a complete cursor scan
  (`scan_queries`); scans keep a fixed page size, reject repeated cursors and
  refuse to stop short past the page budget; missing results after a truncated
  scan never prove absence.
- The bounded `summary` keeps selected cursors, partial flags and errors even
  with no writes. Receipt-free results have null receipt/sequence and no
  claimed revisions; nonempty plans always enter the mutation branch.
- Expressions: array construction, joining, byte length; `linked_text_rows`
  validates a closed JSON dictionary and ordered linked text rows against
  reviewed names and limits. No model or participant input. Optional fields map
  to explicit nulls where required; canonical JSON sorts keys recursively.
- Ledger request IDs identify one exact request (reuse with a changed payload
  is refused). A retained terminal intent replays its original operations and
  timestamps through commit recovery; it never re-reads to invent a new intent.

Users include the Learning decisions, Meetings control-request ledgers, Claims
decisions and Town Square's policy/feed/posts/reactions/groups workflows. Their
signed host apply seams stay separate: recording a control request or Claims
decision does not itself act on the host.

### Checkpoint encoding bounds

Workflow checkpoints are sealed compact JSON, size-checked before registry
publication against a 2 MiB limit the reader shares. A legacy pretty-printed
run-state may be up to 4 MiB on disk only when its compact content fits 2 MiB
(depth, node count, identity and HMAC checks still apply), so whitespace never
becomes workflow capacity.
