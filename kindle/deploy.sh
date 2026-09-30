#!/usr/bin/env bash
# Build magkindle for the Kindle PW3 and run it on the device.
#
#   ./deploy.sh            build + deploy + run "pattern"
#   ./deploy.sh touch      ... run "touch" instead (also: paint)
#   ./deploy.sh build      build only
#   ./deploy.sh shell      drop into a root shell on the device
#
# Requires: `ssh kindle` working (see docs/plans/2026-08-28-kindle-pw3-thin-client-plan.md)
set -euo pipefail

TARGET=armv7-unknown-linux-musleabihf
BIN=magkindle
REMOTE=/mnt/us/${BIN}
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/Volumes/build/magician/builds/kindle}"
OUT="${CARGO_TARGET_DIR}/${TARGET}/release/${BIN}"

# `build` and `shell` are local verbs; anything else is a mode passed straight
# through to the binary on the device (pattern|touch|paint).
ACTION="${1:-pattern}"
case "${ACTION}" in
  shell) exec ssh kindle ;;
  build) ;;
  *) ;;
esac

echo "==> building ${BIN} for ${TARGET}"
cargo zigbuild --release --target "${TARGET}"

# Guard against shipping a host binary by mistake.
file "${OUT}" | grep -q "ELF 32-bit.*ARM" \
  || { echo "!! not an ARM binary: $(file "${OUT}")" >&2; exit 1; }
file "${OUT}" | grep -q "statically linked" \
  || { echo "!! not statically linked — it will not run on the Kindle" >&2; exit 1; }
echo "==> $(file -b "${OUT}")"
echo "==> $(du -h "${OUT}" | cut -f1)"

[ "${ACTION}" = "build" ] && exit 0

# Keep the screen awake while working. NOTE: this does not keep the USB link
# alive — the link goes quiet because macOS stops delivering on the RNDIS
# interface when idle, not because the Kindle sleeps. Run ./keepalive.sh for
# that.
echo "==> keeping device awake"
ssh -o BatchMode=yes -o ConnectTimeout=10 kindle 'lipc-set-prop com.lab126.powerd preventScreenSaver 1' >/dev/null 2>&1 \
  || echo "   (warning: could not set preventScreenSaver)"

# The DEV badge watcher runs from ${REMOTE}, which makes scp fail with
# "Text file busy". Stop it, deploy, and put it back if it was running.
BADGE_WAS_RUNNING=0
if ssh -o BatchMode=yes kindle 'p=$(cat /tmp/magkindle-ribbon.pid 2>/dev/null); [ -n "$p" ] && [ -d "/proc/$p" ]' 2>/dev/null; then
  BADGE_WAS_RUNNING=1
  echo "==> pausing DEV badge (holds the binary open)"
  ssh -o BatchMode=yes -o ConnectTimeout=10 kindle "${REMOTE} ribbon off" >/dev/null 2>&1 || true
  sleep 1
fi

echo "==> deploying to kindle:${REMOTE}"
scp -O "${OUT}" "kindle:${REMOTE}" >/dev/null

echo "==> running on device: ${ACTION}"
echo "---------------------------------------------"
ssh -o BatchMode=yes -o ConnectTimeout=15 kindle "chmod +x ${REMOTE} && ${REMOTE} ${ACTION} </dev/null"
RC=$?

if [ "${BADGE_WAS_RUNNING}" = "1" ]; then
  echo "==> restoring DEV badge"
  ssh -o BatchMode=yes -o ConnectTimeout=10 kindle "${REMOTE} ribbon watch" >/dev/null 2>&1 || true
fi
exit $RC
