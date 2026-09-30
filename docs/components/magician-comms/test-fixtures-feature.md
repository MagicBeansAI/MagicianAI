# The `test-fixtures` feature

`magician-comms` gates 78 sites on `#[cfg(any(test, feature = "test-fixtures"))]`
so its fixtures can be shared with satellite crates' test builds — the same
pattern `magician` uses.

The feature must be declared in `Cargo.toml`: an undeclared feature can never be
enabled, so the arm is only true under `cfg(test)` and the fixtures are
unreachable to other crates. rustc flags that as `unexpected cfg condition
value` (see `docs/components/scripts/source-scanning-guards.md`).

## Both entries in the feature list are load-bearing

```toml
test-fixtures = ["dep:wiremock", "magician/test-fixtures"]
```

Neither was obvious from reading; each surfaced only by compiling with the
feature switched on.

- **`dep:wiremock`** — the gated blocks that first required it lived in
  `channel_assist/centrality.rs`; that file moved lib-side with the resurfacing
  engine, where `magician`'s own
  `test-fixtures` feature now supplies wiremock. The entry is retained so the
  feature's build surface is unchanged; `wiremock` remains an `optional`
  regular dependency and the dev-dependency stays so plain `cargo test` is
  unaffected.
- **`magician/test-fixtures`** — the comms-side resurfacing modules
  (`actions.rs`, `interaction.rs`) import
  `magician::magician_v2::test_support`, which exists only when magician's
  own fixtures are on, and the lib-resident engine's fixture blocks need the
  propagation too. **A feature that does not propagate to its dependency is a
  feature that cannot compile.**

Verify with:

```
cargo check -p magician-comms --features test-fixtures
```

That command is the point. A declaration that merely silences the warning would
leave the feature exactly as inert as no declaration at all, while looking fixed.
