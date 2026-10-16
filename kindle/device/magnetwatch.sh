#!/bin/sh
# Network watchdog for dev mode.
#
# The Kindle stays fully awake (screen on, responsive) while its USB network
# stack stops answering. The only channel to the device is the thing that
# breaks, so this records state locally and we read it afterwards.
#
# Started by magdev.sh on DEV entry, stopped on the way out.

LOG=/mnt/us/magnet.log
PIDFILE=/tmp/magnetwatch.pid

echo $$ > "$PIDFILE"

# Fresh log each dev session; the interesting window is always the most recent.
{
  echo "=== netwatch start $(date) ==="
  echo "kernel: $(uname -r)"
} > "$LOG" 2>&1
sync

prev_state=""
while : ; do
  ts=$(date +%H:%M:%S)

  usb0_up=$(ifconfig usb0 2>/dev/null | grep -c "UP")
  usb0_ip=$(ifconfig usb0 2>/dev/null | sed -n 's/.*inet addr:\([0-9.]*\).*/\1/p')
  gether=$(lsmod 2>/dev/null | grep -c g_ether)
  drop=$(ps 2>/dev/null | grep -v grep | grep -c dropbear)
  # Gadget controller state: the definitive "is the USB link live" signal.
  udc=$(cat /sys/class/udc/*/state 2>/dev/null | tr '\n' ' ')
  carrier=$(cat /sys/class/net/usb0/carrier 2>/dev/null)
  operstate=$(cat /sys/class/net/usb0/operstate 2>/dev/null)

  state="up=$usb0_up ip=$usb0_ip g_ether=$gether dropbear=$drop udc=$udc carrier=$carrier oper=$operstate"

  # Log every sample, but shout when something actually changes.
  if [ "$state" != "$prev_state" ]; then
    echo "$ts  CHANGED  $state" >> "$LOG"
    echo "$ts  --- dmesg tail at change ---" >> "$LOG"
    dmesg 2>/dev/null | tail -12 >> "$LOG"
    prev_state="$state"
  else
    echo "$ts  $state" >> "$LOG"
  fi
  sync

  sleep 15
done
