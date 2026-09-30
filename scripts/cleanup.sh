#!/usr/bin/env bash
# cleanup.sh — standalone cleanup for Magician Desktop container environment
# Reads the install manifest and removes installed artifacts intelligently.
# Works even if the Tauri app is broken or uninstalled.
set -euo pipefail

# ── Defaults ─────────────────────────────────────────────────────────────────
MODE="tools-and-data"
DRY_RUN=false

# ── Usage ────────────────────────────────────────────────────────────────────
usage() {
  cat <<'USAGE'
Usage: cleanup.sh [OPTIONS]

Remove Magician Desktop container artifacts based on the install manifest.

Options:
  --tools-and-data  Full cleanup: tools, container, data (default)
  --only-tools      Remove tools + container, keep data directories
  --only-data       Remove data directories only, keep tools
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
echo "  Mode:               $MODE"
if $DRY_RUN; then
  echo "  *** DRY RUN — no changes will be made ***"
fi
echo ""

# ── Step 1: Always stop + remove container ───────────────────────────────────
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

# ── Step 2: Remove tools (if requested) ─────────────────────────────────────
if [[ "$MODE" == "tools-and-data" || "$MODE" == "only-tools" ]]; then
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

  # Remove container runtime only if we installed it (not pre-existing)
  if [[ "$PRE_EXISTING_RUNTIME" == "false" ]]; then
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

      echo "  Removing: $dir"
      run_or_dry rm -rf "$dir"
    done
  fi
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
