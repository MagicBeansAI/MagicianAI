#!/usr/bin/env bash
# install.sh — installs this package. Ships inside it, so the thing that
# unpacks a release is the thing the release was built with.
#
# It does three jobs and refuses to guess at any of them: check that the bytes
# are the bytes that were built, put them somewhere, and say what is next. It
# never installs Ollama, notes, tunnels or permissions — those are the wizard's,
# and doing them here would be a second installer disagreeing with the first.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${MAGICIAN_PREFIX:-$HOME/.magician}"
ASSUME_YES=0
SKIP_VERIFY=0

ok()   { printf '  OK %s\n' "$*"; }
err()  { printf '  ERROR %s\n' "$*" >&2; }
note() { printf '       %s\n' "$*"; }

usage() {
  cat <<USAGE
Install this Magician package.

  --prefix DIR   where to install (default: \$HOME/.magician)
  --yes          do not ask before replacing an existing install
  --no-verify    skip the checksum check (you are on your own)
  --help         this text
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --prefix=*) PREFIX="${1#*=}"; shift ;;
    --yes|-y) ASSUME_YES=1; shift ;;
    --no-verify) SKIP_VERIFY=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) err "unknown argument: $1"; usage >&2; exit 2 ;;
  esac
done

# --- 1. are these the bytes that were built? ---------------------------------
# Before anything is copied. A partial install of tampered files is worse than
# no install, and the check costs a second.
if [[ "$SKIP_VERIFY" -eq 1 ]]; then
  note "checksum check skipped by request"
elif [[ ! -f "$HERE/SHA256SUMS" ]]; then
  err "SHA256SUMS is missing — this package is incomplete."
  note "Download it again rather than installing what is here."
  exit 1
else
  hasher=""
  for candidate in shasum sha256sum; do
    command -v "$candidate" >/dev/null 2>&1 && { hasher="$candidate"; break; }
  done
  if [[ -z "$hasher" ]]; then
    err "no shasum or sha256sum on this machine, so the package cannot be verified."
    note "Install one, or re-run with --no-verify if you understand the risk."
    exit 1
  fi
  # Both spellings take -c; only shasum needs the algorithm named.
  args=(-c); [[ "$hasher" == shasum ]] && args=(-a 256 -c)
  if ( cd "$HERE" && "$hasher" "${args[@]}" SHA256SUMS >/dev/null 2>&1 ); then
    ok "checksums verify ($(grep -c . "$HERE/SHA256SUMS") files)"
  else
    err "this package does not match its own checksums."
    note "Something changed between building and here. Do not install it;"
    note "download it again. To see which files:"
    note "  cd $HERE && $hasher ${args[*]} SHA256SUMS"
    exit 1
  fi
fi

# Refuse a healthy package for a different machine before replacing anything.
case "$(uname -s):$(uname -m)" in
  Darwin:arm64|Darwin:aarch64) expected_target="aarch64-apple-darwin" ;;
  Darwin:x86_64)               err "Magician needs a Mac with Apple Silicon on macOS 14 or newer; Intel Macs are not supported"; exit 1 ;;
  Linux:arm64|Linux:aarch64)   expected_target="aarch64-unknown-linux-gnu" ;;
  Linux:x86_64|Linux:amd64)    expected_target="x86_64-unknown-linux-gnu" ;;
  *) err "native Magician packages are not supported on $(uname -s)/$(uname -m)"; exit 1 ;;
esac
package_target="$(awk '/^target:/{print $2; exit}' "$HERE/MANIFEST.yaml" 2>/dev/null || true)"
if [[ "$package_target" != "$expected_target" ]]; then
  err "package target '$package_target' does not match this computer ($expected_target)."
  exit 1
fi
ok "package target matches this computer ($expected_target)"

# --- 2. where it goes --------------------------------------------------------
if [[ -e "$PREFIX" ]] && [[ -n "$(ls -A "$PREFIX" 2>/dev/null || true)" ]]; then
  existing="an existing install"
  [[ -f "$PREFIX/MANIFEST.yaml" ]] &&
    existing="version $(awk '/^version:/{print $2; exit}' "$PREFIX/MANIFEST.yaml" 2>/dev/null || echo unknown)"
  if [[ "$ASSUME_YES" -ne 1 ]]; then
    printf '  %s is already at %s. Replace it? [y/N] ' "$existing" "$PREFIX"
    read -r answer </dev/tty || answer=""
    case "$(printf '%s' "$answer" | tr -d '\r' | tr '[:upper:]' '[:lower:]')" in
      y|yes) ;;
      *) echo "  Nothing was changed."; exit 0 ;;
    esac
  fi
fi

mkdir -p "$PREFIX"
# The binaries move last. Until they land the old install still runs, and a
# failure halfway leaves something that works rather than a half-swapped stack.
for dir in scripts share; do
  [[ -d "$HERE/$dir" ]] || continue
  rm -rf "${PREFIX:?}/$dir"
  cp -R "$HERE/$dir" "$PREFIX/$dir"
done
if [[ -f "$HERE/tool-runtime-config.yaml" ]]; then
  cp "$HERE/tool-runtime-config.yaml" "$PREFIX/tool-runtime-config.yaml"
fi
# The uninstaller lives with the install, not with the package: whoever wants
# it later has the install and has usually long since deleted the tarball.
cp "$HERE/uninstall.sh" "$PREFIX/uninstall.sh" 2>/dev/null || true
chmod 755 "$PREFIX/uninstall.sh" 2>/dev/null || true
cp "$HERE/MANIFEST.yaml" "$PREFIX/MANIFEST.yaml" 2>/dev/null || true
cp "$HERE/SHA256SUMS" "$PREFIX/SHA256SUMS" 2>/dev/null || true
for binary in "$HERE"/*.bin; do
  [[ -e "$binary" ]] || continue
  install -m 755 "$binary" "$PREFIX/$(basename "$binary")"
done
ok "installed to $PREFIX"

# --- 3. macOS will have opinions --------------------------------------------
# An unsigned binary downloaded through a browser carries a quarantine flag and
# is killed on exec with a dialog that does not say why. Saying so here beats
# the user discovering it as a crash.
if [[ "$(uname -s)" = Darwin ]]; then
  # What the package says about itself, which is a different question from what
  # this machine will do with it. An ad-hoc or unsigned package is quarantined
  # on download however healthy codesign looks locally.
  case "$(awk '/^signing:/{print $2; exit}' "$HERE/MANIFEST.yaml" 2>/dev/null)" in
    developer-id) ;;
    adhoc) note "this package is ad-hoc signed, which is identity for one machine, not distribution" ;;
    *)     note "this package is unsigned" ;;
  esac
  quarantined=0
  for binary in "$PREFIX"/*.bin; do
    [[ -e "$binary" ]] || continue
    if xattr -p com.apple.quarantine "$binary" >/dev/null 2>&1; then quarantined=1; fi
  done
  if [[ "$quarantined" -eq 1 ]]; then
    note "macOS has quarantined these binaries because the package is unsigned."
    note "Clear it with:  xattr -dr com.apple.quarantine $PREFIX"
  fi
fi

echo
echo "Installed. What is here now is the backend; nothing else has been set up."
echo "Next: run the setup wizard to choose what you want it to do, and to"
echo "install the pieces those capabilities need (models, notes, permissions)."
echo
echo "To remove all of this later: $PREFIX/uninstall.sh"
echo "Your data at ${MAGICIAN_ROOT_DIR:-\$HOME/MagicianNotes} is never touched unless you ask."
