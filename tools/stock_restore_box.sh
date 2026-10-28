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
#
# ============================== PROFILE=onefile ==========================================
# An adapter converted by the one-file install (ccpa/installer/) has no NCM to fall back on, and
# after ocbm_finish.sh no stock either: the way back is this profile, over the OCBM console. SRC is
# the tree tools/stock_restore_onefile_tree.sh makes from the file-level archive the adapter took
# of its own rootfs during the trial (stock plus OCBM, OCBM's files dropped). The OCBM dispatcher
# (/script/start_main_service.sh, mode `final` or `trial`) keeps booting OCBM until ONE rename puts
# the stock start script over it, so every stopping point boots either OCBM, where this can be run
# again, or complete stock:
#   1. create missing directories
#   2. restore every file and link except the start script (written .rsnew, md5-checked, renamed;
#      free space checked before each). No OCBM file is touched yet, so OCBM still boots
#   3. check: anything other than the start script and OCBM's own files still differing stops here
#   4. THE SWITCH: the stock start script, written as .rsnew, checked, renamed over the dispatcher
#   5. remove OCBM's files (each through is_radio(): the radio seam's radio_*.sh stay, inert), then
#      directories left empty; verify
# It REFUSES, changing nothing, unless the start script is OCBM's dispatcher, ocbmd and ocbm_boot.sh
# are there, the mode is `trial` or `final` (a `confirmed` trial finishes when its session ends), no
# finish is pending, and SRC's start script is stock. It takes ocbm_mode.lock, as ocbm_revert.sh and
# ocbm_finish.sh do, so none of the three interleave.
# Test hooks (tools/stock_restore_test.sh): RUN (the lock's directory), STOP_AT=switch (exit 99
# right before the switch), SKIP_WRITE=<path> (a write that did not land).
set -u

MODE="${1:-plan}"
SRC="${SRC:-/tmp/rs}"
ROOT="${ROOT:-/}"
W="${WORK:-/tmp/rsw}"
RESERVE_KB="${RESERVE_KB:-384}"
PROFILE="${PROFILE:-ncm}"
RUN="${RUN:-/tmp}"
SWITCH="script/start_main_service.sh"
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

# THE WLAN/BT GUARD, as in ccpa/installer/once.sh and ocbm_finish.sh: no deletion may take a radio
# file. Used by the onefile profile's removals ($1 is a path from /).
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
  : > "$W/main"; : > "$W/boot"; : > "$W/login"; : > "$W/conflict"; : > "$W/protected"; : > "$W/switch"
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
        if [ "$PROFILE" = onefile ] && [ "$p" = "$SWITCH" ]; then printf '%s\t%s\t%s\n' "$act" "$key" "$p" >> "$W/switch"
        elif in_list "$p" "$BOOT_SET"; then printf '%s\t%s\t%s\n' "$act" "$key" "$p" >> "$W/boot"
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
    if [ -n "${SKIP_WRITE:-}" ] && [ "$p" = "$SKIP_WRITE" ]; then say "test hook: $p not written"; continue; fi
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

# ---- PROFILE=onefile (see the header) --------------------------------------------------------

onefile_refusals() {  # each one stops before anything changes
  grep -q '^# OCBM_DISPATCH' "$ROOT/$SWITCH" 2>/dev/null \
    || die "/$SWITCH is not OCBM's dispatcher, and the restore relies on it to boot OCBM until the switch; refusing"
  [ -x "$ROOT/usr/sbin/ocbmd" ] && [ -f "$ROOT/script/ocbm_boot.sh" ] \
    || die "OCBM is not complete here (no ocbmd or ocbm_boot.sh), so a stop before the switch might not boot; refusing"
  m=$(cat "$ROOT/script/ocbm_mode" 2>/dev/null)
  case "$m" in
    trial|final) ;;
    confirmed) die "the trial is confirmed and finishes when this session ends; let it finish, or go back to stock from the app, then restore" ;;
    *) die "mode '${m:-none}' would not boot OCBM; refusing" ;;
  esac
  [ -e "$ROOT/etc/ocbm/finish_pending" ] \
    && die "a finish was interrupted: start the adapter on OCBM so it completes, then restore"
  [ -f "$SRC/$SWITCH" ] || die "the restore tree has no /$SWITCH"
  grep -q '^# OCBM_DISPATCH' "$SRC/$SWITCH" && die "the restore tree's /$SWITCH is OCBM's dispatcher, not stock's"
  [ -e "$SRC/usr/sbin/ocbmd" ] && die "the restore tree still holds OCBM (usr/sbin/ocbmd); make it with tools/stock_restore_onefile_tree.sh"
  return 0
}

remove_guarded() {  # $1 list of paths, $2 label: as remove_list, but a radio file stays
  n=0; k=0
  while IFS= read -r p; do
    in_list "$p" "$PROTECT" && continue
    if is_radio "/$p"; then k=$((k + 1)); continue; fi
    if [ -L "$ROOT/$p" ] || [ -e "$ROOT/$p" ]; then
      [ -d "$ROOT/$p" ] && [ ! -L "$ROOT/$p" ] && continue
      rm -f "$ROOT/$p" || die "could not remove $p"
      n=$((n + 1))
    fi
  done < "$1"
  say "$2: $n removed, $k kept by the radio guard"
}

do_apply_onefile() {
  [ -s "$W/conflict" ] && { sed 's/^/    /' "$W/conflict"; die "type conflicts in the plan; resolve them by hand first"; }
  onefile_refusals
  mkdir "$RUN/ocbm_mode.lock" 2>/dev/null || die "a go-back or a finish holds $RUN/ocbm_mode.lock"
  MODE_LOCKED=1
  n=0
  while IFS= read -r p; do mkdir -p "$ROOT/$p" || die "mkdir failed: $p"; n=$((n + 1)); done < "$W/mkdir"
  say "1/5 directories: $n created"
  walk "$W/main" "2/5 files and links"
  walk "$W/boot.ordered" "2/5 boot scripts other than the start script"
  walk "$W/login" "2/5 login files"
  sync
  diff_trees; split_actions
  awk -F "$TAB" -v s="$SWITCH" '$1 != "extra" && $4 != s' "$W/actions" > "$W/left"
  if [ -s "$W/left" ]; then
    say "$(count "$W/left") differences before the switch:"
    head -20 "$W/left" | sed 's/^/    /'
    die "not switching: stock is not complete. The adapter still boots OCBM; fix the cause and re-run apply"
  fi
  say "3/5 check: every stock file is back; only the start script and OCBM's own files differ"
  [ "${STOP_AT:-}" = switch ] && { say "test hook: stopping before the switch"; exit 99; }
  [ -s "$W/switch" ] || die "/$SWITCH already matches the restore tree"
  walk "$W/switch" "4/5 switch"
  sync
  say "4/5 the stock start script is in place: the next start boots stock"
  remove_guarded "$W/extra_payload" "5/5 OCBM programs"
  remove_guarded "$W/extra_late" "5/5 OCBM scripts and settings"
  sort -r "$W/extra_dirs" | while IFS= read -r p; do rmdir "$ROOT/$p" 2>/dev/null && echo x; done > "$W/rmdir.done"
  say "5/5 directories: $(count "$W/rmdir.done") removed (non-empty ones kept)"
  sync
}

remaining() {  # $1 out: differences that still count (not the NCM flags; onefile: not radio files the guard keeps)
  : > "$1"
  while IFS="$TAB" read -r act typ key p; do
    in_list "$p" "$PROTECT" && continue
    [ "$PROFILE" = onefile ] && [ "$act" = extra ] && is_radio "/$p" && continue
    printf '%s\t%s\t%s\t%s\n' "$act" "$typ" "$key" "$p" >> "$1"
  done < "$W/actions"
}

[ -d "$SRC" ] || die "no backup tree at $SRC"
[ -d "$ROOT" ] || die "no rootfs at $ROOT"
mkdir -p "$W" || die "cannot create $W"
case "$PROFILE" in ncm|onefile) ;; *) die "unknown PROFILE '$PROFILE' (ncm or onefile)" ;; esac

case "$MODE" in
  plan)
    diff_trees; split_actions; report
    if [ "$PROFILE" = onefile ]; then
      onefile_refusals
      say "switch      : /$SWITCH, OCBM's dispatcher until stock is complete, then stock's"
    fi
    echo "RESTORE_PLAN_OK" ;;
  apply)
    mkdir "$W/lock" 2>/dev/null || die "another apply is running (or crashed): remove $W/lock if not"
    MODE_LOCKED=0
    trap 'rmdir "$W/lock" 2>/dev/null; [ "$MODE_LOCKED" = 1 ] && rmdir "$RUN/ocbm_mode.lock" 2>/dev/null' EXIT
    diff_trees; split_actions; report
    if [ "$PROFILE" = onefile ]; then do_apply_onefile; else do_apply; fi
    diff_trees; split_actions; remaining "$W/left"
    [ -s "$W/left" ] && { say "$(count "$W/left") differences remain:"; head -20 "$W/left" | sed 's/^/    /'; die "apply finished with differences"; }
    say "rootfs matches the backup"
    echo "RESTORE_DONE" ;;
  verify)
    diff_trees; split_actions; remaining "$W/left"
    if [ ! -s "$W/left" ]; then say "rootfs matches the backup"; echo "RESTORE_VERIFIED"; exit 0; fi
    say "$(count "$W/left") differences:"
    sed 's/^/    /' "$W/left"
    echo "RESTORE_DIFFERS"; exit 1 ;;
  *) echo "usage: $0 plan|apply|verify" >&2; exit 2 ;;
esac
