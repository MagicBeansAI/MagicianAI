#!/usr/bin/env bash
# Sourced helper for dedicated embedding-Ollama host QoS/cpuset wraps.
# Defines functions only. Do not execute. Do not pin Magician workers.

# MAGICIAN_OLLAMA_EMBEDDING_QOS (default background):
#   background | utility | on | true | 1 | unset → apply QoS
#   off | pass_through | 0 | false → no wrap (kill switch)
# MAGICIAN_OLLAMA_EMBEDDING_CPUSET: Linux CPU list (e.g. 0-1). Ignored unless QoS is on.
# Linux without CPUSET uses nice -n 10. Does not pin Magician workers.

embedding_qos_normalized() {
  printf '%s' "${MAGICIAN_OLLAMA_EMBEDDING_QOS:-background}" | tr '[:upper:]' '[:lower:]'
}

embedding_qos_on() {
  case "$(embedding_qos_normalized)" in
    background|utility|on|true|1) return 0 ;;
    *) return 1 ;;
  esac
}

embedding_qos_mode() {
  if embedding_qos_on; then
    printf 'background'
  else
    printf 'off'
  fi
}

embedding_qos_find_cmd() {
  local name="$1" resolved p
  resolved="$(command -v "$name" 2>/dev/null || true)"
  if [[ -n "$resolved" ]]; then
    printf '%s' "$resolved"
    return 0
  fi
  for p in "/usr/bin/$name" "/usr/sbin/$name" "/bin/$name" "/sbin/$name"; do
    if [[ -x "$p" ]]; then
      printf '%s' "$p"
      return 0
    fi
  done
  return 1
}

# Print prefix tokens for `ollama serve`, or nothing. Optional $1 overrides
# `uname -s` (Darwin|Linux) so provider-free tests can cover both kernels.
embedding_serve_prefix() {
  local kernel cmd cpuset
  kernel="${1:-$(uname -s 2>/dev/null || true)}"
  embedding_qos_on || return 0
  case "$kernel" in
    Darwin)
      cmd="$(embedding_qos_find_cmd taskpolicy || true)"
      if [[ -n "$cmd" ]]; then
        printf '%s -b' "$cmd"
      else
        printf '  WARN taskpolicy not found; embedding Ollama will start without background QoS\n' >&2
      fi
      ;;
    Linux)
      cpuset="${MAGICIAN_OLLAMA_EMBEDDING_CPUSET:-}"
      cpuset="${cpuset#"${cpuset%%[![:space:]]*}"}"
      cpuset="${cpuset%"${cpuset##*[![:space:]]}"}"
      if [[ -n "$cpuset" ]]; then
        cmd="$(embedding_qos_find_cmd taskset || true)"
        if [[ -n "$cmd" ]]; then
          printf '%s -c %s' "$cmd" "$cpuset"
        else
          printf '  WARN taskset not found; embedding Ollama will start without a cpuset\n' >&2
        fi
      else
        cmd="$(embedding_qos_find_cmd nice || true)"
        if [[ -n "$cmd" ]]; then
          printf '%s -n 10' "$cmd"
        else
          printf '  WARN nice not found; embedding Ollama will start without background niceness\n' >&2
        fi
      fi
      ;;
  esac
}

embedding_print_wrap_command() {
  local prefix
  prefix="$(embedding_serve_prefix)"
  if [[ -n "$prefix" ]]; then
    printf '%s ollama serve\n' "$prefix"
  else
    printf 'ollama serve\n'
  fi
}
