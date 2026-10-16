# Unified UI testing

The Unified UI uses two Vitest projects, so fast logic tests do not inherit a
browser runtime while interaction tests mount real Svelte components in JSDOM.

## Projects

| Project | Config | Files | Environment |
| --- | --- | --- | --- |
| Unit | `ui/unified-ui/vitest.unit.config.ts` | `src/**/*.test.ts`, excluding component tests | Node |
| Component | `ui/unified-ui/vitest.component.config.ts` | `src/**/*.component.test.ts` | JSDOM + Svelte Testing Library |

`ui/unified-ui/vitest.config.ts` runs both. The normal Vite app config installs
no browser-test plugins, so app builds do not inherit test-only conditions.

Run the complete suite:

```bash
cd ui/unified-ui
npm test
npm run check
```

From the repository root, `make test-ui` is the UI-only target; `make test`
runs it after the Rust suite and before the Magicutor extension, desktop tray
and macOS suites. `make test-ui-verbose` and `make test-verbose` give
verbose reporting. The UI test targets depend on the idempotent
`setup-ui-deps`, which runs `npm ci` against the exact
`ui/unified-ui/package-lock.json` graph (without rewriting it) and verifies
Vitest and the matching V8 coverage provider; `setup-all` uses the same target.
`make verify-ui-test-deps` is a read-only CI/preflight check.

`make check-ui` type-checks API normalizers as well as Svelte components (for
example, the WhatsApp ring mapper returns `WhatsAppRingProfile | null` before
invalid profiles are filtered).

Every run of these Make targets writes a self-contained dashboard to
`coverage/frontend/latest.html` and prints a `file://` link on success and
failure. Make returns Vitest's status, or the report generator's when the tests
passed but the report failed. The dashboard has searchable file/suite/case
results, failure messages and durations, and links to the raw Vitest JSON in a
timestamped `coverage/frontend/results/<run>/` directory. V8 coverage (per-file
line/function/branch numbers plus annotated HTML) is collected by
`make test-ui-verbose` always and by `make test-ui` only with `COVERAGE=1`
(default `0`).
Override the artifact root when needed:

```bash
make test-ui UI_TEST_REPORT_DIR="$HOME/Desktop/frontend-test-report"
```

Run one layer while iterating:

```bash
npx vitest run --config vitest.unit.config.ts
npx vitest run --config vitest.component.config.ts
```

## Test shape

- Keep pure normalization, projection, store, URL, pagination and API-contract
  tests beside their source as `*.test.ts`.
- Use `*.component.test.ts` when keyboard, focus, form binding, disabled state,
  tabs, dialogs or visible feedback is the behavior under test.
- Use a small fixture under `src/test/fixtures/` when a component needs host
  state; do not copy production controller logic into it.
- Large route components should delegate request plumbing to a typed API or
  controller module. Test that boundary directly and mount the shared
  components that own the interaction.
- Assert accessible roles, names and visible outcomes. Avoid snapshots of large
  route DOM trees and styling-only selectors.

`src/test/browser.ts` owns deterministic fetch routing, JSON/text responses,
WebSocket control, in-memory storage and the shared browser API polyfills. Add
capabilities there only when several tests need them.

`installBrowserTestPolyfills()` installs `WebSocket`, `ResizeObserver` and
`IntersectionObserver` with `Object.defineProperty`, not `vi.stubGlobal`, so
`vi.unstubAllGlobals()` cannot remove them. They are installed once from
`src/test/setup.ts`. A test that stubs one of them still works: unstubbing
restores the polyfill.

JSDOM's `HTMLCanvasElement.getContext()` reports every call as unimplemented.
The shared setup replaces it with a deterministic `null` context, the fallback
canvas-owning components already support. A test that needs drawing must
install its own context mock.

### A throwaway config must still say `browser: true`

`make check-ui` reinstalls locked dependencies and runs `svelte-kit sync`. On
the shared checkout, stop Vite first (`make stop-ui-dev`), run the checks, and
restart it (`make run-ui-dev`): a running Vite can keep the old route-module map
while `.svelte-kit` is rewritten, causing failed dynamic imports or an error
page. Source edits alone hot-reload as usual. Dev startup and type checks
install with `--no-audit --no-fund`; run `npm audit` separately.

Running the component project without the `sveltekit()` plugin is sometimes
necessary, since that plugin's `svelte-kit sync` rewrites
`.svelte-kit/ambient.d.ts` and breaks a running dev server. Such a config must
alias `$app/environment` itself, and **the JSDOM project's stub must set
`browser: true`.** The real config reads `BROWSER` from `esm-env`, which is
`true` in JSDOM and `false` in the Node unit project. A stub saying `false` is
not a safe default: `TasksWorkspace.svelte` opens its `onMount` with
`if (!browser) return`, so it renders its skeleton and never starts the store.
Verify a harness before trusting a red it reports.

### One task, two buttons with its title

While a task's panel is open, its title names two buttons: the card's title
button in the list and the panel header's clamp expand toggle in the
`Task panel` dialog. Both are correct, so `getByRole('button', {
name: title })` throws as ambiguous. Pick the list card from the match inside an
`<article>`, which the drawer does not render. Do not use a `data-testid`; the
accessible name is what is worth asserting.

Add a focused test whenever a user-visible defect is fixed, at the lowest layer
that reproduces it faithfully.

### Server render smoke tests

Svelte runs `onDestroy` during server rendering but not `onMount`. A component
that detaches `window`/`document` listeners in `onDestroy` throws
`window is not defined` on the server and the route answers 500. Attach and
detach browser listeners inside `onMount` and return the cleanup from it.
`src/lib/apps/appServerRender.test.ts` renders the app region components with
`svelte/server`; add any component that can appear on a server-rendered route.

### Memory connection clarification

`src/lib/hitl/MemoryConnection.component.test.ts` drives the real attention
modal, prompt store and response adapter with the memory worker's choice
contract. Four cases cover evidence rendering, required free text for
“Remember my clarification”, acknowledge/dismiss without memory text, and
cancellation without an answer. HTTP is mocked; these checks do not certify
browser layout, Tauri host integration or native iOS/Android interaction. See
[the behavioral evaluation](../magician/memory-connections-evaluation.md).
