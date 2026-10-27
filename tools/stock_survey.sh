#!/usr/bin/env bash
# stock_survey.sh — build the read-only survey stick for a STOCK adapter (M0 of the one-file install).
#
#   tools/stock_survey.sh [OUT_DIR]      # default ~/.cache/carlink-artifacts/stock_survey
#
# Copy OUT_DIR/A15W_Update.img to the root of a FAT32 stick and plug it into the stock adapter as for
# a Carlinkit stick update. The image's once.sh (tools/stock_survey_box.sh) writes ocbm_survey_<n>/
# to the stick (report.txt, inventory.tsv, processes.txt, dmesg.txt, SURVEY_DONE) and then fails the update on
# purpose, so nothing on flash changes. Pull the stick once SURVEY_DONE is there, then power-cycle the
# adapter: stock keeps CarPlay down after a failed update.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$HOME/.cache/carlink-artifacts/stock_survey}"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/tmp"
install -m 755 "$REPO/tools/stock_survey_box.sh" "$stage/tmp/once.sh"
"$REPO/tools/make_update_img.sh" pack "$stage" "$OUT/A15W_Update.img"
