# Unified Work qualification

Acceptance evidence for [Unified Work](unified-work.md), qualified from
2026-10-06 on branch `codex/unified-work` (base
`99d7f7f9ecbb59535607473750d0988ab14af808`) and merged into the root checkout.
The interface plan
and complete core migration plan
are archived; the [Work contract](unified-work.md) and this record own current
behavior and evidence. Integration did not push, deploy, rebuild or restart the
running runtime. Evidence paths under `.local/` are machine-local (gitignored)
and not part of the repository.

## Complete migration final acceptance

The core UI migration is implemented on web, iOS and Android; the
migration completion matrix
maps the original inventory to Work. Legacy page hosts were later removed (see
*Final route and navigation cleanup*); shared workspaces and runtime owners
remain.

| Final gate | Evidence |
| --- | --- |
| Web type/compile check and full suite | Zero errors/warnings; 456 files / 4,607 tests passed, one file / 11 conditional tests skipped |
| Web geometry/themes | 3,888 core + 2,160 specialist cases, 24 themes × nine size/text configurations; zero failures or exceptions |
| Remote access and return | 64 auth/filter/return regressions; a real off-device browser login redirect keeps exact Work and specialist context |
| iOS final Work lane | 113 unit/API/deep-link/theme tests and 24 UI journeys on the 375pt iPhone SE simulator, iOS 26.5 |
| Android final Work lane | Debug build and debug/release unit lanes; 23 device journeys at 320dp / 130% text, incl. 200% Crew text and 24 themes |
| Documentation | Docs and link guard pass; 292 known dangling links across 76 files, none added |

Native guarantees: schedule writes read current data and keep stored fields
with exact integer fidelity; monitor refresh keeps loaded spans beyond the
200-record request ceiling; internal cleanup reports per-record partial failure
and stays retryable; App lifecycle writes stay with the App; Crew cycles keep
execution-only capability gates. iOS adds 17 task-detail and 39 monitor
view-model regressions.

Evidence: `.local/qualification/` (`ios-work-final.log`,
`android-check-final.log`, `android-work-final.log`) and
`.local/designs/2026-10-06-work-migration-*.json`. General program authoring,
new human backlog mutations and OPC business UI (P05/P06/P10) are unimplemented
extensions.

## Consolidation and deep-panel review

Fixes from reviewing the consolidated hosts:

- An open Plan Inspector now follows the URL: it retargets when `selected`
  changes and closes when `plan=open` is removed; closing or switching tasks
  clears plan and run focus.
- Internal work and Crew cycle drawers no longer offer the inert Plan graph
  action.
- Consumed native deep-link targets clear on manual navigation, New work and
  connection changes.
- Same-identity credential rotation invalidates Quality review/artifact/policy
  state and reloads Crew roster/member/tool controls, rejecting stale results.

`DeepWorkPanel.swift` holds only compatibility helpers, not a second renderer.
Gates: web `make check-ui` clean and 456 files / 4,613 tests; iOS 113 tests and 25 UI journeys; Android build, unit lanes and 24
device journeys; 1,296 browser cases
(`.local/designs/2026-10-06-work-consolidation-review.json`). Logs in
`.local/qualification/`: `web-work-review-full.log`, `web-work-review-check.log`,
`ios-work-consolidation-review.log`, `android-work-consolidation-review.log`,
`android-work-review-check.log`.

## Earlier native and remote checkpoint

Superseded by the final acceptance. Lasting rules: Crew cycles load only
`/v3/executions/{execution}/execution-panel` (no task actions, as on web);
returns carry no credentials in URLs; simple creation excludes disabled and
Worker records and picks the current primary Personal owner; program overview
reads are capped at four concurrent requests; custom live surfaces use the
authenticated web Work member. Checkpoint gates: web 456 files / 4,607 tests;
64 remote tests; iOS 51 tests and 22 UI journeys on iPhone SE (50/22 on iPhone
17 Pro) plus 17 shared-inspector tests; Android 19 journeys at 320dp / 130%.

## Complete migration web checkpoint

`make check-ui` clean; `make test-ui -o setup-ui-deps UI_TEST_ARGS=--maxWorkers=4`
passed 455 files / 4,605 tests (one file / 11 conditional skipped). Browser fixtures: 3,888 core (18 routes × 24 themes × nine
configurations) and 2,160 specialist cases (10 routes) at 320, 375, 430, 768,
959, 960 and 1280px plus 320/390px at 130% text
(`.local/designs/2026-10-06-work-migration-core.json`,
`.local/designs/2026-10-06-work-migration-specialists.json`). Fixtures qualify
UI geometry and in-memory behavior only.

## Scope and preservation

Work replaces Tasks on web, iOS and Android (All work, Needs you, Crew; Programs,
Quality and Company loop under More). New work opens the existing task form. No
new task store, runner, backend endpoint, configuration or data migration.

| Area | Preservation and qualification approach |
| --- | --- |
| Web task/internal/monitor controls | Existing workspaces, task panel and Plan Inspector |
| Web Crew, Attention, Relentless and operations | Reusable workspaces; Relentless lives in Work → Quality |
| Native tasks and owner decisions | Existing implementations, wrapped in the Work shell |
| Native Crew/program/operator/quality | Frozen connection clients, existing write contracts, pinned versions |
| Programs and OPC | Scoped read/history/restore; `program.md` stays durable direction |
| Small screens and themes | Full-screen web inspector below 960px; installed theme tokens/fonts |

## Web checks

| Check | Result |
| --- | --- |
| `make check-ui` | Zero errors and warnings |
| Focused Work and preserved task regressions | Passed in the full suite, incl. authenticated hydration and dispatch interruption |
| `make test-ui COVERAGE=0` | 447 files / 4,539 tests (after the third review); one file / 11 conditional skipped |
| Browser fixture qualification | 2,016 layout cases; zero failures or exceptions |

The matrix is 12 destinations × 24 themes × 320, 390, 768, 960 and 1280px plus
320/390px at 130% text (themes via the installed store, fonts loaded), checking
document/workspace overflow, clipping, feed height, inspector bounds, the open
More panel, theme tokens and the creation touch target. The fixture
(`ui/unified-ui/scripts/work-browser-fixture.js`) intercepts requests only in its
own browser context and never mutates runtime records. Evidence:
`.local/designs/unified-work-third-review-matrix.json`; unit report
`target/coverage/frontend/latest.html` (in the isolated worktree).

### Authenticated runtime journey

On the local runtime, a browser created, approved and ran a task through New
work and saw its output; Quality and Crew links returned to the same selection.
The test task stays identifiable by its title (**Unified Work qualification —
local interface check**); no runtime data was deleted. `ExecutionControls` now keeps the Steer draft across execution polls
for the same execution and scope, clearing it on a real target/scope change. Not
exercised: a live Steer submission, OS Open/Reveal/export effects and
provider-specific actions.

## Native checks

Native apps ran against deterministic HTTP and offline task fixtures; no device
enrollment, model provider or runtime mutation.

| Platform/check | Result |
| --- | --- |
| iOS `make test-ios-work` | Passed: 40 API/deep-link/theme and 18 UI tests |
| Android `make check-magdroid` | Passed: debug build and debug/release unit lanes |
| Android `make test-magdroid-work ANDROID_SERIAL=<device>` | Passed: 12 native UI tests at 320dp, incl. double-text Crew editor and Crew → Quality review/start |

iOS ran on iPhone 17 Pro and iPhone SE (375×667pt) simulators, iOS 26.5, up to
the largest accessibility text; Android on an API 34 emulator at 320dp, text
1.3 (2.0 for one Crew test). Both cover all 24 themes.

Native transports preserve encoded identities, unknown fields, large integer
budgets, immutable headers, JSON trigger bodies and bounded quality defaults;
they reject missing or stale versions and refuse redirects. After a scope change, old records clear and
pending writes cannot reach the new connection.

## Deep review rounds

Review rounds checked scope boundaries, responsive/theme layouts,
adversarial interactions and request authority. Fixes:

| Finding | Correction |
| --- | --- |
| Cold task deep link lost selection on first scope resolve | Keep the identity through first hydration |
| Company-loop dispatch outlived navigation or scope change | Abort; drop stale results |
| Android re-pairing didn't notify mounted views | Compare connection authority, ignoring display-name edits |
| iOS Crew Save disabled on first presentation | Capture ETag and scope in an immutable sheet session |
| Narrow inspectors clipped Plan graph or Close | Own header column; Close unshrunk |
| Large themed text pushed controls out | Wrap headings; scroll the phone More panel |
| Program preview collapsed the phone task list | Context scrolls away; browser checks reject a feed below 160px |
| Large native text buried Crew editing controls | Edit first; scrolling editor |
| Android bottom navigation broke long labels | One-line ellipsis; full accessibility labels |
| Filter change during a busy write left old records | Load the latest requested path |
| iOS task creation dropped the draft on failure | Keep draft and error; block duplicate submit |
| iOS policy confirmation could pick up a newer version | Snapshot the reviewed version; server checks it |
| Android artifact save failure escaped the UI coroutine | Catch local I/O; show a recoverable error |
| Browser matrix skipped the font loader | Apply themes via the installed store; await fonts |
| Pending thread move used a new task or failed lookup | Freeze task and credential revision |
| Thread store could create with replacement credentials | Credential revision in request identity |
| Rejected task move showed success | Return the acknowledged result |
| Tab/Shift+Tab escaped the phone task dialog | Shared focus trap |
| Android Crew → Quality defaulted to `engineer` | Carry the member through route, filter and draft |
| Programs used replacement credentials before scope update | Stamp scope/credential identity; check after every await |
| Late restore failure appeared on another program | Bind the error to the original program and scope |
| Native artifact size checked after full buffering | Stream; reject over 128 MiB before EOF |
| Smallest header painted the wordmark under controls | Hide the ⌘K hint at ≤420px and the wordmark at ≤385px (`TopBar.svelte`) |
| Retried draft reused its admission ID after a change | Key retry on the full reviewed request |
| Slow previous-attempt comparison blocked approval | Load the selected artifact independently |
| Late run failures hit another run | Fence by scope and selection revision |
| Bookmarked run vanished during workspace resolution | Keep the target pending until refresh |

The backend still verifies sealed blobs and the run journal. Advanced JSON
editors submit real version-checked requests. The iOS streaming fixture sends a
full 64 KiB chunk (Foundation coalesces tiny URLProtocol prefixes). No
placeholder actions were found; no backend, configuration or runtime data
changed.
Evidence: `.local/designs/unified-work-deep-review-final.json`,
`.local/designs/unified-work-deep-review-crew.json`,
`.local/designs/unified-work-second-review-matrix.json`,
`.local/designs/unified-work-second-review-keyboard.json`,
`.local/designs/unified-work-third-review-header.json`,
`.local/designs/unified-work-fourth-review-quality.json`.

| Round | Web suite | Other gates |
| --- | --- | --- |
| Relentless cleanup | 446 files / 4,518 tests | 2,016-case matrix; 24 bookmark/navigation and 144 menu cases; `make check-all` then stopped at the store-durability baseline (fixed in the final route cleanup) |
| Follow-up review | 446 / 4,518 | iOS 55 (37 API + 18 UI) and 88 `TasksViewModelTests`; Android 11 UI tests; 168-case Crew + 2,016-case matrix |
| Second review | 447 / 4,533 (+15 regressions) | 2,016-case matrix; 43 keyboard checks; Android 12 UI tests |
| Third review | 447 / 4,539 | iOS 58 (40 + 18); Android 12 UI tests; 264 header/dialog cases; 2,016-case matrix |
| Fourth review | 447 / 4,553 (+14 regressions) | 38 Quality component tests; 168 comparison-notice cases; native lanes not rerun |

Each round ended with no unresolved critical or important defect in the
reviewed paths.

## Limits and release boundaries

- Native evidence is simulator/emulator only: physical-device signing, pairing,
  notification delivery and live native provider journeys are not qualified.
- Specialist tools use authenticated browser handoff with context and return.
- The scoped program API supports read/history/restore only; no arbitrary
  document writer exists.
- Broader OPC domains (engagements, rooms, commitments, envelopes, receipts)
  keep their API/operator coverage; Company loop adds no UI for them.
- Relentless/provider rollout, outward authority and backend recovery stay
  governed by their own contracts.
- Remote Work and the explicit specialist allowlist are authorized; root login
  hydration and scoped API permissions are unchanged; other operator routes keep
  the hostname restriction. Signed-out Work links redirect
  to login with exact return context; a newly paired remote phone login is a
  separate check.
- Not a release deployment: notice/report payloads, execution and authority
  contracts are unchanged; release gates and physical-device acceptance belong
  to the release process.

## Relentless route cleanup

The standalone page is removed; Quality lives in Work. Durable notices and
report bodies are not rewritten (that would break same-ID replay); saved links
are normalized in-app. The task overflow menu receives its task ID, so
**Improve quality** shows on eligible tasks; the menu fits all clipping
ancestors, caps its height with an internal scroll, recalculates on resize and
focuses its first action once visible. Evidence:
`.local/designs/unified-work-cleanup-matrix.json`,
`.local/designs/unified-work-cleanup-navigation.json` (all four quality target
types), `.local/designs/unified-work-cleanup-menus.json` (24 themes).

## Final route and navigation cleanup

All eleven Tasks/Crew/Relentless/Internal/thread/company-loop alias hosts
(including `/tasks`, `/crew` and `/relentless`) and their redirect-only loaders
are removed; direct requests return 404 and old bookmarks no longer resolve.
Work-owned child editors, shared inspectors, Square's Crew dock and backend APIs
remain; the palette has one Work destination searchable by kind. Android's persisted `Destination.Tasks` identifier keeps
its value, labelled Work. New links use Work; durable proposal notices keep
their replay identity, stored old links are normalized in-app, and monitor
links keep their exact update target.

Gates: 456 files / 4,620 web tests; 648 browser cases; iOS 113 tests and 25 UI
journeys; Android build and unit suites; same 292 known dangling links.
Budget-grant sidecars
(`magician/src/magician_v2/execution/agentic/run_budget.rs`) now use the shared
durable byte writer, so the store-durability guard passes; two pre-existing test
compile errors (`serde_json::Value` qualification, a stale optional-boolean
fixture) were fixed.
`make test-magicvault-foundation` passed 32 tests and the pinned MagicVault lane
31; `make check-all -o check-ui -o check-magdroid` passed (the desktop crate
declares a standalone workspace for nested-worktree Cargo discovery) and
`make ios-debug-check` compiled iOS. Logs:
`.local/qualification/work-route-retirement-*`;
`.local/designs/2026-10-06-work-route-retirement.json`.

## Root integration and completed plan archive (2026-10-06)

The combined source integrates root `meetable_bot` at `9e7bedb5c` with feature
`codex/unified-work` at `7dd0577dd`, keeping both root changes and Work's
credential/scope invalidation. Eight unrelated uncommitted Relentless root files
stayed uncommitted, outside the qualified source.

| Combined-source gate | Result |
| --- | --- |
| Full frontend suite | 457 files / 4,636 tests; one file / 11 conditional tests skipped |
| Browser geometry/themes | 864 cases (feed, Plan Inspector, Crew member, task-bound Quality) × 24 themes × nine configurations; zero failures |
| Compact iOS Work lane | 113 tests and 25 UI journeys on iPhone SE, zero failures |
| Android | Debug build and unit suites; earlier 24 device journeys cover unchanged Work |
| `make check-all` | Passed (Rust, desktop, web types, Android, extracted libraries, iOS simulator compile); existing warnings remain |
| Documentation | Docs guard, whitespace and link checks; 292 known dangling links, none added |

Evidence is archived with verified manifests under `.local/worktree-archives/unified-work-2026-10-06/`
(pre-merge root WIP in
`.local/worktree-archives/unified-work-2026-10-06/root-wip-before-merge/`) and, where names do not
conflict, under `.local/designs/` and `.local/qualification/`. No runtime data
was removed; the physical-device and provider limits above still apply.
