#!/bin/sh
# Cross-build libfdk-aac (static, armv7 musl) into scratchpad/fdk/install for carplayd's
# mic-uplink-eld feature. build.sh calls this when the install prefix is missing and the
# source is present at scratchpad/fdk/src (fdk-aac 2.0.3, github.com/mstorsjo/fdk-aac).
#
# Why a plain compile loop instead of autotools/cmake: the box toolchain is `zig cc`, which has
# no configure triplet, and fdk-aac has no build-time configuration beyond the compiler flags
# in its Makefile.am (-fno-exceptions -fno-rtti). Output layout matches what
# crates/vendor/eld-codec/build.rs expects: include/fdk-aac/*.h + lib/libfdk-aac.a.
set -e
cd "$(dirname "$0")/.."
SRC="${FDK_AAC_SRC:-$PWD/scratchpad/fdk/src}"
DST="${FDK_AAC_PREFIX:-$PWD/scratchpad/fdk/install}"
TARGET="${FDK_ZIG_TARGET:-arm-linux-musleabihf}"
CPU="${FDK_ZIG_CPU:-cortex_a7}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"

[ -d "$SRC/libAACenc/src" ] || { echo "build_fdk: no fdk-aac source at $SRC" >&2; exit 1; }
command -v zig >/dev/null 2>&1 || { echo "build_fdk: zig not found" >&2; exit 1; }

OBJ="$DST/.obj"
rm -rf "$DST"
mkdir -p "$OBJ" "$DST/lib" "$DST/include/fdk-aac"

INC=""
for d in "$SRC"/lib*/include "$SRC"/lib*/src; do INC="$INC -I$d"; done

CXX="zig c++ -target $TARGET -mcpu=$CPU"
# Same flags as fdk-aac's Makefile.am (AM_CXXFLAGS) plus the box size profile.
CXXFLAGS="-Os -fPIC -fno-exceptions -fno-rtti -fno-sanitize=all -Wno-date-time -DNDEBUG -ffunction-sections -fdata-sections"

echo "build_fdk: compiling $(ls "$SRC"/lib*/src/*.cpp | wc -l) files for $TARGET/$CPU with zig $(zig version)"
ls "$SRC"/lib*/src/*.cpp | xargs -P "$JOBS" -I{} sh -c '
  f="$1"; lib=$(basename "$(dirname "$(dirname "$f")")"); o="$2/${lib}_$(basename "$f" .cpp).o"
  '"$CXX $CXXFLAGS $INC"' -c "$f" -o "$o"' _ {} "$OBJ"

zig ar rcs "$DST/lib/libfdk-aac.a" "$OBJ"/*.o
rm -rf "$OBJ"

# Public headers, same set fdk-aac's `make install` publishes under include/fdk-aac/.
for h in libAACenc/include/aacenc_lib.h libAACdec/include/aacdecoder_lib.h \
         libSYS/include/FDK_audio.h libSYS/include/genericStds.h \
         libSYS/include/machine_type.h libSYS/include/syslib_channelMapDescr.h; do
  cp "$SRC/$h" "$DST/include/fdk-aac/"
done

echo "built  $DST/lib/libfdk-aac.a  ($(wc -c < "$DST/lib/libfdk-aac.a") bytes, $TARGET static)"
