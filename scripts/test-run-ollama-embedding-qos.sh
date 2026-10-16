#!/usr/bin/env bash
# Provider-free embedding Ollama QoS wrap tests. No ollama/ruby required.
#
# Run: bash scripts/test-run-ollama-embedding-qos.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=ollama-embedding-qos.sh
source "$SCRIPT_DIR/ollama-embedding-qos.sh"

pass=0
fail=0

check() {
  local name="$1" want="$2" got="$3"
  if [[ "$want" == "$got" ]]; then
    printf '  PASS  %s\n' "$name"
    pass=$((pass + 1))
  else
    printf '  FAIL  %s: want %q got %q\n' "$name" "$want" "$got"
    fail=$((fail + 1))
  fi
}

contains() {
  local name="$1" needle="$2" haystack="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    printf '  PASS  %s\n' "$name"
    pass=$((pass + 1))
  else
    printf '  FAIL  %s: expected %q in %q\n' "$name" "$needle" "$haystack"
    fail=$((fail + 1))
  fi
}

not_contains() {
  local name="$1" needle="$2" haystack="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    printf '  PASS  %s\n' "$name"
    pass=$((pass + 1))
  else
    printf '  FAIL  %s: did not expect %q in %q\n' "$name" "$needle" "$haystack"
    fail=$((fail + 1))
  fi
}

launch_prefix() {
  MAGICIAN_OLLAMA_EMBEDDING_QOS_DRY_RUN=prefix \
    bash "$SCRIPT_DIR/run-ollama-embedding.sh"
}

STUB_DIR="$(mktemp -d)"
trap 'rm -rf "$STUB_DIR"' EXIT
printf '#!/bin/sh\nexit 0\n' >"$STUB_DIR/taskpolicy"
printf '#!/bin/sh\nexit 0\n' >"$STUB_DIR/taskset"
printf '#!/bin/sh\nexit 0\n' >"$STUB_DIR/nice"
chmod +x "$STUB_DIR/taskpolicy" "$STUB_DIR/taskset" "$STUB_DIR/nice"

unset MAGICIAN_OLLAMA_EMBEDDING_QOS || true
unset MAGICIAN_OLLAMA_EMBEDDING_CPUSET || true
unset MAGICIAN_OLLAMA_EMBEDDING_QOS_DRY_RUN || true

printf 'ollama embedding QoS:\n'

got="$(PATH="$STUB_DIR:$PATH" embedding_serve_prefix Darwin)"
contains "unset QoS Darwin default is taskpolicy -b" "taskpolicy -b" "$got"
got="$(PATH="$STUB_DIR:$PATH" embedding_serve_prefix Linux)"
contains "unset QoS Linux default is nice -n 10" "nice -n 10" "$got"
got="$(PATH="$STUB_DIR:$PATH" launch_prefix)"
contains "unset QoS dry-run still ends with ollama serve" "ollama serve" "$got"

got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=off embedding_serve_prefix)"
check "QOS=off helper prefix empty" "" "$got"
got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=off launch_prefix)"
check "QOS=off dry-run is ollama serve" "ollama serve" "$got"

got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=pass_through embedding_serve_prefix)"
check "QOS=pass_through helper prefix empty" "" "$got"
got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=false embedding_serve_prefix)"
check "QOS=false helper prefix empty" "" "$got"
got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=0 embedding_serve_prefix)"
check "QOS=0 helper prefix empty" "" "$got"

got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=off MAGICIAN_OLLAMA_EMBEDDING_CPUSET=0 embedding_serve_prefix Linux)"
check "CPUSET ignored when QoS off" "" "$got"

got="$(PATH="$STUB_DIR:$PATH" MAGICIAN_OLLAMA_EMBEDDING_QOS=background embedding_serve_prefix Linux)"
contains "Linux background without CPUSET uses nice -n 10" "nice -n 10" "$got"

got="$(
  PATH="$STUB_DIR:$PATH" \
    MAGICIAN_OLLAMA_EMBEDDING_QOS=background \
    MAGICIAN_OLLAMA_EMBEDDING_CPUSET=0 \
    embedding_serve_prefix Linux
)"
contains "Linux background CPUSET=0 uses taskset" "taskset" "$got"
contains "Linux background CPUSET=0 passes -c 0" "-c 0" "$got"
not_contains "Linux cpuset wrap does not use taskpolicy" "taskpolicy" "$got"

got="$(
  PATH="$STUB_DIR:$PATH" \
    MAGICIAN_OLLAMA_EMBEDDING_QOS=background \
    embedding_serve_prefix Darwin
)"
contains "Darwin background uses taskpolicy -b" "taskpolicy -b" "$got"
not_contains "Darwin wrap does not use taskset" "taskset" "$got"

host="$(uname -s 2>/dev/null || true)"
case "$host" in
  Darwin)
    got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=background embedding_serve_prefix)"
    contains "host Darwin QOS=background prefix has taskpolicy -b" "taskpolicy -b" "$got"
    got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=background launch_prefix)"
    contains "host Darwin dry-run has taskpolicy -b" "taskpolicy -b" "$got"
    contains "host Darwin dry-run still ends with ollama serve" "ollama serve" "$got"
    ;;
  Linux)
    got="$(PATH="$STUB_DIR:$PATH" MAGICIAN_OLLAMA_EMBEDDING_QOS=background embedding_serve_prefix)"
    contains "host Linux QOS=background without CPUSET uses nice" "nice -n 10" "$got"
    got="$(
      PATH="$STUB_DIR:$PATH" \
        MAGICIAN_OLLAMA_EMBEDDING_QOS=background \
        MAGICIAN_OLLAMA_EMBEDDING_CPUSET=0 \
        embedding_serve_prefix
    )"
    contains "host Linux CPUSET=0 prefix has taskset" "taskset" "$got"
    got="$(
      PATH="$STUB_DIR:$PATH" \
        MAGICIAN_OLLAMA_EMBEDDING_QOS=background \
        MAGICIAN_OLLAMA_EMBEDDING_CPUSET=0 \
        launch_prefix
    )"
    contains "host Linux dry-run CPUSET=0 has taskset" "taskset" "$got"
    ;;
esac

got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=on PATH="$STUB_DIR:$PATH" embedding_serve_prefix Darwin)"
contains "QOS=on is background wrap" "taskpolicy -b" "$got"
got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=utility PATH="$STUB_DIR:$PATH" embedding_serve_prefix Darwin)"
contains "QOS=utility is background wrap" "taskpolicy -b" "$got"
got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=true PATH="$STUB_DIR:$PATH" embedding_serve_prefix Darwin)"
contains "QOS=true is background wrap" "taskpolicy -b" "$got"
got="$(MAGICIAN_OLLAMA_EMBEDDING_QOS=1 PATH="$STUB_DIR:$PATH" embedding_serve_prefix Darwin)"
contains "QOS=1 is background wrap" "taskpolicy -b" "$got"

check "qos mode unset is background" "background" "$(embedding_qos_mode)"
check "qos mode background" "background" "$(MAGICIAN_OLLAMA_EMBEDDING_QOS=background embedding_qos_mode)"

if [[ "$fail" -ne 0 ]]; then
  printf 'embedding QoS tests: %s passed, %s failed\n' "$pass" "$fail" >&2
  exit 1
fi
printf 'embedding QoS tests: %s passed\n' "$pass"
exit 0
