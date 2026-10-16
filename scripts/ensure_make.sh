#!/usr/bin/env bash
# Ensure GNU `make` is available on PATH. The desktop tray's Services
# submenu shells out to `make restart-magician`, `make stop-supervisor`,
# etc., and Tauri tray binaries launched from Finder/Spotlight inherit
# the system default PATH (not the user's shell PATH). If `make` is
# missing, the tray menu items silently fail; this script catches that
# before it bites.
#
# Behaviour by platform:
#   macOS  — `make` ships with the Xcode Command Line Tools. Triggers
#            the official installer dialog via `xcode-select --install`
#            and waits for the user to complete it before returning.
#   Linux  — apt / dnf / pacman / zypper; otherwise prints the install
#            hint and exits non-zero so the caller can surface the gap.
#   Other  — prints a hint and exits non-zero.
#
# After install, the script verifies `make` is now resolvable via the
# system default PATH (`/usr/bin:/bin:/usr/sbin:/sbin`) so the tray,
# which inherits that minimal PATH, can actually find it.

set -euo pipefail

if command -v make >/dev/null 2>&1; then
  echo "✓ make is already installed: $(command -v make)"
  exit 0
fi

uname_s="$(uname -s)"

case "$uname_s" in
  Darwin)
    echo "make not found. Installing Xcode Command Line Tools..."
    echo "  A system dialog will appear — accept and wait for the install"
    echo "  to finish, then re-run this script."
    if xcode-select -p >/dev/null 2>&1; then
      echo "  (CLT already reported as installed, but make is still"
      echo "   missing — possible broken install. Try:"
      echo "     sudo rm -rf /Library/Developer/CommandLineTools"
      echo "     xcode-select --install"
      echo "   then re-run.)"
      exit 1
    fi
    xcode-select --install || true
    echo ""
    echo "Waiting for CLT install to complete (will poll every 10s)..."
    while ! command -v make >/dev/null 2>&1; do
      sleep 10
      echo "  still waiting..."
    done
    ;;
  Linux)
    if command -v apt-get >/dev/null 2>&1; then
      echo "Installing make via apt..."
      sudo apt-get update && sudo apt-get install -y make
    elif command -v dnf >/dev/null 2>&1; then
      echo "Installing make via dnf..."
      sudo dnf install -y make
    elif command -v pacman >/dev/null 2>&1; then
      echo "Installing make via pacman..."
      sudo pacman -S --noconfirm make
    elif command -v zypper >/dev/null 2>&1; then
      echo "Installing make via zypper..."
      sudo zypper install -y make
    else
      echo "No recognised package manager. Install make manually then re-run." >&2
      exit 1
    fi
    ;;
  *)
    echo "Unsupported platform '$uname_s'. Install make manually." >&2
    exit 1
    ;;
esac

# Confirm the installed binary is reachable from the *system default*
# PATH that GUI apps (like the tray launched from Finder) inherit.
# `/usr/bin/make` after Xcode CLT and `/usr/bin/make` on most Linux
# distros both satisfy this — but verify rather than assume.
if PATH="/usr/bin:/bin:/usr/sbin:/sbin" command -v make >/dev/null 2>&1; then
  echo "✓ make installed and reachable from system PATH:"
  PATH="/usr/bin:/bin:/usr/sbin:/sbin" command -v make
else
  echo "⚠ make installed at $(command -v make) but NOT on the system" >&2
  echo "  default PATH that GUI apps inherit. Tray menu commands may" >&2
  echo "  still fail. Add a symlink to /usr/local/bin or restart the" >&2
  echo "  tray from a terminal so it picks up the current shell PATH." >&2
  exit 2
fi
