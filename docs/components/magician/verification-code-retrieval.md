# Verification-code retrieval

How a one-time code that arrives by email, in the local Messages store or on
the owner's Android phone answers a live verification challenge — without
the code ever reaching a model, a history, a tool result or a cache. Secure
HITL plan §6.2, phase P6 (`docs/plans/2026-09-15-secure-hitl-credentials-and-otp.md`;
implementation `docs/plans/2026-09-19-secure-hitl-otp-implementation.md`).

## The challenge is the pending ask

There is no separate challenge object. A challenge is a pending secure ask
whose published spec (`hitl.requested` `input_schema.sensitive`) is a
single-value `otp`, with its `collection_deadline_ms` and optional bound
`expected_destination`. A form with a code field beside a password is never a
challenge.

The resolver (`magician_v2::verification_codes::VerificationCodeResolver`)
keys on the ask's correlation id and answers through the ask's own path
(`ChallengeAnswerSink`):
- `user_request` (chat asks included) → `UserRequestService::respond_scoped`
  with channel `verification_code_resolver`. The value enters service custody
  like a typed answer; the record keeps this channel (other relays are
  rewritten to `secure_ui`) so audit can tell retrieved from typed.
- agentic pause → the API's resume path (`ApiAgenticAnswerSink`).

First response wins — that is the atomic challenge resolution. `POST
/hitl/{id}/respond` refuses `channel: verification_code_resolver` from HTTP
callers (`reserved_channel`).

**One live code challenge per scope.** A second `otp` ask while one is watched
makes *both* `ambiguous` — evidence names no challenge, and guessing is how the
wrong code gets typed.

**A message answers one challenge, a code answers one challenge.** For 15
minutes the resolver remembers message identities (`(source, account,
message_id)`) and codes (salted digests) that answered. A retry after a
rejected code never re-uses that message or code. Cost: a service that
re-sends the *same* code is typed by the person for those 15 minutes.

## Authority is a purpose the owner grants per source

Observation consent authorises nothing. A source is read only when enabled
**and** carrying the `verification_codes` purpose **and** reachable now
(`RuntimeSourceRegistry`, re-read before every fetch and before the answer):

| Source | Grant | Where |
| --- | --- | --- |
| Gmail (`email`), local Messages (`imessage`); AgentMail can be granted but reports itself unavailable | `ChannelEntry.purposes` contains `verification_codes` | Observe → **Use for verification codes** (`PUT /channel-assist/channels/purpose`). Kept across enablement rewrites that keep the account observed; dropped when saved as not observed; always withdrawable even when observation is off; never granted to an unconfigured account |
| Android phone | `device_policy.verification_code_devices` lists the paired device, and it is connected to the device hub | Devices → **Use notifications for verification codes** (`PUT /devices/policy/verification-codes`); pairing alone grants nothing |

Withdrawing stops a watcher before its next fetch and never answers with
already-fetched material. For the phone, `POST /hitl/{id}/respond` with channel
`android_notification` is accepted only over the paired device's credential
*and* while `verification_code_devices` lists it. Retrieval grants nothing
else (payments, resets, recovery keep their own approvals).

## Extraction is deterministic and bounded

`verification_codes::extract`: a 4–8 digit code (one run, or equal groups
joined by space/dash/dot; leading zeros kept) within 96 characters of a
verification cue ("verification code", "one-time", "passcode", "OTP", "code
is", "your code", "use code", …; ASCII-only lowercasing). The bare word "code"
is not a cue. URLs are cut before matching; links are never followed. Dates,
amounts, phone numbers, ids, a four-digit year and all-same digits are not
codes. Messages about recovery codes, authenticator setup keys, TOTP secrets,
password resets, promotions, postal/area codes, QR codes or tracking numbers
are refused. Two distinct candidates ⇒ `ambiguous`. Optional
`sensitive.expected_digits` (4–8) narrows length. Text past 32 KiB is not read.
Message content is data, never instructions.

The Android companion's `OtpWatcher` applies the same rules; both are tested
against `magician/src/magician_v2/verification_codes/extraction_fixtures.json`.

## Matching uses every trusted fact

`verification_codes::matching::judge`, per message, in order:

1. **Window.** Provider receive time (Gmail `internalDate`, Messages store
   date, notification post time — never the `Date` header) must lie in
   `[window start, deadline]`. Window start = `challenge start − lookback (90 s)`,
   clamped to not before the previous challenge *for the same service* was
   decided (`ChallengeContext::not_before_ms`, keyed per bound host). Challenge
   start is the server-observed `hitl.requested`. The clamp covers stale codes
   the resolver never saw (typed by the owner, deposited by the phone).
2. **Sender.** With a bound destination, the sender's registrable domain
   (public-suffix list, `psl`) must belong to the destination's host, and a
   sender the provider failed to authenticate is refused. Without a binding,
   mail must be provider-authenticated; SMS/notifications carry no sender
   identity, so window and cue decide.

   *Provider authentication* = Gmail's own `Authentication-Results`
   (authserv-id `mx.google.com`; other hops and ARC are ignored) shows
   `dmarc=pass`, or `dkim=pass`/`spf=pass` aligned with the `From`
   registrable domain (relaxed alignment).
3. **Extraction**, above.

`decide` judges one poll together: one distinct code wins; two codes or an
ambiguous message go to the person; each message judged once per challenge;
codes that answered earlier challenges are excluded; the newest code is never
preferred over an older one. Across sources, the first match waits one poll
interval for the other sources (a lone source waits for nobody): same code
adds nothing, a different one ⇒ `ambiguous` before any answer. Invariant: the
watcher count is written under the lock that publishes the challenge, before
any watcher spawns, so the first watcher cannot skip the settle.

## Watchers are bounded

One watcher per (challenge, authorised source), from `hitl.requested` until
deadline, 300 s, 40 polls or resolution; poll every 4 s (deadline-aware); 10
messages per poll, bodies capped. Three *consecutive* source failures end that
watcher; one source failing never ends the challenge while another reads; the
final `unavailable` carries every source's reason.

Raw content stays in watcher memory and is dropped after matching; watches
read provider clients directly, never through ingest, so no model-facing copy
exists. Logs carry counts and reasons only.

Status rows live at most 30 minutes. `hitl.resolved` retires a row early
**unless** the row already carries a decided outcome (`code_used`,
`ambiguous`, `unavailable`) — whether this resolver or the companion answered —
so `GET /hitl/{id}/retrieval` can still explain an automatic answer. Safe
because each ask carries its own correlation id (`AgenticPauseState::ask_id`).
Observe cadence is unchanged.

Sources (`magician-comms/src/channel_assist/verification_sources/`):
- **Gmail** — `messages list` with `after:`; headers first
  (`get_message_headers_for_verification`, `format=metadata`: receive time,
  `From`, `Subject`, `Authentication-Results`), MIME text only when inside the
  window; a message counts as read only once read; at most three pages.
- **AgentMail** — **not a source**: no sender authentication, so it reports
  `unavailable`.
- **Messages** — bounded read-only query of inbound rows in the window.
- **Android** — below.

## Android is a trusted handoff

`android_await_otp` is not on the server's four-verb roster; the resolver's
`AndroidVerificationWatch` calls it through the device hub with the
*challenge* (`correlation_id`, `source` — the owning lane as announced,
`window_start_ms`, `deadline_ms`, `expected_digits`) in slices of at most 30 s
so the grant is re-read between slices. A companion whose `android_await_otp`
does not *require* `challenge` is an old build that would return digits and is
never called (`unavailable`, "the companion app is too old").

On the phone, `OtpWatcher.decide` judges notifications in the window (protected
apps excluded; two codes ⇒ ambiguous) and `ChallengeDeposit` answers the ask
over the paired credential (`POST /hitl/{id}/respond`, `input_type: otp`,
channel `android_notification`). MCP returns `{status: deposited | no_code |
ambiguous | already_resolved | deposit_failed | unavailable |
requires_challenge}` — never digits. Mapping: `deposited` → `code_used`,
`ambiguous` → `ambiguous`, `already_resolved` → watch ends, `no_code` → next
slice, rest → `unavailable` with reason. A result carrying `code` disqualifies
the bridge unread. `android_get_notifications` returns a withheld marker
(`withheld: true`) for any notification the extractor reads as a code. The tool
is annotated as an idempotent write.

## Status is safe and visible

`RuntimeTransportEvent::VerificationRetrievalStatus { correlation_id, status,
sources, reason }` (`waiting`, `code_used`, `ambiguous`, `unavailable`,
`stopped`) and `GET /api/magician/v2/hitl/{id}/retrieval` carry the state. The
Attention prompt (`subscribeRetrievalStatus`) fetches once, follows events, and
re-fetches on reconnect; it never shows the code or message. `Verdict` and
`Extraction` print redacted (`Match { code: <redacted> }`).

## Retrieval first, alert second — briefly

The P5 delivery coordinator delays *channel* alerts for a critical `otp` ask by
`hitl.verification_codes.retrieval_grace_secs` (20 s) when retrieval is
expected for the scope (`RetrievalOracle`), unless the deadline is under 90 s;
push is never delayed. `ambiguous` or `unavailable` ends the grace at once.

## Settings

```yaml
hitl:
  verification_codes:
    enabled: true            # off: no source is watched, every code is typed
    retrieval_grace_secs: 20
    lookback_secs: 90
```

Repo seed and template carry the section; the live runtime YAML is written
only by the settings surface. Which sources may be read is never here.

## Out of scope, on purpose

Event-driven Gmail arrival (bounded polling inside a five-minute window);
other message providers until they implement matching and the handoff under
this contract; magic links, TOTP seeds and reset links (separate typed
integrations).

## Tests

`make test-verification-codes`. Rust: `verification_codes::{extract, matching,
resolver, android}` tests, magician-comms `verification_sources`, magician-api
`verification_codes_api`, `hitl_delivery::coordinator`; magdroid
`OtpWatcher*Test`; unified-ui `retrievalStatus.test.ts`. Live journeys:
`whattotest.md` P6.
