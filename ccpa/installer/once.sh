#!/bin/sh
# once.sh — Stage A of the one-file OCBM install. Ships as tmp/once.sh in A15W_Update.img
# (tools/make_ocbm_installer.sh) and runs as root while the adapter still runs stock Carlinkit
# 2025.10.15.1127: the stock updater (update_box.sh from a stick, update_box_ota.sh after a SendFile
# over USB) unpacks the image to /tmp/update and runs this BEFORE its own space check and copy. A
# non-zero exit stops the stock updater with nothing copied. The payload is tmp/ocbm/ next to this.
#
#   1 checks     refuse, changing nothing: product, firmware, chip, factory mode, OCBM state,
#                payload checksums, flash room, stick room, same-version guard
#   2 backup     stick: mtd0-2 to OCBM_BACKUP/<id>/, each read back against its SHA-256; both
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
# after a failed update, and an image left in place would fail again on every boot.
#
# Contract with Stage B (ccpa/installer/ocbm_dispatch.sh and ocbm_revert.sh): the dispatcher
# carries a line starting "# OCBM_DISPATCH", and a revert to stock appends
# "reverted version=<first line of VERSION>" to /etc/ocbm/last_result.txt. Arming clears Stage B's
# per-try state in /etc/ocbm (boot_attempts, first_start.txt, pending_result.txt).
#
# Test hooks (tools/ocbm_installer_test.sh): ROOT SYS PROC DEVDIR STICK UPDATE ROUTE PAYLOAD LOG
# STATE RESULT FAKE_FREE_KB FAKE_FREE_AFTER_KB FAKE_STICK_FREE_KB, and STOP_AT=<step> to simulate
# power loss (exit 99) right before that step.
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
step() {
  [ "${STOP_AT:-}" = "$1" ] && { say "simulated power loss before $1"; exit 99; }
  say "== $1"
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
  result "REFUSED" "$1" "" "Nothing on the adapter was changed." \
    "Remove the stick, then unplug the adapter and plug it back in: it starts stock CarPlay as before."
  park refused
  exit 1
}
abort() {  # past the prune: stock still boots, the extras are gone
  say "STOPPED: $1"
  result "STOPPED" "$1" "" "The adapter still starts stock CarPlay and Android Auto." \
    "To make room, HiCar, the Android Mirror helper and the web pages were removed (listed in" \
    "/etc/ocbm/pruned.txt). Carlinkit's own $WANT_FW image puts them back." \
    "Remove the stick, then unplug the adapter and plug it back in."
  park stopped
  exit 1
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

prune_targets() {  # existing paths matched by prune.list, radio files excluded. stdout is the list:
  while read -r line; do   # notes go to the log only
    case "$line" in ''|'#'*|/*|*..*) continue ;; esac
    for p in $ROOT/$line; do
      [ -e "$p" ] || [ -L "$p" ] || continue
      if is_radio "/${p#$ROOT/}"; then echo "[ocbm] keep (radio guard) /${p#$ROOT/}" >> "$LOG"; continue; fi
      echo "$p"
    done
  done < "$PAYLOAD/prune.list"
}
under_root() { case "$1" in "$ROOT"/?*) return 0 ;; esac; return 1; }

sum_of() { sha256sum "$1" 2>/dev/null | cut -c1-64; }
free_kb() { df -k "$1" 2>/dev/null | awk 'NR>1 {f=$4} END {print f+0}'; }

# ---------------------------------------------------------------------------------------------
step checks
say "route: $ROUTE; installer: ${VER:-?}"
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
  have=$(cat "$EO/installed_version" 2>/dev/null)
  if [ "$have" = "$VER" ]; then   # e.g. the stick went in again after power was lost before finish.sh
    say "already installed: $VER ($(cat "$MODE"))"
    result "ALREADY INSTALLED" "OCBM $VER is already on this adapter (mode: $(cat "$MODE")). Nothing was changed." \
      "" "Remove the stick."
    park installed
    exit 1
  fi
  refuse "OCBM ${have:-(another version)} is already installed on this adapter (mode: $(cat "$MODE")). Updating it this way is not supported yet."
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
  [ -e "$ROOT/$rel" ] && [ ! -d "$EO" ] \
    && refuse "The adapter already has /$rel, which the installer would overwrite."
done < "$PAYLOAD/SHA256SUMS"

need_kb=$(sed -n 's/^need_kb=//p' "$PAYLOAD/BUDGET" 2>/dev/null)
[ -n "$need_kb" ] || refuse "The installer's BUDGET file is missing: the image is damaged."
prune_kb=0
for p in $(prune_targets); do
  under_root "$p" && prune_kb=$((prune_kb + $(du -sk "$p" | cut -f1)))
done
free=${FAKE_FREE_KB:-$(free_kb "$ROOT/")}
gain=$((prune_kb * 7 / 10))   # JFFS2 holds these at ~0.8 of their size; count 0.7
say "flash: $free KB free, ~$gain KB more after making room, OCBM needs $need_kb KB + $RESERVE_KB KB spare"
[ $((free + gain)) -ge $((need_kb + RESERVE_KB)) ] \
  || refuse "Not enough flash: $free KB free and about $gain KB more after making room, but OCBM needs $need_kb KB plus $RESERVE_KB KB spare."

if [ "$ROUTE" = stick ]; then
  [ -d "$STICK" ] || refuse "No USB stick is mounted."
  sfree=${FAKE_STICK_FREE_KB:-$(free_kb "$STICK")}
  [ "$sfree" -ge "$STICK_NEED_KB" ] \
    || refuse "The stick has $((sfree / 1024)) MB free; the flash backup needs 40 MB."
fi
if grep -qxF "reverted version=$VER" "$EO/last_result.txt" 2>/dev/null && [ ! -e "$STICK/OCBM_RETRY" ]; then
  refuse "This installer ($VER) was installed and undone on this adapter before. To try it again anyway, put an empty file named OCBM_RETRY on the stick next to the image."
fi

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
exit 0
