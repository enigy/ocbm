#!/bin/sh
# stock_restore_box.sh — make this adapter's rootfs match its own pre-conversion backup again.
#
# Runs ON the box (busybox 1.32 ash), driven by tools/stock_restore.sh. The backup's rootfs.tar.gz
# (from `ncm_base_install.sh backup`, taken before anything was stripped) is unpacked at $SRC first.
# ROOT is the live rootfs; the host-side test points it at a scratch tree instead.
#
#   stock_restore_box.sh plan     read-only: what would change, and whether it fits
#   stock_restore_box.sh apply    make ROOT match SRC. Idempotent: re-run it after any abort
#   stock_restore_box.sh verify   read-only: list every remaining difference; exit 1 if any
#
# WHY FILE BY FILE AND NOT `flashcp mtd2.bin`: writing the partition the running system is mounted
# from has no safe stopping point. A power cut or a crash mid-erase leaves no rootfs at all, and
# then only an SPI programmer helps. File by file, every intermediate state still boots NCM.
#
# APPLY ORDER — chosen so the box boots NCM if it stops at ANY point:
#   1. remove extras outside /script and /etc (OCBM's binaries). That is the room the restore needs:
#      the Carlinkit stack is ~5 MB on flash and the OCBM box has less than that free.
#   2. create missing directories
#   3. restore files and links outside the boot and login sets. Each file is written as .rsnew,
#      md5-checked, then renamed; free space is checked before every file
#   4. restore the boot set (custom_init.sh, udisk_insert.sh, start_main_service.sh, rcS, inittab)
#   5. remove extras under /script and /etc, then any directory left empty
#   6. restore the login set (passwd, shadow, dropbear host keys, authorized_keys) LAST, so the
#      host's ssh connections keep working through every earlier step
#
# NEVER TOUCHED: proc sys dev tmp mnt media (not in the backup either), the NCM mode flags
# (/script/ncm_only, /script/ncm_wifi — removing one mid-restore would boot an OCBM box whose OCBM
# is gone, with no way in but the UART), and volatile runtime state (var/run, var/log, var/lock,
# var/tmp, the dhcp lease file, shell history), which is neither restored nor counted as a diff.
#
# apply REFUSES on a live rootfs unless an NCM flag is set, for the reason above: get there first
# with `tools/ocbm_install.sh revert`. File modes come from the backup via `cp -p`; a file whose
# content already matches keeps its current mode (modes are not compared).
set -u

MODE="${1:-plan}"
SRC="${SRC:-/tmp/rs}"
ROOT="${ROOT:-/}"
W="${WORK:-/tmp/rsw}"
RESERVE_KB="${RESERVE_KB:-384}"
TAB="$(printf '\t')"
export LC_ALL=C

BOOT_SET="script/custom_init.sh etc/mdev/udisk_insert.sh script/start_main_service.sh etc/init.d/rcS etc/inittab"
LOGIN_SET="etc/passwd etc/passwd- etc/shadow etc/shadow- etc/dropbear/dropbear_rsa_host_key etc/dropbear/dropbear_dss_host_key root/.ssh/authorized_keys"
PROTECT="script/ncm_only script/ncm_wifi"

say() { echo "[restore] $*"; }
die() { echo "[restore] ABORT: $*"; echo "RESTORE_ABORT"; exit 1; }

in_list() {  # $1 word, $2 list
  for _w in $2; do [ "$_w" = "$1" ] && return 0; done
  return 1
}

md5() { md5sum < "$1" | cut -c1-32; }

free_kb() {
  if [ -n "${FAKE_FREE_KB:-}" ]; then echo "$FAKE_FREE_KB"; return; fi
  df -k "$ROOT" | awk 'NR > 1 && NF >= 4 { f = $4 } END { print f + 0 }'
}

size_kb() { echo $(( ($(wc -c < "$1") + 1023) / 1024 )); }

# Inventory of a tree, one line per entry: TYPE <tab> KEY <tab> PATH, sorted by PATH.
# TYPE d (KEY -), l (KEY = link target), f (KEY = md5), o (KEY -, anything else).
inv() {  # $1 tree, $2 output file
  (
    cd "$1" || exit 1
    P='( -path ./proc -o -path ./sys -o -path ./dev -o -path ./tmp -o -path ./mnt -o -path ./media )'
    # shellcheck disable=SC2086
    find . -xdev $P -prune -o -type d -print | sed 's|^\./||' | grep -v '^\.$' \
      | awk '{ printf "d\t-\t%s\n", $0 }'
    # shellcheck disable=SC2086
    find . -xdev $P -prune -o -type l -print | while IFS= read -r p; do
      printf 'l\t%s\t%s\n' "$(readlink "$p")" "${p#./}"
    done
    # shellcheck disable=SC2086
    find . -xdev $P -prune -o -type f -print | sed 's|^\./||' | xargs md5sum \
      | awk '{ h = $1; sub(/^[0-9a-f]+ +\*?/, ""); printf "f\t%s\t%s\n", h, $0 }'
    # shellcheck disable=SC2086
    find . -xdev $P -prune -o ! -type d ! -type l ! -type f -print | sed 's|^\./||' \
      | awk '{ printf "o\t-\t%s\n", $0 }'
  ) | awk -F "$TAB" '
      $3 ~ /^var\/(run|log|lock|tmp)\// { next }
      $3 == "var/lib/udhcpd.leases" || $3 == "root/.ash_history" { next }
      { print }' \
    | sort -t "$TAB" -k3,3 > "$2"
}

# Compare SRC (want) with ROOT (have). One action per line: ACTION <tab> TYPE <tab> KEY <tab> PATH.
#   mkdir, file, link       restore from SRC
#   extra                   on the box, not in the backup: remove
#   conflict                a directory on one side and not the other: never resolved automatically
WANT_DONE=0
diff_trees() {
  # The backup tree never changes within a run, and hashing it costs real time on a 528 MHz ARM.
  if [ "$WANT_DONE" = 0 ]; then
    inv "$SRC" "$W/want.inv" || die "cannot read the backup tree at $SRC"
    WANT_DONE=1
  fi
  inv "$ROOT" "$W/have.inv" || die "cannot read the rootfs at $ROOT"
  awk -F "$TAB" -v OFS="$TAB" '
    NR == FNR { wt[$3] = $1; wk[$3] = $2; next }
    { ht[$3] = $1; hk[$3] = $2 }
    END {
      for (p in wt) {
        if (!(p in ht)) { print (wt[p] == "d" ? "mkdir" : wt[p] == "l" ? "link" : "file"), wt[p], wk[p], p; continue }
        if (wt[p] == "d" && ht[p] == "d") continue
        if (wt[p] == "d" || ht[p] == "d") { print "conflict", wt[p], ht[p], p; continue }
        if (wt[p] == ht[p] && wk[p] == hk[p]) continue
        print (wt[p] == "l" ? "link" : "file"), wt[p], wk[p], p
      }
      for (p in ht) if (!(p in wt)) print "extra", ht[p], hk[p], p
    }' "$W/want.inv" "$W/have.inv" | sort -t "$TAB" -k4,4 > "$W/actions"
}

# Split the action list into the ordered work lists apply walks.
split_actions() {
  : > "$W/extra_payload"; : > "$W/extra_late"; : > "$W/extra_dirs"; : > "$W/mkdir"
  : > "$W/main"; : > "$W/boot"; : > "$W/login"; : > "$W/conflict"; : > "$W/protected"
  while IFS="$TAB" read -r act typ key p; do
    case "$p" in ''|/*|..|../*|*/..|*/../*) die "refusing an unsafe path in the plan: '$p'" ;; esac
    case "$act" in
      extra)
        if in_list "$p" "$PROTECT"; then printf '%s\n' "$p" >> "$W/protected"
        elif [ "$typ" = d ]; then printf '%s\n' "$p" >> "$W/extra_dirs"
        else case "$p" in
               script/*|etc/*|root/*) printf '%s\n' "$p" >> "$W/extra_late" ;;
               *) printf '%s\n' "$p" >> "$W/extra_payload" ;;
             esac
        fi ;;
      mkdir) printf '%s\n' "$p" >> "$W/mkdir" ;;
      conflict) printf '%s\t%s\t%s\n' "$typ" "$key" "$p" >> "$W/conflict" ;;
      file|link)
        if in_list "$p" "$BOOT_SET"; then printf '%s\t%s\t%s\n' "$act" "$key" "$p" >> "$W/boot"
        elif in_list "$p" "$LOGIN_SET"; then printf '%s\t%s\t%s\n' "$act" "$key" "$p" >> "$W/login"
        else printf '%s\t%s\t%s\n' "$act" "$key" "$p" >> "$W/main"; fi ;;
    esac
  done < "$W/actions"
  # The boot set goes in a fixed order: the scripts start_main_service calls, then the callers.
  : > "$W/boot.ordered"
  for b in $BOOT_SET; do awk -F "$TAB" -v p="$b" '$3 == p' "$W/boot" >> "$W/boot.ordered"; done
}

count() { [ -s "$1" ] && wc -l < "$1" | tr -d ' ' || echo 0; }

kb_of_src() {  # total KB of the SRC files named in a work list
  awk -F "$TAB" '$1 == "file" { print $3 }' "$@" | while IFS= read -r p; do size_kb "$SRC/$p"; done \
    | awk '{ s += $1 } END { print s + 0 }'
}

kb_of_root() {  # total KB of ROOT files named one per line
  while IFS= read -r p; do [ -f "$ROOT/$p" ] && [ ! -L "$ROOT/$p" ] && size_kb "$ROOT/$p"; done < "$1" \
    | awk '{ s += $1 } END { print s + 0 }'
}

report() {
  need=$(kb_of_src "$W/main" "$W/boot" "$W/login")
  freed=$(kb_of_root "$W/extra_payload")
  freed_late=$(kb_of_root "$W/extra_late")
  free_now=$(free_kb)
  say "backup tree : $SRC ($(count "$W/want.inv") entries)"
  say "rootfs      : $ROOT ($(count "$W/have.inv") entries)"
  say "restore     : $(awk -F "$TAB" '$1 == "file"' "$W/main" "$W/boot" "$W/login" | wc -l | tr -d ' ') files, $(awk -F "$TAB" '$1 == "link"' "$W/main" "$W/boot" "$W/login" | wc -l | tr -d ' ') links, $(count "$W/mkdir") dirs, ${need} KB to write"
  say "boot set    : $(count "$W/boot") of 5 differ: $(cut -f3 "$W/boot.ordered" | tr '\n' ' ')"
  say "login set   : $(count "$W/login") differ: $(cut -f3 "$W/login" | tr '\n' ' ')"
  say "remove      : $(count "$W/extra_payload") payload files (${freed} KB), $(count "$W/extra_late") script/etc files (${freed_late} KB), $(count "$W/extra_dirs") dirs if empty"
  [ -s "$W/protected" ] && say "kept        : $(tr '\n' ' ' < "$W/protected")(NCM mode flags are never removed)"
  say "flash       : ${free_now} KB free now; about $(( free_now + freed - need )) KB after, by uncompressed sizes (JFFS2 compresses, so the real figure is higher)"
  if [ -s "$W/conflict" ]; then
    say "CONFLICTS (a directory on one side only; apply will refuse):"
    sed 's/^/    /' "$W/conflict"
  fi
  { echo "== actions (ACTION TYPE KEY PATH)"; cat "$W/actions"; } > "$W/plan.txt"
  say "full plan   : $W/plan.txt"
}

put_file() {  # $1 relative path
  src="$SRC/$1"; dst="$ROOT/$1"; new="$dst.rsnew"
  need=$(( $(size_kb "$src") + RESERVE_KB ))
  free=$(free_kb)
  [ "$free" -ge "$need" ] || die "no room for $1: ${free} KB free, ${need} KB needed with reserve. Nothing half-written; re-run apply after freeing space"
  [ -d "$dst" ] && [ ! -L "$dst" ] && die "$1 is a directory on the box"
  rm -f "$new"
  cp -p "$src" "$new" || { rm -f "$new"; die "copy failed: $1"; }
  [ "$(md5 "$new")" = "$(md5 "$src")" ] || { rm -f "$new"; die "written copy of $1 does not match the backup"; }
  # mv onto a symlink that points at a directory would move INTO that directory: drop the link first.
  [ -L "$dst" ] && rm -f "$dst"
  mv -f "$new" "$dst" || die "rename failed: $1"
}

put_link() {  # $1 relative path, $2 target
  dst="$ROOT/$1"
  if [ -L "$dst" ] || [ -f "$dst" ]; then rm -f "$dst"
  elif [ -e "$dst" ]; then die "$1 is a directory on the box"; fi
  ln -s "$2" "$dst" || die "link failed: $1"
}

walk() {  # $1 work list of ACTION KEY PATH, $2 label
  n=0
  while IFS="$TAB" read -r act key p; do
    if [ "$act" = file ]; then put_file "$p"; else put_link "$p" "$key"; fi
    n=$((n + 1))
  done < "$1"
  say "$2: $n restored"
}

remove_list() {  # $1 list of paths, $2 label
  n=0
  while IFS= read -r p; do
    in_list "$p" "$PROTECT" && continue
    if [ -L "$ROOT/$p" ] || [ -e "$ROOT/$p" ]; then
      [ -d "$ROOT/$p" ] && [ ! -L "$ROOT/$p" ] && continue
      rm -f "$ROOT/$p" || die "could not remove $p"
      n=$((n + 1))
    fi
  done < "$1"
  say "$2: $n removed"
}

do_apply() {
  [ -s "$W/conflict" ] && { sed 's/^/    /' "$W/conflict"; die "type conflicts in the plan; resolve them by hand first"; }
  if [ "$ROOT" = / ]; then
    [ -e /script/ncm_only ] || [ -e /script/ncm_wifi ] \
      || die "no NCM flag: this box would boot OCBM with OCBM removed. Run 'tools/ocbm_install.sh revert' first"
  fi
  remove_list "$W/extra_payload" "1/6 payload extras"
  sync
  n=0
  while IFS= read -r p; do mkdir -p "$ROOT/$p" || die "mkdir failed: $p"; n=$((n + 1)); done < "$W/mkdir"
  say "2/6 directories: $n created"
  walk "$W/main" "3/6 files and links"
  sync
  walk "$W/boot.ordered" "4/6 boot set"
  sync
  remove_list "$W/extra_late" "5/6 script/etc extras"
  n=0
  sort -r "$W/extra_dirs" | while IFS= read -r p; do rmdir "$ROOT/$p" 2>/dev/null && echo x; done > "$W/rmdir.done"
  say "5/6 directories: $(count "$W/rmdir.done") removed (non-empty ones kept)"
  walk "$W/login" "6/6 login set"
  sync
}

[ -d "$SRC" ] || die "no backup tree at $SRC"
[ -d "$ROOT" ] || die "no rootfs at $ROOT"
mkdir -p "$W" || die "cannot create $W"

case "$MODE" in
  plan)
    diff_trees; split_actions; report
    echo "RESTORE_PLAN_OK" ;;
  apply)
    mkdir "$W/lock" 2>/dev/null || die "another apply is running (or crashed): remove $W/lock if not"
    trap 'rmdir "$W/lock" 2>/dev/null' EXIT
    diff_trees; split_actions; report
    do_apply
    diff_trees; split_actions
    left=$(( $(count "$W/actions") - $(count "$W/protected") ))
    [ "$left" -eq 0 ] || { say "$left differences remain:"; grep -v -F -f "$W/protected" "$W/actions" 2>/dev/null | head -20 | sed 's/^/    /'; die "apply finished with differences"; }
    say "rootfs matches the backup"
    echo "RESTORE_DONE" ;;
  verify)
    diff_trees; split_actions
    left=$(( $(count "$W/actions") - $(count "$W/protected") ))
    if [ "$left" -eq 0 ]; then say "rootfs matches the backup"; echo "RESTORE_VERIFIED"; exit 0; fi
    say "$left differences:"
    if [ -s "$W/protected" ]; then grep -v -F -f "$W/protected" "$W/actions"; else cat "$W/actions"; fi | sed 's/^/    /'
    echo "RESTORE_DIFFERS"; exit 1 ;;
  *) echo "usage: $0 plan|apply|verify" >&2; exit 2 ;;
esac
