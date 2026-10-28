#!/bin/sh
# OCBM_STAGE_C 1
# ocbm_finish.sh — Stage C of the one-file install: once an app that offers the way back has confirmed
# the trial (MGMT_INSTALL_CONFIRM) and that session is over, remove the Carlinkit stack and set
# /script/ocbm_mode to final. After this there is no stock left to go back to.
#
#   ocbm_finish.sh --after-session   ocbmd, when a confirmed session ends. Waits GRACE (60) s first and
#                                    stops if an app is back meanwhile (/tmp/host_present = 1): an app
#                                    restarting mid-drive has not ended the session.
#   ocbm_finish.sh                   the dispatcher: at a start in `confirmed` once the self-test passed
#                                    (the car cut the power before the session could end), and at a
#                                    start in `final` that a power cut interrupted (finish_pending)
#
# Power loss: /etc/ocbm/finish_pending is written, then the mode becomes `final`, so no later start
# hands the boot to a half-removed stock. The deletes follow, and finish_pending goes last; a start in
# `final` that still finds it runs this again. ocbm_revert.sh takes the same lock, so a go-back and a
# finish never interleave.
#
# What goes is tools/ncm_base_install.sh's strip set (a test keeps the lists equal): the vendor
# programs (SBIN_A), scripts (SCRIPT_A, the stock updater among them), config and logos (ETC_A,
# /etc/boa), and the vendor and toolchain libraries (LIB_VENDOR, LIB_DEV) that no surviving file
# names, plus start_main_service.stock.sh. Every candidate passes is_radio() first, and no Carlinkit
# daemon may be running. Kept: the radio stack, the i2c tools, riddle.conf, and what the OCBM start
# still uses (rcS, init_gpio.sh, ko.tar.gz). The stick handlers keep mounting sticks, with their call
# into the deleted updater commented out, as in ccpa/rootfs/etc/mdev/udisk_insert.sh.
#
# Test hooks (tools/ocbm_stageb_test.sh): ROOT RUN GRACE POLL, FINISH_IGNORE for daemon names the test
# host itself runs (macOS has its own usbmuxd), and STOP_AT=deletes to simulate power loss (exit 99)
# right before the first delete.
ROOT="${ROOT:-}"
RUN="${RUN:-/tmp}"
GRACE="${GRACE:-60}"
POLL="${POLL:-2}"
S="$ROOT/script"
EO="$ROOT/etc/ocbm"
MODE="$S/ocbm_mode"
PENDING="$EO/finish_pending"
GONE="$EO/finished.txt"
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH

SBIN_A="AppleCarPlay ARMadb-driver ARMandroid_Mirror ARMAndroidAuto ARMHiCar ARMiPhoneIAP2 \
AutomaticTest boa boa_old boxHUDServer boxImgTools boxImgTools.zip boxNetworkService \
colorLightDaemon fakeAADevice fakeCarLife fakeiOSDevice hwSecret mdnsd riddle_top \
riddleBoxCfg usbmuxd"
SCRIPT_A="check_mfg_mode.sh check_log_size.sh cpu_UsageRate.sh check_update.sh check_udisk_log.sh \
phone_link_deamon.sh start_iphone.sh start_aoa.sh start_accessory.sh \
start_accessory_mass_storage.sh start_mass_storage.sh start_mtp.sh start_hnp.sh \
start_iap2_ncm.sh start_android_ncm.sh up_apple_net.sh getFuncModule.sh \
start_mic_record.sh test_mic.sh set_wm8960_mix.sh set_wm8978_mix.sh init_audio_codec.sh \
mount-SK.sh mount-UPAN.sh mount-TF.sh mount-LJ.sh mount-gadgetfs.sh smart_copy.sh \
reset_usb_bus.sh open_log.sh goto_burn_mode.sh update_box.sh update_box_ota.sh \
update_kernel.sh update_dtb.sh module_upgrade.sh quickly_update.sh custom_init.sh.disabled"
ETC_A="airplay.conf airplay_brand.conf airplay_car.conf airplay_none.conf airplay_siri.conf \
android_work_mode BoxHelper.apk box_brand.png car.png car_manu_info icon_120x120.png \
icon_180x180.png icon_256x256.png oem_icon.png siri.png rgbLight.conf mime.types \
dhcp6s.conf udhcpd_hicar_ap.conf dbus-1/system.d/riddle.conf"
LIB_VENDOR="libARMtool.so libauthagent.so libboxtrans.so libcrypto.so libcrypto.so.1.1 \
libdmsdp.so libdmsdpaudiohandler.so libdmsdpcamerahandler.so libdmsdpdvaudio.so \
libdmsdpdvcamera.so libdmsdpdvdevice.so libdmsdpdvgps.so libdmsdpdvinterface.so \
libdmsdpgpshandler.so libdmsdphisight.so libdmsdpplatform.so libdmsdpsec.so libdns_sd.so \
libfdk-aac.so libfdk-aac.so.1 libfdk-aac.so.1.0.0 libhicar.so libHisightSink.so \
libHwDeviceAuthSDK.so libHwKeystoreSDK.so libmanagement.so libnearby.so libsecurec.so \
libssl.so libssl.so.1.1 libtinyalsa.so libusb-1.0.so libusb-1.0.so.0 libusb-1.0.so.0.1.0"
LIB_DEV="Mcrt1.o Scrt1.o crt1.o crti.o crtn.o gcrt1.o libasan_preinit.o libasan.la \
libatomic.la libgomp.la libitm.la libsanitizer.spec libssp.la libssp_nonshared.la \
libstdc++.la libsupc++.la libubsan.la libgomp.spec libitm.spec \
libanl-2.20.so libanl.so libanl.so.1 libasan.so libasan.so.1 \
libBrokenLocale-2.20.so libBrokenLocale.so libBrokenLocale.so.1 libc.so \
libcidn-2.20.so libcidn.so libcidn.so.1 libgcc_s.so libgomp.so libgomp.so.1 \
libgomp.so.1.0.0 libitm.so libitm.so.1 libitm.so.1.0.0 libmemusage.so \
libnss_db-2.20.so libnss_db.so libnss_db.so.2 libnss_hesiod-2.20.so libnss_hesiod.so \
libnss_hesiod.so.2 libnss_nisplus-2.20.so libnss_nisplus.so libnss_nisplus.so.2 \
libpcprofile.so libpthread.so libSegFault.so libstdc++.so libthread_db-1.0.so \
libthread_db.so libthread_db.so.1 libubsan.so libubsan.so.0"
KILL_PROCS="ARMadb-driver ARMiPhoneIAP2 ARMAndroidAuto ARMHiCar ARMandroid_Mirror AppleCarPlay \
AutomaticTest boa boxNetworkService boxHUDServer colorLightDaemon mdnsd riddle_top usbmuxd \
fakeiOSDevice fakeCarLife fakeAADevice"

say() { echo "[finish] $*"; }
free_kb() { df -k "$ROOT/" 2>/dev/null | awk 'NR>1 {f=$4} END {print f+0}'; }
running() { if command -v pidof >/dev/null 2>&1; then pidof "$1"; else pgrep -x "$1"; fi >/dev/null 2>&1; }

# THE WLAN/BT GUARD, as in once.sh and tools/ncm_base_install.sh: no deletion may take a radio file.
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
gone() {  # $1 path on the box: remove it and record it, unless it is a radio file
  [ -e "$ROOT$1" ] || [ -L "$ROOT$1" ] || return 0
  if is_radio "$1"; then say "keep (radio guard) $1"; return 0; fi
  rm -rf "$ROOT$1" && echo "$1" >> "$GONE"
}

# The same lock as ocbm_revert.sh: a go-back and a finish never interleave. Released only if held,
# so an exit never frees a lock that a revert took meanwhile.
locked=0
lock() { mkdir "$RUN/ocbm_mode.lock" 2>/dev/null && locked=1; }
unlock() { [ "$locked" = 1 ] && rmdir "$RUN/ocbm_mode.lock" 2>/dev/null; locked=0; }
trap unlock EXIT
lock || { say "a revert or another finish is running"; exit 0; }
mode=$(cat "$MODE" 2>/dev/null)
case "$mode" in
  confirmed) ;;
  final) [ -e "$PENDING" ] || { say "already final"; exit 0; } ;;
  *) say "nothing to finish (mode ${mode:-none})"; exit 0 ;;
esac

if [ "$mode" = confirmed ]; then
  if [ "${1:-}" = --after-session ]; then
    unlock   # do not hold off a go-back while waiting
    i=0
    while [ "$i" -lt "$GRACE" ]; do
      [ "$(cat "$RUN/host_present" 2>/dev/null)" = 1 ] && { say "an app is back: not finishing yet"; exit 0; }
      sleep "$POLL"; i=$((i + POLL))
    done
    lock || { say "a revert started meanwhile"; exit 0; }
    [ "$(cat "$MODE" 2>/dev/null)" = confirmed ] || { say "the mode changed meanwhile"; exit 0; }
  fi
  [ -x "$ROOT/usr/sbin/ocbmd" ] || { say "refused: no ocbmd, so OCBM could not start without stock"; exit 1; }
  still=""
  for p in $KILL_PROCS; do
    case " ${FINISH_IGNORE:-} " in *" $p "*) continue ;; esac
    running "$p" && still="$still $p"
  done
  [ -z "$still" ] || { say "refused: Carlinkit programs are running:$still"; exit 1; }
  { : > "$PENDING" && echo final > "$MODE.new" && mv -f "$MODE.new" "$MODE"; } \
    || { rm -f "$PENDING" "$MODE.new"; say "could not write the mode; nothing removed"; exit 1; }
  sync
  say "mode final: removing the Carlinkit stack ($(free_kb) KB free)"
else
  say "resuming an interrupted finish"
fi

[ "${STOP_AT:-}" = deletes ] && { say "simulated power loss before the deletes"; exit 99; }
for b in $SBIN_A; do gone "/usr/sbin/$b"; done
for s in $SCRIPT_A; do gone "/script/$s"; done
for e in $ETC_A; do gone "/etc/$e"; done
gone /etc/boa
gone /script/start_main_service.stock.sh
for h in udisk_insert.sh udisk_hotplug.sh; do   # keep mounting sticks; the updater is gone
  f="$ROOT/etc/mdev/$h"
  grep -q '^[[:space:]]*/script/update_box\.sh' "$f" 2>/dev/null || continue
  sed 's|^\([[:space:]]*\)\(/script/update_box\.sh\)|# OTA-removed #\1\2|' "$f" > "$f.new" \
    && grep -q 'mount' "$f.new" && chmod 755 "$f.new" && mv -f "$f.new" "$f" \
    && echo "/etc/mdev/$h: the updater call" >> "$GONE"
done
sync

# Libraries: only those no surviving file names (tools/ncm_base_install.sh phase_prunelibs). A scan
# that finds too little would delete every candidate, libcrypto and libssl included, so it must
# find libc.so before anything goes; otherwise the libraries stay and the finish still completes.
cand=""
for l in $LIB_VENDOR; do [ -e "$ROOT/usr/lib/$l" ] && cand="$cand /usr/lib/$l"; done
for l in $LIB_DEV; do [ -e "$ROOT/lib/$l" ] && cand="$cand /lib/$l"; done
K="$RUN/ocbm_finish.keep"; F="$RUN/ocbm_finish.refs"
: > "$K"
for d in /bin /sbin /usr/bin /usr/sbin /usr/lib /lib; do
  [ -d "$ROOT$d" ] || continue
  for f in "$ROOT$d"/*; do
    [ -f "$f" ] && [ ! -L "$f" ] || continue   # /bin and /sbin are mostly links to busybox
    case " $cand " in *" ${f#$ROOT} "*) continue ;; esac
    echo "$f" >> "$K"
  done
done
set -f   # the extractors below are split on purpose, and the sed one must not glob
for m in "strings" "tr -c a-zA-Z0-9_.+- \n" "sed s/[^a-zA-Z0-9_.+-]/\n/g"; do
  # shellcheck disable=SC2046,SC2086
  cat $(cat "$K") 2>/dev/null | $m 2>/dev/null | grep -o 'lib[A-Za-z0-9_.+-]*\.so[0-9.]*' | sort -u > "$F"
  [ -s "$F" ] && break
done
set +f
if [ "$(wc -l < "$F")" -lt 10 ] || ! grep -q '^libc\.so' "$F"; then
  say "library scan found $(wc -l < "$F" | tr -d ' ') names and no libc.so: the libraries stay"
else
  for c in $cand; do
    b=${c##*/}
    stem=$(echo "$b" | sed 's/\.so.*$//; s/-[0-9].*$//; s/\.la$//; s/\.o$//; s/\.spec$//')
    if [ -n "$stem" ] && grep -q "^${stem}[.-]" "$F"; then say "keep $c (still named)"; continue; fi
    gone "$c"
  done
fi
rm -f "$K" "$F"

rm -f "$PENDING"
sync
say "finished: $(wc -l < "$GONE" | tr -d ' ') entries removed, $(free_kb) KB free; OCBM only from now on"
exit 0
