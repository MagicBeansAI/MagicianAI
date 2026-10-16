# Mounting the OPC surfaces

`magician-bin/src/main.rs` wires the OPC surfaces, and one of them is mounted
differently on purpose.

**Inside `/api/magician/v2`** (Cloudflare Access wrapped, owner-facing):
`configure_approval_envelope_routes` and `configure_engagement_routes`, beside
the resource-authority routes. Resource authority governs how much of a
commodity an act may spend; these govern what class of outcome it may cause
and who it may act for.

**At the app root, outside that scope:** `configure_data_room_reader_routes`.
Its audience is counterparties, not team members, so an Access-wrapped mount
would lock them out. Its authority is the capability link presented on each
request, checked against the living audience.

App state: `ApprovalEnvelopeApi` and the reader's `ReaderSurface` are built
beside the other `shared_*` values. The reader takes an `AudienceSource`; the
shipped implementation resolves membership through the counterparty register,
and the trait lets a programme roster or panel supply one.

## The writers

Four writer surfaces mount inside `/api/magician/v2` (owner-facing, so behind
Cloudflare Access):

| surface | what it writes |
|---|---|
| `data_room_api` | opens rooms, issues links |
| `suppression_api` | the suppression register |
| `counterparties_api` | mints organisations |
| `work_modules_api` | run state, scheduling, claim manifests and obligations |

`counterparties_api` takes no app state: its handlers resolve scope from
headers and open their own store. `data_room_api::OwnerSurface` takes the
SAME `AudienceSource` as the reader, so a room and its reader cannot disagree
about who is in the audience.

## Two workers, and why they are named bindings

```rust
let delivery_hygiene_worker = SuppressionSweepWorker::spawn_with_receipts(…, bounce_puller);
let shared_delivery_watch_health = web::Data::new(delivery_hygiene_worker.health());
let _delivery_hygiene_worker = delivery_hygiene_worker;
let _obligation_sweep_worker = ObligationSweepWorker::spawn(…);
```

Both are bound to a name, not `let _ = …`, because `let _` drops the
`JoinHandle` immediately and the worker would never run.

**Delivery hygiene** records bounces and complaints, so
`outward_gate::contact_refusal` does not screen outward acts against an empty
register (which would pass vacuously).

**Obligation sweeps** fill the obligation register from real activity: the
data-room follow-up sweep and the scheduling silence sweep.

Both decline to start when their config is off or paused, and report that
through a health snapshot.

## The maturity sweep and the proposal pass

**The maturity sweep** (`spawn_configured_maturity_sweep`) is the only
producer of `silent` outcome observations. Without it the cohort comparison
sees only replies, so a variant everybody ignored looks like one nobody tried.

**It ships OFF**, unlike its two siblings, on purpose. They *derive* from
existing records; the maturity sweep records a **judgement** (that someone who
has not answered within a chosen window has decided) into an append-only store
that cannot unwrite it. A wrong window permanently records a decision nobody
made.

Turning it on is an owner act, sensible only once real outward acts exist and
at least one cohort is declared. The cohort key has no subsystem producer, so
until someone declares `variant_ref`/`variant_version` the sweep finds acts and
binds none; its health snapshot reports that as **degraded**, not a quiet
zero.

**The proposal pass** reads the maturity worker's *health*, not just its
config: a tenant that no completed sweep covers has every comparison withheld
with its counts instead of proposed.

## Two values are shared on purpose

`obligation_sweep_config` is passed to **both** the sweep worker and
`WorkModulesApi::with_sweep_config`, so the writer and the owner-facing read
cannot use different waiting windows.

`delivery_hygiene_worker.health()` is handed to the app as
`shared_delivery_watch_health`. Without it `GET /delivery/watch` answers 503
by design: **"no watcher is running" must never render as "the watcher found
nothing"**, since a rising unreconciled count is the only early warning that
sending has silently broken.

## The receipt door

`configure_delivery_mail_routes` mounts beside the suppression routes inside
`/api/magician/v2`: the owner-authenticated door a receipt comes through.

**There is no provider-direct endpoint, deliberately.** No AgentMail skill
exposes a delivery-event surface, and the API has no provider-signature
middleware for an unattended POST. A receipt endpoint anyone can post to is a
**remote suppression primitive**: mark a message `complained` and a real
recipient is suppressed permanently, and `OptOut`/`Complaint` can be lifted
only by an explicit owner act. So the door stays owner-authenticated until a
signing key exists in the deployment, and the intake is provider-agnostic so
one can plug in later.

The Node webhook receivers do not carry delivery events: AgentMail's
port-3011 receiver ignores them, and Kapso's HMAC-verified receiver handles
inbound messages only.

## Where bounces actually come from

From the mail itself. A hard bounce returns to the sending inbox as an RFC
3464 delivery-status notification. When `delivery_hygiene.bounce_mailbox`
names a provider and `maildir_path`, the hygiene worker pulls DSNs from that
Maildir on each tick (`delivery_receipts::pull::DsnPuller`); with no mailbox
it reports `no_source` every tick, and DSNs come in only through
`POST /delivery/receipts/mail`. In both paths `delivery_receipts` recognises a DSN by
its structured markers (`multipart/report`, `Action:`, `Status:`), never by a
subject line, since a legitimate message about a failed delivery would
otherwise become a suppression.

`5.x.x` is a hard bounce; `4.x.x` is transient and suppresses nobody. When
correlation back to the original send is uncertain, **nothing is recorded**: a
misattributed hard bounce permanently suppresses the wrong address.
