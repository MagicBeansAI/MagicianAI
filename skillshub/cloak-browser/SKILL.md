---
name: cloak-browser
version: 0.2.1
description: Stealth Chromium engine for the `browser` tool. When installed, the browser session controller routes headed and headless modes through CloakBrowser (a Chromium fork with compiled-in fingerprint resistance) instead of stock Chrome for Testing. If the licensed binary reports its exact concurrent-session-limit signal, the controller retries once with bundled Chrome for Testing. CDP-mode sessions (the user's actual Chrome via Magicutor) are unaffected.
compatibility: macOS arm64/x64, Linux x64/arm64, Windows x64. Requires Python 3.10+ in skillshub/.venv (set up by `make -C skillshub setup-python`) plus a one-time ~200MB stealth Chromium fetch via `make -C skillshub setup-cloak-browser`.
metadata:
  magician:
    requires:
      bins: ["python3"]
      python_packages: ["cloakbrowser[geoip]==0.5.3"]
    install_hint:
      docs: "run `make -C skillshub setup-cloak-browser` (downloads ~200MB binary on first run; also pulls geoip2 via the cloakbrowser[geoip] extra)"
---

# Cloak Browser

This is **not** an LLM-facing tool. It's an engine that the existing `browser` tool launches under the hood for `headless` and `headed` connection modes. The LLM keeps invoking `browser` the same way; the runtime swaps the binary.

## How it actually works

CloakBrowser 0.5.3 resolves the current Chromium 150 build with 71 source-level C++ patches for fingerprint resistance (canvas/WebGL noise, GPU spoofing, navigator.webdriver removal, TLS ja3n/ja4 matching, WebRTC IP spoofing, CDP-detection removal, automation-signal stripping). It is a real Chromium with the patches *compiled in*, not a runtime JS-injection layer that gets defeated by Chromium updates.

We do **not** use the CloakBrowser SDK. The SDK launches Chromium via Playwright, and Playwright's launch flags re-introduce `navigator.webdriver=true` and the `HeadlessChrome/...` UA suffix that the C++ patches are supposed to suppress — defeating the very feature we want. We hit this empirically and confirmed it by tracing the SDK launch path.

Instead, **agent-browser launches the CloakBrowser binary directly via `AGENT_BROWSER_EXECUTABLE_PATH`**. The C++ patches in the binary survive because agent-browser doesn't pass Playwright's automation flags. The full feature set we care about (fingerprint, TLS, canvas, WebGL, GPU, audio, JS-layer webdriver+UA) all stays on.

We replace the SDK with two tiny shims:

- **`scripts/resolve.py`** — calls into the `cloakbrowser` library for the things it does best (binary path lookup, default Chromium flag set, optional geoip-from-proxy resolution). Returns a JSON envelope the runtime parses to set agent-browser env vars. Never launches a browser.
- **`scripts/stealth-init.js`** — defense-in-depth JS shim. The binary already patches `navigator.webdriver` + UA; this script reapplies them at the JS layer as belt-and-suspenders, plus patches a handful of secondary signals (`window.chrome.runtime`, `permissions.query` for notifications, plugins array).

## Runtime integration

```
session opens with connection_mode = headless | headed
        ↓
runtime checks: is cloak-browser installed? (resolve.py returns binary)
   ├── yes → call resolve.py, parse JSON, set these env vars
   │           on the agent-browser subprocess:
   │             AGENT_BROWSER_EXECUTABLE_PATH = <binary>
   │             AGENT_BROWSER_HEADED          = (headed mode)
   │             AGENT_BROWSER_ARGS            = (fingerprint flags, csv)
   │             AGENT_BROWSER_INIT_SCRIPTS    = (stealth-init.js path, csv)
   │             TZ                            = (timezone, if geoip resolved)
   │         then run agent-browser normally; it launches the binary itself
   │
   └── no  → explicit configured-engine resolution failure (no silent downgrade)
```

If a launched CloakBrowser process exits with its documented code `76`
(`session limit reached`), the session controller closes the failed
agent-browser daemon and retries the same initial open exactly once with
agent-browser's bundled Chrome for Testing. Exit codes `77`–`79`, crashes,
navigation failures, HTTP errors, and site blocks do not activate this gate.
The pinned agent-browser fork retains the child process through the initial CDP
connection, target-discovery, and first transport operation. If CloakBrowser
publishes `DevToolsActivePort` and then exits during any of those stages, the
actual exit code and its license meaning are reported instead of masking it as
`Handshake not finished` or `CDP response channel closed`. A process-health
check that reaps the child caches the same diagnostic for the command error path.

CDP mode (`connection_mode=cdp`, attaching to the user's real Chrome through the Magicutor browser extension) is intentionally unaffected — the user's own profile + cookies carry the legitimacy there.

## Install

```bash
# 1. Aggregates cloakbrowser into the shared skillshub venv
make -C skillshub setup-python

# 2. Fetches the ~200MB stealth Chromium binary into ~/.cloakbrowser/
make -C skillshub setup-cloak-browser
```

`setup-cloak-browser` is idempotent. Re-running on an already-installed binary is a no-op.

## Verify

```bash
make -C skillshub verify-cloak-browser
```

Or run the full end-to-end smoke test (probes fingerprint signals, queries Google search, fetches FlightAware):

```bash
bash skillshub/cloak-browser/scripts/smoke-test.sh
```

The smoke test uses `CLOAKBROWSER_LICENSE_KEY` from the current environment
when present. Otherwise it reads only that key (without sourcing or printing
the rest of the file) from the active runtime root's `.env.development`, then
`.env`, matching the runtime's precedence. The runtime root is
`MAGICIAN_ROOT_DIR`, legacy `MAGICIAN_STORAGE_PATH`, or `$HOME/MagicianNotes`.
If no key is available, the test stops before browser launch with an explicit
configuration error.

## Resolver CLI

You can invoke `resolve.py` directly to inspect what config the runtime would apply:

```bash
skillshub/.venv/bin/python skillshub/cloak-browser/scripts/resolve.py \
    --proxy http://user:pass@1.2.3.4:8080 \
    --profile ~/.cache/cloak-profile-a \
    --headed
```

Output is a JSON envelope:

```json
{
  "binary_path": "/Users/.../.cloakbrowser/chromium-150...-pro/Chromium.app/Contents/MacOS/Chromium",
  "version": "150.0.7871.114.3",
  "args": [
    "--no-sandbox",
    "--fingerprint=613214",
    "--fingerprint-platform=macos",
    "--fingerprint-webrtc-ip=...",
    "--proxy-server=http://user:pass@1.2.3.4:8080",
    "--lang=en-US",
    "--user-data-dir=/Users/.../cloak-profile-a"
  ],
  "init_scripts": ["/Users/.../stealth-init.js"],
  "headed": true,
  "geoip": { "timezone": "America/New_York", "locale": "en-US", "exit_ip": "1.2.3.4" },
  "env": { "TZ": "America/New_York" }
}
```

## What we get / what we lose vs the SDK

| Feature | Path | Notes |
|---|---|---|
| C++ binary patches (fingerprint, TLS, canvas, WebGL, GPU, audio, webdriver, UA) | Active | The whole point. Works because we don't use the SDK launch path. |
| `navigator.webdriver=false`, no `HeadlessChrome` UA | Active | Confirmed via fingerprint probe in smoke-test.sh. |
| Geoip auto-resolve (proxy → timezone/locale) | Replicated | `cloakbrowser.maybe_resolve_geoip` is a standalone function we call from `resolve.py`. |
| Authenticated proxy (`http://user:pass@...`) | Native Chromium | Pass via `--proxy-server` flag or agent-browser's `--proxy` flag. |
| Persistent profile | Native Chromium | `--user-data-dir` flag, or agent-browser's `--profile`. |
| Timezone / locale spoof | Replicated | We pass `--lang=<locale>` to Chromium and set `TZ=<timezone>` on the agent-browser subprocess env. |
| WebRTC IP spoofing | Replicated | `--fingerprint-webrtc-ip=<resolved exit IP>` flag, auto-applied when geoip resolves. |
| `humanize` (realistic mouse curves / typing) | **Lost (unreachable in any architecture)** | Humanize works by Python-side patches on Playwright's mouse/keyboard API. agent-browser drives Chromium via raw CDP, so it never goes through the SDK's Python API surface. Humanize cannot intercept what doesn't go through it. This is the same with or without the SDK in the launch path — the architectural mismatch is between Python-layer interception and raw-CDP driving. |

If you ever need humanize for a specific task, write a standalone Python script using the SDK directly for that one job. It coexists with this engine.

## License posture

- This skill's code: MIT.
- The CloakBrowser **binary** is never bundled. End users install it themselves via the pinned wrapper plus `python -m cloakbrowser install`. A free GitHub-issued key provides the current binary with one concurrent session; larger production concurrency requires a paid plan.
- For an eventual commercial product distribution we'd need an OEM license from CloakHQ (`cloakhq@pm.me`). For now, personal use is in scope.

## Smoke test details

`scripts/smoke-test.sh` first performs the non-secret license preflight above,
then runs three checks:

1. **Fingerprint probe** — opens httpbin/headers, evals `navigator.webdriver` + `navigator.userAgent`. Fails if `wd=true` or UA contains `HeadlessChrome`.
2. **Google search** — opens a real search query. Fails if redirected to `google.com/sorry/...` (bot challenge).
3. **FlightAware** — opens an Akasa flight page. Sanity check that ordinary aggregator sites are happy.

Pass the smoke test before wiring this engine into the runtime.

## What this skill is NOT for

- A new LLM-facing tool. The LLM keeps using `browser`.
- A replacement for agent-browser. agent-browser stays as the CLI; CloakBrowser just provides the binary it launches.
- A help for CDP-mode sessions (those connect to the user's actual Chrome).
- A bypass of server-side bot detection that relies on behavior (e.g. captcha solving, purchase pattern detection). Anti-detection at the browser-fingerprint layer only.
- Multi-step GUI automation. For Mac app automation use the `macos-script-automation` (osascript one-liners) or `macos-ui-automation` (AX-tree UI walks) skills when applicable.
