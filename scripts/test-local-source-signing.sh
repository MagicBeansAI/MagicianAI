#!/usr/bin/env bash
# Real Mach-O qualification: local signing and reinstall while a binary runs.
set -euo pipefail
if [[ "$(uname -s)" != Darwin ]]; then
  echo "local source signing: macOS qualification skipped on this host"
  exit 0
fi
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/magician-local-signing.XXXXXX")"
fixture_pid=""
cleanup() {
  if [[ -n "$fixture_pid" ]]; then
    kill "$fixture_pid" 2>/dev/null || true
    wait "$fixture_pid" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT
mkdir -p "$TMP_ROOT/release"
cat > "$TMP_ROOT/fixture.c" <<'C'
#include <unistd.h>
int main(void) { sleep(60); return 0; }
C
cc "$TMP_ROOT/fixture.c" -o "$TMP_ROOT/release/magician"
for binary in magicutor magic-supervisor decision-engine; do
  cp "$TMP_ROOT/release/magician" "$TMP_ROOT/release/$binary"
done
MAGICIAN_SIGN_LOCAL=1 MAGICIAN_PACKAGE_SKIP_SKILLS=1 RELEASE_VERSION=fixture \
MAGICIAN_PACKAGE_RELEASE_DIR="$TMP_ROOT/release" \
MAGICIAN_PACKAGE_DIR="$TMP_ROOT/packages" \
MAGICIAN_PACKAGE_PATH_FILE="$TMP_ROOT/package-path" \
  bash "$SCRIPT_DIR/package-release.sh" > "$TMP_ROOT/package.log" 2>&1 \
  || { cat "$TMP_ROOT/package.log"; exit 1; }
archive="$(cat "$TMP_ROOT/package-path")"
package="${archive%.tar.gz}"
grep -q '^signing: local$' "$package/MANIFEST.yaml"
identity="$(codesign --display --verbose=4 "$package/magician.bin" 2>&1)"
grep -q '^Identifier=com.magicbeans.magician$' <<< "$identity"
codesign --verify --strict "$package/magician.bin"
# A failed signature must not replace a previously runnable canonical binary.
mkdir -p "$TMP_ROOT/failing-signer" "$TMP_ROOT/staged"
cp "$package/magician.bin" "$TMP_ROOT/staged/magician.bin"
cat > "$TMP_ROOT/failing-signer/codesign" <<'SH'
#!/usr/bin/env bash
exit 7
SH
chmod 755 "$TMP_ROOT/failing-signer/codesign"
if PATH="$TMP_ROOT/failing-signer:$PATH" MAGICIAN_DESKTOP_SIGN_IDENTITY=- \
  bash "$SCRIPT_DIR/replace-debug-bin.sh" "$TMP_ROOT/release/magician" \
    "$TMP_ROOT/staged/magician.bin" > "$TMP_ROOT/signing-failure.log" 2>&1; then
  echo "failed signing unexpectedly succeeded" >&2
  exit 1
fi
cmp "$package/magician.bin" "$TMP_ROOT/staged/magician.bin"
codesign --verify --strict "$TMP_ROOT/staged/magician.bin"
[[ -z "$(find "$TMP_ROOT/staged" -name '.magician.bin.tmp.*' -print -quit)" ]]
desktop="${MAGICIAN_SOURCE_DIR:-$SCRIPT_DIR/..}/.local/Magican-Debug.app"
if [[ -d "$desktop" ]]; then
  runtime_team="$(sed -n 's/^TeamIdentifier=//p' <<< "$identity")"
  desktop_team="$(codesign --display --verbose=4 "$desktop" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
  [[ "$runtime_team" == "$desktop_team" ]] \
    || { echo "local runtime and Desktop signing teams differ" >&2; exit 1; }
fi
bash "$package/install.sh" --yes --prefix "$TMP_ROOT/installed" > /dev/null
"$TMP_ROOT/installed/magician.bin" &
fixture_pid=$!
sleep 1
kill -0 "$fixture_pid"
before="$(stat -f %i "$TMP_ROOT/installed/magician.bin")"
bash "$package/install.sh" --yes --prefix "$TMP_ROOT/installed" > /dev/null
after="$(stat -f %i "$TMP_ROOT/installed/magician.bin")"
[[ "$before" != "$after" ]]
kill -0 "$fixture_pid"
cmp "$package/magician.bin" "$TMP_ROOT/installed/magician.bin"
codesign --verify --strict "$TMP_ROOT/installed/magician.bin"
echo "local source signing and running-binary reinstall: passed"
