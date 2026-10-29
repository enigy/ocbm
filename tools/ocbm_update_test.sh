#!/usr/bin/env bash
# ocbm_update_test.sh — updating an adapter that already runs OCBM, on the host: ocbm_update.sh
# places a package, the dispatcher (ocbm_dispatch.sh as /script/start_main_service.sh) runs its trial
# and keeps it or puts the old version back, and tools/make_ocbm_update.sh builds packages that fit.
# Synthetic converted adapters in a scratch tree (ROOT, RUN, SYS, PROC); the self-test, ocbm_boot.sh
# and the respawn wrappers are stand-ins, the update, the dispatcher and the revert are the real
# scripts. Under every POSIX shell found (sh, dash), because the box runs busybox ash. Needs no
# adapter and no box build.
#
#   tools/ocbm_update_test.sh
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
INST="$REPO/ccpa/installer"
T="$(mktemp -d "${TMPDIR:-/tmp}/ocbm_update_test.XXXXXX")"
reap() { pkill -f "$T/" 2>/dev/null; sleep 0.2; }
trap 'reap; if [ -n "${KEEP:-}" ]; then echo "scratch kept: $T"; else rm -rf "$T"; fi' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }
waitfor() {  # $1 condition, $2 seconds
  local i=0
  until eval "$1"; do i=$((i + 1)); [ "$i" -ge $(($2 * 10)) ] && return 1; sleep 0.1; done
}
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
OLD="1.0.0 ocbm old"
NEW="1.1.0 ocbm new"

stand_in() {  # $1 path, $2 mark file, $3 extra shell line
  printf '#!/bin/sh\necho ran >> "%s"\n%s\n' "$2" "${3:-exit 0}" > "$1"; chmod 755 "$1"
}
# The old version's own files: what the adapter runs before the update. The dispatcher and the update
# script are the real ones, so a package that replaces them replaces the real thing.
old_tree() {  # $1 dir
  mkdir -p "$1/usr/sbin" "$1/script"
  printf 'old-ocbmd\n' > "$1/usr/sbin/ocbmd"
  printf 'old-carplayd\n' > "$1/usr/sbin/carplayd"
  printf 'btd-same\n' > "$1/usr/sbin/btd"
  cp "$INST/ocbm_dispatch.sh" "$1/script/ocbm_dispatch.sh"
  cp "$INST/ocbm_update.sh" "$1/script/ocbm_update.sh"
  chmod 755 "$1"/usr/sbin/* "$1"/script/*
}
sums_of() {  # $1 tree: its SHA256SUMS, as the installer image and the package carry them
  ( cd "$1" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | while read -r f; do sha "$f"; done ) | awk '{print $1 "  " $2}'
}
make_box() {  # $1 dir, $2 mode: a one-file install of $OLD
  local d="$1" R="$1/root" k
  mkdir -p "$R/script" "$R/etc/ocbm" "$R/usr/sbin" "$d/run" "$d/stick" "$d/proc" "$d/sys/bus/sdio/devices/mmc0" \
           "$d/sys/module/libcomposite/parameters"
  old_tree "$d/old"
  cp -p "$d/old/usr/sbin/"* "$R/usr/sbin/"
  cp -p "$d/old/script/ocbm_update.sh" "$R/script/ocbm_update.sh"
  cp -p "$d/old/script/ocbm_dispatch.sh" "$R/script/start_main_service.sh"
  cp "$INST/ocbm_revert.sh" "$R/script/ocbm_revert.sh"; chmod 755 "$R/script/ocbm_revert.sh"
  stand_in "$R/script/start_main_service.stock.sh" "$d/m.stock"
  stand_in "$R/script/ocbm_boot.sh" "$d/m.boot"
  stand_in "$R/script/init_gpio.sh" "$d/m.gpio"
  for k in run_ocbmd run_supervisor; do stand_in "$R/script/$k.sh" "$d/m.$k" 'exec sleep 30'; done
  # the self-test stand-in: fails when $d/st.fail exists
  stand_in "$R/script/ocbm_selftest.sh" "$d/m.selftest" \
    "[ -e '$d/st.fail' ] && { echo 'FAILED after 30 s: ocbmd is not running'; exit 1; }
echo 'PASSED in 3 s'"
  echo "$2" > "$R/script/ocbm_mode"
  [ "$2" = final ] && rm -f "$R/script/start_main_service.stock.sh"
  echo "$OLD" > "$R/etc/ocbm/installed_version"
  echo "--- first start" > "$R/etc/ocbm/first_start.txt"
  echo A15W > "$R/etc/box_product_type"
  echo 0x9159 > "$d/sys/bus/sdio/devices/mmc0/device"
  echo 0 > "$d/sys/module/libcomposite/parameters/mfgmode"
  : > "$d/proc/mounts"
  echo "42.17 80.00" > "$d/proc/uptime"
}
# A package that takes $OLD to $NEW: new ocbmd, a new program (aa-bridge), the same btd, and with
# $2 = dispatch a changed start script too. $3 = from_min. $4 = what to leave out of files/.
make_pkg() {  # $1 out file, $2 extra ('' | dispatch), $3 from_min, $4 omit
  local w="$T/pkg.$$" k
  rm -rf "$w"; mkdir -p "$w/new"
  old_tree "$w/new"
  printf 'new-ocbmd\n' > "$w/new/usr/sbin/ocbmd"
  printf 'new-aa-bridge\n' > "$w/new/usr/sbin/aa-bridge"
  chmod 755 "$w/new/usr/sbin/aa-bridge"
  [ "$2" = dispatch ] && printf '\n# changed in %s\n' "$NEW" >> "$w/new/script/ocbm_dispatch.sh"
  mkdir -p "$w/pkg"
  sums_of "$w/new" > "$w/pkg/SHA256SUMS"
  old_tree "$w/oldref"; sums_of "$w/oldref" > "$w/oldsums"
  while read -r s rel; do
    [ "$(awk -v r="$rel" '$2 == r {print $1}' "$w/oldsums")" = "$s" ] && continue
    [ -n "${4:-}" ] && [ "$rel" = "$4" ] && continue
    mkdir -p "$w/pkg/files/$(dirname "$rel")"; cp -p "$w/new/$rel" "$w/pkg/files/$rel"
  done < "$w/pkg/SHA256SUMS"
  echo "$NEW" > "$w/pkg/VERSION"
  echo "${3:-1.0.0}" > "$w/pkg/FROM_MIN"
  ( cd "$w/pkg" && tar -cf - . ) | gzip -c > "$1"
  rm -rf "$w"
}
update() {  # $1 shell, $2 dir, then KEY=VALUE overrides: run ocbm_update.sh as ocbmd does
  local sh="$1" d="$2"; shift 2
  env ROOT="$d/root" SYS="$d/sys" PROC="$d/proc" RUN="$d/run" REBOOT="touch $d/rebooted" "$@" \
    "$sh" "$d/root/script/ocbm_update.sh" "$d/run/ocbm-update.tgz" > "$d/update.out" 2>&1
}
boot() {  # $1 shell, $2 dir: one start of the adapter
  env ROOT="$2/root" RUN="$2/run" STICK="$2/stick" PROC="$2/proc" POLL=1 STICK_WAIT=1 \
      REBOOT="touch $2/rebooted" "$1" "$2/root/script/start_main_service.sh" > "$2/boot.out" 2>&1
}
restart() { reap; rm -rf "${1:?}/run"; mkdir -p "$1/run"; rm -f "$1"/m.* "$1/rebooted"; }  # power cycle: /tmp is gone
content() { cat "$1/root/$2" 2>/dev/null; }
is_old() {  # $1 dir: the old version in place, nothing of the update left
  [ "$(content "$1" usr/sbin/ocbmd)" = old-ocbmd ] && [ ! -e "$1/root/usr/sbin/aa-bridge" ] \
    && cmp -s "$1/root/script/start_main_service.sh" "$1/old/script/ocbm_dispatch.sh" \
    && [ "$(content "$1" etc/ocbm/installed_version)" = "$OLD" ] && [ ! -e "$1/root/etc/ocbm/update_pending" ] \
    && [ -z "$(find "$1/root" -name '*.prev' -o -name '*.new' | head -1)" ]
}
is_new() {  # $1 dir: the update kept, its .prev copies gone
  [ "$(content "$1" usr/sbin/ocbmd)" = new-ocbmd ] && [ "$(content "$1" usr/sbin/aa-bridge)" = new-aa-bridge ] \
    && [ "$(content "$1" usr/sbin/btd)" = btd-same ] \
    && [ "$(content "$1" etc/ocbm/installed_version)" = "$NEW" ] && [ ! -e "$1/root/etc/ocbm/update_pending" ] \
    && [ -z "$(find "$1/root" -name '*.prev' -o -name '*.new' | head -1)" ]
}
result_is() { grep -q "^$2" "$1/root/etc/ocbm/update_result" 2>/dev/null; }

for sh in ${SHELLS:-sh dash}; do
  command -v "$sh" >/dev/null 2>&1 || { echo "== $sh: not installed, skipped"; continue; }

  echo "== $sh: an update on a final adapter that passes its self-test"
  d="$T/$sh-pass"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" dispatch
  update "$sh" "$d"; rc=$?
  check '[ $rc = 0 ] && [ -e "$d/rebooted" ] && grep -qx placed "$d/root/etc/ocbm/update_pending"' "placed, journalled, restarting"
  check '[ "$(content "$d" usr/sbin/ocbmd)" = new-ocbmd ] && [ "$(content "$d" usr/sbin/ocbmd.prev)" = old-ocbmd ]' \
    "the new ocbmd in place, the old one kept as .prev"
  check '! grep -q "usr/sbin/btd" "$d/root/etc/ocbm/update_pending"' "a file that did not change is left alone"
  check 'grep -qx "replace script/start_main_service.sh" "$d/root/etc/ocbm/update_pending" && [ "$(grep -E "^(replace|add) " "$d/root/etc/ocbm/update_pending" | tail -1)" = "replace script/start_main_service.sh" ]' \
    "the start script is swapped last"
  check '[ ! -e "$d/run/ocbm-update.tgz" ] && [ ! -d "$d/run/ocbm-update" ]' "the package and its unpacked copy go"
  restart "$d"; boot "$sh" "$d"
  check 'waitfor "is_new \"$d\"" 5' "the trial passes: the update is kept, installed_version moves on"
  check 'result_is "$d" "committed $NEW (from $OLD)"' "update_result: committed"
  check '[ ! -e "$d/rebooted" ] && [ -e "$d/m.boot" ] && [ "$(cat "$d/root/script/ocbm_mode")" = final ]' "no restart; still final"

  echo "== $sh: an update whose self-test fails goes back to the version it replaced"
  d="$T/$sh-fail"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" dispatch
  update "$sh" "$d"; restart "$d"; touch "$d/st.fail"; boot "$sh" "$d"
  check 'waitfor "is_old \"$d\" && [ -e \"$d/rebooted\" ]" 5' "the old version back, aa-bridge gone, and a restart"
  check 'result_is "$d" "rolled_back $NEW: its start-up self-test failed"' "update_result: rolled_back, with the reason"
  check '[ "$(cat "$d/root/script/ocbm_mode")" = final ]' "still final"

  echo "== $sh: a trial adapter's failed update falls back to the old OCBM, not to stock"
  d="$T/$sh-trial"; make_box "$d" trial; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d"; restart "$d"; touch "$d/st.fail"; boot "$sh" "$d"
  check 'waitfor "[ -e \"$d/rebooted\" ]" 5 && is_old "$d"' "the old OCBM back"
  check '[ "$(cat "$d/root/script/ocbm_mode")" = trial ] && [ ! -e "$d/m.stock" ] && [ ! -e "$d/root/etc/ocbm/pending_result.txt" ]' \
    "still in its trial, stock not started, no revert"
  check '[ ! -e "$d/root/etc/ocbm/boot_attempts" ]' "the update's failed start does not count against the install's trial"
  restart "$d"; rm -f "$d/st.fail"; boot "$sh" "$d"
  check 'waitfor "[ -e \"$d/m.selftest\" ]" 5 && [ ! -e "$d/m.stock" ] && [ "$(cat "$d/root/script/ocbm_mode")" = trial ]' \
    "the next start is the old OCBM's ordinary trial start"
  d="$T/$sh-trial-missing"; make_box "$d" trial; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d"; restart "$d"; chmod -x "$d/root/usr/sbin/ocbmd"; boot "$sh" "$d"
  check '[ -e "$d/rebooted" ] && is_old "$d" && result_is "$d" "rolled_back $NEW: /usr/sbin/ocbmd is missing" && [ ! -e "$d/m.stock" ] && [ "$(cat "$d/root/script/ocbm_mode")" = trial ]' \
    "a file missing in the update's trial puts the old OCBM back and restarts, not stock"

  echo "== $sh: two starts that never pass put the old version back before OCBM starts"
  d="$T/$sh-hang"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d"; restart "$d"; echo 2 > "$d/root/etc/ocbm/update_attempts"; boot "$sh" "$d"
  check 'is_old "$d" && result_is "$d" "rolled_back $NEW: OCBM 1.1.0 started 2 times in a row"' "rolled back at boot"
  check '[ -e "$d/m.boot" ] && [ ! -e "$d/rebooted" ]' "and the old OCBM starts, with no restart"

  echo "== $sh: a power cut at every step ends on a working version"
  for at in unpack checks place journal swap swap-mid restart; do
    d="$T/$sh-cut-$at"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" dispatch
    update "$sh" "$d" STOP_AT="$at"; rc=$?
    restart "$d"; boot "$sh" "$d"
    case "$at" in
      restart) check 'waitfor "is_new \"$d\"" 5' "cut before $at: the trial runs and keeps the update" ;;
      journal)   # .new files written, no journal: the old version runs; the next update run removes them
        check '[ $rc = 99 ] && [ "$(content "$d" usr/sbin/ocbmd)" = old-ocbmd ] && [ ! -e "$d/root/etc/ocbm/update_pending" ] && waitfor "[ -e \"$d/m.boot\" ]" 5' \
          "cut before $at: the old version, with .new files for the next run to remove" ;;
      swap|swap-mid)
        check '[ $rc = 99 ] && waitfor "is_old \"$d\"" 5 && result_is "$d" "rolled_back $NEW: power was lost while the update was being placed"' \
          "cut before $at: the next start undoes it" ;;
      *) check '[ $rc = 99 ] && [ "$(content "$d" usr/sbin/ocbmd)" = old-ocbmd ] && [ ! -e "$d/root/etc/ocbm/update_pending" ]' \
           "cut before $at: the old version, untouched" ;;
    esac
  done
  d="$T/$sh-cut-place2"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d" STOP_AT=journal; restart "$d"; make_pkg "$d/run/ocbm-update.tgz" ""   # .new files left; power cycled; the update again
  update "$sh" "$d"; rc=$?
  check '[ $rc = 0 ] && grep -qx placed "$d/root/etc/ocbm/update_pending" && [ -z "$(find "$d/root" -name "*.new" | head -1)" ]' \
    "a run after a cut before the journal starts over, and leaves no .new behind"
  d="$T/$sh-cut-strays"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d" STOP_AT=journal; restart "$d"; echo garbage > "$d/run/ocbm-update.tgz"; update "$sh" "$d"
  check '[ -z "$(find "$d/root" -name "*.new" | head -1)" ] && [ "$(content "$d" usr/sbin/ocbmd)" = old-ocbmd ]' \
    "a refused run removes the .new files a cut left"
  d="$T/$sh-cut-commit"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d"; echo committing >> "$d/root/etc/ocbm/update_pending"; rm -f "$d/root/usr/sbin/ocbmd.prev"
  restart "$d"; boot "$sh" "$d"
  check 'is_new "$d" && result_is "$d" "committed $NEW"' "cut in the commit: the next start finishes it"
  d="$T/$sh-cut-undo"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d"; echo "rolling_back=its start-up self-test failed" >> "$d/root/etc/ocbm/update_pending"
  mv -f "$d/root/usr/sbin/ocbmd.prev" "$d/root/usr/sbin/ocbmd"   # the undo had got one file back
  restart "$d"; boot "$sh" "$d"
  check 'is_old "$d" && result_is "$d" "rolled_back $NEW: its start-up self-test failed"' "cut in the undo: the next start finishes it"

  echo "== $sh: refusals change nothing"
  refusal() {  # $1 label, $2 expected words, then KEY=VALUE overrides; the box and package as set up
    what="$1"; words="$2"; shift 2
    update "$sh" "$d" "$@"; rc=$?
    check '[ $rc = 1 ] && is_old "$d" && result_is "$d" "refused" && grep -q "$words" "$d/root/etc/ocbm/update_result" && [ ! -e "$d/rebooted" ] && [ ! -d "$d/run/ocbm_mode.lock" ]' "$what"
  }
  d="$T/$sh-r-room"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  refusal "too little flash for the new files and the spare" "Not enough flash" FAKE_FREE_KB=300
  d="$T/$sh-r-damaged"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  mkdir -p "$T/x" && (cd "$T/x" && gzip -dc "$d/run/ocbm-update.tgz" | tar -xf - && echo tampered >> files/usr/sbin/ocbmd \
    && tar -cf - . | gzip -c > "$d/run/ocbm-update.tgz") && rm -rf "$T/x"
  refusal "a file that does not match its sum" "do not match their checksums"
  d="$T/$sh-r-garbage"; make_box "$d" final; echo garbage > "$d/run/ocbm-update.tgz"
  refusal "a package that is not one" "could not be unpacked"
  d="$T/$sh-r-same"; make_box "$d" final; NEW_SAVE="$NEW"; NEW="1.0.0 ocbm again"; make_pkg "$d/run/ocbm-update.tgz" ""; NEW="$NEW_SAVE"
  refusal "a version that is not newer" "already runs OCBM 1.0.0"
  d="$T/$sh-r-older"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" "" 1.0.5
  refusal "an adapter older than FROM_MIN" "updates OCBM 1.0.5 or newer"
  d="$T/$sh-r-lacks"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" "" 1.0.0 usr/sbin/ocbmd
  refusal "a package that lacks a file this adapter needs" "lacks /usr/sbin/ocbmd"
  d="$T/$sh-r-confirmed"; make_box "$d" trial; echo confirmed > "$d/root/script/ocbm_mode"; make_pkg "$d/run/ocbm-update.tgz" ""
  refusal "a confirmed trial waits for its finish" "finishes its install when this drive ends"
  d="$T/$sh-r-nomode"; make_box "$d" final; rm -f "$d/root/script/ocbm_mode"; make_pkg "$d/run/ocbm-update.tgz" ""
  refusal "an adapter on stock" "does not run a one-file OCBM install"
  d="$T/$sh-r-product"; make_box "$d" final; echo A15H > "$d/root/etc/box_product_type"; make_pkg "$d/run/ocbm-update.tgz" ""
  refusal "another product" "not an A15W"
  d="$T/$sh-r-chip"; make_box "$d" final; echo 0x4354 > "$d/sys/bus/sdio/devices/mmc0/device"; make_pkg "$d/run/ocbm-update.tgz" ""
  refusal "another chip" "is not one OCBM supports"
  d="$T/$sh-r-mfg"; make_box "$d" final; echo 1 > "$d/sys/module/libcomposite/parameters/mfgmode"; make_pkg "$d/run/ocbm-update.tgz" ""
  refusal "factory mode" "factory mode 1"
  d="$T/$sh-r-parse"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  mkdir -p "$T/x" && (cd "$T/x" && gzip -dc "$d/run/ocbm-update.tgz" | tar -xf - && mkdir -p files/script \
    && printf 'if then fi (\n' > files/script/ocbm_dispatch.sh \
    && awk -v s="$(sha files/script/ocbm_dispatch.sh | cut -c1-64)" '$2 == "script/ocbm_dispatch.sh" {$1 = s; print $1 "  " $2; next} {print}' SHA256SUMS > S2 \
    && mv S2 SHA256SUMS && tar -cf - . | gzip -c > "$d/run/ocbm-update.tgz") && rm -rf "$T/x"
  refusal "a start script that does not parse" "does not parse"
  d="$T/$sh-r-pending"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d"; rm -f "$d/rebooted"; make_pkg "$d/run/ocbm-update.tgz" ""; update "$sh" "$d"; rc=$?
  check '[ $rc = 1 ] && grep -q "already in its trial" "$d/root/etc/ocbm/update_result"' "an update already in its trial"
  d="$T/$sh-r-lock"; make_box "$d" final; make_pkg "$d/run/ocbm-update.tgz" ""; mkdir -p "$d/run/ocbm_mode.lock"
  update "$sh" "$d"; rc=$?
  check '[ $rc = 1 ] && is_old "$d"' "while a go-back or the finish holds the lock"

  echo "== $sh: the rest"
  d="$T/$sh-bump"; make_box "$d" final; NEW_SAVE="$NEW"; NEW="1.0.1 ocbm bump"
  w="$T/bump"; rm -rf "$w"; mkdir -p "$w"; old_tree "$T/bumptree"; sums_of "$T/bumptree" > "$w/SHA256SUMS"; rm -rf "$T/bumptree"
  echo "$NEW" > "$w/VERSION"; echo 1.0.0 > "$w/FROM_MIN"; (cd "$w" && tar -cf - .) | gzip -c > "$d/run/ocbm-update.tgz"
  update "$sh" "$d"; rc=$?
  check '[ $rc = 0 ] && [ ! -e "$d/rebooted" ] && [ "$(content "$d" etc/ocbm/installed_version)" = "$NEW" ] && result_is "$d" "committed $NEW"' \
    "a new version with no new files: committed at once, no restart"
  NEW="$NEW_SAVE"
  d="$T/$sh-revert"; make_box "$d" trial; make_pkg "$d/run/ocbm-update.tgz" ""; update "$sh" "$d"
  env ROOT="$d/root" RUN="$d/run" REBOOT="touch $d/rebooted.revert" "$sh" "$d/root/script/ocbm_revert.sh" "Go back" > /dev/null 2>&1; rc=$?
  check '[ $rc = 1 ] && [ "$(cat "$d/root/script/ocbm_mode")" = trial ] && [ ! -e "$d/rebooted.revert" ]' \
    "a go-back waits while an update is in its trial"
  d="$T/$sh-beta"; make_box "$d" final; NEW_SAVE="$NEW"; NEW="1.1.0-beta.1 ocbm beta"; make_pkg "$d/run/ocbm-update.tgz" ""
  update "$sh" "$d"; rc=$?
  check '[ $rc = 0 ]' "a pre-release (1.1.0-beta.1) is newer than 1.0.0"
  NEW="$NEW_SAVE"
  d="$T/$sh-beta2"; make_box "$d" final; echo "1.1.0 ocbm rel" > "$d/root/etc/ocbm/installed_version"
  NEW_SAVE="$NEW"; NEW="1.1.0-beta.2 ocbm beta"; make_pkg "$d/run/ocbm-update.tgz" "" 1.0.0
  update "$sh" "$d"; rc=$?
  check '[ $rc = 1 ] && grep -q "already runs OCBM 1.1.0" "$d/root/etc/ocbm/update_result"' "but older than 1.1.0 itself"
  NEW="$NEW_SAVE"
done

echo "== make_ocbm_update.sh"
b="$T/build"; mkdir -p "$b/bin" "$b/inst" "$b/from" "$b/out"
for f in ocbmd iap2d carplayd btd aa-bridge; do printf '%s v1\n' "$f" > "$b/bin/$f"; done
printf 'irs\n' > "$b/irs"
for f in ocbm_dispatch ocbm_selftest ocbm_revert ocbm_finish ocbm_update; do cp "$INST/$f.sh" "$b/inst/$f.sh"; done
echo 1.0.0 > "$b/inst/VERSION"
printf 'final_kb=3900\ntrial_kb=1252\n' > "$b/inst/update_budget"
export BIN_DIR="$b/bin" IRS_BIN="$b/irs" INSTALLER_DIR="$b/inst"
src_sums() {  # the payload's SHA256SUMS as the installer image would carry it
  local s="$b/stage"; rm -rf "$s"; mkdir -p "$s"
  "$REPO/tools/make_ocbm_installer.sh" --sources | while IFS='|' read -r src dst; do mkdir -p "$s/$(dirname "$dst")"; cp "$src" "$s/$dst"; done
  sums_of "$s"; rm -rf "$s"
}
src_sums > "$b/from/SHA256SUMS"; echo "1.0.0 ocbm base" > "$b/from/VERSION"
check '! "$REPO/tools/make_ocbm_update.sh" --from "$b/from/SHA256SUMS" "$b/out" >/dev/null 2>&1' "refuses to build a package of the version it updates from"
echo 1.1.0 > "$b/inst/VERSION"; printf 'ocbmd v2\n' > "$b/bin/ocbmd"
out=$("$REPO/tools/make_ocbm_update.sh" --from "$b/from/SHA256SUMS" "$b/out" 2>&1); rc=$?
pk="$b/out/ocbm-update-1.1.0.tgz"
mkdir -p "$b/x" && (cd "$b/x" && gzip -dc "$pk" | tar -xf -)
check '[ $rc = 0 ] && [ -s "$pk" ] && [ -s "$pk.sha256" ]' "builds ocbm-update-1.1.0.tgz and its .sha256"
check '[ "$(cd "$b/x/files" && find . -type f | sed "s|^\./||")" = usr/sbin/ocbmd ]' "files/ holds only what changed since FROM_MIN"
check '[ "$(wc -l < "$b/x/SHA256SUMS")" = "$(wc -l < "$b/from/SHA256SUMS")" ] && grep -q " script/ocbm_update.sh$" "$b/x/SHA256SUMS"' \
  "SHA256SUMS covers every payload file, the update script included"
check '[ "$(cat "$b/x/FROM_MIN")" = 1.0.0 ] && grep -q "^1.1.0 ocbm " "$b/x/VERSION" && grep -qx "trial=yes" "$b/x/ROOM"' \
  "VERSION, FROM_MIN and ROOM (fits a trial adapter)"
check '[ "$(gzip -dc "$pk" | tar -tvf - | awk "{print \$2}" | sort -u)" = root/root ] && ! gzip -dc "$pk" | tar -tf - | grep -q "\._"' \
  "every entry root-owned, and no AppleDouble ._ entries"
printf 'final_kb=3900\ntrial_kb=300\n' > "$b/inst/update_budget"
out=$("$REPO/tools/make_ocbm_update.sh" --from "$b/from/SHA256SUMS" "$b/out" 2>&1); rc=$?
rm -rf "$b/x"; mkdir -p "$b/x" && (cd "$b/x" && gzip -dc "$pk" | tar -xf -)
check '[ $rc = 0 ] && grep -qx "trial=no" "$b/x/ROOM" && echo "$out" | grep -q "going back to stock and converting again"' \
  "over the trial budget: built, marked trial=no"
printf 'final_kb=200\ntrial_kb=100\n' > "$b/inst/update_budget"
check '! "$REPO/tools/make_ocbm_update.sh" --from "$b/from/SHA256SUMS" "$b/out" >/dev/null 2>&1' "over the final budget: no package (no adapter could take it)"

echo
[ "$fails" = 0 ] && { echo "ocbm_update_test: all passed"; exit 0; }
echo "ocbm_update_test: $fails failed"; exit 1
