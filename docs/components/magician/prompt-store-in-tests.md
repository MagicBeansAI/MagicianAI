# The prompt store in test binaries

`magician`'s tests never run the startup builder that installs the process-global
`PromptManager`, so any code path reaching for a managed prompt would fail on
wiring rather than on its subject.

`magician/Cargo.toml` therefore carries, in `[dev-dependencies]` **only**:

```toml
magician-core = { path = "../magician-core", features = ["test-support"] }
```

which lets `required_prompt_manager` fall back to the on-disk store at
`data/magician_v2/prompts` (resolved from `CARGO_MANIFEST_DIR`, so it does not
depend on the working directory).

## Why this is a dev-dependency and must stay one

The production behaviour is the opposite and must not change: a release build
that reaches for a prompt without an installed manager should fail loudly, not
quietly read a development directory. `cargo build` does not resolve
`[dev-dependencies]`, and the workspace sets `resolver = "2"`, so the feature
cannot unify into a normal build.

Verify with `cargo tree -p magician -e features`: the `[dependencies]` edge
resolves `magician-core feature "default"` alone; only the `[dev-dependencies]`
edge carries `test-support`.

## What this means when you write a test

Tests render the **real** templates from the store rather than a stub. That is
deliberate — the store is the source of truth for prompts, so a template that
stops rendering shows up as a failing test. It also means a test can depend on
prompt content without saying so; if you need a specific template, assert on it.

Background: `docs/components/magician-core/README.md`, *The `cfg(test)` trap*.
