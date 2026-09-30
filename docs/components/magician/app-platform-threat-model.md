# App Platform Threat Model And Policy Boundary

The implementation lives under `magician/src/magician_v2/apps/`, including the
request/approval/policy boundaries, registry/entity/surface owners,
`workflows.rs`, `processing_boundary.rs`, `tool_disclosure.rs`,
`resource_authority.rs` and `resource_contract.rs`; the red-case matrix is
`magician-apps/src/apps/threat_model.rs`. This page
freezes the security vocabulary and separates pure policy from the load-bearing
request, store, execution, physical-disclosure and recovery adapters that
enforce it.

## Mechanical meanings

- **Server-minted**: a Rust value with private fields and no transport
  deserializer, created only from a named trusted resolver/registry seam — never
  a JSON field a client, package or model can assert.
- **Authenticated scope**: the server bound an actor and session to a
  principal/workspace and minted `AuthenticatedAppScope`. A body, query,
  forwarded header or model result is not authentication.
- **Local processing**: a current server-owned endpoint attestation says the
  endpoint is explicitly local-processing eligible. A provider/model name,
  loopback-looking string or self-hosted claim is not evidence.
- **Publisher**: a package-trust resolver verified an immutable revision and
  publisher evidence. Manifest prose is not publisher identity.
- **User-owned**: held under an authenticated scoped installation and subject to
  lifecycle, export, purge and retention contracts; bytes already accepted by a
  third party are not thereby deletable.

## Trust zones

Trusted server code owns scope authentication, current installation/grant/schema
resolution, endpoint attestations, hidden-consumer capabilities, policy joins
and final consequential decisions. Packages, workflows, model output, external
content, browser/network clients and custom-surface documents are untrusted.
Standalone skills, the scoped app store, Storage Governance, the archive writer,
resource authority, consequential dispatchers and retained provider state are
separate boundaries; trust in one never transfers authority to another.

Threat inventory:

- malicious or mistaken package/workflow authors and imported bundles;
- compromised or mutable standalone skill dependencies;
- prompt injection in records or external adapter results;
- invented tools/actions or incorrect model output;
- forged local/network clients and cross-scope requests;
- compromised providers and misclassified self-hosted endpoints;
- same-user processes calling loopback APIs or reading files;
- crash/replay/recovery races across stores and projections;
- malicious custom-surface JavaScript and resource/message floods.

`APP_PHASE0_RED_CASES` maps every actor to a stable case ID, enforcement zone,
expected result and status (`ContractSpecified`: pure kernel test;
`DormantBoundarySpecified`: non-deserializable fence + boundary test, no live
adopter; `LoadBearingAdapterPending`: visibly open). A closure invariant rejects
projection-only or pending status for required request/dispatch/store/approval
rows. The experience-class rows cover `overlay-draw` / `narration` admission and
the stop row `app-voice-invocation-phrases-stay-unadmitted` (see
[app-interactive-capabilities.md](app-interactive-capabilities.md)).

## Authenticated realtime-voice owner carrier

Governed app discovery/invocation and `local_only` app-memory rendering run on
realtime voice only for a directly authenticated personal-assistant call on an
identified owner surface. Media registration mints a process-only,
move-on-connect credential from the verified actor/session/scope, keyed by a
separate server-random correlation; neither has a wire form, and a native caller
can supply only a profile preference.

The credential binds the exact voice session, chat session and turn plus the
server-selected profile, provider adapter, model, base URL, backend-proxied
topology, transport cohort and `no_provider_storage` posture. Live processing
trust is reopened before provider setup, after async prompt/tool work, and at
final delivery; a new turn cancels prior turn fences. Close, expiry, rotation,
reconnect, route substitution, trust change or cross-scope/session/turn replay
withholds protected bytes. No profile fallback, no voice history reuse. P2P and
hands-free voice, delegated/autonomous/feature workflows, meetings,
public/extension surfaces and unknown owners fail closed. Eligible profiles are
explicitly trusted `openai_realtime_backend` or `gemini_live` Assistant profiles
with no fallback and no-provider-storage; a local declaration also needs an
explicit loopback base URL.

## Accepted app-memory hybrid-index boundary

App/entity writes cannot index proposal text. Only the canonical memory
destination's accepted projection produces RemoteAllowed index documents,
carrying the exact proposal/source head, destination generation/receipt,
installation/package/grant/schema/port identity, expiry, handling and
provider-policy partition. Its content-free FullScope journal is written after
projection commit and replayed idempotently; a lost signal is repaired before
prompt scoring.

LocalOnly text is never loaded by the background indexer. A direct-owner turn may
rank it through an ephemeral uncached pass only while the runtime credential
authorizes the exact loopback embedding endpoint/model/contract, reopening
attestation, no-provider-storage, trust revision and cohort before and after
provider I/O; any substitution or remote fallback denies the whole LocalOnly
pass.

Source drift, candidate reject/revoke/expiry, record forget, installation
update/disable/quarantine/uninstall-retain, grant revoke and purge replace the
projection with a sealed empty tombstone and dirty the index. Prompt handoff
revalidates source and provider authority independently, so stale vector/BM25
bytes cannot disclose or resurrect content. Accepted app text stays a
non-authoritative hypothesis; apps cannot assign rank, confidence or heat.

## Request, dispatch, store and approval fences

- The procedure loader reads bounded routing metadata before deserialization;
  `skill_type: app` enters the strict app parser for validation only, and
  malformed apps cannot fall back to procedures.
- App workflow prompts are package-private and non-serializable. Procedure
  invocation accepts no mutable scoped skill name: a standalone procedure arrives
  as registry-minted bytes matching the lock, or a vendored one is rejoined to
  the package and its subtree digest. Stale package authority, missing/extra/
  duplicate revisions and denied tools are rejected; the skill's `allowed-tools`
  is intersected with the workflow's slice of the resolved grant/agent/trust/
  parent authority. Dependency-lock changes are part of package-revision
  identity; in-place dependency substitution is rejected.
- `VerifiedAppTransportSession` treats caller scope only as an equality
  assertion against the authenticated session; loopback fallback needs the real
  socket peer plus an explicit single-user deployment.
- A complete current authority resolution mints move-only evidence at the
  consequential boundary. Dispatch and store fences recheck the projected
  identity; invented tools, undeclared context, stale/revoked/delegated
  authority, cross-installation queries and stale-schema mutations fail closed.
  Store fences also bind the digest of the exact validated query or mutation.
- Reviewed lifecycle transitions use a separate move-only approval fence binding
  approval ID/revision, attempt and kind, package, authority/data/resource/
  schema/migration decisions, global policy and the live
  scope/actor/session/auth kind/revision. The durable approval is compared and
  consumed atomically with installation publication.

## Labeled-content join

Every `AppDataEnvelope` recomputes its content digest during contract
validation; its serialized labels are untrusted claims. A trusted resolver mints
non-deserializable resolved labels, and `RevalidatedAppEnvelope` accepts the
pair only when wire and trusted labels match and the digest is valid (borrowing
the source and its metrics, so `join_app_content` does not re-traverse).
Consequential model/tool and hidden-consumer decisions require a
non-deserializable effective policy from `ResolvedAppAuthority`; a deserialized
`AppDataHandlingPolicy` is not type-compatible.

The production adapter pins one provider endpoint/model cohort and
no-provider-storage. Protected requests cannot use provider continuation IDs,
prompt caches, streaming, fallback, internal retry or redirects; custom provider
paths fail closed. The last authority check follows queue and resource waits;
resource expiry is sampled at physical I/O start; provider usage is checked
before narrowing into accounting.

Generic retrieval/memory, task state, reviewer/refinement, local preparation,
capture/eval, reflection/synthesis and learning sinks are not V1 labeled-content
consumers and receive no app payload (constant or digest/count metadata only).
Protected pause and tool continuation bytes need current sidecar membership,
handling digests and non-deserializable retention authority to re-enter a
prompt.

`join_app_content` takes one or more revalidated views; empty and cross-scope
input fail closed; input count, aggregate bytes/nodes, derived JSON and the
provenance set are capped; hashing streams into BLAKE3; over-deep derived values
are dropped iteratively. The result is deterministic and order-independent:

- classification = most restrictive source/destination floor;
- model processing = narrowest source/destination permission;
- installation, package, grant, schema origins and source revisions are in
  provenance identity;
- purpose, audience and all policy digests are in policy identity;
- the derived value gets its own content digest.

`AppJoinedContent` is serializable for audit but not deserializable as evidence,
so nothing downstream can lower these axes.

## Endpoint and egress decision

`AttestedAppEndpoint` distinguishes loopback/managed-host, trusted
self-hosted/LAN and external classes; local eligibility is an explicit
server-owned property that external endpoints cannot claim. Attestations expire
and bind a trust revision and configuration digest. The API accepts no
provider-name input.

Final authorization distinguishes three destinations:

- deterministic processing is compatible with `model_processing: none`;
- model processing obeys the narrowest content and policy setting; `local_only`
  requires an eligible current endpoint;
- external-tool disclosure independently requires the exact approved
  destination; remote-model permission never implies email/upload/tool egress.

The tool dispatcher mints its move-only disclosure permit only after every
queue, engagement and endpoint-trust wait, then compares exact outbound bytes and
attestation at the I/O timestamp. Permits are not queue authority: delayed work
re-resolves authority and mints a fresh permit.

Prompt memory uses the same rule: `local_only` app candidates need a
request-bound direct-owner credential for one exact physical profile, revalidated
just before model I/O; remote substitution, profile changes, expiry, turn replay
and session crossing fail closed, and provider-bound candidates are excluded from
the delayed utility-review consumer. Chat and the realtime lane above are the
only enabled consumers.

## Hidden consumers

Local preparation, embeddings, prompt assembly, prompt/debug dumps, raw trace
capture, eval artifacts, reflection, synthesis, analytics and crash diagnostics
are enumerated; a trusted registry supplies each one's label support, maximum
classification, retention capability and revision. The decision is full
content, metadata only, or denied. Unlabeled input is denied; label-blind
consumers get metadata only; analytics and crash diagnostics are always
metadata-only; debug/trace/eval payload retention needs an explicit protected
labeled-retention capability; model-backed consumers must also authorize the
attested processing target.

## Hostile package boundary

`manifest.rs` never sends app frontmatter through ordinary skill inference. It
applies byte, syntax, indentation and depth ceilings to a restricted V1 YAML
subset, then an iterative decoded-value walk for node/depth/scalar ceilings,
before deserializing types that deny unknown fields at every level. Unsupported
Board views, underspecified or type-incompatible views, aliases/anchors/tags/
merge keys, block scalars, nested documents, duplicate normalized IDs/routes,
unsafe or host-global routes and incoherent references fail before a candidate
exists. Same-line block sequence/mapping markers count toward lexical depth.

Complete-bundle admission rejects path traversal, absolute/platform-specific
paths, compatibility-character separator/colon escapes, non-portable non-ASCII
paths, case collisions, links, special files, oversized members and missing
prompt/asset/vendor members. Production ceilings cannot be widened by an import
caller; staging caps reads before allocation. Canonical identity covers the
manifest and every normalized member path and byte. `package_lock.rs` accepts
only non-deserializable registry evidence minted from an immutable positive
revision and exact bytes — a mutable skill name or supplied digest is
insufficient. Vendored procedure identity derives from the bundle. Candidate and
lock evidence carry no grants. Every capability a workflow names needs an exact
version requirement; no implicit wildcard. This module does no filesystem
traversal (and so makes no TOCTOU claim); the stager must read no-follow, hold
race-safe file evidence and publish atomically.

## App-memory source boundary

`memory.rs` makes an app record a governed memory source, never memory by
default. Candidates consume the server-produced labeled join and a
non-deserializable current-record resolution sharing the decision timestamp and
authenticated actor/session/auth revision with its authority (cached resolutions
are rejected at proposal and retrieval). Serialized identities on the candidate
are never current-source authority.

Only an accepted candidate with a freshly resolved `AppMemoryEligibilityFence`
proceeds to retrieval; missing evidence, identity or policy drift, revoked
promotion or denied model processing fail closed. Disabled, quarantined,
updating and retained installations are dormant. Deleted/purged sole-source
candidates need a tombstone; multi-source candidates go stale and must be
re-derived. The reducer never resurrects stale or tombstoned candidates. The
fence is non-deserializable and non-cloneable; the canonical memory-store
adapter must compare source eligibility at retrieval and prompt assembly.

## Portability, publisher and import boundary

Package software and personal data use disjoint logical manifests: the package
shape holds only immutable bundle/lock identity and advisory verification
evidence; the data shape uses archive-local ordinal aliases instead of source
IDs. Data and combined archives default to versioned authenticated encryption
with a KDF. Plaintext data needs a non-deserializable warned approval bound to
scope, actor, session, auth kind/revision, logical digest and a validity window
within the session; secret data is denied outright. The archive writer rechecks
that authority. Payloads are bounded before logical hashing.

Publisher lineage is separate from byte identity: a deserializable signature
claim has no trust; only non-deserializable verifier evidence bound to the
package digest and current trust-registry revision preserves lineage. Unsigned or
forked packages get a new local identity; collisions fail closed. Imported
verification evidence is advisory, no foreign grant transfers, and local digest
verification, conformance, permission review and executable
rebuild/verification/sandbox qualification stay mandatory.

The import preview is server-produced and non-deserializable, binding source
archive/package/schema, destination package/schema, every record decision,
missing attachments and an idempotent batch key. Destination IDs are new local
IDs; merges need expected revision and reviewed rule. Apply authority binds
scope/session/auth revision, preview, archive and current destination
installation generation; replay evidence is observed at that same boundary.

## Update, migration and rollback boundary

An update/reinstall candidate binds one parked installation generation, attempt
kind, package/lock, grant/schema/surface revisions and source-record high-water
before review. The coordinator persists exact compiled migration operation
bodies and digest — never publisher SQL or prose-derived approval. Code-only
changes carry no record plan. Add/default, rename/map, widening and explicitly
destructive drop are reviewable; unrepresentable transforms fail closed.

Dry-run and staging never change active reads. Writes advance the source journal
once, staged catch-up is bounded, and final quiesce/catch-up precedes the short
lifecycle CAS/outbox commit. Destructive plans need the exact encrypted
portability backup receipt tied to source fence and plan digest. Any stale
source, grant, review, plan, backup, replay identity or generation denies.

Abort before switch removes staged state. After switch, code-only rollback keeps
current grants and records; data rewind needs a fresh backup-bound preview and
owner confirmation, mints local IDs, reports conflicts, tombstones post-update
live heads, keeps prior tombstones, and never restores old permissions or
resurrects memory/retrieval/index projections.

## Retention and purge boundary

Retention has explicit time, byte and revision ceilings plus scope-keyed at-rest
encryption from personal data up. Purge uses an exact 17-class Storage
Governance inventory and a server-produced scope-bound preview/approval; the
receipt distinguishes deletion, cryptographic erasure, shared/policy retention,
provider-unknown history and failure. Missing targets, partial cleanup or
retained canonical rows block `purged`. The generic lifecycle reducer always
rejects `Purge`; only the retention owner applies the terminal generation after
matching a complete receipt, and exact receipt replay stays idempotent.
Provider-side history is reported unknown or policy-retained, never "deleted"
because a continuation ID vanished. The whole-installation coordinator
implements the preview, acknowledgement fence, atomic local deletion and
terminal receipt/CAS; encrypted external chunk writers and non-registry stores
are separate load-bearing adapters.

**Registry encryption.** SQLCipher 4, keyed before schema access from a
dedicated OS-keychain root and a domain-separated authenticated-scope key.
Database, index, WAL and temp pages share one encrypted posture; SQLite temp
state is memory-only. The registry records format, algorithm and non-secret key
ID. A plaintext v1–v19 registry is verified, checkpointed, exported to an
encrypted temp database, fsynced and atomically replaced before schema v20
commits. Wrong, missing, cross-scope or corrupt keys fail before any row
decodes.

Root rotation keeps historical keychain generations so an idle scope can
authenticate once and rekey on its next writable open; a concurrent schema
upgrade validates the key that opened the store, commits, then rekeys. Rotation
is only via localhost `POST /api/magician/v2/secrets/app-data-root-key/rotate`
(setup token plus verified scoped identity), refuses past the bounded
historical-generation ceiling until a retirement audit, and rekeys the scope
before returning. There are no per-installation DEKs: purge may report secure
row deletion and shared WAL/key retention but never cryptographic erasure (the
contract rejects that claim). Indexes are ordinary encrypted pages, not blind
indexes.

## Interactive capability review boundary

Browser, macOS and Android availability is not app authority. An app declares a
complete logical `InteractiveCapabilityRequest`; package-lock V5 binds it to one
exact Observe/snapshot primitive source, descriptor, action, schemas, effects,
implementation plan and result ceiling. Review persists the requested matrix and
the owner's explicit narrowing in grant and approval; the grant authority digest
includes the selection. Launch and the final pre-I/O edge reopen installation
and grant truth and revalidate the primitive against lock and grant; drift in
origin, target/profile selector, action class, background/capture/transfer
posture, resources, expiry, source, schema, effects, implementation or result
ceiling aborts before disclosure. Opaque session records bind the reviewed
digests; raw device, host, CDP, window/tab, control-token and transport IDs never
enter review or client types. Legacy exact single-snapshot dependencies may get
the bounded Observe projection; other legacy interactive breadth is inert until
re-review.

## Continuation boundary

`AppContinuationPartition` binds scope, installation, package/grant/schema
revisions, endpoint reference/trust revision/config digest, model,
disclosure-policy digest, effective authority digest and handling digest.
Comparison is exact equality, not bearer authority; the adapter still resolves
current authority and a live attestation. Any transition yields a new partition
instead of replaying protected history.

## Canonical resource-authority boundary

`resource_contract.rs` records why specialized meters cannot authorize apps
alone: two child/retry meters can each sit under a local ceiling while the root
exceeds it, and LLM/task, token, VibeDev active-time, tool, browser and storage
observations have different identities and settlement windows. They are only
named observation sources.

- The flat journal binds scope, installation generation, active revisions, root
  execution, ledger and period but is only a claim. Replay needs a fresh
  non-deserializable resource fence from current app authority and a refreshed
  snapshot excluding the evaluated root. Exact replay is idempotent; rebinding
  fails closed.
- Every retry, resume, repair, child, tool/browser call, synthesis and
  reflection uses the root ceiling. Additive units use checked sums; parallel
  active time uses interval union.
- An observation above its persisted upper bound is rejected unexposed and held
  for conservative recovery; an expired uncertain effect stays reserved until
  reconciliation proves use or non-dispatch. `proven_unspent` releases only when
  the crash reconciler is the sole source and separate evidence binds the exact
  reservation; ordinary meters cannot self-declare unspent.
- Replay never rejects a historically admitted root because of today's
  capacity; it reports current period breaches. New roots/reservations are
  evaluated against a refreshed non-regressing period revision, compare-and-
  appended, and yield a move-only dispatch permit only after commit. A
  process-shared root registration prevents two live leases per execution.
  Cross-month resume reuses the root's original period/deadline.
- The reducer freezes no-progress, installation concurrency, background
  frequency, package size, monthly token/cost and scheduler foreground-reserve
  semantics; `resource_authority.rs` persists them as one tree/period authority
  (durable elapsed-time origin, digest-bound crash evidence, closed-period
  settlement, retirement tombstones), adopted by the shared workflow/executor
  owner for LLM and tool attempts, terminal store operations, progress, cleanup
  and maintenance. Usage pages are rebuildable projections, never fallback
  enforcement. Work-expanding events test prior no-progress state during the
  single replay (a heartbeat cannot clear an incurred breach); held-reservation
  liveness uses a counted expiry index.
- Outstanding operation deadlines are interval-unioned into admission before a
  provider permit; committed intervals must stay inside the reservation. Caller
  timestamps, activity feeds or analytics cannot create active time or release
  an uncertain effect.
- Protected pauses are not authorized by their file seal alone: the registry
  keeps a monotonic current/prepared/claimed/consumed generation; claims are
  short-lived and heartbeated; resume needs the exact authenticated scope and a
  canonically paused execution; terminal/reask/rejection paths CAS the same
  generation. The scheduler lease is released only after the pause is durable;
  crash recovery reconciles reservations before fresh dispatch.

See the [app-platform contract kernel](app-platform-contract-kernel.md) and the
archived app-platform design.
Unattended execution: [app background behaviors threat model](app-background-behaviors-threat-model.md).
Canonical events and owner attention:
[app events and owner notifications threat model](app-events-owner-notifications-threat-model.md).
