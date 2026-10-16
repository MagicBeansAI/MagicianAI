# Content Sources And Reader Adapters

`magician_v2::content_sources` is the provider-neutral acquisition boundary for
feeds, recurring monitors, research and later consumers. It owns discovery and
read contracts and transport policy, not feed matching, monitor conditions,
scheduling, notifications or presentation.

## Contracts

- `ContentCandidate`: cheap pre-fetch record (stable source identity, bounded
  title and selection text, canonical URL, timestamps, privacy, content hash,
  provenance, metadata).
- `ContentDocument`: optional bounded full content tied to the exact candidate
  identity.
- `ReadSelectionEvidence`: mandatory for feed and recurring-monitor reads.
  Invocation source has no default, so an omitted source never reads as a
  privileged internal read; interactive user reads stay explicit intent.
- `DiscoveryAdapter` returns candidates; `ContentReader` returns documents.
- `ContentSourceRegistry` rejects duplicate adapters, unsupported capabilities,
  invalid or oversized output (provenance, URLs, hashes, names and timestamps
  included), foreign identities, disallowed remote query/content transmission,
  and cost that disagrees with the adapter's `metered` declaration.

The schema is versioned apart from feed and monitor storage. Consumers keep
source identity and provenance and never invent provider identifiers.

The Relentless native adapter can reuse `PublicHttpFetcher` under a runtime-only
finite request/byte/redirect/time policy. Its scoped compiled
`web_fetch`/`web_search` handlers bypass retrieval fallbacks and browsers; each
request reserves durable capacity before I/O, completed responses are immutable,
and unresolved dispatches are not resent. Fetched text gets candidate-bound
identity and raw-body provenance; search snippets stay discovery only. It is
attached only when a Relentless recipe declares `public_research`
(`HarnessActor::attach_research_boundary` → `ResearchBoundary::admit`); feed and
monitor consumers are unchanged. Public-address validation, per-hop DNS pinning, no credentials or
proxy, and bounded media/bytes stay the fetcher's job.

## Sources

### Manifest-driven capability sources

`ScopedDeterministicCapabilityInvoker` runs CLI-template skills for agents and
services with no LLM selection, keeping the scoped skill directory and `.env`,
sandbox, timeout, spend gate, progress, failure folding, active-owner identity
and learning telemetry. Each call carries a non-content source tag
(`agent_tool`, `user_feed`, `recurring_monitor`, `interactive_read`,
`observed_source`, `internal_system`) so attribution never logs private
queries. Agents use the same invoker, so feeds add no second provider path.

A skill opts into discovery by adding `metadata.magician.content_source` to its
`SKILL.md`; registration validates it against that file's typed actions and
registers a `CapabilityDiscoveryAdapter`. No provider-specific Rust is needed.
Output contracts:

- `mapped`: RFC 6901 JSON pointers map an existing JSON response (typed
  options, fixed arguments, metadata, cursors, provider error envelopes, decimal
  or microunit cost).
- `canonical_v1`: the script emits `{items, next_cursor, cost}` directly; for
  complex parsing. The manifest is not a transformation language.

Registration rules:

- Skill roots are taken in priority order; the first directory for a name wins
  (scoped over system), even when broken, so nothing fails open.
- The directory must match `capability.name`; adapter IDs are unique; the action
  must compile to a deterministic CLI primitive; every argument must be declared
  by it and every required parameter supplied; extension fields are deny-unknown.
  Legacy product sidecars fail validation.
- Extensions join the operation and rung they declare. `priority` orders within
  a rung (lower first, then action ID); explicit ladder action lists stay
  operator overrides and gates for system and browser actions. No Rust or config
  edit is needed to add one.
- File-level symlinks work; symlinked skill directories are not traversed.
- A broken `SKILL.md` is reported once in catalog readiness as
  `kind=skill_installation,error_class=broken_skill_installation` (not also as
  an unavailable adapter and reader).

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

Rust, not the script, attaches scope, privacy, provenance, observation time and
content hash. Metered manifests must return valid cost; cursors need both the
declaration and input mapping to enable them. Malformed rows are isolated.

### Existing comms-assist channels

`candidate_from_channel_message` projects the post-distillation
`MailMessageMeta`, covering every `ChannelAdapter` (Gmail, WhatsApp, Telegram,
AgentMail, iMessage). Only distilled summaries and briefs from rows in state
exactly `done` project (pending, failed, skipped and suppressed rows do not,
even with a stale summary); raw bodies never do; candidates stay `private`.
Feeds page the existing channel store; there is no resync.

### RSS, Atom, and JSON Feed

`RssDiscoveryAdapter` parses with `feed-rs`, strips tracking parameters, keeps
native identity in metadata and emits bounded public candidates over the public
HTTP transport below. Only the feed URL reaches the origin, never feed intent.
ETag/Last-Modified checkpoints: a `304` keeps the sent checkpoint, and cursor and
validators advance only after the consumer durably accepts selected candidates.

### TinyFish search and provider-direct fetch

`web-via-tinyfish/SKILL.md` declares free web search at priority 25, ahead of
Exa (100) and Tavily (200) in the automatic `public_search` rung; agents still
call `content_search`, and an unavailable TinyFish falls through the normal
ladder. Its direct `fetch` action (explicit requests, up to ten
public HTTP(S) URLs) is deliberately not a `content_reader`: Rust must own URL,
DNS, redirect, cache and provenance policy, so TinyFish Fetch is not
claim-eligible evidence; research uses `content_read`. Typed mappings lower to
inert `tinyfish search query` and `tinyfish fetch content get` argv on the
official `tinyfish` executable; `TINYFISH_API_KEY` is injected only at execution;
Skillshub `setup-deps` installs the pinned CLI under the shared Node 24 runtime
(`skillshub/.node-version`).

### Exa semantic discovery

`semantic-websearch-via-exa/SKILL.md` declares Exa for the generic adapter:
explicit remote-query permission, 100-result bound, typed options, passages to
`cheap_text`, USD cost. The `exa-search` executable owns transport; the scoped
credential is injected after authorization. Highlights only; the static reader
does any full-page read.

### Static public-page reading

`htmltotext/SKILL.md` declares the `static-http` reader. Rust owns acquisition
and policy; the Trafilatura/BeautifulSoup capability only extracts from a scoped
scratch file (never the URL, stdin bytes or the query).

- It recovers content tables with their unit captions (rules in the skill).
- It detects client-rendered pages (hydration payloads in `application/json`
  scripts, flight chunks or Astro `props`; an empty app root beside a bundle)
  and reports `/client_rendered` (manifest `shell_pointer` /
  `shell_reason_pointer`); the read fails as `javascript_shell` so the ladder
  escalates to the browser.
- The cache key's capability revision digests only `SKILL.md`: an extractor
  output change must bump the skill `version` (or stale extractions outlive the
  fix), and since the manifest is deny-unknown, a new output pointer must not
  reach `SKILL.md` before a binary that knows it.
- The scratch path is a product-brokered `workspace_path` authority that agents
  cannot forge; other typed paths stay under the scope's `workdirs/`.
- A governed extractor's bounded stderr failure envelope survives as a typed
  code and message.

The public HTTP transport (static reads and RSS) accepts only bounded HTTP(S)
URLs without credentials; rejects local or non-global addresses and DNS answers
(mixed included); disables proxies and pins validated DNS per request,
revalidating each redirect; rejects HTTPS→HTTP downgrades, loops, excess
redirects, error statuses, missing or unsupported static content types and
oversized bodies; carries `ETag` and `Last-Modified`.

Cache: `content_sources/cache` under the runtime root, scope names hashed into
principal/workspace trees. URL records are keyed by reader ID, capability
revision, read depth, scope and URL; text is stored once per BLAKE3 hash per
scope without sharing identity or provenance. Fresh `cached_ok` reads skip
network; stale ones revalidate and may fall back to the stale entry; `fresh`
reads fail rather than degrade. Concurrent reads of one URL coalesce.
Maintenance runs at most every 15 minutes per scope: drops URL records older
than 30 days, collects unreferenced bodies, repairs tampered ones on the next
write, removes abandoned scratch, and evicts oldest references to 512 MiB
without deleting a body another URL still references. A cancellation-safe
scratch guard unlinks the download when a read future drops. CLI capture is
capped at 64 MiB stdout and 8 MiB stderr; progress lines are bounded
separately.

The quality gate is depth-aware (`gist` versus `full_text`) and purely
structural. Bad output fails closed; JavaScript, login and interaction outcomes
are typed ladder inputs, never implicit browser launches.

### Unified scalar and vector research

`content_search` and `content_read` take a scalar `query`, `candidate` or `url`,
or a `requests` array (at most 8 search / 6 read items) for bounded fan-out with
per-request results in request order; one item runs serially. No separate batch
providers or schemas. Every branch runs the same core (scope, fallback order,
cache, receipts, authority, spend, telemetry). Parameter denies and approvals
inspect `common` and every request, so vectors cannot bypass a scalar
restriction. Use vectors for independent work; dependent chains stay
sequential.

- Requested concurrency only narrows
  `progressive_retrieval.max_parallel_actions`; ceilings are four search and
  five read branches, one read per host by default (ceiling two).
- Each page keeps its own sequential static → verified replay → public rendered
  → owner/auth ladder; no racing stages or shared browser sessions.
- Lightpanda's process-wide engine permit holds at the rendered reader: other
  branches' cache/static/provider work overlaps, Lightpanda sessions do not.
- Vector reads default to `gist`; the scalar reader keeps its own default.

Evidence projection:

- Search records are `evidence_role: discovery_only`, `claim_eligible: false`;
  a returned `url` goes straight to `content_read` (no `read_result` needed).
- A receipt-verified snippet may satisfy a gist read without I/O but stays
  `fetch_status: not_fetched` and not claim-eligible.
- Reads carry `requested_url`, `final_url`, `redirected`, title, excerpt and a
  neutral `fetch_status` (`complete` means fetched, not relevant).
- Failed or irrelevant attempts are reported only when they leave a material
  evidence gap; the shared output synthesizer applies the same rule.

Precision review before agentic publication sees only claim-eligible opened
pages, never snippets or failed fetches. An unsupported draft gets a bounded
repair observation that prescribes no source, query or tool. Each draft backed
by new evidence spends one of three strikes; a resubmit without new evidence
ends the repair. Exhausted repair fails closed only when nothing was completed;
otherwise completed work ships as a partial with the verdict as caveat.

Deadlines and results:

- One shared deadline. `minimum_successes` defaults to all branches (lower it
  only for interchangeable evidence); once met, unfinished siblings are
  cancelled via a child token, with a post-permit check closing the
  queued-branch race (parent cancellation still propagates).
- Per-branch envelopes stay lossless under `results`; a bounded `evidence` lane
  carries top records (round-robin by rank) or an excerpt. Telemetry reports
  wall time, summed time, overlap saved and costs.
- Budgets in `common` apply per branch, not as a shared pool. Failed or
  cancelled envelopes never count as success or enter `evidence` (raw envelopes
  stay inspectable); degraded and handoff envelopes are partial.
- Compiled-pack results are normalized from `ActionResult::Text` to canonical
  JSON, so `/evidence` and `/candidates` resolve.

Shared fields go in `common`; top-level `fresh`, `limit`, `depth` and
`timeout_secs` fold into it. Branch fields (`query`, `candidate`, `url`,
receipts, grants) beside `requests` and conflicting values are rejected; policy
sees effective values.

Sleuth defaults to one authoritative gist page per entity, adding sources only
for a freshness, ambiguity or contradiction gap. Repeated `agentic_decision` is
pinned to Luna medium-reasoning/tool. No full-text quota or forced report
shape; corroboration follows stakes, not a fixed count. Explicit deep research
may expand.

### Rendered, authenticated, and replay reads

Browser and API-replay rungs run without a nested browser agent.

**Public read.** `browser.headless.read` opens one controller-owned session,
waits for network idle and a bounded render, extracts title, final URL and body,
and always closes. Generic browser-mode env overrides are ignored, so it cannot
become CDP. `content_acquisition.browser.public_read_engine` (shipped
`lightpanda`) is a soft preference for this public read-only path only; on
failure it falls to `engine` (shipped `cloak-browser`) then bundled Chrome for
Testing, skipping duplicate or absent stages, never for authenticated or
side-effecting work. No preference uses `engine`; neither uses agent-browser's
bundled browser. Metadata records the final engine, fallback marker, original
preference and attempt sequence. Robots, rate and concurrency policy apply.

**Engine choice.** Callers may set `engine` on the first call: `lightpanda` for
public profile-free headless DOM, text and extraction; full fidelity for pixels,
headed, profiles, logins, files or anti-bot. Screenshot, PDF and recording are
rejected on Lightpanda. Meeting and preview paths use `engine`. A CloakBrowser
concurrent-session denial retries once with `bundled_chrome`, which
observability records. Lightpanda is headless only; ordinary sessions may
request `bundled_chrome` when the configured engine is unavailable.

**Handoffs and grants.** `content_search`/`content_read` return expiring
handoffs for headless navigation, headed owner help or authenticated CDP; headed
is not logged in. Identity-bearing reads and interactions need separate grants
from `authorize_content_read`, bound to principal, workspace, domain, action,
mode and expiry. "Approve Once" cannot be replayed. A grant must permit sending
private observations to the task model; read never authorizes interaction. CDP
uses a dedicated Magicutor proxy session and cleanup leaves the user's browser
open.

**API replay.** `api_replay.read` runs before rendering for a validated
read-only API Mining capability. It rejects private candidates,
credential-bearing headers or fields, cross-domain targets, proxies, non-public
DNS and redirects, and pins DNS. A bare anonymous `401`/`403` stays on the
public ladder; a visible login or paywall yields typed authentication
remediation; other unavailable outcomes fall through once.

Receipts hold mode, transport, engine, hashed session and capability identity,
latencies, approval and cleanup outcome, never cookies, headers, queries, bodies
or credentials. Live gate: `make test-content-retrieval-runtime-live-eval`
([Evaluation](#evaluation)); per-skill live runs belong to the tool-runtime
canary lane.

## Observable Sources On Observe

### Uniform restart catch-up

Automatic Observe sources share the scope-owned `observe_catch_up/config.json`
policy (lookback 1/5/7/14/30 days, per-source cap 10–200, total cap 50–1,000,
duration 5–60 minutes; `observe_catchup::CATCH_UP_*_OPTIONS`) and one process-boot admission ledger above provider cursors: it narrows
history floor, per-source and aggregate counts and admission time. Manual runs
are outside it.

Scheduled Notes and public-feed subscriptions reserve before acquisition; rows
are filtered by `published_at_ms` (or observation time) before selection and
cursor commit, and a source that finished its catch-up stays at the post-boot
floor for the boot. Notes restarts at its newest page. Errors return
reservations.

Replay capability: Notes is checkpointed; RSS is current-snapshot-only;
Calendar uses the same choices via its task prompt and the Task runtime and does
not replay missed cron runs. The duration stops new admissions only; a source
holding a durable lease finishes on its bounded provider path.

### Sources and eligibility

A strict `metadata.magician.observe_source` block declares a source identity and
named profiles. Scoped and system roots load with normal shadowing; invalid
definitions are isolated; only opted-in profiles are shown.

Eligibility is server-owned: the default permits exactly `rss.discover` at
`public_remote_read`, denies metered and browser actions, and needs an
unattended read-only profile with one action and a public target, so RSS never
falls back to Exa, static reading, a browser, CDP, an agent or an LLM. Product
Hunt and arXiv ship as RSS offers; users can add a custom RSS/Atom URL.

Notes is a private built-in: `notes.discover` reads Markdown under the scoped
Local Markdown root and any configured SilverBullet Space, with no network,
browser or LLM. Notes settings gate it (disabled shows **Needs setup**; reads
fail closed). Root configuration is revisioned; editing a note changes only
that note's revision, so writes do not stale the subscription.

### Subscriptions, runs and handoff

Subscriptions live per scope in `observe_sources/subscriptions.json` (revisions,
action binding, cadence, intent, bounds, cursor, validators, dedup history,
health, lease, optimistic revision). Notes uses a `notes:<offset>` checkpoint,
then returns to the newest page; revision-aware fingerprints skip unchanged
Markdown. Scope or identity inconsistencies and unknown fields fail closed,
except the pre-release `lease_id`, read as `lease_owner` (rewritten canonically
on the next write). Any source/profile/action/target revision change clears
checkpoints before the next run. List APIs paginate by
cursor; a stale cursor returns a conflict.

`observe_sources/observability.json` keeps per-source aggregates and the last
4,096 run receipts: identifiers, status, counts, timing, cost, cursor movement,
response bytes and error classes, never URLs, intent, text, credentials or raw
errors (aggregate names such as `modified_targets` or `candidates_discovered`
are not sensitive). Deleting a subscription removes its records.

The scheduler and **Check now** share one semaphore and durable per-run leases.
A run reloads the source, runs its pinned path, deduplicates, applies the
optional keyword-plus-embedding filter and issues an `ObserveMatch` receipt.
Pause, deletion, revision change or lease expiry blocks handoff and cursor
advance; failures release the lease and back off exponentially.

Selected candidates are admitted atomically as `ObservedContentEnvelope` records
under `observe_sources/enrichment_ingress/`. A consumer turns them into typed
`web` or `note` resurfacing candidates (note bodies stay local) and removes the
file after the SQLite write; transient failures stay pending, invalid envelopes
go to `observe_sources/enrichment_failed/`. Successful batches wake Worth a look
curation. Feed, memory and alerts are separate consumers.

`/observe` Sources has **Listening**, **Available** and **Needs setup** tabs,
cadence and pause/resume/stop/check controls, Notes and custom RSS setup.
`/observe/stats` shows health, flow, conditional fetches, latency, handoff
counts and run history. Accepted items can show in **Today → Worth a look**.

- `GET|PUT /api/magician/v2/observe/catch-up`
- `GET /api/magician/v2/observe/sources`
- `GET /api/magician/v2/observe/subscriptions`
- `PUT|DELETE /api/magician/v2/observe/subscriptions/{id}`
- `POST /api/magician/v2/observe/subscriptions/{id}/run`
- `GET /api/magician/v2/observe/sources/observability`
- `GET /api/magician/v2/observe/subscriptions/{id}/runs`

`content_acquisition.observe` sets interval, concurrency, lease TTL and catalog
policy. Manifests reload at resolution; process-level settings need a config
reload or restart. Process-lifetime metrics sit beside the durable ledger;
neither holds queries, item text, credentials or raw provider errors.

## Runtime Handoff

`ContentAcquisitionResolver` resolves one `ContentAcquisitionService` per exact
principal/workspace and capability revision, sharing the scoped executable
registry, spend authority and runtime paths with Chat and autonomous execution.
Scope values that normalization would rewrite are rejected. Services are cached
single-flight in a bounded cache keyed by a revision over the full `SKILL.md`
contract; skill mutations evict them, out-of-band edits are caught on the next
resolution, a mid-build revision change retries (bounded), invalidation is
serialized with in-flight builds, the per-scope build-lock map is bounded too,
and foreign-scope requests fail before any adapter runs.

`catalog()` returns descriptors plus credential-free unavailable-source records;
one bad manifest or filesystem failure is isolated to its skill, and shadowing
stays fail-closed. `metrics()` reports per-adapter calls,
outcomes, latency, items, metered microunits, denials and cache outcomes, never
queries, bodies, env values or credentials. Readiness accepts only types the
deterministic CLI invoker runs. The resolver is in Actix app data and the
orchestrator; `SkillsApi` uses it for invalidation.

## Progressive Retrieval Ladders

`RetrievalLadderController` takes a versioned `RetrievalNeed` (`discover` or
`read`, evidence goal, scope, freshness, remote-data policy, maximum authority,
deadline, attempt bound, optional action allow-list, per-commodity budget) and
walks the rungs in `content_acquisition.progressive_retrieval` without an LLM.

- Each descriptor projects one action (operation, rung, outputs, authority,
  options, targets, parallel safety, cost). Unknown or duplicate required
  actions, mismatches and unsafe parallel config fail closed. Browser, session
  and replay actions are admitted separately, so a static failure cannot gain
  authority. Source-specific options are stripped before other actions run;
  optional unavailable actions stay visible as readiness data. Source-native
  actions run only for an explicit selection (RSS also for an explicit feed
  target).
- Discovery merges, deduplicates and checks relevance and source coverage until
  the goal passes. `eligible_parallel` rungs obey concurrency and attempt caps;
  sequential rungs keep exact action order.
- Gist reads reuse inline text only when the whole candidate matches its
  receipt; otherwise the cache-first static reader runs. Direct URL calls take
  no caller-supplied inline text. Thin, error, login, paywall, shell,
  missing-metadata and low-information pages are rejected.
- Read goals: `gist`, `full_text`, `structured` (must parse as a non-empty JSON
  object or array). Readers declare media types, so PDFs skip HTML extraction
  and the JSON-LD reader skips article reads. Each reader's lower bound applies
  under a hard 32 MiB ceiling. Spend is admitted per action or parallel wave, so
  an uncharged failure does not eat the next fallback's budget.
- Selection receipts are opaque, single-use and expiring, bound to scope,
  capability revision, the full candidate and invocation reason. A bad receipt
  disables inline reuse for interactive URLs; feed and monitor reads fail closed.
- Per-action failure circuits. Results carry a trace ID, action order,
  classification, quality findings, latency, cost, status and unmet reasons; no
  queries, bodies, credentials or raw debug payloads are logged.

### Failure classification and diagnosability

| Class | Meaning and effect |
|---|---|
| `ProviderUnavailable` | Unrecognized error or per-provider timeout. Advances the ladder and counts toward the circuit. |
| `ProviderMisconfigured` | TLS trust failure, missing or rejected API key, malformed capability JSON. Opens that circuit on first observation and falls through; **not** ladder-terminal. |
| `DeadlineExceeded` | Outer request deadline ran out. Terminal; the attempt bound cannot extend it. |

`content_acquisition.progressive_retrieval.discovery_attempt_timeout_ms`
(default `15000`) bounds each discovery provider inside the request deadline
(`default_deadline_ms`, default `20000`).
TLS classification needs a failure token (`verify failed`, `handshake`,
`issuer`, `expired`) plus a topic token (`ssl`, `tls`, `certificate`). Provider
key problems never become `AuthenticationRequired` or `InvalidRequest`.
`failure_detail` comes from the root cause, not the joined chain; governed CLIs
emit `{"error": {"kind", "message"}}`; messages are redacted before truncation.

Trusted agents research through `content_search` and `content_read`; Exa,
Tavily, DuckDuckGo, source-native adapters, RSS and `htmltotext` stay behind the
controller. Direct provider tools are for explicit requests or diagnosis (Sleuth
keeps source-envelope tools for situation awareness). Legacy `web_fetch`
delegates to the controller and fails closed without it (no direct-network
bypass). The web-researcher denies `websearch-via-claude` and
`deep-research-with-claude`; the OpenAI deep-research pack is only for an
explicit exhaustive-report request.

## Consumer Rules

1. Feed and monitor definitions select adapter IDs; adapters do not inspect
   product definitions.
2. Matching runs over `cheap_text` and metadata before any full read; feed and
   monitor reads carry selection evidence.
3. A feed may mix channel and external candidates.
4. Private content stays local unless a later policy authorizes a derived
   artifact. Raw channel bodies are never in this contract.
5. Metered adapters register only after explicit runtime config. A missing
   provider fails; it never selects a paid fallback.

## Evaluation

- `make test-content-reader-eval`: offline extraction fixtures and the depth
  quality evaluator.
- `make test-content-retrieval-eval`: catalog, fallback, receipts, quality,
  shipped manifests, `content_search` / `content_read` / `web_fetch` handlers,
  offline ladder corpus. `make test` runs `test-content-retrieval-eval-harness`.
- `make test-content-retrieval-runtime-live-eval`: network lane over the
  production cores, set by
  `CONTENT_RETRIEVAL_RUNTIME_LIVE_{QUERY,DISCOVERY_ACTION,URL,TIMEOUT_SECS}`
  (empty discovery action tests automatic ordering;
  `CONTENT_RETRIEVAL_RUNTIME_LIVE_EVAL_ARGS=--only-discovery` for one
  adapter). Synthetic credentials under a temp root (never the operator's
  vault), public targets only, no authenticated authority, and bounded output
  (no URLs, queries, bodies, scope names, trace/receipt IDs or session
  fingerprints). Runs at the end of `make test live_evals=true`;
  `LIVE_EVAL_ONLY=content-retrieval-runtime make test-live-evals` isolates it;
  `--self-test` and `--dry-run` need no network.
- `make test-observable-sources-eval-harness` (normal suite) and opt-in
  `make test-observable-sources-live-eval` (two shipped RSS feeds and the
  source-offer API, strict caps).

Design history: Web-Reader Adapters And Cost-Aware Reading.
Related plans: Content Retrieval Budget Scheduler And Within-Rung Adaptation,
User-Defined Feeds.
