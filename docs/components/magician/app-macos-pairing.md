# Apps macOS pairing owner

The Apps macOS host has no ambient endpoint, executable or signing-key
fallback. A runtime scope must explicitly pair with the native desktop before
the typed `/host/apps/macos/action` transport becomes available. Pairing is
stored below the caller-provided system root in a scope-digest-namespaced file;
it never consults `HOME`, an environment variable, a global config file or a
package-provided path.

The authenticated lifecycle is the scoped
`GET /api/magician/v2/apps/macos-pairing` status route and the
`POST .../setup`, `POST .../advance`, `POST .../revoke`,
`POST .../reset/challenge` and `POST .../reset` control routes. Setup accepts
only the literal-loopback typed action URL on `127.0.0.1:3017` and one bundle
ID; the runtime derives the logical target reference and keeps proposal keys
and wire records private. Native approval is a separate desktop operation.
`advance` posts or recovers the retained transition; it cannot mint a
different finalization after an uncertain send.

Native Settings is the only owner-code workflow. It shows the pinned
fingerprint and a memory-only one-time code, then the authenticated scope and
every `{target_ref, bundle_id}` pair before approval. Approval must echo the
displayed setup, generation and review-material digest, and the owner picks
the CUA executable inside its signed bundle,
`/Applications/CuaDriver.app/Contents/MacOS/cua-driver` (symlinks such as
`~/.local/bin/cua-driver` are rejected; the whole bundle is staged and bound).
Upgrading CuaDriver changes that identity, so older pairings must be
re-approved. Changing displayed material clears confirmation. The UI can
recover finalization and issue the signed generation revoke; a native emergency
revoke is labeled separately for runtime-loss cases. Runtime control responses
are streamed under a 16 KiB UI ceiling. No owner code is persisted, logged or
exposed to an App or model action.

## Authenticated lifecycle

An identity gate runs before any key is disclosed, then three durable
authority states follow:

1. The runtime sends a 30-second random challenge to the fixed
   `127.0.0.1:3017` desktop endpoint, binding the owner-entered Keychain key
   ID/public key and the digest of the code shown by the native owner. A valid
   Ed25519 attestation over that challenge and the desktop-observed host
   identity must arrive before the runtime creates any pairing HMAC key. The
   desktop keeps a one-shot attested-bootstrap permit that the proposal
   HMAC-binds and consumes. A bare self-HMAC proposal is refused.
2. `Pending`: the runtime creates a random 32-byte key, opaque setup and key
   IDs, the scope, typed action URL and requested target/bundle pairs. Its
   custom `Debug` omits key material; the challenge, attestation and digests
   are retained.
3. `ApprovedPendingFinalize`: after native approval, the runtime verifies its
   keyed signature and the pinned desktop Ed25519 signature, plus proposal,
   endpoint, scope, target set, host identity, CUA binary identity and TCC
   policy and epoch. This is not yet workflow authority. The runtime computes
   the owner profile and implementation digests, signs a finalization over the
   proposal and approval digests, and stores it before sending.
4. `Active`: only an acknowledgement signed by both the pairing key and the
   pinned desktop identity over those exact finalization bytes promotes the
   generation. Workflows use only this state. A crash after desktop activation
   is recovered with a fresh 30-second keyed status request bound to the
   proposal, approval and finalization digests; authority is never
   reconstructed.

Setup and approval messages live ten minutes; status messages live 30 seconds.
The split lets the runtime recover an already activated generation after a
crash without letting an expired setup authorize a new finalization. A
finalization minted before expiry is verified at its signed `finalized_at`
time, so it can be delivered and acknowledged after restart.

## Rotation and revocation

A new setup cannot replace a live pending or approved stage. With an active
generation, a replacement proposal carries the previous generation and is
signed by both the previous and the new key. The current snapshot stays usable
until the signed desktop acknowledgement, then is replaced atomically. V1
rotation must keep the same pinned desktop identity; replacing that Keychain
identity needs a separately reviewed prior-key cross-sign or the two-store
reset protocol, never a new native approval.

Revocation needs the current generation and a fresh runtime-signed capability.
The desktop verifies it, durably writes a keyless signed `Revoked`
acknowledgement, and only then removes the verifier and key. The runtime
verifies that acknowledgement while it still holds the proposal key, removes
active and pre-active material, advances the generation high-water mark and
writes a keyless tombstone with the acknowledgement digest. A lost response is
recovered by signed status; unknown setup IDs never pass as revocation. Old
approvals, finalizations and acknowledgements cannot re-enter after a rotation
or revoke. An uncertain stage holding a finalization is never replaced
automatically: the caller recovers desktop status or revokes.

A desktop `Revoked` status for the exact transition is applied as a runtime
tombstone too. During rotation it clears both the replacement and the previous
active generation, so native revocation leaves no stale authority.

The bootstrap boundary matters. Literal `127.0.0.1`, redirect refusal and
keyed responses do not authenticate the first listener, because the proposal
carries the new key. The one-time owner code and the pinned Keychain-backed
Ed25519 challenge give that proof before key disclosure, and every later
response is bound to the same identity. Installation review admits the eight
sealed catalog-Ready macOS host actions (`launch`, `focus`, `snapshot`,
`click`, `type`, `key`, `scroll`, `drag`); raw macOS actions stay denied.

## One-sided store-loss reset

Native revoke keeps a monotonic desktop generation floor, so if the runtime
store is lost a silent generation-1 setup stays denied. The trusted UI requests
a 120-second one-shot runtime reset challenge and shows its scope and nonce
beside the pinned desktop identity, host identity and old floor. Only explicit
owner confirmation asks Keychain Ed25519 to sign
`AppMacosHostPairingResetAck` over that material. The desktop stores
`{challenge, acknowledgment}`, clears key material and keeps the floor. The
runtime verifies its retained challenge and the signed identity, host and
floor, rejects any Active or transition state, and pins those values in a
keyless reset anchor. The next proposal must be above the floor. The challenge
is marked consumed after the anchor commits, so deleting the main document
cannot revive an old acknowledgement. Mismatches, floor lowering and silent
reset are denied.

## Durable storage boundary

The namespace directory is a real mode-`0700` directory and the document a
real mode-`0600` regular file. Reads are size-checked, opened with
`O_NOFOLLOW` on Unix, identity-checked and capped at 512 KiB. Unknown JSON
fields, bad signatures, scope or path substitution, generation gaps, symlinks,
non-regular files and oversized documents fail unavailable. Only the
owner-confirmed, desktop-signed reset flow may replace a corrupt document;
ordinary open or setup never recreates it.

The runtime pairing document and desktop V3 document require the verified
challenge, attestation and identity digests in every live state and in the
revocation tombstone. Pending rotations revalidate their previous-key
authorization across restart. V1 development documents fail closed.

Writers serialize in-process and through a private advisory lock, reload the
latest document, mutate a clone, validate the whole state machine, then write
mode `0600` with atomic rename and parent fsync, so authority is never only in
memory. Decoded signing keys, `AppMacosPairingSecret`, the signing-key hex of
a dropped `AppMacosPairingDocument` and the serialized document buffer are
zeroized on drop; the stored document keeps the key until revocation, which
persists no signing key.

See [Apps macOS host owner](app-macos-host.md) for the action permit, physical
identity, cancellation, evidence and settlement path.
