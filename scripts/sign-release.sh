#!/usr/bin/env bash
# sign-release.sh — sign a staged package's binaries for distribution.
#
# Two different things get called "signing" on macOS and only one of them lets
# a stranger run your binary:
#
#   ad-hoc (`--sign -`)      identity for TCC on THIS machine. Downloaded, it
#                            is still quarantined and still killed on exec.
#   Developer ID Application the distribution certificate. With a hardened
#                            runtime and a secure timestamp, this is what
#                            notarisation will accept.
#
# `replace-debug-bin.sh` already does the first for local builds. This does the
# second, and refuses to pretend when it cannot: an unsigned package that says
# so is honest, an unsigned package that claims otherwise is a support ticket
# from someone whose Mac killed the binary with no explanation.
set -euo pipefail

STAGE="${1:?usage: sign-release.sh <staged-package-dir>}"
ADHOC="${MAGICIAN_SIGN_ADHOC:-0}"

ok()   { printf '  OK %s\n' "$*"; }
err()  { printf '  ERROR %s\n' "$*" >&2; }
note() { printf '       %s\n' "$*"; }

[[ -d "$STAGE" ]] || { err "not a directory: $STAGE"; exit 1; }
command -v codesign >/dev/null 2>&1 || { err "codesign is not available (macOS only)"; exit 1; }

# --- which identity, and is it one that actually travels? --------------------
IDENTITY="${MAGICIAN_SIGN_IDENTITY:-}"
if [[ -z "$IDENTITY" && "$ADHOC" != "1" ]]; then
  # `|| true`: no Developer ID is the expected case here, and without it the
  # failing grep takes the whole script down under `set -e` — exiting 1 with no
  # output, which is exactly the message this path exists to print.
  IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
    | grep 'Developer ID Application' | head -1 \
    | sed -n 's/.*"\(.*\)".*/\1/p' || true)"
fi

if [[ -z "$IDENTITY" && "$ADHOC" != "1" ]]; then
  err "no Developer ID Application certificate in the keychain."
  note "That is the certificate distribution requires — an Apple Development"
  note "certificate cannot sign for it, and notarisation will not accept one."
  note "Get one from developer.apple.com (Certificates → Developer ID"
  note "Application), or set MAGICIAN_SIGN_ADHOC=1 to sign ad-hoc for local"
  note "testing only, which produces a package strangers still cannot run."
  exit 1
fi

if [[ "$ADHOC" != "1" ]]; then
  identity_record="$(security find-identity -v -p codesigning 2>/dev/null \
    | grep -F "$IDENTITY" | head -1 || true)"
  if [[ "$identity_record" != *"Developer ID Application"* ]]; then
    err "the selected identity is not a Developer ID Application certificate: $IDENTITY"
    exit 1
  fi
fi

# --- sign ---------------------------------------------------------------------
# Hardened runtime and a secure timestamp are not optional extras: notarisation
# rejects a submission without them, and finding that out at submission time
# costs a round trip.
if [[ "$ADHOC" == "1" ]]; then
  SIGN_ARGS=(--force --options runtime --timestamp=none --sign -)
  LABEL="ad-hoc (local testing only)"
else
  SIGN_ARGS=(--force --options runtime --timestamp --sign "$IDENTITY")
  LABEL="$IDENTITY"
fi

signed=0
for binary in "$STAGE"/*.bin; do
  [[ -e "$binary" ]] || continue
  if ! codesign "${SIGN_ARGS[@]}" "$binary" 2>/dev/null; then
    err "could not sign $(basename "$binary")"
    exit 1
  fi
  # Verify each one immediately. A signature that does not verify is worse than
  # none, because everything downstream will believe it.
  if ! codesign --verify --strict "$binary" 2>/dev/null; then
    err "$(basename "$binary") did not verify after signing"
    exit 1
  fi
  signed=$((signed + 1))
done

if [[ "$signed" -eq 0 ]]; then
  err "no .bin files in $STAGE — nothing was signed"
  exit 1
fi
ok "signed and verified $signed binaries with $LABEL"

# --- record what was actually done -------------------------------------------
# The installer and the wizard both need to know, and asking codesign later on
# the target machine answers a different question than what happened here.
if [[ "$ADHOC" == "1" ]]; then
  printf 'signing: adhoc\n' >> "$STAGE/MANIFEST.yaml"
  note "ad-hoc: macOS will still quarantine this package on download."
else
  printf 'signing: developer-id\n' >> "$STAGE/MANIFEST.yaml"
  printf 'signing_identity: %s\n' "$IDENTITY" >> "$STAGE/MANIFEST.yaml"
  note "Signed, but NOT notarised. Gatekeeper needs both; notarisation is a"
  note "separate submission to Apple (xcrun notarytool submit --wait) and then"
  note "stapling. Neither has ever run here — there are no credentials."
fi
