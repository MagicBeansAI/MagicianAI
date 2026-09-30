#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

fresh="$TMP_ROOT/fresh"
MAGICIAN_INSTALL_DRYRUN=1 bash "$REPO/scripts/install.sh" \
  --mode user --flow container --runtime docker --data-dir "$fresh" --yes >/dev/null
for path in \
  magician-config.yaml \
  llm-router.yaml \
  .env \
  operator-config.yaml \
  scopes/anonymous/default/programs/harness_reliability.md \
  scopes/anonymous/default/agent_runtime/agents/harness-sre/definition.agent.yaml; do
  [[ -s "$fresh/$path" ]] || { echo "ERROR: fresh install did not seed $path" >&2; exit 1; }
done

existing="$TMP_ROOT/existing"
mkdir -p "$existing/scopes/anonymous/default/programs"
printf 'operator-owned-config\n' > "$existing/magician-config.yaml"
printf 'operator-owned-router\n' > "$existing/llm-router.yaml"
printf 'operator-owned-env\n' > "$existing/.env"
printf 'operator-owned-program\n' > "$existing/scopes/anonymous/default/programs/harness_reliability.md"
before="$(find "$existing" -type f -print0 | sort -z | xargs -0 shasum -a 256)"

MAGICIAN_INSTALL_DRYRUN=1 bash "$REPO/scripts/install.sh" \
  --mode user --flow container --runtime docker --data-dir "$existing" --yes >/dev/null

[[ "$(cat "$existing/magician-config.yaml")" == "operator-owned-config" ]]
[[ "$(cat "$existing/llm-router.yaml")" == "operator-owned-router" ]]
[[ "$(cat "$existing/.env")" == "operator-owned-env" ]]
[[ "$(cat "$existing/scopes/anonymous/default/programs/harness_reliability.md")" == "operator-owned-program" ]]
after_owned="$(find "$existing" -type f \( -name magician-config.yaml -o -name llm-router.yaml -o -name .env -o -name harness_reliability.md \) -print0 | sort -z | xargs -0 shasum -a 256)"
[[ "$before" == "$after_owned" ]] || { echo "ERROR: installer changed operator-owned files" >&2; exit 1; }

unsupported="$TMP_ROOT/unsupported"
if MAGICIAN_INSTALL_DRYRUN=1 bash "$REPO/scripts/install.sh" \
  --mode user --flow local --runtime docker --data-dir "$unsupported" --yes >/dev/null 2>&1; then
  echo "ERROR: unsupported user/local mode unexpectedly succeeded" >&2
  exit 1
fi
[[ ! -e "$unsupported" ]] || { echo "ERROR: unsupported mode mutated the runtime root" >&2; exit 1; }

# Pi is a required harness engine for the host runtime: dev/local schedules it
# right after prerequisites, so a missing Node fails before the long build.
local_plan="$(MAGICIAN_INSTALL_DRYRUN=1 bash "$REPO/scripts/install.sh" \
  --mode dev --flow local --data-dir "$TMP_ROOT/local" --yes 2>&1)"
grep -q 'would run: phase_prereqs' <<<"$local_plan" && grep -q 'would run: phase_pi' <<<"$local_plan" \
  || { echo "ERROR: dev/local install does not schedule phase_pi" >&2; exit 1; }
[[ "$(grep -o 'would run: phase_[a-z_]*' <<<"$local_plan" | sed -n 2p)" == "would run: phase_pi" ]] \
  || { echo "ERROR: phase_pi must run right after phase_prereqs" >&2; exit 1; }
grep -q 'would run: phase_meet_audio' <<<"$local_plan" \
  || { echo "ERROR: dev/local install does not schedule phase_meet_audio" >&2; exit 1; }

# pipewire-pulse replaces pulseaudio. Install it only when no daemon package
# is present and nothing is answering pactl.
# shellcheck disable=SC1091
source "$REPO/scripts/setup-meet-bot.sh"
[ "$(linux_pipewire_install_decision 0 1)" = skip ]
[ "$(linux_pipewire_install_decision 1 0)" = skip ]
[ "$(linux_pipewire_install_decision 1 1)" = skip ]
[ "$(linux_pipewire_install_decision 0 0)" = install ]

echo "composed installer non-clobber tests passed"
