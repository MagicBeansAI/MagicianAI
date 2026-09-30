# Native app widgets, slots, and indicators

Magios renders the Apps platform's V1 widget models directly in SwiftUI. The
client uses the enrolled `MobileConnectionProfile` and `MagicianAccess`
authorization path; responses never provide an origin, scope, binder query, or
action input for the client to reinterpret.

`AppsLauncherView.swift` owns the strict wire mirror and renderer. It enforces
the server's 12-widget batch, 32-row, 32-indicator, 1 MiB widget response, JSON
depth, JSON node, text, identity, generation, and ETag limits before publishing
state. Unknown fields and model/state vocabulary fail closed. Each widget and
batch carries a required RFC 3339 `refresh_after`. A `304` is accepted only
when the response ETag exactly matches an existing request-bound representation
and its `X-App-Widget-Refresh-After` header supplies the next valid deadline.
Connection changes clear every cached model synchronously.

The generic `AppNativeSlotRegion(page:region:)` first resolves the canonical
page-qualified slot through the exact single-slot route and then submits a
one-target request to the batch render route. The Swift slot encoding is the
backend's injective `page:<route-hex>:<region>` encoding. Today adopts `/` +
`primary`; Observe adopts `/observe` + `reviews`. Disabled, unavailable,
quarantined, generation-mismatched or digest-mismatched assignments render
empty. An unavailable native response may retain only its request-bound hidden
representation and ETag so its server-provided foreground cadence can recover
without exposing stale UI.

Native renderers cover Detail, List, Table, Timeline, Tree, and Graph. Graph and
tree relationships use only the server-projected structural fields and remain
bounded by the 32-row response ceiling. Governed buttons are shown only from a
ready native model. Their native client method has no input parameter and sends
exactly `{ "input": {} }` to the supported-public action-run route; the server
continues to mint current workflow authority.

The app indicator strip is not mounted in Today or the root tab shell; passive
status stays on the owning app's page. The retained indicator renderer reads
from the bounded materialization endpoint, never evaluates client-side,
and removes values on expiry or any refresh failure. Widgets and indicators
refresh only on view appearance, connection change, app foreground/focus, or
the server-provided widget cadence while the slot remains
foreground-active. The cadence task is cancelled as soon as the view or app is
inactive; Magios introduces no background polling or refresh task.

Mini-frame execution is unsupported. The server may compile
only an exact same-view native fallback while stripping frame capability and
entry-point authority. `AppSurfaceScriptedHostView` has a separately minted
session, navigation lock, bridge budget, and page-level lifecycle; Magios does
not reuse that WebView without a future widget contract that binds page/session
limits, unmount TTL, and visible-frame budgets.

Parity gaps are explicit, not implied by the platform-neutral wire model: iOS
does not batch slot resolution and rendering across a page, does not fit the
home `secondary` or contextual entity slots, has no native assignment
picker/removal UI, and omits global indicator placement.
