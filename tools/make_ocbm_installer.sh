#!/usr/bin/env bash
# make_ocbm_installer.sh — build the one-file OCBM installer (A15W_Update.img) that converts a STOCK
# CPC200-CCPA on Carlinkit 2025.10.15.1127 from a USB stick or over USB from Carlink3.
#
#   tools/make_ocbm_installer.sh [OUT_DIR]   # default ~/.cache/carlink-artifacts/ocbm_installer
#   tools/make_ocbm_installer.sh --list      # print the payload's on-box paths
#
# Stages tmp/once.sh + tmp/finish.sh (Stage A, ccpa/installer/) and tmp/ocbm/: the box binaries and
# scripts of `ocbm_install.sh --full` (keep the two lists in step: make_ocbm_installer's test
# compares them), the Stage B/C scripts, prune.list and identity.list. Writes tmp/ocbm/VERSION,
# BUDGET (flash the payload needs on JFFS2) and SHA256SUMS, then wraps the tree with
# make_update_img.sh. Build the box binaries first (build.sh).
#
# Overrides for tools/ocbm_installer_test.sh: BIN_DIR, IRS_BIN, INSTALLER_DIR.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${BIN_DIR:-$REPO/target/armv7-unknown-linux-musleabihf/release}"
IRS_BIN="${IRS_BIN:-$REPO/accessory_init/iap_role_switch.armv7}"
INSTALLER_DIR="${INSTALLER_DIR:-$REPO/ccpa/installer}"

die() { printf 'make_ocbm_installer: %s\n' "$*" >&2; exit 1; }

payload() {  # SOURCE|PATH-ON-BOX: ocbm_install.sh manifest() --full, then Stage B/C
  local b s
  for b in ocbmd iap2d carplayd btd aa-bridge; do echo "$BIN_DIR/$b|usr/sbin/$b"; done
  echo "$IRS_BIN|usr/bin/iap_role_switch"
  echo "$REPO/ccpa/rootfs/script/ocbm_boot.sh|script/ocbm_boot.sh"
  echo "$REPO/tools/run_ocbmd.sh|script/run_ocbmd.sh"
  for s in radio_detect radio_hal radio_ap_up; do echo "$REPO/ccpa/rootfs/script/$s.sh|script/$s.sh"; done
  for s in session_supervisor projection_up phone_reset run_supervisor; do echo "$REPO/tools/$s.sh|script/$s.sh"; done
  for s in ocbm_dispatch ocbm_selftest ocbm_revert ocbm_finish; do echo "$INSTALLER_DIR/$s.sh|script/$s.sh"; done
}

if [ "${1:-}" = --list ]; then payload | cut -d'|' -f2; exit 0; fi
OUT="${1:-$HOME/.cache/carlink-artifacts/ocbm_installer}"

missing=""
while IFS='|' read -r src dst; do [ -s "$src" ] || missing="$missing\n  $src"; done < <(payload)
for f in once.sh finish.sh prune.list identity.list; do [ -s "$INSTALLER_DIR/$f" ] || missing="$missing\n  $INSTALLER_DIR/$f"; done
[ -z "$missing" ] || die "missing inputs:$(printf "$missing")
  (box binaries: run build.sh; ocbm_dispatch/selftest/revert/finish.sh are Stage B/C, milestones M3-M4)"
grep -q '^# OCBM_DISPATCH' "$INSTALLER_DIR/ocbm_dispatch.sh" || die "ocbm_dispatch.sh lacks its '# OCBM_DISPATCH' line"
if grep -vE '^[[:space:]]*(#|$)' "$INSTALLER_DIR/prune.list" | grep -qE '^/|\.\.'; then
  die "prune.list entries must be relative to / and must not contain '..'"
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
P="$stage/tmp/ocbm"
mkdir -p "$P"
install -m 755 "$INSTALLER_DIR/once.sh" "$stage/tmp/once.sh"
install -m 755 "$INSTALLER_DIR/finish.sh" "$stage/tmp/finish.sh"
install -m 644 "$INSTALLER_DIR/prune.list" "$INSTALLER_DIR/identity.list" "$P/"
while IFS='|' read -r src dst; do
  mkdir -p "$P/$(dirname "$dst")"
  install -m 755 "$src" "$P/$dst"
done < <(payload)

rev="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)"
git -C "$REPO" diff --quiet HEAD 2>/dev/null || rev="$rev-dirty"
echo "1.0.0 ocbm $rev" > "$P/VERSION"

# BUDGET: what the payload occupies once written to JFFS2 (4 KiB pages, zlib, 68-byte node headers,
# a dirent each), plus 5%. This model came within 3% of df on the factory survey of 2026-10-27.
need_kb="$(python3 -I - "$P" <<'EOF'
import os, sys, zlib
total = 0
for top in ("usr", "script"):
    for d, _, fs in os.walk(os.path.join(sys.argv[1], top)):
        for f in fs:
            data = open(os.path.join(d, f), "rb").read()
            for i in range(0, max(len(data), 1), 4096):
                page = data[i:i + 4096]
                total += 68 + ((min(len(zlib.compress(page, 3)), len(page)) + 3) & ~3)
            total += 40 + ((len(f) + 3) & ~3)
print(int(total * 1.05 / 1024) + 1)
EOF
)"
echo "need_kb=$need_kb" > "$P/BUDGET"

( cd "$P" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | xargs shasum -a 256 ) > "$stage/SHA256SUMS"
mv "$stage/SHA256SUMS" "$P/SHA256SUMS"
printf 'make_ocbm_installer: %s, payload needs %s KB on flash\n' "$(cat "$P/VERSION")" "$need_kb"
"$REPO/tools/make_update_img.sh" pack "$stage" "$OUT/A15W_Update.img"
