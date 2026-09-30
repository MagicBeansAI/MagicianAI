#!/usr/bin/env bash
# uninstall.sh — removes a Magician install. Ships in the package beside
# install.sh, so the same uninstaller serves a downloaded release and a locally
# built one; there is nothing route-specific about taking it away.
#
# Four things get touched by an install and they are not equally precious:
#
#   processes      stop them, or files are removed under a running binary
#   the prefix     ours, created by install.sh, safe to delete
#   /Applications  ours, but shared ground — asked for
#   the data root  YOURS. Notes, secrets, memory. Never removed unless you
#                  type its path, and never by --yes.
set -euo pipefail

PREFIX="${MAGICIAN_PREFIX:-$HOME/.magician}"
DATA_DIR="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
ASSUME_YES=0
DELETE_DATA=0
DRY_RUN=0

ok()   { printf '  OK %s\n' "$*"; }
err()  { printf '  ERROR %s\n' "$*" >&2; }
note() { printf '       %s\n' "$*"; }
act()  { if [ "$DRY_RUN" -eq 1 ]; then printf '  would %s\n' "$*"; else printf '  %s\n' "$*"; fi; }

usage() {
  cat <<USAGE
Remove a Magician install.

  --prefix DIR    the install to remove (default: \$HOME/.magician)
  --data-dir DIR  the runtime data root (default: \$HOME/MagicianNotes)
  --delete-data   also offer to remove the data root — still asks, and still
                  needs you to type its path
  --dry-run       print what would happen and change nothing
  --yes           do not ask about the install or the app. Never applies to
                  your data.
  --help          this text
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --prefix=*) PREFIX="${1#*=}"; shift ;;
    --data-dir) DATA_DIR="${2:?--data-dir needs a directory}"; shift 2 ;;
    --data-dir=*) DATA_DIR="${1#*=}"; shift ;;
    --delete-data) DELETE_DATA=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --yes|-y) ASSUME_YES=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) err "unknown argument: $1"; usage >&2; exit 2 ;;
  esac
done

ask() {
  # $1 question, $2 default (y|n). --yes answers the default only where the
  # default is yes; it never turns a no into a yes.
  local prompt="$1" default="$2" answer
  if [ "$ASSUME_YES" -eq 1 ] && [ "$default" = y ]; then return 0; fi
  if [ ! -t 0 ] && [ ! -r /dev/tty ]; then return 1; fi
  printf '  %s [%s] ' "$prompt" "$([ "$default" = y ] && echo Y/n || echo y/N)"
  read -r answer </dev/tty 2>/dev/null || answer=""
  answer="$(printf '%s' "$answer" | tr -d '\r' | tr '[:upper:]' '[:lower:]')"
  [ -z "$answer" ] && answer="$default"
  [ "$answer" = y ] || [ "$answer" = yes ]
}

# --- 1. stop what is running -------------------------------------------------
# Matched by executable path under the prefix, not by name. `pkill magician`
# on a developer's machine is a good way to kill something they were building.
echo "Stopping anything running from ${PREFIX}…"
if [ -x "$PREFIX/scripts/stop-ollama.sh" ]; then
  # Magician-owned daemons only; the script itself will not touch an Ollama
  # someone else started.
  act "stop the Magician-owned Ollama daemons"
  [ "$DRY_RUN" -eq 1 ] || bash "$PREFIX/scripts/stop-ollama.sh" >/dev/null 2>&1 || true
fi

stopped=0
for binary in magic-supervisor magician magicutor; do
  target="$PREFIX/$binary.bin"
  [ -e "$target" ] || continue
  # -f matches the full command line, and the prefix makes it specific to this
  # install rather than to every binary of that name on the machine.
  pids="$(pgrep -f "^$target" 2>/dev/null || true)"
  [ -z "$pids" ] && continue
  act "stop $binary (pid $(printf '%s' "$pids" | tr '\n' ' '))"
  if [ "$DRY_RUN" -eq 0 ]; then
    # TERM, a moment, then KILL only what ignored it.
    printf '%s\n' "$pids" | while read -r pid; do [ -n "$pid" ] && kill "$pid" 2>/dev/null || true; done
    sleep 1
    printf '%s\n' "$pids" | while read -r pid; do [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true; done
  fi
  stopped=$((stopped + 1))
done
[ "$stopped" -eq 0 ] && note "nothing was running"

# --- 2. the install itself ---------------------------------------------------
if [ -d "$PREFIX" ]; then
  version="$(awk '/^version:/{print $2; exit}' "$PREFIX/MANIFEST.yaml" 2>/dev/null || echo unknown)"
  if ask "Remove the install at $PREFIX (version $version)?" y; then
    act "remove $PREFIX"
    # Reporting a removal that a dry run did not perform is the one lie this
    # script must not tell, since a dry run is read as a rehearsal of the truth.
    if [ "$DRY_RUN" -eq 0 ]; then rm -rf "${PREFIX:?}"; ok "install removed"; fi
  else
    note "install kept"
  fi
else
  note "no install at $PREFIX"
fi

# --- 3. the desktop app ------------------------------------------------------
for app in /Applications/Magician.app /Applications/magician.app; do
  [ -d "$app" ] || continue
  if ask "Remove $app?" n; then
    act "remove $app"
    if [ "$DRY_RUN" -eq 0 ]; then rm -rf "$app"; ok "desktop app removed"; fi
  else
    note "$app kept"
  fi
done

# --- 4. your data ------------------------------------------------------------
# Deliberately the most awkward step in the script. Everything above can be
# reinstalled in minutes; this cannot be recovered at all.
if [ "$DELETE_DATA" -eq 1 ] && [ -d "$DATA_DIR" ]; then
  size="$(du -sh "$DATA_DIR" 2>/dev/null | cut -f1 || echo unknown)"
  echo
  echo "  $DATA_DIR holds your notes, secrets and memory — $size."
  echo "  Removing it cannot be undone, and nothing else here can bring it back."
  if [ "$DRY_RUN" -eq 1 ]; then
    act "ask you to type the path, then remove $DATA_DIR"
  elif [ ! -r /dev/tty ]; then
    note "not asking without a terminal — your data was left alone"
  else
    printf '  Type the full path to confirm, or anything else to keep it:\n  '
    read -r typed </dev/tty 2>/dev/null || typed=""
    # A pty can deliver the carriage return, and so can a pasted line from a
    # Windows editor. Comparing paths should not hinge on that.
    typed="$(printf '%s' "$typed" | tr -d '\r')"
    if [ "$typed" = "$DATA_DIR" ]; then
      rm -rf "${DATA_DIR:?}"
      ok "data root removed"
    else
      note "that did not match — your data was left alone"
    fi
  fi
elif [ -d "$DATA_DIR" ]; then
  note "your data at $DATA_DIR was left alone (--delete-data to be asked about it)"
fi

echo
echo "Done."
