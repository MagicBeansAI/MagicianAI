#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "Usage: $0 <source-bin> <dest-bin>" >&2
    exit 1
fi

src="$1"
dest="$2"
dest_dir="$(dirname "$dest")"
dest_base="$(basename "$dest")"
tmp="$(mktemp "${dest_dir}/.${dest_base}.tmp.XXXXXX")"

cleanup() {
    rm -f "$tmp"
}

trap cleanup EXIT

cp "$src" "$tmp"
chmod 755 "$tmp"
mv -f "$tmp" "$dest"

identifier="com.magicbeans.${dest_base%.bin}"
if command -v codesign >/dev/null 2>&1; then
    # Use the same real identity selected by materialize-desktop-debug-app.sh.
    # The Android Apps owner bootstrap requires the desktop and Magician
    # runtime to carry the exact same Apple Team ID; signing this staged
    # runtime ad-hoc made that protected socket reject it every two seconds.
    sign_identity="${MAGICIAN_DESKTOP_SIGN_IDENTITY:-}"
    if [ -z "$sign_identity" ] && command -v security >/dev/null 2>&1; then
        # Pick the identity by its SHA-1, and skip any the keychain marks
        # revoked or expired (`find-identity -v` appends CSSMERR_TP_CERT_*).
        # A re-minted certificate carries the same name as the revoked one it
        # replaces, so a name pick can land on the dead one — and Gatekeeper
        # kills and trashes a binary signed with a revoked certificate.
        # Prefer Developer ID Application: an Apple Development certificate is
        # Xcode-managed and per machine, and re-minting one elsewhere revokes
        # it — three were revoked here in a week, and the keychain kept listing
        # the latest as valid while Gatekeeper already refused it. Developer ID
        # does not churn that way. Apple Development stays the fallback for a
        # machine without a paid team. The desktop app picks by the same rule
        # (materialize-desktop-debug-app.sh) so both carry one Team ID.
        identities=$(security find-identity -v -p codesigning 2>/dev/null | grep -v CSSMERR_TP_CERT || true)
        sign_identity=$(printf '%s\n' "$identities" \
            | awk '!found && /Developer ID Application/ {print $2; found=1}')
        if [ -z "$sign_identity" ]; then
            sign_identity=$(printf '%s\n' "$identities" \
                | awk '!found && /Apple Development/ {print $2; found=1}')
        fi
    fi
    if [ -n "$sign_identity" ]; then
        codesign --force --timestamp=none --options runtime \
            --identifier "$identifier" --sign "$sign_identity" "$dest"
    else
        # A machine without a usable certificate still gets a runnable local
        # binary. Native authority sockets preflight this state and remain
        # unavailable instead of accepting an ad-hoc process identity.
        codesign --force --timestamp=none --options runtime \
            --identifier "$identifier" --sign - "$dest" 2>/dev/null
    fi

    # The keychain's own validity flag lags Apple's revocation: a certificate
    # minted yesterday can read valid in `find-identity -v` while Gatekeeper's
    # check already says CSSMERR_TP_CERT_REVOKED — and a binary signed with it
    # is killed about a minute after launch and deleted from disk, twice in one
    # evening here. Ask Gatekeeper about the staged binary itself; on a revoked
    # verdict fall back to an ad-hoc signature, which runs (the native authority
    # sockets then preflight unavailable, as they do on a machine with no
    # certificate) and say so loudly, because the fix is a new certificate.
    # Captured, not piped: spctl exits non-zero for every rejected binary and
    # this script runs under `pipefail`, so `spctl | grep -q` was false even
    # when grep matched — the fallback never fired and the third re-stage
    # died like the first two.
    gatekeeper_verdict=""
    if [ -n "$sign_identity" ]; then
        gatekeeper_verdict=$(spctl --assess --type execute "$dest" 2>&1 || true)
    fi
    if [ -n "$sign_identity" ] && [[ "$gatekeeper_verdict" == *CSSMERR_TP_CERT_REVOKED* ]]; then
        echo "warning: Gatekeeper reports the signing certificate REVOKED; staging $dest ad-hoc signed." >&2
        echo "         The desktop-owner sockets (Android Apps, macOS app pairing) will be unavailable" >&2
        echo "         until a new Apple Development certificate is minted and both binaries are re-staged." >&2
        sign_identity=""
        codesign --force --timestamp=none --options runtime \
            --identifier "$identifier" --sign - "$dest"
    fi

    # Signing is deliberately a post-copy operation on the canonical path.
    # Any later byte mutation would invalidate the signature, so make a valid
    # signature a hard staging postcondition instead of leaving launch-time
    # code verification to diagnose a partially staged artifact.
    codesign --verify --strict --verbose=2 "$dest"

    staged_identifier=$(codesign --display --verbose=4 "$dest" 2>&1 \
        | awk -F= '/^Identifier=/{print $2}')
    if [ "$staged_identifier" != "$identifier" ]; then
        echo "error: staged code identifier '$staged_identifier' does not match '$identifier'" >&2
        exit 1
    fi

    staged_team=$(codesign --display --verbose=4 "$dest" 2>&1 \
        | awk -F= '/^TeamIdentifier=/{print $2}')
    if [ -n "$sign_identity" ]; then
        if [ -z "$staged_team" ] || [ "$staged_team" = "not set" ]; then
            echo "error: real-signed staged binary has no Apple Team ID: $dest" >&2
            exit 1
        fi
        echo "   signed staged binary after copy: $dest (team $staged_team)"
    else
        echo "warning: staged binary is ad-hoc signed; native owner authority remains unavailable: $dest" >&2
    fi
fi

trap - EXIT
