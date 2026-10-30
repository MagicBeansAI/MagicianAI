#!/usr/bin/env bash
# cleanup.sh — standalone cleanup for Magician Desktop (container or native)
# Reads the install manifest and removes installed artifacts intelligently.
# Works even if the Tauri app is broken or uninstalled. Mirrors Desktop's
# cleanup (desktop/src-tauri/src/cleanup.rs): only what Desktop installed is
# removed, and a service is stopped or unregistered before its data goes.
set -euo pipefail

# ── Defaults ─────────────────────────────────────────────────────────────────
MODE="tools-and-data"
DRY_RUN=false

# ── Usage ────────────────────────────────────────────────────────────────────
usage() {
  cat <<'USAGE'
Usage: cleanup.sh [OPTIONS]

Remove Magician Desktop artifacts (container or native backend) based on the
install manifest. Services are stopped or unregistered before data is removed.

Options:
  --tools-and-data  Full cleanup: tools, container, data (default)
  --only-tools      Remove tools + container, keep data directories
  --only-data       Remove data directories only, keep tools (a native
                    backend Desktop installed is stopped first, then
                    re-seeded and started again)
  --dry-run         Show what would be removed without executing
  --help            Show this help message

Examples:
  cleanup.sh                    # full cleanup
  cleanup.sh --only-data        # remove data, keep tools
  cleanup.sh --dry-run          # preview what would happen
USAGE
}

# ── Argument parsing ─────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tools-and-data) MODE="tools-and-data"; shift ;;
    --only-tools)     MODE="only-tools";     shift ;;
    --only-data)      MODE="only-data";      shift ;;
    --dry-run)        DRY_RUN=true;          shift ;;
    --help|-h)        usage; exit 0 ;;
    *)
      echo "Error: unknown option '$1'" >&2
      echo "Run with --help for usage." >&2
      exit 1
      ;;
  esac
done

# ── Helpers ──────────────────────────────────────────────────────────────────

# Execute a command, or print it if in dry-run mode.
run_or_dry() {
  if $DRY_RUN; then
    echo "[DRY RUN] Would run: $*"
  else
    "$@" || true
  fi
}

# Execute a command with admin privileges, or print it if in dry-run mode.
# macOS: uses osascript (native password dialog)
# Linux: uses pkexec (polkit GUI prompt)
run_admin_or_dry() {
  local cmd="$*"
  if $DRY_RUN; then
    echo "[DRY RUN] Would run (admin): $cmd"
    return 0
  fi

  case "$(uname -s)" in
    Darwin)
      local escaped
      escaped=$(printf '%s' "$cmd" | sed 's/\\/\\\\/g; s/"/\\"/g')
      osascript -e "do shell script \"${escaped}\" with administrator privileges with prompt \"Magician needs to uninstall components\"" || true
      ;;
    Linux)
      pkexec bash -c "$cmd" || true
      ;;
    *)
      echo "  Warning: unsupported OS for admin escalation, trying direct execution"
      bash -c "$cmd" || true
      ;;
  esac
}

# Locate homebrew binary.
find_brew() {
  if [[ -x /opt/homebrew/bin/brew ]]; then
    echo /opt/homebrew/bin/brew
  elif [[ -x /usr/local/bin/brew ]]; then
    echo /usr/local/bin/brew
  elif command -v brew &>/dev/null; then
    command -v brew
  else
    echo ""
  fi
}

# The runtime root the registered native service uses (empty if unknown).
native_registered_root() {
  case "$(uname -s)" in
    Darwin)
      [[ -f "$NATIVE_PLIST" ]] && /usr/libexec/PlistBuddy -c "Print :EnvironmentVariables:MAGICIAN_ROOT_DIR" "$NATIVE_PLIST" 2>/dev/null
      ;;
    Linux)
      [[ -f "$NATIVE_UNIT" ]] && sed -n 's/^Environment="MAGICIAN_ROOT_DIR=\(.*\)"$/\1/p' "$NATIVE_UNIT" | head -1 | sed 's/\\"/"/g; s/\\\\/\\/g'
      ;;
  esac
  return 0
}

native_stop_service() {
  case "$(uname -s)" in
    Darwin) run_or_dry launchctl bootout "$NATIVE_TARGET" ;;
    Linux)  run_or_dry systemctl --user stop magician-backend.service ;;
  esac
}

native_start_service() {
  case "$(uname -s)" in
    Darwin)
      run_or_dry launchctl enable "$NATIVE_TARGET"
      run_or_dry launchctl bootstrap "gui/$(id -u)" "$NATIVE_PLIST"
      run_or_dry launchctl kickstart "$NATIVE_TARGET"
      ;;
    Linux) run_or_dry systemctl --user start magician-backend.service ;;
  esac
}

# Unregister Desktop's native service: bootout, remove the plist, and clear
# the `launchctl disable` override so a later install is not silently
# disabled (Linux: disable + remove the user unit).
native_unregister_service() {
  case "$(uname -s)" in
    Darwin)
      run_or_dry launchctl bootout "$NATIVE_TARGET"
      run_or_dry rm -f "$NATIVE_PLIST"
      run_or_dry launchctl enable "$NATIVE_TARGET"
      ;;
    Linux)
      run_or_dry systemctl --user disable --now magician-backend.service
      run_or_dry rm -f "$NATIVE_UNIT"
      run_or_dry systemctl --user daemon-reload
      ;;
  esac
}

# Re-seed a runtime root emptied under the native service, like Desktop's
# seed_runtime_root: the native launcher does not seed a missing config.
native_reseed_root() {
  local root="$1" seed="$NATIVE_EXPECTED_PREFIX/share/seed" pair source destination target
  # A symlinked root whose target was removed: recreate the target, keep the
  # link (the owner chose that layout).
  if [[ -L "$root" && ! -e "$root" ]]; then
    target=$(readlink "$root")
    [[ "$target" == /* ]] || target="$(dirname "$root")/$target"
    run_or_dry mkdir -p "$target"
  fi
  run_or_dry mkdir -p "$root" "$root/logs"
  for pair in magician-config.yaml:magician-config.yaml llm-router.yaml:llm-router.yaml \
      decision-engine.yaml:decision-engine.yaml .env.example:.env \
      operator-config.template.yaml:operator-config.yaml; do
    source="$seed/${pair%%:*}"
    destination="$root/${pair#*:}"
    if [[ -f "$source" && ! -e "$destination" ]]; then
      run_or_dry cp "$source" "$destination"
    fi
  done
}

# Whether $1 is a directory this run actually removed, or inside one. A
# refused or failed delete never counts, so nothing is seeded into an intact
# root.
REMOVED_DIRS=()
root_is_removed() {
  local root="$1" dir
  [[ -n "$root" ]] || return 1
  [[ ${#REMOVED_DIRS[@]} -eq 0 ]] && return 1
  for dir in "${REMOVED_DIRS[@]}"; do
    if [[ "$root" == "$dir" || "$root" == "$dir"/* ]]; then
      return 0
    fi
  done
  return 1
}

# ── Locate manifest ─────────────────────────────────────────────────────────
case "$(uname -s)" in
  Darwin) MANIFEST_DIR="$HOME/Library/Application Support/dev.magician.desktop" ;;
  Linux)  MANIFEST_DIR="$HOME/.config/magician" ;;
  *)      MANIFEST_DIR="$HOME/.config/magician" ;;
esac
MANIFEST_PATH="$MANIFEST_DIR/install-manifest.json"

if [[ ! -f "$MANIFEST_PATH" ]]; then
  echo "Error: install manifest not found at: $MANIFEST_PATH" >&2
  echo "" >&2
  echo "Manual cleanup instructions:" >&2
  echo "  1. Stop and remove the container:  docker rm -f magician" >&2
  echo "  2. Remove the container image:     docker rmi magician:latest" >&2
  echo "  3. Remove data directories:" >&2
  echo "       rm -rf \"\$HOME/Library/Application Support/dev.magician.desktop\"" >&2
  echo "       rm -rf \"\$HOME/.local/share/magician\"" >&2
  echo "  4. If you installed colima/docker via Magician:" >&2
  echo "       colima stop && brew uninstall colima docker" >&2
  exit 1
fi

# ── Require jq ───────────────────────────────────────────────────────────────
if ! command -v jq &>/dev/null; then
  echo "Error: jq is required but not installed." >&2
  echo "Install it with: brew install jq  (macOS) or apt-get install jq (Linux)" >&2
  exit 1
fi

# ── Parse manifest ───────────────────────────────────────────────────────────
MANIFEST=$(cat "$MANIFEST_PATH")

INSTALLED_AT=$(echo "$MANIFEST"       | jq -r '.installed_at // "unknown"')
PLATFORM=$(echo "$MANIFEST"           | jq -r '.platform // "unknown"')
RUNTIME=$(echo "$MANIFEST"            | jq -r '.runtime // "unknown"')
CONTAINER_NAME=$(echo "$MANIFEST"     | jq -r '.installed_by_us.container_name // "magician"')
CONTAINER_IMAGE=$(echo "$MANIFEST"    | jq -r '.installed_by_us.container_image // "magician:latest"')
CONTAINER_RUNTIME=$(echo "$MANIFEST"  | jq -r '.installed_by_us.container_runtime // ""')
LAUNCH_AGENT=$(echo "$MANIFEST"       | jq -r '.installed_by_us.launch_agent // false')
HOMEBREW_INSTALLED=$(echo "$MANIFEST" | jq -r '.installed_by_us.homebrew // false')
PRE_EXISTING_RUNTIME=$(echo "$MANIFEST" | jq -r '.pre_existing.container_runtime // false')
PRE_EXISTING_BREW=$(echo "$MANIFEST"  | jq -r '.pre_existing.homebrew // false')
PRE_EXISTING_NATIVE_SERVICE=$(echo "$MANIFEST" | jq -r '.pre_existing.native_service // false')
PRE_EXISTING_NATIVE_BACKEND=$(echo "$MANIFEST" | jq -r '.pre_existing.native_backend // false')
NATIVE_PREFIX=$(echo "$MANIFEST"      | jq -r '.installed_by_us.native_prefix // ""')

# Native backend (desktop/src-tauri/src/native_runtime.rs): RUNTIME_NAME,
# SERVICE_ID and install_prefix() must stay in step with these.
NATIVE_RUNTIME_NAME="Native services"
NATIVE_SERVICE_ID="ai.magicbeans.magican.backend"
NATIVE_EXPECTED_PREFIX="${MAGICIAN_PREFIX:-$HOME/.magician}"
NATIVE_PLIST="$HOME/Library/LaunchAgents/$NATIVE_SERVICE_ID.plist"
NATIVE_UNIT="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/magician-backend.service"
NATIVE_TARGET="gui/$(id -u)/$NATIVE_SERVICE_ID"
IS_NATIVE=false
[[ "$RUNTIME" == "$NATIVE_RUNTIME_NAME" ]] && IS_NATIVE=true
# Desktop's own startup service (not one that predates Desktop setup).
NATIVE_SERVICE_OURS=false
$IS_NATIVE && [[ "$PRE_EXISTING_NATIVE_SERVICE" != "true" ]] && NATIVE_SERVICE_OURS=true

# Read data_dirs as a bash array
DATA_DIRS=()
while IFS= read -r dir; do
  [[ -n "$dir" ]] && DATA_DIRS+=("$dir")
done < <(echo "$MANIFEST" | jq -r '.data_dirs[]? // empty')

# ── Print summary ────────────────────────────────────────────────────────────
echo "╔══════════════════════════════════════════════════════════╗"
echo "║            Magician Desktop — Cleanup                   ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""
echo "  Manifest:           $MANIFEST_PATH"
echo "  Installed at:       $INSTALLED_AT"
echo "  Platform:           $PLATFORM"
echo "  Runtime:            $RUNTIME"
echo "  Container runtime:  $CONTAINER_RUNTIME"
echo "  Container:          $CONTAINER_NAME ($CONTAINER_IMAGE)"
echo "  Pre-existing runtime: $PRE_EXISTING_RUNTIME"
echo "  Pre-existing brew:  $PRE_EXISTING_BREW"
echo "  LaunchAgent:        $LAUNCH_AGENT"
if $IS_NATIVE; then
  echo "  Native prefix:      ${NATIVE_PREFIX:-none} (pre-existing: $PRE_EXISTING_NATIVE_BACKEND)"
  echo "  Native service:     $NATIVE_SERVICE_ID (pre-existing: $PRE_EXISTING_NATIVE_SERVICE)"
fi
echo "  Mode:               $MODE"
if $DRY_RUN; then
  echo "  *** DRY RUN — no changes will be made ***"
fi
echo ""

# ── Step 1: Always stop + remove container (container installs) ─────────────
if $IS_NATIVE; then
  echo "── Native install: no container to remove."
else
  echo "── Stopping and removing container: $CONTAINER_NAME"

  if command -v docker &>/dev/null; then
    run_or_dry docker stop "$CONTAINER_NAME"
    run_or_dry docker rm -f "$CONTAINER_NAME"
  elif command -v container &>/dev/null; then
    run_or_dry container stop "$CONTAINER_NAME"
    run_or_dry container rm "$CONTAINER_NAME"
  else
    echo "  Warning: no container CLI found; skipping container removal."
  fi
fi

# ── Step 2: Remove tools (if requested) ─────────────────────────────────────
if [[ "$MODE" == "tools-and-data" || "$MODE" == "only-tools" ]]; then
  if ! $IS_NATIVE; then
  echo ""
  echo "── Removing container image: $CONTAINER_IMAGE"

  if command -v docker &>/dev/null; then
    run_or_dry docker rmi "$CONTAINER_IMAGE"
    # Remove rollback tag
    run_or_dry docker rmi "magician:previous"
  elif command -v container &>/dev/null; then
    run_or_dry container image remove "$CONTAINER_IMAGE"
    run_or_dry container image remove "magician:previous"
  else
    echo "  Warning: no container CLI found; skipping image removal."
  fi
  fi

  # Remove LaunchAgent if we installed it
  if [[ "$LAUNCH_AGENT" == "true" ]]; then
    echo ""
    echo "── Removing LaunchAgent"
    LAUNCH_AGENT_PLIST="$HOME/Library/LaunchAgents/dev.magician.desktop.plist"
    if [[ -f "$LAUNCH_AGENT_PLIST" ]]; then
      run_or_dry launchctl unload "$LAUNCH_AGENT_PLIST"
      run_or_dry rm -f "$LAUNCH_AGENT_PLIST"
    else
      echo "  LaunchAgent plist not found at $LAUNCH_AGENT_PLIST; skipping."
    fi
  fi

  # The desktop app's clean-quit marker (magic-supervisor's desktop watch).
  if [[ -e "$MANIFEST_DIR/desktop-app.quit" ]]; then
    echo ""
    echo "── Removing the desktop clean-quit marker"
    run_or_dry rm -f "$MANIFEST_DIR/desktop-app.quit"
  fi

  if $IS_NATIVE; then
    # Native backend: unregister the service, then remove the prefix — all
    # before any data directory is touched (Step 3).
    echo ""
    if $NATIVE_SERVICE_OURS; then
      echo "── Removing the native backend startup service: $NATIVE_SERVICE_ID"
      native_unregister_service
    else
      echo "── Native backend startup service existed before Desktop setup; left in place."
    fi
    echo ""
    if [[ "$PRE_EXISTING_NATIVE_BACKEND" == "true" ]]; then
      echo "── Native backend files existed before Desktop setup; left in place."
    elif [[ -z "$NATIVE_PREFIX" || "$NATIVE_PREFIX" != "$NATIVE_EXPECTED_PREFIX" || ! -f "$NATIVE_PREFIX/MANIFEST.yaml" ]]; then
      echo "── REFUSED to remove native backend files at '${NATIVE_PREFIX:-<none>}' (installation identity did not match $NATIVE_EXPECTED_PREFIX)."
    else
      echo "── Removing native backend files: $NATIVE_PREFIX"
      run_or_dry rm -rf "$NATIVE_PREFIX"
    fi
  # Remove container runtime only if we installed it (not pre-existing)
  elif [[ "$PRE_EXISTING_RUNTIME" == "false" ]]; then
    BREW=$(find_brew)
    echo ""
    echo "── Removing container runtime: $CONTAINER_RUNTIME (installed by Magician)"
    echo "   You may be prompted for your password."

    case "$CONTAINER_RUNTIME" in
      colima|colima+docker|docker+colima)
        if command -v colima &>/dev/null; then
          run_or_dry colima stop
        fi
        if [[ -n "$BREW" ]]; then
          run_admin_or_dry "$BREW" uninstall colima docker
        else
          echo "  Warning: brew not found; cannot uninstall colima/docker."
        fi
        ;;
      apple-container|container)
        if [[ -n "$BREW" ]]; then
          run_admin_or_dry "$BREW" uninstall container
        else
          echo "  Warning: brew not found; cannot uninstall container."
        fi
        ;;
      docker)
        # Linux: Docker was installed via get.docker.com with admin privileges
        echo "  Removing Docker packages (requires admin)..."
        run_admin_or_dry "apt-get remove -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin 2>/dev/null || yum remove -y docker-ce docker-ce-cli containerd.io 2>/dev/null || true"
        ;;
      *)
        echo "  Unknown container runtime '$CONTAINER_RUNTIME'; skipping runtime removal."
        ;;
    esac
  else
    echo ""
    echo "── Container runtime was pre-existing; skipping runtime removal."
  fi

  # Homebrew: never auto-remove, print manual instructions
  if [[ "$HOMEBREW_INSTALLED" == "true" && "$PRE_EXISTING_BREW" == "false" ]]; then
    echo ""
    echo "── Homebrew was installed by Magician."
    echo "   For safety, Homebrew is NOT automatically removed."
    echo "   To uninstall manually, run:"
    echo "     /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/uninstall.sh)\""
  fi
fi

# ── Step 3: Remove data directories (if requested) ──────────────────────────
NATIVE_RESEED_ROOT=""
NATIVE_ROOT=""
NATIVE_ROOT_RESOLVED=""
if [[ "$MODE" == "only-data" ]] && $IS_NATIVE; then
  echo ""
  if $NATIVE_SERVICE_OURS; then
    # Stop the backend before its data goes; re-seed and start it after.
    NATIVE_ROOT=$(native_registered_root)
    if [[ -n "$NATIVE_ROOT" ]]; then
      NATIVE_ROOT_RESOLVED=$(realpath "$NATIVE_ROOT" 2>/dev/null || echo "$NATIVE_ROOT")
    fi
    echo "── Stopping the native backend before removing its data"
    native_stop_service
  else
    echo "── Warning: the native backend service existed before Desktop setup and is left running; its data is removed under it."
  fi
fi

if [[ "$MODE" == "tools-and-data" || "$MODE" == "only-data" ]]; then
  echo ""
  echo "── Removing data directories"

  if [[ ${#DATA_DIRS[@]} -eq 0 ]]; then
    echo "  No data directories listed in manifest."
  else
    for dir in "${DATA_DIRS[@]}"; do
      # Safety: refuse to delete paths outside $HOME or containing ".."
      case "$dir" in
        *..*)
          echo "  REFUSED to delete '$dir' (contains '..' component)"
          continue
          ;;
      esac

      if [[ ! -e "$dir" ]]; then
        echo "  Already gone: $dir"
        continue
      fi

      # Resolve symlinks to validate the real target path
      resolved=$(realpath "$dir" 2>/dev/null || readlink -f "$dir" 2>/dev/null || echo "")
      if [[ -z "$resolved" ]]; then
        echo "  REFUSED to delete '$dir' (cannot resolve real path)"
        continue
      fi

      case "$resolved" in
        "$HOME"/*) ;; # OK — resolved path is under home directory
        *)
          echo "  REFUSED to delete '$dir' (resolves to '$resolved', not under \$HOME)"
          continue
          ;;
      esac

      # Remove the resolved target, like Desktop (a symlinked data folder's
      # real contents go; the link is left for the owner's layout).
      echo "  Removing: $dir"
      run_or_dry rm -rf "$resolved"
      if $DRY_RUN || [[ ! -e "$resolved" ]]; then
        REMOVED_DIRS+=("$dir" "$resolved")
      fi
    done
  fi
fi

if [[ -n "$NATIVE_ROOT" ]] && { root_is_removed "$NATIVE_ROOT" || root_is_removed "$NATIVE_ROOT_RESOLVED"; }; then
  NATIVE_RESEED_ROOT="$NATIVE_ROOT"
fi

if [[ "$MODE" == "only-data" ]] && $NATIVE_SERVICE_OURS; then
  echo ""
  if [[ -n "$NATIVE_RESEED_ROOT" ]]; then
    echo "── Re-seeding $NATIVE_RESEED_ROOT and starting the native backend again"
    native_reseed_root "$NATIVE_RESEED_ROOT"
  else
    echo "── Starting the native backend again"
  fi
  native_start_service
fi

# ── Step 4: Remove manifest itself (full cleanup only) ──────────────────────
if [[ "$MODE" == "tools-and-data" ]]; then
  echo ""
  echo "── Removing install manifest"
  run_or_dry rm -f "$MANIFEST_PATH"
  # Remove manifest directory if empty
  if [[ -d "$MANIFEST_DIR" ]]; then
    run_or_dry rmdir "$MANIFEST_DIR" 2>/dev/null
  fi
fi

# ── Done ─────────────────────────────────────────────────────────────────────
echo ""
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  Cleanup complete ($MODE)                     "
echo "╚══════════════════════════════════════════════════════════╝"
if $DRY_RUN; then
  echo "  (dry run — nothing was actually changed)"
fi
