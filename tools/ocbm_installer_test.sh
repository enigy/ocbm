#!/usr/bin/env bash
# ocbm_installer_test.sh — the one-file OCBM installer end to end, on the host. Builds an image with
# make_ocbm_installer.sh from stand-in box binaries and Stage B scripts, unwraps it, and runs its
# tmp/once.sh and tmp/finish.sh against synthetic stock 2025.10.15.1127 adapters (ROOT, SYS, PROC,
# DEVDIR, STICK and UPDATE point into a scratch tree), under every POSIX shell found (sh, dash),
# because the box runs busybox ash. Needs no adapter and no box build.
#
#   tools/ocbm_installer_test.sh
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/ocbm_installer_test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }
blob() { mkdir -p "$(dirname "$1")"; head -c "$2" /dev/urandom > "$1"; }
snap() { ( cd "$1" && find . | LC_ALL=C sort | while IFS= read -r p; do
           if [ -f "$p" ] && [ ! -L "$p" ]; then echo "$p $(cksum < "$p")"; else echo "$p"; fi; done ); }

PRUNED="usr/sbin/ARMHiCar usr/lib/libdmsdp.so usr/lib/libdmsdpplatform.so usr/lib/libHisightSink.so
  usr/lib/libHwKeystoreSDK.so usr/lib/libsecurec.so usr/lib/libnearby.so etc/udhcpd_hicar_ap.conf
  etc/hichain/hks_keystore etc/BoxHelper.apk usr/sbin/AutomaticTest script/test_mic.sh lib/crt1.o
  lib/libgomp.spec lib/libasan.la lib/libstdc++.so.6.0.20-gdb.py lib/libSegFault.so
  etc/boa/www/index.html etc/boa/www/js/app.js.gz etc/boa/cgi-bin/server.cgi etc/boa/images/carlogo.png"
KEPT="usr/sbin/boa etc/boa/boa.conf etc/mime.types usr/sbin/i2cset usr/sbin/i2cdetect usr/sbin/tinymix
  usr/sbin/AppleCarPlay usr/sbin/ARMadb-driver lib/libc-2.20.so etc/box_brand.png
  lib/firmware/nxp/uartiw416_bt_v0.bin lib/sdio_wlan.o usr/lib/libbluetooth.so.2.11.2 usr/lib/libxml2.so.2.9.2"

# --- build an image from stand-ins ---------------------------------------------------------
mkdir -p "$T/bin"
for b in ocbmd iap2d carplayd btd aa-bridge; do blob "$T/bin/$b" 70000; done
blob "$T/irs" 20000
cp -R "$REPO/ccpa/installer" "$T/inst"
echo "== build"
BIN_DIR="$T/bin" IRS_BIN="$T/irs" INSTALLER_DIR="$T/inst" bash "$HERE/make_ocbm_installer.sh" "$T/out" > "$T/build.log" 2>&1
check '[ -s "$T/out/A15W_Update.img" ]' "the builder writes A15W_Update.img"
bash "$HERE/make_update_img.sh" unpack "$T/out/A15W_Update.img" "$T/u.tgz"
mkdir -p "$T/img"; tar -xzf "$T/u.tgz" -C "$T/img"
check '( cd "$T/img/tmp/ocbm" && sha256sum -c SHA256SUMS >/dev/null 2>&1 )' "the payload manifest verifies"
check 'grep -q "^need_kb=[0-9]" "$T/img/tmp/ocbm/BUDGET"' "BUDGET carries need_kb"
if grep -q '^# OCBM_STAGE_C_PLACEHOLDER' "$REPO/ccpa/installer/ocbm_finish.sh"; then
  check 'head -1 "$T/img/tmp/ocbm/VERSION" | grep -q "^1.0.0-bench ocbm "' "with the Stage C placeholder, VERSION says bench"
fi
want="$(grep -oE '\|/(usr/sbin|usr/bin|script)/[^|]+\|755' "$REPO/tools/ocbm_install.sh" | cut -d'|' -f2 | sed 's|^/||' | LC_ALL=C sort -u)"
have="$(bash "$HERE/make_ocbm_installer.sh" --list | grep -vE '^script/ocbm_(dispatch|selftest|revert|finish)\.sh$' | LC_ALL=C sort -u)"
check '[ "$want" = "$have" ]' "the payload is ocbm_install.sh --full's file set"
mkdir -p "$T/noinst"; cp "$REPO/ccpa/installer/"* "$T/noinst/"; rm -f "$T/noinst/ocbm_dispatch.sh"
check '! BIN_DIR="$T/bin" IRS_BIN="$T/irs" INSTALLER_DIR="$T/noinst" bash "$HERE/make_ocbm_installer.sh" "$T/out2" >/dev/null 2>&1' \
  "the builder refuses without the Stage B scripts"
VER="$(head -1 "$T/img/tmp/ocbm/VERSION")"
STOCK_SMS='#!/bin/sh
# stock start_main_service.sh (test stand-in)
echo stock'

make_box() {  # $1 dir: root/ sys/ proc/ dev/ stick/ update/ of a stock adapter with the image
  local d="$1" R="$1/root" f
  mkdir -p "$R/etc" "$R/script" "$R/usr/bin" "$R/var/lib/lockdown" "$R/etc/bluetooth"
  printf A15W > "$R/etc/box_product_type"
  printf 2025.10.15.1127 > "$R/etc/software_version"
  printf '%s\n' "$STOCK_SMS" > "$R/script/start_main_service.sh"; chmod 755 "$R/script/start_main_service.sh"
  for f in $PRUNED; do blob "$R/$f" 3000; done
  for f in $KEPT; do blob "$R/$f" 2000; done
  ln -s libbluetooth.so.2.11.2 "$R/usr/lib/libbluetooth.so.2"
  echo 'WiFiPassword=secret' > "$R/etc/riddle.conf"
  echo 'wpa_passphrase=secret' > "$R/etc/hostapd.conf"
  blob "$R/etc/bluetooth/mybluetooth.tar.gz" 500
  printf 'ABCDEF0123456789' > "$R/etc/device_serial"
  printf 20250101 > "$R/etc/manufacture_date"
  blob "$R/var/lib/lockdown/root_key.pem" 300
  mkdir -p "$d/sys/bus/sdio/devices/mmc0:0001:1" "$d/sys/module/libcomposite/parameters" \
           "$d/sys/class/net/wlan0" "$d/sys/class/bluetooth/hci0" "$d/proc" "$d/dev" "$d/stick" "$d/update"
  echo 0x9159 > "$d/sys/bus/sdio/devices/mmc0:0001:1/device"
  echo 0 > "$d/sys/module/libcomposite/parameters/mfgmode"
  echo aa:bb:cc:dd:ee:ff > "$d/sys/class/net/wlan0/address"
  echo 11:22:33:44:55:66 > "$d/sys/class/bluetooth/hci0/address"
  blob "$d/dev/mtd0" 8192; blob "$d/dev/mtd1" 16384; blob "$d/dev/mtd2" 32768
  printf 'dev:    size   erasesize  name\nmtd0: 00002000 00010000 "uboot"\nmtd1: 00004000 00010000 "kernel"\nmtd2: 00008000 00010000 "rootfs"\n' > "$d/proc/mtd"
  refresh "$d"
}
refresh() {  # $1 dir: a fresh image on the stick and a fresh /tmp/update, as on a re-insert
  rm -rf "$1/update"; mkdir -p "$1/update"; cp -R "$T/img/." "$1/update/"
  cp "$T/out/A15W_Update.img" "$1/stick/A15W_Update.img"
}
run_once() {  # $1 shell, $2 dir, then VAR=value overrides; returns once.sh's status
  local sh="$1" d="$2"; shift 2
  env ROOT="$d/root" SYS="$d/sys" PROC="$d/proc" DEVDIR="$d/dev" STICK="$d/stick" UPDATE="$d/update" \
      ROUTE=stick LOG="$d/install.log" STATE="$d/state" RESULT="$d/result.txt" \
      FAKE_FREE_KB=6000 FAKE_STICK_FREE_KB=100000 "$@" "$sh" "$d/update/tmp/once.sh" > "$d/once.out" 2>&1
}
run_finish() {  # $1 shell, $2 dir
  env STATE="$2/state" REBOOT="touch $2/rebooted" REBOOT_DELAY=0 "$1" "$2/update/tmp/finish.sh" 0 > "$2/finish.out" 2>&1
  sleep 1
}
installed() {  # $1 dir: the armed state, complete
  local R="$1/root" rel
  grep -q '^# OCBM_DISPATCH' "$R/script/start_main_service.sh" || return 1
  [ "$(cat "$R/script/start_main_service.stock.sh" 2>/dev/null)" = "$STOCK_SMS" ] || return 1
  [ "$(cat "$R/script/ocbm_mode" 2>/dev/null)" = trial ] || return 1
  for rel in $(bash "$HERE/make_ocbm_installer.sh" --list); do
    [ "$rel" = script/ocbm_dispatch.sh ] && continue
    cmp -s "$T/img/tmp/ocbm/$rel" "$R/$rel" && [ -x "$R/$rel" ] || return 1
  done
}
stock_boot() { [ "$(cat "$1/root/script/start_main_service.sh")" = "$STOCK_SMS" ]; }

for sh in ${SHELLS:-sh dash}; do
  command -v "$sh" >/dev/null 2>&1 || { echo "== $sh: not installed, skipped"; continue; }
  echo "== $sh: install from a stick"
  d="$T/$sh-ok"; make_box "$d"; snap "$d/root" > "$d/before"
  run_once "$sh" "$d"; rc=$?
  check '[ $rc = 0 ]' "once.sh succeeds (rc=$rc)"
  check 'installed "$d"' "armed: dispatcher in place, stock script kept, mode trial, payload placed"
  check 'n=0; for f in $PRUNED; do [ -e "$d/root/$f" ] && n=$((n+1)); done; [ $n = 0 ]' "every prune target is gone"
  snap "$d/root" > "$d/after"
  check '( for f in $KEPT; do grep -q "^\./$f " "$d/before" || exit 1; [ "$(grep "^\./$f " "$d/before")" = "$(grep "^\./$f " "$d/after")" ] || exit 1; done )' \
    "kept files are untouched (boa, i2c/tinymix, CarPlay, libs, logos, radio files)"
  check '[ -e "$d/root/lib/sdio_wlan.o" ]' "the radio guard keeps lib/sdio_wlan.o although lib/*.o is pruned"
  comm -13 <(sed 's/ .*//' "$d/before") <(sed 's/ .*//' "$d/after") > "$d/added"
  comm -23 <(sed 's/ .*//' "$d/before") <(sed 's/ .*//' "$d/after") > "$d/removed"
  exp_add="$( { for rel in $(bash "$HERE/make_ocbm_installer.sh" --list); do [ "$rel" = script/ocbm_dispatch.sh ] || echo "./$rel"; done
               printf '%s\n' ./etc/ocbm ./etc/ocbm/addresses.txt ./etc/ocbm/identity.tgz ./etc/ocbm/installed_version ./etc/ocbm/pruned.txt \
                 ./script/ocbm_mode ./script/start_main_service.stock.sh; } | LC_ALL=C sort)"
  check '[ "$(cat "$d/added")" = "$exp_add" ]' "nothing is added beyond the payload, /etc/ocbm and the arm files"
  check '! grep -vE "^\./(usr/sbin/ARMHiCar|usr/lib/lib(dmsdp|Hisight|HwKeystore|securec|nearby)|etc/(udhcpd_hicar_ap.conf|hichain|BoxHelper.apk|boa/(www|cgi-bin|images))|usr/sbin/AutomaticTest|script/test_mic.sh|lib/(crt1.o|libgomp.spec|libasan.la|libstdc\+\+.so.6.0.20-gdb.py|libSegFault.so))" "$d/removed"' \
    "nothing is removed beyond prune.list"
  changed="$(comm -23 <(sort "$d/before") <(sort "$d/after") | sed 's/ .*//' | grep -vxF -f "$d/removed")"
  check '[ "$changed" = "./script/start_main_service.sh" ]' "the only rewritten stock file is start_main_service.sh"
  check '[ "$(wc -l < "$d/root/etc/ocbm/pruned.txt" | tr -d " ")" = 20 ] && grep -qx /etc/boa/www "$d/root/etc/ocbm/pruned.txt"' "pruned.txt records the deletions"
  check 'gzip -dc "$d/root/etc/ocbm/identity.tgz" | tar -tf - | grep -qx etc/riddle.conf && gzip -dc "$d/root/etc/ocbm/identity.tgz" | tar -tf - | grep -q "^var/lib/lockdown/root_key.pem" && grep -q "^hci0=11:22" "$d/root/etc/ocbm/addresses.txt"' \
    "identity.tgz holds riddle.conf, pairing records and the addresses"
  B="$d/stick/OCBM_BACKUP/ABCDEF0123456789"
  check '( for n in 0 1 2; do cmp -s "$d/dev/mtd$n" "$B/mtd$n.bin" || exit 1; done; cd "$B" && sha256sum -c SHA256SUMS >/dev/null 2>&1 )' \
    "mtd0-2 are on the stick and verify"
  check '[ "$(cd "$d/update" && find . -type f)" = "./tmp/finish.sh" ]' "/tmp/update holds only tmp/finish.sh"
  run_finish "$sh" "$d"
  check 'grep -q "INSTALLED (trial)" "$d/stick/OCBM_RESULT.txt" && grep -q "keep the stick private" "$d/stick/OCBM_RESULT.txt"' "finish.sh writes the result to the stick"
  check '[ ! -e "$d/stick/A15W_Update.img" ] && [ -e "$d/stick/A15W_Update.img.installed" ]' "finish.sh parks the image as .installed"
  check '[ -e "$d/rebooted" ]' "finish.sh restarts the adapter"

  echo "== $sh: refusals change nothing"
  refresh "$d"; snap "$d/root" > "$d/b2"; run_once "$sh" "$d"; rc=$?
  check '[ $rc = 1 ] && grep -q "^OCBM install: ALREADY INSTALLED" "$d/stick/OCBM_RESULT.txt" && [ "$(snap "$d/root")" = "$(cat "$d/b2")" ] && [ ! -e "$d/stick/A15W_Update.img" ]' \
    "the same version again: ALREADY INSTALLED, nothing changed, image parked"
  echo "0.9.0 ocbm older" > "$d/root/etc/ocbm/installed_version"; refresh "$d"; snap "$d/root" > "$d/b3"; run_once "$sh" "$d"; rc=$?
  check '[ $rc = 1 ] && grep -q "^OCBM install: REFUSED" "$d/stick/OCBM_RESULT.txt" && [ "$(snap "$d/root")" = "$(cat "$d/b3")" ] && [ -e "$d/stick/A15W_Update.img.refused" ]' \
    "another version installed: REFUSED, nothing changed, image parked as .refused"
  for c in product firmware chip factory handmade damaged flash stick backup; do
    r="$T/$sh-$c"; make_box "$r"; extra=""
    case $c in
      product)  printf A15 > "$r/root/etc/box_product_type"; cp "$r/stick/A15W_Update.img" "$r/stick/A15_Update.img" ;;
      firmware) printf 2025.02.25.1521 > "$r/root/etc/software_version" ;;
      chip)     echo 0xb822 > "$r/sys/bus/sdio/devices/mmc0:0001:1/device" ;;
      factory)  echo 2 > "$r/sys/module/libcomposite/parameters/mfgmode" ;;
      handmade) blob "$r/root/usr/sbin/ocbmd" 100 ;;
      damaged)  echo x >> "$r/update/tmp/ocbm/script/radio_hal.sh" ;;
      flash)    extra="FAKE_FREE_KB=100" ;;
      stick)    extra="FAKE_STICK_FREE_KB=1000" ;;
      backup)   head -c 100 "$r/dev/mtd1" > "$r/dev/mtd1.x"; mv "$r/dev/mtd1.x" "$r/dev/mtd1" ;;
    esac
    snap "$r/root" > "$r/before"
    run_once "$sh" "$r" $extra; rc=$?
    check '[ $rc = 1 ] && grep -q "^OCBM install: REFUSED" "$r/stick/OCBM_RESULT.txt" && [ "$(snap "$r/root")" = "$(cat "$r/before")" ]' \
      "refuses on $c, adapter unchanged ($(sed -n 's/.*REFUSED: //p' "$r/once.out" | cut -c1-60)...)"
  done

  echo "== $sh: same-version guard after ocbm_revert.sh, and a retry keeps the real stock script"
  r="$T/$sh-again"; make_box "$r"; run_once "$sh" "$r" >/dev/null; mkdir -p "$r/run"
  echo 'wpa_passphrase=ocbm' > "$r/root/etc/hostapd.conf"
  env ROOT="$r/root" RUN="$r/run" "$sh" "$r/root/script/ocbm_revert.sh" --no-reboot "test" > "$r/revert.out" 2>&1; rc=$?
  check '[ $rc = 0 ] && [ ! -e "$r/root/script/ocbm_mode" ] && grep -qxF "reverted version=$VER" "$r/root/etc/ocbm/last_result.txt" && grep -qx "wpa_passphrase=secret" "$r/root/etc/hostapd.conf"' \
    "the installed ocbm_revert.sh records the version and puts hostapd.conf back from identity.tgz"
  echo 2 > "$r/root/etc/ocbm/boot_attempts"
  refresh "$r"; run_once "$sh" "$r"; rc=$?
  check '[ $rc = 1 ] && grep -q "OCBM_RETRY" "$r/stick/OCBM_RESULT.txt"' "an undone version is refused without OCBM_RETRY"
  touch "$r/stick/OCBM_RETRY"; refresh "$r"; run_once "$sh" "$r"; rc=$?
  check '[ $rc = 0 ] && installed "$r"' "with OCBM_RETRY it installs again, and start_main_service.stock.sh is still stock"
  check '[ ! -e "$r/root/etc/ocbm/boot_attempts" ] && [ ! -e "$r/root/etc/ocbm/pending_result.txt" ]' "arming clears the earlier try's start counter and report"
  echo "== $sh: re-install after a revert, on a flash with no extras left to delete"
  r="$T/$sh-reinst"; make_box "$r"; run_once "$sh" "$r" >/dev/null; mkdir -p "$r/run"
  env ROOT="$r/root" RUN="$r/run" "$sh" "$r/root/script/ocbm_revert.sh" --no-reboot "test" > "$r/revert.out" 2>&1
  old_kb=0; for rel in $(bash "$HERE/make_ocbm_installer.sh" --list); do
    [ "$rel" = script/ocbm_dispatch.sh ] || old_kb=$((old_kb + $(du -sk "$r/root/$rel" | cut -f1))); done
  need=$(( $(sed -n 's/^need_kb=//p' "$T/img/tmp/ocbm/BUDGET") + 384 ))
  touch "$r/stick/OCBM_RETRY"; refresh "$r"
  B="$r/stick/OCBM_BACKUP/ABCDEF0123456789"; bk="$(cksum "$B"/*)"; echo changed >> "$r/dev/mtd2"
  run_once "$sh" "$r" FAKE_FREE_KB=$((need - old_kb * 6 / 10 + 8)) FAKE_FREE_AFTER_KB=$((need + 8)); rc=$?
  check '[ $rc = 0 ] && installed "$r" && grep -q "removed the earlier try" "$r/install.log"' \
    "the earlier try's OCBM files count as room, are removed and placed again"
  check '[ "$(cksum "$B"/*)" = "$bk" ] && grep -q "kept the earlier one" "$r/install.log"' \
    "the earlier try's flash backup is kept, not dumped over with the changed flash"
  head -c 100 "$B/mtd2.bin" > "$B/x"; mv "$B/x" "$B/mtd2.bin"
  r2="$T/$sh-badbk"; make_box "$r2"; mkdir -p "$r2/stick/OCBM_BACKUP"; cp -R "$B" "$r2/stick/OCBM_BACKUP/"
  run_once "$sh" "$r2"; rc=$?
  check '[ $rc = 0 ] && ( cd "$r2/stick/OCBM_BACKUP/ABCDEF0123456789" && sha256sum -c SHA256SUMS >/dev/null 2>&1 ) && cmp -s "$r2/dev/mtd2" "$r2/stick/OCBM_BACKUP/ABCDEF0123456789/mtd2.bin"' \
    "a damaged earlier backup is taken again"

  echo "== $sh: not enough room after the prune"
  r="$T/$sh-tight"; make_box "$r"; run_once "$sh" "$r" FAKE_FREE_AFTER_KB=10; rc=$?
  check '[ $rc = 1 ] && grep -q "^OCBM install: STOPPED" "$r/stick/OCBM_RESULT.txt" && stock_boot "$r" && [ ! -e "$r/root/script/ocbm_mode" ] && [ -e "$r/stick/A15W_Update.img.stopped" ]' \
    "stops before placing anything; the adapter still boots stock"

  echo "== $sh: power lost at each step, then the stick goes in again"
  for s in backup make_room place arm rename hand_off; do
    r="$T/$sh-pl-$s"; make_box "$r"; run_once "$sh" "$r" STOP_AT=$s; rc=$?
    check '[ $rc = 99 ] && { stock_boot "$r" || installed "$r"; }' "power lost before $s: boots stock or fully armed"
    run_once "$sh" "$r"; rc=$?
    if [ "$s" = hand_off ]; then
      check '[ $rc = 1 ] && installed "$r" && grep -q "ALREADY INSTALLED" "$r/stick/OCBM_RESULT.txt"' "re-run after power loss before $s: already installed"
    else
      check '[ $rc = 0 ] && installed "$r"' "re-run after power loss before $s completes"
    fi
  done

  echo "== $sh: over USB (Carlink3 route)"
  r="$T/$sh-ota"; make_box "$r"; rm -rf "$r/stick"; run_once "$sh" "$r" ROUTE=ota; rc=$?
  check '[ $rc = 0 ] && installed "$r" && [ -s "$r/root/etc/ocbm/identity.tgz" ] && [ ! -d "$r/stick" ]' "installs with no stick: identity archive only"
  run_finish "$sh" "$r"
  check 'grep -q "INSTALLED (trial)" "$r/result.txt" && [ ! -e "$r/rebooted" ]' "finish.sh leaves the restart to update_box_ota.sh"
done

[ "$fails" -eq 0 ] && echo "ocbm_installer_test: all passed" || { echo "ocbm_installer_test: $fails failed"; exit 1; }
