#!/bin/sh
# stock_restore_onefile_tree.sh — the stock tree a one-file adapter is restored to (PROFILE=onefile in
# tools/stock_restore_box.sh), made from the file-level archive the adapter took of its own rootfs
# during its OCBM trial (rootfs_files_trial.tgz on its backup stick).
#
#   tools/stock_restore_onefile_tree.sh ARCHIVE OUT_DIR
#
# During the trial the rootfs is stock plus OCBM. To make stock of it:
#   - drop OCBM's own files: the image's payload (tools/make_ocbm_installer.sh --list), /etc/ocbm/,
#     /script/ocbm_mode, and what OCBM writes at runtime (RUNTIME below)
#   - put /etc/hostapd.conf back from /etc/ocbm/identity.tgz, as ocbm_revert.sh does: OCBM rewrites
#     the access point's name, passphrase and interface there
#   - put start_main_service.stock.sh back as start_main_service.sh
# Not in it: the extras the install pruned (HiCar, the Android Mirror helper, the web pages). Once
# the stock updater is back, Carlinkit's own 2025.10.15.1127 image restores them.
#
# REFUSES an archive that is not a trial's: it must hold OCBM's dispatcher as start_main_service.sh
# and a stock start_main_service.stock.sh. Prints TREE_OK on success.
#
# OUT_DIR then holds the adapter's Wi-Fi passphrase and Bluetooth link keys: keep it, like the
# archive, out of repos, issues and logs, and delete it when the restore is done.
#
# Packing it for the box (tools/stock_restore.sh, stage):
#
#   tools/stock_restore_onefile_tree.sh --pack TREE TGZ    pack TREE into TGZ, then check it as below
#   tools/stock_restore_onefile_tree.sh --check TREE TGZ   refuse (and delete) a TGZ whose entries are
#                                                          not exactly TREE's own; prints PACK_OK
#
# macOS tar adds a "._name" AppleDouble entry beside every file that has extended attributes unless
# COPYFILE_DISABLE is set, and on the box each one is a file written to flash: the Mac's dry run of
# 28 Oct planned 702 of them next to the one real write. AppleDouble files on disk (a tree made on a
# FAT or exFAT volume) are refused the same way.
# Test hook (tools/stock_restore_test.sh): OCBM_LIST, a file of payload paths in place of --list.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ARCHIVE="${1:-}"
OUT="${2:-}"
RUNTIME="script/ocbm_mode script/ocbm_dispatch.sh script/ocbm_trial script/ocbm_trial.log \
script/ocbm_failover script/ocbm_failover.log script/radio_preload script/.radio_preload_inflight \
script/box_crash.log etc/carplay_ident etc/hostapd.conf.stock etc/hostapd.conf.new"

say() { echo "[onefile-tree] $*"; }
die() { echo "[onefile-tree] ABORT: $*" >&2; exit 1; }

entries() {  # dir DIR | tgz FILE: its entries as relative paths, one a line, sorted bytewise
  case "$1" in
    dir) (cd "$2" && find . -print) | sed 's#^\./##' ;;
    tgz) gzip -dc "$2" | tar -tf - | sed 's#^\./##; s#/$##' ;;
  esac | grep -v '^\.\{0,1\}$' | LC_ALL=C sort -u
}

check_pack() {  # $1 tree, $2 tgz: exactly the tree's entries, and no AppleDouble file, or $2 goes
  w="$(mktemp -d "${TMPDIR:-/tmp}/onefile_pack.XXXXXX")" || die "cannot make a temp dir"
  entries dir "$1" > "$w/tree"
  entries tgz "$2" > "$w/tgz"
  extra="$(LC_ALL=C comm -23 "$w/tgz" "$w/tree")"
  missing="$(LC_ALL=C comm -13 "$w/tgz" "$w/tree")"
  apple="$(grep -E '(^|/)\._' "$w/tgz")"
  n="$(wc -l < "$w/tree" | tr -d ' ')"
  rm -rf "$w"
  [ -z "$extra$missing$apple" ] && { say "packed: exactly the tree's $n entries"; return 0; }
  rm -f "$2"
  [ -n "$extra" ] && say "in the pack, not in the tree: $(printf '%s\n' "$extra" | wc -l | tr -d ' '), first $(printf '%s\n' "$extra" | head -1)"
  [ -n "$missing" ] && say "in the tree, not in the pack: $(printf '%s\n' "$missing" | wc -l | tr -d ' '), first $(printf '%s\n' "$missing" | head -1)"
  [ -n "$apple" ] && say "macOS AppleDouble files: $(printf '%s\n' "$apple" | wc -l | tr -d ' '), first $(printf '%s\n' "$apple" | head -1)"
  die "the pack is not exactly the stock tree; deleted it, nothing goes to the box"
}

case "${1:-}" in
  --pack|--check)
    TREE="${2:-}"; TGZ="${3:-}"
    [ -d "$TREE" ] && [ -n "$TGZ" ] || die "usage: $0 $1 TREE TGZ"
    TREE="$(cd "$TREE" && pwd)"
    case "$TGZ" in /*) ;; *) TGZ="$(pwd)/$TGZ" ;; esac
    case "$TGZ" in "$TREE"/*) die "the pack cannot go inside the tree" ;; esac
    if [ "$1" = --pack ]; then
      rm -f "$TGZ"
      (cd "$TREE" && COPYFILE_DISABLE=1 tar -czf "$TGZ" .) || { rm -f "$TGZ"; die "could not pack $TREE"; }
    fi
    [ -s "$TGZ" ] || die "no pack at $TGZ"
    check_pack "$TREE" "$TGZ"
    echo "PACK_OK"
    exit 0 ;;
esac

[ -n "$ARCHIVE" ] && [ -n "$OUT" ] || die "usage: $0 ARCHIVE OUT_DIR"
[ -s "$ARCHIVE" ] || die "no archive at $ARCHIVE"
if [ -e "$OUT" ]; then
  [ -d "$OUT" ] && [ -z "$(ls -A "$OUT")" ] || die "$OUT exists and is not an empty directory"
else
  mkdir -p "$OUT" || die "cannot create $OUT"
fi

# Leading "/" and "./" both unpack relative to OUT.
gzip -dc "$ARCHIVE" | (cd "$OUT" && tar -xf -) || die "could not unpack $ARCHIVE"
S="$OUT/script"
grep -q '^# OCBM_DISPATCH' "$S/start_main_service.sh" 2>/dev/null \
  || die "the archive's start_main_service.sh is not OCBM's dispatcher: this is not a trial's rootfs"
[ -s "$S/start_main_service.stock.sh" ] || die "the archive has no start_main_service.stock.sh, the stock start script"
grep -q '^# OCBM_DISPATCH' "$S/start_main_service.stock.sh" && die "the archive's start_main_service.stock.sh is OCBM's dispatcher"

t="$OUT/.identity.$$"
mkdir "$t" || die "cannot create $t"
if gzip -dc "$OUT/etc/ocbm/identity.tgz" 2>/dev/null | (cd "$t" && tar -xf - etc/hostapd.conf 2>/dev/null) \
   && [ -s "$t/etc/hostapd.conf" ]; then
  cp -p "$t/etc/hostapd.conf" "$OUT/etc/hostapd.conf" || die "could not put back /etc/hostapd.conf"
  say "/etc/hostapd.conf: put back from the identity archive"
else
  say "no hostapd.conf in /etc/ocbm/identity.tgz: keeping the archive's /etc/hostapd.conf"
fi
rm -rf "$t"   # it held the Wi-Fi passphrase

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
rm -rf "$OUT/etc/ocbm" || die "could not drop /etc/ocbm"
mv -f "$S/start_main_service.stock.sh" "$S/start_main_service.sh" || die "could not put the stock start script back"
say "dropped $n OCBM files and /etc/ocbm/; start_main_service.sh is stock's"
say "tree: $OUT ($(find "$OUT" | wc -l | tr -d ' ') entries)"
echo "TREE_OK"
