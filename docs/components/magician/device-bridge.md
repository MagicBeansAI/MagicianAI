# Device Bridge

The Android companion dials `GET /api/magician/v2/devices/bridge` and holds
the socket; Magician sends work down it. Nothing listens on the device.
Magdroid is `ai.magicbeans.magican` (`versionName` 0.6.2, `versionCode` 29);
Magican Desktop is `ai.magicbeans.magican.desktop`. Magican
and legacy `ai.magicbeans.magdroid` cannot be observation targets.

## Why the device dials out

The companion can read and type into anything, so the phone exposes no port;
Cloudflare carries it off the LAN.

## The protocol is MCP

MCP `2026-07-28` JSON-RPC text frames only. Magician is the governed MCP client,
the phone a tools-only MCP server. The phone declares exactly `server/discover`,
`tools/list`, `tools/call` and one socket-scoped `subscriptions/listen` stream
for `toolsListChanged`, emitting `notifications/tools/list_changed` when Android
permissions change. Resources, prompts, sampling, elicitation, OAuth, Streamable
HTTP and the legacy initialize handshake are not supported.

`magician-mcp-client` runs the discover lifecycle over a bounded duplex-JSON
transport; rmcp owns correlation, discovery, tool IDs, result validation,
subscriptions and cancellation. The WebSocket handler owns only pairing, scope,
framing and socket lifetime.

## What the hub guarantees

- **Authoritative discovery.** Calls use tool IDs from the latest successful
  discovery; an unadvertised name never reaches the dispatcher.
- **Bounded transport and wait.** 64-frame channel per direction, a hard byte
  ceiling per message, one deadline per dispatch covering readiness and call. A
  full or malformed stream is closed.
- **Disconnect fails callers immediately**, as transport failures.
- **A reconnect is a new generation.** A second connection for the same scoped
  device ends the first; a generated connection id keeps the stale socket's
  `stopped` callback from unregistering its replacement. The new connection
  rediscovers and resubscribes; no MCP session resumes.
- **Blast radius is one device.** One phone dropping never disturbs another's
  pending work.
- **Scope is identity.** Devices are keyed by principal, workspace and device
  id; the same id under another principal is unaddressable.

## Testing

The hub takes a `DeviceSink` trait, so a fake phone drives the real rmcp
client without a WebSocket. Subscription precedes the first list, closing the
listen-acknowledgement race.

## Pairing

Only roster clients are admitted; nothing self-registers. The owner starts
enrollment in authenticated Settings: Magician binds a random pending id to that
exact principal/workspace for five minutes, keeps only the secret digest, and
renders the QR SVG itself, recording the requested client kind (iOS, Android
or Desktop Edge) first. iOS and Android get only `mobile_client`, Desktop
Edge only `edge_client`; neither grants Apps automation. The phone validates the
narrow `magican://connect` URI, shows the normalized HTTPS (or
loopback-development) origin, and waits for confirmation before trading the
one-time secret for a durable device token.

- The exchange is atomic and single-use; a wrong secret does not burn the
  enrollment, success, expiry or cancellation does. Pending enrollments are
  memory-only, capped at 64.
- The QR never carries the durable token or Access secret.
- Tokens are BLAKE3 digests compared in constant time; a lost token means
  re-pairing.
- The physically guarded legacy `/devices/pair` route is for ESP terminals: the
  route (not the unauthenticated body) records `client_kind: esp32` and grants only `mobile_client` (no APNs/FCM
  registration, Desktop Edge or Android automation). Older entries recorded as
  Android keep their authority until the owner repairs that ESP connection.

**Origins.** `mobile_access.public_origin` is the remote QR origin;
`MAGICIAN_MOBILE_PUBLIC_ORIGIN` is the machine-local override from Cloudflare
provisioning (`MAGICIAN_DEVICE_PUBLIC_ORIGIN` is a migration fallback). Native
services may also advertise a private Same Wi-Fi origin. The owner picks
`same_wifi` or `remote` (never a browser-supplied address), bound to the
capability so both routes can pair concurrently. Public plaintext, credentials,
paths, queries and fragments are rejected.

**Desktop Edge** reuses the sealed roster, one-time exchange, exact scope,
hashed token, rotation, last-seen and owner revocation. Its client opens
`GET /api/magician/v2/edge/bridge` (iOS and Android credentials refused; the
Android Apps bridge refuses desktop ones) and speaks the separate
`runtime-core::edge` protocol, so a remote engine can address a named desktop
without its browser, CUA or OS credentials. The Settings WebView creates the
one-time desktop ticket; Tauri exchanges it, keeps the desktop token and
optional outer Access credential in the OS keyring and runs the
heartbeat/reconnect loop; it advertises CUA, Magicutor/CDP health, discovery and
command operations, plus read-only Messages queries on macOS only (Linux and
Windows manifests never list iMessage). Availability changes rotate capability
generations; an invocation must match the exact live device, operation,
capability generation and server-minted limits.

### Android Apps action review

Pairing does not let an App observe a phone; Android Apps uses a separate
owner-initiated `magican://apps-connect` enrollment. App-side grant and
per-action fences: [App interactive capabilities](app-interactive-capabilities.md).

**Desktop trust.** Native Settings pins the desktop's Keychain-backed Ed25519
identity (runtime nonce, displayed fingerprint, one-time owner code,
challenge-bound attestation) over a private owner-only Unix-domain socket, never
HTTP, at
`~/Library/Application Support/Magican/app-android-owner-v1/bootstrap-v1.sock`
(short enough for `sun_path`; only this path is stable across bundle-id changes,
owner data stays in the bundle-specific app-data root). Both sides check the peer's audit-token Apple code
identity and shared Team ID before any frame; socket owner, mode, path, device
and inode are fenced through bind, connect and peer check. Unsigned dev builds,
non-macOS hosts, symlinked paths and unverifiable peers leave Android Apps typed
unavailable without blocking unrelated features; the bootstrap is independent of
macOS target pairing. Signed completions are kept durably for byte-identical
retry; an expired request that minted nothing clears via `ExpiredUnspent`.
Owner-control requests fail until the pin is durable and reconciled with the
real desktop in the current process.

**Handset attestation.** Magdroid creates a P-256 Keystore key with that
challenge. The backend verifies a bounded X.509 path in an isolated OpenSSL
store holding only the configured Android attestation root (CA, key-cert-sign,
path-length, validity, self-signature and critical-extension rules; partial,
alternate, injected and system-trust paths rejected; a missing Authority Key
Identifier is tolerated), parses Key Attestation from that leaf only, and
requires TEE or StrongBox, locked verified boot, the exact package and APK
signing fingerprint, and an exact enrollment signature.
`mobile_access.android_apps_version_codes` is the exact version allowlist (no
compiled exception). The base-APK SHA-256 (splits rejected) is diagnostic only;
private builds hash PackageManager's base-APK path directly (vendor checksum
callbacks never complete for ADB installs); Play enrollment stays
PackageManager-only.

**Two authority choices** in web Settings:

- **Google Play release**: a Google-decoded Play Integrity standard token whose
  requestHash binds the hardware-signed enrollment; fresh, `PLAY_RECOGNIZED`,
  matching package, exact version/certificate set and device-integrity verdict.
- **Private / self-hosted build**: no Google call; the deployment's exact signer,
  app-version and attestation-root pins.

Empty common pins fail both closed; a missing Play service account disables only
Play. Both stage the same signed `EnrollAttestedDevice` owner proposal (with the
canonical `blake3:`-prefixed automation identity digest) before any
`device_automation` credential exists; proposals live at most two minutes (the
scan ticket lives longer). The
desktop picks **Same Wi-Fi** or **Remote** for the App Pilot QR, mapped to a
configured origin; the handset keeps the scanned origin and no response can
retarget it. An APK change after enrollment shows **Re-enroll**.

**Every reconnect** signs a fresh server nonce, socket UUID, opaque target,
review generation and MCP protocol with the pinned handset key. Play binds a
fresh Integrity token to that proof and decodes it before hub admission; private
requires an empty Play token and derives its verdict from the signed nonce and
reviewed private policy. The verdict digest stays with the socket generation and
audit; stable completion identity binds the normalized reviewed policy, not a
timestamped verdict.
A legacy bearer or self-asserted MCP identity is not enough. Roster and hub have
file, global, per-scope and live-connection ceilings.

**The owner review** approves exactly the canonical ordered eight actions
(`snapshot`, `screenshot`, `launch`, `close`, `tap`, `type`, `key`, `scroll`) and
exact target package ids; subsets and generic Android/MCP actions are not
reviewable. `ai.magicbeans.magican` and legacy `ai.magicbeans.magdroid` are
rejected as targets. Raw-device-id PUT/DELETE compatibility handlers are
unmounted and hard denied.

**Signed writes.** The Svelte surface does no HTTP and never sees a signer,
public key, attestation or reusable capability. Eight closed Tauri commands
(enrollment begin/cancel, target listing, review proposal, pending listing,
receipt submission, recovery) sign fresh Ed25519 requests over operation, body
digest, nonce, pinned desktop identity and short lifetime; the runtime consumes
each nonce once and derives scope itself. The handset exchange stays
`owner_approval_pending` until Settings signs the displayed proposal and the
runtime durably applies that receipt, before publishing the device credential.
The handset's proof precommits a connection-secret digest; the secret never
leaves it and is never written to an owner document or the roster. Exact retries reconcile both
crash windows (receipt-before-roster, roster-before-pending-clear) without
minting authority.

**Handset retry.** Before its first exchange write the handset seals its
enrollment state (origin, digests, QR correlation, Keystore key, APK identity,
signed request, connection secret) in non-backup encrypted storage. A retry
rotates only the Play token and body digest over the same requestHash, never the
key, proof or connection secret; private-build retries keep the absent Play
project number so the request digest matches. The record clears only in the
transaction that publishes the credential after `/devices/me` verifies it, or
once a Gone proves it unspent; a new App Pilot QR mid-retry returns to the
retained request rather than replacing a possibly submitted key. The desktop accepts success only if generation, receipt
digest, target and review generation match its receipt. Approval and revocation
invalidate the old hub generation before publishing the new owner head.

**Receipts and recovery.** The native owner shows every signed field and a
display digest before signing. Receipts form a generation/predecessor chain in a
private bounded file plus a Keychain pending/committed high-water that never
lowers. Routine status is nonce-bound and record-free; full-record recovery
needs a separately confirmed digest. If the runtime
authority document is lost, the desktop refuses a generation-zero bootstrap and
offers a rebind (same Keychain identity, previous bootstrap digest, preserved
owner generation and receipt head) that Settings confirms with a new owner code
before the UDS accepts `RecoveryHello`; actions stay denied until the owner
imports the signed full-record snapshot.

**The stored review** (opaque target, action roster, sorted packages, monotonic
generation, digest, time) is fsynced and renamed before the protected high-water
advances. Startup accepts only the exact anchor or one verified forward
generation (an ambiguous anchor write is reread for exact generation and seal
equality), else the in-process owner latches unavailable until reopen. Updates
carry the generation the owner saw; stale ones fail.
Revoke persists a keyless
tombstone and leaves mobile access paired; re-pairing rotates the target, bumps
the generation and clears the review. A workflow resolves exactly one active
review and rereads target, generation and digest at its final pre-start fence;
the hub checks the socket generation before and after the call. V1 offers only
eight-action approval and review revocation; whole-device revocation is not
shown and the desktop refuses to sign it until its roster deletion and recovery
lifecycle exist.

**Redaction and results.** Password, secure and editable accessibility nodes and
their subtrees are redacted before any text, selector-bearing field, element
identity, digest or result byte is built; only roles, interaction flags and
geometry remain. Screenshot is capped at 8 MiB plus fixed protocol overhead,
other results lower; fragmentation and overflow are rejected. The effect
identity excludes the socket UUID, so a completed result recovers after
reconnect.

**Wire mapping.** Only the private Apps owner lowering sends the package
allowlist; public App arguments cannot name a device, package, raw coordinate or
MCP verb. The handset accepts `_magician_apps` only on
`android_get_ui_tree`/snapshot, `android_screenshot`/screenshot,
`android_launch_app`/launch, `android_close_app`/close, `android_tap`/tap,
`android_input_text`/type, `android_press_key`/key and `android_swipe`/scroll,
with closed arguments. Snapshot freezes package and geometry across capture;
screenshot and lifecycle check the target before and after I/O; tap, type, key
and scroll revalidate foreground package, geometry and snapshot SHA-256 just
before I/O. Success returns a uniform `structuredContent.apps_owner` receipt
(nullable target/foreground/observation/geometry fields, bounded evidence
counts, `outcome: settled`, lowercase SHA-256 of the returned text or JPEG bytes),
which the runtime parses deny-unknown and re-hashes before an App sees it.

### Mobile exchange and Access

The legacy mobile exchange route is a narrow Cloudflare Access bypass; its
five-minute 256-bit single-use capability is the authority. It returns the outer
Access pair plus the device token, committed atomically (iOS one Keychain
profile, Android one encrypted-preference transaction) after a `/devices/me`
probe through normal Access. Native requests present device id and token; middleware derives
principal/workspace from the roster, never headers. An outer Access credential
without a device identity is rejected. Revoking the roster entry ends mobile API
use and Android automation together. The Android Apps exchange returns only
`owner_approval_pending` or `active`, never a bearer token.

Provisioning writes the Access audience and issuer to `MAGICIAN_CF_ACCESS_AUD`
and `MAGICIAN_CF_ACCESS_TEAM_DOMAIN`. Assertions for the dev UI host's Access
app (`ui-<machine>.<zone>`, which proxies `/api` here) are accepted too when its
audience is in `MAGICIAN_CF_ACCESS_UI_AUD` (written by `ensure-ui-access.sh`);
without it every sign-in through that host gets 401. A service-token `common_name` never maps
to a user scope; only an interactive human or the paired-device branch can.

Presence refreshes at most once a minute; a failed presence write is only
logged and never changes a valid authorization. "No such device" and "wrong
token" return the same error; failures do not touch `last_seen`. Unpair ends the
current socket generation at once, even if the roster write fails (authority
then stays unchanged, but the handset must open a fresh socket generation);
re-pairing ends the old generation once the new token is committed.

### At-a-glance and remote delivery

`PUT /api/magician/v2/devices/me/push` registers a route under the
paired-device credential; `DELETE` removes one. The body carries only platform,
registration kind, provider token, environment and optional task id; scope and
device come from middleware, and APNs must match an iOS entry, FCM an Android
one. Responses return an opaque monotonic route revision (never the token) that
`DELETE` can pass to remove only that generation. Absent platform credentials
return `503 mobile_push_provider_not_configured`; clients keep widget polling.

Routes live in private `system/mobile-push-registrations.json` (2,048 rows, 16 per device; atomic
replace, `0600` on Unix and repaired to it on open), not the roster, and are
removed on unpair (idempotently, even when the row is already gone).
Task-bound routes (ActivityKit, Android ongoing notification) are removed after
the terminal update; overflow evicts the oldest ephemeral route, never the
application route.

**What is sent.** Only `HitlRequested`, `HitlResolved` and
`ExecutionPanelDelta`. No HITL prompt or secret reaches a lock screen: Attention
gets a generic alert and correlation deep link, task progress bounded
title/status and the task link. Event timestamps ride along, so late delivery
cannot roll a card back.

**Rate and priority.** At most eight provider requests at once. One
non-terminal update per execution per second; terminal state bypasses it and
leaves a watermark rejecting late progress. Android keeps the canonical
millisecond event time; ActivityKit gets a separate strictly increasing
seconds-level timestamp. Progress goes at background
priority; Needs You and terminal state go immediately. iOS
non-terminal updates carry a 20-minute stale date. If ActivityKit cannot create
the surface, dispatch continues with no hidden audio or socket keepalive.

**Coding runs.** Two scope-wide kinds without task id: `coding_runs` (iPhone
Live Activity update token, or Android FCM token for the "Coding runs"
notification) and `coding_runs_start` (iPhone push-to-start).
`vibedev::runs_push` recomputes a scope's summary at most every four seconds
after a run-state event (and every two minutes) and, if changed, calls
`MobilePushDispatcher::deliver_coding_runs`: ActivityKit `update`, or `end` when
idle (the route is dropped once the end is accepted); push-to-start to iPhones without an activity route when a run just
started; an FCM data message (`kind=coding_runs`, counts, headline, detail,
`active`) to Android. Content-state keys are the Swift
`CodingRunsActivityAttributes.ContentState` property names.

A failed device retries alone with backoff (up to five minutes apart) until it
lands, the summary changes or the route is gone; a changed summary goes to every
device at once. An owed
start is never held back and no route gets two. An unchanged active summary is
re-sent every ten minutes (`stale-date` twenty minutes out), and each scope gets
one after a restart. Push-to-start or a rise in needs-you is `urgent` (APNs
priority 10, FCM high); routine changes go at 5 / normal. Each project
contributes its newest 150 runs, active run, open crews' lanes and runs waiting
on the owner.

**Magician at work.** The coding-runs routes now carry the owner's whole
long-running work (`work_live::scope_work_live`): coding runs, crews (one item
each) and Work tasks, ranked by tier (needs the owner, longest waiting first;
followed or started from a phone; just failed; working, owner-started first;
just done), the featured item held five minutes unless something needs the
owner or fails. Ready tasks appear only for outstanding owner actions.
The payload keeps every key above and adds `featured_*`,
`action_*`, `more` and `next` (content-state `featuredKind`, `featuredState`,
`featuredTitle`, `featuredContext`, `featuredStep`, `actionKind`, `actionId`,
`deepLink`, `more`, `next`); push-to-start titles it "Magician at work", and a
new failure is urgent too. A third kind, `work_item`, follows one item: its
`task_id` carries the item id (`run:…`, `crew:…`, `task:…`), the phone
registers it on Follow (the per-item activity's token, or Android's FCM token),
and the pusher sends `kind=work_item` updates when the item changes
(ActivityKit `FollowedItemActivityAttributes`), then `end` / `done=true` once it
ends, after which the route is dropped.

**Pruning.** Provider-invalid tokens are pruned in one durable batch per wave,
compare-and-delete on token and (for terminal cleanup, since Android reuses one
FCM installation id across task routes) the strictly monotonic registration
revision, which both clients keep for teardown. A provider-auth rejection discards only that cached
bearer. Provider bodies and APNs URLs (which hold the device token) never reach
logs.

**Credentials.** APNs: `MAGICIAN_APNS_KEY_ID`, `MAGICIAN_APNS_TEAM_ID`,
`MAGICIAN_APNS_PRIVATE_KEY_PATH`, optional `MAGICIAN_APNS_TOPIC`. FCM:
`MAGICIAN_FCM_SERVICE_ACCOUNT_PATH`, from the Firebase project the Android app
was built with.

**Roster durability.** `DevicePairingStore::open` refuses a truncated file and
the daemon will not start over it. Writes use one writer, a unique temp name
(never a fixed `paired-devices.json.tmp`, since `verify` persists on every
connection), fsync before rename, directory fsync after, temp removal on failure.

### Push at scale and hosted (SaaS) deployments

Today's design fits one Magician and a few devices: one route file capped at
2,048 rows (16 per device), in-process sending, one provider request per
registered device (fan-out is APNs/FCM/web push's job). Pushes are content-free: the
device fetches the ask from its own Magician, so question text never passes
through Apple, Google or a relay.

The first limit is credentials: the publisher's keys (`MAGICIAN_APNS_*`,
`MAGICIAN_FCM_SERVICE_ACCOUNT_PATH`) must not ship to every instance. For
distribution or SaaS, use **a central push relay run by the publisher**: it
alone holds the APNs, FCM and web push VAPID keys, authenticates and rate-limits
each instance, accepts only content-free sends (device route, correlation id,
kind, expiry, answer mode, option count), returns provider rejections for
pruning and records only delivery metadata. Each instance authenticates with its
own credential, never a provider key. A managed service (FCM, which accepts an
APNs key for iOS, OneSignal, AWS SNS) can replace only its sending half, with
vendor templating, analytics and storage off; keep the iOS Notification Service
Extension fetch, since vendor "rich push" would put content on the vendor path.

For multi-tenant Magician, replace the in-process pieces first:

| Today | At scale |
|---|---|
| `system/mobile-push-registrations.json`, rewritten whole | Table indexed by principal, workspace, device; route revisions kept |
| Sending inline from the event bus | Durable queue, stateless sender workers |
| Eight in-flight provider requests | Pooled HTTP/2 to APNs, cached JWT, FCM quota management (~600k/min per project by default, raisable) |
| Prune on provider rejection | Same, plus backoff, idempotent sends keyed by correlation id, cross-worker dedupe |
| Per-instance web push VAPID key | One VAPID key per relay; one encrypted request per browser subscription |

HITL asks are one-to-one (no broadcast herd): budget about one fetch per pushed
ask per device.

## The agent-facing verbs

Five tools: `android_snapshot`, `android_act`, `android_screenshot`,
`android_app` and read-only `android_notifications`. `android_act` takes tap,
long_press, double_tap, type, swipe, drag, pinch, key (back, home, enter),
open_url, set_clipboard, system (recents, notifications, quick_settings),
wait_for, wait_gone, wait_idle and scroll_to, each mapped to a handset tool
(`android_double_tap`, `android_drag`, `android_open_url`,
`android_global_action`, `android_wait_for_element`, …) that must resolve in the
current roster. There is no `android_tools` pack; discovery happens inside the
governed device session and only these compiled verbs reach the model. Sight is
structural first: the snapshot is the default read, a screenshot the exception.
With one phone no device is named; with several and none named, the call is
refused, not guessed. Errors separate retryable (phone asleep) from not (device
ran and refused).

**Verification codes are withheld from every screen read.** `OtpWatcher` at the
companion's shared tool seam makes `android_get_ui_tree`,
`android_find_elements`, `android_get_screen_context` and the waiting tools
return a withheld marker with `verification_text_withheld`, judging content line
by line (a heads-up banner can overlay any app). It is redaction, not refusal.
`FOREGROUND_GATED` does not cover this: it refuses only while a protected app is
foregrounded, and the shade is SystemUI. **`android_screenshot` is the
exception**: pixels cannot be scrubbed.

`android_notifications` reads the shade via `android_get_notifications` under its
own `NOTIFICATION_FILTERED` disposition, so a paired, attested phone offers it
without action approval. Code notifications arrive as markers counted in
`withheld_count`, never digits; codes reach a run only through the custody lane
and never the model (secure HITL P6). It is the
sanctioned shade door (not `android_snapshot` or `android_screenshot`), per the
pack guide and `android-operator`. Its pack is deliberately **not** categorised
`android`, which marks Apps device-owner primitives that must resolve to
reviewed eight-action leaves; an unresolvable pack makes the compiled catalog
report `ResolverUnavailable`. Non-primitive verbs take ordinary descriptive
categories.

## Governance

**Protected apps are enforced on the device**, at the dispatch seam every tool
passes (including notification and OTP tools the five verbs do not expose).
While one is foregrounded, observe and act tools return a success-shaped
`app_protected` refusal (`isError` flattened), which Magician maps to a non-retryable `app_protected`
error; notification tools drop protected sources. The list (seeded with
authenticators) is owner-managed under Settings > Privacy. Checks repeat per
tree walk, poll and capture; durations are clamped. `back`/`home` stay available; `open_url` refuses intents resolving
to a protected package; agent recents or shade opening is refused while anything
is protected. The sealed Apps snapshot refuses SystemUI, the launcher/recents
owner, non-application and unknown windows, and resamples package and window
class before releasing evidence.

**The blanket screenshot policy is enforced in Magician** before any round trip,
by `device_governance::DevicePolicyStore` in `system/device-policy.json`, apart
from the sealed roster (it governs legacy screenshot actions, not the Apps
authority chain). Default `allow_unless_protected`; `block_all` refuses
`android_screenshot` deployment-wide. `GET`/`PUT /api/magician/v2/devices/policy`.
A missing file is the default; an unreadable or corrupt one logs a warning and
blocks all screenshots (`block_all`) until the owner writes a valid policy. The
per-app gate holds either way.

**Every device action is audited durably** by
`device_governance::DeviceActionAudit` to append-only, per-record-fsynced
`system/device-actions.jsonl`; past ~4 MiB the newest 10k records are kept. One
record per dispatch and per refusal past argument validation (protected-app,
policy-blocked, bridge-unavailable, unresolved-device): tool, wire action,
device, device-stamped foreground package from `structuredContent`, verdict
(`ok` / `app_protected` / `policy_blocked` / `error`), and whether pixels left
the device. `GET /api/magician/v2/devices/audit` reads newest first, caller
scoped; torn tails and corrupt lines are skipped and counted in
`global_corrupt_lines` and `global_torn_tail_bytes` (deployment-global). Legacy
verbs audit best-effort and warn on failure. The sealed Apps snapshot must
append durably after I/O before returning `Completed`; writer failure,
cancellation or deadline drift settles `OutcomeUncertain` with no result bytes.

**The sealed roster** is a versioned authenticated document plus a monotonic
platform anchor. A corrupt roster or unavailable seal owner disables Apps
pairing and bridge authority (fail-closed) without stopping Magician, never
falling back to unsigned JSON or an in-memory key. A legacy owner-only roster upgrades once, at
anchor zero: mobile bearer digests are kept, all Android automation authority
(capability, identity, review, generation) is stripped before sealing generation
one, so Apps authority always needs a new signed enrollment and owner review.
Exact forward-recovery rules cover the publish/anchor crash window. An unsigned document below a non-zero anchor is refused as rollback.

While the seal owner is unavailable no QR is minted. Exchange serializes one
ticket through roster publication and removes it only after success, so storage
or key failure returns 503, not "expired or already used"; 404 means the binary
lacks the enrollment route (desktop/iOS clients tell the owner to rebuild). Missing common signer, version or attestation pins, or no
desktop trust, disable both Android choices; a missing Play project, release
verdicts or service credentials disable only Play. Mutation, screenshot,
app-lifecycle, raw-device, coordinate and general MCP stay outside the admitted
descriptor.
