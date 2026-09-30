# Custom Interactive Surfaces V1 (manifest surface and review)

The owner's per-installation entry-point grant is persisted and enforced at
runtime. Design:
`docs/archive/plans/2026-08-26-custom-interactive-surfaces-design.md`.

## Manifest surface

A package declares the capability; the owner admits it; nothing is inferred
from bundle contents alone.

- **Feature.** `metadata.magician.required_features: [custom_surfaces_v1]`
  (`AppManifestFeature::CustomSurfacesV1`, additive and closed like every
  manifest feature).
- **Permission.** `app.permissions: [custom_surface]`
  (`AppManifestPermission::CustomSurface`, a closed one-entry V1
  vocabulary; unknown permission strings fail to decode).
- **Declaration block.** `app.custom_surface.entry_points` lists at most
  eight entries, each `{ route, document }`:

  ```yaml
  app:
    permissions: [custom_surface]
    custom_surface:
      entry_points:
        - route: /canvas
          document: surfaces/canvas.html
  ```

Fail-closed coherence, enforced by the manifest kernel
(`magician/src/magician_v2/apps/manifest.rs`):

- Permission without the declaration block, the block without the
  permission, and either without the `custom_surfaces_v1` feature are all
  refused at parse time.
- Entry documents must be `surfaces/`-relative HTML members of the exact
  verified bundle (`MissingBundleMember` otherwise).
- Entry-point routes are canonical bounded routes, unique, and may not
  overlap any declared view route.
- Caps: `APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS = 8` (matching the bridge
  watchdog's 8 sessions per installation) and
  `APP_CUSTOM_SURFACE_MAX_EXECUTABLE_BYTES = 8 MiB` over total
  `.js`/`.mjs` bytes under `surfaces/`. Package-wide bundle caps are
  unchanged.
- `.wasm` members under `surfaces/` are refused while the capability is
  declared (WASM stays refused until a wasm-specific threat review).
- There is deliberately no network field on the block. V1 surfaces have no
  egress channel; a manifest attempting to declare one is invalid (unknown
  field), not narrowed.

A package that declares nothing keeps its manifest digest and behavior:
executable `surfaces/` members stay refused by the resolver.

## Operator switch

`app_platform.custom_surfaces_v1.enabled` in `magician-config.yaml` has a
false schema default. The repo-root seed explicitly enables it for reviewed
system app destinations such as Brainstorm and Meetings; existing runtime
configs must enable it too. Absent or false keeps every host on the no-script
baseline. The switch is process-wide; per-package admission still needs the feature, the
permission, and the owner's review. It is consulted on **all three**
scripted routes — host-open, asset serving, and the bridge — not only
host-open: disabled means no scripted-surface traffic at all, answered
with the same 404 as an absent capability. The switch is read per request
through a focused reader (string-level YAML validation plus one read of
the `app_platform.custom_surfaces_v1` section) rather than a full config
load, because the handlers hold no config snapshot, there is no
config-reload event to invalidate a cache against, and a cached kill
switch that lagged the operator's flip would defeat its purpose.

An enabled app installation can appear in the command palette as an
"app destination" while this separate host switch is off. Opening a scripted
destination then reports `the custom_surfaces_v1 capability is disabled for
this process`. Enable it in the active runtime config (normally
`$MAGICIAN_ROOT_DIR/magician-config.yaml`, defaulting to
`~/MagicianNotes/magician-config.yaml`; `MAGICIAN_CONFIG_PATH` overrides it):

```yaml
app_platform:
  custom_surfaces_v1:
    enabled: true
```

Merge this into the existing `app_platform` section, then reload the app page.
No rebuild or process restart is required. Declarative destinations such as
Learning do not use this switch.

The switch alone does not grant an installed app permission to host its pages.
An empty `granted_custom_surface_entry_points` still refuses them. Current
system boot enablement omits that field. In the web installation review, select
the required **Interactive pages** before approving. The page choices start
unchecked and grant only the selected route/document pairs, bound to the exact
reviewed request digest. Fully interactive pages need that explicit approval. Until then, web
navigation destinations with a declared `fallback_view` load that authorized
standard view when the scripted host returns 404. Learning links resolve its
`queue` view to its declared `/` route, rather than requesting `/queue` and
incorrectly probing the scripted host.

An already-enabled installation with an empty page grant needs a fresh reviewed
update; toggling the operator switch or re-enabling the installation does not
rewrite its grant. For a shipped system package, **Begin update** parks the
existing installation, and the next service boot publishes the trusted seed as
its review candidate. Review that candidate, select its interactive pages, and
approve the update through the normal migration/review flow. The installation
identity and retained data remain in place; **Abort update** restores the
previous operational state before approval. A service restart alone never
grants these pages.

## Owner review

`installation_review.rs` hydrates a
`requested_custom_surface` block (module `custom_surface_review.rs` in
`magician-apps`) whenever a staged package declares the permission:

- the declared entry points with each entry document's verified content
  digest;
- the **full executable-member inventory** — every `.js`/`.mjs` under
  `surfaces/` (extensions matched case-insensitively by the same
  predicate the manifest kernel's 8 MiB cap uses, so a swapped-case
  `.JS` cannot count toward the cap while escaping the inventory) with
  content digests, the canonical "what code ships" list;
- the platform's static asset-scan findings (`fetch`, `XMLHttpRequest`,
  `WebSocket`, `sendBeacon`, `window.open`, form actions, external URLs,
  `postMessage` to `parent`/`top`). The scan is a review aid, never a
  boundary — minification defeats it; the boundary is the isolation kernel;
- the fixed posture constants the review displays and never chooses
  (`allow-scripts` sandbox, deny-egress CSP body).

The block is folded into the review-material digest, so a package that
swaps its canvas script changes the digest the owner must attest at
approval.

**Grant narrowing.** `POST .../approve` accepts
`granted_custom_surface_entry_points`, each entry naming an exactly
reviewed `(route, document)` pair bound to the reviewed request digest.
Omitted or empty grants **no** surfaces — the grant is never implicit-all
— and granting entry points that were not requested, or binding a stale
digest, fails closed. The approve receipt lists the granted routes.

The granted subset is persisted on the canonical `AppGrantRevision` as
`granted_custom_surface_entry_points` — the exact `(route, document,
document_digest)` triples the owner attested, with `serde(default,
skip_serializing_if)` so grant revisions without it decode unchanged and keep
byte-identical canonical authority digests (the axis joins
`app_granted_authority_digest` whenever non-empty, including when it is
the grant's only non-legacy authority axis). At host-open the API resolves
the installation's live grant revision through the same enabled-installation
resolver the review and execution kernels use, and
`compile_scripted_surface_host_plan` admits ONLY an entry point whose
`(route, document)` is exactly in the granted set AND whose live entry
document still hashes to the granted digest:

- declared but not granted (or granted for a different document) →
  `EntryPointNotGranted`, answered 404 exactly like an undeclared route;
- stale digest (the live document no longer matches the attested bytes)
  → `GrantedDigestStale`, 409 fail-closed until a fresh review re-grants;
- empty grant → zero surfaces: every declared route is refused.

The operator switch remains the process-wide control; this grant is the
per-installation control. The granted set also participates in the
permission-diff axis (`custom_surface_entry_points`): entries compare by
full `(route, document, digest)` equality, so a newly granted route or a
swapped entry-document digest is an expansion that requires re-review,
while narrowing is visible without forcing one.

## Serving posture

- **Kernel CSP on every served member.** The asset route attaches
  `custom_surface_v1_csp` to EVERY response regardless of media type. A
  sandboxed frame that self-navigates to an SVG (`image/svg+xml`) still
  meets `default-src 'none'` / `connect-src 'none'`. If the CSP cannot
  be composed, the request fails; an empty CSP header is never emitted.
- **Script-capable documents serve only as declared entry points.**
  `.svg` plus `.htm`/`.xhtml`/`.xht` that are not a declared entry
  document are `ScriptCapableDocumentRefused`; undeclared `.html` is
  also refused. Entry documents must be `.html`, so SVG is not servable.
- **JavaScript media type.** `.js`/`.mjs` (case-insensitive) serve as
  `text/javascript; charset=utf-8` so they execute under
  `script-src 'self'`.
- **The switch guards every route.** Asset and bridge refuse with the
  same 404 when `custom_surfaces_v1.enabled` is off.

## Session-scoped sibling resolution

Relative `<script src="canvas.js">` resolves under the entry document's
digest-keyed address, so the digest segment names the ENTRY document,
not the requested member. A sandboxed frame cannot construct any other
address.

`AppScriptedSurfaceRuntime::serve_asset` admits exactly one widened
address: when the digest does not name the requested member, it must
name the session's entry-document digest, and the live entry document
must still hash to that digest. The sibling is then served from the
same live package bundle with its own manifest-verified bytes.

- the member must be a validated `surfaces/` member of the exact live
  package revision;
- script-capable non-entry documents stay refused under both address
  forms; `.wasm` stays refused;
- a digest naming neither the member nor the still-live entry document
  is `DigestMismatch` (404); unknown / TTL-expired / budget-exceeded /
  torn-down sessions answer `SessionGone`.

Immutable caching only when the address names the served bytes.
Sibling responses are `private, no-store` because the URL names the
entry document, not the script.

## Session-scoped entry URL

The asset route identifies the session by a REQUIRED first path segment after
`assets/` — the one credential a sandboxed (`allow-scripts`, opaque-origin)
frame can present. `scripted_surface_asset_address` mints `entry_url` as
`…/custom-surface-v1/assets/<session>/<entry-digest>/<entry-document>`. A query
parameter cannot serve: resolving `<script src="canvas.js">` replaces the base
query, while relative subresources keep the session and digest path.

- **Web.** `parseScriptedSurfaceHostPlan` pins the entry address to exactly
  `<plan.session_ref>/<entry-digest>/<entry-document>`; a missing or substituted
  session, an extra query, or any other address refuses the mount.
- **iOS.** `magapp-surface://<installation>/<session>/<digest>/<member>`; the
  scheme proxy forwards exactly that canonical path. Missing, rewritten, encoded
  or query-augmented addresses are refused client-side.
- **Security.** The session is a read-only bearer for one installation's
  reviewed `surfaces/` members (script-capable documents only when declared,
  `.wasm` never, kernel CSP on every response), 15-minute TTL, `SessionGone` once
  expired or torn down. It cannot reach bridge methods (entity queries, actions),
  which need the host page's authentication plus the admitted envelope (nonce,
  sequence, revision bindings). Responses are `private`; it is never an owner
  credential.

## Browser asset authentication

Browsers cannot attach the host's Authorization header to a sandboxed frame, so
the composition root supplies the app platform's shared live-session verifier to
the global bearer gate. Only canonical asset GETs with no query or explicit
Authorization header can use this path. It verifies the session lifetime and
installation and attaches an opaque file-serving proof; it never constructs a
general API identity. The asset handler resolves the stored minting scope and
still checks the enabled installation, package bytes, and pinned revisions.

The proof cannot authorize host-open, bridge, entity-data, or reload-note calls.
Unknown, expired, torn-down and substituted sessions fail, and an explicitly
invalid bearer remains invalid. Cloudflare Access policy is still enforced.
The session URL is a read-only credential with the 15-minute lifetime; it
contains no owner login token and must not be logged or shared.

In local Vite development, `/api/magician` preserves the UI Host header so an
unset `mobile_access.public_origin` (which can also come from the runtime
environment) produces the correct `frame-ancestors` origin; a configured public
origin takes precedence. For local Vite access, the development proxy replaces
that single HTTP(S) frame ancestor with `'self'` only for reviewed HTML assets
requested with a loopback Host **and** a loopback socket peer; all other CSP
directives stay intact, tunnel/remote requests and explicit `'none'` policies
keep the backend policy, and adapted responses are private and not cached.
Neither the sandbox nor the deny-network CSP is relaxed; the production server
is unchanged.

## Session-bound scope (hosted-web CF Access fix)

Hosted web behind Cloudflare Access has no workspace on the identity
(`workspace: None`; the owner picks one via `X-Workspace`), and the frame can
send no headers, so the entry document's GET could not resolve a scope. **The
session carries the minting request's scope.**

- **Mint.** `GET .../host` resolves its normal authenticated scope and
  `open_host` binds principal + workspace to the live session
  (`AppScriptedSurfaceRuntime.session_scope`) for its lifetime. The binding is
  server-side and never serialized into the host plan.
- **Serve (dual path).** `GET .../assets/{session}/{digest}/{tail}` first
  authenticates like every route. Only on the workspace-missing error does the
  session's bound scope resolve the serving scope. The verified identity's
  principal must equal the minting principal (`app_scope_mismatch` otherwise)
  and any engraved workspace must agree; the lent scope then goes through the
  same scope-checked installation/package resolution.
- **Fail-closed.** Unknown, expired, budget-exceeded or torn-down sessions are
  `SessionGone`. No other auth refusal recovers.
- **Bridge unchanged.** `POST .../bridge` still needs the full host envelope.
  Native clients sending their bearer use the ordinary identity path; local
  loopback and paired-device deployments never take the fallback.

## Bridge

- **Transport order.** Web and iOS hosts serialize admitted bridge submissions
  through one per-frame FIFO: the server's sequence fence is strict and
  independent HTTP/2 / `URLSession` tasks have no arrival-order guarantee, so
  `N+1` is not submitted until `N` completes. Success and ordinary failure
  advance the FIFO; session-level refusal drops pending work; replies are
  addressed by the request id captured per item.
- **Frame launches are server-minted.** The bridge admits the minimal
  `AppDirectActionRequest` (idempotency key + typed input) and takes the action
  name from the session-bound message. A widget/native host may include the
  closed object `expected_installation_binding: { generation,
  package_revision_ref }` (both required when present, generation positive);
  stale truth returns `409 app_workflow_stale`. An exact retry may recover a run
  already started under the same sealed binding. The authenticated host resolves
  installation-bound action/schema/grant revisions, scope, policy and provenance
  immediately before admission; caller provenance is host-owned and stable across
  bridge-session replacement, so retrying an idempotency key after a frame reopen
  recovers the same task. A full `AppActionInvocation` remains a compatibility
  input; frame-supplied mutable authority is not required.
- Each operation's awaited state lives on the heap so the dispatch future stays
  small enough for a debug HTTP worker stack; the regression lane caps its size.

Regression lane: `make test-app-scripted-surfaces` (live asset credentials,
revision binding, both production authentication layers).

## Client host wiring

The operator switch stays process-wide on every client.

- **Web.** `AppSurfacePage.svelte` mounts `AppScriptedSurfaceHost` as the first
  fallback after the declarative surface fails, at the installation root and
  every declared route (`?route=` selects the entry point). Plan parse enforces
  the exact `allow-scripts` sandbox, deny-egress CSP, the session-and-digest entry
  path, and `isAppReference` on `session_ref`.
- **iOS.** `AppRouteBrowserView` probes `custom-surface-v1/host` at an
  installation root (declared MUIJ views are never probed) and mounts a
  `WKWebView` under `magapp-surface://<installation>`; a refused session shows
  the closed failure notice. Viewless enabled installations get Open-app gated
  on `custom_surface_entry_count` (from the admitted manifest, 0 when none).
  Probe refusal keeps the launcher WebView.
- **Desktop.** Unified-ui host plus the desktop origin policy on the
  desktop-managed webviews: a scripted frame cannot load from `tauri://`, a
  dev-server, or any absolute origin, and a widened sandbox constant fails the
  guard closed. See
  [custom-surfaces-v1-origin-policy.md](../desktop/custom-surfaces-v1-origin-policy.md).
- **Android.** Closed in V1: no WebView host; unsupported notice.

## Reference consumer

`brainstorm-canvas` (`magician_data_v3/system/thinking_map/app/`): workflow
`sync_maps` projects `thinking_map_summary` / `thinking_map_snapshot` via
`thinking_maps_data` (`list_maps` + `read_map`); `surfaces/canvas.html` +
`surfaces/canvas.js` render the board as SVG over `query_data`. The map substrate
is never written. A declarative `library` table remains the default surface; the
entry document keeps a boot notice until the bridge answers (a refused script
leaves the notice as the whole surface).

Tests: `authoring.rs`
(`first_party_brainstorm_canvas_package_admits_and_locks_the_thinking_map_read_port`)
and `custom_surface_review.rs`
(`brainstorm_canvas_reference_package_reviews_its_full_surface_request`).
