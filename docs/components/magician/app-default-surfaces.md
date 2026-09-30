# App default surfaces

An admitted declarative app view compiles into the existing MUIJ render
contract: a complete validated empty generation is persisted atomically,
hydrated under authenticated scope as a canonical indexed entity page, rendered
on responsive Unified UI routes, mutated through the canonical entity store, and
published through bounded durable deltas. The Apps directory, launchers, Today
pins and the iOS WebView posture sit on the same owners.

## Compilation and persistence

`apps/surface_compiler.rs` accepts an opaque `VerifiedAppSurfaceSource` minted
only by rejoining staged package bytes, the immutable registry package record and
the compiled schema revision, so callers cannot supply a manifest with another
package reference or schema digest. It returns one `CompiledAppSurface` per
view:

- the durable `AppSurfaceBinding`, including the collision-free
  `/apps/<installation>/<local-route>` host route and a canonical compiled-view
  digest;
- an `AppSurfaceEnvelope` with installation ID, surface revision, view ID,
  validated `MuijDocument` and `interaction_mode: app`.

Activation uses `CompiledAppSurfaceSet`: it verifies entity/view digests once,
iterates the canonical view map once, and returns every route under one active
`surface_revision` plus a stable set digest. Single-view authoring preview
returns only an envelope and cannot mint an active binding. Activation
timestamps derive from immutable schema evidence, so exact retries do not drift.

The active revision names a generation; the route key is
`(installation_id, surface_revision, view_id)`. The registry persists one
`app_surface_generations` row plus exact binding and envelope bytes per
`app_surface_generation_members` row in the review transaction; activation
consumes only a complete compiled set.

Envelope fields are private. Only the registry-owned hydration reader decodes
persisted bytes, rerunning bounded JSON admission, MUIJ validation, query-binding
validation and member/set digest checks. The app adapter is distinct from the
agent `ui.interaction` path.

## Authenticated hydration route

```text
GET /api/magician/v2/apps/installations/<installation_id>/surfaces[/<local-route>]
GET ...?cursor=<opaque-server-cursor>
GET ...?sort_field=<declared-field>&sort_direction=<ascending|descending>
```

Credential middleware supplies `VerifiedRequestIdentity`; the handler derives
`AuthenticatedAppScope` and never takes scope, installation or grant from
query/body. Disabled, quarantined, retained, purged, missing and cross-scope
installations are concealed as not found. Percent-encoded, traversing,
non-ASCII and non-canonical paths are rejected.

One hydration read:

1. loads the enabled installation and its active package/schema/surface tuple;
2. loads the generation's route/digest index, validates host routes and
   overlap-free member count, and recomputes the set digest without decoding
   every MUIJ document;
3. resolves the concrete path, including named route parameters;
4. decodes and revalidates only the selected binding/envelope and derives a
   canonical `AppQueryRequest` from its compiler-owned `viewBinding`, adding only
   the server cursor and installation. Named parameters become typed equality
   nodes, so `/plans/:plan_id` cannot hydrate every plan;
5. runs the indexed owner query;
6. rechecks the active generation and selected route/byte fingerprint.

The response carries the `AppSurfaceBinding`, empty `AppSurfaceEnvelope`, route
parameters and canonical `AppQueryPage`. Initial state needs no event delivery;
a concurrent generation change returns a stale conflict rather than mixing old
surface and new schema.

## Responsive app route and CRUD adapter

Unified UI owns `/apps/<installation_id>[/<local-route>]` in browser and Tauri.
It hydrates the MUIJ envelope, replaces only the `app_entity_query_v1`
component with the returned page, and passes an empty agent identity to
`MuijRenderer`. Page, sort, row and Tree-selection events are handled locally,
never via `ui.interaction`.

- List and Table use `EntityGrid` row actions; Tree selection opens the same
  typed form; Timeline and Graph are read-only.
- Mutable views hydrate the complete bounded entity payload even when a Table
  shows fewer columns, so a form save never overwrites an unseen field.
- Timeline uses `ServerPager`. A V1 Tree must fit in one bounded page: the
  server returns a size error rather than promoting orphaned children to roots,
  and the client rejects partial Trees. Opaque server cursors are the only
  forward-page authority; unknown totals show `+`.

Surface edits:

```text
POST /api/magician/v2/apps/installations/<installation_id>/surface-mutations
```

The body names only protocol/surface/view/client-mutation identity plus a
create, update or delete — never scope, installation, entity, schema, grant or
provenance. The server resolves the active view, derives entity and surface
origin, requires expected record revisions, and rechecks the surface revision
inside the entity-store transaction.

- Optimistic rows are bounded to the current page; ordinary failures restore
  the prior page; stale conflicts refresh canonical state.
- If transport fails before commit is known, the UI keeps the exact request and
  client-mutation ID for a same-request retry (never a fresh ID). A confirmed
  mutation whose refresh fails stays confirmed and retries the refresh.
- Tree cycles, missing parents and malformed/duplicate records fail before
  render via iterative bounded walks. The browser revalidates the compiler shape
  per view kind, route and field binding. Optimistic creates use a UI-only ID
  obeying the record-ID grammar; timestamp inputs preserve the instant across
  `datetime-local`.

## Durable deltas and published navigation

Correctness never depends on a WebSocket event; initial load and gap recovery
return to indexed hydration. The registry entity outbox owns mutation, import
and rebuild evidence. One projection worker claims it in sequence, emits only
installation/surface identity, record identity, revision and change-sequence,
and acks after projection (idempotent crash replay). Missing history, rebuild,
sequence gap or rewound store forces a fresh page; an ahead-of-head cursor is an
explicit conflict.

Realtime signals are advisory: the web client also runs a visible-page bounded
poll with exponential retry. The iOS `magapp:` scheme runtime does not open the
WebSocket; it uses the same authenticated entity-change REST contract through
its scheme handler.

The same worker consumes lifecycle outbox evidence and reconciles app-owned
`PublishedSurfaceRecord` values. Boot repair reads one bounded record
inventory, validates scope and app ownership before replacing the shared index,
plans from batched registry snapshots, and verifies lifecycle generation around
the write. Record and index writes (including task deletion and startup repair)
share one scope lock, so the shared index is never replaced from a stale
snapshot. App navigation records carry no fabricated `task_id`, output, thread
or execution owner.

## Apps discovery and native placement

`GET /api/magician/v2/apps/directory` is the registry-owned, metadata-only
directory: installed, pinned, recent, attention, disabled and
retained/recovery sections with opaque cursors and bounded pages. Search sees
only package name/description. Views, actions, grants and storage totals are
strict summaries; entity bodies never appear. One page batches view-member,
pin, action and grant reads (no per-app surface decode). Launch and pin activity
is revalidated against the current enabled installation in the same
transaction.

Unified UI exposes `/apps`, top-bar/command-palette discovery and at most eight
pinned enabled views on Today. iOS reads the same contract and opens a route in
an ephemeral WebView; the custom-scheme proxy preserves origin and port, rejects
redirects/cookies/cross-installation APIs and hostile paths, allowlists headers,
and bounds declared and streamed bodies (4 MiB envelope, chunked; cancellation
completes outside the loader lock). Directory entries and pins are placement
metadata, not Briefing content.

The directory exposes only mounted lifecycle kernels: disable, quarantine,
retained uninstall, grant revoke, update begin/abort and reviewed
install/update/reinstall approval. Re-enable is a separate exact review/commit
and cannot clear quarantine, update-pending, retained-uninstall or revoked
grants. The `magician app` CLI uses the same owners with explicit scope,
generation, review and replay identities and never opens registry storage. An
`uninstalled_retained` installation can request a five-minute whole-installation
purge preview listing all 17 Storage Governance owners, with a second
destructive confirmation; the client persists the preview and one idempotency
identity before POST, and the receipt discloses shared WAL, shared
package/Artifact, policy-retained audit and provider-unknown evidence rather than
claiming full erasure.

Installation review renders a bounded human-readable matrix of policy,
destinations, capability availability and resource ceilings — never screenshots,
accessibility text, prompts or captured content. A contribution panel reads the
memory and personal-agent retrieval owners (state, source revision, reason,
retention) with no signing control; accepted-memory revoke stays a
Keychain-signed action in desktop Settings.

## Widget slot assignment backend

`apps/slot_assignments.rs` is a separate layout owner storing workspace
defaults and per-user choices; it neither interprets manifests nor renders
widgets. A host-owned inventory resolver supplies bounded current widget/package
bindings. Persisted identity: installation id, package id/revision, exact
package-content digest, installation generation, widget id.

- `GET /api/magician/v2/apps/slot-assignments` returns bounded assignment and
  picker pages and acquires a monotonic write fence (a newer editor supersedes an
  older one). Every page carries its `inventory_revision`; clients restart
  pagination if it changes.
- `POST /api/magician/v2/apps/slot-assignments` needs the fence and the exact
  layout revision (stale → conflict, never rebased). Mutation ids are
  exact-request idempotency keys. `assign` echoes the picker's
  `expected_candidate`; installation, package revision, content digest,
  generation and widget id are compared with current inventory (`409` on
  drift). The 128 most recent receipts are retained; older retries fail revision
  CAS rather than reapplying.
- `GET /api/magician/v2/apps/slots/<slot_id>` is the non-mutating resolved read
  (blocking lane).
- `POST /api/magician/v2/apps/slots/resolve-batch` takes 1–12 unique
  page-qualified slot ids and returns results in request order from one
  inventory snapshot (request ≤ 16 KiB, response ≤ 128 KiB).

The picker includes only installed, enabled, currently declared widgets; the
store persists only the resolver-owned current binding, never caller package
evidence. Native widget actions may add
`expected_installation_binding: { generation, package_revision_ref }` (both
required; generation positive); a mismatch with the live installation returns
`409 app_workflow_stale` before admission. A response-lost retry may recover a
run already past its durable start under the same sealed identity; a missing or
pristine pending binding still fails stale.

Before exact compiler registration the widget runtime resolver returns an empty
inventory with no assignment authority; registry rows alone never become
candidates; a missing resolver is `503`. The document is rooted by
`(principal, workspace)` with the principal as user key, so "workspace default"
means workspace-wide within the single-owner deployment, not cross-principal.

Slots are page-qualified: key `page:<route-hex>:<region>` (every route byte
encoded, so identical region names on different pages cannot collide). Bare
region names are rejected everywhere. Picker suggestions keep page, region and
`system_default`; packages cannot set that bit.

**System defaults.** `distribution: system` or directory metadata cannot mint a
default. Only host-trusted-directory, digest-pinned boot admission of exact
package bytes does, and that owner maintains the whole workspace default set.
Users never delete it: assigning or removing records customization, restore
clears it, and an uncustomized slot automatically gets newly admitted defaults.
At boot the trusted-directory owner resolves the host seed inventory, admits
exact bundle digests, publishes inert installations, and — with
`system_packages.enable_at_boot` — uses a non-transport host grantor to run the
normal review/digest/approval transition. Ordinary staging rejects
`distribution: system`.

- Boot admission (`apps_api.rs` in `magician-api`) refreshes the scope's widget
  registrations before minting from the runtime inventory, then accounts for
  every admitted installation: an enabled package missing from the snapshot
  refuses the whole replacement, while awaiting-review or disabled installations
  contribute nothing.
- The registry keeps one revision per package version
  (`app_package_revision_version_immutable_idx`), and the revision reference
  hashes the resolved dependency lock. A version seals bytes and lock, so lock
  drift or changed bytes under a published version are refused with a
  version-bump error; changing a seed's embedded primitive binding requires a
  seed version bump.
- Seed dependency evidence resolves against the binary's embedded catalog only
  (`embedded_reviewed_tool_catalog`: embedded packs plus the default agent, no
  discovery roots), so a system package's lock is a function of build and seed
  bytes, identical in every scope; a seed declaring a scope skill fails loudly.
- Boot never moves an enabled system installation to a newer seed: it reports
  `update_available`. Because the trusted pin holds only current seed digests,
  the old revision compiles no widgets and slot defaults resolve
  `package_unavailable` until the owner runs `update-begin`, restarts and
  approves.

Widget render failures log at `warn` once per installation generation and reason
(then `debug`). `render-batch` never returns a near-now deadline (5 s floor;
cached renders within `max(5 s, refresh/5)` of expiry are recomputed).

**Assignment survival.** Disable, quarantine, update-pending and
retained-uninstall hide a widget without deleting its assignment. Resolution
needs a monotonic generation (`current >= pinned`) for the same installation and
package identity and the exact digest, so an exact-digest re-enable/reinstall
restores it; any package/digest change stays hidden
(`exact_digest_only`; no compatibility guessing).

**Routes.** Manifest admission rejects overlapping route templates
(`/plans/:plan_id` vs `/plans/new`, `/plans/:left` vs `/plans/:right`), so route
ownership is deterministic. A parameter must name a declared portable scalar
field; its kind and enum values persist in `routeBindings`, and non-canonical
numeric aliases are not found.

## Custom-surface host pump

Unified UI shows only the server-compiled no-script `srcdoc` envelope in an
iframe with empty `sandbox`, `no-referrer` and deny-all network/script CSP. The
optional `surfaces/*.js` program runs only in Magician's killable OS worker; its
`last_render` metadata is never interpreted as HTML, script, URL or MUIJ.
Scripted custom surfaces are disabled on iOS.

The worker's private socket carries a bounded correlated request/response pump.
`magician.query`, `magician.mutate` and `magician.invoke` block until the host
has (1) derived installation, package, surface, grant, nonce, origin, request ID
and monotonic sequence from the live session; (2) admitted the message through
the bridge watchdog; (3) executed it via the owner query/mutation/workflow
services; (4) returned one bounded response with matching ID and sequence. The
worker supplies no authority-bearing field. Unknown methods, duplicate IDs,
skipped sequences, stale revisions, malformed or oversized values, response
substitution, worker failure or a 5 s pump timeout tear down or refuse the
session. `magician.subscribe(afterChangeSequence, limit)` is a bounded read over
the entity-change service.

Run lifecycle: `magician.getRun(action, runRef)`,
`magician.waitRun(action, runRef, maxPolls, pollIntervalMs)` (defaults 8 / 50 ms),
`magician.cancelRun(action, runRef, expectedGeneration, idempotencyKey)`. Each is
correlated to the host session, installation and manifest action and reopens
installation/package/surface/grant authority. Replies carry only opaque
`run_ref`, closed status/terminal truth, result-withheld flag, policy-admitted
result or typed error, cancellation generation/sanitized receipt and a closed
retry disposition — never task/execution IDs or raw errors. Aborting a fetch or
poll is only transport abort. Wrong correlation, unknown fields, stale authority
or substitution retires the session.

Declared actions bind to the task/execution system via
`POST /api/magician/v2/apps/installations/{installation_id}/actions/{action_id}/invocations`,
`GET .../contract` and `POST .../launch`, reusing surface, registry, cursor,
mutation, publication and scope authorities.

Action dialogs derive accessible fields from the server schema and render
progress, label/provenance metadata, source counts, typed errors, receipts and
result envelopes without raw source references. Request abort and dialog
closure are not run cancellation. A payload-free physical-activity panel shows
opaque session/target refs, profile/activity, resource claim, expiry and the
eight newest settlement summaries, and can submit one session-correlated stop
(shown as `stop requested` until settlement; see
[interactive capabilities](app-interactive-capabilities.md)).

The host envelope carries the durable `change_sequence`; the payload-free
WebSocket signal is only a wake-up hint for the cursor-based entity-change
route.

## View mapping

| Manifest view | MUIJ component | Empty-state contract |
|---|---|---|
| List | `EntityGrid` | canonical entity fields, server page metadata, 25-row request, no records |
| Table | `EntityGrid` | declared columns only, server page metadata, 25-row request, no records |
| Tree | `Tree` | declared structural bindings, collapsed empty nodes, hard depth 3 |
| Timeline | `ActivityFeed` | declared activity bindings, empty items, app/update defaults |
| Graph | `Graph` | declared node/edge bindings, empty nodes and edges, layered layout default |

Board returns the explicit V1 `UnsupportedViewKind`; it never degrades to lists.

**Graph.** One record is one node; the label is the first non-structural text or
markdown field; a required nullable self-reference `parent_field` derives
parent→child edges; optional numeric `order_field` and enum `status_field` —
that is the whole V1 shape, validated fail-closed (no timeline/tree bindings, no
table columns, label field required). Graphs are read-only and hydrate from a
complete page like trees. Props decode into the bounded `MuijGraphSpec`
(`gaui/muij.rs`) and validate through the standard MUIJ validator (see
[unified-ui/muij-native-ui-boundaries](../unified-ui/muij-native-ui-boundaries.md)).

**Tree labels** use the first non-structural Text field in canonical order, then
Markdown; no `title`/`name` heuristics; no eligible field fails conformance.

## Declarative core composition

A List or Table view may replace its single grid with a closed `components` set:

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

`detail`, `form`, `list` and `table` bind only named fields on the view entity;
`section` contains those. IDs are unique across the tree; accessible labels are
compiler-derived. Unknown fields fail decoding, so no raw HTML, script, URLs,
transport, query text, snapshots, page size, mutation authority, scope, cursor,
grant or provider identity. Limits: 64 components, depth 8, 64 KiB, 64 exposed
fields; rows always come from the ≤ 25-record validated page. The compiler emits
one inert query-owned `Stack` with a strict private `surfaceComponents` binding;
Unified UI renders semantic headings, description lists, lists, tables and the
typed form (keyboard-accessible, scrollable tables on narrow screens), never as
HTML. Omitting `components` keeps the exact prior `EntityGrid` output and
digests. Tree and Timeline cannot carry a component tree.

## Correctness and performance invariants

- Component IDs hash length-delimited installation, view, schema revision and
  component path (no concatenation collisions).
- The compiled-view digest covers compiler version, package/schema/view/route
  identity and the exact MUIJ layout, excluding `generated_at`.
- Compilation recomputes entity-schema and view-schema digests at publication
  and before view selection; the compiled schema must name the same package and
  source entity digest.
- The compiler emits no record body and no `static_snapshot`; the
  `app_entity_query_v1`/`view:<id>` locator carries no scope, grant, cursor or
  bearer authority. One strict typed `viewBinding` per view is the only query
  source (no raw SQL or private dialect).
- Route values hydrate through bounded equality predicates and never enter the
  MUIJ document; multi-parameter conjunctions use one indexed child to bound
  candidates and re-evaluate the full predicate.
- List/Table reject > 64 exposed fields before allocating rows; initial request
  25; `EntityGrid` server mode never re-sorts or re-slices client-side.
- Component admission and envelope recovery reject unknown or substituted
  fields, duplicate IDs, missing entity fields and bound overruns.
- Tree normalization is iterative, rejects shared/cyclic/duplicate members,
  inspects ≤ 1,000 candidates, admits ≤ 500 nodes, depth ≤ 3. EntityGrid filter
  projection uses a bounded iterative flattener.
- Compilation is bounded and iterative, owns no cache or database. Hydration
  scans the member index but decodes only the requested envelope; directory
  discovery never decodes member documents.
- Editable hydration selects the full bounded field contract while keeping the
  display projection; sort is limited to visible sortable fields; mutation
  provenance is minted below the surface adapter; the store fence binds the
  surface revision through the final transaction.
- Output passes the MUIJ component registry and `AppSurfaceBinding`
  route/contract validation.
