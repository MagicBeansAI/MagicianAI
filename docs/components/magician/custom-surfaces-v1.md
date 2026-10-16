# Custom Interactive Surfaces V1 (manifest surface and review)

The owner's per-installation entry-point grant is persisted and enforced at
runtime. Design:
`docs/archive/plans/2026-08-26-custom-interactive-surfaces-design.md`.

## Manifest surface

A package declares the capability and the owner admits it; nothing is inferred
from bundle contents.

- **Feature.** `metadata.magician.required_features: [custom_surfaces_v1]`
  (`AppManifestFeature::CustomSurfacesV1`, additive and closed).
- **Permission.** `app.permissions: [custom_surface]`
  (`AppManifestPermission::CustomSurface`, a closed one-entry V1 vocabulary;
  unknown strings fail to decode).
- **Declaration block.** `app.custom_surface.entry_points`, at most eight
  `{ route, document }` entries:

  ```yaml
  app:
    permissions: [custom_surface]
    custom_surface:
      entry_points:
        - route: /canvas
          document: surfaces/canvas.html
  ```

The manifest kernel (`magician/src/magician_v2/apps/manifest.rs`) fails closed:

- Permission without the block, the block without the permission, or either
  without the `custom_surfaces_v1` feature is refused at parse time.
- Entry documents must be `surfaces/`-relative HTML members of the exact
  verified bundle (`MissingBundleMember` otherwise).
- Routes are canonical, bounded, unique and must not overlap a declared view
  route.
- Caps: `APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS = 8` (matching the bridge
  watchdog's 8 sessions per installation) and
  `APP_CUSTOM_SURFACE_MAX_EXECUTABLE_BYTES = 8 MiB` over all `.js`/`.mjs` under
  `surfaces/`. Package-wide caps are unchanged.
- `.wasm` under `surfaces/` is refused while the capability is declared, until a
  wasm-specific threat review.
- There is deliberately no network field. V1 surfaces have no egress channel; a
  manifest declaring one is invalid (unknown field), not narrowed.

A package that declares nothing keeps its manifest digest and behavior:
executable `surfaces/` members stay refused.

## Operator switch

`app_platform.custom_surfaces_v1.enabled` in `magician-config.yaml` defaults to
false in the schema. The repo-root seed sets it `true`, process-wide; the
system packages that declare `custom_surfaces_v1` are Brainstorm
(`thinking_map`), Meetings, Claims Review and Town Square. Existing runtime
configs must enable it too. Absent or false keeps every host on the no-script baseline. The
switch is process-wide and guards **all three** scripted routes (host-open,
asset serving, bridge): off means no scripted traffic, answered with the same
404 as an absent capability. Per-package admission still needs the feature, the
permission and owner review. The switch is read per request by a focused reader
(string-level YAML validation plus one read of the
`app_platform.custom_surfaces_v1` section), not a full config load: the handlers
hold no config snapshot, there is no reload event to invalidate a cache, and a
cached kill switch that lagged the operator's flip would defeat its purpose.

An enabled app can appear in the command palette as an "app destination" while
the switch is off; opening a scripted destination then reports
`the custom_surfaces_v1 capability is disabled for this process`. Enable it in
the active runtime config (normally `$MAGICIAN_ROOT_DIR/magician-config.yaml`,
default `~/MagicianNotes/magician-config.yaml`; `MAGICIAN_CONFIG_PATH`
overrides):

```yaml
app_platform:
  custom_surfaces_v1:
    enabled: true
```

Merge it into the existing `app_platform` section and reload the app page; no
rebuild or restart is needed. Declarative destinations such as Learning do not
use this switch.

The switch does not grant an app permission to host its pages: an empty
`granted_custom_surface_entry_points` still refuses them, and current system
boot enablement omits that field. In the web installation review, select the
required **Interactive pages** before approving; choices start unchecked and
grant only the selected route/document pairs, bound to the reviewed request
digest. Until then, web destinations with a declared `fallback_view` load that
standard view when the scripted host returns 404. Learning links resolve its
`queue` view to its declared `/` route rather than probing `/queue` on the
scripted host.

An enabled installation with an empty page grant needs a fresh reviewed update;
toggling the switch or re-enabling does not rewrite its grant. For a shipped
system package, **Begin update** parks the installation and the next boot
publishes the trusted seed as its review candidate. Review it, select its
interactive pages and approve through the normal migration/review flow.
Identity and data stay in place; **Abort update** restores the previous state
before approval. A restart alone never grants these pages.

## Owner review

`installation_review.rs` hydrates a `requested_custom_surface` block (module
`custom_surface_review.rs` in `magician-apps`) whenever a staged package
declares the permission:

- the declared entry points with each document's verified content digest;
- the **full executable-member inventory**: every `.js`/`.mjs` under
  `surfaces/` with digests, matched case-insensitively by the same predicate as
  the 8 MiB cap, so a `.JS` cannot count toward the cap yet escape the list;
- static asset-scan findings (`fetch`, `XMLHttpRequest`, `WebSocket`,
  `sendBeacon`, `window.open`, form actions, external URLs, `postMessage` to
  `parent`/`top`). The scan is a review aid, not a boundary (minification
  defeats it); the boundary is the isolation kernel;
- the fixed posture the review shows but never chooses (`allow-scripts`
  sandbox, deny-egress CSP body).

The block is folded into the review-material digest, so swapping the canvas
script changes the digest the owner attests.

**Grant narrowing.** `POST .../approve` accepts
`granted_custom_surface_entry_points`, each naming an exactly reviewed
`(route, document)` pair bound to the reviewed request digest. Omitted or empty
grants **no** surfaces; the grant is never implicit-all. Unrequested entry
points or a stale digest fail closed. The receipt lists the granted routes.

The subset is persisted on the canonical `AppGrantRevision` as
`granted_custom_surface_entry_points`: the exact `(route, document,
document_digest)` triples attested, with `serde(default, skip_serializing_if)`
so older revisions decode unchanged and keep byte-identical authority digests.
The axis joins `app_granted_authority_digest` whenever non-empty, even as the
grant's only non-legacy axis. At host-open the API resolves the live grant
revision through the same enabled-installation resolver the review and
execution kernels use, and `compile_scripted_surface_host_plan` admits ONLY an
entry point whose `(route, document)` is in the granted set AND whose live
document still hashes to the granted digest:

- declared but not granted (or granted for another document):
  `EntryPointNotGranted`, a 404 like an undeclared route;
- stale digest: `GrantedDigestStale`, 409 until a fresh review re-grants;
- empty grant: every declared route is refused.

The switch is the process-wide control; this grant is the per-installation one.
The granted set is also a permission-diff axis (`custom_surface_entry_points`)
compared by full `(route, document, digest)` equality: a new route or swapped
digest is an expansion needing re-review, while narrowing shows without forcing
one.

## Serving posture

- **Kernel CSP on every served member.** The asset route attaches
  `custom_surface_v1_csp` to every response whatever its media type, so a frame
  that navigates itself to an SVG (`image/svg+xml`) still meets
  `default-src 'none'` / `connect-src 'none'`. If the CSP cannot be composed the
  request fails; an empty CSP header is never sent.
- **Script-capable documents serve only as declared entry points.** `.svg` and
  `.htm`/`.xhtml`/`.xht` that are not a declared entry are
  `ScriptCapableDocumentRefused`; undeclared `.html` is refused too. Entries
  must be `.html`, so SVG is never servable.
- **JavaScript media type.** `.js`/`.mjs` (any case) serve as
  `text/javascript; charset=utf-8` so they run under `script-src 'self'`.
- **The switch guards every route.** Asset and bridge return the same 404 when
  `custom_surfaces_v1.enabled` is off.

## Session-scoped sibling resolution

Relative `<script src="canvas.js">` resolves under the entry document's
digest-keyed address, so the digest segment names the entry document, not the
requested member. A sandboxed frame cannot build any other address.

`AppScriptedSurfaceRuntime::serve_asset` admits one widened address: when the
digest does not name the requested member, it must name the session's entry
digest, and the live entry document must still hash to it. The sibling is then
served from the same live bundle with its own manifest-verified bytes.

- The member must be a validated `surfaces/` member of the exact live revision.
- Script-capable non-entry documents stay refused under both address forms;
  `.wasm` stays refused.
- A digest naming neither the member nor the live entry is `DigestMismatch`
  (404); unknown, TTL-expired, budget-exceeded or torn-down sessions answer
  `SessionGone`.

Responses are cached immutably only when the address names the served bytes;
sibling responses are `private, no-store`.

## Session-scoped entry URL

The asset route takes the session as a REQUIRED first path segment after
`assets/`, the one credential a sandboxed (`allow-scripts`, opaque-origin) frame
can present. `scripted_surface_asset_address` mints `entry_url` as
`…/custom-surface-v1/assets/<session>/<entry-digest>/<entry-document>`. A query
parameter would not work: resolving `<script src="canvas.js">` drops the base
query, while relative subresources keep the path.

- **Web.** `parseScriptedSurfaceHostPlan` pins the entry address to exactly
  `<plan.session_ref>/<entry-digest>/<entry-document>`; a missing or swapped
  session, an extra query or any other address refuses the mount.
- **iOS.** `magapp-surface://<installation>/<session>/<digest>/<member>`; the
  scheme proxy forwards exactly that path. Missing, rewritten, encoded or
  query-augmented addresses are refused client-side.
- **Security.** The session is a read-only bearer for one installation's
  reviewed `surfaces/` members (script-capable documents only when declared,
  `.wasm` never, kernel CSP always), with a 15-minute TTL and `SessionGone` once
  expired or torn down. It cannot reach bridge methods (entity queries,
  actions), which need the host page's authentication and the admitted envelope
  (nonce, sequence, revision bindings). Responses are `private`; it is never an
  owner credential.

## Browser asset authentication

Browsers cannot attach the host's Authorization header to a sandboxed frame, so
the composition root gives the global bearer gate the app platform's shared
live-session verifier. Only canonical asset GETs with no query and no explicit
Authorization header can use it. It checks session lifetime and installation
and attaches an opaque file-serving proof; it never builds a general API
identity. The asset handler resolves the stored minting scope and still checks
the enabled installation, package bytes and pinned revisions.

The proof cannot authorize host-open, bridge, entity-data or reload-note calls.
Unknown, expired, torn-down and substituted sessions fail, and an explicitly
invalid bearer stays invalid. Cloudflare Access policy still applies. The
session URL is a read-only credential with the 15-minute lifetime; it holds no
owner login token and must not be logged or shared.

In local Vite development, `/api/magician` keeps the UI Host header so an unset
`mobile_access.public_origin` (which can also come from the runtime env) yields
the right `frame-ancestors` origin; a configured public origin wins. The dev
proxy replaces that single HTTP(S) frame ancestor with `'self'` only for
reviewed HTML assets requested with a loopback Host **and** a loopback socket
peer. Other CSP directives stay, tunnel or remote requests and explicit `'none'`
policies keep the backend policy, and adapted responses are private and
uncached. The sandbox and deny-network CSP are never relaxed, and production is
unchanged.

## Session-bound scope (hosted-web CF Access fix)

Hosted web behind Cloudflare Access has no workspace on the identity
(`workspace: None`; the owner picks one via `X-Workspace`), and the frame can
send no headers, so the entry document's GET could not resolve a scope. **The
session carries the minting request's scope.**

- **Mint.** `GET .../host` resolves its normal authenticated scope, and
  `open_host` binds principal and workspace to the live session
  (`AppScriptedSurfaceRuntime.session_scope`) for its lifetime. The binding is
  server-side and never serialized into the host plan.
- **Serve (dual path).** `GET .../assets/{session}/{digest}/{tail}`
  authenticates like every route. Only on the workspace-missing error does the
  session's bound scope apply. The verified principal must equal the minting
  principal (`app_scope_mismatch` otherwise) and any engraved workspace must
  agree; the lent scope then goes through the same scope-checked installation
  and package resolution.
- **Fail-closed.** Unknown, expired, budget-exceeded or torn-down sessions are
  `SessionGone`. No other auth refusal recovers.
- **Bridge unchanged.** `POST .../bridge` still needs the full host envelope.
  Native clients with a bearer use the ordinary identity path; loopback and
  paired-device deployments never take the fallback.

## Bridge

- **Transport order.** Web and iOS hosts send admitted bridge submissions through
  one per-frame FIFO: the server's sequence fence is strict and separate HTTP/2
  or `URLSession` tasks have no order guarantee, so `N+1` waits for `N`. Success
  and ordinary failure advance the FIFO; a session-level refusal drops pending
  work; replies match the request id captured per item.
- **Frame launches are server-minted.** The bridge admits the minimal
  `AppDirectActionRequest` (idempotency key + typed input) and takes the action
  name from the session-bound message. A widget or native host may include the
  closed object `expected_installation_binding: { generation,
  package_revision_ref }` (both required when present, generation positive);
  stale truth returns `409 app_workflow_stale`. An exact retry may recover a run
  started under the same sealed binding. The host resolves installation-bound
  action, schema and grant revisions, scope, policy and provenance just before
  admission. Caller provenance is host-owned and stable across bridge-session
  replacement, so retrying an idempotency key after a frame reopen recovers the
  same task. A full `AppActionInvocation` is still accepted for compatibility;
  frame-supplied mutable authority is not required.
- Each operation's awaited state lives on the heap so the dispatch future fits a
  debug HTTP worker stack; the regression lane caps its size.

Regression lane: `make test-app-scripted-surfaces` (live asset credentials,
revision binding, both production authentication layers).

## Client host wiring

The operator switch stays process-wide on every client.

- **Web.** `AppSurfacePage.svelte` mounts `AppScriptedSurfaceHost` as the first
  fallback after the declarative surface fails, at the installation root and
  every declared route (`?route=` selects the entry point). Plan parsing
  enforces the exact `allow-scripts` sandbox, deny-egress CSP, the
  session-and-digest entry path, and `isAppReference` on `session_ref`.
- **iOS.** `AppRouteBrowserView` probes `custom-surface-v1/host` at an
  installation root (declared MUIJ views are never probed) and mounts a
  `WKWebView` under `magapp-surface://<installation>`; a refused session shows
  the closed failure notice. Viewless enabled installations get Open-app gated on
  `custom_surface_entry_count` (from the admitted manifest, 0 when none). Probe
  refusal keeps the launcher WebView.
- **Desktop.** The unified-ui host plus the desktop origin policy on
  desktop-managed webviews: a scripted frame cannot load from `tauri://`, a dev
  server or any absolute origin, and a widened sandbox constant fails the guard
  closed. See
  [custom-surfaces-v1-origin-policy.md](../desktop/custom-surfaces-v1-origin-policy.md).
- **Android.** Closed in V1: no WebView host; unsupported notice.

## Reference consumer

`brainstorm-canvas` (`magician_data_v3/system/thinking_map/app/`): workflow
`sync_maps` projects `thinking_map_summary` / `thinking_map_snapshot` via
`thinking_maps_data` (`list_maps` + `read_map`); `surfaces/canvas.html` and
`surfaces/canvas.js` render the board as SVG over `query_data`. The map
substrate is never written. A declarative `library` table stays the default
surface; the entry document shows a boot notice until the bridge answers, so a
refused script leaves only the notice.

Tests: `authoring.rs`
(`first_party_brainstorm_canvas_package_admits_and_locks_the_thinking_map_read_port`)
and `custom_surface_review.rs`
(`brainstorm_canvas_reference_package_reviews_its_full_surface_request`).
