#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
mkdir -p "$TMP_ROOT/bin" "$TMP_ROOT/state"

cat > "$TMP_ROOT/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
state="${FAKE_DOCKER_STATE:?}"
case "${1:-} ${2:-}" in
  "info ") exit 0 ;;
  "image inspect") exit 0 ;;
  "run -d")
    data_dir=""
    args=("$@")
    for ((i=0; i<${#args[@]}; i++)); do
      if [[ "${args[$i]}" == "-v" ]]; then
        data_dir="${args[$((i+1))]%%:/data}"
      fi
    done
    [[ -n "$data_dir" ]]
    printf '%s\n' "$data_dir" > "$state/data-dir"
    mkdir -p "$data_dir/scopes"
    [[ -e "$data_dir/magician-config.yaml" ]] || printf 'runtime: {}\n' > "$data_dir/magician-config.yaml"
    if [[ "${FAKE_DOCKER_SKIP_ROUTER:-0}" != 1 && ! -e "$data_dir/llm-router.yaml" ]]; then
      printf 'profiles: {}\n' > "$data_dir/llm-router.yaml"
    fi
    echo fake-container-id
    ;;
  "stop magician-test"|"rm magician-test"|"kill magician-test") exit 0 ;;
  "exec magician-test")
    data_dir="$(cat "$state/data-dir")"
    shift 2
    if [[ "$1" == "whoami" ]]; then
      echo magician
    elif [[ "$1 $2" == "env -u" ]]; then
      exit 0
    elif [[ "$1 $2" == "cat /data/host-test.txt" ]]; then
      cat "$data_dir/host-test.txt"
    elif [[ "$1 $2" == "sh -c" ]]; then
      printf 'container-write-test\n' > "$data_dir/container-test.txt"
    else
      echo "unexpected exec: $*" >&2
      exit 1
    fi
    ;;
  "inspect --format") echo '1000000000|2147483648' ;;
  "inspect magician-test") printf '%s\n' '[{"State":{"Status":"running"},"HostConfig":{"NanoCpus":1000000000,"Memory":2147483648}}]' ;;
  "logs magician-test") echo 'qualified log line' ;;
  *) echo "unexpected docker command: $*" >&2; exit 1 ;;
esac
EOF

cat > "$TMP_ROOT/bin/curl" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "$TMP_ROOT/bin/nc" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$TMP_ROOT/bin/docker" "$TMP_ROOT/bin/curl" "$TMP_ROOT/bin/nc"

FAKE_DOCKER_STATE="$TMP_ROOT/state" PATH="$TMP_ROOT/bin:$PATH" \
  bash "$REPO/scripts/test-container.sh" \
    --skip-build \
    --runtime docker \
    --image example.test/magician:1 \
    --host-port-magician 14002 \
    --host-port-magicutor 14003 \
    --report "$TMP_ROOT/report.json"

jq -e '.runtime == "docker" and .ports.magician == 14002 and .ports.magicutor == 14003' "$TMP_ROOT/report.json" >/dev/null
jq -e '.totals.failed == 0 and .totals.passed >= 15' "$TMP_ROOT/report.json" >/dev/null
jq -e '.tests[] | select(.name == "Existing runtime config is not overwritten" and .status == "pass")' "$TMP_ROOT/report.json" >/dev/null
jq -e '.tests[] | select(.name == "Existing LLM router tables are not overwritten" and .status == "pass")' "$TMP_ROOT/report.json" >/dev/null
jq -e '.tests[] | select(.name == "Docker restart policy recovers after process kill" and .status == "pass")' "$TMP_ROOT/report.json" >/dev/null

# A health stub alone must not qualify an image missing a boot-critical seed.
if FAKE_DOCKER_SKIP_ROUTER=1 FAKE_DOCKER_STATE="$TMP_ROOT/state" PATH="$TMP_ROOT/bin:$PATH" \
  bash "$REPO/scripts/test-container.sh" --skip-build --runtime docker \
    --image example.test/magician:broken --host-port-magician 14002 \
    --host-port-magicutor 14003 --report "$TMP_ROOT/missing-router.json" >/dev/null; then
  echo 'ERROR: image without router tables was qualified' >&2
  exit 1
fi
jq -e '.tests[] | select(.name == "LLM router tables were seeded" and .status == "fail")' "$TMP_ROOT/missing-router.json" >/dev/null

echo "container qualification harness tests passed"
