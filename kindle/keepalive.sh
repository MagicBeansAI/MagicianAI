#!/usr/bin/env bash
# Keep the Kindle's USB network link alive.
#
# WHY THIS IS NEEDED — and where the fault actually is.
#
# macOS stops delivering packets on the RNDIS/CDC-ECM interface after it goes
# idle. The Kindle is not at fault: instrumenting the device and staying silent
# for 32 minutes showed its state byte-identical throughout —
#
#   up=1 ip=192.168.15.244 g_ether=2 dropbear=1 carrier=1 oper=up
#
# — with no transition at all, while the Mac could not reach it. On the host,
# Opkts kept climbing and Ipkts stayed at 0: macOS transmits and receives
# nothing back. Periodic traffic keeps its receive path awake.
#
# So this is a workaround for a host driver behaviour, not for anything on the
# Kindle. Run it for the duration of a dev session:
#
#     ./keepalive.sh &            # background it
#     ./keepalive.sh --verbose    # watch it
set -uo pipefail

KINDLE_IP=${KINDLE_IP:-192.168.15.244}
INTERVAL=${INTERVAL:-10}
VERBOSE=0
[ "${1:-}" = "--verbose" ] && VERBOSE=1

iface_for_kindle() {
  # The RNDIS interface number changes across re-enumeration, so resolve it
  # rather than hardcoding en16.
  networksetup -listallhardwareports 2>/dev/null \
    | awk '/RNDIS\/Ethernet Gadget/{getline; print $2; exit}'
}

last_state=""
while : ; do
  iface=$(iface_for_kindle)
  if [ -z "$iface" ]; then
    state="no-gadget"
  elif ping -c 1 -t 2 "$KINDLE_IP" >/dev/null 2>&1; then
    state="up"
  else
    state="unreachable"
  fi

  if [ "$state" != "$last_state" ]; then
    printf '%s  %s' "$(date +%H:%M:%S)" "$state"
    [ -n "$iface" ] && printf ' (%s)' "$iface"
    printf '\n'
    last_state="$state"
  elif [ "$VERBOSE" = 1 ]; then
    printf '%s  %s\n' "$(date +%H:%M:%S)" "$state"
  fi

  sleep "$INTERVAL"
done
