# Magdroid Android workspace

Current app version: `0.5.0` (version code `25`). See
[`../CHANGELOG.md`](../CHANGELOG.md).

This release derives its constrained wake grammar from the primary-agent
`wake_spellings`, while display remains Magican.

Native Android client and on-device automation companion for Magician. The
companion uses `AccessibilityService` for structural screen reads, gestures,
text input, app control, notifications, and screenshots.

## Connection model

The phone always dials Magician. It opens the authenticated WebSocket at
`/api/magician/v2/devices/bridge` using its paired device id, token, principal,
workspace, and Cloudflare Access headers. Nothing listens on the handset.

That socket carries MCP `2026-07-28` JSON-RPC text frames. Connection direction
does not change protocol roles: Magdroid is the MCP server and Magician is the
governed client. The handset implements only:

- `server/discover`
- `tools/list`
- `tools/call`
- one socket-scoped `subscriptions/listen` stream for
  `notifications/tools/list_changed`

The roster comes from `McpToolRegistry` and changes with available Android
permissions. Every definition includes conservative MCP safety annotations.
Unsupported protocol families return JSON-RPC `-32601` and malformed, oversized,
or incorrectly typed requests fail closed.

There is no HTTP MCP server, TCP automation server, debug PIN, open device port,
or `adb forward` path in the APK.

## Modules

| module | responsibility |
| --- | --- |
| `:app` | Compose shell: Chat, Today, Tasks, Settings, App Pilot, native navigation |
| `:bridge` | automation engine, outbound MCP connection, repositories, voice and device services |

## Native Apps surfacing

Android consumes the Apps surfacing contract without a browser host. The
bridge-owned `apps/AppSurfacingModels.kt`, `AppSurfacingRepository.kt`, and
`AppSurfacingViewModel.kt` resolve canonical page-qualified slot assignments,
resolve every page through one bounded slot batch/inventory snapshot, and batch
the resulting widget targets through
`POST /api/magician/v2/apps/widgets/render-batch`, retain bodies only across
matching ETags and exact slot-to-target fingerprints, and read materialized
indicators from `GET /api/magician/v2/apps/indicators`. Reads run only while the
application is foregrounded and revalidate on tab arrival. Modified responses
carry a strict RFC3339 `refresh_after`; a 304 preserves the cached body but
replaces its deadline from `X-App-Widget-Refresh-After`. The foreground owner
wakes at the earlier of that deadline or the bounded indicator poll. Every
response body is read through a byte-counted channel ceiling, including
chunked transfers, before UTF-8 decoding or JSON allocation.
One captured non-secret pairing/scope fingerprint fences slot resolution,
rendering, indicators, and publication so mutable credentials cannot combine
two authorities in one refresh. Governed actions use the supported-public
`/actions/{action_id}/runs` route and accept only a receipt whose run handle
matches the exact installation and action. Each widget launch also carries the
exact rendered `generation` and `package_revision_ref` under
`expected_installation_binding`; a package update cannot reinterpret a stale
button and instead causes a closed refresh.

The Compose shell provides a generic `AppWidgetSlotRegion`; Today adopts the
batched `("/", "primary" + "secondary")` page and Observe adopts
`("/observe", "reviews")`. Detail, list, table, timeline, tree, and graph models
render with native Material/MUIJ-adjacent primitives. Widget buttons can launch
only their returned governed action identifier and the repository always sends
the exact empty input object. Indicator materialization remains available to
app-owned surfaces, but the global shell does not mount an indicator chip or
badge. Town Square autonomy therefore stays on Town Square instead of occupying
every Android top bar. Widget failures render a closed native unavailable state.

Typography mirrors all 22 Web theme variants through the same four roles:
Outfit owns the brand, each theme selects its display and body families, and
technical data uses the theme's mono family. The APK bundles those font files,
so switching themes does not depend on a font-network request.

`CustomSurfaceSupport.supported` remains `false`. No WebView, JavaScript bridge,
or mini-frame host was added. A mini-frame can therefore render only a
server-declared native fallback supported by the render contract, otherwise it
is hidden/unavailable.

Android currently consumes assigned slots only. The slot picker/removal flow
and contextual entity fitting are web-only in this landing; adding them to
Compose remains explicit parity work rather than an implied capability.

Key automation code:

| path | responsibility |
| --- | --- |
| `bridge/.../service/MagdroidAccessibilityService.kt` | service lifecycle and automation engines |
| `bridge/.../bridge/MagicianBridgeClient.kt` | authenticated dial-out, reconnect, WebSocket framing |
| `bridge/.../mcp/MagdroidMcpServer.kt` | bounded MCP server and tool-roster subscription |
| `bridge/.../mcp/McpToolRegistry.kt` | authoritative tool names, descriptions, schemas, annotations |
| `bridge/.../mcp/McpToolHandler.kt` | dispatch into the live Android engines |
| `bridge/.../uitree/UiTreeWalker.kt` | structural screen sight |
| `bridge/.../gesture/`, `input/` | taps, swipes, typing, keys, clipboard |
| `bridge/.../screenshot/ScreenshotPipeline.kt` | pixels on demand |
| `bridge/.../notification/NotificationListener.kt` | permission-dependent notification tools |

## Build and test

From the repository root:

```bash
make setup-magdroid-build
make build-magdroid
make test-magdroid
make check-magdroid
```

The Makefile pins JDK 21 and Gradle 8 because newer host defaults are not
compatible with the current Android Gradle Plugin. The targets skip with a clear
message when the Android SDK is unavailable.

To install and cold-start on an attached device:

```bash
make install-magdroid
make run-magdroid
```

## Device setup

1. Open App Pilot and enable the automation service.
2. Grant Accessibility access. Notification access, screen capture, microphone,
   overlay, assistant role, notifications, and unrestricted battery are shown as
   separate state-aware grants.
3. Enter the Magician host and pairing credentials in App Pilot's Connection
   accordion.
4. Keep the foreground service enabled when the phone must remain reachable
   while locked or changing networks.

App Pilot reports connection state, device identity, command latency, grants,
and a bounded activity timeline. It deliberately does not expose a second local
transport or developer authentication surface.

## Security boundary

Pairing admits one exact `(principal, workspace, device_id)` connection. The
device's MCP roster is discovery data, not authority: Apps receive only the
reviewed eight-action owner surface (`snapshot`, `screenshot`, `launch`, `close`,
`tap`, `type`, `key`, `scroll`), and each internal action resolves to its exact
handset tool identifier under a closed private claim. Generic Android/MCP,
device, shell, ADB and raw-provider vocabulary is not projected to Apps.

Tool failures remain distinct from protocol and transport failures so agents do
not retry destructive actions merely because a tap or input reported failure.
The full protocol rationale and verification matrix are in
`docs/archive/plans/2026-08-10-android-mcp-migration.md`.
