# agent-browser v0.38.1-Magician.0

Magician's pinned fork of
[vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) `v0.38.1`.
This rebase keeps upstream's agent-oriented reads, accessibility audits,
session/restore lifecycle, renderer recovery, WebGPU, allowed-domain
containment, tab pinning, snapshot deltas, persistent refs, query-string CDP
WebSocket URLs, WebMCP, and the one-hour headless idle default. The Magician
suffix contains only the behavior that is still absent upstream. Magician still
sets `AGENT_BROWSER_IDLE_TIMEOUT_MS=0` for headless sessions it owns.

## Retained Magician deltas

1. Downloads configure `Browser.setDownloadBehavior` at browser scope with
   `allowAndName` and events enabled.
2. The CLI socket read floor is 60 seconds, on top of upstream's per-command
   duration plus 10-second margin, so it cannot race the daemon's 30-second
   download completion window and duplicate a click on retry.
3. Download activation uses `Runtime.callFunctionOn` with `userGesture: true`.
   Completion races CDP events against new, non-temporary filesystem entries
   whose non-zero size is stable across observations.
4. `close --keep-browser` detaches from a locally launched browser without
   sending `Browser.close` or triggering process-group teardown. After close,
   the client waits until the previous daemon owner releases the session socket.
5. Local browser exits during the initial CDP handshake, target discovery, or
   first transport operation are observed for a bounded 750 ms. CloakBrowser
   codes 76–79 receive actionable capacity/license/configuration diagnostics,
   and a health check cannot erase a previously reaped status.
6. A scoped `browser/bin/agent-browser` mirror discovers the adjacent pinned
   package at `browser/node_modules/agent-browser/{skills,skill-data}`.
7. `setinterceptfilechooser` and `awaitfilechooser` suppress native file-picker
   dialogs and populate page or OOPIF chooser events atomically with an optional
   click. They have CLI, MCP, help, core-skill, and Magician schema parity.
8. `frame <css-selector>` resolves the selected frame element's actual CDP
   frame ID rather than comparing an element id/src to `frame.name`.
9. `session id` sizes its human-readable prefix for the effective Unix socket
   directory, including namespaces. It preserves the stable hash suffix and
   never emits an ID that the next command rejects for socket-path length.

The accumulated source diff is
[`patches/0001-magician-patches.patch`](./patches/0001-magician-patches.patch).
Upstream download issue context remains tracked in
[#1300](https://github.com/vercel-labs/agent-browser/issues/1300).

## Build and deployment

The browser skill installs stock npm `agent-browser@0.38.1` for its JavaScript
launcher and version-matched skill data, then overlays the patched native
binary. The accumulated patch is the committed source of truth. macOS arm64 is
built on demand into the gitignored `bin/agent-browser-darwin-arm64`; the
container applies the same patch and builds its native Linux binary on the
Rust 1.92 Bookworm stage. No native executable is committed.

```bash
make -C skillshub agent-browser-source
make -C skillshub rebuild-agent-browser
make -C skillshub setup-agent-browser
make -C skillshub verify-agent-browser
```

`setup-agent-browser` invokes `rebuild-agent-browser` automatically when the
local artifact is absent, so a clean clone is reproducible from the upstream
tag plus the committed patch. The source cache is versioned by upstream tag at
`.cache/agent-browser-src-v0.38.1-magician`, so an older dirty rebase is never
deleted by the setup target. `rebuild-agent-browser` builds the cache,
regenerates the single accumulated patch with `git diff v0.38.1`, ad-hoc signs
the macOS binary, and deploys it over the npm-native copy when present.

`verify-agent-browser` checks the exact fork version, macOS signature, mirrored
binary, and relocatable `skills get core --full` discovery. Container builds run
the same skill-discovery acceptance against the Linux binary.

## Upgrade checklist

1. Audit upstream release notes and diff before carrying any old hunk. Drop
   behavior that upstream now implements.
2. Bump `AGENT_BROWSER_UPSTREAM_TAG`, `AGENT_BROWSER_VERSION`, npm package and
   lockfile, Docker tag/path/toolchain, Cargo package version, and docs.
3. Clone the new clean tag into its versioned cache and port the remaining
   deltas with parser, native action, MCP, help, skill, and regression coverage.
4. Run `make -C skillshub rebuild-agent-browser`, then setup and verification.
5. Verify download, chooser, keep-browser, Cloak exit diagnostics, and mirrored
   skill discovery before removing the superseded vendor directory.

The container builds its own glibc artifact. Standalone darwin-x64, Linux
outside the image, and Windows installations use the stock npm-native artifact
unless their platform build is supplied.
