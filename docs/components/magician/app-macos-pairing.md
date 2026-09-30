# Apps macOS pairing owner

The Apps macOS host has no ambient endpoint, executable or signing-key
fallback. A runtime scope must explicitly pair with the native desktop before
the typed `/host/apps/macos/action` transport can become available. Pairing is
stored below the caller-provided system root in a scope-digest-namespaced file;
it never consults `HOME`, an environment variable, a global config file or a
package-provided path.

The authenticated runtime lifecycle is reachable through the scoped
`GET /api/magician/v2/apps/macos-pairing` status route and the bounded
`POST .../setup`, `POST .../advance`, `POST .../revoke`,
`POST .../reset/challenge`, and `POST .../reset` control routes.
Setup accepts only the literal-loopback typed action URL on `127.0.0.1:3017`
and one bundle ID; the runtime derives the logical target reference and keeps
proposal keys and wire records private. Native approval remains a separate
explicit desktop operation. Calling `advance` posts or recovers the retained
exact transition; it cannot mint a different finalization after an uncertain
send.

The shipped native Settings surface is the only owner-code workflow. It shows
the pinned fingerprint and memory-only one-time code, then displays the exact
authenticated scope binding and every `{target_ref, bundle_id}` pair before
approval. The approval command must echo the displayed setup, generation and
canonical review-material digest, and the owner chooses one bounded absolute
CUA source path: the executable inside its signed bundle,
`/Applications/CuaDriver.app/Contents/MacOS/cua-driver` (symlinks such as
`~/.local/bin/cua-driver` are rejected; the whole bundle is staged and bound).
Upgrading CuaDriver changes that identity, so pairings made with an older
driver must be re-approved. Changing displayed material clears confirmation. The UI can
explicitly recover finalization and issue the primary signed generation
revoke; a native emergency revoke is labeled separately for runtime-loss
cases. Runtime control responses are streamed under a 16 KiB UI ceiling rather
than materialized from an unbounded body. No owner code is persisted, logged or
exposed to an App/model action.

## Authenticated lifecycle

The lifecycle begins with a pre-key-disclosure identity gate, followed by
three durable authority states:

1. The runtime sends a 30-second random challenge to the fixed
   `127.0.0.1:3017` desktop endpoint. The challenge binds the owner-entered
   Keychain key ID/public key and digest of the one-time code displayed by the
   native owner. A valid Ed25519 attestation over that exact challenge and the
   desktop-observed host identity must arrive before the runtime creates or
   discloses any pairing HMAC key. The desktop durably retains a one-shot
   attested-bootstrap permit; the subsequent proposal HMAC-binds its exact
   attestation digest and atomically consumes that permit. A bare self-HMAC
   proposal is refused.
2. `Pending`: the runtime creates a random 32-byte key, opaque setup/key IDs,
   the exact scope, typed action URL and requested logical target/bundle pairs.
   The proposal is keyed and bounded. Its custom `Debug` omits key material;
   the exact challenge, attestation and their digests are durably retained.
3. `ApprovedPendingFinalize`: after explicit native approval, the runtime
   verifies both its keyed signature and the pinned desktop Ed25519 signature,
   plus the exact proposal, endpoint, scope, target set, attested host identity,
   CUA binary identity and TCC policy/epoch. This state is not workflow
   authority. The runtime computes the reviewed owner profile and
   implementation digests, signs a finalization over the exact proposal and
   approval digests, and durably retains it before sending it.
4. `Active`: only an acknowledgement carrying both the pairing-key signature
   and the pinned desktop-identity signature for those exact finalization bytes
   promotes the generation. Workflows consume only this state. A crash after
   desktop activation is recovered with a fresh 30-second keyed status request
   that binds the exact proposal, approval and finalization digests; it does
   not reinterpret or reconstruct authority.

Setup/approval messages have a ten-minute lifetime; status messages have a
30-second live TTL. This separation is load-bearing: it lets the runtime
recover an already activated desktop generation after a crash without allowing
an expired setup to authorize a new finalization.
An exact finalization durably minted before expiry is verified at its signed
`finalized_at` time and can be delivered and acknowledged after restart/expiry;
acknowledgment wall time is not incorrectly treated as new setup authority.

## Rotation and revocation

A new setup cannot replace a live pending/approved stage. When an active
generation exists, a replacement proposal carries the previous generation and
is signed with the previous active key as well as its new key. The current
active snapshot remains usable while native review and finalization of the
rotation are incomplete, and is replaced atomically only after the signed
desktop acknowledgement. V1 rotation must retain the exact pinned desktop
identity. Replacing that Keychain identity requires a separately reviewed
prior-key cross-sign or the explicit two-store reset protocol; it is never
inferred from a new native approval.

Revocation requires the exact current generation and a fresh runtime-signed
capability. The desktop first verifies it, durably writes a keyless signed
`Revoked` acknowledgement, and only then removes the physical verifier and
key. The runtime verifies that exact acknowledgement while it still retains
the proposal key, removes active and pre-active material, advances the
generation high-water mark, and writes its own keyless tombstone containing
the acknowledgement digest. A lost response is recovered by exact signed
status; unknown setup IDs never masquerade as revocation. Old approvals,
finalizations and acknowledgements cannot re-enter after a rotation or revoke.
A finalization-bearing uncertain stage is never automatically replaced: the
caller must recover the desktop status or explicitly revoke it.

A desktop `Revoked` status for the exact signed transition is also applied as
a durable runtime tombstone. During rotation that clears both the replacement
transition and the previous active generation, so native revocation cannot
leave stale workflow authority live.

The bootstrap boundary is load-bearing. Literal `127.0.0.1`, redirect refusal
and keyed handshake responses alone do not authenticate the first listener,
because the proposal itself carries the new key. The one-time owner code and
pinned Keychain-backed Ed25519 challenge establish that independent proof
before key disclosure, and every approval/finalized/revoked response is bound
to the same identity. Installation review admits the eight sealed catalog-Ready
macOS host actions (`launch`, `focus`, `snapshot`, `click`, `type`, `key`,
`scroll`, `drag`); raw macOS actions remain denied.

## One-sided store-loss reset

Native revoke intentionally preserves a monotonic desktop generation floor.
If the runtime store is then lost, a silent generation-1 setup remains denied.
The trusted UI requests a 120-second one-shot runtime reset challenge and
displays its exact scope/nonce beside the pinned desktop identity, host identity
and old floor. Only explicit owner confirmation asks Keychain Ed25519 to sign
`AppMacosHostPairingResetAck` over that material. The desktop durably stores
`{challenge, acknowledgment}`, clears live/revoked key material and preserves
the floor before returning the ack. The runtime verifies its retained challenge
and the exact owner-displayed signed identity/host/floor, rejects any Active or
transition state, and pins those values in a keyless reset anchor before
accepting another setup. The next proposal must be above the preserved floor.
The runtime challenge is durably marked consumed after the reset anchor commits,
so deleting the main document cannot make a captured old acknowledgement usable
again. Exact anchored acknowledgements remain replayable after crash or expiry;
mismatches, floor lowering and silent reset are denied.

## Durable storage boundary

The namespace directory is a real mode-`0700` directory and the document is a
real mode-`0600` regular file. Reads are size-preflighted, opened with
`O_NOFOLLOW` on Unix, identity-checked and capped at 512 KiB. Unknown JSON
fields, corrupt signatures, scope/path substitutions, generation gaps,
symlinks, non-regular files and oversized documents fail unavailable. Only the
explicit owner-confirmed, desktop-signed reset flow may replace a corrupt
regular pairing document; ordinary open/setup never recreates it silently.

The runtime pairing document and desktop V3 document require the exact verified desktop
challenge, attestation and identity digests in every live state and in the
signed keyless revocation tombstone. Pending rotations retain and revalidate
their previous-active-key authorization across restart. V1 development
documents fail closed rather than being silently reinterpreted.

Writers serialize both inside the process and through a private advisory lock.
They reload the latest document while locked, mutate a clone, validate the
whole state machine, then use the shared durable writer to apply mode `0600`
before atomic rename and fsync the parent directory. Authority is therefore
never published only in memory. Serialization/read buffers and dropped
document snapshots zeroize clear key bytes where their owned representation
allows it; revocation persists no signing key.

See [Apps macOS host owner](app-macos-host.md) for the action permit, physical
identity, cancellation, evidence and common settlement path.
