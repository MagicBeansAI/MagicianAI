#!/usr/bin/env bash
#
# setup-meet-bot.sh — host audio for the meeting bot.
#
# macOS:
#   Speak:  BlackHole 16ch, a virtual microphone Meet reads. No other API
#           creates that device. Homebrew's cask needs sudo and a reboot.
#   Route:  switchaudio-osx (SwitchAudioSource). The joiner points the system
#           default input at BlackHole for the meeting and restores it after.
#   Listen: ScreenCaptureKit via magician-macos-meet-audio.bin, plus the
#           Screen Recording privacy grant. BlackHole is not the listen path.
#
# Linux:
#   There is no BlackHole-style kernel driver. Join creates Pulse null-sinks
#   at meeting start (parec to listen, pacat to speak) and Xvfb when DISPLAY
#   is unset. pipewire-pulse is installed only when no Pulse daemon package
#   is already installed. An installed pulseaudio package is left alone.
#
# Wired into `make setup-meet-bot`, `make setup-all`, and the composed
# installer (`phase_meet_audio` in scripts/install.sh).
#
# MAGICIAN_MEET_BOT_SKIP_OLLAMA=1 skips the Ollama delegate. The composed
# installer sets it because phase_ollama already ran.
#
# Usage:  bash scripts/setup-meet-bot.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

log()  { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m[ok]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[!]\033[0m %s\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

# brew/apt will call sudo. A non-interactive shell with no cached credential
# must not hang the rest of the install.
can_take_sudo() {
  [ "$(id -u)" -eq 0 ] && return 0
  sudo -n true 2>/dev/null && return 0
  [ -t 0 ] || [ -t 1 ]
}

setup_macos() {
  log "macOS meeting audio"

  if ! have brew; then
    warn "Homebrew not found — install from https://brew.sh then re-run."
    warn "    brew install --cask blackhole-16ch && brew install switchaudio-osx"
    return 0
  fi

  if [ -d "/Library/Audio/Plug-Ins/HAL/BlackHole16ch.driver" ]; then
    ok "BlackHole 16ch installed (virtual microphone the bot speaks into)."
  elif ! can_take_sudo; then
    warn "BlackHole 16ch is not installed, and this shell cannot prompt for sudo."
    warn "    brew install --cask blackhole-16ch"
    warn "    Reboot afterwards. The cask does not take effect until then."
  else
    log "Installing BlackHole 16ch (this asks for your password)..."
    if HOMEBREW_NO_AUTO_UPDATE=1 brew install --cask blackhole-16ch; then
      ok "BlackHole 16ch package installed."
      warn "Reboot before the meeting bot speaks. Homebrew requires a reboot for this driver."
    else
      warn "brew install --cask blackhole-16ch failed."
      warn "Run that command yourself, then reboot."
    fi
  fi

  # BlackHole 2ch was the Spike 1 capture device. Production listen is
  # ScreenCaptureKit, so 2ch is leftover.
  if [ -d "/Library/Audio/Plug-Ins/HAL/BlackHole2ch.driver" ]; then
    warn "BlackHole 2ch is installed. Production listen does not use it."
    warn "    Safe to remove: brew uninstall --cask blackhole-2ch"
  fi

  if have SwitchAudioSource; then
    ok "switchaudio-osx present."
  else
    log "Installing switchaudio-osx..."
    if brew install switchaudio-osx; then
      ok "switchaudio-osx installed."
    else
      warn "switchaudio-osx install failed. The bot needs SwitchAudioSource to point Meet at BlackHole."
    fi
  fi

  if have SwitchAudioSource && [ -d "/Library/Audio/Plug-Ins/HAL/BlackHole16ch.driver" ]; then
    if SwitchAudioSource -a -t input 2>/dev/null | grep -q "BlackHole 16ch"; then
      ok "BlackHole 16ch is visible as an input device."
    else
      warn "BlackHole 16ch is on disk but Core Audio cannot see it yet. Reboot."
    fi
  fi

  if [ "${MAGICIAN_MEET_BOT_SKIP_OLLAMA:-0}" = 1 ]; then
    ok "Ollama left to the installer (MAGICIAN_MEET_BOT_SKIP_OLLAMA=1)."
  else
    log "Setting up configured Ollama models..."
    bash "$SCRIPT_DIR/setup-ollama-host.sh" || warn "Ollama setup failed — meeting summaries need it."
  fi

  local helper="${MAGICIAN_MACOS_MEET_AUDIO_BIN:-$REPO_ROOT/magician-macos-meet-audio.bin}"
  if [ -x "$helper" ]; then
    ok "meet-audio helper staged ($helper)."
  elif ! have swift; then
    warn "meet-audio helper is not staged and swift is unavailable."
    warn "    make build-macos-speech-helper"
  else
    log "Building the ScreenCaptureKit meet-audio helper..."
    if make -C "$REPO_ROOT" build-macos-speech-helper \
        && [ -x "$REPO_ROOT/magician-macos-meet-audio.bin" ]; then
      ok "meet-audio helper staged ($REPO_ROOT/magician-macos-meet-audio.bin)."
    else
      warn "meet-audio helper is still missing after make build-macos-speech-helper."
    fi
  fi

  cat <<'EOF'

────────────────────────────────────────────────────────────────────────────
Meeting audio on macOS

  Speak   BlackHole 16ch. Meet's microphone must be this device (or follow
          the system default — the joiner switches the default input for the
          meeting and puts it back on leave). Reboot once after install.
  Listen  ScreenCaptureKit, not BlackHole. System Settings → Privacy &
          Security → Screen Recording → allow the app that runs the bot.
  Route   SwitchAudioSource (switchaudio-osx), installed above.

BlackHole 2ch is the old capture spike. Production does not use it.
ffmpeg and sox are only for scripts/meet-bot/transcribe_loop.py.

Instructions: docs/components/magician/meetings.md
────────────────────────────────────────────────────────────────────────────
EOF
}

# stdout is "install" or "skip". pipewire-pulse Conflicts/Replaces pulseaudio,
# so a missing `pactl info` answer is not enough when a daemon package exists.
linux_pipewire_install_decision() {
  local info_ok="$1" daemon_present="$2"
  if [ "$info_ok" -eq 1 ] || [ "$daemon_present" -eq 1 ]; then
    printf '%s\n' skip
  else
    printf '%s\n' install
  fi
}

# True when a PulseAudio or PipeWire-Pulse daemon package is installed.
# A stopped user session still counts: replacing it would remove the desktop's server.
pulse_daemon_package_installed() {
  if have dpkg; then
    dpkg -s pulseaudio >/dev/null 2>&1 && return 0
    dpkg -s pipewire-pulse >/dev/null 2>&1 && return 0
  elif have rpm; then
    rpm -q pulseaudio >/dev/null 2>&1 && return 0
    rpm -q pipewire-pulseaudio >/dev/null 2>&1 && return 0
  elif have pacman; then
    pacman -Q pulseaudio >/dev/null 2>&1 && return 0
    pacman -Q pipewire-pulse >/dev/null 2>&1 && return 0
  fi
  return 1
}

install_linux_clients() {
  local need_cli="$1" need_xvfb="$2"
  if [ "$need_cli" -eq 0 ] && [ "$need_xvfb" -eq 0 ]; then
    return 0
  fi
  if ! can_take_sudo; then
    warn "Linux meeting-audio packages are missing, and this shell cannot prompt for sudo."
    warn "    Debian/Ubuntu: sudo apt-get install -y pulseaudio-utils xvfb"
    return 0
  fi
  if have apt-get; then
    local apt_pkgs=()
    [ "$need_cli" -eq 1 ] && apt_pkgs+=(pulseaudio-utils)
    [ "$need_xvfb" -eq 1 ] && apt_pkgs+=(xvfb)
    log "Installing ${apt_pkgs[*]}"
    if sudo apt-get update && sudo apt-get install -y "${apt_pkgs[@]}"; then
      ok "Linux meeting-audio client packages installed."
    else
      warn "apt-get install failed: ${apt_pkgs[*]}"
    fi
  elif have dnf; then
    local dnf_pkgs=()
    [ "$need_cli" -eq 1 ] && dnf_pkgs+=(pulseaudio-utils)
    [ "$need_xvfb" -eq 1 ] && dnf_pkgs+=(xorg-x11-server-Xvfb)
    log "Installing ${dnf_pkgs[*]}"
    if sudo dnf install -y "${dnf_pkgs[@]}"; then
      ok "Linux meeting-audio client packages installed."
    else
      warn "dnf install failed: ${dnf_pkgs[*]}"
    fi
  elif have pacman; then
    local pacman_pkgs=()
    [ "$need_cli" -eq 1 ] && pacman_pkgs+=(libpulse)
    [ "$need_xvfb" -eq 1 ] && pacman_pkgs+=(xorg-server-xvfb)
    log "Installing ${pacman_pkgs[*]}"
    if sudo pacman -S --needed --noconfirm "${pacman_pkgs[@]}"; then
      ok "Linux meeting-audio client packages installed."
    else
      warn "pacman install failed: ${pacman_pkgs[*]}"
    fi
  else
    warn "No apt-get, dnf, or pacman. Install pulseaudio-utils (pactl, parec, pacat) and Xvfb yourself."
  fi
}

install_pipewire_server() {
  if ! can_take_sudo; then
    warn "No Pulse daemon is installed, and this shell cannot prompt for sudo."
    warn "    Debian/Ubuntu: sudo apt-get install -y pipewire-pulse wireplumber"
    return 0
  fi
  if have apt-get; then
    log "Installing pipewire-pulse wireplumber"
    if sudo apt-get update && sudo apt-get install -y pipewire-pulse wireplumber; then
      ok "pipewire-pulse installed."
    else
      warn "apt-get install failed: pipewire-pulse wireplumber"
    fi
  elif have dnf; then
    log "Installing pipewire-pulseaudio wireplumber"
    if sudo dnf install -y pipewire-pulseaudio wireplumber; then
      ok "pipewire-pulseaudio installed."
    else
      warn "dnf install failed: pipewire-pulseaudio wireplumber"
    fi
  elif have pacman; then
    log "Installing pipewire-pulse wireplumber"
    if sudo pacman -S --needed --noconfirm pipewire-pulse wireplumber; then
      ok "pipewire-pulse installed."
    else
      warn "pacman install failed: pipewire-pulse wireplumber"
    fi
  else
    warn "No apt-get, dnf, or pacman. Install pipewire-pulse and wireplumber yourself."
  fi
}

# Client tools first. A Pulse daemon package is never replaced: pipewire-pulse
# Conflicts with pulseaudio, and `pactl info` fails from SSH when the desktop
# session is simply not visible to this shell.
setup_linux() {
  log "Linux meeting audio (userspace Pulse — no kernel driver)"

  local need_cli=0 need_xvfb=0
  if ! have pactl || ! have parec || ! have pacat; then need_cli=1; fi
  if ! have Xvfb; then need_xvfb=1; fi
  if [ "$need_cli" -eq 0 ] && [ "$need_xvfb" -eq 0 ]; then
    ok "pactl, parec, pacat, and Xvfb are present."
  else
    install_linux_clients "$need_cli" "$need_xvfb"
  fi

  local info_ok=0 daemon_present=0
  if pactl info >/dev/null 2>&1; then info_ok=1; fi
  if pulse_daemon_package_installed; then daemon_present=1; fi
  local decision
  decision="$(linux_pipewire_install_decision "$info_ok" "$daemon_present")"
  if [ "$decision" = skip ]; then
    if [ "$info_ok" -eq 1 ]; then
      ok "Pulse server is answering."
    else
      warn "A Pulse or PipeWire-Pulse package is installed, but pactl info did not connect."
      warn "Leaving that daemon in place. Start the desktop session's sound server, then re-run make setup-meet-bot."
    fi
  else
    warn "No Pulse daemon package is installed."
    install_pipewire_server
  fi

  cat <<'EOF'

────────────────────────────────────────────────────────────────────────────
Meeting audio on Linux

  There is no BlackHole equivalent. The devices are Pulse modules, created
  when a meeting starts, not a driver you reboot into:

    Listen   parec on the browser's capture sink, or the desktop sink
    Speak    pacat into magician_meet_mic (the browser mic is its .monitor)
    Display  Xvfb when DISPLAY is unset
    Restore  the previous default sink is put back, and moved streams return

  Join works on this host. A container does not see host Pulse devices, and
  the image does not install them yet.

Instructions: docs/components/magician/meetings.md
────────────────────────────────────────────────────────────────────────────
EOF
}

main() {
  case "$(uname -s)" in
    Darwin) setup_macos ;;
    Linux)  setup_linux ;;
    *) warn "Meeting audio setup does not know $(uname -s)." ;;
  esac
  log "Done."
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
