#!/usr/bin/env bash
# make_ocbm_update.sh — build the update package (ocbm-update-X.Y.Z.tgz) that takes an adapter already
# on a one-file OCBM install (trial or final) to this tree's OCBM over the app's OCBM link: Carlink
# pushes it to /tmp/ocbm-update.tgz and sends MGMT_UPDATE, and ccpa/installer/ocbm_update.sh installs
# it as a trial that falls back to the version it replaced.
#
#   tools/make_ocbm_update.sh --from SUMS [OUT_DIR]   # default ~/.cache/carlink-artifacts/ocbm_update
#
# SUMS is the SHA256SUMS of the version the package updates from (an installer image's tmp/ocbm/
# SHA256SUMS, or an earlier package's), with that version's VERSION file beside it. The package holds
# this version's sums for every payload file (SHA256SUMS), but only the files that differ from SUMS
# (files/), and FROM_MIN names the version SUMS came from: an adapter on it, or on anything between it
# and this version, takes the package; ocbm_update.sh refuses an older one rather than leave it half
# updated.
#
# Room: the files that change, at JFFS2's ~0.7 of their size, plus the 384 KB spare, must fit
# final_kb in ccpa/installer/update_budget, or the build fails: no adapter could take the package.
# Over trial_kb it says so, and writes trial=no in the package's ROOM file: Carlink then updates
# trial adapters by going back to stock and converting again.
#
# Build the box binaries first (build.sh), as for tools/make_ocbm_installer.sh, whose payload list
# this uses (--sources). Test hooks (tools/ocbm_update_test.sh): BIN_DIR IRS_BIN INSTALLER_DIR BUDGET.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER_DIR="${INSTALLER_DIR:-$REPO/ccpa/installer}"
BUDGET="${BUDGET:-$INSTALLER_DIR/update_budget}"
RESERVE_KB=384
export INSTALLER_DIR

die() { printf 'make_ocbm_update: %s\n' "$*" >&2; exit 1; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
kb_on_flash() { local s; s=$(du -k "$1" | cut -f1); echo $(( (s * 7 + 9) / 10 )); }   # as ocbm_update.sh counts

from=""
OUT="$HOME/.cache/carlink-artifacts/ocbm_update"
while [ $# -gt 0 ]; do
  case "$1" in
    --from) from="${2:-}"; shift 2 ;;
    -*) die "unknown option $1" ;;
    *) OUT="$1"; shift ;;
  esac
done
[ -s "$from" ] || die "usage: make_ocbm_update.sh --from SUMS [OUT_DIR] (SUMS: the SHA256SUMS of the version it updates from)"
from_ver=$(head -1 "$(dirname "$from")/VERSION" 2>/dev/null | awk '{print $1}')
[ -n "$from_ver" ] || die "no VERSION beside $from: it names the version the package updates from"
final_kb=$(sed -n 's/^final_kb=//p' "$BUDGET" 2>/dev/null)
trial_kb=$(sed -n 's/^trial_kb=//p' "$BUDGET" 2>/dev/null)
[ -n "$final_kb" ] && [ -n "$trial_kb" ] || die "no final_kb and trial_kb in $BUDGET"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/all" "$stage/pkg/files"
sources="$("$REPO/tools/make_ocbm_installer.sh" --sources)" || die "tools/make_ocbm_installer.sh --sources failed"
missing=""
while IFS='|' read -r src dst; do
  [ -s "$src" ] || { missing="$missing $src"; continue; }
  mkdir -p "$stage/all/$(dirname "$dst")"
  cp "$src" "$stage/all/$dst" && chmod 755 "$stage/all/$dst"
done <<< "$sources"
[ -z "$missing" ] || die "missing inputs:$missing (box binaries: run build.sh)"

( cd "$stage/all" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | while read -r f; do sha "$f"; done ) \
  | awk '{print $1 "  " $2}' > "$stage/pkg/SHA256SUMS"
changed=0
need_kb=$RESERVE_KB
while read -r sum rel; do
  old=$(awk -v r="$rel" '$2 == r {print $1}' "$from")
  [ "$old" = "$sum" ] && continue
  mkdir -p "$stage/pkg/files/$(dirname "$rel")"
  cp -p "$stage/all/$rel" "$stage/pkg/files/$rel"
  need_kb=$((need_kb + $(kb_on_flash "$stage/all/$rel")))
  changed=$((changed + 1))
  echo "  changed: $rel"
done < "$stage/pkg/SHA256SUMS"

rev="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)"
git -C "$REPO" diff --quiet HEAD 2>/dev/null || rev="$rev-dirty"
ver=$(head -1 "$INSTALLER_DIR/VERSION" 2>/dev/null)
[ -n "$ver" ] || die "no $INSTALLER_DIR/VERSION"
[ "$ver" != "$from_ver" ] || die "this tree is OCBM $ver, the version it would update from: bump $INSTALLER_DIR/VERSION"
echo "$ver ocbm $rev" > "$stage/pkg/VERSION"
echo "$from_ver" > "$stage/pkg/FROM_MIN"

[ "$need_kb" -le "$final_kb" ] \
  || die "the package needs about $need_kb KB of flash ($changed files plus $RESERVE_KB KB spare), more than a final adapter has ($final_kb KB, $BUDGET): no adapter could take it"
trial=yes
if [ "$need_kb" -gt "$trial_kb" ]; then
  trial=no
  echo "make_ocbm_update: about $need_kb KB is more than a trial adapter has ($trial_kb KB): Carlink updates trial adapters by going back to stock and converting again" >&2
fi
printf 'need_kb=%s\ntrial=%s\n' "$need_kb" "$trial" > "$stage/pkg/ROOM"

mkdir -p "$OUT"
pkg="$OUT/ocbm-update-$ver.tgz"
# Plain ustar, root-owned, and nothing but the package: the Mac's tar adds a ._ entry beside every file
# with extended attributes unless told not to (b93e050), and the adapter would unpack each one.
if tar --version 2>/dev/null | grep -q bsdtar; then
  tflags=(--format ustar --uid 0 --gid 0 --uname root --gname root --no-mac-metadata --no-xattrs)
else
  tflags=(--format=ustar --owner=root:0 --group=root:0 --sort=name)
fi
( cd "$stage/pkg" && COPYFILE_DISABLE=1 tar "${tflags[@]}" -cf - VERSION FROM_MIN SHA256SUMS ROOM files ) | gzip -n -9 > "$pkg"
stray=$(gzip -dc "$pkg" | tar -tf - | grep -Ev '^(VERSION|FROM_MIN|SHA256SUMS|ROOM|files/.*)$' || true)
stray="$stray$(gzip -dc "$pkg" | tar -tf - | grep -E '(^|/)\._' || true)"
[ -z "$stray" ] || { rm -f "$pkg"; die "the package holds entries it should not:"$'\n'"$stray"; }
( cd "$OUT" && sha "$(basename "$pkg")" > "$(basename "$pkg").sha256" )
printf 'make_ocbm_update: %s (from %s): %s files changed, about %s KB on flash, trial=%s\n  %s\n  %s\n' \
  "$ver ocbm $rev" "$from_ver" "$changed" "$need_kb" "$trial" "$pkg" "$(cut -d' ' -f1 "$pkg.sha256")"
