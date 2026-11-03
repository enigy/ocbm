#!/bin/sh
# ocbm_restore.sh — back to stock from the USB stick, on the adapter: no computer and no internet
# (Scott, 3 Nov). The source is the flash backup the first one-file install wrote to that stick before
# it changed anything (OCBM_BACKUP/<adapter>/mtd0.bin mtd1.bin mtd2.bin + SHA256SUMS), i.e. the
# factory rootfs. ocbm_dispatch.sh runs this at a final start when the stick holds OCBM_RESTORE (a
# trial keeps its revert, ocbm_revert.sh: stock is still on it). The same steps as tools/stock_restore.sh
# --profile onefile --dump, with the decoding done here instead of on a computer:
#
#   1 checks   refuse, changing nothing: the backup for THIS adapter (its id, as once.sh named it), its
#              SHA256SUMS verifying; mtd0 and mtd1 byte for byte as the backup's (a backup of another
#              adapter is never applied); the install final with no finish pending; 30 MB free in /tmp
#   2 decode   mtd2.bin into /tmp (RAM) with `ocbmd jffs2-extract`, owners as on the flash: it must be
#              a factory rootfs (stock start script, no OCBM) whose software_version is this adapter's
#   3 restore  tools/stock_restore_box.sh (installed as /script/stock_restore_box.sh), PROFILE=onefile:
#              plan, apply, verify. Bootable at every step; until its one rename the adapter starts OCBM
#   4 report   OCBM_RESULT.txt on the stick, OCBM_RESTORE renamed .done, the stick let go, restart
#
# mtd0 (U-Boot) and mtd1 (the kernel) are read, never written: the boot stack is fixed platform firmware.
# Power lost before apply's switch: OCBM_RESTORE is still on the stick and the next start runs this again
# (apply is idempotent). Lost after it: the adapter starts stock, and OCBM's files may be left, unused.
# Any other failure renames OCBM_RESTORE to OCBM_RESTORE.refused, with the reason on the stick: rename
# it back to try again.
#
# Exit 0: back to stock, restarting. Exit 1: refused or stopped (OCBM still runs).
# Test hooks (tools/stock_restore_test.sh): ROOT SYS DEVDIR RUN STICK REBOOT UMOUNT OCBMD ENGINE
# TMP_MIN_KB, and the engine's STOP_AT=switch, a power cut right before its switch (exit 99).
ROOT="${ROOT:-}"
SYS="${SYS:-/sys}"
PROC="${PROC:-/proc}"
DEVDIR="${DEVDIR:-/dev}"
RUN="${RUN:-/tmp}"
STICK="${STICK:-/mnt/UPAN}"
REBOOT="${REBOOT:-/sbin/reboot}"
UMOUNT="${UMOUNT:-umount}"
OCBMD="${OCBMD:-$ROOT/usr/sbin/ocbmd}"
ENGINE="${ENGINE:-$ROOT/script/stock_restore_box.sh}"
TMP_MIN_KB="${TMP_MIN_KB:-30000}"
S="$ROOT/script"
EO="$ROOT/etc/ocbm"
X="$RUN/ocbm-restore"   # the decoded backup: it holds the Wi-Fi passphrase and Bluetooth keys, RAM only

log() { echo "[restore] $*"; echo "[restore] $*" >> "$RUN/box.log"; }
report() {  # $1 headline, then lines: OCBM_RESULT.txt on the stick, the one before kept below it
  r="$STICK/OCBM_RESULT.txt"
  { echo "OCBM restore: $1"; shift; echo; for l in "$@"; do echo "$l"; done
    if [ -f "$r" ]; then echo; echo "--- before that"; cat "$r"; fi; } > "$r.new" && mv -f "$r.new" "$r"
  sync
}
let_go() { sync; $UMOUNT "$STICK" 2>/dev/null || mount -o remount,ro "$STICK" 2>/dev/null; }
refuse() {  # nothing changed
  log "REFUSED: $1"
  rm -rf "$X"
  mv -f "$STICK/OCBM_RESTORE" "$STICK/OCBM_RESTORE.refused" 2>/dev/null
  report REFUSED "$1" "" "Nothing on the adapter was changed: it still runs OCBM." \
    "To try again, rename OCBM_RESTORE.refused on this stick back to OCBM_RESTORE and restart the adapter with the stick in."
  let_go
  exit 1
}
stopped() {  # apply ran part way: OCBM still starts, apply can run again
  log "STOPPED: $1"
  rm -rf "$X"
  mv -f "$STICK/OCBM_RESTORE" "$STICK/OCBM_RESTORE.refused" 2>/dev/null
  report STOPPED "$1" "" "The adapter still starts OCBM. Running the restore again picks up where this one stopped:" \
    "rename OCBM_RESTORE.refused on this stick back to OCBM_RESTORE and restart the adapter with the stick in." \
    "" "--- the restore's log" "$(cat "$RUN/ocbm_restore_apply.txt" "$RUN/ocbm_restore_verify.txt" 2>/dev/null | tail -40)"
  let_go
  exit 1
}

# ---- 1 checks -----------------------------------------------------------------------------------
log "OCBM_RESTORE on the stick: back to stock from its flash backup"
id=$(tr -cd 'A-Za-z0-9' < "$ROOT/etc/device_serial" 2>/dev/null | cut -c1-32)
[ -n "$id" ] || id=$(tr -cd '0-9A-Fa-f' < "$SYS/class/net/wlan0/address" 2>/dev/null)
[ -n "$id" ] || id=adapter
B="$STICK/OCBM_BACKUP/$id"
[ -d "$B" ] || refuse "This stick has no flash backup of this adapter (OCBM_BACKUP/$id). Use the stick the adapter's first install was made from."
for f in mtd0.bin mtd1.bin mtd2.bin SHA256SUMS; do
  [ -s "$B/$f" ] || refuse "The backup OCBM_BACKUP/$id has no $f."
done
[ "$(grep -cE '^[0-9a-f]{64}  mtd[012]\.bin$' "$B/SHA256SUMS")" = 3 ] && (cd "$B" && sha256sum -c SHA256SUMS >/dev/null 2>&1) \
  || refuse "The backup OCBM_BACKUP/$id does not match its SHA256SUMS: it is damaged. Restore from a computer copy with tools/stock_restore.sh."
for n in 0 1; do
  cmp -s "$DEVDIR/mtd$n" "$B/mtd$n.bin" \
    || refuse "This adapter's mtd$n differs from the backup's: OCBM_BACKUP/$id is not this adapter's flash, and is never applied to it."
done
log "the backup verifies, and mtd0 and mtd1 match it byte for byte"
[ "$(cat "$S/ocbm_mode" 2>/dev/null)" = final ] || refuse "The install is not final: stock is still on the adapter, so Go back in Carlink (or OCBM_RESTORE during a trial) takes it back."
[ -e "$EO/finish_pending" ] && refuse "Removing stock CarPlay was interrupted and is still finishing. Restart the adapter, let it finish, then try again."
[ -x "$OCBMD" ] && [ -s "$ENGINE" ] || refuse "This OCBM lacks the restore's own tools (ocbmd, stock_restore_box.sh). Update OCBM first, or restore from a computer with tools/stock_restore.sh."
free=$(df -k "$RUN" 2>/dev/null | awk 'NR>1 {f=$4} END {print f+0}')
[ "${free:-0}" -ge "$TMP_MIN_KB" ] || refuse "Only ${free:-?} KB of RAM is free for the decoded backup; it needs $TMP_MIN_KB KB."

# ---- 2 decode -----------------------------------------------------------------------------------
rm -rf "$X"
"$OCBMD" jffs2-extract "$B/mtd2.bin" "$X" >> "$RUN/box.log" 2>&1 \
  || refuse "The flash backup did not decode cleanly (see /tmp/box.log), so it is not restored from."
T="$X/files"
grep -q '^# OCBM_DISPATCH' "$T/script/start_main_service.sh" 2>/dev/null || [ -e "$T/usr/sbin/ocbmd" ] \
  && refuse "The flash backup holds OCBM: it was taken by a later install, not before the first one. Restore from a computer with tools/stock_restore.sh."
[ -s "$T/script/start_main_service.sh" ] || refuse "The flash backup has no stock start script."
want=$(head -1 "$T/etc/software_version" 2>/dev/null | tr -d '\r')
have=$(head -1 "$ROOT/etc/software_version" 2>/dev/null | tr -d '\r')
[ -n "$want" ] && [ "$want" = "$have" ] || refuse "The backup's firmware is '${want:-missing}' and this adapter's is '${have:-missing}'."
for d in etc/boa/www etc/boa/cgi-bin etc/boa/images etc/hichain; do   # as stock_restore_onefile_tree.sh makes them
  [ -d "$T/$d" ] || { mkdir -p "$T/$d" && chmod 775 "$T/$d" && chown 1000:1000 "$T/$d" 2>/dev/null; }
done
log "decoded: the factory rootfs, $want"

# ---- 3 restore ----------------------------------------------------------------------------------
export PROFILE=onefile SRC="$T" LINKS="$X/links.tsv" ROOT="${ROOT:-/}" RUN WORK="$RUN/ocbm-restore-work"
sh "$ENGINE" plan > "$RUN/ocbm_restore_plan.txt" 2>&1
grep -q RESTORE_PLAN_OK "$RUN/ocbm_restore_plan.txt" \
  || refuse "The restore's plan refused ($(grep -m1 'ABORT' "$RUN/ocbm_restore_plan.txt" | sed 's/^\[restore\] //'))."
log "plan: $(grep -c . "$RUN/ocbm_restore_plan.txt") lines; applying"
sh "$ENGINE" apply > "$RUN/ocbm_restore_apply.txt" 2>&1; rc=$?
cat "$RUN/ocbm_restore_apply.txt" >> "$RUN/box.log"
[ "$rc" = 99 ] && exit 99   # the engine's STOP_AT test hook: a power cut, OCBM_RESTORE left for the next start
grep -q RESTORE_DONE "$RUN/ocbm_restore_apply.txt" \
  || stopped "The restore did not finish: $(grep -m1 'ABORT' "$RUN/ocbm_restore_apply.txt" | sed 's/^\[restore\] //')"
sh "$ENGINE" verify > "$RUN/ocbm_restore_verify.txt" 2>&1
grep -q RESTORE_VERIFIED "$RUN/ocbm_restore_verify.txt" \
  || stopped "The rootfs does not match the backup yet ($(grep -c . "$RUN/ocbm_restore_verify.txt") lines in the check)."
# OCBM's bond store: the OCBM still running may have written it again since apply removed it.
[ -e "$T/etc/carplay" ] || rm -rf "${ROOT%/}/etc/carplay"
rm -rf "$X"

# ---- 4 report -----------------------------------------------------------------------------------
mv -f "$STICK/OCBM_RESTORE" "$STICK/OCBM_RESTORE.done" 2>/dev/null
report "BACK TO STOCK" "The adapter is back to the stock Carlinkit firmware ($want) from its own flash backup," \
  "and the rootfs matches the backup. It restarts on stock CarPlay now." "" \
  "To use OCBM again, install it from a stick with the OCBM installer image (A15W_Update.img)."
log "back to stock: the rootfs matches the backup; restarting"
let_go
sync; $REBOOT
exit 0
