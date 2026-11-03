#!/bin/sh
# once.sh — Stage A of the one-file OCBM install. Ships as tmp/once.sh in A15W_Update.img
# (tools/make_ocbm_installer.sh) and runs as root while the adapter still runs stock Carlinkit
# 2025.10.15.1127: the stock updater (update_box.sh from a stick, update_box_ota.sh after a SendFile
# over USB) unpacks the image to /tmp/update and runs this BEFORE its own space check and copy. A
# non-zero exit stops the stock updater with nothing copied. The payload is tmp/ocbm/ next to this.
#
#   1 checks     refuse, changing nothing: product, firmware, chip, factory mode, OCBM state,
#                payload checksums, stick room, same-version guard (stick only), then flash room.
#                For that last one an earlier try's OCBM files (after a revert, or a try stopped
#                part way) are removed first, so the room is measured: nothing uses them while no
#                mode is set, and a refusal then says so.
#   2 backup     stick: mtd0-2 to OCBM_BACKUP/<id>/, each read back against its SHA-256 (a complete one
#                from an earlier try is kept: it predates every change); both
#                routes: /etc/ocbm/identity.tgz. A failed backup is a refusal.
#   3 make room  delete prune.list (HiCar, Android Mirror, test leftovers, web UI pages); stock
#                CarPlay and Android Auto keep working. Removed paths go to /etc/ocbm/pruned.txt.
#   4 place      each payload file as .new, checked against SHA256SUMS, then renamed
#   5 arm        keep start_main_service.stock.sh, write /script/ocbm_mode=trial, then the one
#                boot-critical write: rename the dispatcher over start_main_service.sh. Power lost
#                before that rename leaves an adapter that boots stock, and running this again
#                picks up where it stopped.
#   6 hand off   empty /tmp/update except tmp/finish.sh, so the stock copy step copies nothing
#
# A refusal or a stop renames the image on the stick (.refused/.stopped): stock keeps CarPlay down
# after a failed update, and an image left in place would fail again on every boot. Each stick run
# writes OCBM_RESULT.txt anew and keeps the one before as OCBM_RESULT.prev.txt; an install that
# used OCBM_RETRY renames it OCBM_RETRY.used. The log times each step from power-on.
#
# Contract with Stage B (ccpa/installer/ocbm_dispatch.sh and ocbm_revert.sh): the dispatcher
# carries a line starting "# OCBM_DISPATCH", and a revert to stock appends
# "reverted version=<first line of VERSION>" to /etc/ocbm/last_result.txt. Arming clears Stage B's
# per-try state in /etc/ocbm (boot_attempts, first_start.txt, pending_result.txt).
#
# Test hooks (tools/ocbm_installer_test.sh): ROOT SYS PROC DEVDIR STICK UPDATE ROUTE PAYLOAD LOG
# STATE RESULT FAKE_FREE_KB FAKE_FREE_AFTER_KB FAKE_STICK_FREE_KB, and STOP_AT=<step> to simulate
# power loss (exit 99) right before that step. FAKE_FREE_KB rises by what the checks remove of an
# earlier try, as df does.
ROOT="${ROOT:-}"
SYS="${SYS:-/sys}"
PROC="${PROC:-/proc}"
DEVDIR="${DEVDIR:-/dev}"
STICK="${STICK:-/mnt/UPAN}"
UPDATE="${UPDATE:-/tmp/update}"
PAYLOAD="${PAYLOAD:-$(cd "$(dirname "$0")" && pwd)/ocbm}"
LOG="${LOG:-/tmp/ocbm_install.log}"
STATE="${STATE:-/tmp/ocbm_install.state}"
RESULT="${RESULT:-/tmp/OCBM_RESULT.txt}"
RESERVE_KB="${RESERVE_KB:-384}"
STICK_NEED_KB=40960
WANT_PRODUCT=A15W
WANT_FW=2025.10.15.1127
CHIPS="0x9159 0xc822"
MARK="# OCBM_DISPATCH"

SMS="$ROOT/script/start_main_service.sh"
SMS_STOCK="$ROOT/script/start_main_service.stock.sh"
MODE="$ROOT/script/ocbm_mode"
EO="$ROOT/etc/ocbm"

: > "$LOG"
say() { echo "[ocbm] $*" | tee -a "$LOG"; }
up() { u=$(cut -d. -f1 "$PROC/uptime" 2>/dev/null); echo "${u:-0}"; }   # seconds since power-on
T0=$(up)
step() {
  [ "${STOP_AT:-}" = "$1" ] && { say "simulated power loss before $1"; exit 99; }
  say "== $1 (+$(( $(up) - T0 )) s)"
}

product=$(cat "$ROOT/etc/box_product_type" 2>/dev/null)
IMG="$STICK/${product:-$WANT_PRODUCT}_Update.img"
VER=$(head -1 "$PAYLOAD/VERSION" 2>/dev/null)
if [ -z "${ROUTE:-}" ]; then
  case "$(tr '\0' ' ' < "$PROC/$PPID/cmdline" 2>/dev/null)" in
    *update_box_ota*) ROUTE=ota ;;
    *) ROUTE=stick ;;
  esac
fi
if [ "$ROUTE" = stick ] && [ -f "$STICK/OCBM_RESULT.txt" ]; then   # this run's report replaces it
  mv -f "$STICK/OCBM_RESULT.txt" "$STICK/OCBM_RESULT.prev.txt"; sync
fi

result() {  # $1 headline, then lines of text for the user
  { echo "OCBM install: $1"; echo "installer: ${VER:-unknown}"; echo; shift
    for l in "$@"; do echo "$l"; done
    echo; echo "--- log"; cat "$LOG"; } > "$RESULT"
  if [ "$ROUTE" = stick ] && [ -d "$STICK" ]; then cp "$RESULT" "$STICK/OCBM_RESULT.txt"; fi
  sync
}
park() {  # $1 suffix
  [ "$ROUTE" = stick ] && [ -f "$IMG" ] && mv -f "$IMG" "$IMG.$1"
  sync
}
refuse() {
  say "REFUSED: $1"
  result "REFUSED" "$1" "" "${refused_note:-Nothing on the adapter was changed.}" \
    "Remove the stick, then unplug the adapter and plug it back in: it starts stock CarPlay as before."
  park refused
  exit 1
}
abort() {  # past the prune: stock still boots, the extras are gone
  say "STOPPED: $1"
  result "STOPPED" "$1" "" "The adapter still starts stock CarPlay and Android Auto." \
    "To make room, HiCar, the Android Mirror helper and the web pages were removed (listed in" \
    "/etc/ocbm/pruned.txt). Carlinkit's own $WANT_FW image puts back what it ships; all of it," \
    "this unit's HiCar identity included, is in the flash backup on this stick." \
    "Remove the stick, then unplug the adapter and plug it back in."
  park stopped
  exit 1
}

# OCBM's image on an adapter that already runs OCBM, trial or final: an update, not a refusal (Scott,
# 3 Nov: one image on a stick takes stock to OCBM and any OCBM to this one, with no way back to stock
# first). The payload's programs and scripts, with their sums, become a package that updates from any
# version (FROM_MIN 0.0.0), and the adapter's own ocbm_update.sh installs it: its checks (newer, room,
# product, chip, factory mode, mode) and its trial, which keeps the new version once it passes its
# self-test and puts the old one back otherwise. Only the files that differ are written. From a stick,
# the dispatcher reports the trial's outcome above this run's report (stick_update in /etc/ocbm).
image_update() {
  # Never mid-drive: the stick handler runs this the moment a stick goes in, and the update restarts
  # the adapter. At a start the stick is read before an app connects (mdev at boot), so it runs then.
  if [ "$(cat "${RUN:-/tmp}/host_present" 2>/dev/null)" = 1 ]; then
    say "an app is connected: this image updates OCBM at the next start, not during the drive"
    result "WAITING FOR THE NEXT START" \
      "OCBM ${have:-?} runs on this adapter, and this image updates it to $VER, but not during a drive:" \
      "the update restarts the adapter." "" \
      "Leave the stick in: the update runs the next time the adapter starts (the car is turned on, or it is unplugged and plugged back in)."
    exit 1
  fi
  pk="$UPDATE/ocbm-image-update"
  rm -rf "$pk" "$pk.tgz"; mkdir -p "$pk/files" || refuse "Could not make the update package in RAM."
  grep -E '  (usr/sbin|usr/bin|script)/' "$PAYLOAD/SHA256SUMS" > "$pk/SHA256SUMS"
  while read -r sum rel; do
    mkdir -p "$pk/files/${rel%/*}" && cp "$PAYLOAD/$rel" "$pk/files/$rel" || refuse "Could not make the update package in RAM."
  done < "$pk/SHA256SUMS"
  echo "$VER" > "$pk/VERSION"; echo 0.0.0 > "$pk/FROM_MIN"
  (cd "$pk" && tar -cf - VERSION FROM_MIN SHA256SUMS files) | gzip -c > "$pk.tgz" && rm -rf "$pk" \
    || refuse "Could not make the update package in RAM."
  say "OCBM ${have:-?} runs here: updating it to $VER with the adapter's ocbm_update.sh"
  [ "$ROUTE" = stick ] && { echo "${IMG##*/}" > "$EO/stick_update"; sync; }
  result "UPDATING" "OCBM ${have:-?} runs on this adapter: this image updates it to $VER." "" \
    "The adapter restarts and tries the new version; the outcome is added above this when the trial ends." \
    "If the new version fails its start-up self-test, the adapter goes back to OCBM ${have:-?} by itself." "" \
    "No update is guaranteed to succeed, least of all one the OCBM project did not build; what you put on" \
    "your adapter is your choice (MIGRATING.md)."
  park used   # one use: the update restarts the adapter, and a stick left in must not run it again
  "$ROOT/script/ocbm_update.sh" "$pk.tgz" >> "$LOG" 2>&1   # placed: it restarts the adapter
  [ -f "$EO/update_pending" ] && exit 1
  rm -f "$EO/stick_update" "$pk.tgz"
  r=$(cat "$EO/update_result" 2>/dev/null)
  case "$r" in
    committed*) result "UPDATED" "$r"; exit 1 ;;
  esac
  [ "$ROUTE" = stick ] && [ -f "$IMG.used" ] && mv -f "$IMG.used" "$IMG.refused" && sync
  refuse "${r:-The update did not run.}"
}

# THE WLAN/BT GUARD, as in tools/ncm_base_install.sh: no deletion may take a unit's radio files.
is_radio() {
  case "$1" in
    */lib/firmware/*|*/lib/modules/*|*.ko|*/nvram*|*/*fw*.bin|*/*_bt_*|*/*wlan*|*/*WlanCal*) return 0;;
  esac
  b=${1##*/}
  case "$b" in
    init_bluetooth_wifi.sh|close_bluetooth_wifi.sh|load_bluetooth_wifi.sh|\
    attach_bluetooth.sh|start_bluetooth_wifi.sh|wlan_on.sh|wlan_off.sh|bt_on.sh|bt_off.sh) return 0;;
    radio_*) return 0;;
    *hciattach*|hci*|bluetooth*|*bluetooth*|bt_*|*_bt|hfpd|dbus-daemon|dbus*|\
    hostapd|wpa_supplicant|wpa_cli|p2p_supplicant*|wl|iw|iwpriv|iwconfig|iwlist|\
    fw_loader*|set_wifi_mac|*patchram*|sdio*|libnl*|libbluetooth*) return 0;;
    mlan*|moal*|88x2*|8733*|bcmdhd*|*_ko.tar.gz|*_rootfs.tar.gz|\
    *8822*|*8987*|*iw416*|*IW416*|*bcm4*|*BCM4*|*rtl8*|*RTL8*|*rtk*|txpower*|wifi_mod_para*) return 0;;
    *wifi*|*WiFi*|*wlan*|*WLAN*) return 0;;
  esac
  return 1
}

prune_matches() {  # $1 plain|guarded: existing paths matched by prune.list's lines of that kind, radio files excluded
  while read -r line; do   # notes go to the log only
    case "$line" in ''|'#'*) continue ;; '?'*) [ "$1" = guarded ] || continue; line=${line#?} ;; *) [ "$1" = plain ] || continue ;; esac
    case "$line" in /*|*..*) continue ;; esac
    for p in $ROOT/$line; do
      [ -e "$p" ] || [ -L "$p" ] || continue
      if is_radio "/${p#$ROOT/}"; then echo "[ocbm] keep (radio guard) /${p#$ROOT/}" >> "$LOG"; continue; fi
      echo "$p"
    done
  done < "$PAYLOAD/prune.list"
}

prune_targets() {  # what the prune removes. stdout is the list: every plain match, then each guarded
  # one ("?" in prune.list) that no file staying behind names. A library is named by its stem
  # (libz.so for libz.so.1.2.11), anything else by its own name.
  plain=$(prune_matches plain); guarded=$(prune_matches guarded)
  [ -n "$plain" ] && echo "$plain"
  [ -n "$guarded" ] || return 0
  x="$STATE.prune_x"; printf '%s\n' $plain $guarded > "$x"
  for p in $guarded; do
    b=${p##*/}
    case "$b" in lib*.so*) b="${b%%.so*}.so" ;; esac
    by=$(grep -rlsF -- "$b" "$ROOT/bin" "$ROOT/sbin" "$ROOT/usr" "$ROOT/lib" "$ROOT/script" "$ROOT/etc" 2>/dev/null \
      | grep -vxF -f "$x" | head -1)
    if [ -n "$by" ]; then echo "[ocbm] keep /${p#$ROOT/}: /${by#$ROOT/} names it" >> "$LOG"; else echo "$p"; fi
  done
  rm -f "$x"
}
under_root() { case "$1" in "$ROOT"/?*) return 0 ;; esac; return 1; }
payload_files() {  # what the place step writes: the manifest's usr/sbin, usr/bin and script files
  while read -r sum rel; do
    case "$rel" in script/ocbm_dispatch.sh) ;; usr/sbin/*|usr/bin/*|script/*) echo "$rel" ;; esac
  done < "$PAYLOAD/SHA256SUMS"
}

sum_of() { sha256sum "$1" 2>/dev/null | cut -c1-64; }
free_kb() { df -k "$1" 2>/dev/null | awk 'NR>1 {f=$4} END {print f+0}'; }

# ---------------------------------------------------------------------------------------------
step checks
say "route: $ROUTE; installer: ${VER:-?}; started $T0 s after power-on"
[ -n "$VER" ] || refuse "The installer's VERSION file is missing: the image is damaged."
case "$UPDATE" in ''|/) refuse "The update folder is not set." ;; esac
[ -f "$UPDATE/tmp/finish.sh" ] || refuse "The image has no finish.sh: it is damaged."
[ "$product" = "$WANT_PRODUCT" ] \
  || refuse "This adapter is a '${product:-unknown}', not an $WANT_PRODUCT (CPC200-CCPA)."
fw=$(cat "$ROOT/etc/software_version" 2>/dev/null)
[ "$fw" = "$WANT_FW" ] \
  || refuse "This adapter runs firmware ${fw:-unknown}. The installer needs $WANT_FW: update with Carlinkit's $WANT_FW image first."
chips=""
for d in "$SYS"/bus/sdio/devices/*; do [ -r "$d/device" ] && chips="$chips $(cat "$d/device")"; done
chip_ok=0
for c in $chips; do case " $CHIPS " in *" $c "*) chip_ok=1 ;; esac; done
[ "$chip_ok" = 1 ] \
  || refuse "The Wi-Fi/Bluetooth chip (${chips:- none found}) is not one OCBM supports yet ($CHIPS)."
mfg=$(cat "$SYS/module/libcomposite/parameters/mfgmode" 2>/dev/null)
[ "${mfg:-0}" = 0 ] || refuse "The adapter is in factory mode ${mfg}."

dispatcher=0
grep -q "^$MARK" "$SMS" 2>/dev/null && dispatcher=1
if [ "$dispatcher" = 1 ] && [ -e "$MODE" ]; then
  # OCBM_RESTORE on the same stick goes first (the truck adapter, 2 Nov). The stick handler runs this
  # image the moment the stick goes into a running trial, before the dispatcher's restore_watch sees
  # OCBM_RESTORE; refusing the installed OCBM then parked the image as .refused, and it had to be
  # renamed back by hand for the install after the restore. Stand aside, image left in place.
  if [ "$ROUTE" = stick ] && [ -e "$STICK/OCBM_RESTORE" ]; then
    say "OCBM_RESTORE is on the stick: the adapter goes back to stock first; this image waits"
    result "WAITING FOR THE RESTORE" \
      "OCBM_RESTORE is on this stick, so the adapter goes back to stock first. Nothing was installed." "" \
      "When the adapter has restarted, unplug it and plug it back in with this stick in: the install runs then."
    exit 1
  fi
  # An update package on the same stick: OCBM takes it itself at its start (ocbm_dispatch.sh, stick_update),
  # with the update's own trial; this image would only refuse the installed OCBM. Stand aside, image left.
  if [ "$ROUTE" = stick ] && ls "$STICK"/ocbm-update-*.tgz >/dev/null 2>&1; then
    say "an OCBM update package is on the stick: OCBM updates itself from it when it starts; this image waits"
    result "WAITING FOR THE UPDATE" \
      "An OCBM update package is on this stick, and OCBM on this adapter updates itself from it when it starts." \
      "This image was not used." "" "The update's own report goes above this one when it has finished."
    exit 1
  fi
  have=$(cat "$EO/installed_version" 2>/dev/null)
  if [ "$have" != "$VER" ] && [ -x "$ROOT/script/ocbm_update.sh" ]; then image_update; fi
  if [ "$have" = "$VER" ]; then   # e.g. the stick went in again after power was lost before finish.sh
    say "already installed: $VER ($(cat "$MODE"))"
    result "ALREADY INSTALLED" "OCBM $VER is already on this adapter (mode: $(cat "$MODE")). Nothing was changed." \
      "" "Remove the stick."
    park installed
    exit 1
  fi
  refuse "OCBM ${have:-(another version)} is already installed on this adapter (mode: $(cat "$MODE")). This OCBM is too old to update itself from an image: go back to stock first, then install this image."
fi
[ -e "$ROOT/usr/sbin/ocbmd" ] && [ ! -d "$EO" ] \
  && refuse "This adapter was converted to OCBM by hand (tools/ocbm_install.sh); manage it with that tool."
[ "$dispatcher" = 1 ] && [ ! -s "$SMS_STOCK" ] \
  && refuse "The OCBM start script is in place but the stock one it should keep is missing."
[ -s "$SMS" ] || refuse "This adapter has no /script/start_main_service.sh."

[ -s "$PAYLOAD/SHA256SUMS" ] && (cd "$PAYLOAD" && sha256sum -c SHA256SUMS >/dev/null 2>&1) \
  || refuse "The installer's files do not match their checksums: the image is damaged. Download it again and check its SHA-256."
grep -q "^$MARK" "$PAYLOAD/script/ocbm_dispatch.sh" 2>/dev/null \
  || refuse "The image has no OCBM start script: it is damaged or incomplete."
while read -r sum rel; do
  case "$rel" in script/ocbm_dispatch.sh) continue ;; usr/sbin/*|usr/bin/*|script/*) ;; *) continue ;; esac
  # The radio seam (script/radio_*.sh) is ours even without /etc/ocbm: stock never ships it, and every
  # tool that takes OCBM off keeps it through is_radio(), stock_restore.sh included. An adapter restored
  # to stock still has it (the factory adapter, 29 Oct), so it is replaced like the rest of the payload.
  case "$rel" in script/radio_*) continue ;; esac
  [ -e "$ROOT/$rel" ] && [ ! -d "$EO" ] \
    && refuse "The adapter already has /$rel, which the installer would overwrite."
done < "$PAYLOAD/SHA256SUMS"

need_kb=$(sed -n 's/^need_kb=//p' "$PAYLOAD/BUDGET" 2>/dev/null)
[ -n "$need_kb" ] || refuse "The installer's BUDGET file is missing: the image is damaged."

if [ "$ROUTE" = stick ]; then
  [ -d "$STICK" ] || refuse "No USB stick is mounted."
  sfree=${FAKE_STICK_FREE_KB:-$(free_kb "$STICK")}
  [ "$sfree" -ge "$STICK_NEED_KB" ] \
    || refuse "The stick has $((sfree / 1024)) MB free; the flash backup needs 40 MB."
fi
# The same-version guard is for the stick: an image left on it would reinstall a version that was undone
# (a failed self-test, say) on every start. Over USB each install is a press of Convert in the app, which
# has no way to put OCBM_RETRY anywhere, so the guard would only block a deliberate retry.
if [ "$ROUTE" = stick ] && grep -qxF "reverted version=$VER" "$EO/last_result.txt" 2>/dev/null && [ ! -e "$STICK/OCBM_RETRY" ]; then
  refuse "This installer ($VER) was installed and undone on this adapter before. To try it again anyway, put an empty file named OCBM_RETRY on the stick next to the image."
fi

# The flash room, last: it is the one check that may change something. An earlier try's OCBM files
# (reverted, or stopped part way) are replaced anyway and nothing names them while no mode is set, so
# they go now and the room is measured. Dev adapter, 3 Nov: power cut mid-place, and the retry counted
# only the finished files, at 0.6 of their size, and the half-written .new not at all ("2188 KB free and
# about 732 KB more"), so it refused a flash with ~1620 KB in those files; a cut during the first file
# would have kept its .new for good. The radio seam stays (is_radio): place replaces it like the rest.
freed_kb=0
if [ -d "$EO" ]; then
  for rel in $(payload_files); do
    for f in "$ROOT/$rel.new" "$ROOT/$rel"; do
      [ -f "$f" ] || continue
      case "$f" in "$ROOT"/script/radio_*.sh) continue ;; esac
      freed_kb=$((freed_kb + $(du -sk "$f" | cut -f1)))
      rm -f "$f"
    done
  done
  if [ "$freed_kb" -gt 0 ]; then
    sync
    say "removed the earlier try's OCBM files ($freed_kb KB), which nothing uses while no mode is set"
    refused_note="Only the earlier try's unused OCBM files were removed; nothing else on the adapter was changed."
    [ -n "${FAKE_FREE_KB:-}" ] && FAKE_FREE_KB=$((FAKE_FREE_KB + freed_kb))
  fi
fi
prune_kb=0
for p in $(prune_targets); do
  under_root "$p" && prune_kb=$((prune_kb + $(du -sk "$p" | cut -f1)))
done
free=${FAKE_FREE_KB:-$(free_kb "$ROOT/")}
gain=$((prune_kb * 7 / 10))   # JFFS2 holds these at ~0.8 of their size
say "flash: $free KB free, ~$gain KB more after making room, OCBM needs $need_kb KB + $RESERVE_KB KB spare"
[ $((free + gain)) -ge $((need_kb + RESERVE_KB)) ] \
  || refuse "Not enough flash: $free KB free and about $gain KB more after making room, but OCBM needs $need_kb KB plus $RESERVE_KB KB spare."

# ---------------------------------------------------------------------------------------------
step backup
new_eo=0
[ -d "$EO" ] || new_eo=1
fail_backup() {
  rm -f "$EO/identity.tgz.new"
  [ "$new_eo" = 1 ] && rm -rf "$EO"
  refuse "The backup failed ($1), so nothing was installed."
}
id=$(tr -cd 'A-Za-z0-9' < "$ROOT/etc/device_serial" 2>/dev/null | cut -c1-32)
[ -n "$id" ] || id=$(tr -cd '0-9A-Fa-f' < "$SYS/class/net/wlan0/address" 2>/dev/null)
[ -n "$id" ] || id=adapter
B=""
if [ "$ROUTE" = stick ]; then
  B="$STICK/OCBM_BACKUP/$id"
  # A complete backup of this adapter already on the stick was taken by an earlier try before it
  # changed anything, so it holds the factory flash: keep it rather than dump the changed one over it.
  if [ "$(grep -cE '^[0-9a-f]{64}  mtd[012]\.bin$' "$B/SHA256SUMS" 2>/dev/null)" = 3 ] \
     && (cd "$B" && sha256sum -c SHA256SUMS >/dev/null 2>&1); then
    say "flash backup: kept the earlier one in OCBM_BACKUP/$id"
  else
  mkdir -p "$B" || fail_backup "cannot write to the stick"
  rm -f "$B/SHA256SUMS"
  for n in 0 1 2; do
    hex=$(awk -v d="mtd$n:" '$1 == d {print $2}' "$PROC/mtd" 2>/dev/null)
    [ -n "$hex" ] || fail_backup "mtd$n is not in /proc/mtd"
    sum=$(cat "$DEVDIR/mtd$n" | tee "$B/mtd$n.bin" | sha256sum | cut -c1-64)
    sync
    [ "$PROC" = /proc ] && echo 3 > /proc/sys/vm/drop_caches   # read back from the stick, not the cache
    [ "$(wc -c < "$B/mtd$n.bin" | tr -d ' ')" = "$((0x$hex))" ] || fail_backup "mtd$n.bin is short"
    [ "$(sum_of "$B/mtd$n.bin")" = "$sum" ] || fail_backup "mtd$n.bin did not read back intact"
    echo "$sum  mtd$n.bin" >> "$B/SHA256SUMS"
  done
  say "flash backup: OCBM_BACKUP/$id on the stick"
  fi
fi
mkdir -p "$EO" || fail_backup "cannot create /etc/ocbm"
bt=$(cat "$SYS/class/bluetooth/hci0/address" 2>/dev/null)
[ -n "$bt" ] || bt=$(hciconfig hci0 2>/dev/null | sed -n 's/.*BD Address: \([0-9A-Fa-f:]*\).*/\1/p')
{ echo "wlan0=$(cat "$SYS/class/net/wlan0/address" 2>/dev/null)"; echo "hci0=$bt"; } > "$EO/addresses.txt" \
  || fail_backup "cannot write /etc/ocbm/addresses.txt"
files="etc/ocbm/addresses.txt"
while read -r line; do
  case "$line" in ''|'#'*|/*|*..*) continue ;; esac
  for p in $ROOT/$line; do [ -e "$p" ] && files="$files ${p#$ROOT/}"; done
done < "$PAYLOAD/identity.list"
# shellcheck disable=SC2086
(cd "$ROOT/" && tar -cf - $files) | gzip -c > "$EO/identity.tgz.new" \
  && [ -n "$(gzip -dc "$EO/identity.tgz.new" | tar -tf - 2>/dev/null)" ] \
  && mv -f "$EO/identity.tgz.new" "$EO/identity.tgz" || fail_backup "cannot write /etc/ocbm/identity.tgz"
sync
say "identity: /etc/ocbm/identity.tgz ($(echo $files | wc -w | tr -d ' ') entries)"

# ---------------------------------------------------------------------------------------------
step make_room
[ -z "$ROOT" ] && killall ARMHiCar >/dev/null 2>&1
for p in $(prune_targets); do
  under_root "$p" || continue
  rm -rf "$p" && echo "/${p#$ROOT/}" >> "$EO/pruned.txt"
done
sync
free2=${FAKE_FREE_AFTER_KB:-${FAKE_FREE_KB:-$(free_kb "$ROOT/")}}
say "flash after making room: $free2 KB free"
[ "$free2" -ge $((need_kb + RESERVE_KB)) ] \
  || abort "After making room there are $free2 KB free, but OCBM needs $need_kb KB plus $RESERVE_KB KB spare."

# ---------------------------------------------------------------------------------------------
step place
while read -r sum rel; do
  case "$rel" in script/ocbm_dispatch.sh) continue ;; usr/sbin/*|usr/bin/*|script/*) ;; *) continue ;; esac
  dest="$ROOT/$rel"
  if cp "$PAYLOAD/$rel" "$dest.new" && chmod 755 "$dest.new" && [ "$(sum_of "$dest.new")" = "$sum" ]; then
    mv -f "$dest.new" "$dest"
  else
    rm -f "$dest.new"
    abort "Could not write /$rel (the flash may be full or failing)."
  fi
done < "$PAYLOAD/SHA256SUMS"
sync
say "placed the OCBM files"

# ---------------------------------------------------------------------------------------------
step arm
if [ "$dispatcher" = 0 ]; then   # the current start script IS stock: keep it for the fallback
  if ! { cp -p "$SMS" "$SMS_STOCK.new" && cmp -s "$SMS" "$SMS_STOCK.new" && mv -f "$SMS_STOCK.new" "$SMS_STOCK"; }; then
    rm -f "$SMS_STOCK.new"
    abort "Could not keep a copy of the stock start script."
  fi
fi
rm -f "$EO/boot_attempts" "$EO/first_start.txt" "$EO/pending_result.txt"   # Stage B state of an earlier try
{ echo trial > "$MODE.new" && mv -f "$MODE.new" "$MODE"; } || abort "Could not write /script/ocbm_mode."
echo "$VER" > "$EO/installed_version" || abort "Could not write /etc/ocbm/installed_version."
want=$(awk '$2 == "script/ocbm_dispatch.sh" {print $1}' "$PAYLOAD/SHA256SUMS")
if ! { cp "$PAYLOAD/script/ocbm_dispatch.sh" "$SMS.new" && chmod 755 "$SMS.new" && [ "$(sum_of "$SMS.new")" = "$want" ]; }; then
  rm -f "$SMS.new" "$MODE"
  abort "Could not write the OCBM start script."
fi
sync
step rename
mv -f "$SMS.new" "$SMS" || { rm -f "$MODE"; abort "Could not put the OCBM start script in place."; }
sync
say "armed: the next start runs OCBM (trial); stock is kept as start_main_service.stock.sh"
# OCBM_RETRY is consent for one install: left on the stick, it would switch the same-version guard
# off for every later install from it.
if [ "$ROUTE" = stick ] && [ -e "$STICK/OCBM_RETRY" ]; then
  mv -f "$STICK/OCBM_RETRY" "$STICK/OCBM_RETRY.used" && say "OCBM_RETRY used: renamed OCBM_RETRY.used"
  sync
fi

# ---------------------------------------------------------------------------------------------
step hand_off
{ echo "VER='$VER'"; echo "ROUTE='$ROUTE'"; echo "STICK='$STICK'"; echo "IMG='$IMG'"
  echo "BACKUP='$B'"; echo "LOG='$LOG'"; echo "RESULT='$RESULT'"; } > "$STATE"
for e in "$UPDATE"/* "$UPDATE"/.[!.]*; do
  [ -e "$e" ] || continue
  [ "$e" = "$UPDATE/tmp" ] || rm -rf "$e"
done
for e in "$UPDATE"/tmp/* "$UPDATE"/tmp/.[!.]*; do
  [ -e "$e" ] || continue
  [ "$e" = "$UPDATE/tmp/finish.sh" ] || rm -rf "$e"
done
say "handed off: the stock copy step has nothing to copy"
say "installer took $(( $(up) - T0 )) s; the restart into the trial follows"
exit 0
