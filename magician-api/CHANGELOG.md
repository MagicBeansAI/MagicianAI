# Changelog

All notable changes to the Magician API project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

- Proxy owner-authenticated Decision Model settings over the engine Unix socket.

- Proxy owner-authenticated Decision Model routing settings to the Decision Engine.

- Notes: watch each notes folder so an outside edit refreshes the search index after the folder has been still for about a second.

- Add owner-only recovery reads for interrupted claim confirmations without applying a decision.

- Require an interactive owner session for statement and commitment decisions so bots cannot approve their own records under a supplied human name.

- Require recipient/text-bound Envoy attempt receipts and preserve tracked delivery after Envoy configuration changes.

- Add bot-authenticated Envoy send receipts and bounded, searchable Claims Review listings with delivery provenance.

- Allow scoped service-health notices to be dismissed through the existing HITL response endpoint.

- Persist the owner-controlled Decision Engine mode and expose it in the engine roster; refresh Grok choices to CLI-supported model IDs.

_Current development version: `0.3.31`._

- Serve embedded app pages through their live-session credentials and keep bridge operations on the heap to prevent Meetings sync stack overflows.

- Add `GET`/`POST /apps/installations/{id}/memory-access` for owner-controlled app memory grants.

- Screen clips record through CuaDriver 0.28's `start_recording` / `stop_recording` on a held-open `cua-driver mcp` session (the removed `set_recording` failed every clip start, and 0.28 ends a recording when its client disconnects); `/screen/describe` reads the front window's tree with `include_screenshot:false`, picks the window by `z_index`, and surfaces driver error codes.

- Voice `session.start` accepts `chat_choice` (the client's composer engine, validated like a typed turn's), and `chat.engine.updated` is visible to every scope.

- Expose scoped automatic database-maintenance status for web and native clients, and gate background app workers on HTTP readiness.

- Chat requests accept a per-turn harness and model choice, with installed
  harness validation. Plane engine Settings exposes Pi and a separately
  validated `pi_profile` for agentic execution.
- Tighten verification-code, critical-alert, and Plane API handling found in
  the integration reviews.


### 2026-09-21 — 0.3.24 — A resume outlives its HTTP request

- `POST /executions/{id}/resume` and the HITL `respond` route run the whole
  resume — loop and settlement — as their own `tokio::spawn` task
  (`MagicianV2Api::resume_detached_from_request`). Actix drops a handler's
  future when the client disconnects; the loop already ran in a spawned job,
  so a browser that timed out on a phone flow left the loop running and the
  settlement that persists the next pause gone — the delegated child asked
  its next question and no card ever existed. The response is rebuilt from
  status, content type and in-memory bytes after the join.
- The HITL `respond` route answers as soon as the answer is admitted to the
  loop (`MagicianV2Api::resume_agentic_execution_acknowledged_when_admitted`):
  `202 {accepted: true, reason: "resume_admitted"}` once
  `execute_agentically_resume_with_validation` fires its `loop_admitted`
  oneshot, which is after validation — a re-ask, an abort, a refused or
  missing pause and every error still return as before. The plain resume
  keeps its synchronous contract, which on a device flow is minutes; the
  UI's fetch gives up at 30 s and the operator's retry then found the
  pause consumed. The spawned task settles the resume either way.
- A resume that finds no pause releases its lifecycle fence
  (`drop(hitl_lifecycle_exclusion)`) before
  `finalize_orphaned_user_pause_by_execution_id`, which takes the same
  fence for itself. The in-process stripe is a plain tokio mutex: the
  re-acquire parked the retry forever, and the resumed loop's next pause
  waited behind it for its terminal receipt — emitted, never persisted,
  `Executing` until restart. One source test pins the order.
- The resume handler announces the answer as the pause's resolution
  (`AgenticResumed` + `HitlResolved`) at admission — before the loop runs
  on it — instead of after the loop returns. A run's next ask carries the
  same correlation id, and a resolution stamped after the loop landed after
  that ask and hid it (magician's lifecycle journal and attention builder
  now order by time). A validation re-ask and a loop failure re-request the
  restored pause canonically (`emit_hitl_requested_for_restored_pause`).
- A resumed run's executors carry the scope's MagicVault secret store
  (`MagicianV2Api::with_scoped_secret_store_for_resume`). Executors rebuilt
  for a pause read back from disk had none, so an answered password was
  never registered in the ephemeral partition and `[REDACTED:password]`
  never resolved: the phone operator's `android_act type` was blocked as an
  unresolved secret reference right after the owner supplied the password.
- A resumed run's executors carry its canonical event scope
  (`MagicianV2Api::with_canonical_event_scope_for_resume`, in the resume and
  continue handlers): the registered scope, else one built from the pause's
  own identity. Executors rebuilt for a pause read back from disk had none —
  a delegated child's pause always is — so every event the resumed loop
  journalled was routed transport-only: the child's second ask reached the
  websocket, but no `input.requested`/`hitl.requested` fact was persisted
  and no card was built; the no-phone double-pause check showed the second
  pause on disk in `WaitingUser` with nothing pointing at it. One test.

### 2026-09-21 — 0.3.23 — A private Magdroid build is pinned at the owner's approval, not in the config

`GET /devices/apps-automation/trust-options` reported the private build
not ready without `android_apps_signing_sha256`,
`android_attestation_root_sha256` and `android_apps_version_codes` in the
live config — values that describe the APK, not the server, and had to be
copied to every server a phone was pointed at.

- `AndroidAppsAttestationPolicy::learning`: a private-build policy with
  no signer or version pinned. The enrollment verifies the chain to the
  reviewed roots (the config's, or `DEFAULT_ANDROID_ATTESTATION_ROOT_SHA256`
  — Google's two published roots — when the config lists none), every
  hardware-enforced claim (package, version code, signing-cert digest,
  locked bootloader, verified boot, TEE/StrongBox, challenge) and the
  proof exactly as a pinned policy does; the one check it does not make
  is `signer ∈ config pins`. The identity it returns carries the digest
  of the policy `pinned_to_build` — the attested signer and version — so
  the desktop approval (which shows package, version and signer) approves
  that exact build and nothing else.
- `AndroidAutomationTrustPolicy::reviewed` learns for
  `OwnerPinnedPrivateBuild` when the config has no signer or versions;
  Play keeps needing its pins. `reviewed_policy_for_identity` takes the
  identity's own build and also tries the learning policy pinned to it,
  so reconnect admits an approved build and nothing else; a config change
  still re-reviews.
- A pinned policy now checks the version code at enrollment too, where
  before an unpinned version enrolled and failed at first reconnect.
- Trust options: the private build is ready with only the address set,
  and says so.
- A refused Apps enrollment now leaves a content-free trace:
  `[ANDROID-ATTESTATION] rejected reason=<pins_mismatch | claims_mismatch
  | root_not_reviewed | chain_order | chain_path | leaf_key_curve |
  software_enforced_key | hardware_authorization | attestation_application_id>`
  at the verifier, and `[ANDROID-ATTESTATION] Apps enrollment rejected`
  at the exchange with the error class, trust mode, whether the policy
  was learning, the declared package and version code, and the chain
  length. The phone shows only "did not match the reviewed hardware
  attestation policy", and the service log said nothing.
- The verifier's opening shape check carried a second version-pin test
  (`policy.app_version_codes.contains`, split across lines) that refused
  every learning enrollment as `Malformed` before any real check ran —
  the first live attempt (Magdroid v21, five-certificate chain,
  2026-09-21). The version pin is now the trust decision beside the
  signer: a pinned policy refuses an unreviewed version as
  `Untrusted` (`pins_mismatch`), a learning policy admits the attested
  one. Every verifier stage now names itself on refusal
  (`[ANDROID-ATTESTATION] refused at stage stage=bounds | spki |
  signature | chain_decode | chain_x509 | chain_validate |
  attestation_extension | key_description`).
### 2026-09-20 — 0.3.22 — Declarative component and skill setup

- Expose scoped component planning/install jobs, typed Skillshub setup metadata, write-only bot and skill configuration, managed account pairing, and governed remote-MCP OAuth for Desktop setup.

### 2026-09-18 — 0.3.21 — ESP pairing metadata and valid realtime context ids

- `POST /devices/pair` records the server-owned `esp32` client kind and only
  `mobile_client`; APNs and FCM registration continue to reject ESP terminals.
- OpenAI Realtime turn-context item ids are now unprefixed 32-character UUID
  hex strings, satisfying the provider's item-id limit instead of sending the
  former 46-character value.

### 2026-09-18 — 0.3.20 — Interaction status on the voice control socket

- The voice control actor forwards `RealtimeProviderEvent::InteractionStatus` as an `interaction.status` envelope (`in_progress` / `idle`) so clients can show a Gemini 3.8 Live Extended Thinking turn still working after its "let me check…" utterance closed.

---

Older entries: `docs/archive/changelogs/magician-api.md`
