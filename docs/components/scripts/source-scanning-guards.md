# Source-scanning guards, and the assumption that broke nine of them

Several guards assert properties of the *source text* rather than runtime
behaviour: which capability a file may reach, whether production code calls a
forbidden helper, whether a boundary is still lazily constructed. They separate
production code from test code and scan only the former.

## Finding the test module

Test modules are gated either `#[cfg(test)]` or
`#[cfg(any(test, feature = "test-fixtures"))]` (the latter lets fixtures cross
crate boundaries). Splitting on the exact literal `#[cfg(test)]` misses the
second form, returns the whole file, and the guard then scans its own test
module — failing loudly for a reason unrelated to what it checks. Match the
attribute's **opening** instead:

```python
# Python
line.lstrip().startswith(("#[cfg(test)]", "#[cfg(any(test"))
```

```rust
// Rust — earliest of either form, take the prefix
whole.find("\n#[cfg(test)]").into_iter()
    .chain(whole.find("\n#[cfg(any(test"))
    .min().map(|at| &whole[..at])
```

Whole-file guards are the exception: `scripts/storage_catalog_guard.py` never
truncates, because a large store file can have a test helper in the middle and
production `Connection::open` after it. Its allowlist is whole-file and two-way:
a listed path that no longer opens a database must be removed, and a new open
must be named against a catalog owner.

## Typed storage boundaries (Task 21)

`scripts/check_typed_storage_boundaries.py` is the program-wide implicit-path
ratchet. It scans production Rust whole-file for:

- ambient `StorageRuntime` / `LocalStorage` / `S3ObjectStore` / `SqlitePool`
  construction and `StorageRuntime::install`
- DuckDB `read_parquet(` globs
- direct object-store SDK symbols
- `default_storage_base_path` / runtime-root env resolution

Roots: the Magician workspace crates plus `magician-storage-gate1`,
`document-to-markdown-cli`, `kindle`, and the structured-decision crates
(`magician-decision`, `decision-engine`, `decision-engine-contract`). The
decision engine's own runtime-root resolution (`decision-engine/src/main.rs`,
its settings file and socket) is a reviewed exact-file entry;
`magic-supervisor` learns the socket from `decision-engine --print-socket`
rather than resolving the root itself. Manifest `rglob("Cargo.toml")` catches
object-SDK and remote-crate deps. Exact files in
`scripts/typed_storage_boundary_allowlist.yaml` are two-way; prefix trees are
reviewed adapters and kits. `magician-bin` must not depend on
`magician-storage-s3`, `magician-storage-state`, or
`magician-storage-migration`. Raw `Connection::open` stays on the catalog
guard. The self-test writes a temporary bypass fixture that must fail.
`scripts/docs_guard.py` and `make check-all` both run it.

## Skillshub runtime-root linter (Task 16A)

Rust guards cannot see Python or shell skills.
`scripts/skillshub_runtime_root_linter.py` scans `skillshub/**/*.py`, `*.sh`,
`*.js`, `*.ts`, and `skillshub/Makefile` for direct `MAGICIAN_ROOT_DIR` /
`MAGICIAN_STORAGE_PATH` resolution. The approved shim is
`skillshub/scripts/runtime_root_shim.py`; the operator `skillshub/Makefile` is
allowlisted as install bootstrap. Its self-test writes a bypass fixture that must
fail; `scripts/docs_guard.py` runs it in the same CI and pre-commit gate.

## Extracted-library evidence

`check_phase5g_conformance.py` keeps its manifest and symbol claims for runtime
evidence files that now live in MagicRun, resolving them against the exact
consumed MagicRun pin. It requires a clean, prepared checkout and fails on
missing or mismatched source; it never fetches or skips evidence. The Makefile's
`setup-extracted-libraries` prerequisite may fetch the pinned source first.
`check-all` also includes `check-extracted-libraries`, which runs the source
ratchets owned by MagicRun/MagicVault.

## Marker exemptions

`check_service_name_boundary.py` allows bounded technical uses of the backend
name:

- **Hash domain separators** — `hasher.update(b"magician.chat.…v1\0")`. They
  never reach a user or a model and must *not* be renamed: the literal is blake3
  input, so changing it invalidates every id derived from it.
- **Literal CLI invocations** (`magician app`, `magician storage`,
  `restart-magician`, `stop-magician`), so operator copy can name the real
  command without personifying the backend.

## Feature unification and dead-code warnings

`magician-api` and `magician-comms` list
`magician = { features = ["test-fixtures"] }` in their **dev-dependencies**.
Cargo unifies features across a `--workspace` build, so every item behind
`#[cfg(any(test, feature = "test-fixtures"))]` compiles into magician's plain
library target, whose callers live in other crates' test binaries — thousands of
spurious dead-code warnings. The crate root therefore carries
`#![cfg_attr(all(not(test), feature = "test-fixtures"), allow(dead_code,
unused_imports))]`, applying only when the feature is on and this is not
magician's own test build.

That hides real dead code in the unified build, so `check-all` also runs
`cargo check -p magician --lib` (feature off) — the only place real dead code in
magician's production source surfaces.

A crate that gates code on `feature = "test-fixtures"` must declare the feature
and propagate it to what the gated code uses (for `magician-comms`:
`dep:wiremock` and `magician/test-fixtures`). An undeclared feature is never true
outside `cfg(test)`, so those fixtures are unreachable from other crates.

## Symbol-moving refactors

Guards that grep for symbols break whenever the code they scan moves. The
LLM-trace phase script `test_llm_trace_phase1.py` reads
`magician/src/magician_v2/execution/agentic/run_loop/state.rs`
alongside the executor, because the run's ambient-identity `current_*` fields
live there, not on `ActionExecutors`. `check_rank_recompute_semantics_drift.py`
points at `magician/src/magician_v2/attention/learning/rank_recompute.rs`.

## If you add one

Prefer asserting behaviour over text. When text is the only handle, match on
something a refactor cannot quietly invalidate:

- **A marker that stops matching does not fail safe** — it silently widens the
  scan.
- **A guard that cannot find its subject must fail as loudly** as one that finds
  a violation.
- A grep cannot tell "moved" from "deleted". When one fails after a refactor,
  find where the symbol went before relaxing the assertion; a guard loosened
  until it passes vacuously is worse than none.
