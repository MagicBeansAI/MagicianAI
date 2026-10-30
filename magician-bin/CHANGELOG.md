# Changelog

## [Unreleased]

- 0.2.41: Flush write-behind memory temperature counters on shutdown and CLI exit.

- Shutdown (and every CLI exit) flushes the memory temperature counters held for write-behind, bounded to 10 s.

- 0.2.40: Register public MCP profile and owner identity routes at startup.

- New `magician google-project-access [--grant]` checks (and grants) each Google account's Cloud project roles for setup scripts.

### 2026-10-24 — 0.2.39

- `make benchmark-chat-context-retrieval` runs as the opt-in `chat-context-retrieval-eval` binary (feature `retrieval-eval`) using the runtime keychain, and reports duplicate-collapse, snapshot-cache and registry-reuse timings.
- Re-adopts background shell jobs left by a previous process at startup, installs `execution.harness_turn_idle_seconds` at boot, and registers the chat asset delete route.

### 2026-10-23 — 0.2.38

- Registers the chat session assets route, and stops every bot (and the CLIs it forked) as soon as shutdown starts, before the supervisor's SIGKILL.

### 2026-10-23 — 0.2.37

- Publishes `outward_actions.browser_submission_guard` at boot and registers the `/browser/submission-grants` routes.
- Starts the spend envelope at boot (feed attention notifier, debounced checkpoint, flush at shutdown) and registers the `/budgets/envelope` routes.

### 2026-10-22 — 0.2.36

- Mount VibeDev’s run-scoped original-folder edit broker and paged history routes.

### 2026-10-20 — 0.2.35

- Boot installs the memory index notice broadcaster (rebuild-required notices).

---

Older entries: `docs/archive/changelogs/magician-bin.md`
