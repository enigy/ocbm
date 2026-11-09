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
# Carlinkit daemon names this host runs itself (macOS has its own usbmuxd): ocbm_finish.sh skips them.
HOST_DAEMONS="$(for n in usbmuxd mdnsd boa; do pgrep -x "$n" >/dev/null 2>&1 && printf '%s ' "$n"; done)"
# umount as the box has it, recording what it was asked to let go of (deliver() lets the stick go).
printf '#!/bin/sh\necho "$*" >> "%s/umount.calls"\n' "$T" > "$T/umount"; chmod 755 "$T/umount"
boot() {  # $1 shell, $2 dir: one start of the adapter
  env UMOUNT="$T/umount" ROOT="$2/root" RUN="$2/run" STICK="$2/stick" PROC="$2/proc" POLL=1 STICK_WAIT=2 STICK_PKG_WAIT=2 ANSWER_SECS=3 INSTALL_START=3 INSTALL_WAIT=12 FINISH_IGNORE="$HOST_DAEMONS" \
      REBOOT="touch $2/rebooted" "$1" "$2/root/script/start_main_service.sh" > "$2/boot.out" 2>&1
}
restart() { reap; rm -rf "${1:?}/run"; mkdir -p "$1/run"; rm -f "$1"/m.* "$1/rebooted"; }  # power cycle: /tmp is gone
mode_is() { [ "$(cat "$1/root/script/ocbm_mode" 2>/dev/null)" = "$2" ]; }
reverted() {  # $1 dir: on stock, recorded for once.sh's same-version guard, report pending
  [ ! -e "$1/root/script/ocbm_mode" ] && grep -qxF "reverted version=$VER" "$1/root/etc/ocbm/last_result.txt" \
    && grep -q '^OCBM install: BACK TO STOCK' "$1/root/etc/ocbm/pending_result.txt" \
    && grep -qx 'wpa_passphrase=stock-pass' "$1/root/etc/hostapd.conf" && [ ! -e "$1/root/etc/ocbm/boot_attempts" ]
}

. "$HERE/test_shells.sh"
for sh in ${SHELLS:-$(distinct_shells sh dash)}; do
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
  check 'grep -q "^self-test: PASSED, 42 s after power-on" "$d/stick/OCBM_RESULT.txt"' "the verdict says when, counted from power-on"
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
  check 'grep -qxF "$d/stick" "$T/umount.calls"' "and the stick is unmounted once the report is synced (the truck's FAT, 3 Nov)"
  d="$T/$sh-late"; make_box "$d" ""; echo "OCBM install: BACK TO STOCK" > "$d/root/etc/ocbm/pending_result.txt"
  : > "$d/proc/mounts"; rm -f "$d/stick/OCBM_RESULT.txt"; boot "$sh" "$d"; sleep 2
  check '[ -e "$d/run/not_change_mass_storage" ] && [ ! -e "$d/stick/OCBM_RESULT.txt" ]' "with no stick in, the report waits"
  echo "/dev/sda1 $d/stick vfat rw 0 0" > "$d/proc/mounts"
  check 'waitfor "[ -e $d/stick/OCBM_RESULT.txt ]" 5 && grep -q "BACK TO STOCK" "$d/stick/OCBM_RESULT.txt"' "and goes to the stick when one is plugged in"
  # A stick the report cannot be written to (full, on the Mac, 3 Nov): here OCBM_RESULT.txt.new is a
  # directory, so the write fails inside deliver(). Nothing may be lost or counted delivered.
  d="$T/$sh-full"; make_box "$d" ""; echo "OCBM install: BACK TO STOCK" > "$d/root/etc/ocbm/pending_result.txt"
  echo "an older report" > "$d/stick/OCBM_RESULT.txt"; mkdir "$d/stick/OCBM_RESULT.txt.new"; boot "$sh" "$d"; sleep 3
  check '[ "$(cat "$d/stick/OCBM_RESULT.txt")" = "an older report" ] && [ -e "$d/root/etc/ocbm/pending_result.txt" ] && [ ! -e "$d/root/etc/ocbm/delivered_result.txt" ]' \
    "a stick it cannot write to: the older report stays whole, and the report stays pending"
  rmdir "$d/stick/OCBM_RESULT.txt.new"; restart "$d"; boot "$sh" "$d"
  # All inside waitfor: delivered_result.txt appears only after deliver()'s sync, slow on macOS (the Mac, 3 Nov).
  check 'waitfor "grep -q \"^OCBM install: BACK TO STOCK\" $d/stick/OCBM_RESULT.txt && grep -q \"an older report\" $d/stick/OCBM_RESULT.txt && [ -e $d/root/etc/ocbm/delivered_result.txt ]" 15' \
    "and the next start delivers it, above the older one"
  # An install image on the stick at that start: once.sh takes the report, and deliver() must neither write nor
  # unmount the stick while it runs (the truck, 4 Nov: an install that never finished beside the delivery).
  d="$T/$sh-owed"; make_box "$d" ""; echo "OCBM install: BACK TO STOCK" > "$d/root/etc/ocbm/pending_result.txt"
  rm -f "$d/stick/OCBM_RESULT.txt"; echo img > "$d/stick/A15W_Update.img"; mkdir -p "$d/run"; : > "$d/run/ocbm_install.running"
  boot "$sh" "$d"; sleep 4
  check '[ ! -e "$d/stick/OCBM_RESULT.txt" ] && ! grep -qxF "$d/stick" "$T/umount.calls" && [ -e "$d/root/etc/ocbm/pending_result.txt" ]' \
    "with an install running from the stick, deliver() leaves the stick alone"
  mv -f "$d/root/etc/ocbm/pending_result.txt" "$d/root/etc/ocbm/delivered_result.txt"; mv "$d/stick/A15W_Update.img" "$d/stick/A15W_Update.img.used"; rm -f "$d/run/ocbm_install.running"
  check 'waitfor "grep -q \"took the report\" $d/run/ocbm_dispatch.log" 5 && [ ! -e "$d/stick/OCBM_RESULT.txt" ] && waitfor "[ ! -e $d/run/not_change_mass_storage ]" 3' \
    "and once the install took the report, writes nothing and hands sticks back to stock"
  d="$T/$sh-owed2"; make_box "$d" ""; echo "OCBM install: BACK TO STOCK" > "$d/root/etc/ocbm/pending_result.txt"
  rm -f "$d/stick/OCBM_RESULT.txt"; echo img > "$d/stick/A15W_Update.img"; boot "$sh" "$d"
  check 'waitfor "grep -q \"^OCBM install: BACK TO STOCK\" $d/stick/OCBM_RESULT.txt" 10' \
    "an image whose install never starts (not OCBM's): the report goes once the wait for it is over"

  echo "== $sh: the start counter catches a start that never finishes"
  d="$T/$sh-hang"; make_box "$d" trial; touch "$d/st.hang"
  boot "$sh" "$d"; n1="$(cat "$d/root/etc/ocbm/boot_attempts" 2>/dev/null)"
  restart "$d"; boot "$sh" "$d"; n2="$(cat "$d/root/etc/ocbm/boot_attempts" 2>/dev/null)"
  check '[ "$n1" = 1 ] && [ "$n2" = 2 ] && [ -e "$d/m.boot" ] && mode_is "$d" trial' "each start is counted before OCBM starts"
  restart "$d"; boot "$sh" "$d"
  check '[ -e "$d/m.stock" ] && [ ! -e "$d/m.boot" ] && [ ! -e "$d/rebooted" ] && reverted "$d"' \
    "the third start goes straight to stock, without a restart"
  check 'grep -q "started 2 times in a row" "$d/root/etc/ocbm/last_result.txt"' "and says why"

  echo "== $sh: a host ocbmd hears and never answers sends the trial back (the truck adapter, 2 Nov)"
  d="$T/$sh-deaf"; make_box "$d" trial; boot "$sh" "$d"
  check 'waitfor "grep -q PASSED $d/root/etc/ocbm/first_start.txt 2>/dev/null" 5' "the self-test passes: it has no host to ask"
  echo 40 > "$d/run/ocbmd_heard"   # heard at uptime 40; the fake uptime reads 42
  check '! waitfor "[ -e $d/rebooted ]" 2 && mode_is "$d" trial' "inside ANSWER_SECS of the first HELLO: the trial stays"
  echo "90.00 80.00" > "$d/proc/uptime"
  check 'waitfor "[ -e $d/rebooted ]" 5 && reverted "$d" && grep -q "ocbmd heard the host for 50 s and never answered it" "$d/root/etc/ocbm/last_result.txt"' \
    "heard 50 s with no answer: back to stock, with the reason recorded"
  d="$T/$sh-answered"; make_box "$d" trial; boot "$sh" "$d"
  echo 11 > "$d/run/ocbmd_answered"; echo 10 > "$d/run/ocbmd_heard"; echo "500.00 80.00" > "$d/proc/uptime"
  check '! waitfor "[ -e $d/rebooted ]" 3 && mode_is "$d" trial' "answered: the trial stays"
  # The truck adapter, 2 Nov 21:06: the ocbmd that hears the HELLO restarts (single-threaded I/O), and
  # the host's answer comes to the one run_ocbmd.sh starts next.
  d="$T/$sh-restarted"; make_box "$d" trial
  stand_in "$d/root/script/run_ocbmd.sh" "$d/m.run_ocbmd" \
    "[ -e '$d/run/ocbmd_heard' ] && { echo 43 > '$d/run/ocbmd_answered'; exec sleep 30; }
echo 40 > '$d/run/ocbmd_heard'; exit 1"
  boot "$sh" "$d"
  check 'waitfor "[ -e $d/run/ocbmd_answered ]" 5 && [ "$(grep -c ran "$d/m.run_ocbmd")" = 2 ]' \
    "one ocbmd hears the host and the next one answers it"
  echo "500.00 80.00" > "$d/proc/uptime"
  check '! waitfor "[ -e $d/rebooted ]" 3 && mode_is "$d" trial' "an answer to another ocbmd's HELLO keeps the trial"
  d="$T/$sh-deaf-final"; make_box "$d" final; boot "$sh" "$d"
  echo 10 > "$d/run/ocbmd_heard"; echo "500.00 80.00" > "$d/proc/uptime"
  check '! waitfor "[ -e $d/rebooted ]" 3 && mode_is "$d" final' "final has nothing to go back to: left alone"

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
    "no counter, no self-test, OCBM_RESTORE left as it is"
  check 'grep -q "OCBM on this adapter is final" "$d/stick/OCBM_RESTORE.txt" && grep -q "tools/stock_restore.sh" "$d/stick/OCBM_RESTORE.txt"' \
    "and OCBM_RESTORE.txt beside it says why, and where the way back is now"
  # Scott, 3 Nov: a final adapter goes back to stock from the stick itself (ocbm_restore.sh) at its start.
  d="$T/$sh-final-rs"; make_box "$d" final
  stand_in "$d/root/script/ocbm_restore.sh" "$d/m.restore" "mv -f '$d/stick/OCBM_RESTORE' '$d/stick/OCBM_RESTORE.done'"   # as the real one does
  touch "$d/stick/OCBM_RESTORE"; boot "$sh" "$d"
  check 'waitfor "[ -e $d/m.restore ]" 5 && [ ! -e "$d/stick/OCBM_RESTORE.txt" ]' "a final start with OCBM_RESTORE on the stick runs ocbm_restore.sh"
  # The dev adapter, 3 Nov: the watcher of the boot the finish ran in outlived the finish.
  d="$T/$sh-final-late"; make_box "$d" trial; boot "$sh" "$d"
  check 'waitfor "grep -q PASSED $d/root/etc/ocbm/first_start.txt 2>/dev/null" 5' "a trial boot, its restore watcher running"
  echo final > "$d/root/script/ocbm_mode"; touch "$d/stick/OCBM_RESTORE"   # the finish ran; then the stick
  check 'waitfor "[ -e $d/stick/OCBM_RESTORE.txt ]" 5 && [ -e "$d/stick/OCBM_RESTORE" ] && [ ! -e "$d/stick/OCBM_RESTORE.done" ] && mode_is "$d" final && [ ! -e "$d/rebooted" ]' \
    "once the mode is final it leaves OCBM_RESTORE alone and writes the note, with no revert"

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

  echo "== $sh: ocbm_finish.sh (Stage C)"
  fin_box() {  # $1 dir, $2 mode: a converted adapter that still carries a (small) Carlinkit stack
    local R="$1/root" f
    make_box "$1" "$2"
    cp "$INST/ocbm_finish.sh" "$R/script/ocbm_finish.sh"; chmod 755 "$R/script/ocbm_finish.sh"
    mkdir -p "$R/usr/lib" "$R/lib" "$R/bin" "$R/etc/mdev" "$R/etc/boa/www" "$R/etc/dbus-1/system.d"
    for f in AppleCarPlay ARMadb-driver mdnsd boa riddle_top i2cset; do printf 'elf' > "$R/usr/sbin/$f"; done
    printf 'hostapd: libssl.so.1.1 libc.so.6\n' > "$R/usr/sbin/hostapd"
    printf 'busybox: libc.so.6 libm.so.6 libcrypt.so.1 libgcc_s.so.1 libpthread.so.0 libdl.so.2 librt.so.1 libresolv.so.2 libutil.so.1 libnsl.so.1\n' > "$R/bin/busybox"
    for f in usr/lib/libssl.so.1.1 usr/lib/libdmsdp.so lib/crt1.o etc/BoxHelper.apk etc/rgbLight.conf \
             etc/dbus-1/system.d/riddle.conf etc/boa/www/index.html etc/riddle.conf; do printf 'x' > "$R/$f"; done
    for f in update_box.sh update_box_ota.sh quickly_update.sh check_update.sh smart_copy.sh phone_link_deamon.sh init_audio_codec.sh init_bluetooth_wifi.sh; do printf '#!/bin/sh\n' > "$R/script/$f"; done
    printf '#!/bin/sh\nif [ -d /sys/block/*/$MDEV ] ; then\n\tmount /dev/$MDEV /mnt/UPAN -t vfat -o utf8=1\n\t/script/update_box.sh /dev/$MDEV >> /dev/console &\nfi\n' \
      > "$R/etc/mdev/udisk_insert.sh"
  }
  run_finish() {  # $1 dir, then arguments: output in $1/fin.out
    local d="$1"; shift
    env ROOT="$d/root" RUN="$d/run" GRACE=2 POLL=1 FINISH_IGNORE="$HOST_DAEMONS" "$sh" "$d/root/script/ocbm_finish.sh" "$@" > "$d/fin.out" 2>&1
  }
  stock_left() { [ -e "$1/root/usr/sbin/AppleCarPlay" ] && [ -e "$1/root/script/update_box.sh" ] && [ -e "$1/root/script/start_main_service.stock.sh" ]; }
  d="$T/$sh-fin-trial"; fin_box "$d" trial; run_finish "$d" --after-session
  check 'mode_is "$d" trial && stock_left "$d"' "in the trial it does nothing"
  d="$T/$sh-fin-back"; fin_box "$d" confirmed; echo 1 > "$d/run/host_present"; run_finish "$d" --after-session
  check 'mode_is "$d" confirmed && stock_left "$d" && grep -q "an app is back" "$d/fin.out"' "an app back within the grace: not yet"
  d="$T/$sh-fin"; fin_box "$d" confirmed; echo 0 > "$d/run/host_present"
  # The truck's hand-install leftovers (3 Nov): dropbear links libz, and nothing else does.
  R="$d/root"; mkdir -p "$R/usr/libexec" "$R/root/.ssh"
  printf 'dropbear: libz.so.1 libc.so.6\n' > "$R/usr/sbin/dropbear"; printf 'sftp\n' > "$R/usr/libexec/sftp-server"
  printf 'key\n' > "$R/root/.ssh/authorized_keys"; printf 'ls\n' > "$R/.ash_history"; printf '#!/bin/sh\n' > "$R/script/custom_init.sh"
  printf 'z' > "$R/lib/libz.so.1.2.11"; ln -s libz.so.1.2.11 "$R/lib/libz.so.1"
  run_finish "$d" --after-session; rc=$?
  check '[ $rc = 0 ] && mode_is "$d" final && [ ! -e "$R/etc/ocbm/finish_pending" ]' "after the grace: mode final"
  check '[ ! -e "$R/usr/sbin/AppleCarPlay" ] && [ ! -e "$R/usr/sbin/mdnsd" ] && [ ! -e "$R/usr/sbin/boa" ] && [ ! -e "$R/script/phone_link_deamon.sh" ] && [ ! -e "$R/script/update_box_ota.sh" ] && [ ! -e "$R/etc/boa" ] && [ ! -e "$R/etc/BoxHelper.apk" ] && [ ! -e "$R/script/start_main_service.stock.sh" ]' \
    "the Carlinkit programs, scripts, web pages and the stock start script are gone"
  check '[ -e "$R/script/update_box.sh" ] && [ -e "$R/script/quickly_update.sh" ] && [ -e "$R/script/check_update.sh" ] && [ -e "$R/script/smart_copy.sh" ] && grep -q "^[[:space:]]*/script/update_box.sh" "$R/etc/mdev/udisk_insert.sh"' \
    "the stock stick updater stays, called by the stick handler: a final adapter runs an image as stock does (Scott, 3 Nov)"
  check '[ -e "$R/usr/sbin/hostapd" ] && [ -e "$R/usr/sbin/i2cset" ] && [ -e "$R/etc/riddle.conf" ] && [ -e "$R/script/init_bluetooth_wifi.sh" ] && [ -x "$R/usr/sbin/ocbmd" ] && [ -e "$R/script/start_main_service.sh" ]' \
    "the radio stack, i2c tools, riddle.conf, OCBM and the start script stay"
  check '[ -e "$R/usr/lib/libssl.so.1.1" ] && [ ! -e "$R/usr/lib/libdmsdp.so" ] && [ ! -e "$R/lib/crt1.o" ]' \
    "libraries: a still-named one stays (libssl for hostapd), unnamed ones go"
  check 'n=0; for f in usr/sbin/dropbear usr/libexec/sftp-server root/.ssh/authorized_keys .ash_history script/custom_init.sh lib/libz.so.1.2.11 lib/libz.so.1; do { [ -e "$R/$f" ] || [ -L "$R/$f" ]; } && n=$((n+1)); done; [ $n = 0 ] && grep -qx /usr/sbin/dropbear "$R/etc/ocbm/finished.txt"' \
    "a hand conversion's leftovers go too, libz once dropbear no longer names it (the truck, 3 Nov)"
  check 'grep -qx /usr/sbin/AppleCarPlay "$R/etc/ocbm/finished.txt" && grep -qx /usr/lib/libdmsdp.so "$R/etc/ocbm/finished.txt"' "finished.txt records what went"
  env ROOT="$R" RUN="$d/run" REBOOT="touch $d/rebooted" "$sh" "$R/script/ocbm_revert.sh" "test" > "$d/rv.out" 2>&1; rc=$?
  check '[ $rc = 1 ] && mode_is "$d" final && [ ! -e "$d/rebooted" ]' "afterwards there is no going back"
  d="$T/$sh-fin-thin"; fin_box "$d" confirmed; printf 'static\n' > "$d/root/bin/busybox"; printf 'hostapd\n' > "$d/root/usr/sbin/hostapd"
  run_finish "$d"; check 'mode_is "$d" final && [ -e "$d/root/usr/lib/libdmsdp.so" ] && [ -e "$d/root/usr/lib/libssl.so.1.1" ] && grep -q "the libraries stay" "$d/fin.out"' \
    "a library scan that finds no libc.so removes no library, and the finish still completes"
  d="$T/$sh-fin-busy"; fin_box "$d" confirmed; pname="riddle_top"
  cp "$(command -v sleep)" "$T/$pname"; [ "$(uname)" = Darwin ] && codesign -f -s - "$T/$pname" 2>/dev/null
  "$T/$pname" 20 & busy=$!; sleep 0.3
  run_finish "$d"; rc=$?; kill "$busy" 2>/dev/null; wait "$busy" 2>/dev/null
  check '[ $rc = 1 ] && mode_is "$d" confirmed && stock_left "$d" && grep -q "running: riddle_top" "$d/fin.out"' "refuses while a Carlinkit daemon runs, nothing removed"
  d="$T/$sh-fin-noocbmd"; fin_box "$d" confirmed; rm -f "$d/root/usr/sbin/ocbmd"; run_finish "$d"; rc=$?
  check '[ $rc = 1 ] && mode_is "$d" confirmed && stock_left "$d"' "refuses without ocbmd, nothing removed"
  d="$T/$sh-fin-lock"; fin_box "$d" confirmed; mkdir "$d/run/ocbm_mode.lock"; run_finish "$d"
  check 'mode_is "$d" confirmed && stock_left "$d" && [ -d "$d/run/ocbm_mode.lock" ]' "waits its turn behind a revert, and leaves that revert's lock alone"
  env ROOT="$d/root" RUN="$d/run" REBOOT="touch $d/rebooted" "$sh" "$d/root/script/ocbm_revert.sh" "test" > "$d/rv.out" 2>&1; rc=$?
  check '[ $rc = 1 ] && mode_is "$d" confirmed' "and a revert that finds the lock taken refuses (exit 1)"
  d="$T/$sh-fin-cut"; fin_box "$d" confirmed
  env ROOT="$d/root" RUN="$d/run" FINISH_IGNORE="$HOST_DAEMONS" STOP_AT=deletes "$sh" "$d/root/script/ocbm_finish.sh" > "$d/fin.out" 2>&1; rc=$?
  check '[ $rc = 99 ] && mode_is "$d" final && [ -e "$d/root/etc/ocbm/finish_pending" ] && stock_left "$d"' \
    "power lost before the first delete: already final, so no start can hand the boot to a half-removed stock"
  boot "$sh" "$d"
  check 'waitfor "[ ! -e $d/root/etc/ocbm/finish_pending ]" 5 && [ ! -e "$d/root/usr/sbin/AppleCarPlay" ] && mode_is "$d" final' \
    "a start in final with finish_pending (power cut mid-finish) completes it"
  d="$T/$sh-fin-boot"; fin_box "$d" confirmed; boot "$sh" "$d"
  check 'waitfor "mode_is $d final" 8 && waitfor "[ ! -e $d/root/usr/sbin/AppleCarPlay ]" 5' \
    "a start in confirmed (the power went off during the session) finishes once the self-test passes"
  d="$T/$sh-fin-fail"; fin_box "$d" confirmed; touch "$d/st.fail"; boot "$sh" "$d"
  check 'waitfor "[ -e $d/rebooted ]" 5 && reverted "$d" && stock_left "$d"' "a start in confirmed that fails its self-test goes back to stock instead"
  reap

  echo "== $sh: ocbm_selftest.sh"
  d="$T/$sh-st"; G="$d/sys/class/android_usb_accessory/android0"
  mkdir -p "$G" "$d/sys/bus/sdio/devices/mmc0:0001:1" "$d/dev" "$d/run" "$d/bin"
  echo 0x9159 > "$d/sys/bus/sdio/devices/mmc0:0001:1/device"
  printf '#!/bin/sh\nprintf "RADIO_CHIP=nxp_iw416\\nRADIO_SDIO_DEVICE=%%s\\nRADIO_BACKEND=mapped\\nRADIO_WIRELESS=%%s\\n" "$(cat %s 2>/dev/null || echo 0x9159)" "$(cat %s 2>/dev/null || echo yes)" > "$RADIO_CAPS"\n' \
    "$d/radio_id" "$d/radio_wl" > "$d/radio_detect.sh"
  gadget() { echo "$1" > "$G/functions"; echo "${3:-1314}" > "$G/idVendor"; echo "$2" > "$G/idProduct"; echo 1 > "$G/enable"; echo CONFIGURED > "$G/state"; }
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
  gadget accessory 2d00 12d1; selftest; rc=$?
  check '[ $rc = 1 ] && tail -1 "$d/st.out" | grep -q "the USB gadget is not the OCBM accessory$"' "fails on the module's own vendor id (12d1:2d00), which host apps do not match"
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

# ocbm_finish.sh's strip lists were kept equal to tools/ncm_base_install.sh's; that hand installer left main
# (decision 4, 8 Nov), so they are the only copy: each list is there, and names the stock programs it must.
echo "== ocbm_finish.sh's strip lists"
lists() { bash -c 'eval "$(sed -n "/^SBIN_A=/,/^KILL_PROCS=/p;/^KILL_PROCS=/,/^[A-Z_]*=\"[^\"]*\"$/p" "$1" | sed "/^KILL_LOOPS=/d")" 2>/dev/null
  for v in SBIN_A SCRIPT_A ETC_A LIB_VENDOR LIB_DEV KILL_PROCS; do eval "echo $v: \$$v"; done' _ "$1"; }
L="$(lists "$INST/ocbm_finish.sh")"
check '[ "$(echo "$L" | grep -cE "^[A-Z_]+: [^ ]")" = 6 ]' "SBIN_A SCRIPT_A ETC_A LIB_VENDOR LIB_DEV KILL_PROCS are all set"
check 'echo "$L" | grep -q "^SBIN_A: AppleCarPlay " && echo "$L" | grep "^KILL_PROCS:" | grep -q " AppleCarPlay"' "they name Carlinkit's CarPlay program"

[ "$fails" -eq 0 ] && echo "ocbm_stageb_test: all passed" || { echo "ocbm_stageb_test: $fails failed"; exit 1; }
