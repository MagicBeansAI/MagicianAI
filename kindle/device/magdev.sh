#!/bin/sh
# Name: Magician Dev Mode
# Author: magician
#
# One tap flips the Kindle between:
#
#   NORMAL  - USB is a drive, sleep behaves normally (a plain Kindle)
#   DEV     - USB is a network gadget, SSH on 192.168.15.244, sleep disabled
#
# WHY THIS WAITS INSTEAD OF REFUSING
#
# USB gadget reconfiguration cannot happen while the cable is attached, so the
# toggle needs an unplugged device. The obvious design — refuse, and ask the
# user to unplug and tap again — is unusable here: KPPMainApp, the framework
# process that hosts scriptlets, dies on its second launch and leaves the UI
# stuck on "Opening..." forever. That is a platform fault, not ours; a two-line
# scriptlet that only echoes reproduces it exactly. It is the same component
# that coredumped on this device's first KUAL launch.
#
# So one tap does the whole job: if the cable is in, we wait for it to come
# out, then toggle. The scriptlet is never opened twice, so the crash is never
# triggered.

UB=/mnt/us/usbnet/bin
LOG=/mnt/us/magdev.log
MAGKINDLE=/mnt/us/magkindle
WAIT_SECS=120

# Leave cleanly: send the framework home so the scriptlet is not left open.
# Always exit 0 — a non-zero exit makes the framework mark the item as failed.
close_out() {
  sleep "${1:-3}"
  lipc-set-prop com.lab126.appmgrd start app://com.lab126.booklet.home 2>/dev/null
  exit 0
}

# Same sources the vendor's usbnet.sh consults.
is_plugged() {
  if [ -d /etc/kdb ]; then
    f="$(kdb get system/driver/usb/SYS_CONNECTED 2>/dev/null)"
    if [ -n "$f" ] && [ "$(cat "$f" 2>/dev/null)" = "1" ]; then
      return 0
    fi
  fi
  [ "$(cat /sys/devices/platform/charger/charging 2>/dev/null)" = "1" ] && return 0
  return 1
}

if lsmod 2>/dev/null | grep -q g_ether; then
  STATE=dev
else
  STATE=normal
fi

if [ "$STATE" = "dev" ]; then
  GOING="NORMAL"
else
  GOING="DEV"
fi

{
  echo "=== magician dev-mode toggle ==="
  echo "date   : $(date)"
  echo "state  : $STATE -> $GOING"
} > "$LOG" 2>&1
sync

echo "  $STATE  ->  $GOING"
echo ""

if is_plugged; then
  echo "UNPLUG THE USB CABLE NOW."
  echo ""
  echo "Waiting for you to pull it out,"
  echo "then switching automatically."
  echo ""

  waited=0
  while is_plugged && [ "$waited" -lt "$WAIT_SECS" ]; do
    sleep 3
    waited=$((waited + 3))
    # Print sparingly: every line is piped to FBInk and costs a panel refresh.
    if [ $((waited % 15)) -eq 0 ]; then
      echo "  ...still plugged in (${waited}s)"
    fi
  done

  if is_plugged; then
    echo ""
    echo "Timed out after ${WAIT_SECS}s. Nothing changed."
    echo "Tap again when you can unplug."
    echo "timed out waiting for unplug" >> "$LOG" 2>&1
    sync
    close_out 5
  fi

  echo ""
  echo "Cable out. Switching..."
  # The gadget layer needs a moment to settle after the physical disconnect.
  sleep 2
fi

if [ "$STATE" = "dev" ]; then
  # Clear the badge first, so the screen is clean even if the USB toggle
  # below misbehaves.
  #
  # `ribbon off` signals the pid the watcher recorded itself. Deliberately NOT
  # pkill: busybox ps truncates arguments, and a -f pattern also matches the
  # shell running it, so pkill here can kill its own caller.
  [ -x "$MAGKINDLE" ] && "$MAGKINDLE" ribbon off >> "$LOG" 2>&1
  # Stop the network watchdog; its log stays for inspection.
  nwpid=$(cat /tmp/magnetwatch.pid 2>/dev/null)
  [ -n "$nwpid" ] && kill "$nwpid" 2>/dev/null
  "${UB}/usbnetwork" >> "$LOG" 2>&1
  lipc-set-prop com.lab126.powerd preventScreenSaver 0 >> "$LOG" 2>&1
else
  "${UB}/usbnetwork" >> "$LOG" 2>&1
  lipc-set-prop com.lab126.powerd preventScreenSaver 1 >> "$LOG" 2>&1
  # `ribbon watch` double-forks and setsids itself, so it survives this script
  # exiting. `nohup ... &` is NOT sufficient: the scriptlet runner reaps its
  # whole process group on exit and silently killed the watcher.
  [ -x "$MAGKINDLE" ] && "$MAGKINDLE" ribbon watch >> "$LOG" 2>&1
  # Network watchdog. The USB link dies while the device stays awake, and the
  # only channel to it is the thing that breaks — so record state locally.
  # setsid, not `nohup ... &`: the scriptlet runner reaps its process group.
  NW=/mnt/us/bin/magnetwatch.sh
  if [ -x "$NW" ]; then
    nwpid=$(cat /tmp/magnetwatch.pid 2>/dev/null)
    [ -n "$nwpid" ] && kill "$nwpid" 2>/dev/null
    if command -v setsid >/dev/null 2>&1; then
      setsid sh "$NW" </dev/null >/dev/null 2>&1 &
    else
      ( sh "$NW" </dev/null >/dev/null 2>&1 & ) &
    fi
  fi
fi

sleep 1
if lsmod 2>/dev/null | grep -q g_ether; then NOW=dev; else NOW=normal; fi
echo "after  : $NOW" >> "$LOG" 2>&1
sync

echo ""
if [ "$NOW" = "$STATE" ]; then
  echo "   TOGGLE DID NOT TAKE"
  echo ""
  echo "still in: $STATE"
  echo "see /mnt/us/magdev.log"
elif [ "$NOW" = "dev" ]; then
  echo "   DEV MODE"
  echo ""
  echo "SSH   : ssh kindle"
  echo "IP    : 192.168.15.244"
  echo "Sleep : disabled"
  echo ""
  echo "Plug in USB now."
else
  echo "   NORMAL KINDLE"
  echo ""
  echo "USB   : drive (mass storage)"
  echo "Sleep : enabled"
  echo ""
  echo "Plug in to mount it."
fi

close_out 5
