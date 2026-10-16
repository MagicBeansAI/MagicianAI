#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}/magician-config.yaml"

usage() {
  cat <<'EOF'
Usage: scripts/app-platform-processing-profile.sh [status|local|remote]

status   Print the effective processing profile settings from live config.
local    Select local processing (`privacy.processing.mode=local`).
remote   Select cloud processing (`privacy.processing.mode=cloud`).
EOF
}

run_command_status() {
  local mode
  mode="$(python3 - "$CONFIG_PATH" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text()
match = re.search(
    r"(?m)^privacy:\n  processing:\n    mode:\s*(local|cloud)(?:\s+#.*)?$",
    text,
)
if match is None:
    raise SystemExit("privacy.processing.mode is missing or invalid")
print(match.group(1))
PY
)"
  printf 'processing-locality settings in %s\n' "$CONFIG_PATH"
  printf '  privacy.processing.mode: %s\n' "$mode"
  if [[ "$mode" == "cloud" ]]; then
    printf '  derived app_platform.processing.remote_processing_enabled: true\n'
  else
    printf '  derived app_platform.processing.remote_processing_enabled: false\n'
  fi
  printf '  local_profile: %s\n' "$(rg -m1 '^[[:space:]]+local_profile:' "$CONFIG_PATH" | awk -F': ' '{print $2}' || true)"
  printf '  remote_profile: %s\n' "$(rg -m1 '^[[:space:]]+remote_profile:' "$CONFIG_PATH" | awk -F': ' '{print $2}' || true)"
  # Profiles live in the sibling `llm-router.yaml`, not the config, so look
  # there when it exists and fall back to the config for a pre-split or
  # self-contained one.
  TABLES_PATH="$(dirname "$CONFIG_PATH")/llm-router.yaml"
  PROFILE_SOURCE="$CONFIG_PATH"
  if [[ -f "$TABLES_PATH" ]]; then
    PROFILE_SOURCE="$TABLES_PATH"
  fi
  printf '  profiles op-app-workflow-local: '
  if rg -q '^[[:space:]]{6}op-app-workflow-local:' "$PROFILE_SOURCE"; then
    echo present
  else
    echo missing
  fi
  printf '  profiles op-app-workflow-remote: '
  if rg -q '^[[:space:]]{6}op-app-workflow-remote:' "$PROFILE_SOURCE"; then
    echo present
  else
    echo missing
  fi
}

set_processing_mode() {
python3 - "$1" "$2" <<'PY'
import os
import re
import sys
import tempfile
from pathlib import Path

config_path = Path(sys.argv[1])
mode = sys.argv[2]
local_profile = "op-app-workflow-local"
remote_profile = "op-app-workflow-remote"

text = config_path.read_text()

if f"{'    '}local_profile: {local_profile}" not in text:
    raise SystemExit("local profile key missing in app_platform.processing")

if f"{'    '}remote_profile: {remote_profile}" not in text:
    raise SystemExit("remote profile key missing in app_platform.processing")

if f"{'      '}{remote_profile}:" not in text:
    raise SystemExit("remote profile block missing under app_platform.processing.profiles")

pattern = re.compile(
    r"(?m)^(privacy:\n  processing:\n    mode:\s*)(local|cloud)(\s*(?:#.*)?)$"
)
text, replacements = pattern.subn(rf"\g<1>{mode}\g<3>", text, count=1)
if replacements != 1:
    raise SystemExit("privacy.processing.mode is missing or invalid")

# Avoid a second control silently returning through an old helper or manual
# edit. The runtime derives this field from privacy.processing.mode.
if re.search(r"(?m)^\s*remote_processing_enabled:\s*", text):
    raise SystemExit(
        "retired remote_processing_enabled setting is present; remove it and retry"
    )

fd, temporary_name = tempfile.mkstemp(
    prefix=f".{config_path.name}.", suffix=".tmp", dir=config_path.parent
)
try:
    with os.fdopen(fd, "w") as temporary:
        temporary.write(text)
        temporary.flush()
        os.fsync(temporary.fileno())
    os.chmod(temporary_name, config_path.stat().st_mode & 0o777)
    os.replace(temporary_name, config_path)
finally:
    if os.path.exists(temporary_name):
        os.unlink(temporary_name)
PY
}

main() {
  if [[ ! -f "$CONFIG_PATH" ]]; then
    echo "FAIL: missing live config at $CONFIG_PATH"
    exit 1
  fi

  local mode="${1:-status}"

  case "$mode" in
    status)
      run_command_status
      ;;
    local)
      set_processing_mode "$CONFIG_PATH" local
      echo "Updated live config for local processing. Reload config or restart the server."
      run_command_status
      ;;
    remote)
      set_processing_mode "$CONFIG_PATH" cloud
      echo "Updated live config for cloud processing. Reload config or restart the server."
      run_command_status
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

main "${1:-status}"
