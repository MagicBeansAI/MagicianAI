# Content Sources And Reader Adapters

`magician_v2::content_sources` is the provider-neutral acquisition boundary for
user-defined feeds, recurring monitors, research, and future content consumers. It owns
discovery/read contracts and transport policy. It does not own feed matching, monitor
conditions, scheduling, notifications, or presentation.

## Contracts

- `ContentCandidate` is the cheap pre-fetch record: stable source identity, bounded title
  and selection text, canonical URL, timestamps, privacy, content hash, provenance, and
  structured metadata.
- `ContentDocument` is an optional bounded full-content result tied to the exact candidate
  identity.
- `ReadSelectionEvidence` is mandatory for feed and recurring-monitor reads. Invocation source
  has no unspecified/default value, so an omitted source cannot be interpreted as a privileged
  internal read. This makes pre-fetch ranking an enforced contract; an interactive user read
  remains explicit intent.
- `DiscoveryAdapter` returns candidates; `ContentReader` returns documents.
- `ContentSourceRegistry` rejects duplicate adapters, unsupported capabilities, invalid or
  oversized output, foreign identities, disallowed remote query/content transmission, and
  discovery cost that disagrees with the adapter's `metered` declaration. Provenance strings,
  content URLs/hashes, display names, and observation/fetch timestamps are bounded and validated
  alongside title, text, and metadata.

The schema is versioned independently from feed or monitor storage. Consumers must retain
the source identity and provenance instead of inventing provider-specific identifiers.

## Sources

### Manifest-driven capability sources

`ScopedDeterministicCapabilityInvoker` is the shared execution substrate for CLI-template
skills used by agents and application services. It performs no LLM selection. It preserves
the existing scoped skill directory and `.env`, sandbox, timeout, spend gate, progress,
failure folding, active-owner identity, and learning telemetry. Calls carry a non-content
source tag (`agent_tool`, `user_feed`, `recurring_monitor`, `interactive_read`,
`observed_source`, or `internal_system`) for attribution without putting private queries in logs.

The agent primitive path uses this invoker, so feed invocation does not create a second
provider implementation or silently change how LLM-selected tools run.

Provider-specific Rust adapters are not required. A tool skill opts into discovery by adding
`metadata.magician.content_source` beside its runtime contract and typed actions in the
same `SKILL.md`. Generic registration scans the supplied shipped and scoped skill roots,
validates every strict versioned extension against that same-file action contract, and registers
a `CapabilityDiscoveryAdapter`. Ordinary skills without the extension are unchanged.

Two intentionally bounded output contracts are supported:

- `mapped` uses RFC 6901 JSON pointers to map an existing skill's JSON response into candidate
  fields. It supports typed request-option mappings, fixed arguments, response/item metadata,
  cursors, provider error envelopes, and decimal or microunit cost fields.
- `canonical_v1` lets a Python, Node, shell, or other capability emit provider-neutral
  `{items, next_cursor, cost}` JSON directly. Rust still attaches trusted scope, identity,
  privacy, provenance, canonical URLs, hashes, limits, and validation.

The manifest is a bounded adapter declaration, not a general transformation language. Complex
provider parsing stays in the skill script and emits `canonical_v1`; simple list/search APIs
use `mapped`.

Registration accepts skill roots in priority order. The first directory for a
skill name wins (scoped-over-system shadowing). The owning directory must match
`capability.name`, adapter IDs must be unique, and the declared action must
compile to a deterministic CLI primitive. Every fixed, dynamic, and typed option
argument must be declared by that action, and every required action parameter
must be supplied before registration succeeds; extension fields are deny-unknown.
Legacy product sidecars fail validation instead of publishing duplicate or
ambiguous content routes. Installed content extensions participate automatically
in the operation/rung they declare. Bounded numeric `priority` supplies
deterministic within-rung order (lower first, then action ID); explicit ladder
action lists remain operator ordering overrides and availability gates for
system/browser actions. Adding a skill-backed adapter requires no action-ID edit
in Rust or runtime configuration. Standard runtime file-level symlinks are
supported; symlinked skill directories are not traversed. A broken installed
`SKILL.md` marker is reported once in catalog readiness as
`kind=skill_installation,error_class=broken_skill_installation`; it is not
duplicated or misclassified as both an unavailable discovery adapter and reader.
The broken higher package continues to shadow a lower package of the same name,
preventing accidental fail-open reactivation.

A canonical script emits:

```json
{
  "items": [{
    "source_item_id": "optional-provider-id",
    "title": "Result title",
    "cheap_text": "Bounded provider-normalized selection text",
    "canonical_url": "https://example.com/item",
    "published_at_ms": 1784707200000,
    "source_label": "example.com",
    "metadata": {"rank": 1}
  }],
  "next_cursor": null,
  "cost": {"commodity": "credits", "amount_microunits": 2}
}
```

The script cannot choose identity scope, privacy, provenance, observation time, or content hash;
the generic Rust boundary attaches or computes those. Metered manifests must return valid cost,
and an adapter may return a cursor only when both its capability declaration and input mapping
enable cursors. Both `mapped` and `canonical_v1` modes isolate malformed result rows so one bad
provider item does not discard valid neighbors.

### Existing comms-assist channels

`candidate_from_channel_message` projects the provider-neutral `MailMessageMeta` produced
after channel distillation. It therefore applies uniformly to Gmail, WhatsApp, Telegram,
AgentMail, iMessage, and future `ChannelAdapter` implementations. It uses only locally
distilled summaries and information briefs from rows whose state is exactly `done`. Pending,
failed, skipped, sensitive-suppressed, and suppressed rows do not project even if retry state
still carries a previous derived summary. Raw message bodies do not project; channel candidates
remain `private`.

The function is a projection, not a second channel adapter. Feed integration should page
the existing channel store and apply this projection rather than resyncing provider data.

### RSS, Atom, and JSON Feed

`RssDiscoveryAdapter` parses syndication formats through `feed-rs`, removes common tracking
parameters from canonical URLs, preserves native source identity in metadata, and emits
bounded public candidates. Fetches have response-size, timeout, redirect, scheme,
credential, and non-public-network controls. The configured feed URL reaches its origin;
the user's feed intent/query does not. RSS discovery also accepts provider-neutral ETag and
Last-Modified checkpoints. A `304` preserves the sent checkpoint, while cursor/validator state is
advanced only after the consumer durably accepts selected candidates.

### TinyFish search and provider-direct fetch

`web-via-tinyfish/SKILL.md` embeds a free web-search declaration at priority
25, ahead of the Exa (100) and Tavily (200) adapters in the automatically
discovered `public_search` rung. Ordinary agents continue to call
`content_search`; installing and credentialing this scoped skill changes the
provider order without exposing a provider-specific schema to those agents.
If TinyFish is unavailable, the existing controller moves through its normal
bounded fallback ladder.

The same governed package exposes a direct `fetch` action for explicit
TinyFish requests and bounded extraction of up to ten public HTTP(S) URLs.
That action is not a `content_reader` extension. The current reader contract
requires Rust to own URL validation, DNS/redirect policy, acquisition, cache,
and provenance before passing a local file to an extractor. Registering a
third-party remote fetch under that name would bypass those guarantees.
Claim-bearing research therefore continues to use `content_read`; direct
TinyFish Fetch is an extraction tool rather than claim-eligible evidence.

The package declares the official `tinyfish` executable directly. Typed action
mappings lower governed parameters to inert `tinyfish search query` and
`tinyfish fetch content get` argv. The scoped secret authority injects
`TINYFISH_API_KEY` only for execution. Skillshub `setup-deps` installs the
pinned official CLI under the shared Node 24 runtime; there is no
TinyFish-specific installer or protocol wrapper.

### Exa semantic discovery

`semantic-websearch-via-exa/SKILL.md` embeds the governed Exa content-source declaration used by
the generic capability adapter. The extension declares explicit remote-query permission, Exa's
100-result bound, typed options, JSON-pointer mappings, relevant-passages-to-`cheap_text`, and
USD cost encoding. The self-contained `exa-search` executable remains the sole owner of Exa
request construction and provider transport; the governed runtime resolves and injects the
exact scoped credential only after authorization. There is no Exa-specific Rust adapter.

The adapter asks the skill for highlights with full content disabled. The static reader owns
the optional full-page retrieval and extraction after selection.

### Static public-page reading

`htmltotext/SKILL.md` embeds the shipped `static-http` reader declaration without a
provider-specific Rust adapter. Rust owns acquisition and policy; the extension binds the
already-shipped local Trafilatura/BeautifulSoup capability as the extraction stage. The
capability receives a scoped scratch file, never the remote URL, page bytes through stdin,
or a user query. Replacing the local extractor is therefore a skill/manifest change.
The extractor recovers content tables the article extractor drops (doc sites wrap
them in scroll containers) and carries a table's unit caption in above it; both
rules and the reason are in the skill. It also judges whether the page rendered
its content on the client: content in hydration payloads (`application/json`
scripts, framework flight chunks, JSON-bearing attributes such as Astro `props`)
that never reached the server-rendered DOM, or an empty application root beside
a script bundle. The verdict rides the envelope (`/client_rendered`, declared as
`shell_pointer` / `shell_reason_pointer` in the reader manifest) and the reader
fails such a read as a `javascript_shell` — the same code the phrase detector
uses — so the retrieval ladder escalates to the browser handoff instead of
serving a blurb as the page (a character-count quality boundary alone cannot
tell a blurb from the page). The scope's
capability revision — the content cache's key — digests only `SKILL.md`, so a
change to the extractor's output must bump the skill's `version` or cached
extractions outlive the fix; and because the manifest rejects unknown fields, a
new output pointer must not land in `SKILL.md` before a binary that knows it runs.
The scratch path is carried as an exact product-brokered `workspace_path`
authority, separate from model arguments. Direct agent calls cannot forge that
binding; non-brokered typed paths are restricted to normalized entries under
the scope's `workdirs/` tree. A governed extractor's strict bounded stderr
failure envelope survives as a typed code/message so retrieval can distinguish
an unsupported document from an opaque process failure.

The shared public HTTP transport is used by both static reads and RSS. It:

- accepts only bounded HTTP(S) URLs without credentials;
- rejects local/non-global literal addresses and DNS answers, including mixed answers;
- disables process HTTP proxies, pins the validated DNS addresses into each request, and
  revalidates every manually followed redirect;
- rejects HTTPS-to-HTTP downgrade redirects, loops, excessive redirects, error statuses,
  unsupported/missing static-page content types, and decoded bodies above the byte limit;
- carries `ETag` and `Last-Modified` validators for conditional revalidation.

The cache lives under the configured runtime root at `content_sources/cache`. Scope names
are hashed into separate principal/workspace trees. URL records contain validators and
document metadata and are keyed by reader ID, capability revision, requested read depth, scope,
and URL. Extracted text is stored once per BLAKE3 content hash within that scope,
so different RSS/search identities can reuse an exact page body without inheriting one
another's identity or provenance. Fresh `cached_ok` reads avoid network and extraction;
stale `cached_ok` reads conditionally refresh and may use their stale entry if refresh fails;
`fresh` reads fail instead of silently degrading. Concurrent reads for one scope and URL
coalesce behind one fetch/extraction.

Cache maintenance is scoped and mutation-safe. Reads and writes trigger it at most once per
scope every 15 minutes. It removes URL records older than 30 days, collects unreferenced
content-addressed bodies, repairs tampered bodies on the next write, removes abandoned
scratch files, and evicts oldest URL references until the scope is at or below 512 MiB
without deleting a body still referenced by another URL. The extractor also owns a
cancellation-safe scratch guard, so dropping a read future unlinks its downloaded page
immediately. Shared CLI-template capture is bounded to 64 MiB stdout and 8 MiB stderr;
progress lines are independently bounded before publication.

The deterministic quality gate is depth-aware (`gist` versus `full_text`) and uses only
bounded structural completeness (characters/words), not topic-specific phrases. Invalid,
empty, or oversized extractor output fails closed. JavaScript, login, and interaction outcomes
remain typed inputs to the broader retrieval ladder rather than implicit browser launches.

### Unified scalar and vector research

`content_search` and `content_read` each expose one public capability with two
request shapes. A scalar `query`, `candidate`, or `url` keeps the original
single-need result contract. A `requests` array activates bounded fan-out and
returns stable per-request results; one vector item is scheduled serially.
There are no separate batch providers or duplicate model-visible schemas.
Parameter denies and conditional approval rules inspect `common` plus every
object in `requests`, so the vector form cannot bypass a scalar URL, query, or
other per-branch restriction.

Every branch calls the same `content_search` or `content_read` core, preserving
scope, configured provider/fallback order, cache, selection receipts, authority,
spend gates, attempt telemetry, and typed failures. Results return in request
order even when branches finish out of order. Vector calls are for independent
work (comparisons, subquestions, independently selected pages); dependent
search → read → refine chains remain sequential.

Requested global concurrency can only narrow
`progressive_retrieval.max_parallel_actions`. Hard ceilings: four search
branches and five read branches. Reads default to one in-flight branch per host
(hard ceiling two). Each page keeps its own sequential static → verified replay
→ public rendered → owner/auth ladder; parallelism never races fallback stages
for one URL or shares a browser session between pages. Lightpanda's process-wide
engine permit is enforced at the rendered-reader boundary: cache/static/provider
work for other branches may overlap, but overlapping content-acquisition
Lightpanda sessions cannot be launched.

Vector reads default omitted depth/output to `gist`; callers must ask for
`full_text`. This is vector-only—the scalar reader keeps its existing default.

The bounded model projection distinguishes discovery from opened evidence:

- Search records carry `evidence_role: discovery_only` and
  `claim_eligible: false`. Snippets select pages; they cannot support factual
  claims. Returned `url` may be passed directly to `content_read` (no
  `read_result` continuation is needed to reconstruct the candidate envelope).
- A receipt-verified candidate snippet may satisfy a cheap gist request without
  network I/O, but still carries `fetch_status: not_fetched`,
  `evidence_role: discovery_only`, and `claim_eligible: false`. It is not an
  opened source.
- Read records from an actual reader carry `requested_url`, `final_url`,
  `redirected`, title, excerpt, and a neutral `fetch_status`. `complete` means
  the fetch finished, not that the page is relevant or proves a claim.
- Retrieval attempts are not answer content. When an alternate opened page fully
  supports the claim, a blocked, redirected, empty, stale, irrelevant, or
  authenticated candidate is superseded and omitted. A source limitation is
  reported only when the failed attempt leaves a material evidence gap. The
  shared output synthesizer applies the same rule so a later rendering pass
  cannot turn an abandoned attempt into a user-facing caveat or add unsupported
  advice.

Precision review before agentic terminal publication sees only successful,
claim-eligible opened-page results, never discovery snippets or failed fetches.
An unsupported draft returns as a bounded repair observation; the runtime does
not prescribe a source, query, or tool. Re-submitting without new claim-eligible
evidence receives explicit feedback; three unsupported terminal drafts fail
closed. A faithful draft may use any alternate opened authority and does not
mechanically retry a dead URL.

One shared deadline is divided by elapsed wall time. `minimum_successes`
defaults to all branches; callers may lower it only for interchangeable
evidence. Once reached, the vector scheduler cancels unfinished sibling branches
through a child token while the owning execution remains live. Parent
cancellation still propagates. A post-permit cancellation check closes the race
where a queued branch could acquire a concurrency permit in the same poll that a
sibling satisfied the evidence threshold.

Per-branch controller envelopes remain lossless under `results`. A separate
bounded `evidence` lane carries the top search records or a deliberate page
excerpt; capability-owned projection contracts prioritize that lane. Search
evidence is ordered round-robin by candidate rank. Vector telemetry reports wall
time, summed branch time, estimated parallel overlap saved, and aggregated
actual costs. Cost budgets in `common` are still enforced independently by every
branch; hard branch limits are the aggregate structural bound, not a shared
pool. Failed, cancelled, or error envelopes cannot make the aggregate look
partially successful and cannot contribute to the model-visible evidence lane;
raw envelopes remain inspectable. Degraded and actionable handoff/approval
envelopes remain partial.

Autonomous compiled-pack results are normalized from `ActionResult::Text` back
into the pack's canonical JSON before materialization and projection, so
`/evidence` and `/candidates` address real fields.

Canonical vector spelling puts shared fields in `common`. The runtime also folds
recognized shared scalar fields (`fresh`, `limit`, `depth`, and pack-injected
`timeout_secs`) from the top level into `common`. It rejects branch-owned scalar
fields (`query`, `candidate`, `url`, receipts, grants) beside `requests`, and
rejects conflicting top-level and `common` values. Policy checks inspect
effective top-level, common, and per-request values, so this recovery cannot
bypass a parameter deny or authority boundary.

Sleuth defaults to this bounded mode: one authoritative page per entity at gist
depth, then sources or refinement only for a concrete freshness, ambiguity, or
contradiction gap. Repeated `agentic_decision` is pinned to Luna
medium-reasoning/tool; other operations keep their governed mappings. No legacy
percentage quota for full-article reads and no forced report shape. Explicit
deep research can still expand sources, request full text, and use a long-form
report. A directly relevant authoritative primary source can satisfy a bounded
low-impact claim; corroboration is driven by stakes, ambiguity, freshness, or
contradiction rather than a fixed source count.

### Rendered, authenticated, and replay reads

Browser and verified API-replay rungs sit on the retrieval ladder without a nested browser agent.
For a known public URL, `browser.headless.read` uses one unique controller-owned session and a
fixed local sequence: open, network-idle wait, bounded render wait, title/final-URL/body extract,
then guaranteed close. The retrieval mode is exact and ignores generic browser mode environment
overrides, so a public read cannot become CDP. The configured
`content_acquisition.browser.public_read_engine` is a soft preference used only
for this isolated public, read-only path. The shipped preference is
`lightpanda`, making it the first browser engine for high-volume/fan-out public
reads after cheaper static/API/RSS readers have been exhausted. If resolution,
navigation, Web API coverage, extraction, or cleanup fails, that
controller-owned session is closed and the read advances through a bounded
`public_read_engine` → `engine` → bundled Chrome for Testing chain, skipping
duplicate or absent stages. The shipped full-fidelity engine remains
`cloak-browser`. This retry cannot apply to authenticated or side-effecting
work. Retrieval metadata records the final engine, fallback marker, original
preference, and attempted engine sequence. Omitting the preference uses `engine`
directly; omitting both uses agent-browser's bundled browser. High-volume here
means efficient cross-page fan-out; existing per-origin robots, rate, and
concurrency policy still applies.

Ordinary browser calls may override `engine` explicitly on their first call.
`lightpanda` is for fresh, high-volume, public, profile-free headless DOM/text,
accessibility snapshot, semantic navigation, and structured extraction; it is
not selected by a site allowlist. Use the configured full-fidelity engine for
rendered pixels, headed presentation, profiles/extensions/authenticated state,
file workflows, or anti-bot Chromium fidelity. Screenshot, PDF, and recording
commands are rejected on an active Lightpanda session rather than returning
placeholder pixels. Meeting and screenshot-preview paths inherit `engine`, never
`public_read_engine`. CloakBrowser's concurrent-session denial still closes the
failed daemon and retries its initial open once with bundled Chrome for Testing.
Observability records the engine that actually ran, including `bundled_chrome`
after a public-read or capacity fallback. CloakBrowser supports headless and
headed; Lightpanda is strictly headless. Ordinary sessions can request
`bundled_chrome` when the configured engine is unavailable.

Navigation and interaction remain calling-agent work. Scalar and vector
`content_search`/`content_read` calls return typed, expiring handoffs for fresh
headless navigation, visible headed owner assistance, or authenticated CDP
interaction. Headed does not imply logged-in. Identity-bearing deterministic
reads and interactions require distinct grants from `authorize_content_read`;
grants and CDP handoff sessions are bound to principal, workspace, domain,
action, mode, and expiry. “Approve Once” is claimed by one operation/session and
cannot be replayed. The approval also states that private page observations
return to the assistant model configured for the task; grants without that
explicit permission cannot run a private read or interaction handoff. Read
authority cannot authorize interaction. CDP uses a dedicated Magicutor proxy
session; cleanup disconnects the Magician session without closing the user's
browser.

`api_replay.read` runs before rendering when API Mining has a validated read-only capability.
It rejects private candidates, credential-bearing headers/query/body fields, cross-domain
targets, proxies, non-public DNS answers, and redirects; execution is pinned to the validated DNS
answers. A successful replay completes the rung without launching a browser. A
bare anonymous `401`/`403` transport response is ambiguous and stays on the
public ladder so the rendered reader can try next; it is not treated as proof
that user identity is required. An explicit login wall/paywall observed in
content still produces typed authentication remediation. Other unavailable
replay outcomes fall through once.

Browser/replay receipts contain requested and resolved mode, actual transport, resolved engine,
hashed session/capability identity, render/extract latency, approval receipt, and cleanup/handoff
outcome. They never contain cookies, auth headers, queries, page bodies, or credentials.

The opt-in live gate is `make test-content-retrieval-runtime-live-eval`
([Evaluation](#evaluation)). It constructs the production resolver, registry,
authority boundary, and configured browser engine, then invokes the same
compiled `content_search` and `content_read` cores (including vector requests).
It covers native discovery, static reading, deterministic browser reading,
replay-to-browser fallback, public handoff, and authenticated-read approval
without granting or opening private content. There is no second,
direct-transport canary; per-skill live execution belongs to the tool-runtime
canary lane.

## Observable Sources On Observe

### Uniform restart catch-up

Automatic item-producing Observe sources share the scope-owned
`observe_catch_up/config.json` policy and one process-boot admission ledger.
This is deliberately above provider cursors: an adapter retains its native
checkpoint and conditional-fetch semantics, while the shared controller
narrows the accepted history floor, per-source count, aggregate count, and
admission duration. Manual subscription runs stay outside the startup ledger.

Scheduled Notes and public-feed subscriptions reserve from the ledger before
acquisition. Candidate discovery receives the narrowed limit; returned rows are
also filtered by `published_at_ms` (falling back to observation time) before
selection and cursor commit. A source that completed its bounded historical
pass remains constrained to the post-boot floor on later automatic ticks in
the same process boot, preventing a scheduler tick from walking the omitted
backlog. Notes also restarts at its newest catalog page because its cursor is
an offset rather than a temporal checkpoint, then advances through bounded
pages until the eligible window or source cap is exhausted. Errors return
reservations and retry within the remaining boot budget.

Replay capability is reported rather than inferred. Notes is checkpointed.
RSS-family sources are current-snapshot-only and cannot promise recovery of
items no longer present upstream. Calendar consumes the same history-days and
per-source item choices but remains governed by the generated task prompt and
Task runtime rather than the heterogeneous feed-item ledger; it does not
replay every missed cron occurrence. The controller's duration stops new
item-source admissions; it does not cancel a source after a durable lease is
acquired. That lease completes or fails using the source runtime's existing
bounded provider path.

User-controlled continuous-source observation does not expose acquisition tools as
products. A strict `metadata.magician.observe_source` block describes a stable source identity
and named profiles. The backend loads scoped/system roots with normal scoped shadowing, isolates an
invalid definition from healthy siblings, hashes source/profile revisions, and projects only
profiles that explicitly opt into the Observe surface.

Eligibility is server-owned. The default policy permits exactly `rss.discover` at
`public_remote_read` authority, denies metered and browser actions, and requires an unattended,
read-only discover profile with one exact action and a public target. An RSS profile therefore
cannot fall back to Exa, static reading, a browser, CDP, an agent, or an LLM. Product Hunt and
arXiv ship as manifest-only RSS offers; users can add a custom public RSS/Atom URL through the
same validation and execution path.

Notes is a first-class, private built-in source rather than a synthetic RSS
feed or browser target. It observes Markdown below the scoped Local Markdown
root and any explicitly configured SilverBullet Space that passes the Notes-only
storage boundary. The built-in `notes.discover` action never invokes the public
content ladder, browser, remote endpoint, or an LLM. Notes settings remain the
authorization gate: a disabled Notes provider stays visible as **Needs setup**
and observation reads fail closed. Provider root configuration is revisioned,
while editing a note changes only that note's content revision, so normal writes
do not stale the subscription.

Subscriptions are stored per principal/workspace under
`observe_sources/subscriptions.json`. They retain source/profile revisions, exact action binding,
allowed cadence, intent, bounds, cursor/validators, insertion-ordered dedup history, health,
lease, and optimistic revision state. Notes uses a bounded `notes:<offset>`
checkpoint to work through more files than one run can admit, then returns to
the newest page; revision-aware fingerprints prevent unchanged Markdown from
being reprocessed while allowing an edited file to update its stable Notes
candidate. Persisted scope or identity inconsistencies fail closed.
The loader accepts the pre-release `lease_id` field as an alias for `lease_owner` and emits only
the canonical name on the next write; unrelated unknown fields remain fail-closed.
Source/profile/action/target revisions clear old checkpoints before the revised subscription can
run. Server list APIs use cursor pagination and authoritative totals; stale cursors return a
stable conflict so clients restart from page one.

Completed run telemetry is stored separately under scoped
`observe_sources/observability.json`. Its bounded ledger keeps durable per-source aggregates and
the most recent 4,096 run receipts across process restarts. Receipts contain only stable
source/profile/subscription/action identifiers, trigger/status, counts, timing, cost, cursor
movement, RSS modified/not-modified counts, response-byte totals, and stable error classes. Feed
URLs, intent, item text, credentials, and raw provider errors are excluded.
Deleting a subscription also removes its retained observability records.
Public aggregate field names such as `modified_targets` or
`candidates_discovered` are not treated as sensitive.

The scheduler and **Check now** use one global configured concurrency semaphore
plus unique per-run durable leases. Every run reloads the exact source
definition, invokes its pinned acquisition path, revision-deduplicates, applies
the optional keyword-plus-local-embedding intent filter and hard per-run
bounds, and issues an `ObserveMatch` receipt. Pause, deletion, revision
changes, or lease expiry before commit prevent handoff and cursor advancement.
Failures release the lease and enter bounded exponential backoff.

Selected candidates are atomically and idempotently admitted as provider-neutral
`ObservedContentEnvelope` records under the scope's
`observe_sources/enrichment_ingress/` directory. The envelope carries source,
profile, subscription, action, candidate, and selection-receipt provenance and
contains no mail-specific conversion. A bounded consumer normalizes readable
source text, preserves canonical URL, publication, author, receipt, and
source-affinity provenance, and idempotently admits the result as a typed `web`
or `note` resurfacing candidate. Note bodies remain private, stay inside the
scoped local pipeline, and carry only provider-relative source identity plus
bounded display content into resurfacing. The ingress file is removed only after
the resurfacing SQLite write succeeds; transient failures remain pending,
permanently invalid envelopes move to scoped
`observe_sources/enrichment_failed/`, and successful batches wake Worth a look
curation without waiting for its normal cadence. Feed, memory, and alert
materialization remain separate consumers rather than side effects of the
polling transaction.

`/observe` Sources has server-paginated **Listening**, **Available**, and
**Needs setup** tabs, fixed loading/error/empty states, supported-cadence
controls, pause/resume/stop/check, the built-in Notes target, and custom RSS
setup. `/observe/stats` adds durable health, discover-to-selection-to-processing
flow, conditional-fetch behavior, latency, pending/quarantined handoff counts,
and server-paginated per-source run history. Accepted web and Notes items are
reviewed by the shared resurfacing curator and can appear in **Today → Worth a
look**; web items retain their canonical source URL and Notes retain a stable
provider/path identity. HTTP:

- `GET /api/magician/v2/observe/sources`
- `GET /api/magician/v2/observe/subscriptions`
- `PUT|DELETE /api/magician/v2/observe/subscriptions/{id}`
- `POST /api/magician/v2/observe/subscriptions/{id}/run`
- `GET /api/magician/v2/observe/sources/observability`
- `GET /api/magician/v2/observe/subscriptions/{id}/runs`

`content_acquisition.observe` controls scheduler interval, shared concurrency, lease TTL, and
source-catalog policy. Source manifests and action readiness are reloaded at catalog/run
resolution; changing process-level Observe settings requires the normal Magician config reload
or restart lifecycle. Process-lifetime metrics remain available beside the durable scoped ledger
so operators can separate current-runtime throttling and policy pressure from historical source
health. Neither surface contains feed queries, item text, credentials, or raw provider errors.

## Runtime Handoff

`ContentAcquisitionResolver` is the process-owned composition boundary. It resolves one
`ContentAcquisitionService` for an exact principal/workspace and the current immutable capability
revision. The resolver uses the same scoped executable registry as Chat and autonomous execution,
passes the same compiled-dispatch spend authority and canonical runtime paths to the deterministic
invoker, scans scope-first skill roots, and stores extracted-content cache data under the configured
runtime root. Scope values that workspace normalization would rewrite are rejected before path or
cache construction.

Resolved services are single-flight cached by scope plus a revision covering the
complete `SKILL.md` contract. Skill mutations eagerly evict both executable and
content-service snapshots; out-of-band contract edits are detected by the
revision on the next resolution. A revision change during registry construction
triggers a bounded retry before publication, and explicit invalidation is
serialized with in-flight construction. Foreign-scope discovery/read requests
fail before an adapter is called. Both the service cache and its per-scope
build-lock map are bounded.

`catalog()` returns registered descriptors and credential-free unavailable-source records. A bad
optional manifest does not hide RSS or other valid adapters. `metrics()` reports per-adapter calls,
success/failure, latency totals, returned-item counts, metered microunits, policy denials, and reader
cache outcomes. It never stores query text, document bodies, environment values, or credentials.
Manifest filesystem failures are isolated to the owning skill while scoped shadowing remains
fail-closed. Generic broken-skill installations are likewise isolated and
reported once without hiding healthy adapters. Readiness accepts only
implementation types the deterministic CLI invoker can execute.

The resolver is available through Actix application data and the orchestrator. `SkillsApi` consumes
it for lifecycle invalidation. Feed matching, monitor scheduling, result persistence, API contracts,
and UI remain consumer-owned work.

## Progressive Retrieval Ladders

`RetrievalLadderController` sits above the scope-bound acquisition service.
Callers submit a versioned `RetrievalNeed` with one operation (`discover` or
`read`), evidence goal, scope, freshness, remote-data policy, maximum authority,
deadline, attempt bound, optional exact action allow-list, and per-commodity
spend budget. The controller, not an LLM, walks the ordered rungs in
`content_acquisition.progressive_retrieval`.

Every native or manifest-backed descriptor projects one stable retrieval action
with operation, rung, output kinds, authority, accepted options/targets,
parallel safety, and optional estimated cost. Source-specific request options
are removed before other actions run. Required unknown or duplicate actions,
operation/rung mismatches, and unsafe parallel configuration fail closed;
explicitly optional unavailable actions remain visible as readiness data without
changing order. Browser, authenticated-session, and API-replay actions are
separately configured and admitted by the rendered/authenticated authority
boundary, so static failure still cannot silently acquire broader authority.
Source-native actions are eligible only for an explicit matching action
selection (RSS also accepts an explicit feed target), so general discovery
cannot fan out to every vertical source.

Discovery merges and canonical-deduplicates bounded candidates, checks lexical
relevance and independent-source coverage, and stops only when the declared
evidence goal passes. Configured `eligible_parallel` rungs obey the global
concurrency and attempt caps; sequential rungs preserve exact action order.
Reads may reuse discovered inline selection text for gist goals only when the
complete candidate matches its server-issued receipt; otherwise they ignore the
text and use the scoped cache-first static reader. Direct URL calls do not
expose a caller-supplied inline-text argument. Structural checks reject thin
output, error/login/paywall pages, JavaScript shells, missing required metadata,
and low-information extraction. Login and JavaScript outcomes remain typed
boundaries for rendered/authenticated reads rather than implicit browser
launches.

Read goals select `gist`, `full_text`, or explicit `structured` output. Reader
descriptors project accepted media types and output capabilities into the action
catalog, allowing known PDFs to skip HTML extraction and preventing the JSON-LD
reader from running during an ordinary article read. Secure public fetches
enforce each reader's lower response bound under a hard 32 MiB ceiling.
Structured results must parse as a non-empty JSON object or array. Spend
admission is just-in-time per sequential action and per bounded parallel wave,
so an uncharged failed provider does not consume the next fallback's estimated
budget.

Discovered candidates carry opaque, single-use, expiring selection receipts bound
to principal, workspace, capability revision, the complete candidate envelope
(including its inline text and metadata), and invocation reason. A model cannot
retain a discovered URL while replacing its snippet: the fingerprint no longer
matches. For an interactive public URL, a missing, stale, replayed, or
mismatched receipt disables inline reuse and falls back to the reader ladder; it
does not turn supplied text into evidence. Feed and monitor reads still fail
closed without the exact matching receipt. Controller state keeps per-action
failure circuits. Every returned result has a sanitized trace ID,
configured/actual action order, classification, quality findings, latency, cost,
final/degraded status, and unmet-goal reasons. Queries, bodies, credentials, and
raw provider debug payloads are not logged.

### Failure classification and diagnosability

Each discovery provider is bounded independently by
`content_acquisition.progressive_retrieval.discovery_attempt_timeout_ms`
(15 seconds by default) inside the 20-second request deadline. The repository
seed and live runtime config carry the same `15000` default. A provider-level timeout is classified as `ProviderUnavailable`,
advances the sequential ladder, and contributes to that action's circuit
breaker. Exhausting the outer request deadline remains a terminal
`DeadlineExceeded`; the attempt bound cannot extend it. This prevents a hanging
first-choice provider from consuming the complete fallback window.

`ProviderUnavailable` is the catch-all for unrecognized errors.
`ProviderMisconfigured` names a trust-store failure distinctly. Its circuit
opens on the first observation rather than after the ordinary failure threshold.
It is **not** in the ladder-terminal set: a TLS fault in one adapter must not
veto other adapters on other rungs (Rust-native readers use rustls and are
unaffected by a Python interpreter's missing bundle). The classifier requires a
failure token (`verify failed`, `handshake`, `issuer`, `expired`, …) alongside
a topic token (`ssl`, `tls`, `certificate`); topic tokens alone would treat a
transient error against a host whose name contains `ssl` as non-retryable.

Attempt telemetry carries a bounded `failure_detail` from the error's root
cause, not the joined chain.

Governed CLI adapters emit `{"error": {"kind", "message"}}` rather than a bare
exception class name. Adapter messages are redacted before truncation, never
after: truncating first can leave a partial secret as a tail fragment.

Provider-owned credentials for public discovery are also kept separate from
user identity. A missing or rejected provider API key, or malformed capability
JSON, is `ProviderMisconfigured`: it opens that provider's circuit and falls
through. It never becomes `AuthenticationRequired` or `InvalidRequest`, both of
which would incorrectly stop the public-search ladder.

Trusted agents receive `content_search` and `content_read` as the normal
research tools. The web-researcher template grants no competing
provider-specific web-search or extraction ladder; Exa, Tavily, DuckDuckGo,
source-native adapters, RSS, and `htmltotext` remain registered behind the
controller. Sleuth retains direct source-envelope tools only for its separately
documented situation-awareness workflow. Other agents may still receive direct
provider tools for an explicit provider request or diagnosis, but the generic
path invokes the same scoped capability registry, spend authority, secrets,
sandbox, and telemetry. The legacy `web_fetch` handler delegates to the
controller and fails closed when the resolver is unavailable. It has no
direct-network compatibility bypass.

The default web-researcher does not grant Anthropic-backed web search or deep
research. `websearch-via-claude` and `deep-research-with-claude` are also in
its explicit deny list. Ordinary discovery and synthesis stay in Sleuth's outer
model over provider-neutral controller evidence; the OpenAI deep-research pack
is reserved for an explicit exhaustive-report request.

## Consumer Rules

1. Feed and monitor definitions select source adapter IDs; adapters do not inspect product
   definitions.
2. Feed matching runs over `cheap_text` and metadata before optional full-content reads;
   feed/monitor reads must carry matching selection evidence.
3. A feed may combine channel candidates and external candidates in one result stream.
4. Private/restricted content remains local unless a later policy explicitly authorizes a
   derived artifact. Raw channel bodies are never part of this contract.
5. Metered search/read adapters must be registered only after explicit runtime config is
   resolved. Missing providers fail instead of silently selecting a paid fallback.

## Evaluation

- `make test-content-reader-eval` — provider-free extraction fixtures and the
  production depth-quality evaluator.
- `make test-content-retrieval-eval` — the focused retrieval gate (catalog
  validation, sequential fallback, receipts, quality classes, shipped-manifest
  coverage, generic `content_search` / `content_read` / `web_fetch` handlers,
  and the versioned offline ladder corpus). `make test` runs
  `test-content-retrieval-eval-harness` as a named non-live suite.
- `make test-content-retrieval-runtime-live-eval` — the network lane, configured
  by `CONTENT_RETRIEVAL_RUNTIME_LIVE_{QUERY,DISCOVERY_ACTION,URL,TIMEOUT_SECS}`
  (an empty discovery action exercises automatic ordering;
  `CONTENT_RETRIEVAL_RUNTIME_LIVE_EVAL_ARGS=--only-discovery` focuses one
  adapter). It keeps synthetic credentials under a temporary root, never opens
  the operator's encrypted vault, refuses non-public and credential-bearing
  targets, never grants authenticated authority, and reports only bounded
  classifications, counts, labels, timings, costs and pass/fail codes (no URLs,
  queries, bodies, scope names, trace/receipt IDs or session fingerprints). It
  runs at the end of `make test live_evals=true`;
  `LIVE_EVAL_ONLY=content-retrieval-runtime make test-live-evals` isolates it;
  `--self-test` and `--dry-run` are network-free.
- `make test-observable-sources-eval-harness` (normal suite) and opt-in
  `make test-observable-sources-live-eval`, which reads only the two shipped
  public RSS feeds and the read-only source-offer API under strict caps and
  reports hosts, counts, content type, validator presence and latency only.

Design history: Web-Reader Adapters And Cost-Aware Reading.
Related plans: Content Retrieval Budget Scheduler And Within-Rung Adaptation,
User-Defined Feeds.
