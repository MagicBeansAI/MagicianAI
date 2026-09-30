# Magician Agent Memory

The active runtime stack is:

- `magician`
- `magicutor`
- `magic-supervisor`

Shared crates: `tool-runtime-core`, `runtime-core`.

Primary docs: `README.md`, `docs/quickstart.md`, `docs/testing.md`,
`docs/README.md`, `docs/process/documentation-governance.md`,
`docs/process/ai-doc-hooks.md`.

## Project Defaults

- Use `docs/README.md` as the docs entrypoint.
- Live runtime config is outside git at `$MAGICIAN_ROOT_DIR/magician-config.yaml`
  (default `~/MagicianNotes`).
  During this dev phase, repo-root `magician-config.yaml` and `llm-router.yaml`
  are the git-backed dev/package seeds (there is no separate template copy).
  When adding/editing config properties, defaults, profiles or operation
  mappings, update both surfaces: the live runtime file and its repo-root
  seed.
- The Skills page's Environment tab is a sanctioned UI writer of the
  runtime env files (`/api/magician/v2/runtime/env`, setup-token gated,
  values write-only) alongside the setup scripts; per-skill `config/.env`
  edits ride `/skills/catalog/{name}/env`.
- Live runtime env files are outside git at `$MAGICIAN_ROOT_DIR/.env`
  and `$MAGICIAN_ROOT_DIR/.env.development`. When adding or rotating
  runtime credentials/default env values needed by the running service, update
  both live env files when applicable and mirror non-secret guidance in
  `magician_data_v3/.env.example`.
- Never spawn a child with a bare program name while touching its `PATH`
  (`env("PATH", ..)`, `env_remove("PATH")`, or `env_clear()` alone): Rust
  then forks instead of `posix_spawn`, and a forked copy of this process can
  hang in macOS atfork handlers before exec. Resolve the program with
  `runtime_core::process::resolve_program` against the PATH the child will
  actually receive — every spawn site that overrides `PATH` does, and a
  model- or config-supplied env map that may carry `PATH` (bash step env,
  bot `config.env`, skill `config/.env`, pack/template `env`, verification
  `spec.env`) counts as an override. A bare name the resolver cannot find
  comes back as a path under the first PATH dir, so a missing CLI fails with
  not-found instead of forking.
- Keep docs fresh with `scripts/docs_guard.py` and rules in `docs/docs_guard_rules.json`.
- For releases, run `make check-all`, then `make build-all-release` (the old
  `build-all-release-verified` target no longer exists).
- For local development and runtime validation, use `make build-all-debug`.
- Prefer `Makefile` targets whenever equivalent commands exist (`make help` first).
- For compile checks, use `make check-all` instead of build targets for quicker feedback.
- Run `build-*` targets only when binaries are needed.
- Run Rust checks, builds, tests, clippy, docs, and other Cargo-backed commands
  through the `Makefile`, which exports `CARGO_TARGET_DIR` (an external build
  volume when one is mounted, else `./target`). When running raw `cargo`
  commands, set the same `CARGO_TARGET_DIR` explicitly so artifacts land in
  one place.
- On this development machine, keep builds on `/Volumes/build/magician/`:
  set `CARGO_TARGET_DIR` and compiler `TMPDIR` there, and place isolated build
  source snapshots there too. Use a separate output directory on SSD1 when
  concurrent agents need independent builds; do not fall back to the internal
  disk while SSD1 is available. Isolated target dirs let Cargo run in
  parallel — they do not cap RAM. rustc has no RSS limit; this host OOMs
  when two or three unconstrained `cargo` invocations each spawn `hw.ncpu`
  compilers. Host-local `~/.cargo/config.toml` sets `build.jobs = 4` and
  `build.rustc-wrapper` to `scripts/rustc-job-gate` (installed at
  `~/.cargo/rustc-job-gate`) so concurrent agents share a machine-wide cap
  of 4 compiling rustc processes. Makefile-driven builds pick that wrapper
  up automatically. Override with `CARGO_BUILD_JOBS=N` /
  `RUSTC_JOB_GATE_SLOTS=N`. Do not pass `cargo -j` above that cap. Avoid
  `FAST=1` (`-Zthreads`) and concurrent `--release` (thin LTO) while
  another agent is already compiling.
- Test-coverage artifacts follow the same rule: `COVERAGE_BASE_DIR` defaults
  beside the build dir and the repo-root `coverage/` is a gitignored symlink to
  it. Clickable `coverage/…/latest.html` links still work; override
  `COVERAGE_BASE_DIR=…` to relocate all lanes.
- The repo-root `target/` is a symlink to the build volume too, created by
  `make link-target-dir` (once per clone, like `make setup-hooks`). An editor
  setting cannot catch every writer: `.vscode/settings.json` already points
  rust-analyzer at SSD1, and the internal disk still filled because a plain
  `cargo` run in the repo — one that forgot `CARGO_TARGET_DIR` — wrote 16 GB to
  `target/debug`. The symlink catches any writer. `.gitignore` lists `/target/`
  with a trailing slash, which matches a directory but not a symlink, so add
  `/target` to `.git/info/exclude` rather than editing the shared ignore file.
  A full internal disk does not fail loudly: `replace-restart-magician` needs
  ~1.1 GB for its temp copy, and when that copy fails the restart silently does
  not happen while `/health` keeps answering 200 from the old process.

## Hook Setup

- Run `make setup-hooks` once per clone/worktree.
- Prefer creating worktrees via:
  `make worktree-add WT_PATH=../my-worktree WT_BRANCH=my-branch WT_BASE=origin/main`

## Docs Freshness Workflow

- Staged docs enforcement: `python3 scripts/docs_guard.py --staged`
- Reminder-only check: `make docs-remind`
- Behavior-bearing code changes must include matching docs updates in owned paths.

## Browser Automation

- Prefer `agent-browser` CLI for browser automation tasks.
- Before using it for a new task, run `agent-browser skills get core --full`.
- Use the snapshot/ref workflow: `open`, `snapshot -i`, interact with `@eN` refs, then re-snapshot after page changes.
- Use `--session <name>` or `--session-name <name>` when state should persist across commands.

## Codebase Navigation

- For most code search, architecture lookup, and code understanding tasks, start with codegraph MCP tools before raw file search.
- Before using `rg` for structure, symbol, capability, endpoint, or crate discovery, use the relevant `cgraph_*` tool first.
- Use `cgraph_search` for keyword lookups across the graph.
- Use `cgraph_detail` to inspect what a crate contains.
- Use `cgraph_skills` to inspect available skills and tool packs (`cgraph_capabilities` is retired).
- Use `cgraph_endpoints` to inspect API routes and handlers.
- Use `cgraph_how` for architecture blueprints and subsystem explanations.
- Fall back to `rg`, globbing, or direct file reads only when codegraph does not answer the question or when exact text matching is the actual task.

## Policy

When behavior-bearing code changes, update owned docs as defined in `docs/docs_guard_rules.json`.
Never delete anything under `magician_data_v3/`, `data/`, or the runtime data root without explicit confirmation.

## AI Hooking

Docs-reminder hooks are non-blocking (`docs_guard.py --working-tree --remind-only`).
`DOCS_HOOK_DISABLE=1` silences all of them. Manual: `make docs-remind`.
Details: `docs/process/ai-doc-hooks.md`.

- Claude: `.claude/settings.json` → `scripts/claudecode_docs_hook.sh`
- Codex: home `~/.codex/config.toml` `notify`, or `scripts/codexhook`
- Grok: `.grok/hooks/docs-remind.json` → `scripts/grok_docs_hook.sh` (needs `/hooks-trust`)
- Gemini CLI: `.gemini/settings.json` `hooks` → `scripts/docs_remind_hook.sh gemini`
- Antigravity (`agy`): `.agents/hooks.json` → `scripts/docs_remind_hook.sh agy`
- ZCode: `.zcode/config.json` `hooks` → `scripts/docs_remind_hook.sh zcode`

Codegraph MCP: `.mcp.json` (Claude/Grok), `.codex/config.toml`,
`.gemini/settings.json`, `.agents/mcp_config.json` (`agy`), `.zcode/config.json`.
