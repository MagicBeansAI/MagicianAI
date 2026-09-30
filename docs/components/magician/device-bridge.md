# Device Bridge

Magician's side of a connection that a device opens and keeps open. The
Android companion dials `GET /api/magician/v2/devices/bridge` and holds the
socket. Magician sends work down it and correlates the answers. Nothing
listens on the device. Magdroid is `ai.magicbeans.magican` (`versionName`
0.5.0, `versionCode` 25). Magican Desktop is
`ai.magicbeans.magican.desktop`. Magican and legacy
`ai.magicbeans.magdroid` cannot be observation targets.

## Why the device dials out

The companion can read the screen and type into anything on it. There is no
port on the phone to reach; Cloudflare is how it reaches Magician off the LAN.

## The protocol is MCP

The accepted WebSocket carries MCP `2026-07-28` JSON-RPC text frames only.
Connection direction does not change protocol roles: Magician is the governed
MCP client and the phone is a tools-only MCP server even though the phone opened
the socket.

The phone declares exactly four request methods: `server/discover`,
`tools/list`, `tools/call`, and one socket-scoped `subscriptions/listen` stream
for `toolsListChanged`. It emits the request-bound
`notifications/tools/list_changed` invalidation when Android permissions change.
Resources, prompts, sampling, elicitation, OAuth, Streamable HTTP, and the legacy
initialize handshake are not advertised or accepted.

Magician uses `magician-mcp-client` over its bounded duplex-JSON transport. That
crate explicitly selects the discover lifecycle and keeps rmcp responsible for
JSON-RPC correlation, protocol metadata, discovery, tool IDs, result validation,
subscriptions, and cancellation. The WebSocket handler owns only pairing, scope,
framing, and socket lifetime.

## What the hub guarantees

**Authoritative discovery.** Calls use tool identifiers minted from the latest
successful rmcp discovery. A model-selected name that the current handset does
not advertise cannot bypass discovery and reach the dispatcher.

**Bounded transport and wait.** Each direction has a 64-frame channel, raw MCP
messages have a hard byte ceiling, and every public dispatch takes one deadline
covering readiness plus the tool call. A full or malformed stream is closed
rather than buffered without bound.

**Disconnect fails callers immediately.** rmcp owns each request lifecycle. A
dropped transport resolves its pending calls as transport failures rather than
leaving product-level correlation entries behind.

**A reconnect is a new generation.** A second connection for the same scoped
device terminates the first. The actor's generated connection id fences cleanup,
so the stale socket's eventual `stopped` callback cannot unregister its
replacement. The new connection rediscovers the roster and resubscribes; no
durable MCP session is resumed.

**Blast radius is one device.** Pending requests remember which device they went
to, so one phone disconnecting never disturbs another's in-flight work.

**Scope is identity.** A device is keyed by principal, workspace and device id.
The same device id under a different principal is a different device and is not
addressable.

## Testing

The hub takes a `DeviceSink` trait rather than a concrete socket; its fake phone
answers the exact MCP discovery/subscription/list/call sequence, so tests drive
the real rmcp client without a WebSocket. Subscription precedes the initial
list so the first snapshot closes the listen-acknowledgement race.

## Pairing

A native client is admitted only if it is on the roster. Normal enrollment begins in
the owner's authenticated Settings surface: Magician binds a random pending id
to that exact principal/workspace for five minutes and renders a QR locally.
Only the secret digest is kept server-side. Magician records whether the owner
requested iOS, Android, or Desktop Edge before producing the enrollment
capability. iOS and Android receive only `mobile_client`; Desktop Edge receives
only `edge_client`; neither flow can grant Apps automation. The phone validates
the narrow `magican://connect` URI, shows the normalized HTTPS (or
loopback-development) origin,
and waits for explicit confirmation before exchanging the one-time secret for a
durable device token. Nothing self-registers — the owner creates the enrollment
and confirms its destination, while the companion can only consume that bounded
capability.

The physically guarded legacy `/devices/pair` bootstrap is reserved for ESP
terminals. The route, rather than its unauthenticated request body, records
`client_kind: esp32` and grants only `mobile_client`. ESP credentials cannot
register APNs/FCM tokens, open Desktop Edge, or acquire Android automation
authority. Older roster entries that were recorded as Android keep their
existing authority until the owner explicitly repairs that ESP connection.

The exchange is atomic and single-use. A wrong secret does not burn the real
owner's pending enrollment; success, expiry, or cancellation does. Pending
enrollments are memory-only, capped at 64. The QR carries the server-owned
runtime origin and ephemeral capability, never the durable token or Access
secret; the SVG is generated inside Magician.

`mobile_access.public_origin` is the remote deployment origin advertised in the QR.
`MAGICIAN_MOBILE_PUBLIC_ORIGIN` is the machine-local override written by the
Cloudflare provisioning path, with `MAGICIAN_DEVICE_PUBLIC_ORIGIN` retained as a
migration fallback. Native services may also advertise a private Same Wi-Fi
origin. The owner chooses `same_wifi` or `remote`; the browser never supplies an
address, and the chosen origin is retained with the one-time capability so both
routes can be paired concurrently. Public plaintext, credentials, paths,
queries, and fragments are rejected.

Tokens are stored as BLAKE3 digests, never in the clear, and compared in constant
time. An owner who loses one re-pairs rather than recovers it.

Desktop Edge reuses the same sealed roster, one-time exchange, exact scope,
hashed token, rotation, last-seen, and owner revocation. Its native client opens
`GET /api/magician/v2/edge/bridge`; iOS and Android credentials are refused at
that route, and desktop credentials are refused by the Android Apps bridge.
The Edge socket carries the separate `runtime-core::edge` protocol rather than
mobile MCP. A remote engine can therefore address a named desktop without
receiving that desktop's browser, CUA, or operating-system credentials. The
authenticated Settings WebView creates the one-time desktop ticket; Tauri
exchanges it, retains the desktop token and optional outer Access credential in
the OS keyring, and maintains the outbound heartbeat/reconnect loop. Its typed
local dispatcher advertises the installed CUA operation list, bounded
Magicutor/CDP health, discovery and command operations, plus read-only Messages
queries only on macOS. Linux and Windows manifests never contain iMessage.
Availability changes rotate capability generations; an invocation is accepted
only for the exact live device, operation, generation and server-minted limits.

### Android Apps action review

Pairing an Android device does not by itself let an App observe it. Android Apps
uses a separate owner-initiated `magican://apps-connect` enrollment. The bundled
native Settings surface first pins the desktop's Keychain-backed Ed25519
identity through a runtime nonce, displayed fingerprint and one-time owner code,
and a challenge-bound attestation. This first trust exchange never uses HTTP:
the signed runtime connects to a private owner-only Unix-domain socket, and both
processes validate the peer's audit-token-selected live and static Apple code
identity, expected identifier, and shared Team ID before exchanging any frame.
Socket owner, mode, path, device, and inode are fenced across bind/connect and
the final peer check. Unsigned development builds, non-macOS hosts, symlinked
paths, and unverifiable peers leave Android Apps typed unavailable without
blocking unrelated runtime features. This bootstrap is independent of macOS
target pairing. A signed completion and finalized digest acknowledgment are
durably retained for byte-identical retry; an exact expired request that never
minted an attestation is cleared through the correlated `ExpiredUnspent`
response. The runtime rejects Android owner-control requests until the pin is
durable and reconciled with the real desktop in the current process.

The reviewed Magdroid package creates a P-256 Android Keystore key with that
attestation challenge. The backend verifies a bounded X.509 path in an isolated
OpenSSL trust store containing only the exact configured Android attestation
root. Path validation enforces CA/basic-constraints, key-cert-sign,
path-length, validity, self-signature, and critical-extension rules; partial,
alternate, injected, and system-trust paths are rejected. Android Key
Attestation fields are then parsed only from that validated end-entity
certificate. The verifier additionally requires TEE or StrongBox security,
locked verified boot, the exact reviewed package and APK signing fingerprint,
and an exact enrollment signature. Android attestation leaves may omit the
generic X.509 Authority Key Identifier; the exact chain, pinned root,
signatures, validity, CA constraints, and critical extensions remain verified.
`mobile_access.android_apps_version_codes`
owns the exact version allowlist; there is no compiled version exception. The
local whole-base-APK SHA-256 (with installed splits rejected) is diagnostic
only. Owner-pinned private builds hash PackageManager's exact installed base-APK
path directly, avoiding vendor checksum callbacks that never complete for
ADB-installed packages. Google Play enrollment remains PackageManager-only.
The handset shows whether it is creating the hardware identity, contacting
Magician, or waiting for desktop approval.

Web Settings offers two explicit authority choices. **Google
Play release** adds a Google-server-decoded Play Integrity standard token whose
requestHash binds the hardware-key-signed enrollment; the decoded result must
be fresh, `PLAY_RECOGNIZED`, and match the configured package, exact
version/certificate set, and required device-integrity verdict. **Private /
self-hosted build** omits the Google call and instead relies on the deployment's
exact signer, app-version, and Android attestation-root pins. Both paths stage
the same signed `EnrollAttestedDevice` owner proposal, including the canonical
`blake3:`-prefixed automation identity digest, before publishing any
`device_automation` credential. Owner proposals use the contract's shared
two-minute maximum lifetime even though the scan ticket remains valid longer.
The handset always verifies and stores the origin from the scanned App Pilot
link. A completed response cannot retarget it to the deployment's configured
remote origin; this keeps Same Wi-Fi enrollment on the reviewed local route.
The desktop keeps the optional per-phone action and target-package review
collapsed until the owner opens it. If a development reinstall changes the
APK after enrollment, the phone reports **Re-enroll** instead of waiting at a
generic connecting state; a fresh attested enrollment binds the new APK.
Empty common pins fail both paths closed; a
missing Play service account disables only the Play choice.

Every bridge reconnect receives a fresh server nonce and must sign the exact
socket UUID, opaque target, review generation and MCP protocol with the pinned
handset key. The Play choice binds a fresh standard Play Integrity token to
that complete socket proof and decodes it before hub admission. The private
choice requires an empty Play token and derives a session verdict from the
same signed nonce plus the reviewed private policy. The verdict digest
stays with the live socket generation and durable action audit/evidence, while
stable completion identity binds the normalized reviewed Play policy rather
than an ephemeral timestamped verdict. A legacy bearer or self-asserted MCP
identity is insufficient. The roster and hub have explicit file, global,
per-scope and live-connection ceilings.

In Settings, the owner must separately approve the canonical ordered roster of
exactly eight actions—`snapshot`, `screenshot`, `launch`, `close`, `tap`,
`type`, `key`, and `scroll`—and enter the exact Android package ids they may
target. Subsets and generic Android/MCP actions are not reviewable. The
attested `ai.magicbeans.magican` owner package is displayed as device-owner
identity, never inserted into that target allowlist, and is rejected if
requested as a target (legacy `ai.magicbeans.magdroid` is rejected the same
way). The general raw-device-id PUT/DELETE compatibility handlers are unmounted
and hard denied. The Svelte surface performs no HTTP and receives no signer,
public key, attestation, or reusable capability. After the authenticated socket
bootstrap, eight closed Tauri commands construct fresh Ed25519-signed requests
for enrollment begin/cancel, target listing, review proposal, pending listing,
receipt submission and explicit recovery. Each request binds the exact
operation, typed body digest, random nonce, pinned desktop identity and short
lifetime; the runtime consumes the nonce once and derives scope internally.
The attested handset exchange remains `owner_approval_pending` until Settings
signs the displayed `EnrollAttestedDevice` proposal. The runtime durably applies
that exact receipt before publishing the device credential. The handset's
hardware-key proof precommits a locally generated high-entropy connection-secret
digest; plaintext stays on the handset and is never returned by the runtime or
written to either owner document or paired roster. Exact retries reconcile both
crash windows—receipt-before-roster and roster-before-pending-clear—without
minting replacement authority.

The reciprocal macOS bootstrap socket uses the shared short path
`~/Library/Application Support/Magican/app-android-owner-v1/bootstrap-v1.sock`.
Desktop owner data remains in the bundle-specific app-data root; only the socket
location is stable across bundle-identifier changes and bounded for `sun_path`.

Before its first exchange write, the handset durably retains origin,
enrollment/request digests, QR correlation, Keystore alias/key id, APK
identity, signed request bytes and connection secret in non-backup encrypted
storage. Private-build retries preserve the absence of a Play project number so
the reconstructed link has the same request digest.
Each retry rotates only the opaque Play token and sealed request-body
digest over the same requestHash; it never regenerates the hardware key,
proof, or connection secret. The record is cleared only in the same durable
preference transaction that publishes the credential, after `/devices/me` has
verified it, or after an explicit Gone proves the request unspent. The desktop
accepts success only when generation, receipt digest, target and review
generation exactly acknowledge its retained receipt. For action-roster
approval and revocation, the runtime advances the durable roster and
invalidates the old hub generation before publishing the desktop receipt as
its new owner head.

If the owner scans a different App Pilot QR while an exact retry remains, the
phone switches back to that retained request and explains that it must be
reconciled first. It never replaces a possibly submitted key with the newly
scanned capability.

The trusted desktop approval also selects **Same Wi-Fi** or **Remote** before it
creates the App Pilot QR. Magician resolves that enum to its configured local or
public origin; neither the desktop webview nor the phone can inject an address.

The native owner displays every scope, target, identity, roster, allowed
package, generation, expiry and canonical display digest before signing.
Receipts persist under a global generation/predecessor chain using a private
bounded file and Keychain pending/committed high-water transaction. Routine
status is nonce-bound and record-free. Full-record recovery requires a
separate displayed digest and explicit confirmation.

If the runtime authority document is lost while the desktop's owner store
survives, the desktop does not silently accept a generation-zero bootstrap.
An unknown fresh nonce receives a rebind offer (same Keychain identity,
previous finalized bootstrap digest, new runtime nonce, preserved owner
generation/latest-receipt head). Settings must confirm the offer's canonical
display digest and the new one-time owner code before the UDS accepts
`RecoveryHello`. A nonzero desktop head keeps runtime actions denied until
the owner imports the signed full-record recovery snapshot; an empty desktop
head cannot emit such a snapshot. Neither path lowers the desktop high-water.

The roster stores an exact eight-action review containing a credential-derived
opaque target, canonical action roster, sorted package set, monotonic generation,
digest, and review time. Its bounded sealed document is fsynced and renamed
before advancing the protected high-water. Startup accepts only the exact
anchor or one verified forward generation; an ambiguous anchor-write result is
reread for exact generation and seal equality, otherwise the in-process owner
is latched unavailable until reopen so the recoverable forward document cannot
be overwritten. The Settings/API update supplies the generation the owner saw;
stale writes are rejected.

Review revoke advances and persists a keyless tombstone generation while
leaving normal mobile access paired. Re-pairing rotates the credential-derived
opaque target, increments the generation and clears the review. A workflow must
resolve exactly one active review in its principal/workspace, and its final
pre-start fence reopens the roster and compares the target, generation and
digest. The physical owner separately binds the current authenticated socket
UUID and the hub checks that generation before and after the bounded call.
V1 exposes only exact eight-action approval and action-review revocation.
Whole-device revocation is not shown by Settings and the desktop refuses to
stage or sign that transition until its roster deletion and recovery lifecycle
are complete.

Password/secure/editable accessibility nodes and their entire descendant
subtrees are redacted before text, descriptions, selector-bearing resource or
class fields, element identities, semantic digests or result bytes are built.
Only bounded structural roles, interaction flags and geometry remain. Ordinary
and snapshot results stay within their smaller action ceilings; screenshot has
an 8 MiB result ceiling plus fixed protocol overhead. Fragmentation and
aggregate overflow are rejected rather than silently lost. The stable effect
identity binds the attested installation, review and package authority but
excludes the ephemeral socket UUID, which remains in live fences and durable
audit/evidence so a completed result can recover after reconnect.

Only the private Apps owner lowering carries the package allowlist to the
handset. Public App arguments cannot name a device, package, raw coordinate, or
MCP verb. The handset accepts `_magician_apps` only on the exact mapping
`android_get_ui_tree`/snapshot, `android_screenshot`/screenshot,
`android_launch_app`/launch, `android_close_app`/close, `android_tap`/tap,
`android_input_text`/type, `android_press_key`/key and `android_swipe`/scroll.
The claim and action arguments are closed; generic device, shell, ADB and other
MCP vocabulary remain outside the Apps descriptor.

Snapshot freezes package and geometry across capture. Screenshot and lifecycle
check their target before and after I/O where applicable. Tap, type, key and
scroll require the exact prior foreground package, geometry and structured
snapshot SHA-256 to revalidate immediately before physical I/O. Every success
returns a uniform `structuredContent.apps_owner` receipt with nullable target,
foreground, observation and geometry fields, bounded evidence counts,
`outcome: settled`, and lowercase SHA-256 over the exact returned UTF-8 text or
decoded JPEG bytes. Runtime deny-unknown parsing and independent hashing run
before any result is projected to an App.

The legacy mobile exchange route is a narrow Cloudflare Access bypass because an
unpaired phone cannot yet possess either credential. Its five-minute 256-bit
single-use capability is the authority. A successful exchange returns the
current outer Access pair plus the per-device token; iOS stores one atomic
Keychain profile and Android one encrypted-preference transaction. Both clients
then probe `/devices/me` through the normal Access policy before committing the
new profile. Every ordinary native request presents the device id/token, and
middleware derives principal/workspace from the roster rather than trusting
phone headers. A shared outer Access credential without that device identity is
rejected and can never become the user's principal. Revoking the roster entry
therefore revokes mobile API use as well as Android automation.

The Android Apps exchange described above is separate: it returns only
`owner_approval_pending` or an `active` status and never returns a bearer token.

Provisioning also reads the real Access application audience and Zero Trust
organization issuer into `MAGICIAN_CF_ACCESS_AUD` and
`MAGICIAN_CF_ACCESS_TEAM_DOMAIN`. The origin verifies the assertion and never
maps a shared service-token `common_name` to a user scope; only an interactive
human identity or the paired-device branch can establish one.

Accepted ordinary mobile requests refresh roster presence at most once per
minute. Persistence failure is diagnostic and never changes an otherwise valid
authorization.

Two refusals share one error shape. "No such device" and "wrong token" answer
identically. A failed attempt does not update `last_seen`.

Unpair is immediate revocation, not a promise about the next reconnect. Before
the durable roster removal, the hub removes and terminates the current socket
generation; its eventual actor cleanup is stale and cannot touch a replacement.
If the disk write fails, durable and in-memory authority remain unchanged but
the handset must prove a fresh socket generation before it can be used again.
Re-pairing an existing device also terminates its old generation as soon as the
new token is committed.

### At-a-glance and remote delivery

The same paired-device credential owns mobile push registration at
`PUT /api/magician/v2/devices/me/push`; `DELETE` removes a registration. The
body carries only platform, registration kind, provider token, environment, and
an optional task id. Middleware supplies the canonical scope/device identity,
and the server cross-checks APNs against an iOS roster entry and FCM against an
Android entry. A handset therefore cannot register a token in another scope or
claim the other platform. Responses never return the provider token; they do
return an opaque monotonic route revision that a client may supply on `DELETE`
to remove only the generation it registered.

Provider routes live in private `system/mobile-push-registrations.json`, not in
the owner-visible paired-device roster. The file is capped, atomically replaced,
created mode `0600` on Unix, and repaired to that mode when an existing store is
opened. Unpairing also removes those routes; idempotent cleanup persists even
when the row is already absent. APNs ActivityKit and Android ongoing-notification
routes are task-bound and removed after the terminal update. A newer task route
replaces only its oldest ephemeral task route, never its application route.
Provider-invalid APNs tokens and FCM installation registrations are pruned
automatically in one durable batch per delivery wave. Cleanup compares the
provider token observed by that request before deleting the deterministic row.
Terminal cleanup additionally compares the registration revision because Android
legitimately reuses one FCM installation id across task-route generations.
Replacement revisions are strictly monotonic per row even when two uploads
arrive inside the same wall-clock millisecond. Both mobile clients retain that
revision for teardown. Typed provider-auth rejections discard only the exact
cached bearer that was used, while provider response bodies never cross into
application logs. APNs transport errors are classified before logging because
its request URL contains the device token.

The bounded subscriber projects only canonical `HitlRequested`, `HitlResolved`,
and `ExecutionPanelDelta` events. It sends no HITL prompt or secret to a lock
screen: Attention gets a generic alert and exact correlation deep link; task
progress gets bounded title/status text and the exact task link. Event timestamps
travel through APNs/FCM so delayed provider delivery cannot roll a newer card
backward. Registration returns an explicit `503
mobile_push_provider_not_configured` when that platform's provider credentials
are absent; the clients retain local widget polling/realtime instead of
claiming a remote route exists.

Event scheduling and provider HTTP admission have separate semaphores: many
canonical events may wait independently, but the process never multiplies them
into more than eight simultaneous provider requests. Each execution admits at
most one non-terminal provider update per second. Terminal state bypasses that
rate and leaves a bounded watermark that rejects a late progress event for the
ended execution. The canonical millisecond event time remains unchanged for
Android; ActivityKit receives a separate strictly increasing seconds-level
timestamp so its ordering rule cannot conflate progress and terminal truth from
one second.

Ordinary task progress stays at provider background priority. Needs You alerts
and terminal task truth use immediate priority. iOS non-terminal ActivityKit
updates carry a 20-minute stale date matching the local watchdog. When
ActivityKit cannot create that surface, task dispatch continues without an
invisible audio/socket keepalive.

APNs is enabled by `MAGICIAN_APNS_KEY_ID`, `MAGICIAN_APNS_TEAM_ID`,
`MAGICIAN_APNS_PRIVATE_KEY_PATH`, and optionally `MAGICIAN_APNS_TOPIC`. FCM is
enabled by `MAGICIAN_FCM_SERVICE_ACCOUNT_PATH`; the JSON must belong to the same
Firebase project used to build the Android app.

The roster is published durably — `DevicePairingStore::open` refuses to parse a
truncated file and the daemon refuses to start over it. The write needs one
writer at a time, a temp name unique per write, the temp fsynced before the
rename, the directory fsynced after, and the temp removed on any failure.
`verify` persists on every accepted connection under a read guard, so
concurrent reconnects must not share a fixed `paired-devices.json.tmp`.

## The agent-facing verbs

Five tools: `android_snapshot`, `android_act`, `android_screenshot`,
`android_app`, and the read-only `android_notifications`. The device's own
action names are the vocabulary *inside* `android_act`: tap, long_press,
double_tap, type, swipe, drag, pinch, key (back, home, enter), open_url,
set_clipboard, system (recents, notifications, quick_settings), wait_for,
wait_gone, wait_idle and scroll_to, each mapped to the handset tool of the same
purpose (`android_double_tap`, `android_drag`, `android_open_url`,
`android_global_action`, `android_wait_for_element`, …). There is no
`android_tools` pack; discovery happens inside the governed device session.
Only those compiled public verbs are projected to the model, and their
internal Android action names must still resolve to a tool ID from the
handset's current authoritative roster.

A verification code is held out of **every** screen read, not only the
notification one. The companion applies the same `OtpWatcher` judgement at the
seam its tools share, so a value carrying a code comes back as the withheld
marker from `android_get_ui_tree`, `android_find_elements`,
`android_get_screen_context` and the waiting tools, with
`verification_text_withheld` beside the result. This is redaction, not
refusal, for the reason the notification tools already give — a code can
arrive while an agent is legitimately reading a screen — and it is judged on
content rather than on which surface is showing, because a heads-up banner
puts the same text over whatever app is open. A long value is judged line by
line so the Apps snapshot keeps every row that carried nothing.

`FOREGROUND_GATED` alone does not cover this: it refuses only while a
*protected app* is foregrounded, and the notification shade is SystemUI.
**`android_screenshot` remains the exception**: its payload is pixels, which
this cannot scrub, so a code visible on screen is readable by a model that can
see the image.

Its pack is deliberately **not** categorised `android`: that category marks an
Apps Android device-owner primitive, which must resolve to reviewed action
leaves under the eight-action owner roster. Notifications are not one of those
actions, and an unresolvable pack makes the whole compiled tool catalog report
`ResolverUnavailable`. Agent-facing verbs that are not app primitives take
ordinary descriptive categories.

`android_notifications` reads the shade through the companion's
`android_get_notifications`, which the handset serves under its own
`NOTIFICATION_FILTERED` disposition rather than the eight-action review
roster — so it is available to a paired, attested phone without an action
approval. Verification material is withheld **on the device**: such a
notification arrives as a marker and is counted in `withheld_count`, never as
digits, because a code reaches a run through the custody lane and must never
reach the model (secure HITL P6). The verb exists so the filtered door is the
sanctioned one; otherwise an agent reads the shade via `android_snapshot` or
`android_screenshot`. The pack guide and the `android-operator` definition say
so.

Sight is structural first: a snapshot usually answers; a screenshot is the
exception. With one phone connected the agent does not name a device; with
several, the call is refused rather than guessed. Errors separate retryable
from not: a phone that was asleep is worth another try; an action the device
ran and refused is not.

## Governance

Protected apps, screenshot policy, and audit are enforced at the seam each one
belongs to.

**Protected apps are enforced on the device**, at the single dispatch seam
every tool passes through — including notification and OTP tools the
five-verb surface does not expose. While a protected app is foregrounded,
observe and act tools return a structured `app_protected` refusal (a
success-shaped MCP result; `isError` is flattened to a bare string);
notification tools drop entries whose *source* package is protected. The
list is seeded with authenticators and owner-managed under Settings >
Privacy, through one process-wide store shared with the live gate. Magician
maps the refusal to a non-retryable `app_protected` error.

Tree-reading handlers re-check the root they actually walk; wait and scroll
tools re-check every poll iteration; screenshots check at the instant of
capture; caller-chosen durations are clamped server-side. `back`/`home` stay
available under protection; `open_url` refuses view intents that resolve to a
protected package; agent-driven recents or notification-shade opening is
refused while anything is protected. The sealed Apps snapshot always refuses
SystemUI, the current launcher/recents owner, every non-application window,
and any window whose type cannot be established. It resamples package and
window class after capture before releasing evidence.

**The blanket screenshot policy is enforced in Magician**, before any device
round trip: `device_governance::DevicePolicyStore`, its own
`system/device-policy.json` beside the versioned, sealed pairing roster. The
policy remains a separate compatibility document because it governs legacy
device screenshot actions, not the Apps authority chain.
`allow_unless_protected` by default; `block_all` refuses `android_screenshot`
for the whole deployment. `GET`/`PUT /api/magician/v2/devices/policy`. A
corrupt policy file falls back to the default loudly rather than refusing boot
— the default is the safe direction here because the per-app gate on the device
still holds.

**Every device action lands in a durable audit trail** —
`device_governance::DeviceActionAudit`, append-only JSONL at
`system/device-actions.jsonl`, fsynced per record and **bounded**: past ~4 MiB
the newest 10k records are rewritten through the durable writer and older ones
age out. One record per dispatch *and per refusal past argument validation* —
protected-app, policy-blocked, bridge-unavailable, and unresolved-device
refusals included: tool, wire action, device, the foreground package the
device stamped into `structuredContent` at completion time, the verdict
(`ok` / `app_protected` / `policy_blocked` / `error`), and whether pixels left
the device. Reads are tolerant — a torn tail is counted and dropped, a corrupt
interior line is counted and skipped — through
`GET /api/magician/v2/devices/audit`, newest first, records filtered to the
caller's scope. The loss counts are deployment-global
(`global_corrupt_lines`, `global_torn_tail_bytes`). Legacy device verbs keep
their historical best-effort audit behavior and warn on failure. The sealed
Apps snapshot is stricter: after physical I/O it must durably append through
the bounded writer before returning `Completed`; writer failure, cancellation,
or deadline drift settles `OutcomeUncertain` and releases no result bytes.

The sealed roster uses a versioned authenticated document plus a monotonic
platform anchor and exact forward-recovery rules for the publish/anchor crash
window. A corrupt existing roster or unavailable protected seal owner disables
Apps pairing and bridge authority fail-closed. It does not abort unrelated
Magician startup, and it never falls back to unsigned JSON or an in-memory key.
An owner-only legacy roster may be upgraded exactly once only while the durable
generation anchor is still zero. That migration preserves existing mobile
bearer digests but strips every Android automation capability, identity, review,
and generation before sealing generation one; restoring Apps authority always
requires a new signed native enrollment and owner review. An unsigned document
below a non-zero anchor remains a rollback/corruption refusal.

Enrollment will not mint a QR while this durable owner is unavailable.
Exchange serializes one ticket through durable roster publication and removes
it only after publication succeeds, so a storage/key failure returns 503
rather than "expired or already used". A 404 means the running binary omitted
the enrollment route; desktop/iOS clients direct the owner to rebuild.

Missing common signer/version/attestation pins or unestablished code-verified
desktop trust leaves both Android choices unavailable. Missing Play project,
release verdicts, or service credentials leaves only the Play choice
unavailable. Mutation, screenshot, app-lifecycle,
raw-device, coordinate, and general MCP stay outside the admitted descriptor.
