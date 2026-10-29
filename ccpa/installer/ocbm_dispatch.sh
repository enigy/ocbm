#!/bin/sh
# OCBM_DISPATCH 1
# ocbm_dispatch.sh — /script/start_main_service.sh on an adapter converted by the one-file install
# (once.sh renames it into place). rcS starts it in the background on every boot, and
# /script/ocbm_mode picks what boots:
#
#   (none)            stock: hand the boot to start_main_service.stock.sh, Carlinkit's own script as
#                     it was before the install. A revert leaves the adapter here, and the revert's
#                     report goes to the next USB stick plugged in.
#   trial, confirmed  OCBM, with stock kept as the fallback. Before anything starts, count the start:
#                     two in a row that never passed the self-test go back to stock (hangs, reboot
#                     loops). Then start OCBM, keep ocbmd and the session supervisor running, run
#                     ocbm_selftest.sh (a failure goes back to stock), and watch the stick for a file
#                     named OCBM_RESTORE (goes back to stock). In `confirmed` a passing self-test runs
#                     ocbm_finish.sh: the confirming session was ended by the power going off.
#   final             OCBM only: ocbm_finish.sh removed the Carlinkit stack, so there is nothing to
#                     go back to. A finish a power cut interrupted (finish_pending) is resumed.
#
# An update (ocbm_update.sh, in trial or final) leaves /etc/ocbm/update_pending, its journal, and
# restarts. Before OCBM starts, the dispatcher settles it: a commit a power cut interrupted is
# finished, a swap or an undo it interrupted is undone, and otherwise the start is counted (two that
# never passed the self-test put the old version back). Then the self-test decides: a pass keeps the
# update (the .prev copies go, installed_version moves on), a failure puts the version it replaced
# back and restarts. During an update's trial its own counter and self-test replace the install's: a
# failed update falls back to the OCBM it replaced, never to stock. The outcome is one line in
# /etc/ocbm/update_result.
#
# In every OCBM mode it creates /tmp/not_change_mass_storage before OCBM starts. Without it the stock
# stick handler (update_box.sh, still run by mdev) switches the USB port to stick sharing when a
# stick without an image goes in, and Carlink loses the adapter.
#
# The install leaves /etc/inittab alone, and the stock one has no respawn lines, so this script runs
# run_ocbmd.sh and run_supervisor.sh in loops of its own: the same restart-when-dead that
# ocbm_install.sh gets from ::respawn.
#
# Test hooks (tools/ocbm_stageb_test.sh, tools/ocbm_update_test.sh): ROOT RUN STICK PROC POLL
# STICK_WAIT REBOOT.
ROOT="${ROOT:-}"
RUN="${RUN:-/tmp}"
STICK="${STICK:-/mnt/UPAN}"
PROC="${PROC:-/proc}"
POLL="${POLL:-2}"
STICK_WAIT="${STICK_WAIT:-60}"
S="$ROOT/script"
EO="$ROOT/etc/ocbm"
SMS_STOCK="$S/start_main_service.stock.sh"
U="$EO/update_pending"
REBOOT="${REBOOT:-/sbin/reboot}"
update_trial=0
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH

log() { echo "[dispatch] $*" >> "$RUN/ocbm_dispatch.log"; echo "[dispatch] $*" >> "$RUN/box.log"; }
stick_in() { [ -d "$STICK" ] && grep -q " $STICK " "$PROC/mounts" 2>/dev/null; }

deliver() {  # the last revert's report, onto the next stick that is plugged in
  while ! stick_in; do sleep "$POLL"; done
  sleep "$POLL"   # let update_box.sh look at the stick first
  r="$STICK/OCBM_RESULT.txt"
  if [ -f "$r" ]; then
    { cat "$EO/pending_result.txt"; echo; echo "--- before that"; cat "$r"; } > "$r.new"
  else
    cp "$EO/pending_result.txt" "$r.new"
  fi
  mv -f "$r.new" "$r" && sync && mv -f "$EO/pending_result.txt" "$EO/delivered_result.txt"
  sync
  rm -f "$RUN/not_change_mass_storage"   # later sticks are shared with the car again, as stock does
}

stock() {
  if [ -s "$SMS_STOCK" ]; then
    if [ -f "$EO/pending_result.txt" ]; then
      touch "$RUN/not_change_mass_storage"   # keep the next stick mounted until the report is on it
      deliver </dev/null >/dev/null 2>&1 &
    fi
    exec sh "$SMS_STOCK"
  fi
  log "the stock start script is missing: starting OCBM instead"
}

back_to_stock() {  # at boot, before OCBM has started: no restart needed
  if [ "$update_trial" = 1 ]; then   # an update's trial falls back to the OCBM it replaced, not to stock
    update_rollback "$1"
    sync; $REBOOT
    exit 0
  fi
  log "going back to stock: $1"
  if [ -x "$S/ocbm_revert.sh" ]; then
    "$S/ocbm_revert.sh" --no-reboot "$1" && stock
  else   # the revert script itself is gone: the least it would have done
    { echo "$1"; echo "reverted version=$(cat "$EO/installed_version" 2>/dev/null)"; } >> "$EO/last_result.txt"
    rm -f "$S/ocbm_mode" "$EO/boot_attempts"; sync
    stock
  fi
  log "could not go back to stock; starting OCBM anyway"
}

first_start() {  # $1 PASSED: the first trial start adds its verdict to the install's result file
  [ -e "$EO/first_start.txt" ] && return 0
  up=$(cut -d. -f1 "$PROC/uptime" 2>/dev/null)
  { echo; echo "--- first start"
    echo "self-test: $1, ${up:-?} s after power-on. Connect the adapter with Carlink."
    cat "$RUN/ocbm_selftest.txt"; } > "$EO/first_start.txt"
  sync
  i=0
  while ! stick_in && [ "$i" -lt "$STICK_WAIT" ]; do sleep "$POLL"; i=$((i + POLL)); done
  if stick_in && [ -f "$STICK/OCBM_RESULT.txt" ]; then
    cat "$EO/first_start.txt" >> "$STICK/OCBM_RESULT.txt" && sync
  fi
}

# ---- an update's trial (ocbm_update.sh writes the journal; these settle it) ----------------------
journal() { sed -n "s/^$1=//p" "$U" | head -1; }
ver_of() { set -- $1; echo "${1:-?}"; }
put() {  # $1 file, $2 its one line: written .new + rename, so a power cut leaves the old line or the new
  { echo "$2" > "$1.new" && mv -f "$1.new" "$1"; } 2>/dev/null
}

update_commit() {  # the update passed its self-test, or a power cut stopped its commit: keep it
  grep -qx committing "$U" || { echo committing >> "$U"; sync; }
  awk '$1 == "replace" { print $2 }' "$U" | while read -r t; do rm -f "$ROOT/$t.prev"; done
  to=$(journal to); from=$(journal from)
  put "$EO/installed_version" "$to"
  put "$EO/update_result" "committed $to (from $from)"
  rm -f "$EO/update_attempts" "$U"; sync
  log "update committed: $to (from $from)"
}

update_rollback() {  # $1 why: put back the version the update replaced
  grep -q '^rolling_back=' "$U" || { echo "rolling_back=$1" >> "$U"; sync; }
  why=$(journal rolling_back)
  awk '$1 == "replace" || $1 == "add" { print $1, $2 }' "$U" | while read -r kind t; do
    f="$ROOT/$t"
    if [ "$kind" = replace ]; then
      [ -e "$f.prev" ] && mv -f "$f.prev" "$f"
    else
      rm -f "$f"
    fi
    rm -f "$f.new"
  done
  to=$(journal to); from=$(journal from)
  put "$EO/update_result" "rolled_back $to: $why (kept $from)"
  # The failed starts were the update's, not the install's.
  rm -f "$EO/update_attempts" "$EO/boot_attempts" "$U"; sync
  log "update rolled back to $from: $why"
}

update_boot() {  # before OCBM starts
  if grep -qx committing "$U"; then update_commit; return; fi
  if grep -q '^rolling_back=' "$U"; then update_rollback "power was lost while the update was being undone"; return; fi
  if ! grep -qx placed "$U"; then update_rollback "power was lost while the update was being placed"; return; fi
  n=$(cat "$EO/update_attempts" 2>/dev/null)
  case "$n" in ''|*[!0-9]*) n=0 ;; esac
  if [ "$n" -ge 2 ]; then
    update_rollback "OCBM $(ver_of "$(journal to)") started $n times in a row without passing its start-up self-test"
    return
  fi
  { echo $((n + 1)) > "$EO/update_attempts.new" && mv -f "$EO/update_attempts.new" "$EO/update_attempts" && sync; } \
    || log "could not count this start (flash full?)"
  update_trial=1
  log "update trial: $(journal to), start $((n + 1))"
}

selftest() {
  if "$S/ocbm_selftest.sh" > "$RUN/ocbm_selftest.txt" 2>&1; then
    rm -f "$EO/boot_attempts"; sync
    log "self-test: $(tail -1 "$RUN/ocbm_selftest.txt")"
    [ "$update_trial" = 1 ] && update_commit
    first_start PASSED
    [ "$mode" = confirmed ] && [ -x "$S/ocbm_finish.sh" ] && "$S/ocbm_finish.sh" >> "$RUN/box.log" 2>&1
  else
    log "self-test: $(tail -1 "$RUN/ocbm_selftest.txt")"
    if [ "$update_trial" = 1 ]; then   # back to the OCBM the update replaced, not to stock
      update_rollback "its start-up self-test failed ($(tail -1 "$RUN/ocbm_selftest.txt"))"
      sync; $REBOOT
      return
    fi
    "$S/ocbm_revert.sh" "the start-up self-test failed ($(tail -1 "$RUN/ocbm_selftest.txt"))"
  fi
}

restore_watch() {  # OCBM_RESTORE on a stick: back to stock
  while :; do
    if stick_in && [ -e "$STICK/OCBM_RESTORE" ]; then
      mv -f "$STICK/OCBM_RESTORE" "$STICK/OCBM_RESTORE.done"; sync   # one use: a later install is not undone
      log "OCBM_RESTORE found on the stick"
      [ -f "$U" ] && update_rollback "a file named OCBM_RESTORE was found on a USB stick"   # stock gets the OCBM it had
      "$S/ocbm_revert.sh" "a file named OCBM_RESTORE was found on a USB stick"
      return
    fi
    sleep "$POLL"
  done
}

respawn() { while :; do "$@"; sleep 1; done; }

# ---------------------------------------------------------------------------------------------
mode=$(cat "$S/ocbm_mode" 2>/dev/null)
case "$mode" in
  trial|confirmed|final) ;;
  '') stock ;;
  *) log "unknown mode '$mode': starting stock"; stock ;;
esac
[ -s "$SMS_STOCK" ] || mode=final   # nothing to go back to
log "mode $mode, uptime $(cut -d' ' -f1 "$PROC/uptime" 2>/dev/null)s"
[ -f "$U" ] && update_boot

if [ "$mode" != final ]; then
  for f in usr/sbin/ocbmd script/ocbm_boot.sh script/ocbm_selftest.sh script/ocbm_revert.sh \
           script/run_ocbmd.sh script/run_supervisor.sh; do
    [ -x "$ROOT/$f" ] || back_to_stock "/$f is missing"
  done
  if [ "$update_trial" = 0 ]; then   # an update's trial counts its own starts
    n=$(cat "$EO/boot_attempts" 2>/dev/null)
    case "$n" in ''|*[!0-9]*) n=0 ;; esac
    [ "$n" -ge 2 ] && back_to_stock "OCBM started $n times in a row without passing its start-up self-test"
    { echo $((n + 1)) > "$EO/boot_attempts.new" && mv -f "$EO/boot_attempts.new" "$EO/boot_attempts" && sync; } \
      || log "could not count this start (flash full?)"
  fi
fi

touch "$RUN/not_change_mass_storage"
if [ -z "$ROOT" ]; then   # the first lines of stock's start script (and ccpa/rootfs's owned one)
  date -s "2020-01-02 $(date +%T)" >/dev/null 2>&1 &
  rm -f /dev/random && ln -s /dev/urandom /dev/random &                 # non-blocking openssl RNG
  echo y > /sys/module/printk/parameters/time && echo 7 > /proc/sys/kernel/printk &
fi
"$S/ocbm_boot.sh"   # gadget as the OCBM accessory, ocbmd, session supervisor; returns at once
[ -x "$S/init_gpio.sh" ] && "$S/init_gpio.sh" >/dev/null 2>&1 &   # stock: Bluetooth reset line, quick-charge pins
respawn "$S/run_ocbmd.sh" </dev/null >/dev/null 2>&1 &
respawn "$S/run_supervisor.sh" </dev/null >/dev/null 2>&1 &
if [ "$mode" = final ]; then
  [ -e "$EO/finish_pending" ] && [ -x "$S/ocbm_finish.sh" ] && "$S/ocbm_finish.sh" </dev/null >> "$RUN/box.log" 2>&1 &
  [ "$update_trial" = 1 ] && selftest </dev/null >/dev/null 2>&1 &
  exit 0
fi
selftest </dev/null >/dev/null 2>&1 &
restore_watch </dev/null >/dev/null 2>&1 &
exit 0
