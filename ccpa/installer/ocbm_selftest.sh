#!/bin/sh
# ocbm_selftest.sh — does OCBM work on this adapter, with no car and no app? The dispatcher runs it
# on every trial start; a failure sends the adapter back to stock. It passes the first moment all of
# these hold together, checking once a second for up to SECS (30):
#
#   gadget    the USB gadget is the OCBM accessory: functions=accessory, 1314:2d00, enabled
#   node      /dev/usb_accessory exists
#   ocbmd     ocbmd is running
#   radio     radio_detect.sh resolves this unit's own Wi-Fi/Bluetooth bring-up: its descriptor
#             carries the SDIO id the hardware reports, a backend, and wireless=yes. It reads the
#             unit's own files, never a chip list, and loads nothing.
#
# No host is needed: CONNECTED or CONFIGURED is reported, not required. Prints one line per check,
# and the verdict last (the dispatcher quotes that line).
#
# Test hooks (tools/ocbm_stageb_test.sh): SYS DEVDIR RUN SECS OCBMD RADIO_DETECT
SYS="${SYS:-/sys}"
DEVDIR="${DEVDIR:-/dev}"
RUN="${RUN:-/tmp}"
SECS="${SECS:-30}"
OCBMD="${OCBMD:-ocbmd}"
RADIO_DETECT="${RADIO_DETECT:-/script/radio_detect.sh}"
A="$SYS/class/android_usb_accessory/android0"
CAPS="$RUN/radio_caps"

cap() { sed -n "s/^RADIO_$1=//p" "$CAPS" 2>/dev/null | tr -d '"' | head -1; }
hw=""
for d in "$SYS"/bus/sdio/devices/*; do [ -r "$d/device" ] && { hw=$(cat "$d/device"); break; }; done
RADIO_CAPS="$CAPS" sh "$RADIO_DETECT" >/dev/null 2>&1
radio_ok=0
[ -n "$hw" ] && [ "$(cap SDIO_DEVICE)" = "$hw" ] && [ -n "$(cap BACKEND)" ] && [ "$(cap BACKEND)" != none ] \
  && [ "$(cap WIRELESS)" = yes ] && radio_ok=1

gadget_ok() {
  [ "$(cat "$A/functions" 2>/dev/null)" = accessory ] || return 1
  case "$(cat "$A/idVendor" 2>/dev/null)" in 1314|0x1314) ;; *) return 1 ;; esac   # what host apps match
  case "$(cat "$A/idProduct" 2>/dev/null)" in 2d00|0x2d00|2D00|0x2D00) ;; *) return 1 ;; esac
  [ "$(cat "$A/enable" 2>/dev/null)" = 1 ]
}
node_ok() { [ -e "$DEVDIR/usb_accessory" ]; }
# busybox pidof matches the process name, argv[0]'s basename or the executable. Not busybox pgrep -x:
# it matches all of argv[0], so it never sees an ocbmd started as /usr/sbin/ocbmd (factory bench,
# 2026-10-28: a healthy trial reverted). pgrep -x is for hosts without pidof (macOS, the tests).
ocbmd_pids() { if command -v pidof >/dev/null 2>&1; then pidof "$OCBMD"; else pgrep -x "$OCBMD"; fi 2>/dev/null; }
ocbmd_ok() { [ -n "$(ocbmd_pids)" ]; }

i=0
while :; do
  g=0; n=0; o=0
  gadget_ok && g=1
  node_ok && n=1
  ocbmd_ok && o=1
  [ $g$n$o$radio_ok = 1111 ] && break
  [ "$i" -ge "$SECS" ] && break
  sleep 1; i=$((i + 1))
done

echo "gadget: functions=$(cat "$A/functions" 2>/dev/null) id=$(cat "$A/idVendor" 2>/dev/null):$(cat "$A/idProduct" 2>/dev/null) enable=$(cat "$A/enable" 2>/dev/null) state=$(cat "$A/state" 2>/dev/null)"
echo "node: $([ $n = 1 ] && echo "$DEVDIR/usb_accessory present" || echo "no $DEVDIR/usb_accessory")"
echo "ocbmd: $([ $o = 1 ] && echo "running (pid $(ocbmd_pids | tr ' ' '\n' | head -1))" || echo "not running")"
echo "radio: hardware ${hw:-none}, descriptor $(cap CHIP) $(cap SDIO_DEVICE), backend $(cap BACKEND), wireless $(cap WIRELESS)"
if [ $g$n$o$radio_ok = 1111 ]; then
  echo "PASSED in ${i} s"
  exit 0
fi
why=""
[ $g = 1 ] || why="$why, the USB gadget is not the OCBM accessory"
[ $n = 1 ] || why="$why, no /dev/usb_accessory"
[ $o = 1 ] || why="$why, ocbmd is not running"
[ $radio_ok = 1 ] || why="$why, the radio bring-up was not found"
echo "FAILED after ${i} s: ${why#, }"
exit 1
