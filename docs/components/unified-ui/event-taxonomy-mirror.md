# The event-taxonomy mirror

`ui/unified-ui/src/lib/realtime/event-taxonomy.ts` is **generated**, not written.
It mirrors the Rust event taxonomy so the web client and the backend cannot
disagree about what an event is called or what it carries.

## How it stays honest

The generated file embeds a `SOURCE_HASH` over its three Rust sources:

- `magician/src/magician_v2/realtime_events.rs`
- `magician-event-taxonomy/src/lib.rs`
- `magician-event-taxonomy/src/bin/event_taxonomy_dump.rs`

`make event-taxonomy-check` recomputes that hash and fails when it differs, which
is a **prerequisite of `make test-rust`** — so a stale mirror stops the Rust
suite before a single test runs. That is deliberate: a client decoding events by
a taxonomy the server no longer speaks fails at runtime, in front of a user,
rather than in CI.

The repository-wide `make check-all` gate includes the same freshness check, so
backend-only realtime refactors must regenerate the mirror even when the emitted
event vocabulary itself is unchanged.

To regenerate after changing any of the three sources:

```
make event-taxonomy-codegen
git add ui/unified-ui/src/lib/realtime/event-taxonomy.ts
```

## When only the hash changes

A regeneration whose diff is a single `SOURCE_HASH` line means the taxonomy's
*content* was already in sync and only the stamp had drifted — a Rust source was
edited in a way that did not change any event. That is normal after a
refactor-only commit, and it is still worth committing: until the stamp matches,
the gate stays red for everyone.
