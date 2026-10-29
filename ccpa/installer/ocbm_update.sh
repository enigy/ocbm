#!/bin/sh
# ocbm_update.sh — update an adapter that already runs OCBM (a one-file install, in its trial or
# final) to a newer OCBM, from a package the app pushed over CH_FILE (ocbm-update-X.Y.Z.tgz, built by
# tools/make_ocbm_update.sh). ocbmd runs it on MGMT_UPDATE once the package's SHA-256 matches the one
# the app sent. The next start is the update's trial (ocbm_dispatch.sh): the new version is kept once
# it passes its start-up self-test, and the version it replaced is put back otherwise.
#
#   ocbm_update.sh PACKAGE
#
#   1 unpack   the package into $WORK
#   2 checks   refuse, changing nothing: the package's own sums; product, chip and factory mode as
#              once.sh checks them; the mode (trial or final: a confirmed trial finishes when its
#              session ends, so it waits); a version newer than installed_version and at least the
#              package's FROM_MIN; no update already pending; every file the new version needs, in
#              the package or already on the adapter as it should be; the new start script parses;
#              and flash room for the files that change plus RESERVE_KB. The files they replace stay
#              where they are (renamed .prev), so a rollback needs no room of its own.
#   3 place    each changed file as <path>.new, checked against its sum. Running out of flash here is
#              a refusal: the .new files go and nothing else has changed.
#   4 journal  /etc/ocbm/update_pending: the two versions and every file, written .new + rename
#   5 swap     each file: the old one to <path>.prev, the new one into place, the start script last;
#              then "placed" in the journal
#   6 restart  into the update's trial
#
# A package carries the new version's sums for every file (SHA256SUMS) but only the files that
# changed since FROM_MIN (files/). An adapter between FROM_MIN and the new version already has the
# rest; one that has neither is refused, never left half updated.
#
# Power lost before the journal leaves the old version and stray .new files (the next run removes
# them). After it, the dispatcher finishes the swap's undo at the next start. Every outcome is one
# line in /etc/ocbm/update_result, which ocbmd reports in MGMT_INFO ("update", "update_result").
#
# Exit 0: placed, restarting (or nothing to change). Exit 1: refused, nothing changed.
#
# Test hooks (tools/ocbm_update_test.sh): ROOT SYS PROC RUN WORK REBOOT RESERVE_KB FAKE_FREE_KB, and
# STOP_AT=<step> to simulate power loss (exit 99) right before that step; STOP_AT=swap-mid stops
# between the two renames of the first file swapped.
ROOT="${ROOT:-}"
SYS="${SYS:-/sys}"
PROC="${PROC:-/proc}"
RUN="${RUN:-/tmp}"
WORK="${WORK:-$RUN/ocbm-update}"
REBOOT="${REBOOT:-/sbin/reboot}"
RESERVE_KB="${RESERVE_KB:-384}"
WANT_PRODUCT=A15W
CHIPS="0x9159 0xc822"
S="$ROOT/script"
EO="$ROOT/etc/ocbm"
MODE="$S/ocbm_mode"
U="$EO/update_pending"
R="$EO/update_result"
PKG="${1:-}"
VER=""

log() { echo "[update] $*"; echo "[update] $*" >> "$RUN/box.log"; }
step() {
  [ "${STOP_AT:-}" = "$1" ] && { log "simulated power loss before $1"; exit 99; }
  log "== $1"
}
result() { { echo "$1" > "$R.new" && mv -f "$R.new" "$R"; } 2>/dev/null; sync; }

# The same lock as ocbm_revert.sh and ocbm_finish.sh: an update, a go-back and a finish never interleave.
mkdir -p "$RUN"
mkdir "$RUN/ocbm_mode.lock" 2>/dev/null || { log "refused: a go-back, the finish or another update is running"; exit 1; }
unlock() { rmdir "$RUN/ocbm_mode.lock" 2>/dev/null; }
clean() {  # what this run (or one a power cut stopped before its journal) left behind
  [ -f "$U" ] || find "$ROOT/script" "$ROOT/usr/sbin" "$ROOT/usr/bin" -name '*.new' -type f 2>/dev/null \
    | while read -r f; do rm -f "$f"; done
  rm -rf "$WORK"
}
refuse() {
  log "REFUSED: $1"
  result "refused ${VER:-?}: $1"
  clean
  unlock
  exit 1
}

target() {  # where a package path lives on the adapter: once.sh installs the dispatcher as the start script
  case "$1" in script/ocbm_dispatch.sh) echo script/start_main_service.sh ;; *) echo "$1" ;; esac
}
sum_of() { sha256sum "$1" 2>/dev/null | cut -c1-64; }
free_kb() { df -k "$1" 2>/dev/null | awk 'NR>1 {f=$4} END {print f+0}'; }
kb_on_flash() { s=$(du -k "$1" 2>/dev/null | cut -f1); echo $(( (${s:-0} * 7 + 9) / 10 )); }  # JFFS2 holds OCBM's files at ~0.62-0.8 of their size
first() { set -- $1; echo "${1:-}"; }
# Is version $1 newer than $2? X.Y.Z compared as numbers; a -suffix (1.1.0-beta.1) ranks below 1.1.0.
newer() {
  awk -v a="$1" -v b="$2" '
    function key(v,   i, p, n, x, k) {
      i = index(v, "-"); p = ""
      if (i) { p = substr(v, i + 1); v = substr(v, 1, i - 1) }
      n = split(v, x, "."); k = ""
      for (i = 1; i <= 3; i++) k = k sprintf("%06d", (i <= n ? x[i] : 0) + 0)
      return k (p == "" ? "~" : "-" p)
    }
    BEGIN { exit !(key(a) > key(b)) }'
}

# ---------------------------------------------------------------------------------------------
step unpack
[ -n "$PKG" ] && [ -f "$PKG" ] || refuse "The update package is missing."
rm -rf "$WORK"
mkdir -p "$WORK" && gzip -dc "$PKG" 2>/dev/null | (cd "$WORK" && tar -xf -) \
  || refuse "The update package could not be unpacked: it is damaged."

step checks
VER=$(head -1 "$WORK/VERSION" 2>/dev/null)
new=$(first "$VER")
from_min=$(head -1 "$WORK/FROM_MIN" 2>/dev/null)
[ -n "$new" ] || refuse "The update package has no VERSION: it is damaged."
[ -n "$from_min" ] || refuse "The update package has no FROM_MIN: it is damaged."
[ -s "$WORK/SHA256SUMS" ] || refuse "The update package has no SHA256SUMS: it is damaged."
bad=""
while read -r want rel; do
  [ -e "$WORK/files/$rel" ] || continue
  [ "$(sum_of "$WORK/files/$rel")" = "$want" ] || bad="$bad /$rel"
done < "$WORK/SHA256SUMS"
[ -z "$bad" ] || refuse "The update package's files do not match their checksums ($bad): it is damaged."

product=$(cat "$ROOT/etc/box_product_type" 2>/dev/null)
[ "$product" = "$WANT_PRODUCT" ] || refuse "This adapter is a '${product:-unknown}', not an $WANT_PRODUCT (CPC200-CCPA)."
chips=""
for d in "$SYS"/bus/sdio/devices/*; do [ -r "$d/device" ] && chips="$chips $(cat "$d/device")"; done
chip_ok=0
for c in $chips; do case " $CHIPS " in *" $c "*) chip_ok=1 ;; esac; done
[ "$chip_ok" = 1 ] || refuse "The Wi-Fi/Bluetooth chip (${chips:- none found}) is not one OCBM supports yet ($CHIPS)."
mfg=$(cat "$SYS/module/libcomposite/parameters/mfgmode" 2>/dev/null)
[ "${mfg:-0}" = 0 ] || refuse "The adapter is in factory mode ${mfg}."

mode=$(cat "$MODE" 2>/dev/null)
case "$mode" in
  trial|final) ;;
  confirmed) refuse "The adapter finishes its install when this drive ends. Update it after that." ;;
  '') refuse "This adapter does not run a one-file OCBM install." ;;
  *) refuse "The adapter's install mode '$mode' is not one an update knows." ;;
esac
[ -f "$U" ] && refuse "An update is already in its trial on this adapter."
have_line=$(head -1 "$EO/installed_version" 2>/dev/null)
have=$(first "$have_line")
[ -n "$have" ] || refuse "The adapter does not say which OCBM it runs (/etc/ocbm/installed_version)."
newer "$new" "$have" || refuse "The adapter already runs OCBM $have; this package is $new."
newer "$from_min" "$have" && refuse "This package updates OCBM $from_min or newer; the adapter runs $have."

# What changes: every file of the new version that the adapter does not already have as it should.
list="$WORK/changed"
: > "$list"
need_kb=0
while read -r want rel; do
  t=$(target "$rel")
  [ "$(sum_of "$ROOT/$t")" = "$want" ] && continue
  [ -e "$WORK/files/$rel" ] || refuse "The package lacks /$t, which this adapter needs: it updates from OCBM $from_min, and the adapter runs $have."
  echo "$want $rel" >> "$list"
  need_kb=$((need_kb + $(kb_on_flash "$WORK/files/$rel")))
done < "$WORK/SHA256SUMS"
if [ ! -s "$list" ]; then   # a new version number with no new files
  { echo "$VER" > "$EO/installed_version.new" && mv -f "$EO/installed_version.new" "$EO/installed_version"; } \
    || refuse "Could not write /etc/ocbm/installed_version."
  result "committed $VER (from $have_line): no files changed"
  log "nothing to place: the adapter's files already match $VER"
  clean; unlock; exit 0
fi
if grep -q ' script/ocbm_dispatch.sh$' "$list"; then
  sh -n "$WORK/files/script/ocbm_dispatch.sh" 2>/dev/null \
    || refuse "The package's start script does not parse: it is damaged."
fi
free=${FAKE_FREE_KB:-$(free_kb "$ROOT/")}
log "updating $have_line to $VER: $(wc -l < "$list") files, ~$need_kb KB on flash; $free KB free, $RESERVE_KB KB kept spare"
[ "$free" -ge $((need_kb + RESERVE_KB)) ] \
  || refuse "Not enough flash: $free KB free, and the update needs about $need_kb KB plus $RESERVE_KB KB spare. The files it replaces stay until it passes its self-test, so it can always be undone."

step place
while read -r want rel; do
  t=$(target "$rel")
  mkdir -p "$(dirname "$ROOT/$t")"
  # Every payload file is a program or a script: 755, owned by whoever runs this (root on the adapter).
  { cp "$WORK/files/$rel" "$ROOT/$t.new" && chmod 755 "$ROOT/$t.new" && [ "$(sum_of "$ROOT/$t.new")" = "$want" ]; } 2>/dev/null \
    || refuse "Could not write /$t (the flash may be full). Nothing was changed."
done < "$list"
sync

step journal
{ echo "journal=1"; echo "from=$have_line"; echo "to=$VER"
  grep -v ' script/ocbm_dispatch.sh$' "$list" | while read -r want rel; do
    t=$(target "$rel"); if [ -e "$ROOT/$t" ]; then echo "replace $t"; else echo "add $t"; fi
  done
  if grep -q ' script/ocbm_dispatch.sh$' "$list"; then echo "replace script/start_main_service.sh"; fi   # boot-critical: last
} > "$U.new" && mv -f "$U.new" "$U" && sync || refuse "Could not write the update journal."

step swap
first_swap=1
awk '$1 == "replace" || $1 == "add" { print $2 }' "$U" | while read -r t; do
  f="$ROOT/$t"
  if [ -e "$f" ]; then mv -f "$f" "$f.prev" || exit 98; fi
  if [ "${STOP_AT:-}" = swap-mid ] && [ "$first_swap" = 1 ]; then log "simulated power loss in the swap"; exit 99; fi
  first_swap=0
  mv -f "$f.new" "$f" || exit 98
done
rc=$?
[ "$rc" = 99 ] && exit 99
if [ "$rc" != 0 ]; then   # no "placed" in the journal: the next start undoes what was swapped
  log "a file could not be swapped; restarting so the dispatcher puts $have_line back"
  result "trial $VER: a file could not be swapped, and the next start undoes the update"
  sync; unlock; $REBOOT; exit 1
fi
echo placed >> "$U" && sync

step restart
result "trial $VER (from $have_line)"
log "placed $VER; restarting into its trial"
rm -rf "$WORK" "$PKG"
sync
unlock
$REBOOT
exit 0
