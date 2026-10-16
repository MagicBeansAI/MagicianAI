# App default surfaces

An admitted declarative app view compiles into the MUIJ render contract. The
empty generation is persisted atomically, hydrated under authenticated scope as
an indexed entity page, rendered on Unified UI routes, edited through the entity
store and kept fresh with durable deltas. The Apps directory, launchers, Today
pins and the iOS WebView use the same owners.

## Compilation and persistence

`apps/surface_compiler.rs` accepts only a `VerifiedAppSurfaceSource`, minted by
rejoining staged package bytes, the immutable registry package record and the
compiled schema revision, so a caller cannot supply another package or schema
digest. It returns one `CompiledAppSurface` per view: the durable
`AppSurfaceBinding` (collision-free `/apps/<installation>/<local-route>` route,
canonical compiled-view digest) and an `AppSurfaceEnvelope` (installation ID,
surface revision, view ID, validated `MuijDocument`, `interaction_mode: app`).

Activation uses `CompiledAppSurfaceSet`: every route under one active
`surface_revision` plus a stable set digest. Single-view authoring preview
returns only an envelope and cannot mint an active binding. Timestamps derive
from schema evidence, so exact retries do not drift.

The route key is `(installation_id, surface_revision, view_id)`. The review
transaction persists one `app_surface_generations` row plus exact binding and
envelope bytes per `app_surface_generation_members` row; activation consumes
only a complete set. Envelope fields are private: only the registry hydration
reader decodes them, rerunning JSON admission, MUIJ, query-binding and digest
checks. The app adapter is separate from the agent `ui.interaction` path.

## Authenticated hydration route

```text
GET /api/magician/v2/apps/installations/<installation_id>/surfaces[/<local-route>]
GET ...?cursor=<opaque-server-cursor>
GET ...?sort_field=<declared-field>&sort_direction=<ascending|descending>
```

Credential middleware supplies `VerifiedRequestIdentity`; the handler derives
`AuthenticatedAppScope` and never takes scope, installation or grant from the
request. Disabled, quarantined, retained, purged, missing and cross-scope
installations look not found. Percent-encoded, traversing, non-ASCII and
non-canonical paths are rejected.

One read loads the enabled installation and its active package/schema/surface
tuple, checks the generation's route index, host routes, overlap-free member
count and set digest without decoding every document, resolves the path and named parameters, decodes only the
selected envelope, derives an `AppQueryRequest` from its `viewBinding` (named
parameters become typed equality nodes, so `/plans/:plan_id` cannot hydrate
every plan), runs the indexed query, then rechecks the active generation and the selected
route/byte fingerprint. The
response carries the `AppSurfaceBinding`, empty `AppSurfaceEnvelope`, route
parameters and `AppQueryPage`. A concurrent generation change returns a stale
conflict instead of mixing old surface and new schema.

## Responsive app route and CRUD adapter

Unified UI owns `/apps/<installation_id>[/<local-route>]` in browser and Tauri.
It replaces only the `app_entity_query_v1` component with the returned page and
passes an empty agent identity to `MuijRenderer`. Page, sort, row and Tree
events are local, never `ui.interaction`.

- List and Table use `EntityGrid` row actions; Tree selection opens the same
  form; Timeline and Graph are read-only.
- Mutable views hydrate the full entity payload even when a Table shows fewer
  columns, so a save never overwrites an unseen field.
- Timeline uses `ServerPager`. A V1 Tree must fit one page: the server returns
  a size error rather than promote orphans, and the client rejects partial
  Trees. Opaque server cursors are the only paging authority; unknown totals
  show `+`.

```text
POST /api/magician/v2/apps/installations/<installation_id>/surface-mutations
```

The body names only protocol/surface/view/client-mutation identity and a
create, update or delete, never scope, installation, entity, schema, grant or
provenance. The server resolves the active view, requires expected record
revisions and rechecks the surface revision inside the entity-store
transaction. Optimistic rows stay on the current page; failures restore it;
stale conflicts refresh. If commit is unknown, the UI keeps the exact request
and client-mutation ID for a same-request retry (never a fresh ID); a confirmed
mutation whose refresh fails stays confirmed and retries the refresh. Tree
cycles, missing parents and malformed or duplicate records fail before render,
and the browser revalidates the compiler shape per view kind, route and field
binding. Optimistic creates use a UI-only ID obeying the record-ID
grammar; timestamps keep the instant across `datetime-local`.

## Durable deltas and published navigation

Correctness never depends on a WebSocket event; initial load and gap recovery
use indexed hydration. One projection worker claims the registry entity outbox
in order, emits only installation/surface identity, record identity, revision
and change-sequence, and acks after projection (idempotent replay). Missing
history, rebuild, a sequence gap or a rewound store forces a fresh page; a
cursor ahead of head is a conflict. Realtime signals are advisory: the web
client also polls the visible page with backoff. The iOS `magapp:` scheme runtime opens no WebSocket and uses the
same entity-change REST contract.

The worker also reconciles app-owned `PublishedSurfaceRecord` values from
lifecycle outbox evidence. Boot repair validates scope and ownership before
replacing the shared index and verifies lifecycle generation around the write;
all record and index writes share one scope
lock, so the index is never replaced from a stale snapshot. App navigation
records carry no fabricated `task_id`, output, thread or execution owner.

## Apps discovery and native placement

`GET /api/magician/v2/apps/directory` is the registry-owned, metadata-only
directory: installed, pinned, recent, attention, disabled and
retained/recovery sections with opaque cursors. Search sees only package
name and description. Views, actions, grants and storage totals are strict
summaries; entity bodies never appear. One page batches its reads, with no
per-app surface decode. Launch and pin activity is revalidated against the
current enabled installation in the same transaction.

Unified UI exposes `/apps`, top-bar and command-palette discovery. iOS Today
shows up to eight pinned enabled views (`PinnedAppsTodayProjection.maximumItems`)
and opens a route in an ephemeral WebView; the custom-scheme proxy keeps origin
and port, rejects redirects, cookies, cross-installation APIs and hostile paths,
allowlists headers and bounds bodies (32 MiB per response, read in chunks;
1 MiB per request body; cancellation completes outside the loader lock).
Directory entries and pins are placement metadata, not
Briefing content.

The directory exposes only mounted lifecycle kernels: disable, quarantine,
retained uninstall, grant revoke, update begin/abort and reviewed
install/update/reinstall approval. Re-enable is a separate review/commit and
cannot clear quarantine, update-pending, retained-uninstall or revoked grants.
The `magician app` CLI uses the same owners with explicit scope, generation,
review and replay identities and never opens registry storage.
An `uninstalled_retained` installation can request a five-minute purge preview
listing all 17 Storage Governance owners, then a second destructive
confirmation; the client persists the preview and one idempotency identity
before POST. The receipt discloses shared WAL, shared package/Artifact,
policy-retained audit and provider-unknown evidence instead of claiming full
erasure.

Installation review shows a matrix of policy, destinations, capability
availability and resource ceilings, never screenshots, accessibility text,
prompts or captured content. A contribution panel reads the memory and
personal-agent retrieval owners with no signing control; accepted-memory revoke
stays a Keychain-signed action in desktop Settings.

## Widget slot assignment backend

`apps/slot_assignments.rs` is a layout owner storing workspace defaults and
per-user choices; it neither interprets manifests nor renders widgets. A host
inventory resolver supplies current bindings. Persisted identity: installation
id, package id/revision, package-content digest, installation generation,
widget id.

| Route | Behavior |
|---|---|
| `GET /api/magician/v2/apps/slot-assignments` | Assignment and picker pages; takes a monotonic write fence (newer editor wins). Pages carry `inventory_revision`; restart paging if it changes. |
| `POST /api/magician/v2/apps/slot-assignments` | Needs the fence and exact layout revision (stale is a conflict, never rebased). Mutation ids are idempotency keys. `assign` echoes `expected_candidate`; installation, package revision, content digest, generation or widget-id drift from current inventory returns `409`. The 128 latest receipts are kept; older retries fail revision CAS instead of reapplying. |
| `GET /api/magician/v2/apps/slots/<slot_id>` | Non-mutating resolved read (blocking lane). |
| `POST /api/magician/v2/apps/slots/resolve-batch` | 1–12 unique page-qualified slot ids, one snapshot, request order (request ≤ 16 KiB, response ≤ 128 KiB). |

The picker lists only installed, enabled, declared widgets, and the store keeps
only the resolver's binding, never caller evidence. Native widget actions may
add `expected_installation_binding: { generation, package_revision_ref }`
(both required; generation positive); a mismatch returns
`409 app_workflow_stale` before admission. A response-lost retry may recover a
run already past its durable start under the same sealed identity; a missing
or pristine pending binding still fails stale. Before compiler registration the
resolver returns an empty inventory with no assignment authority, and registry
rows alone never become candidates; a missing resolver is `503`. The document
is keyed by `(principal, workspace)` with the principal as user key, so
"workspace default" is workspace-wide within the single-owner deployment, not
cross-principal.

Slot keys are `page:<route-hex>:<region>` (every route byte encoded), so equal region names on different
pages cannot collide; bare region names are rejected. Packages cannot set
`system_default`.

**System defaults.** `distribution: system` or directory metadata cannot mint a
default. Only digest-pinned boot admission of exact package bytes from the
host-trusted directory does, and that owner maintains the workspace default
set. Users never delete it: assigning or removing records a customization,
restore clears it, and an uncustomized slot picks up new defaults. At boot the
trusted-directory owner resolves the host seed inventory, admits exact bundle
digests and publishes inert installations; with
`system_packages.enable_at_boot`, a non-transport host grantor runs the normal
review/digest/approval transition.
Ordinary staging rejects `distribution: system`.

- Boot admission (`apps_api.rs` in `magician-api`) refreshes widget
  registrations first; an enabled package missing from the snapshot refuses
  the whole replacement, while awaiting-review or disabled installations
  contribute nothing.
- One revision per package version
  (`app_package_revision_version_immutable_idx`); the revision reference
  hashes the dependency lock. Changed bytes or lock under a published version
  are refused with a version-bump error; changing a seed's embedded primitive
  binding requires a seed version bump.
- Seed dependencies resolve only against `embedded_reviewed_tool_catalog`
  (embedded packs plus the default agent, no discovery roots), so a
  system package's lock is identical in every scope; a seed declaring a scope
  skill fails.
- Boot never moves an enabled system installation to a newer seed; it reports
  `update_available`. The trusted pin holds only current seed digests, so the
  old revision compiles no widgets and slot defaults resolve `package_unavailable` until the
  owner runs `update-begin`, restarts and approves.

Widget render failures log at `warn` once per generation and reason, then
`debug`. `render-batch` keeps a 5 s deadline floor and recomputes cached renders
within `max(5 s, refresh/5)` of expiry.

**Assignment survival.** Disable, quarantine, update-pending and
retained-uninstall hide a widget without deleting its assignment. Resolution
needs `current >= pinned` generation, the same installation and package
identity and the exact digest, so an exact-digest re-enable or reinstall
restores it and any package or digest change stays hidden
(`exact_digest_only`).

**Routes.** Manifest admission rejects overlapping templates (`/plans/:plan_id`
vs `/plans/new`, `/plans/:left` vs `/plans/:right`). A parameter must name a
declared portable scalar field; its kind and enum values persist in
`routeBindings`, and non-canonical numeric aliases are not found.

## Custom-surface host pump

Unified UI shows only the server-compiled no-script `srcdoc` envelope in an
iframe with empty `sandbox`, `no-referrer` and deny-all CSP. The optional
`surfaces/*.js` program runs only in Magician's killable OS worker; its
`last_render` metadata is never read as HTML, script, URL or MUIJ. On iOS, a
route root probes `custom-surface-v1/host`; a minted plan mounts
`AppSurfaceScriptedHostView` (non-persistent WKWebView on a per-installation
`magapp-surface://` origin, 32-message bridge budget), and any refusal keeps
the plain route WebView.

The worker's private socket carries a correlated request/response pump.
`magician.query`, `magician.mutate` and `magician.invoke` block until the host
derives all identity (installation, package, surface, grant, nonce, origin,
request ID, monotonic sequence) from the live session, admits the message through the
bridge watchdog, runs it through the owner services and returns one bounded
response with matching ID and sequence. The worker supplies no authority-bearing field. Unknown methods,
duplicate IDs, skipped sequences, stale revisions, bad or oversized values,
substitution, worker failure or a 5 s timeout end the session.
`magician.subscribe(afterChangeSequence, limit)` reads the entity-change
service.

Run lifecycle: `magician.getRun(action, runRef)`,
`magician.waitRun(action, runRef, maxPolls, pollIntervalMs)` (defaults 8 / 50 ms),
`magician.cancelRun(action, runRef, expectedGeneration, idempotencyKey)`. Each
is correlated to the host session, installation and manifest action and
reopens installation/package/surface/grant authority. Replies carry only opaque
`run_ref`, status, a result-withheld flag, the admitted result or typed error,
a cancellation receipt and a retry disposition, never task/execution IDs or raw
errors. Aborting a fetch is not cancellation. Wrong correlation, unknown fields,
stale authority or substitution retires the session.

Declared actions bind to the task system via
`POST /api/magician/v2/apps/installations/{installation_id}/actions/{action_id}/invocations`,
`GET .../contract` and `POST .../launch`, reusing surface, registry, cursor,
mutation, publication and scope authorities. Action dialogs derive fields from
the server schema and show progress, provenance, source counts, typed errors,
receipts and result envelopes without
raw source references; closing a dialog is not cancellation. A payload-free
physical-activity panel shows session/target refs, resource claim, expiry and
the eight newest settlements, and can submit one stop (shown as
`stop requested` until settlement; see
[interactive capabilities](app-interactive-capabilities.md)). The envelope
carries the durable `change_sequence`; the WebSocket signal is only a wake-up
hint.

## View mapping

| Manifest view | MUIJ component | Empty-state contract |
|---|---|---|
| List | `EntityGrid` | entity fields, server page metadata, 25-row request, no records |
| Table | `EntityGrid` | declared columns only, server page metadata, 25-row request, no records |
| Tree | `Tree` | declared structural bindings, collapsed empty nodes, depth 3 |
| Timeline | `ActivityFeed` | declared activity bindings, empty items, app/update defaults |
| Graph | `Graph` | declared node/edge bindings, empty nodes and edges, layered layout default |

Board returns `UnsupportedViewKind` in V1; it never degrades to lists.

**Graph.** One record is one node, labeled by the first non-structural text or
markdown field. A required nullable self-reference `parent_field` gives
parent→child edges, plus optional numeric `order_field` and enum
`status_field`; nothing else is accepted (no timeline/tree bindings or table
columns), and a label field is required. Graphs are read-only and hydrate from
one complete page. Props decode into `MuijGraphSpec` (`gaui/muij.rs`) and pass
the standard validator (see
[unified-ui/muij-native-ui-boundaries](../unified-ui/muij-native-ui-boundaries.md)).

**Tree labels** use the first non-structural Text field in canonical order,
then Markdown; there are no `title`/`name` heuristics, and no eligible field
fails conformance.

## Declarative core composition

A List or Table view may replace its grid with a closed `components` set:

```yaml
views:
  plans:
    entity: plan
    kind: list
    route: /
    components:
      - component: section
        id: overview
        children:
          - component: detail
            id: current_plan
            fields: [topic, status]
          - component: form
            id: create_plan
            fields: [topic, status]
      - component: list
        id: plan_cards
        fields: [topic, status]
      - component: table
        id: plan_table
        columns: [status, topic]
```

`detail`, `form`, `list` and `table` bind named fields of the view entity;
`section` contains them. IDs are unique; labels are compiler-derived. Unknown
fields fail decoding, so there is no HTML, script, URL, transport, query text,
snapshot, page size, mutation authority, scope, cursor, grant or provider
identity. Limits: 64 components, depth 8, 64 KiB, 64 exposed
fields; rows come from the ≤ 25-record page. The compiler emits one query-owned
`Stack` with a private `surfaceComponents` binding, and Unified UI renders
headings, description lists, lists, tables and the typed form natively, never
as HTML. Omitting `components`
keeps the prior `EntityGrid` output and digests. Tree and Timeline cannot carry
components.

## Correctness and performance invariants

- Component IDs hash length-delimited installation, view, schema revision and
  path.
- The compiled-view digest covers compiler version, package/schema/view/route
  identity and the MUIJ layout, excluding `generated_at`.
- Entity-schema and view-schema digests are recomputed at publication and
  before view selection; the compiled schema must name the same package and
  source entity digest.
- The compiler emits no record body and no `static_snapshot`; the
  `app_entity_query_v1`/`view:<id>` locator carries no authority. One typed
  `viewBinding` per view is the only query source (no raw SQL).
- Route values become bounded equality predicates and never enter the MUIJ
  document; multi-parameter conjunctions use one indexed child to bound
  candidates and re-evaluate the full predicate.
- List/Table reject more than 64 exposed fields before allocating rows;
  `EntityGrid` server mode never
  re-sorts or re-slices on the client.
- Component admission and envelope recovery reject unknown or substituted
  fields, duplicate IDs, missing entity fields and bound overruns.
- The Unified UI `Tree` component normalizes iteratively, rejects shared,
  cyclic or duplicate members, inspects ≤ 1,000 candidates, admits ≤ 500
  nodes, depth ≤ 3 (`Tree.svelte`).
- Compilation owns no cache or database. Hydration decodes only the requested
  envelope; the directory decodes no member documents.
- Sort is limited to visible sortable fields; mutation provenance is minted
  below the surface adapter; the store fence binds the surface revision through
  the final transaction.
- Output passes the MUIJ component registry and `AppSurfaceBinding`
  validation.
