#!/bin/sh
# Cross-build wpa_supplicant + wpa_cli (static, armv7 musl) into scratchpad/wpa/install for the Wi-Fi
# transport's STA side (ccpa/rootfs/script/wifi_sta.sh). Neither OCBM nor the stock Carlinkit image
# ships one (stock's scripts only enable STA mode if /usr/sbin/wpa_supplicant exists). make_ocbm_installer.sh
# puts them in the payload when scratchpad/wpa/install has them.
#
# Sources (checked by hand; neither is fetched here):
#   scratchpad/wpa/src/wpa_supplicant-2.11   https://w1.fi/releases/wpa_supplicant-2.11.tar.gz
#                                            sha512 9a0a3a9d...e277b2 (Alpine aports main/wpa_supplicant)
#   scratchpad/wpa/src/libnl-3.12.0          https://github.com/thom311/libnl/releases/download/libnl3_12_0/
#                                            libnl-3.12.0.tar.gz, sha256 fc51ca71...c0dd18 (release + Alpine)
#
# WHY nl80211 AND NOT JUST WEXT. WEXT is the smaller build (no libnl), but it doesn't pass a beacon's
# RSNXE up from the scan, so against a WPA3-capable router (SAE-H2E) the handshake fails with "RSNXE
# mismatch ... msg 3/4" (bench 2026-11-11). nl80211 is also what the vendor's scripts use. WEXT stays
# compiled in as the fallback (`-D nl80211,wext`).
#
# WHY ONLY libnl-3 AND libnl-genl-3. wpa_supplicant links nothing else, and libnl's route library needs
# a newer bison than macOS ships to regenerate its grammars.
set -eu
REPO=$(cd "$(dirname "$0")/.." && pwd)
W=${WPA_DIR:-$REPO/scratchpad/wpa}
WPA_SRC=${WPA_SRC:-$W/src/wpa_supplicant-2.11}
NL_SRC=${LIBNL_SRC:-$W/src/libnl-3.12.0}
OUT=$W/install
[ -d "$WPA_SRC/wpa_supplicant" ] || { echo "build_wpa_supplicant: no source at $WPA_SRC" >&2; exit 1; }
[ -d "$NL_SRC/lib" ] || { echo "build_wpa_supplicant: no libnl source at $NL_SRC" >&2; exit 1; }

CC_ARM="zig cc -target arm-linux-musleabihf -mcpu=cortex_a7"
export ZIG_GLOBAL_CACHE_DIR=${ZIG_GLOBAL_CACHE_DIR:-$W/zigcache} ZIG_LOCAL_CACHE_DIR=${ZIG_LOCAL_CACHE_DIR:-$W/zigcache}

# libnl: the two libraries only.
if [ ! -f "$NL_SRC/lib/.libs/libnl-genl-3.a" ]; then
  ( cd "$NL_SRC"
    CC="$CC_ARM" AR="zig ar" RANLIB="zig ranlib" CFLAGS="-Os -ffunction-sections -fdata-sections" \
      ./configure --host=arm-linux-musleabihf --disable-shared --enable-static --disable-cli --disable-debug >/dev/null
    make -j8 lib/libnl-3.la lib/libnl-genl-3.la >/dev/null )
fi

# wpa_supplicant: nl80211 + wext, internal crypto, control interface for wpa_cli.
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
# Flags through the environment, not make's command line: a command-line CFLAGS replaces the
# Makefile's own -I additions instead of extending them.
( cd "$WPA_SRC/wpa_supplicant"
  make clean >/dev/null 2>&1 || true
  CC="$CC_ARM" CFLAGS="-Os -MMD -ffunction-sections -fdata-sections" \
    LDFLAGS="-static -Wl,--gc-sections -s -L$NL_SRC/lib/.libs" \
    make -j8 wpa_supplicant wpa_cli >/dev/null )

mkdir -p "$OUT"
install -m 755 "$WPA_SRC/wpa_supplicant/wpa_supplicant" "$WPA_SRC/wpa_supplicant/wpa_cli" "$OUT/"
# The licence travels with the binaries (BSD; libnl is LGPL-2.1, linked statically: its source is the
# release named above, unmodified).
cp "$WPA_SRC/COPYING" "$OUT/COPYING.wpa_supplicant"
cp "$NL_SRC/COPYING" "$OUT/COPYING.libnl"
echo "built  $OUT/wpa_supplicant ($(wc -c < "$OUT/wpa_supplicant") bytes), wpa_cli ($(wc -c < "$OUT/wpa_cli") bytes)"
