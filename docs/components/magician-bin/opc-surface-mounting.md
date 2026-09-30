# Mounting the OPC surfaces

`magician-bin/src/main.rs` wires three OPC surfaces, and one of them is mounted
differently on purpose.

**Inside `/api/magician/v2`** (Cloudflare Access wrapped, owner-facing):
`configure_approval_envelope_routes` and `configure_engagement_routes`, beside
the resource-authority routes they are a sibling of — that one governs how much
of a commodity an act may spend, these govern what class of outcome it may cause
and who it may act for.

**At the app root, outside that scope:** `configure_data_room_reader_routes`.
The people this surface serves are counterparties, not team members, so an
Access-wrapped mount would lock out exactly its intended audience. Its authority
is the capability link presented on each request, checked against the living
audience — not a team identity.

App state: `ApprovalEnvelopeApi` and the reader's `ReaderSurface` are built beside
the other `shared_*` values. The reader takes an `AudienceSource`; the shipped
implementation resolves membership through the counterparty register, and the
trait exists so a programme roster or panel can supply one without the surface
knowing what kind of relationship it is serving.

## Tier 2: the writers, and two workers

Four writer surfaces mount inside `/api/magician/v2` (owner-facing, so unlike
the counterparty reader they belong behind Cloudflare Access):

| surface | what it writes |
|---|---|
| `data_room_api` | opens rooms, issues links |
| `suppression_api` | the suppression register |
| `counterparties_api` | mints organisations |
| `work_modules_api` | run state, scheduling, claim manifests and obligations |

`counterparties_api` takes no app state — its handlers resolve scope from
headers and open their own store, so there is nothing to construct at boot.
`data_room_api::OwnerSurface` takes the SAME `AudienceSource` the reader does,
deliberately: a room and its reader must agree on who is in the audience, and
two independently-constructed resolvers would eventually disagree.

## Two workers, and why they are named bindings

```rust
let _delivery_hygiene_worker = SuppressionSweepWorker::spawn(…);
let _obligation_sweep_worker = ObligationSweepWorker::spawn(…);
```

Both are bound to a name rather than `let _ = …`, because `let _` drops the
`JoinHandle` immediately and the worker would never run — a spawn that reads as
started and is not.

**Delivery hygiene** records bounces and complaints, so
`outward_gate::contact_refusal` does not screen outward acts against an empty
register (which would pass vacuously).

**Obligation sweeps** fill the obligation register from real activity — the
data-room follow-up sweep and the scheduling silence sweep. Without them the
register an agent's cycle reads stays empty.

Both decline to start when their config is off or paused, and say so through a
health snapshot rather than by silently not running.

## Tier 3: the loops, and what starts them

Two more workers, and one shared value.

**The maturity sweep** (`spawn_configured_maturity_sweep`) is the only producer
of `silent` outcome observations. Without it the cohort comparison sees replies
and nothing else, so a variant everybody ignored is indistinguishable from one
nobody tried.

**It ships OFF**, unlike its two siblings, and the asymmetry is deliberate. The
delivery-hygiene and obligation sweeps *derive* from records that already exist.
The maturity sweep records a **judgement** — that somebody who has not answered
inside a chosen window has decided — into an append-only store that cannot
un-write it. A wrong window permanently records a decision on a date nobody made
one.

Turning it on is an owner act, and only sensible once real outward acts exist
and at least one cohort is declared. The second is the sharper
condition — the cohort key has no subsystem producer, so until someone declares
`variant_ref`/`variant_version` the sweep finds acts and binds none. Its health
snapshot reports that as **degraded** rather than as a quiet zero, so "on and
silently doing nothing" cannot hide.

**The proposal pass** reads the maturity worker's *health*, not merely its
config: a cohort out of a store that only ever received replies is not evidence
about a variant, so a tenant that no completed sweep covers has every comparison
withheld with its counts rather than proposed.

## Two values are shared on purpose

`obligation_sweep_config` is hoisted and passed to **both** the sweep worker and
`WorkModulesApi::with_sweep_config`, so the writer and the owner-facing read
cannot be given different waiting windows and disagree about what is overdue.

`delivery_hygiene_worker.health()` is handed to the app as
`shared_delivery_watch_health`. Without it `GET /delivery/watch` answers 503 by
design: **"no watcher is running" must never render as "the watcher found
nothing"** — a rising unreconciled count is the only early warning that sending
has silently broken.

## Tier 4: the receipt door

`configure_delivery_mail_routes` mounts beside the suppression routes inside
`/api/magician/v2`. It is the owner-authenticated door a receipt comes through.

**There is no provider-direct endpoint, and that is a finding rather than an
omission.** No AgentMail skill exposes a delivery-event surface, and the
Magician API has no provider-signature middleware for an unattended POST. A
receipt endpoint anyone can post to is a **remote suppression primitive** —
mark a message `complained` and a real recipient is suppressed permanently, and
`OptOut`/`Complaint` cannot be lifted except by an explicit owner act. So the
door is owner-authenticated until a signing key exists in the deployment, and
the intake is provider-agnostic so that when one does, it plugs in rather than
requiring this work twice.

The separate Node webhook receivers do not carry delivery events: AgentMail's
port-3011 receiver ignores them, and Kapso's HMAC-verified receiver handles
inbound messages only.

## Where bounces actually come from

Not a webhook: the post. A hard bounce returns to the sending inbox as an RFC
3464 delivery-status notification, and `delivery_receipts` reads it — recognising
a DSN by its structured markers (`multipart/report`, `Action:`, `Status:`) rather
than by a subject line, because "undeliverable" in a subject is how a legitimate
message about a failed delivery becomes a suppression.

`5.x.x` is a hard bounce; `4.x.x` is transient and suppresses nobody. Where
correlation back to the original send is uncertain, **nothing is recorded** — a
misattributed hard bounce permanently suppresses the wrong address.
