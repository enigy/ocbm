#!/bin/sh
# finish.sh — the last step of the one-file OCBM install. The stock updater runs it after its copy
# step ($1 = that step's status), which once.sh left with nothing to copy. It only runs when once.sh
# succeeded, so OCBM is armed: tell the user, keep the image from running again, and restart into
# the trial. On the stick route the stock updater never restarts the adapter itself; over USB
# (update_box_ota.sh) it reboots a few seconds after this returns.
#
# Test hooks (tools/ocbm_installer_test.sh): STATE, REBOOT, REBOOT_DELAY.
STATE="${STATE:-/tmp/ocbm_install.state}"
REBOOT="${REBOOT:-/sbin/reboot}"
REBOOT_DELAY="${REBOOT_DELAY:-3}"
[ -r "$STATE" ] || exit 0
. "$STATE"

{
  echo "OCBM install: INSTALLED (trial)"
  echo "installer: $VER"
  echo
  echo "The adapter restarts now and runs OCBM. Connect it with Carlink."
  echo "Stock CarPlay stays on the adapter as a fallback until the Carlink session that confirms it ends."
  echo "If OCBM fails its start-up self-test, the adapter goes back to stock CarPlay by itself."
  if [ -n "$BACKUP" ]; then
    echo
    echo "A full flash backup is in ${BACKUP#$STICK/} on this stick. It holds the adapter's Wi-Fi"
    echo "password and Bluetooth keys: keep the stick private."
  fi
  [ "${1:-0}" = 0 ] || echo "(the stock copy step reported status $1; it had nothing to copy)"
  echo
  echo "--- log"
  cat "$LOG"
} > "$RESULT"

if [ "$ROUTE" = stick ] && [ -d "$STICK" ]; then
  cp "$RESULT" "$STICK/OCBM_RESULT.txt"
  [ -f "$IMG" ] && mv -f "$IMG" "$IMG.installed"
  sync
  ( sleep "$REBOOT_DELAY"; $REBOOT ) >/dev/null 2>&1 &
fi
exit 0
