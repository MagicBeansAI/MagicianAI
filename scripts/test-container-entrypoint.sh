#!/usr/bin/env bash
# Exercise the real entrypoint with disposable image/runtime roots. The final
# skill materializer and supervisor are stand-ins; no container or Rust build.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

fixture() {
  app="$TMP_ROOT/$1/app"
  runtime="$TMP_ROOT/$1/runtime"
  mkdir -p "$app/scripts" "$app/magician_data_v3" "$app/skillshub" "$runtime"
  cp "$REPO/scripts/container-entrypoint.sh" "$app/scripts/"
  printf 'packaged-config\n' > "$app/magician-config.yaml"
  printf 'packaged-router\n' > "$app/llm-router.yaml"
  cat > "$app/scripts/materialize-container-browser-skill.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == "$PWD/skillshub" ]]
[[ "$2" == "$MAGICIAN_ROOT_DIR/scopes/anonymous/default/skills" ]]
[[ -s "$MAGICIAN_ROOT_DIR/llm-router.yaml" ]]
exit "${TEST_MATERIALIZE_EXIT:-0}"
SH
  cat > "$app/magic-supervisor" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ -s "$MAGICIAN_ROOT_DIR/magician-config.yaml" ]]
[[ -s "$MAGICIAN_ROOT_DIR/llm-router.yaml" ]]
mkdir -p "$MAGICIAN_ROOT_DIR/system"
python3 -c 'import os,stat; assert stat.S_IMODE(os.stat(os.environ["MAGICIAN_ROOT_DIR"]+"/system").st_mode)==0o700'
printf 'started\n' > "$MAGICIAN_ROOT_DIR/supervisor-started"
exit "${TEST_SUPERVISOR_EXIT:-0}"
SH
  chmod +x "$app/magic-supervisor"
}

boot() {
  MAGICIAN_ROOT_DIR="$runtime" MAGICIAN_SEED_ROOT="$app/magician_data_v3" \
    bash "$app/scripts/container-entrypoint.sh" > "$TMP_ROOT/boot.log" 2>&1
}

fixture fresh
boot
cmp "$app/magician-config.yaml" "$runtime/magician-config.yaml"
cmp "$app/llm-router.yaml" "$runtime/llm-router.yaml"
[[ -f "$runtime/supervisor-started" ]]

fixture upgrade
printf 'operator-config\n' > "$runtime/magician-config.yaml"
boot
[[ "$(cat "$runtime/magician-config.yaml")" == operator-config ]]
cmp "$app/llm-router.yaml" "$runtime/llm-router.yaml"
# Restart/image upgrade must preserve both operator-owned files byte for byte.
printf 'operator-router\n' > "$runtime/llm-router.yaml"
boot
[[ "$(cat "$runtime/magician-config.yaml")" == operator-config ]]
[[ "$(cat "$runtime/llm-router.yaml")" == operator-router ]]

fixture missing_config
mv "$app/magician-config.yaml" "$TMP_ROOT/not-a-config-seed.yaml"
if boot; then echo 'ERROR: missing config seed allowed startup' >&2; exit 1; fi
[[ ! -e "$runtime/supervisor-started" ]]
grep -q 'required runtime config is missing or empty' "$TMP_ROOT/boot.log"

fixture missing_router
mv "$app/llm-router.yaml" "$TMP_ROOT/not-a-seed.yaml"
if boot; then echo 'ERROR: missing router allowed startup' >&2; exit 1; fi
[[ ! -e "$runtime/supervisor-started" ]]
grep -q 'required runtime config is missing or empty' "$TMP_ROOT/boot.log"

fixture empty_router
: > "$runtime/llm-router.yaml"
if boot; then echo 'ERROR: empty operator router allowed startup' >&2; exit 1; fi
[[ ! -s "$runtime/llm-router.yaml" ]]
[[ ! -e "$runtime/supervisor-started" ]]

fixture materialization_failure
if TEST_MATERIALIZE_EXIT=23 boot; then
  echo 'ERROR: failed materialization allowed startup' >&2; exit 1
else
  [[ "$?" == 23 ]]
fi
[[ ! -e "$runtime/supervisor-started" ]]

fixture supervisor_failure
if TEST_SUPERVISOR_EXIT=17 boot; then
  echo 'ERROR: supervisor failure was hidden' >&2; exit 1
else
  [[ "$?" == 17 ]]
fi
fixture partial_keyring
if MAGICIAN_KEYRING_STATE_DIR="$TMP_ROOT/keyring" boot; then
  echo 'ERROR: incomplete keyring configuration allowed startup' >&2; exit 1
fi
[[ ! -e "$runtime/supervisor-started" ]]

fixture keyring_launcher
cat > "$app/scripts/run-linux-keyring.py" <<'PY'
import os, sys
if sys.argv[1] == '--check-inputs':
    assert not os.path.exists(os.path.join(os.environ['MAGICIAN_ROOT_DIR'], 'magician-config.yaml'))
    assert sys.argv[2:] == ['--state-dir', os.environ['MAGICIAN_KEYRING_STATE_DIR'],
                           '--password-file', os.environ['MAGICIAN_KEYRING_PASSWORD_FILE']]
    sys.exit(0)
assert sys.argv[1:6] == ['--state-dir', os.environ['MAGICIAN_KEYRING_STATE_DIR'],
                          '--password-file', os.environ['MAGICIAN_KEYRING_PASSWORD_FILE'], '--']
os.execv(sys.argv[6], sys.argv[6:])
PY
MAGICIAN_KEYRING_STATE_DIR="$TMP_ROOT/keyring" \
  MAGICIAN_KEYRING_PASSWORD_FILE="$TMP_ROOT/unlock" boot
[[ -f "$runtime/supervisor-started" ]]

fixture inaccessible_keyring
cat > "$app/scripts/run-linux-keyring.py" <<'PY'
import sys
assert sys.argv[1] == '--check-inputs'
sys.exit(27)
PY
if MAGICIAN_KEYRING_STATE_DIR="$TMP_ROOT/keyring" \
    MAGICIAN_KEYRING_PASSWORD_FILE="$TMP_ROOT/unlock" boot; then
  echo 'ERROR: inaccessible private mounts allowed runtime seeding' >&2; exit 1
else
  [[ "$?" == 27 ]]
fi
[[ ! -e "$runtime/magician-config.yaml" && ! -e "$runtime/supervisor-started" ]]

echo 'container entrypoint tests passed (10 cases)'
