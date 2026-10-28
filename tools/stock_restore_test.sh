#!/bin/sh
# stock_restore_test.sh — exercise tools/stock_restore_box.sh against synthetic trees, on the host.
#
# Builds a fake "backup" rootfs and a fake "converted box" derived from it (Carlinkit files deleted,
# boot scripts replaced, OCBM binaries added, links retargeted, runtime files, NCM flags), then runs
# plan / apply / verify with ROOT pointed at the scratch tree. Needs no adapter and no real backup.
# Runs the payload under every POSIX shell it finds (sh, dash), because the box runs busybox ash.
#
#   tools/stock_restore_test.sh
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
PAYLOAD="${STOCK_RESTORE_PAYLOAD:-$HERE/stock_restore_box.sh}"   # override to mutation-test the test
T="$(mktemp -d "${TMPDIR:-/tmp}/stock_restore_test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

blob() { head -c "$2" /dev/urandom > "$1"; }   # $1 file, $2 bytes

make_src() {  # $1 dir: the pre-conversion backup
  s="$1"
  mkdir -p "$s/bin" "$s/usr/sbin" "$s/usr/lib" "$s/script" "$s/etc/init.d" "$s/etc/mdev" \
           "$s/etc/dropbear" "$s/root/.ssh" "$s/var/run" "$s/var/log" "$s/lib/firmware/nxp" \
           "$s/etc/boa/www" "$s/proc" "$s/tmp"
  blob "$s/bin/busybox" 60000; chmod 755 "$s/bin/busybox"
  ln -s busybox "$s/bin/sh"; ln -s busybox "$s/bin/ls"
  for b in ARMadb-driver AppleCarPlay ARMiPhoneIAP2 hfpd; do blob "$s/usr/sbin/$b" 40000; chmod 755 "$s/usr/sbin/$b"; done
  blob "$s/usr/lib/libhicar.so" 30000
  blob "$s/usr/lib/libssl.so.1.1" 20000; ln -s libssl.so.1.1 "$s/usr/lib/libssl.so"
  blob "$s/usr/lib/libz.so.1.2.11" 9000; ln -s libz.so.1.2.11 "$s/usr/lib/libz.so"
  echo "stock start_main_service" > "$s/script/start_main_service.sh"
  echo "ncm cfw custom_init"      > "$s/script/custom_init.sh"
  echo "stock rcS"                > "$s/etc/init.d/rcS"
  echo "stock inittab"            > "$s/etc/inittab"
  echo "stock udisk_insert"       > "$s/etc/mdev/udisk_insert.sh"
  chmod 755 "$s/script/start_main_service.sh" "$s/script/custom_init.sh" "$s/etc/init.d/rcS"
  : > "$s/script/ncm_only"
  echo "wpa_passphrase=12345678"  > "$s/etc/hostapd.conf"
  echo "root::0:0:99999:7:::"     > "$s/etc/shadow"
  echo "root:x:0:0:root:/root:/bin/sh" > "$s/etc/passwd"
  blob "$s/etc/dropbear/dropbear_rsa_host_key" 800
  echo "ssh-ed25519 AAAA test"    > "$s/root/.ssh/authorized_keys"
  echo 123                        > "$s/var/run/dropbear.pid"
  echo "old log"                  > "$s/var/log/box.log"
  blob "$s/lib/firmware/nxp/sd8987.bin" 5000
  echo "<html>stock</html>"       > "$s/etc/boa/www/index.html"
  echo "should never be read"     > "$s/proc/version"
  echo "should never be read"     > "$s/tmp/scratch"
}

make_box() {  # $1 backup dir, $2 box dir: what the converted adapter looks like
  cp -pR "$1" "$2"
  b="$2"
  rm -f "$b/usr/sbin/ARMadb-driver" "$b/usr/lib/libhicar.so" "$b/usr/lib/libssl.so"
  rm -rf "$b/etc/boa"
  echo "owned start_main_service" > "$b/script/start_main_service.sh"
  echo "owned rcS"                 > "$b/etc/init.d/rcS"
  echo "owned inittab + ocbmd"     > "$b/etc/inittab"
  echo "wpa_passphrase=random20"   > "$b/etc/hostapd.conf"
  echo "root:\$1\$changed:0:0:::"  > "$b/etc/shadow"
  mkdir -p "$b/usr/bin" "$b/etc/ocbm"
  for x in usr/sbin/ocbmd usr/sbin/carplayd usr/bin/iap_role_switch; do blob "$b/$x" 50000; chmod 755 "$b/$x"; done
  echo "ocbm boot"   > "$b/script/ocbm_boot.sh"
  echo "radio hal"   > "$b/script/radio_hal.sh"
  : > "$b/script/radio_preload"
  : > "$b/script/ncm_wifi"
  echo "old copy"    > "$b/script/start_main_service.sh.pre"
  blob "$b/etc/ocbm/identity.tgz" 4000
  rm -f "$b/bin/ls"; ln -s /bin/busybox "$b/bin/ls"           # link retargeted
  rm -f "$b/usr/lib/libz.so"; blob "$b/usr/lib/libz.so" 100      # link replaced by a file
  rm -f "$b/usr/sbin/hfpd"; ln -s /usr/sbin/btd "$b/usr/sbin/hfpd" # file replaced by a link
  echo 999           > "$b/var/run/ocbmd.pid"                     # volatile extra
  echo "new log"     > "$b/var/log/box.log"                      # volatile change
}

tree_sum() { ( cd "$1" && find . -print | sort | while IFS= read -r p; do
  if [ -L "$p" ]; then echo "L $p $(readlink "$p")"; elif [ -f "$p" ]; then echo "F $p $(md5sum < "$p" | cut -c1-32)"; else echo "D $p"; fi
done ) | md5sum | cut -c1-32; }

run_payload() {  # $1 shell, $2 mode, $3 root, rest: env
  sh_="$1" mode_="$2" root_="$3"; shift 3
  env "$@" SRC="$T/src" ROOT="$root_" WORK="$T/work.$$" "$sh_" "$PAYLOAD" "$mode_" > "$T/out" 2>&1
  rc_=$?
  rm -rf "$T/work.$$"
  return $rc_
}

# ---- PROFILE=onefile: a one-file adapter, finished by the real ocbm_finish.sh, restored to stock ----
REPO="$(cd "$HERE/.." && pwd)"
TREE="${STOCK_RESTORE_TREE:-$HERE/stock_restore_onefile_tree.sh}"   # override to mutation-test the test
FINISH="$REPO/ccpa/installer/ocbm_finish.sh"
OCBM_PATHS="$(bash "$HERE/make_ocbm_installer.sh" --list)"
# Carlinkit daemon names this host runs itself (macOS has its own usbmuxd): ocbm_finish.sh skips them.
HOST_DAEMONS="$(for n in usbmuxd mdnsd boa; do pgrep -x "$n" >/dev/null 2>&1 && printf '%s ' "$n"; done)"

make_trial() {  # $1 dir: an adapter in its one-file trial — stock kept, OCBM placed, the dispatcher armed
  r="$1"
  mkdir -p "$r/bin" "$r/usr/sbin" "$r/usr/bin" "$r/usr/lib" "$r/lib/firmware/nxp" "$r/script" "$r/etc/mdev" \
           "$r/etc/init.d" "$r/etc/ocbm" "$r/etc/bluetooth" "$r/var/run" "$r/var/log" "$r/proc" "$r/tmp" "$r/id/etc"
  blob "$r/bin/busybox" 60000; chmod 755 "$r/bin/busybox"; ln -s busybox "$r/bin/sh"
  for b in ARMadb-driver AppleCarPlay ARMiPhoneIAP2 boa mdnsd hostapd; do blob "$r/usr/sbin/$b" 40000; chmod 755 "$r/usr/sbin/$b"; done
  blob "$r/usr/lib/libssl.so.1.1" 20000; ln -s libssl.so.1.1 "$r/usr/lib/libssl.so"
  blob "$r/lib/firmware/nxp/sd8987.bin" 5000
  for s in update_box.sh update_box_ota.sh init_gpio.sh; do echo "stock $s" > "$r/script/$s"; chmod 755 "$r/script/$s"; done
  echo "stock radio bring-up" > "$r/script/init_bluetooth_wifi.sh"
  echo "stock airplay.conf"   > "$r/etc/airplay.conf"
  echo "stock riddle.conf"    > "$r/etc/riddle.conf"
  printf 'mount /dev/sda1 /mnt/UPAN\n/script/update_box.sh\n' > "$r/etc/mdev/udisk_insert.sh"; chmod 755 "$r/etc/mdev/udisk_insert.sh"
  echo "stock rcS"     > "$r/etc/init.d/rcS"
  echo "stock inittab" > "$r/etc/inittab"
  echo "bond: phone A" > "$r/etc/bluetooth/keys"
  echo "stock start_main_service" > "$r/script/start_main_service.stock.sh"; chmod 755 "$r/script/start_main_service.stock.sh"
  printf '#!/bin/sh\n# OCBM_DISPATCH 1\necho dispatch\n' > "$r/script/start_main_service.sh"; chmod 755 "$r/script/start_main_service.sh"
  for p in $OCBM_PATHS; do mkdir -p "$r/$(dirname "$p")"; blob "$r/$p" 3000; chmod 755 "$r/$p"; done
  echo trial > "$r/script/ocbm_mode"
  echo "1.0.0 ocbm 153df7f" > "$r/etc/ocbm/installed_version"
  echo 'wpa_passphrase=stock-original' > "$r/id/etc/hostapd.conf"
  (cd "$r/id" && tar -cf - etc/hostapd.conf) | gzip -c > "$r/etc/ocbm/identity.tgz"; rm -rf "$r/id"
  echo 'wpa_passphrase=ocbm-rewritten' > "$r/etc/hostapd.conf"
  echo "ccpa-1234" > "$r/etc/carplay_ident"
  echo "crash"     > "$r/script/box_crash.log"
  echo 77          > "$r/var/run/ocbmd.pid"
}

archive_of() {  # $1 trial dir, $2 archive: what the adapter took of its own rootfs during the trial
  (cd "$1" && tar -cf - --exclude=./proc --exclude=./tmp .) | gzip -c > "$2"
}

finish_box() {  # $1 trial dir, $2 box dir: the real ocbm_finish.sh after a confirmed session
  cp -pR "$1" "$2"
  echo confirmed > "$2/script/ocbm_mode"
  mkdir -p "$2.run"
  env ROOT="$2" RUN="$2.run" FINISH_IGNORE="$HOST_DAEMONS" sh "$FINISH" > "$2.fin" 2>&1
  echo "bond: phone A, phone B" > "$2/etc/bluetooth/keys"   # paired after the archive was taken
}

run_onefile() {  # $1 shell, $2 mode, $3 root, rest: env. The lock lives in $T/run1.
  sh_="$1" mode_="$2" root_="$3"; shift 3
  mkdir -p "$T/run1"
  env PROFILE=onefile RUN="$T/run1" SRC="$T/tree" ROOT="$root_" WORK="$T/work.$$" "$@" "$sh_" "$PAYLOAD" "$mode_" > "$T/out" 2>&1
  rc_=$?
  rm -rf "$T/work.$$"
  return $rc_
}

is_dispatcher() { grep -q '^# OCBM_DISPATCH' "$1/script/start_main_service.sh"; }

onefile_tests() {  # $1 shell
  SH1="$1"
  echo "=== onefile profile under $SH1"
  rm -rf "$T/trial" "$T/fin" "$T/fin.run" "$T/tree" "$T/run1" "$T/a.tgz"
  make_trial "$T/trial"; archive_of "$T/trial" "$T/a.tgz"; finish_box "$T/trial" "$T/fin"
  check '[ "$(cat "$T/fin/script/ocbm_mode")" = final ] && [ ! -e "$T/fin/usr/sbin/ARMadb-driver" ] && [ ! -e "$T/fin/script/start_main_service.stock.sh" ]' \
    "the real ocbm_finish.sh left the box final, its Carlinkit stack gone"

  "$SH1" "$TREE" "$T/a.tgz" "$T/tree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -eq 0 ] && grep -q TREE_OK "$T/out"' "tree: made from the trial archive"
  check '[ "$(cat "$T/tree/script/start_main_service.sh")" = "stock start_main_service" ] && [ ! -e "$T/tree/script/start_main_service.stock.sh" ]' "tree: the stock start script is back under its own name"
  check '[ ! -e "$T/tree/usr/sbin/ocbmd" ] && [ ! -e "$T/tree/script/ocbm_boot.sh" ] && [ ! -e "$T/tree/etc/ocbm" ] && [ ! -e "$T/tree/script/ocbm_mode" ]' "tree: OCBM's payload, /etc/ocbm and the mode are dropped"
  check '[ ! -e "$T/tree/etc/carplay_ident" ] && [ ! -e "$T/tree/script/box_crash.log" ]' "tree: OCBM's runtime files are dropped"
  left_ocbm=0; for p in $OCBM_PATHS; do [ -e "$T/tree/$p" ] && left_ocbm=$((left_ocbm + 1)); done
  check '[ $left_ocbm -eq 0 ]' "tree: every path of make_ocbm_installer.sh --list is dropped"
  check '[ "$(cat "$T/tree/etc/hostapd.conf")" = "wpa_passphrase=stock-original" ]' "tree: hostapd.conf comes from the identity archive"
  check '[ ! -e "$T/tree/proc/version" ] && [ -x "$T/tree/usr/sbin/ARMadb-driver" ]' "tree: stock programs kept"
  rm -rf "$T/bad"; cp -pR "$T/trial" "$T/bad"; cp "$T/bad/script/start_main_service.stock.sh" "$T/bad/script/start_main_service.sh"
  archive_of "$T/bad" "$T/bad.tgz"; rm -rf "$T/badtree"
  "$SH1" "$TREE" "$T/bad.tgz" "$T/badtree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "not a trial" "$T/out"' "tree: refuses an archive whose start script is not the dispatcher"
  rm -rf "$T/bad"; cp -pR "$T/trial" "$T/bad"; rm -f "$T/bad/script/start_main_service.stock.sh"
  archive_of "$T/bad" "$T/bad.tgz"; rm -rf "$T/badtree"
  "$SH1" "$TREE" "$T/bad.tgz" "$T/badtree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "no start_main_service.stock.sh" "$T/out"' "tree: refuses an archive without the stock start script"

  # The pack for the box: exactly the tree. macOS tar adds a ._ AppleDouble entry per file that has
  # extended attributes unless COPYFILE_DISABLE is set (the Mac's dry run of 28 Oct planned 702); this
  # host's tar never does, so a tar that notes its environment shows the pack sets it.
  rm -rf "$T/fakebin" "$T/tar.env" "$T/pack.tgz"; mkdir -p "$T/fakebin"
  printf '#!/bin/sh\necho "COPYFILE_DISABLE=${COPYFILE_DISABLE:-unset} $1" >> "%s"\nexec %s "$@"\n' "$T/tar.env" "$(command -v tar)" > "$T/fakebin/tar"
  chmod 755 "$T/fakebin/tar"
  PATH="$T/fakebin:$PATH" "$SH1" "$TREE" --pack "$T/tree" "$T/pack.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -eq 0 ] && grep -q PACK_OK "$T/out" && grep -q "exactly the tree" "$T/out"' "pack: the tree packs, entry for entry"
  check '[ "$(head -1 "$T/tar.env")" = "COPYFILE_DISABLE=1 -czf" ]' "pack: tar packs with COPYFILE_DISABLE=1"
  check '[ "$(gzip -dc "$T/pack.tgz" | tar -tf - | grep -c "start_main_service.sh")" -eq 1 ] && ! gzip -dc "$T/pack.tgz" | tar -tf - | grep -q "/\._"' "pack: the stock start script is in it, no AppleDouble entry"
  rm -rf "$T/mac"; cp -pR "$T/tree" "$T/mac"; echo "AppleDouble" > "$T/mac/bin/._busybox"
  (cd "$T/mac" && tar -czf "$T/mac.tgz" .)
  "$SH1" "$TREE" --check "$T/tree" "$T/mac.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "not in the tree: 1, first bin/._busybox" "$T/out" && [ ! -e "$T/mac.tgz" ]' "pack: refuses an entry the tree lacks, and deletes the pack"
  rm -rf "$T/mac"; cp -pR "$T/tree" "$T/mac"; rm -f "$T/mac/etc/inittab"
  (cd "$T/mac" && tar -czf "$T/mac.tgz" .)
  "$SH1" "$TREE" --check "$T/tree" "$T/mac.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "not in the pack: 1, first etc/inittab" "$T/out"' "pack: refuses a pack missing a file of the tree"
  rm -rf "$T/mac"; cp -pR "$T/tree" "$T/mac"; echo "AppleDouble" > "$T/mac/etc/._inittab"
  "$SH1" "$TREE" --pack "$T/mac" "$T/mac.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "AppleDouble files: 1, first etc/._inittab" "$T/out" && [ ! -e "$T/mac.tgz" ]' "pack: refuses AppleDouble files in the tree itself (a FAT or exFAT volume)"
  rm -rf "$T/mac" "$T/mac.tgz" "$T/pack.tgz" "$T/fakebin" "$T/tar.env"

  rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"
  before=$(tree_sum "$T/box")
  run_onefile "$SH1" plan "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_PLAN_OK "$T/out" && grep -q "switch      :" "$T/out"' "plan succeeds and names the switch"
  check '[ "$(tree_sum "$T/box")" = "$before" ]' "plan changes nothing"

  # Stopped right before the switch: every stock file back, OCBM still whole and booting.
  run_onefile "$SH1" apply "$T/box" STOP_AT=switch; rc=$?
  check '[ $rc -eq 99 ] && grep -q "3/5 check" "$T/out"' "apply reaches the check before the switch"
  check 'cmp -s "$T/tree/usr/sbin/ARMadb-driver" "$T/box/usr/sbin/ARMadb-driver" && [ "$(readlink "$T/box/usr/lib/libssl.so")" = libssl.so.1.1 ]' "before the switch: the Carlinkit stack is back"
  check 'is_dispatcher "$T/box" && [ -x "$T/box/usr/sbin/ocbmd" ] && [ -f "$T/box/script/ocbm_boot.sh" ] && [ "$(cat "$T/box/script/ocbm_mode")" = final ]' "OCBM kept until the switch"
  check '[ ! -e "$T/run1/ocbm_mode.lock" ]' "the mode lock is released after a stop"

  run_onefile "$SH1" apply "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out"' "a re-run after the stop converges"
  check 'awk "/2\/5 files/{a=NR} /3\/5 check/{b=NR} /4\/5 switch/{c=NR} /5\/5 OCBM programs/{d=NR} END{exit !(a<b && b<c && c<d)}" "$T/out"' "order: stock files, check, switch, then OCBM's files"
  check '[ "$(cat "$T/box/script/start_main_service.sh")" = "stock start_main_service" ]' "the stock start script is in place"
  check '[ ! -e "$T/box/usr/sbin/ocbmd" ] && [ ! -e "$T/box/etc/ocbm" ] && [ ! -e "$T/box/script/ocbm_mode" ] && [ ! -e "$T/box/script/ocbm_boot.sh" ]' "OCBM's files removed after the switch"
  check '[ -e "$T/box/script/radio_hal.sh" ] && grep -q "kept by the radio guard" "$T/out"' "the radio guard keeps the radio seam"
  check 'grep -q "^/script/update_box.sh" "$T/box/etc/mdev/udisk_insert.sh"' "the stick handler calls the stock updater again"
  check '[ "$(cat "$T/box/etc/hostapd.conf")" = "wpa_passphrase=stock-original" ] && [ "$(cat "$T/box/etc/bluetooth/keys")" = "bond: phone A" ]' "Wi-Fi settings and bonds as in the trial archive"
  check '[ -z "$(find "$T/box" -name "*.rsnew")" ] && [ ! -e "$T/run1/ocbm_mode.lock" ]' "no .rsnew and no lock left behind"
  run_onefile "$SH1" verify "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_VERIFIED "$T/out"' "verify passes after apply"
  before=$(tree_sum "$T/box")
  run_onefile "$SH1" apply "$T/box"; rc=$?
  check '[ $rc -ne 0 ] && grep -q "not OCBM.s dispatcher" "$T/out" && [ "$(tree_sum "$T/box")" = "$before" ]' "refusal without the OCBM start script, nothing changed"

  # A write that did not land: the check stops before the switch.
  rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"
  run_onefile "$SH1" apply "$T/box" SKIP_WRITE=usr/sbin/ARMadb-driver; rc=$?
  check '[ $rc -ne 0 ] && grep -q "not switching" "$T/out"' "a difference left before the switch stops the apply"
  check 'is_dispatcher "$T/box" && [ -x "$T/box/usr/sbin/ocbmd" ]' "after that stop OCBM still boots"

  # Room shortfall: stops before the first file; the box still boots OCBM; a re-run converges.
  rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"
  run_onefile "$SH1" apply "$T/box" FAKE_FREE_KB=10; rc=$?
  check '[ $rc -ne 0 ] && grep -q "no room for" "$T/out" && grep -q RESTORE_ABORT "$T/out"' "too little flash aborts"
  check 'is_dispatcher "$T/box" && [ "$(cat "$T/box/script/ocbm_mode")" = final ] && [ -z "$(find "$T/box" -name "*.rsnew")" ]' "the abort leaves OCBM booting and no partial file"
  run_onefile "$SH1" apply "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out"' "a re-run after the shortfall converges"

  # Refusals that change nothing.
  rm -rf "$T/tree2"; cp -pR "$T/tree" "$T/tree2"
  for case_ in confirmed pending lock noocbmd noboot srcdispatch srcocbm; do
    rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"; rm -rf "$T/run1"; mkdir -p "$T/run1"
    rm -rf "$T/tree2"; cp -pR "$T/tree" "$T/tree2"
    case "$case_" in
      confirmed)   echo confirmed > "$T/box/script/ocbm_mode"; want="is confirmed" ;;
      pending)     : > "$T/box/etc/ocbm/finish_pending"; want="finish was interrupted" ;;
      lock)        mkdir "$T/run1/ocbm_mode.lock"; want="holds" ;;
      noocbmd)     rm -f "$T/box/usr/sbin/ocbmd"; want="OCBM is not complete" ;;
      noboot)      rm -f "$T/box/script/ocbm_boot.sh"; want="OCBM is not complete" ;;
      srcdispatch) cp "$T/box/script/start_main_service.sh" "$T/tree2/script/start_main_service.sh"; want="dispatcher, not stock" ;;
      srcocbm)     cp "$T/box/usr/sbin/ocbmd" "$T/tree2/usr/sbin/ocbmd"; want="still holds OCBM" ;;
    esac
    before=$(tree_sum "$T/box")
    run_onefile "$SH1" apply "$T/box" SRC="$T/tree2"; rc=$?
    check '[ $rc -ne 0 ] && grep -q "$want" "$T/out" && [ "$(tree_sum "$T/box")" = "$before" ]' "refuses ($case_), nothing changed"
  done
  rm -rf "$T/run1" "$T/tree2"

  # The rehearsal: the same restore from a trial that was never finished.
  rm -rf "$T/box"; cp -pR "$T/trial" "$T/box"
  run_onefile "$SH1" apply "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out" && [ "$(cat "$T/box/script/start_main_service.sh")" = "stock start_main_service" ] && [ ! -e "$T/box/script/start_main_service.stock.sh" ]' \
    "a trial restores the same way (the rehearsal)"
}

SHELLS="sh"
command -v dash >/dev/null 2>&1 && SHELLS="$SHELLS dash"

for SH in $SHELLS; do
  echo "=== payload under $SH"
  "$SH" -n "$PAYLOAD" && ok "parses" || bad "parses"
  rm -rf "$T/src" "$T/box"
  make_src "$T/src"; make_box "$T/src" "$T/box"

  before=$(tree_sum "$T/box")
  run_payload "$SH" plan "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_PLAN_OK "$T/out"' "plan succeeds"
  check '[ "$(tree_sum "$T/box")" = "$before" ]' "plan changes nothing"
  check 'grep -q "boot set    : 3 of 5" "$T/out"' "plan finds the 3 replaced boot scripts"
  check 'grep -q "kept        : script/ncm_wifi" "$T/out"' "plan keeps the NCM flag it would otherwise remove"
  check '! grep -q "proc/version\|tmp/scratch" "$T/out"' "proc and tmp are never inventoried"

  run_payload "$SH" verify "$T/box"; rc=$?
  check '[ $rc -eq 1 ] && grep -q RESTORE_DIFFERS "$T/out"' "verify reports differences before apply"

  run_payload "$SH" apply "$T/box"; rc=$?
  cp "$T/out" "$T/apply.out"
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out"' "apply succeeds"
  check 'awk "/3\/6 files/{a=NR} /4\/6 boot/{b=NR} /6\/6 login/{c=NR} END{exit !(a<b && b<c)}" "$T/apply.out"' "order: files, then boot set, then login set"
  run_payload "$SH" verify "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_VERIFIED "$T/out"' "verify passes after apply"
  check 'cmp -s "$T/src/usr/sbin/ARMadb-driver" "$T/box/usr/sbin/ARMadb-driver" && [ -x "$T/box/usr/sbin/ARMadb-driver" ]' "deleted vendor binary restored, executable"
  check '[ "$(cat "$T/box/script/start_main_service.sh")" = "stock start_main_service" ]' "boot script restored"
  check '[ "$(readlink "$T/box/bin/ls")" = busybox ] && [ "$(readlink "$T/box/usr/lib/libssl.so")" = libssl.so.1.1 ]' "links restored and retargeted"
  check '[ -L "$T/box/usr/lib/libz.so" ] && [ -f "$T/box/usr/sbin/hfpd" ] && [ ! -L "$T/box/usr/sbin/hfpd" ]' "file/link type swaps undone"
  check '[ ! -e "$T/box/usr/sbin/ocbmd" ] && [ ! -e "$T/box/script/ocbm_boot.sh" ] && [ ! -e "$T/box/etc/ocbm" ]' "OCBM files and their directory removed"
  check '[ -e "$T/box/script/ncm_wifi" ] && [ -e "$T/box/script/ncm_only" ]' "NCM flags survive"
  check '[ "$(cat "$T/box/var/log/box.log")" = "new log" ] && [ -e "$T/box/var/run/ocbmd.pid" ]' "volatile runtime files untouched"
  check '[ "$(cat "$T/box/etc/shadow")" = "root::0:0:99999:7:::" ]' "login file restored"
  check '[ -z "$(find "$T/box" -name "*.rsnew")" ]' "no .rsnew left behind"

  run_payload "$SH" apply "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q "3/6 files and links: 0 restored" "$T/out"' "second apply is a no-op"

  # Space shortfall: stops before the boot set, and the box keeps its owned (bootable) scripts.
  rm -rf "$T/box"; make_box "$T/src" "$T/box"
  run_payload "$SH" apply "$T/box" FAKE_FREE_KB=10; rc=$?
  check '[ $rc -ne 0 ] && grep -q "no room for" "$T/out" && grep -q RESTORE_ABORT "$T/out"' "too little flash aborts"
  check '[ "$(cat "$T/box/script/start_main_service.sh")" = "owned start_main_service" ]' "abort leaves the boot set untouched"
  check '[ -z "$(find "$T/box" -name "*.rsnew")" ]' "abort leaves no partial file"
  run_payload "$SH" apply "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out"' "re-run after the abort converges"

  # A directory where the backup has a file: refused, nothing changed.
  rm -rf "$T/box"; make_box "$T/src" "$T/box"
  rm -f "$T/box/usr/sbin/AppleCarPlay"; mkdir "$T/box/usr/sbin/AppleCarPlay"
  before=$(tree_sum "$T/box")
  run_payload "$SH" apply "$T/box"; rc=$?
  check '[ $rc -ne 0 ] && grep -q "type conflicts" "$T/out"' "type conflict refuses"
  check '[ "$(tree_sum "$T/box")" = "$before" ]' "refusal changes nothing"

  onefile_tests "$SH"
done

echo
if [ "$fails" -eq 0 ]; then echo "stock_restore_test: all passed"; exit 0; fi
echo "stock_restore_test: $fails FAILED"; exit 1
