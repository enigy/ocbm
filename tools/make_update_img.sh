#!/usr/bin/env bash
# make_update_img.sh — pack a staged tree into a Carlinkit stick/OTA update image, or unpack one.
#
# The stock updater (/script/update_box.sh, byte-identical on 2025.02.25.1521 and 2025.10.15.1127)
# copies <box_product_type>_Update.img (A15W_Update.img on a CCPA) from the stick to /tmp, unwraps it
# with ARMimg_maker, untars update.tar.gz into /tmp/update, runs tmp/once.sh as root, copies the tree
# over / (quickly_update.sh) and runs tmp/finish.sh. A non-zero once.sh stops it before the copy.
# The wrapping is AES-128-CBC with key = IV = "AutoPlay9uPT4n17" over the whole 16-byte blocks; a
# tail shorter than 16 bytes stays in clear. There is no signature.
#
#   tools/make_update_img.sh pack STAGE_DIR OUT.img      # tar (./ entries, root:root) + wrap + check
#   tools/make_update_img.sh unpack IN.img OUT.tar.gz    # unwrap only
#
# pack refuses a tree carrying tmp/AutoBox.img: vendor once.sh scripts flash it with `upg` over U-Boot
# and the kernel, which this project never modifies. pack writes SHA256SUMS next to OUT.img.
# Keep images out of ~/Documents (iCloud); callers default to ~/.cache/carlink-artifacts.
set -euo pipefail

KEY="$(printf %s AutoPlay9uPT4n17 | od -A n -t x1 | tr -d ' \n')"

die() { printf 'make_update_img: %s\n' "$*" >&2; exit 1; }

wrap() {  # $1 -e|-d, $2 in, $3 out
  local sz whole tmp
  sz=$(wc -c < "$2" | tr -d ' '); whole=$(( sz - sz % 16 ))
  tmp="$(mktemp)"
  head -c "$whole" "$2" > "$tmp"
  openssl enc "$1" -aes-128-cbc -nopad -K "$KEY" -iv "$KEY" -in "$tmp" -out "$3"
  tail -c $(( sz - whole )) "$2" >> "$3"
  rm -f "$tmp"
}

pack() {  # $1 stage dir, $2 out.img
  local stage out work owners
  stage="$(cd "$1" && pwd)"; out="$2"
  [ -n "$(ls -A "$stage")" ] || die "stage dir is empty: $stage"
  [ ! -e "$stage/tmp/AutoBox.img" ] || die "refusing tmp/AutoBox.img: it rewrites U-Boot and the kernel"
  if [ -e "$stage/tmp/once.sh" ]; then
    [ -x "$stage/tmp/once.sh" ] || die "tmp/once.sh must be executable: update_box.sh runs it directly"
    head -1 "$stage/tmp/once.sh" | grep -q '^#!/bin/sh' || die "tmp/once.sh must start with #!/bin/sh"
  fi
  work="$(mktemp -d)"
  # ./-prefixed entries like Carlinkit's own images; root:root because busybox tar keeps numeric owners.
  ( cd "$stage" && find . -mindepth 1 | LC_ALL=C sort > "$work/list" )
  ( cd "$stage" && COPYFILE_DISABLE=1 tar --format ustar --uid 0 --gid 0 --uname root --gname root \
      --no-mac-metadata --no-xattrs -n -c -f - -T "$work/list" ) | gzip -n -9 > "$work/update.tar.gz"
  mkdir -p "$(dirname "$out")"
  wrap -e "$work/update.tar.gz" "$out"

  # Round trip: what the box will unwrap must be the archive we built, and every entry root-owned.
  wrap -d "$out" "$work/check.tar.gz"
  cmp -s "$work/update.tar.gz" "$work/check.tar.gz" || die "round trip failed: unwrapped image differs"
  owners="$(tar -tvzf "$work/check.tar.gz" | awk '{print $3":"$4}' | sort -u)"
  [ "$owners" = "root:root" ] || die "archive owners are '$owners', want root:root"
  ( cd "$(dirname "$out")" && shasum -a 256 "$(basename "$out")" > SHA256SUMS )
  printf 'make_update_img: %s (%s bytes, %s entries) round trip ok\n' \
    "$out" "$(wc -c < "$out" | tr -d ' ')" "$(wc -l < "$work/list" | tr -d ' ')"
  rm -rf "$work"
}

case "${1:-}" in
  pack)   [ $# -eq 3 ] || die "usage: pack STAGE_DIR OUT.img"; pack "$2" "$3" ;;
  unpack) [ $# -eq 3 ] || die "usage: unpack IN.img OUT.tar.gz"; wrap -d "$2" "$3" ;;
  *)      sed -n '2,17p' "$0"; exit 2 ;;
esac
