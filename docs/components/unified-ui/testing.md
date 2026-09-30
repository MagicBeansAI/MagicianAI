# Unified UI testing

The Unified UI uses two Vitest projects so fast logic tests do not inherit a
browser runtime, while interaction tests compile and mount real Svelte
components in JSDOM.

## Projects

| Project | Config | Files | Environment |
| --- | --- | --- | --- |
| Unit | `ui/unified-ui/vitest.unit.config.ts` | `src/**/*.test.ts`, excluding component tests | Node |
| Component | `ui/unified-ui/vitest.component.config.ts` | `src/**/*.component.test.ts` | JSDOM + Svelte Testing Library |

`ui/unified-ui/vitest.config.ts` runs both projects. The normal Vite app config
does not install browser-test plugins, so development and production builds do
not inherit test-only conditions.

Run the complete suite:

```bash
cd ui/unified-ui
npm test
npm run check
```

From the repository root, the equivalent UI-only target is `make test-ui`.
`make test` includes that target after the Rust suite and before the desktop
tray and macOS presence-host suites. Use `make test-ui-verbose` or
`make test-verbose` for verbose Vitest reporting. The runnable UI test targets
invoke the idempotent `setup-ui-deps` prerequisite, so a fresh checkout uses
`npm ci` to install the exact `ui/unified-ui/package-lock.json` graph without
rewriting the lockfile, verifies Vitest and the matching V8 coverage provider,
and then continues into the tests. `setup-all` delegates to the same target. Use
`make verify-ui-test-deps` when a read-only CI/preflight check is required
without installation.

The Make targets also collect V8 coverage and write a self-contained dashboard
to `coverage/frontend/latest.html`. A clickable `file://` link is printed near
the end of successful and failed runs; Make returns the original Vitest status
after report generation. The dashboard contains searchable file/suite/case
results, failure messages, duration, and per-file line/function/branch coverage.
It links to the raw Vitest JSON and source-annotated HTML coverage retained in a
timestamped `coverage/frontend/results/<run>/` directory. Override the artifact
root when needed:

```bash
make test-ui UI_TEST_REPORT_DIR="$HOME/Desktop/frontend-test-report"
```

Run one layer while iterating:

```bash
npx vitest run --config vitest.unit.config.ts
npx vitest run --config vitest.component.config.ts
```

## Test shape

- Keep pure normalization, projection, store, URL, pagination, and API-contract
  tests beside their source as `*.test.ts`.
- Use `*.component.test.ts` when keyboard, focus, form binding, disabled state,
  tabs, dialogs, or visible feedback is the behavior being protected.
- Prefer a small fixture under `src/test/fixtures/` when a component's public
  event and binding contract needs host state. Do not duplicate production
  controller logic in the fixture.
- Large route components should delegate request plumbing to a typed API or
  controller module. Test that boundary directly and mount the stable shared
  components that own the interaction.
- Assert accessible roles, names, and visible outcomes. Avoid snapshots of
  large route DOM trees and selectors coupled only to styling.

`src/test/browser.ts` owns deterministic fetch routing, JSON/text responses,
WebSocket control, in-memory storage, and the small browser API polyfills shared
by component tests. Add capabilities there only when several tests need them;
keep one-off mocks local to their test.

`installBrowserTestPolyfills()` installs `WebSocket`, `ResizeObserver` and
`IntersectionObserver` with `Object.defineProperty` rather than `vi.stubGlobal`,
so `vi.unstubAllGlobals()` cannot remove them. They are the environment jsdom does
not provide, installed once from `src/test/setup.ts` before any test runs — not
per-test state for vitest to roll back. A test that deliberately stubs one of
these still works: `vi.stubGlobal` saves whatever is present and restores it on
unstub, which restores the polyfill instead of deleting the property.

JSDOM also exposes `HTMLCanvasElement.getContext()` but reports every call as an
unimplemented operation. The shared setup replaces it with a deterministic
`null` context, matching the fallback that canvas-owning components already
support and preventing successful component tests from emitting false warning
stacks. A test that needs drawing behavior must install its own focused context
mock.

### A throwaway config must still say `browser: true`

Running the component project without the `sveltekit()` plugin is a legitimate
and sometimes necessary thing to do — that plugin triggers the `svelte-kit sync`
that rewrites `.svelte-kit/ambient.d.ts`, which breaks a dev server someone is
using. Such a config has to alias `$app/environment` itself, and **the JSDOM
project's stub must set `browser: true`.** The real config resolves the name to
SvelteKit's runtime module, which reads `BROWSER` from `esm-env`; under
`vitest.component.config.ts`'s plugin set that resolves to `true` in JSDOM, and
to `false` in the Node unit project. A stub that says `false` everywhere is not
a conservative default, because `TasksWorkspace.svelte` opens its `onMount` with
`if (!browser) return` — the component mounts, renders its skeleton, and never
starts the store. Verify a harness before trusting a red it reports.

### One task, two buttons with its title

Whenever a task's panel is open, the task's title is the accessible name of two
buttons: the card's title button in the list, and the panel header's clamp
expand toggle in the `Task panel` dialog. Both are correctly named — the second
is a disclosure whose name is the text it expands — so `getByRole('button', {
name: title })` is ambiguous rather than wrong, and it throws. Resolve the list
card from the match that sits inside an `<article>`, which the drawer does not
render. Do not reach for a `data-testid`: the accessible name is what a reader
uses to find the row, and it is the thing worth asserting.

Add a focused test whenever a user-visible defect is fixed, and put it at the
lowest layer that can reproduce the failure faithfully.

### Server render smoke tests

Svelte runs `onDestroy` during server rendering and does not run `onMount`. A
component that detaches `window`/`document` listeners in `onDestroy` throws
`window is not defined` on the server and the route answers 500 (`/chat` did
when `TopBar` still mounted `AppIndicatorRegion`). Attach and detach browser listeners
inside `onMount` and return the cleanup from it.
`src/lib/apps/appServerRender.test.ts` renders the app region components with
`svelte/server`; add a component there whenever it can appear on a
server-rendered route.

### Memory connection clarification

`src/lib/hitl/MemoryConnection.component.test.ts` drives the real attention modal,
prompt store and response adapter with the memory worker's choice contract. Four
focused cases cover evidence rendering, required free text for “Remember my
clarification”, acknowledge/dismiss without memory text, and cancellation without
an answer. HTTP is mocked; these component checks do not certify browser layout,
Tauri host integration or native iOS/Android interaction. See
[the behavioral evaluation](../magician/memory-connections-evaluation.md).
