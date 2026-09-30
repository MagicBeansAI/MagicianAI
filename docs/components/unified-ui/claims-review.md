# Claims Review

`/claims-review` is the native owner page for the authoritative transcript claim
register. Open **Claims Review** in the command palette. It uses the same visual
language as Crew and Observe: a masthead, selectable count cards, a searchable
queue, and an exact-words detail panel. The panels stack on narrower screens.

## What arrives here

- New text replies in persisted Envoy sessions on Telegram, Telegram Self,
  WhatsApp, Kapso, AgentMail, and Gmail are prepared before channel dispatch.
  The configured Envoy identity and the session’s channel target determine the
  source. Tracked replies preserve the prepared text, including whitespace,
  through adapter chunking. Owner-control chat, incoming guest words, internal web conversations,
  attachments, and unrelated tool sends are not automatically captured.
- Each message becomes one pending candidate, even if it contains several
  sentences. Capture is structural and uses no model call.
- **Add a conversation** records exact words from an external meeting or call,
  attributed to a named speaker on our side. This sends no message. An optional
  existing relationship ID enables commitment review for that relationship.

An interrupted import keeps its exact request and conversation key for **Retry
conversation**. To change the details, choose **Start a separate import**, which
assigns a new key; check the queue first because the earlier attempt may have
completed. Reusing an existing key with changed source context is refused.

A channel target may be an opaque chat or reply-message ID. Gmail resolves the reply message through provider metadata and binds the actual
reply address to the stored session address. A different Reply-To is refused,
not silently treated as the original contact. Envoy capture
does not invent an account, engagement, or relationship binding.

## What a decision means

**Confirm statement** records that these words were said to these recipients.
It does not establish truth, approve a disclosure, or confirm a promise. A named
reviewer, an authenticated interactive owner session, and the displayed revision are required.
A display name is attribution, not authority: channel bots, API tokens and terminal
grants cannot confirm/reject statements or confirm commitments. The server refuses extractor
self-confirmation, stale decisions, and confirming an Envoy reply without channel
acceptance. Rejection remains in the history.

The delivery badge distinguishes preparation, awaiting a send receipt, provider
acceptance, delivery, and unknown outcomes. **Accepted by channel** is not proof
that a recipient read or received the message. Suppressed, partial, failed, or
unreceipted adapter sends remain unknown and cannot be confirmed as said.
Kapso replies require a provider message ID for every chunk. Missing receipts
or a provider request exceeding 30 seconds leave the outcome unknown, stop
remaining chunks, and do not cause an automatic resend.
Telegram checks each receipt's message ID and chat and bounds each send to
30 seconds. AgentMail's 30-second deadline also covers a stalled response body.
Neither channel can turn an incomplete response into acceptance or resend on
timeout.
While the bot remains running, failed receipt reports retry every 30 seconds
and on message replay, using the original attempt proof without sending the
message again. A process restart without a recorded receipt still leaves the
outcome unknown.

For a claim already linked to a relationship, **Record a possible commitment**
creates an unconfirmed term. **Review commitment** then requires a separate
named confirmation. Unbound channel replies cannot silently create promises.

Search, status filters, and 40-row cursor pages are server backed. Count cards
show the register totals. Visible queues refresh every 30 seconds, pausing while
a decision is being reviewed. Failed reads display an error, never a false empty
queue. Switching scope clears the prior workspace’s records and cancels pending
requests. Older commitment responses cannot replace newer reads or decisions. Lost decision responses can be retried with the same decision ID and
revision; refresh reads the current record before starting a different decision.
Before opening a decision form, the page checks for an interrupted confirmation
saved on the server. If present, it restores the original reviewer, note,
revision and decision ID, even after a reload, and asks the owner to explicitly
retry it. A competing rejection cannot replace that admitted confirmation.
Failure to read recovery state blocks a new decision. HTTP success alone does
not complete a write: claim and commitment responses must contain a receipt
matching the submitted decision ID, target, operation, revision and reviewer,
with a consistent resulting record. Commitment responses also bind the
relationship and exact terms. Import responses must account for the submitted
words, attribution, recipients and conversation key. Missing or mismatched
acknowledgements retain the original request and retry ID; imports retain their
entered text. A valid receipt replay may return a commitment that was confirmed,
withdrawn or superseded since the original request, so the notice states its
current status instead of presenting an old acknowledgement as its current state.

## Installed app

The `claims_review` package `0.2.11` adopts the same masthead, queue/detail,
exact-words and responsive treatment. **Sync claim register** runs its declared
read-only `sync_claims` action for a bounded page of all statuses and the selected
claim’s detail. Refresh after that run completes. The atomic `claim_sync_page`
projection stores the exact page membership and source continuation cursor,
including empty bounded scans. **Sync next page** continues that cursor;
**Sync first page** starts over. The selected claim's summary is refreshed from
its detail read even outside the current page, and Refresh reloads cached detail.
The exact-words panel preserves the complete text in a scrollable block. Claims without a relationship
are projected with null relationship fields rather than dropped; they cannot
create a relationship-bound commitment. Its rows remain
bounded app projections and its action buttons record review requests; those
requests do not constitute applied owner decisions.

The app host offers **Open native Claims Review** for direct owner review.
The sandboxed app does not receive owner API access. Native delivery badges,
register-wide counts, and direct decision application are not fabricated from
the app’s partial projection. Installing the new assets requires a reviewed
package update with its exact grants; editing the seed does not update an
already admitted installation.

## Verification and rollout

Run `make test-envoy-claims-review` for the focused Rust, app recipe, bot SDK, and UI tests;
`make check-ui` and `make check-bots` check the client surfaces. The runtime and
bot SDK/adapters must be rebuilt together for the delivery receipt protocol.
Older bots cannot acknowledge new candidates, and an unprepared historical
Envoy reply is refused by the new dispatch gate rather than sent without a
record. There is no automatic historical backfill or resend after an uncertain
attempt. No external message is sent by this test lane.
