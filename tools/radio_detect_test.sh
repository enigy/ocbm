#!/usr/bin/env bash
# radio_detect_test.sh — ccpa/rootfs/script/radio_detect.sh against synthetic units, on the host.
# radio_detect.sh reads absolute paths (/sys, /lib/firmware, /usr/sbin, /script) and has no root
# prefix, so the test runs a copy with those paths moved into a scratch tree. Cases: an NXP IW416
# unit whose init_bluetooth_wifi.sh is Carlinkit's own 2025.10.15.1127 text (unquoted STA gate) and
# one with lvalen's NCM-image text (quoted), each with and without the STA-mode gate file; and a
# branch with three assignments, which is not closed-form and must stay refused. Since 20 Nov 2026,
# under each POSIX shell found: the order read off the unit's own attach branch (RADIO_BT_TOGETHER,
# against an attach script shaped like Carlinkit's), and the key a kept descriptor is checked
# against (radio_detect_key): unchanged by post-load facts, changed by each input it covers.
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
test -e /sys/bus/sdio/devices/mmc0:0001:1/device && /script/attach_bluetooth.sh &
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
detect() {  # $1 dir: run the path-moved copy (at the path the script hashes itself by), print RADIO_WLAN_INSMOD
  local R="$1"
  mkdir -p "$R/script"
  sed -e "s#/sys/#@R@/sys/#g; s#/lib/firmware#@R@/lib/firmware#g; s#/usr/sbin/#@R@/usr/sbin/#g" \
      -e "s#/script/#@R@/script/#g; s#/proc/uptime#@R@/proc/uptime#g; s#@R@#$R#g" \
      "$REPO/ccpa/rootfs/script/radio_detect.sh" > "$R/script/radio_detect.sh"
  RADIO_CAPS="$R/caps" "${RD_SH:-sh}" "$R/script/radio_detect.sh" >/dev/null 2>&1
  sed -n 's/^RADIO_WLAN_INSMOD=//p' "$R/caps"
}
cap() { sed -n "s/^RADIO_$2=//p" "$1/caps" | tr -d '"'; }   # $1 dir, $2 key: the descriptor's value
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

# ---- the order, read off this unit's own attach branch (20 Nov 2026) ----------------------------------------------
# An attach script shaped like Carlinkit's 2025.10.15.1127 one (its structure, not its text): a Realtek branch whose
# 0xc822 case waits for the Wi-Fi interface, an SD8987 branch that waits, and an IW416 branch that pushes its firmware
# and attaches without waiting. The whole file waits (BT_AFTER_WLAN=1 for all); RADIO_BT_TOGETHER only for 0x9159.
# Its paths carry the scratch root, as the path-moved radio_detect.sh looks for them.
attach_dispatch() {  # $1 dir, $2 extra lines at the top of the IW416 branch, $3 lines before the dispatch, $4 text after it
  local R="$1"
  cat > "$R/script/attach_bluetooth.sh" <<EOF
attach_bluetooth() {
	sdioCardID=\`cat /sys/bus/sdio/devices/mmc0:0001:1/device\`
${3:-}
	if [ "\$sdioCardID" == "0xb822" ] || [ "\$sdioCardID" == "0xc822" ]; then
		if [ "\$sdioCardID" == "0xc822" ]; then
			while [ ! -e $R/sys/class/net/wlan0/address ]; do
				sleep 0.1
			done
		fi
		rtk_hciattach -s 115200 ttymxc2 rtk_h5
		hciconfig hci0 scomtu 240:32
	elif [ "\$sdioCardID" == "0x9149" ] || [ "\$sdioCardID" == "0x9141" ]; then
		while [ ! -e $R/sys/class/net/wlan0/address ]; do
			sleep 0.1
		done
		if [ -e $R/sys/class/net/wlan0/address ]; then
			hciattach /dev/ttymxc2 any 3000000 flow
			hciconfig hci0 scomtu 240:32
		fi
	elif [ "\$sdioCardID" == "0x9159" ]; then
$2
		fw_loader_linux /dev/ttymxc2 115200 1 /lib/firmware/nxp/uartiw416_bt_v0.bin 3000000
		if [ -e $R/usr/sbin/hciattach ]; then
			hciattach /dev/ttymxc2 any 3000000 flow
			hciconfig hci0 scomtu 240:32
			# route sco data to hci
			hcitool -i hci0 cmd 0x3f 0x1d 0x00
		fi
	fi
}
${4:-}
while [ ! -e /tmp/*hci_uart.ko ]; do sleep 0.1; done
attach_bluetooth
EOF
}
. "$HERE/test_shells.sh"
for RD_SH in ${SHELLS:-$(distinct_shells sh dash)}; do
  echo "== radio_detect.sh under $RD_SH: the order, by this unit's own attach branch"
  d="$T/order.$RD_SH"; make_unit "$d" 'if [ $supportStaMode -eq 1 ]; then' ""; attach_dispatch "$d" ""
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_AFTER_WLAN)" = 1 ] && [ "$(cap "$d" BT_TOGETHER)" = 1 ]' \
    "IW416 (0x9159): the file waits, its own branch does not, and it pushes its firmware: together"
  check '[ -n "$(cap "$d" BT_PRELOAD_CMD)" ] && [ "$(cap "$d" BT_ATTACH_CMD)" = "hciattach /dev/ttymxc2 any 3000000 flow" ]' \
    "  (its preload and attach lines, as before)"
  for id in 0x9149 0xc822 0xb822 0x1234; do
    echo "$id" > "$d/sys/bus/sdio/devices/mmc0:0001:1/device"
    detect "$d" >/dev/null
    check '[ "$(cap "$d" BT_AFTER_WLAN)" = 1 ] && [ "$(cap "$d" BT_TOGETHER)" = 0 ]' "$id: today's order (BT_TOGETHER=0)"
  done
  echo 0x9159 > "$d/sys/bus/sdio/devices/mmc0:0001:1/device"
  attach_dispatch "$d" "		while [ ! -e $d/sys/class/net/sta0/address ]; do sleep 0.1; done"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_TOGETHER)" = 0 ]' "an IW416 branch of its own that waits: today's order"
  printf '\twhile [ ! -e %s/sys/class/net/wlan0/address ]; do sleep 1; done\n\t\thciattach /dev/ttymxc2 any 3000000 flow\n' "$d" \
    > "$d/script/attach_bluetooth.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_AFTER_WLAN)" = 1 ] && [ "$(cap "$d" BT_TOGETHER)" = 0 ]' "a one-branch attach script that waits: today's order"
  printf '\tfw_loader_linux /dev/ttymxc2 115200 1 /x.bin 3000000\n\thciattach /dev/ttymxc2 any 3000000 flow\n' > "$d/script/attach_bluetooth.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_AFTER_WLAN)" = 0 ] && [ "$(cap "$d" BT_TOGETHER)" = 0 ]' "one that never waits (the owned rewrite's shape): nothing to change, 0"

  # Conservative since 21 Nov (review C): any sign that this unit's attach waits for the Wi-Fi side, in any form, is
  # today's order; a wait for the Bluetooth driver's unpack (the stock top level's) is none.
  echo "== radio_detect.sh under $RD_SH: the order, conservative"
  together_with() {  # $1 IW416 branch lines, $2 lines before the dispatch, $3 text after it: prints BT_TOGETHER
    attach_dispatch "$d" "$1" "$2" "$3"; detect "$d" >/dev/null; cap "$d" BT_TOGETHER
  }
  check '[ "$(together_with "" "" "")" = 1 ]' "the stock shape again (a top-level wait for the Bluetooth driver's unpack included): together"
  check '[ "$(together_with "" "	while [ ! -e $d/sys/class/net/wlan0/address ]; do sleep 0.1; done" "")" = 0 ]' \
    "(a) a wait for the Wi-Fi interface before the sdioCardID dispatch: today's order"
  check '[ "$(together_with "		until [ -e $d/sys/class/net/wlan0/address ]; do sleep 0.1; done" "" "")" = 0 ]' \
    "(b) an until loop in its own branch: today's order"
  check '[ "$(together_with "		until [ -e /tmp/bt_ready ]; do :; done" "" "")" = 0 ]' \
    "  an until loop alone (no sleep, nothing of the Wi-Fi side named): today's order"
  check '[ "$(together_with "		while ! hcitool dev | grep -q hci; do :; done" "" "")" = 0 ]' \
    "  a while loop alone: today's order"
  check '[ "$(together_with "		wifi_wait" "" "wifi_wait() {
	while [ ! -e $d/sys/class/net/wlan0/address ]; do sleep 0.1; done
}")" = 0 ]' "(c) a helper function its branch calls: today's order"
  check '[ "$(together_with "		ready_check" "" "ready_check() {
	true
}")" = 0 ]' "  a call to any function of the script's own: today's order"
  check '[ "$(together_with "		while [ ! -e \"$d/sys/class/net/wlan0/address\" ]; do sleep 0.1; done" "" "")" = 0 ]' \
    "(d) a quoted path in its branch's wait: today's order"
  check '[ "$(together_with "		sleep 2" "" "")" = 0 ]' "  a bare sleep in its branch: today's order"
  check '[ "$(together_with "		lsmod | grep -q moal || echo no" "" "")" = 0 ]' "  a look at one of its Wi-Fi modules: today's order"
  check '[ "$(together_with "" "" ". $d/script/helpers.sh")" = 0 ]' "  a sourced file: today's order"
  attach_dispatch "$d" "" "" ""; head -n 20 "$d/script/attach_bluetooth.sh" > "$T/trunc"; cp "$T/trunc" "$d/script/attach_bluetooth.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_TOGETHER)" = 0 ]' "  an attach script cut short (its chain never closes): today's order"
  attach_dispatch "$d" "" "" ""; head -n 28 "$d/script/attach_bluetooth.sh" > "$T/trunc"; cp "$T/trunc" "$d/script/attach_bluetooth.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_TOGETHER)" = 0 ] && [ -n "$(cap "$d" BT_PRELOAD_CMD)" ] && [ -n "$(cap "$d" BT_ATTACH_CMD)" ]' \
    "  one cut right after the IW416 branch (its lines read, the chain never closed): today's order"
  attach_dispatch "$d" "" "" ""
  cp "$d/script/init_bluetooth_wifi.sh" "$T/disp.order"
  grep -v 'attach_bluetooth.sh &' "$T/disp.order" \
    | awk '{ print } /test -e \/tmp\/moal.ko/ { print "\t/script/attach_bluetooth.sh &" }' > "$d/script/init_bluetooth_wifi.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_TOGETHER)" = 0 ] && grep -q "^	/script/attach_bluetooth.sh &" "$d/script/init_bluetooth_wifi.sh"' \
    "(e) a dispatcher that starts the attach only after the Wi-Fi driver: today's order"
  sed 's#attach_bluetooth.sh &#attach_bluetooth.sh#' "$T/disp.order" > "$d/script/init_bluetooth_wifi.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_TOGETHER)" = 0 ]' "  one that starts it first but waits for it (not in the background): today's order"
  grep -v 'attach_bluetooth' "$T/disp.order" > "$d/script/init_bluetooth_wifi.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_TOGETHER)" = 0 ]' "  one that never starts it (the order cannot be read): today's order"
  # The dispatcher's own text before it starts the attach (its line included): a wait, a sleep, a sourced file or a
  # sign of the Wi-Fi side there, or the start inside a function or a group, is today's order.
  disp_before() {  # $1 lines put before the dispatcher's attach line: prints BT_TOGETHER
    awk -v pre="$1" '/attach_bluetooth.sh &/ { print pre } { print }' "$T/disp.order" > "$d/script/init_bluetooth_wifi.sh"
    detect "$d" >/dev/null; cap "$d" BT_TOGETHER
  }
  check '[ "$(disp_before "while [ ! -e /sys/class/net/sta0 ]; do sleep 0.1; done")" = 0 ]' \
    "(a) the dispatcher waits for the Wi-Fi interface before it starts the attach (its driver loaded elsewhere): today's order"
  check '[ "$(disp_before "sleep 1")" = 0 ]' "  a sleep before the start: today's order"
  check '[ "$(disp_before ". /script/wifi_first.sh")" = 0 ]' "  a file sourced before the start: today's order"
  check '[ "$(disp_before "ifconfig wlan0 up")" = 0 ]' "  the Wi-Fi side named before the start: today's order"
  awk '/attach_bluetooth.sh &/ { print "start_bt() {"; print "\t/script/attach_bluetooth.sh &"; print "}"; print "start_bt"; next } { print }' \
    "$T/disp.order" > "$d/script/init_bluetooth_wifi.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_TOGETHER)" = 0 ] && grep -q "^start_bt() {" "$d/script/init_bluetooth_wifi.sh"' \
    "  the start inside a function of the dispatcher (when it runs is not read): today's order"
  check '[ "$(disp_before "echo starting the radios")" = 1 ]' "  a line that is none of these: together still"
  cp "$T/disp.order" "$d/script/init_bluetooth_wifi.sh"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" BT_TOGETHER)" = 1 ]' "  the stock dispatcher back: together"

  # ---- the key a kept copy is checked against (radio_detect_key) ---------------------------------------------------
  echo "== radio_detect.sh under $RD_SH: the key"
  d="$T/key.$RD_SH"; make_unit "$d" 'if [ $supportStaMode -eq 1 ]; then' ""; attach_dispatch "$d" ""
  detect "$d" >/dev/null; k0=$(cap "$d" DETECT_KEY)
  check '[ -n "$k0" ] && [ "$k0" != unkeepable ] && [ "$(cap "$d" CAPS_FROM)" = detected ]' "a detection carries its key (${k0%% sums=*} ...)"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" DETECT_KEY)" = "$k0" ]' "the same unit, detected again: the same key"
  rm -f "$d/caps"
  fkey=$(RADIO_DETECT_FUNCS=1 RADIO_CAPS="$d/caps" "$RD_SH" -c '. "$1" && radio_detect_key' sh "$d/script/radio_detect.sh" 2>/dev/null)
  check '[ "$fkey" = "$k0" ] && [ ! -e "$d/caps" ]' "the function alone, sourced as radio_hal.sh sources it (RADIO_DETECT_FUNCS=1), gives it too, and detects nothing"
  mkdir -p "$d/sys/class/net/sta0/wireless" "$d/sys/class/bluetooth/hci0"; echo 02:00:00:00:00:01 > "$d/sys/class/net/sta0/address"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" DETECT_KEY)" = "$k0" ] && [ "$(cap "$d" WLAN_IF)" = sta0 ]' "an interface and hci0 up since (post-load facts): the same key"
  kchanged() {  # $1 what changed: the key differs from k0, then the change is undone by the caller
    detect "$d" >/dev/null; [ "$(cap "$d" DETECT_KEY)" != "$k0" ]
  }
  cp "$d/script/init_bluetooth_wifi.sh" "$T/disp.keep"; echo "# a reflash" >> "$d/script/init_bluetooth_wifi.sh"
  check 'kchanged' "the dispatcher's text changed: another key"; cp "$T/disp.keep" "$d/script/init_bluetooth_wifi.sh"
  cp "$d/script/attach_bluetooth.sh" "$T/att.keep"; echo "# a reflash" >> "$d/script/attach_bluetooth.sh"
  check 'kchanged' "the attach script's text: another key"; cp "$T/att.keep" "$d/script/attach_bluetooth.sh"
  cp "$d/lib/firmware/nxp/iw416_ko.tar.gz" "$T/ko.keep"; (cd "$d/ko" && : > extra.ko && tar -czf "$d/lib/firmware/nxp/iw416_ko.tar.gz" mlan.ko moal.ko hci_uart.ko extra.ko)
  check 'kchanged' "the driver tarball's contents: another key"; cp "$T/ko.keep" "$d/lib/firmware/nxp/iw416_ko.tar.gz"
  echo 0x9149 > "$d/sys/bus/sdio/devices/mmc0:0001:1/device"
  check 'kchanged' "another SDIO id: another key"; echo 0x9159 > "$d/sys/bus/sdio/devices/mmc0:0001:1/device"
  mkdir "$d/lib/firmware/rtlbt"; check 'kchanged' "a firmware tree more: another key"; rmdir "$d/lib/firmware/rtlbt"
  rm "$d/usr/sbin/fw_loader_linux"; check 'kchanged' "a helper gone: another key"; : > "$d/usr/sbin/fw_loader_linux"; chmod 755 "$d/usr/sbin/fw_loader_linux"
  : > "$d/usr/sbin/wpa_supplicant"; check 'kchanged' "the dispatcher's gate file there (its STA mode): another key"; rm "$d/usr/sbin/wpa_supplicant"
  # No key a copy could be checked against (the 21 Nov reviews, D and L): no md5sum output, the sums of nothing, no card.
  mkdir -p "$T/nomd5"; printf '#!/bin/sh\nexit 1\n' > "$T/nomd5/md5sum"; chmod 755 "$T/nomd5/md5sum"
  PATH="$T/nomd5:$PATH" detect "$d" >/dev/null
  check '[ "$(cap "$d" DETECT_KEY)" = unkeepable ]' "md5sum gives nothing: unkeepable"
  printf '#!/bin/sh\n[ $# -eq 0 ] && cat >/dev/null\necho "d41d8cd98f00b204e9800998ecf8427e  -"\n' > "$T/nomd5/md5sum"
  PATH="$T/nomd5:$PATH" detect "$d" >/dev/null
  check '[ "$(cap "$d" DETECT_KEY)" = unkeepable ]' "the sums of nothing (no file readable): unkeepable"
  mv "$d/sys/bus/sdio/devices/mmc0:0001:1/device" "$T/sdio.keep"; detect "$d" >/dev/null
  check '[ "$(cap "$d" DETECT_KEY)" = unkeepable ] && [ -z "$(cap "$d" SDIO_DEVICE)" ]' "no SDIO card yet: unkeepable"
  mv "$T/sdio.keep" "$d/sys/bus/sdio/devices/mmc0:0001:1/device"
  echo "# a newer radio_detect.sh" >> "$d/script/radio_detect.sh"
  fkey=$(RADIO_DETECT_FUNCS=1 "$RD_SH" -c '. "$1" && radio_detect_key' sh "$d/script/radio_detect.sh" 2>/dev/null)
  check '[ -n "$fkey" ] && [ "$fkey" != "$k0" ]' "radio_detect.sh's own text: another key"
  detect "$d" >/dev/null
  check '[ "$(cap "$d" DETECT_KEY)" = "$k0" ]' "everything back as it was: the first key again"


done
RD_SH=sh

# radio_hal.sh's box_name, the one name for Wi-Fi and Bluetooth: btd's CarLink-<wlan0's last 4 hex> (Scott,
# 12 Nov: the dev adapter's Wi-Fi said ccpa-fe01, from the AP interface sta0, beside Bluetooth's CarLink-fd6c).
# The functions are taken out of radio_hal.sh, with /etc and /sys moved into a scratch root.
names() {  # $1 root: prints box_name's answer there
  local R="$1"
  sed -n '/^IDENT_FILE=/,/^}/p' "$REPO/ccpa/rootfs/script/radio_hal.sh" | sed "s#/etc/#$R/etc/#g; s#/sys/#$R/sys/#g" > "$R/fns.sh"
  sh -c ". '$R/fns.sh'; box_name"
}
R="$T/name"; mkdir -p "$R/etc/ocbm" "$R/sys/class/net/wlan0" "$R/sys/class/net/sta0"
echo "02:00:00:00:fe:01" > "$R/sys/class/net/sta0/address"; echo "02:00:00:00:FD:6C" > "$R/sys/class/net/wlan0/address"
echo "ccpa-fe01" > "$R/etc/carplay_ident"; echo "SN 00A1B2C3" > "$R/etc/serial_number"
check '[ "$(names "$R")" = CarLink-fd6c ]' "an older ccpa- name is derived again from wlan0, as btd's"
check '[ "$(head -1 "$R/etc/carplay_ident")" = CarLink-fd6c ]' "and kept"
rm "$R/sys/class/net/wlan0/address"
check '[ "$(names "$R")" = CarLink-fd6c ]' "kept: the radios' state doesn't change it"
rm "$R/etc/carplay_ident"; printf 'wlan0=02:00:00:00:fd:6c\nhci0=02:00:00:00:fd:6d\n' > "$R/etc/ocbm/addresses.txt"
check '[ "$(names "$R")" = CarLink-fd6c ]' "radios down: wlan0's MAC from the installer's record"
rm "$R/etc/carplay_ident" "$R/etc/ocbm/addresses.txt"
check '[ "$(names "$R")" = CarLink-b2c3 ]' "no wlan0 MAC: the serial's, as btd's"
check '[ ! -e "$R/etc/carplay_ident" ]' "and not kept, so wlan0's decides once known"
rm "$R/etc/serial_number"
check '[ "$(names "$R")" = CarLink ]' "nothing readable: the bare brand, as btd's"
[ "$fails" -eq 0 ] && echo "radio_detect_test: all passed" || { echo "radio_detect_test: $fails failed"; exit 1; }
