#!/usr/bin/env bash
# ocbm_stageb_test.sh — Stage B of the one-file install, on the host: the dispatcher
# (ccpa/installer/ocbm_dispatch.sh, which becomes /script/start_main_service.sh), ocbm_revert.sh and
# ocbm_selftest.sh, against synthetic converted adapters (ROOT, RUN, STICK, PROC, SYS and DEVDIR point
# into a scratch tree). The stock start script, ocbm_boot.sh and the respawn wrappers are stand-ins
# that leave a mark; the revert and the self-test are the real scripts. Under every POSIX shell found
# (sh, dash), because the box runs busybox ash. Needs no adapter and no box build.
#
#   tools/ocbm_stageb_test.sh
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
INST="$REPO/ccpa/installer"
T="$(mktemp -d "${TMPDIR:-/tmp}/ocbm_stageb_test.XXXXXX")"
reap() { pkill -f "$T/" 2>/dev/null; sleep 0.2; }
trap 'reap; rm -rf "$T"' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }
waitfor() {  # $1 condition, $2 seconds
  local i=0
  until eval "$1"; do i=$((i + 1)); [ "$i" -ge $(($2 * 10)) ] && return 1; sleep 0.1; done
}
VER="1.0.0 ocbm test"

stand_in() {  # $1 path, $2 mark file, $3 extra shell line
  printf '#!/bin/sh\necho ran >> "%s"\n%s\n' "$2" "${3:-exit 0}" > "$1"; chmod 755 "$1"
}
make_box() {  # $1 dir, $2 mode ('' = stock): a converted adapter, stick in and mounted
  local d="$1" R="$1/root" k
  mkdir -p "$R/script" "$R/etc/ocbm" "$R/usr/sbin" "$d/run" "$d/stick" "$d/proc" "$d/id/etc"
  cp "$INST/ocbm_dispatch.sh" "$R/script/start_main_service.sh"
  cp "$INST/ocbm_revert.sh" "$R/script/ocbm_revert.sh"
  chmod 755 "$R/script/start_main_service.sh" "$R/script/ocbm_revert.sh"
  stand_in "$R/script/start_main_service.stock.sh" "$d/m.stock"
  stand_in "$R/script/ocbm_boot.sh" "$d/m.boot"
  stand_in "$R/script/init_gpio.sh" "$d/m.gpio"
  for k in run_ocbmd run_supervisor; do stand_in "$R/script/$k.sh" "$d/m.$k" 'exec sleep 30'; done
  # the self-test stand-in: fails when $d/st.fail exists, hangs when $d/st.hang exists
  stand_in "$R/script/ocbm_selftest.sh" "$d/m.selftest" \
    "[ -e '$d/st.hang' ] && exec sleep 30
[ -e '$d/st.fail' ] && { echo 'FAILED after 30 s: ocbmd is not running'; exit 1; }
echo 'PASSED in 3 s'"
  printf 'x' > "$R/usr/sbin/ocbmd"; chmod 755 "$R/usr/sbin/ocbmd"
  [ -n "$2" ] && echo "$2" > "$R/script/ocbm_mode"
  echo "$VER" > "$R/etc/ocbm/installed_version"
  echo 'wpa_passphrase=stock-pass' > "$d/id/etc/hostapd.conf"
  (cd "$d/id" && tar -cf - etc/hostapd.conf) | gzip -c > "$R/etc/ocbm/identity.tgz"
  echo 'wpa_passphrase=ocbm-pass' > "$R/etc/hostapd.conf"
  echo "/dev/sda1 $d/stick vfat rw 0 0" > "$d/proc/mounts"
  echo "42.17 80.00" > "$d/proc/uptime"
  printf 'OCBM install: INSTALLED (trial)\ninstaller: %s\n' "$VER" > "$d/stick/OCBM_RESULT.txt"
}
boot() {  # $1 shell, $2 dir: one start of the adapter
  env ROOT="$2/root" RUN="$2/run" STICK="$2/stick" PROC="$2/proc" POLL=1 STICK_WAIT=2 \
      REBOOT="touch $2/rebooted" "$1" "$2/root/script/start_main_service.sh" > "$2/boot.out" 2>&1
}
restart() { reap; rm -rf "${1:?}/run"; mkdir -p "$1/run"; rm -f "$1"/m.* "$1/rebooted"; }  # power cycle: /tmp is gone
mode_is() { [ "$(cat "$1/root/script/ocbm_mode" 2>/dev/null)" = "$2" ]; }
reverted() {  # $1 dir: on stock, recorded for once.sh's same-version guard, report pending
  [ ! -e "$1/root/script/ocbm_mode" ] && grep -qxF "reverted version=$VER" "$1/root/etc/ocbm/last_result.txt" \
    && grep -q '^OCBM install: BACK TO STOCK' "$1/root/etc/ocbm/pending_result.txt" \
    && grep -qx 'wpa_passphrase=stock-pass' "$1/root/etc/hostapd.conf" && [ ! -e "$1/root/etc/ocbm/boot_attempts" ]
}

for sh in ${SHELLS:-sh dash}; do
  command -v "$sh" >/dev/null 2>&1 || { echo "== $sh: not installed, skipped"; continue; }

  echo "== $sh: stock mode hands the boot to Carlinkit's script"
  d="$T/$sh-stock"; make_box "$d" ""; boot "$sh" "$d"
  check '[ -e "$d/m.stock" ] && [ ! -e "$d/m.boot" ] && [ ! -e "$d/run/not_change_mass_storage" ]' \
    "start_main_service.stock.sh runs, OCBM does not, stick sharing is left to stock"
  d="$T/$sh-odd"; make_box "$d" "sideways"; boot "$sh" "$d"
  check '[ -e "$d/m.stock" ] && [ ! -e "$d/m.boot" ]' "an unknown mode starts stock"

  echo "== $sh: a trial start that passes"
  d="$T/$sh-trial"; make_box "$d" trial; boot "$sh" "$d"
  check '[ -e "$d/run/not_change_mass_storage" ] && [ -e "$d/m.boot" ] && [ ! -e "$d/m.stock" ]' \
    "OCBM starts, with stick sharing turned off first"
  check 'waitfor "[ -e $d/m.run_ocbmd ] && [ -e $d/m.run_supervisor ] && [ -e $d/m.gpio ]" 5' \
    "the ocbmd and supervisor respawn loops run, and stock init_gpio.sh"
  check 'waitfor "grep -q \"^--- first start\" $d/stick/OCBM_RESULT.txt" 5' \
    "the first start adds its verdict to OCBM_RESULT.txt on the stick"
  check '[ ! -e "$d/root/etc/ocbm/boot_attempts" ] && mode_is "$d" trial && [ ! -e "$d/rebooted" ] && grep -q "PASSED" "$d/root/etc/ocbm/first_start.txt"' \
    "a pass clears the start counter and stays in the trial"
  restart "$d"; boot "$sh" "$d"; sleep 3
  check '[ "$(grep -c "^--- first start" "$d/stick/OCBM_RESULT.txt")" = 1 ]' "later starts do not add it again"
  d="$T/$sh-conf"; make_box "$d" confirmed; boot "$sh" "$d"
  check 'waitfor "[ -e $d/m.selftest ]" 5 && [ -e "$d/m.boot" ]' "confirmed starts OCBM and still runs the self-test"

  echo "== $sh: a failed self-test goes back to stock"
  d="$T/$sh-fail"; make_box "$d" trial; touch "$d/st.fail"; boot "$sh" "$d"
  check 'waitfor "[ -e $d/rebooted ]" 5 && reverted "$d"' \
    "revert: mode removed, hostapd.conf put back, 'reverted version=' recorded, report pending, restart"
  check 'grep -q "self-test failed (FAILED after 30 s: ocbmd is not running)" "$d/root/etc/ocbm/last_result.txt" && grep -q "^FAILED" "$d/root/etc/ocbm/pending_result.txt"' \
    "the reason and the self-test output are in the record and the report"
  restart "$d"; boot "$sh" "$d"
  check '[ -e "$d/m.stock" ] && [ ! -e "$d/m.boot" ]' "the next start is stock"
  check 'waitfor "grep -q \"^OCBM install: BACK TO STOCK\" $d/stick/OCBM_RESULT.txt" 5 && grep -q "^--- before that" "$d/stick/OCBM_RESULT.txt"' \
    "the report goes to the stick, above the install's result"
  check 'waitfor "[ ! -e $d/run/not_change_mass_storage ]" 3 && [ -e "$d/root/etc/ocbm/delivered_result.txt" ] && [ ! -e "$d/root/etc/ocbm/pending_result.txt" ]' \
    "delivered once; later sticks are shared with the car again"
  d="$T/$sh-late"; make_box "$d" ""; echo "OCBM install: BACK TO STOCK" > "$d/root/etc/ocbm/pending_result.txt"
  : > "$d/proc/mounts"; rm -f "$d/stick/OCBM_RESULT.txt"; boot "$sh" "$d"; sleep 2
  check '[ -e "$d/run/not_change_mass_storage" ] && [ ! -e "$d/stick/OCBM_RESULT.txt" ]' "with no stick in, the report waits"
  echo "/dev/sda1 $d/stick vfat rw 0 0" > "$d/proc/mounts"
  check 'waitfor "[ -e $d/stick/OCBM_RESULT.txt ]" 5 && grep -q "BACK TO STOCK" "$d/stick/OCBM_RESULT.txt"' "and goes to the stick when one is plugged in"

  echo "== $sh: the start counter catches a start that never finishes"
  d="$T/$sh-hang"; make_box "$d" trial; touch "$d/st.hang"
  boot "$sh" "$d"; n1="$(cat "$d/root/etc/ocbm/boot_attempts" 2>/dev/null)"
  restart "$d"; boot "$sh" "$d"; n2="$(cat "$d/root/etc/ocbm/boot_attempts" 2>/dev/null)"
  check '[ "$n1" = 1 ] && [ "$n2" = 2 ] && [ -e "$d/m.boot" ] && mode_is "$d" trial' "each start is counted before OCBM starts"
  restart "$d"; boot "$sh" "$d"
  check '[ -e "$d/m.stock" ] && [ ! -e "$d/m.boot" ] && [ ! -e "$d/rebooted" ] && reverted "$d"' \
    "the third start goes straight to stock, without a restart"
  check 'grep -q "started 2 times in a row" "$d/root/etc/ocbm/last_result.txt"' "and says why"

  echo "== $sh: OCBM_RESTORE on a stick"
  d="$T/$sh-restore"; make_box "$d" trial; touch "$d/st.hang" "$d/stick/OCBM_RESTORE"; boot "$sh" "$d"
  check 'waitfor "[ -e $d/rebooted ]" 5 && reverted "$d" && grep -q OCBM_RESTORE "$d/root/etc/ocbm/last_result.txt"' "goes back to stock and restarts"
  check '[ ! -e "$d/stick/OCBM_RESTORE" ] && [ -e "$d/stick/OCBM_RESTORE.done" ]' "the file is used once (renamed .done)"
  d="$T/$sh-restore2"; make_box "$d" trial; touch "$d/st.hang"; boot "$sh" "$d"; sleep 2
  touch "$d/stick/OCBM_RESTORE"
  check 'waitfor "[ -e $d/rebooted ]" 5 && reverted "$d"' "also when the stick goes in later"

  echo "== $sh: missing pieces"
  d="$T/$sh-noocbmd"; make_box "$d" trial; rm -f "$d/root/usr/sbin/ocbmd"; boot "$sh" "$d"
  check '[ -e "$d/m.stock" ] && [ ! -e "$d/m.boot" ] && reverted "$d" && grep -q "/usr/sbin/ocbmd is missing" "$d/root/etc/ocbm/last_result.txt"' \
    "no ocbmd: straight to stock"
  d="$T/$sh-norevert"; make_box "$d" trial; rm -f "$d/root/usr/sbin/ocbmd" "$d/root/script/ocbm_revert.sh"; boot "$sh" "$d"
  check '[ -e "$d/m.stock" ] && [ ! -e "$d/root/script/ocbm_mode" ] && grep -qxF "reverted version=$VER" "$d/root/etc/ocbm/last_result.txt"' \
    "no revert script either: the dispatcher does the least of it itself"
  d="$T/$sh-nostock"; make_box "$d" trial; rm -f "$d/root/script/start_main_service.stock.sh"; touch "$d/st.fail" "$d/stick/OCBM_RESTORE"
  boot "$sh" "$d"; sleep 3
  check '[ -e "$d/m.boot" ] && mode_is "$d" trial && [ ! -e "$d/rebooted" ] && [ ! -e "$d/m.selftest" ]' \
    "no stock script: OCBM starts with no way back offered"

  echo "== $sh: final mode"
  d="$T/$sh-final"; make_box "$d" final; touch "$d/st.fail" "$d/stick/OCBM_RESTORE"; boot "$sh" "$d"
  check 'waitfor "[ -e $d/m.run_ocbmd ]" 5 && [ -e "$d/m.boot" ]' "OCBM starts and is respawned"
  sleep 3
  check '[ ! -e "$d/root/etc/ocbm/boot_attempts" ] && [ ! -e "$d/m.selftest" ] && mode_is "$d" final && [ ! -e "$d/rebooted" ] && [ -e "$d/stick/OCBM_RESTORE" ]' \
    "no counter, no self-test, OCBM_RESTORE ignored"

  echo "== $sh: ocbm_revert.sh"
  d="$T/$sh-rv"; make_box "$d" final
  env ROOT="$d/root" RUN="$d/run" REBOOT="touch $d/rebooted" "$sh" "$d/root/script/ocbm_revert.sh" "test" > "$d/rv.out" 2>&1; rc=$?
  check '[ $rc = 1 ] && mode_is "$d" final && [ ! -e "$d/root/etc/ocbm/last_result.txt" ] && [ ! -e "$d/rebooted" ]' "refuses in final mode, nothing changed"
  echo trial > "$d/root/script/ocbm_mode"; rm -f "$d/root/script/start_main_service.stock.sh"
  env ROOT="$d/root" RUN="$d/run" REBOOT="touch $d/rebooted" "$sh" "$d/root/script/ocbm_revert.sh" "test" > "$d/rv.out" 2>&1; rc=$?
  check '[ $rc = 1 ] && mode_is "$d" trial && [ ! -e "$d/rebooted" ]' "refuses without the stock start script"
  d="$T/$sh-rv2"; make_box "$d" ""
  env ROOT="$d/root" RUN="$d/run" REBOOT="touch $d/rebooted" "$sh" "$d/root/script/ocbm_revert.sh" "test" > "$d/rv.out" 2>&1; rc=$?
  check '[ $rc = 0 ] && [ ! -e "$d/root/etc/ocbm/last_result.txt" ] && [ ! -e "$d/rebooted" ]' "on stock already: nothing to do"

  echo "== $sh: ocbm_selftest.sh"
  d="$T/$sh-st"; G="$d/sys/class/android_usb_accessory/android0"
  mkdir -p "$G" "$d/sys/bus/sdio/devices/mmc0:0001:1" "$d/dev" "$d/run" "$d/bin"
  echo 0x9159 > "$d/sys/bus/sdio/devices/mmc0:0001:1/device"
  printf '#!/bin/sh\nprintf "RADIO_CHIP=nxp_iw416\\nRADIO_SDIO_DEVICE=%%s\\nRADIO_BACKEND=mapped\\nRADIO_WIRELESS=%%s\\n" "$(cat %s 2>/dev/null || echo 0x9159)" "$(cat %s 2>/dev/null || echo yes)" > "$RADIO_CAPS"\n' \
    "$d/radio_id" "$d/radio_wl" > "$d/radio_detect.sh"
  gadget() { echo "$1" > "$G/functions"; echo "$2" > "$G/idProduct"; echo 1 > "$G/enable"; echo CONFIGURED > "$G/state"; }
  name="ocbmdt$$"; cp "$(command -v sleep)" "$d/bin/$name"   # a process that pgrep -x sees as $name
  [ "$(uname)" = Darwin ] && codesign -f -s - "$d/bin/$name" 2>/dev/null   # macOS kills a copied system binary
  selftest() {  # env overrides, then: run it, output in $d/st.out
    env SYS="$d/sys" DEVDIR="$d/dev" RUN="$d/run" OCBMD="$name" RADIO_DETECT="$d/radio_detect.sh" SECS=1 "$@" \
      "$sh" "$INST/ocbm_selftest.sh" > "$d/st.out" 2>&1
  }
  gadget accessory 2d00; : > "$d/dev/usb_accessory"; "$d/bin/$name" 30 & ocbmd_pid=$!
  selftest; rc=$?
  check '[ $rc = 0 ] && [ "$(tail -1 "$d/st.out")" = "PASSED in 0 s" ] && grep -q "running (pid" "$d/st.out"' "passes on a working adapter"
  gadget mass_storage a4a5; selftest; rc=$?
  check '[ $rc = 1 ] && tail -1 "$d/st.out" | grep -q "^FAILED after 1 s: the USB gadget is not the OCBM accessory$"' "fails while the port is in stick sharing"
  gadget accessory 2d00; rm -f "$d/dev/usb_accessory"; selftest; rc=$?
  check '[ $rc = 1 ] && tail -1 "$d/st.out" | grep -q "no /dev/usb_accessory$"' "fails without /dev/usb_accessory"
  : > "$d/dev/usb_accessory"; kill "$ocbmd_pid" 2>/dev/null; wait "$ocbmd_pid" 2>/dev/null; selftest; rc=$?
  check '[ $rc = 1 ] && tail -1 "$d/st.out" | grep -q "ocbmd is not running$"' "fails when ocbmd is not running"
  "$d/bin/$name" 30 & ocbmd_pid=$!
  echo 0xc822 > "$d/radio_id"; selftest; rc=$?
  check '[ $rc = 1 ] && tail -1 "$d/st.out" | grep -q "the radio bring-up was not found$"' "fails when the radio descriptor is not this unit's"
  rm -f "$d/radio_id"; echo no > "$d/radio_wl"; selftest; rc=$?
  check '[ $rc = 1 ] && tail -1 "$d/st.out" | grep -q "the radio bring-up was not found$"' "fails when the unit carries no wireless bring-up"
  rm -f "$d/radio_wl"
  rm -f "$d/radio_id" "$d/dev/usb_accessory"; ( sleep 2; : > "$d/dev/usb_accessory" ) &
  selftest SECS=6; rc=$?
  check '[ $rc = 0 ] && tail -1 "$d/st.out" | grep -qE "^PASSED in [2-4] s$"' "waits for a slow start ($(tail -1 "$d/st.out"))"
  kill "$ocbmd_pid" 2>/dev/null; wait "$ocbmd_pid" 2>/dev/null
  reap
done

[ "$fails" -eq 0 ] && echo "ocbm_stageb_test: all passed" || { echo "ocbm_stageb_test: $fails failed"; exit 1; }
