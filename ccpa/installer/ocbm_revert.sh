#!/bin/sh
# ocbm_revert.sh — send an adapter in its OCBM trial back to stock Carlinkit, then restart.
#
#   ocbm_revert.sh [--no-reboot] REASON
#
# Called by the dispatcher (a failed self-test, OCBM_RESTORE on a stick, and with --no-reboot at
# boot for the start counter, when it hands the boot to stock itself), and later by ocbmd for
# Carlink3's "Go back to stock" (M4). Removing /script/ocbm_mode is the switch: everything before it
# is safe to repeat, and the dispatcher then hands every boot to start_main_service.stock.sh.
#
#   1 put back /etc/hostapd.conf from /etc/ocbm/identity.tgz: OCBM rewrites the access point's
#     name, passphrase and interface there, and the stock Wi-Fi script rewrites only some of them
#   2 /etc/ocbm/last_result.txt gets the reason and the line "reverted version=<VERSION>", which
#     once.sh's same-version guard reads
#   3 /etc/ocbm/pending_result.txt, the report the dispatcher copies to the next stick plugged in
#   4 remove /script/ocbm_mode, then the start counter
#
# OCBM's own files stay on flash, and the extras the install removed stay removed.
# Exit 0: on stock now (or already was). Exit 1: refused, nothing changed (mode final: the Carlinkit
# stack is gone; start_main_service.stock.sh is missing; or ocbm_finish.sh holds the lock).
#
# Test hooks (tools/ocbm_stageb_test.sh): ROOT RUN REBOOT
ROOT="${ROOT:-}"
RUN="${RUN:-/tmp}"
REBOOT="${REBOOT:-/sbin/reboot}"
S="$ROOT/script"
EO="$ROOT/etc/ocbm"
MODE="$S/ocbm_mode"
restart=1
[ "${1:-}" = --no-reboot ] && { restart=0; shift; }
why="${1:-no reason given}"

log() { echo "[revert] $*"; echo "[revert] $*" >> "$RUN/box.log"; }
# The same lock as ocbm_finish.sh: a go-back and a finish never interleave.
mkdir "$RUN/ocbm_mode.lock" 2>/dev/null || { log "refused: another revert, or the finish, is running"; exit 1; }
done_() { rmdir "$RUN/ocbm_mode.lock" 2>/dev/null; exit "$1"; }

mode=$(cat "$MODE" 2>/dev/null)
case "$mode" in
  '') log "already on stock"; done_ 0 ;;
  final) log "refused: OCBM is final and the Carlinkit files are gone"; done_ 1 ;;
esac
[ -s "$S/start_main_service.stock.sh" ] || { log "refused: start_main_service.stock.sh is missing"; done_ 1; }
ver=$(cat "$EO/installed_version" 2>/dev/null)
up=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
log "going back to stock from $mode (${ver:-unknown version}): $why"

t="$RUN/ocbm_revert.$$"
mkdir -p "$t"
if gzip -dc "$EO/identity.tgz" 2>/dev/null | (cd "$t" && tar -xf - etc/hostapd.conf 2>/dev/null) \
   && [ -s "$t/etc/hostapd.conf" ]; then
  cp "$t/etc/hostapd.conf" "$ROOT/etc/hostapd.conf.new" && mv -f "$ROOT/etc/hostapd.conf.new" "$ROOT/etc/hostapd.conf" \
    && log "put back /etc/hostapd.conf"
fi
rm -rf "$t"   # it held the Wi-Fi passphrase

{ echo "uptime ${up:-?}s, mode $mode: $why"; echo "reverted version=$ver"; } >> "$EO/last_result.txt"
{
  echo "OCBM install: BACK TO STOCK"
  echo "installer: ${ver:-unknown}"
  echo
  echo "The adapter went back to stock CarPlay: $why."
  echo "Stock CarPlay and Android Auto work as before. HiCar, the Android Mirror helper and the web"
  echo "pages stay removed; Carlinkit's own 2025.10.15.1127 image puts them back."
  echo "To try OCBM again, put the installer on the stick together with an empty file named OCBM_RETRY."
  echo
  echo "--- self-test"; cat "$RUN/ocbm_selftest.txt" 2>/dev/null
  echo; echo "--- start log"; cat "$RUN/ocbm_dispatch.log" 2>/dev/null
  echo; echo "--- box.log, last 60 lines"; tail -n 60 "$RUN/box.log" 2>/dev/null
} > "$EO/pending_result.txt.new" && mv -f "$EO/pending_result.txt.new" "$EO/pending_result.txt"
sync

rm -f "$MODE"; sync
[ -e "$MODE" ] && { log "could not remove /script/ocbm_mode"; done_ 1; }
rm -f "$EO/boot_attempts"; sync
log "on stock from the next start"
[ "$restart" = 1 ] && { sync; $REBOOT; }
done_ 0
