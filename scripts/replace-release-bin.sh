#!/usr/bin/env bash
set -euo pipefail

# Root-local release binaries participate in the same native code-identity
# checks as debug binaries. A production release additionally needs the same
# Developer ID + hardened-runtime + secure-timestamp signature as every other
# executable inside the notarized app. Signing the enclosing .app does not
# repair an incompatible nested signature.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ "${MAGICIAN_SIGN:-0}" != "1" ]]; then
    exec "$script_dir/replace-debug-bin.sh" "$@"
fi

if [[ "$#" -ne 2 ]]; then
    echo "Usage: $0 <source-bin> <dest-bin>" >&2
    exit 1
fi

src="$1"
dest="$2"
dest_dir="$(dirname "$dest")"
dest_base="$(basename "$dest")"
identity="${MAGICIAN_SIGN_IDENTITY:-${APPLE_SIGNING_IDENTITY:-}}"

if [[ -z "$identity" ]]; then
    echo "error: MAGICIAN_SIGN=1 requires MAGICIAN_SIGN_IDENTITY or APPLE_SIGNING_IDENTITY" >&2
    exit 1
fi
if ! command -v codesign >/dev/null 2>&1 || ! command -v security >/dev/null 2>&1; then
    echo "error: production release signing requires macOS codesign and security" >&2
    exit 1
fi

identity_record="$(security find-identity -v -p codesigning 2>/dev/null \
    | grep -F "$identity" | head -1 || true)"
if [[ "$identity_record" != *"Developer ID Application"* ]]; then
    echo "error: production release identity is not a valid Developer ID Application certificate: $identity" >&2
    exit 1
fi

tmp="$(mktemp "${dest_dir}/.${dest_base}.tmp.XXXXXX")"
cleanup() {
    rm -f "$tmp"
}
trap cleanup EXIT

cp "$src" "$tmp"
chmod 755 "$tmp"
mv -f "$tmp" "$dest"

identifier="com.magicbeans.${dest_base%.bin}"
codesign --force --timestamp --options runtime \
    --identifier "$identifier" --sign "$identity" "$dest"
codesign --verify --strict --verbose=2 "$dest"

details="$(codesign --display --verbose=4 "$dest" 2>&1)"
staged_identifier="$(awk -F= '/^Identifier=/{print $2}' <<<"$details")"
if [[ "$staged_identifier" != "$identifier" ]]; then
    echo "error: staged code identifier '$staged_identifier' does not match '$identifier'" >&2
    exit 1
fi
if [[ "$details" != *"Authority=$identity"* ]]; then
    echo "error: staged release binary does not carry the requested Developer ID authority: $dest" >&2
    exit 1
fi
if [[ "$details" != *"Runtime Version="* ]]; then
    echo "error: staged release binary is missing hardened-runtime metadata: $dest" >&2
    exit 1
fi
if [[ "$details" != *"Timestamp="* ]]; then
    echo "error: staged release binary is missing a secure timestamp: $dest" >&2
    exit 1
fi

staged_team="$(awk -F= '/^TeamIdentifier=/{print $2}' <<<"$details")"
echo "   distribution-signed staged binary after copy: $dest (team $staged_team)"

trap - EXIT
