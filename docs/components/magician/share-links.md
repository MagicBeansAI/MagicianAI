# Share links — capability grants for audience members

Deal Close plan phase 2, generalised. Module: `magician/src/magician_v2/share_links/`.

The plan's contract: *"One identity, one link, expiring, revocable, bound to the
engagement."* Generalised to bound to an **audience** and a `resource_ref` — a
data room today, any shared resource tomorrow.

## The secret is never stored

The caller supplies the secret at issue time (entropy is the runtime's job);
the store keeps only its blake3 hash. A store holding plaintext capabilities is
a breach amplifier — one file read becomes every live link.

## One identity, one link

A second issue while a live link exists for `(resource, identity)` is refused,
so revocation always has exactly one target. `rotate` is revoke-plus-issue in
one call; a rotate that changes the audience is **refused** — a rebind is a
different operation, and silently resetting the presentation count would
restart the record of how often that identity's link has been used.

A secret whose hash matches a **dead** credential in the same slot may never be
re-armed: re-issuing it would resurrect a revoked URL, and extend-by-reissue
would silently make a leaked link longer-lived.

## Expiry is required, revocation is forward-only

A possession-based grant that never lapses is the blanket-yes failure, so
issuance without an expiry is refused; expiry is inclusive. Revocation prevents
future presentation and recalls nothing already fetched — the record says so
rather than implying otherwise, and past presentations stay on the record.

## Presentation is possession, checked against the living relationship

`present(resource, secret, audience, now)` refuses — with a distinct, honest
reason for the audit trail — unless ALL hold: the secret names a current
credential; unrevoked; unexpired; the supplied audience is the one the link was
bound to (kind included); the audience is current; the audience still admits the
identity. The transport layer decides what to reveal externally; the log gets
the truth.

The returned `sequence` numbers **presentations** of the secret, and the count
belongs to the identity (it survives rotation). It is **not** the access log's
`AccessEvent.sequence`, which numbers *visits*. The reader surface
(`magician-api/src/data_room_reader_api.rs`, mounted at the app root) presents
the credential on every request — presenting it is the authorisation — and
writes exactly one `AccessEvent` per successful presentation, refused or
served; but it numbers that event from the access lane, not from here.
`GET /rooms/{room_id}` takes the token's next number and a document fetch
carries the number of the opening it followed, so fetching a listing and then
two documents is three presentations here and **one** visit there. Writing this `sequence` into the log would make every fetch a
visit — see `docs/components/magician/data-room.md`, *Visits, not clicks*.

The owner HTTP surface (`magician-api/src/data_room_api.rs`, under
`/api/magician/v2`) issues, rotates, and revokes on the production path:
`POST /data-rooms/{room_id}/links`, `/links/rotate`, and `/links/revoke`. The
reader presents on every request.

A killed credential can never live again: the store keeps a resource-wide
dead-hash set, so a replayed issue cannot re-arm a rotated-away or revoked
secret from any generation, and rotation is recorded explicitly on the record
(`rotation_of`) rather than inferred from timestamp coincidence.

## Failure semantics

Reads distinguish absent from unreadable (`magician_v2::jsonl`): an I/O fault
propagates rather than folding to an empty store, because an empty fold here
fails **open** — a revoke would report success while revoking nothing, and the
one-identity-one-link guard would pass vacuously.

## Consumers

When a document is added, `data_room_api` computes `live_holders()` from the
grant log — everyone holding an unrevoked, unexpired credential on that room —
and uses that set as the disclosure audience-of-record.
