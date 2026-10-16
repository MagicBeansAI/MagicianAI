# App interactive capability kernel and physical-owner actions

The owner-neutral interactive authority kernel plus the browser, macOS and
Android owner adapters. Each dependency selects exactly one Ready leaf (several
leaves need several aliases). Browser projects `snapshot`, `navigate`,
`scroll`, `click`. macOS projects `launch`, `focus`, `snapshot`, `click`,
`type`, `key`, `scroll`, `drag`. Android projects only `snapshot`,
`screenshot`, `launch`, `close`, `tap`, `type`, `key`, `scroll`; raw
driver/device, shell, ADB and generic mutation vocabulary are unavailable.

Ready means catalog and installation-review admission, not deployment
availability. Android stays typed-unavailable until reviewed attestation exists
(Play build: nonempty Play release and device-verdict pins and a usable Play
service credential; private build: the roots, from config or Google's published
defaults, and the build approved at pairing) and code-verified desktop owner
trust exists.

## Shared contract

`magician/src/magician_v2/apps/interactive.rs` defines, for all owners:

- closed execution profiles and action classes;
- a strict manifest request: owner kind, logical origins and targets, profile
  class, exact action classes, background/capture/transfer posture, resource
  ceilings, grant and session expiry;
- per action: input/result schema digests, owner implementation and profile
  digests, target-policy digest, result ceiling, required observation kind;
- an immutable grant descriptor bound to installation generation, grant and
  policy revisions, revocation epoch, target policy, background posture,
  observation kinds and resource ceilings;
- a current-policy fence rebuilt after every wait;
- opaque, move-only, non-Serde session, observation, action, effect and owner
  I/O permits;
- installation/run/lease/physical-owner session identity with finite expiry;
- observations carrying sequence, state generation, target, geometry, content
  and label digests, bounded bytes/nodes/pixels and expiry;
- a required pre-start join to `AppEffectBinding`, then use of the common effect
  kernel's one-shot provider-I/O authorization;
- payload-minimal completed or uncertain receipts. A provider-token rejection
  keeps the consumed owner permit, so the adapter must still emit an uncertain
  receipt; no owner can manufacture `cancelled_before_io` or proven-unspent
  after dispatch-start.

A state-changing action advances the session generation, and older
observations then fail closed. Stop handles are cloneable because they can only
reduce authority: the first stop, revoke, run-cancel or deadline reason wins.

## Persisted request, lock, review, and approval

An interactive dependency carries authority only when all four layers agree:

1. the manifest declares one `interactive` request beside an exact singleton
   action selector (`actions: [snapshot]`, `actions: [click]`);
2. package-lock V5 binds the request and target/profile digests to the exact
   primitive source, action, schemas, effects, implementation plan and result
   ceiling;
3. installation review shows the full request and records the owner's explicit
   narrowing in the grant and consumed approval;
4. launch, resume and the final pre-I/O edge reopen installation, grant, lock
   and a freshly resolved descriptor.

The grant authority digest covers the selected interactive set, so any change
to origin, target, profile, action, posture, resource, expiry, source, schema,
effect, implementation or result ceiling needs re-review. A legacy dependency
is projected only if it selects exactly one dispatchable `snapshot`; other
legacy declarations stay inert until republished. There is no implicit
all-actions mode.

Admitted: the owner action classes below, direct-owner execution, denied
transfer, invocation- or run-bound sessions. Capture defaults to structured
evidence; only Android `screenshot` admits reviewed pixels. Untyped, empty,
multi-class, all-actions and background requests fail closed.

## First-party session and stop projection

After effect dispatch-start and before provider I/O, the workflow owner
persists a payload-free session record. Its V2 identity binds the reviewed
grant, lock and request digests plus only opaque run/session and public target
refs, owner profile, action class, resource claim, sequence and time window
(the private physical target digest is not retained). The read model's audit
shows the eight most recent settlement receipts (ref, session, action class,
sequence, terminal class, result and evidence bytes); receipts older than the
session record are omitted.

The read model always lists Browser, macOS and Android in that order.
`conditional` means declared in the sealed workflow and revalidated at use, not
that a binary, TCC permission or device is live. `active` holds only while the
process-local owner registry has the session's stop-only capability. Lost
ownership, expiry, undeclared profiles and Artifact terminal truth are distinct
unavailable reasons. Targets show only an opaque `interactive-target` ref and
broad kind.

Stop state (`available`, `requested`, typed `unavailable`) is separate from
physical settlement. `requested` carries the stop ref, session ref and time; a
stale stop record cannot attach to a newer session. The legacy
`stop_available` boolean is derived, and the UI rejects it if it disagrees.

Routes:

- `GET /api/magician/v2/apps/action-runs/{run_ref}/interactive-state`
- `POST /api/magician/v2/apps/action-runs/{run_ref}/interactive-stop`

Both reopen the authenticated run and canonical execution. Stop accepts only
the current opaque session and an idempotency ref (a retry returns the same
receipt). It publishes the stop head only while the prior stop identity and
run-state head still match, then signals the cancellation owner; success is
only `stop_requested`, never a task-state rewrite or cancellation claim. The
action dialog waits for completed, cancelled-before-I/O or outcome-uncertain
settlement. It never receives raw argv, CDP/session IDs, bundle/package/device
IDs, permission evidence, pixels, accessibility trees or provider payloads, and
has no remote-control path. The protected `interactive_stop` control kind
requires registry schema V19+. The supported-public SDK separately exposes
logical-run cancellation at `POST /action-runs/{run_ref}/cancel` (no
task/execution IDs; no `cancelled` without pre-I/O proof).

## Browser adapter

`magician/src/magician_v2/apps/browser_capability.rs` reuses the
execution-native `BrowserDispatcher` and `AgentBrowserSession` (no second
engine, no OS-jail shell-out).

| Typed action | Class | Required binding |
| --- | --- | --- |
| `observe` | observe | current isolated session and bounded structured evidence |
| `navigate` | navigate or launch | exact reviewed HTTPS origin and canonical URL |
| `scroll` | interact | fresh observation from the same physical session |
| `click` | outward commit | fresh opaque element from the same observation |

Navigate validates URL and origin first, then re-observes the URL and the full
session-owned tab set; an ungranted redirect or popup, lost response,
post-start cancellation or fence change is outcome-uncertain. Scroll and click
need a fresh same-session observation; replay, stale generation, selector
substitution and cross-session refs fail before I/O.

Sessions are ephemeral, installation- and run-specific, headed or headless,
under the scope's runtime storage. Owner Chrome/CDP, persistent profiles,
caller-chosen executable or profile paths, raw argv, `eval`, cookie/storage/auth
commands, credentials, upload/download paths, screenshots and filesystem
artifacts are not in the vocabulary. The vendored CLI resolves from the scoped
runtime root (no ambient fallback), and its content digest is rechecked at the
provider edge. Principal, workspace, lease and session ordinal are in the
private session identity, so sessions cannot alias.

Every action:

1. revalidates grant, global policy, direct-owner posture, target/profile/
   implementation digest, run, session expiry, cancellation and resource claim;
2. matches the locked action and result schemas, implementation digest and
   result ceiling;
3. joins the common effect binding before dispatch-start;
4. consumes the one-shot provider token just before the dispatcher;
5. races every owner await against the permit deadline and stop capability;
6. re-observes URL and tab list (ungranted origin or too many tabs is
   outcome-uncertain);
7. rejects any returned physical artifact;
8. replaces `@eN` selectors with fresh unguessable refs kept in the move-owned
   observation;
9. requires freshly observed viewport geometry and checks evidence and result
   bounds before the completed receipt.

The workflow caller reserves resources, authorizes disclosure, records
dispatch-start, consumes the token, and persists the result and interactive
receipt in one sealed control-plane generation before common settlement.
Recovery requires that receipt and revalidates lock, action, result ceiling and
target.

**Effect identity.** V2 stable digests hash installation/grant/schema, locked
primitive/action, physical target, invocation, canonical input and result
ceiling; admission timestamps stay on the permit, so restart revalidation is
idempotent. Legacy V1 digests (which included timestamps) need operator
reconciliation, never reinterpretation. The resource journal treats an
effect-binding digest as an opaque key; the sealed V2 completion intent is the
only re-attribution record, and orphaned journal starts are post-I/O evidence,
never fresh authorization.

The implementation-plan witness covers this adapter, the dispatcher, session
owner and owned-tab projection; the physical target also binds profile, target
policy, installation generation, run, scope digest, lease, ordinal and a
session-ID digest.

## macOS paired-host adapter

`magician/src/magician_v2/apps/macos_host.rs` lowers exactly `launch`, `focus`,
`observe`, `click_element`, `type_text`, `press_key`, `scroll_element` and
`drag_elements` to the signed paired desktop host. The grant binds the
bundle target, owner profile and implementation, selected leaf, schema,
effects, result ceiling, TCC policy and epoch, pairing finalization and the
run-owned session. Input actions need a fresh same-session observation; opaque
refs resolve to AX identities only inside the owner. Mutating and lifecycle
leaves record intent before I/O and settle completed, failed-before-I/O or
outcome-uncertain; cancellation, response loss, pairing/TCC drift and stale
observations are never clean cancellation. Details:
[Apps macOS host owner](app-macos-host.md),
[pairing](app-macos-pairing.md).

## Android paired-device adapter

`magician/src/magician_v2/apps/android_device.rs` reuses the process-owned
`DeviceBridgeHub`, paired-device roster, protected-app enforcement and device
audit (no new socket client, MCP executor or transport). Four sealed packs:
`android_snapshot` (`snapshot`), `android_screenshot` (`screenshot`),
`android_app` (`launch`, `close`), `android_act` (`tap`, `type`, `key`,
`scroll`). Raw device IDs, shell/ADB, argv, coordinates, unreviewed packages,
filters, depth and generic MCP messages are denied.

The agent-facing compiled `android_snapshot` tool
(`execution/compiled_handlers/android_device.rs`; the Apps adapter does not
retry) asks up to three times, 750 ms apart, while the device reports "No
active window available" (the accessibility root is null during a window
handover, such as right after `launch`, and while the screen is off); a
still-missing window returns `no_active_window` with `retryable: true`.

**Enrollment.** Android Apps credentials come only from the owner-created Apps
QR flow, after native Settings pins the desktop's Keychain Ed25519 identity
(runtime nonce, displayed fingerprint, one-time owner code, challenge-bound
attestation) over the desktop's private Unix socket. Both sides identify the
peer from the kernel audit token and require the expected Apple code
identifier under the same Team ID; socket ownership, mode and inode are
resampled before the nonce is accepted. There is no HTTP or loopback-TOFU
fallback; unsigned or non-macOS peers leave the capability unavailable. An
expired request without completion gets `ExpiredUnspent` and rotates. Signed
Android owner operations are rejected until the pin is durable.

The handset creates a challenge-bound Keystore P-256 key. The runtime verifies
a strict X.509 path to only the deployment-pinned Android root, binds the
pinned Magdroid signer and version, and requires TEE/StrongBox and locked
verified boot. Authority is a fresh server-decoded Play Integrity
`PLAY_RECOGNIZED` verdict with exact release certificates, version and device
verdicts (the local APK checksum is only diagnostic). Every socket binds a new
token to nonce, connection UUID, opaque target, review generation, protocol and
hardware signature before hub admission. Legacy pairing stays
`mobile_client`-only.

**Owner review.** Settings reviews one paired credential, the fixed
eight-action roster and an exact set of target package IDs (the Magdroid owner
app cannot be a target). The scope-bound review holds the opaque target,
roster, monotonic generation, digest and time. Updates need the generation the
owner saw; revoke advances a tombstone; re-pairing rotates target and
generation. Apps cannot submit or infer package authority. The workflow needs
exactly one active reviewed target. Whole-device revocation is hidden and
rejected until the roster deletion and recovery path is sealed.

**Store-loss rebind.** One-sided runtime-store loss uses its own ceremony: the
socket presents the fresh nonce, the same Keychain identity, the previous
bootstrap digest and the preserved desktop owner generation under one display
digest; Settings confirms with a new owner code before `RecoveryHello`. Signed
mutations and App actions stay denied until a separately shown recovery
snapshot restores a nonzero desktop head. The desktop never lowers its
high-water.

**Signed writes.** Review writes use eight closed native commands; Settings JS
never calls HTTP or sees the signer, receipts, attestation or snapshots. The
desktop signs a one-shot request over operation and body digest to a fixed
literal-loopback endpoint; the runtime derives scope and consumes the nonce
once. The owner persists a generation receipt and Keychain high-water before
submission. The runtime publishes the device only after applying the signed
enrollment receipt. The hardware proof binds a handset-generated
connection-secret digest (the secret is never sent or stored). The handset
seals its enrollment state in encrypted non-backup storage before the first
POST; only a verified `/devices/me` plus local commit, or a server Gone, clears
it; Keystore failure disables enrollment. Old raw-device-id review handlers are
unmounted.

**Per-action fences.** The grant binds review generation and digest, roster,
packages, locked source/action/schema/plan, installation, run, lease and
direct-owner posture; the physical owner binds socket UUID, protocol, server
name and version and device key. The roster and review are reopened before
dispatch-start and again before the call, while the hub rechecks the socket
generation. Review transitions invalidate the socket generation before
publishing the new owner head.

**Handset side.** A private `_magician_apps` claim is accepted only for the
eight mapped wire tools; mismatched actions, targets, observations, unknown
fields and legacy selectors are rejected. Snapshot freezes package and geometry
across capture; screenshot and lifecycle check the package before and after;
mutations recompute foreground, geometry and snapshot SHA-256 just before I/O.
The runtime re-parses the receipt, recomputes text and JPEG SHA-256, verifies
bounds and package, and replaces row indices with fresh refs. Secure and
password nodes lose text before IDs, digests or results. One WebSocket frame
per action (≤ 8 MiB for screenshot); fragmentation and overflow fail. The
stable effect target uses attested device, review and package identity.
Cancellation, expiry, disconnect, pairing/review/socket change, overflow,
malformed or injected results and missing audit after start are
outcome-uncertain.

## Experience primitive classes

Admission surface only: two classes are admitted and a third is stopped
(design: layering plan).
No route, skill, workflow or package consumer is wired; both admitted
descriptors are `Conditional`.

### What a package would declare (the admission contract)

Both are `Interactive`-kind platform primitives projected by
`primitive_catalog.rs` from embedded source bytes, one leaf each:

| Primitive | Execution class | Containment | Leaf | Host surface |
| --- | --- | --- | --- | --- |
| `overlay-draw` | `overlay_draw_owner` | `overlay_surface` | `draw` | `/host/overlay/draw` (URL unchanged) |
| `narration` | `narration_owner` | `media_rail` | `speak` | the reviewed TTS provider chain (`media_rail_tts`) |

A manifest declares the normal single-action interactive dependency
(`capability:overlay-draw` + `actions: [draw]`, or
`capability:overlay-draw__draw`). Admission lives in
`magician/src/magician_v2/apps/experience_capability.rs`:

- **Overlay-draw** accepts one `shape_json` checked by the tutor validators
  (`tutor::validate_tutor_draw_payload_shape`,
  `tutor::validate_tutor_draw_storyboard_payload`) plus app ceilings of 256 KiB
  and 64 storyboard steps. The result is a receipt only (payload digest, step
  count, cleared flag): no pixels, observations or element refs. Run-scoped
  grounding (`validate_tutor_draw_payload_for_run`) is left to the consumer.
- **Narration** accepts one utterance ≤ 4,000 chars / 8 KiB with no control or
  invisible format characters (ASCII controls, U+00AD, U+061C, U+200B–U+200F,
  U+2028/U+2029, U+202A–U+202E, U+2060–U+2064, U+2066–U+2069, U+FEFF,
  U+FFF9–U+FFFB), an optional rate clamped to 0.5–2.0x, and delivery hints
  limited to `tts_types` enums (emotion/style/pace/voice_mode) plus a
  ≤ 160-char emphasis phrase. There is no voice, model, provider or format
  field; voice selection stays with the host. A per-run budget
  (`AppNarrationRunBudget`: 32 utterances / 24,000 chars) must be consumed
  before any provider call. Receipt only; audio goes to the host speaker.

Implementation-plan digests bind each leaf to its admission identity (module
bytes plus the reused tutor validators or TTS vocabulary).

### Why the classes are Conditional, not Ready

No physical dispatch path exists, so descriptors and leaves are `Conditional`
("admission only: the reviewed physical-owner adapter is not wired (plan 1.3)")
and installation review fails closed three ways: the dispatch-Ready gate in
`sealed_interactive_owner_is_reviewed`, no interactive grant for these classes,
and the non-dispatchable locked action. Revalidation already exists in
`magician-apps/src/apps/installation_review.rs`, so a consumer can flip
readiness with its review identity sealed. That consumer must choose between
the interactive-grant machinery and a lighter experience grant, and extend the
admission digests with its transport files.

### Voice invocation is stopped

App-declared voice invocation through the orb wake path is **stopped**: no
enum variant, descriptor or phrase API. The
`app-voice-invocation-phrases-stay-unadmitted` red case and tests that
app-shaped utterances match no core lane grammar pin this. Reasons:

1. **Typed markers cannot be namespaced.** `@app-<name>` shares the `@token`
   sigil with core lanes (`@vibedev`), so a no-collision check against future
   core lanes cannot be proven at review.
2. **Spoken phrases cannot be namespaced at all.** ASR drops casing,
   punctuation and sigils; core grammar is leading-verb phrases ("start …",
   "open …"), and mishearings, homophones and other languages cannot be
   bounded.
3. **The grammars are code, not data**: five copies (backend rail, web regex,
   iOS, Android, voice) and no published catalog to prove absence.
4. **Microphone-authorized ambient listening** turns invocation confusion into
   privilege confusion; app realtime-voice conversation is already fail-closed,
   and wake invocation is broader.

Tutor and Copilot packaging is not blocked: they stay seam-registered, and only
overlay-draw and narration are available to a future packaging decision.

### Threat rows

In `magician-apps/src/apps/threat_model.rs`:
`overlay-draw-payload-outside-reviewed-recipe-vocabulary-rejected`,
`overlay-draw-returns-receipt-never-screen-observation`,
`overlay-draw-stays-non-dispatchable-until-reviewed-consumer-lands`,
`narration-input-caps-and-run-budget-fail-closed`,
`narration-carries-no-provider-voice-selection-authority` (all
DormantBoundarySpecified), and `app-voice-invocation-phrases-stay-unadmitted`
(ContractSpecified).
