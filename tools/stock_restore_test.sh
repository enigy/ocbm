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
done

echo
if [ "$fails" -eq 0 ]; then echo "stock_restore_test: all passed"; exit 0; fi
echo "stock_restore_test: $fails FAILED"; exit 1
