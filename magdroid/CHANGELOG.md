# Changelog - Magdroid Companion

All notable changes to the Android automation companion are documented here.

This file also serves the Apache 2.0 §4(b) obligation to state significant
changes made to the vendored work.

---

## [Unreleased]

- App widgets no longer flash "App widget unavailable". A refresh deadline that is already due, or missing, now means "ask again in 2 s" instead of an error. A 304 renews the cached widgets. The last good widgets stay up through brief failures for up to 5 min, or the widget's `max_staleness_seconds`. The card stays visible during a refresh with a small spinner. A hidden workspace default shows the add-a-widget slot.

- Observe is now the web Command Deck: header, 2×2 KPI view switchers (Now, Sources, Audio, Notes), a LIVE block above every view, capture launchpad, Recent captures, device + view-only web sources, meeting/listening audio profile pickers, Published Notes at iOS parity (search now actually filters), `magican://observe?pane=…`, and the battery warning and notification permission surfaced.

- Bold text no longer turns hairline-thin after some launches. Outfit, Manrope and Geist Mono now ship as one static file per weight (400–800), replacing one variable file each whose default weight was Thin.

- 0.5.0: Today is the Morning Edition, matching web: a compact masthead with
  the greeting and refresh in the top bar; a realtime wire; a one-line lead
  story or "Slate is Clear"; and a collapsible operations carousel (Economics,
  State of Operations, State of the Crew) with dots, auto-advance and fixed
  heights. The Reading Room has a sideways-only Morning Brief swipe deck and a
  tabbed, server-paged Broadsheet. Then § 3 briefings, § 4 deliverables and
  § 5 digest, set in static Newsreader weights.
  - Activity moved into the wire drawer. The At a glance grid, pulse chips and
    lane picker are gone.
  - Impressions start only for on-screen cards. Sheets that fail to load show
    Retry.
  - Channel follow-up snooze is a single "hide from Today", and resurfacing
    dismiss reasons match the server.

- Tasks match web: Result and Publish to Notes on finished cards (Reset only
  after a run), ↻ Recurring chip with a readable schedule, Output split into
  Deliverables and collapsed intermediates, delegation envelopes with a
  Grouped/Chronological toggle, run durations in History, a single Reset in
  detail, unclipped verdict text and theme-palette detail colours.

- Recognize service-health HITL notices in task attention metadata.

- Refresh chat, profiles, and engine choices immediately after pairing or replacing the server connection.

_Current development version: `0.5.0` (Android version code `25`)._

- Storage settings display automatic database-maintenance progress, completion and reclaimed space.

### Chat harness choice (0.4.7)

- The chat composer can select Magician, Pi, or another installed harness and
  its matching profile or model. The selection stays on this device.

### A verification code is held out of every screen read, not just the notification one

- The withholding `android_get_notifications` applies now runs at the seam
  every screen-reading tool shares, judged by the same `OtpWatcher` classifier
  so a screen read and a notification read cannot disagree about what a code
  is. The gate did not cover this: it refuses while a *protected app* is
  foregrounded, and the notification shade is not an app — so the message the
  notification tool withheld came back verbatim from `android_get_ui_tree`.
  The test is on the content, not the surface, because a heads-up banner puts
  the same text over whatever app is open.
- A screenshot is pixels, so the regions carrying a code are blacked out
  instead, region by region rather than by refusing the tool: a verification
  message can sit in the shade for days. `verification_regions_withheld`
  travels in the metadata, which now precedes the image so it survives a
  shortened payload. An unreadable screen while a code notification is live
  refuses the capture rather than reporting it clean.

### A verification code answers its challenge on the phone (secure HITL P6)

- `android_await_otp` is a trusted handoff now: the runtime names the
  challenge (`challenge.correlation_id`, `source`, `window_start_ms`,
  `deadline_ms`, `expected_digits`), the bridge judges the notifications
  posted inside that window (`OtpWatcher.decide` — protected apps excluded;
  two different codes are ambiguous, never the newest; a code from before the
  window is not this challenge's) and answers the pending ask itself over the
  device's paired credential (`ChallengeDeposit` → `POST /hitl/{id}/respond`,
  `input_type: otp`, channel `android_notification`). What returns over MCP is
  status only — `deposited`, `no_code`, `ambiguous`, `already_resolved`,
  `deposit_failed`, `requires_challenge` — never the digits, never the
  message; a call without a challenge is refused rather than served the old
  way. The tool's roster entry and schema say so, and its annotations call
  it an idempotent write (it answers an ask), not a read.
- `OtpWatcher` applies the runtime's own extraction rules, rule for rule:
  approved formats (4–8 digits, one run or equal groups joined by a space, a
  dash, a dot or a non-breaking space), a verification cue within 96
  characters (the bare word "code" is none), links cut out, recovery /
  setup / reset / promotion / postal / tracking wording refused, a
  four-digit year and all-same digits refused, two candidates ambiguous.
  Both extractors are checked against one fixture list
  (`magician/src/magician_v2/verification_codes/extraction_fixtures.json`,
  `OtpWatcherFixtureTest`), so a rule that changes on one side fails the
  other's test until it follows. The runtime calls in slices of at most 30 s
  and refuses the phone's answer (`deposit_failed`, HTTP 403) once the
  owner withdraws the device's verification-code grant.
- `android_get_notifications` no longer hands the model the digits
  `android_await_otp` refuses to return. A notification whose text the same
  extractor reads as a verification code comes back as
  `"[verification message withheld — answer the ask with android_await_otp]"`
  with `withheld: true` (the count is reported as `withheld_count`); its
  package and timestamp are unchanged. Protected apps cover authenticator
  apps, not Messages or Gmail — which is where service codes actually arrive —
  so the adjacent read tool was a way around the refusal.
- The `challenge` the runtime sends now names the ask's own lane (`source`).
  Without it the bridge guessed `user_request`, a lane with no record of an
  agentic pause, and every deposit was refused; the default is `agentic` so an
  older runtime paired with this build still works.

### Secrets are masked by the backend's spec (secure HITL P3)

- The attention answer sheet and the chat escalation card read the backend's
  value-free `input_schema.sensitive` (`SensitiveSpec`) and render by it
  (`hitlRenderKind`): a code as a masked `Password` field (not `NumberPassword`:
  a code is an exact string and some are alphanumeric), a password or other
  secret as a masked field, an identifier readable but marked kept private; a
  form masks exactly the flagged fields. The new `otp` input type
  posts the password value shape, exact string; a `text` ask the backend
  classified keeps its type and is never trimmed. A window counts down; past it
  the fields close and **Request a fresh code** posts `aborted`. Dismissing a
  secret ask posts `aborted` (`HitlResponseValue.Aborted`).
- `android_await_otp` still returns raw digits in a tool result; that is
  replaced by the trusted custody handoff in P6.

- App Pilot enrollment and reconnect now honor the trust method selected in
  Web Settings. Private/self-hosted builds omit Google Play tokens while
  retaining hardware-key attestation and signed socket proofs. Enrollment
  failures now distinguish hardware policy, Play verification, desktop owner,
  and secure-route problems instead of reporting every 403 as Cloudflare.

### 2026-09-20 — same-Wi-Fi or remote pairing

- **Chat history:** task lifecycle snapshots now advance one card to its latest
  state instead of leaving earlier Running and Planning cards in past chats;
  terminal system records render as completed cards instead of empty gaps.
  Historical assistant responses hydrate their Steps as they enter view, task
  cards link to the full run, and Markdown renders as formatted text instead of
  exposing its source markers.
- **Connection:** pairing gives the user an explicit Same Wi-Fi or Remote choice
  before scanning, refuses a QR for the other route with recovery guidance, and
  accepts server-issued private-LAN HTTP addresses while continuing to reject
  public plaintext destinations.
- **App Pilot status:** ordinary chat/tasks/notes/voice pairing is now shown separately
  from attested App Pilot enrollment; Magician MCP reports Not paired, Needs setup,
  Connecting, Disabled, or Running instead of calling every disconnected state Off.
- **Pairing surfaces:** ordinary mobile access now lives in Settings → Connection;
  App Pilot accepts only the separate attested automation QR, and both screens
  reject a QR intended for the other credential.

---

Older entries: `docs/archive/changelogs/magdroid.md`
