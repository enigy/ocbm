#!/usr/bin/env bash
# radio_detect_test.sh — ccpa/rootfs/script/radio_detect.sh against synthetic units, on the host.
# radio_detect.sh reads absolute paths (/sys, /lib/firmware, /usr/sbin, /script) and has no root
# prefix, so the test runs a copy with those paths moved into a scratch tree. Cases: an NXP IW416
# unit whose init_bluetooth_wifi.sh is Carlinkit's own 2025.10.15.1127 text (unquoted STA gate) and
# one with lvalen's NCM-image text (quoted), each with and without the STA-mode gate file; and a
# branch with three assignments, which is not closed-form and must stay refused.
#
#   tools/radio_detect_test.sh
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/radio_detect_test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

make_unit() {  # $1 dir, $2 gate line (if ...; then), $3 assignment lines after the default
  local R="$1"
  mkdir -p "$R/sys/bus/sdio/devices/mmc0:0001:1" "$R/lib/firmware/nxp" "$R/usr/sbin" "$R/script" "$R/sys/class/net" \
           "$R/sys/class/bluetooth" "$R/proc" "$R/ko"
  echo 0x9159 > "$R/sys/bus/sdio/devices/mmc0:0001:1/device"
  : > "$R/ko/mlan.ko"; : > "$R/ko/moal.ko"; : > "$R/ko/hci_uart.ko"
  (cd "$R/ko" && tar -czf "$R/lib/firmware/nxp/iw416_ko.tar.gz" mlan.ko moal.ko hci_uart.ko)
  : > "$R/lib/firmware/nxp/uartiw416_bt_v0.bin"
  for b in hciattach hciconfig fw_loader_linux; do : > "$R/usr/sbin/$b"; chmod 755 "$R/usr/sbin/$b"; done
  echo "100.0 1.0" > "$R/proc/uptime"
  cat > "$R/script/init_bluetooth_wifi.sh" <<EOF
test -e $R/usr/sbin/wpa_supplicant && supportStaMode=1 || supportStaMode=0
sdioCardID=\`cat /sys/bus/sdio/devices/mmc0:0001:1/device\`
if [ "\$sdioCardID" == "0xb822" ]; then
	test -e /tmp/88x2bs.ko && insmod /tmp/88x2bs.ko if2name=sta0
elif [ "\$sdioCardID" == "0x9159" ]; then
	nxpWiFiConfig=nxp/wifi_mod_para.conf
	$2
		nxpWiFiConfig=nxp/wifi_mod_para_p2psta.conf
	fi
$3
	tar -xvf /lib/firmware/nxp/iw416_ko.tar.gz -C /tmp
	test -e /tmp/mlan.ko && insmod /tmp/mlan.ko
	test -e /tmp/moal.ko && insmod /tmp/moal.ko mod_para=\$nxpWiFiConfig
fi
EOF
  printf '\twhile [ ! -e /sys/class/net/wlan0/address ]; do sleep 1; done\n\t\thciattach /dev/ttymxc2 any 3000000 flow\n' \
    > "$R/script/attach_bluetooth.sh"
}
detect() {  # $1 dir: run the path-moved copy, print RADIO_WLAN_INSMOD
  local R="$1"
  sed -e "s#/sys/#@R@/sys/#g; s#/lib/firmware#@R@/lib/firmware#g; s#/usr/sbin/#@R@/usr/sbin/#g" \
      -e "s#/script/#@R@/script/#g; s#/proc/uptime#@R@/proc/uptime#g; s#@R@#$R#g" \
      "$REPO/ccpa/rootfs/script/radio_detect.sh" > "$R/radio_detect.sh"
  RADIO_CAPS="$R/caps" sh "$R/radio_detect.sh" >/dev/null 2>&1
  sed -n 's/^RADIO_WLAN_INSMOD=//p' "$R/caps"
}
want_dflt='"insmod /tmp/mlan.ko;insmod /tmp/moal.ko mod_para=nxp/wifi_mod_para.conf;"'
want_alt='"insmod /tmp/mlan.ko;insmod /tmp/moal.ko mod_para=nxp/wifi_mod_para_p2psta.conf;"'

echo "== radio_detect.sh: NXP IW416 module line"
for form in stock lvalen; do
  case $form in
    stock)  gate='if [ $supportStaMode -eq 1 ]; then' ;;
    lvalen) gate='if [ "$supportStaMode" -eq 1 ]; then' ;;
  esac
  d="$T/$form"; make_unit "$d" "$gate" ""
  got="$(detect "$d")"
  check '[ "$got" = "$want_dflt" ]' "$form text: moal.ko resolves to the default ($got)"
  : > "$d/usr/sbin/wpa_supplicant"; got="$(detect "$d")"
  check '[ "$got" = "$want_alt" ]' "$form text with the STA gate file: the alternative ($got)"
  check 'grep -qx "RADIO_WIRELESS=yes" "$d/caps" && grep -qx "RADIO_BACKEND=mapped" "$d/caps"' "$form text: wireless, mapped backend"
done
d="$T/three"; make_unit "$d" 'if [ $supportStaMode -eq 1 ]; then' '	nxpWiFiConfig=nxp/other.conf'
got="$(detect "$d")"
check '[ "$got" = "\"insmod /tmp/mlan.ko;\"" ]' "three assignments are not closed-form: moal.ko stays refused ($got)"


# radio_hal.sh's box_name, the one name for Wi-Fi and Bluetooth: btd's CarLink-<wlan0's last 4 hex> (Scott,
# 12 Nov: the dev adapter's Wi-Fi said ccpa-fe01, from the AP interface sta0, beside Bluetooth's CarLink-fd6c).
# The functions are taken out of radio_hal.sh, with /etc and /sys moved into a scratch root.
names() {  # $1 root: prints box_name's answer there
  local R="$1"
  sed -n '/^IDENT_FILE=/,/^}/p' "$REPO/ccpa/rootfs/script/radio_hal.sh" | sed "s#/etc/#$R/etc/#g; s#/sys/#$R/sys/#g" > "$R/fns.sh"
  sh -c ". '$R/fns.sh'; box_name"
}
R="$T/name"; mkdir -p "$R/etc/ocbm" "$R/sys/class/net/wlan0" "$R/sys/class/net/sta0"
echo "a8:5b:36:2c:fe:01" > "$R/sys/class/net/sta0/address"; echo "A8:5B:36:2C:FD:6C" > "$R/sys/class/net/wlan0/address"
echo "ccpa-fe01" > "$R/etc/carplay_ident"; echo "SN 00A1B2C3" > "$R/etc/serial_number"
check '[ "$(names "$R")" = CarLink-fd6c ]' "an older ccpa- name is derived again from wlan0, as btd's"
check '[ "$(head -1 "$R/etc/carplay_ident")" = CarLink-fd6c ]' "and kept"
rm "$R/sys/class/net/wlan0/address"
check '[ "$(names "$R")" = CarLink-fd6c ]' "kept: the radios' state doesn't change it"
rm "$R/etc/carplay_ident"; printf 'wlan0=a8:5b:36:2c:fd:6c\nhci0=a8:5b:36:2c:fd:6d\n' > "$R/etc/ocbm/addresses.txt"
check '[ "$(names "$R")" = CarLink-fd6c ]' "radios down: wlan0's MAC from the installer's record"
rm "$R/etc/carplay_ident" "$R/etc/ocbm/addresses.txt"
check '[ "$(names "$R")" = CarLink-b2c3 ]' "no wlan0 MAC: the serial's, as btd's"
check '[ ! -e "$R/etc/carplay_ident" ]' "and not kept, so wlan0's decides once known"
rm "$R/etc/serial_number"
check '[ "$(names "$R")" = CarLink ]' "nothing readable: the bare brand, as btd's"
[ "$fails" -eq 0 ] && echo "radio_detect_test: all passed" || { echo "radio_detect_test: $fails failed"; exit 1; }
