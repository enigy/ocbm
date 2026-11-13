#!/bin/sh
# stock_restore_onefile_tree.sh — the stock tree a one-file adapter is restored to (PROFILE=onefile in
# tools/stock_restore_box.sh), made from one of the adapter's own backups.
#
#   tools/stock_restore_onefile_tree.sh ARCHIVE OUT_DIR       from the file-level archive the adapter
#                                                             took of its own rootfs during its trial
#   tools/stock_restore_onefile_tree.sh --dump MTD2 OUT_DIR   from the flash dump the install put on
#                                                             its stick (OCBM_BACKUP/<adapter>/mtd2.bin),
#                                                             decoded by tools/jffs2_extract.py
#
# The dump is the better source, and the only one for an adapter without an archive: the first
# install dumps the flash before it changes anything, so that dump is the factory rootfs, the
# extras the install pruned (HiCar, the Android Mirror helper, the web pages, the unit's HiCar
# identity in etc/hichain) included. A dump taken by a later install holds stock plus a dormant
# OCBM, like a trial archive, and is treated like one. A trial archive never has the extras.
#
# From a trial's rootfs (an archive, or a dump whose start script is OCBM's dispatcher):
#   - drop OCBM's own files: the image's payload (tools/make_ocbm_installer.sh --list), /etc/ocbm/,
#     /etc/carplay/ (OCBM's Bluetooth bond store), /script/ocbm_mode, and what OCBM writes at runtime
#     (RUNTIME below)
#   - put /etc/hostapd.conf and /etc/bluetooth_name back from /etc/ocbm/identity.tgz, as
#     ocbm_revert.sh does: OCBM rewrites the access point's name, passphrase and interface, and the
#     Bluetooth name. The seam's Bluetooth name flag /etc/.custom_bluetooth_name (CarLink-<4 hex>, ccpa- before 2.0; while
#     it is there stock's bluetoothDaemon applies that name) comes back as the install found it, or
#     is dropped when the install found none
#   - put start_main_service.stock.sh back as start_main_service.sh
# A dump from before any install has nothing of OCBM's and is taken as it is. Either way the tree has
# the stock directories the install used to delete (etc/boa/www, etc/boa/cgi-bin, etc/boa/images,
# etc/hichain): Carlinkit's own image unpacks its web pages INTO etc/boa/www and fails silently when
# it is missing (a tester, Oct 2026), and it ships only an empty etc/hichain, so the unit's HiCar
# identity comes back from a dump or not at all.
#
# REFUSES an archive that is not a trial's (it must hold OCBM's dispatcher as start_main_service.sh
# and a stock start_main_service.stock.sh), a dump that does not decode cleanly, and a dump with
# OCBM's programs but a stock start script. Prints TREE_OK on success.
#
# OUT_DIR then holds the adapter's Wi-Fi passphrase and Bluetooth link keys: keep it, like the
# archive and the dump, out of repos, issues and logs, and delete it when the restore is done.
# Beside it, OUT_DIR.owners lists each entry's owner as on the adapter (tools/pack_tree.py owners).
#
# Packing it for the box (tools/stock_restore.sh, stage), by tools/pack_tree.py:
#
#   tools/stock_restore_onefile_tree.sh --pack TREE TGZ    pack TREE into TGZ, then check it as below
#   tools/stock_restore_onefile_tree.sh --check TREE TGZ   refuse (and delete) a TGZ whose entries are
#                                                          not exactly TREE's own, or not owned as
#                                                          TREE.owners says; prints PACK_OK
#
# The pack is made with Python's tarfile, which writes no "._name" AppleDouble entries whatever the
# files' extended attributes (macOS tar adds one per such file unless COPYFILE_DISABLE is set, and
# the Mac's dry run of 28 Oct planned 702 of them), and with each entry's owner from TREE.owners:
# a plain tar from this Mac made the restored files 502:20 (a tester, Oct 2026). AppleDouble files on
# disk (a tree made on a FAT or exFAT volume) are refused.
# Test hook (tools/stock_restore_test.sh): OCBM_LIST, a file of payload paths in place of --list.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
RUNTIME="script/ocbm_mode script/ocbm_dispatch.sh script/ocbm_trial script/ocbm_trial.log \
script/ocbm_failover script/ocbm_failover.log script/radio_preload script/radio_preload_off script/.radio_preload_inflight script/.radio_preload_unfinished \
script/box_crash.log etc/carplay_ident etc/carplay_peers.bin etc/hostapd.conf.stock etc/hostapd.conf.new"
STOCK_DIRS="etc/boa/www etc/boa/cgi-bin etc/boa/images etc/hichain"
BT_FLAG="etc/.custom_bluetooth_name"

say() { echo "[onefile-tree] $*"; }
die() { echo "[onefile-tree] ABORT: $*" >&2; exit 1; }

case "${1:-}" in
  --pack|--check)
    TREE="${2:-}"; TGZ="${3:-}"
    [ -d "$TREE" ] && [ -n "$TGZ" ] || die "usage: $0 $1 TREE TGZ"
    TREE="$(cd "$TREE" && pwd)"
    case "$TGZ" in /*) ;; *) TGZ="$(pwd)/$TGZ" ;; esac
    OWNERS=""; [ -f "$TREE.owners" ] && OWNERS="$TREE.owners"
    # shellcheck disable=SC2086
    exec python3 -I "$HERE/pack_tree.py" "${1#--}" "$TREE" "$TGZ" $OWNERS ;;
  --dump) KIND=dump; SOURCE="${2:-}"; OUT="${3:-}" ;;
  *) KIND=archive; SOURCE="${1:-}"; OUT="${2:-}" ;;
esac

[ -n "$SOURCE" ] && [ -n "$OUT" ] || die "usage: $0 ARCHIVE OUT_DIR, or $0 --dump MTD2 OUT_DIR"
[ -s "$SOURCE" ] || die "nothing at $SOURCE"
if [ -e "$OUT" ]; then
  [ -d "$OUT" ] && [ -z "$(ls -A "$OUT")" ] || die "$OUT exists and is not an empty directory"
else
  mkdir -p "$OUT" || die "cannot create $OUT"
fi
chmod 700 "$OUT"

if [ "$KIND" = archive ]; then
  # Leading "/" and "./" both unpack relative to OUT.
  gzip -dc "$SOURCE" | (cd "$OUT" && tar -xf -) || die "could not unpack $SOURCE"
  OWNERS_SRC="$SOURCE"
else
  X="$(mktemp -d "${TMPDIR:-/tmp}/onefile_dump.XXXXXX")" || die "cannot make a temp dir"
  trap 'rm -rf "$X"' EXIT
  python3 -I "$HERE/jffs2_extract.py" "$SOURCE" "$X/x" > "$X/log" 2>&1; rc=$?
  sed 's/^/[onefile-tree] /' "$X/log"
  [ "$rc" = 0 ] || die "the dump did not decode cleanly (BAD entries or refused names); not restoring from it"
  rmdir "$OUT" && mv "$X/x/files" "$OUT" || die "could not move the decoded rootfs to $OUT"
  OWNERS_SRC="$X/x/report.tsv"
fi

S="$OUT/script"
if grep -q '^# OCBM_DISPATCH' "$S/start_main_service.sh" 2>/dev/null; then
  [ -s "$S/start_main_service.stock.sh" ] || die "the $KIND has no start_main_service.stock.sh, the stock start script"
  grep -q '^# OCBM_DISPATCH' "$S/start_main_service.stock.sh" && die "the $KIND's start_main_service.stock.sh is OCBM's dispatcher"
  TRIAL=1
else
  [ "$KIND" = archive ] && die "the archive's start_main_service.sh is not OCBM's dispatcher: this is not a trial's rootfs"
  [ -s "$S/start_main_service.sh" ] || die "the dump has no /script/start_main_service.sh"
  [ -e "$OUT/usr/sbin/ocbmd" ] && die "the dump holds OCBM's programs but a stock start script: not a state to restore from"
  TRIAL=0
  say "a dump from before any install: stock as the factory left it"
fi

if [ "$TRIAL" = 1 ]; then
  t="$OUT/.identity.$$"
  mkdir "$t" || die "cannot create $t"
  gzip -dc "$OUT/etc/ocbm/identity.tgz" 2>/dev/null | (cd "$t" && tar -xf - 2>/dev/null)
  for f in etc/hostapd.conf etc/bluetooth_name; do
    if [ -s "$t/$f" ]; then
      cp -p "$t/$f" "$OUT/$f" || die "could not put back /$f"
      say "/$f: put back from the identity archive"
    else
      say "no $f in /etc/ocbm/identity.tgz: keeping the $KIND's /$f"
    fi
  done
  if [ -f "$t/$BT_FLAG" ]; then
    cp -p "$t/$BT_FLAG" "$OUT/$BT_FLAG" || die "could not put back /$BT_FLAG"
    say "/$BT_FLAG: put back from the identity archive"
  elif grep -qxE '(ccpa|CarLink)-[0-9a-f]{4}' "$OUT/$BT_FLAG" 2>/dev/null; then
    rm -f "$OUT/$BT_FLAG" && say "/$BT_FLAG dropped: OCBM's Bluetooth name flag, which the install did not find"
  fi
  rm -rf "$t"   # it held the Wi-Fi passphrase and Bluetooth keys

  if [ -n "${OCBM_LIST:-}" ]; then
    payload=$(cat "$OCBM_LIST") || die "cannot read $OCBM_LIST"
  else
    payload=$(bash "$HERE/make_ocbm_installer.sh" --list) || die "tools/make_ocbm_installer.sh --list failed"
  fi
  [ -n "$payload" ] || die "the payload list is empty"
  n=0
  for p in $payload $RUNTIME; do
    case "$p" in ''|/*|..|../*|*/..|*/../*) die "refusing an unsafe path: '$p'" ;; esac
    if [ -e "$OUT/$p" ] || [ -L "$OUT/$p" ]; then rm -f "$OUT/$p" && n=$((n + 1)); fi
  done
  rm -rf "$OUT/etc/ocbm" "$OUT/etc/carplay" || die "could not drop /etc/ocbm and /etc/carplay"
  mv -f "$S/start_main_service.stock.sh" "$S/start_main_service.sh" || die "could not put the stock start script back"
  say "dropped $n OCBM files, /etc/ocbm/ and /etc/carplay/; start_main_service.sh is stock's"
fi

made=""
for d in $STOCK_DIRS; do
  [ -d "$OUT/$d" ] || { mkdir -p "$OUT/$d" && chmod 775 "$OUT/$d" && made="$made /$d"; }
done
[ -n "$made" ] && say "stock directories the install had deleted, back (empty):$made"

python3 -I "$HERE/pack_tree.py" owners "$OWNERS_SRC" "$OUT" \
  --rename script/start_main_service.stock.sh=script/start_main_service.sh > "$OUT.owners.new" \
  || die "could not read the owners from $SOURCE"
# A directory made back here was Carlinkit's build user's (1000:1000), as in the factory image.
awk -F '\t' -v OFS='\t' -v made="$made" 'BEGIN { n = split(made, m, " "); for (i = 1; i <= n; i++) back[substr(m[i], 2)] = 1 }
  ($3 in back) { $1 = 1000; $2 = 1000 } { print }' "$OUT.owners.new" > "$OUT.owners" && rm -f "$OUT.owners.new" \
  || die "could not write $OUT.owners"
chmod 600 "$OUT.owners"
say "tree: $OUT ($(find "$OUT" | wc -l | tr -d ' ') entries; owners in $OUT.owners)"
echo "TREE_OK"
