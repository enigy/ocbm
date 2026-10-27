#!/usr/bin/env bash
# make_update_img_test.sh — exercise tools/make_update_img.sh and the survey image, on the host.
#
# Packs synthetic trees, unwraps them again and checks owners, modes and content; checks both refusals;
# builds the survey image and checks it carries exactly stock_survey_box.sh as tmp/once.sh.
# With STOCK_IMG=<Carlinkit A15W_Update.img> it also unwraps a real vendor image.
#
#   tools/make_update_img_test.sh
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
MK="$HERE/make_update_img.sh"
T="$(mktemp -d "${TMPDIR:-/tmp}/make_update_img_test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

mkdir -p "$T/st/tmp" "$T/st/etc/sub"
printf '#!/bin/sh\nexit 0\n' > "$T/st/tmp/once.sh"; chmod 755 "$T/st/tmp/once.sh"
head -c 4099 /dev/urandom > "$T/st/etc/sub/blob"
"$MK" pack "$T/st" "$T/out/A15W_Update.img" >/dev/null 2>&1
check '[ -s "$T/out/A15W_Update.img" ]' "pack writes the image"
check '( cd "$T/out" && shasum -a 256 -c SHA256SUMS >/dev/null 2>&1 )' "SHA256SUMS verifies"
check '! cmp -s <(head -c 2 "$T/out/A15W_Update.img") <(printf "\037\213")' "the image is wrapped, not a bare gzip"
"$MK" unpack "$T/out/A15W_Update.img" "$T/u.tgz" >/dev/null 2>&1
mkdir -p "$T/x"; tar -xzf "$T/u.tgz" -C "$T/x" 2>/dev/null
check 'cmp -s "$T/st/etc/sub/blob" "$T/x/etc/sub/blob"' "unpack gives back the staged bytes"
check '[ -x "$T/x/tmp/once.sh" ]' "once.sh stays executable"
check '[ "$(tar -tvzf "$T/u.tgz" | awk "{print \$3\":\"\$4}" | sort -u)" = root:root ]' "every entry is root:root"
check 'tar -tzf "$T/u.tgz" | grep -qx "./tmp/once.sh"' "entries are ./-prefixed like Carlinkit's"

mkdir -p "$T/bad1/tmp"; touch "$T/bad1/tmp/AutoBox.img"
check '! "$MK" pack "$T/bad1" "$T/bad1.img" >/dev/null 2>&1 && [ ! -e "$T/bad1.img" ]' "refuses tmp/AutoBox.img"
mkdir -p "$T/bad2/tmp"; printf '#!/bin/sh\n' > "$T/bad2/tmp/once.sh"; chmod 644 "$T/bad2/tmp/once.sh"
check '! "$MK" pack "$T/bad2" "$T/bad2.img" >/dev/null 2>&1' "refuses a non-executable once.sh"

bash "$HERE/stock_survey.sh" "$T/survey" >/dev/null 2>&1
"$MK" unpack "$T/survey/A15W_Update.img" "$T/s.tgz" >/dev/null 2>&1
mkdir -p "$T/sx"; tar -xzf "$T/s.tgz" -C "$T/sx" 2>/dev/null
check '[ "$(tar -tzf "$T/s.tgz" | tr "\n" " ")" = "./tmp/ ./tmp/once.sh " ]' "survey image holds only tmp/once.sh"
check 'cmp -s "$HERE/stock_survey_box.sh" "$T/sx/tmp/once.sh"' "survey once.sh is stock_survey_box.sh"
check 'sh -n "$T/sx/tmp/once.sh"' "survey once.sh parses"
check '[ "$(grep -v "^[[:space:]]*\(#\|$\)" "$T/sx/tmp/once.sh" | tail -1)" = "exit 1" ]' "survey ends by failing the update"

if [ -n "${STOCK_IMG:-}" ]; then
  "$MK" unpack "$STOCK_IMG" "$T/v.tgz" >/dev/null 2>&1
  check 'tar -tzf "$T/v.tgz" 2>/dev/null | grep -qx "./tmp/once.sh"' "vendor image unwraps ($STOCK_IMG)"
fi

[ "$fails" -eq 0 ] && echo "make_update_img_test: all passed" || { echo "make_update_img_test: $fails failed"; exit 1; }
