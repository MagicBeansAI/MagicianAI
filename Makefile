# Magic Development Makefile
#
# All skill-related targets (setup, build, install, agent-browser dev,
# metabase CLI, bots, .venv, .env templates) live in skillshub/Makefile.
# This file delegates via `$(MAKE) -C skillshub <target>`.

.PHONY: help ensure-make setup-hooks worktree-add restart-supervisor restart-ui-dev setup-all setup-ui-deps verify-ui-test-deps setup-rust-test-report-deps verify-rust-test-report-deps setup-pi-coding-agent setup-ollama setup-ollama-embedding setup-local-generation-selected run-ollama stop-ollama install setup-bots setup-capability-tools setup-agent-browser agent-browser-source rebuild-agent-browser setup-printing-press setup-metabase-cli setup-kapso-cli setup-higgsfield-cli setup-officecli verify-officecli regen-metabase-cli refresh-metabase-spec setup-skillshub-deps setup-skill-bins setup-skill-system-bins setup-skillshub-python setup-skill-env build-all-release build-all-release-ollama build-magician-release build-all-debug build-magician-debug build-all-dev build-magician-dev replace-magician-bin replace-magicutor-bin replace-supervisor-bin replace-restart-magician replace-restart-magicutor build-bots build-ui check-ui test-ui test-ui-verbose test-magicutor-extension check-bots run-ui-dev stop-ui-dev preview-ui ensure-ui-tunnel serve-ui-tunnel stop-serve-ui-tunnel ensure-tunnel stop-tunnel connect-access connect-setup connect-status connect-local connect-container connect-remote setup-desktop-vosk setup-magios-vosk build-desktop-tray build-desktop-tray-debug run-desktop-tray run-desktop-tray-debug stop-desktop-tray test-desktop-tray run-supervisor stop-supervisor restart-magician restart-magicutor stop-magician stop-magicutor stop-bots stop-macos-audio-engine status-magician status-magicutor restart-magictunnel stop-magictunnel status-magictunnel supervisor-health tail-service-log open-service-log tail-magician-v2-log memory-index-status memory-index-prewarm memory-index-rebuild memory-index-optimize analytics-export analytics-import analytics-reprice-llm generate-media-local-speech-fixtures benchmark-media-recording-stt benchmark-media-offline-audio benchmark-media-vad benchmark-media-stt benchmark-media-tts benchmark-media-diarization test-media-offline-audio-eval verify-media-audio-rollout audio-engine-status audio-model-prewarm audio-model-unload build-macos-audio-engine build-macos-audio-engine-debug build-macos-audio-engine-release build-macos-speech-helper build-macos-speech-helper-debug build-macos-speech-helper-release test-macos-audio-engine test test-verbose test-live-evals test-suite-runner ollama-chunking-readiness ollama-chunking-shadow-eval test-rust test-rust-verbose test-agentic-default-stack test-agent-router test-data-structures test-streaming test-performance test-grpc test-integration test-storage-budgets test-mcp-server test-registry test-capabilities test-tool-skills test-tool-skills-live check-all check-magician check-magic-supervisor check-ollama-chokepoint sccache-stats sccache-clear prune-incremental check-build-space fmt fmt-check clippy clippy-all clean dev-check watch-test docs audit update install-tools build-container build-container-multiarch push-container run-container stop-container test-container test-container-full test-container-tooling qualify-container-release qualify-container-e2e qualify-container-e2e-live measure-container-image release-container release-desktop release-desktop-target release-desktop-signed release-all generate-signing-key export-apple-signing-setup verify-apple-signing-bundle import-apple-signing-setup verify-apple-signing-setup dev-desktop-setup dev-desktop-build dev-update-server dev-container-rebuild cleanup cleanup-tools cleanup-data cleanup-dry-run graph-index graph-index-force graph-check graph-serve graph-serve-bg graph-serve-stop graph-serve-restart event-taxonomy-codegen event-taxonomy-check setup-meet-bot
.PHONY: generate-media-local-speech-fixtures
.PHONY: setup-decision-models build-decision-engine-release build-decision-engine-mlx-release build-decision-engine-mlx-debug setup-decision-engine-mlx-build replace-decision-engine-bin replace-restart-decision-engine restart-decision-engine stop-decision-engine status-decision-engine
.PHONY: benchmark-chat-context-retrieval test-chat-context-retrieval-eval test-chat-context-retrieval-live-eval test-tool-result-projection-context-eval-harness test-tool-result-projection-context-live-eval test-provider-replay-eval-harness test-provider-replay-live-eval test-preplan-flow-eval-harness test-preplan-flow-live-eval eval-preplan-flow-live-interactive test-web-researcher-eval-harness test-web-researcher-live-eval test-memory-tier-health-eval test-memory-temperature-eval-harness test-memory-temperature-live-eval test-memory-ann-shadow-eval-harness test-memory-ann-shadow-live-eval test-admission-300-task-eval-harness test-content-reader-eval test-content-retrieval-eval-harness test-content-retrieval-eval test-content-retrieval-runtime-eval-harness test-content-retrieval-runtime-live-eval test-working-set-eval-harness test-working-set-live-eval test-working-set-routing-eval-harness test-working-set-routing-live-eval test-working-set-routing-as-configured test-observable-sources-eval-harness test-observable-sources-live-eval test-storage-governance-eval-harness test-storage-governance-live-eval test-minimax-cli-eval-harness eval-minimax-cli-live
.PHONY: test-agent-tool-visibility-eval-harness test-agent-tool-visibility-live-eval test-agent-surface-runtime-eval test-capability-recall-eval test-task-verdict-eval test-attention-historical-bootstrap-eval
.PHONY: llm-trace-coverage llm-trace-phase0-baseline llm-trace-phase1-audit llm-trace-phase2f-audit llm-trace-phase3-audit llm-trace-phase4-audit test-llm-trace-phase0 test-llm-trace-phase1 test-llm-trace-phase2 test-llm-trace-phase2b test-llm-trace-phase2c test-llm-trace-phase2d test-llm-trace-phase2e test-llm-trace-phase2f test-llm-trace-phase3 test-llm-trace-phase4
.PHONY: test-realtime-local-transcript test-realtime-local-transcript-live test-gemini-live-models test-gemini-live-models-live
.PHONY: restart-desktop-tray restart-desktop-tray-debug generate-magican-app-icons generate-magican-control-symbol
.PHONY: setup-protoc setup-ffmpeg check-ffmpeg setup-test-suite-runner-deps verify-test-suite-runner-deps test-pty-live verify-magios-vosk setup-desktop-pnpm

# Use sccache for all Makefile-driven Rust builds when it is installed.
# Override with `make RUSTC_WRAPPER=` to disable or
# `make RUSTC_WRAPPER=/path/to/sccache` to pin a binary.
SCCACHE ?= $(shell command -v sccache 2>/dev/null)
ifneq ($(strip $(SCCACHE)),)
RUSTC_WRAPPER ?= $(SCCACHE)
export RUSTC_WRAPPER
# A single Magician compile can exceed sccache's idle timeout.
# Keep the daemon alive; an exit mid-compile can start a duplicate fallback.
SCCACHE_IDLE_TIMEOUT ?= 0
export SCCACHE_IDLE_TIMEOUT
endif

# rustc has no RSS cap. Cargo `jobs` is per-invocation, so two or three
# unconstrained agent builds each spawn hw.ncpu rustc processes and OOM a
# 32 GiB host. The host-local wrapper (~/.cargo/rustc-job-gate, not used in
# CI) is a machine-wide flock gate; CARGO_BUILD_JOBS is the per-cargo cap.
# Override with `make CARGO_BUILD_JOBS=N RUSTC_JOB_GATE_SLOTS=N`. Disable
# the gate with `make RUSTC_JOB_GATE=`.
RUSTC_JOB_GATE ?= $(HOME)/.cargo/rustc-job-gate
ifneq ($(wildcard $(RUSTC_JOB_GATE)),)
CARGO_BUILD_JOBS ?= 4
RUSTC_JOB_GATE_SLOTS ?= 4
export CARGO_BUILD_JOBS
export RUSTC_JOB_GATE_SLOTS
endif

# Desktop tooling always uses the exact package-manager version declared by
# desktop/package.json. Keep it checkout-local so Node 25 (which no longer
# bundles Corepack) and machines with a drifting global pnpm behave identically.
# An explicit `make PNPM=...` override still wins, but setup validates its
# reported version against the same manifest pin.
PNPM_USER_SUPPLIED := $(if $(filter undefined,$(origin PNPM)),0,1)
DESKTOP_PNPM_TOOL_ROOT ?= $(CURDIR)/.cache/desktop-pnpm
DESKTOP_PNPM_BIN ?= $(DESKTOP_PNPM_TOOL_ROOT)/node_modules/.bin/pnpm
PNPM ?= $(DESKTOP_PNPM_BIN)

MACOS_AUDIO_ENGINE_PACKAGE ?= native/macos-audio-engine
MACOS_AUDIO_ENGINE_BUILD_DIR ?= $(CARGO_TARGET_DIR)/macos-audio-engine
MACOS_SPEECH_HELPER_PACKAGE ?= native/macos-speech-helper
MACOS_SPEECH_HELPER_BUILD_DIR ?= $(CARGO_TARGET_DIR)/macos-speech-helper
SWIFT_CACHE_DIR ?= $(CARGO_TARGET_DIR)/swift-cache
SWIFT_CONFIG_DIR ?= $(CARGO_TARGET_DIR)/swift-config
SWIFT_SECURITY_DIR ?= $(CARGO_TARGET_DIR)/swift-security
SWIFT_CLANG_MODULE_CACHE_DIR ?= $(CARGO_TARGET_DIR)/swift-clang-module-cache
SWIFT_AUDIO_ENGINE_TARGET_FLAGS ?=
# Every package in this graph compiles in the language mode its own manifest
# declares. Magician's package declares Swift 5 (`swiftLanguageModes` in
# native/macos-audio-engine/Package.swift); the dependencies keep their own.
# This used to force `-swift-version 5` on the whole graph for FluidAudio
# 0.12.4, which no longer needs it on the current toolchain, and the global
# override downgraded 26 tools-version-6 packages and made every module warn
# that its language mode was overridden.
SWIFT_AUDIO_ENGINE_FLAGS = --package-path $(MACOS_AUDIO_ENGINE_PACKAGE) --scratch-path $(MACOS_AUDIO_ENGINE_BUILD_DIR) --cache-path $(SWIFT_CACHE_DIR) --config-path $(SWIFT_CONFIG_DIR) --security-path $(SWIFT_SECURITY_DIR) --manifest-cache local --disable-sandbox
SWIFT_SPEECH_HELPER_FLAGS = --package-path $(MACOS_SPEECH_HELPER_PACKAGE) --scratch-path $(MACOS_SPEECH_HELPER_BUILD_DIR) --cache-path $(SWIFT_CACHE_DIR) --config-path $(SWIFT_CONFIG_DIR) --security-path $(SWIFT_SECURITY_DIR) --manifest-cache local --disable-sandbox

DESKTOP_DIR ?= desktop
DESKTOP_TAURI_MANIFEST ?= $(DESKTOP_DIR)/src-tauri/Cargo.toml
DESKTOP_TAURI_DIR ?= $(patsubst %/,%,$(dir $(DESKTOP_TAURI_MANIFEST)))
DESKTOP_VOSK_VENDOR_DIR ?= $(DESKTOP_TAURI_DIR)/vendor/vosk
DESKTOP_VOSK_BUNDLE_DIR ?= $(DESKTOP_TAURI_DIR)/vosk-model
RUNTIME_ROOT_DIR ?= $(or $(MAGICIAN_ROOT_DIR),$(MAGICIAN_STORAGE_PATH),$(HOME)/MagicianNotes)
DESKTOP_VOSK_RUNTIME_DIR ?= $(RUNTIME_ROOT_DIR)/vosk-model
DESKTOP_TRAY_BIN ?= $(CARGO_TARGET_DIR)/debug/magician-desktop
HOST_GATEWAY_URL ?= http://127.0.0.1:3017
HOST_GATEWAY_PORT ?= 3017
DESKTOP_TRAY_LAUNCHD_LABEL ?= ai.magicbeans.magican.desktop.debug
DESKTOP_MANAGE_RUNTIME ?= 0
UI_DEV_LOG_FILE ?= $(CURDIR)/magician-ui-dev.log
HOST_TRAY_LOG_FILE ?= $(CURDIR)/magician-host-tray.log
SERVICE ?= stack

# Default target
help:
	@echo "MCP Proxy Development Commands:"
	@echo ""
	@echo "  test-memory-lifecycle    - Focused lifecycle regressions with an Evals report"
	@echo "  test-memory-lifecycle-live-eval - Frozen model journeys; optional profile comparison"
	@echo "  test-memory-lifecycle-evals-integration - Focused Run/history/report contracts"

	@echo "Setup (run after fresh clone):"
	@echo "  install                  - Composed installer (MODE=dev|user FLOW=local|container DATA_DIR=... RUNTIME=docker|apple-container [YES=1])"
	@echo "  setup-all                - Install all deps + build SDK artifacts (bots, tools, ui, rust, Android on macOS)"
	@echo "  setup-hooks              - Configure this clone/worktree to use .githooks"
	@echo "  worktree-add             - Create a Git worktree and configure its hooks"
	@echo "                            Usage: make worktree-add WT_PATH=../my-worktree WT_BRANCH=my-branch [WT_BASE=origin/main]"
	@echo "  setup-ui-deps            - Install + verify Unified UI build/test/report dependencies"
	@echo "  setup-rust-test-report-deps - Install + verify nextest and LLVM coverage tools"
	@echo "  setup-test-suite-runner-deps - Install/verify isolated Python deps for test/eval harnesses"
	@echo "  setup-protoc            - Install/verify protoc required by Rust workspace builds"
	@echo "  setup-ffmpeg            - Install/verify ffmpeg + ffprobe required by media-edit tests/runtime"
	@echo "  setup-desktop-pnpm      - Install/verify desktop/package.json's pnpm pin locally"
	@echo "  setup-magios-vosk       - Fetch + verify gitignored iOS Vosk framework/model artifacts"
	@echo "  verify-magios-vosk      - Read-only integrity check for the iOS Vosk artifacts"
	@echo "  setup-pi-coding-agent    - Reconcile Pi CLI to the exact reviewed run_coding_task pin"
	@echo "  setup-meet-bot           - Install meeting audio (macOS BlackHole 16ch; Linux Pulse + Xvfb)"

	@echo "  setup-ollama             - Install Ollama + pull configured embedding and mapped generation models"
	@echo "  setup-ollama-embedding   - Install Ollama + the required PPLX memory embedder only"
	@echo "  setup-local-generation   - List/install/pin a local generation model (MODEL=woof-4b SELECT=1)"
	@echo "  setup-local-generation-selected - Install the model already selected by Desktop setup"
	@echo "  setup-prerequisites      - Optional: ensure brew, node, python3, uv, git, protoc, ffmpeg (macOS only)"
	@echo "  setup-cua-driver         - Install CuaDriver on the desktop host (ARGS=--start to start and verify)"
	@echo "  check-cua-driver         - Read-only CUA check (ARGS=--relay for a headless backend)"
	@echo "  export-apple-signing-setup - Encrypt Apple/Tauri release keys for another Mac (OUTPUT=...)"
	@echo "  import-apple-signing-setup - Import that bundle on another Mac (BUNDLE=...)"
	@echo "  verify-apple-signing-setup - Verify this Mac's signing setup (ARGS=--network for Apple auth)"
	@echo "  release-desktop-signed  - Build native Desktop, sign/notarize/staple app+DMG (JOBS=2)"
	@echo "  ensure-make              - Ensure GNU make is installed + on system PATH (run if tray Services menu can't find make)"
	@echo ""
	@echo "  # All skill-related setup, build, install, clean lives in skillshub/Makefile."
	@echo "  # Run \`make -C skillshub help\` to see the full list (setup-deps,"
	@echo "  # setup-agent-browser, setup-skill-bins, setup-officecli, setup-system-bins, setup-python,"
	@echo "  # setup-env, setup-gws-accounts, setup-bots, setup-marimo, setup-metabase-cli,"
	@echo "  # install-scope, validate, list, clean, clean-runtime,"
	@echo "  # verify-agent-browser, agent-browser-source, rebuild-agent-browser,"
	@echo "  # refresh-metabase-spec, regen-metabase-cli)."
	@echo "  # Backward-compat shims here delegate; new flows should call skillshub directly."
	@echo ""
	@echo "Cleanup:"
	@echo "  clean                    - cargo clean (Rust build artifacts only)"
	@echo "  clean-skills             - skillshub clean: node_modules, .venv, dist/, bin/, .pp-build/"
	@echo "  clean-skills-runtime     - skillshub clean-runtime: system/skills/ + scope skills/bots copies"
	@echo "  clean-skills-all         - skillshub clean + clean-runtime"
	@echo "  clean-all-artifacts      - cargo clean + skillshub clean-all (preserves .env, config/, auth/)"
	@echo ""
	@echo "Build & Run:"
	@echo "  Rust target dir         - $(CARGO_TARGET_DIR) (override with CARGO_TARGET_DIR=...)"
	@echo "  Test report dir         - $(COVERAGE_BASE_DIR) (override with COVERAGE_BASE_DIR=...)"
	@echo "  rustc job cap           - jobs=$(or $(CARGO_BUILD_JOBS),unset) gate=$(RUSTC_JOB_GATE) slots=$(or $(RUSTC_JOB_GATE_SLOTS),default)"
	@echo "  build-all-release        - Build all binaries for release (magician + magicutor + supervisor)"
	@echo "  build-all-release-ollama - Build all with Ollama env vars"
	@echo "  build-magician-release   - Build just magician (release mode)"
	@echo "  build-all-debug          - Build runtime binaries + desktop tray + native audio engine + iOS debug"
	@echo "  build-magician-debug     - Build just magician in debug and copy it into magician.bin"
	@echo "  generate-magican-app-icons  - Regenerate iOS, Android, and Tauri icons from the Caveat M glyph"
	@echo "  generate-magican-control-symbol - Regenerate the iOS Control Center Magican talk SF Symbol"
	@echo "  replace-restart-magician - Copy \$${CARGO_TARGET_DIR:-$(CARGO_TARGET_DIR)}/\$${BUILD_PROFILE:-debug}/magician into magician.bin and restart it"
	@echo "  replace-restart-magicutor - Copy \$${CARGO_TARGET_DIR:-$(CARGO_TARGET_DIR)}/\$${BUILD_PROFILE:-debug}/magicutor into magicutor.bin and restart it"
	@echo "  build-decision-engine-release - Build only the decision engine and install decision-engine.bin (MLX on Apple Silicon; DECISION_ENGINE_MLX=0 for CPU)"
	@echo "  build-decision-engine-mlx-release - The same with Kev on the GPU (kev-mlx; needs cmake, Metal Toolchain, Rust 1.95)"
	@echo "  build-decision-engine-mlx-debug - Debug MLX decision engine for local development"
	@echo "  replace-restart-decision-engine - Copy the built decision-engine into decision-engine.bin and restart it"
	@echo ""
	@echo "  # Legacy aliases:"
	@echo "  # build-all-dev          - Alias for build-all-debug"
	@echo "  # build-magician-dev     - Alias for build-magician-debug"
	@echo ""
	@echo "UI (Unified Monorepo):"
	@echo "  build-ui                 - Build unified-ui (/presto primary workspace; /done compatibility)"
	@echo "  check-ui                 - Type check unified-ui"
	@echo "  test-ui                  - Run Unified UI tests with coverage + a clickable HTML report"
	@echo "  test-envoy-claims-review  - Check Envoy receipts, Claims Review APIs, native UI and app navigation"
	@echo "  test-ui-verbose          - Run the same UI report flow with verbose terminal output"
	@echo "  benchmark-media-recording-stt - Directly benchmark Dictation STT providers without Magician (MEDIA_STT_BENCHMARK_ARGS=...)"
	@echo "  benchmark-media-offline-audio - Directly evaluate cached VAD/STT/TTS/diarization models without Magician"
	@echo "  test-media-offline-audio-eval - Run provider-free metric, selection, and schema regressions"
	@echo "  test-realtime-local-transcript - Run focused local-transcript Rust, web, iOS, and eval regressions"
	@echo "  test-realtime-local-transcript-live - Run opt-in local transcript quality/recovery/cost gate"
	@echo "  test-gemini-live-models - Run provider-free Gemini Live contract, payload, pricing, and picker-order regressions"
	@echo "  test-gemini-live-models-live - Open every shipped Gemini Live model (3.8 Live, 3.8 Extended Thinking, 3.1 Flash) and prove setup, tool round-trip, interaction status, and priced usage"
	@echo "  benchmark-chat-context-retrieval - Measure live memory/procedure chat retrieval without an LLM generation call"
	@echo "  test-chat-context-retrieval-eval - Run provider-free timing/concurrency/fallback regressions"
	@echo "  test-chat-context-retrieval-live-eval - Gate live hybrid/coalescing plus optional and memory-writer contention latency"
	@echo "  test-tool-result-projection-context-live-eval - Runtime-backed six-repeat counterbalanced projection/context quality, privacy, token, cost, and latency gate"
	@echo "  test-provider-replay-eval-harness - Run provider-free replay/checkpoint matrix and report regressions"
	@echo "  test-provider-replay-live-eval - Gate interrupted-history repair across every configured provider family"
	@echo "  test-preplan-flow-live-eval - Gate real mapped-profile planning, HITL, Plan-panel, Attention, replan, graph, and telemetry flow (PREPLAN_LIVE_PROMPT='...')"
	@echo "  eval-preplan-flow-live-interactive - Run the same live pre-plan gate with HITL answers read from this terminal"
	@echo "  test-web-researcher-live-eval - Evaluate direct/delegated web answers, citations, telemetry, and observed latency"
	@echo "  test-harness-conformance-live-eval - Swap the chat/execution engine for each installed harness and grade by effect (HARNESS_CONFORMANCE_LANE=chat|execution|voice|plane)"
	@echo "  test-content-reader-eval - Run provider-free static-reader quality and contract evaluation"
	@echo "  test-content-retrieval-eval - Run hermetic Phase 7/8 ladder, browser, handler, manifest, and extraction gates"
	@echo "  test-content-retrieval-runtime-live-eval - Run production resolver/compiled-handler retrieval checks"
	@echo "  test-working-set-eval-harness - Provider-free Boundary B: working-set lane vs context packing on planted facts, plus the live evaluator's self-test"
	@echo "  test-working-set-live-eval - Live Boundary B: a model answers each planted probe from each lane's bytes; the evidence the working-set routing gate waits for"
	@echo "  test-working-set-routing-eval-harness - Provider-free: the router's unit tests, the deterministic suite driven through the read seam, and the A/B's report contract"
	@echo "  test-working-set-routing-live-eval - Live A/B on the real web-researcher: lane off vs on by config swap; verdict must not regress, every on run decided under the open lane, every narrowed run must search, cost and tokens must not rise"
	@echo "  test-working-set-routing-as-configured - After opening or closing a lane: run the web-researcher against the live config as found (no writes) and check routing matches what the file says"
	@echo "  test-task-recipes-eval - Compile and replay the provider-free 12-case Task Recipes corpus"
	@echo "  test-task-recipes-safety - Run the Task Recipes no-wrong-mutation and no-secret-leak contracts"
	@echo "  test-task-recipes-live-eval - Prove cold browser learning, warm/variant browserless replay, and drift self-healing"
	@echo "  test-task-recipes-fixture-eval - Local fixture sites: cold LLM browser run per case, then browserless warm/variant/heal/write/drift replay with fixture-side proof"
	@echo "  build-task-recipes-fixture-eval - Build the fixture-eval harness binary (the eval lanes run it without cargo)"
	@echo "  test-task-recipes-fixture-iterate - Rebuild debug magician + harness, hot-swap + restart, wait for health, run the prebuilt fixture lane"
	@echo "  test-observable-sources-live-eval - Gate shipped RSS offers, public feeds, and Observe API pagination"
	@echo "  test-storage-governance-live-eval - Gate isolated inventory, compaction, retention, and safety boundaries"
	@echo "  eval-minimax-cli-live - Live-eval the 5 MiniMax CLI (mmx-cli) skills (needs MINIMAX_API_KEY)"
	@echo "  test-memory-temperature-live-eval - Gate recall plus durable leased Ollama utility review"
	@echo "  test-memory-ann-shadow-eval-harness - Provider-free ANN shadow/flat vector-search regressions"
	@echo "  test-memory-ann-shadow-live-eval - Live memory recall under MAGICIAN_VECTOR_SEARCH=ann_shadow"
	@echo "  test-admission-300-task-eval-harness - Provider-free 300-task admission-count harness"
	@echo "  test-memory-tier-health-eval - Gate tier effectiveness (unearned promotion, working set, tier lift)"
	@echo "  llm-trace-coverage       - Audit every declared LLM call boundary, operation family, validator, and sink"
	@echo "  llm-trace-phase0-baseline - Write content-free LLM population/volume JSON and HTML reports"
	@echo "  test-llm-trace-phase0   - Run provider-free Phase 0 observability contract regressions"
	@echo "  llm-trace-phase1-audit  - Audit stable call/attempt/dispatch correlation and write HTML/JSON reports"
	@echo "  test-llm-trace-phase1   - Run provider-free Phase 1 identity and join regressions"
	@echo "  test-llm-trace-phase2   - Run provider-free Phase 2 fact, lifecycle, and journal-contract regressions"
	@echo "  test-llm-trace-phase2b  - Run focused Phase 2B durability, replay, saturation, and shutdown regressions"
	@echo "  test-llm-trace-phase2c  - Run focused Phase 2C Parquet, idempotency, scope, partition, and watermark regressions"
	@echo "  test-llm-trace-phase2d  - Run focused Phase 2D compaction, governed-read, schema, compatibility, and scope regressions"
	@echo "  test-llm-trace-phase2e  - Run focused Phase 2E REST/provider parity, scope, envelope, and fact-SQL regressions"
	@echo "  test-llm-trace-phase2f  - Run focused Phase 2F activation, mirror-dedup, replay, retention, and reconciliation regressions"
	@echo "  llm-trace-phase2f-audit - Reconcile live canonical facts, compatibility mirrors, gaps, pricing, timing, and API totals"
	@echo "  test-llm-trace-phase3   - Run sanitized capture, privacy, restricted access, replay, deletion, and retention regressions"
	@echo "  llm-trace-phase3-audit  - Audit live sanitized content without writing payloads into the JSON/HTML report"
	@echo "  test-llm-trace-phase4   - Run model-to-tool lineage, delegation, consumption, rollback, API, and UI regressions"
	@echo "  llm-trace-phase4-audit  - Audit live fact-only tool lifecycle lineage and write sanitized HTML/JSON reports"
	@echo "  verify-media-audio-rollout - Validate audio config parity and dev/desktop/container packaging boundaries"
	@echo "  generate-media-local-speech-fixtures - Generate ignored macOS TTS speech fixtures for benchmark smoke runs"
	@echo "  run-ui-dev               - Run unified-ui dev server (http://localhost:5173)"
	@echo "  stop-ui-dev              - Stop the local Vite UI listener on :5173"
	@echo "  preview-ui               - Build + serve the production bundle on :5173 (test SW/PWA here)"
	@echo "  serve-ui-tunnel          - Expose the dev UI over the Cloudflare Tunnel at https://ui.<zone>/ (phone access; gate w/ Cloudflare Access)"
	@echo "  stop-serve-ui-tunnel     - Info: the UI shares the persistent tunnel; stop it all with 'brew services stop cloudflared'"
	@echo ""
	@echo "  # Bot daemons (Telegram, WhatsApp, Kapso, Gmail) live under skillshub/bots/."
	@echo "  # Build/check via \`make -C skillshub build-bots|check-bots\` or"
	@echo "  # the root shims \`make build-bots|check-bots\`."
	@echo ""
	@echo "  # Legacy separate UIs archived in ui/archive/"
	@echo ""
	@echo "Service Control (via magic-supervisor):"
	@echo "  run-all                  - Start UI dev server + Cloudflare Tunnel (Kapso webhook) + magic-supervisor + host tray"
	@echo "  run-ollama               - Validate/start managed Ollama and prewarm configured local models"
	@echo "  stop-ollama              - HTTP-unload Ollama models and stop only a Magician-launched daemon"
	@echo "  test-ollama-residency    - Verify exact Ollama default-name/tag and context residency matching"
	@echo "  test-ollama-embedding-qos - Provider-free embedding Ollama QoS wrap dry-run (default background; writes /evals report)"

	@echo "  run-supervisor           - Start magic-supervisor only (no funnel)"
	@echo "  run-supervisor-chat-trace - Run supervisor with per-turn chat trace dumps for debugging"
	@echo "  stop-supervisor          - Stop magic-supervisor and its managed services"
	@echo "  stop-macos-audio-engine  - Reap a stale Magician FluidAudio listener on port 3029"
	@echo "  tail-service-log SERVICE=stack|magician|magicutor|ui|host-tray"
	@echo "  open-service-log SERVICE=... - Open a macOS Terminal tailing that service log"
	@echo "  memory-index-status      - Inspect the scoped LanceDB memory index"
	@echo "  memory-index-prewarm     - Rebuild the scoped memory index only when stale"
	@echo "  memory-index-optimize    - Compact and optimize the scoped LanceDB memory index"
	@echo "                             Use MEMORY_INDEX_REBUILD_FLAGS=--force to hard-rebuild during prewarm"
	@echo "  audio-engine-status      - Inspect configured audio engine/model residency (AUDIO_ENGINE=fluid_audio)"
	@echo "  audio-model-prewarm      - Load AUDIO_MODEL through Magician's managed audio engine"
	@echo "  audio-model-unload       - Unload AUDIO_MODEL, or all models when AUDIO_MODEL is empty"
	@echo "  analytics-reprice-llm    - Recompute historical llm_calls cost_usd against current pricing"
	@echo "                             Dry-run by default; ARGS=\"--apply\" rewrites changed Parquet files"
	@echo ""
	@echo "Host-native Dev:"
	@echo "  build-desktop-tray       - Build the Tauri tray/gateway in debug; stages ./$(DESKTOP_TRAY_REPO_BIN)"
	@echo "  check-desktop-target     - Compile-check desktop for TARGET=<rust-target> without bundling"
	@echo "  test-desktop-tray        - Run desktop Svelte check, UI model tests + Tauri Rust tests"
	@echo "  test-desktop-container-routing - Focused tray/Apple inspection regressions"
	@echo "  test-desktop-managed-container - Opt-in isolated Linux backend pairing/recreation acceptance"
	@echo "  test-runtime-service-endpoints - Host routing and reviewed app endpoint regressions"
	@echo "  test-desktop-auth         - Native credential persistence, origin routing and webview hydration"
	@echo "  run-desktop-tray         - Launch ./$(DESKTOP_TRAY_REPO_BIN)"
	@echo "  restart-desktop-tray     - Restart the staged debug tray detached; return the shell immediately"
	@echo "  build-macos-audio-engine  - Build and stage the supervised FluidAudio sidecar"
	@echo "  build-macos-speech-helper - Build and stage the Apple Speech / meet-audio helpers the tray and media rails spawn"
	@echo "  test-macos-audio-engine   - Run FluidAudio sidecar protocol and PCM tests"
	@echo "                             Every artifact is also staged at the repo root next to magician.bin"
	@echo ""
	@echo "Runtime Service Commands:"
	@echo "  restart-magician         - Restart Magician backend service"
	@echo "  restart-magicutor        - Restart Magicutor backend service"
	@echo "  restart-decision-engine  - Restart the decision engine (supervised when decision-engine.bin exists)"
	@echo "  stop-magician            - Stop Magician backend service (also kills bot daemons)"
	@echo "  stop-magicutor           - Stop Magicutor backend service"
	@echo "  stop-bots                - Kill bot daemons + detached subprocs (gws watchers, agent-browser sessions + their Chrome children, CloakBrowser)"
	@echo "  status-magician          - Check Magician service status"
	@echo "  status-magicutor         - Check Magicutor service status"
	@echo "  connect-setup            - Provision connect.<zone> Access and route it to MAGICIAN_CONNECT_BACKEND (default local)"
	@echo "  connect-status           - Show the saved public-route choice and listener health"
	@echo "  connect-local            - Route the stable device hostname to the healthy native backend"
	@echo "  connect-container        - Route the stable device hostname to the healthy container backend"
	@echo "  connect-remote           - Route the stable device hostname to CONNECT_REMOTE_URL (HTTPS)"
	@echo "  supervisor-health        - Check supervisor health status"
	@echo "  tail-magician-v2-log     - Stream Magician V2 log entries to magician_v2.log"
	@echo ""
	@echo "  # Run commands (commented out):"
	@echo "  # run                  - Run the proxy with default config"
	@echo "  # run-config           - Run with config.yaml"
	@echo "  # run-release-openai   - Run release with OpenAI API key"
	@echo "  # run-release-env      - Run release mode with .env file support"
	@echo "  # run-dev-env          - Run in dev mode with .env file support"
	@echo "  # dev                  - Run in development mode with debug logging"
	@echo ""
	@echo "  # Smart Discovery Run Options (Real Semantic Search):"
	@echo "  # run-release-ollama   - Ollama server (RECOMMENDED for local development)"
	@echo "  # run-release-semantic - OpenAI embeddings (RECOMMENDED for production)"
	@echo "  # run-release-external - Custom embedding API"
	@echo ""
	@echo "  # Smart Discovery Run Options (Hash Fallbacks - Testing Only):"
	@echo "  # run-release-local    - Hash fallback (all-MiniLM-L6-v2)"
	@echo "  # run-release-hq       - Hash fallback (all-mpnet-base-v2)"
	@echo ""
	@echo "  # Embedding pre-generation disabled (magictunnel removed from workspace)"
	@echo ""
	@echo "  # Other pregenerate commands (commented out):"
	@echo "  # pregenerate-embeddings-openai  - OpenAI API (RECOMMENDED for production)"
	@echo "  # pregenerate-embeddings-external - Custom embedding API"
	@echo ""
	@echo "  # Embedding Pre-generation (Hash Fallbacks - Testing Only):"
	@echo "  # pregenerate-embeddings-local   - Hash fallback (all-MiniLM-L6-v2)"
	@echo "  # pregenerate-embeddings-hq      - Hash fallback (all-mpnet-base-v2)"
	@echo "  # pregenerate-embeddings         - Uses config model (may be fallback)"
	@echo ""
	@echo "  # Environment Support (commented out):"
	@echo "  # ENV=production make run-release-env  - Use .env.production"
	@echo "  # ENV=staging make run-release-env     - Use .env.staging"
	@echo "  # ENV=development make run-dev-env     - Use .env.development"
	@echo ""
	@echo "Testing:"
	@echo "  test                  - Run all non-live suites (including Android/iOS), then ask whether to run live evals"
	@echo "  test live_evals=true  - Run live evals last without prompting"
	@echo "  test live_evals=false - Skip live evals without prompting"
	@echo "  test-verbose          - Run the same reported suite with verbose Rust/UI output"
	@echo "  test-live-evals       - Run only the real-provider and resident-local-model eval suite"
	@echo "  test-suite-runner     - Verify live eval ordering, prompting, and explicit true/false behavior"
	@echo "  test-agent-tool-visibility-eval-harness - Run provider-free authorization eval/report regressions"
	@echo "  test-agent-tool-visibility-live-eval - Run authorization + surface hot/deferred baseline/candidate live gates"
	@echo "  test-agent-surface-runtime-eval - Run shared cache/family parity eval + HTML report"
	@echo "  test-capability-recall-eval - Provider-free tool_search + find_agents recall over all packs/agents"
	@echo "  test-attention-historical-bootstrap-eval - Run frozen provider-free attention migration/ranking replay"
	@echo "  ollama-chunking-readiness - Provider-free Phase 7 config + planner report"
	@echo "  ollama-chunking-shadow-eval - Cost-bearing local/cloud Phase 7 verification report"
	@echo "    LOCAL_CHUNK_EVAL_RUNTIME=mlx-lm records an MLX compatibility lane honestly"
	@echo "  test-rust             - Run every Rust test with coverage + a clickable HTML report"
	@echo "  test-rust-verbose     - Run the same Rust report flow with verbose terminal output"
	@echo "  test-agentic-default-stack - Run deep-input, entry-point, and 1,000-iteration qualification with ordinary stack defaults"
	@echo "  test-terminal-outbox       - Run terminal recovery and deleted-task retirement regressions"
	@echo "  test-ios              - Run Magios UNIT tests with code coverage (blocking gate; macOS + Xcode)"
	@echo "  test-concurrent-voice-ios - Run focused native voice coordinator tests"
	@echo "  test-ios-live-concurrent-voice - Test queue and speech on an enrolled physical iPhone"
	@echo "  test-ios-ui           - Run Magios UI smoke tests (MagiosUITests) on demand, retry-on-failure"
	@echo "  test-ios-live-chat    - Opt-in chat/restart acceptance on an enrolled physical iPhone"
	@echo "  test-ios-live-recovery - Terminate an active iPhone turn and verify canonical recovery"
	@echo "  test-ios-live-today   - Opt-in Today widget acceptance on an enrolled physical iPhone"
	@echo "  test-ios-live-notes   - Open the in-app notes library on an enrolled iPhone"
	@echo "  test-ios-live-appearance - Capture Longhand Day/Night on an enrolled physical iPhone"
	@echo "  test-ios-live-upload  - Share a synthetic PDF from Safari and verify its chat reply"
	@echo "  test-ios-live-playback - Verify backend reply audio on an enrolled physical iPhone"
	@echo "  test-ios-live-session-routing - Verify live conversation routing with a second phone"
	@echo "  test-ios-live-enrollment - Confirm a supplied connection link and restart persistence"
	@echo "  test-ios-live-question - Answer a pending test question and verify chat continuation"
	@echo "  test-magesp-connection - Check ESP32 backend switching and authenticated readiness"
	@echo "  build-magesp          - Build ESP32-C6 firmware on the build volume with one job"
	@echo "  test-agent-router     - Run agent router tests"
	@echo "  test-data-structures  - Run data structure tests"
	@echo "  test-streaming        - Run streaming protocol tests"
	@echo "  test-performance      - Run performance tests"
	@echo "  test-grpc             - Run gRPC integration tests"
	@echo "  test-integration      - Run integration tests"
	@echo "  test-mcp-server       - Run MCP server tests"
	@echo "  test-phase4-local     - Run offline CLI lifecycle contracts and Magician process-owner tests"
	@echo "  test-phase5g          - Run local governance plus exact-pinned official MCP conformance"
	@echo "  test-phase5g-local    - Run the offline/local Phase 5G qualification packages"
	@echo "  test-mcp-official-conformance - Run official Rust SDK client suites for both supported MCP versions"
	@echo "  test-registry         - Run registry service tests"
	@echo "  test-app-registry-admission - Run fast App registry queue/concurrency tests"
	@echo "  test-app-storage       - Run App Storage HTTP and production inventory tests"
	@echo "  test-capabilities     - Run capability file validation"
	@echo "  test-tool-skills      - Run hermetic contract tests over every governed tool skill"
	@echo "  test-tool-skills-live - Call tool canaries through the governed runtime (TOOL_CANARY_SKILLS=a,b focuses the run; spends money)"
	@echo "  test-pty-live          - Run opt-in PTY smokes against installed operator CLIs"
	@echo ""
	@echo "Code Quality:"
	@echo "  check-all              - Run Rust, Unified UI, Android build/tests, and iOS compile checks"
	@echo "  check-service-name-boundary - Guard product/agent copy from backend service-name leakage"
	@echo "  check-phase5g-conformance - Verify Phase 5G pins and evidence mapping offline"
	@echo "  check-rank-recompute-semantics - Guard the result-semantics vocabulary across backend, fixture, and web"
	@echo "  check-notes-capture-surface - Guard the save-to-notes action id and bearer-only capture contract"
	@echo "  setup-magdroid-build - Install JDK 21, the Android SDK, and the gradle wrapper jar"
	@echo "  build-magdroid   - Build the Android automation companion (skips without an Android SDK)"
	@echo "  test-magdroid    - Run the Android companion unit tests (skips without an Android SDK)"
	@echo "  check-magdroid   - Build the companion and run its tests"
	@echo "  install-magdroid - Build, then install the debug APK on the attached device"
	@echo "  run-magdroid     - Build, install, and cold-start the app"
	@echo "  logs-magdroid    - Tail logcat for the running companion"
	@echo "  screenshot-magdroid - Capture the device screen (OUT=path.png)"
	@echo "  check-device-bridge-protocol - Guard the bounded Android MCP contract across Rust and Kotlin"
	@echo "  check-storage-catalog - Guard the canonical storage catalog against inventory and I/O drift"
	@echo "  check-typed-storage-boundaries - Task 21 ratchet: reject new implicit-path storage bypasses"
	@echo "  docs-remind            - Non-blocking docs-guard reminder over the working tree"
	@echo "  check-changelogs       - Fail if any per-crate CHANGELOG exceeds the retention limit"
	@echo "  trim-changelogs        - Keep the 5 newest entries per CHANGELOG; archive the rest"
	@echo "  test-storage-gate1 - Disposable Decision Gate 1 spike (SQLite always; Postgres if MAGICIAN_GATE1_POSTGRES_URL)"
	@echo "  test-storage-s3 - Dormant S3 object/dataset adapter conformance (hermetic; not default startup)"
	@echo "  test-storage-state - Dormant SQLite/Postgres repository foundations and SQL leases"
	@echo "  test-storage-migration - Dormant owner-closure and migration coordinator (not default startup)"
	@echo "  test-storage-budgets - Enforce Gate 3 wall-clock budgets one test at a time (the report lane only records them)"
	@echo "  check-magician         - Run cargo check on magician only"
	@echo "  check-magic-supervisor - Run cargo check on magic-supervisor only"
	@echo "  check-ollama-chokepoint- Drift gate: fail on hand-built /api/generate|/api/chat POST outside the OllamaProvider allowlist (wire into make check)"
	@echo "  sccache-stats          - Show Rust compiler cache statistics"
	@echo "  sccache-clear          - Clear the Rust compiler cache"
	@echo "  prune-incremental      - Prune the shared incremental cache to INCREMENTAL_BUDGET_GB (oldest first, never one in use); CRATE=<name> clears one crate's caches after an incremental-compilation ICE; WORKSPACE_ARTIFACTS=1 cargo-cleans every workspace crate's stale generations under deps/ (refused while a build runs)"
	@echo "  check-build-space      - Report free space on the build volume and fail before cargo does when it is below INCREMENTAL_MIN_FREE_GB"
	@echo "  fmt                    - Format code with rustfmt"
	@echo "  fmt-check              - Check code formatting"
	@echo "  clippy                 - Run clippy lints"
	@echo "  clippy-all             - Run clippy with all features"
	@echo "  dev-check              - Run full development check (fmt + clippy + test + build)"
	@echo "  graph-index            - Generate ALL codegraph artifacts (graph + contracts + dead-code + coverage + audit); skips when no source changed since the last index"
	@echo "  graph-index FORCE=1    - Same, but reindex even when nothing changed (alias: graph-index-force)"
	@echo "  graph-contracts        - Only extract behavioral contracts (validation rules, constants, type shapes)"
	@echo "  graph-all              - Alias for graph-index (back-compat)"
	@echo "  graph-audit            - Audit graph coverage vs filesystem (per-ext gaps + stale entries)"
	@echo "  graph-dead-code        - Dead-code candidates (functions w/ no production callers) [also run by graph-index]"
	@echo "  graph-coverage         - Structural test-coverage buckets from the call graph [also run by graph-index]"
	@echo "  graph-check            - Validate generated code graph artifact schema"
	@echo "  graph-serve            - Serve docs/codegraph + dev make API at http://localhost:8077"
	@echo "  graph-serve-restart    - Restart codegraph explorer in the background (logs to /tmp/codegraph_dev_server.log)"
	@echo "  graph-serve-stop       - Stop any codegraph explorer listening on :8077"
	@echo "  event-taxonomy-codegen - Regenerate ui/.../event-taxonomy.ts from realtime_events.rs (run after editing the taxonomies! macro)"
	@echo "  event-taxonomy-check   - Drift gate: verify the TS taxonomy mirror is in sync with the Rust source (wired into check-all/test/build)"
	@echo "  setup-wizard           - Guided installer: pick capabilities, it works out what to install (ARGS=--status to just look)"
	@echo "  component-graph        - Render component-graph.html from graph.yaml (open it in a browser)"
	@echo "  component-graph-check  - Drift gate: verify that page matches the graph (wired into check-all/test/build)"
	@echo "  presentation-identity-codegen - Regenerate Rust/web/iOS/Android presentation identity constants"
	@echo "  presentation-identity-check   - Verify generated identity constants and migrated consumers"
	@echo "  app-contract-codegen          - Regenerate supported-public Apps schemas/OpenAPI/client fixtures"
	@echo "  app-contract-check            - Verify generated app contract artifacts have not drifted"
	@echo "  test-app-contract-clients     - Round-trip the generated web and Swift fixture contracts"
	@echo "  test-app-typescript-sdk       - Build and run the focused supported-public TypeScript SDK suite"
	@echo "  test-app-typescript-consumer-canary - Pack/install the private SDK in a clean external HTTP canary"
	@echo "  test-app-typescript-real-server-canary - Run the packed SDK against real authenticated Apps routes/stores"
	@echo "  test-app-authoring            - Run app authoring, procedure publication, and review-candidate tests"
	@echo "  test-app-scripted-surfaces    - Verify embedded page credentials, serving, and API authentication"
	@echo ""
	@echo "Development:"
	@echo "  watch-test    - Watch for changes and run tests"
	@echo "  # watch-run   - Watch for changes and run proxy (commented out)"
	@echo "  docs          - Generate and open documentation"
	@echo "  clean         - Clean build artifacts"
	@echo ""
	@echo "Container:"
	@echo "  build-container          - Build Docker image locally (magician:dev)"
	@echo "  build-container-runtime  - Build the three Linux services inside the image builder"
	@echo "  build-container-runtime-zig - Cross-compile the three Linux services directly on macOS"
	@echo "  build-container-tools-image - Add Linux skill executables to a cached image; no service/UI build"
	@echo "  build-container-prebuilt-tools-image - Package qualified Linux tool binaries without compiling"
	@echo "  build-container-keyring-image - Add persistent Linux pairing dependencies to a cached image"
	@echo "  test-container-keyring-provisioning - Private host custody and safe recreation preflight"
	@echo "  container-oci            - Export/capture/package/verify prebuilt OCI artifacts (CONTAINER_OCI_ARGS)"
	@echo "  build-container-sdk      - Prebuild the reusable Linux compiler SDK (no application build)"
	@echo "  prebuild-container-artifacts - Build Linux binaries/UI with persistent caches (CONTAINER_ARTIFACT_ARGS)"
	@echo "  prepare-container-image  - Prepare/import a verified OCI image from HEAD; ARGS=--dry-run previews"
	@echo "  prepare-container-image-local - Automatic cached local image; ARGS=--force-rebuild-from-scratch overrides"
	@echo "  test-prepare-container-image - Offline one-command pipeline regressions; no builds/containers"
	@echo "  test-container-oci       - Offline OCI packaging and build-helper regression tests"
	@echo "  build-container-multiarch - Build multi-arch image (amd64 + arm64)"
	@echo "  push-container           - Push image to ghcr.io/magicbeanbs100x/magician"
	@echo "  run-container            - Run container for local testing"
	@echo "  stop-container           - Stop and remove dev container"
	@echo "  test-container           - Build and run container integration tests"
	@echo "  test-container-full      - Build from scratch and run container integration tests"
	@echo "  test-container-tooling   - Run provider-free qualification/measurement script tests"
	@echo "  test-container-readiness - Offline host-forwarding and Linux executable checks"
	@echo "  qualify-container-release - Qualify an existing image and retain JSON evidence"
	@echo "  qualify-container-e2e    - Build and qualify a disposable real local container"
	@echo "  qualify-container-e2e-live - Install and qualify the live personal stack (explicit confirmation)"
	@echo "  qualify-container-integration - Attach to a test container for host/browser/skill probes (CONTAINER_INTEGRATION_ARGS=...)"
	@echo "  test-container-integration-harness - Run offline harness regressions; no runtime or build"
	@echo "  measure-container-image  - Measure multi-arch compressed/expanded image size"
	@echo ""
	@echo "Release:"
	@echo "  release-container        - Build + push multi-arch container image (version tagged)"
	@echo "  release-desktop          - Build desktop app for current platform"
	@echo "  build-native-backend-release - Build the three backend services for native packaging"
	@echo "  release-desktop-native   - Build backend, stage it, then build the Desktop installer"
	@echo "  verify-desktop-native-release - Verify the backend inside a finished local Desktop bundle"
	@echo "  release-desktop-target   - Build desktop for specific target (TARGET=aarch64-apple-darwin)"
	@echo "  stage-desktop-native-backend - Stage existing backend binaries into Desktop resources"
	@echo "  test-native-package      - Verify package/install/staging with fixture binaries"
	@echo "  release-all              - Release container + desktop for current platform"
	@echo "  generate-signing-key     - Generate Tauri updater signing keypair (one-time)"
	@echo ""
	@echo "Dev Workflow (local update testing):"
	@echo "  dev-desktop-setup        - One-time: generate signing key + install desktop JS deps"
	@echo "  dev-desktop-build        - Build desktop app with local update endpoint"
	@echo "  dev-update-server        - Run local update server (serves latest.json + artifacts)"
	@echo "  dev-container-rebuild    - Rebuild container image to trigger container update detection"
	@echo ""
	@echo "Cleanup:"
	@echo "  cleanup              - Remove everything Magician installed (tools + data)"
	@echo "  cleanup-tools        - Remove tools + container only, keep user data"
	@echo "  cleanup-data         - Remove data directories only"
	@echo "  cleanup-dry-run      - Dry run: show what would be removed"
	@echo ""
	@echo "Maintenance:"
	@echo "  install-tools - Install development tools"
	@echo "  setup-env     - Set up .env file for development"
	@echo "  audit         - Check for security vulnerabilities"
	@echo "  update        - Update dependencies"

setup-hooks:
	./scripts/setup_hooks.sh

# Point the repo-local target/ at the build volume so a cargo run that forgets
# CARGO_TARGET_DIR still writes to SSD1 instead of filling the internal disk.
# Idempotent: a correct symlink is left alone, a real directory is reported and
# never deleted. Run once per clone, alongside setup-hooks.
link-target-dir:
	@dest="$${CARGO_TARGET_DIR_BASE:-/Volumes/build/magician/ra-target}"; \
	if [ -L target ]; then \
		echo "target -> $$(readlink target) (already a symlink)"; \
	elif [ -d target ]; then \
		echo "target/ is a real directory ($$(du -sh target | cut -f1)); remove it yourself, then re-run"; \
	else \
		mkdir -p "$$dest" && ln -s "$$dest" target && echo "target -> $$dest"; \
	fi

worktree-add:
	@if [ -z "$(WT_PATH)" ] || [ -z "$(WT_BRANCH)" ]; then \
		echo "Usage: make worktree-add WT_PATH=../my-worktree WT_BRANCH=my-branch [WT_BASE=origin/main]"; \
		exit 1; \
	fi
	@if [ -n "$(WT_BASE)" ]; then \
		./scripts/worktree_add.sh "$(WT_PATH)" "$(WT_BRANCH)" "$(WT_BASE)"; \
	else \
		./scripts/worktree_add.sh "$(WT_PATH)" "$(WT_BRANCH)"; \
	fi

# Release binary filenames (avoid clashing with crate directories)
MAGICIAN_BIN ?= magician.bin
MAGICUTOR_BIN ?= magicutor.bin
SUPERVISOR_BIN ?= magic-supervisor.bin
DECISION_ENGINE_BIN ?= decision-engine.bin
# Decision engine flavour for every build path (build-all-*,
# build-decision-engine-*): MLX (Kev on the GPU) on Apple Silicon, CPU
# elsewhere. A CPU engine skips every kev-mlx model, so the configured
# memory-decision backups silently vanish. DECISION_ENGINE_MLX=0 forces CPU.
DECISION_ENGINE_MLX ?= $(shell [ "$$(uname -s)" = Darwin ] && [ "$$(uname -m)" = arm64 ] && echo 1 || echo 0)
# root so all four artifacts live next to each other. Avoids the
# stale-binary footgun (a build going to one path, a run picking up
# from another) and gives every runner / launcher a single canonical
# location to read from. Local debug and release staging atomically replace the
# final path, sign that final path with the desktop identity when available,
# and strictly verify it before launch. This preserves macOS TCC grants and the
# reciprocal native-owner Team-ID boundary across rebuilds.
DESKTOP_TRAY_REPO_BIN ?= magician-desktop.bin
DESKTOP_TRAY_DEBUG_APP ?= .local/Magican-Debug.app
DESKTOP_TRAY_DEBUG_APP_BIN ?= $(DESKTOP_TRAY_DEBUG_APP)/Contents/MacOS/magician-desktop.bin
MACOS_SPEECH_HELPER_REPO_BIN ?= magician-macos-speech-helper.bin
MACOS_MEET_AUDIO_HELPER_REPO_BIN ?= magician-macos-meet-audio.bin
MACOS_AUDIO_ENGINE_REPO_BIN ?= magician-macos-audio-engine.bin
BUILD_PROFILE ?= debug

# --- Per-worktree Rust target dir -------------------------------------------
# The Rust build cache (target dir) is huge and, critically, cargo takes an
# EXCLUSIVE file lock on it for every build/test. When two git worktrees share
# ONE target dir they (a) serialize behind that lock — parallel builds queue
# instead of running concurrently ("Blocking waiting for file lock on build
# directory") — and (b) thrash each other's incremental cache, because their
# `magician` sources fingerprint differently, so a one-character edit in one
# worktree can force a near-full rebuild in the other.
#
# Fix: when the SSD cache is writable, the PRIMARY checkout keeps the legacy
# shared dir (warm cache, no migration, no extra disk) and each LINKED worktree
# gets its own isolated dir. Fresh clones on hosts without that writable volume
# fall back to the checkout-local, gitignored target/ directory.
# Dependency compilation is still shared across all of them via sccache
# (RUSTC_WRAPPER above), so isolation costs disk for the workspace crates only,
# not the whole dependency graph. Override CARGO_TARGET_DIR (or CARGO_TARGET_ROOT
# to relocate the parent) to opt out.
CARGO_TARGET_ROOT_PREFERRED ?= /Volumes/build/magician/builds
CARGO_TARGET_ROOT_FALLBACK ?= $(CURDIR)/target
CARGO_TARGET_ROOT ?= $(shell \
	preferred="$(CARGO_TARGET_ROOT_PREFERRED)"; \
	parent="$$(dirname "$$preferred")"; \
	if [ -e "$$preferred" ]; then probe="$$preferred"; \
	elif [ -e "$$parent" ]; then probe="$$parent"; \
	else probe="$$(dirname "$$parent")"; fi; \
	if [ -d "$$probe" ] && [ -w "$$probe" ]; then \
		printf '%s' "$$preferred"; \
	else \
		printf '%s' "$(CARGO_TARGET_ROOT_FALLBACK)"; \
	fi)
MAGICIAN_WT_LINKED := $(shell if [ "`git rev-parse --git-dir 2>/dev/null`" = "`git rev-parse --git-common-dir 2>/dev/null`" ]; then echo no; else echo yes; fi)
ifeq ($(MAGICIAN_WT_LINKED),yes)
CARGO_TARGET_DIR ?= $(CARGO_TARGET_ROOT)/wt/$(notdir $(CURDIR))
else
CARGO_TARGET_DIR ?= $(CARGO_TARGET_ROOT)
endif
export CARGO_TARGET_DIR

# Rust's temporary objects can exhaust the internal disk even when target/
# is on SSD1. A command-line TMPDIR still wins for isolated build sessions.
ifneq ($(wildcard /Volumes/build/magician/tmp),)
TMPDIR := /Volumes/build/magician/tmp
export TMPDIR
endif

# Keep the composed test-runner and tool-result-projection Python dependencies
# out of the operator's system interpreter and the runtime skill environment.
# The location follows the per-worktree build cache, so concurrent worktrees
# cannot mutate one another.
TEST_SUITE_RUNNER_REQUIREMENTS ?= scripts/test-suite-runner-requirements.txt
TEST_SUITE_RUNNER_VENV ?= $(CARGO_TARGET_DIR)/test-suite-runner-venv
TEST_SUITE_RUNNER_PYTHON ?= $(TEST_SUITE_RUNNER_VENV)/bin/python

# --- FAST=1: whole-workspace fast lane --------------------------------------
# Prefix ANY existing target with FAST=1 to compile the WHOLE workspace on the
# nightly toolchain with the parallel rustc frontend (-Zthreads). This speeds up
# BOTH checks and builds across every crate — no per-crate command needed:
#     make check-all       FAST=1
#     make build-all-debug FAST=1
#     make test            FAST=1
#
# FAST uses its OWN target dir (…-fast): nightly + extra RUSTFLAGS fingerprint
# differently from the stable cache, so sharing would thrash both. The first FAST
# run is coldish (deps recompile under nightly; sccache softens it), then warm.
# Run a plain (non-FAST) `make check-all` before committing so the committed code
# is verified on the stable toolchain you actually ship.
#
# Add FAST_CRANELIFT=1 to ALSO swap in the Cranelift codegen backend for builds
# (~2-3x faster codegen). Opt-in: this graph contains `ring`, whose assembly can
# refuse to build under Cranelift — if a crate fails, drop back to plain FAST=1.
ifeq ($(FAST),1)
# Apply exactly ONCE. Recursive $(MAKE) calls (graph-index, check-ui, …) inherit
# FAST=1 as a command-line var AND the already-modified env; without this guard
# they would re-suffix the dir (…-fast-fast) and duplicate -Zthreads. The
# exported sentinel makes sub-makes skip re-application and just inherit the
# parent's CARGO_TARGET_DIR / RUSTFLAGS / toolchain from the environment.
ifndef MAGICIAN_FAST_APPLIED
override CARGO_TARGET_DIR := $(CARGO_TARGET_DIR)-fast
export CARGO_TARGET_DIR
export RUSTUP_TOOLCHAIN := nightly
FAST_THREADS ?= 8
FAST_RUSTFLAGS := -Zthreads=$(FAST_THREADS)
ifeq ($(FAST_CRANELIFT),1)
FAST_RUSTFLAGS += -Zcodegen-backend=cranelift
RUSTC_WRAPPER :=
export RUSTC_WRAPPER
# Empty NEXT disables sccache auto-detect in rustc-job-gate so Cranelift
# cannot be served LLVM objects from the sccache.
export RUSTC_JOB_GATE_NEXT :=
endif
RUSTFLAGS := $(strip $(RUSTFLAGS) $(FAST_RUSTFLAGS))
export RUSTFLAGS
export MAGICIAN_FAST_APPLIED := 1
endif
endif

# Apply the host-local rustc job gate AFTER FAST may have cleared sccache, so
# Cranelift still participates in the machine-wide cap. Skip when the wrapper
# is absent (CI) or when RUSTC_JOB_GATE= is passed.
ifneq ($(wildcard $(RUSTC_JOB_GATE)),)
ifneq ($(RUSTC_WRAPPER),$(RUSTC_JOB_GATE))
ifneq ($(strip $(RUSTC_WRAPPER)),)
# Immediate assignment: `?=` is recursive, so NEXT would later expand to the
# gate itself after WRAPPER is overwritten and exec-loop.
ifeq ($(origin RUSTC_JOB_GATE_NEXT),undefined)
RUSTC_JOB_GATE_NEXT := $(RUSTC_WRAPPER)
endif
export RUSTC_JOB_GATE_NEXT
endif
RUSTC_WRAPPER := $(RUSTC_JOB_GATE)
export RUSTC_WRAPPER
endif
endif

# Test-coverage artifacts (Rust LLVM coverage, iOS .xcresult bundles, Vitest V8
# reports) are large — multiple GB — and fully regenerable. Prefer the SSD used
# by the Rust build cache when it is writable; fresh machines without that
# volume fall back to target/coverage in the checkout. The per-lane *_REPORT_DIR
# vars below default under here; override COVERAGE_BASE_DIR to relocate all of
# them at once. On SSD-backed development machines, the repo-root `coverage/`
# can remain a gitignored symlink to the preferred directory so tooling that
# writes to a repo-relative `coverage/...` path also lands there.
COVERAGE_BASE_DIR_PREFERRED ?= /Volumes/build/magician/coverage
COVERAGE_BASE_DIR_FALLBACK ?= $(CARGO_TARGET_ROOT_FALLBACK)/coverage
COVERAGE_BASE_DIR ?= $(shell \
	preferred="$(COVERAGE_BASE_DIR_PREFERRED)"; \
	parent="$$(dirname "$$preferred")"; \
	if [ -e "$$preferred" ]; then probe="$$preferred"; \
	elif [ -e "$$parent" ]; then probe="$$parent"; \
	else probe="$$(dirname "$$parent")"; fi; \
	if [ -d "$$probe" ] && [ -w "$$probe" ]; then \
		printf '%s' "$$preferred"; \
	else \
		printf '%s' "$(COVERAGE_BASE_DIR_FALLBACK)"; \
	fi)

# The Magician lib suite has thousands of tests, many of which open temp dirs,
# DuckDB stores, mock sockets, Tokio runtimes, or background tasks. Rust's
# default "one test thread per CPU" can spike file descriptors on macOS and make
# otherwise unrelated tests fail with EMFILE. Keep Makefile-driven full-suite
# runs bounded; override with `make test-rust RUST_TEST_THREADS=8` when needed.
RUST_TEST_THREADS ?= 4
RUST_TEST_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/rust
TEST_SUMMARY_REPORT_DIR ?= $(COVERAGE_BASE_DIR)
LIVE_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/live-suite
AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/agent-surface-runtime
CAPABILITY_RECALL_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/capability-recall
CHAT_TURN_ENGINE_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/chat-turn-engine
ATTENTION_HISTORICAL_BOOTSTRAP_EVAL_REPORT ?= $(COVERAGE_BASE_DIR)/evals/attention-historical-bootstrap/latest.json
TASK_VERDICT_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/task-verdict
LIVE_EVAL_RUNS ?= 1
LIVE_EVAL_WORKERS ?= 1
LIVE_CHUNK_EVAL_RUNS ?= 5
LIVE_AUTH_EVAL_RUNS ?= 5
CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS ?= 10
CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS ?= 2
CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS ?= 500
CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS ?= 800
CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS ?= 30
CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS ?= 3
CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS ?= 4
CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS ?= 5000
CHAT_CONTEXT_RETRIEVAL_LIVE_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/chat-context-retrieval/live-suite-latest.json
CHAT_CONTEXT_RETRIEVAL_LIVE_EVAL_ARGS ?=
MEMORY_TEMPERATURE_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/memory-temperature/latest
MEMORY_CONNECTIONS_EVAL_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/memory-connections/live
MEMORY_CONNECTIONS_EVAL_PARTITION ?= smoke
MEMORY_CONNECTIONS_EVAL_REPEATS ?= 1
MEMORY_CONNECTIONS_EVAL_MAX_CALLS ?= 12
MEMORY_CONNECTIONS_EVAL_MAX_TOKENS ?= 100000
MEMORY_LIFECYCLE_EVAL_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/memory-lifecycle/live
MEMORY_LIFECYCLE_TEST_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/memory-lifecycle/deterministic
MEMORY_LIFECYCLE_EVAL_BINARY ?= $(CARGO_TARGET_DIR)/debug/examples/memory_lifecycle_eval
MEMORY_LIFECYCLE_EVAL_PROFILES ?=
MEMORY_LIFECYCLE_EVAL_REPEATS ?= 3
MEMORY_LIFECYCLE_EVAL_PARTITION ?= all
MEMORY_LIFECYCLE_TMPDIR ?= $(CARGO_TARGET_DIR)/tmp
MEMORY_ANN_SHADOW_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/memory-ann-shadow/deterministic/latest
MEMORY_ANN_SHADOW_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/memory-ann-shadow/live/latest
ADMISSION_300_TASK_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/admission-300-task/deterministic/latest
OLLAMA_EMBEDDING_QOS_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/ollama-embedding-qos/deterministic/latest
MEMORY_TEMPERATURE_LIVE_MIN_RECALL ?= 0.80
MEMORY_TEMPERATURE_LIVE_MIN_HYBRID ?= 0.80
MEMORY_TEMPERATURE_LIVE_MIN_UTILITY ?= 0.75
MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS ?= 1000
MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS ?= 120000
MEMORY_TEMPERATURE_LIVE_EVAL_ARGS ?=
COMPACTOR_EVAL_RUNS ?= 3
COMPACTOR_MIN_VALID_RATE ?= 0.9
COMPACTOR_MAX_EXHAUSTION_RATE ?= 0.1
COMPACTOR_MIN_SEMANTIC_RETENTION ?= 0.95
COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION ?= 1.0
LLM_TRACE_PHASE0_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/llm-observability-phase0/latest
LLM_TRACE_PHASE0_ARGS ?=
LLM_TRACE_PHASE1_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/llm-observability-phase1/latest
LLM_TRACE_PHASE1_ARGS ?=
LLM_TRACE_PHASE2F_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/llm-observability-phase2f/latest
LLM_TRACE_PHASE2F_ARGS ?=
LLM_TRACE_PHASE3_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/llm-observability-phase3/latest
LLM_TRACE_PHASE3_ARGS ?=
LLM_TRACE_PHASE4_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/llm-observability-phase4/latest
LLM_TRACE_PHASE4_ARGS ?=
LLM_PHASE3_PRINCIPAL ?= anonymous
LLM_PHASE3_WORKSPACE ?= default
LLM_PHASE3_CALL_ID ?=
LLM_PHASE4_PRINCIPAL ?= anonymous
LLM_PHASE4_WORKSPACE ?= default
LIVE_CHUNK_EVAL_OPERATION ?=
OLLAMA_CHUNK_EVAL_MODEL ?= qwen3.8-ud2-mtp
OLLAMA_CHUNK_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/ollama-logical-chunking
LOCAL_CHUNK_EVAL_RUNTIME ?= ollama
LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL ?=
LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE ?=
LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE ?=
LOCAL_CHUNK_EVAL_CONTEXT_TOKENS ?=
LIVE_EVAL_CONFIG ?=
TOOL_RESULT_PROJECTION_LIVE_RUNS ?= 6
TOOL_RESULT_PROJECTION_LIVE_PROFILE ?= chat-gptterra-responses-vision-toolsauto-fast
TOOL_RESULT_PROJECTION_LIVE_SCENARIOS ?=
TOOL_RESULT_PROJECTION_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/tool-result-projection-context/deterministic
TOOL_RESULT_PROJECTION_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/tool-result-projection-context/live/latest
PROVIDER_REPLAY_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/provider-replay/deterministic
PROVIDER_REPLAY_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/provider-replay/live/latest
PROVIDER_REPLAY_LIVE_PROFILES ?=
PREPLAN_LIVE_API_BASE_URL ?= http://127.0.0.1:3002
PREPLAN_LIVE_PRINCIPAL ?= anonymous
PREPLAN_LIVE_WORKSPACE ?= default
PREPLAN_LIVE_HITL_MODE ?= fixtures
PREPLAN_LIVE_RUNS ?= 1
PREPLAN_LIVE_TIMEOUT_SECS ?= 1200
PREPLAN_LIVE_PROJECTION_TIMEOUT_SECS ?= 45
PREPLAN_LIVE_HTTP_TIMEOUT_SECS ?= 120
PREPLAN_FLOW_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/preplan-flow/deterministic/latest
PREPLAN_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/preplan-flow/latest
PREPLAN_LIVE_EVAL_ARGS ?=
WEB_RESEARCHER_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/web-researcher/deterministic/latest
WEB_RESEARCHER_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/web-researcher/live/latest
WEB_RESEARCHER_LIVE_API_BASE_URL ?= http://127.0.0.1:3002
WEB_RESEARCHER_LIVE_RUNS ?= 1
WEB_RESEARCHER_LIVE_HTTP_TIMEOUT_SECS ?= 120
WEB_RESEARCHER_LIVE_CITATION_PROBE_LIMIT ?= 3
WEB_RESEARCHER_LIVE_LLM_PROFILE ?= gpt6luna-responses-toolsany
WEB_RESEARCHER_LIVE_EVAL_ARGS ?=
HARNESS_CONFORMANCE_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/harness-conformance/deterministic/latest
HARNESS_CONFORMANCE_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/harness-conformance/live/latest
HARNESS_CONFORMANCE_API_BASE_URL ?= http://127.0.0.1:3002
HARNESS_CONFORMANCE_LANE ?= chat
HARNESS_CONFORMANCE_ENGINES ?=
HARNESS_CONFORMANCE_RUNS ?= 1
HARNESS_CONFORMANCE_LIVE_EVAL_ARGS ?=
RUNTIME_PERFORMANCE_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/runtime-performance/deterministic/latest
RUNTIME_PERFORMANCE_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/runtime-performance/live/latest
RUNTIME_PERFORMANCE_LIVE_API_BASE_URL ?= http://127.0.0.1:3002
RUNTIME_PERFORMANCE_LIVE_PRINCIPAL ?= anonymous
RUNTIME_PERFORMANCE_LIVE_WORKSPACE ?= default
RUNTIME_PERFORMANCE_LIVE_LLM_PROFILE ?= gpt6luna-responses-toolsany
RUNTIME_PERFORMANCE_LIVE_DATA_ROOT ?= $(HOME)/MagicianNotes
RUNTIME_PERFORMANCE_LIVE_LOG_PATH ?= $(CURDIR)/magician.log
RUNTIME_PERFORMANCE_LIVE_REQUIRE_LOG ?= true
RUNTIME_PERFORMANCE_LIVE_BASELINE ?= $(COVERAGE_BASE_DIR)/evals/runtime-performance/live/baseline/report.json
RUNTIME_PERFORMANCE_LIVE_CAPTURE_BASELINE_IF_MISSING ?= true
RUNTIME_PERFORMANCE_LIVE_SCENARIOS ?=
RUNTIME_PERFORMANCE_LIVE_RUNS ?= 1
RUNTIME_PERFORMANCE_LIVE_TIMEOUT_SECS ?= 1200
RUNTIME_PERFORMANCE_LIVE_HTTP_TIMEOUT_SECS ?= 600
RUNTIME_PERFORMANCE_LIVE_RECOVERY_SECS ?= 5
RUNTIME_PERFORMANCE_LIVE_MAX_PEAK_RSS_MIB ?= 0
RUNTIME_PERFORMANCE_LIVE_MAX_END_GROWTH_MIB ?= 256
RUNTIME_PERFORMANCE_LIVE_MAX_STORAGE_GROWTH_MIB ?= 256
RUNTIME_PERFORMANCE_LIVE_EVAL_ARGS ?=
LIVE_EVALS ?=
live_evals ?=
live-evals ?=
# Lowercase `live_evals=true|false` is the operator-facing Make flag. Preserve the
# original uppercase spelling and a hyphenated spelling as compatibility
# aliases; the first explicitly non-empty spelling wins.
LIVE_EVAL_SETTING = $(strip $(or $(live_evals),$(live-evals),$(LIVE_EVALS)))
LIVE_EVAL_FLAG = $(if $(LIVE_EVAL_SETTING),--live-evals=$(LIVE_EVAL_SETTING),)

COVERAGE ?= 0
coverage ?=
COVERAGE_SETTING = $(strip $(or $(coverage),$(COVERAGE)))
COVERAGE_FLAG = $(if $(filter 1 true yes on,$(COVERAGE_SETTING)),--coverage,)

CARGO_NEXTEST_VERSION ?= 0.9.140
CARGO_LLVM_COV_VERSION ?= 0.8.7

# The thread cap alone is not enough: macOS ships a 256 open-file soft limit
# (launchctl `maxfiles`), and even 4 concurrent tests each opening a DuckDB
# db + .wal + lock exhaust it -> intermittent `Too many open files (os error
# 24)` in unrelated tests (ui_threads, analytics, ...). Raise the soft limit for
# the Makefile-driven full-suite runs; 16384 is far below kern.maxfilesperproc
# (122880) so it is always accepted. Override with `make test TEST_FD_LIMIT=...`.
TEST_FD_LIMIT ?= 16384

# Default log level for supervisor commands (override with RUST_LOG=debug make run-supervisor)
RUST_LOG ?= info

# Default chat trace level. Set to 1 to dump per-turn chat debug detail
# (rendered prompts, LLM response, tool calls, tool results) to the
# session-local trace dir. Off by default in production. Override on the
# command line: MAGICIAN_CHAT_TRACE=1 make run-supervisor
MAGICIAN_CHAT_TRACE ?= 0
MEMORY_INDEX_PRINCIPAL ?= anonymous
MEMORY_INDEX_WORKSPACE ?= default
MEMORY_INDEX_REBUILD_FLAGS ?=

# Build the Rust binaries (magician + magicutor + supervisor) for debug.
# Skill-side artifacts (bot dist/, marimo venv, metabase-pp-cli, patched
# agent-browser) are NOT prereqs here — they're interpreted skill binaries
# owned by `make setup-all`. Each skillshub setup-* target self-verifies
# (binary version + signature for agent-browser, --version for printing-
# press / metabase-pp-cli, etc), so setup IS the verification path.
build-all-debug: event-taxonomy-check component-graph-check presentation-identity-check
	cargo build && sleep 1 && ./scripts/replace-debug-bin.sh "$(CARGO_TARGET_DIR)/debug/magician" "$(MAGICIAN_BIN)" && ./scripts/replace-debug-bin.sh "$(CARGO_TARGET_DIR)/debug/magicutor" "$(MAGICUTOR_BIN)" && ./scripts/replace-debug-bin.sh "$(CARGO_TARGET_DIR)/debug/magic-supervisor" "$(SUPERVISOR_BIN)"
	@$(MAKE) --no-print-directory build-decision-engine-debug
	@$(MAKE) -C skillshub setup-document-to-markdown PROFILE=debug
	@$(MAKE) graph-index
	@$(MAKE) build-ui
	@$(MAKE) build-macos-audio-engine-debug
	@$(MAKE) build-macos-speech-helper-debug
	@$(MAKE) build-desktop-tray-debug
	@$(MAKE) ios-debug-build

# Legacy alias for build-all-debug
build-all-dev: build-all-debug

# Build the Rust binaries (magician + magicutor + supervisor) for release.
# Same scope as build-all-debug — Rust binaries only. See note there.
build-all-release: event-taxonomy-check component-graph-check presentation-identity-check
	cargo build --release && sleep 2 && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/magician" "$(MAGICIAN_BIN)" && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/magicutor" "$(MAGICUTOR_BIN)" && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/magic-supervisor" "$(SUPERVISOR_BIN)"
	@$(MAKE) --no-print-directory build-decision-engine-release
	@$(MAKE) -C skillshub setup-document-to-markdown PROFILE=release
	@$(MAKE) graph-index
	@$(MAKE) build-macos-audio-engine-release
	@$(MAKE) build-macos-speech-helper-release

# Manual version management (version-manager removed)
# Update version manually in Cargo.toml and update references as needed

# Build all binaries and libs for release with semantic search environment variables
build-all-release-ollama:
	OLLAMA_BASE_URL="http://localhost:11434" \
	cargo build --release && sleep 2 && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/magician" "$(MAGICIAN_BIN)" && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/magicutor" "$(MAGICUTOR_BIN)" && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/magic-supervisor" "$(SUPERVISOR_BIN)"

# Build just magician (debug mode)
build-magician-debug:
	cargo build -p magician-bin --bin magician && sleep 1 && ./scripts/replace-debug-bin.sh "$(CARGO_TARGET_DIR)/debug/magician" "$(MAGICIAN_BIN)"

# Legacy alias for build-magician-debug
build-magician-dev: build-magician-debug

# Build and stage only the supervisor for staged service-rollout validation.
build-magic-supervisor-debug:
	cargo build -p magic-supervisor && ./scripts/replace-debug-bin.sh "$(CARGO_TARGET_DIR)/debug/magic-supervisor" "$(SUPERVISOR_BIN)"
.PHONY: build-magic-supervisor-debug

# Build just magician (release mode)
build-magician-release:
	cargo build --bin magician --release && sleep 1 && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/magician" "$(MAGICIAN_BIN)"

# Replace local .bin files from an existing cargo build output
replace-magician-bin:
	./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/$(BUILD_PROFILE)/magician" "$(MAGICIAN_BIN)"

replace-magicutor-bin:
	./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/$(BUILD_PROFILE)/magicutor" "$(MAGICUTOR_BIN)"

replace-supervisor-bin:
	./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/$(BUILD_PROFILE)/magic-supervisor" "$(SUPERVISOR_BIN)"

replace-decision-engine-bin:
	./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/$(BUILD_PROFILE)/decision-engine" "$(DECISION_ENGINE_BIN)"

# Build the decision engine alone and hand it to the supervisor: a decision
# change (routes, thresholds, step judges) ships without a magician rebuild.
# Follows DECISION_ENGINE_MLX; build-all-* reach the engine only through these.
build-decision-engine-debug:
ifeq ($(DECISION_ENGINE_MLX),1)
	@$(MAKE) --no-print-directory build-decision-engine-mlx-debug
else
	cargo build -p decision-engine && ./scripts/replace-debug-bin.sh "$(CARGO_TARGET_DIR)/debug/decision-engine" "$(DECISION_ENGINE_BIN)"
endif

build-decision-engine-release:
ifeq ($(DECISION_ENGINE_MLX),1)
	@$(MAKE) --no-print-directory build-decision-engine-mlx-release
else
	cargo build --release -p decision-engine && sleep 1 && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/decision-engine" "$(DECISION_ENGINE_BIN)"
endif

# The same, with Kev on the GPU (`kev-mlx`; Apple Silicon). kev-rs needs
# Rust >= 1.95, CMake, and Xcode's Metal Toolchain to compile MLX; the
# resulting binary needs none of them.
MLX_RUST_TOOLCHAIN ?= 1.95.0
setup-decision-engine-mlx-build:
	@command -v cmake >/dev/null || { echo "❌ cmake not found (brew install cmake, or pip install cmake)"; exit 1; }
	@xcrun -sdk macosx metal --version >/dev/null 2>&1 || { echo "❌ Metal Toolchain missing: xcodebuild -downloadComponent MetalToolchain"; exit 1; }
	@rustup toolchain list | grep -q "^$(MLX_RUST_TOOLCHAIN)" || rustup toolchain install $(MLX_RUST_TOOLCHAIN) --profile minimal

build-decision-engine-mlx-debug: setup-decision-engine-mlx-build
	RUSTUP_TOOLCHAIN=$(MLX_RUST_TOOLCHAIN) cargo build -p decision-engine --features mlx && sleep 1 && ./scripts/replace-debug-bin.sh "$(CARGO_TARGET_DIR)/debug/decision-engine" "$(DECISION_ENGINE_BIN)"

build-decision-engine-mlx-release: setup-decision-engine-mlx-build
	RUSTUP_TOOLCHAIN=$(MLX_RUST_TOOLCHAIN) cargo build --release -p decision-engine --features mlx && sleep 1 && ./scripts/replace-release-bin.sh "$(CARGO_TARGET_DIR)/release/decision-engine" "$(DECISION_ENGINE_BIN)"

# Replace a managed service binary and restart the running supervised service
replace-restart-magician: replace-magician-bin
	@echo "🔁 Replacing $(MAGICIAN_BIN) from $(CARGO_TARGET_DIR)/$(BUILD_PROFILE)/magician and restarting Magician..."
	./supervisor-ctl restart-magician

replace-restart-magicutor: replace-magicutor-bin
	@echo "🔁 Replacing $(MAGICUTOR_BIN) from $(CARGO_TARGET_DIR)/$(BUILD_PROFILE)/magicutor and restarting Magicutor..."
	./supervisor-ctl restart-magicutor

replace-restart-decision-engine: replace-decision-engine-bin
	@echo "🔁 Replacing $(DECISION_ENGINE_BIN) from $(CARGO_TARGET_DIR)/$(BUILD_PROFILE)/decision-engine and restarting the decision engine..."
	./supervisor-ctl restart-decision-engine

# Build unified-ui (monorepo serving /presto and /done)
build-ui: setup-app-typescript-sdk-deps
	@echo "🎨 Building unified-ui..."
	@echo "   - Primary workspace: /presto"
	@echo "   - Compatibility routes: /done"
	cd ui/unified-ui && npm ci && npm run build
	@echo "✅ unified-ui built successfully!"

# Build the marketing site payload for Cloudflare Pages
build-marketing-site: setup-app-typescript-sdk-deps
	@echo "🎨 Building static marketing site..."
	cd ui/unified-ui && npm ci && npm run build
	@rm -rf marketing-site
	@mkdir -p marketing-site
	@cp -R ui/unified-ui/build/* marketing-site/
	@rm -rf marketing-site/vosk
# `static/` ships wholesale, so anything left there is deployed whether or not
# a page ever asks for it. Measured on the landing rebuild: a full top-to-bottom
# traverse of `/` fetches ONLY `prologue/reel-life` (the montage, ~13.5MB of the
# 369 frames it needs). The rest is 252MB of payload nothing can request —
# `reel` and `reel-diorama` are Act 0's retired films, reachable only from the
# unmounted ReelSwitcher; `src` and `pilot` are gitignored local masters and
# style probes that were never meant to leave the machine.
#
# Stripped here rather than deleted from the repo: the two reels are tracked,
# expensive to regenerate, and may yet be wanted. If a future page mounts one
# again, drop its line.
	@rm -rf marketing-site/prologue/reel marketing-site/prologue/reel-diorama
	@rm -rf marketing-site/prologue/src marketing-site/prologue/pilot
	@rm -rf marketing-site/contact-sheet
	@cp scripts/marketing-service-worker-retirement.js marketing-site/service_worker.js
	@cp scripts/marketing-service-worker-retirement.js marketing-site/service-worker.js
	@cp scripts/marketing-service-worker-retirement.js marketing-site/sw.js
	@cp scripts/marketing-site-headers marketing-site/_headers
	@cp scripts/marketing-site-404.html marketing-site/404.html
	@echo "✅ Marketing site built successfully in ./marketing-site/"

.PHONY: publish-marketing-site
publish-marketing-site:
	@bash scripts/publish-marketing-site.sh

# Type check unified-ui
check-ui: setup-app-typescript-sdk-deps
	@echo "🔍 Type checking unified-ui..."
	cd ui/unified-ui && npm ci && npm run check
	@echo "✅ unified-ui type-checked successfully!"

# Run both Vitest projects: fast Node unit tests and mounted JSDOM/Svelte tests.
# The runnable test targets invoke the idempotent installer first so a fresh
# checkout can continue; verify-ui-test-deps remains the read-only CI/preflight
# surface when mutation is not desired.
UI_TEST_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/frontend
verify-ui-test-deps:
	@if ! command -v python3 >/dev/null 2>&1; then \
		echo "❌ python3 is required for the frontend HTML report. Run 'make setup-prerequisites'."; \
		exit 1; \
	fi
	@if ! (cd ui/unified-ui && npm ls --depth=0 vitest @vitest/coverage-v8 >/dev/null 2>&1); then \
		echo "❌ Unified UI test/report dependencies are missing or incompatible. Run 'make setup-ui-deps'."; \
		exit 1; \
	fi

test-ui: setup-ui-deps
	@echo "🧪 Testing unified-ui (unit + mounted component projects)..."
	@mkdir -p '$(UI_TEST_REPORT_DIR)/results'; \
		run_id=$$(date '+%Y%m%d-%H%M%S')-$$$$; \
		run_dir='$(UI_TEST_REPORT_DIR)'/results/$$run_id; \
		mkdir -p "$$run_dir"; \
		test_status=0; \
		( cd ui/unified-ui && COVERAGE='$(COVERAGE_SETTING)' UI_TEST_RESULTS_PATH="$$run_dir/vitest-results.json" \
			UI_TEST_COVERAGE_DIR="$$run_dir/coverage" npm run test:report ) || test_status=$$?; \
		report_status=0; \
		if [ "$(COVERAGE_SETTING)" = "1" ] || [ "$(COVERAGE_SETTING)" = "true" ] || [ "$(COVERAGE_SETTING)" = "yes" ] || [ "$(COVERAGE_SETTING)" = "on" ]; then \
			python3 scripts/frontend_test_report.py --results "$$run_dir/vitest-results.json" \
				--coverage-summary "$$run_dir/coverage/coverage-summary.json" \
				--coverage-html "$$run_dir/coverage/index.html" \
				--output '$(UI_TEST_REPORT_DIR)/latest.html' --test-status "$$test_status" || report_status=$$?; \
		else \
			python3 scripts/frontend_test_report.py --results "$$run_dir/vitest-results.json" \
				--output '$(UI_TEST_REPORT_DIR)/latest.html' --test-status "$$test_status" || report_status=$$?; \
		fi; \
		if [ $$test_status -eq 0 ] && [ $$report_status -ne 0 ]; then exit $$report_status; fi; \
		exit $$test_status

.PHONY: test-concurrent-voice check-concurrent-voice
check-concurrent-voice:
	cargo check -p magician-api -p magician-media -p magician-bin --all-targets

.PHONY: test-chat-queue-actions
test-chat-queue-actions:
	cargo test -p magician --lib -- queue_actions --nocapture

.PHONY: test-terminal-artifact-links
test-terminal-artifact-links:
	cargo test -p magician --lib terminal_file_links_recover_legacy_paths -- --nocapture

test-concurrent-voice:
	cargo test -p magician -p magician-api -p magician-media --lib -- voice_requests concurrent_voice --nocapture

.PHONY: test-chat-completion-repeat
test-chat-completion-repeat:
	cargo test -p magician --lib -- concurrent_voice voice_requests chat_decision_rail_projected_empty_success_and_pending --nocapture

.PHONY: test-concurrent-voice-ui
test-concurrent-voice-ui:
	cd ui/unified-ui && npx --no-install vitest run src/lib/media/voice/concurrentVoiceCoordinator.test.ts src/lib/media/voice/providers/openai.component.test.ts src/lib/media/voice/realtimeVoiceClient.component.test.ts src/lib/stores/chatTurnEventsStore.test.ts

test-ui-verbose: setup-ui-deps
	@echo "🧪 Testing unified-ui with verbose reporting..."
	@mkdir -p '$(UI_TEST_REPORT_DIR)/results'; \
		run_id=$$(date '+%Y%m%d-%H%M%S')-$$$$; \
		run_dir='$(UI_TEST_REPORT_DIR)'/results/$$run_id; \
		mkdir -p "$$run_dir"; \
		test_status=0; \
		( cd ui/unified-ui && UI_TEST_VERBOSE=1 UI_TEST_RESULTS_PATH="$$run_dir/vitest-results.json" \
			UI_TEST_COVERAGE_DIR="$$run_dir/coverage" npm run test:report ) || test_status=$$?; \
		report_status=0; \
		python3 scripts/frontend_test_report.py --results "$$run_dir/vitest-results.json" \
			--coverage-summary "$$run_dir/coverage/coverage-summary.json" \
			--coverage-html "$$run_dir/coverage/index.html" \
			--output '$(UI_TEST_REPORT_DIR)/latest.html' --test-status "$$test_status" || report_status=$$?; \
		if [ $$test_status -eq 0 ] && [ $$report_status -ne 0 ]; then exit $$report_status; fi; \
		exit $$test_status

test-magicutor-extension:
	@echo "🧪 Testing Magicutor extension bridge and latency helpers..."
	node --test magicutor/extension/*.test.cjs

# Phone access to the dev UI now rides the shared Cloudflare Tunnel
# (ui.<zone> -> :5173), set up ONCE with `make serve-ui-tunnel`. There is no
# per-run serve to configure or tear down, so this is a silent no-op kept only as
# a stable prereq for run-ui-dev / preview-ui (so `make run-ui-dev` stays fully
# offline-friendly — it never touches cloudflared).
ensure-ui-tunnel:
	@exit 0

# Run unified-ui dev server. Phone-reachable via the Cloudflare Tunnel at
# https://ui.<zone>/ once you've run `make serve-ui-tunnel` (the named tunnel
# persists across vite restarts, so there's nothing to tear down here).
run-ui-dev: setup-app-typescript-sdk-deps ensure-ui-tunnel
	@echo "🚀 Starting unified-ui dev server..."
	@echo "   - Dev server: http://localhost:5173"
	@echo "   - Primary workspace (/presto)"
	@echo "   - Compatibility routes (/done)"
	@echo "   - Phone access: run 'make serve-ui-tunnel' once (https://ui.<zone>/ via Cloudflare Tunnel)"
	@bash -c 'exec > >(tee -a "$(UI_DEV_LOG_FILE)") 2>&1; \
	cd ui/unified-ui && npm ci && npm run dev'

stop-ui-dev:
	@echo "⏹️  Stopping unified-ui dev server on :5173..."
	@PIDS=$$(lsof -nP -iTCP:5173 -sTCP:LISTEN -t 2>/dev/null | sort -u); \
	if [ -n "$$PIDS" ]; then \
		echo "   stopping $$(echo $$PIDS | wc -w | tr -d ' ') UI dev listener(s)..."; \
		kill $$PIDS 2>/dev/null || true; \
		sleep 1; \
		REMAINING=$$(lsof -nP -iTCP:5173 -sTCP:LISTEN -t 2>/dev/null | sort -u); \
		if [ -n "$$REMAINING" ]; then \
			echo "   sending SIGKILL to remaining UI dev listener(s)..."; \
			kill -9 $$REMAINING 2>/dev/null || true; \
		fi; \
	else \
		echo "   No UI dev listener found."; \
	fi

# Build the UI and serve the production output locally on :5173. Use this (not
# run-ui-dev) when verifying anything that only activates against the built
# bundle — service worker registration, PWA install prompt, real bundle
# splitting, production-mode CSS purges. Phone-reachable via the Cloudflare
# Tunnel at https://ui.<zone>/ once `make serve-ui-tunnel` has been run.
# Vite preview defaults to :4173; --port 5173 keeps it on the tunnel's UI port.
preview-ui: build-ui ensure-ui-tunnel
	@echo "🚀 Starting unified-ui preview server (production build)..."
	@echo "   - Preview server: http://localhost:5173 (matches dev port)"
	@echo "   - Phone access: run 'make serve-ui-tunnel' once (https://ui.<zone>/ via Cloudflare Tunnel)"
	@bash -c 'cd ui/unified-ui && npm run preview -- --host --port 5173'

# Expose the unified-ui dev server (localhost:5173) over the public Cloudflare
# Tunnel at https://ui.<zone>/ (real cert) so it's reachable from a phone/tablet.
# HTTPS is mandatory for the audio/voice features that ride on getUserMedia +
# MediaRecorder + service workers — those APIs refuse to initialise outside a
# secure context. This shares the single persistent named 'magician' tunnel that
# also serves the Kapso webhook; the script (scripts/ensure-magician-tunnel.sh)
# maps ui.<zone> -> :5173 in the same config.
#
# IMPORTANT: gate ui.<zone> behind a Cloudflare Access policy so the dev UI is
# not open to the public internet (the Kapso webhook hostname stays open by
# design — it's an inbound callback endpoint).
serve-ui-tunnel:
	@MAGICIAN_ENABLE_FUNNEL=1 KAPSO_WEBHOOK_PORT=$(KAPSO_WEBHOOK_PORT) bash scripts/ensure-magician-tunnel.sh

# The dev UI shares the single persistent 'magician' Cloudflare Tunnel with the
# Kapso webhook, so there is nothing UI-scoped to tear down. To stop the whole
# tunnel (which also stops Kapso webhook ingress): 'brew services stop cloudflared'.
stop-serve-ui-tunnel:
	@echo "🔗 The dev UI shares the persistent Cloudflare Tunnel with the Kapso webhook — nothing UI-scoped to remove."
	@echo "   Stop the whole tunnel with: brew services stop cloudflared"


# Legacy targets (archived - kept for reference)
# build-ui-magician:
# 	@echo "⚠️  magician-ui has been archived. Use 'make build-ui' for unified-ui"
# 	@echo "   Location: ui/archive/magician-ui-*/"
#
# build-ui-magictunnel:
# 	@echo "⚠️  magictunnel-ui has been archived. Use 'make build-ui' for unified-ui"
# 	@echo "   Location: ui/archive/magictunnel-ui-*/"

# --- Setup targets (run after fresh clone) ---

# Top-level setup. Delegates skill setup to skillshub/Makefile, then installs
# the macOS-hosted Android toolchain, UI deps, and Rust toolchain components.
setup-all:
	@# ffmpeg first: skillshub's own setup-all runs `setup-media-fetch`, which
	@# symlinks the host ffmpeg/ffprobe and exits non-zero when they are absent.
	@# That chain is `&&`-linked, so on a fresh machine an ffmpeg installed
	@# later in this target is installed too late — skillshub setup aborts
	@# first and nothing after it runs.
	@$(MAKE) setup-ffmpeg
	@$(MAKE) -C skillshub setup-all
	@$(MAKE) setup-pi-coding-agent
	@$(MAKE) setup-meet-bot
	@$(MAKE) setup-desktop-pnpm
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		$(MAKE) setup-magdroid-build; \
	else \
		echo "Skipping Magdroid toolchain setup: the installer is macOS/Homebrew-specific."; \
	fi
	@$(MAKE) setup-ui-deps
	@$(MAKE) setup-rust-test-report-deps
	@$(MAKE) setup-test-suite-runner-deps
	@echo ""
	@echo "Installing Rust toolchain components..."
	rustup component add rustfmt clippy 2>/dev/null || true
	@echo ""
	@echo "Setup complete! All sub-projects should now type-check cleanly."

setup-ui-deps: setup-app-typescript-sdk-deps
	@echo ""
	@echo "Installing unified-ui build, test, and HTML-report dependencies..."
	cd ui/unified-ui && npm ci
	@$(MAKE) verify-ui-test-deps
	@echo "✅ Unified UI dependencies installed and verified (Vitest + V8 coverage)."

setup-desktop-pnpm:
	@if [ "$(PNPM_USER_SUPPLIED)" = "1" ]; then \
		DESKTOP_PNPM_TOOL_ROOT='$(DESKTOP_PNPM_TOOL_ROOT)' \
			bash scripts/setup-desktop-pnpm.sh --verify-command $(PNPM); \
	else \
		DESKTOP_PNPM_TOOL_ROOT='$(DESKTOP_PNPM_TOOL_ROOT)' \
			bash scripts/setup-desktop-pnpm.sh; \
	fi

setup-rust-test-report-deps:
	@echo ""
	@echo "Installing Rust test and HTML-report dependencies..."
	@CARGO_NEXTEST_VERSION='$(CARGO_NEXTEST_VERSION)' \
		CARGO_LLVM_COV_VERSION='$(CARGO_LLVM_COV_VERSION)' \
		./scripts/setup-rust-test-report-deps.sh
	@$(MAKE) verify-rust-test-report-deps
	@echo "✅ Rust test-report dependencies installed and verified."

verify-rust-test-report-deps:
	@if ! command -v python3 >/dev/null 2>&1; then \
		echo "❌ python3 is required for the Rust HTML report. Run 'make setup-prerequisites'."; \
		exit 1; \
	fi
	@if ! rustup component list --installed | grep -q '^llvm-tools'; then \
		echo "❌ Rust's llvm-tools-preview component is missing. Run 'make setup-rust-test-report-deps'."; \
		exit 1; \
	fi
	@if [ "$$(cargo nextest --version 2>/dev/null | awk 'NR == 1 { print $$2 }')" != '$(CARGO_NEXTEST_VERSION)' ]; then \
		echo "❌ cargo-nextest $(CARGO_NEXTEST_VERSION) is required. Run 'make setup-rust-test-report-deps'."; \
		exit 1; \
	fi
	@if [ "$$(cargo llvm-cov --version 2>/dev/null | awk 'NR == 1 { print $$2 }')" != '$(CARGO_LLVM_COV_VERSION)' ]; then \
		echo "❌ cargo-llvm-cov $(CARGO_LLVM_COV_VERSION) is required. Run 'make setup-rust-test-report-deps'."; \
		exit 1; \
	fi

setup-test-suite-runner-deps:
	@if ! command -v python3 >/dev/null 2>&1; then \
		echo "❌ python3 is required for repository test/eval harnesses. Run 'make setup-prerequisites'."; \
		exit 1; \
	fi
	@if ! python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)'; then \
		echo "❌ Python 3.10 or newer is required for repository test/eval harnesses (found $$(python3 --version 2>&1))."; \
		exit 1; \
	fi
	@if [ -x "$(TEST_SUITE_RUNNER_PYTHON)" ] && \
	   ! "$(TEST_SUITE_RUNNER_PYTHON)" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)'; then \
		echo "Upgrading stale test-suite Python environment..."; \
		python3 -m venv --upgrade "$(TEST_SUITE_RUNNER_VENV)"; \
	fi
	@if [ ! -x "$(TEST_SUITE_RUNNER_PYTHON)" ]; then \
		echo "Creating isolated test-suite Python environment..."; \
		mkdir -p "$(dir $(TEST_SUITE_RUNNER_VENV))"; \
		python3 -m venv "$(TEST_SUITE_RUNNER_VENV)"; \
	fi
	@PIP_CACHE_DIR="$(CARGO_TARGET_DIR)/pip-cache" \
		"$(TEST_SUITE_RUNNER_PYTHON)" -m pip install --disable-pip-version-check --quiet \
		--requirement "$(TEST_SUITE_RUNNER_REQUIREMENTS)"
	@$(MAKE) --no-print-directory verify-test-suite-runner-deps

verify-test-suite-runner-deps:
	@if [ ! -x "$(TEST_SUITE_RUNNER_PYTHON)" ]; then \
		echo "❌ Test-suite Python environment is missing. Run 'make setup-test-suite-runner-deps'."; \
		exit 1; \
	fi
	@"$(TEST_SUITE_RUNNER_PYTHON)" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' || { \
		echo "❌ Test-suite Python environment must use Python 3.10 or newer. Run 'make setup-test-suite-runner-deps'."; \
		exit 1; \
	}
	@"$(TEST_SUITE_RUNNER_PYTHON)" -c 'import jsonschema, requests, websocket, yaml' || { \
		echo "❌ Test-suite Python dependencies are incomplete. Run 'make setup-test-suite-runner-deps'."; \
		exit 1; \
	}
	@"$(TEST_SUITE_RUNNER_PYTHON)" -c 'import importlib.metadata as metadata, pathlib, sys; lines = pathlib.Path("$(TEST_SUITE_RUNNER_REQUIREMENTS)").read_text(encoding="utf-8").splitlines(); required = dict(line.split("==", 1) for line in lines if line and not line.startswith("#")); mismatches = [f"{name}=={metadata.version(name)} (expected {expected})" for name, expected in required.items() if metadata.version(name) != expected]; sys.exit(", ".join(mismatches)) if mismatches else None' || { \
		echo "❌ Test-suite Python dependency pins have drifted. Run 'make setup-test-suite-runner-deps'."; \
		exit 1; \
	}
	@PIP_CACHE_DIR="$(CARGO_TARGET_DIR)/pip-cache" \
		"$(TEST_SUITE_RUNNER_PYTHON)" -m pip check >/dev/null || { \
		echo "❌ Test-suite Python dependencies are inconsistent. Run 'make setup-test-suite-runner-deps'."; \
		exit 1; \
	}
	@echo "✓ Test-suite Python dependencies available in $(TEST_SUITE_RUNNER_VENV)"

# [Optional] Ensure brew, node, python3, uv, protoc, ffmpeg (macOS only — NOT wired into setup-all)
setup-prerequisites:  ## [Optional] Ensure brew, node, python3, uv, git, protoc, ffmpeg (macOS only)
	@bash scripts/install-prerequisites.sh

install: ## Composed installer (MODE=dev|user, FLOW=local|container, DATA_DIR=..., RUNTIME=docker|apple-container)
	@bash scripts/install.sh $(if $(MODE),--mode $(MODE),) $(if $(FLOW),--flow $(FLOW),) $(if $(DATA_DIR),--data-dir $(DATA_DIR),) $(if $(RUNTIME),--runtime $(RUNTIME),) $(if $(YES),--yes,)

setup-identity: ## Operator identity layer: prompt + write data-root .env and operator-config values (single writer)
	@bash scripts/setup-identity.sh

setup-pi-coding-agent:
	@bash scripts/setup-pi-coding-agent.sh

setup-meet-bot:
	@bash scripts/setup-meet-bot.sh

# Install Ollama on the HOST + pull the models magician uses (idempotent).
# SilverBullet + Ollama run on the host; the magician container reaches them over
# --network host. Model identities come from magician-config.yaml.
setup-ollama:
	@bash scripts/setup-ollama-host.sh

setup-ollama-embedding:
	@bash scripts/setup-ollama-embedding.sh

setup-local-generation-selected:
	@bash scripts/setup-selected-local-generation-model.sh

# Kitty of local generation models (Qwen 27B, Gemma 4 12B, Woof 4B).
# No MODEL= lists scores. MODEL=woof-4b installs. SELECT=1 pins selected.
.PHONY: setup-local-generation
setup-local-generation:
	@bash scripts/setup-local-generation-model.sh $(MODEL) $(if $(filter 1 true yes,$(SELECT)),--select,)

# The guided installer. `ARGS=--status` prints what is here and exits, which is
# also what it does with no terminal to draw on.
.PHONY: setup-wizard
setup-wizard:
	@bash scripts/setup-wizard.sh $(ARGS)

.PHONY: setup-cua-driver check-cua-driver test-cua-setup test-cua-platform check-edge-transport check-edge-connector test-edge-protocol test-edge-sessions test-edge-admission test-edge-connector test-edge-transport
setup-cua-driver:
	@python3 scripts/setup-cua-driver.py $(ARGS)

check-cua-driver:
	@python3 scripts/setup-cua-driver.py --check $(ARGS)

test-cua-setup:
	@PYTHONDONTWRITEBYTECODE=1 python3 scripts/test-cua-setup.py

test-cua-platform:
	cargo test -p runtime-core --lib cua::tests
	cargo test -p magician-components --lib cua_setup_is_available

check-edge-transport:
	cargo check -p runtime-core -p magician -p magician-api -p magician-bin -p magicutor
	$(MAKE) check-edge-connector

check-edge-connector:
	cargo check --manifest-path desktop/src-tauri/Cargo.toml

test-edge-protocol:
	cargo test -p runtime-core --lib edge::tests

test-edge-sessions:
	cargo test -p magician --lib magician_v2::runtime::edge_sessions::tests

test-edge-admission:
	cargo test -p magician --lib desktop_enrollment_mints_only_revocable_edge_authority
	cargo test -p magician-api --lib edge_bridge_handler::tests

test-edge-connector:
	cargo test -p magicutor --lib server::routes::tests::health_reports_extension_connection_state
	cargo test --manifest-path desktop/src-tauri/Cargo.toml --bin magician-desktop edge_client::tests
	cargo test --manifest-path desktop/src-tauri/Cargo.toml --bin magician-desktop edge_dispatch::tests

test-edge-transport: test-edge-protocol test-edge-sessions test-edge-admission test-edge-connector

run-ollama:
	@bash scripts/run-ollama.sh

stop-ollama:
	@bash scripts/stop-ollama.sh

.PHONY: test-ollama-residency
test-ollama-residency:
	@python3 -m unittest scripts/test_verify_ollama_residency.py

.PHONY: test-ollama-embedding-qos
## eval: kind=harness report=evals/ollama-embedding-qos/deterministic/latest
## desc: Provider-free embedding Ollama QoS wrap (default background; off is the kill switch)
test-ollama-embedding-qos:
	@python3 scripts/write_eval_harness_report.py \
		--title 'Ollama embedding QoS wrap' \
		--output-dir '$(OLLAMA_EMBEDDING_QOS_EVAL_REPORT_DIR)' \
		-- bash scripts/test-run-ollama-embedding-qos.sh

# --- AgentSkills v1 targets ---
#
# Skills are authored under skillshub/<name>/ and installed into the
# regenerable runtime tree at $(RUNTIME_ROOT_DIR)/scopes/<scope>/skills/<name>/.
# Targets here are thin pass-throughs to skillshub/Makefile so the root
# Makefile stays a one-stop shop. The standalone CLI wrapper at
# scripts/magician-skills exposes the same subcommands for users who
# don't want to type `make` every time.

.PHONY: skills-list skills-list-source skills-validate skills-install-scope

skills-list:
	@$(MAKE) -C skillshub list DATA_ROOT="$(RUNTIME_ROOT_DIR)"

skills-list-source:
	@$(MAKE) -C skillshub list-source

skills-validate:
	@$(MAKE) -C skillshub validate

skills-install-scope:
	@test -n "$(SCOPE)" || (echo "SCOPE=<principal>/<workspace> required" && exit 1)
	@$(MAKE) -C skillshub install-scope SCOPE=$(SCOPE) DATA_ROOT="$(RUNTIME_ROOT_DIR)"

# --- Bots targets ---
# Delegated to skillshub/Makefile. The bot daemons live there with
# the rest of the skill content.

build-bots:
	@$(MAKE) -C skillshub build-bots

check-bots:
	@$(MAKE) -C skillshub check-bots

setup-desktop-vosk:
	@MAGICIAN_ROOT_DIR="$(RUNTIME_ROOT_DIR)" \
		MAGICIAN_VOSK_MODEL_DIR="$(DESKTOP_VOSK_RUNTIME_DIR)" \
		VOSK_VENDOR_DIR="$(DESKTOP_VOSK_VENDOR_DIR)" \
		VOSK_BUNDLE_MODEL_DIR="$(DESKTOP_VOSK_BUNDLE_DIR)" \
		bash scripts/setup-desktop-vosk.sh

# Magios ambient-mode wake spotter: fetch + checksum the iOS libvosk static
# archive and the on-device model. Both are gitignored, so this is a one-time
# step per clone/worktree. Runnable iOS tests invoke it automatically; project
# regeneration, when intentionally requested, must happen after this target.
setup-magios-vosk:
	@bash scripts/setup-magios-vosk.sh

# Read-only companion used by compile/check lanes. It never downloads or
# rewrites an artifact and skips where the iOS compiler itself is unavailable.
verify-magios-vosk:
	@if ! command -v xcodebuild >/dev/null 2>&1; then \
		echo "⏭  xcodebuild not found — skipping Magios Vosk verification."; \
	else \
		bash scripts/setup-magios-vosk.sh --verify-only; \
	fi

generate-magican-app-icons:
	@python3 scripts/generate-magican-app-icons.py

generate-magican-control-symbol:
	@bash scripts/generate-magican-control-symbol.sh

build-desktop-tray: build-desktop-tray-debug

build-desktop-tray-debug: setup-desktop-vosk
	@$(MAKE) --no-print-directory setup-desktop-pnpm || exit $$?; \
		echo "🖥️  Building Tauri tray/gateway debug binary..."; \
		$(PNPM) --dir "$(DESKTOP_DIR)" install && $(PNPM) --dir "$(DESKTOP_DIR)" run build || exit $$?; \
		if [ "$$(uname -s)" = "Darwin" ]; then \
		CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" CARGO_INCREMENTAL=0 cargo build --manifest-path "$(DESKTOP_TAURI_MANIFEST)" --locked --features native-wake || exit $$?; \
		./scripts/replace-debug-bin.sh "$(DESKTOP_TRAY_BIN)" "$(DESKTOP_TRAY_REPO_BIN)" || exit $$?; \
		$(MAKE) --no-print-directory build-macos-speech-helper-debug || exit $$?; \
		bash scripts/materialize-desktop-debug-app.sh \
			"$(DESKTOP_TRAY_REPO_BIN)" \
			"$(DESKTOP_TRAY_DEBUG_APP)" \
			"desktop/src-tauri/icons/icon.icns" \
			"desktop/src-tauri/Info.plist" \
			"$$(node -p "require('./desktop/package.json').version")"; \
		else \
			CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" CARGO_INCREMENTAL=0 cargo build --manifest-path "$(DESKTOP_TAURI_MANIFEST)" --locked || exit $$?; \
	fi

run-desktop-tray: run-desktop-tray-debug

# Launches the DEBUG APP BUNDLE, never the raw repo-root binary. The raw binary
# is real-signed with the hardened runtime, and its @executable_path rpath
# resolves to nothing outside a bundle, so dyld fell back to the ad-hoc vendor
# libvosk and refused it ("different Team IDs"); run-all then carried on with a
# tray that had already died, with the loader's message only in the tray log.
# The bundle carries a copy signed with the same identity, and the preflight
# script proves that before anything is launched.
run-desktop-tray-debug:
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		$(MAKE) --no-print-directory setup-desktop-pnpm || exit $$?; \
		echo "🖥️  Starting Tauri desktop development app..."; \
		cd "$(DESKTOP_DIR)" && $(PNPM) install && $(PNPM) tauri dev; \
	elif [ ! -x "$(DESKTOP_TRAY_DEBUG_APP_BIN)" ]; then \
		echo "❌ Missing debug app bundle: $(DESKTOP_TRAY_DEBUG_APP)"; \
		echo "   Run 'make build-desktop-tray-debug' first."; \
		exit 1; \
	else \
		bash scripts/verify-desktop-debug-app.sh "$(DESKTOP_TRAY_DEBUG_APP)" || exit $$?; \
		echo "🖥️  Starting Tauri tray/gateway through LaunchServices..."; \
		echo "   - App bundle: $(DESKTOP_TRAY_DEBUG_APP)"; \
		if [ -x "$(DESKTOP_TRAY_DEBUG_APP)/Contents/MacOS/$(MACOS_SPEECH_HELPER_REPO_BIN)" ]; then \
			echo "   - Speech helper: $(DESKTOP_TRAY_DEBUG_APP)/Contents/MacOS/$(MACOS_SPEECH_HELPER_REPO_BIN)"; \
		else \
			echo "   - Speech helper: MISSING from the bundle — Speech will fail; run 'make build-desktop-tray-debug'"; \
		fi; \
		echo "   - Host gateway: $(HOST_GATEWAY_URL)"; \
		echo "   - Runtime stack management: $(DESKTOP_MANAGE_RUNTIME) (1=container, 0=external/native)"; \
		echo "   - Runtime log: $(HOST_TRAY_LOG_FILE)"; \
		open -n -g \
			--stdout "$(abspath $(HOST_TRAY_LOG_FILE))" \
			--stderr "$(abspath $(HOST_TRAY_LOG_FILE))" \
			--env "MAGICIAN_DESKTOP_MANAGE_RUNTIME=$(DESKTOP_MANAGE_RUNTIME)" \
			--env "MAGICIAN_ROOT_DIR=$(RUNTIME_ROOT_DIR)" \
			--env "MAGICIAN_HOST_GATEWAY_URL=$(HOST_GATEWAY_URL)" \
			$(if $(wildcard $(DESKTOP_VOSK_RUNTIME_DIR)/am/final.mdl),--env "MAGICIAN_VOSK_MODEL_DIR=$(abspath $(DESKTOP_VOSK_RUNTIME_DIR))",) \
			"$(abspath $(DESKTOP_TRAY_DEBUG_APP))"; \
		PID=""; \
		for _ in $$(seq 1 60); do \
			PID=$$(lsof -tiTCP:$(HOST_GATEWAY_PORT) -sTCP:LISTEN 2>/dev/null | head -1); \
			if [ -n "$$PID" ]; then break; fi; \
			sleep 1; \
		done; \
		if [ -n "$$PID" ]; then \
			echo "✅ Desktop tray is running (PID: $$PID)"; \
		else \
			echo "❌ Desktop tray did not become ready; last lines of $(HOST_TRAY_LOG_FILE):"; \
			tail -n 12 "$(HOST_TRAY_LOG_FILE)" 2>/dev/null | sed 's/^/   | /'; \
			exit 1; \
		fi; \
	fi

stop-desktop-tray:
	@echo "⏹️  Stopping desktop tray/gateway..."
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		launchctl remove "$(DESKTOP_TRAY_LAUNCHD_LABEL)" >/dev/null 2>&1 || true; \
		PIDS=$$( (pgrep -f "$(DESKTOP_TRAY_BIN)" 2>/dev/null; pgrep -f "$(DESKTOP_TRAY_REPO_BIN)" 2>/dev/null; lsof -tiTCP:$(HOST_GATEWAY_PORT) -sTCP:LISTEN 2>/dev/null) | sort -u); \
		if [ -n "$$PIDS" ]; then \
			echo "   killing $$(echo $$PIDS | wc -w | tr -d ' ') desktop tray process(es)..."; \
			kill $$PIDS 2>/dev/null || true; \
			sleep 1; \
			REMAINING=$$( (pgrep -f "$(DESKTOP_TRAY_BIN)" 2>/dev/null; pgrep -f "$(DESKTOP_TRAY_REPO_BIN)" 2>/dev/null; lsof -tiTCP:$(HOST_GATEWAY_PORT) -sTCP:LISTEN 2>/dev/null) | sort -u); \
			if [ -n "$$REMAINING" ]; then \
				echo "   sending SIGKILL to remaining desktop tray process(es)..."; \
				kill -9 $$REMAINING 2>/dev/null || true; \
				sleep 1; \
				STILL_RUNNING=$$(lsof -tiTCP:$(HOST_GATEWAY_PORT) -sTCP:LISTEN 2>/dev/null); \
				if [ -n "$$STILL_RUNNING" ]; then \
					echo "❌ Could not stop desktop tray host gateway (PID: $$STILL_RUNNING)."; \
					exit 1; \
				fi; \
			fi; \
		else \
			echo "   No Makefile-launched desktop tray found."; \
		fi; \
	else \
		echo "   Skipped outside macOS."; \
	fi

# Restart the already-staged local tray without owning the caller's terminal.
# Unlike Magician/Magicutor, the host tray is not a magic-supervisor child, so
# LaunchServices owns the replacement inside the user's GUI login session.
# nohup is insufficient here: nested make/bash/tee descendants remain in the
# invoking execution session and can be reaped when that shell exits. A proper
# .app launch also gives AppKit/Tauri the GUI activation context that a raw
# `launchctl submit` process lacks.
restart-desktop-tray: restart-desktop-tray-debug

restart-desktop-tray-debug: stop-desktop-tray
	@if [ ! -x "$(DESKTOP_TRAY_DEBUG_APP_BIN)" ]; then \
		echo "❌ Missing debug app bundle: $(DESKTOP_TRAY_DEBUG_APP)"; \
		echo "   Run 'make build-desktop-tray-debug' first."; \
		exit 1; \
	fi
	@echo "🔄 Restarting desktop tray/gateway through LaunchServices..."
	@open -n -g \
		--stdout "$(abspath $(HOST_TRAY_LOG_FILE))" \
		--stderr "$(abspath $(HOST_TRAY_LOG_FILE))" \
		--env "MAGICIAN_DESKTOP_MANAGE_RUNTIME=0" \
		--env "MAGICIAN_ROOT_DIR=$(RUNTIME_ROOT_DIR)" \
		--env "MAGICIAN_HOST_GATEWAY_URL=$(HOST_GATEWAY_URL)" \
		"$(abspath $(DESKTOP_TRAY_DEBUG_APP))"
	@PID=""; \
		for _ in $$(seq 1 60); do \
			PID=$$(lsof -tiTCP:$(HOST_GATEWAY_PORT) -sTCP:LISTEN 2>/dev/null | head -1); \
			if [ -n "$$PID" ]; then break; fi; \
			sleep 1; \
		done; \
		if [ -n "$$PID" ]; then \
			echo "✅ Desktop tray is running (PID: $$PID)"; \
			echo "   Runtime log: $(HOST_TRAY_LOG_FILE)"; \
		else \
			echo "❌ Desktop tray did not become ready."; \
			echo "   app bundle: $(DESKTOP_TRAY_DEBUG_APP)"; \
			echo "   Runtime log: $(HOST_TRAY_LOG_FILE)"; \
			exit 1; \
		fi

# The desktop crate is its own workspace, so `cargo check --workspace` at the
# root never sees it. That gap is not theoretical: a plain compile error and a
# same-origin trust bug both shipped in it while every root gate stayed green.
#
# `--features native-wake` matches what `test-desktop-tray` compiles, so the 16
# feature-gated sites are covered rather than silently skipped. It needs no
# `setup-desktop-vosk`: the crate's own manifest notes that `cargo check` and
# tests do not require `libvosk` — only a wake-enabled *build* links it — so
# this adds no model download to `make check-all`.
.PHONY: check-desktop
check-desktop:
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		echo "🔍 Checking desktop tray (all targets, native-wake)..."; \
		CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" CARGO_INCREMENTAL=0 cargo check --manifest-path "$(DESKTOP_TAURI_MANIFEST)" --locked --all-targets --features native-wake; \
	else \
		echo "🔍 Checking desktop tray (all targets)..."; \
		CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" CARGO_INCREMENTAL=0 cargo check --manifest-path "$(DESKTOP_TAURI_MANIFEST)" --locked --all-targets; \
	fi

.PHONY: check-desktop-target
check-desktop-target:
	@if [ -z "$(TARGET)" ]; then \
		echo "Usage: make check-desktop-target TARGET=<rust-target>"; \
		exit 1; \
	fi
	@echo "🔍 Checking desktop tray for $(TARGET)..."
	@CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" CARGO_INCREMENTAL=0 cargo check \
		--manifest-path "$(DESKTOP_TAURI_MANIFEST)" --locked --all-targets --target "$(TARGET)"

test-desktop-tray: setup-desktop-vosk
	@$(MAKE) --no-print-directory setup-desktop-pnpm || exit $$?; \
		echo "🧪 Testing Tauri tray/gateway..."; \
		CI=true $(PNPM) --dir "$(DESKTOP_DIR)" install && $(PNPM) --dir "$(DESKTOP_DIR)" run check && $(PNPM) --dir "$(DESKTOP_DIR)" run test || exit $$?; \
		if [ "$$(uname -s)" = "Darwin" ]; then \
			CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" CARGO_INCREMENTAL=0 cargo test --manifest-path "$(DESKTOP_TAURI_MANIFEST)" --locked --features native-wake; \
		else \
			CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" CARGO_INCREMENTAL=0 cargo test --manifest-path "$(DESKTOP_TAURI_MANIFEST)" --locked; \
	fi

.PHONY: test-desktop-container-routing test-desktop-connect-route
test-desktop-connect-route:
	@echo "🧪 Testing desktop remote-device route selection..."
	@CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" CARGO_INCREMENTAL=0 cargo test \
		--manifest-path "$(DESKTOP_TAURI_MANIFEST)" --locked connect_route::tests::

.PHONY: test-desktop-auth
test-desktop-auth:
	CARGO_INCREMENTAL=0 cargo test --manifest-path "$(DESKTOP_TAURI_MANIFEST)" \
		--features native-wake magician_auth:: -- --test-threads=1
	node --test desktop/src/lib/magicianAuthPolicy.test.js
	cd ui/unified-ui && ./node_modules/.bin/vitest run --project unit --maxWorkers 1 \
		src/lib/stores/scopeIdentityStore.auth.test.ts src/lib/stores/scopeIdentityStore.native.test.ts
	cd ui/unified-ui && ./node_modules/.bin/vitest run --project component --maxWorkers 1 \
		src/lib/media/voice/realtimeVoiceClient.component.test.ts src/lib/stores/chatTurnEventsStore.test.ts

DESKTOP_CONTAINER_TEST_ARGS ?=
test-desktop-container-routing:
	CARGO_INCREMENTAL=0 cargo test --manifest-path "$(DESKTOP_TAURI_MANIFEST)" \
		--features native-wake container_routing_ -- $(DESKTOP_CONTAINER_TEST_ARGS) --test-threads=1

.PHONY: test-desktop-managed-container
test-desktop-managed-container:
	@test "$$(uname -s)" = Darwin || { echo "This acceptance lane requires Apple Container on macOS"; exit 1; }
	@test -n "$(MANAGED_CONTAINER_TEST_ROOT)" -a -n "$(MANAGED_CONTAINER_TEST_IMAGE)" || \
		{ echo "Set MANAGED_CONTAINER_TEST_ROOT to a new absolute path and MANAGED_CONTAINER_TEST_IMAGE to a local image"; exit 1; }
	MAGICIAN_MANAGED_CONTAINER_TEST_ROOT="$(MANAGED_CONTAINER_TEST_ROOT)" \
	MAGICIAN_MANAGED_CONTAINER_TEST_IMAGE="$(MANAGED_CONTAINER_TEST_IMAGE)" \
	MAGICIAN_CONTAINER_KEYRING_HOME="$(MANAGED_CONTAINER_TEST_ROOT)/custody" \
	CARGO_INCREMENTAL=0 cargo test --manifest-path "$(DESKTOP_TAURI_MANIFEST)" \
		--features native-wake managed_container_preserves_device_credentials -- --ignored --nocapture --test-threads=1

.PHONY: test-runtime-service-endpoints
test-runtime-service-endpoints: check-protoc
	cargo test -p magician --lib config::tests:: -- --test-threads=1

.PHONY: test-container-host-relay
test-container-host-relay:
	python3 scripts/test-container-host-relay.py

# --- Other skill setup convenience shims (delegate to skillshub/) ---
# These let `make setup-bots`, `make setup-agent-browser`, etc. continue
# to work from the repo root without having to type `make -C skillshub …`.

setup-bots:
	@$(MAKE) -C skillshub setup-bots

setup-agent-browser:
	@$(MAKE) -C skillshub setup-agent-browser

agent-browser-source:
	@$(MAKE) -C skillshub agent-browser-source

rebuild-agent-browser:
	@$(MAKE) -C skillshub rebuild-agent-browser

verify-agent-browser:
	@$(MAKE) -C skillshub verify-agent-browser

setup-printing-press:
	@$(MAKE) -C skillshub setup-printing-press

setup-metabase-cli:
	@$(MAKE) -C skillshub setup-metabase-cli

setup-kapso-cli:
	@$(MAKE) -C skillshub setup-kapso-cli

setup-higgsfield-cli:
	@$(MAKE) -C skillshub setup-higgsfield-cli

setup-officecli:
	@$(MAKE) -C skillshub setup-officecli

verify-officecli:
	@$(MAKE) -C skillshub verify-officecli

regen-metabase-cli:
	@$(MAKE) -C skillshub regen-metabase-cli

refresh-metabase-spec:
	@$(MAKE) -C skillshub refresh-metabase-spec

setup-capability-tools:
	@$(MAKE) -C skillshub setup-marimo
	@$(MAKE) -C skillshub setup-metabase-cli

# Backward-compat shims: skill setup targets that were renamed when moved
# into skillshub/Makefile. The new names (setup-deps, setup-skill-bins,
# setup-system-bins, setup-python, setup-env) live under skillshub/.

setup-skillshub-deps:
	@$(MAKE) -C skillshub setup-deps

setup-skill-bins:
	@$(MAKE) -C skillshub setup-skill-bins

setup-skill-system-bins:
	@$(MAKE) -C skillshub setup-system-bins

setup-skillshub-python:
	@$(MAKE) -C skillshub setup-python

setup-skill-env:
	@$(MAKE) -C skillshub setup-env

setup-gws-accounts:
	@$(MAKE) -C skillshub setup-gws-accounts SCOPE=$(SCOPE)

memory-index-status:
	@if [ ! -x "./$(MAGICIAN_BIN)" ]; then \
		echo "Missing executable: ./$(MAGICIAN_BIN)"; \
		echo "Run 'make build-all-release' first."; \
		exit 1; \
	fi
	@./$(MAGICIAN_BIN) memory-index status --principal "$(MEMORY_INDEX_PRINCIPAL)" --workspace "$(MEMORY_INDEX_WORKSPACE)"

memory-index-prewarm:
	@if [ ! -x "./$(MAGICIAN_BIN)" ]; then \
		echo "Missing executable: ./$(MAGICIAN_BIN)"; \
		echo "Run 'make build-all-release' first."; \
		exit 1; \
	fi
	@./$(MAGICIAN_BIN) memory-index rebuild --principal "$(MEMORY_INDEX_PRINCIPAL)" --workspace "$(MEMORY_INDEX_WORKSPACE)" --skip-if-fresh $(MEMORY_INDEX_REBUILD_FLAGS)

memory-index-rebuild:
	@if [ ! -x "./$(MAGICIAN_BIN)" ]; then \
		echo "Missing executable: ./$(MAGICIAN_BIN)"; \
		echo "Run 'make build-all-release' first."; \
		exit 1; \
	fi
	@./$(MAGICIAN_BIN) memory-index rebuild --principal "$(MEMORY_INDEX_PRINCIPAL)" --workspace "$(MEMORY_INDEX_WORKSPACE)"

memory-index-optimize:
	@if [ ! -x "./$(MAGICIAN_BIN)" ]; then \
		echo "Missing executable: ./$(MAGICIAN_BIN)"; \
		echo "Run 'make build-all-release' first."; \
		exit 1; \
	fi
	@./$(MAGICIAN_BIN) memory-index optimize --principal "$(MEMORY_INDEX_PRINCIPAL)" --workspace "$(MEMORY_INDEX_WORKSPACE)"

# Analytics DuckDB cross-version migration (see
# docs/runbooks/2026-06-13-analytics-duckdb-version-migration.md). No secrets
# needed, so these run with MAGICIAN_SKIP_KEYCHAIN=1 (headless-safe). EXPORT must
# run with a binary whose bundled DuckDB can read the source file (the OLD pin
# for a legacy DB); IMPORT with the current pin.
analytics-export:
	@if [ ! -x "./$(MAGICIAN_BIN)" ]; then \
		echo "Missing executable: ./$(MAGICIAN_BIN)"; \
		echo "Run 'make build-magician-debug' first."; \
		exit 1; \
	fi
	@if [ -z "$(DB)" ] || [ -z "$(OUT)" ]; then \
		echo "usage: make analytics-export DB=<analytics.duckdb> OUT=<export_dir>"; \
		exit 2; \
	fi
	@MAGICIAN_SKIP_KEYCHAIN=1 ./$(MAGICIAN_BIN) analytics export --db "$(DB)" --out "$(OUT)"

analytics-import:
	@if [ ! -x "./$(MAGICIAN_BIN)" ]; then \
		echo "Missing executable: ./$(MAGICIAN_BIN)"; \
		echo "Run 'make build-magician-debug' first."; \
		exit 1; \
	fi
	@if [ -z "$(IN)" ] || [ -z "$(DB)" ]; then \
		echo "usage: make analytics-import IN=<export_dir> DB=<analytics.duckdb>"; \
		exit 2; \
	fi
	@MAGICIAN_SKIP_KEYCHAIN=1 ./$(MAGICIAN_BIN) analytics import --in "$(IN)" --db "$(DB)"

# Corrective reprice of historical llm_calls Parquet partitions against the
# ACTIVE effective-dated pricing table (llm_pricing.json layered over the
# built-in base). Dry-run by default — pass ARGS="--apply" to rewrite, plus
# optional --principal/--workspace/--from/--to/--include-today. No secrets
# needed, so it runs with MAGICIAN_SKIP_KEYCHAIN=1 (headless-safe).
analytics-reprice-llm:
	@if [ ! -x "./$(MAGICIAN_BIN)" ]; then \
		echo "Missing executable: ./$(MAGICIAN_BIN)"; \
		echo "Run 'make build-magician-debug' first."; \
		exit 1; \
	fi
	@MAGICIAN_SKIP_KEYCHAIN=1 ./$(MAGICIAN_BIN) analytics reprice-llm-calls $(ARGS)

build-macos-audio-engine: build-macos-audio-engine-debug

build-macos-audio-engine-debug:
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		echo "⚠️  Skipping FluidAudio sidecar build outside macOS."; \
	elif ! command -v swift >/dev/null 2>&1; then \
		echo "⚠️  Skipping FluidAudio sidecar build because swift is unavailable."; \
	else \
		mkdir -p "$(MACOS_AUDIO_ENGINE_BUILD_DIR)" "$(SWIFT_CACHE_DIR)" "$(SWIFT_CONFIG_DIR)" "$(SWIFT_SECURITY_DIR)" "$(SWIFT_CLANG_MODULE_CACHE_DIR)"; \
		CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift build $(SWIFT_AUDIO_ENGINE_FLAGS); \
		bin_path=$$(CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift build $(SWIFT_AUDIO_ENGINE_FLAGS) --show-bin-path); \
		./scripts/replace-debug-bin.sh "$$bin_path/magician-macos-audio-engine" "$(MACOS_AUDIO_ENGINE_REPO_BIN)"; \
	fi

build-macos-audio-engine-release:
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		echo "⚠️  Skipping FluidAudio sidecar release build outside macOS."; \
	elif ! command -v swift >/dev/null 2>&1; then \
		echo "⚠️  Skipping FluidAudio sidecar release build because swift is unavailable."; \
	else \
		mkdir -p "$(MACOS_AUDIO_ENGINE_BUILD_DIR)" "$(SWIFT_CACHE_DIR)" "$(SWIFT_CONFIG_DIR)" "$(SWIFT_SECURITY_DIR)" "$(SWIFT_CLANG_MODULE_CACHE_DIR)"; \
		CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift build -c release $(SWIFT_AUDIO_ENGINE_FLAGS) $(SWIFT_AUDIO_ENGINE_TARGET_FLAGS); \
		bin_path=$$(CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift build -c release $(SWIFT_AUDIO_ENGINE_FLAGS) $(SWIFT_AUDIO_ENGINE_TARGET_FLAGS) --show-bin-path); \
		./scripts/replace-release-bin.sh "$$bin_path/magician-macos-audio-engine" "$(MACOS_AUDIO_ENGINE_REPO_BIN)"; \
	fi

test-macos-audio-engine:
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		echo "⚠️  Skipping FluidAudio sidecar tests outside macOS."; \
	elif ! command -v swift >/dev/null 2>&1; then \
		echo "⚠️  Skipping FluidAudio sidecar tests because swift is unavailable."; \
	else \
		mkdir -p "$(MACOS_AUDIO_ENGINE_BUILD_DIR)" "$(SWIFT_CACHE_DIR)" "$(SWIFT_CONFIG_DIR)" "$(SWIFT_SECURITY_DIR)" "$(SWIFT_CLANG_MODULE_CACHE_DIR)"; \
		CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift test $(SWIFT_AUDIO_ENGINE_FLAGS); \
	fi

# The Apple Speech helper (desktop Speech permission, /host/speech/transcribe,
# macos_tts) and the meet-audio tap (meeting bridge, macos_speech_stt). Both
# came out of the retired presence-host package; the tray launch targets pass
# the staged .bin by absolute path, so a missing stage is a dead Speech button.
build-macos-speech-helper: build-macos-speech-helper-debug

build-macos-speech-helper-debug:
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		echo "⚠️  Skipping macOS speech helper build outside macOS."; \
	elif ! command -v swift >/dev/null 2>&1; then \
		echo "⚠️  Skipping macOS speech helper build because swift is unavailable."; \
	else \
		mkdir -p "$(MACOS_SPEECH_HELPER_BUILD_DIR)" "$(SWIFT_CACHE_DIR)" "$(SWIFT_CONFIG_DIR)" "$(SWIFT_SECURITY_DIR)" "$(SWIFT_CLANG_MODULE_CACHE_DIR)"; \
		CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift build $(SWIFT_SPEECH_HELPER_FLAGS); \
		bin_path=$$(CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift build $(SWIFT_SPEECH_HELPER_FLAGS) --show-bin-path); \
		./scripts/replace-debug-bin.sh "$$bin_path/magician-macos-speech-helper" "$(MACOS_SPEECH_HELPER_REPO_BIN)"; \
		./scripts/replace-debug-bin.sh "$$bin_path/magician-macos-meet-audio" "$(MACOS_MEET_AUDIO_HELPER_REPO_BIN)"; \
	fi

build-macos-speech-helper-release:
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		echo "⚠️  Skipping macOS speech helper release build outside macOS."; \
	elif ! command -v swift >/dev/null 2>&1; then \
		echo "⚠️  Skipping macOS speech helper release build because swift is unavailable."; \
	else \
		mkdir -p "$(MACOS_SPEECH_HELPER_BUILD_DIR)" "$(SWIFT_CACHE_DIR)" "$(SWIFT_CONFIG_DIR)" "$(SWIFT_SECURITY_DIR)" "$(SWIFT_CLANG_MODULE_CACHE_DIR)"; \
		CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift build -c release $(SWIFT_SPEECH_HELPER_FLAGS); \
		bin_path=$$(CLANG_MODULE_CACHE_PATH="$(SWIFT_CLANG_MODULE_CACHE_DIR)" swift build -c release $(SWIFT_SPEECH_HELPER_FLAGS) --show-bin-path); \
		./scripts/replace-release-bin.sh "$$bin_path/magician-macos-speech-helper" "$(MACOS_SPEECH_HELPER_REPO_BIN)"; \
		./scripts/replace-release-bin.sh "$$bin_path/magician-macos-meet-audio" "$(MACOS_MEET_AUDIO_HELPER_REPO_BIN)"; \
	fi

MEDIA_STT_BENCHMARK_ARGS ?=
generate-media-local-speech-fixtures:
	@./scripts/generate-media-local-speech-fixtures.sh

benchmark-media-recording-stt:
	@./scripts/generate-media-audio-fixtures.sh
	@python3 scripts/benchmark-media-recording-stt.py $(MEDIA_STT_BENCHMARK_ARGS)

MEDIA_OFFLINE_AUDIO_EVAL_ARGS ?=
REALTIME_LOCAL_TRANSCRIPT_EVAL_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/realtime-local-transcript/latest
REALTIME_LOCAL_TRANSCRIPT_SESSION_EVIDENCE ?=
benchmark-media-offline-audio: setup-test-suite-runner-deps generate-media-local-speech-fixtures
	@./scripts/generate-media-audio-fixtures.sh
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/media_offline_audio_eval.py $(MEDIA_OFFLINE_AUDIO_EVAL_ARGS)

benchmark-media-vad: setup-test-suite-runner-deps generate-media-local-speech-fixtures
	@./scripts/generate-media-audio-fixtures.sh
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/media_offline_audio_eval.py --stages vad $(MEDIA_OFFLINE_AUDIO_EVAL_ARGS)

benchmark-media-stt: setup-test-suite-runner-deps generate-media-local-speech-fixtures
	@./scripts/generate-media-audio-fixtures.sh
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/media_offline_audio_eval.py --stages stt $(MEDIA_OFFLINE_AUDIO_EVAL_ARGS)

benchmark-media-tts: setup-test-suite-runner-deps
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/media_offline_audio_eval.py --stages tts $(MEDIA_OFFLINE_AUDIO_EVAL_ARGS)

benchmark-media-diarization: setup-test-suite-runner-deps generate-media-local-speech-fixtures
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/media_offline_audio_eval.py --stages diarization $(MEDIA_OFFLINE_AUDIO_EVAL_ARGS)

GEMINI_LIVE_MODELS_EVAL_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/gemini-live-models/latest
## eval: kind=harness
## desc: Run provider-free metric, selection, and schema regressions
test-media-offline-audio-eval: setup-test-suite-runner-deps
	@"$(TEST_SUITE_RUNNER_PYTHON)" -m unittest scripts.test_media_offline_audio_eval scripts.test_eval_realtime_local_transcript_live
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/media_offline_audio_eval.py --self-test
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/eval_realtime_local_transcript_live.py --self-test

## eval: kind=harness report=evals/attention-historical-bootstrap
## desc: Run frozen provider-free historical attention migration and within-lane replay
test-attention-historical-bootstrap-eval:
	@cargo test -p magician --lib production_worker_routes_late_scopes_only_through_live_repairs
	@cargo test -p magician --lib obsolete_supplied_embedding_contract_is_queued_for_current_contract_repair
	@cargo test -p magician --lib outcome_and_missing_embedding_repair_are_committed_together
	@cargo test -p magician --lib embedding_bind_queue_is_scope_fair_and_classifies_retryable_failures
	@cargo test -p magician --lib legacy_embedding_bind_rows_migrate_and_poison_surfaces_are_quarantined
	@cargo test -p magician --lib live_feedback_outbox_is_atomic_and_event_idempotent
	@cargo test -p magician --lib action_rejects_malformed_event_ids_before_source_feedback_commits
	@cargo test -p magician --lib malformed_feedback_repair_rows_remain_cursor_visible
	@python3 scripts/eval_attention_historical_bootstrap.py \
		--fixtures data/magician_v2/attention_learning/historical-bootstrap-frozen-v1.json \
		--report '$(ATTENTION_HISTORICAL_BOOTSTRAP_EVAL_REPORT)'

test-realtime-local-transcript:
	@cargo test -p magicllm --lib transcription -- --nocapture
	@cargo test -p magician --lib local_realtime_transcript -- --nocapture
	@cargo test -p magician --lib streaming_fallback -- --nocapture
	@cargo test -p magician --lib local_transcript_ -- --nocapture
	@cargo test -p magician --lib finish_waits_for -- --nocapture
	@cargo test -p magician --lib shipped_backend_realtime_profiles_request_local_user_transcription -- --nocapture
	@cargo test --manifest-path desktop/src-tauri/Cargo.toml live_ptt_recoverable_errors_do_not_terminate_the_session -- --nocapture
	@npm --prefix ui/unified-ui test -- realtimeVoiceClient.component.test.ts
	@if command -v xcodebuild >/dev/null 2>&1; then \
		xcodebuild -project magios/Magios.xcodeproj -scheme Magios -configuration Debug \
			-destination '$(IOS_TEST_DESTINATION)' \
			-derivedDataPath '$(IOS_REALTIME_LOCAL_TRANSCRIPT_DERIVED_DATA)' \
			-only-testing:MagiosTests/RealtimeVoiceProtocolTests -quiet test; \
	else \
		echo "Skipping focused iOS protocol tests because xcodebuild is unavailable."; \
	fi
	@python3 -m unittest scripts.test_eval_realtime_local_transcript_live
	@python3 scripts/eval_realtime_local_transcript_live.py --self-test

## desc: Provider-free Gemini Live regressions: model contract, setup/tool payloads, interaction status, thinking-token usage, pricing rows, picker order
test-gemini-live-models:
	@cargo test -p magicllm --lib realtime::gemini -- --nocapture
	@cargo test -p magicllm --lib realtime::factory -- --nocapture
	@cargo test -p magicllm --lib realtime::types -- --nocapture
	@cargo test -p magicllm --lib gemini_live -- --nocapture
	@cargo test -p magicllm --lib pricing::tests -- --nocapture
	@cargo test -p magician --lib realtime_voice_profiles_follow_display_order_then_id -- --nocapture
	@cargo test -p magician --lib shipped_configs_offer_gemini_3_8_live_and_list_3_1_last -- --nocapture

## eval: kind=live requires=provider_keys report=evals/gemini-live-models
## desc: Open every shipped Gemini Live model through the production provider; each must accept setup, call a tool, speak the tool's answer, report interaction status (3.8) or not (3.1), and price above $0
test-gemini-live-models-live:
	@mkdir -p '$(GEMINI_LIVE_MODELS_EVAL_OUTPUT_DIR)'
	@rm -f '$(GEMINI_LIVE_MODELS_EVAL_OUTPUT_DIR)/report.jsonl'
	@GEMINI_LIVE_MODELS_EVAL_REPORT='$(GEMINI_LIVE_MODELS_EVAL_OUTPUT_DIR)/report.jsonl' \
		cargo test -p magicllm --test live_gemini_live_models -- --ignored --nocapture live_gemini_live_models_summary
	@echo "report: $(GEMINI_LIVE_MODELS_EVAL_OUTPUT_DIR)/report.jsonl"

## eval: kind=live report=evals/realtime-local-transcript
## desc: Gate local streaming STT quality plus audited live-session recovery and cost
test-realtime-local-transcript-live:
	@if [ -z '$(REALTIME_LOCAL_TRANSCRIPT_SESSION_EVIDENCE)' ]; then \
		echo "REALTIME_LOCAL_TRANSCRIPT_SESSION_EVIDENCE must point to one audited call JSON file." >&2; \
		exit 2; \
	fi
	@python3 scripts/eval_realtime_local_transcript_live.py \
		--session-evidence '$(REALTIME_LOCAL_TRANSCRIPT_SESSION_EVIDENCE)' \
		--output-dir '$(REALTIME_LOCAL_TRANSCRIPT_EVAL_OUTPUT_DIR)' \
		$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',)

## eval: kind=harness
## desc: Run provider-free authorization eval/report regressions
test-agent-tool-visibility-eval-harness: setup-test-suite-runner-deps
	@"$(TEST_SUITE_RUNNER_PYTHON)" -m unittest scripts.test_agent_tool_visibility_authorization_eval scripts.test_test_suite_summary_report
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/eval-agent-tool-visibility-authorization-live.py --self-test

## eval: kind=harness report=evals/agent-surface-runtime
## desc: Run shared cache/family parity eval + HTML report
test-agent-surface-runtime-eval:
	@python3 -m unittest scripts.test_agent_surface_runtime_cache_eval
	@python3 scripts/eval-agent-surface-runtime-cache.py --self-test
	@AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR='$(AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR)' \
		python3 scripts/eval-agent-surface-runtime-cache.py

## eval: kind=harness report=evals/capability-recall
## desc: Provider-free tool_search + find_agents recall over all packs/agents
test-capability-recall-eval:
	@python3 -m unittest scripts.test_capability_recall_eval
	@python3 scripts/eval-capability-recall.py --self-test
	@CAPABILITY_RECALL_EVAL_REPORT_DIR='$(CAPABILITY_RECALL_EVAL_REPORT_DIR)' \
		CARGO_TARGET_DIR='$(CARGO_TARGET_DIR)' TMPDIR='$(or $(TMPDIR),/Volumes/build/magician/tmp)' \
		python3 scripts/eval-capability-recall.py

## eval: kind=harness report=evals/task-verdict
## desc: Replay real task states and assert no verdict misleads about what happened
# Provider-free: pure frontend modules over a captured corpus of task shapes, so
# it costs nothing and needs no service. The report is vitest's own JSON result,
# written as `report.json` because that is one of the names `/v2/evals` serves as
# a report index — and written on failure as well as on success, which is the
# run the link is actually wanted for.
test-task-verdict-eval:
	@mkdir -p '$(TASK_VERDICT_EVAL_REPORT_DIR)'
	@cd ui/unified-ui && UI_TEST_RESULTS_PATH='$(abspath $(TASK_VERDICT_EVAL_REPORT_DIR))/report.json' \
		npx vitest run --config vitest.report.config.ts \
		src/lib/magician/tasks/taskVerdict.corpus.eval.test.ts

## eval: kind=harness
## desc: Run provider-free static-reader quality and contract evaluation
test-content-reader-eval:
	@python3 -m unittest scripts.test_content_reader_extraction_eval
	@cargo run --quiet -p magician --example content_reader_quality_eval

## eval: kind=harness
## desc: Self-test the content-retrieval evaluators and run the fixture browser eval
test-content-retrieval-eval-harness:
	@python3 -m unittest discover -s skillshub/arxiv-search/tests -p 'test_*.py'
	@python3 -m unittest discover -s skillshub/structured-web-data/tests -p 'test_*.py'
	@python3 -m unittest discover -s skillshub/youtube-search/tests -p 'test_*.py'
	@python3 -m unittest discover -s skillshub/media-fetch/tests -p 'test_*.py'
	@python3 -m unittest scripts.test_content_browser_fixture_eval
	@python3 scripts/eval-content-browser-fixtures.py
	@$(MAKE) --no-print-directory test-observable-sources-eval-harness
	@$(MAKE) --no-print-directory test-content-retrieval-runtime-eval-harness

## eval: kind=harness
## desc: Run hermetic Phase 7/8 ladder, browser, handler, manifest, and extraction gates
test-content-retrieval-eval: test-content-retrieval-eval-harness
	@cargo test -p magician --lib magician_v2::content_sources -- --nocapture
	@cargo test -p magician --lib magician_v2::execution::primitive_dispatch::browser -- --nocapture
	@cargo test -p magician --lib typed_retrieval_handoff_requires_exact_mode_and_magicutor_proxy -- --nocapture
	@cargo test -p magician --lib browser_engine_override_precedes_config_and_config_precedes_bundled_default -- --nocapture
	@cargo test -p magician --lib magician_v2::execution::compiled_handlers::content_search -- --nocapture
	@cargo test -p magician --lib magician_v2::execution::compiled_handlers::content_read -- --nocapture
	@cargo test -p magician --lib magician_v2::execution::compiled_handlers::web_fetch -- --nocapture
	@cargo test -p magician --lib magician_v2::execution::compiled_providers::tests::embedded_compiled_pack_defs_parses_all_embedded -- --nocapture

CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY ?= AI
CONTENT_RETRIEVAL_RUNTIME_LIVE_DISCOVERY_ACTION ?= hacker_news.discover
CONTENT_RETRIEVAL_RUNTIME_LIVE_URL ?= https://news.ycombinator.com/
CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS ?= 25
CONTENT_RETRIEVAL_RUNTIME_LIVE_CONFIG ?= $(LIVE_EVAL_CONFIG)
CONTENT_RETRIEVAL_RUNTIME_LIVE_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/content-retrieval-runtime-live/latest.json
CONTENT_RETRIEVAL_RUNTIME_LIVE_EVAL_ARGS ?=

## eval: kind=harness
## desc: Self-test the production-runtime content-retrieval evaluator
test-content-retrieval-runtime-eval-harness:
	@cargo run --quiet -p magician --example content_retrieval_runtime_live_eval -- --self-test

## eval: kind=live requires=magicutor report=evals/content-retrieval-runtime-live
## desc: Run production resolver/compiled-handler retrieval checks
test-content-retrieval-runtime-live-eval:
	@cargo run --quiet -p magician --example content_retrieval_runtime_live_eval -- \
		--repo-root '$(CURDIR)' \
		--query '$(CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY)' \
		--discovery-action '$(CONTENT_RETRIEVAL_RUNTIME_LIVE_DISCOVERY_ACTION)' \
		--public-url '$(CONTENT_RETRIEVAL_RUNTIME_LIVE_URL)' \
		--timeout-secs '$(CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS)' \
		--output '$(CONTENT_RETRIEVAL_RUNTIME_LIVE_OUTPUT)' \
		$(if $(CONTENT_RETRIEVAL_RUNTIME_LIVE_CONFIG),--config '$(CONTENT_RETRIEVAL_RUNTIME_LIVE_CONFIG)',) \
		$(CONTENT_RETRIEVAL_RUNTIME_LIVE_EVAL_ARGS)

WORKING_SET_EVAL_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/working-set/deterministic/latest
WORKING_SET_LIVE_EVAL_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/working-set/live/latest
WORKING_SET_LIVE_EVAL_REPEAT ?= 1
WORKING_SET_LIVE_EVAL_ARGS ?=

## eval: kind=harness report=evals/working-set/deterministic/latest
## desc: Boundary B provider-free: planted-fact grounding, attribution and bytes for the working-set lane vs context packing, plus the live evaluator's report contract
test-working-set-eval-harness:
	@cargo test --quiet -p magician --lib -- artifact_v2::working_set_eval
	@cargo run --quiet -p magician-bin --example working_set_live_eval -- --self-test \
		--output-dir '$(WORKING_SET_EVAL_OUTPUT)'

## eval: kind=live requires=provider_keys report=evals/working-set/live/latest
## desc: Boundary B's live half: a model answers every planted probe from each lane's shipped bytes; gates on correctness beyond and within the pack budget, judged quality, and cost — the evidence `working_sets.activation.enabled_lanes` waits for
test-working-set-live-eval:
	@cargo run --quiet -p magician-bin --example working_set_live_eval -- \
		--repeat '$(WORKING_SET_LIVE_EVAL_REPEAT)' \
		--output-dir '$(WORKING_SET_LIVE_EVAL_OUTPUT)' \
		$(WORKING_SET_LIVE_EVAL_ARGS)

WORKING_SET_ROUTING_EVAL_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/working-set-routing/deterministic/latest
WORKING_SET_ROUTING_LIVE_EVAL_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/working-set-routing/live/latest
WORKING_SET_ROUTING_RUNS ?= 1
WORKING_SET_ROUTING_CASE ?= direct_openai_sarvam_pricing
WORKING_SET_ROUTING_LIVE_EVAL_ARGS ?=

## eval: kind=harness report=evals/working-set-routing/deterministic/latest
## desc: Boundary B's router, provider-free: the execution index and execution-scoped search, lane configuration, the decision recorded at the read seam and the activated projection, the deterministic suite driven through that seam as sequential reads, and the live A/B's report contract
test-working-set-routing-eval-harness:
	@cargo test --quiet -p magician --lib -- artifact_v2::working_sets artifact_v2::working_set_eval \
		content_sources::retrieval::tests::an_agent execution::compiled_handlers::working_sets
	@python3 scripts/eval-working-set-routing-live.py --self-test \
		--output-dir '$(WORKING_SET_ROUTING_EVAL_OUTPUT)'

## eval: kind=live requires=magician,provider_keys report=evals/working-set-routing/live/latest
## desc: Boundary B's router on the real web-researcher: the child case lane-off then lane-on by live-config swap (restored in finally); the child's verdict must not regress, every lane-on run must be decided under the open lane with its sentence recorded (activated only when its scale qualifies), every run that narrowed a read must use the execution-scoped search, and input tokens and cost must not rise beyond a 10 % noise band (the benefit on large pages is test-working-set-live-eval's to prove)
test-working-set-routing-live-eval:
	@python3 scripts/eval-working-set-routing-live.py \
		--case '$(WORKING_SET_ROUTING_CASE)' \
		--runs '$(WORKING_SET_ROUTING_RUNS)' \
		--output-dir '$(WORKING_SET_ROUTING_LIVE_EVAL_OUTPUT)' \
		$(WORKING_SET_ROUTING_LIVE_EVAL_ARGS)

WORKING_SET_ROUTING_AS_CONFIGURED_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/working-set-routing/as-configured/latest

## eval: kind=live requires=magician,provider_keys report=evals/working-set-routing/as-configured/latest
## desc: The check after an operator opens or closes a working-set lane: no config writes; the web-researcher case runs against the live file as found and every run must be routed into the lane when the file opens it, and none when it does not
test-working-set-routing-as-configured:
	@python3 scripts/eval-working-set-routing-live.py --as-configured \
		--case '$(WORKING_SET_ROUTING_CASE)' \
		--runs '$(WORKING_SET_ROUTING_RUNS)' \
		--output-dir '$(WORKING_SET_ROUTING_AS_CONFIGURED_OUTPUT)' \
		$(WORKING_SET_ROUTING_LIVE_EVAL_ARGS)

TASK_RECIPES_EVAL_ARGS ?=
TASK_RECIPES_EVAL_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/task-recipes/offline

## eval: kind=harness report=evals/task-recipes/offline
## desc: Compile and replay the redacted Task Recipes corpus with exact answer, privacy, relevance, matcher, and safety gates
test-task-recipes-eval:
	@cargo run --quiet -p magician --example task_recipes_eval --features test-fixtures -- \
		--fixtures '$(CURDIR)/magician/tests/fixtures/task_recipes' \
		--output '$(TASK_RECIPES_EVAL_OUTPUT)' $(TASK_RECIPES_EVAL_ARGS)

test-task-recipes-safety:
	@cargo test -p magician --test task_recipes_safety_contract --features test-fixtures -- --nocapture
	@cargo test -p magician-api direct_recipe_replay_returns_conflict_before_an_ungranted_write -- --nocapture
	@cargo test -p magician-api api_mining_off_keeps_reads_available_and_blocks_direct_recipe_replay -- --nocapture

TASK_RECIPES_LIVE_BASE_URL ?= http://127.0.0.1:3002
TASK_RECIPES_LIVE_MAGICUTOR_BASE_URL ?= http://127.0.0.1:3003
TASK_RECIPES_LIVE_TIMEOUT_SECS ?= 900
TASK_RECIPES_LIVE_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/task-recipes/live
TASK_RECIPES_LIVE_EVAL_ARGS ?=

## eval: kind=live requires=magician,magicutor report=evals/task-recipes/live
## desc: Cold browser learning -> warm and variant browserless replay -> forced drift and self-heal; fails if a browser signal moves on warm runs
test-task-recipes-live-eval:
	@cargo run --quiet -p magician --example task_recipes_live_eval -- \
		--base-url '$(TASK_RECIPES_LIVE_BASE_URL)' \
		--magicutor-base-url '$(TASK_RECIPES_LIVE_MAGICUTOR_BASE_URL)' \
		--runtime-root '$(RUNTIME_ROOT_DIR)' \
		--output '$(TASK_RECIPES_LIVE_OUTPUT)' \
		--timeout-secs '$(TASK_RECIPES_LIVE_TIMEOUT_SECS)' $(TASK_RECIPES_LIVE_EVAL_ARGS)

TASK_RECIPES_FIXTURE_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/task-recipes/fixture
TASK_RECIPES_FIXTURE_TIMEOUT_SECS ?= 600
# Empty = a fresh recipes-eval-<id> workspace per run (agent memory is per
# workspace; reusing one lets the agent answer from memory without the site).
TASK_RECIPES_FIXTURE_WORKSPACE ?=
TASK_RECIPES_FIXTURE_EVAL_ARGS ?=

# The harness binary is built separately from running it: the run step never
# touches cargo, so a concurrent agent holding the build-directory lock cannot
# stall the eval while the dev instance it measures sits idle.
TASK_RECIPES_FIXTURE_EVAL_BIN ?= $(CARGO_TARGET_DIR)/$(BUILD_PROFILE)/examples/task_recipes_fixture_eval
TASK_RECIPES_FIXTURE_EVAL_CMD = '$(TASK_RECIPES_FIXTURE_EVAL_BIN)' \
		--base-url '$(TASK_RECIPES_LIVE_BASE_URL)' \
		--magicutor-base-url '$(TASK_RECIPES_LIVE_MAGICUTOR_BASE_URL)' \
		$(if $(strip $(TASK_RECIPES_FIXTURE_WORKSPACE)),--workspace-slug '$(TASK_RECIPES_FIXTURE_WORKSPACE)',) \
		--output '$(TASK_RECIPES_FIXTURE_OUTPUT)' \
		--timeout-secs '$(TASK_RECIPES_FIXTURE_TIMEOUT_SECS)' $(TASK_RECIPES_FIXTURE_EVAL_ARGS)

build-task-recipes-fixture-eval:
	@cargo build --quiet -p magician --example task_recipes_fixture_eval

## eval: kind=live requires=magician,magicutor,provider_keys report=evals/task-recipes/fixture
## desc: Local fixture sites (search, list->detail, Document answer, cookie session + auth heal, guarded write + HITL, GraphQL + drift): one cold LLM browser run per case, then browserless replay proven by the fixture's own request log
test-task-recipes-fixture-eval: build-task-recipes-fixture-eval
	@$(TASK_RECIPES_FIXTURE_EVAL_CMD)

# One iteration of the fix loop: rebuild the debug binary and the harness,
# stop the dev instance, remove earlier eval workspaces (safe only while it is
# stopped — the live service recreates any scope it has hydrated), hot-swap +
# restart, wait for /health, then run the prebuilt harness so the eval never
# measures a stale binary and never waits on cargo after the restart.
test-task-recipes-fixture-iterate: build-magician-debug build-task-recipes-fixture-eval
	./supervisor-ctl stop-magician
	@scripts/purge_task_recipes_eval_workspaces.sh
	@$(MAKE) --no-print-directory replace-restart-magician
	@for i in $$(seq 1 90); do curl -sf '$(TASK_RECIPES_LIVE_BASE_URL)/health' >/dev/null && break; sleep 2; done
	@$(TASK_RECIPES_FIXTURE_EVAL_CMD)

OBSERVABLE_SOURCES_LIVE_API_BASE_URL ?= http://127.0.0.1:3002
OBSERVABLE_SOURCES_LIVE_PRINCIPAL ?= anonymous
OBSERVABLE_SOURCES_LIVE_WORKSPACE ?= default
OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS ?= 20
OBSERVABLE_SOURCES_LIVE_OUTPUT ?= $(COVERAGE_BASE_DIR)/evals/observable-sources-live/latest.json
OBSERVABLE_SOURCES_LIVE_EVAL_ARGS ?=

## eval: kind=harness
## desc: Self-test the observable-sources live evaluator
test-observable-sources-eval-harness:
	@python3 -m unittest scripts.test_observable_sources_live_eval
	@python3 scripts/eval-observable-sources-live.py --self-test

## eval: kind=live requires=magician report=evals/observable-sources-live
## desc: Gate shipped RSS offers, public feeds, and Observe API pagination
test-observable-sources-live-eval:
	@python3 scripts/eval-observable-sources-live.py \
		--allow-network \
		--api-base-url '$(OBSERVABLE_SOURCES_LIVE_API_BASE_URL)' \
		--principal '$(OBSERVABLE_SOURCES_LIVE_PRINCIPAL)' \
		--workspace '$(OBSERVABLE_SOURCES_LIVE_WORKSPACE)' \
		--timeout-secs '$(OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS)' \
		--output '$(OBSERVABLE_SOURCES_LIVE_OUTPUT)' \
		$(OBSERVABLE_SOURCES_LIVE_EVAL_ARGS)

STORAGE_GOVERNANCE_LIVE_API_BASE_URL ?= http://127.0.0.1:3002
STORAGE_GOVERNANCE_LIVE_PRINCIPAL ?= storage-live-eval
STORAGE_GOVERNANCE_LIVE_WORKSPACE ?= governance
STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS ?= 60
STORAGE_GOVERNANCE_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/storage-governance-live/latest
STORAGE_GOVERNANCE_LIVE_EVAL_ARGS ?=

## eval: kind=harness
## desc: Self-test the storage-governance live evaluator
test-storage-governance-eval-harness:
	@python3 -m unittest scripts.test_storage_governance_live_eval
	@python3 scripts/eval-storage-governance-live.py --self-test

## eval: kind=live requires=magician report=evals/storage-governance-live
## desc: Gate isolated inventory, compaction, retention, and safety boundaries
test-storage-governance-live-eval:
	@python3 scripts/eval-storage-governance-live.py \
		--api-base-url '$(STORAGE_GOVERNANCE_LIVE_API_BASE_URL)' \
		--principal '$(STORAGE_GOVERNANCE_LIVE_PRINCIPAL)' \
		--workspace '$(STORAGE_GOVERNANCE_LIVE_WORKSPACE)' \
		--timeout-secs '$(STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS)' \
		--output-dir '$(STORAGE_GOVERNANCE_LIVE_OUTPUT_DIR)' \
		$(STORAGE_GOVERNANCE_LIVE_EVAL_ARGS)

# MiniMax CLI (mmx-cli) skill-wrapper live eval. Provider-free harness runs the
# validators + the static --region guard (CI-safe); the live target exercises
# the 5 skills against the real MiniMax API (needs MINIMAX_API_KEY; region
# defaults to `global`, override with MINIMAX_REGION). search/vision make one
# cheap real call each; media are dry-run unless MINIMAX_CLI_EVAL_ARGS=--real-media.
MINIMAX_CLI_EVAL_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/minimax-cli/latest
MINIMAX_CLI_EVAL_ARGS ?=
## eval: kind=harness
## desc: Self-test the MiniMax CLI skill-wrapper eval without spending API quota
test-minimax-cli-eval-harness:
	@python3 scripts/eval-minimax-cli-live.py --self-test

## eval: kind=live requires=provider_keys report=evals/minimax-cli
## desc: Live-eval the 5 MiniMax CLI (mmx-cli) skills (needs MINIMAX_API_KEY)
eval-minimax-cli-live:
	@python3 scripts/eval-minimax-cli-live.py \
		--output-dir '$(MINIMAX_CLI_EVAL_OUTPUT_DIR)' \
		$(MINIMAX_CLI_EVAL_ARGS)

CHAT_CONTEXT_RETRIEVAL_EVAL_ARGS ?=
benchmark-chat-context-retrieval:
	@cargo run --quiet -p magician --example chat_context_retrieval_eval -- $(CHAT_CONTEXT_RETRIEVAL_EVAL_ARGS)

## eval: kind=harness
## desc: Run provider-free timing/concurrency/fallback regressions
test-chat-context-retrieval-eval:
	@cargo test -p magician-vector-index ollama_keep_alive -- --nocapture
	@cargo test -p magician-vector-index embedding_scheduler -- --nocapture
	@cargo test -p magician-vector-index query_embedding -- --nocapture
	@cargo test -p magician-vector-index logical_embedding -- --nocapture
	@cargo test -p magician-vector-index memory_index::tests -- --nocapture
	@cargo test -p magician --lib chat_context_retrieval -- --nocapture
	@cargo test -p magician --lib memory_index_maintainer::tests -- --nocapture
	@cargo test -p magician --lib resurfacing:: -- --nocapture
	@cargo test -p magician --example chat_context_retrieval_eval -- --nocapture

## eval: kind=live requires=ollama report=evals/chat-context-retrieval
## desc: Gate live hybrid backends, embedding coalescing, and retrieval latency
test-chat-context-retrieval-live-eval:
	@cargo run --quiet -p magician --example chat_context_retrieval_eval -- \
		--runs '$(CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS)' \
		--warmups '$(CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS)' \
		--procedure-index-wait-secs '$(CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS)' \
		--require-hybrid --require-coalescing --require-background-priority \
		--background-contention-runs '$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS)' \
		--background-embedding-inputs '$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS)' \
		--max-background-contention-p95-ms '$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS)' \
		--max-concurrent-wall-p50-ms '$(CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS)' \
		--max-concurrent-wall-p95-ms '$(CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS)' \
		$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',) \
		--output '$(CHAT_CONTEXT_RETRIEVAL_LIVE_OUTPUT)' \
		$(CHAT_CONTEXT_RETRIEVAL_LIVE_EVAL_ARGS)

## eval: kind=harness report=evals/chat-turn-engine
## desc: Chat harness mouth seam, ChatScoped grants, fail-closed unknown engines
test-chat-turn-engine-eval:
	@mkdir -p '$(CHAT_TURN_ENGINE_EVAL_REPORT_DIR)'
	@cp evals/chat-turn-engine/index.html '$(CHAT_TURN_ENGINE_EVAL_REPORT_DIR)/index.html'
	@cp evals/chat-turn-engine/index.html '$(CHAT_TURN_ENGINE_EVAL_REPORT_DIR)/latest.html'
	@cargo test -p magician --lib chat_turn -- --test-threads=1

## eval: kind=harness report=evals/tool-result-projection-context
## desc: Run provider-free matrix/report regressions for structured tool results and staged context
test-tool-result-projection-context-eval-harness: setup-test-suite-runner-deps
	@"$(TEST_SUITE_RUNNER_PYTHON)" -m unittest scripts/test_tool_result_projection_context_live_eval.py
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/eval-tool-result-projection-context-live.py --self-test \
		--output-dir '$(TOOL_RESULT_PROJECTION_EVAL_REPORT_DIR)'

## eval: kind=live requires=provider_keys report=evals/tool-result-projection-context
## desc: Runtime-backed six-repeat counterbalanced projection/context quality, privacy, token, cost, and latency gate
test-tool-result-projection-context-live-eval: setup-test-suite-runner-deps
	@"$(TEST_SUITE_RUNNER_PYTHON)" scripts/eval-tool-result-projection-context-live.py \
		--runs '$(TOOL_RESULT_PROJECTION_LIVE_RUNS)' \
		--profile '$(TOOL_RESULT_PROJECTION_LIVE_PROFILE)' \
		$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',) \
		$(foreach scenario,$(TOOL_RESULT_PROJECTION_LIVE_SCENARIOS),--scenario '$(scenario)') \
		$(if $(TOOL_RESULT_PROJECTION_LIVE_OUTPUT_DIR),--output-dir '$(TOOL_RESULT_PROJECTION_LIVE_OUTPUT_DIR)',)

## eval: kind=harness report=evals/provider-replay
## desc: Provider-free repair matrix, profile selection, checkpoint, and report regressions
test-provider-replay-eval-harness:
	@cargo test -p magician provider_replay -- --nocapture
	@cargo run --quiet -p magician --example provider_replay_live_eval -- \
		--self-test --output-dir '$(PROVIDER_REPLAY_EVAL_REPORT_DIR)'

## eval: kind=live requires=ollama,provider_keys report=evals/provider-replay
## desc: Cross-provider interrupted-history acceptance plus OpenAI Responses checkpoint continuation
test-provider-replay-live-eval:
	@cargo run --quiet -p magician --example provider_replay_live_eval -- \
		$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',) \
		$(foreach profile,$(PROVIDER_REPLAY_LIVE_PROFILES),--profile '$(profile)') \
		--output-dir '$(PROVIDER_REPLAY_LIVE_OUTPUT_DIR)' \
		$(PROVIDER_REPLAY_LIVE_EVAL_ARGS)

## eval: kind=harness report=evals/preplan-flow/deterministic/latest
## desc: Provider-free routing, fixture, projection-identity, lifecycle, and report regressions
test-preplan-flow-eval-harness:
	@python3 -m unittest scripts.test_preplan_flow_live_eval
	@python3 scripts/eval-preplan-flow-live.py --self-test \
		$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',) \
		--output-dir '$(PREPLAN_FLOW_EVAL_REPORT_DIR)'

## eval: kind=live requires=magician,provider_keys report=evals/preplan-flow/latest
## desc: Real mapped-profile pre-plan, terminal-equivalent HITL, Plan-panel, Attention, replan, and telemetry gate
test-preplan-flow-live-eval:
	@python3 scripts/eval-preplan-flow-live.py \
		--api-base-url '$(PREPLAN_LIVE_API_BASE_URL)' \
		--principal '$(PREPLAN_LIVE_PRINCIPAL)' \
		--workspace '$(PREPLAN_LIVE_WORKSPACE)' \
		--hitl-mode '$(PREPLAN_LIVE_HITL_MODE)' \
		--runs '$(PREPLAN_LIVE_RUNS)' \
		--timeout-secs '$(PREPLAN_LIVE_TIMEOUT_SECS)' \
		--projection-timeout-secs '$(PREPLAN_LIVE_PROJECTION_TIMEOUT_SECS)' \
		--http-timeout-secs '$(PREPLAN_LIVE_HTTP_TIMEOUT_SECS)' \
		$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',) \
		--output-dir '$(PREPLAN_LIVE_OUTPUT_DIR)' \
		$(PREPLAN_LIVE_EVAL_ARGS)

# Human-driven companion to the fixture-backed /evals lane. Reading from
# /dev/tty keeps the questions interactive even when Make output is tee'd into
# the normal test report. The assertions are identical to the automated lane.
eval-preplan-flow-live-interactive:
	@$(MAKE) --no-print-directory test-preplan-flow-live-eval \
		PREPLAN_LIVE_HITL_MODE=terminal

## eval: kind=harness report=evals/web-researcher/deterministic/latest
## desc: Provider-free web-research task, lineage, citation, latency, and report regressions
test-web-researcher-eval-harness:
	@python3 -m unittest scripts.test_web_researcher_live_eval
	@python3 scripts/eval-web-researcher-live.py --self-test \
		--output-dir '$(WEB_RESEARCHER_EVAL_REPORT_DIR)'

## eval: kind=live requires=magician,provider_keys report=evals/web-researcher/live/latest
## desc: Real web-researcher direct/delegated answers with observed latency and operator cancellation
test-web-researcher-live-eval:
	@python3 scripts/eval-web-researcher-live.py \
		--api-base-url '$(WEB_RESEARCHER_LIVE_API_BASE_URL)' \
		--runs '$(WEB_RESEARCHER_LIVE_RUNS)' \
		--http-timeout-secs '$(WEB_RESEARCHER_LIVE_HTTP_TIMEOUT_SECS)' \
		--citation-probe-limit '$(WEB_RESEARCHER_LIVE_CITATION_PROBE_LIMIT)' \
		--llm-profile '$(WEB_RESEARCHER_LIVE_LLM_PROFILE)' \
		$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',) \
		--output-dir '$(WEB_RESEARCHER_LIVE_OUTPUT_DIR)' \
		$(WEB_RESEARCHER_LIVE_EVAL_ARGS)

.PHONY: test-harness-conformance-eval-harness test-harness-conformance-live-eval
## eval: kind=harness report=evals/harness-conformance/deterministic/latest
## desc: Provider-free verdict, fixture, and report regressions for the swapped-engine, voice-delegation, and plane-client lanes
test-harness-conformance-eval-harness:
	@python3 scripts/eval-harness-conformance-live.py --self-test --lane '$(HARNESS_CONFORMANCE_LANE)' \
		--output-dir '$(HARNESS_CONFORMANCE_EVAL_REPORT_DIR)'

## eval: kind=live requires=magician,provider_keys report=evals/harness-conformance/live/latest
## desc: Same turns under every installed harness engine (chat lane first), graded by effect plus proof the harness answered; restores the engine afterwards
test-harness-conformance-live-eval:
	@python3 scripts/eval-harness-conformance-live.py \
		--lane '$(HARNESS_CONFORMANCE_LANE)' \
		--api-base-url '$(HARNESS_CONFORMANCE_API_BASE_URL)' \
		--runs '$(HARNESS_CONFORMANCE_RUNS)' \
		$(if $(HARNESS_CONFORMANCE_ENGINES),--engines '$(HARNESS_CONFORMANCE_ENGINES)',) \
		--output-dir '$(HARNESS_CONFORMANCE_LIVE_OUTPUT_DIR)' \
		$(HARNESS_CONFORMANCE_LIVE_EVAL_ARGS)

.PHONY: test-runtime-performance-eval-harness test-runtime-performance-live-eval
## eval: kind=harness report=evals/runtime-performance/deterministic/latest
## desc: Provider-free RSS, storage-growth, crash-scan, baseline, and report regressions
test-runtime-performance-eval-harness:
	@python3 -m unittest scripts.test_runtime_performance_live_eval
	@python3 scripts/eval-runtime-performance-live.py --self-test \
		--output-dir '$(RUNTIME_PERFORMANCE_EVAL_REPORT_DIR)'

## eval: kind=live requires=magician,ollama,provider_keys report=evals/runtime-performance/live/latest
## desc: Gate direct/delegated tasks, HITL, web/mobile Tutor, Attention polling, RSS, context, and store growth
test-runtime-performance-live-eval:
	@python3 scripts/eval-runtime-performance-live.py \
		--api-base-url '$(RUNTIME_PERFORMANCE_LIVE_API_BASE_URL)' \
		--principal '$(RUNTIME_PERFORMANCE_LIVE_PRINCIPAL)' \
		--workspace '$(RUNTIME_PERFORMANCE_LIVE_WORKSPACE)' \
		--llm-profile '$(RUNTIME_PERFORMANCE_LIVE_LLM_PROFILE)' \
		--data-root '$(RUNTIME_PERFORMANCE_LIVE_DATA_ROOT)' \
		--log-path '$(RUNTIME_PERFORMANCE_LIVE_LOG_PATH)' \
		$(if $(filter 1 true yes on,$(RUNTIME_PERFORMANCE_LIVE_REQUIRE_LOG)),--require-log,) \
		--runs '$(RUNTIME_PERFORMANCE_LIVE_RUNS)' \
		--scenario-timeout-seconds '$(RUNTIME_PERFORMANCE_LIVE_TIMEOUT_SECS)' \
		--http-timeout-seconds '$(RUNTIME_PERFORMANCE_LIVE_HTTP_TIMEOUT_SECS)' \
		--recovery-seconds '$(RUNTIME_PERFORMANCE_LIVE_RECOVERY_SECS)' \
		--max-peak-rss-mib '$(RUNTIME_PERFORMANCE_LIVE_MAX_PEAK_RSS_MIB)' \
		--max-end-growth-mib '$(RUNTIME_PERFORMANCE_LIVE_MAX_END_GROWTH_MIB)' \
		--max-storage-growth-mib '$(RUNTIME_PERFORMANCE_LIVE_MAX_STORAGE_GROWTH_MIB)' \
	$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',) \
	$(if $(RUNTIME_PERFORMANCE_LIVE_BASELINE),--baseline '$(RUNTIME_PERFORMANCE_LIVE_BASELINE)',) \
	$(if $(filter 1 true yes on,$(RUNTIME_PERFORMANCE_LIVE_CAPTURE_BASELINE_IF_MISSING)),--capture-baseline-if-missing,) \
	$(foreach scenario,$(RUNTIME_PERFORMANCE_LIVE_SCENARIOS),--scenario '$(scenario)') \
		--output-dir '$(RUNTIME_PERFORMANCE_LIVE_OUTPUT_DIR)' \
		$(RUNTIME_PERFORMANCE_LIVE_EVAL_ARGS)

MEMORY_TIER_HEALTH_OVERLAY ?= $(RUNTIME_ROOT_DIR)/scopes/$(MEMORY_INDEX_PRINCIPAL)/$(MEMORY_INDEX_WORKSPACE)/memory/index/temperature_overlay.json
MEMORY_TIER_HEALTH_DOCUMENTS ?= $(RUNTIME_ROOT_DIR)/scopes/$(MEMORY_INDEX_PRINCIPAL)/$(MEMORY_INDEX_WORKSPACE)/memory/index/documents.jsonl

## eval: kind=harness
## desc: Gate tier-effectiveness metrics for a scoped temperature overlay (read-only, provider-free)
test-memory-tier-health-eval:
	@cargo run --quiet -p magician-vector-index --example memory_tier_health -- \
		--overlay '$(MEMORY_TIER_HEALTH_OVERLAY)' \
		--documents '$(MEMORY_TIER_HEALTH_DOCUMENTS)' $(MEMORY_TIER_HEALTH_FLAGS)

## eval: kind=harness
## desc: Run the provider-free memory-temperature evaluator regressions
test-memory-temperature-eval-harness:
	@cargo test -p magician --example memory_temperature_live_eval -- --nocapture

## eval: kind=harness report=evals/memory-ann-shadow/deterministic/latest
## desc: Provider-free ANN shadow/flat vector-search regressions (YAML default is ann)
test-memory-ann-shadow-eval-harness:
	@python3 -m unittest scripts.test_write_eval_harness_report
	@python3 scripts/write_eval_harness_report.py \
		--title 'ANN shadow/flat vector-search harness' \
		--output-dir '$(MEMORY_ANN_SHADOW_EVAL_REPORT_DIR)' \
		-- cargo test -p magician-vector-index --lib vector_search -- --nocapture

## eval: kind=live requires=ollama,magician_binary report=evals/memory-ann-shadow/live/latest
## desc: Live memory recall under MAGICIAN_VECTOR_SEARCH=ann_shadow (serves flat; observes ANN)
test-memory-ann-shadow-live-eval:
	@MAGICIAN_VECTOR_SEARCH=ann_shadow $(MAKE) --no-print-directory test-memory-temperature-live-eval \
		MEMORY_TEMPERATURE_LIVE_OUTPUT_DIR='$(MEMORY_ANN_SHADOW_LIVE_OUTPUT_DIR)'

.PHONY: test-admission-300-task-eval-harness
## eval: kind=harness report=evals/admission-300-task/deterministic/latest
## desc: Provider-free 300-task admission-count harness (not the owner-gated live soak)
test-admission-300-task-eval-harness:
	@python3 scripts/write_eval_harness_report.py \
		--title '300-task admission-count harness' \
		--output-dir '$(ADMISSION_300_TASK_EVAL_REPORT_DIR)' \
		-- cargo test -p magician-core --lib admit_agent_loop_300_task_admission_count_harness -- --nocapture

.PHONY: build-memory-connections-eval test-memory-connections-live-eval
.PHONY: build-memory-lifecycle-eval test-memory-lifecycle test-memory-lifecycle-unit test-memory-lifecycle-live-eval test-memory-lifecycle-evals-integration
build-memory-lifecycle-eval:
	@mkdir -p '$(MEMORY_LIFECYCLE_TMPDIR)'
	@TMPDIR='$(MEMORY_LIFECYCLE_TMPDIR)' cargo build --locked --offline -p magician-comms --example memory_lifecycle_eval

## eval: kind=harness report=evals/memory-lifecycle/deterministic
## desc: Focused memory lifecycle preservation, consolidation and owner-answer regressions
test-memory-lifecycle:
	@mkdir -p '$(MEMORY_LIFECYCLE_TMPDIR)'
	@TMPDIR='$(MEMORY_LIFECYCLE_TMPDIR)' python3 scripts/eval-memory-lifecycle.py --mode harness \
		--output-dir '$(MEMORY_LIFECYCLE_TEST_OUTPUT_DIR)' --run-id '$(EVAL_RUN_ID)'

test-memory-lifecycle-unit:
	@cargo test --locked --offline -p magician --features test-fixtures --lib memory_lifecycle -- --test-threads=1
	@cargo test --locked --offline -p magician-comms --features test-fixtures --lib memory_lifecycle -- --test-threads=1

## eval: kind=live requires=provider_keys report=evals/memory-lifecycle/live
## desc: Frozen memory lifecycle journeys; compare configured profiles, recall and durable owner answers
test-memory-lifecycle-live-eval:
	@mkdir -p '$(MEMORY_LIFECYCLE_TMPDIR)'
	@TMPDIR='$(MEMORY_LIFECYCLE_TMPDIR)' python3 scripts/eval-memory-lifecycle.py \
		--binary '$(MEMORY_LIFECYCLE_EVAL_BINARY)' --config '$(LIVE_EVAL_CONFIG)' \
		--output-dir '$(MEMORY_LIFECYCLE_EVAL_OUTPUT_DIR)' --run-id '$(EVAL_RUN_ID)' \
		--profiles '$(MEMORY_LIFECYCLE_EVAL_PROFILES)' --repeats '$(MEMORY_LIFECYCLE_EVAL_REPEATS)' \
		--partition '$(MEMORY_LIFECYCLE_EVAL_PARTITION)'

test-memory-lifecycle-evals-integration:
	@PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_memory_lifecycle_evals.py
	@cargo test --locked --offline -p magician-surfaces --lib evals::runner::tests -- --test-threads=1
	@cargo test --locked --offline -p magician-surfaces --lib evals::cost::tests -- --test-threads=1
	@cargo test --locked --offline -p magician-surfaces --lib evals::options::tests -- --test-threads=1
	@cargo test --locked --offline -p magician-api --lib evals_api::tests -- --test-threads=1

build-memory-connections-eval:
	@cargo build --locked --offline -p magician-comms --example memory_connections_live_eval --example memory_capture_journey_live_eval --example memory_connection_response_journey

.PHONY: test-memory-connections-runtime
test-memory-connections-runtime:
	@cargo test --locked --offline -p magician-comms --lib memory_connections_ -- --test-threads=1

.PHONY: test-memory-connections-eval-reporting
test-memory-connections-eval-reporting:
	@cargo test --locked --offline -p magician-comms --example memory_connections_live_eval memory_eval_usage_requires_a_provider_usage_object
	@PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_memory_connections_eval.py

## eval: kind=live requires=provider_keys report=evals/memory-connections/live/latest
## desc: Real-model memory connections over frozen synthetic cases and production recall/delivery
test-memory-connections-live-eval: build-memory-connections-eval
	@python3 scripts/eval-memory-connections.py \
		--binary '$(CARGO_TARGET_DIR)/debug/examples/memory_connections_live_eval' \
		--config '$(LIVE_EVAL_CONFIG)' --output-dir '$(MEMORY_CONNECTIONS_EVAL_OUTPUT_DIR)' \
		--partition '$(MEMORY_CONNECTIONS_EVAL_PARTITION)' --repeats '$(MEMORY_CONNECTIONS_EVAL_REPEATS)' \
		--max-calls '$(MEMORY_CONNECTIONS_EVAL_MAX_CALLS)' --max-reported-tokens '$(MEMORY_CONNECTIONS_EVAL_MAX_TOKENS)'

## eval: kind=live requires=ollama,magician_binary report=evals/memory-temperature
## desc: Gate real/synthetic memory recall and Ollama utility review
test-memory-temperature-live-eval:
	@cargo build --quiet -p magician --example memory_temperature_live_eval
	@MAGICIAN_CONFIG_PATH='$(LIVE_EVAL_CONFIG)' $(MAKE) --no-print-directory run-ollama
	@MAGICIAN_CONFIG_PATH='$(LIVE_EVAL_CONFIG)' $(MAKE) memory-index-prewarm \
		MEMORY_INDEX_PRINCIPAL='$(MEMORY_INDEX_PRINCIPAL)' \
		MEMORY_INDEX_WORKSPACE='$(MEMORY_INDEX_WORKSPACE)'
	@$(CARGO_TARGET_DIR)/debug/examples/memory_temperature_live_eval \
		--principal '$(MEMORY_INDEX_PRINCIPAL)' \
		--workspace '$(MEMORY_INDEX_WORKSPACE)' \
		--output-dir '$(MEMORY_TEMPERATURE_LIVE_OUTPUT_DIR)' \
		--min-real-recall '$(MEMORY_TEMPERATURE_LIVE_MIN_RECALL)' \
		--min-hybrid-coverage '$(MEMORY_TEMPERATURE_LIVE_MIN_HYBRID)' \
		--min-utility-accuracy '$(MEMORY_TEMPERATURE_LIVE_MIN_UTILITY)' \
		--max-retrieval-p95-ms '$(MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS)' \
		--max-utility-p95-ms '$(MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS)' \
		$(if $(LIVE_EVAL_CONFIG),--config '$(LIVE_EVAL_CONFIG)',) \
		$(MEMORY_TEMPERATURE_LIVE_EVAL_ARGS)

MEDIA_AUDIO_ROLLOUT_ARGS ?=
verify-media-audio-rollout:
	@python3 scripts/verify-media-audio-rollout.py $(MEDIA_AUDIO_ROLLOUT_ARGS)

AUDIO_ENGINE ?= fluid_audio
AUDIO_MODEL ?=
AUDIO_ENGINE_API ?= http://127.0.0.1:3002

audio-engine-status:
	@python3 scripts/audio-engine-control.py status --base-url "$(AUDIO_ENGINE_API)" --engine "$(AUDIO_ENGINE)"

audio-model-prewarm:
	@if [ -z "$(AUDIO_MODEL)" ]; then echo "AUDIO_MODEL is required"; exit 2; fi
	@python3 scripts/audio-engine-control.py prewarm --base-url "$(AUDIO_ENGINE_API)" --engine "$(AUDIO_ENGINE)" --model "$(AUDIO_MODEL)"

audio-model-unload:
	@python3 scripts/audio-engine-control.py unload --base-url "$(AUDIO_ENGINE_API)" --engine "$(AUDIO_ENGINE)" $(if $(AUDIO_MODEL),--model "$(AUDIO_MODEL)")

# Start magic-supervisor (runs magician + magicutor)
run-supervisor:
	@echo "🚀 Starting magic-supervisor..."
	@echo "   - Control port: 8081"
	@echo "   - Magician: http://localhost:3002"
	@echo "   - Magicutor: http://localhost:3003"
	@if [ "$(MAGICIAN_CHAT_TRACE)" = "1" ]; then \
		echo "   - Chat tracing: ON (per-turn dumps under each chat session's trace dir)"; \
	fi
	@echo ""
	@RUST_LOG=$(RUST_LOG) MAGICIAN_CHAT_TRACE=$(MAGICIAN_CHAT_TRACE) MAGICIAN_HOST_GATEWAY_URL="$(HOST_GATEWAY_URL)" ./scripts/run-supervisor.sh

# Run supervisor with chat tracing enabled — dumps every chat turn's prompt,
# LLM response, tool calls, and tool results under the session's trace dir.
# Useful when debugging chat regressions or auditing the agent's decisions.
run-supervisor-chat-trace:
	@$(MAKE) run-supervisor MAGICIAN_CHAT_TRACE=1 RUST_LOG=info,magician=debug

# Start everything: Vite UI + Cloudflare Tunnel (for Kapso webhook) + magic-supervisor + host tray
KAPSO_WEBHOOK_PORT ?= 3010
# Boot everything: Vite UI (:5173) + Cloudflare Tunnel (Kapso webhook) + code-graph
# server + supervisor + host tray. Ctrl+C / SIGTERM on the supervisor process
# triggers `stop-all` so the UI + supervisor + tray get torn down cleanly. The
# Cloudflare Tunnel is a persistent service (brew services) shared across
# restarts, so stop-all deliberately leaves it up.
#
# Note: ensure-tunnel + graph-serve-bg are prereqs (run before the trap is
# installed). The trap covers the supervisor invocation that actually blocks the
# foreground.
run-all: ensure-tunnel graph-serve-bg
	@bash -c 'set -e; \
		UI_PID=""; \
		SUPERVISOR_PID=""; \
		TRAY_PID=""; \
		cleanup() { \
			echo ""; \
			echo "🧹 Shutting down run-all stack..."; \
			$(MAKE) --no-print-directory stop-all; \
		}; \
		trap cleanup INT TERM EXIT; \
		echo "🎨 Starting unified-ui dev server..."; \
		$(MAKE) --no-print-directory run-ui-dev & \
		UI_PID=$$!; \
		echo "🚀 Starting runtime supervisor first..."; \
		$(MAKE) --no-print-directory run-supervisor & \
		SUPERVISOR_PID=$$!; \
		echo "⏳ Waiting for Magician API health on http://127.0.0.1:3002/health..."; \
		READY=0; \
		for _ in $$(seq 1 90); do \
			if curl -fsS http://127.0.0.1:3002/health >/dev/null 2>&1; then \
				READY=1; \
				break; \
			fi; \
			sleep 1; \
		done; \
		if [ "$$READY" = "1" ]; then \
			echo "✅ Magician API is healthy; starting host tray/gateway."; \
		else \
			echo "⚠️  Magician API did not report healthy yet; starting host tray/gateway anyway."; \
		fi; \
		$(MAKE) --no-print-directory run-desktop-tray-debug & \
		TRAY_PID=$$!; \
		echo "   - ui dev pid: $$UI_PID"; \
		echo "   - supervisor pid: $$SUPERVISOR_PID"; \
		echo "   - tray/gateway pid: $$TRAY_PID"; \
		wait $$SUPERVISOR_PID'

# Ensure the public Cloudflare Tunnel (cloudflared) is running for Kapso webhook
# ingress. Delegates to scripts/ensure-magician-tunnel.sh — a single persistent
# named tunnel ('magician') on the operator domain that maps
# webhook.<zone> -> :$(KAPSO_WEBHOOK_PORT) (and ui.<zone> -> :5173) via
# ~/.cloudflared/config.yml, started as a brew service so it survives reboots /
# shell exits. First run is interactive (cloudflared login + tunnel create); the
# script DETECTS that, prints the exact one-time steps, and exits cleanly. It
# always writes the stable webhook URL to the runtime root's funnel-url file.
ensure-tunnel:
	@KAPSO_WEBHOOK_PORT=$(KAPSO_WEBHOOK_PORT) bash scripts/ensure-magician-tunnel.sh \
		|| echo "⚠️  cloudflared tunnel setup reported a failure — Kapso webhooks may not be reachable"

# One stable device hostname can point to exactly one active backend. Selection
# is explicit even if both listeners exist; the helper refuses an unhealthy
# target and persists the choice only after Cloudflare confirms the route.
connect-access:
	@bash scripts/ensure-connect-access.sh

connect-setup: connect-access
	@bash scripts/select-connect-backend.sh "$${MAGICIAN_CONNECT_BACKEND:-local}"

connect-status:
	@bash scripts/select-connect-backend.sh status

connect-local:
	@bash scripts/select-connect-backend.sh local

connect-container:
	@bash scripts/select-connect-backend.sh container

connect-remote:
	@bash scripts/select-connect-backend.sh remote "$${CONNECT_REMOTE_URL:-$${MAGICIAN_CONNECT_REMOTE_URL:-}}"

# Stop the shared Cloudflare Tunnel service. Not part of stop-all (the tunnel is
# a persistent service shared across restarts); stopping it also stops the dev-UI
# (ui.<zone>) ingress. Use when you explicitly want the public endpoints down.
stop-tunnel:
	@if command -v brew >/dev/null 2>&1; then \
		echo "🔗 Stopping cloudflared service (brew services stop cloudflared)..."; \
		brew services stop cloudflared >/dev/null 2>&1 || true; \
		echo "   ✅ Cloudflare Tunnel stopped (Kapso webhook + dev UI ingress are now down)."; \
	else \
		echo "⚠️  brew not found — stop the tunnel manually (kill the 'cloudflared tunnel run' process)."; \
	fi

# Teardown for the full local stack. Order matters: stop the host tray and its
# process-owned native surfaces first, then stop Vite, then stop the runtime
# supervisor/bots. The Cloudflare Tunnel is a persistent service shared across
# restarts, so it is deliberately LEFT UP (stop it explicitly with `make
# stop-tunnel` / `brew services stop cloudflared`).
stop-all:
	@$(MAKE) --no-print-directory stop-desktop-tray
	@$(MAKE) --no-print-directory stop-ui-dev
	@$(MAKE) --no-print-directory stop-supervisor
	@echo "🔗 Cloudflare Tunnel left running (persistent service) — Kapso webhooks stay reachable. Stop with 'make stop-tunnel'."

# Stop magic-supervisor (also kills bot daemons — they're spawned out-of-band
# from the supervised magician process and won't get reaped on supervisor exit)
stop-supervisor: stop-bots
	@echo "⏹️  Stopping magic-supervisor..."
	@SUPERVISOR_CTL_ASSUME_YES=1 SUPERVISOR_CTL_QUIET=1 ./supervisor-ctl shutdown >/dev/null 2>&1 || echo "   No running magic-supervisor found."
	@$(MAKE) --no-print-directory stop-macos-audio-engine
	@$(MAKE) --no-print-directory stop-ollama

# Compatibility sweep for sidecars orphaned by older supervisor binaries or
# by a native Magician crash. New supervised launches use a dedicated process
# group, but this remains intentionally idempotent for existing local stacks.
stop-macos-audio-engine:
	@bash scripts/stop-macos-audio-engine.sh

# Kill bot daemons + detached subprocesses spawned by the magician runtime.
#
# Four categories of orphan are swept:
#   1. Node bot daemons under `<scope>/bots/<bot>/dist/index.js` (new layout)
#      or the legacy `<scope>/capabilities/bots/<bot>/dist/index.js` path.
#   2. `gws` subprocesses spawned by Gmail/Calendar/Sheets/Tasks bots —
#      these live inside the bot's node_modules and run e.g. `gws gmail
#      +watch`. They sometimes survive their parent because of Node's
#      detached child semantics; clean them up explicitly.
#   3. `agent-browser` CLI sessions spawned for browser-tool calls —
#      both the dev-path (`skillshub/browser/...`) and the installed-path
#      (`$(RUNTIME_ROOT_DIR)/system/skills/browser/...`).
#   4. (Reserved) future detached subprocesses.
#
# For each category: count, SIGTERM, 1s grace, SIGKILL any survivor.
# Patterns are anchored to magician's own paths to avoid matching
# unrelated processes elsewhere on the system.
stop-bots:
	@echo "⏹️  Stopping bot daemons + detached magician subprocesses..."
	@sweep() { \
		pattern="$$1"; label="$$2"; \
		n=$$(pgrep -f "$$pattern" 2>/dev/null | wc -l | tr -d ' '); \
		if [ "$$n" -eq 0 ]; then return 0; fi; \
		echo "   killing $$n $$label..."; \
		pkill -f "$$pattern" 2>/dev/null || true; \
		sleep 1; \
		remaining=$$(pgrep -f "$$pattern" 2>/dev/null | wc -l | tr -d ' '); \
		if [ "$$remaining" -gt 0 ]; then \
			echo "   $$remaining $$label still running, sending SIGKILL..."; \
			pkill -9 -f "$$pattern" 2>/dev/null || true; \
		fi; \
	}; \
	sweep 'node.*scopes/.*/bots/.*/dist/index\.js' 'bot daemon(s)'; \
	sweep '(skillshub|scopes/[^ ]*/bots)/.*/gws ' 'gws subprocess(es)'; \
	sweep 'magician/.*agent-browser-(darwin|linux|windows)' 'agent-browser session(s)'; \
	sweep 'agent-browser-chrome-[0-9a-f]' 'Chrome-for-Testing session(s) launched by agent-browser'; \
	sweep '\.agent-browser/browsers/chrome-' 'agent-browser Chromium helper(s)'; \
	sweep 'cloakbrowser-darwin-|cloakbrowser-linux-|\.cloakbrowser/chromium-' 'CloakBrowser session(s)'; \
	echo "   done."

# Restart Magician service
restart-magician:
	@echo "🔄 Restarting Magician service..."
	./supervisor-ctl restart-magician

# Restart Magicutor service
restart-magicutor:
	@echo "🔄 Restarting Magicutor service..."
	./supervisor-ctl restart-magicutor

# Stop Magician service (also stops bot daemons — bots are spawned by the
# magician runtime but detach into independent node processes that survive
# magician restarts on their own)
stop-magician: stop-bots
	@echo "⏹️  Stopping Magician service..."
	./supervisor-ctl stop-magician
	@$(MAKE) --no-print-directory stop-macos-audio-engine

# Stop Magicutor service
stop-magicutor:
	@echo "⏹️  Stopping Magicutor service..."
	./supervisor-ctl stop-magicutor

# Check Magician service status
status-magician:
	@echo "ℹ️  Checking Magician service status..."
	./supervisor-ctl status-magician

# Check Magicutor service status
status-magicutor:
	@echo "ℹ️  Checking Magicutor service status..."
	./supervisor-ctl status-magicutor

# Local ONNX runtime + models for the decision engine (pinned + SHA-256
# verified) under the runtime root. Default: ONNX Runtime 1.30 + laya.
# MODELS="onnxruntime laya-multilingual kev-0.8b" (kev-4b is ~4.7 GB).
setup-decision-models:
	./scripts/setup-decision-models.sh $(MODELS)

# Decision engine service (optional; managed when decision-engine.bin exists)
restart-decision-engine:
	@echo "🔄 Restarting the decision engine..."
	./supervisor-ctl restart-decision-engine

stop-decision-engine:
	@echo "⏹️  Stopping the decision engine..."
	./supervisor-ctl stop-decision-engine

status-decision-engine:
	@echo "ℹ️  Checking the decision engine status..."
	./supervisor-ctl status-decision-engine

# Legacy target aliases for older docs/scripts
restart-magictunnel: restart-magicutor

stop-magictunnel: stop-magicutor

status-magictunnel: status-magicutor

# Check supervisor health
supervisor-health:
	@echo "🏥 Checking supervisor health..."
	./supervisor-ctl health

tail-service-log:
	@case "$(SERVICE)" in \
		stack|supervisor) \
			touch magician.log; \
			echo "📜 Tailing full supervisor stack log: magician.log"; \
			tail -F magician.log; \
			;; \
		magician) \
			touch magician.log; \
			echo "📜 Tailing Magician service lines from magician.log"; \
			tail -F magician.log | grep --line-buffered '\[Magician\]'; \
			;; \
		magicutor) \
			touch magician.log; \
			echo "📜 Tailing Magicutor service lines from magician.log"; \
			tail -F magician.log | grep --line-buffered '\[Magicutor\]'; \
			;; \
		ui|vite) \
			touch "$(UI_DEV_LOG_FILE)"; \
			echo "📜 Tailing Vite UI log: $(UI_DEV_LOG_FILE)"; \
			tail -F "$(UI_DEV_LOG_FILE)"; \
			;; \
		host-tray|tray) \
			touch "$(HOST_TRAY_LOG_FILE)"; \
			echo "📜 Tailing host tray/gateway log: $(HOST_TRAY_LOG_FILE)"; \
			tail -F "$(HOST_TRAY_LOG_FILE)"; \
			;; \
		*) \
			echo "Unknown SERVICE=$(SERVICE). Use stack, magician, magicutor, ui, or host-tray."; \
			exit 2; \
			;; \
	esac

open-service-log:
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		echo "❌ open-service-log is macOS-only. Run: make tail-service-log SERVICE=$(SERVICE)"; \
		exit 1; \
	fi
	@osascript -e 'tell application "Terminal" to do script "cd \"$(CURDIR)\" && make tail-service-log SERVICE=$(SERVICE)"'

# Tail Magician V2 log entries
tail-magician-v2-log:
	@echo "📜 Tailing Magician V2 log entries (writing to magician_v2.log)..."
	tail -f magician-v2.log | grep --line-buffered "\[MAGICIAN-V2" | tee magician_v2.log

# Install the deterministic host prerequisite used by LanceDB's build scripts.
# Runnable test targets use this self-healing surface; check-protoc below stays
# read-only for check/CI callers that do not authorize package-manager changes.
setup-protoc:
	@if [ -n "$${PROTOC:-}" ] && "$$PROTOC" --version >/dev/null 2>&1; then \
		echo "✓ protoc available through PROTOC=$$PROTOC"; \
	elif command -v protoc >/dev/null 2>&1 && protoc --version >/dev/null 2>&1; then \
		echo "✓ $$(protoc --version)"; \
	elif [ "$$(uname -s)" = "Darwin" ] && command -v brew >/dev/null 2>&1; then \
		echo "Installing protobuf (protoc) for Rust workspace tests..."; \
		brew install protobuf; \
	else \
		echo "❌ protoc is required and could not be installed automatically."; \
		echo "   macOS: brew install protobuf"; \
		echo "   Or set PROTOC=/absolute/path/to/protoc"; \
		exit 1; \
	fi
	@$(MAKE) --no-print-directory check-protoc

setup-ffmpeg:
	@if ! (command -v ffmpeg >/dev/null 2>&1 && ffmpeg -version >/dev/null 2>&1 && \
	      command -v ffprobe >/dev/null 2>&1 && ffprobe -version >/dev/null 2>&1); then \
		if [ "$$(uname -s)" = "Darwin" ] && command -v brew >/dev/null 2>&1; then \
		echo "Installing ffmpeg for media-edit runtime/tests..."; \
		brew install ffmpeg; \
		fi; \
	fi
	@$(MAKE) --no-print-directory check-ffmpeg

check-ffmpeg:
	@if command -v ffmpeg >/dev/null 2>&1 && ffmpeg -version >/dev/null 2>&1 && \
	   command -v ffprobe >/dev/null 2>&1 && ffprobe -version >/dev/null 2>&1; then \
		echo "✓ ffmpeg and ffprobe available"; \
	else \
		echo "❌ ffmpeg and ffprobe are required and could not be verified."; \
		echo "   macOS: brew install ffmpeg"; \
		exit 1; \
	fi

# Fail before the long Rust build when LanceDB's protobuf compiler dependency is
# absent. prost-build also supports an explicit PROTOC executable override.
check-protoc:
	@if [ -n "$${PROTOC:-}" ]; then \
		if "$$PROTOC" --version >/dev/null 2>&1; then exit 0; fi; \
		echo "❌ PROTOC is set but is not an executable protoc binary: $$PROTOC"; \
		exit 1; \
	elif command -v protoc >/dev/null 2>&1 && protoc --version >/dev/null 2>&1; then \
		exit 0; \
	else \
		echo "❌ Protocol Buffers compiler (protoc) is required by LanceDB builds."; \
		echo "   macOS: brew install protobuf"; \
		echo "   Or run: make setup-prerequisites"; \
		echo "   Or set PROTOC=/absolute/path/to/protoc"; \
		exit 1; \
	fi

.PHONY: check-protoc

# Run all compile/type-check gates: Rust workspace, Unified UI,
# Android build/tests, and iOS (mobile gates skip without their toolchains).
check-all: check-protoc verify-magios-vosk event-taxonomy-check component-graph-check presentation-identity-check check-service-name-boundary check-phase5g-conformance check-rank-recompute-semantics check-notes-capture-surface check-device-bridge-protocol check-magdroid check-store-durability check-storage-catalog check-typed-storage-boundaries check-links check-bg-run check-ollama-chokepoint check-extracted-libraries
	cargo check --workspace --all-targets
	@# The workspace build unifies `magician/test-fixtures` on (magician-api and
	@# magician-comms request it in dev-dependencies), which compiles every
	@# fixture into magician's plain lib target where nothing calls it. The
	@# crate root suppresses dead_code there because "unused" carries no
	@# information in that configuration — so run the DEFAULT build too, which
	@# is the only place real dead code in magician's production source shows up.
	cargo check -p magician --lib
	@$(MAKE) check-desktop
	@$(MAKE) check-ui
	@$(MAKE) ios-debug-check

# Prevent the backend service identity from becoming an assistant, agent,
# product, or app identity in active prompts and presentation surfaces.
check-service-name-boundary:
	@python3 scripts/check_service_name_boundary.py
	@python3 -m unittest scripts/test_service_name_boundary.py

.PHONY: check-service-name-boundary

# Product-facing identity is authored once and generated into each language so
# offline/first-run native surfaces never need a live backend just to name the
# product. The check also pins migrated consumers to the generated seam.
presentation-identity-codegen:
	@python3 scripts/presentation_identity_codegen.py generate

presentation-identity-check:
	@python3 scripts/presentation_identity_codegen.py check
	@python3 -m unittest scripts/test_presentation_identity_codegen.py

.PHONY: presentation-identity-codegen presentation-identity-check

# The pure public contract, canonical supported-public operation inventory and
# current Rust wire types jointly generate schemas, OpenAPI and cross-client
# fixtures. Route registration consumes the same inventory.
app-contract-codegen:
	@echo "[codegen] regenerating Apps supported-public contract artifacts…"
	@cargo run -p magician-apps --bin app_contract_codegen --quiet -- generate

app-contract-check:
	@cargo run -p magician-apps --bin app_contract_codegen --quiet -- check
	@cargo test -p magician-apps --bin app_contract_codegen --quiet
	@cargo test -p magician-api every_supported_public_inventory_entry_is_registered_with_its_method --quiet
	@$(MAKE) test-app-contract-clients
	@$(MAKE) test-app-typescript-sdk
	@$(MAKE) test-app-typescript-consumer-canary
	@$(MAKE) test-app-typescript-real-server-canary

test-app-contract-clients: verify-ui-test-deps
	@cd ui/unified-ui && npx vitest run src/lib/app-platform/AppContractFixtures.generated.test.ts
	@mkdir -p '$(CARGO_TARGET_DIR)'
	@xcrun swiftc magios/Shared/AppContractFixtures.generated.swift \
		magios/AppContractFixtureTests/main.swift \
		-o '$(CARGO_TARGET_DIR)/magician-app-contract-fixture-tests'
	@'$(CARGO_TARGET_DIR)/magician-app-contract-fixture-tests'

.PHONY: setup-app-typescript-sdk-deps
# Unified UI imports SDK source directly; resolve runtime dependencies beside it.
setup-app-typescript-sdk-deps:
	@npm --prefix sdk/typescript ci --ignore-scripts --no-audit --no-fund

test-app-typescript-sdk: setup-app-typescript-sdk-deps
	@npm --prefix sdk/typescript test

test-app-typescript-consumer-canary:
	@bash scripts/test-app-typescript-consumer-canary.sh

test-app-typescript-real-server-canary:
	@bash scripts/test-app-typescript-real-server-canary.sh

test-app-authoring:
	@cargo test -p magician --lib magician_v2::apps::authoring::tests
	@cargo test -p magician --lib magician_v2::apps::procedure_publication::tests
	@cargo test -p magician --lib magician_v2::apps::candidate_publication::tests

.PHONY: test-app-native-owner
test-app-native-owner:
	@env -u RUST_MIN_STACK cargo test -p magician-api --lib apps_api::provider_free_native_tests -- --nocapture

.PHONY: test-android-native-authorization-contract test-android-attestation-policy test-android-automation-trust
test-android-native-authorization-contract:
	@env -u RUST_MIN_STACK cargo test -p magician-app-contract --lib begin_enrollment_native_request_requires_the_typed_body_digest

test-android-attestation-policy:
	@env -u RUST_MIN_STACK cargo test -p magician-api --lib android_apps_attestation::tests

test-android-automation-trust: test-android-native-authorization-contract test-android-attestation-policy
	@env -u RUST_MIN_STACK cargo test -p magician-api --lib android_automation_trust::tests

.PHONY: test-app-runtime-concurrency-regressions
test-app-runtime-concurrency-regressions:
	@env -u RUST_MIN_STACK cargo test -p magician --features test-fixtures --lib app_runtime_concurrency_regression -- --test-threads=1

.PHONY: test-app-workflow-seals
test-app-workflow-seals:
	@env -u RUST_MIN_STACK cargo test -p magician --features test-fixtures --lib workflow_sidecar_seal -- --test-threads=1

.PHONY: test-app-reconciliation
test-app-reconciliation:
	@cargo test -p magician-bin --test app_reconciliation_contract

.PHONY: test-plane-elicitation
test-plane-elicitation:
	@cargo test -p magician-bin --test plane_elicitation_contract

.PHONY: test-execution-harness
## eval: kind=harness report=evals/harness-conformance/execution/deterministic/latest
## desc: Execution harness completion, token accounting, catalog, approval and continuation contracts
test-execution-harness:
	@cargo test -p magician-bin --test execution_harness_contract -- --test-threads=1
	@$(MAKE) test-execution-harness-eval

.PHONY: test-execution-harness-eval
## eval: kind=harness report=evals/harness-conformance/execution/deterministic/latest
## desc: Provider-free checks of tool, recovery and real-child delegation eval evidence; no running service needed
test-execution-harness-eval:
	@python3 scripts/eval-harness-conformance-live.py --lane execution --self-test \
		--output-dir '$(COVERAGE_BASE_DIR)/evals/harness-conformance/execution/deterministic/latest'

.PHONY: test-execution-harness-live
## eval: kind=live requires=magician,provider_keys report=evals/harness-conformance/execution/live/latest
## desc: Stage-one matrix: governed read/write, tool-error recovery, child delegation and parent continuation under all six engines
test-execution-harness-live:
	@$(MAKE) test-harness-conformance-live-eval HARNESS_CONFORMANCE_LANE=execution \
		HARNESS_CONFORMANCE_LIVE_OUTPUT_DIR='$(COVERAGE_BASE_DIR)/evals/harness-conformance/execution/live/latest'

HARNESS_EXECUTION_ADAPTER_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/harness-conformance/execution/adapters/latest

.PHONY: test-execution-harness-adapters-live
## eval: kind=live requires=provider_keys report=evals/harness-conformance/execution/adapters/latest
## desc: Drive installed execution harnesses through the production MCP HTTP handler on an isolated temporary port
test-execution-harness-adapters-live:
	@HARNESS_EXECUTION_LIVE_ENGINES='$(HARNESS_CONFORMANCE_ENGINES)' \
		HARNESS_EXECUTION_ADAPTER_REPORT_DIR='$(HARNESS_EXECUTION_ADAPTER_REPORT_DIR)' \
		cargo test -p magician-bin --test execution_harness_contract installed_harnesses_execute_governed_file_flow -- --ignored --nocapture --test-threads=1

.PHONY: run-execution-harness-adapters-live
## eval: kind=live requires=provider_keys report=evals/harness-conformance/execution/adapters/latest
## desc: Repeat the live adapter flow against a previously compiled test binary while other agents edit the workspace
run-execution-harness-adapters-live:
	@test -n '$(HARNESS_EXECUTION_TEST_BINARY)' -a -x '$(HARNESS_EXECUTION_TEST_BINARY)' || \
		{ echo 'Set HARNESS_EXECUTION_TEST_BINARY to the execution_harness_contract binary printed by make test-execution-harness'; exit 2; }
	@HARNESS_EXECUTION_LIVE_ENGINES='$(HARNESS_CONFORMANCE_ENGINES)' \
		HARNESS_EXECUTION_ADAPTER_REPORT_DIR='$(HARNESS_EXECUTION_ADAPTER_REPORT_DIR)' \
		'$(HARNESS_EXECUTION_TEST_BINARY)' installed_harnesses_execute_governed_file_flow --ignored --nocapture --test-threads=1

.PHONY: test-plane-harness-eval
## eval: kind=harness report=evals/harness-conformance/plane/deterministic/latest
## desc: Provider-free checks of the plane-door evidence: SSE framing, elicitation answers, scripted-client and CLI grading; no running service needed
test-plane-harness-eval:
	@python3 scripts/eval-harness-conformance-live.py --lane plane --self-test \
		--output-dir '$(COVERAGE_BASE_DIR)/evals/harness-conformance/plane/deterministic/latest'

.PHONY: test-plane-harness-live
## eval: kind=live requires=magician,provider_keys report=evals/harness-conformance/plane/live/latest
## desc: External MCP clients on the plane door: a scripted client and the installed CLIs launch a run through a plt_ grant, answer its elicitation, and are refused across sessions, on replay and after revoke
test-plane-harness-live:
	@$(MAKE) test-harness-conformance-live-eval HARNESS_CONFORMANCE_LANE=plane \
		HARNESS_CONFORMANCE_LIVE_OUTPUT_DIR='$(COVERAGE_BASE_DIR)/evals/harness-conformance/plane/live/latest'

.PHONY: test-voice-harness-eval
## eval: kind=harness report=evals/harness-conformance/voice/deterministic/latest
## desc: Provider-free checks of the voice-delegation evidence: spoken nonce, drivers, frame folding, grading, delegation rate; no running service needed
test-voice-harness-eval:
	@python3 scripts/eval-harness-conformance-live.py --lane voice --self-test \
		--output-dir '$(COVERAGE_BASE_DIR)/evals/harness-conformance/voice/deterministic/latest'

.PHONY: test-voice-harness-live
## eval: kind=live requires=magician,provider_keys report=evals/harness-conformance/voice/live/latest
## desc: Drive GPT Realtime by text and GPT Live 1 by speech, N runs per profile, and report the delegation rate with the delegated runs graded on the done frame, the voice call row, the hands on the turn and the task
test-voice-harness-live:
	@$(MAKE) test-harness-conformance-live-eval HARNESS_CONFORMANCE_LANE=voice \
		HARNESS_CONFORMANCE_LIVE_OUTPUT_DIR='$(COVERAGE_BASE_DIR)/evals/harness-conformance/voice/live/latest'

.PHONY: test-app-encryption-recovery
test-app-encryption-recovery:
	@cargo test -p magician --lib magician_v2::apps::registry::encryption_recovery::tests

app-platform-private-check:
	@bash scripts/check-app-platform-private-scale.sh

qualify-app-custom-surface-host:
	@bash scripts/qualify-app-custom-surface-host.sh

test-app-platform-p7-qualification:
	@bash scripts/test-app-platform-p7-qualification.sh

app-platform-processing-status:
	@bash scripts/app-platform-processing-profile.sh status

app-platform-processing-local:
	@bash scripts/app-platform-processing-profile.sh local

app-platform-processing-remote:
	@bash scripts/app-platform-processing-profile.sh remote

.PHONY: app-contract-codegen app-contract-check test-app-contract-clients test-app-typescript-sdk test-app-typescript-consumer-canary test-app-typescript-real-server-canary test-app-authoring app-platform-private-check qualify-app-custom-surface-host test-app-platform-p7-qualification app-platform-processing-status app-platform-processing-local app-platform-processing-remote

# The device-bridge wire format is written in Rust twice and Kotlin once, and a
# mismatch is silent: the phone connects, cannot parse the work it is sent, and
# answers nothing — which looks exactly like an idle device.
check-device-bridge-protocol:
	@python3 scripts/check_device_bridge_protocol_drift.py

.PHONY: check-device-bridge-protocol

# Android automation companion. There is no Android SDK in this workspace's
# toolchain, so these skip rather than fail when it is absent — the same shape as
# the iOS targets do for xcodebuild. A missing SDK is not a broken checkout.
# One-time toolchain setup: JDK 21, the Android SDK, and the wrapper jar that
# upstream never committed. Safe to re-run.
setup-magdroid-build:
	@bash scripts/setup-magdroid-build.sh

.PHONY: setup-magdroid-build

# Gradle 8.13 rejects Java 24+, and this workspace's default JDK is newer, so
# both targets pin JAVA_HOME rather than inheriting whatever `java` resolves to.
MAGDROID_JAVA_HOME ?= $(shell brew --prefix 2>/dev/null)/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home
MAGDROID_SDK ?= $(or $(ANDROID_SDK_ROOT),$(ANDROID_HOME),$(HOME)/Library/Android/sdk)
# The wrapper is preferred, but its bootstrap fetches the distribution over a
# redirect that fails in some sandboxes where curl succeeds. A pinned Gradle 8
# keg is used when present: AGP 8.x rejects Gradle 9, so the plain `gradle` on
# PATH is not a safe fallback.
MAGDROID_GRADLE ?= $(firstword $(wildcard $(shell brew --prefix 2>/dev/null)/opt/gradle@8/bin/gradle) ./gradlew)

build-magdroid:
	@if [ ! -d '$(MAGDROID_JAVA_HOME)' ] || [ ! -d '$(MAGDROID_SDK)' ]; then \
		echo "Skipping magdroid build: run 'make setup-magdroid-build' first (needs JDK 21 and the Android SDK)."; \
	else \
		cd magdroid/android && JAVA_HOME='$(MAGDROID_JAVA_HOME)' ANDROID_SDK_ROOT='$(MAGDROID_SDK)' '$(MAGDROID_GRADLE)' assembleDebug; \
	fi

.PHONY: build-magdroid

test-magdroid:
	@if [ ! -d '$(MAGDROID_JAVA_HOME)' ] || [ ! -d '$(MAGDROID_SDK)' ]; then \
		echo "Skipping magdroid tests: run 'make setup-magdroid-build' first."; \
	else \
		cd magdroid/android && JAVA_HOME='$(MAGDROID_JAVA_HOME)' ANDROID_SDK_ROOT='$(MAGDROID_SDK)' '$(MAGDROID_GRADLE)' test; \
	fi

.PHONY: test-magdroid

# One gate to run before committing Android work, so "it builds" and "the tests
# pass" are not two things anyone has to remember separately.
check-magdroid: build-magdroid test-magdroid

.PHONY: check-magdroid

# Device-side targets. Each builds first: installing a stale APK and then
# hunting for a change that was never compiled is a specific and expensive
# waste of an afternoon.
install-magdroid: build-magdroid
	@bash scripts/magdroid-device.sh install

run-magdroid: build-magdroid
	@bash scripts/magdroid-device.sh run

# No build: this attaches to whatever is already running.
logs-magdroid:
	@bash scripts/magdroid-device.sh logs

# `make screenshot-magdroid OUT=before.png`
screenshot-magdroid:
	@bash scripts/magdroid-device.sh screenshot '$(or $(OUT),magdroid-screen.png)'

.PHONY: install-magdroid run-magdroid logs-magdroid screenshot-magdroid

# The rank-recompute result-semantics vocabulary lives in three languages and a
# mismatch fails silently: the web parser returns null for a label it does not
# know, so the page renders with every attributed result missing.
check-rank-recompute-semantics:
	@python3 scripts/check_rank_recompute_semantics_drift.py

.PHONY: check-rank-recompute-semantics

# Selection capture spans Rust, Svelte and extension JS. Guard the shared action
# id, the extension's bearer-only request boundary, and the search result cap.
check-notes-capture-surface:
	@python3 scripts/check_notes_capture_surface_drift.py

.PHONY: check-notes-capture-surface

# Ratchet adoption of the durable-write and tolerant-read helpers this repo
# already has. Counts known-bad call sites per file and fails when the number
# moves in either direction — up means a new hand-rolled write, down means a
# store was fixed and the baseline owes an update.
check-store-durability:
	@python3 scripts/check_store_durability_adoption.py

.PHONY: check-store-durability

# Canonical storage catalog: YAML is the source of truth; the /storage
# inventory is a projection. Drift between catalog governance_id values and
# the live/test-fixtures Rust snapshots fails, as does an unlisted on-disk
# Connection::open.
check-storage-catalog:
	@python3 scripts/storage_catalog_guard.py
	@python3 -m unittest scripts/test_storage_catalog_guard.py

.PHONY: check-storage-catalog

# Task 21: new runtime-root I/O, Parquet globs, object SDKs, and ambient
# backend construction fail unless they sit in a reviewed adapter, owner kit,
# backup/export path, or the magician-bin composition root. Exact allowlist
# files are two-way. Connection::open stays on check-storage-catalog.
check-typed-storage-boundaries:
	@python3 scripts/check_typed_storage_boundaries.py
	@python3 -m unittest scripts/test_typed_storage_boundaries.py

.PHONY: check-typed-storage-boundaries

# Disposable Gate 1 spike. Synthetic fixtures only; never opens user data.
test-storage-gate1:
	cargo test -p magician-storage-gate1

.PHONY: test-storage-gate1

test-storage-s3:
	cargo test -p magician-storage-s3

.PHONY: test-storage-s3

test-storage-state:
	cargo test -p magician-storage-state

.PHONY: test-storage-state

test-storage-migration:
	cargo test -p magician-storage-migration

.PHONY: test-storage-migration

# Non-blocking reminder over the working tree. The blocking gate remains
# `python3 scripts/docs_guard.py --staged` from `.githooks/pre-commit`.
docs-remind:
	@python3 scripts/docs_guard.py --working-tree --remind-only --source make

.PHONY: docs-remind

# Per-crate changelogs are append-only and nothing ever removed from them, so
# left alone they reach tens of thousands of lines, bury the recent entries, and
# ship that bulk to the public mirror. `check-changelogs` fails when a file is
# over the retention limit; `trim-changelogs` moves the older entries into
# docs/archive/changelogs/ (never deleted, just no longer published) and repairs
# the relative links that archiving would otherwise rot. The repository-root
# CHANGELOG.md is the curated product release log and is deliberately skipped.

check-changelogs:
	@python3 scripts/trim_changelogs.py --check

trim-changelogs:
	@python3 scripts/trim_changelogs.py --apply

.PHONY: check-changelogs trim-changelogs

# docs_guard enforces that a doc changed alongside the code it owns; nothing
# enforced that its links still point anywhere. Ratchets dangling relative links
# per file, so moving a doc cannot silently leave 404s behind it. Case counts:
# a link that differs only in case works on macOS and breaks on Linux.
check-links:
	@python3 scripts/check_markdown_links.py
	@python3 -m unittest scripts/test_check_markdown_links.py

.PHONY: check-links

# Proves the background-job pair still reports an outcome however a job dies.
# Builds here run 5-16 minutes, so they get backgrounded and waited on, and a
# waiter that can hang costs an hour before anyone looks. The cases that matter
# are the kills: SIGTERM (the producer's trap must still record it) and SIGKILL
# (uncatchable, so the waiter must detect the death itself). Both are
# regressions from real incidents, not hypotheticals.
check-bg-run:
	@sh scripts/test_bg_run.sh

.PHONY: check-bg-run

# Fast, deterministic, offline proof that every Phase 5G qualification gate remains
# mapped to exact-pinned official tooling and named executable coverage.
check-phase5g-conformance: setup-extracted-libraries
	@python3 scripts/check_phase5g_conformance.py

# Phase 4 remains dormant, but its complete lifecycle contract and product process-owner
# boundary have a named offline regression lane.
test-phase4-local:
	$(MAKE) test-magicrun-lifecycle
	cargo test -p magician credential_lifecycle_executor::tests

# Local qualification owns Magician-specific governance; protocol/OAuth mechanics stay
# in the official SDK. The official lane is deliberately separate so its network clone
# and SDK build are visible rather than being hidden inside an ordinary package test.
test-phase5g-local: check-phase5g-conformance
	cargo test -p magician-mcp-client
	$(MAKE) test-magicrun
	cargo test -p magician --test phase7_migration --test tool_skill_contract
	cargo test -p magician credential_material_adapter::tests
	cargo test -p magician mcp_oauth_vault::tests
	cargo test -p magician mcp_oauth_api::tests

setup-mcp-official-conformance:
	@cd scripts/mcp-conformance && npm ci --ignore-scripts

test-mcp-official-conformance: setup-mcp-official-conformance check-phase5g-conformance
	@bash scripts/mcp-conformance/run-official-client-conformance.sh

test-phase5g: test-phase5g-local test-mcp-official-conformance

.PHONY: check-phase5g-conformance test-phase4-local test-phase5g-local setup-mcp-official-conformance test-mcp-official-conformance test-phase5g

# Run cargo check on magician only
check-magician:
	cargo check -p magician --all-targets

# Tightest inner-loop check: just the magician *binary* (lib + bin), skipping
# the thousands of test targets that `--all-targets` also type-checks. Use this
# during rapid edit/compile cycles; run `check-magician` (or `check-all`) before
# committing to also catch test-only breakage.
.PHONY: check-magician-fast
check-magician-fast:
	cargo check -p magician-bin --bin magician

# --- Experimental: Cranelift dev codegen backend ----------------------------
# Cranelift compiles (unoptimized) debug code roughly 2-3x faster than LLVM by
# skipping LLVM's optimization pipeline. Dev/iteration ONLY — never release; the
# binary is slower at runtime. Needs the nightly toolchain + the cg_clif
# component (both already installed here; `setup-cranelift` (re)installs them).
#
# It builds into a SEPARATE target dir (…/cranelift) on purpose: cranelift and
# LLVM object files fingerprint differently, so sharing the main dir would
# thrash the normal cache. sccache is disabled for the same reason (it won't
# cache cranelift artifacts cleanly). If a dependency fails to build under
# cranelift, fall back to `build-magician-dev`.
.PHONY: setup-fast setup-cranelift build-magician-cranelift watch-magician
# Provision the nightly toolchain that FAST=1 uses. Run ONCE before `make …
# FAST=1`. FAST exports RUSTUP_TOOLCHAIN=nightly, so every sub-target — including
# the coverage/test-report gate (verify-rust-test-report-deps) — resolves against
# nightly; llvm-tools-preview must therefore exist on nightly (it's also the
# correct match for coverage of nightly-compiled binaries). NOTE: coverage lanes
# are incompatible with FAST_CRANELIFT=1 (Cranelift can't emit instrumentation) —
# use the stable toolchain (no FAST) for authoritative coverage numbers.
setup-fast:
	rustup toolchain install nightly --profile minimal 2>/dev/null || true
	rustup component add llvm-tools-preview --toolchain nightly

setup-cranelift: setup-fast
	rustup component add rustc-codegen-cranelift-preview --toolchain nightly

# Cranelift builds into its OWN per-worktree dir (a sibling of the normal target
# dir). Two reasons it must not share: cranelift and LLVM objects fingerprint
# differently (sharing thrashes both caches), and — like the main dir — two
# worktrees sharing it would thrash each other. Deriving from $(CARGO_TARGET_DIR)
# inherits the per-worktree isolation automatically.
CRANELIFT_TARGET_DIR ?= $(CARGO_TARGET_DIR)-cranelift
build-magician-cranelift:
	CARGO_TARGET_DIR=$(CRANELIFT_TARGET_DIR) \
	  RUSTC_WRAPPER="$(wildcard $(HOME)/.cargo/rustc-job-gate)" \
	  RUSTC_JOB_GATE_NEXT= \
	  RUSTFLAGS="-Zcodegen-backend=cranelift" \
	  cargo +nightly build --bin magician
	@echo "🦀 cranelift binary: $(CRANELIFT_TARGET_DIR)/debug/magician"

# Tightest inner loop: recompile just the magician binary on every save. Uses
# cargo-watch (stable CLI, honours the exported per-worktree CARGO_TARGET_DIR).
# Run an occasional `make check-magician` before committing to also catch
# test-only breakage that `--bin magician` skips.
watch-magician:
	@if command -v cargo-watch >/dev/null 2>&1; then \
	  cargo watch -x 'check -p magician --bin magician'; \
	else \
	  echo "Needs a file-watcher:  cargo install cargo-watch   (bacon also works, but wants a bacon.toml to scope to one bin)"; \
	  echo "Then re-run: make watch-magician"; \
	  exit 1; \
	fi

# Run cargo check on magic-supervisor only
check-magic-supervisor:
	cargo check -p magic-supervisor

# Ollama single-chokepoint drift gate — fast (~10ms; pure text scan, no cargo,
# no ollama). Fails if any hand-built `/api/generate`, `/api/chat`, or
# `/api/embed` POST appears outside the allowlist (the OllamaProvider itself,
# the documented prewarm exception, and test code). Scans every workspace crate
# with Rust sources. Enforces
# docs/plans/2026-07-14-ollama-single-chokepoint.md and the embedding closure in
# docs/archive/plans/2026-08-24-llm-chokepoint-closure.md. Wired into `check-all` as a
# prerequisite so a future bypass is a red build, not a silent regression.
check-ollama-chokepoint:
	@python3 scripts/check_ollama_single_chokepoint.py

.PHONY: check-ollama-chokepoint

sccache-stats:
	@if command -v sccache >/dev/null 2>&1; then \
		sccache --show-stats; \
	else \
		echo "sccache not found on PATH"; \
		exit 1; \
	fi

sccache-clear:
	@if command -v sccache >/dev/null 2>&1; then \
		sccache --clear-cache; \
	else \
		echo "sccache not found on PATH"; \
		exit 1; \
	fi

# The incremental cache is shared by every agent and worktree on this volume,
# and it does not bound itself: it reached 368 GB once and 94 GB again, and a
# full volume makes every build fail with "No space left on device", which
# reads like a compile error. Turning incremental off would cost every debug
# rebuild of an 850k-LOC crate, so prune instead — oldest first, and never a
# cache another process has open.
INCREMENTAL_BUDGET_GB ?= 40
INCREMENTAL_MIN_FREE_GB ?= 60

prune-incremental:
	@if [ -n "$(WORKSPACE_ARTIFACTS)" ]; then \
		scripts/prune-incremental.sh --workspace-artifacts $(if $(DRY_RUN),--dry-run,); \
	elif [ -n "$(CRATE)" ]; then \
		scripts/prune-incremental.sh --crate "$(CRATE)" $(if $(DRY_RUN),--dry-run,); \
	else \
		scripts/prune-incremental.sh --budget-gb "$(INCREMENTAL_BUDGET_GB)" $(if $(DRY_RUN),--dry-run,); \
	fi

# Fail with the actual reason instead of letting cargo die of ENOSPC halfway
# through a link. Advisory: it never deletes anything on its own, because the
# caches it would take belong to whoever builds next.
check-build-space:
	@free_gb=$$(df -g "$(CARGO_TARGET_DIR)" 2>/dev/null | awk 'NR==2 {print $$4}'); \
	if [ -z "$$free_gb" ]; then \
		echo "could not read free space for $(CARGO_TARGET_DIR)"; \
		exit 0; \
	fi; \
	echo "build volume: $$free_gb GB free at $(CARGO_TARGET_DIR)"; \
	if [ "$$free_gb" -lt "$(INCREMENTAL_MIN_FREE_GB)" ]; then \
		echo ""; \
		echo "Below INCREMENTAL_MIN_FREE_GB=$(INCREMENTAL_MIN_FREE_GB) GB. A build that runs out here"; \
		echo "fails with 'No space left on device', which reads like a compile error."; \
		echo "Run: make prune-incremental        (add DRY_RUN=1 to see it first)"; \
		echo "Or:  make prune-incremental WORKSPACE_ARTIFACTS=1   — the stale generations of the"; \
		echo "     workspace crates under deps/, which is where a full volume usually went"; \
		exit 1; \
	fi

# Format code (workspace + the two crates outside it)
.PHONY: fmt-desktop fmt-desktop-check
fmt-desktop:
	cargo fmt --manifest-path desktop/src-tauri/Cargo.toml

fmt-desktop-check:
	cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check

fmt:
	cargo fmt --all
	cargo fmt --manifest-path desktop/src-tauri/Cargo.toml
	cargo fmt --manifest-path kindle/Cargo.toml

# Check formatting (what CI runs)
fmt-check:
	cargo fmt --all -- --check
	cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check
	cargo fmt --manifest-path kindle/Cargo.toml -- --check

# Run clippy
clippy:
	cargo clippy -- -D warnings

# Run clippy with all features
clippy-all:
	cargo clippy --all-features -- -D warnings

# Clean Rust build artifacts. For skill-related cleanup (node_modules,
# .venv, runtime install tree), use `make clean-skills` or
# `make -C skillshub clean[-build|-runtime|-all]`.
clean:
	cargo clean

# Skillshub cleanup delegations.
# - clean-skills          → skillshub clean (build artifacts: node_modules, .venv, dist/, bin/, .pp-build/)
# - clean-skills-runtime  → skillshub clean-runtime (system/skills/, scope skills/bots; preserves config/auth/.env)
# - clean-skills-all      → both
# - clean-all-artifacts   → cargo clean + skillshub clean-all (full reset; preserves config/auth/.env)
.PHONY: clean-skills clean-skills-runtime clean-skills-all clean-all-artifacts

clean-skills:
	@$(MAKE) -C skillshub clean

clean-skills-runtime:
	@$(MAKE) -C skillshub clean-runtime

clean-skills-all:
	@$(MAKE) -C skillshub clean-all

clean-all-artifacts: clean clean-skills-all
	@echo "  ✓ all build + runtime artifacts cleaned"
	@echo "    Preserved: scope-level config/, auth/, workdirs/ + scope .env files"

# Generate every code-graph artifact in one pass:
#   1. graph.json + stats.json + payload_profiles.json (structural index)
#   2. contracts.json (validation rules, kind-gating, constants, type shapes)
#   3. dead_code_candidates.md (functions with no production callers)
#   4. test_coverage.md (per-fn test-call buckets)
#   5. final coverage audit (filesystem ↔ graph diff)
# All generated artifacts are listed in scripts/codegraph_exclude.txt
# so subsequent runs don't re-index them as repo content.
# Indexing walks the whole repository and takes ~4 minutes, and both
# `build-all-debug` and `build-all-release` call it. The stamp records a
# fingerprint of everything the generator could read (HEAD, the content of
# every tracked modification, and any non-ignored untracked file), so a
# rebuild with no source change skips it. The check costs ~80ms.
# `make graph-index FORCE=1` reindexes regardless.
CODEGRAPH_STAMP ?= docs/codegraph/.graph-index.stamp
CODEGRAPH_OUTPUTS := docs/codegraph/graph.json docs/codegraph/stats.json \
	docs/codegraph/payload_profiles.json docs/codegraph/contracts.json \
	docs/codegraph/dead_code_candidates.md docs/codegraph/test_coverage.md \
	docs/codegraph/c4.json

graph-index:
	@if [ -z "$(FORCE)" ] && python3 scripts/graph_index_stamp.py check \
		--stamp $(CODEGRAPH_STAMP) --output $(CODEGRAPH_OUTPUTS); then \
		echo "  ✓ codegraph up to date — no source change since the last index (FORCE=1 to reindex)"; \
	else \
		$(MAKE) --no-print-directory graph-index-force; \
	fi

# The indexing itself. Depend on this directly to reindex unconditionally.
graph-index-force:
	python3 scripts/generate_code_graph.py --output docs/codegraph/graph.json --stats docs/codegraph/stats.json --payload docs/codegraph/payload_profiles.json
	python3 scripts/extract_contracts.py --out docs/codegraph/contracts.json
	@python3 scripts/find_dead_code.py --limit 5000 > docs/codegraph/dead_code_candidates.md
	@echo "  ✓ docs/codegraph/dead_code_candidates.md"
	@python3 scripts/test_coverage.py --limit 5000 > docs/codegraph/test_coverage.md
	@echo "  ✓ docs/codegraph/test_coverage.md"
	@python3 scripts/audit_code_graph.py | tail -3
	python3 scripts/generate_c4_index.py
	@python3 scripts/graph_index_stamp.py write --stamp $(CODEGRAPH_STAMP)

# Extract behavioral contracts only (subset of `graph-index`).
graph-contracts:
	python3 scripts/extract_contracts.py --out docs/codegraph/contracts.json

# Alias for graph-index (back-compat).
graph-all: graph-index

# Validate generated code graph artifact schema
graph-check:
	python3 scripts/generate_code_graph.py --validate docs/codegraph/graph.json
	python3 scripts/generate_c4_index.py --strict

# Audit graph coverage: re-walks the repo with the same pruning rules
# and diffs filesystem ground-truth against file nodes in graph.json.
# Surfaces files-on-disk-that-should-be-indexed-but-aren't (gaps) and
# files-in-graph-no-longer-on-disk (stale). Per-extension breakdown.
graph-audit:
	python3 scripts/audit_code_graph.py

# Dead-code candidates: production functions with no production callers.
# Test callers (structural — #[test], *.test.ts, test_*.py, *Tests.swift)
# are tracked separately and surfaced as `tcalls=N` info, not used to
# keep a function alive. Saves a full report to docs/codegraph/.
graph-dead-code:
	@python3 scripts/find_dead_code.py --limit 5000 > docs/codegraph/dead_code_candidates.md
	@echo "Saved docs/codegraph/dead_code_candidates.md"
	@python3 scripts/find_dead_code.py --limit 0 | head -16

# Structural test-coverage report — buckets every production function
# by how many `test_calls` edges point to it. Surfaces production code
# tests never exercise. Saves full report to docs/codegraph/.
graph-coverage:
	@python3 scripts/test_coverage.py --limit 5000 > docs/codegraph/test_coverage.md
	@echo "Saved docs/codegraph/test_coverage.md"
	@python3 scripts/test_coverage.py --limit 0 | head -10

# Serve docs/codegraph explorer + dev API. Instant — assumes the graph
# artifacts already exist (run `make graph-index` to regenerate them).
graph-serve:
	@lsof -ti:8077 | xargs kill -9 2>/dev/null || true
	python3 docs/codegraph/dev_server.py --host localhost --port 8077

# Start codegraph explorer in background (used by run-all). Instant —
# assumes graph artifacts already exist (run `make graph-index` to
# regenerate them).
graph-serve-bg:
	@lsof -ti:8077 | xargs kill -9 2>/dev/null || true
	@echo "🔍 Starting codegraph explorer on http://localhost:8077..."
	@python3 docs/codegraph/dev_server.py --host localhost --port 8077 &

# Stop any codegraph explorer instance listening on :8077.
graph-serve-stop:
	@PIDS=$$(lsof -ti:8077 2>/dev/null); \
	if [ -n "$$PIDS" ]; then \
		echo "⏹️  Stopping codegraph explorer (PIDs: $$PIDS)..."; \
		kill -TERM $$PIDS 2>/dev/null || true; \
		sleep 1; \
		REMAINING=$$(lsof -ti:8077 2>/dev/null); \
		if [ -n "$$REMAINING" ]; then kill -9 $$REMAINING 2>/dev/null || true; fi; \
		echo "✅ Stopped."; \
	else \
		echo "ℹ️  Nothing listening on :8077."; \
	fi

# Restart the codegraph explorer in the background. Picks up edits to
# dev_server.py, mcp_server.py, codegraph_ext/* without leaving the
# shell tied up.
graph-serve-restart: graph-serve-stop
	@echo "🔄 Restarting codegraph explorer on http://localhost:8077..."
	@nohup python3 docs/codegraph/dev_server.py --host localhost --port 8077 \
		> /tmp/codegraph_dev_server.log 2>&1 &
	@sleep 1
	@if lsof -ti:8077 >/dev/null 2>&1; then \
		echo "✅ Up — logs: /tmp/codegraph_dev_server.log"; \
	else \
		echo "❌ Failed to bind :8077 — check /tmp/codegraph_dev_server.log"; \
		exit 1; \
	fi

# Run the proxy (commented out)
# run-release-wo-config:
# 	cargo run --bin magictunnel --release

# Run in development mode (commented out)
# run-dev-wo-config:
# 	cargo run --bin magictunnel -- --log-level debug

# Run in development mode (commented out)
# run-dev:
# 	cargo run --bin magictunnel -- --log-level debug --config magictunnel/magictunnel-config.yaml

# Run with custom config (commented out)
# run-release:
# 	MAGICTUNNEL_ENV=development \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info

# Run with OpenAI API key for smart discovery (commented out)
# run-release-openai:
# 	@if [ -z "$(OPENAI_API_KEY)" ]; then \
# 		echo "Error: OPENAI_API_KEY environment variable is not set"; \
# 		echo "Usage: make run-release-openai OPENAI_API_KEY=sk-your-key-here"; \
# 		exit 1; \
# 	fi
# 	OPENAI_API_KEY="$(OPENAI_API_KEY)" \
# 	cargo run --bin magictunnel -- --config magictunnel/magictunnel-config.yaml --log-level info

# Run with semantic search environment variables override (OpenAI) (commented out)
# run-release-semantic:
# 	@if [ -z "$(OPENAI_API_KEY)" ]; then \
# 		echo "Error: OPENAI_API_KEY environment variable is not set for OpenAI embeddings"; \
# 		echo "Usage: make run-release-semantic OPENAI_API_KEY=sk-your-key-here"; \
# 		exit 1; \
# 	fi
# 	@echo "🧠 Starting MagicTunnel with OpenAI semantic search..."
# 	@echo "   - Model: openai:text-embedding-3-small"
# 	@echo "   - Semantic search: enabled"
# 	OPENAI_API_KEY="$(OPENAI_API_KEY)" \
# 	MAGICTUNNEL_SEMANTIC_MODEL="openai:text-embedding-3-small" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info

# Run with local transformer models (fallback embeddings - limited functionality) (commented out)
# run-release-local:
# 	@echo "⚠️  Starting MagicTunnel with fallback embeddings (LIMITED FUNCTIONALITY)..."
# 	@echo "   - Model: all-MiniLM-L6-v2 (384-dim, hash-based fallback)"
# 	@echo "   - WARNING: Uses deterministic fallback, not real semantic embeddings"
# 	@echo "   - RECOMMENDED: Use 'make run-release-ollama' instead for real embeddings"
# 	MAGICTUNNEL_SEMANTIC_MODEL="all-MiniLM-L6-v2" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info

# Run with high-quality local model (fallback embeddings - limited functionality) (commented out)
# run-release-hq:
# 	@echo "⚠️  Starting MagicTunnel with fallback embeddings (LIMITED FUNCTIONALITY)..."
# 	@echo "   - Model: all-mpnet-base-v2 (768-dim, hash-based fallback)"
# 	@echo "   - WARNING: Uses deterministic fallback, not real semantic embeddings"
# 	@echo "   - RECOMMENDED: Use 'make run-release-ollama' instead for real embeddings"
# 	MAGICTUNNEL_SEMANTIC_MODEL="all-mpnet-base-v2" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info

# Run with Ollama (local LLM server) (commented out)
# run-release-ollama:
# 	@echo "🧠 Starting MagicTunnel with Ollama..."
# 	@echo "   - Model: Uses your local Ollama server"
# 	@echo "   - Make sure Ollama is running with an embedding model!"
# 	OLLAMA_BASE_URL="http://localhost:11434" \
# 	MAGICTUNNEL_SEMANTIC_MODEL="ollama:nomic-embed-text" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info

# Run with custom external API (commented out)
# run-release-external:
# 	@if [ -z "$(EMBEDDING_API_URL)" ]; then \
# 		echo "Error: EMBEDDING_API_URL environment variable is not set"; \
# 		echo "Usage: make run-release-external EMBEDDING_API_URL=http://your-server:8080"; \
# 		exit 1; \
# 	fi
# 	@echo "🧠 Starting MagicTunnel with external embedding API..."
# 	@echo "   - API URL: $(EMBEDDING_API_URL)"
# 	@echo "   - Custom embedding service"
# 	EMBEDDING_API_URL="$(EMBEDDING_API_URL)" \
# 	MAGICTUNNEL_SEMANTIC_MODEL="external:api" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info

# Pre-generate embeddings for all enabled capabilities (commented out)
# pregenerate-embeddings:
# 	@if [ -z "$(OPENAI_API_KEY)" ]; then \
# 		echo "Error: OPENAI_API_KEY environment variable is not set for embedding generation"; \
# 		echo "Usage: make pregenerate-embeddings OPENAI_API_KEY=sk-your-key-here"; \
# 		exit 1; \
# 	fi
# 	@echo "🧠 Pre-generating embeddings for all enabled capabilities..."
# 	@echo "   - Model: configured in magictunnel/magictunnel-config.yaml"
# 	@echo "   - This will make server startup much faster!"
# 	OPENAI_API_KEY="$(OPENAI_API_KEY)" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info --pregenerate-embeddings

# Pre-generate embeddings with OpenAI model override (commented out)
# pregenerate-embeddings-openai:
# 	@if [ -z "$(OPENAI_API_KEY)" ]; then \
# 		echo "Error: OPENAI_API_KEY environment variable is not set for OpenAI embeddings"; \
# 		echo "Usage: make pregenerate-embeddings-openai OPENAI_API_KEY=sk-your-key-here"; \
# 		exit 1; \
# 	fi
# 	@echo "🧠 Pre-generating embeddings with OpenAI model override..."
# 	@echo "   - Model: openai:text-embedding-3-small (environment override)"
# 	@echo "   - This will make server startup much faster!"
# 	OPENAI_API_KEY="$(OPENAI_API_KEY)" \
# 	MAGICTUNNEL_SEMANTIC_MODEL="openai:text-embedding-3-small" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info --pregenerate-embeddings

# Pre-generate embeddings with fallback models (limited functionality) (commented out)
# pregenerate-embeddings-local:
# 	@echo "⚠️  Pre-generating fallback embeddings (LIMITED FUNCTIONALITY)..."
# 	@echo "   - Model: all-MiniLM-L6-v2 (384-dim, hash-based fallback)"
# 	@echo "   - WARNING: Uses deterministic fallback, not real semantic embeddings"
# 	@echo "   - RECOMMENDED: Use 'make pregenerate-embeddings-ollama' instead"
# 	MAGICTUNNEL_SEMANTIC_MODEL="all-MiniLM-L6-v2" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info --pregenerate-embeddings

# Pre-generate embeddings with fallback models (limited functionality) (commented out)
# pregenerate-embeddings-hq:
# 	@echo "⚠️  Pre-generating fallback embeddings (LIMITED FUNCTIONALITY)..."
# 	@echo "   - Model: all-mpnet-base-v2 (768-dim, hash-based fallback)"
# 	@echo "   - WARNING: Uses deterministic fallback, not real semantic embeddings"
# 	@echo "   - RECOMMENDED: Use 'make pregenerate-embeddings-ollama' instead"
# 	MAGICTUNNEL_SEMANTIC_MODEL="all-mpnet-base-v2" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info --pregenerate-embeddings

# Pre-generate embeddings with Ollama (local LLM server) - disabled (magictunnel removed)
# pregenerate-embeddings-ollama:
# 	OLLAMA_BASE_URL="http://localhost:11434" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info --pregenerate-embeddings

# Pre-generate embeddings with custom external API (commented out)
# pregenerate-embeddings-external:
# 	@if [ -z "$(EMBEDDING_API_URL)" ]; then \
# 		echo "Error: EMBEDDING_API_URL environment variable is not set"; \
# 		echo "Usage: make pregenerate-embeddings-external EMBEDDING_API_URL=http://your-server:8080"; \
# 		exit 1; \
# 	fi
# 	@echo "🧠 Pre-generating embeddings with external API..."
# 	@echo "   - API URL: $(EMBEDDING_API_URL)"
# 	@echo "   - Custom embedding service"
# 	EMBEDDING_API_URL="$(EMBEDDING_API_URL)" \
# 	MAGICTUNNEL_SEMANTIC_MODEL="external:api" \
# 	MAGICTUNNEL_DISABLE_SEMANTIC="false" \
# 	cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info --pregenerate-embeddings


# Run in release mode with .env file support (commented out)
# run-release-env:
# 	@if [ ! -f .env ] && [ ! -f .env.development ] && [ ! -f .env.production ]; then \
# 		echo "🚨 No .env files found!"; \
# 		echo ""; \
# 		echo "📝 To get started:"; \
# 		echo "  1. Copy the example: cp .env.example .env"; \
# 		echo "  2. Edit .env and set your OPENAI_API_KEY"; \
# 		echo "  3. Run: make run-release-env"; \
# 		echo ""; \
# 		echo "Or use environment-specific files:"; \
# 		echo "  make run-release-env ENV=production"; \
# 		echo "  make run-release-env ENV=staging"; \
# 		echo ""; \
# 		exit 1; \
# 	fi
# 	@echo "🚀 Starting MagicTunnel (Release) with .env configuration..."
# 	@echo "   - Building in release mode..."
# 	$(if $(ENV),MAGICTUNNEL_ENV=$(ENV)) cargo run --bin magictunnel --release -- --config magictunnel/magictunnel-config.yaml --log-level info

# Run in development mode with .env file support (commented out)
# run-dev-env:
# 	@if [ ! -f .env ] && [ ! -f .env.development ] && [ ! -f .env.production ]; then \
# 		echo "🚨 No .env files found!"; \
# 		echo ""; \
# 		echo "📝 To get started:"; \
# 		echo "  1. Copy the example: cp .env.example .env"; \
# 		echo "  2. Edit .env and set your OPENAI_API_KEY"; \
# 		echo "  3. Run: make run-dev-env"; \
# 		echo ""; \
# 		echo "Or use environment-specific files:"; \
# 		echo "  make run-dev-env ENV=development"; \
# 		echo "  make run-dev-env ENV=staging"; \
# 		echo ""; \
# 		exit 1; \
# 	fi
# 	@echo "🚀 Starting MagicTunnel with .env configuration..."
# 	$(if $(ENV),MAGICTUNNEL_ENV=$(ENV)) cargo run --bin magictunnel -- --config magictunnel/magictunnel-config.yaml --log-level debug

# Set up .env file for development
setup-env:
	@if [ -f .env ]; then \
		echo "✅ .env file already exists"; \
	else \
		echo "📝 Creating .env file from example..."; \
		cp .env.example .env; \
		echo "✅ .env file created!"; \
		echo ""; \
		echo "⚠️  IMPORTANT: Edit .env file and set your OpenAI API key:"; \
		echo "   OPENAI_API_KEY=sk-your-actual-openai-key-here"; \
		echo ""; \
		echo "🚀 Then run: make run-dev-env"; \
	fi

# Install development tools
install-tools:
	rustup component add rustfmt clippy

# Full development check (format, clippy, test, check)
dev-check: fmt-check clippy test check-all
	@echo "✅ All development checks passed!"

# Watch for changes and run tests
watch-test:
	cargo watch -x test

# Watch for changes and run the proxy (commented out)
# watch-run:
# 	cargo watch -x "run -- --log-level debug"

# Run all Rust tests with structured nextest results, the workspace's doctests,
# LLVM source coverage, retained raw artifacts, and a stable clickable report.
# Runnable test targets self-heal deterministic tooling and agent-browser
# prerequisites. The verify-* targets remain available as read-only gates.
test-rust: event-taxonomy-check component-graph-check setup-protoc setup-ffmpeg setup-rust-test-report-deps setup-agent-browser test-extracted-libraries
	@$(MAKE) -C skillshub verify-agent-browser
	@RUST_TEST_REPORT_DIR='$(RUST_TEST_REPORT_DIR)' \
		RUST_TEST_THREADS='$(RUST_TEST_THREADS)' TEST_FD_LIMIT='$(TEST_FD_LIMIT)' \
		./scripts/run-rust-tests-with-report.sh $(COVERAGE_FLAG)

test-rust-verbose: event-taxonomy-check component-graph-check setup-protoc setup-ffmpeg setup-rust-test-report-deps setup-agent-browser test-extracted-libraries
	@$(MAKE) -C skillshub verify-agent-browser
	@RUST_TEST_REPORT_DIR='$(RUST_TEST_REPORT_DIR)' \
		RUST_TEST_THREADS='$(RUST_TEST_THREADS)' TEST_FD_LIMIT='$(TEST_FD_LIMIT)' \
		./scripts/run-rust-tests-with-report.sh --verbose $(COVERAGE_FLAG)

# Explicit stress lane for the ordinary Rust/Tokio stack contract. Routine
# `test-rust` already unsets RUST_MIN_STACK and runs the non-ignored depth and
# architecture regressions; this target additionally commissions the expensive
# 1,000-iteration qualification that remains ignored in the normal suite.
test-agentic-default-stack:
	@env -u RUST_MIN_STACK cargo test -p magician --lib default_stack_ -- --nocapture
	@env -u RUST_MIN_STACK cargo test -p magician --test agentic_default_stack_contract -- --nocapture
	@env -u RUST_MIN_STACK cargo test -p magician --lib default_stack_one_thousand_sequential_agentic_iterations -- --ignored --nocapture

.PHONY: test-terminal-outbox
test-terminal-outbox:
	@cargo test -p magician --features test-fixtures --lib magician_v2::execution::agentic::run_loop::terminal_outbox::tests -- --test-threads=1

.PHONY: test-envoy-claims-review
test-envoy-claims-review:
	$(MAKE) test-app-contextual-round
	cargo test -p magician -p magician-api --lib -- envoy_claims evidence::transcript_ingestion::tests --test-threads=1
	cd skillshub/bots/sdk && npm run build && node --test test/envoy-delivery.test.mjs test/email-mailbox.test.mjs
	cd skillshub/bots/gmail && npm test
	cd skillshub/bots/agentmail && npm test
	cd skillshub/bots/telegram && npm test
	cd skillshub/bots/kapso && npm test
	cd skillshub/bots/whatsapp && npm test
	cd ui/unified-ui && npm test -- src/lib/claims-review src/lib/apps/appNavigation.test.ts src/lib/apps/AppSurfacePage.component.test.ts

.PHONY: test-app-scripted-surfaces
test-app-scripted-surfaces:
	@cargo test -p magician-apps -p magician-api --lib scripted
	@cargo test -p magician-api --lib auth_api::tests

test-pty-live:
	@cargo test -p magician-pty session::tests::live:: -- --ignored --nocapture

# Run every repository test surface, even when an earlier suite fails, then
# write one stable dashboard linking every detailed child report produced by
# the run. Only after every non-live suite completes, the runner obeys an
# explicit live_evals=true|false value or asks on an interactive terminal. The
# runner returns the first failing suite only after the summary is available.
test:
	@MAKE_BIN='$(MAKE)' TEST_SUMMARY_REPORT_DIR='$(TEST_SUMMARY_REPORT_DIR)' \
		RUST_TEST_REPORT_DIR='$(RUST_TEST_REPORT_DIR)' UI_TEST_REPORT_DIR='$(UI_TEST_REPORT_DIR)' \
		IOS_TEST_REPORT_DIR='$(IOS_TEST_REPORT_DIR)' LIVE_EVAL_REPORT_DIR='$(LIVE_EVAL_REPORT_DIR)' \
		MAGDROID_JAVA_HOME='$(MAGDROID_JAVA_HOME)' MAGDROID_SDK='$(MAGDROID_SDK)' \
		AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR='$(AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR)' \
		ATTENTION_HISTORICAL_BOOTSTRAP_EVAL_REPORT='$(ATTENTION_HISTORICAL_BOOTSTRAP_EVAL_REPORT)' \
		TOOL_RESULT_PROJECTION_EVAL_REPORT_DIR='$(TOOL_RESULT_PROJECTION_EVAL_REPORT_DIR)' \
		PROVIDER_REPLAY_EVAL_REPORT_DIR='$(PROVIDER_REPLAY_EVAL_REPORT_DIR)' \
		CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY='$(CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY)' CONTENT_RETRIEVAL_RUNTIME_LIVE_URL='$(CONTENT_RETRIEVAL_RUNTIME_LIVE_URL)' \
		CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS='$(CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS)' \
		OBSERVABLE_SOURCES_LIVE_API_BASE_URL='$(OBSERVABLE_SOURCES_LIVE_API_BASE_URL)' OBSERVABLE_SOURCES_LIVE_PRINCIPAL='$(OBSERVABLE_SOURCES_LIVE_PRINCIPAL)' \
		OBSERVABLE_SOURCES_LIVE_WORKSPACE='$(OBSERVABLE_SOURCES_LIVE_WORKSPACE)' OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS='$(OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS)' \
		STORAGE_GOVERNANCE_LIVE_API_BASE_URL='$(STORAGE_GOVERNANCE_LIVE_API_BASE_URL)' STORAGE_GOVERNANCE_LIVE_PRINCIPAL='$(STORAGE_GOVERNANCE_LIVE_PRINCIPAL)' \
		STORAGE_GOVERNANCE_LIVE_WORKSPACE='$(STORAGE_GOVERNANCE_LIVE_WORKSPACE)' STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS='$(STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS)' \
		LIVE_EVAL_RUNS='$(LIVE_EVAL_RUNS)' LIVE_EVAL_WORKERS='$(LIVE_EVAL_WORKERS)' \
		LIVE_CHUNK_EVAL_RUNS='$(LIVE_CHUNK_EVAL_RUNS)' LIVE_AUTH_EVAL_RUNS='$(LIVE_AUTH_EVAL_RUNS)' OLLAMA_CHUNK_EVAL_MODEL='$(OLLAMA_CHUNK_EVAL_MODEL)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS)' CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS)' CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS)' \
		CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS='$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS)' CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS='$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS)' \
		CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS='$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS)' \
		TOOL_RESULT_PROJECTION_LIVE_RUNS='$(TOOL_RESULT_PROJECTION_LIVE_RUNS)' TOOL_RESULT_PROJECTION_LIVE_PROFILE='$(TOOL_RESULT_PROJECTION_LIVE_PROFILE)' \
		PROVIDER_REPLAY_LIVE_PROFILES='$(PROVIDER_REPLAY_LIVE_PROFILES)' \
		PREPLAN_LIVE_API_BASE_URL='$(PREPLAN_LIVE_API_BASE_URL)' PREPLAN_LIVE_PRINCIPAL='$(PREPLAN_LIVE_PRINCIPAL)' \
		PREPLAN_LIVE_WORKSPACE='$(PREPLAN_LIVE_WORKSPACE)' PREPLAN_LIVE_HITL_MODE='$(PREPLAN_LIVE_HITL_MODE)' \
		PREPLAN_LIVE_TIMEOUT_SECS='$(PREPLAN_LIVE_TIMEOUT_SECS)' PREPLAN_LIVE_PROJECTION_TIMEOUT_SECS='$(PREPLAN_LIVE_PROJECTION_TIMEOUT_SECS)' PREPLAN_LIVE_HTTP_TIMEOUT_SECS='$(PREPLAN_LIVE_HTTP_TIMEOUT_SECS)' \
		WEB_RESEARCHER_LIVE_API_BASE_URL='$(WEB_RESEARCHER_LIVE_API_BASE_URL)' WEB_RESEARCHER_LIVE_RUNS='$(WEB_RESEARCHER_LIVE_RUNS)' \
		WEB_RESEARCHER_LIVE_HTTP_TIMEOUT_SECS='$(WEB_RESEARCHER_LIVE_HTTP_TIMEOUT_SECS)' WEB_RESEARCHER_LIVE_CITATION_PROBE_LIMIT='$(WEB_RESEARCHER_LIVE_CITATION_PROBE_LIMIT)' \
		MEMORY_TEMPERATURE_LIVE_MIN_RECALL='$(MEMORY_TEMPERATURE_LIVE_MIN_RECALL)' MEMORY_TEMPERATURE_LIVE_MIN_HYBRID='$(MEMORY_TEMPERATURE_LIVE_MIN_HYBRID)' \
		MEMORY_TEMPERATURE_LIVE_MIN_UTILITY='$(MEMORY_TEMPERATURE_LIVE_MIN_UTILITY)' MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS='$(MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS)' \
		MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS='$(MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS)' \
		COMPACTOR_EVAL_RUNS='$(COMPACTOR_EVAL_RUNS)' COMPACTOR_MIN_VALID_RATE='$(COMPACTOR_MIN_VALID_RATE)' COMPACTOR_MAX_EXHAUSTION_RATE='$(COMPACTOR_MAX_EXHAUSTION_RATE)' \
		COMPACTOR_MIN_SEMANTIC_RETENTION='$(COMPACTOR_MIN_SEMANTIC_RETENTION)' COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION='$(COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION)' \
		LOCAL_CHUNK_EVAL_RUNTIME='$(LOCAL_CHUNK_EVAL_RUNTIME)' LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL='$(LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL)' \
		LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE='$(LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE)' LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE='$(LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE)' \
		LOCAL_CHUNK_EVAL_CONTEXT_TOKENS='$(LOCAL_CHUNK_EVAL_CONTEXT_TOKENS)' \
		LLM_PHASE3_PRINCIPAL='$(LLM_PHASE3_PRINCIPAL)' LLM_PHASE3_WORKSPACE='$(LLM_PHASE3_WORKSPACE)' LLM_PHASE3_CALL_ID='$(LLM_PHASE3_CALL_ID)' \
		LLM_PHASE4_PRINCIPAL='$(LLM_PHASE4_PRINCIPAL)' LLM_PHASE4_WORKSPACE='$(LLM_PHASE4_WORKSPACE)' \
		LIVE_EVAL_CONFIG='$(LIVE_EVAL_CONFIG)' \
		./scripts/run-all-tests-with-report.sh $(LIVE_EVAL_FLAG) $(COVERAGE_FLAG)

# Explicit real-provider and resident-local-model lane. `make test` runs it
# last when selected; this target runs the live checks alone.
#
# Start both Ollama daemons (:11434 generation + :11435 embedding) up front: the
# chunking local-shadow lane and chat-context retrieval need them, but only the
# later memory-temperature eval used to start Ollama, so those earlier lanes
# failed fast (connection refused). run-ollama.sh is idempotent — a no-op when
# already healthy, so memory-temperature's own run-ollama call still re-verifies.
#
# `requires=` is the UNION of the sub-evals this umbrella runs, because only
# monitor-live probes-and-skips (run-live-evals-with-report.sh) — the rest fail
# outright: memory-temperature shells out to ./$(MAGICIAN_BIN) via
# memory-index-prewarm, content-retrieval-runtime needs Magicutor, and
# observable-sources/storage-governance need the running server's HTTP API.
.PHONY: test-live-evals
## eval: kind=live requires=ollama,magician,magician_binary,magicutor,provider_keys report=evals/live-suite
## desc: Run only the real-provider and resident-local-model eval suite
test-live-evals: setup-test-suite-runner-deps
	@MAGICIAN_CONFIG_PATH='$(LIVE_EVAL_CONFIG)' $(MAKE) --no-print-directory run-ollama
	@LIVE_EVAL_REPORT_DIR='$(LIVE_EVAL_REPORT_DIR)' LIVE_EVAL_RUNS='$(LIVE_EVAL_RUNS)' \
		LIVE_EVAL_WORKERS='$(LIVE_EVAL_WORKERS)' LIVE_CHUNK_EVAL_RUNS='$(LIVE_CHUNK_EVAL_RUNS)' LIVE_AUTH_EVAL_RUNS='$(LIVE_AUTH_EVAL_RUNS)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS)' CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS)' CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS)' \
		CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS='$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_RUNS)' CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS='$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_INPUTS)' \
		CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS='$(CHAT_CONTEXT_RETRIEVAL_BACKGROUND_P95_MAX_MS)' \
		TOOL_RESULT_PROJECTION_LIVE_RUNS='$(TOOL_RESULT_PROJECTION_LIVE_RUNS)' TOOL_RESULT_PROJECTION_LIVE_PROFILE='$(TOOL_RESULT_PROJECTION_LIVE_PROFILE)' \
		PROVIDER_REPLAY_LIVE_PROFILES='$(PROVIDER_REPLAY_LIVE_PROFILES)' \
		PREPLAN_LIVE_API_BASE_URL='$(PREPLAN_LIVE_API_BASE_URL)' PREPLAN_LIVE_PRINCIPAL='$(PREPLAN_LIVE_PRINCIPAL)' \
		PREPLAN_LIVE_WORKSPACE='$(PREPLAN_LIVE_WORKSPACE)' PREPLAN_LIVE_HITL_MODE='$(PREPLAN_LIVE_HITL_MODE)' \
		PREPLAN_LIVE_TIMEOUT_SECS='$(PREPLAN_LIVE_TIMEOUT_SECS)' PREPLAN_LIVE_PROJECTION_TIMEOUT_SECS='$(PREPLAN_LIVE_PROJECTION_TIMEOUT_SECS)' PREPLAN_LIVE_HTTP_TIMEOUT_SECS='$(PREPLAN_LIVE_HTTP_TIMEOUT_SECS)' \
		MEMORY_TEMPERATURE_LIVE_MIN_RECALL='$(MEMORY_TEMPERATURE_LIVE_MIN_RECALL)' MEMORY_TEMPERATURE_LIVE_MIN_HYBRID='$(MEMORY_TEMPERATURE_LIVE_MIN_HYBRID)' \
		MEMORY_TEMPERATURE_LIVE_MIN_UTILITY='$(MEMORY_TEMPERATURE_LIVE_MIN_UTILITY)' MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS='$(MEMORY_TEMPERATURE_LIVE_RETRIEVAL_P95_MAX_MS)' \
		MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS='$(MEMORY_TEMPERATURE_LIVE_UTILITY_P95_MAX_MS)' \
		COMPACTOR_EVAL_RUNS='$(COMPACTOR_EVAL_RUNS)' COMPACTOR_MIN_VALID_RATE='$(COMPACTOR_MIN_VALID_RATE)' COMPACTOR_MAX_EXHAUSTION_RATE='$(COMPACTOR_MAX_EXHAUSTION_RATE)' \
		COMPACTOR_MIN_SEMANTIC_RETENTION='$(COMPACTOR_MIN_SEMANTIC_RETENTION)' COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION='$(COMPACTOR_MIN_PROTECTED_SEMANTIC_RETENTION)' \
		CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY='$(CONTENT_RETRIEVAL_RUNTIME_LIVE_QUERY)' CONTENT_RETRIEVAL_RUNTIME_LIVE_URL='$(CONTENT_RETRIEVAL_RUNTIME_LIVE_URL)' \
		CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS='$(CONTENT_RETRIEVAL_RUNTIME_LIVE_TIMEOUT_SECS)' \
		OBSERVABLE_SOURCES_LIVE_API_BASE_URL='$(OBSERVABLE_SOURCES_LIVE_API_BASE_URL)' OBSERVABLE_SOURCES_LIVE_PRINCIPAL='$(OBSERVABLE_SOURCES_LIVE_PRINCIPAL)' \
		OBSERVABLE_SOURCES_LIVE_WORKSPACE='$(OBSERVABLE_SOURCES_LIVE_WORKSPACE)' OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS='$(OBSERVABLE_SOURCES_LIVE_TIMEOUT_SECS)' \
		STORAGE_GOVERNANCE_LIVE_API_BASE_URL='$(STORAGE_GOVERNANCE_LIVE_API_BASE_URL)' STORAGE_GOVERNANCE_LIVE_PRINCIPAL='$(STORAGE_GOVERNANCE_LIVE_PRINCIPAL)' \
		STORAGE_GOVERNANCE_LIVE_WORKSPACE='$(STORAGE_GOVERNANCE_LIVE_WORKSPACE)' STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS='$(STORAGE_GOVERNANCE_LIVE_TIMEOUT_SECS)' \
		OLLAMA_CHUNK_EVAL_MODEL='$(OLLAMA_CHUNK_EVAL_MODEL)' \
		LOCAL_CHUNK_EVAL_RUNTIME='$(LOCAL_CHUNK_EVAL_RUNTIME)' LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL='$(LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL)' \
		LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE='$(LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE)' LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE='$(LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE)' \
		LOCAL_CHUNK_EVAL_CONTEXT_TOKENS='$(LOCAL_CHUNK_EVAL_CONTEXT_TOKENS)' \
		LLM_PHASE3_PRINCIPAL='$(LLM_PHASE3_PRINCIPAL)' LLM_PHASE3_WORKSPACE='$(LLM_PHASE3_WORKSPACE)' LLM_PHASE3_CALL_ID='$(LLM_PHASE3_CALL_ID)' \
		LLM_PHASE4_PRINCIPAL='$(LLM_PHASE4_PRINCIPAL)' LLM_PHASE4_WORKSPACE='$(LLM_PHASE4_WORKSPACE)' \
		LIVE_EVAL_CONFIG='$(LIVE_EVAL_CONFIG)' LIVE_EVAL_PYTHON='$(TEST_SUITE_RUNNER_PYTHON)' \
		./scripts/run-live-evals-with-report.sh

llm-trace-coverage:
	@python3 scripts/audit-llm-trace-coverage.py

## eval: kind=harness report=evals/llm-observability-phase0
## desc: Audit LLM call-path coverage and baseline call/dispatch field population
llm-trace-phase0-baseline: llm-trace-coverage
	@python3 scripts/eval-llm-observability-phase0.py --output-dir '$(LLM_TRACE_PHASE0_REPORT_DIR)' $(LLM_TRACE_PHASE0_ARGS)

test-llm-trace-phase0:
	@python3 -m unittest scripts/test_llm_trace_phase0.py

## eval: kind=harness report=evals/llm-observability-phase1
## desc: Audit content-free call/dispatch/attempt correlation identity on local facts
llm-trace-phase1-audit:
	@python3 scripts/eval-llm-observability-phase1.py --output-dir '$(LLM_TRACE_PHASE1_REPORT_DIR)' $(LLM_TRACE_PHASE1_ARGS)

test-llm-trace-phase1:
	@python3 -m unittest scripts/test_llm_trace_phase1.py

test-llm-trace-phase2:
	@python3 -m unittest scripts/test_llm_trace_phase2.py

test-llm-trace-phase2b:
	@python3 -m unittest scripts/test_llm_trace_phase2b.py
	@cargo test -p magician --lib llm_trace_journal -- --nocapture

test-llm-trace-phase2c:
	@python3 -m unittest scripts/test_llm_trace_phase2c.py
	@cargo test -p magician --lib llm_trace_materializer -- --nocapture

test-llm-trace-phase2d:
	@python3 -m unittest scripts/test_llm_trace_phase2d.py
	@cargo test -p magician --lib llm_fact_registry -- --nocapture
	@cargo test -p magician --lib llm_fact_compactor -- --nocapture
	@cargo test -p magician --lib llm_analytics_read_service -- --nocapture

test-llm-trace-phase2e:
	@python3 -m unittest scripts/test_llm_trace_phase2e.py
	@cargo test -p magician --lib llm_analytics_read_service -- --nocapture
	@cargo test -p magician --lib internal_data_provider -- --nocapture
	@cargo test -p magician --lib analytics_api -- --nocapture

## eval: kind=live requires=magician report=evals/llm-observability-phase2f
## desc: Reconcile canonical LLM facts against the running server's overview API
llm-trace-phase2f-audit:
	@python3 scripts/eval-llm-observability-phase2f.py --strict --output-dir '$(LLM_TRACE_PHASE2F_REPORT_DIR)' $(LLM_TRACE_PHASE2F_ARGS)

test-llm-trace-phase2f:
	@python3 -m unittest scripts/test_llm_trace_phase2f.py
	@python3 scripts/eval-llm-observability-phase2f.py --self-test
	@cargo test -p magician --lib llm_trace_activation -- --nocapture
	@cargo test -p magician --lib llm_trace_journal -- --nocapture
	@cargo test -p magician --lib llm_trace_materializer -- --nocapture
	@cargo test -p magician --lib llm_analytics_read_service -- --nocapture
	@cargo test -p magician --lib llm_parquet_sink -- --nocapture

## eval: kind=harness report=evals/llm-observability-phase3
## desc: Audit restricted LLM content storage for redaction and scope violations
llm-trace-phase3-audit:
	@python3 scripts/eval-llm-observability-phase3.py --strict --output-dir '$(LLM_TRACE_PHASE3_REPORT_DIR)' $(LLM_TRACE_PHASE3_ARGS)

test-llm-trace-phase3:
	@python3 -m unittest scripts/test_llm_trace_phase3.py
	@python3 scripts/eval-llm-observability-phase3.py --self-test
	@cargo test -p magicllm --lib content_observer -- --nocapture
	@cargo test -p magician --lib llm_trace_content -- --nocapture
	@cargo test -p magician --lib llm_restricted_content -- --nocapture
	@cargo test -p magician --lib llm_trace_journal -- --nocapture
	@cargo test -p magician --lib llm_trace_materializer -- --nocapture

## eval: kind=harness report=evals/llm-observability-phase4
## desc: Audit tool-call lineage and capture-gap accounting on local LLM facts
llm-trace-phase4-audit:
	@python3 scripts/eval-llm-observability-phase4.py --strict --principal '$(LLM_PHASE4_PRINCIPAL)' --workspace '$(LLM_PHASE4_WORKSPACE)' --output-dir '$(LLM_TRACE_PHASE4_REPORT_DIR)' $(LLM_TRACE_PHASE4_ARGS)

test-llm-trace-phase4:
	@python3 -m unittest scripts/test_llm_trace_phase4.py
	@python3 scripts/eval-llm-observability-phase4.py --self-test
	@cargo test -p magician --lib llm_tool_lineage -- --nocapture
	@cargo test -p magician --lib llm_trace_recorder -- --nocapture
	@cargo test -p magician --lib llm_trace_activation -- --nocapture
	@cargo test -p magician --lib llm_trace_journal -- --nocapture
	@cargo test -p magician --lib llm_trace_materializer -- --nocapture
	@cargo test -p magician --lib llm_analytics_read_service -- --nocapture
	@cargo test -p magician --lib internal_data_provider -- --nocapture
	@cargo test -p magician --lib analytics_api -- --nocapture

test-suite-runner: setup-test-suite-runner-deps
	@"$(TEST_SUITE_RUNNER_PYTHON)" -m unittest scripts/test_run_all_tests_with_report.py scripts/test_setup_desktop_pnpm.py scripts/test_run_live_evals_with_report.py scripts/test_magios_test_report.py scripts/test_run_magios_tests_with_report.py scripts/test_tool_result_projection_context_live_eval.py scripts/test_storage_governance_live_eval.py scripts/test_verify_ollama_residency.py scripts/test_llm_trace_phase0.py scripts/test_llm_trace_phase1.py scripts/test_llm_trace_phase2.py scripts/test_llm_trace_phase2b.py scripts/test_llm_trace_phase2c.py scripts/test_llm_trace_phase2d.py scripts/test_llm_trace_phase2e.py scripts/test_llm_trace_phase2f.py scripts/test_llm_trace_phase3.py scripts/test_llm_trace_phase4.py

# Live semantic eval for the Thinking Map interpreter (plan Phase 3, item 8).
# Runs fixture utterances against the RUNNING server's /interpret (real LLM),
# SHADOW-ONLY on scratch maps (soft-deleted afterwards). Gates: correction-
# target accuracy >=95%, zero authority violations, envelope validity >=95%.
# Cost bound: ~20 mini-class LLM calls per run (TM_EVAL_RUNS=1 => 8-call smoke).
# Report: coverage/evals/thinking-map/latest.html
.PHONY: eval-thinking-map-live
## eval: kind=live requires=magician report=evals/thinking-map
## desc: Gate Thinking Map interpreter accuracy on the running server (shadow-only)
eval-thinking-map-live:
	@TM_EVAL_RUNS='$(TM_EVAL_RUNS)' TM_EVAL_REPORT_DIR='$(TM_EVAL_REPORT_DIR)' \
		python3 scripts/eval-thinking-map-live.py

# Provider-free golden eval for the Recurring Monitors change ledger
# (plan Phase 6 item 4): runs the deterministic Rust scenario suite
# (magician/tests/monitor_golden_scenarios.rs over
# tests/fixtures/monitors/golden/) and writes
# coverage/evals/monitor/latest.{html,json}. No LLM, no server — the live
# run-quality lane is explicitly out of scope.
.PHONY: eval-monitor-golden
## eval: kind=harness report=evals/monitor
## desc: Provider-free golden eval for the Recurring Monitors change ledger
eval-monitor-golden:
	@MONITOR_EVAL_REPORT_DIR='$(MONITOR_EVAL_REPORT_DIR)' \
		python3 scripts/eval-monitor-change-ledger.py

# Live evals for Recurring Monitors (need a running rebuilt server;
# see the script header for env). LLM-led agentic compaction evals
# were removed with the feature.
.PHONY: test-monitor-live-eval-harness eval-monitor-live
## eval: kind=harness
## desc: Self-test the Recurring Monitors live evaluator
test-monitor-live-eval-harness:
	@python3 -m unittest scripts.test_eval_monitor_live

## eval: kind=live requires=magician report=evals/monitor-live
## desc: Gate real monitor run quality against a running server
eval-monitor-live:
	@python3 scripts/eval-monitor-live.py

.PHONY: test-agent-tool-visibility-live-eval
## eval: kind=live requires=provider_keys report=evals/live-suite
## desc: Run authorization + surface hot/deferred baseline/candidate live gates
test-agent-tool-visibility-live-eval: setup-test-suite-runner-deps
	@LIVE_EVAL_REPORT_DIR='$(LIVE_EVAL_REPORT_DIR)' LIVE_AUTH_EVAL_RUNS='$(LIVE_AUTH_EVAL_RUNS)' \
		LIVE_EVAL_ONLY='agent-tool-visibility-authorization' LIVE_EVAL_CONFIG='$(LIVE_EVAL_CONFIG)' \
		LIVE_EVAL_PYTHON='$(TEST_SUITE_RUNNER_PYTHON)' \
		./scripts/run-live-evals-with-report.sh

## eval: kind=harness report=evals/ollama-logical-chunking
## desc: Dry-run chunking config surfaces and plan validity without spending model time
ollama-chunking-readiness:
	@python3 scripts/eval-ollama-logical-chunking.py --dry-run --runs 1 \
		--local-model '$(OLLAMA_CHUNK_EVAL_MODEL)' \
		--runtime-kind '$(LOCAL_CHUNK_EVAL_RUNTIME)' \
		$(if $(LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL),--served-model-label '$(LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL)',) \
		$(if $(LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE),--structured-output-mode '$(LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE)',) \
		$(if $(LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE),--phase-timing-source '$(LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE)',) \
		$(if $(LOCAL_CHUNK_EVAL_CONTEXT_TOKENS),--runtime-context-tokens '$(LOCAL_CHUNK_EVAL_CONTEXT_TOKENS)',) \
		$(if $(LIVE_CHUNK_EVAL_OPERATION),--operation '$(LIVE_CHUNK_EVAL_OPERATION)',) \
		--output-dir '$(OLLAMA_CHUNK_EVAL_REPORT_DIR)'

## eval: kind=live requires=ollama,provider_keys report=evals/ollama-logical-chunking
## desc: Cost-bearing local/cloud Phase 7 verification report
ollama-chunking-shadow-eval:
	@python3 scripts/eval-ollama-logical-chunking.py --runs '$(LIVE_CHUNK_EVAL_RUNS)' \
		--local-model '$(OLLAMA_CHUNK_EVAL_MODEL)' \
		--runtime-kind '$(LOCAL_CHUNK_EVAL_RUNTIME)' \
		$(if $(LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL),--served-model-label '$(LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL)',) \
		$(if $(LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE),--structured-output-mode '$(LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE)',) \
		$(if $(LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE),--phase-timing-source '$(LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE)',) \
		$(if $(LOCAL_CHUNK_EVAL_CONTEXT_TOKENS),--runtime-context-tokens '$(LOCAL_CHUNK_EVAL_CONTEXT_TOKENS)',) \
		$(if $(LIVE_CHUNK_EVAL_OPERATION),--operation '$(LIVE_CHUNK_EVAL_OPERATION)',) \
		--output-dir '$(OLLAMA_CHUNK_EVAL_REPORT_DIR)'

# Codegen: regenerate the TS event-taxonomy mirror from the Rust
# `taxonomies!` macro (single source of truth in
# magician/src/magician_v2/realtime_events.rs).
#
# Run manually whenever realtime_events.rs or event_taxonomy_dump.rs
# changes. The follow-up `event-taxonomy-stamp` step embeds a SHA-256
# of the source files into the generated TS so the fast
# `event-taxonomy-check` gate (wired into test/build-all-*) can detect
# drift without re-running the slow cargo build.
#
# `cargo run --quiet` suppresses cargo's compile progress output, so on a
# cold `$(CARGO_TARGET_DIR)` the recipe can sit silent for 3–8 minutes while the
# magician crate + dependencies build. The leading echo prints a visible
# "starting…" marker so operators don't think the recipe is stuck.
EVENT_TAXONOMY_TS := ui/unified-ui/src/lib/realtime/event-taxonomy.ts
# Sources hashed by event-taxonomy-check/stamp.  The thin taxonomy crate is
# now the single source of truth for EVENT_TAXONOMY_TABLE and GAUI_EVENT_TAXONOMY;
# realtime_events.rs is still hashed because its taxonomies! invocation drives
# the runtime taxonomy_lookup match (the two must stay in sync).
EVENT_TAXONOMY_SOURCES := \
	magician/src/magician_v2/realtime_events.rs \
	magician-event-taxonomy/src/lib.rs \
	magician-event-taxonomy/src/bin/event_taxonomy_dump.rs

event-taxonomy-codegen:
	@echo "[codegen] regenerating $(EVENT_TAXONOMY_TS) (compiling magician-event-taxonomy only — should be fast after first build)…"
	@cargo run -p magician-event-taxonomy --bin event_taxonomy_dump --quiet > $(EVENT_TAXONOMY_TS).tmp
	@mv $(EVENT_TAXONOMY_TS).tmp $(EVENT_TAXONOMY_TS)
	@python3 scripts/codegen_drift.py stamp --label event-taxonomy \
		--regen "make event-taxonomy-codegen" --output $(EVENT_TAXONOMY_TS) $(EVENT_TAXONOMY_SOURCES)
	@echo "[codegen] $(EVENT_TAXONOMY_TS) regenerated"

# Drift gate — fast (~10ms; pure file-IO). Wired as a prerequisite to
# `check-all`, `test-rust`, `build-all-debug` and `build-all-release` so a
# forgotten codegen fails with a clear remediation message instead of silently
# letting the TS mirror diverge from the Rust source of truth. `check-all`
# matters most: it is the target CLAUDE.md points people at for validation, so
# a gate absent from it is a gate most runs never reach.
event-taxonomy-check:
	@python3 scripts/codegen_drift.py check --label event-taxonomy \
		--regen "make event-taxonomy-codegen" --output $(EVENT_TAXONOMY_TS) $(EVENT_TAXONOMY_SOURCES)

.PHONY: event-taxonomy-codegen event-taxonomy-check

# The component graph rendered as a page you can open and click through: turn a
# component off and watch which features go dark and why. Generated from
# graph.yaml through the crate's own loader, so a graph the runtime would reject
# fails here before anything is written, and the page can never describe a
# system the resolver would not agree with.
#
# Committed rather than gitignored so it can be read without a build, which is
# the whole point of a page that explains the install. It is excluded from the
# public mirror (.mirror/exclude.txt) — it documents this operator's install
# surface, not the product.
COMPONENT_GRAPH_PAGE := component-graph.html
COMPONENT_GRAPH_SOURCES := \
	magician-components/src/graph.yaml \
	magician-components/src/setup_catalog.yaml \
	magician-components/src/bin/graph_page.html \
	magician-components/src/bin/component_graph_page.rs

component-graph:
	@echo "[codegen] rendering $(COMPONENT_GRAPH_PAGE) from the component graph"
	@cargo run -q -p magician-components --features codegen --bin component_graph_page > $(COMPONENT_GRAPH_PAGE).tmp
	@mv $(COMPONENT_GRAPH_PAGE).tmp $(COMPONENT_GRAPH_PAGE)
	@python3 scripts/codegen_drift.py stamp --label component-graph \
		--regen "make component-graph" --output $(COMPONENT_GRAPH_PAGE) $(COMPONENT_GRAPH_SOURCES)

# Same shape as the taxonomy gate: pure file IO, milliseconds, wired into build
# and test so a forgotten regeneration fails loudly instead of leaving the page
# describing a graph that has moved on.
component-graph-check:
	@python3 scripts/codegen_drift.py check --label component-graph \
		--regen "make component-graph" --output $(COMPONENT_GRAPH_PAGE) $(COMPONENT_GRAPH_SOURCES)

.PHONY: component-graph component-graph-check

# Run the same composed report flow with detailed Rust and frontend output.
test-verbose:
	@MAKE_BIN='$(MAKE)' TEST_SUMMARY_REPORT_DIR='$(TEST_SUMMARY_REPORT_DIR)' \
		RUST_TEST_REPORT_DIR='$(RUST_TEST_REPORT_DIR)' UI_TEST_REPORT_DIR='$(UI_TEST_REPORT_DIR)' \
		IOS_TEST_REPORT_DIR='$(IOS_TEST_REPORT_DIR)' LIVE_EVAL_REPORT_DIR='$(LIVE_EVAL_REPORT_DIR)' \
		MAGDROID_JAVA_HOME='$(MAGDROID_JAVA_HOME)' MAGDROID_SDK='$(MAGDROID_SDK)' \
		AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR='$(AGENT_SURFACE_RUNTIME_EVAL_REPORT_DIR)' \
		ATTENTION_HISTORICAL_BOOTSTRAP_EVAL_REPORT='$(ATTENTION_HISTORICAL_BOOTSTRAP_EVAL_REPORT)' \
		TOOL_RESULT_PROJECTION_EVAL_REPORT_DIR='$(TOOL_RESULT_PROJECTION_EVAL_REPORT_DIR)' \
		PROVIDER_REPLAY_EVAL_REPORT_DIR='$(PROVIDER_REPLAY_EVAL_REPORT_DIR)' \
		LIVE_EVAL_RUNS='$(LIVE_EVAL_RUNS)' LIVE_EVAL_WORKERS='$(LIVE_EVAL_WORKERS)' \
		LIVE_CHUNK_EVAL_RUNS='$(LIVE_CHUNK_EVAL_RUNS)' OLLAMA_CHUNK_EVAL_MODEL='$(OLLAMA_CHUNK_EVAL_MODEL)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS)' CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_WARMUPS)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS)' CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_P95_MAX_MS)' \
		CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS='$(CHAT_CONTEXT_RETRIEVAL_LIVE_INDEX_WAIT_SECS)' \
		PROVIDER_REPLAY_LIVE_PROFILES='$(PROVIDER_REPLAY_LIVE_PROFILES)' \
		LOCAL_CHUNK_EVAL_RUNTIME='$(LOCAL_CHUNK_EVAL_RUNTIME)' LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL='$(LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL)' \
		LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE='$(LOCAL_CHUNK_EVAL_STRUCTURED_OUTPUT_MODE)' LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE='$(LOCAL_CHUNK_EVAL_PHASE_TIMING_SOURCE)' \
		LOCAL_CHUNK_EVAL_CONTEXT_TOKENS='$(LOCAL_CHUNK_EVAL_CONTEXT_TOKENS)' \
		LIVE_EVAL_CONFIG='$(LIVE_EVAL_CONFIG)' \
		./scripts/run-all-tests-with-report.sh --verbose $(LIVE_EVAL_FLAG)

# Run specific test suite
test-agent-router:
	cargo test --test agent_router_test

test-data-structures:
	cargo test --test data_structures_test

test-streaming:
	cargo test --test streaming_protocols_test

test-performance:
	cargo test --test performance_test

# Gate 3 wall-clock budgets, enforced. The workspace report lane runs several
# test processes at once and so waives these lines; this target runs them one
# at a time on an otherwise idle machine, which is the only way the numbers
# mean anything. See docs/components/magician-storage/adr-2026-09-01-performance-capacity.md
test-storage-budgets:
	MAGICIAN_GATE3_ENFORCE_LATENCY=1 cargo nextest run \
		-p magician-storage -p magician-storage-s3 -p magician-storage-state \
		--test gate3 --test-threads 1
	MAGICIAN_GATE3_ENFORCE_LATENCY=1 cargo nextest run \
		-p magician -E 'test(magician_v2::gate3::drills::)' --test-threads 1

test-grpc:
	cargo test --test grpc_integration_test

test-integration:
	cargo test --test integration_test

test-mcp-server:
	cargo test --test mcp_server_test

test-registry:
	cargo test --test registry_service_test

# Run capability file validation
test-capabilities: test-gws-profile-config
	cargo test -p magician --test yaml_parsing_test

# Hermetic package-shape contracts over every governed tool skill in skillshub/:
# manifests, shipped executables, secret references, adapter parameters and
# defaults, timeout margins, and CA-bundle delivery. No provider is called.
test-tool-skills:
	cargo test -p magician --test tool_skill_contract

# Remote dependency tests are not included by Cargo's --workspace selection.
# These explicit lanes prepare the exact root-manifest pin in an isolated cache;
# preparation may use the network and never resets an existing edited checkout.
setup-extracted-libraries:
	python3 scripts/with_extracted_library.py magicrun prepare
	python3 scripts/with_extracted_library.py magicvault prepare

# Resolution only: no compilation, checks, tests, or verification gates.
# Seed an existing resolution before this target to preserve transitive versions.
sync-extracted-lockfile:
	cargo update --workspace

check-extracted-libraries: setup-extracted-libraries
	python3 -m unittest discover -s scripts -p 'test_extracted_library_source.py'
	python3 scripts/with_extracted_library.py magicrun check
	python3 scripts/with_extracted_library.py magicvault check

test-magicrun: setup-extracted-libraries
	python3 scripts/with_extracted_library.py magicrun test

test-magicrun-lifecycle: setup-extracted-libraries
	python3 scripts/with_extracted_library.py magicrun test-lifecycle

magicrun-inventory magicrun-classification magicrun-replay:
	python3 scripts/with_extracted_library.py magicrun $(patsubst magicrun-%,%,$@) -- $(ARGS)

test-magicvault: setup-extracted-libraries
	python3 scripts/with_extracted_library.py magicvault test

test-magicvault-foundation: setup-extracted-libraries
	python3 scripts/with_extracted_library.py magicvault test-foundation

test-extracted-libraries: test-magicrun test-magicvault

test-magicvault-compatibility: setup-extracted-libraries
	python3 scripts/with_extracted_library.py magicvault test-compatibility
	cargo test -p magician-core --lib durable_io
	cargo test -p magician --test magicvault_facade
	cargo test -p magician --lib magician_v2::secrets

# Rerun only the retained product unit group after a test-only change; avoid
# rebuilding the separate integration harness and repeating upstream suites.
test-magicvault-secrets:
	cargo test --locked -p magician --lib magician_v2::secrets

# Secure HITL P5: critical-request delivery — the coordinator, alert, policy,
# records and settings units, the push sink, the API handlers' shapes, the
# bot SDK's claim/report runtime, and the Settings panel.
test-hitl-delivery:
	cargo test --locked -p magician --lib -- magician_v2::hitl_delivery magician_v2::critical_delivery_settings magician_v2::mobile_push
	cargo test --locked -p magician-api --lib -- hitl_delivery_api
	@$(MAKE) -C skillshub setup-deps >/dev/null
	cd skillshub/bots/sdk && PATH="$(CURDIR)/skillshub/.node/bin:$$PATH" npm run test
	cd ui/unified-ui && npx vitest run src/lib/settings/CriticalDeliveryPanel.component.test.ts

# Secure HITL P6: inbound verification-code retrieval — extraction, matching,
# the resolver, the Android handoff, the source watches, the API shapes, the
# coordinator's retrieval grace, and the prompt's status line.
test-verification-codes:
	cargo test --locked -p magician --lib -- magician_v2::verification_codes magician_v2::hitl_delivery::coordinator::tests::a_code_ask_under_automatic_retrieval magician_v2::user_requests::service::custody_tests::an_automatic_answerers_channel_stays_on_the_record
	cargo test --locked -p magician-comms --lib -- verification_sources gws_client::tests::a_verification_read gws_client::tests::only_gmails_own ingest_agentmail::tests::a_verification_read ingest_imessage::tests::a_verification_read
	cargo test --locked -p magician-api --lib -- verification_codes_api
	cd ui/unified-ui && npx vitest run src/lib/hitl/retrievalStatus.test.ts src/lib/devices/DevicePairingPanel.component.test.ts
	@if [ ! -d '$(MAGDROID_JAVA_HOME)' ] || [ ! -d '$(MAGDROID_SDK)' ]; then \
		echo "Skipping magdroid extractor tests: run 'make setup-magdroid-build' first."; \
	else \
		cd magdroid/android && JAVA_HOME='$(MAGDROID_JAVA_HOME)' ANDROID_SDK_ROOT='$(MAGDROID_SDK)' '$(MAGDROID_GRADLE)' :bridge:testDebugUnitTest --tests 'ai.magicbeans.magdroid.notification.*' --tests 'ai.magicbeans.magdroid.mcp.McpToolRegistryTest'; \
	fi

.PHONY: setup-extracted-libraries sync-extracted-lockfile check-extracted-libraries test-magicrun test-magicrun-lifecycle test-magicvault test-extracted-libraries test-magicvault-compatibility
.PHONY: magicrun-inventory magicrun-classification magicrun-replay
.PHONY: test-magicvault-foundation
# Secure HITL qualification (plan §8, P7): scripted-model journeys through the
# in-process API against the fixture login service, with canary sweeps over
# every storage class the runtime wrote, the realtime feed and the record.
test-secure-hitl-qualification:
	cargo test --locked -p magician --test secure_hitl_qualification

.PHONY: test-magicvault-secrets test-hitl-delivery test-verification-codes test-secure-hitl-qualification

# Tier 2: the live canary lane. Executes each skill's declared canary through
# the production GovernedExecutionCoordinator — the same path a real agent call
# takes — so it reports on the runtime production actually has. It lives in the
# magician crate for that reason; see the header of the test file.
#
# Spends real money. The default ceiling covers the free and cheap tiers;
# TOOL_CANARY_TIER=expensive additionally reaches media generation and the
# delegating coding agents. A skill whose credential is absent reports SKIPPED,
# never PASS.
## eval: kind=live report=evals/tool-skills
## desc: Call every governed tool skill's canary through the real governed runtime
.PHONY: test-tool-skills-live
test-tool-skills-live:
	cargo test -p magician --test tool_skill_canary -- --ignored --nocapture

.PHONY: test-gws-profile-config
test-gws-profile-config:
	@python3 -m unittest discover -s skillshub/scripts/tests -p 'test_*.py'

# Generate documentation
docs:
	cargo doc --open

# Check for security vulnerabilities
audit:
	cargo audit

# Update dependencies
update:
	cargo update

# --- Container targets ---

# Container image name
#
# Installer reconciliation (no behavior change for existing callers — every var
# stays `?=`, so plain `make run-container` / `make build-container` resolve to
# the same defaults as before). The installer (scripts/install.sh) addresses its
# container by a full $IMAGE_REF (e.g. ghcr.io/...:latest) under CONTAINER_NAME=magician.
# These aliases let the SAME `stop-container`/`run-container` targets clean up /
# match the installer's container when its env is exported:
#   MAGICIAN_IMAGE_REF      -> overrides CONTAINER_IMAGE + CONTAINER_TAG (split on last ':')
#   MAGICIAN_CONTAINER_NAME -> overrides CONTAINER_NAME
# Unset, all three fall back to the historical defaults below.
CONTAINER_IMAGE ?= $(if $(MAGICIAN_IMAGE_REF),$(shell printf '%s' "$(MAGICIAN_IMAGE_REF)" | sed 's/:[^:]*$$//'),magician)
CONTAINER_TAG ?= $(if $(MAGICIAN_IMAGE_REF),$(lastword $(subst :, ,$(MAGICIAN_IMAGE_REF))),dev)
CONTAINER_REGISTRY ?= ghcr.io/magicbeanbs100x
CONTAINER_NAME ?= $(or $(MAGICIAN_CONTAINER_NAME),magician-dev)
CONTAINER_RUNTIME ?= docker
CONTAINER_QUALIFICATION_REPORT ?= coverage/container-qualification/qualification.json
CONTAINER_IMAGE_REPORT ?= coverage/container-qualification/image-size.json
CONTAINER_MAX_COMPRESSED_BYTES ?=
CONTAINER_MAX_EXPANDED_BYTES ?=
CONTAINER_MAX_COLD_START_MS ?=
CONTAINER_E2E_RUNTIME ?= auto
CONTAINER_E2E_IMAGE ?= magician:e2e
CONTAINER_E2E_REPORT_DIR ?=
CONTAINER_E2E_ROOT ?= $(or $(MAGICIAN_ROOT_DIR),$(HOME)/MagicianNotes)
CONTAINER_E2E_LIVE_CONFIRM ?= 0

# Container builder entrypoint (keeps package selection aligned with the split).
# Split the larger runtime crates' LLVM work to reduce peak memory in the VM.
# This override leaves dependency fingerprints and the host release lane alone.
CONTAINER_MAGICIAN_CODEGEN_UNITS ?= 16
CONTAINER_API_CODEGEN_UNITS ?= 16
CONTAINER_BIN_CODEGEN_UNITS ?= 16
.PHONY: build-container-runtime
build-container-runtime: check-protoc
	cargo build --locked --release -p magician-bin -p magicutor -p magic-supervisor \
		--bin magician --bin magicutor --bin magic-supervisor \
		--config 'profile.release.package.magician.codegen-units=$(CONTAINER_MAGICIAN_CODEGEN_UNITS)' \
		--config 'profile.release.package.magician-api.codegen-units=$(CONTAINER_API_CODEGEN_UNITS)' \
		--config 'profile.release.package.magician-bin.codegen-units=$(CONTAINER_BIN_CODEGEN_UNITS)'

# macOS host compiler lane. cargo-zigbuild supplies the Linux linker and native
# C/C++ toolchain; the caller owns the target install and external cache paths.
CONTAINER_ZIG_TARGET ?= aarch64-unknown-linux-gnu.2.36
.PHONY: build-container-runtime-zig
build-container-runtime-zig: check-protoc
	cargo zigbuild --locked --release --target "$(CONTAINER_ZIG_TARGET)" \
		-p magician-bin -p magicutor -p magic-supervisor \
		--bin magician --bin magicutor --bin magic-supervisor \
		--config 'profile.release.package.magician.codegen-units=$(CONTAINER_MAGICIAN_CODEGEN_UNITS)' \
		--config 'profile.release.package.magician-api.codegen-units=$(CONTAINER_API_CODEGEN_UNITS)' \
		--config 'profile.release.package.magician-bin.codegen-units=$(CONTAINER_BIN_CODEGEN_UNITS)'

# Independent tool layer: never compiles the Magician service/UI graph. Called
# in an isolated Linux builder; the native developer's skill outputs are not used.
CONTAINER_SKILL_TOOLS_OUTPUT ?= /out/skill-tools
.PHONY: build-container-skill-tools build-container-tools-image
build-container-skill-tools:
	@test "$$(uname -s)" = Linux || { echo "Container skill tools must be built on Linux" >&2; exit 1; }
	cargo build --locked --release -p document-to-markdown-cli --bin document-to-markdown
	GOMAXPROCS=1 GOFLAGS=-p=1 $(MAKE) -C skillshub setup-metabase-cli PP_GO=/usr/local/go/bin/go
	mkdir -p "$(CONTAINER_SKILL_TOOLS_OUTPUT)"
	strip -o "$(CONTAINER_SKILL_TOOLS_OUTPUT)/document-to-markdown" "$(CARGO_TARGET_DIR)/release/document-to-markdown"
	install -m 0755 skillshub/metabase/bin/metabase-pp-cli "$(CONTAINER_SKILL_TOOLS_OUTPUT)/metabase-pp-cli"

CONTAINER_TOOLS_ENGINE ?= container
CONTAINER_TOOLS_BASE ?= magician:apple-3651d3eb50-r1
CONTAINER_TOOLS_IMAGE ?= magician:tools-ready
CONTAINER_TOOLS_ARGS ?=
build-container-tools-image:
	python3 scripts/build-container-layer.py --engine "$(CONTAINER_TOOLS_ENGINE)" -- $(CONTAINER_TOOLS_ARGS) --file containers/tools/Dockerfile \
		--build-arg "RUNTIME_BASE=$(CONTAINER_TOOLS_BASE)" --tag "$(CONTAINER_TOOLS_IMAGE)" .

# Explicit external context holds only qualified Linux executables and sources.
CONTAINER_PREBUILT_TOOLS_CONTEXT ?=
.PHONY: build-container-prebuilt-tools-image
build-container-prebuilt-tools-image:
	@test -n "$(CONTAINER_PREBUILT_TOOLS_CONTEXT)" || { echo "Set CONTAINER_PREBUILT_TOOLS_CONTEXT to an isolated prepared context"; exit 1; }
	python3 scripts/build-container-layer.py --engine "$(CONTAINER_TOOLS_ENGINE)" -- $(CONTAINER_TOOLS_ARGS) --file "$(CONTAINER_PREBUILT_TOOLS_CONTEXT)/containers/tools/prebuilt.Dockerfile" \
		--build-arg "RUNTIME_BASE=$(CONTAINER_TOOLS_BASE)" --tag "$(CONTAINER_TOOLS_IMAGE)" "$(CONTAINER_PREBUILT_TOOLS_CONTEXT)"

CONTAINER_KEYRING_ENGINE ?= container
CONTAINER_KEYRING_BASE ?= magician:runtime-base
CONTAINER_KEYRING_IMAGE ?= magician:keyring-ready
CONTAINER_KEYRING_ARGS ?=
CONTAINER_KEYRING_CONTEXT ?= .
.PHONY: build-container-keyring-image
build-container-keyring-image:
	python3 scripts/build-container-layer.py --engine "$(CONTAINER_KEYRING_ENGINE)" -- $(CONTAINER_KEYRING_ARGS) --file "$(CONTAINER_KEYRING_CONTEXT)/containers/keyring/Dockerfile" \
		--build-arg "RUNTIME_BASE=$(CONTAINER_KEYRING_BASE)" --tag "$(CONTAINER_KEYRING_IMAGE)" "$(CONTAINER_KEYRING_CONTEXT)"

# Local prebuild pipeline: packaging never invokes Cargo or BuildKit.
CONTAINER_OCI_ARGS ?= --help
CONTAINER_ARTIFACT_ARGS ?= --help
CONTAINER_SDK_ENGINE ?= container
CONTAINER_SDK_IMAGE ?= magician-build-sdk:rust1.92-bookworm
# Cached-layer wrapper repeats inspected limits; omitted Apple flags would use
# system defaults and can recreate the builder. Artifact VMs default to 2 CPUs.
CONTAINER_SDK_ARGS ?=
.PHONY: container-oci build-container-sdk prebuild-container-artifacts test-container-oci
container-oci:
	python3 scripts/container-oci.py $(CONTAINER_OCI_ARGS)

build-container-sdk:
	python3 scripts/build-container-layer.py --engine "$(CONTAINER_SDK_ENGINE)" -- $(CONTAINER_SDK_ARGS) --tag "$(CONTAINER_SDK_IMAGE)" \
		--file containers/sdk/Dockerfile containers/sdk

prebuild-container-artifacts:
	python3 scripts/build-container-artifacts.py $(CONTAINER_ARTIFACT_ARGS)

.PHONY: prepare-container-image prepare-container-image-local test-prepare-container-image
prepare-container-image:
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/prepare-container-image.py $(ARGS)

CONTAINER_LOCAL_BASE_IMAGE ?= magician:qualified-runtime
prepare-container-image-local:
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/prepare-container-image.py \
		--initial-base-image "$(CONTAINER_LOCAL_BASE_IMAGE)" $(ARGS)

test-prepare-container-image:
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test-prepare-container-image.py

test-container-oci:
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test-container-oci.py

# Build Docker image locally (loads into local daemon)
build-container:
	@echo "🐳 Building container image $(CONTAINER_IMAGE):$(CONTAINER_TAG)..."
	docker buildx build --load -t $(CONTAINER_IMAGE):$(CONTAINER_TAG) .
	@echo "✅ Image built: $(CONTAINER_IMAGE):$(CONTAINER_TAG)"

# Build multi-arch image (amd64 + arm64)
build-container-multiarch:
	@echo "🐳 Building multi-arch container image..."
	docker buildx build --platform linux/amd64,linux/arm64 \
		-t $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(CONTAINER_TAG) .
	@echo "✅ Multi-arch image built"

# Push image to container registry
push-container:
	@echo "🚀 Pushing $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(CONTAINER_TAG)..."
	docker buildx build --platform linux/amd64,linux/arm64 \
		-t $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(CONTAINER_TAG) --push .
	@echo "✅ Pushed to $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(CONTAINER_TAG)"

# Run container for local testing
# Ollama runs on the HOST (see `setup-ollama`).
# --network host lets the container reach the host's generation Ollama (:11434)
# and dedicated embedding Ollama (:11435) on localhost, matching the config.
# The host runtime root (override with MAGICIAN_RUNTIME_ROOT or MAGICIAN_ROOT_DIR) is mounted to /data,
# where the container's MAGICIAN_ROOT_DIR points; the seed is baked into the image.
# NOTE: --network host is for a LINUX host. On a macOS Docker Desktop host, run
# magician natively instead (the VM's localhost is not the host's).
# This is the DEV-LOOP variant (magician-dev); the installer's phase_run owns the
# released container lifecycle (under CONTAINER_NAME=magician / $IMAGE_REF).
run-container:
	@runtime="$${CONTAINER_RUNTIME:-auto}"; \
	if [ "$$runtime" = auto ] || [ -z "$$runtime" ]; then \
		runtime=docker; \
		if [ "$$(uname -s)" = Darwin ] && [ "$$(uname -m)" = arm64 ]; then \
			major="$$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)"; \
			if [ -n "$$major" ] && [ "$$major" -ge 26 ] 2>/dev/null; then \
				runtime=apple-container; \
			fi; \
		fi; \
	fi; \
	if [ "$$runtime" = apple-container ] && ! command -v container >/dev/null 2>&1; then \
		echo "WARN: container CLI not found; falling back to docker runtime."; \
		runtime=docker; \
	fi; \
	echo "🐳 Starting $(CONTAINER_NAME) ($$runtime, runtime-aware)..."; \
	echo "   - Magician :3002   Magicutor :3003"; \
	echo "   - Runtime root: $${MAGICIAN_RUNTIME_ROOT:-$${MAGICIAN_ROOT_DIR:-$$HOME/MagicianNotes}} → /data"; \
	echo "   - Host services: Ollama generation :11434, embeddings :11435"; \
	runtime_root="$${MAGICIAN_RUNTIME_ROOT:-$${MAGICIAN_ROOT_DIR:-$$HOME/MagicianNotes}}"; \
		mkdir -p "$$runtime_root"; \
		if [ ! -e "$$runtime_root/magician-config.yaml" ]; then \
			if [ -f magician-config.yaml ]; then \
				cp magician-config.yaml "$$runtime_root/magician-config.yaml"; \
				echo "   - Seeded $$runtime_root/magician-config.yaml from repo-root magician-config.yaml"; \
			else \
				echo "   - WARNING: no config seed found for $$runtime_root/magician-config.yaml"; \
			fi; \
		fi; \
		if [ ! -e "$$runtime_root/llm-router.yaml" ]; then \
			if [ -f llm-router.yaml ]; then \
				cp llm-router.yaml "$$runtime_root/llm-router.yaml"; \
				echo "   - Seeded $$runtime_root/llm-router.yaml from repo-root llm-router.yaml"; \
			else \
				echo "   - WARNING: no router-tables seed found; the config cannot load without it"; \
			fi; \
		fi; \
	if [ "$$runtime" = apple-container ]; then \
		if ! command -v container >/dev/null 2>&1; then \
			echo "ERROR: container CLI not found for apple-container runtime." >&2; \
			exit 1; \
		fi; \
		if ! container system dns list --quiet 2>/dev/null | grep -Fxq "host.container.internal"; then \
			echo "WARN: host.container.internal is not configured; configure via setup-container-runtime.sh for host service forwarding."; \
		fi; \
		container run -d --name $(CONTAINER_NAME) \
			-v "$${runtime_root}:/data" \
			-p 3002:3002 -p 3003:3003 \
			-e MAGICIAN_CONTAINER_HOST=host.container.internal \
			-e MAGICIAN_HOST_GATEWAY_URL=http://host.container.internal:3017 \
			$(CONTAINER_IMAGE):$(CONTAINER_TAG); \
	else \
		if [ "$$(uname -s)" = Darwin ]; then \
			docker run -d --name $(CONTAINER_NAME) \
				-v "$${runtime_root}:/data" \
				-p 3002:3002 -p 3003:3003 \
				-e MAGICIAN_CONTAINER_HOST=host.docker.internal \
				-e MAGICIAN_HOST_GATEWAY_URL=http://host.docker.internal:3017 \
				$(CONTAINER_IMAGE):$(CONTAINER_TAG); \
		else \
			docker run -d --name $(CONTAINER_NAME) \
				--network host \
				-v "$${runtime_root}:/data" \
				$(CONTAINER_IMAGE):$(CONTAINER_TAG); \
		fi; \
	fi
	@echo "✅ Container started: $(CONTAINER_NAME)"

# Stop and remove the dev container (dev-loop variant: magician-dev). The
# installer's phase_run owns the released lifecycle; pass MAGICIAN_CONTAINER_NAME
# (and/or MAGICIAN_IMAGE_REF) to clean up the installer's container with this target.
stop-container:
	@echo "⏹️  Stopping $(CONTAINER_NAME)..."
	@runtime="$${CONTAINER_RUNTIME:-auto}"; \
	if [ "$$runtime" = auto ] || [ -z "$$runtime" ]; then \
		runtime=docker; \
		if [ "$$(uname -s)" = Darwin ] && [ "$$(uname -m)" = arm64 ]; then \
			major="$$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)"; \
			if [ -n "$$major" ] && [ "$$major" -ge 26 ] 2>/dev/null; then \
				runtime=apple-container; \
			fi; \
		fi; \
	fi; \
	if [ "$$runtime" = apple-container ] && ! command -v container >/dev/null 2>&1; then \
		echo "WARN: container CLI not found; falling back to docker runtime."; \
		runtime=docker; \
	fi; \
	if [ "$$runtime" = apple-container ]; then \
		if command -v container >/dev/null 2>&1; then \
			container stop $(CONTAINER_NAME) >/dev/null 2>&1 || true; \
			container rm $(CONTAINER_NAME) >/dev/null 2>&1 || true; \
		else \
			echo "WARN: container CLI not found; skipping container stop/rm." >&2; \
		fi; \
	else \
		docker stop $(CONTAINER_NAME) >/dev/null 2>&1 || true; \
		docker rm $(CONTAINER_NAME) >/dev/null 2>&1 || true; \
	fi
	@echo "✅ Container removed: $(CONTAINER_NAME)"

# Run container integration tests (builds image first via dependency)
test-container: build-container  ## Run container integration tests
	./scripts/test-container.sh --skip-build --runtime docker --image $(CONTAINER_IMAGE):$(CONTAINER_TAG)

# Build from scratch and run container integration tests
test-container-full:  ## Build and test container from scratch
	./scripts/test-container.sh --runtime $(CONTAINER_RUNTIME) --image $(CONTAINER_IMAGE):$(CONTAINER_TAG)

test-container-tooling: test-container-integration-harness test-container-oci test-container-readiness test-prepare-container-image test-linux-keyring test-container-keyring-provisioning test-mobile-connectivity  ## Provider-free release qualification tooling regressions
	bash scripts/test-container-entrypoint.sh
	bash scripts/test-composed-installer.sh
	bash scripts/test-container-qualification.sh
	bash scripts/test-qualify-container-e2e.sh
	bash scripts/test-measure-container-image.sh
	bash -n scripts/test-container.sh scripts/qualify-container-e2e.sh scripts/measure-container-image.sh scripts/setup-container-runtime.sh scripts/install.sh

.PHONY: qualify-container-integration test-container-integration-harness
CONTAINER_INTEGRATION_ARGS ?=
qualify-container-integration:  ## Attach only: never build, install, or restart services
	python3 scripts/qualify-container-integration.py $(CONTAINER_INTEGRATION_ARGS)

test-container-integration-harness:
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_container_integration.py

.PHONY: test-container-readiness
test-container-readiness:
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test-container-readiness.py

.PHONY: test-linux-keyring
test-linux-keyring:  ## Real Secret Service lifecycle on Linux; input checks elsewhere
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test-linux-keyring.py

.PHONY: test-container-keyring-provisioning
test-container-keyring-provisioning:  ## Private host custody and safe recreation preflight
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test-container-keyring-provisioning.py

.PHONY: qualify-mobile-connectivity test-mobile-connectivity
MOBILE_CONNECTIVITY_ARGS ?=
qualify-mobile-connectivity:  ## Explicit synthetic pairing phases on an isolated running backend
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/qualify-mobile-connectivity.py $(MOBILE_CONNECTIVITY_ARGS)

test-mobile-connectivity:  ## Offline credential routing and probe isolation checks
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test-mobile-connectivity.py

qualify-container-release:  ## Qualify an existing image and retain JSON evidence
	./scripts/test-container.sh --skip-build --runtime $(CONTAINER_RUNTIME) \
		--image $(CONTAINER_IMAGE):$(CONTAINER_TAG) \
		--report $(CONTAINER_QUALIFICATION_REPORT) \
		$(if $(CONTAINER_MAX_COLD_START_MS),--max-cold-start-ms $(CONTAINER_MAX_COLD_START_MS),)

qualify-container-e2e:  ## Build and qualify a disposable real local container
	./scripts/qualify-container-e2e.sh \
		--runtime '$(CONTAINER_E2E_RUNTIME)' \
		--image '$(CONTAINER_E2E_IMAGE)' \
		$(if $(CONTAINER_E2E_REPORT_DIR),--report-dir '$(CONTAINER_E2E_REPORT_DIR)',)

qualify-container-e2e-live:  ## Install and qualify the live personal container stack
	@test "$(CONTAINER_E2E_LIVE_CONFIRM)" = "1" || { \
		echo "Set CONTAINER_E2E_LIVE_CONFIRM=1; this stops the current dev stack and installs against $(CONTAINER_E2E_ROOT)." >&2; \
		exit 2; \
	}
	./scripts/qualify-container-e2e.sh --live --yes \
		--runtime '$(CONTAINER_E2E_RUNTIME)' \
		--image '$(CONTAINER_E2E_IMAGE)' \
		--root '$(CONTAINER_E2E_ROOT)' \
		$(if $(CONTAINER_E2E_REPORT_DIR),--report-dir '$(CONTAINER_E2E_REPORT_DIR)',)

measure-container-image:  ## Measure remote multi-arch image size and enforce optional budgets
	./scripts/measure-container-image.sh \
		--image $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(CONTAINER_TAG) \
		--output $(CONTAINER_IMAGE_REPORT) \
		$(if $(CONTAINER_MAX_COMPRESSED_BYTES),--max-compressed-bytes $(CONTAINER_MAX_COMPRESSED_BYTES),) \
		$(if $(CONTAINER_MAX_EXPANDED_BYTES),--max-expanded-bytes $(CONTAINER_MAX_EXPANDED_BYTES),)

# --- Release targets ---

export-apple-signing-setup:
	@if [ -z "$(OUTPUT)" ]; then echo "Usage: make export-apple-signing-setup OUTPUT=/path/outside/repo/magican-release.magican-signing.enc [ARGS=...]" >&2; exit 1; fi
	@python3 scripts/apple_signing_transfer.py export --output "$(OUTPUT)" $(ARGS)

verify-apple-signing-bundle:
	@if [ -z "$(BUNDLE)" ]; then echo "Usage: make verify-apple-signing-bundle BUNDLE=/path/outside/repo/magican-release.magican-signing.enc [ARGS=...]" >&2; exit 1; fi
	@python3 scripts/apple_signing_transfer.py verify --bundle "$(BUNDLE)" $(ARGS)

import-apple-signing-setup:
	@if [ -z "$(BUNDLE)" ]; then echo "Usage: make import-apple-signing-setup BUNDLE=/path/outside/repo/magican-release.magican-signing.enc [ARGS=--dry-run|--replace]" >&2; exit 1; fi
	@python3 scripts/apple_signing_transfer.py import --bundle "$(BUNDLE)" $(ARGS)

verify-apple-signing-setup:
	@python3 scripts/apple_signing_transfer.py status $(ARGS)

.PHONY: print-cargo-target-dir
print-cargo-target-dir:
	@printf '%s\n' "$(CARGO_TARGET_DIR)"

release-desktop-signed:
	@python3 scripts/release_desktop_signed.py --jobs "$(or $(JOBS),2)" $(ARGS)

# Version from git tag or Cargo.toml
RELEASE_VERSION ?= $(shell git describe --tags --abbrev=0 2>/dev/null || echo "0.1.0")

# Release: build and push container image (multi-arch, tagged with version)
release-container:
	@echo "🚀 Releasing container image $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(RELEASE_VERSION)..."
	docker buildx build --platform linux/amd64,linux/arm64 \
		-t $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(RELEASE_VERSION) \
		-t $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):latest \
		--push .
	@echo "✅ Container released: $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(RELEASE_VERSION)"

# Release: build desktop app for the current platform
release-desktop: setup-desktop-pnpm setup-desktop-vosk
	@echo "🖥️  Building desktop app release..."
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		$(MAKE) --no-print-directory build-macos-audio-engine-release || exit $$?; \
		cd desktop && $(PNPM) install && $(PNPM) tauri build --config src-tauri/tauri.macos-audio.conf.json --features native-wake -- --locked; \
	else \
		cd desktop && $(PNPM) install && $(PNPM) tauri build -- --locked; \
	fi
	@echo ""
	@echo "✅ Desktop app built. Artifacts:"
	@for dir in "$(CARGO_TARGET_DIR)/release/bundle" "desktop/src-tauri/target/release/bundle"; do \
		if [ -d "$$dir" ]; then \
			find "$$dir" -type f \( -name "*.dmg" -o -name "*.AppImage" -o -name "*.exe" -o -name "*.msi" -o -name "*.tar.gz" -o -name "*.nsis.zip" \) 2>/dev/null; \
		fi; \
	done | sort
	@echo ""
	@echo "For cross-platform builds, use CI: git tag vX.Y.Z && git push --tags"

# Release: build desktop for a specific target (e.g., make release-desktop-target TARGET=aarch64-apple-darwin)
release-desktop-target: setup-desktop-pnpm setup-desktop-vosk
	@if [ -z "$(TARGET)" ]; then \
		echo "Usage: make release-desktop-target TARGET=<rust-target>"; \
		echo ""; \
		echo "Supported targets:"; \
		echo "  aarch64-apple-darwin      macOS Apple Silicon"; \
		echo "  x86_64-unknown-linux-gnu  Linux x86_64"; \
		echo "  x86_64-pc-windows-msvc    Windows x86_64"; \
		exit 1; \
	fi
	@echo "🖥️  Building desktop app for $(TARGET)..."
	@case "$(TARGET)" in \
		*-apple-darwin*) \
			$(MAKE) build-macos-audio-engine-release SWIFT_AUDIO_ENGINE_TARGET_FLAGS="--triple $(TARGET)"; \
			cd desktop && $(PNPM) install && $(PNPM) tauri build --config src-tauri/tauri.macos-audio.conf.json --target $(TARGET) --features native-wake -- --locked; \
			;; \
		*) cd desktop && $(PNPM) install && $(PNPM) tauri build --target $(TARGET) -- --locked ;; \
	esac
	@echo "✅ Desktop app built for $(TARGET)"
	@for dir in "$(CARGO_TARGET_DIR)/$(TARGET)/release/bundle" "desktop/src-tauri/target/$(TARGET)/release/bundle"; do \
		if [ -d "$$dir" ]; then \
			find "$$dir" -type f \( -name "*.dmg" -o -name "*.AppImage" -o -name "*.exe" -o -name "*.msi" -o -name "*.tar.gz" -o -name "*.nsis.zip" \) 2>/dev/null; \
		fi; \
	done | sort

# Release: full release (container + desktop for current platform)
# The native backend artifact: the one thing release-container and
# release-desktop between them do not produce, and the reason MODE=user
# FLOW=local has always been refused.
# Removing an install is the same job whichever route created it, so there is
# one uninstaller and this only points at it.
.PHONY: uninstall-magician
uninstall-magician: ## Stop services and remove an install (--dry-run first; your data is kept unless you ask)
	@bash scripts/package-uninstaller.sh $(ARGS)

.PHONY: package-release
package-release: ## Package built backend binaries + seed into a versioned tarball (build-native-backend-release first)
	@bash scripts/package-release.sh

.PHONY: build-native-backend-release
build-native-backend-release: ## Build only the three services needed by the native Desktop package
	@$(MAKE) --no-print-directory check-protoc
	@if printf '%s' "$(NATIVE_BACKEND_TARGET)" | grep -q -- '-windows-'; then \
		env -u MAKEFLAGS -u MFLAGS cargo build --release --locked --target "$(NATIVE_BACKEND_TARGET)" \
			-p magician-bin -p magicutor -p magic-supervisor; \
	else \
		cargo build --release --locked $(if $(NATIVE_BACKEND_TARGET),--target "$(NATIVE_BACKEND_TARGET)",) \
			-p magician-bin -p magicutor -p magic-supervisor; \
	fi

.PHONY: stage-desktop-native-backend
stage-desktop-native-backend: ## Stage a built native backend package inside the Desktop installer (does not build)
	@bash scripts/stage-desktop-native-backend.sh

.PHONY: test-native-package
test-native-package: ## Verify the native backend package, installer, and Desktop staging contract with fixtures
	@bash scripts/test-native-package.sh
	@if [ "$$(uname -s | cut -c1-5)" = "MINGW" ] || [ "$$(uname -s | cut -c1-4)" = "MSYS" ] || [ "$$(uname -s | cut -c1-6)" = "CYGWIN" ]; then \
		powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/test-native-package.ps1; \
	fi

.PHONY: release-desktop-native
release-desktop-native: ## Build backend, stage its native package, then build the Desktop installer
	@$(MAKE) --no-print-directory build-native-backend-release
	@$(MAKE) --no-print-directory stage-desktop-native-backend
	@MAGICIAN_REQUIRE_NATIVE_BACKEND=1 $(MAKE) --no-print-directory release-desktop
	@$(MAKE) --no-print-directory verify-desktop-native-release

.PHONY: verify-desktop-native-release
verify-desktop-native-release: ## Verify the native backend inside a finished local Desktop bundle (BUNDLE=... overrides discovery)
	@bundle="$(BUNDLE)"; platform=""; \
	if [ -z "$$bundle" ]; then \
		case "$$(uname -s)" in \
			Darwin) platform=macos; bundle="$$(find "$(CARGO_TARGET_DIR)/release/bundle/macos" "desktop/src-tauri/target/release/bundle/macos" -maxdepth 2 -name '*.app' -type d -print -quit 2>/dev/null)" ;; \
			Linux) platform=linux; bundle="$$(find "$(CARGO_TARGET_DIR)/release/bundle/appimage" "desktop/src-tauri/target/release/bundle/appimage" -maxdepth 1 -name '*.AppImage' -type f -print -quit 2>/dev/null)" ;; \
			MINGW*|MSYS*|CYGWIN*) platform=windows; bundle="$$(find "$(CARGO_TARGET_DIR)/release/bundle/nsis" "desktop/src-tauri/target/release/bundle/nsis" -maxdepth 1 -name '*-setup.exe' -type f -print -quit 2>/dev/null)" ;; \
			*) echo "Native Desktop bundle verification is supported on macOS, Linux, and Windows" >&2; exit 1 ;; \
		esac; \
	else \
		case "$$(uname -s)" in Darwin) platform=macos ;; Linux) platform=linux ;; MINGW*|MSYS*|CYGWIN*) platform=windows ;; *) exit 1 ;; esac; \
	fi; \
	test -n "$$bundle" || { echo "No finished Desktop bundle was found" >&2; exit 1; }; \
	bash scripts/verify-desktop-native-bundle.sh --bundle "$$bundle" --platform "$$platform"

release-all: release-container release-desktop
	@echo ""
	@echo "🎉 Full release complete!"
	@echo "   Container: $(CONTAINER_REGISTRY)/$(CONTAINER_IMAGE):$(RELEASE_VERSION)"
	@echo "   Desktop: see artifacts above"

# Generate Tauri signing keypair (one-time setup)
generate-signing-key: setup-desktop-pnpm
	@echo "🔑 Generating Tauri signing keypair..."
	@echo "   This will create a keypair for signing desktop app updates."
	@echo "   Store the PRIVATE key in GitHub Secrets as TAURI_SIGNING_PRIVATE_KEY"
	@echo "   Store the PUBLIC key in the GitHub Actions variable TAURI_UPDATER_PUBLIC_KEY"
	@echo ""
	cd desktop && $(PNPM) tauri signer generate -w ~/.tauri/magician.key
	@echo ""
	@echo "✅ Keypair generated!"
	@echo "   Private key: ~/.tauri/magician.key"
	@echo "   Public key: ~/.tauri/magician.key.pub"
	@echo ""
	@echo "Next steps:"
	@echo "  1. cat ~/.tauri/magician.key.pub   → add as repository variable TAURI_UPDATER_PUBLIC_KEY"
	@echo "  2. cat ~/.tauri/magician.key       → add as GitHub Secret TAURI_SIGNING_PRIVATE_KEY"

# --- Dev workflow targets (local update testing) ---

# Dev signing key location
DEV_SIGNING_KEY ?= $(HOME)/.tauri/magician.key
DEV_SIGNING_PUBKEY ?= $(HOME)/.tauri/magician.key.pub
DEV_UPDATE_PORT ?= 8432

# Ensure `make` itself is installed and reachable from the system
# default PATH that GUI apps inherit. The tray's Services submenu
# shells out to `make restart-magician` etc., and Finder-launched
# Tauri apps don't see the user's shell PATH — only the OS default.
# Delegated to a shell script because the Makefile can't bootstrap
# `make` itself if it's missing.
ensure-make:
	@./scripts/ensure_make.sh

# Restart magic-supervisor by composing stop → run. No native target
# exists because the supervisor is a long-running blocking process;
# we sequence the two halves explicitly. Used by the tray's Services
# submenu.
restart-supervisor:
	@$(MAKE) --no-print-directory stop-supervisor
	@$(MAKE) --no-print-directory run-supervisor

# Restart the unified-ui dev server. Composes stop → run for the
# same reason as restart-supervisor. Used by the tray's Services
# submenu.
restart-ui-dev:
	@$(MAKE) --no-print-directory stop-ui-dev
	@$(MAKE) --no-print-directory run-ui-dev

# One-time setup: generate signing key + install desktop deps
dev-desktop-setup: ensure-make setup-desktop-pnpm
	@echo "Setting up local dev environment for desktop updates..."
	@echo ""
	@if [ -f "$(DEV_SIGNING_KEY)" ]; then \
		echo "Signing key already exists: $(DEV_SIGNING_KEY)"; \
		echo "Public key: $(DEV_SIGNING_PUBKEY)"; \
	else \
		echo "Generating signing keypair..."; \
		mkdir -p $(HOME)/.tauri; \
		cd desktop && $(PNPM) install && $(PNPM) tauri signer generate -w $(DEV_SIGNING_KEY); \
		echo ""; \
		echo "Keypair generated:"; \
		echo "  Private: $(DEV_SIGNING_KEY)"; \
		echo "  Public:  $(DEV_SIGNING_PUBKEY)"; \
	fi
	@echo ""
	@echo "Installing desktop dependencies..."
	cd desktop && $(PNPM) install
	@echo ""
	@echo "Setup complete! Next: make dev-desktop-build"

# Build desktop app with local signing key and local update endpoint
dev-desktop-build: setup-desktop-pnpm setup-desktop-vosk build-macos-audio-engine-release build-macos-speech-helper-release
	@if [ ! -f "$(DEV_SIGNING_KEY)" ]; then \
		echo "ERROR: Signing key not found at $(DEV_SIGNING_KEY)"; \
		echo "Run 'make dev-desktop-setup' first."; \
		exit 1; \
	fi
	@if [ ! -f "$(DEV_SIGNING_PUBKEY)" ]; then \
		echo "ERROR: Public key not found at $(DEV_SIGNING_PUBKEY)"; \
		echo "Run 'make dev-desktop-setup' first."; \
		exit 1; \
	fi
	@echo "Building desktop app with local update endpoint..."
	@echo "  Signing key: $(DEV_SIGNING_KEY)"
	@echo "  Update endpoint: http://localhost:$(DEV_UPDATE_PORT)/latest.json"
	@echo ""
	@PUBKEY=$$(cat "$(DEV_SIGNING_PUBKEY)") && \
	TAURI_SIGNING_PRIVATE_KEY=$$(cat "$(DEV_SIGNING_KEY)") \
	TAURI_CONFIG="{\"plugins\":{\"updater\":{\"pubkey\":\"$$PUBKEY\",\"endpoints\":[\"http://localhost:$(DEV_UPDATE_PORT)/latest.json\"]}}}" \
	bash -c 'cd desktop && $(PNPM) install && $(PNPM) tauri build --config src-tauri/tauri.macos-audio.conf.json --features native-wake'
	@echo ""
	@echo "Build complete! Artifacts:"
	@for dir in "$(CARGO_TARGET_DIR)/release/bundle" "desktop/src-tauri/target/release/bundle"; do \
		if [ -d "$$dir" ]; then \
			find "$$dir" -type f \( -name "*.dmg" -o -name "*.app.tar.gz" -o -name "*.app.tar.gz.sig" \) 2>/dev/null; \
		fi; \
	done | sort
	@echo ""
	@echo "To install: open the .dmg above and drag to /Applications"
	@echo "To test updates: bump version in desktop/src-tauri/tauri.conf.json, rebuild, then 'make dev-update-server'"

# Run local update server serving latest.json + signed artifacts
dev-update-server:
	@if [ ! -f "$(DEV_SIGNING_KEY)" ]; then \
		echo "ERROR: Signing key not found. Run 'make dev-desktop-setup' first."; \
		exit 1; \
	fi
	./scripts/local-update-server.sh $(DEV_UPDATE_PORT)

# Rebuild container image and cycle the running container to pick up new binaries.
# This rebuilds the Docker image (compiles Rust inside the container), then
# stops the old container, removes it, and starts a new one from the fresh image.
#
# Docker layer caching makes incremental rebuilds fast — only changed Rust source
# files are recompiled, not all dependencies.
#
# For faster iteration on service code (no container), use: make run-supervisor
dev-container-rebuild:
	@echo "Rebuilding container image with latest service binaries..."
	@runtime="$${CONTAINER_RUNTIME:-auto}"; \
	if [ "$$runtime" = auto ] || [ -z "$$runtime" ]; then \
		runtime=docker; \
		if [ "$$(uname -s)" = Darwin ] && [ "$$(uname -m)" = arm64 ]; then \
			major="$$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)"; \
			if [ -n "$$major" ] && [ "$$major" -ge 26 ] 2>/dev/null; then \
				runtime=apple-container; \
			fi; \
		fi; \
	fi; \
	if [ "$$runtime" = apple-container ] && ! command -v container >/dev/null 2>&1; then \
		echo "WARN: container CLI not found; falling back to docker runtime."; \
		runtime=docker; \
	fi; \
	case "$$runtime" in \
		apple-container) \
			if ! command -v container >/dev/null 2>&1; then \
				echo "ERROR: container CLI not found." >&2; \
				exit 1; \
			fi; \
			container build -t $(CONTAINER_IMAGE):latest . || exit 1; \
			;; \
		*) \
			docker buildx build --load -t $(CONTAINER_IMAGE):latest . || exit 1; \
			;; \
	esac; \
	echo "Cycling the container to pick up new binaries..."; \
	if [ "$$runtime" = apple-container ]; then \
		container stop magician 2>/dev/null || true; \
		container rm magician 2>/dev/null || true; \
	else \
		docker stop magician 2>/dev/null || true; \
		docker rm magician 2>/dev/null || true; \
	fi; \
	runtime_root="$${MAGICIAN_RUNTIME_ROOT:-$${MAGICIAN_ROOT_DIR:-$$HOME/MagicianNotes}}"; \
	mkdir -p "$$runtime_root"; \
	if [ ! -e "$$runtime_root/magician-config.yaml" ]; then \
		if [ -f magician-config.yaml ]; then \
			cp magician-config.yaml "$$runtime_root/magician-config.yaml"; \
			echo "Seeded $$runtime_root/magician-config.yaml from repo-root magician-config.yaml"; \
		else \
			echo "WARNING: no config seed found for $$runtime_root/magician-config.yaml"; \
		fi; \
	fi; \
	if [ ! -e "$$runtime_root/llm-router.yaml" ]; then \
		if [ -f llm-router.yaml ]; then \
			cp llm-router.yaml "$$runtime_root/llm-router.yaml"; \
			echo "Seeded $$runtime_root/llm-router.yaml from repo-root llm-router.yaml"; \
		else \
			echo "WARNING: no router-tables seed found; the config cannot load without it"; \
		fi; \
	fi; \
	if [ "$$runtime" = apple-container ]; then \
		if ! command -v container >/dev/null 2>&1; then \
			echo "ERROR: container CLI not found for run phase." >&2; \
			exit 1; \
		fi; \
		container run -d --name magician \
			-v "$$runtime_root:/data" \
			-p 3002:3002 -p 3003:3003 \
			-e MAGICIAN_CONTAINER_HOST=host.container.internal \
			-e MAGICIAN_HOST_GATEWAY_URL=http://host.container.internal:3017 \
			$(CONTAINER_IMAGE):latest; \
	else \
		if [ "$$(uname -s)" = Darwin ]; then \
			docker run -d --name magician \
				-v "$$runtime_root:/data" \
				-e MAGICIAN_OLLAMA_BASE_URL=http://host.docker.internal:11434 \
				-e MAGICIAN_MEMORY_OLLAMA_URL=http://host.docker.internal:11435 \
				-e MAGICIAN_HOST_GATEWAY_URL=http://host.docker.internal:3017 \
				-p 3002:3002 -p 3003:3003 \
				$(CONTAINER_IMAGE):latest; \
		else \
			docker run -d --name magician \
				--network host \
				-v "$$runtime_root:/data" \
				$(CONTAINER_IMAGE):latest; \
		fi; \
	fi; \
	echo ""; \
	echo "Done! Container restarted with updated binaries."; \
	echo "  magician:  http://localhost:3002/health"; \
	echo "  magicutor: http://localhost:3003/health"; \
	echo "  Ollama generation reached via host.docker.internal:11434;"; \
	echo "  dedicated embeddings reached via host.docker.internal:11435."; \
	echo "    Summary profiles still need their config URL pointed at :11434, or run native."
	@echo ""
	@echo "If the desktop tray app is running, it will detect the container"
	@echo "is healthy within ~5 seconds and update its tray state."
	@echo ""
	@echo "For faster iteration without containers: make run-supervisor"

# --- Cleanup targets ---

# Cleanup: remove everything Magician installed (tools + data + container)
cleanup:
	./scripts/cleanup.sh --tools-and-data

# Cleanup: remove tools + container only, keep user data
cleanup-tools:
	./scripts/cleanup.sh --only-tools

# Cleanup: remove data directories only
cleanup-data:
	./scripts/cleanup.sh --only-data

# Cleanup: dry run — show what would be removed
cleanup-dry-run:
	./scripts/cleanup.sh --tools-and-data --dry-run


# --- iOS Companion (Magios) ---
# Magios UNIT tests + Xcode source coverage. This is the BLOCKING iOS gate that
# `make test` runs. It deliberately SKIPS the XCUITest smoke suite (MagiosUITests)
# — those live in the non-blocking `test-ios-ui` lane below.
#
# Why the split: MagiosUITests pass every assertion and pass in isolation, but the
# app is reproducibly slow to TERMINATE for some tests under the accumulated load
# of running the full suite first. XCUITest leaves the app lingering ~30s after the
# test body, then kills it at session cleanup; under `make test` load that late
# SIGTERM lands inside the framework's per-test window and is misattributed as
# "Test crashed with signal term." That's framework/timing flakiness, not an app
# defect, so it must not block the gate. The UI smoke tests still run on demand via
# `make test-ios-ui` (with retry + offline `--ui-test` launch), and unit-test
# coverage — the reason coverage is enabled here — is unaffected by the split.
# Override IOS_TEST_DESTINATION when a different installed simulator is desired.
IOS_TEST_DESTINATION ?= platform=iOS Simulator,name=iPhone 17 Pro
IOS_TEST_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/ios
IOS_TEST_WORK_ROOT ?= $(CARGO_TARGET_DIR)/ios-tests
IOS_UI_TEST_DERIVED_DATA ?= $(CARGO_TARGET_DIR)/ios-ui-tests
IOS_REALTIME_LOCAL_TRANSCRIPT_DERIVED_DATA ?= $(CARGO_TARGET_DIR)/ios-realtime-local-transcript-tests
# Plain (non-test) simulator builds need the same treatment: without an explicit
# derived-data path xcodebuild falls back to SYMROOT's default of
# `$(PROJECT_DIR)/build`, which puts build output back inside the repo.
MAGIOS_DEBUG_DERIVED_DATA ?= $(CARGO_TARGET_DIR)/magios-debug
# Xcode's host services can fail before XCTest starts when the per-user Darwin
# cache/FSEvents service is transiently unavailable. Every attempt gets an
# isolated writable CF home/temp/cache/DerivedData under this SSD root and the
# runner enters the logged-in user bootstrap when available. Only that exact
# pre-test SIGABRT class is retried, at most once and only without XCTest
# activity; a missing Aqua bootstrap disables the futile retry. Report mutation
# is serialized, and aggregate runs consume a caller-correlated immutable report.
# Assertion/test failures are never retried.
IOS_TEST_INFRASTRUCTURE_RETRIES ?= 1
IOS_TEST_LOCK_TIMEOUT ?= 5
IOS_TEST_DIAGNOSTIC_TIMEOUT ?= 30
.PHONY: test-ios
test-concurrent-voice-ios: verify-magios-vosk
	@xcodebuild -jobs 2 -project magios/Magios.xcodeproj -scheme Magios -configuration Debug \
		-destination '$(IOS_TEST_DESTINATION)' -derivedDataPath '$(MAGIOS_DEBUG_DERIVED_DATA)' \
		-clonedSourcePackagesDirPath '$(CARGO_TARGET_DIR)/magios-spm' \
		-only-testing:MagiosTests/ConcurrentVoiceCoordinatorTests \
		-parallel-testing-enabled NO -enableCodeCoverage NO \
		CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=YES -allowProvisioningUpdates test

.PHONY: test-concurrent-voice-ios
test-ios-live-concurrent-voice: verify-magios-vosk
	@test -n '$(MAGIOS_DEVICE_ID)' || { echo 'Set MAGIOS_DEVICE_ID to the enrolled physical iPhone.'; exit 1; }
	@TEST_RUNNER_MAGIOS_LIVE_CONCURRENT_VOICE=1 xcodebuild -jobs 2 \
		-project magios/Magios.xcodeproj -scheme Magios -configuration Debug \
		-destination 'platform=iOS,id=$(MAGIOS_DEVICE_ID)' \
		-derivedDataPath '$(CARGO_TARGET_DIR)/magios-device' \
		-clonedSourcePackagesDirPath '$(CARGO_TARGET_DIR)/magios-spm' \
		-only-testing:MagiosUITests/MagiosLiveConcurrentVoiceUITests \
		-parallel-testing-enabled NO -enableCodeCoverage NO \
		CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=YES -allowProvisioningUpdates test

# Typed synthetic admission + source navigation only; no microphone or playback.
.PHONY: test-ios-live-answer-links
.PHONY: test-ios-live-text-queue
.PHONY: test-ios-live-mixed-input
test-ios-live-mixed-input: verify-magios-vosk
	@test -n '$(MAGIOS_DEVICE_ID)' || { echo 'Set MAGIOS_DEVICE_ID to the enrolled physical iPhone.'; exit 1; }
	@TEST_RUNNER_MAGIOS_LIVE_MIXED_INPUT=1 xcodebuild -jobs 2 \
		-project magios/Magios.xcodeproj -scheme Magios -configuration Debug \
		-destination 'platform=iOS,id=$(MAGIOS_DEVICE_ID)' \
		-derivedDataPath '$(CARGO_TARGET_DIR)/magios-device' \
		-clonedSourcePackagesDirPath '$(CARGO_TARGET_DIR)/magios-spm' \
		-only-testing:MagiosUITests/MagiosLiveConcurrentVoiceUITests/testTextQueueDrainsWhileRealtimeCallStaysConnected \
		-parallel-testing-enabled NO -enableCodeCoverage NO \
		CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=YES -allowProvisioningUpdates test

test-ios-live-text-queue: verify-magios-vosk
	@test -n '$(MAGIOS_DEVICE_ID)' || { echo 'Set MAGIOS_DEVICE_ID to the enrolled physical iPhone.'; exit 1; }
	@TEST_RUNNER_MAGIOS_LIVE_TEXT_QUEUE=1 xcodebuild -jobs 2 \
		-project magios/Magios.xcodeproj -scheme Magios -configuration Debug \
		-destination 'platform=iOS,id=$(MAGIOS_DEVICE_ID)' \
		-derivedDataPath '$(CARGO_TARGET_DIR)/magios-device' \
		-clonedSourcePackagesDirPath '$(CARGO_TARGET_DIR)/magios-spm' \
		-only-testing:MagiosUITests/MagiosLiveConcurrentVoiceUITests/testTypedFollowupQueuesWhileParallelRemainsExplicit \
		-parallel-testing-enabled NO -enableCodeCoverage NO \
		CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=YES -allowProvisioningUpdates test

test-ios-live-answer-links: verify-magios-vosk
	@test -n '$(MAGIOS_DEVICE_ID)' || { echo 'Set MAGIOS_DEVICE_ID to the enrolled physical iPhone.'; exit 1; }
	@TEST_RUNNER_MAGIOS_LIVE_ANSWER_LINK=1 xcodebuild -jobs 2 \
		-project magios/Magios.xcodeproj -scheme Magios -configuration Debug \
		-destination 'platform=iOS,id=$(MAGIOS_DEVICE_ID)' \
		-derivedDataPath '$(CARGO_TARGET_DIR)/magios-device' \
		-clonedSourcePackagesDirPath '$(CARGO_TARGET_DIR)/magios-spm' \
		-only-testing:MagiosUITests/MagiosLiveConcurrentVoiceUITests/testOriginalAnswerLinkOpensItsExecutionConversation \
		-parallel-testing-enabled NO -enableCodeCoverage NO \
		CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=YES -allowProvisioningUpdates test

.PHONY: test-ios-live-concurrent-voice
test-ios: setup-magios-vosk
	@python3 scripts/run_magios_tests_with_report.py \
		--project magios/Magios.xcodeproj --scheme Magios --configuration Debug \
		--destination '$(IOS_TEST_DESTINATION)' \
		--report-dir '$(IOS_TEST_REPORT_DIR)' --work-root '$(IOS_TEST_WORK_ROOT)' \
		--infrastructure-retries '$(IOS_TEST_INFRASTRUCTURE_RETRIES)' \
		--lock-timeout '$(IOS_TEST_LOCK_TIMEOUT)' \
		--diagnostic-timeout '$(IOS_TEST_DIAGNOSTIC_TIMEOUT)'

# On-demand XCUITest smoke suite (MagiosUITests) — the non-blocking UI lane. NOT
# part of `make test` (see the test-ios comment for the framework slow-terminate
# flakiness that keeps it out of the gate). Every test launches OFFLINE via the
# app's `--ui-test` arg and terminates the app in tearDown, so most runs are clean;
# but XCUITest's app-teardown timing still occasionally SIGTERMs a single test, so
# `-retry-tests-on-failure -test-iterations 3` gives each failed test up to two
# reruns. This lane is BEST-EFFORT — a transient red here is a rerun, not a gate
# break. No coverage (smoke tests assert always-present chrome, not code paths).
.PHONY: test-ios-ui
test-ios-ui: setup-magios-vosk
	@if ! command -v xcodebuild >/dev/null 2>&1; then \
		echo "⏭  xcodebuild not found — skipping Magios UI smoke tests (macOS + Xcode only)."; \
	else \
		echo "🧪 iOS UI smoke tests (MagiosUITests, retry-on-failure; best-effort)..."; \
		xcodebuild -project magios/Magios.xcodeproj -scheme Magios -configuration Debug \
			-destination '$(IOS_TEST_DESTINATION)' \
			-derivedDataPath '$(IOS_UI_TEST_DERIVED_DATA)' \
			-only-testing:MagiosUITests \
			-retry-tests-on-failure -test-iterations 3 \
			-quiet test; \
	fi

# iOS compile GATE for `make check-all` — a pure compile of Magios (debug,
# simulator, no signing). Output isn't installable; it just surfaces iOS breakages
# cheaply (incremental). For an installable build use `ios-debug-build`. Skips
# gracefully when xcodebuild is absent (non-macOS / no Xcode); on macOS it FAILS on
# a real compile error.
.PHONY: ios-debug-check
ios-debug-check: verify-magios-vosk
	@if ! command -v xcodebuild >/dev/null 2>&1; then \
		echo "⏭  xcodebuild not found — skipping Magios compile check (macOS + Xcode only)."; \
	else \
		echo "🍎 iOS compile check (Magios, debug, simulator)..."; \
		xcodebuild -project magios/Magios.xcodeproj -scheme Magios -sdk iphonesimulator \
			-destination 'generic/platform=iOS Simulator' -configuration Debug \
			-derivedDataPath '$(MAGIOS_DEBUG_DERIVED_DATA)' \
			CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO -quiet build; \
	fi

# Magios debug build / deploy — targets a CONNECTED iPhone when present, else a
# simulator. Customer endpoints and credentials arrive through QR enrollment;
# none are compiled into the app. The Access GATE is a one-time setup
# (connect-access / ios-full-setup-debug) and is NOT re-run here.
#   ios-debug-build   build only (compile), no install; Personal Team compatible
#   ios-debug-build-push  paid-team sandbox APNs build only
#   ios-debug-deploy  install + launch the LAST build, no rebuild
#   ios-debug-run     build + deploy (ios-debug-build + ios-debug-deploy)
#   ios-debug-run-push    paid-team sandbox APNs build + deploy
# First device install may need the developer trusted: Settings → General → VPN &
# Device Management.
.PHONY: ios-debug-build ios-debug-build-push ios-debug-deploy ios-debug-run ios-debug-run-push test-ios-live-chat test-ios-live-recovery test-ios-live-today
test-ios-live-chat:
	@bash scripts/magios-run.sh live-chat
test-ios-live-recovery:
	@bash scripts/magios-run.sh live-recovery
test-ios-live-today:
	@bash scripts/magios-run.sh live-today
.PHONY: test-ios-live-notes
test-ios-live-notes:
	@bash scripts/magios-run.sh live-notes
.PHONY: test-ios-live-appearance
test-ios-live-appearance:
	@bash scripts/magios-run.sh live-appearance
.PHONY: test-ios-live-upload
test-ios-live-upload:
	@bash scripts/magios-run.sh live-upload
.PHONY: test-ios-live-playback
test-ios-live-playback:
	@bash scripts/magios-run.sh live-playback
.PHONY: test-ios-live-session-routing
test-ios-live-session-routing:
	@bash scripts/magios-run.sh live-session-routing

.PHONY: test-ios-live-enrollment
test-ios-live-enrollment:
	@bash scripts/magios-run.sh live-enrollment

.PHONY: test-ios-live-question
test-ios-live-question:
	@bash scripts/magios-run.sh live-question

.PHONY: test-magesp-connection
test-magesp-connection:
	@python3 scripts/test-magesp-connection.py

.PHONY: build-magesp
build-magesp:
	@bash scripts/build-magesp.sh
ios-debug-build:
	@bash scripts/magios-run.sh build
ios-debug-build-push:
	@MAGIOS_PUSH_NOTIFICATIONS=1 bash scripts/magios-run.sh build
ios-debug-deploy:
	@bash scripts/magios-run.sh deploy
ios-debug-run:
	@bash scripts/magios-run.sh run
ios-debug-run-push:
	@MAGIOS_PUSH_NOTIFICATIONS=1 bash scripts/magios-run.sh run

# Ensure the configured device hostname's Cloudflare Access gate. Idempotent; safe
# to re-run. Needs CLOUDFLARE_API_TOKEN with the Access edit scopes (see
# scripts/ensure-connect-access.sh). ios-access remains a compatibility alias.
.PHONY: ios-access
ios-access: connect-access

# One-shot debug setup: use the checked-in project, ensure BOTH Cloudflare Access
# gates (device service token + ui email), apply the tunnel routing, then do a debug
# simulator build. Every step is idempotent and best-effort, so re-running is safe
# and a missing token/allowlist warns but doesn't abort. Needs, in the runtime env:
# CLOUDFLARE_API_TOKEN (+ Access edit scopes), CLOUDFLARED_TOKEN, and
# MAGICIAN_UI_ACCESS_EMAILS. Note: ordering here creates the ui gate BEFORE the
# tunnel publishes ui.<zone>, so there is no ungated exposure window.
.PHONY: ios-full-setup-debug
ios-full-setup-debug:
	@echo "🔐 [1/4] Ensuring the mobile Access gate and QR bootstrap route..."
	@bash scripts/ensure-connect-access.sh || echo "⚠️  connection Access step failed (need CLOUDFLARE_API_TOKEN + Access edit) — continuing."
	@echo "🔐 [2/4] Ensuring ui.<zone> Access gate (email allowlist)..."
	@bash scripts/ensure-ui-access.sh || echo "⚠️  ui Access step failed (need allowlist + token) — continuing."
	@echo "🌐 [3/4] Applying tunnel routing (webhook + ui + connect path-split)..."
	@$(MAKE) --no-print-directory ensure-tunnel
	@echo "🔨 [4/4] Debug simulator build..."
	@xcodebuild -project magios/Magios.xcodeproj -scheme Magios -sdk iphonesimulator -configuration Debug -quiet build \
		-derivedDataPath '$(MAGIOS_DEBUG_DERIVED_DATA)' \
		&& echo "✅ Debug build OK." \
		|| echo "⚠️  Simulator build failed — open magios/Magios.xcodeproj in Xcode for device signing + Run."
	@echo "🚀 Device install: open magios/Magios.xcodeproj in Xcode (handles signing), then Run."

# ---------------------------------------------------------------------------
# Agent-definition cache maintenance (live backend)
# ---------------------------------------------------------------------------
# The agent scheduler caches per-scope definition lists in-process. If a
# definition is edited on disk out-of-band, clear + reload the cache without a
# restart. Overridable: MAGICIAN_API and MAGICIAN_BEARER_TOKEN. The bearer is
# optional only while the server runs in local open-auth mode.
MAGICIAN_API ?= http://127.0.0.1:3002
MAGICIAN_BEARER_TOKEN ?=

.PHONY: refresh-agent-definitions
refresh-agent-definitions: ## Clear + reload the running backend's agent-definition cache (no restart)
	@echo "🔄 Refreshing agent definitions on $(MAGICIAN_API)..."
	@curl -fsS -X POST \
		$(if $(strip $(MAGICIAN_BEARER_TOKEN)),-H "Authorization: Bearer $(MAGICIAN_BEARER_TOKEN)") \
		"$(MAGICIAN_API)/api/magician/v2/agents/refresh-definitions" \
		&& echo "" \
		|| echo "⚠️  Refresh failed — is magician running on $(MAGICIAN_API)?"

# Test production queue admission and encrypted connection reuse without
# rebuilding the monolith's fixture graph or calling a model.
.PHONY: test-app-registry-admission
test-app-registry-admission:
	@cargo test --manifest-path scripts/registry_admission_checks/Cargo.toml --offline

.PHONY: check-app-runtime
check-app-runtime: check-protoc
	@cargo check -p magician -p magician-api --lib

.PHONY: check-app-runtime-tests
check-app-runtime-tests: check-protoc
	@cargo check -p magician --lib --tests

.PHONY: test-app-contextual-round
.PHONY: fmt-app-recurring
fmt-app-recurring:
	@rustfmt --edition 2021 magician/src/magician_v2/apps/workflows/recurring.rs magician/src/magician_v2/apps/registry/recurring.rs magician/src/magician_v2/apps/background_behaviors/recurring.rs magician/src/magician_v2/apps/background_behaviors/recurring_tests.rs

.PHONY: check-app-recurring test-app-recurring
check-app-recurring:
	@cargo check -p magician-bin

test-app-recurring:
	@env -u RUST_MIN_STACK cargo test -p magician --features test-fixtures --lib -- recurring_ app_runtime_concurrency_regression_owner_retry_preserves_fire_and_counters --test-threads=1 --nocapture

.PHONY: test-app-recurring-ui
test-app-recurring-ui:
	@npm --prefix ui/unified-ui test -- --run src/lib/internalTasks/api.test.ts src/lib/internalTasks/InternalTasksWorkspace.component.test.ts


test-app-contextual-round:
	@cargo test --manifest-path scripts/app_round_checks/Cargo.toml --offline

.PHONY: test-app-storage
.PHONY: test-app-pagination
test-app-indexed-query:
	@cargo test --manifest-path scripts/app_query_checks/Cargo.toml --offline -- --show-output

.PHONY: test-app-indexed-query
.PHONY: test-app-keyset
.PHONY: test-app-data-cleanup
test-app-data-cleanup:
	@cargo test -p magician-apps -p magician-api --lib -- age_cleanup_ apps::entity_retention::tests:: --test-threads=1

test-app-keyset:
	@cargo test -p magician --lib keyset_ -- --test-threads=1

test-app-pagination:
	@cargo test -p magician --lib magician_v2::apps::entity_store::tests:: -- --test-threads=1

.PHONY: test-app-source-projection
test-app-source-projection:
	@env -u RUST_MIN_STACK cargo test -p magician -p magician-api --lib -- --test-threads=1 --nocapture \
		app_source_projection_ magician_v2::apps::reconciliation::tests:: \
		apps_api::provider_free_native_tests::

.PHONY: test-app-resource-cleanup
test-app-resource-cleanup:
	@env -u RUST_MIN_STACK cargo test -p magician -p magician-api --lib -- --test-threads=1 --nocapture \
		magician_v2::apps::resource_contract::tests:: \
		magician_v2::apps::resource_authority::tests::accepted_ \
		apps_api::provider_free_native_tests::

test-app-storage:
	@cargo test -p magician-api -p magician-comms --lib -- \
		--test-threads=1 \
		storage_governance_api::tests::app_store_ \
		channel_assist::governance::tests::dormant_app_ \
		channel_assist::governance::tests::app_storage_inventory_progresses_during_attention_maintenance \
		storage_governance_api::tests::attention_actions_keep_online_optimization_cleanup_and_reclaim_separate

# Focused writer-starvation, queue recovery and health/semantic contract checks.
.PHONY: test-attention-recovery
test-attention-recovery:
	@cargo test -p magician -p magician-comms -p magician-api --lib -- --test-threads=1 \
		attention_recovery \
		magician_v2::attention::learning::store::tests::rank_recompute_ \
		attention_learning::rank_recompute::tests:: \
		attention_learning_api::tests::
	@npm --prefix ui/unified-ui test -- src/lib/attention/AttentionLearningHealthStrip.test.ts src/lib/attention/attentionSemanticExtraction.test.ts

.PHONY: test-decision-rail test-decision-rail-host test-decision-mode test-decision-mode-api
test-decision-rail:
	cargo test -p decision-engine

test-decision-mode:
	cargo test -p magician -p magician-api --lib decision_rail -- --test-threads=1

test-decision-mode-api:
	cargo test -p magician -p magician-api --lib decision_rail_setting -- --test-threads=1

# Host authority, captured images, shared lifecycle and planner transports.
test-decision-rail-host:
	cargo test -p magician -p magician-api --lib -- --test-threads=1 decision_rail decision_planner terminal_usage service_health no_progress_ task_backed_ synthetic_yield_ churn_without restricted_toolset::tests outward_actions::tests synthetic_tail_pairs_fold a_model_tail_is_left_alone a_desktop_snapshots_capture

.PHONY: test-decision-crate
test-decision-crate:
	@cargo test -p magician-decision

.PHONY: bench-decision-jev
# Live replay of the shared action rail against the real Jev API,
# through the decision engine. Needs TYPESAFE_EVAL_KEY in the environment;
# never runs in CI.
bench-decision-jev:
	@cargo test -p decision-engine --test jev_live -- --ignored --nocapture

.PHONY: test-distill-history
test-distill-history:
	@cargo test -p magician-comms -p magician-api --lib -- --test-threads=1 \
		distill_history_ channel_assist::assist::distill::tests:: \
		channel_assist::store::tests::retryable_distill_listing_is_cap_aware_and_newest_first
	@npm --prefix ui/unified-ui test -- src/lib/stores/channelStatsStore.test.ts

.PHONY: test-database-maintenance
.PHONY: test-startup
test-startup:
	cargo test -p magician-bin --bin magician -- --test-threads=1
	cargo test -p magician --lib magician_v2::runtime:: -- --test-threads=1

.PHONY: test-startup-ui
test-startup-ui: setup-ui-deps
	cd ui/unified-ui && npm exec vitest run src/lib/shell/StartupGate.component.test.ts

test-database-maintenance:
	cargo test -p magician --lib magician_v2::storage_governance:: -- --test-threads=4
	cargo test -p magician --lib magician_v2::feed::store::tests:: -- --test-threads=4
	cargo test -p magician-comms --lib channel_assist::store::tests:: -- --test-threads=4
	cargo test -p magician-comms --lib channel_assist::governance:: -- --test-threads=4
	cargo test -p magician-api --lib storage_governance_api:: -- --test-threads=4

.PHONY: test-decision-planner-live
## desc: Shared rail proposal transport through installed CLIs and the real isolated MCP HTTP handler
## eval: kind=live requires=provider_keys
test-decision-planner-live:
	DECISION_PLANNER_LIVE_ENGINES='$(or $(DECISION_PLANNER_LIVE_ENGINES),pi)' cargo test -p magician-bin --test execution_harness_contract installed_harness_returns_a_proposal_without_work_authority -- --ignored --nocapture --test-threads=1

# Automatic, conservative retention: only old incremental sessions under this
# target, guarded by Cargo's own profile lock. Explicitly retry after an active
# build if a lock caused maintenance to skip.
.PHONY: maintain-build-cache test-build-cache-maintenance build-decision-engine-debug
maintain-build-cache:
	@python3 scripts/maintain-build-cache.py --target-dir "$(CARGO_TARGET_DIR)"

test-build-cache-maintenance:
	@python3 scripts/test_build_cache_maintenance.py

build-all-debug build-magician-debug build-decision-engine-debug build-decision-engine-mlx-debug check-all test-decision-rail-host test-decision-crate: | maintain-build-cache

.PHONY: test-gpt61-sol
test-gpt61-sol:
	cargo test -p magicllm --lib -- pricing::tests providers::tests::gpt_6 providers::openai_responses::tests::gpt_6_1_sol
	cargo test -p magician --lib -- --test-threads=1 shipped_configs_retire_gpt54 locality_switch_preserves_reviewed llm_pricing_config::tests brainstorm_facilitation_profile_policy gpt_6_1_sol_pi_profile
	cd ui/unified-ui && npm exec vitest run src/lib/magician/crew/agentModelPins.test.ts src/lib/magician/crew/AgentModelPinsPanel.component.test.ts

.PHONY: test-decision-model-metering
test-decision-model-metering:
	cargo test -p magician-decision --test typesafe_mapping --test routing
	cargo test -p decision-engine --test decision_rail
	cargo test -p magicllm --lib decision_model_pricing
	cargo test -p magician --lib -- --test-threads=1 decision_model llm_pricing_config
	cd ui/unified-ui && npm exec vitest run src/lib/llm/decisionModels.test.ts src/lib/llm/callsQuery.test.ts src/lib/llm/spendBreakdown.test.ts src/lib/today/pulseQueries.test.ts 'src/routes/(app)/llm/CallsTable.component.test.ts'

# Classification contract and memory decision regression lanes. No provider keys.
.PHONY: test-decision-memory-contract
test-decision-memory-contract:
	cargo test -p decision-engine-contract --features client

.PHONY: test-decision-memory
DECISION_MEMORY_GROUP ?= all
test-decision-memory:
	@case "$(DECISION_MEMORY_GROUP)" in \
	  all) $(MAKE) test-decision-memory-contract test-decision-rail test-decision-crate test-decision-memory-host test-decision-memory-report ;; \
	  engine) $(MAKE) test-decision-memory-contract test-decision-rail test-decision-crate ;; \
	  host) $(MAKE) test-decision-memory-host ;; \
	  report) $(MAKE) test-decision-memory-report ;; \
	  *) echo "DECISION_MEMORY_GROUP must be all, engine, host or report"; exit 2 ;; \
	esac

.PHONY: test-decision-memory-host test-decision-memory-magician test-decision-memory-comms
test-decision-memory-host:
	$(MAKE) test-decision-memory-magician
	$(MAKE) test-decision-memory-comms

test-decision-memory-magician:
	cargo test -p magician --lib -- --test-threads=1 analytics::operation_llm_telemetry::tests memory_decision_ decisions::observation::tests decisions::reference::tests memory_applicability::tests memory_stage2::tests memory_utility_reviewer::tests memory_consolidator::memory_decisions::memory_conflict_replay_tests memory_consolidator::tests::episode_quality memory_consolidator::tests::memory_conflict memory_lifecycle evidence:: memory_connections learning::reflection:: learning::procedure_feedback::

test-decision-memory-comms:
	cargo test -p magician-comms --lib memory_connections -- --test-threads=1

.PHONY: test-decision-memory-report
test-decision-memory-report:
	python3 -m unittest discover -s scripts -p test_decision_memory_report.py

.PHONY: test-decision-memory-human-review eval-decision-memory-human-review build-decision-pricing-helper
test-decision-memory-human-review:
	python3 -m unittest scripts/test_eval_memory_human_review.py

# ARGS supplies --socket and a fresh --output directory; performs paid API calls.
build-decision-pricing-helper:
	cargo build -p magician --example decision_model_pricing

eval-decision-memory-human-review: build-decision-pricing-helper
	python3 scripts/eval-memory-human-review.py --pricing-helper "$(CARGO_TARGET_DIR)/debug/examples/decision_model_pricing" $(ARGS)

.PHONY: test-decision-memory-live
# Requires MAGICIAN_MEMORY_EVAL_CONFIG, DECISION_ENGINE_SOCKET and a fresh
# MAGICIAN_MEMORY_EVAL_OUTPUT on the external build/artifact volume.
test-decision-memory-live:
	cargo test -p magician --features test-fixtures --test memory_decision_live memory_decision_live_replay -- --ignored --nocapture --test-threads=1

.PHONY: test-decision-memory-diagnostic
test-decision-memory-diagnostic:
	cargo test -p magician --features test-fixtures --test memory_decision_live memory_decision_incumbent_diagnostic -- --ignored --nocapture --test-threads=1

.PHONY: test-decision-memory-owners-build test-decision-memory-owners-live
test-decision-memory-owners-build:
	cargo test -p magician --features test-fixtures --test memory_decision_live --no-run

test-decision-memory-owners-live:
	cargo test -p magician --features test-fixtures --test memory_decision_live memory_decision_owner_live_replay -- --ignored --nocapture --test-threads=1

.PHONY: test-decision-memory-publication-live
test-decision-memory-publication-live:
	cargo test -p magician-comms --test memory_connections_live memory_connection_publication_live -- --ignored --nocapture --test-threads=1

.PHONY: test-model-dispatch
test-model-dispatch:
	cargo test -p magicllm --lib dispatch::
	cargo test -p magician-decision --lib dispatch::tests
	cargo test -p magician-decision --test classification_fallback
	cargo test -p decision-engine --test classification_fallback

.PHONY: test-decision-routing test-decision-routing-api test-decision-routing-ui
test-decision-routing:
	cargo test -p decision-engine -p magician-decision -p decision-engine-contract

test-decision-routing-api:
	cargo test -p magician-api --lib decision_routing -- --test-threads=1

test-decision-routing-ui:
	@npm --prefix ui/unified-ui test -- src/lib/plane/DecisionRoutingPanel.component.test.ts src/lib/plane/EnginesPanel.component.test.ts

.PHONY: test-decision-routing-locality
test-decision-routing-locality:
	cargo test -p magician --lib decision_routing_locality -- --test-threads=1
