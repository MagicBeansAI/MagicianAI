# skillshub/Makefile — single source for everything skill-related.
#
# All skill setup, build, and install targets live here. The root
# Makefile delegates via `$(MAKE) -C skillshub <target>`.
#
# Layout:
#   <skill>/SKILL.md           — every direct subdir with a SKILL.md is a skill
#   <skill>/SKILL.md metadata.magician — complete typed runtime and product contract
#   <skill>/scripts/           — runnable scripts (run.sh, *.py)
#   <skill>/bin/               — pre-built binaries (gitignored when symlinks)
#   workspace crates / external packages — independently maintained executable source
#   <skill>/config/.env.example — per-skill API key template
#   <skill>/package.json       — only browser/ has its own; others use root
#   package.json               — npm workspace root for rg, dugite, bots
#   node_modules/              — gitignored; hoisted by `npm install`
#   .venv/                     — gitignored; uv venv from requirements.txt
#   requirements.txt           — committed; aggregated from skill SKILL.mds
#   bots/                      — bot daemon source code (telegram, whatsapp, …)
#   browser/                   — special: own node_modules + patched binary

.DEFAULT_GOAL := help

# Every direct subdir except scripts/, node_modules/, bots/ that contains
# a SKILL.md is a skill.
SKILL_DIRS := $(sort $(dir $(wildcard */SKILL.md)))

DATA_ROOT ?= $(or $(MAGICIAN_ROOT_DIR),$(MAGICIAN_STORAGE_PATH),$(HOME)/MagicianNotes)
SCOPE ?=
REPO_ROOT ?= $(abspath $(CURDIR)/..)
CARGO_TARGET_ROOT_PREFERRED ?= /Volumes/build/magician/builds
CARGO_TARGET_ROOT_FALLBACK ?= $(REPO_ROOT)/target
CARGO_TARGET_DIR ?= $(shell \
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
AGENT_BROWSER_CARGO_TARGET_DIR ?= $(CARGO_TARGET_DIR)/agent-browser
PROFILE ?= debug
ifeq ($(PROFILE),release)
DOCUMENT_TO_MARKDOWN_CARGO_FLAGS := --release
DOCUMENT_TO_MARKDOWN_PROFILE_DIR := release
else ifeq ($(PROFILE),debug)
DOCUMENT_TO_MARKDOWN_CARGO_FLAGS :=
DOCUMENT_TO_MARKDOWN_PROFILE_DIR := debug
else
$(error PROFILE must be debug or release)
endif
DOCUMENT_TO_MARKDOWN_BIN := $(CARGO_TARGET_DIR)/$(DOCUMENT_TO_MARKDOWN_PROFILE_DIR)/document-to-markdown

# Project-local Node 24 LTS lives at `.node/` (managed by `setup-node`).
# Pin the patch in `.node-version` so the version is single-source-of-truth
# and bumpable without editing Make. The cli_template dispatcher prepends
# `.node/bin` to skill subprocess PATH so `node`, `npm`, `npx`, plus the
# corepack shims (pnpm, yarn) all resolve from this install.
CAPABILITY_NODE_VERSION := $(shell cat $(CURDIR)/.node-version 2>/dev/null | sed 's/^v//')
CAPABILITY_NODE_MAJOR := $(firstword $(subst ., ,$(CAPABILITY_NODE_VERSION)))
NODE_ROOT := $(CURDIR)/.node
NODE_BIN := $(NODE_ROOT)/bin
ENSURE_CAPABILITY_NODE_RUNTIME = \
	if [ -x "$(NODE_BIN)/node" ]; then \
		export PATH="$(NODE_BIN):$$PATH"; \
	fi; \
	NODE_VERSION="$$(node -p 'process.versions.node' 2>/dev/null || true)"; \
	if [ -z "$$NODE_VERSION" ]; then \
		echo "Error: node not found. Run 'make setup-node' to install project-local Node $(CAPABILITY_NODE_VERSION)."; exit 1; \
	fi; \
	NODE_MAJOR="$${NODE_VERSION%%.*}"; \
	if [ "$$NODE_MAJOR" != "$(CAPABILITY_NODE_MAJOR)" ]; then \
		echo "Error: skillshub requires Node $(CAPABILITY_NODE_MAJOR).x (recommended $(CAPABILITY_NODE_VERSION))."; \
		echo "  current: node $$NODE_VERSION at $$(command -v node)"; \
		echo "  run: 'make setup-node' to install the project-local copy"; exit 1; \
	fi

.PHONY: help validate build install-scope uninstall-scope list list-source clean \
	clean-build clean-runtime clean-legacy-scope clean-all \
	setup-all setup-all-plan setup-node setup-deps setup-python-bootstrap setup-agent-browser verify-agent-browser \
	ensure-agent-browser-vendor-bin \
	kill-stale-agent-browser \
	agent-browser-source rebuild-agent-browser setup-skill-bins setup-officecli verify-officecli \
	setup-document-to-markdown setup-pdftotext setup-media-fetch setup-ocr setup-python verify-python setup-env setup-gws-accounts \
	seed-scope setup-bot-envs setup-bots install-bot-bundles build-bots \
	check-bots setup-printing-press setup-metabase-cli regen-metabase-cli \
	refresh-metabase-spec setup-marimo \
	setup-cloak-browser verify-cloak-browser setup-lightpanda verify-lightpanda \
	setup-kapso-cli setup-higgsfield-cli

help:
	@echo "skillshub/ targets:"
	@echo "  Cargo target dir                     $(CARGO_TARGET_DIR)"
	@echo "  Agent-browser Cargo target dir       $(AGENT_BROWSER_CARGO_TARGET_DIR)"
	@echo ""
	@echo "Setup chain (run by 'make -C skillshub setup-all' or 'make setup-all' from root):"
	@echo "  setup-all [DRY=1]                   Run every setup target in order  (DRY=1: print plan only)"
	@echo "  setup-node                          Fetch pinned Node $(CAPABILITY_NODE_VERSION) tarball into skillshub/.node/ (also enables corepack)"
	@echo "  setup-deps                          npm install at skillshub root (shared CLIs, rg, dugite, bots)"
	@echo "  setup-python-bootstrap              Ensure PyYAML for setup/materialization scripts"
	@echo "  setup-agent-browser                 Browser skill: own npm install + patched-binary overlay"
	@echo "  setup-skill-bins                    Symlink rg, dugite, and TinyFish binaries to root node_modules"
	@echo "  setup-document-to-markdown          Build the standalone AnyDoc CLI workspace tool and link it into its skill"
	@echo "  setup-officecli                     Fetch pinned, checksum-verified OfficeCLI and link all Office skills"
	@echo "  verify-officecli                    Verify the pinned OfficeCLI binary and skill links without downloading"
	@echo "  setup-pdftotext                     Symlink host poppler binaries into skillshub/pdftotext/bin/"
	@echo "  setup-media-fetch                   Symlink host ffmpeg/ffprobe binaries into skillshub/media-fetch/bin/"
	@echo "  setup-ocr                           Symlink host tesseract binary into skillshub/ocr/bin/"
	@echo "  setup-python                        uv venv at skillshub/.venv from aggregated SKILL.md python_packages"
	@echo "  verify-python                       Fail if skillshub/.venv is missing or lacks its packages (end of setup-all)"
	@echo "  seed-scope [SCOPE=...]              Mkdir scope tree (bots/auth/workdirs/skills) + seed bot_configs.yaml"
	@echo "  setup-env [SCOPE=...] [DATA_ROOT=...] Populate per-skill config/.env files (idempotent)"
	@echo "  setup-bot-envs [SCOPE=..] [FORCE=1] Generate per-bot env files at scope from root secrets source"
	@echo "  setup-bots [FORCE=1]                Build bot daemon dist/index.js (esbuild bundle, mtime-skipped)"
	@echo "  setup-marimo                        Dedicated venv at skillshub/marimo/.venv + bin/marimo symlink"
	@echo "  setup-metabase-cli                  Build metabase-pp-cli via printing-press"
	@echo "  setup-cloak-browser                 Cloak Browser: fetch stealth Chromium binary (one-time, ~200MB)"
	@echo "  verify-cloak-browser                Confirm CloakBrowser SDK + binary + wrapper script are healthy"
	@echo "  setup-lightpanda                    Install the official Lightpanda nightly (Homebrew on macOS)"
	@echo "  verify-lightpanda                   Confirm Lightpanda + its engine resolver are healthy"
	@echo "  setup-kapso-cli                     Verify the shared pinned Kapso CLI installed by setup-deps"
	@echo "  setup-higgsfield-cli [SCOPE=...]    Link the authenticated Higgsfield CLI and seed its scoped credentials"
	@echo ""
	@echo "Skill catalog:"
	@echo "  validate                   Validate every SKILL.md frontmatter"
	@echo "  build                      Run any per-skill build steps (bin/ compilation)"
	@echo "  install-scope SCOPE=<id> [NAMES=a,b,c]"
	@echo "                              Install skills (symlinks back to skillshub/) to DATA_ROOT/scopes/<id>/skills/."
	@echo "                              Default: ALL skills. Pass NAMES=\"a,b\" for a surgical subset (e.g. one new skill)."
	@echo "                              Example: make install-scope SCOPE=anonymous/default NAMES=\"image-generation-via-minimax\""
	@echo "  uninstall-scope SCOPE=<id> Remove all skill + bot bundle symlinks; preserves config/.env, auth/, bot node_modules, .env.<account>, workdirs/"
	@echo "  list                       List installed system skills"
	@echo "  list-source                List source skills under skillshub/ (preview)"
	@echo ""
	@echo "Cleanup (preserves auth/, agent_runtime/, tasks/, chat/, memory/, source files):"
	@echo "  clean / clean-build [DRY=1]                     Remove node_modules, .venv, dist/, bin/, .pp-build/"
	@echo "  clean-runtime [DRY=1]                           Aggressive: rm -rf scope skills/ + bots/ trees entirely (also nukes config/.env, bot node_modules, .env.<account>). Prefer 'uninstall-scope SCOPE=...' for surgical removal."
	@echo "  clean-legacy-scope [DRY=1] [SCOPE=..] [ALL=1]   One-shot: drop legacy <scope>/capabilities/{node_modules,package.json,bot src/}"
	@echo "  clean-all [DRY=1]                               Both of the above"
	@echo ""
	@echo "Agent-browser dev:"
	@echo "  verify-agent-browser       Confirm patched fork is the live runtime binary (kills stale daemons first)"
	@echo "  kill-stale-agent-browser   Kill leaked repo agent-browser daemons + clear stale ~/.agent-browser sockets"
	@echo "  agent-browser-source       Clone upstream + apply our patch into .cache/"
	@echo "  rebuild-agent-browser      Build local native artifact and regenerate the tracked patch"
	@echo ""
	@echo "DATA_ROOT defaults to MAGICIAN_ROOT_DIR, then MAGICIAN_STORAGE_PATH, then ~/MagicianNotes."
	@echo "Skills found:"
	@for d in $(SKILL_DIRS); do echo "  - $$(basename $$d)"; done

# ============================================================================
# Catalog management (validate, build, install)
# ============================================================================

validate:
	@python3 scripts/validate_skill_md.py $(SKILL_DIRS)

list:
	@python3 scripts/list_skills.py --data-root "$(DATA_ROOT)"

list-source:
	@python3 scripts/list_skills.py --source-only

build: setup-document-to-markdown
	@for d in $(SKILL_DIRS); do \
		if [ -f $$d/Makefile ]; then \
			echo ">>> building $$d"; \
			$(MAKE) -C $$d build || exit 1; \
		fi; \
	done

# install-scope deploys skills to `<scope>/skills/<skill>/`.
# Materialization model: file-level symlinks back to skillshub/<name>/*
# (see scripts/install_skill_layer.py). Edits to source are live at the
# runtime path without re-running this target. Per-scope provider state below
# `config/` stays outside the swapped package view in `.skill-state/`; the
# installed `config` entry is a relative symlink to that durable directory.
#
# By default installs EVERY skill. Pass `NAMES="a,b,c"` to install only
# a subset — useful when adding one or two new skills without re-touching
# all 60+ (the operator log no longer floods).
#   make install-scope SCOPE=anonymous/default
#   make install-scope SCOPE=anonymous/default NAMES="image-generation-via-minimax,video-generation-via-minimax"
install-scope: validate build
	@test -n "$(SCOPE)" || (echo "SCOPE=<principal>/<workspace> required" && exit 1)
	@NAMES_ARG=""; \
	 if [ -n "$(NAMES)" ]; then NAMES_ARG="--names $(NAMES)"; fi; \
	 python3 scripts/install_skill_layer.py --layer all --dest "$(DATA_ROOT)/scopes/$(SCOPE)/skills" $$NAMES_ARG
	@SCOPE_ROOT="$(DATA_ROOT)/scopes/$(SCOPE)"; \
	 WU_SRC="$$SCOPE_ROOT/bots/whatsapp/node_modules/.bin/wu"; \
	 WU_DST="$$SCOPE_ROOT/skills/whatsapp/bin/wu"; \
	 if [ -e "$$WU_SRC" ] && [ -d "$$SCOPE_ROOT/skills/whatsapp" ]; then \
	   mkdir -p "$$(dirname "$$WU_DST")"; \
	   ln -sf "../../../bots/whatsapp/node_modules/.bin/wu" "$$WU_DST"; \
	   echo ">>> linked scoped whatsapp wu executable at $$WU_DST"; \
	 fi

# uninstall-scope: surgical reverse of install-scope. Removes symlinks under
# each public `<scope>/skills/<name>/` package view and every bot bundle
# symlink under `<scope>/bots/`. Preserves installer-owned hidden state,
# including `.skill-state/<name>/config`,
# whatsapp `node_modules/`, bot `package.json` + `.env.<account>`,
# `bot_configs.yaml`, OAuth tokens under `<scope>/auth/`, workdirs/.
# Run before deleting a workspace, before reinstalling a different
# pinned set of skills, or to free up the symlink layer for inspection.
#
# Idempotent; does NOT remove auth/, agent_runtime/, tasks/, memory/,
# chat history, or any other data. For the nuclear option (wipe all
# runtime trees including auth tokens), see `clean-runtime`.
uninstall-scope:
	@test -n "$(SCOPE)" || (echo "SCOPE=<principal>/<workspace> required" && exit 1)
	@python3 scripts/uninstall_skill_layer.py --dest "$(DATA_ROOT)/scopes/$(SCOPE)/skills"
	@python3 scripts/uninstall_bot_bundles.py --scope "$(SCOPE)" --data-root "$(DATA_ROOT)"
	@echo ">>> uninstall-scope done (kept: config/.env, auth/, bot node_modules, .env.<account>, bot_configs.yaml, workdirs/)"

clean: clean-build
	@for d in $(SKILL_DIRS); do \
		if [ -f $$d/Makefile ]; then $(MAKE) -C $$d clean; fi; \
	done

# Clean per-skill build artifacts. Preserves source.
# Removed:
#   - skillshub root node_modules/ + the workspace package-lock state
#   - skillshub/.venv/ (regenerated by `make setup-python`)
#   - skillshub/marimo/.venv/ (regenerated by `make setup-marimo`)
#   - skillshub/browser/node_modules/ (regenerated by `make setup-agent-browser`)
#   - skillshub/*/bin/ (regenerated symlinks from setup-skill-bins,
#       setup-pdftotext, setup-ocr, setup-marimo; browser/bin/ holds
#       the patched binary, regenerated by setup-agent-browser)
#   - skillshub/metabase/{bin,.pp-build}/ (regenerated by setup-metabase-cli)
#   - skillshub/bots/*/dist/ (regenerated by setup-bots)
# Preserves:
#   - All SKILL.md, scripts/, config/, references/, assets/, and independent source
#   - All package.json, package-lock.json, requirements.txt
#   - Source code, frontmatters, anything tracked in git
# Pass DRY=1 to preview without deleting (e.g. `make clean-build DRY=1`).
clean-build:
	@if [ -n "$(DRY)" ]; then \
	  echo "[dry] would remove skillshub build artifacts:"; \
	else \
	  echo "Removing skillshub build artifacts..."; \
	fi
	@for p in node_modules .venv marimo/.venv browser/node_modules \
	         metabase/.pp-build $$(ls -d */bin 2>/dev/null) \
	         $$(ls -d bots/*/dist 2>/dev/null); do \
	  if [ -e "$$p" ]; then \
	    sz="$$(du -sh "$$p" 2>/dev/null | cut -f1)"; \
	    if [ -n "$(DRY)" ]; then \
	      printf "  [dry] would rm %-50s (%s)\n" "$$p" "$$sz"; \
	    else \
	      rm -rf "$$p"; \
	      printf "  ✓ removed %-50s (%s)\n" "$$p" "$$sz"; \
	    fi; \
	  fi; \
	done
	@if [ -z "$(DRY)" ]; then echo "  ✓ build artifacts cleaned (sources preserved)"; fi

# Clean the runtime install tree. Removes only what's regenerable
# from skillshub/ via `make install-scope` / `install-bot-bundles`.
#
# Removed:
#   - $(DATA_ROOT)/system/skills/        (legacy; the system-shared install tier was retired — nothing populates this dir now, but `clean-runtime` removes any leftover content)
#   - $(DATA_ROOT)/scopes/*/*/skills/    (regenerable: install-scope + setup-env)
#   - $(DATA_ROOT)/scopes/*/*/bots/      (regenerable: setup-bots + setup-bot-envs + install-bot-bundles)
#   - $(DATA_ROOT)/scopes/*/*/capabilities/  (legacy umbrella — pre-refactor; kept for safety
#                                              when migrating off the old layout. WARNING: if
#                                              you have a partly-migrated scope this also nukes
#                                              the legacy `capabilities/auth/` OAuth tokens —
#                                              run `clean-legacy-scope` first if unsure.)
# Preserved:
#   - $(DATA_ROOT)/scopes/*/*/auth/      (OAuth tokens, per-account client_secret.json)
#   - $(DATA_ROOT)/scopes/*/*/workdirs/  (scope-isolated $HOME for skill/bot subprocesses)
#   - $(DATA_ROOT)/scopes/*/*/{agent_runtime,tasks,chat,memory,...}  (live agent/conversation state)
#   - MAGICIAN_ROOT_DIR/{operator-config.yaml, client_secret.json}  (operator source files; skillshub/ in-tree fallback)
# Pass DRY=1 to preview without deleting (e.g. `make clean-runtime DRY=1`).
clean-runtime:
	@if [ -n "$(DRY)" ]; then \
	  echo "[dry] would remove runtime install tree (preserving auth/, agent_runtime/, tasks/, chat/, memory/, ...):"; \
	else \
	  echo "Removing runtime install tree (preserving auth/, agent_runtime/, tasks/, chat/, memory/, ...)..."; \
	fi
	@for d in "$(DATA_ROOT)/system/skills" \
	          $$(ls -d $(DATA_ROOT)/scopes/*/*/skills 2>/dev/null) \
	          $$(ls -d $(DATA_ROOT)/scopes/*/*/bots 2>/dev/null) \
	          $$(ls -d $(DATA_ROOT)/scopes/*/*/capabilities 2>/dev/null); do \
	  if [ -d "$$d" ]; then \
	    sz="$$(du -sh "$$d" 2>/dev/null | cut -f1)"; \
	    if [ -n "$(DRY)" ]; then \
	      printf "  [dry] would rm %-70s (%s)\n" "$$d" "$$sz"; \
	    else \
	      rm -rf "$$d"; \
	      printf "  ✓ removed %-70s (%s)\n" "$$d" "$$sz"; \
	    fi; \
	  fi; \
	done
	@if [ -z "$(DRY)" ]; then echo "  ✓ runtime tree cleaned (auth/, OAuth tokens, agent_runtime/, all preserved)"; fi

# One-shot legacy-scope cleanup. Drops the pre-bundling-era artifacts
# under each scope's `capabilities/` tree:
#   - <scope>/capabilities/{node_modules,package.json,package-lock.json}
#   - <scope>/capabilities/tools/{node_modules,package-lock.json}
#   - <scope>/capabilities/bots/<bot>/{src,test,node_modules,
#       package.json,package-lock.json,tsconfig.json,README.md,CHANGELOG.md}
#   - dist/*.d.ts, dist/*.js.map  (only dist/index.js is kept)
# Preserves .env.*, dist/index.js, bot_configs.yaml, and everything
# outside the listed paths.
#
# Optional: SCOPE=<principal>/<workspace>  (defaults to anonymous/default)
# Optional: ALL=1                          (clean every scope discovered)
# Optional: DRY=1                          (preview without deleting)
clean-legacy-scope:
	@SCOPE_ARG=""; \
	 if [ -n "$(SCOPE)" ]; then SCOPE_ARG="--scope $(SCOPE)"; fi; \
	 ALL_ARG=""; \
	 if [ -n "$(ALL)" ]; then ALL_ARG="--all-scopes"; fi; \
	 DRY_ARG=""; \
	 if [ -n "$(DRY)" ]; then DRY_ARG="--dry-run"; fi; \
	 python3 scripts/clean_legacy_scope.py $$SCOPE_ARG $$ALL_ARG $$DRY_ARG --data-root "$(DATA_ROOT)"

# Full clean: build artifacts + runtime install tree.
# Pass DRY=1 to preview without deleting.
clean-all: clean clean-runtime
	@if [ -z "$(DRY)" ]; then echo "  ✓ skillshub clean-all complete"; fi

# ============================================================================
# Setup chain — run by `make setup-all` or invoked individually.
# Idempotent throughout: re-running `setup-all` is fast.
# ============================================================================

# Walks every skill setup target in order. Each is independently
# idempotent.
# Pass DRY=1 to print the chain without executing.
setup-all:
	@if [ -n "$(DRY)" ]; then \
	  $(MAKE) --no-print-directory setup-all-plan; \
	  exit 0; \
	fi; \
	$(MAKE) setup-node && \
	$(MAKE) setup-deps && \
	$(MAKE) setup-python && \
	$(MAKE) setup-agent-browser && \
	$(MAKE) setup-lightpanda && \
	$(MAKE) setup-skill-bins && \
	$(MAKE) setup-document-to-markdown && \
	$(MAKE) setup-officecli && \
	$(MAKE) setup-pdftotext && \
	$(MAKE) setup-media-fetch && \
	$(MAKE) setup-ocr && \
	$(MAKE) setup-marimo && \
	$(MAKE) setup-cloak-browser && \
	$(MAKE) setup-kapso-cli && \
	$(MAKE) setup-metabase-cli && \
	$(MAKE) setup-bots && \
	$(MAKE) seed-scope && \
	$(MAKE) install-scope SCOPE=$(or $(SCOPE),anonymous/default) && \
	$(MAKE) setup-env && \
	$(MAKE) setup-gws-accounts && \
	$(MAKE) setup-bot-envs && \
	$(MAKE) install-bot-bundles && \
	$(MAKE) verify-python
	@if [ -z "$(DRY)" ]; then echo ""; echo "Skillshub setup complete."; fi

# Plan view: print the full setup chain with destination paths.
# Drives `make setup-all DRY=1`.
setup-all-plan:
	@DR=$(DATA_ROOT); SC=$(or $(SCOPE),anonymous/default); \
	 SYS_SKILLS=$$(python3 scripts/skill_layer.py --layer system --names-only | wc -l | tr -d ' '); \
	 SCO_SKILLS=$$(python3 scripts/skill_layer.py --layer scope --names-only | wc -l | tr -d ' '); \
	 BOT_COUNT=$$(ls -d bots/*/ 2>/dev/null | grep -vE '/(sdk|build_bundle|bot_configs)/' | wc -l | tr -d ' '); \
	 ACC_COUNT=$$(python3 -c 'import yaml,sys; d=yaml.safe_load(open("operator-config.yaml")) or {}; print(len(d.get("gws_accounts") or []))' 2>/dev/null || echo 0); \
	 echo ""; \
	 echo "[dry] make -C skillshub setup-all  (SCOPE=$$SC)"; \
	 echo "──────────────────────────────────────────────────────"; \
	 echo "Phase A: build artifacts in skillshub/ (no scope writes)"; \
	 echo " 1. setup-node              fetch Node $(CAPABILITY_NODE_VERSION) tarball → skillshub/.node/ + corepack enable"; \
	 echo " 2. setup-deps              npm install at skillshub/ (shared CLIs, rg, dugite, esbuild, bots/*)"; \
	 echo " 2b. setup-python           uv venv at skillshub/.venv + aggregated python deps (early: most Python skills need it)"; \
	 echo " 3. setup-agent-browser     npm install @ skillshub/browser + deploy patched native binary"; \
	 echo " 3b. setup-lightpanda       install/verify official Lightpanda nightly (macOS Homebrew; optional on Linux)"; \
	 echo " 4. setup-skill-bins        symlink rg + dugite + TinyFish binaries into their skill bin/ dirs"; \
	 echo " 4b. setup-document-to-markdown build standalone AnyDoc CLI → skillshub/document-to-markdown/bin/"; \
	 echo " 5. setup-officecli         fetch pinned + checksummed OfficeCLI → skillshub/officecli-runtime/bin/"; \
	 echo " 6. setup-pdftotext         symlink host pdftotext + pdftoppm → skillshub/pdftotext/bin/"; \
	 echo " 6b. setup-media-fetch      symlink host ffmpeg + ffprobe → skillshub/media-fetch/bin/"; \
	 echo " 7. setup-ocr               symlink host tesseract → skillshub/ocr/bin/"; \
	 echo " 9. setup-marimo            uv venv at skillshub/marimo/.venv + marimo + data-science libs"; \
	 echo " 9a. setup-cloak-browser    fetch/verify licensed stealth Chromium binary"; \
	 echo " 9b. setup-kapso-cli        verify shared pinned Kapso CLI in skillshub/node_modules/.bin"; \
	 echo "10. setup-metabase-cli      go build skillshub/metabase/bin/metabase-pp-cli"; \
	 echo "11. setup-bots              esbuild bundle each bot → skillshub/bots/<bot>/dist/index.js"; \
	 echo "                            ($$BOT_COUNT bot(s): $$(ls -d bots/*/ 2>/dev/null | grep -vE '/(sdk|build_bundle|bot_configs)/' | xargs -n1 basename | tr '\n' ' '))"; \
	 echo ""; \
	 echo "Phase B: deploy to scope-aware runtime tree"; \
	 echo "12. seed-scope              mkdir $$DR/scopes/$$SC/{skills,bots,auth,workdirs/home}/"; \
	 echo "                            seed $$DR/scopes/$$SC/bots/bot_configs.yaml"; \
	 echo "13. install-scope           symlink ALL skills (system + scope = $$(($$SYS_SKILLS + $$SCO_SKILLS))) → $$DR/scopes/$$SC/skills/  (back to skillshub/ source)"; \
	 echo "14. setup-env               populate $$DR/scopes/$$SC/skills/<skill>/config/.env"; \
	 echo "                            from skillshub/operator-config.yaml (secrets section)"; \
	 echo "15. setup-gws-accounts      copy client_secret.json (MAGICIAN_ROOT_DIR, else skillshub/) → $$DR/scopes/$$SC/auth/gws-<account>/"; \
	 echo "                            for each of $$ACC_COUNT account(s) in operator-config.yaml gws_accounts:"; \
	 echo "16. setup-bot-envs          generate $$DR/scopes/$$SC/bots/<bot>/.env.<account>"; \
	 echo "                            (gmail: per-account; kapso/telegram/whatsapp: .env.development)"; \
	 echo "17. install-bot-bundles     copy each bot's dist/index.js → $$DR/scopes/$$SC/bots/<bot>/dist/"; \
	 echo "                            + (whatsapp only) write runtime package.json + npm install --omit=dev"; \
	 echo "                            for the externalized @ibrahimwithi/wu-cli dep"; \
	 echo "                            + link $$DR/scopes/$$SC/skills/whatsapp/bin/wu to the scoped runtime"; \
	 echo "                            (idempotent; ~30-50M one-time on first install per scope)"; \
	 echo "──────────────────────────────────────────────────────"; \
	 echo "Approx total time on a cold clone: 3-5 minutes (cached: <30s)"; \
	 echo "Disk added: ~1.1G in skillshub/ (node_modules, .venvs, dist), ~50M in $$DR/scopes/$$SC/"; \
	 echo ""

# Seed a scope's runtime tree (bots/, auth/, workdirs/home, skills/,
# bots/bot_configs.yaml). Mirrors what the magician daemon's
# `materialize_scope` does — run this after `clean-legacy-scope`
# (or fresh checkout) so subsequent setup targets have the
# directory layout to write into without booting the daemon.
#
# Optional: SCOPE=<principal>/<workspace> (defaults to anonymous/default)
seed-scope:
	@SCOPE_ARG=""; \
	 if [ -n "$(SCOPE)" ]; then SCOPE_ARG="--scope $(SCOPE)"; fi; \
	 python3 scripts/seed_scope.py $$SCOPE_ARG --data-root "$(DATA_ROOT)"

# Install skillshub root npm workspace (rg, dugite, bots). Browser is
# intentionally NOT a workspace member — it owns its own node_modules
# so the patched-binary overlay survives root reinstalls.
# Stamp-file rule: `node_modules/.package-lock.json` is npm-generated
# and updated whenever npm install actually does something. Make's
# mtime-based deps mean re-running `setup-deps` is a true no-op when
# the lockfile is unchanged.
NPM_DEPS_STAMP := node_modules/.package-lock.json

# Project-local Node 24 LTS — fetches the official tarball from nodejs.org
# for the host platform/arch and unpacks into `skillshub/.node/`. Same
# idea as Python's `.venv/`: every contributor / CI machine gets the
# identical pinned runtime without depending on system brew / nvm / fnm.
# Bumping: edit `.node-version`, re-run `make setup-node`, then
# `make setup-deps` — a new major is a new ABI, and `setup-deps` rebuilds
# the native addons that no longer load (see NODE_ABI_STAMP).
.node/bin/node: .node-version
	@TARGET_VERSION="v$(CAPABILITY_NODE_VERSION)"; \
	 if [ -x "$(NODE_BIN)/node" ] && [ "$$($(NODE_BIN)/node --version)" = "$$TARGET_VERSION" ]; then \
	   echo "  ✓ node $$TARGET_VERSION already installed at .node/"; \
	   exit 0; \
	 fi; \
	 UNAME_S="$$(uname -s | tr '[:upper:]' '[:lower:]')"; \
	 UNAME_M="$$(uname -m)"; \
	 case "$$UNAME_M" in \
	   x86_64) NODE_ARCH=x64;; \
	   aarch64|arm64) NODE_ARCH=arm64;; \
	   *) echo "Error: unsupported arch $$UNAME_M"; exit 1;; \
	 esac; \
	 case "$$UNAME_S" in \
	   darwin|linux) NODE_PLATFORM=$$UNAME_S;; \
	   *) echo "Error: unsupported OS $$UNAME_S"; exit 1;; \
	 esac; \
	 NODE_DIST="node-$$TARGET_VERSION-$$NODE_PLATFORM-$$NODE_ARCH"; \
	 NODE_URL="https://nodejs.org/dist/$$TARGET_VERSION/$$NODE_DIST.tar.xz"; \
	 echo "  Downloading $$NODE_DIST (~30MB) from nodejs.org..."; \
	 rm -rf $(NODE_ROOT) .node-tmp; \
	 mkdir -p .node-tmp; \
	 curl -fsSL "$$NODE_URL" | tar -xJ -C .node-tmp; \
	 mv .node-tmp/$$NODE_DIST $(NODE_ROOT); \
	 rm -rf .node-tmp; \
	 echo "  ✓ node $$($(NODE_BIN)/node --version) installed at $(NODE_ROOT)"; \
	 echo "  Enabling corepack (pnpm + yarn shims)..."; \
	 PATH="$(NODE_BIN):$$PATH" $(NODE_BIN)/corepack enable >/dev/null 2>&1 || \
	   echo "  ⚠ corepack enable failed — pnpm/yarn may need manual setup"; \
	 echo "  ✓ corepack enabled"; \
	 if [ -d node_modules ]; then \
	   echo "  → run 'make setup-deps' to rebuild native addons for the new Node ABI"; \
	 fi

setup-node: .node/bin/node
	@:

# Native addons are compiled against ONE Node ABI (`process.versions.modules`),
# and the lockfile does not move when `.node/` is swapped for a new major —
# so the deps stamp alone reads a stale tree as current, and the first bot to
# `require` such an addon dies with ERR_DLOPEN_FAILED / NODE_MODULE_VERSION
# mismatch (the WhatsApp bot's better-sqlite3, 2026-09-17: built for ABI 127
# under Node 22, loaded by Node 24 = ABI 137; `.node/` had been swapped three
# weeks earlier and nothing noticed). The runtime is the only reliable oracle
# for "does this addon load", so it is asked, and exactly the packages it
# cannot load are rebuilt — never a hardcoded name (the next addon added is
# covered) and never a blanket `npm rebuild` (whose install scripts would
# re-download git, agentmail and gws binaries on every bump). The stamp
# records the ABI the tree was last checked against and is redone whenever
# the runtime or the installed tree changes.
NODE_ABI_STAMP := node_modules/.node-abi

$(NODE_ABI_STAMP): $(NPM_DEPS_STAMP) .node/bin/node scripts/native_addons_needing_rebuild.mjs
	@export PATH="$(NODE_BIN):$$PATH"; \
	 STALE="$$(node scripts/native_addons_needing_rebuild.mjs node_modules | tr '\n' ' ')"; \
	 if [ -n "$$STALE" ]; then \
	   echo "Rebuilding native addons for Node $$(node --version) (ABI $$(node -p process.versions.modules)): $$STALE"; \
	   npm rebuild $$STALE; \
	   STILL="$$(node scripts/native_addons_needing_rebuild.mjs node_modules | tr '\n' ' ')"; \
	   if [ -n "$$STILL" ]; then \
	     echo "  ✗ still not loadable after rebuild: $$STILL"; exit 1; \
	   fi; \
	   echo "  ✓ native addons rebuilt: $$STALE"; \
	 fi; \
	 node -p process.versions.modules > $@

setup-deps: setup-node $(NPM_DEPS_STAMP) $(NODE_ABI_STAMP)
	@:

$(NPM_DEPS_STAMP): package.json package-lock.json | .node/bin/node
	@$(ENSURE_CAPABILITY_NODE_RUNTIME); \
	 echo "Using capability Node $$NODE_VERSION at $$(command -v node)"
	@echo "Installing skillshub shared CLI/workspace deps (Kapso, TinyFish, rg, dugite, bots, esbuild)..."
	@PATH="$(NODE_BIN):$$PATH" npm install --include=optional >/dev/null 2>&1
	@echo "  ✓ skillshub/node_modules populated"

# Wire skill bin/ symlinks to the root node_modules. Idempotent.
# Uses ABSOLUTE symlinks so the symlinks stay valid after rsync to
# `<data_root>/system/skills/<skill>/bin/` (where there's no sibling
# `node_modules/`). The runtime hits the absolute path back into
# `skillshub/node_modules/` which always exists.
setup-skill-bins: setup-deps
	@mkdir -p rg/bin
	@export PATH="$(NODE_BIN):$$PATH"; \
	 RG_BIN="$$(node -e "import('@vscode/ripgrep').then(m => process.stdout.write(m.rgPath))" 2>/dev/null)"; \
	 if [ -n "$$RG_BIN" ] && [ -x "$$RG_BIN" ]; then \
	   ln -sf "$$RG_BIN" rg/bin/rg; \
	   echo "  ✓ rg/bin/rg → $$RG_BIN"; \
	 else \
	   echo "  ✗ ripgrep binary not resolvable via @vscode/ripgrep"; exit 1; \
	 fi

	@mkdir -p dugite/bin
	@DUGITE_GIT="$$(pwd)/node_modules/dugite/git/bin/git"; \
	 if [ -x "$$DUGITE_GIT" ]; then \
	   ln -sf "$$DUGITE_GIT" dugite/bin/git; \
	   echo "  ✓ dugite/bin/git → $$DUGITE_GIT"; \
	 else \
	   echo "  ✗ dugite git binary missing at $$DUGITE_GIT"; exit 1; \
	 fi

	@mkdir -p web-via-tinyfish/bin
	@TINYFISH_BIN="$$(pwd)/node_modules/.bin/tinyfish"; \
	 NODE_BIN_PATH="$$(pwd)/.node/bin/node"; \
	 if [ -x "$$TINYFISH_BIN" ]; then \
	   ln -sf "$$TINYFISH_BIN" web-via-tinyfish/bin/tinyfish; \
	   echo "  ✓ web-via-tinyfish/bin/tinyfish → $$TINYFISH_BIN"; \
	 else \
	   echo "  ✗ TinyFish CLI missing at $$TINYFISH_BIN"; exit 1; \
	 fi; \
	 if [ -x "$$NODE_BIN_PATH" ]; then \
	   ln -sf "$$NODE_BIN_PATH" web-via-tinyfish/bin/node; \
	   echo "  ✓ web-via-tinyfish/bin/node → $$NODE_BIN_PATH"; \
	 else \
	   echo "  ✗ pinned Node missing — run 'make setup-node'"; exit 1; \
	 fi
	@echo "Skill bins ready."

# AnyDoc is a library, so the repository owns one small standalone CLI adapter.
# Its source is a normal workspace crate, not vendored inside the skill. The
# skill only exposes the resulting executable through its generic bin/ boundary.
setup-document-to-markdown:
	@CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --locked \
		--manifest-path ../Cargo.toml -p document-to-markdown-cli $(DOCUMENT_TO_MARKDOWN_CARGO_FLAGS)
	@mkdir -p document-to-markdown/bin
	@ln -sf "$(DOCUMENT_TO_MARKDOWN_BIN)" document-to-markdown/bin/document-to-markdown
	@echo "  ✓ document-to-markdown/bin/document-to-markdown → $(DOCUMENT_TO_MARKDOWN_BIN)"

# OfficeCLI is one shared binary behind three least-privilege skill surfaces:
# office-word, office-excel, and office-powerpoint. The setup script downloads
# a pinned GitHub release, checks the committed SHA-256, then creates per-skill
# bin symlinks. It does not run OfficeCLI's upstream self-installer, modify
# developer-agent config, or enable auto-updates.
setup-officecli:
	@python3 scripts/setup_officecli.py

verify-officecli:
	@python3 scripts/setup_officecli.py --verify-only

# pdftotext skill: poppler-utils binaries (pdftotext, pdftoppm). The
# `pdftotext` skill is a thin wrapper around the host's poppler install.
# We don't auto-install via brew/apt — the user owns the host package
# manager. Instead we resolve the binaries already on PATH and symlink
# them into `skillshub/pdftotext/bin/` so the skill folder is the
# stable reference (and the dispatcher's PATH-prepend of
# `<skill_dir>/bin` resolves them at runtime). Idempotent.
setup-pdftotext:
	@PDFTOTEXT_BIN="$$(command -v pdftotext 2>/dev/null)"; \
	 if [ -z "$$PDFTOTEXT_BIN" ]; then \
	   echo "  ✗ pdftotext not found on PATH. Install poppler-utils, then re-run:"; \
	   echo "      macOS:    brew install poppler"; \
	   echo "      Debian:   apt install poppler-utils"; \
	   echo "      Arch:     pacman -S poppler"; \
	   echo "      RHEL:     dnf install poppler-utils"; \
	   exit 1; \
	 fi; \
	 mkdir -p pdftotext/bin; \
	 ln -sf "$$PDFTOTEXT_BIN" pdftotext/bin/pdftotext; \
	 echo "  ✓ pdftotext/bin/pdftotext → $$PDFTOTEXT_BIN"; \
	 PDFTOPPM_BIN="$$(command -v pdftoppm 2>/dev/null)"; \
	 if [ -n "$$PDFTOPPM_BIN" ]; then \
	   ln -sf "$$PDFTOPPM_BIN" pdftotext/bin/pdftoppm; \
	   echo "  ✓ pdftotext/bin/pdftoppm  → $$PDFTOPPM_BIN"; \
	 fi

# media-fetch skill: ffmpeg/ffprobe binaries. Same model as setup-pdftotext —
# symlink the host's ffmpeg/ffprobe into `skillshub/media-fetch/bin/` rather
# than declaring the binaries by their real path. ffmpeg lives in a directory
# (e.g. /opt/homebrew/bin) shared with the entire Homebrew install, and the
# governed runtime builds the sandboxed PATH only from directories that
# resolve a pack's declared `bins`. Declaring ffmpeg directly would put all
# of Homebrew on that governed PATH; a pack-local symlink resolves it without
# widening the sandbox. Idempotent.
setup-media-fetch:
	@FFMPEG_BIN="$$(command -v ffmpeg 2>/dev/null)"; \
	 FFPROBE_BIN="$$(command -v ffprobe 2>/dev/null)"; \
	 if [ -z "$$FFMPEG_BIN" ] || [ -z "$$FFPROBE_BIN" ]; then \
	   echo "  ✗ ffmpeg/ffprobe not found on PATH. Install, then re-run:"; \
	   echo "      macOS:    brew install ffmpeg"; \
	   echo "      Debian:   apt install ffmpeg"; \
	   exit 1; \
	 fi; \
	 mkdir -p media-fetch/bin; \
	 ln -sf "$$FFMPEG_BIN"  media-fetch/bin/ffmpeg; \
	 ln -sf "$$FFPROBE_BIN" media-fetch/bin/ffprobe; \
	 echo "  ✓ media-fetch/bin/ffmpeg  → $$FFMPEG_BIN"; \
	 echo "  ✓ media-fetch/bin/ffprobe → $$FFPROBE_BIN"

# ocr skill: tesseract binary. Same model as setup-pdftotext — symlink
# the host's tesseract into `skillshub/ocr/bin/`. Tessdata stays where
# the host package manager installed it (tesseract resolves it via
# its compiled-in TESSDATA_PREFIX); no env-var fiddling required.
setup-ocr:
	@TESSERACT_BIN="$$(command -v tesseract 2>/dev/null)"; \
	 if [ -z "$$TESSERACT_BIN" ]; then \
	   echo "  ✗ tesseract not found on PATH. Install tesseract + lang data, then re-run:"; \
	   echo "      macOS:    brew install tesseract tesseract-lang"; \
	   echo "      Debian:   apt install tesseract-ocr tesseract-ocr-all"; \
	   echo "      Arch:     pacman -S tesseract tesseract-data-eng"; \
	   echo "      RHEL:     dnf install tesseract tesseract-langpack-eng"; \
	   exit 1; \
	 fi; \
	 mkdir -p ocr/bin; \
	 ln -sf "$$TESSERACT_BIN" ocr/bin/tesseract; \
	 echo "  ✓ ocr/bin/tesseract → $$TESSERACT_BIN"

# Bootstrap scripts classify governed skill frontmatter before the complete
# skill runtime venv necessarily exists. Prefer an existing venv or system
# install, use uv for a lightweight project-local install when available, and
# retain the python.org macOS distribution's working pip --user fallback.
setup-python-bootstrap:
	@if [ -x .venv/bin/python ] && .venv/bin/python -c 'import yaml' >/dev/null 2>&1; then \
	  echo "  ✓ PyYAML available in skillshub/.venv"; \
	elif python3 -c 'import yaml' >/dev/null 2>&1; then \
	  echo "  ✓ PyYAML available to $$(python3 --version 2>&1)"; \
	elif command -v uv >/dev/null 2>&1; then \
	  echo "  Installing bootstrap PyYAML in skillshub/.venv..."; \
	  if [ ! -x .venv/bin/python ]; then uv venv .venv --python 3.12 --quiet; fi; \
	  uv pip install --python .venv/bin/python PyYAML --quiet; \
	  .venv/bin/python -c 'import yaml'; \
	else \
	  echo "  Installing bootstrap PyYAML for $$(python3 --version 2>&1)..."; \
	  python3 -m pip install --user PyYAML; \
	  python3 -c 'import yaml'; \
	fi

# Aggregate per-skill `metadata.magician.requires.python_packages` from
# every SKILL.md and install the unique set into a single venv at
# `skillshub/.venv/`. Requires uv. Idempotent.
# Fails loudly when the shared skill environment is missing or incomplete:
# youtube-search, media-fetch and other Python skills import their packages
# from it (the dispatcher prepends .venv/bin to the skill PATH), and a
# silently absent .venv shows up only later as a failed tool call.
verify-python:
	@test -x .venv/bin/python || { echo "  ✗ skillshub/.venv missing — run 'make -C skillshub setup-python'"; exit 1; }
	@.venv/bin/python -c 'import yaml, yt_dlp' || { echo "  ✗ skillshub/.venv incomplete — run 'make -C skillshub setup-python'"; exit 1; }
	@echo "  ✓ skillshub/.venv verified: $$( .venv/bin/python --version )"

setup-python: setup-python-bootstrap
	@command -v uv >/dev/null 2>&1 || { \
	  echo "  ✗ uv not found — install via 'curl -LsSf https://astral.sh/uv/install.sh | sh'"; exit 1; \
	}
	@echo "Aggregating python_packages from skill SKILL.md frontmatters..."
	@python3 scripts/aggregate_python_packages.py
	@echo "Creating/refreshing .venv ..."
	@if [ ! -x .venv/bin/python ]; then \
	  uv venv .venv --python 3.12 --quiet; \
	fi
	@uv pip install --python .venv/bin/python -r requirements.txt --quiet
	@echo "  ✓ .venv ready: $$( .venv/bin/python --version )"
	@echo "  Magician's cli_template dispatcher auto-prepends skillshub/.venv/bin to skill subprocess PATH — no env var needed."

# Aggregate per-skill `config/.env.example` templates into the scope-level
# `.env.example` and seed `.env` if missing. Idempotent: never overwrites
# existing `.env`.
#
# Optional: SCOPE=<principal>/<workspace> (defaults to anonymous/default).
setup-env:
	@SCOPE_ARG=""; \
	 if [ -n "$(SCOPE)" ]; then SCOPE_ARG="--scope $(SCOPE)"; fi; \
	 python3 scripts/aggregate_env_examples.py $$SCOPE_ARG --data-root "$(DATA_ROOT)"

# Bootstrap per-account auth dirs for the Google Workspace skills
# (gmail, calendar, sheets — all three use the shared `gws` CLI).
# Account list comes from `skillshub/operator-config.yaml`'s
# `gws_accounts:` section. Each entry has a `name:` (used for the
# `gws-<name>` auth dir) and an optional `expected_email:` (consumed
# by `setup_bot_envs.py` to pin GWS_EXPECTED_EMAIL in the gmail bot
# env, so the auth flow refuses to bind a different Google identity).
# OAuth client comes from `MAGICIAN_ROOT_DIR/client_secret.json` (in-tree
# `skillshub/client_secret.json` is the dev fallback). The script
# copies `client_secret.json` into each `<scope>/auth/gws-<account>/`
# as a real file (no symlinks — each account's auth dir is fully
# self-contained). Idempotent: existing OAuth tokens are preserved;
# only `client_secret.json` is refreshed when the source copy is newer.
# Falls back to legacy <repo>/{accounts.txt, client_secret.json} when
# the new files are absent — emits a deprecation warning.
#
# Optional: SCOPE=<principal>/<workspace> (defaults to anonymous/default).
setup-gws-accounts:
	@SCOPE_ARG=""; \
	 if [ -n "$(SCOPE)" ]; then SCOPE_ARG="--scope $(SCOPE)"; fi; \
	 python3 scripts/setup_gws_accounts.py $$SCOPE_ARG --data-root "$(DATA_ROOT)"

# Generate per-bot runtime env files at <scope>/bots/<bot>/ from the
# repo-root secrets source (.env.copy.development). Multi-account bots
# (gmail) get one file per account in <repo>/accounts.txt; single-
# instance bots (kapso, telegram, …) get .env.development. Idempotent
# — existing files are kept; pass FORCE=1 to overwrite.
#
# Optional: SCOPE=<principal>/<workspace> (defaults to anonymous/default)
# Optional: FORCE=1 to regenerate over existing files.
setup-bot-envs:
	@SCOPE_ARG=""; \
	 if [ -n "$(SCOPE)" ]; then SCOPE_ARG="--scope $(SCOPE)"; fi; \
	 FORCE_ARG=""; \
	 if [ -n "$(FORCE)" ]; then FORCE_ARG="--force"; fi; \
	 python3 scripts/setup_bot_envs.py $$SCOPE_ARG $$FORCE_ARG --data-root "$(DATA_ROOT)"

# Build all bot packages (sdk first, then telegram + whatsapp + others).
# `setup-deps` is the workspace install; `setup-bots` is the per-package
# `npm run build`.
# Per-bot mtime check. Rebuilds only when src/ has files newer than
# dist/index.js (or when the SDK rebuilt and is newer than the bundle).
# `setup-bots FORCE=1` always rebuilds.
BOT_SDK_BUNDLE := bots/sdk/dist/index.js
LAUNCHABLE_BOTS := telegram telegram-self whatsapp kapso gmail agentmail

setup-bots: setup-deps
	@# 1. Rebuild SDK only when its src is newer than its tsc output.
	@# Fail fast on tsc errors — silent failure here propagates a stale
	@# or missing dist/index.js into every consumer bot's bundle.
	@export PATH="$(NODE_BIN):$$PATH"; \
	 if [ -n "$(FORCE)" ] || [ ! -f "$(BOT_SDK_BUNDLE)" ] || \
	   [ -n "$$(find bots/sdk/src -type f -newer $(BOT_SDK_BUNDLE) 2>/dev/null | head -1)" ]; then \
	  echo "  building @magician/bot-sdk..."; \
	  npm --prefix . run build -w @magician/bot-sdk >/dev/null \
	    || { echo "  ✗ bot-sdk: tsc failed (re-run without redirection to see errors)"; exit 1; }; \
	  echo "  ✓ bot-sdk: built ($$(du -sh $(BOT_SDK_BUNDLE) 2>/dev/null | cut -f1))"; \
	else \
	  echo "  ✓ bot-sdk: up to date ($$(du -sh $(BOT_SDK_BUNDLE) 2>/dev/null | cut -f1))"; \
	fi
	@# 2. esbuild-bundle each launchable bot only when needed.
	@# Fail fast: a silently-failed esbuild leaves a missing or stale
	@# dist/index.js, which install-bot-bundles would copy into the
	@# scope and the bot daemon would crash at startup with a confusing
	@# error far from the cause.
	@export PATH="$(NODE_BIN):$$PATH"; \
	 for bot in $(LAUNCHABLE_BOTS); do \
	  bundle="bots/$$bot/dist/index.js"; \
	  needs_build="0"; \
	  if [ -n "$(FORCE)" ] || [ ! -f "$$bundle" ]; then \
	    needs_build="1"; \
	  elif [ -n "$$(find bots/$$bot/src -type f -newer $$bundle 2>/dev/null | head -1)" ]; then \
	    needs_build="1"; \
	  elif [ "$(BOT_SDK_BUNDLE)" -nt "$$bundle" ]; then \
	    needs_build="1"; \
	  fi; \
	  if [ "$$needs_build" = "1" ]; then \
	    echo "  bundling $$bot..."; \
	    case "$$bot" in \
	      whatsapp) extra_args="--external @ibrahimwithi/wu-cli" ;; \
	      *) extra_args="" ;; \
	    esac; \
	    ( cd bots/$$bot && node ../build_bundle.mjs $$extra_args >/dev/null ) \
	      || { echo "  ✗ $$bot: esbuild failed"; exit 1; }; \
	    [ -f "$$bundle" ] || { echo "  ✗ $$bot: bundle missing post-build"; exit 1; }; \
	    echo "  ✓ $$bot: bundled ($$(du -sh $$bundle 2>/dev/null | cut -f1))"; \
	  else \
	    echo "  ✓ $$bot: up to date ($$(du -sh $$bundle 2>/dev/null | cut -f1))"; \
	  fi; \
	done

# Install the freshly-bundled bot dist/index.js files into the scope's
# bots/ tree. The runtime also does this on next workspace open via
# `materialize_scope` → `sync_bot_bundles`; this target lets the user
# get a runnable scope without booting magician first. Idempotent.
#
# Optional: SCOPE=<principal>/<workspace>  (defaults to anonymous/default)
# Optional: ALL=1   to install into every scope under data-root/scopes/
install-bot-bundles:
	@SCOPE_ARG=""; \
	 if [ -n "$(SCOPE)" ]; then SCOPE_ARG="--scope $(SCOPE)"; fi; \
	 ALL_ARG=""; \
	 if [ -n "$(ALL)" ]; then ALL_ARG="--all-scopes"; fi; \
	 python3 scripts/install_bot_bundles.py $$SCOPE_ARG $$ALL_ARG --data-root "$(DATA_ROOT)"

build-bots: setup-deps
	@$(ENSURE_CAPABILITY_NODE_RUNTIME); \
	 echo "Building all bot packages..."; \
	 npm run build -w @magician/bots
	@echo "All bot packages built!"

check-bots: setup-deps
	@$(ENSURE_CAPABILITY_NODE_RUNTIME); \
	 echo "Type checking all bot packages..."; \
	 npm run check -w @magician/bots

# ============================================================================
# Marimo skill setup — dedicated venv at `skillshub/marimo/.venv/` with
# the heavy data-science deps that only marimo needs. We symlink
# `skillshub/marimo/bin/marimo` to the venv's marimo executable, so
# the dispatcher's PATH-prepend of `<skill_dir>/bin` resolves it at
# runtime without polluting the shared `skillshub/.venv/`.
# Idempotent: re-running just upgrades the packages in place.
# ============================================================================
MARIMO_VENV := marimo/.venv
MARIMO_BIN := marimo/bin

setup-marimo:
	@command -v uv >/dev/null 2>&1 || { \
	  echo "  ✗ uv not found — install via 'curl -LsSf https://astral.sh/uv/install.sh | sh'"; exit 1; \
	}
	@if [ ! -x $(MARIMO_VENV)/bin/python ]; then \
	  echo "  Creating dedicated marimo venv at $(MARIMO_VENV)..."; \
	  uv venv $(MARIMO_VENV) --python 3.12 --quiet; \
	fi
	@echo "  Installing marimo + data science libs into $(MARIMO_VENV)..."
	@uv pip install --python $(MARIMO_VENV)/bin/python --quiet --upgrade \
	  marimo pandas polars plotly matplotlib seaborn numpy scipy duckdb \
	  openpyxl xlsxwriter requests beautifulsoup4 lxml tabulate
	@mkdir -p $(MARIMO_BIN)
	@ln -sf "$(abspath $(MARIMO_VENV))/bin/marimo" $(MARIMO_BIN)/marimo
	@echo "  ✓ marimo/bin/marimo → $$($(MARIMO_BIN)/marimo --version 2>/dev/null || echo 'unknown')"

# ============================================================================
# Agent-browser skill setup — own npm install + patched-binary overlay.
# Browser is the one skill that's NOT a workspace member.
# ============================================================================
AGENT_BROWSER_VERSION ?= 0.38.1-Magician.0
AGENT_BROWSER_UPSTREAM_TAG ?= v0.38.1
AGENT_BROWSER_UPSTREAM_VERSION := $(patsubst v%,%,$(AGENT_BROWSER_UPSTREAM_TAG))
AGENT_BROWSER_SKILL_DIR := browser
AGENT_BROWSER_SKILL_NODE_MODULES := $(AGENT_BROWSER_SKILL_DIR)/node_modules
AGENT_BROWSER_BIN := $(AGENT_BROWSER_SKILL_NODE_MODULES)/.bin/agent-browser
AGENT_BROWSER_NATIVE_BIN := $(AGENT_BROWSER_SKILL_NODE_MODULES)/agent-browser/bin/agent-browser-darwin-arm64
AGENT_BROWSER_VENDOR_DIR := $(AGENT_BROWSER_SKILL_DIR)/_vendor/v$(AGENT_BROWSER_VERSION)
AGENT_BROWSER_SKILL_BIN_DIR := $(AGENT_BROWSER_SKILL_DIR)/bin
AGENT_BROWSER_SKILL_BIN := $(AGENT_BROWSER_SKILL_BIN_DIR)/agent-browser
AGENT_BROWSER_CORE_SKILL := $(AGENT_BROWSER_SKILL_NODE_MODULES)/agent-browser/skill-data/core/SKILL.md
AGENT_BROWSER_DISCOVERY_VERIFY := $(AGENT_BROWSER_SKILL_DIR)/scripts/verify-skill-discovery.sh
AGENT_BROWSER_VENDOR_BIN := $(AGENT_BROWSER_VENDOR_DIR)/bin/agent-browser-darwin-arm64
AGENT_BROWSER_PATCH := $(AGENT_BROWSER_VENDOR_DIR)/patches/0001-magician-patches.patch
AGENT_BROWSER_SRC_CACHE := ../.cache/agent-browser-src-$(AGENT_BROWSER_UPSTREAM_TAG)-magician

ensure-agent-browser-vendor-bin:
	@if [ ! -x "$(AGENT_BROWSER_VENDOR_BIN)" ]; then \
	  echo "  Building the pinned agent-browser fork from its tracked patch..."; \
	  $(MAKE) --no-print-directory rebuild-agent-browser; \
	else \
	  echo "  ✓ local agent-browser build artifact present: $(AGENT_BROWSER_VENDOR_BIN)"; \
	fi

setup-agent-browser: setup-node setup-python-bootstrap ensure-agent-browser-vendor-bin
	@export PATH="$(NODE_BIN):$$PATH"; \
	 if [ -x "$(AGENT_BROWSER_BIN)" ] && \
	   [ -f "$(AGENT_BROWSER_CORE_SKILL)" ] && \
	   [ "$$(node -p "require('./$(AGENT_BROWSER_SKILL_NODE_MODULES)/agent-browser/package.json').version" 2>/dev/null)" = "$(AGENT_BROWSER_UPSTREAM_VERSION)" ] && \
	   [ -f "$(AGENT_BROWSER_VENDOR_BIN)" ] && \
	   cmp -s "$(AGENT_BROWSER_VENDOR_BIN)" "$(AGENT_BROWSER_NATIVE_BIN)" && \
	   [ "$$($(AGENT_BROWSER_BIN) --version 2>/dev/null)" = "agent-browser $(AGENT_BROWSER_VERSION)" ]; then \
	  echo "  ✓ agent-browser $(AGENT_BROWSER_VERSION) (skillshub/browser)"; \
	else \
	  echo "  Installing agent-browser@$(AGENT_BROWSER_UPSTREAM_TAG) at $(AGENT_BROWSER_SKILL_DIR)..."; \
	  ( cd $(AGENT_BROWSER_SKILL_DIR) && npm install --include=optional >/dev/null ); \
	  INST_VER="$$(node -p "require('./$(AGENT_BROWSER_SKILL_NODE_MODULES)/agent-browser/package.json').version" 2>/dev/null || true)"; \
	  if [ "$$INST_VER" != "$(AGENT_BROWSER_UPSTREAM_VERSION)" ]; then \
	    echo "  · npm left agent-browser $$INST_VER; reinstalling $(AGENT_BROWSER_UPSTREAM_VERSION)"; \
	    rm -rf "$(AGENT_BROWSER_SKILL_NODE_MODULES)/agent-browser"; \
	    ( cd $(AGENT_BROWSER_SKILL_DIR) && npm install --include=optional >/dev/null ); \
	    INST_VER="$$(node -p "require('./$(AGENT_BROWSER_SKILL_NODE_MODULES)/agent-browser/package.json').version" 2>/dev/null || true)"; \
	  fi; \
	  if [ "$$INST_VER" != "$(AGENT_BROWSER_UPSTREAM_VERSION)" ]; then \
	    echo "  ✗ installed npm agent-browser is '$$INST_VER', want '$(AGENT_BROWSER_UPSTREAM_VERSION)'"; \
	    exit 1; \
	  fi; \
	  if [ -f "$(AGENT_BROWSER_VENDOR_BIN)" ]; then \
	    echo "  Deploying patched native binary from $(AGENT_BROWSER_VENDOR_BIN)..."; \
	    if [ -f "$(AGENT_BROWSER_NATIVE_BIN)" ] && [ ! -f "$(AGENT_BROWSER_NATIVE_BIN).orig" ]; then \
	      cp "$(AGENT_BROWSER_NATIVE_BIN)" "$(AGENT_BROWSER_NATIVE_BIN).orig"; \
	    fi; \
	    cp "$(AGENT_BROWSER_VENDOR_BIN)" "$(AGENT_BROWSER_NATIVE_BIN)"; \
	  fi; \
	  if [ -x "$(AGENT_BROWSER_BIN)" ]; then \
	    echo "  ✓ agent-browser installed: $$($(AGENT_BROWSER_BIN) --version)"; \
	  else \
	    echo "  ✗ agent-browser install failed"; exit 1; \
	  fi; \
	fi
	@# Mirror the patched binary into the skill's generated bin/ so
	@# `make install-scope SCOPE=<id>` carries it into the runtime tree at
	@# `$(DATA_ROOT)/scopes/<id>/skills/browser/bin/agent-browser`.
	@# Skip the copy only when the skill bin/ is byte-identical to
	@# the vendored binary. Version alone is insufficient while iterating on a
	@# same-version patch: it can leave an older build in skillshub/browser/bin/.
	@mkdir -p "$(AGENT_BROWSER_SKILL_BIN_DIR)"
	@if [ -f "$(AGENT_BROWSER_VENDOR_BIN)" ]; then \
	  VENDOR_VER="$$( "$(AGENT_BROWSER_VENDOR_BIN)" --version 2>/dev/null || true )"; \
	  if [ -f "$(AGENT_BROWSER_SKILL_BIN)" ] && cmp -s "$(AGENT_BROWSER_VENDOR_BIN)" "$(AGENT_BROWSER_SKILL_BIN)"; then \
	    echo "  ✓ skill bin/ already current at $(AGENT_BROWSER_SKILL_BIN) ($$VENDOR_VER)"; \
	  else \
	    cp "$(AGENT_BROWSER_VENDOR_BIN)" "$(AGENT_BROWSER_SKILL_BIN)"; \
	    chmod +x "$(AGENT_BROWSER_SKILL_BIN)"; \
	    echo "  ✓ skill bin/ mirrored at $(AGENT_BROWSER_SKILL_BIN): $$VENDOR_VER"; \
	  fi; \
	fi

# Pre-test hygiene: terminate any agent-browser daemon spawned from THIS repo
# and clear the stale socket lock files under ~/.agent-browser, so a test run
# starts from a clean browser. Orphaned daemons (reparented to launchd) pile up
# across runs and squat default.sock — which flakes verify-agent-browser and can
# trigger EMFILE ("too many open files") retry storms as the skill re-spawns
# throwaway Chromes. The pgrep pattern is ANCHORED (^) to this repo's
# skillshub/browser path, so it matches only real daemons — never your personal
# Chrome, and never the make recipe's own shell (which starts with /bin/sh yet
# mentions "agent-browser" in its echo text). Never fails the build. Set
# AGENT_BROWSER_KEEP_DAEMONS=1 to skip when a live browser session is running.
AGENT_BROWSER_HOME ?= $(HOME)/.agent-browser
AGENT_BROWSER_PROC_RE := ^$(abspath $(AGENT_BROWSER_SKILL_DIR))/.*[a]gent-browser
kill-stale-agent-browser:
	@if [ "$(AGENT_BROWSER_KEEP_DAEMONS)" = "1" ]; then \
	  echo "  · agent-browser daemon kill skipped (AGENT_BROWSER_KEEP_DAEMONS=1)"; \
	else \
	  pids="$$(pgrep -f '$(AGENT_BROWSER_PROC_RE)' 2>/dev/null || true)"; \
	  if [ -n "$$pids" ]; then \
	    echo "  · terminating stale agent-browser daemon(s):" $$pids; \
	    kill -TERM $$pids 2>/dev/null || true; \
	    sleep 1; \
	    pids="$$(pgrep -f '$(AGENT_BROWSER_PROC_RE)' 2>/dev/null || true)"; \
	    if [ -n "$$pids" ]; then \
	      echo "  · SIGKILL survivors:" $$pids; \
	      kill -KILL $$pids 2>/dev/null || true; \
	    fi; \
	  else \
	    echo "  · no stale agent-browser daemons running"; \
	  fi; \
	  if [ -d "$(AGENT_BROWSER_HOME)" ]; then \
	    find "$(AGENT_BROWSER_HOME)" -maxdepth 1 \( -name '*.sock' -o -name '*.pid' -o -name '*.engine' -o -name '*.stream' -o -name '*.version' \) -delete 2>/dev/null || true; \
	    find "$(AGENT_BROWSER_HOME)" -maxdepth 1 -type d -name '_stale_*' -exec rm -rf {} + 2>/dev/null || true; \
	  fi; \
	  echo "  ✓ agent-browser daemon state clean"; \
	fi

# Verify the live runtime binary really is the patched fork. The native npm
# binary must report the fork version and pass codesign, while the mirrored
# scope binary must report the same version and discover the bundled core skill.
# (Prereq kill-stale-agent-browser clears leaked daemons + stale sockets first.)
verify-agent-browser: kill-stale-agent-browser
	@RUNTIME_BIN=$(AGENT_BROWSER_NATIVE_BIN); \
	SKILL_BIN=$(AGENT_BROWSER_SKILL_BIN); \
	if [ ! -x "$$RUNTIME_BIN" ]; then \
	  echo "  ✗ runtime binary missing: $$RUNTIME_BIN"; \
	  echo "    → run 'make setup-agent-browser'"; exit 1; \
	fi; \
	RT_VER=$$("$$RUNTIME_BIN" --version 2>/dev/null); \
	WANT_VER="agent-browser $(AGENT_BROWSER_VERSION)"; \
	if [ "$$RT_VER" != "$$WANT_VER" ]; then \
	  echo "  ✗ runtime version mismatch: got '$$RT_VER', want '$$WANT_VER'"; \
	  echo "    → patched binary not live; run 'make rebuild-agent-browser' to build+deploy it (needed after an upstream-tag bump), or 'make setup-agent-browser' if it's already built"; exit 1; \
	fi; \
	if ! codesign --verify "$$RUNTIME_BIN" >/dev/null 2>&1; then \
	  echo "  ✗ runtime binary signature invalid: $$RUNTIME_BIN"; \
	  echo "    → likely truncated or partially overwritten; run 'make setup-agent-browser'"; exit 1; \
	fi; \
	if [ ! -x "$$SKILL_BIN" ]; then \
	  echo "  ✗ scoped browser binary missing: $$SKILL_BIN"; \
	  echo "    → run 'make setup-agent-browser'"; exit 1; \
	fi; \
	SKILL_VER=$$("$$SKILL_BIN" --version 2>/dev/null); \
	if [ "$$SKILL_VER" != "$$WANT_VER" ]; then \
	  echo "  ✗ scoped browser version mismatch: got '$$SKILL_VER', want '$$WANT_VER'"; \
	  echo "    → run 'make setup-agent-browser'"; exit 1; \
	fi; \
	if ! bash "$(AGENT_BROWSER_DISCOVERY_VERIFY)" "$$SKILL_BIN" "$(AGENT_BROWSER_SKILL_NODE_MODULES)/agent-browser"; then \
	  echo "  ✗ scoped browser cannot discover its bundled core skill"; \
	  echo "    → run 'make setup-agent-browser'"; exit 1; \
	fi; \
	echo "  ✓ patched agent-browser deployed: $$RT_VER  (signature + scoped core skill valid)"

# Refresh the patched agent-browser source clone in $(AGENT_BROWSER_SRC_CACHE).
agent-browser-source:
	@mkdir -p ../.cache
	@if [ ! -d $(AGENT_BROWSER_SRC_CACHE)/.git ]; then \
	  echo "  Cloning agent-browser $(AGENT_BROWSER_UPSTREAM_TAG) into $(AGENT_BROWSER_SRC_CACHE)..."; \
	  rm -rf $(AGENT_BROWSER_SRC_CACHE); \
	  git clone --branch $(AGENT_BROWSER_UPSTREAM_TAG) https://github.com/vercel-labs/agent-browser.git $(AGENT_BROWSER_SRC_CACHE) >/dev/null 2>&1; \
	  echo "  Applying $(AGENT_BROWSER_PATCH) (--3way for upgrade resilience)..."; \
	  cd $(AGENT_BROWSER_SRC_CACHE) && git apply --3way $(abspath $(AGENT_BROWSER_PATCH)) || { \
	    echo ""; \
	    echo "  ⚠  Patch did not apply cleanly against $(AGENT_BROWSER_UPSTREAM_TAG)."; \
	    echo "     If you see <<<<<<< markers, resolve them in $(AGENT_BROWSER_SRC_CACHE),"; \
	    echo "     then re-run 'make rebuild-agent-browser' to capture the rebased patch."; \
	    exit 1; \
	  }; \
	  echo "  ✓ source ready at $(AGENT_BROWSER_SRC_CACHE)"; \
	else \
	  echo "  ✓ source already present at $(AGENT_BROWSER_SRC_CACHE) (edits preserved)"; \
	fi

# Rebuild the patched agent-browser binary, regenerate the tracked patch from
# the current source, and refresh the local (gitignored) build artifact.
rebuild-agent-browser: agent-browser-source
	@echo "  Building agent-browser (release, native arch)..."
	@mkdir -p $(AGENT_BROWSER_CARGO_TARGET_DIR)
	@cd $(AGENT_BROWSER_SRC_CACHE) && CARGO_TARGET_DIR="$(AGENT_BROWSER_CARGO_TARGET_DIR)" cargo build --release --manifest-path cli/Cargo.toml
	@echo "  Copying binary -> $(AGENT_BROWSER_VENDOR_BIN)..."
	@mkdir -p $(AGENT_BROWSER_VENDOR_DIR)/bin
	@cp $(AGENT_BROWSER_CARGO_TARGET_DIR)/release/agent-browser $(AGENT_BROWSER_VENDOR_BIN)
	@codesign --force --sign - $(AGENT_BROWSER_VENDOR_BIN) >/dev/null 2>&1 || true
	@echo "  Regenerating $(AGENT_BROWSER_PATCH) from current source..."
	@mkdir -p $(AGENT_BROWSER_VENDOR_DIR)/patches
	@cd $(AGENT_BROWSER_SRC_CACHE) && git -c core.fileMode=false diff $(AGENT_BROWSER_UPSTREAM_TAG) > $(abspath $(AGENT_BROWSER_PATCH))
	@echo "  ✓ rebuilt: $$($(AGENT_BROWSER_VENDOR_BIN) --version)  ($$( wc -l < $(AGENT_BROWSER_PATCH) ) lines in patch)"
	@if [ -f "$(AGENT_BROWSER_NATIVE_BIN)" ]; then \
	  cp $(AGENT_BROWSER_VENDOR_BIN) $(AGENT_BROWSER_NATIVE_BIN); \
	  echo "  ✓ deployed into $(AGENT_BROWSER_NATIVE_BIN)"; \
	else \
	  echo "  → run 'make setup-agent-browser' to install + deploy into the live runtime"; \
	fi

# ============================================================================
# Metabase CLI — generated via printing-press from a curated OpenAPI spec.
# Used by the `metabase` skill at runtime.
# ============================================================================
PP_VERSION ?= v4.0.3
PP_GO_VERSION ?= go1.23.0
PP_BIN := $(HOME)/go/bin/printing-press
PP_GO := $(HOME)/go/bin/$(PP_GO_VERSION)
# Metabase skill source + build outputs both live under
# skillshub/metabase/. Build artifacts go to skillshub/metabase/bin/
# (gitignored); `install-scope` carries them into the runtime tree
# at $(DATA_ROOT)/scopes/<id>/skills/metabase/bin/metabase-pp-cli.
METABASE_SKILL_DIR := metabase
METABASE_SCRIPTS_DIR := $(METABASE_SKILL_DIR)/scripts
METABASE_BIN_DIR := $(METABASE_SKILL_DIR)/bin
METABASE_SPEC := $(METABASE_SCRIPTS_DIR)/spec.json
METABASE_PP_BIN := $(METABASE_BIN_DIR)/metabase-pp-cli
METABASE_PP_BUILD_DIR := $(METABASE_SKILL_DIR)/.pp-build

setup-printing-press:
	@command -v go >/dev/null 2>&1 || { echo "  ✗ go is required (install go 1.21+ first)"; exit 1; }
	@if [ ! -x "$(PP_GO)" ]; then \
	  echo "  Installing Go $(PP_GO_VERSION) toolchain..."; \
	  go install golang.org/dl/$(PP_GO_VERSION)@latest; \
	  $(PP_GO) download; \
	else \
	  echo "  ✓ Go toolchain $(PP_GO_VERSION) present"; \
	fi
	@PP_VER_STRIPPED="$(PP_VERSION)"; PP_VER_STRIPPED="$${PP_VER_STRIPPED#v}"; \
	if [ ! -x "$(PP_BIN)" ] || ! "$(PP_BIN)" --version 2>/dev/null | grep -q "$$PP_VER_STRIPPED"; then \
	  echo "  Installing printing-press $(PP_VERSION)..."; \
	  GOBIN=$(HOME)/go/bin go install github.com/mvanhorn/cli-printing-press/v4/cmd/printing-press@$(PP_VERSION); \
	  $(PP_BIN) --version; \
	else \
	  echo "  ✓ printing-press $(PP_VERSION) present"; \
	fi

$(METABASE_PP_BIN): $(METABASE_SPEC) | setup-printing-press
	@$(MAKE) --no-print-directory regen-metabase-cli

setup-metabase-cli: $(METABASE_PP_BIN)
	@echo "  ✓ metabase-pp-cli ready: $$($(METABASE_PP_BIN) --version 2>/dev/null || echo 'unknown') (spec.json sha=$$(shasum $(METABASE_SPEC) 2>/dev/null | cut -c1-12))"

# Cloak Browser — stealth Chromium engine for the `browser` tool. The
# `cloakbrowser` pip package is aggregated into skillshub/.venv via
# `setup-python` (declared in skillshub/cloak-browser/SKILL.md). This
# target then fetches the actual stealth Chromium binary (~200MB) into
# ~/.cloakbrowser/ so the first real browser session doesn't pay the
# download cost. The binary is gitignored / never bundled (license).
CLOAK_BROWSER_SKILL_DIR := cloak-browser
CLOAK_BROWSER_INSTALL_SH := $(CLOAK_BROWSER_SKILL_DIR)/scripts/install.sh
CLOAK_BROWSER_WRAPPER_VERSION := 0.5.3
VENV_PYTHON := $(CURDIR)/.venv/bin/python

setup-cloak-browser:
	@if [ ! -x "$(VENV_PYTHON)" ]; then \
	  echo "  ✗ skillshub/.venv missing — run 'make setup-python' first"; exit 1; \
	fi
	@if ! "$(VENV_PYTHON)" -c 'import cloakbrowser' >/dev/null 2>&1; then \
	  echo "  ✗ cloakbrowser not in skillshub/.venv — confirm $(CLOAK_BROWSER_SKILL_DIR)/SKILL.md is being picked up by aggregate_python_packages.py, then re-run 'make setup-python'"; \
	  exit 1; \
	fi
	@if [ "$$($(VENV_PYTHON) -c 'from importlib.metadata import version; print(version("cloakbrowser"))')" != "$(CLOAK_BROWSER_WRAPPER_VERSION)" ]; then \
	  echo "  ✗ cloakbrowser wrapper must be $(CLOAK_BROWSER_WRAPPER_VERSION) — run 'make setup-python'"; exit 1; \
	fi
	@bash $(CLOAK_BROWSER_INSTALL_SH)

# Confirm CloakBrowser is wired up end-to-end: SDK importable, binary
# present on disk, wrapper script present + executable. Cheap; does not
# launch the browser. Use smoke-test.sh for an end-to-end exercise.
verify-cloak-browser:
	@PY="$(VENV_PYTHON)"; \
	if [ ! -x "$$PY" ]; then \
	  echo "  ✗ skillshub/.venv missing — run 'make setup-python'"; exit 1; \
	fi; \
	if ! "$$PY" -c 'import cloakbrowser' >/dev/null 2>&1; then \
	  echo "  ✗ cloakbrowser package not importable from $$PY"; exit 1; \
	fi; \
	WRAPPER_VER="$$($$PY -c 'from importlib.metadata import version; print(version("cloakbrowser"))')"; \
	if [ "$$WRAPPER_VER" != "$(CLOAK_BROWSER_WRAPPER_VERSION)" ]; then \
	  echo "  ✗ cloakbrowser wrapper $$WRAPPER_VER installed, want $(CLOAK_BROWSER_WRAPPER_VERSION)"; exit 1; \
	fi; \
	echo "  wrapper:    $$WRAPPER_VER"; \
	"$$PY" -c 'import sys; from cloakbrowser import binary_info; \
i=binary_info(); \
ok=isinstance(i,dict) and i.get("installed") and i.get("binary_path"); \
print("  binary path:", i.get("binary_path","?")); \
print("  version:    ", i.get("version","?")); \
print("  platform:   ", i.get("platform","?")); \
sys.exit(0 if ok else 1)' || { \
	  echo "  ✗ CloakBrowser binary not installed — run 'make setup-cloak-browser'"; exit 1; \
	}; \
	RESOLVER=$(CLOAK_BROWSER_SKILL_DIR)/scripts/resolve.py; \
	STEALTH_JS=$(CLOAK_BROWSER_SKILL_DIR)/scripts/stealth-init.js; \
	if [ ! -x "$$RESOLVER" ]; then \
	  echo "  ✗ resolver script missing or not executable: $$RESOLVER"; exit 1; \
	fi; \
	if [ ! -f "$$STEALTH_JS" ]; then \
	  echo "  ✗ stealth-init.js missing: $$STEALTH_JS"; exit 1; \
	fi; \
	"$$PY" "$$RESOLVER" >/dev/null || { \
	  echo "  ✗ resolve.py did not emit a clean JSON envelope"; exit 1; \
	}; \
	echo "  ✓ cloak-browser ready (SDK + binary + resolver + stealth-init.js)"

# Lightpanda — fast headless DOM engine behind the existing browser tool.
# agent-browser owns the CDP transport; Lightpanda's own Agent/LLM mode is
# never started. The official Homebrew formula tracks Lightpanda's nightly.
LIGHTPANDA_SKILL_DIR := lightpanda
LIGHTPANDA_INSTALL_SH := $(LIGHTPANDA_SKILL_DIR)/scripts/install.sh
LIGHTPANDA_RESOLVER := $(LIGHTPANDA_SKILL_DIR)/scripts/resolve.py

setup-lightpanda:
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		bash "$(LIGHTPANDA_INSTALL_SH)"; \
	elif command -v lightpanda >/dev/null 2>&1; then \
		lightpanda version; \
	else \
		echo "  ⚠️  Lightpanda automatic setup is macOS-only; install the official Linux nightly to enable the soft public-read preference."; \
	fi

verify-lightpanda:
	@python3 "$(LIGHTPANDA_RESOLVER)" >/dev/null
	@echo "  ✓ lightpanda ready (binary + agent-browser engine envelope; no embedded LLM)"

# Kapso CLI — one immutable install shared by every scope. `@kapso/cli` is
# pinned in the skillshub root package/lock pair, so `setup-deps` owns its
# installation beside the other shared npm-backed tools. Workspace-specific
# credentials and writable state are injected only when a scoped call runs.
KAPSO_CLI_BIN := node_modules/.bin/kapso

setup-kapso-cli: setup-deps
	@EXPECTED="$$(PATH="$(NODE_BIN):$$PATH" $(NODE_BIN)/node -p "require('./package.json').dependencies['@kapso/cli']")"; \
	 ACTUAL="$$(PATH="$(NODE_BIN):node_modules/.bin:$$PATH" $(KAPSO_CLI_BIN) --version 2>/dev/null || true)"; \
	 case "$$ACTUAL" in \
	   "@kapso/cli/$$EXPECTED "*) echo "  ✓ kapso $$EXPECTED (shared skillshub runtime)" ;; \
	   *) echo "  ✗ expected @kapso/cli $$EXPECTED at $(KAPSO_CLI_BIN), got: $${ACTUAL:-missing}" >&2; exit 1 ;; \
	 esac

# Higgsfield CLI — one authenticated CLI behind the generic `higgsfield`
# toolskill. The runtime deliberately gives CLI tools a scope-isolated HOME, so
# an interactive login performed in the operator's HOME would otherwise be
# invisible. Seed a private copy into the selected scope HOME; subsequent token
# refreshes stay scope-local. Existing scoped credentials are never overwritten
# unless FORCE=1 is explicit.
HOST_HIGGSFIELD_CONFIG_DIR ?= $(HOME)/.config/higgsfield

setup-higgsfield-cli:
	@HIGGSFIELD_BIN="$$(command -v higgsfield 2>/dev/null)"; \
	 if [ -z "$$HIGGSFIELD_BIN" ]; then \
	   echo "  ✗ higgsfield CLI not found on PATH"; \
	   echo "    install it, run 'higgsfield auth login', then retry"; \
	   exit 1; \
	 fi; \
	 HIGGSFIELD_LAUNCHER="$$(realpath "$$HIGGSFIELD_BIN" 2>/dev/null || true)"; \
	 if [ -z "$$HIGGSFIELD_LAUNCHER" ]; then \
	   echo "  ✗ unable to resolve the installed higgsfield executable"; \
	   exit 1; \
	 fi; \
	 HIGGSFIELD_VENDOR_BIN="$$(dirname "$$HIGGSFIELD_LAUNCHER")/../vendor/hf"; \
	 if [ -x "$$HIGGSFIELD_VENDOR_BIN" ]; then \
	   HIGGSFIELD_RUNTIME_BIN="$$(realpath "$$HIGGSFIELD_VENDOR_BIN")"; \
	 else \
	   HIGGSFIELD_RUNTIME_BIN="$$HIGGSFIELD_LAUNCHER"; \
	 fi; \
	 mkdir -p higgsfield/bin; \
	 ln -sf "$$HIGGSFIELD_RUNTIME_BIN" higgsfield/bin/higgsfield; \
	 echo "  ✓ higgsfield/bin/higgsfield → $$HIGGSFIELD_RUNTIME_BIN"; \
	 "$$HIGGSFIELD_RUNTIME_BIN" --version
	@SC="$(or $(SCOPE),anonymous/default)"; \
	 SRC="$(HOST_HIGGSFIELD_CONFIG_DIR)"; \
	 DST="$(DATA_ROOT)/scopes/$$SC/workdirs/home/.config/higgsfield"; \
	 if [ ! -f "$$SRC/credentials.json" ]; then \
	   echo "  ✗ no authenticated Higgsfield credentials at $$SRC/credentials.json"; \
	   echo "    run 'higgsfield auth login' as the operator, then retry"; \
	   exit 1; \
	 fi; \
	 mkdir -p "$$DST"; \
	 for FILE in config.json credentials.json; do \
	   if [ -f "$$SRC/$$FILE" ]; then \
	     if [ ! -f "$$DST/$$FILE" ] || [ -n "$(FORCE)" ]; then \
	       cp "$$SRC/$$FILE" "$$DST/$$FILE"; \
	       chmod 600 "$$DST/$$FILE"; \
	       echo "  ✓ seeded scoped Higgsfield $$FILE"; \
	     else \
	       echo "  ✓ kept existing scoped Higgsfield $$FILE"; \
	     fi; \
	   fi; \
	 done; \
	 echo "  ✓ Higgsfield auth ready for scope $$SC"

regen-metabase-cli: setup-printing-press
	@if [ ! -f "$(METABASE_SPEC)" ]; then \
	  echo "  ✗ $(METABASE_SPEC) missing — run 'make refresh-metabase-spec' first"; exit 1; \
	fi
	@mkdir -p $(METABASE_BIN_DIR)
	@echo "  Generating metabase CLI source from $(METABASE_SPEC)..."
	@rm -rf $(METABASE_PP_BUILD_DIR)
	@$(PP_BIN) generate \
	  --spec $(abspath $(METABASE_SPEC)) \
	  --name metabase \
	  --output $(abspath $(METABASE_PP_BUILD_DIR)) \
	  --spec-source official --lenient --validate=false --force >/dev/null
	@# 2026-05-23 — Re-apply hand-maintained patches to the auto-
	@# generated tree. Adds the query-result-aware slim (compactQueryResult)
	@# so card-query / dataset-query responses fit under the agent's
	@# 24 KB tool-result cap. See scripts/apply-postgen-patches.py.
	@echo "  Applying post-gen patches..."
	@python3 $(METABASE_SCRIPTS_DIR)/apply-postgen-patches.py | sed 's/^/    /'
	@echo "  Building $(METABASE_PP_BIN) (Go $(PP_GO_VERSION))..."
	@( cd $(METABASE_PP_BUILD_DIR) && $(PP_GO) mod tidy >/dev/null 2>&1 && \
	   $(PP_GO) build -ldflags='-s -w' -o $(abspath $(METABASE_PP_BIN)) ./cmd/metabase-pp-cli )
	@# Don't copy the PP-generated SKILL.md over the hand-authored one —
	@# PP emits an OpenClaw-style frontmatter (`name: pp-metabase`,
	@# `metadata.openclaw`) that doesn't match our AgentSkills v1 schema.
	@# The hand-authored skillshub/metabase/SKILL.md is the canonical body.
	@touch $(METABASE_PP_BIN)
	@echo "  ✓ metabase-pp-cli built: $$($(METABASE_PP_BIN) --version)  ($$(du -h $(METABASE_PP_BIN) | cut -f1))"

refresh-metabase-spec:
	@: "$${METABASE_BASE_URL:?Set METABASE_BASE_URL in your environment (e.g. source the scoped .env first)}"
	@bash $(METABASE_SCRIPTS_DIR)/refresh-spec.sh
