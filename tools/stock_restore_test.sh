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

# The group of $1 itself, a link not followed. BSD stat on the Mac; GNU stat's -f means the file system
# and does not fail, so a Linux host asks stat -c (cross builder's run-tests, 3 Nov).
grp() { case "$(uname)" in Darwin) stat -f %g "$1" ;; *) stat -c %g "$1" ;; esac; }

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

# ---- back to stock from the stick on the adapter itself (ccpa/installer/ocbm_restore.sh; Scott, 3 Nov) --
RESTORE="$REPO/ccpa/installer/ocbm_restore.sh"
# `ocbmd jffs2-extract` as ocbm_restore.sh runs it: the real one when tools/run_tests.sh built it
# (OCBMD_BIN), else tools/jffs2_extract.py with each link's owner taken from its report.
if [ -n "${OCBMD_BIN:-}" ] && [ -x "$OCBMD_BIN" ]; then
  DECODER="$OCBMD_BIN"
else
  DECODER="$T/ocbmd-standin"
  cat > "$DECODER" <<EOF
#!/bin/sh
[ "\$1" = jffs2-extract ] || exit 2
python3 -I "$HERE/jffs2_extract.py" "\$2" "\$3" || exit \$?
awk -F '\t' '\$1 == "OK" && \$2 == "l" { split(\$8, a, " -> "); p = a[1]; sub(/^\//, "", p); print \$4 "\t" \$5 "\t" p }' \
  "\$3/report.tsv" | sort > "\$3/links.tsv"
EOF
  chmod 755 "$DECODER"
fi
stick_box() {  # $1 dir, $2 mtd2 image: the final adapter $T/fin, its flash, and its first install's stick
  rm -rf "$1"; mkdir -p "$1/dev" "$1/run" "$1/sys"
  cp -pR "$T/fin" "$1/root"; printf 'ABCDEF0123456789' > "$1/root/etc/device_serial"
  # The engine where the installer puts it, which apply removes with OCBM's other scripts: run with ENGINE
  # unset, as on the adapter (3 Nov: it ran from there, and verify found nothing to run).
  cp "$PAYLOAD" "$1/root/script/stock_restore_box.sh"
  echo 2025.10.15.1127 > "$1/root/etc/software_version"
  blob "$1/dev/mtd0" 4096; blob "$1/dev/mtd1" 8192
  b="$1/stick/OCBM_BACKUP/ABCDEF0123456789"; mkdir -p "$b"
  cp "$1/dev/mtd0" "$b/mtd0.bin"; cp "$1/dev/mtd1" "$b/mtd1.bin"; cp "$2" "$b/mtd2.bin"
  ( cd "$b" && sha256sum mtd0.bin mtd1.bin mtd2.bin > SHA256SUMS )
  touch "$1/stick/OCBM_RESTORE"
  echo "not used yet" > "$1/stick/OCBM_RESTORE.txt"   # restore_watch's note, from a stick put in while OCBM ran
}
run_restore() {  # $1 dir, rest: env
  d_="$1"; shift
  env ROOT="$d_/root" RUN="$d_/run" STICK="$d_/stick" DEVDIR="$d_/dev" SYS="$d_/sys" REBOOT="touch $d_/rebooted" \
    UMOUNT=true OCBMD="$DECODER" TMP_MIN_KB=0 "$@" "$SH1" "$RESTORE" > "$d_/out" 2>&1
}
restore_says() { grep -q "^OCBM restore: $2" "$1/stick/OCBM_RESULT.txt" 2>/dev/null; }

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
  echo 'carlink' > "$r/id/etc/bluetooth_name"
  (cd "$r/id" && tar -cf - etc/hostapd.conf etc/bluetooth_name) | gzip -c > "$r/etc/ocbm/identity.tgz"; rm -rf "$r/id"
  echo 'wpa_passphrase=ocbm-rewritten' > "$r/etc/hostapd.conf"
  echo "ccpa-1234" > "$r/etc/carplay_ident"
  # The seam's Bluetooth name (radio_hal.sh bt_set_name), and OCBM's peer store
  echo "ccpa-1234" > "$r/etc/bluetooth_name"; echo "ccpa-1234" > "$r/etc/.custom_bluetooth_name"
  blob "$r/etc/carplay_peers.bin" 200
  mkdir -p "$r/etc/carplay"; echo "bond: phone A (OCBM)" > "$r/etc/carplay/bt_link_keys"
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

# A process that has exited. A zombie still answers kill -0, and a container's pid 1 may not reap the
# orphans these are, so one that /proc shows as a zombie counts too (macOS has no /proc, and reaps).
gone() {
  kill -0 "$1" 2>/dev/null || return 0
  [ "$(awk '{ print $3 }' "/proc/$1/stat" 2>/dev/null)" = Z ]
}

# LINKS beside a tree ($1.links), as the stage makes it: from the tree's pack (tools/pack_tree.py links).
links_from_pack() {  # $1 tree
  rm -f "$1.lp.tgz"
  "$SH1" "$TREE" --pack "$1" "$1.lp.tgz" > /dev/null 2>&1 && python3 -I "$HERE/pack_tree.py" links "$1.lp.tgz" > "$1.links"
  rm -f "$1.lp.tgz"
}

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
  check '[ "$(cat "$T/tree/etc/bluetooth_name")" = carlink ] && [ ! -e "$T/tree/etc/.custom_bluetooth_name" ] && [ ! -e "$T/tree/etc/carplay_peers.bin" ]' \
    "tree: the Bluetooth name comes from the identity archive; the seam's name flag and OCBM's peer store are dropped"
  rm -rf "$T/own"; cp -pR "$T/trial" "$T/own"; mkdir -p "$T/own/id/etc"
  echo 'wpa_passphrase=stock-original' > "$T/own/id/etc/hostapd.conf"; echo "My Car" > "$T/own/id/etc/.custom_bluetooth_name"
  (cd "$T/own/id" && tar -cf - etc) | gzip -c > "$T/own/etc/ocbm/identity.tgz"; rm -rf "$T/own/id"
  archive_of "$T/own" "$T/own.tgz"; rm -rf "$T/owntree" "$T/owntree.owners"
  "$SH1" "$TREE" "$T/own.tgz" "$T/owntree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -eq 0 ] && [ "$(cat "$T/owntree/etc/.custom_bluetooth_name")" = "My Car" ]' "tree: a Bluetooth name flag the install found comes back from the identity archive"
  rm -rf "$T/own" "$T/own.tgz" "$T/owntree" "$T/owntree.owners"
  check '[ ! -e "$T/tree/proc/version" ] && [ -x "$T/tree/usr/sbin/ARMadb-driver" ]' "tree: stock programs kept"
  check '[ ! -e "$T/tree/etc/carplay" ]' "tree: /etc/carplay, OCBM's bond store, is dropped"
  check '[ -d "$T/tree/etc/boa/www" ] && [ -d "$T/tree/etc/hichain" ] && [ -d "$T/tree/etc/boa/images" ] && [ -d "$T/tree/etc/boa/cgi-bin" ] && grep -q "deleted, back (empty)" "$T/out"' \
    "tree: the stock directories an older install deleted are back, for Carlinkit's own image"
  me="$(id -u)"
  check 'grep -q "^$me	[0-9]*	usr/sbin/ARMadb-driver$" "$T/tree.owners" && grep -q "^1000	1000	etc/hichain$" "$T/tree.owners"' \
    "tree: owners come from the archive, a directory made back is 1000:1000 as in the factory image"
  rm -rf "$T/bad"; cp -pR "$T/trial" "$T/bad"; cp "$T/bad/script/start_main_service.stock.sh" "$T/bad/script/start_main_service.sh"
  archive_of "$T/bad" "$T/bad.tgz"; rm -rf "$T/badtree"
  "$SH1" "$TREE" "$T/bad.tgz" "$T/badtree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "not a trial" "$T/out"' "tree: refuses an archive whose start script is not the dispatcher"
  rm -rf "$T/bad"; cp -pR "$T/trial" "$T/bad"; rm -f "$T/bad/script/start_main_service.stock.sh"
  archive_of "$T/bad" "$T/bad.tgz"; rm -rf "$T/badtree"
  "$SH1" "$TREE" "$T/bad.tgz" "$T/badtree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "no start_main_service.stock.sh" "$T/out"' "tree: refuses an archive without the stock start script"

  # The pack for the box (tools/pack_tree.py): exactly the tree, no AppleDouble entry, each entry owned
  # as on the adapter. A plain tar from the Mac made restored files 502:20 (a tester, Oct 2026).
  rm -f "$T/pack.tgz"
  command -v xattr >/dev/null 2>&1 && xattr -w com.ocbm.test 1 "$T/tree/etc/inittab" 2>/dev/null
  "$SH1" "$TREE" --pack "$T/tree" "$T/pack.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -eq 0 ] && grep -q PACK_OK "$T/out" && grep -q "exactly the tree" "$T/out"' "pack: the tree packs, entry for entry"
  check '[ "$(gzip -dc "$T/pack.tgz" | tar -tf - | grep -c "start_main_service.sh")" -eq 1 ] && ! gzip -dc "$T/pack.tgz" | tar -tf - | grep -q "\._"' \
    "pack: the stock start script is in it, no AppleDouble entry (even for a file with extended attributes)"
  check '[ "$(python3 -I -c "import tarfile,sys; m=tarfile.open(sys.argv[1]).getmember(\"./usr/sbin/ARMadb-driver\"); print(m.uid, m.uname or \"-\")" "$T/pack.tgz")" = "$me -" ]' \
    "pack: each entry carries its owner from TREE.owners, numeric only"
  python3 -I "$HERE/pack_tree.py" links "$T/pack.tgz" > "$T/tree.links"; rc=$?
  check '[ $rc -eq 0 ] && [ "$(wc -l < "$T/tree.links" | tr -d " ")" = "$(find "$T/tree" -type l | wc -l | tr -d " ")" ] && grep -q "^$me	[0-9]*	usr/lib/libssl.so$" "$T/tree.links"' \
    "pack: links gives each link of the pack with the owner in its header, for the box (busybox tar sets none)"
  # The bad packs below are made with this host's tar, which records this host's user: refused on
  # owners too, which is the 502:20 case.
  rm -rf "$T/mac"; cp -pR "$T/tree" "$T/mac"; echo "not stock" > "$T/mac/bin/extra"
  (cd "$T/mac" && COPYFILE_DISABLE=1 tar -czf "$T/mac.tgz" .)
  "$SH1" "$TREE" --check "$T/tree" "$T/mac.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "not in the tree: 1, first bin/extra" "$T/out" && [ ! -e "$T/mac.tgz" ]' "pack: refuses an entry the tree lacks, and deletes the pack"
  rm -rf "$T/mac"; cp -pR "$T/tree" "$T/mac"; rm -f "$T/mac/etc/inittab"
  (cd "$T/mac" && COPYFILE_DISABLE=1 tar -czf "$T/mac.tgz" .)
  "$SH1" "$TREE" --check "$T/tree" "$T/mac.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "not in the pack: 1, first etc/inittab" "$T/out"' "pack: refuses a pack missing a file of the tree"
  rm -rf "$T/mac"; cp -pR "$T/tree" "$T/mac"
  (cd "$T/mac" && COPYFILE_DISABLE=1 tar -czf "$T/mac.tgz" .)
  printf '0\t0\t%s\n' $(cd "$T/mac" && find . | sed '1d; s#^\./##') > "$T/mac.owners"
  "$SH1" "$TREE" --check "$T/mac" "$T/mac.tgz" > "$T/out" 2>&1; rc=$?
  if [ "$me" != 0 ]; then
    check '[ $rc -ne 0 ] && grep -q "owned otherwise than on the adapter" "$T/out" && [ ! -e "$T/mac.tgz" ]' "pack: refuses a pack owned by this Mac's user where the adapter's owner is root"
  fi
  rm -f "$T/mac.owners"
  # AppleDouble files in the tree itself (made on a FAT or exFAT volume): refused when packed, and
  # when a pack leaves them out.
  rm -rf "$T/mac"; cp -pR "$T/tree" "$T/mac"; echo "AppleDouble" > "$T/mac/etc/._inittab"
  "$SH1" "$TREE" --pack "$T/mac" "$T/mac.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "AppleDouble files: 1, first etc/._inittab" "$T/out" && [ ! -e "$T/mac.tgz" ]' "pack: refuses AppleDouble files in the tree itself (a FAT or exFAT volume)"
  (cd "$T/mac" && COPYFILE_DISABLE=1 tar -czf "$T/mac.tgz" --exclude="._*" .)
  "$SH1" "$TREE" --check "$T/mac" "$T/mac.tgz" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "AppleDouble files: 1, first etc/._inittab" "$T/out" && grep -q "not in the pack: 1, first etc/._inittab" "$T/out" && [ ! -e "$T/mac.tgz" ]' \
    "pack: refuses them too when the pack leaves them out"
  rm -rf "$T/mac" "$T/mac.tgz" "$T/pack.tgz"

  # The stock tree from a flash dump (--dump): tools/jffs2_extract_test.py writes a tree as an image
  # the way Carlinkit's kernel stores it (type-22 nodes, uid 1000).
  python3 -I "$HERE/jffs2_extract_test.py" --make-image "$T/trial" "$T/trial.bin"
  rm -rf "$T/dtree" "$T/dtree.owners"
  "$SH1" "$TREE" --dump "$T/trial.bin" "$T/dtree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -eq 0 ] && grep -q TREE_OK "$T/out" && [ "$(cat "$T/dtree/script/start_main_service.sh")" = "stock start_main_service" ]' "dump of a trial: the stock tree, start script back"
  check '[ ! -e "$T/dtree/usr/sbin/ocbmd" ] && [ ! -e "$T/dtree/etc/ocbm" ] && [ ! -e "$T/dtree/etc/carplay" ] && [ "$(cat "$T/dtree/etc/hostapd.conf")" = "wpa_passphrase=stock-original" ]' \
    "dump of a trial: OCBM, /etc/ocbm and /etc/carplay dropped, hostapd.conf from the identity archive"
  check '[ ! -e "$T/dtree/etc/.custom_bluetooth_name" ] && [ "$(cat "$T/dtree/etc/bluetooth_name")" = carlink ]' \
    "dump of a trial: the seam's Bluetooth name flag dropped, the name from the identity archive"
  check 'cmp -s "$T/dtree/usr/sbin/ARMadb-driver" "$T/tree/usr/sbin/ARMadb-driver" && grep -q "^1000	0	usr/sbin/ARMadb-driver$" "$T/dtree.owners"' \
    "dump of a trial: the same files as the archive gives, owned as in the image (1000)"
  links_from_pack "$T/dtree"
  # A user who is uid 1000 (the cross builder's) owns as the image does, so only another one can show up.
  check 'grep -q "^1000	0	usr/lib/libssl.so$" "$T/dtree.links" && { [ "$me" = 1000 ] || ! grep -q "^$me	" "$T/dtree.links"; }' \
    "dump of a trial: links gives the image's owner for each link (1000:0, as the dev adapter's 20), not this Mac's user"
  # A dump from before any install: the factory rootfs, extras included, taken as it is.
  rm -rf "$T/stock"; cp -pR "$T/dtree" "$T/stock"; blob "$T/stock/usr/sbin/ARMHiCar" 30000; chmod 755 "$T/stock/usr/sbin/ARMHiCar"
  echo "hichain identity" > "$T/stock/etc/hichain/hks_keystore"
  python3 -I "$HERE/jffs2_extract_test.py" --make-image "$T/stock" "$T/stock.bin"
  rm -rf "$T/ftree" "$T/ftree.owners"
  "$SH1" "$TREE" --dump "$T/stock.bin" "$T/ftree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -eq 0 ] && grep -q "before any install" "$T/out" && cmp -s "$T/ftree/usr/sbin/ARMHiCar" "$T/stock/usr/sbin/ARMHiCar" && [ -s "$T/ftree/etc/hichain/hks_keystore" ]' \
    "dump from before any install: taken as it is, the extras included"
  links_from_pack "$T/ftree"
  rm -rf "$T/bad"; cp -pR "$T/stock" "$T/bad"; blob "$T/bad/usr/sbin/ocbmd" 100
  python3 -I "$HERE/jffs2_extract_test.py" --make-image "$T/bad" "$T/bad.bin"; rm -rf "$T/badtree"
  "$SH1" "$TREE" --dump "$T/bad.bin" "$T/badtree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "OCBM.s programs but a stock start script" "$T/out"' "dump: refuses OCBM's programs behind a stock start script"
  head -c 70000 /dev/urandom > "$T/bad.bin"; rm -rf "$T/badtree"
  "$SH1" "$TREE" --dump "$T/bad.bin" "$T/badtree" > "$T/out" 2>&1; rc=$?
  check '[ $rc -ne 0 ] && grep -q "did not decode cleanly" "$T/out"' "dump: refuses one that does not decode"
  rm -rf "$T/bad" "$T/badtree" "$T/bad.bin"

  echo "=== back to stock from the stick, on the adapter (ocbm_restore.sh) under $SH1"
  # The stock tree as the factory image has it, firmware version included.
  rm -rf "$T/rsstock"; cp -pR "$T/stock" "$T/rsstock"; echo 2025.10.15.1127 > "$T/rsstock/etc/software_version"
  python3 -I "$HERE/jffs2_extract_test.py" --make-image "$T/rsstock" "$T/rsstock.bin"
  r="$T/rs"; stick_box "$r" "$T/rsstock.bin"; m="$(cksum "$r/dev/mtd0" "$r/dev/mtd1")"
  run_restore "$r"; rc=$?
  check '[ $rc -eq 0 ] && restore_says "$r" "BACK TO STOCK" && [ -e "$r/rebooted" ] && [ -e "$r/stick/OCBM_RESTORE.done" ] && [ ! -e "$r/stick/OCBM_RESTORE.txt" ]' \
    "a final adapter goes back to stock from its stick's backup, reports it and restarts; restore_watch's note goes"
  check '! is_dispatcher "$r/root" && cmp -s "$r/root/usr/sbin/ARMHiCar" "$T/stock/usr/sbin/ARMHiCar" && [ ! -e "$r/root/usr/sbin/ocbmd" ] && [ ! -e "$r/root/etc/ocbm" ]' \
    "the rootfs is the factory's, extras included, OCBM gone"
  check '[ ! -e "$r/root/script/stock_restore_box.sh" ] && grep -q RESTORE_VERIFIED "$r/run/ocbm_restore_verify.txt"' \
    "apply removes the engine from the rootfs, and verify still runs, from its copy in RAM"
  check '[ "$(cksum "$r/dev/mtd0" "$r/dev/mtd1")" = "$m" ] && [ ! -d "$r/run/ocbm-restore" ]' "mtd0 and mtd1 are only read; the decoded backup is gone from RAM"
  for case_ in sums identity trial notfinal; do
    r="$T/rs-$case_"; stick_box "$r" "$T/rsstock.bin"; b="$r/stick/OCBM_BACKUP/ABCDEF0123456789"
    case "$case_" in
      sums) echo x >> "$b/mtd2.bin"; want="does not match its SHA256SUMS" ;;
      identity) blob "$b/mtd1.bin" 8192; ( cd "$b" && sha256sum mtd0.bin mtd1.bin mtd2.bin > SHA256SUMS ); want="mtd1 differs" ;;
      trial) cp "$T/trial.bin" "$b/mtd2.bin"; ( cd "$b" && sha256sum mtd0.bin mtd1.bin mtd2.bin > SHA256SUMS ); want="holds OCBM" ;;
      notfinal) echo trial > "$r/root/script/ocbm_mode"; want="not final" ;;
    esac
    before=$(tree_sum "$r/root")
    run_restore "$r"; rc=$?
    check '[ $rc -eq 1 ] && restore_says "$r" REFUSED && grep -q "$want" "$r/stick/OCBM_RESULT.txt" && [ "$(tree_sum "$r/root")" = "$before" ] && [ -e "$r/stick/OCBM_RESTORE.refused" ] && [ ! -e "$r/rebooted" ]' \
      "refuses ($case_), changing nothing"
  done
  r="$T/rs-cut"; stick_box "$r" "$T/rsstock.bin"
  run_restore "$r" STOP_AT=switch; rc=$?
  check '[ $rc -eq 99 ] && is_dispatcher "$r/root" && [ -e "$r/stick/OCBM_RESTORE" ]' "a power cut before the switch: OCBM still starts, OCBM_RESTORE still on the stick"
  run_restore "$r"; rc=$?
  check '[ $rc -eq 0 ] && restore_says "$r" "BACK TO STOCK" && ! is_dispatcher "$r/root"' "and the next start finishes the restore"
  r="$T/rs-late"; stick_box "$r" "$T/rsstock.bin"
  run_restore "$r" RECREATE=usr/sbin/written_late; rc=$?
  check '[ $rc -eq 0 ] && ! is_dispatcher "$r/root" && restore_says "$r" "BACK TO STOCK" && grep -q "did not all finish" "$r/stick/OCBM_RESULT.txt" && ! grep -q "still starts OCBM" "$r/stick/OCBM_RESULT.txt" && [ -e "$r/rebooted" ] && [ -e "$r/stick/OCBM_RESTORE.done" ]' \
    "a failure after the switch: reported back to stock with what did not finish, and it restarts"
  rm -rf "$T"/rs "$T"/rs-* "$T/rsstock" "$T/rsstock.bin"

  rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"
  blob "$T/box/lib/firmware/nxp/added_after.bin" 300    # a radio file of the unit's, added after the archive
  before=$(tree_sum "$T/box")
  run_onefile "$SH1" plan "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_PLAN_OK "$T/out" && grep -q "switch      :" "$T/out"' "plan succeeds and names the switch"
  check '[ "$(tree_sum "$T/box")" = "$before" ]' "plan changes nothing"

  # Stopped right before the switch: every stock file back, OCBM still whole and booting.
  run_onefile "$SH1" apply "$T/box" STOP_AT=switch; rc=$?
  check '[ $rc -eq 99 ] && grep -q "3/5 check" "$T/out"' "apply reaches the check before the switch"
  check 'cmp -s "$T/tree/usr/sbin/ARMadb-driver" "$T/box/usr/sbin/ARMadb-driver" && [ "$(readlink "$T/box/usr/lib/libssl.so")" = libssl.so.1.1 ]' "before the switch: the Carlinkit stack is back"
  check 'is_dispatcher "$T/box" && [ -x "$T/box/usr/sbin/ocbmd" ] && [ -f "$T/box/script/ocbm_boot.sh" ] && [ "$(cat "$T/box/script/ocbm_mode")" = final ]' \
    "OCBM still boots with its console until the switch: the start script, ocbmd, ocbm_boot.sh, the mode"
  check '[ ! -e "$T/box/usr/sbin/carplayd" ] && [ ! -e "$T/box/usr/sbin/iap2d" ] && awk "/1\/5 room/{a=NR} /2\/5 files/{b=NR} END{exit !(a && a<b)}" "$T/out"' \
    "room first: OCBM's other programs go before any stock file is written"
  check '[ ! -e "$T/run1/ocbm_mode.lock" ]' "the mode lock is released after a stop"

  run_onefile "$SH1" apply "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out"' "a re-run after the stop converges"
  check 'awk "/2\/5 files/{a=NR} /3\/5 check/{b=NR} /4\/5 switch/{c=NR} /5\/5 OCBM programs/{d=NR} END{exit !(a<b && b<c && c<d)}" "$T/out"' "order: stock files, check, switch, then OCBM's files"
  check '[ "$(cat "$T/box/script/start_main_service.sh")" = "stock start_main_service" ]' "the stock start script is in place"
  check '[ ! -e "$T/box/usr/sbin/ocbmd" ] && [ ! -e "$T/box/etc/ocbm" ] && [ ! -e "$T/box/script/ocbm_mode" ] && [ ! -e "$T/box/script/ocbm_boot.sh" ]' "OCBM's files removed after the switch"
  check '[ ! -e "$T/box/script/radio_hal.sh" ] && [ ! -e "$T/box/script/radio_detect.sh" ] && [ ! -e "$T/box/script/radio_ap_up.sh" ]' "OCBM's radio seam goes with OCBM: stock has none"
  check '[ -e "$T/box/lib/firmware/nxp/added_after.bin" ] && grep -q "1 kept by the radio guard" "$T/out"' "the radio guard still keeps the unit's own radio files"
  check '[ ! -e "$T/box/etc/carplay" ] && grep -q "/etc/carplay removed" "$T/out"' "/etc/carplay, OCBM's bond store, is removed"
  check 'grep -q "5/5 OCBM scripts and settings: [0-9]* removed, 0 kept by the radio guard" "$T/out"' \
    "the bond store's bt_link_keys is not counted as kept by the radio guard: the next step removes it (the dev adapter, 1 Nov)"
  check '[ ! -e "$T/box/etc/.custom_bluetooth_name" ] && [ ! -e "$T/box/etc/carplay_peers.bin" ] && [ "$(cat "$T/box/etc/bluetooth_name")" = carlink ]' \
    "the seam's Bluetooth name flag goes despite the radio guard (the dev adapter, 31 Oct), with OCBM's peer store; the name is stock's"
  check 'grep -q "^/script/update_box.sh" "$T/box/etc/mdev/udisk_insert.sh"' "the stick handler calls the stock updater again"
  check '[ "$(cat "$T/box/etc/hostapd.conf")" = "wpa_passphrase=stock-original" ] && [ "$(cat "$T/box/etc/bluetooth/keys")" = "bond: phone A" ]' "Wi-Fi settings and bonds as in the trial archive"
  check '[ -z "$(find "$T/box" -name "*.rsnew")" ] && [ ! -e "$T/run1/ocbm_mode.lock" ]' "no .rsnew and no lock left behind"
  run_onefile "$SH1" verify "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_VERIFIED "$T/out"' "verify passes after apply"
  mkdir -p "$T/box/etc/carplay"; echo "written again" > "$T/box/etc/carplay/bt_link_keys"
  run_onefile "$SH1" verify "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_VERIFIED "$T/out"' "verify tolerates /etc/carplay written again by the OCBM still running"
  rm -rf "$T/box/etc/carplay"
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
  check 'grep -q "no room for .* after 0 of [1-9][0-9]* written" "$T/out"' "the abort says how many of the files it wrote (the dev adapter, 2 Nov)"
  check 'is_dispatcher "$T/box" && [ -x "$T/box/usr/sbin/ocbmd" ] && [ "$(cat "$T/box/script/ocbm_mode")" = final ] && [ -z "$(find "$T/box" -name "*.rsnew")" ]' \
    "the abort leaves OCBM booting with its console, and no partial file"
  run_onefile "$SH1" apply "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out"' "a re-run after the shortfall converges"

  # A program the room step removes may still be running, and its blocks stay allocated until it exits:
  # a carplayd from the last session held 1.1 MB on the dev adapter, and apply ran out of room (2 Nov).
  # It is stopped. The console's ocbmd is not, even running from a file an update replaced.
  rm -rf "$T/box" "$T/proc"; cp -pR "$T/fin" "$T/box"
  ( sleep 300 & echo $! > "$T/live.pid" ); live_=$(cat "$T/live.pid")
  ( sleep 300 & echo $! > "$T/con.pid" ); con_=$(cat "$T/con.pid")
  mkdir -p "$T/proc/$live_" "$T/proc/$con_"
  ln -s "$T/box/usr/sbin/carplayd (deleted)" "$T/proc/$live_/exe"
  ln -s "$T/box/usr/sbin/ocbmd (deleted)" "$T/proc/$con_/exe"
  run_onefile "$SH1" apply "$T/box" STOP_AT=switch PROC="$T/proc"; rc=$?
  check '[ $rc -eq 99 ] && grep -q "1/5 room: stopped 1 still running from a removed file: usr/sbin/carplayd" "$T/out" && gone "$live_"' \
    "room: an OCBM program still running from a file the room step removed is stopped (the dev adapter, 2 Nov)"
  check '! gone "$con_"' "the console's ocbmd is never stopped, even running from a replaced file"
  kill "$con_" "$live_" 2>/dev/null; rm -rf "$T/proc" "$T/live.pid" "$T/con.pid"

  # Refusals that change nothing.
  rm -rf "$T/tree2"; cp -pR "$T/tree" "$T/tree2"
  for case_ in confirmed pending lock noocbmd noboot srcdispatch srcocbm; do
    rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"; rm -rf "$T/run1"; mkdir -p "$T/run1"
    rm -rf "$T/tree2"; cp -pR "$T/tree" "$T/tree2"; cp "$T/tree.links" "$T/tree2.links"
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
  rm -rf "$T/run1" "$T/tree2" "$T/tree2.links"

  # A link the box lacks, as the finish deleted the dev adapter's 20 library links (31 Oct): it comes
  # back with the owner LINKS gives, on the link and not on its target. Without root the owner shown
  # is one of this user's groups other than the one a new file gets, which chown -h may set.
  rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"; rm -f "$T/box/usr/lib/libssl.so"
  g0=$(grp "$T/box/usr/lib"); g1=""
  for g in $(id -G); do [ "$g" != "$g0" ] && [ "$g" != "$(grp "$T/tree/usr/lib/libssl.so.1.1")" ] && { g1=$g; break; }; done
  if [ -n "$g1" ]; then
    awk -F '\t' -v OFS='\t' -v g="$g1" '$3 == "usr/lib/libssl.so" { $2 = g } 1' "$T/tree.links" > "$T/tree.g1"
    run_onefile "$SH1" apply "$T/box" LINKS="$T/tree.g1"; rc=$?
    check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out" && [ "$(readlink "$T/box/usr/lib/libssl.so")" = libssl.so.1.1 ] && [ "$(grp "$T/box/usr/lib/libssl.so")" = "$g1" ] && [ "$(grp "$T/box/usr/lib/libssl.so.1.1")" != "$g1" ]' \
      "a link made back takes its owner from LINKS, on the link itself (a bare ln -s left the dev adapter's 20 root's, 31 Oct)"
  else
    echo "  skip a link's owner from LINKS: this user is in no second group"
  fi
  rm -f "$T/tree.g1"
  # No owner in LINKS for a link to make back: plan says so, apply refuses and changes nothing.
  grep -v '	usr/lib/libssl.so$' "$T/tree.links" > "$T/tree.nolink"
  rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"; rm -f "$T/box/usr/lib/libssl.so"; before=$(tree_sum "$T/box")
  run_onefile "$SH1" plan "$T/box" LINKS="$T/tree.nolink"; rc=$?
  check '[ $rc -eq 0 ] && grep -q "LINK OWNERS : .*usr/lib/libssl.so" "$T/out"' "plan names a link LINKS gives no owner for"
  run_onefile "$SH1" apply "$T/box" LINKS="$T/tree.nolink"; rc=$?
  check '[ $rc -ne 0 ] && grep -q "names no owner for the link usr/lib/libssl.so" "$T/out" && [ "$(tree_sum "$T/box")" = "$before" ] && [ ! -e "$T/run1/ocbm_mode.lock" ]' \
    "apply refuses it, nothing changed"
  rm -f "$T/tree.nolink"

  # The OCBM still running writes its bond store again after step 5: apply still ends RESTORE_DONE.
  rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"
  run_onefile "$SH1" apply "$T/box" RECREATE=etc/carplay/bt_link_keys; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out"' "apply tolerates /etc/carplay written again after its removal (a tester, Oct 2026)"

  # A Bluetooth name flag that is not the seam's (a name set on stock after the archive): the radio
  # guard keeps it, and verify does not count it.
  rm -rf "$T/box"; cp -pR "$T/fin" "$T/box"; echo "My Car" > "$T/box/etc/.custom_bluetooth_name"
  run_onefile "$SH1" apply "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out" && [ "$(cat "$T/box/etc/.custom_bluetooth_name")" = "My Car" ]' \
    "a Bluetooth name flag that is not the seam's stays, as the radio guard wants"
  run_onefile "$SH1" verify "$T/box"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_VERIFIED "$T/out"' "and verify passes with it there"

  # From the factory dump onto a trial: the extras the install pruned come back, OCBM goes.
  rm -rf "$T/box"; cp -pR "$T/trial" "$T/box"
  run_onefile "$SH1" apply "$T/box" SRC="$T/ftree"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_DONE "$T/out" && cmp -s "$T/box/usr/sbin/ARMHiCar" "$T/ftree/usr/sbin/ARMHiCar" && [ -s "$T/box/etc/hichain/hks_keystore" ]' \
    "a trial restored from the factory dump gets the pruned extras and the HiCar identity back"
  check '[ ! -e "$T/box/usr/sbin/ocbmd" ] && [ ! -e "$T/box/script/radio_hal.sh" ] && [ ! -e "$T/box/etc/carplay" ] && [ "$(cat "$T/box/script/start_main_service.sh")" = "stock start_main_service" ]' \
    "and nothing of OCBM's is left"
  check '[ ! -e "$T/box/etc/.custom_bluetooth_name" ] && [ ! -e "$T/box/etc/carplay_peers.bin" ]' \
    "not the seam's Bluetooth name flag either, which the radio guard kept on the dev adapter (31 Oct)"
  check '[ "$(ls -ld "$T/box/etc/boa/www" | cut -c1-10)" = drwxrwxr-x ] && [ "$(ls -ld "$T/box/etc/hichain" | cut -c1-10)" = drwxrwxr-x ]' \
    "a directory made back takes the tree's mode (775, not mkdir's 755 as on the dev adapter, 31 Oct)"
  run_onefile "$SH1" verify "$T/box" SRC="$T/ftree"; rc=$?
  check '[ $rc -eq 0 ] && grep -q RESTORE_VERIFIED "$T/out"' "verify passes"
  echo "ccpa-1234" > "$T/box/etc/.custom_bluetooth_name"
  run_onefile "$SH1" verify "$T/box" SRC="$T/ftree"; rc=$?
  check '[ $rc -ne 0 ] && grep -q "etc/.custom_bluetooth_name" "$T/out"' "verify counts the seam's flag when it is still there (it passed it by on 31 Oct)"

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
  # LINKS as the stage makes it, from the backup's own archive (tools/pack_tree.py links)
  (cd "$T/src" && COPYFILE_DISABLE=1 tar -czf "$T/src.tgz" .) && python3 -I "$HERE/pack_tree.py" links "$T/src.tgz" > "$T/src.links"
  check '[ "$(cut -f3 "$T/src.links" | sort | tr "\n" " ")" = "bin/ls bin/sh usr/lib/libssl.so usr/lib/libz.so " ]' "links gives the backup's four links"

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

  # A link to make back that LINKS names no owner for: plan says so, apply refuses, nothing changed.
  rm -rf "$T/box"; make_box "$T/src" "$T/box"
  grep -v '	usr/lib/libssl.so$' "$T/src.links" > "$T/src.nolink"
  run_payload "$SH" plan "$T/box" LINKS="$T/src.nolink"; rc=$?
  check '[ $rc -eq 0 ] && grep -q "LINK OWNERS : .*usr/lib/libssl.so" "$T/out"' "plan names a link LINKS gives no owner for"
  before=$(tree_sum "$T/box")
  run_payload "$SH" apply "$T/box" LINKS="$T/src.nolink"; rc=$?
  check '[ $rc -ne 0 ] && grep -q "names no owner for the link usr/lib/libssl.so" "$T/out" && [ "$(tree_sum "$T/box")" = "$before" ]' \
    "apply refuses it, nothing changed (busybox tar sets no link's owner, 31 Oct)"
  rm -f "$T/src.nolink" "$T/src.tgz"

  onefile_tests "$SH"
done

# new_steps is cut out here and handed in: macOS's bash 3.2 brace-expands "{/,/^}" inside a quoted "$(...)",
# so cutting it out inside the bash -c gave sed two broken scripts. CI's runner has that bash, and both checks
# failed there from 07c94df on. The driver runs under whichever bash env finds, so the checks run under each.
DRIVER="${STOCK_RESTORE_DRIVER:-$HERE/stock_restore.sh}"   # override to mutation-test the test
NEW_STEPS=$(sed -n '/^new_steps() {/,/^}/p' "$DRIVER")
BASHES=""
for B in /bin/bash "$(command -v bash)"; do
  [ -x "$B" ] || continue
  case " $BASHES " in *" $B "*) ;; *) BASHES="$BASHES $B" ;; esac
done
for B in $BASHES; do
  echo "=== the driver's apply monitor (tools/stock_restore.sh) under $B, set -euo pipefail"
  out_=$("$B" -c 'set -euo pipefail; eval "$1"; new_steps "staging, no step yet" "^\[restore\] [0-9]/5|^RESTORE_" 0 | wc -l | tr -d " "; echo survived' _ "$NEW_STEPS" 2>&1)
  check '[ "$out_" = "0
survived" ]' "a log with no step lines yet does not end the driver (it did, silently, right after === apply)"
  out_=$("$B" -c 'set -euo pipefail; eval "$1"; new_steps "$(printf "[restore] 1/5 room\n[restore] x\n[restore] 2/5 files\nRESTORE_DONE\n")" "^\[restore\] [0-9]/5|^RESTORE_" 1' _ "$NEW_STEPS" 2>&1)
  check '[ "$out_" = "[restore] 2/5 files
RESTORE_DONE" ]' "it shows only the step lines not yet shown"
done

echo
if [ "$fails" -eq 0 ]; then echo "stock_restore_test: all passed"; exit 0; fi
echo "stock_restore_test: $fails FAILED"; exit 1
