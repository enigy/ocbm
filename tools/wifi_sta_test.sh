#!/usr/bin/env bash
# wifi_sta_test.sh — the Wi-Fi link's station keeper (ccpa/rootfs/script/wifi_sta.sh) and its DHCP hook, on the host,
# against a scratch root (WIFI_STA_ROOT) with the supplicant, wpa_cli, udhcpc, ifconfig and pidof stood in for: what it
# starts and on which interface, the configuration it writes (0600, the name in hex, the key as given), the state it
# publishes, the channel it keeps, the mismatch line, the scans around a CarPlay start (none while the phone joins the
# AP, the AP's channel and a full scan a minute once the session runs), its waits (no supplicant, one interface, an AP
# on the station's interface), and its clean stop. Under every POSIX shell found (sh, dash), and BusyBox's by
# tools/busybox_suites.sh.
#
#   tools/wifi_sta_test.sh
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
KEEPER="$REPO/ccpa/rootfs/script/wifi_sta.sh"
HOOK="$REPO/ccpa/rootfs/script/wifi_sta_dhcp.sh"
T="$(mktemp -d "${TMPDIR:-/tmp}/wifi_sta_test.XXXXXX")"
BG=""
trap '[ -n "$BG" ] && kill "$BG" 2>/dev/null; for f in "$T"/*/var/run/*.pid; do [ -e "$f" ] && kill -9 "$(cat "$f")" 2>/dev/null; done; rm -rf "$T"' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

S="$T/stub"; mkdir -p "$S"
# The supplicant: daemonizes (-B) as the real one does, writing its pid file (-P) and its argv to the calls file.
cat > "$S/wpa_supplicant" <<'EOF'
#!/bin/sh
echo "wpa_supplicant $*" >> "$STUB_R/calls"
pf=""; while [ $# -gt 0 ]; do [ "$1" = -P ] && pf=$2; shift; done
( trap 'exit 0' TERM; while :; do sleep 0.1; done ) </dev/null >/dev/null 2>&1 &
echo $! > "$pf"
EOF
# wpa_cli: `status` prints the scratch state file; every other command is noted.
cat > "$S/wpa_cli" <<'EOF'
#!/bin/sh
while [ $# -gt 0 ]; do case "$1" in -p|-i) shift 2 ;; *) break ;; esac; done
case "$1" in
  status) cat "$STUB_R/wpa_status" 2>/dev/null ;;
  *) echo "wpa_cli $*" >> "$STUB_R/calls"; echo OK ;;
esac
EOF
cat > "$S/udhcpc" <<'EOF'
#!/bin/sh
echo "udhcpc $*" >> "$STUB_R/calls"
pf=""; while [ $# -gt 0 ]; do [ "$1" = -p ] && pf=$2; shift; done
( trap 'exit 0' TERM; while :; do sleep 0.1; done ) </dev/null >/dev/null 2>&1 &
echo $! > "$pf"
EOF
cat > "$S/ifconfig" <<'EOF'
#!/bin/sh
echo "ifconfig $*" >> "$STUB_R/calls"
EOF
cat > "$S/pidof" <<'EOF'
#!/bin/sh
[ "$1" = hostapd ] && [ -e "$STUB_R/hostapd.running" ]
EOF
chmod 755 "$S"/*
# The seam's verb, stood in for.
cat > "$S/hal.sh" <<'EOF'
echo "radio_hal $*" >> "$STUB_R/calls"
EOF

unit() {  # $1 dir, $2 interfaces ("sta0 wlan0"), $3 psk: a scratch adapter with the link set up
  local R="$1" i
  rm -rf "$R"; mkdir -p "$R/etc/ocbm" "$R/tmp" "$R/proc" "$R/var/run" "$R/usr/sbin" "$R/script"
  for i in $2; do mkdir -p "$R/sys/class/net/$i/wireless"; done
  mkdir -p "$R/sys/class/net/ncm0"
  printf 'key=%s\nssid_hex=%s\npsk=%s\n' "$(printf '11%.0s' $(seq 32))" 4578616d706c6520486f7473706f74 "$3" > "$R/etc/ocbm/wifi_link"
  printf 'interface=wlan0\nchannel=36\nhw_mode=a\n' > "$R/etc/hostapd.conf"
  echo "100.00 50.00" > "$R/proc/uptime"
  ln -s "$S/wpa_supplicant" "$R/usr/sbin/ocbm-wpa_supplicant"
  ln -s "$S/wpa_cli" "$R/usr/sbin/ocbm-wpa_cli"
}
keep() {  # $1 dir, $2 shell, then env: one pass of the keeper
  local R="$1" sh="$2"; shift 2
  env "$@" STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_ONCE=1 WIFI_STA_TICK=0 WIFI_STA_HAL="$S/hal.sh" \
    OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" "$sh" "$KEEPER" >/dev/null 2>&1
}
uptime_at() { echo "$2.00 1.00" > "$1/proc/uptime"; }

. "$HERE/test_shells.sh"
HASH=$(printf 'ab%.0s' $(seq 32))
for sh in ${SHELLS:-$(distinct_shells sh dash)}; do
  echo "== wifi_sta.sh under $sh"
  R="$T/a"; unit "$R" "sta0 wlan0" "$HASH"
  keep "$R" "$sh"
  check 'grep -q "^wpa_supplicant -B -P $R/var/run/wpa_sta.pid -D nl80211,wext -i sta0 -c $R/tmp/wpa_sta.conf" "$R/calls"' \
    "the supplicant on the station's interface (sta0), never the AP's (wlan0)"
  check '! grep -q "wlan0" "$R/calls"' "  and nothing done to wlan0"
  check 'grep -q "station interface: sta0" "$R/tmp/box.log" && ! grep -qi "example\|hotspot\"" "$R/tmp/box.log"' \
    "the box log names the interface, never the hotspot's name"
  # The configuration, as written before the pass's exit removed it: run again with the keeper kept alive.
  R="$T/b"; unit "$R" "sta0 wlan0" "$HASH"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!
  n=0; while [ "$n" -lt 50 ] && [ ! -s "$R/tmp/wifi_link_state" ]; do n=$((n + 1)); sleep 0.1; done
  check '[ "$(ls -l "$R/tmp/wpa_sta.conf" | cut -c1-10)" = "-rw-------" ]' "the supplicant's configuration is 0600"
  check 'grep -qx "	ssid=4578616d706c6520486f7473706f74" "$R/tmp/wpa_sta.conf" && grep -qx "	psk=$HASH" "$R/tmp/wpa_sta.conf"' \
    "  the name in hex, the hashed key as it came"
  check '! grep -q "scan_freq" "$R/tmp/wpa_sta.conf"' "  no AP up: every channel"
  check 'grep -qx "state=joining" "$R/tmp/wifi_link_state" && grep -qx "iface=sta0" "$R/tmp/wifi_link_state"' "joining, published for ocbmd"
  # Joined on the AP's channel.
  printf 'wpa_state=COMPLETED\nfreq=5180\n' > "$R/wpa_status"
  n=0; while [ "$n" -lt 30 ] && ! grep -qx "state=joined" "$R/tmp/wifi_link_state"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -qx "channel=36" "$R/tmp/wifi_link_state" && [ "$(cat "$R/etc/ocbm/wifi_link_channel")" = 36 ]' \
    "joined on channel 36: published, and the channel kept on flash"
  check 'grep -q "^udhcpc -i sta0 -b -R -p" "$R/calls"' "  the DHCP client on sta0"
  check '! grep -q "one radio serves both" "$R/tmp/box.log"' "  the same channel as the AP: no mismatch line"
  # The hotspot moves to 149: said plainly, once.
  printf 'wpa_state=COMPLETED\nfreq=5745\n' > "$R/wpa_status"
  n=0; while [ "$n" -lt 30 ] && ! grep -qx "channel=149" "$R/tmp/wifi_link_state"; do n=$((n + 1)); sleep 0.1; done
  sleep 0.5
  check '[ "$(grep -c "on channel 149 and the adapter.s AP on 36" "$R/tmp/box.log")" = 1 ]' "another channel than the AP's: one plain line"
  # It leaves the hotspot; the AP comes up, the session is not running yet: no scan at all.
  printf 'wpa_state=SCANNING\n' > "$R/wpa_status"; : > "$R/hostapd.running"
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa_cli disconnect" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "wpa_cli disconnect" "$R/calls"' "a CarPlay start on the AP: the supplicant told to stop scanning"
  # The session runs: the AP's channel only, and the supplicant back on.
  echo 1 > "$R/tmp/session_healthy"
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa_cli reconnect" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -qx "	scan_freq=5180" "$R/tmp/wpa_sta.conf" && grep -q "wpa_cli reconfigure" "$R/calls" && grep -q "wpa_cli reconnect" "$R/calls"' \
    "the session running: scans of the AP's channel only (5180), the supplicant on again"
  # A minute later: one full scan.
  uptime_at "$R" 170
  n=0; while [ "$n" -lt 30 ] && ! grep -q "^wpa_cli scan" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "^wpa_cli scan" "$R/calls"' "  and a scan of every channel once FULL_SCAN_EVERY has passed"
  # The AP gone: every channel again.
  rm -f "$R/hostapd.running"
  n=0; while [ "$n" -lt 30 ] && grep -q "scan_freq" "$R/tmp/wpa_sta.conf"; do n=$((n + 1)); sleep 0.1; done
  check '! grep -q "scan_freq" "$R/tmp/wpa_sta.conf"' "no AP: every channel again"
  # A clean stop: its supplicant and DHCP client go, the state and the lock too.
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""
  check '[ ! -e "$R/var/run/wpa_sta.pid" ] && [ ! -e "$R/var/run/udhcpc_sta.pid" ] && [ ! -e "$R/tmp/wifi_link_state" ] && [ ! -d "$R/tmp/.wifi_sta_lock" ]' \
    "SIGTERM: the supplicant, the DHCP client, the state and the lock gone"

  # Waits.
  R="$T/c"; unit "$R" "sta0" "$HASH"; keep "$R" "$sh"
  check 'grep -q "one wireless interface only" "$R/tmp/box.log" && ! grep -q "^wpa_supplicant" "$R/calls" 2>/dev/null' \
    "one wireless interface: no station beside the AP"
  R="$T/d"; unit "$R" "sta0 wlan0" "$HASH"; rm -f "$R/usr/sbin/ocbm-wpa_supplicant"; keep "$R" "$sh"
  check 'grep -q "no wpa_supplicant" "$R/tmp/box.log" && [ ! -e "$R/calls" ]' "no supplicant anywhere: it waits, starting nothing"
  R="$T/e"; unit "$R" "sta0 wlan0" "$HASH"; rm -f "$R/usr/sbin/ocbm-wpa_supplicant" "$R/usr/sbin/ocbm-wpa_cli"
  mkdir -p "$R/tmp/bin"; ln -s "$S/wpa_supplicant" "$R/tmp/bin/wpa_supplicant"; ln -s "$S/wpa_cli" "$R/tmp/bin/wpa_cli"; keep "$R" "$sh"
  check 'grep -q "supplicant: $R/tmp/bin/wpa_supplicant" "$R/tmp/box.log"' "a bench copy in /tmp/bin is taken"
  R="$T/f"; unit "$R" "sta0 wlan0" "$HASH"; printf 'interface=sta0\nchannel=36\n' > "$R/etc/hostapd.conf"; : > "$R/hostapd.running"
  keep "$R" "$sh"
  check 'grep -q "the AP runs on sta0" "$R/tmp/box.log" && ! grep -q "^wpa_supplicant" "$R/calls" 2>/dev/null' \
    "an AP still on the station's interface: the station waits"
  R="$T/g"; unit "$R" "" "$HASH"; keep "$R" "$sh"
  check 'grep -q "^radio_hal wifi_sta_up" "$R/calls"' "no wireless interface: the seam's wifi_sta_up loads the driver"
  R="$T/h"; unit "$R" "sta0 wlan0" ""; sed -i.x '/^ssid_hex=/d' "$R/etc/ocbm/wifi_link"; keep "$R" "$sh"
  check 'grep -q "no hotspot in" "$R/tmp/box.log" && [ ! -e "$R/calls" ]' "no hotspot set up: nothing to keep"
  # The other key forms, written before the pass ended.
  for form in passphrase open; do
    R="$T/k$form"; case $form in passphrase) unit "$R" "sta0 wlan0" "a good passphrase" ;; open) unit "$R" "sta0 wlan0" "" ;; esac
    env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
      "$sh" "$KEEPER" >/dev/null 2>&1 &
    BG=$!
    n=0; while [ "$n" -lt 50 ] && [ ! -s "$R/tmp/wifi_link_state" ]; do n=$((n + 1)); sleep 0.1; done
    case $form in
      passphrase) check 'grep -qx "	psk=\"a good passphrase\"" "$R/tmp/wpa_sta.conf"' "a passphrase goes in quoted" ;;
      open) check 'grep -qx "	key_mgmt=NONE" "$R/tmp/wpa_sta.conf"' "no key: an open network" ;;
    esac
    kill "$BG"; wait "$BG" 2>/dev/null; BG=""
  done
  # A second keeper while one runs: it leaves at once.
  R="$T/l"; unit "$R" "sta0 wlan0" "$HASH"; mkdir -p "$R/tmp/.wifi_sta_lock"; echo $$ > "$R/tmp/.wifi_sta_lock/pid"
  check '[ "$(env WIFI_STA_ROOT="$R" WIFI_STA_ONCE=1 "$sh" "$KEEPER" 2>&1)" = "[wifi_sta] already running (pid $$)" ]' "one keeper at a time"

  echo "== wifi_sta_dhcp.sh under $sh"
  B="$T/hook"; mkdir -p "$B/tmp"; : > "$B/calls"
  hook() { env STUB_R="$B" WIFI_STA_ROOT="$B" PATH="$S:$PATH" interface=sta0 ip="$2" subnet=255.255.255.240 router=172.20.10.1 "$sh" "$HOOK" "$1" 2>/dev/null; }
  hook bound 172.20.10.5
  check 'grep -qx "ifconfig sta0 172.20.10.5 netmask 255.255.255.240 up" "$B/calls" && ! grep -q route "$B/calls"' \
    "bound: the address and netmask only, no default route"
  : > "$B/calls"; hook bound 192.168.43.7
  check 'grep -qx "ifconfig sta0 0.0.0.0" "$B/calls" && ! grep -q "192.168.43.7" "$B/calls"' "an address on the AP's own subnet is not taken"
done
# radio_hal.sh's STA mode: which interface the AP takes, and what its teardown touches. A copy with /sys, the link file
# and hostapd.conf moved into a scratch root, sourced for its functions (as tools/radio_ap_up_test.sh does).
hal_copy() {  # $1 root
  sed -e "s#/sys/#$1/sys/#g; s#/etc/ocbm/wifi_link#$1/etc/ocbm/wifi_link#g; s#/etc/hostapd.conf#$1/etc/hostapd.conf#g" \
      -e "s#/etc/udhcpd.conf#$1/etc/udhcpd.conf#g; s#=/tmp/#=$1/tmp/#g; s# /tmp/udhcpd_setup# $1/tmp/udhcpd_setup#g" \
    "$REPO/ccpa/rootfs/script/radio_hal.sh" > "$1/radio_hal.sh"
}
hal() {  # $1 root, $2 shell, $3 commands (the functions sourced)
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="${OCBM_TEST_PATH:-}" "$2" -c ". \"\$1\"; $3" sh "$1/radio_hal.sh" 2>/dev/null
}
for sh in ${SHELLS:-$(distinct_shells sh dash)}; do
  echo "== radio_hal.sh STA mode under $sh"
  R="$T/hal"; rm -rf "$R"; mkdir -p "$R/etc/ocbm" "$R/sys/class/net/sta0/wireless" "$R/sys/class/net/wlan0/wireless" "$R/sys/class/net/ncm0"
  hal_copy "$R"
  check '[ "$(hal "$R" "$sh" "ap_iface")" = sta0 ] && ! hal "$R" "$sh" "sta_mode"' \
    "no link set up: the AP takes the first interface (sta0), as in 2.7.1"
  gr='grep() { echo grep >> "'"$R"'/calls"; command grep "$@"; }; '
  : > "$R/calls"; hal "$R" "$sh" "${gr}sta_mode; ap_iface >/dev/null; sta_mode"
  check '! grep -q grep "$R/calls"' "  and forks no grep doing it (the shell's own test of the link file first)"
  printf 'key=%s\n' "$(printf '11%.0s' $(seq 32))" > "$R/etc/ocbm/wifi_link"
  check '[ "$(hal "$R" "$sh" "ap_iface")" = sta0 ]' "a link key with no hotspot: still the first interface"
  printf 'ssid_hex=6e6574\npsk=\n' >> "$R/etc/ocbm/wifi_link"
  : > "$R/calls"; hal "$R" "$sh" "${gr}sta_mode"
  check 'grep -q grep "$R/calls"' "  (with a link, the file is read)"
  check '[ "$(hal "$R" "$sh" "ap_iface")" = wlan0 ] && [ "$(hal "$R" "$sh" "sta_iface")" = sta0 ]' \
    "a hotspot set up: the station keeps sta0, the AP takes wlan0"
  check '[ "$(RADIO_STA_IF=wlan0 hal "$R" "$sh" "ap_iface")" = sta0 ]' "RADIO_STA_IF names the station's interface outright"
  rm -rf "$R/sys/class/net/wlan0"
  check '[ "$(hal "$R" "$sh" "ap_iface")" = sta0 ] && ! hal "$R" "$sh" "sta_iface"' \
    "one wireless interface: the AP keeps it, and there is no station"
  mkdir -p "$R/sys/class/net/wlan0/wireless"
  : > "$R/calls"
  off='killall() { :; }; ap_dhcp_stop() { :; }; ap_running() { return 1; }; caps() { return 0; }
       ifconfig() { echo "ifconfig $*" >> "'"$R"'/calls"; }; do_wifi_ap_off >/dev/null'
  printf 'interface=wlan0\nchannel=36\n' > "$R/etc/hostapd.conf"; hal "$R" "$sh" "$off"
  check 'grep -qx "ifconfig wlan0 down" "$R/calls" && ! grep -q sta0 "$R/calls"' "the AP's teardown downs the AP's wlan0, never the station's sta0"
  : > "$R/calls"; printf 'interface=sta0\nchannel=36\n' > "$R/etc/hostapd.conf"; hal "$R" "$sh" "$off"
  check 'grep -qx "ifconfig sta0 0.0.0.0" "$R/calls" && ! grep -q "sta0 down" "$R/calls"' \
    "an AP raised on sta0 before the link was set up: only its address goes, the interface stays up for the station"
  rm -f "$R/etc/ocbm/wifi_link"; : > "$R/calls"; hal "$R" "$sh" "$off"
  check 'grep -qx "ifconfig sta0 down" "$R/calls"' "no link: the teardown downs the first interface, as in 2.7.1"
  # M3's set-up network: its configuration in /tmp from the CarPlay AP's, client isolation on, the flash file untouched.
  mkdir -p "$R/tmp"
  printf 'interface=sta0\nssid=CarLink-0a1b\nwpa_passphrase=private\nchannel=36\nhw_mode=a\n' > "$R/etc/hostapd.conf"
  printf 'start\t192.168.43.100\ninterface\tsta0\nlease_file\t/var/lib/udhcpd.leases\n' > "$R/etc/udhcpd.conf"
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
  check 'grep -q "^ok sta0" "$R/tmp/.radio_setup_ap"' "  the marker ocbmd waits for"
done
[ "$fails" -eq 0 ] && echo "wifi_sta_test: all passed" || { echo "wifi_sta_test: $fails FAILED"; exit 1; }
