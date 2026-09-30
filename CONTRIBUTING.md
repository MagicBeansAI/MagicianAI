# Contributing

## Development Setup
- Guided install: `make setup-wizard` asks which capabilities you want and works
  out what has to be installed for them, in what order, and what declining any
  of it costs. `make setup-wizard ARGS=--status` only looks and prints, which is
  also what it does when there is no terminal to draw on. It builds itself on
  first run and is silent afterwards. Components that this machine cannot run —
  a local generation model needs 16 GB on Apple Silicon — are refused with the
  reason rather than offered and then failed.
- Operator identity: `make setup-identity` writes your name, principal, the
  agent's own inbox/WhatsApp number, and the public-ingress choice into the
  data root's `.env` and `skillshub/operator-config.yaml` — the untracked
  identity layer. No identity value has a code default; run this once per
  machine (details: `docs/components/scripts/README.md`). The tunnel/Access
  scripts require `MAGICIAN_TUNNEL_ZONE` from this layer (no zone is ever
  defaulted), and the tracked `.cargo/config.toml` no longer pins a
  `target-dir` — build location comes from `CARGO_TARGET_DIR` (the root
  `Makefile` exports it; set it yourself for raw `cargo` runs). rustc has
  no RSS cap; Cargo `jobs` is per-invocation. On this 32 GiB host, install
  `scripts/rustc-job-gate` as `~/.cargo/rustc-job-gate` and pin
  `build.jobs = 4` plus `build.rustc-wrapper` in `~/.cargo/config.toml` so
  concurrent agent builds share four compiling rustc processes instead of
  multiplying `hw.ncpu` each. Details:
  `docs/components/scripts/README.md`.
- Fresh macOS checkout: run `make setup-prerequisites` before the first build.
  This installs the local build prerequisites, including the `protoc` compiler
  required by LanceDB's Rust build scripts and `ffmpeg`/`ffprobe` required by
  the media-edit runtime and its subprocess tests.
- The test-suite runner and tool-result-projection evaluator use an isolated
  Python 3.10+ environment with a fully pinned dependency closure. `make
  test-suite-runner` provisions it automatically; use `make
  setup-test-suite-runner-deps` to prepare it ahead of time.
- Build: `cargo build --workspace`
- Desktop JavaScript tooling: `make setup-desktop-pnpm` installs the exact
  `desktop/package.json` pnpm pin under the gitignored `.cache/desktop-pnpm`.
  Desktop build/test/release targets invoke it automatically, including on
  Node installations that do not bundle Corepack. The desktop CI workflow runs
  the bootstrap tests and the same setup target, so update the manifest pin
  rather than hard-coding a separate pnpm version in CI.
- Native desktop wake word (macOS): `make setup-desktop-vosk`
  (`scripts/setup-desktop-vosk.sh`) fetches `libvosk` + the small Vosk model
  (gitignored) into `desktop/src-tauri/`. It runs automatically as a prerequisite
  of `build-desktop-tray` / `release-desktop`, so the desktop links + bundles
  cleanly; for raw dev `cargo` runs export `MAGICIAN_VOSK_MODEL_DIR`.
- Linux desktop packaging requires `xdg-utils`; Tauri's AppImage bundler calls
  `xdg-mime` while assembling the application directory. The desktop CI image
  installs it explicitly rather than relying on a runner's transitive packages.
- Windows desktop packaging uses a native `actions/checkout` before building
  the NSIS installer. Keep every tracked path Windows-compatible; identifiers
  containing reserved characters belong in file contents, not filenames.
  Branch acceptance builds apply an explicit Tauri config that disables updater
  artifacts and clear updater-signing credentials in the build shell. Tag builds
  retain the private key and password required for updater artifacts.
- Rust checks: `cargo check --workspace`
- Desktop crate: `make check-desktop` (also part of `make check-all`).
  `desktop/src-tauri` is its own workspace, so the root
  `cargo check --workspace` never covered it — which is how a compile error and
  a same-origin trust bug both shipped there while every root gate was green. It
  checks `--all-targets --features native-wake`, needs no `setup-desktop-vosk`
  (only a wake-enabled build links `libvosk`), and skips outside macOS.
- UI checks: `npm --prefix ui/unified-ui run -s check`
- Docs guard (staged): `python3 scripts/docs_guard.py --staged`
- Docs reminder (working tree): `make docs-remind`
- Typed storage boundaries: `make check-typed-storage-boundaries` (also part of
  `make check-all` and the blocking `docs_guard.py` path). New runtime-root I/O,
  Parquet globs, object SDKs, or ambient backend construction must go through
  reviewed adapters listed in `scripts/typed_storage_boundary_allowlist.yaml`.
- Release build: `make build-all-release`
- Removing an install: `make uninstall-magician ARGS=--dry-run` first, then
  without it. One uninstaller serves every route. It stops processes matched by
  executable path under the prefix (never by name), removes the install, and
  asks about the desktop app. Your data root is left alone unless you pass
  `--delete-data` *and* type its full path at a terminal — `--yes` never answers
  that question.
- Backend release artifact: `make package-release` tarballs the built backend
  binaries with the runtime-root seed and the scripts an install needs where
  there is no Makefile, plus a manifest and checksums. It packages only — build
  first, or it names what is missing and stops. The output is unsigned, so macOS
  quarantines it on download; it is not publishable to end users until signing
  and notarisation exist.
- Public site: `make build-marketing-site` → `make publish-marketing-site`.
  Note that `ui/unified-ui/static/` ships **wholesale**, so an asset stays
  deployed long after the last import of it goes — retiring a component does
  not retire its media. The target strips the known-unreachable directories
  explicitly (`vosk`, the retired prologue reels, local generation masters);
  when you retire a scroll sequence, add its directory there. Verify by
  measuring `performance.getEntriesByType('resource')` on a real traverse of
  the page rather than by reading the tree.
- Hook setup (automated): `make setup-hooks`
- Target-dir link (once per clone): `make link-target-dir` points the
  repo-local `target/` at the build volume, so a plain `cargo` command that
  forgets `CARGO_TARGET_DIR` still writes there instead of filling the internal
  disk. It is idempotent and never deletes an existing real `target/` — it
  reports the size and leaves it for you. Editor settings alone are not enough:
  `.vscode/settings.json` already points rust-analyzer at the build volume, and
  the internal disk still filled from a `cargo` run that bypassed it. Because
  `.gitignore` lists `/target/` with a trailing slash (which matches a directory
  but not a symlink), add `/target` to `.git/info/exclude`.
- Worktree creation with auto hook setup: `make worktree-add WT_PATH=../my-worktree WT_BRANCH=my-branch WT_BASE=origin/main`
- Optional AI reminder hooks: see `docs/process/ai-doc-hooks.md`
- Test target: `make test` sets dummy LLM API keys, `MAGICIAN_DISABLE_SYSTEM_PROXY=1`, and a temp `MAGICIAN_STORAGE_PATH`. The proxy override keeps HTTP tests hermetic; normal runtime clients retain explicit, environment, and available platform proxy discovery. Bare cargo test binaries also fall back to OS temp storage when no explicit storage path is set.
- `make test-content-retrieval-eval-harness` also runs each content-retrieval
  skill pack's own Python unit tests via `python3 -m unittest discover`
  (`skillshub/arxiv-search/tests`, `skillshub/structured-web-data/tests`,
  `skillshub/youtube-search/tests`, `skillshub/media-fetch/tests`) — those
  packs live outside the Rust workspace, so nothing else runs their suites.
- Tool skill verification, two tiers:
  - `make test-tool-skills` — hermetic contract checks over every governed tool
    skill. Free, no network, safe in CI. Run it after touching any `SKILL.md`,
    any adapter under `skillshub/*/bin/`, or the governed runtime.
  - `make test-tool-skills-live` — executes each skill's declared canary through
    the real governed runtime. Hits the network and **spends money**; it is
    opt-in and never part of `make test`. Add `TOOL_CANARY_TIER=expensive` to
    include media generation.

  The live lane deliberately drives the production coordinator rather than
  building its own invocation. A harness that assembles its own environment
  drifts from the runtime and reports green against code that no longer exists —
  that is exactly how a rewrite once took a third of the tool surface dark
  unnoticed. If you find yourself hand-rolling a call in a test here, that is
  the signal to route through the real path instead.

## Build performance
The `magician` crate is large; to speed up iteration:
- `FAST=1` on any target compiles the whole workspace on the nightly parallel
  rustc frontend in an isolated `builds-fast` dir, e.g. `make check-all FAST=1`
  or `make build-all-debug FAST=1`. Run `make setup-fast` **once** first — FAST
  switches to nightly, so its coverage/test lanes need `llvm-tools-preview` there
  (a plain `make test FAST=1` fails otherwise). Add `FAST_CRANELIFT=1` for the
  Cranelift codegen backend (opt-in; may fail on `ring`; run `make setup-cranelift`
  once). Coverage lanes are incompatible with `FAST_CRANELIFT` (Cranelift can't
  instrument) — use stable (no FAST) for authoritative coverage.
- Makefile-driven Rust commands use `/Volumes/build/magician/builds` when that
  cache is writable and otherwise fall back to the checkout-local `target/`
  directory. Linked git worktrees add `wt/<name>` below the selected root, so
  parallel worktree builds don't serialize on the shared target-dir lock or
  thrash each other's incremental cache. Override `CARGO_TARGET_ROOT` or
  `CARGO_TARGET_DIR` when a different location is preferred.
- Test reports and coverage artifacts similarly prefer
  `/Volumes/build/magician/coverage` and fall back to `target/coverage`; macOS
  audio and Swift caches follow `CARGO_TARGET_DIR`. Override
  `COVERAGE_BASE_DIR` to relocate all test-report lanes together.
- Tight single-crate loops: `make check-magician-fast`, `make watch-magician`.
- Verify on the stable toolchain (`make check-all`, no `FAST`) before committing.

See the quickstart's "Faster builds & tighter iteration" section and
`docs/plans/2026-07-23-magician-crate-split-design-plan.md`.

## Service Lifecycle
- Start: `make run-supervisor` (raises NOFILE soft limit to 8192, then runs `./scripts/run-supervisor.sh`). Python-using skills resolve `python3` through `skillshub/.venv/bin` because the cli_template dispatcher prepends that to subprocess PATH — `make -C skillshub setup-python` populates the venv, and magician logs a startup warning if it's missing.
- Stop everything: `make stop-supervisor` (depends on `stop-bots`). Sweeps **all** magician-spawned subprocesses, not just the supervisor binary itself — including detached Node bot daemons (`<scope>/bots/<bot>/dist/index.js`), `gws` Google Workspace subprocesses spawned from either the scoped bot tree (`<scope>/bots/<bot>/node_modules/...` e.g. `gws gmail +watch`) or the shared skillshub root (`skillshub/node_modules/.bin/gws auth login ...`), `agent-browser` CLI sessions and the Chrome-for-Testing instances they launched, and reserved patterns for CloakBrowser sessions. Each sweep does SIGTERM → 1s grace → SIGKILL, anchored to magician-owned paths to avoid touching unrelated processes (your own Chrome for Testing for manual debugging, the Vite dev server you started via `make run-ui-dev`, etc.).
- Restart: `make restart-magician` / `make restart-magicutor`.
- Stop one service: `make stop-magician` (also runs `stop-bots`) / `make stop-magicutor`.

## Capability Tools Setup
- Full setup: `make setup-capability-tools` (its steps also run during `make setup-all`)
- Includes: marimo and the Metabase CLI (`metabase-pp-cli`, generated by printing-press)
- Metabase spec refresh: `make -C skillshub refresh-metabase-spec`

## Patched agent-browser fork
- The browser pack runs a Magician fork of agent-browser pinned at `skillshub/browser/_vendor/v0.38.1-Magician.0/` (six download-path patches against upstream `v0.38.1`; sibling to upstream issue #1300). See the vendor directory's `README.md` for the bug analysis.
- Daily use: `make setup-agent-browser` deploys the vendored binary into the live runtime (idempotent, version-checked).
- Fresh machines without a writable `/Volumes/build` build the patched browser
  under the checkout-local `target/agent-browser`; root Makefile delegation
  passes its selected `CARGO_TARGET_DIR` through to skillshub. Override
  `AGENT_BROWSER_CARGO_TARGET_DIR` for a separate browser build cache.
- `setup-agent-browser` also ensures bootstrap PyYAML before classifying and
  materializing skills. It prefers `skillshub/.venv`, falls back to an existing
  system installation, and uses `pip --user` only when `uv` is unavailable.
- Verify the patch is live: `make verify-agent-browser` asserts `--version` reports the magician suffix and the binary's ad-hoc signature is valid. `make test` and `make test-verbose` run it (via `test-rust` / `test-rust-verbose`), so every test run either confirms the patched binary is in place or fails loudly with a remediation hint. Run it standalone any time you suspect a stray `npm install` (or anything else) might have stomped the deploy.
- Editing patches: `make agent-browser-source` clones+applies into `.cache/agent-browser-src-<tag>-magician/` (gitignored), then edit `.rs` files there.
- Rebuild: `make rebuild-agent-browser` runs `cargo build --release`, regenerates the patch from `git diff v0.38.1`, and (when the skill is already installed) auto-redeploys the new binary into `skillshub/browser/node_modules/agent-browser/bin/`. Rust release builds are not byte-deterministic, so only commit the binary when the patch (or upstream tag) actually changed.
- Bumping upstream: change `AGENT_BROWSER_UPSTREAM_TAG` and `AGENT_BROWSER_VERSION` in `skillshub/Makefile`, move the vendor directory to the new version path, then re-run the source/rebuild flow.

## Runtime Services
- `magician` (API/orchestration)
- `magicutor` (browser automation)
- `magic-supervisor` (process lifecycle)

## Runtime Config Seeding

The runtime config is two files, not one: `magician-config.yaml` and its sibling
`llm-router.yaml`, which holds `llm.router.profiles` and `operation_mapping`.
The loader splices the tables in and treats a missing sibling as fatal, so
anything that seeds a runtime root must write **both** — the `make` targets and
the desktop packager seed the tables independently of the config, which is what
covers upgrading a runtime root that predates the split.

Read the config from code through the helpers, never with a plain file read:
`load_magician_config_from_path` in the runtime, `shipped_repo_config_yaml()` in
Rust tests, and `scripts/magician_config_text.py` for Python, shell and Ruby.
The file alone parses cleanly into an *empty* router, so a direct read fails far
from its cause. See
[router tables](docs/components/magician/router-tables-file.md).

## HTTP API Notes
- Main `magician` health endpoint: `GET /health`
- Main `magician` HTTP routes are registered in `magician-bin/src/main.rs` when the Actix `App` is built; modular route helpers under `magician-api/src/` are attached there via `.configure(...)`.

## Core Shared Crates

- `tool-runtime-core` (MagicRun; exact Git pin in root workspace dependencies)
- `magicvault-core` / `magicvault-primitives` (MagicVault; same exact Git revision)
- `runtime-core`

Edit the extracted libraries in their own repositories. Magician keeps compatible
imports/facades and product-owned integration tests. Use `make test-magicrun`,
`make test-magicvault` and `make test-magicvault-compatibility` when changing
those seams.

## Formatting

The tree is stable-rustfmt-clean. `rustfmt.toml` uses stable options only, so
plain `cargo fmt` on the stable toolchain is the formatter; nightly-only options
are deliberately not used. Run `make fmt` before pushing (it covers the workspace
plus `desktop/src-tauri` and `kindle`, which sit outside the workspace); CI
rejects a PR that fails `make fmt-check`. The one-time reformat commit is listed
in `.git-blame-ignore-revs` — run
`git config blame.ignoreRevsFile .git-blame-ignore-revs` once per clone so
`git blame` skips it (GitHub honours the file automatically).


## Pull Requests
- Keep changes scoped and incremental.
- Include validation commands in the PR description.
- Update `README.md` and `docs/` when behavior changes.
- Ensure docs ownership rules pass via `python3 scripts/docs_guard.py`.

## Live evals

Optional live-LLM eval lanes are documented per suite in
`docs/components/scripts/README.md`. Feature-scoped gates also exist as
standalone targets — e.g. `make eval-thinking-map-live` runs the Thinking Map
interpreter's semantic eval against the running server (shadow-only scratch
maps; ~20 mini-class LLM calls, `TM_EVAL_RUNS=1` for a smaller smoke).
