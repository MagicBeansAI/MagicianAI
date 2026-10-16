# Crate-root lint configuration

`magician/src/lib.rs` carries two crate-level attributes. Both exist for a
reason that is not obvious from the line itself.

## `#![recursion_limit = "256"]`

datafusion/sqlparser AST types are deeply recursive. Proving `Send`/`Sync` for
large `tokio::spawn` futures that transitively touch a `LogicalPlan` overflows
the default auto-trait recursion limit of 128.

## `#![cfg_attr(all(not(test), feature = "test-fixtures"), allow(dead_code, unused_imports))]`

Test fixtures are not dead code, but the **library** build cannot see that.

`magician-api` and `magician-comms` list
`magician = { features = ["test-fixtures"] }` in their dev-dependencies. Cargo
unifies features across a `--workspace` build, so the feature switches on for
magician's own plain lib target too — and every item behind
`#[cfg(any(test, feature = "test-fixtures"))]` compiles into a target whose
callers live in *other crates' test binaries*. rustc cannot see across that
boundary, so it reports each one as never used.

The effect is large: **55 warnings become 2,631.**

The suppression is deliberately narrow — it applies only when the fixtures
feature is on **and** this is not magician's own test build, which is precisely
the configuration where "unused" carries no information, because everything
test-only is unused by construction.

### The cost, and how it is paid

42 of the 55 genuine warnings are `dead_code`/`unused_imports`, so this attribute
hides them in the unified build — and would hide *new* production dead code
there too. `make check-all` therefore runs `cargo check -p magician --lib`
alongside the workspace build. The feature-off build is the only place real dead
code in magician's production source appears, so it has to run somewhere CI
watches.

If you are looking for dead code in this crate, use that command, not the
workspace build.
