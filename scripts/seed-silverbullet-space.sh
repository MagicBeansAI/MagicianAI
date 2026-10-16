#!/usr/bin/env bash
# seed-silverbullet-space.sh
#
# Initialize a Magician RUNTIME ROOT (a `silverbullet_space` store) for a
# deployment or container. Places the runtime config + workspace-storage
# settings AT the runtime root so a mounted/bind-mounted root is self-contained,
# and (for migrations) copies an existing store's `scopes/` across.
#
# Seed vs runtime split (see docs/plans/2026-06-22-seed-runtime-split.md):
#   * The repo-root `magician-config.yaml` and `llm-router.yaml` are the
#     tracked dev/package config seeds.
#   * The SEED (`magician_data_v3`, in the repo) supplies the read-only
#     templates (agent_templates / db_templates / trust_policy_templates).
#     Templates are read from the seed at runtime (seed_root). A small set of
#     non-secret default scope files may be copied non-clobberingly when they
#     are runtime-authored specs rather than system templates.
#   * The RUNTIME ROOT (`MAGICIAN_ROOT_DIR`, e.g. ~/MagicianNotes) holds live data,
#     all at the TOP LEVEL (no system/ tree — that's seed-only): `scopes/`,
#     `wake_up_queue.json`, `secrets/`, and copies of `magician-config.yaml`
#     (which now carries the `workspace_storage:` provider selector inline) /
#     `.env` / `operator-config.yaml` (so a container with only the runtime root
#     mounted is self-contained).
#
# A fresh runtime root needs no bulk `scopes/` copy — agents/DBs/trust policies
# are materialized from the seed templates on first boot, and explicit default
# scope seed files are copied one by one without overwriting. Bulk `scopes/`
# migration only fires when an existing external local_file store is passed as
# the seed.
#
# Idempotent. Verifies the destination has no iCloud `dataless` placeholders.
#
# Usage:
#   scripts/seed-silverbullet-space.sh [SEED_DIR] [RUNTIME_ROOT]
# Env overrides:
#   SEED_SOURCE_STORE          seed dir (default <repo>/magician_data_v3)
#   MAGICIAN_ROOT_DIR / SEED_SPACE_PATH   runtime root (default ~/MagicianNotes)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

SEED="${1:-${SEED_SOURCE_STORE:-$ROOT_DIR/magician_data_v3}}"
SPACE="${2:-${MAGICIAN_ROOT_DIR:-${SEED_SPACE_PATH:-$HOME/MagicianNotes}}}"

err() { printf '  ERROR %s\n' "$*" >&2; }
ok()  { printf '  OK %s\n' "$*"; }

copy_if_absent() {
  local src="$1" dst="$2" label="$3"
  if [ -e "$dst" ]; then
    ok "kept existing $label"
  elif [ -f "$src" ]; then
    mkdir -p "$(dirname "$dst")"
    cp "$src" "$dst"
    ok "$label -> runtime root"
  else
    err "seed source missing for $label: $src"
  fi
}

# --- sanity ---------------------------------------------------------------
[ -d "$SEED" ] || { err "seed dir not found: $SEED"; exit 1; }
SEED="$(cd "$SEED" && pwd)"
REPO_SEED="$(cd "$ROOT_DIR/magician_data_v3" && pwd)"

echo "Initializing silverbullet_space runtime root:"
echo "  seed         : $SEED"
echo "  runtime root : $SPACE"
mkdir -p "$SPACE/secrets" "$SPACE/Inbox"

# --- 1. runtime config (container flow) ----------------------------------
# Copy magician-config.yaml to the runtime root so a container that only mounts
# the runtime root still finds the config (magician_config_path() checks
# MAGICIAN_ROOT_DIR first). Never overwrite an existing operator-edited runtime
# config. The tracked repo-root config is the seed.
config_seed="$ROOT_DIR/magician-config.yaml"
config_seed_label="magician-config.yaml"
if [ -f "$SPACE/magician-config.yaml" ]; then
  ok "kept existing runtime root magician-config.yaml"
elif [ -f "$config_seed" ]; then
  cp "$config_seed" "$SPACE/magician-config.yaml"
  ok "$config_seed_label -> runtime root (as magician-config.yaml, incl. workspace_storage.provider)"
else
  err "no config seed found at $ROOT_DIR/magician-config.yaml"
fi

# Required sibling of magician-config.yaml; also repair older populated roots.
copy_if_absent "$ROOT_DIR/llm-router.yaml" "$SPACE/llm-router.yaml" "LLM router tables"

# The decision engine's settings (models, operations, rollout); Magician's
# config keeps only how to reach the engine.
copy_if_absent "$ROOT_DIR/decision-engine.yaml" "$SPACE/decision-engine.yaml" "decision engine settings"

# --- 2b. operator config + secrets (container flow) ----------------------
# magician reads these from the runtime root (runtime_config_path prefers
# <root>/<file>, else the legacy in-tree path). Copy them across so a mounted
# root is self-contained — only when a source exists and the dest is absent
# (never clobber operator edits). .env / operator-config.yaml / client_secret.json
# carry real secrets supplied by the operator (templates in the seed:
# .env.example, operator-config.template.yaml).
for pair in \
  "$ROOT_DIR/.env::$SPACE/.env" \
  "$ROOT_DIR/skillshub/operator-config.yaml::$SPACE/operator-config.yaml" \
  "$ROOT_DIR/skillshub/client_secret.json::$SPACE/client_secret.json"; do
  src="${pair%%::*}"; dst="${pair##*::}"
  if [ -f "$src" ] && [ ! -f "$dst" ]; then
    cp "$src" "$dst"
    ok "$(basename "$src") -> runtime root"
  fi
done

# --- 3. migrate an existing local_file store's scopes/ (optional) --------
# The repo seed now contains a narrow non-secret default scope seed. Do not
# treat it as live runtime state. Bulk migration is only for an explicit external
# local_file store passed as SEED, and it must not overwrite files already seeded
# or edited in the runtime root.
if [ "$SEED" != "$REPO_SEED" ] && [ -d "$SEED/scopes" ]; then
  echo "==> migrating scopes/ from source store ..."
  rsync -a \
    --ignore-existing \
    --exclude='.DS_Store' \
    --exclude='lancedb.corrupt-*' \
    --exclude='*.incompatible-*' \
    "$SEED/scopes/" "$SPACE/scopes/"
  ok "scopes/ migrated ($(find "$SPACE/scopes" -type f 2>/dev/null | wc -l | tr -d ' ') files)"
else
  ok "no external scopes/ migration needed (fresh roots materialize templates at boot)"
fi

# --- 3b. default non-secret scoped harness seeds -------------------------
# Program docs are runtime-authored specs, not system templates. Seed the
# anonymous/default reliability lane and its required delegates after any
# external migration, so migrated operator-authored files win and defaults only
# fill missing gaps.
DEFAULT_SCOPE_SEED="$REPO_SEED/scopes/anonymous/default"
DEFAULT_SCOPE_ROOT="$SPACE/scopes/anonymous/default"
copy_if_absent \
  "$DEFAULT_SCOPE_SEED/programs/harness_reliability.md" \
  "$DEFAULT_SCOPE_ROOT/programs/harness_reliability.md" \
  "default harness reliability program"

for agent in harness-sre cto internal-system-analyst; do
  copy_if_absent \
    "$DEFAULT_SCOPE_SEED/agent_runtime/agents/$agent/definition.agent.yaml" \
    "$DEFAULT_SCOPE_ROOT/agent_runtime/agents/$agent/definition.agent.yaml" \
    "default agent definition $agent"
done

# --- 4. verify (no iCloud dataless placeholders) -------------------------
dataless="$(find "$SPACE" -type f -flags +dataless 2>/dev/null | wc -l | tr -d ' ')"
if [ "$dataless" != "0" ]; then
  err "$dataless dataless files under $SPACE — is it on iCloud? reads WILL hang."
else
  ok "0 dataless files (fully local — safe from the iCloud read-hang)"
fi

cat <<EONEXT

Done. Start magician with MAGICIAN_ROOT_DIR="$SPACE" (or it defaults there).
Templates are read from the repo seed ($SEED); all runtime data lives at $SPACE.
EONEXT
