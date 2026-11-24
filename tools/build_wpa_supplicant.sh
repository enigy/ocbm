#!/bin/sh
# build_wpa_supplicant.sh — cross-build the Wi-Fi link's station supplicant for the adapter: wpa_supplicant and wpa_cli
# 2.11, static, armv7 musl (zig cc), with libnl 3.12.0 for nl80211 (WEXT kept as the fallback driver), WPA2-PSK only:
# no SAE, so a WPA3-only hotspot is not supported in this version (WPA2 and WPA2/WPA3 mixed are). The installer image
# places the two as /usr/sbin/ocbm-wpa_supplicant and /usr/sbin/ocbm-wpa_cli (tools/make_ocbm_installer.sh, from
# $WPA_DIR/install), NEVER as /usr/sbin/wpa_supplicant: the vendor's dispatcher reads that path's existence as "STA
# mode" and loads the IW416's driver in another mode (drv_mode=5, no AP interface) on every unit, set up or not.
#
#   tools/build_wpa_supplicant.sh        # WPA_DIR defaults to <repo>/scratchpad/wpa (ignored by git)
#
# THE SOURCES ARE NOT IN THE REPOSITORY. A checkout fetches the two release archives once, by hand, into $WPA_DIR/dl:
#   https://w1.fi/releases/wpa_supplicant-2.11.tar.gz                                          (BSD-3-Clause)
#   https://github.com/thom311/libnl/releases/download/libnl3_12_0/libnl-3.12.0.tar.gz          (LGPL-2.1)
# Each is checked against the SHA-256 pinned below before anything is unpacked, and nothing from them runs but their
# own configure and make, in $WPA_DIR/src, away from the repository's scripts. The pins are set at the first download,
# after comparing each archive with what its publisher (or a distribution's package recipe) lists; until then this
# script refuses to build.
#
# Recipe read from the outside pull request's tools/build_wpa_supplicant.sh (enigy/ocbm#2), written anew: why nl80211
# and not WEXT alone (WEXT does not pass a beacon's RSNXE up from the scan, and the 4-way handshake then fails against
# a WPA3-capable access point with "RSNXE mismatch ... msg 3/4", that request's bench of 11 Nov), and why only libnl-3
# and libnl-genl-3 (wpa_supplicant links nothing else, and libnl's route library needs a newer bison than macOS has).
set -eu
REPO=$(cd "$(dirname "$0")/.." && pwd)
W=${WPA_DIR:-$REPO/scratchpad/wpa}
WPA_TGZ=wpa_supplicant-2.11.tar.gz
# Pinned 24 Nov 2026 from the archives fetched that day (3,841,433 B and 1,191,546 B). wpa_supplicant's SHA-512 begins
# 9a0a3a9d and ends e277b2, as Alpine's aports record it; libnl's SHA-256 is the one its release lists.
WPA_SHA256=912ea06f74e30a8e36fbb68064d6cdff218d8d591db0fc5d75dee6c81ac7fc0a
NL_TGZ=libnl-3.12.0.tar.gz
NL_SHA256=fc51ca7196f1a3f5fdf6ffd3864b50f4f9c02333be28be4eeca057e103c0dd18

die() { echo "build_wpa_supplicant: $*" >&2; exit 1; }
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
verify() {  # $1 archive name, $2 pinned SHA-256
  [ -s "$W/dl/$1" ] || die "no $W/dl/$1: fetch it as the header says"
  [ -n "$2" ] || die "no SHA-256 pinned for $1 yet: compare $(sha256 "$W/dl/$1") with its publisher's, then pin it in $0"
  [ "$(sha256 "$W/dl/$1")" = "$2" ] || die "$1 does not match its pinned SHA-256: refusing to unpack it"
}
command -v zig >/dev/null 2>&1 || die "no zig (the cross C compiler build.sh uses)"
verify "$WPA_TGZ" "$WPA_SHA256"
verify "$NL_TGZ" "$NL_SHA256"

rm -rf "$W/src" "$W/install"
mkdir -p "$W/src" "$W/install"
tar -xzf "$W/dl/$WPA_TGZ" -C "$W/src"
tar -xzf "$W/dl/$NL_TGZ" -C "$W/src"
WPA_SRC=$W/src/${WPA_TGZ%.tar.gz}
NL_SRC=$W/src/${NL_TGZ%.tar.gz}
[ -d "$WPA_SRC/wpa_supplicant" ] && [ -d "$NL_SRC/lib" ] || die "the archives did not unpack as expected"

CC_ARM="zig cc -target arm-linux-musleabihf -mcpu=cortex_a7"
export ZIG_GLOBAL_CACHE_DIR="${ZIG_GLOBAL_CACHE_DIR:-$W/zigcache}" ZIG_LOCAL_CACHE_DIR="${ZIG_LOCAL_CACHE_DIR:-$W/zigcache}"

# libnl: its two libraries, static.
( cd "$NL_SRC"
  CC="$CC_ARM" AR="zig ar" RANLIB="zig ranlib" CFLAGS="-Os -ffunction-sections -fdata-sections" \
    ./configure --host=arm-linux-musleabihf --disable-shared --enable-static --disable-cli --disable-debug >/dev/null
  make -j8 lib/libnl-3.la lib/libnl-genl-3.la >/dev/null )

# wpa_supplicant: nl80211 + WEXT, the control interface (wpa_cli), internal crypto, and nothing else: no EAP, no WPS,
# no P2P, no SAE (WPA2-PSK only), no D-Bus.
cat > "$WPA_SRC/wpa_supplicant/.config" <<EOF
CONFIG_DRIVER_NL80211=y
CONFIG_LIBNL32=y
LIBNL_INC=$NL_SRC/include
CONFIG_DRIVER_WEXT=y
CONFIG_CTRL_IFACE=y
CONFIG_BACKEND=file
CONFIG_TLS=internal
CONFIG_INTERNAL_LIBTOMMATH=y
CONFIG_NO_RANDOM_POOL=y
EOF
# Flags through the environment, not make's command line: a command-line CFLAGS replaces the Makefile's own -I additions.
( cd "$WPA_SRC/wpa_supplicant"
  CC="$CC_ARM" CFLAGS="-Os -MMD -ffunction-sections -fdata-sections" \
    LDFLAGS="-static -Wl,--gc-sections -s -L$NL_SRC/lib/.libs" \
    make -j8 wpa_supplicant wpa_cli >/dev/null )

install -m 755 "$WPA_SRC/wpa_supplicant/wpa_supplicant" "$W/install/ocbm-wpa_supplicant"
install -m 755 "$WPA_SRC/wpa_supplicant/wpa_cli" "$W/install/ocbm-wpa_cli"
# The licences travel with the binaries (libnl is linked statically from the release named above, unmodified).
install -m 644 "$WPA_SRC/COPYING" "$W/install/NOTICE.wpa_supplicant"
install -m 644 "$NL_SRC/COPYING" "$W/install/NOTICE.libnl"
echo "built  $W/install/ocbm-wpa_supplicant ($(wc -c < "$W/install/ocbm-wpa_supplicant") bytes), ocbm-wpa_cli ($(wc -c < "$W/install/ocbm-wpa_cli") bytes)"
