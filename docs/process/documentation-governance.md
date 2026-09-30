# Documentation Governance

## Policy

Any behavior-bearing code change must include a docs change in an owned documentation path.

Ownership mapping is defined in [`docs/docs_guard_rules.json`](../docs_guard_rules.json).
Swift sources and Magios automation scripts are part of the `magios` ownership
rule; their behavior changes must update `docs/components/magios/**`, the
Magios README, or its changelog just like Rust and web changes do.

The standalone macOS audio engine is governed separately by the
`native-macos-audio-engine` rule. Changes there must update its README or the
owned Fluid Audio/realtime-media documentation.

The native macOS speech helper and meeting-audio helper
(`native/macos-speech-helper/`) are covered by the
`native-macos-speech-helper` rule; changes must update its README, the desktop
component docs, or the realtime-media documentation.

Magician runtime skills under `skillshub/` are governed by the `skillshub`
rule. Their executable helpers and typed schemas are owned by the co-located
`SKILL.md`, Skillshub README/changelog, and the Magician skill specification.
Repository-local coding-assistant bundles under `.agents/skills/` are not a
runtime installation surface.

Selected schema-only public-contract baselines under
`magician/tests/fixtures/tool_runtime_legacy_contracts/` are test data, not an
executable runtime surface. The former Phase 7 skill, wrapper, and migration-tooling
trees were removed after closure. Active replacements still require their normal
owned docs.

## Enforcement

The root `Makefile` keeps the release gates in their owned order: source and
documentation guards run before the workspace/all-target Rust check, the
standalone Magician library check, Unified UI diagnostics, and the Magios
compile gate. Release commits should use that `make check-all` surface instead
of selecting only the inexpensive subchecks.

- Local pre-commit: `.githooks/pre-commit` runs `python3 scripts/docs_guard.py --staged`
- CI: `Docs Guard` workflow runs `python3 scripts/docs_guard.py`
- The same docs-guard gate runs `scripts/skillshub_runtime_root_linter.py`,
  which rejects new skillshub resolution of `MAGICIAN_ROOT_DIR` /
  `MAGICIAN_STORAGE_PATH` outside `skillshub/scripts/runtime_root_shim.py`,
  and `scripts/check_typed_storage_boundaries.py`, the Task 21 ratchet for
  runtime-root I/O, Parquet globs, object SDKs, and ambient backend
  construction outside `scripts/typed_storage_boundary_allowlist.yaml`.
  `make check-typed-storage-boundaries` is also part of `make check-all`.
- Agent reminders: Claude/Codex/Grok/Gemini/agy/ZCode hooks run `docs_guard` in reminder mode (see `ai-doc-hooks.md`)
- Generated-and-committed artifacts are gated by `scripts/codegen_drift.py`,
  which embeds a `SOURCE_HASH:` of their sources in the generated file and
  fails the build when the source has moved and the artifact has not. Two
  consumers: `make event-taxonomy-codegen` for the TypeScript event mirror and
  `make component-graph` for the component-graph page. Committing these rather
  than gitignoring them is what lets them be read without a build; the gate is
  what stops them going stale. Both gates run from `make check-all`,
  `make test-rust` and the release builds — `check-all` most importantly, since
  that is the target contributors are pointed at, and a gate missing from it is
  a gate most runs never reach. The two artifacts today are
  `ui/unified-ui/src/lib/realtime/event-taxonomy.ts` and
  `component-graph.html` at the repository root; `make help` names each
  command with its destination. See `docs/components/scripts/README.md`.
- `make check-changelogs` fails when a per-crate `CHANGELOG.md` exceeds its
  retention limit, and `make trim-changelogs` brings it back under. Changelogs
  are append-only and nothing ever removed from them, so unattended they reach
  tens of thousands of lines, bury the recent entries a reader wants, and ship
  that bulk to the public mirror. Trimmed entries move to
  `docs/archive/changelogs/`; they are not deleted, they stop being published.
  The repository-root `CHANGELOG.md` is the curated product release log and is
  skipped. See `docs/components/scripts/README.md`.
- `scripts/check_notes_capture_surface_drift.py` guards the two selection-capture
  literals that cross a language boundary — the save-to-notes action id shared by
  the desktop crate and the contextual-assist overlay, and the default capture
  scope shared by the browser extension and the web UI's scope store, and the
  search result cap shared by the backend clamp and the `/notes` "Show more"
  ceiling. No language can import another's definition and every one of these
  drifts is silent — a renamed id sends a button to the model instead of to
  Notes, a diverged scope files captures where the owner never looks, and a page
  ceiling above the backend's leaves "Show more" promising results that never
  arrive. The guard runs from `make check-notes-capture-surface` and as part of
  `make check-all`.
- `scripts/setup-magdroid-build.sh` (`make setup-magdroid-build`) installs the
  Android toolchain a plain checkout lacks: JDK 21, the SDK, a `gradle@8` keg
  that AGP 8.x can actually run on, and the `gradle-wrapper.jar` upstream never
  committed. It is idempotent and each step checks before installing. The SDK and
  the Gradle cache are placed on the SSD and reached through symlinks, because
  GUI tooling reads those home paths directly and never sees a shell export.
- `scripts/check_device_bridge_protocol_drift.py` guards the Android bridge's
  asymmetric MCP boundary: bounded Kotlin server methods/annotations and the
  explicit-discover rmcp duplex client in Rust. It also fails if the deleted
  private envelope, HTTP listener, auth/PIN manager, loopback helper, or
  `android_tools` pack returns. Runs from `make check-device-bridge-protocol` and
  as part of `make check-all`.
- `scripts/check_ollama_single_chokepoint.py` rejects hand-built Ollama
  generation, chat, and embedding HTTP requests outside the reviewed
  `magicllm` provider boundary. It understands production-vs-test cfg blocks,
  scans every workspace crate, and runs through `make check-ollama-chokepoint`
  as part of `make check-all`; changing its allowlist or endpoint vocabulary is
  therefore a workspace-automation behavior change that must update this
  governance contract.
- `scripts/storage_catalog_guard.py` treats
  `docs/components/magician/storage-catalog.yaml` as the source of truth for
  storage-owner identity, class, tier, and readiness. The `/storage` snapshot
  in `magician-comms` and the test-fixtures copy in
  `magician/src/magician_v2/storage_governance/mod.rs` are a projection keyed
  by `governance_id`. The guard fails on schema violations, inventory drift,
  unlisted on-disk `Connection::open` sites, and a readiness state that is
  missing any predecessor evidence key. Runs from
  `make check-storage-catalog` and as part of `make check-all`.
- `scripts/check_store_durability_adoption.py` ratchets adoption of the
  durable-write and tolerant-read helpers this repo already has. A 2026-08-11
  review of ~45 filesystem-backed stores found that the correct implementations
  all exist — `artifact_v2/io.rs`'s uuid-temp + `sync_all` + parent-dir sync, and
  `workspace.rs`'s `read_committed_jsonl_path` — and are each used by one or two
  stores while their neighbours hand-roll a weaker version. It is an adoption
  problem, and the failure mode is reaching for `std::fs::write` because it is
  right there.

  The guard counts three exact signatures per file — a fixed temp-file name, a
  direct `fs::rename` caller (a hand-rolled atomic write), and a caller of the
  intolerant `read_jsonl_path` — against a baseline in
  `scripts/store_durability_baseline.json`. Counts, not line numbers: a
  line-numbered baseline goes stale on any edit and then silently stops matching,
  which is the failure a baseline exists to prevent.

  **The ratchet runs both ways.** Exceeding a file's count fails; so does beating
  it, because a count allowed to drift downward silently is no longer a ratchet.
  Fix a store, re-run with `--update`, commit the smaller number. Deliberately
  *not* guarded: "is this write atomic?", which is semantic — phrased as a text
  rule it fires on 460 call sites and teaches everyone to ignore the guard. Runs
  from `make check-store-durability` and as part of `make check-all`. Plan:
  `docs/archive/plans/2026-08-11-store-durability-adoption.md`.

  **Comment lines are skipped.** The rules match identifiers, and a comment
  explaining *why* a store should stop using one of them is not another use of
  it — the guard failed on exactly that the first time a tolerant reader was
  added and documented itself with a doc link to the reader it replaces. The
  counts are call sites, so the definitions of the helpers themselves are
  exempted too.
- `scripts/check_markdown_links.py` ratchets dangling relative links across every
  markdown file in the git index. `docs_guard` enforces that when code changes
  the doc that owns it changes too; it says nothing about whether that doc still
  *points* anywhere. Nothing did, and the gap is not theoretical: archiving a plan
  on 2026-08-11 moved a file and left six links hanging, found only by looking
  afterwards. A doc whose links are dead is worse than a missing doc — it reads
  as authoritative and sends you to a 404.

  Counted per file against `scripts/markdown_link_baseline.json`, counts rather
  than line numbers, **two-way** — same shape and same reasoning as the store
  ratchet above. `--list` prints every dangling target with its source file.
  Renaming a file moves its count to a new key and so reads as a regression;
  that is intended, since a rename is exactly when links break.

  **A target resolves against the git index, not the filesystem.** The guard
  picked sources and targets from two different places: `markdown_sources()`
  read the index, while resolution asked the filesystem with `Path.exists()`.
  It therefore scanned only committed or staged docs but let any file lying in
  a working tree satisfy their links, so an untracked target resolved for
  whoever created it and 404d on every fresh clone — the same
  checkout-dependent verdict the repo-root rule below exists to prevent, by a
  third route. Ten links were resolving that way when it was found. Staged
  counts, since `git ls-files` reads the index: write a doc and the file it
  points at, stage both, and the guard is satisfied before either is
  committed. An **empty directory is dangling**, because git tracks files and
  never directories, so an empty one does not survive a clone.

  **Case counts, at every level.** A target that exists under different case is
  a violation: macOS is case-insensitive and Linux is not, so `x`
  pointing at `foo.md` works on the machine that wrote it and 404s everywhere
  else. Git records paths exactly, so index membership is already
  case-correct — which extends the check to intermediate directories, not just
  a target's final component as the earlier `iterdir` version managed.

  **A leading `/` resolves from the repository root**, matching GitHub's
  rendering. It must not resolve from the filesystem root: the tree had 21
  links hardcoding one author's absolute checkout path, and under
  filesystem-root resolution the guard passed them on that machine and would
  have failed every other clone and worktree — a gate whose verdict depends on
  where the repo is checked out. Under repo-root resolution they are dangling
  everywhere, deterministically. **Fences follow CommonMark**: a line that
  merely starts with three backticks but carries another backtick run is an
  inline code span, not a fence — the first tracker treated it as a toggle and
  silently blanked the 600 lines of `pi-coding-engine-contract.md` after one,
  hiding a real dead link a review later surfaced. A close must match the
  opening character and length. HTML `<a href>` anchors are not scanned
  (documented; one exists outside code spans). 

  Deliberately *not* checked: external URLs (reachability is a network property
  that changes without anyone touching this repo, and a guard that fails on
  someone else's outage gets disabled within a week), `#anchor` fragments
  (resolving them means reimplementing GitHub's heading slugifier, and a guard
  that is wrong about anchors is worse than none), `path/file.rs:137` line
  citations, and links inside fenced code blocks, which are examples of markdown
  rather than markdown. Runs from `make check-links` and as part of
  `make check-all`, which also runs the guard's regression battery
  (`scripts/test_check_markdown_links.py`) — every parser bug a review round
  found is pinned there, the same pairing as `test_service_name_boundary.py`.

- **`scripts/check_phase5g_conformance.py` used to pin working-tree crate
  versions. That was a currency check, not the conformance surface.** The
  guard still freezes the official MCP runner, `rmcp` commit, named evidence,
  and `production_routing_enabled`. Crate `Cargo.toml` versions were extra.

  `magician`'s release version was dropped first (`2a79d78f0`, 2026-08-13): it
  bumps on every feature commit, so the pin asserted "magician is at exactly
  the version we conformance-tested" — false the moment anything shipped.
  Six commits existed only to move that pin; none caught a regression; the
  last turned `check-all` red for everyone before `cargo check` ran.

  `magician-mcp-client` and `tool-runtime-core` stayed because they
  "essentially never move". They do, for reasons that are not MCP
  re-qualification: Darwin memory ceilings (`0.1.68` → `0.1.69` on
  2026-08-15) and the apps catalog (`0.1.69` → `0.1.71`). Each miss aborted
  `check-all` naming "component version drift" rather than the bump that
  caused it — which reads as a broken guard rather than an incomplete commit.

  **Decided 2026-08-20: dropped the rest.** A check that fires only falsely
  teaches people to route around the guard. The attested `component_versions`
  block in the JSON is a different list and stays put unless conformance is
  re-run. Do not re-add a working-tree `Cargo.toml` currency check.

- **`make check-bg-run`** (~13s, also in `check-all`) — regression battery for
  the backgrounded-job pair `scripts/bg_run.sh` / `scripts/bg_wait.sh`, which
  exist because a build here runs 5–16 minutes and gets waited on rather than
  watched. The cases that carry the weight are the kills: SIGTERM, where the
  producer's trap must still record an outcome, and SIGKILL, where it cannot
  and the waiter has to detect the death itself. Both are regressions from a
  real incident (2026-08-13: two waiters ran ~1.5h against builds that had
  already died, because the status write was a statement the kill skipped
  rather than a trap). Mutation-tested: removing the TERM trap does not merely
  lose the status, it reports `EXIT:0` for a killed job — a false success,
  worse than the hang. Contract: [Background Jobs](../components/scripts/background-jobs.md).

  **A count is references, not risk.** Of the `read_jsonl_path` hits it started
  with, half were test call sites asserting current behaviour — one of them
  asserting that the read *fails*. The number tells you where to look; it does
  not tell you what a skipped record costs, and that is per store: a listing
  loses a row, a cost ledger understates spend.
- `scripts/check_phase5g_conformance.py` pins the qualified MCP SDK, the
  official runner, named gate evidence, and `production_routing_enabled`.
  Working-tree crate versions are not in that list; see the entry above for
  why they were dropped.
  **A `magician` patch bump lands in two places** — `magician/Cargo.toml`
  and the `_Current development version:_` line in `magician/CHANGELOG.md`.
  (`Cargo.lock` also carries the version but is gitignored in this workspace;
  update it locally so the next build does not churn it.) A bump to
  `magician-mcp-client` or `tool-runtime-core` does not require a matching
  edit in the Phase 5G guard. Editing the attested `component_versions`
  block to match a later crate version would assert a conformance scope
  nobody established.

- **`make graph-index` is incremental as of 2026-09-06.** It walks the whole
  repository and takes ~4 minutes, and `build-all-debug` / `build-all-release`
  both call it, so it now skips when `scripts/graph_index_stamp.py` finds the
  repository fingerprint unchanged and every output still present. The check is
  ~80ms. It deliberately over-triggers (any commit reindexes, even a docs-only
  one) and never under-triggers, because a stale graph answers every query
  against it wrongly. Reindex regardless with `make graph-index FORCE=1`.

Bootstrap hooks once per clone/worktree:

```bash
make setup-hooks
```

For one-off exceptions, bypass locally with:

```bash
DOCS_GUARD_BYPASS=1 git commit ...
```

Use bypass sparingly and follow up with docs in a dedicated commit.

## Contributor Workflow

1. Change code.
2. Update docs in one of the matching owned paths.
3. Run local check:

```bash
python3 scripts/docs_guard.py --staged
```

Optional reminder view:

```bash
python3 scripts/docs_guard.py --working-tree --remind-only
```

4. Commit.

## Writing Standard

- Prefer concise, task-focused docs.
- Keep one canonical page per concept; avoid duplicate variants.
- Keep implementation-heavy details near code when they are tightly coupled.
- Link from `docs/components/<component>/README.md` so navigation remains centralized.

Build maintenance: `make maintain-build-cache` prunes only old incremental sessions under the configured target with Cargo’s profile lock; `make test-build-cache-maintenance` checks lock, age and symlink safety.
