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
#                     ocbm_selftest.sh (a failure goes back to stock), watch that ocbmd answers a host
#                     that talks to it (answer_watch: one it never answers goes back to stock), and
#                     watch the stick for a file named OCBM_RESTORE (goes back to stock). In
#                     `confirmed` a passing self-test runs ocbm_finish.sh: the confirming session was
#                     ended by the power going off.
#   final             OCBM only: ocbm_finish.sh removed the Carlinkit stack, so there is nothing to
#                     go back to. A finish a power cut interrupted (finish_pending) is resumed, and a
#                     stick with OCBM_RESTORE gets OCBM_RESTORE.txt saying where the way back is now.
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
# STICK_WAIT STICK_PKG_WAIT REBOOT ANSWER_SECS UMOUNT.
ROOT="${ROOT:-}"
RUN="${RUN:-/tmp}"
STICK="${STICK:-/mnt/UPAN}"
PROC="${PROC:-/proc}"
POLL="${POLL:-2}"
STICK_WAIT="${STICK_WAIT:-60}"
ANSWER_SECS="${ANSWER_SECS:-120}"
STICK_PKG_WAIT="${STICK_PKG_WAIT:-20}"   # how long a start waits for a stick that may carry an update
S="$ROOT/script"
EO="$ROOT/etc/ocbm"
SMS_STOCK="$S/start_main_service.stock.sh"
U="$EO/update_pending"
REBOOT="${REBOOT:-/sbin/reboot}"
UMOUNT="${UMOUNT:-umount}"
update_trial=0
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH

log() { echo "[dispatch] $*" >> "$RUN/ocbm_dispatch.log"; echo "[dispatch] $*" >> "$RUN/box.log"; }
stick_in() { [ -d "$STICK" ] && grep -q " $STICK " "$PROC/mounts" 2>/dev/null; }

deliver() {  # $1 a report in /etc/ocbm, onto the next stick plugged in; $2 share: then hand sticks back to stock
  while ! stick_in; do sleep "$POLL"; done
  sleep "$POLL"   # let update_box.sh look at the stick first
  r="$STICK/OCBM_RESULT.txt"
  # Every step chained: on a full stick the write fails, and moving on regardless left an empty
  # OCBM_RESULT.txt (an older report on it lost too) and the report counted delivered (the Mac, 3 Nov).
  # A failed write keeps the report pending for the next start.
  if [ -f "$r" ]; then
    { cat "$1" && echo && echo "--- before that" && cat "$r"; } > "$r.new"
  else
    cp "$1" "$r.new"
  fi && mv -f "$r.new" "$r" && sync && mv -f "$1" "$EO/delivered_result.txt" || {
    rm -f "$r.new"
    log "could not write the report to the stick (full?): it is tried again at the next start"
  }
  sync
  # Done with this stick: let it go before stock gets it. The truck, 3 Nov: the report was delivered and
  # synced, but the stick stayed mounted read-write under a stock that cycled for 40 s until the adapter
  # was unplugged, and its FAT came back inconsistent (OCBM_RESULT.txt starting on a free cluster).
  # Unmounted, or read-only if something holds it, nothing on the adapter can write to it again.
  if $UMOUNT "$STICK" 2>/dev/null || mount -o remount,ro "$STICK" 2>/dev/null; then
    log "the report is on the stick; the stick is unmounted or read-only now"
  fi
  [ "${2:-}" = share ] && rm -f "$RUN/not_change_mass_storage"   # later sticks are shared with the car again, as stock does
}

stock() {
  if [ -s "$SMS_STOCK" ]; then
    if [ -f "$EO/pending_result.txt" ]; then
      touch "$RUN/not_change_mass_storage"   # keep the next stick mounted until the report is on it
      deliver "$EO/pending_result.txt" share </dev/null >/dev/null 2>&1 &
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
  stick_outcome UPDATED
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
  stick_outcome "ROLLED BACK"
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

# A host that talks to ocbmd and never hears back. The truck adapter, 2 Nov: its accessory driver let
# OCBM read and never write, the self-test passed with no host to ask, and the adapter sat on a dead
# trial. ocbmd writes the uptime of a host's first HELLO to ocbmd_heard, and of its first other frame,
# which a host sends only once it has our HELLO_ACK, to ocbmd_answered. Both hold for the boot: the
# answer can reach an ocbmd restarted since the HELLO (the truck, 2 Nov 21:06). Heard for ANSWER_SECS
# with no answer goes back as a failed self-test does: to stock, or in an update's trial to the OCBM it
# replaced.
answer_watch() {
  while :; do
    [ -e "$RUN/ocbmd_answered" ] && return
    heard=$(cat "$RUN/ocbmd_heard" 2>/dev/null)
    up=$(cut -d. -f1 "$PROC/uptime" 2>/dev/null)
    case "$heard" in ''|*[!0-9]*) heard="" ;; esac
    case "$up" in ''|*[!0-9]*) up="" ;; esac
    # Asked again last: an answer written since the first look keeps the trial.
    if [ -n "$heard" ] && [ -n "$up" ] && [ $((up - heard)) -ge "$ANSWER_SECS" ] && [ ! -e "$RUN/ocbmd_answered" ]; then
      why="ocbmd heard the host for $((up - heard)) s and never answered it"
      log "$why"
      if [ "$update_trial" = 1 ]; then
        update_rollback "$why"
        sync; $REBOOT
        return
      fi
      "$S/ocbm_revert.sh" "$why"
      return
    fi
    sleep "$POLL"
  done
}

final_note() {  # OCBM_RESTORE.txt on the stick: what OCBM_RESTORE does on a final adapter, put in after its start
  if [ -x "$S/ocbm_restore.sh" ]; then
    { echo "OCBM_RESTORE was not used yet: on this adapter it is used when the adapter starts."
      echo
      echo "Stock CarPlay was removed, so the way back to stock is the flash backup the first install wrote"
      echo "to its stick (OCBM_BACKUP/<adapter>). With that stick in, unplug the adapter and plug it back in"
      echo "(or turn the car off and on): OCBM restores stock from the backup, then restarts on stock."
    } > "$STICK/OCBM_RESTORE.txt" && sync
  else
    { echo "OCBM_RESTORE was not used: OCBM on this adapter is final."
      echo
      echo "Stock CarPlay was removed, and the Carlinkit files with it, so there is no stock start to go"
      echo "back to. The way back to stock is the flash backup the first install wrote to its stick"
      echo "(OCBM_BACKUP/<adapter>), put back with tools/stock_restore.sh: see docs/ops/01_RECOVERY.md."
    } > "$STICK/OCBM_RESTORE.txt" && sync
  fi
  log "OCBM_RESTORE on the stick after the start, and the mode is final: left OCBM_RESTORE.txt saying what to do"
}

# OCBM_RESTORE on a stick: back to stock. The mode is read on every look, and once it is final the file
# is left as it is, with a note beside it: dev adapter, 3 Nov, the watcher of the boot the finish ran in
# outlived the finish, used the driver's OCBM_RESTORE up, and the revert refused ("OCBM is final") with
# nothing on the stick to say why. A final boot runs it for the note alone.
restore_watch() {
  while :; do
    if stick_in && [ -e "$STICK/OCBM_RESTORE" ]; then
      if [ "$(cat "$S/ocbm_mode" 2>/dev/null)" = final ]; then
        [ -e "$STICK/OCBM_RESTORE.txt" ] || final_note
      else
        mv -f "$STICK/OCBM_RESTORE" "$STICK/OCBM_RESTORE.done"; sync   # one use: a later install is not undone
        log "OCBM_RESTORE found on the stick"
        [ -f "$U" ] && update_rollback "a file named OCBM_RESTORE was found on a USB stick"   # stock gets the OCBM it had
        "$S/ocbm_revert.sh" "a file named OCBM_RESTORE was found on a USB stick"
        return
      fi
    fi
    sleep "$POLL"
  done
}

# ---- an update from a USB stick (offline: no internet on the head unit and no computer; Scott, 3 Nov) ----
# At a trial or final start only, never mid-session, since an update restarts the adapter: a stick with
# one ocbm-update-X.Y.Z.tgz and its .sha256 (tools/make_ocbm_update.sh) goes to ocbm_update.sh as ocbmd
# hands it a package on MGMT_UPDATE, with the same checks and the same trial. The package is used once
# (renamed .used, or .refused), and the outcome goes onto the stick as OCBM_RESULT.txt: at once for a
# refusal, after the update's trial for a commit or a rollback. stick_result.txt holds the report until
# a stick takes it, across the restart a rollback makes.
stick_report() {  # $1 headline, $2 the line under it
  { echo "OCBM update: $1"; echo "adapter: OCBM $(cat "$EO/installed_version" 2>/dev/null)"; echo; echo "$2"; echo
    echo "No update is guaranteed to succeed, least of all one the OCBM project did not build. Its .sha256"
    echo "only catches a damaged copy; what you put on your adapter is your choice (MIGRATING.md)."; } \
    > "$EO/stick_result.txt.new" && mv -f "$EO/stick_result.txt.new" "$EO/stick_result.txt"
  sync
  delivering=1
  deliver "$EO/stick_result.txt" </dev/null >/dev/null 2>&1 &
}

stick_outcome() {  # $1 headline: an update from the stick ended its trial
  [ -f "$EO/stick_update" ] || return 0
  rm -f "$EO/stick_update"; sync
  stick_report "$1" "$(cat "$EO/update_result" 2>/dev/null)"
}

stick_update() {
  i=0
  while ! stick_in && [ "$i" -lt "$STICK_PKG_WAIT" ]; do sleep "$POLL"; i=$((i + POLL)); done
  stick_in || return 0
  if [ -e "$STICK/OCBM_RESTORE" ]; then   # the way back goes first: in final from the stick's own backup,
    [ "$mode" = final ] && [ -x "$S/ocbm_restore.sh" ] && "$S/ocbm_restore.sh" </dev/null >/dev/null 2>&1   # it logs to box.log itself
    return 0                              # in a trial by restore_watch's revert
  fi
  set -- "$STICK"/ocbm-update-*.tgz
  [ -f "$1" ] || return 0
  if [ $# -gt 1 ]; then
    stick_report REFUSED "There is more than one OCBM update package on the stick. Leave only the newest, with its .sha256, and restart the adapter."
    return 0
  fi
  pkg=$1; n=${pkg##*/}
  want=$(cut -c1-64 "$pkg.sha256" 2>/dev/null)
  if [ -z "$want" ] || [ "$(sha256sum "$pkg" 2>/dev/null | cut -c1-64)" != "$want" ]; then
    mv -f "$pkg" "$pkg.refused"; sync
    stick_report REFUSED "$n does not match $n.sha256, or has none, so it was not used. Copy both files to the stick again."
    return 0
  fi
  { cp "$pkg" "$RUN/ocbm-update.tgz" && mv -f "$pkg" "$pkg.used" && sync; } || { log "could not take $n off the stick"; return 0; }
  log "update package on the stick: $n"
  echo "$n" > "$EO/stick_update"; sync
  "$S/ocbm_update.sh" "$RUN/ocbm-update.tgz" >/dev/null 2>&1   # it logs to box.log itself (twice, before 3 Nov)
  [ -f "$U" ] && return 0   # placed: the adapter restarts into the update's trial, and its outcome is reported then
  rm -f "$EO/stick_update"
  r=$(cat "$EO/update_result" 2>/dev/null)
  case "$r" in
    committed*) stick_report UPDATED "$r" ;;
    *)
      mv -f "$STICK/$n.used" "$STICK/$n.refused"; sync
      case "$mode:$r" in
        trial:*"Not enough flash"*)
          r="$r In a trial the flash holds stock CarPlay too: remove stock CarPlay in Carlink first (Settings > Adapter), or go back to stock and install the full image." ;;
      esac
      stick_report REFUSED "$r" ;;
  esac
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
# A stick report a restart interrupted; then what a stick in at this start asks for (an update, or in
# final the way back to stock); then, in final, a note on a stick that asks for stock later.
stick_job=0
if [ -f "$EO/stick_result.txt" ]; then   # one report at a time: a new package waits for the next start
  [ -n "${delivering:-}" ] || deliver "$EO/stick_result.txt" </dev/null >/dev/null 2>&1 &
elif [ "$update_trial" = 0 ] && [ "$mode" != confirmed ] && [ ! -f "$U" ]; then
  stick_job=1
fi
if [ "$mode" = final ]; then
  [ -e "$EO/finish_pending" ] && [ -x "$S/ocbm_finish.sh" ] && "$S/ocbm_finish.sh" </dev/null >> "$RUN/box.log" 2>&1 &
  if [ "$update_trial" = 1 ]; then
    selftest </dev/null >/dev/null 2>&1 &
    answer_watch </dev/null >/dev/null 2>&1 &
  else
    { [ "$stick_job" = 1 ] && stick_update; restore_watch; } </dev/null >/dev/null 2>&1 &
  fi
  exit 0
fi
[ "$stick_job" = 1 ] && { stick_update </dev/null >/dev/null 2>&1 & }
selftest </dev/null >/dev/null 2>&1 &
answer_watch </dev/null >/dev/null 2>&1 &
restore_watch </dev/null >/dev/null 2>&1 &
exit 0
