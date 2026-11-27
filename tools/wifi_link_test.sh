#!/usr/bin/env bash
# wifi_link_test.sh — radio_hal.sh's part of the Wi-Fi link (ADDENDUM 24 of the Wi-Fi link plan: mode ap, the head unit
# on the adapter's own AP) and of M3's set-up network, on the host: which interface the AP takes, with no link and in
# mode ap (the uAP beside the STA BSS, one interface, the 1 s wait for the pair), the link file read with no fork, what a
# teardown downs (the AP's own interface, also once the link is removed), a session's teardown leaving mode ap's AP up,
# wifi_ap_restart, the early 2 of wifi_ap_on, the band handed to the AP layer and the configuration written ahead only
# for the AP's interface, and the set-up network's configuration (client isolation, its own pool, channel 6). A copy of
# radio_hal.sh with /sys, the link file, hostapd.conf and /tmp moved into a scratch root, sourced for its functions (as
# tools/radio_ap_up_test.sh does), under every POSIX shell found (sh, dash), and BusyBox's by tools/busybox_suites.sh.
# (The hotspot mode's station keeper and its suite, tools/wifi_sta_test.sh, were removed on 27 Nov 2026.)
#
#   tools/wifi_link_test.sh
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/wifi_link_test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

. "$HERE/test_shells.sh"
# radio_hal.sh with its absolute paths moved into the scratch root $1, sourced for its functions.
hal_copy() {  # $1 root
  sed -e "s#/sys/#$1/sys/#g; s#/etc/ocbm/wifi_link#$1/etc/ocbm/wifi_link#g; s#/etc/hostapd.conf#$1/etc/hostapd.conf#g" \
      -e "s#/etc/udhcpd.conf#$1/etc/udhcpd.conf#g; s#=/tmp/#=$1/tmp/#g; s# /tmp/udhcpd_setup# $1/tmp/udhcpd_setup#g" \
      -e "s#/script/radio_ap_up.sh#$1/script/radio_ap_up.sh#g" \
    "$REPO/ccpa/rootfs/script/radio_hal.sh" > "$1/radio_hal.sh"
}
hal() {  # $1 root, $2 shell, $3 commands (the functions sourced)
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="${OCBM_TEST_PATH:-}" "$2" -c ". \"\$1\"; $3" sh "$1/radio_hal.sh" 2>/dev/null
}
KEY=$(printf '11%.0s' $(seq 32))
aplink() {  # $1: its band line's value; none when not given. A mode-ap link file in the scratch root $R.
  printf 'key=%s\nmode=ap\n' "$KEY" > "$R/etc/ocbm/wifi_link"
  [ -z "${1:-}" ] || echo "band=$1" >> "$R/etc/ocbm/wifi_link"
}
for sh in ${SHELLS:-$(distinct_shells sh dash)}; do
  echo "== radio_hal.sh, the Wi-Fi link's interface and the link file, under $sh"
  R="$T/hal"; rm -rf "$R"; mkdir -p "$R/etc/ocbm" "$R/tmp" "$R/sys/class/net/sta0/wireless" "$R/sys/class/net/wlan0/wireless" "$R/sys/class/net/ncm0"
  hal_copy "$R"
  gr='grep() { echo grep >> "'"$R"'/calls"; command grep "$@"; }; '
  check '[ "$(hal "$R" "$sh" "ap_iface")" = sta0 ] && ! hal "$R" "$sh" "link_ap"' \
    "no link set up: the AP takes the first interface (sta0), as in 2.7.1"
  : > "$R/calls"; hal "$R" "$sh" "${gr}link_ap; ap_iface >/dev/null; link_ap"
  check '[ ! -s "$R/calls" ]' "  and forks no grep doing it (the shell's own test of the link file first)"
  printf 'key=%s\n' "$KEY" > "$R/etc/ocbm/wifi_link"
  check '[ "$(hal "$R" "$sh" "ap_iface")" = sta0 ] && ! hal "$R" "$sh" "link_ap"' \
    "a link file of the retired hotspot mode, the key alone (no mode=ap): no link, the first interface"
  printf 'ssid_hex=6e6574\npsk=\n' >> "$R/etc/ocbm/wifi_link"
  check '[ "$(hal "$R" "$sh" "ap_iface")" = sta0 ] && ! hal "$R" "$sh" "link_ap"' \
    "  and a hotspot's (ssid_hex=, psk=): no link, the first interface"
  band() { hal "$R" "$sh" 'link_ap && echo "[$_lband]" || echo none'; }
  aplink; check '[ "$(band)" = "[2.4]" ] && [ "$(hal "$R" "$sh" "ap_iface")" = wlan0 ] && [ "$(hal "$R" "$sh" "sta_bss_iface")" = sta0 ]' \
    "mode ap, no band line: 2.4 GHz, and the AP on the uAP interface wlan0 (the STA BSS sta0 idle)"
  aplink 5; check '[ "$(band)" = "[5]" ]' "  band=5: 5 GHz"
  : > "$R/calls"; hal "$R" "$sh" "${gr}link_ap"
  check '[ ! -s "$R/calls" ]' "  read with no fork"
  printf 'key=11\nmode=ap\n' > "$R/etc/ocbm/wifi_link"; check '[ "$(band)" = none ]' "  a file with no key of 64 hex: no mode ap"
  aplink 2.4; rm -rf "$R/sys/class/net/wlan0"
  check '[ "$(hal "$R" "$sh" "ap_iface")" = sta0 ] && ! hal "$R" "$sh" "sta_bss_iface"' \
    "mode ap with one wireless interface: the AP takes it"
  st='log() { echo "log $*" >> "'"$R"'/calls"; }; pair_settle; echo "rc=$?"'
  rm -f "$R/tmp/.radio_one_iface"; : > "$R/calls"; t0=$(date +%s)
  check '[ "$(hal "$R" "$sh" "$st")" = rc=1 ] && [ -e "$R/tmp/.radio_one_iface" ] && grep -q "one wireless interface on this driver: mode ap.s AP takes it" "$R/calls"' \
    "  the wait for its pair (pair_settle) ends once the driver's probe has settled, and marks it for the boot"
  check '[ $(( $(date +%s) - t0 )) -le 3 ]' "  in about a second"
  : > "$R/calls"
  check '[ "$(hal "$R" "$sh" "$st")" = rc=1 ] && [ ! -s "$R/calls" ]' "  and at once after that, saying nothing"
  rm -f "$R/tmp/.radio_one_iface"; mkdir -p "$R/sys/class/net/wlan0/wireless"
  check '[ "$(hal "$R" "$sh" "$st")" = rc=0 ] && [ ! -e "$R/tmp/.radio_one_iface" ]' "  with the pair there: at once, nothing marked"

  echo "== radio_hal.sh, what a teardown downs, under $sh"
  off='killall() { :; }; ap_dhcp_stop() { :; }; ap_running() { return 1; }; caps() { return 0; }
       ifconfig() { echo "ifconfig $*" >> "'"$R"'/calls"; }; do_wifi_ap_off >/dev/null'
  rm -f "$R/etc/ocbm/wifi_link" "$R/tmp/.radio_ap_link"; printf 'interface=sta0\nchannel=36\n' > "$R/etc/hostapd.conf"
  : > "$R/calls"; hal "$R" "$sh" "$off"
  check 'grep -qx "ifconfig sta0 down" "$R/calls" && ! grep -q wlan0 "$R/calls"' "no link: the teardown downs the first interface, as in 2.7.1"
  gs='sed() { echo sed >> "'"$R"'/calls"; command sed "$@"; }; '
  : > "$R/calls"; hal "$R" "$sh" "${gr}${gs}$off"
  check '! grep -q -e grep -e sed "$R/calls"' "  forking no grep or sed for the link (the shell's own tests)"
  printf 'interface=wlan0\nchannel=6\n' > "$R/etc/hostapd.conf"; echo "2.4 6" > "$R/tmp/.radio_ap_link"
  : > "$R/calls"; hal "$R" "$sh" "$off"
  check 'grep -qx "ifconfig wlan0 down" "$R/calls" && ! grep -q sta0 "$R/calls" && [ ! -e "$R/tmp/.radio_ap_link" ]' \
    "the link removed with mode ap's AP up (its mark): down on the interface it ran on (wlan0), not the first (sta0)"
  aplink 2.4; printf 'interface=sta0\nchannel=36\n' > "$R/etc/hostapd.conf"
  : > "$R/calls"; hal "$R" "$sh" "_ap_force=1; $off"
  check 'grep -qx "ifconfig sta0 down" "$R/calls" && ! grep -q "ifconfig wlan0" "$R/calls"' \
    "mode ap, forced: an AP raised on sta0 before the link was set up goes down there, the uAP untouched"
  rm -f "$R/etc/ocbm/wifi_link"

  # M3's set-up network: its configuration in /tmp from the CarPlay AP's, client isolation on, the flash file untouched.
  echo "== radio_hal.sh, M3's set-up network, under $sh"
  printf 'interface=sta0\nssid=CarLink-0a1b\nwpa_passphrase=private\nchannel=36\nhw_mode=a\nht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]\n' > "$R/etc/hostapd.conf"
  printf 'start\t10.0.0.100\nend\t10.0.0.200\ninterface\tsta0\nlease_file\t/var/lib/udhcpd.leases\n' > "$R/etc/udhcpd.conf"   # a custom /etc/wifi_ip's pool
  before=$(cat "$R/etc/hostapd.conf")
  on='caps() { RADIO_BACKEND=mapped; return 0; }; wlan_driver_up() { return 0; }; box_name() { echo CarLink-0a1b; }
      ifconfig() { echo "ifconfig $*" >> "'"$R"'/calls"; }; setsid() { echo "setsid $*" >> "'"$R"'/calls"; }
      ap_running() { [ -e "'"$R"'/up" ]; }; setup_dhcp_pids() { echo 1; }; sleep() { : > "'"$R"'/up"; }; log() { :; }
      do_setup_ap_on; echo "rc=$?"'
  : > "$R/calls"; rm -f "$R/up"
  check '[ "$(hal "$R" "$sh" "$on")" = rc=0 ]' "setup_ap_on: the set-up network comes up"
  check 'grep -qx "ssid=CarLink-Setup-0a1b" "$R/tmp/hostapd_setup.conf" && grep -qx "wpa_passphrase=carlinksetup" "$R/tmp/hostapd_setup.conf" && grep -qx "ap_isolate=1" "$R/tmp/hostapd_setup.conf"' \
    "  CarLink-Setup-xxxx, the public passphrase, client isolation"
  check '! grep -q "private" "$R/tmp/hostapd_setup.conf" && [ "$(cat "$R/etc/hostapd.conf")" = "$before" ]' \
    "  the CarPlay AP's passphrase not carried over, its flash configuration untouched"
  check 'grep -q "udhcpd -f $R/tmp/udhcpd_setup.conf" "$R/calls" && grep -q "lease_file	/tmp/udhcpd_setup.leases" "$R/tmp/udhcpd_setup.conf"' \
    "  its own DHCP server and lease file in /tmp"
  check 'grep -qx "start	192.168.43.100" "$R/tmp/udhcpd_setup.conf" && grep -qx "end	192.168.43.200" "$R/tmp/udhcpd_setup.conf" && ! grep -q "10\.0\.0" "$R/tmp/udhcpd_setup.conf"' \
    "  its pool on the set-up address's subnet, whatever /etc/udhcpd.conf holds (a custom /etc/wifi_ip)"
  check 'grep -q "^ok sta0" "$R/tmp/.radio_setup_ap"' "  the marker ocbmd waits for"
  check '[ "$(grep -c "^channel=" "$R/tmp/hostapd_setup.conf")" = 1 ] && grep -qx "channel=6" "$R/tmp/hostapd_setup.conf" && grep -qx "hw_mode=g" "$R/tmp/hostapd_setup.conf" && grep -qxF "ht_capab=[SHORT-GI-20]" "$R/tmp/hostapd_setup.conf"' \
    "  on 2.4 GHz channel 6, 20 MHz, whatever the CarPlay AP's band (ADDENDUM 24: head units that see no 5 GHz)"

  # ADDENDUM 24, mode ap (MGMT_WIFI_LINK v2): a session's teardown leaves the AP up, wifi_ap_restart takes it down and
  # up, and wifi_ap_on with the AP up returns at once.
  echo "== radio_hal.sh, mode ap's AP kept up, restarted, and its band, under $sh"
  rm -f "$R/tmp/.radio_setup_ap" "$R/tmp/hostapd_setup.conf"   # the set-up network's case above
  aplink 2.4
  printf 'interface=wlan0\nchannel=36\n' > "$R/etc/hostapd.conf"; : > "$R/tmp/.radio_ap_link"
  apoff='killall() { echo "killall $*" >> "'"$R"'/calls"; }; ap_dhcp_stop() { :; }; ap_running() { return 1; }; caps() { return 0; }
       ifconfig() { echo "ifconfig $*" >> "'"$R"'/calls"; }; log() { echo "log $*" >> "'"$R"'/calls"; }'
  : > "$R/calls"; hal "$R" "$sh" "${apoff}; do_wifi_ap_off"
  check '! grep -q -e killall -e ifconfig "$R/calls" && grep -q "mode ap: the AP stays up" "$R/calls" && [ -e "$R/tmp/.radio_ap_link" ]' \
    "a session's teardown leaves the AP up (the head unit's network), and says so"
  : > "$R/calls"; hal "$R" "$sh" "${apoff}; _ap_force=1; do_wifi_ap_off"
  check 'grep -qx "killall hostapd" "$R/calls" && grep -qx "ifconfig wlan0 down" "$R/calls" && [ ! -e "$R/tmp/.radio_ap_link" ]' \
    "  forced (wifi_ap_restart's): down, on wlan0, its band's mark gone"
  : > "$R/up"; : > "$R/calls"
  hal "$R" "$sh" "${apoff}; ap_running() { [ -e '$R/up' ]; }; killall() { echo killall >> '$R/calls'; rm -f '$R/up'; }
    do_wifi_ap_on() { echo \"on \$_lband\" >> '$R/calls'; }; do_wifi_ap_restart; echo \"rc=\$?\" >> '$R/calls'"
  check '[ "$(grep -e killall -e "^on" "$R/calls" | tr "\n" " ")" = "killall on 2.4 " ] && grep -qx "rc=0" "$R/calls"' \
    "  wifi_ap_restart: the AP down, then up on the band"
  rm -f "$R/etc/ocbm/wifi_link"; : > "$R/calls"
  hal "$R" "$sh" "${apoff}; do_wifi_ap_restart; echo \"rc=\$?\" >> '$R/calls'"
  check 'grep -qx "rc=3" "$R/calls" && ! grep -q killall "$R/calls"' "  not in mode ap: nothing restarted (3)"
  aplink 2.4; : > "$R/up"; : > "$R/calls"
  hal "$R" "$sh" "${apoff}; ap_running() { [ -e '$R/up' ]; }; do_wifi_ap_on; echo \"rc=\$?\" >> '$R/calls'"
  check 'grep -qx "rc=2" "$R/calls" && grep -q "mode ap: the AP is up already" "$R/calls"' \
    "  wifi_ap_on with the AP up: already converged (2) at once, nothing launched"
  # The band to the AP layer (RADIO_AP_BAND), and the fast path only for the configuration written ahead on the AP's
  # interface (a stamp for sta0, from before mode ap, is not it).
  mkdir -p "$R/script" "$R/tmp"; rm -f "$R/up"
  printf '#!/bin/sh\necho "ap_up $RADIO_WLAN_IF band=[${RADIO_AP_BAND:-}] channel=[${RADIO_AP_CHANNEL-none}] $*" >> "%s/calls"; : > "%s/up"\n' "$R" "$R" > "$R/script/radio_ap_up.sh"
  chmod 755 "$R/script/radio_ap_up.sh"
  echo "wlan0 CarLink-0a1b 192.168.43.1 36 0" > "$R/tmp/.radio_ap_prepared"; : > "$R/calls"
  hal "$R" "$sh" "${apoff}; ap_running() { [ -e '$R/up' ]; }; do_wifi_ap_on; echo \"rc=\$?\" >> '$R/calls'"
  check 'grep -qx "ap_up wlan0 band=\[2.4\] channel=\[none\] AP" "$R/calls" && grep -qx "rc=0" "$R/calls"' \
    "  the configuration written ahead for wlan0: launched, the band handed to the AP layer (and no station's channel)"
  echo "sta0 CarLink-0a1b 192.168.43.1 36 0" > "$R/tmp/.radio_ap_prepared"; rm -f "$R/up"; : > "$R/calls"
  hal "$R" "$sh" "${apoff}; caps() { return 3; }; ap_running() { [ -e '$R/up' ]; }; do_wifi_ap_on >/dev/null; echo \"rc=\$?\" >> '$R/calls'"
  check '! grep -q "configured ahead on sta0" "$R/calls" && ! grep -q "^ap_up" "$R/calls"' \
    "  one written ahead for sta0: not taken (the AP's interface is wlan0)"
  rm -f "$R/etc/ocbm/wifi_link"; rm -f "$R/up"; : > "$R/calls"
  hal "$R" "$sh" "${apoff}; ap_running() { [ -e '$R/up' ]; }; do_wifi_ap_on; echo \"rc=\$?\" >> '$R/calls'"
  check 'grep -qx "ap_up sta0 band=\[\] channel=\[none\] AP" "$R/calls" && grep -qx "rc=0" "$R/calls"' \
    "  no link: the configuration written ahead for sta0 launched as in 2.7.1, no band"
  rm -f "$R/tmp/.radio_ap_prepared" "$R/tmp/.radio_ap_link" "$R/up"
done
# The seam and the payload carry nothing of the retired hotspot mode's station.
check '! grep -q -e wifi_sta_up -e sta_channel -e RADIO_AP_CHANNEL -e "ocbmd wpa" -e bench_sta_ "$REPO/ccpa/rootfs/script/radio_hal.sh" "$REPO/ccpa/rootfs/script/radio_ap_up.sh"' \
  "radio_hal.sh and radio_ap_up.sh: no station verb, channel to follow, supplicant client or station bench lever"
check '[ ! -e "$REPO/ccpa/rootfs/script/wifi_sta.sh" ] && [ ! -e "$REPO/ccpa/rootfs/script/wifi_sta_dhcp.sh" ] && ! bash "$REPO/tools/make_ocbm_installer.sh" --list | grep -q -e wpa -e wifi_sta' \
  "no station keeper, DHCP hook or supplicant in the tree or the payload"
[ "$fails" -eq 0 ] && echo "wifi_link_test: all passed" || { echo "wifi_link_test: $fails FAILED"; exit 1; }
