---
name: lightpanda
version: 0.1.0
description: Fast headless DOM-first engine for the existing `browser` tool. Prefer it for isolated public navigation, accessibility snapshots, text extraction, and high-volume read-only work that does not need rendered pixels, a user profile, extensions, file access, or authenticated browser state. Use CloakBrowser, bundled Chrome, or CDP for full-fidelity, visual, identity-bearing, or owner-visible work.
compatibility: macOS arm64/x64 and glibc Linux arm64/x64. Requires the project-pinned agent-browser build with Lightpanda engine support.
metadata:
  magician:
    requires:
      bins: ["lightpanda"]
    install_hint:
      docs: "run `make -C skillshub setup-lightpanda`; Homebrew installs the official nightly on macOS"
---

# Lightpanda

This is a browser engine, not a new LLM-facing tool. Continue calling
`browser`; select `engine: lightpanda` only on the first call that creates a
fresh `headless` session. The calling runtime remains the sole reasoning loop.

## Runtime contract

`scripts/resolve.py` verifies the installed binary and emits the standard
browser-engine envelope:

```text
AGENT_BROWSER_ENGINE=lightpanda
AGENT_BROWSER_EXECUTABLE_PATH=<resolved lightpanda binary>
LIGHTPANDA_DISABLE_TELEMETRY=true
LIGHTPANDA_DISABLE_CORE_DUMP=1
```

The pinned `agent-browser` process spawns Lightpanda through its CDP engine
adapter. Do not invoke `lightpanda agent`; therefore Lightpanda never discovers
or calls an LLM. `--no-llm` applies only to Lightpanda's own Agent REPL and is
unnecessary on this path.

CDP mode remains the user's Chrome through Magicutor and ignores browser-engine
selection. The resolver rejects `headed` because Lightpanda has no graphical
window.

## Soft selection boundary

Prefer Lightpanda when all known requirements are compatible:

- fresh isolated headless session;
- public or otherwise non-identity-bearing page;
- high-volume/fan-out reading across many public pages;
- DOM, accessibility-tree, text, link, or structured extraction evidence;
- semantic click/fill/navigation that can be verified from page-owned DOM state;
- no requirement for rendered pixels or a visible owner-observable window.

Prefer the configured full-fidelity engine, or explicit `cloak-browser`, when
the task needs any of these:

- screenshots, PDF rendering, visual diffs, or vision/coordinate grounding;
- headed presentation or human observation;
- the user's cookies/profile, extensions, or authenticated state (use CDP);
- persistent profiles/storage-state files, local file access, downloads, or
  uploads whose browser fidelity matters;
- anti-bot/fingerprint resistance or current run evidence that a site requires
  Chromium/Web Platform behavior Lightpanda does not implement.

These are capability preferences, not a domain allowlist. Do not encode site
names. When requirements are unknown or mixed, keep the full-fidelity default.
High-volume means efficient cross-page fan-out, not unbounded per-origin request
rate; preserve robots, rate-limit, and concurrency policy.
When a Lightpanda run returns unsupported-CDP, missing-Web-API, empty-page, or
rendering evidence, retry from a fresh full-fidelity session rather than
repeating the same failing action.

Deterministic `content_read` public-headless retrieval uses a bounded
Lightpanda → configured full-fidelity engine → bundled Chrome for Testing chain,
skipping duplicate/absent stages. That retry is limited to public read-only
work. Identity-bearing or side-effecting browser flows are never automatically
replayed. Static/API/RSS readers should still run before any browser engine.

## Install and verify

```bash
make -C skillshub setup-lightpanda
make -C skillshub verify-lightpanda
```

Or install the official nightly directly and ensure `lightpanda` is on `PATH`.
The resolver also honors `LIGHTPANDA_EXECUTABLE_PATH`.

Upstream references: [Lightpanda's agent-browser integration](https://lightpanda.io/blog/posts/using-lightpanda-with-agent-browser),
[CDP serve command](https://lightpanda.io/docs/run-locally/commands/serve), and
[open-source browser repository](https://github.com/lightpanda-io/browser).

Inspect the exact engine envelope with:

```bash
python3 skillshub/lightpanda/scripts/resolve.py
```
