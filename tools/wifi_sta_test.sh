#!/usr/bin/env bash
# wifi_sta_test.sh — the Wi-Fi link's station keeper (ccpa/rootfs/script/wifi_sta.sh) and its DHCP hook, on the host,
# against a scratch root (WIFI_STA_ROOT) with the supplicant, `ocbmd wpa` (its control socket's client, in place of
# wpa_cli since 24 Nov 2026), udhcpc, ifconfig and pidof stood in for: what it
# starts and on which interface, the configuration it writes (0600, the name in hex, the key as given), the state it
# publishes, the channel it keeps, the mismatch line, the scans around a CarPlay start (none while the phone joins the
# AP, the AP's channel and a look a minute once the session runs), the first look and the adapter's own session's
# (ADDENDUM 9), its waits (no supplicant, one interface, an AP on the station's interface, the seam's wlan lock), and
# its clean stop. Under every POSIX shell found (sh, dash), and BusyBox's by tools/busybox_suites.sh.
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
# ocbmd's wpa subcommand (ccpa/ocbmd/src/wpa.rs), `ocbmd wpa -p DIR IFACE COMMAND...`: only with the keeper's own control
# directory (else no answer, and said); `status` prints the scratch state file; every other request is noted.
cat > "$S/ocbmd" <<'EOF'
#!/bin/sh
[ "$1" = wpa ] || exit 2
shift; dir=""; [ "$1" = -p ] && { dir=$2; shift 2; }
iface=$1; shift
[ "$dir" = "$STUB_R/var/run/wpa_supplicant" ] || { echo "wpa: another control directory: $dir" >> "$STUB_R/calls"; exit 1; }
case "$1" in
  status) cat "$STUB_R/wpa_status" 2>/dev/null ;;
  # A look (ADDENDUM 9): what the scratch wpa_find file says it heard, else nothing.
  find) echo "wpa $iface $*" >> "$STUB_R/calls"; cat "$STUB_R/wpa_find" 2>/dev/null || echo "seen 0" ;;
  *) echo "wpa $iface $*" >> "$STUB_R/calls"; echo OK ;;
esac
EOF
# A wpa_cli an adapter may still hold from a bench package (ocbm-wpa_cli): the keeper must never call it.
printf '#!/bin/sh\necho "wpa_cli $*" >> "$STUB_R/calls"\necho OK\n' > "$S/wpa_cli"
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
# The seam's verb, stood in for; hal3.sh as on a unit with no station (one wireless interface, or no mapped backend).
cat > "$S/hal.sh" <<'EOF'
echo "radio_hal $*" >> "$STUB_R/calls"
EOF
printf 'echo "radio_hal $*" >> "$STUB_R/calls"\nexit 3\n' > "$S/hal3.sh"
# A supplicant that does not start.
printf '#!/bin/sh\necho "wpa_supplicant $*" >> "$STUB_R/calls"\nexit 1\n' > "$S/wpa_fail"; chmod 755 "$S/wpa_fail"

unit() {  # $1 dir, $2 interfaces ("sta0 wlan0"), $3 psk: a scratch adapter with the link set up
  local R="$1" i
  rm -rf "$R"; mkdir -p "$R/etc/ocbm" "$R/tmp" "$R/proc" "$R/var/run" "$R/usr/sbin" "$R/script"
  for i in $2; do mkdir -p "$R/sys/class/net/$i/wireless"; done
  mkdir -p "$R/sys/class/net/ncm0"
  printf 'key=%s\nssid_hex=%s\npsk=%s\n' "$(printf '11%.0s' $(seq 32))" 4578616d706c6520486f7473706f74 "$3" > "$R/etc/ocbm/wifi_link"
  printf 'interface=wlan0\nchannel=36\nhw_mode=a\n' > "$R/etc/hostapd.conf"
  echo "100.00 50.00" > "$R/proc/uptime"
  ln -s "$S/wpa_supplicant" "$R/usr/sbin/ocbm-wpa_supplicant"
  ln -s "$S/wpa_cli" "$R/usr/sbin/ocbm-wpa_cli"   # left by a bench package: never called (checked below)
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
  check '! grep -q -e "scan_freq" -e "freq_list" -e "initial_freq_list" "$R/tmp/wpa_sta.conf"' "  no AP up: every channel"
  check 'grep -qx "disable_scan_offload=1" "$R/tmp/wpa_sta.conf"' "  the supplicant's own scans, never the driver's scheduled one"
  check 'grep -qx "state=joining" "$R/tmp/wifi_link_state" && grep -qx "iface=sta0" "$R/tmp/wifi_link_state"' "joining, published for ocbmd"
  n=0; while [ "$n" -lt 30 ] && ! grep -qx "seen=0 100" "$R/tmp/wifi_link_state"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "^wpa sta0 find 2412-2462,5180-5240,5745-5825 quick$" "$R/calls" && [ "$(sed -n 4p "$R/tmp/wifi_link_state")" = "seen=0 100" ]' \
    "the first look (no last channel kept, quick): the channels an AP may use; heard nowhere, seen=0 as the 4th line"
  check 'grep -q "the hotspot not heard (channels 2412-2462,5180-5240,5745-5825)" "$R/tmp/box.log"' "  said"
  # Joined on the AP's channel.
  printf 'wpa_state=COMPLETED\nfreq=5180\n' > "$R/wpa_status"
  n=0; while [ "$n" -lt 30 ] && ! grep -qx "state=joined" "$R/tmp/wifi_link_state"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -qx "channel=36" "$R/tmp/wifi_link_state" && [ "$(cat "$R/etc/ocbm/wifi_link_channel")" = 36 ]' \
    "joined on channel 36: published, and the channel kept on flash"
  check 'grep -q "^udhcpc -i sta0 -b -R -p" "$R/calls"' "  the DHCP client on sta0"
  check '! grep -q "one radio serves both" "$R/tmp/box.log"' "  the same channel as the AP: no mismatch line"
  # The hotspot moves to 149 while no AP runs: published, nothing said (the next AP starts there, ADDENDUM 6).
  printf 'wpa_state=COMPLETED\nfreq=5745\n' > "$R/wpa_status"
  n=0; while [ "$n" -lt 30 ] && ! grep -qx "channel=149" "$R/tmp/wifi_link_state"; do n=$((n + 1)); sleep 0.1; done
  sleep 0.5
  check 'grep -qx "state=joined" "$R/tmp/wifi_link_state" && ! grep -q "one radio serves both" "$R/tmp/box.log"' \
    "the hotspot on 149 and no AP up: published, nothing said (the next AP starts on 149)"
  # An AP up on its own 36 meanwhile (it came up before the station joined): said plainly, once; the AP not moved.
  : > "$R/hostapd.running"; sleep 0.8
  check '[ "$(grep -c "on channel 149 and the adapter.s AP on 36: one radio serves both; the AP stays for this session" "$R/tmp/box.log")" = 1 ]' \
    "an AP running on its own 36 beside it: one plain line, the AP left for its session"
  check '! grep -q "M1 does not move" "$R/tmp/box.log"' "  (not the old line: the AP does move, at its next start)"
  # The AP on the station's channel, from its configuration where it followed (radio_ap_up.sh): nothing more said.
  printf 'interface=wlan0\nchannel=149\nhw_mode=a\n' > "$R/tmp/hostapd_session.conf"; sleep 0.8
  check '[ "$(grep -c "one radio serves both" "$R/tmp/box.log")" = 1 ]' "  the AP on 149, where it followed: nothing more said"
  # It leaves the hotspot; the AP up, the session not running yet: no scan at all.
  printf 'wpa_state=SCANNING\n' > "$R/wpa_status"
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa sta0 disconnect" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "wpa sta0 disconnect" "$R/calls"' "a CarPlay start on the AP: the supplicant told to stop scanning"
  # The session runs: the AP's channel only, and the supplicant back on. The AP followed to 149: 5745, where the
  # hotspot comes back (it follows the phone's own link to the AP); an AP on hostapd.conf's 36: 5180.
  echo 1 > "$R/tmp/session_healthy"
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa sta0 reconnect" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -qx "	scan_freq=5745" "$R/tmp/wpa_sta.conf" && grep -qx "	freq_list=5745" "$R/tmp/wpa_sta.conf" && grep -q "wpa sta0 reconfigure" "$R/calls" && grep -q "wpa sta0 reconnect" "$R/calls"' \
    "the session running: scans and joins on the AP's channel only, 149 where it followed (5745), the supplicant on again"
  rm -f "$R/tmp/hostapd_session.conf"
  n=0; while [ "$n" -lt 30 ] && ! grep -qx "	scan_freq=5180" "$R/tmp/wpa_sta.conf"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -qx "	scan_freq=5180" "$R/tmp/wpa_sta.conf"' "  an AP on hostapd.conf's own channel, 36: its 5180"
  # A minute later: one look over the channels an AP may use.
  looks=$(grep -c "^wpa sta0 find" "$R/calls")
  check '[ "$looks" = 1 ]' "  no look before FIND_EVERY has passed (a session a host holds: its video on the AP's channel)"
  uptime_at "$R" 170
  n=0; while [ "$n" -lt 30 ] && [ "$(grep -c "^wpa sta0 find" "$R/calls")" = "$looks" ]; do n=$((n + 1)); sleep 0.1; done
  check '[ "$(grep -c "^wpa sta0 find 2412-2462,5180-5240,5745-5825$" "$R/calls")" = 1 ] && ! grep -q "^wpa sta0 scan" "$R/calls"' \
    "  and a look over the channels an AP may use once FIND_EVERY has passed"
  # The AP gone: every channel again.
  rm -f "$R/hostapd.running"
  n=0; while [ "$n" -lt 30 ] && grep -q "scan_freq" "$R/tmp/wpa_sta.conf"; do n=$((n + 1)); sleep 0.1; done
  check '! grep -q "scan_freq" "$R/tmp/wpa_sta.conf"' "no AP: every channel again"
  # A clean stop: its supplicant and DHCP client go, the state and the lock too.
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""
  check '[ ! -e "$R/var/run/wpa_sta.pid" ] && [ ! -e "$R/var/run/udhcpc_sta.pid" ] && [ ! -e "$R/tmp/wifi_link_state" ] && [ ! -d "$R/tmp/.wifi_sta_lock" ]' \
    "SIGTERM: the supplicant, the DHCP client, the state and the lock gone"
  check '! grep -q "another control directory" "$R/calls" && ! grep -q "^wpa_cli" "$R/calls"' \
    "  every request through ocbmd wpa to the keeper's control directory; the wpa_cli left beside it never called"

  # A session starting with no AP yet (a host present: M2's Bluetooth paging comes first): no scans until it runs, then
  # every channel (no AP to stay on).
  R="$T/p"; unit "$R" "sta0 wlan0" "$HASH"; printf 'wpa_state=SCANNING\n' > "$R/wpa_status"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!
  n=0; while [ "$n" -lt 30 ] && [ ! -s "$R/tmp/wifi_link_state" ]; do n=$((n + 1)); sleep 0.1; done
  echo 1 > "$R/tmp/host_present"
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa sta0 disconnect" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "wpa sta0 disconnect" "$R/calls"' "a host present and no AP yet (M2's paging): the supplicant told to stop scanning"
  echo 1 > "$R/tmp/session_healthy"
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa sta0 reconnect" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "wpa sta0 reconnect" "$R/calls" && ! grep -q "scan_freq" "$R/tmp/wpa_sta.conf"' "  the session running with no AP: every channel again"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""

  # The hotspot on a radar channel (100, 5500 MHz): no AP may start there, so the AP runs on its own 36 (radio_ap_up.sh).
  # The station stays joined; the state says so with both channels for MGMT_INFO, and the line says so once.
  R="$T/q"; unit "$R" "sta0 wlan0" "$HASH"; printf 'wpa_state=COMPLETED\nfreq=5500\n' > "$R/wpa_status"; : > "$R/hostapd.running"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!
  n=0; while [ "$n" -lt 50 ] && ! grep -qx "state=joined" "$R/tmp/wifi_link_state" 2>/dev/null; do n=$((n + 1)); sleep 0.1; done
  sleep 0.6
  check 'grep -qx "state=joined" "$R/tmp/wifi_link_state" && grep -qx "channel=100" "$R/tmp/wifi_link_state" && [ "$(grep -c "on channel 100 and the adapter.s AP on 36" "$R/tmp/box.log")" = 1 ]' \
    "the hotspot on radar channel 100, the AP on its own 36: joined on 100 (beside the AP's 36 for MGMT_INFO), said once"
  check '! grep -q "wpa sta0 disconnect" "$R/calls"' "  joined: no scan held or made"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""

  # ADDENDUM 9, the adapter's own session at power-up (ocbmd's parked mark, a host present, no AP yet), the hotspot's
  # last channel 6 kept: the supplicant's first scan on 6 alone, the first look on 6 heard it, so no second look and no
  # pause: scans and joins on 6 alone before the AP comes; the join's events said once (names and codes only).
  R="$T/m2"; unit "$R" "sta0 wlan0" "$HASH"; printf 'wpa_state=SCANNING\n' > "$R/wpa_status"; echo 6 > "$R/etc/ocbm/wifi_link_channel"
  echo 1 > "$R/tmp/host_present"; : > "$R/tmp/wifi_self_parked"
  printf 'seen 6 -40\nevent trying freq=2437\nevent assoc-reject status_code=1\n' > "$R/wpa_find"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!
  n=0; while [ "$n" -lt 30 ] && ! grep -qx "	freq_list=2437" "$R/tmp/wpa_sta.conf" 2>/dev/null; do n=$((n + 1)); sleep 0.1; done
  check '[ "$(grep -c "^wpa sta0 find" "$R/calls")" = 1 ] && grep -q "^wpa sta0 find 2437 quick$" "$R/calls" && [ "$(sed -n 4p "$R/tmp/wifi_link_state")" = "seen=6 100" ]' \
    "the first look on the hotspot's last channel (2437) alone heard it: seen=6, no second look"
  check 'grep -q "the hotspot heard on channel 6 (-40 dBm)" "$R/tmp/box.log" && [ "$(grep -c "supplicant: trying freq=2437, assoc-reject status_code=1" "$R/tmp/box.log")" = 1 ]' \
    "  said, with the join's events (names and codes only)"
  check 'grep -qx "	scan_freq=2437" "$R/tmp/wpa_sta.conf" && grep -qx "	freq_list=2437" "$R/tmp/wpa_sta.conf" && ! grep -q "wpa sta0 disconnect" "$R/calls"' \
    "  no pause: scans and joins on 6 alone, so the station may join before the AP comes"
  # The AP up on 6 (it followed the look), the phone joining: no scans; the session running: a look at once (no host,
  # the video parked), then every PARKED_FIND (20 s). Heard on 11 twice in a row: elsewhere=11 for ocbmd.
  printf 'interface=wlan0\nchannel=6\nhw_mode=g\n' > "$R/tmp/hostapd_session.conf"; : > "$R/hostapd.running"
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa sta0 disconnect" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "wpa sta0 disconnect" "$R/calls" && grep -q "CarPlay is starting: no scans until it runs" "$R/tmp/box.log"' \
    "the AP up, the phone joining: no scans until the session runs"
  looks=$(grep -c "^wpa sta0 find" "$R/calls"); echo 1 > "$R/tmp/session_healthy"
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa sta0 reconnect" "$R/calls"; do n=$((n + 1)); sleep 0.1; done
  sleep 0.6
  check '[ "$(grep -c "^wpa sta0 find" "$R/calls")" = "$looks" ] && grep -q "a look over the channels an AP may use every 20 s" "$R/tmp/box.log"' \
    "  the session running with no host, the AP where the hotspot was heard (6): no look at once (it joins there)"
  printf 'seen 11 -50\n' > "$R/wpa_find"; uptime_at "$R" 121
  n=0; while [ "$n" -lt 30 ] && ! grep -q "heard on channel 11" "$R/tmp/box.log"; do n=$((n + 1)); sleep 0.1; done
  sleep 0.5
  check '! grep -q "^elsewhere=" "$R/tmp/wifi_link_state" && grep -q "the hotspot heard on channel 11 (-50 dBm)" "$R/tmp/box.log"' \
    "  heard on 11 once: said, nothing for ocbmd yet"
  check '[ "$(cat "$R/etc/ocbm/wifi_link_channel")" = 11 ]' "  and kept as the hotspot's last channel"
  uptime_at "$R" 142
  n=0; while [ "$n" -lt 30 ] && ! grep -q "^elsewhere=" "$R/tmp/wifi_link_state"; do n=$((n + 1)); sleep 0.1; done
  check 'grep -qx "elsewhere=11 142" "$R/tmp/wifi_link_state" && grep -q "heard on channel 11 twice and the AP runs on 6: no join while" "$R/tmp/box.log"' \
    "  heard on 11 twice in a row, the AP on 6: elsewhere=11 for ocbmd, and why"
  printf 'seen 6 -40\n' > "$R/wpa_find"; uptime_at "$R" 163
  n=0; while [ "$n" -lt 30 ] && grep -q "^elsewhere=" "$R/tmp/wifi_link_state"; do n=$((n + 1)); sleep 0.1; done
  check '! grep -q "^elsewhere=" "$R/tmp/wifi_link_state"' "  heard on the AP's channel again: elsewhere= gone"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""

  # No session, the hotspot's last channel 6 kept, nothing heard: the supplicant's first scan on 2437 alone
  # (initial_freq_list), the first look on 2437, then over the channels an AP may use; every channel after that.
  R="$T/ini"; unit "$R" "sta0 wlan0" "$HASH"; printf 'wpa_state=SCANNING\n' > "$R/wpa_status"; echo 6 > "$R/etc/ocbm/wifi_link_channel"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!
  n=0; while [ "$n" -lt 30 ] && ! grep -qx "seen=0 100" "$R/tmp/wifi_link_state" 2>/dev/null; do n=$((n + 1)); sleep 0.1; done
  check 'grep -qx "initial_freq_list=2437" "$R/tmp/wpa_sta.conf" && ! grep -q -e "scan_freq" -e "	freq_list" "$R/tmp/wpa_sta.conf"' \
    "the supplicant's first scan on the hotspot's last channel alone (initial_freq_list=2437), every channel after"
  check '[ "$(grep "^wpa sta0 find" "$R/calls" | tr "\n" "|")" = "wpa sta0 find 2437 quick|wpa sta0 find 2412-2462,5180-5240,5745-5825 quick|" ]' \
    "  the first look: its last channel, then the channels an AP may use, quick (no join waited for)"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""

  # The adapter's own session, the first look heard nothing (the hotspot off): paused while the phone joins the AP on
  # the last channel; the session running: a look at once (the hotspot may have come on elsewhere meanwhile).
  R="$T/off"; unit "$R" "sta0 wlan0" "$HASH"; printf 'wpa_state=SCANNING\n' > "$R/wpa_status"; echo 6 > "$R/etc/ocbm/wifi_link_channel"
  echo 1 > "$R/tmp/host_present"; : > "$R/tmp/wifi_self_parked"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa sta0 disconnect" "$R/calls" 2>/dev/null; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "wpa sta0 disconnect" "$R/calls" && grep -qx "seen=0 100" "$R/tmp/wifi_link_state"' \
    "the adapter's own session, the hotspot not heard at the first look: no scans while it starts"
  printf 'interface=wlan0\nchannel=6\nhw_mode=g\n' > "$R/tmp/hostapd_session.conf"; : > "$R/hostapd.running"
  looks=$(grep -c "^wpa sta0 find" "$R/calls"); echo 1 > "$R/tmp/session_healthy"
  n=0; while [ "$n" -lt 30 ] && [ "$(grep -c "^wpa sta0 find" "$R/calls")" = "$looks" ]; do n=$((n + 1)); sleep 0.1; done
  check '[ "$(grep -c "^wpa sta0 find 2412-2462,5180-5240,5745-5825$" "$R/calls")" = 1 ]' \
    "  the session running on the last channel, nothing heard there: a look at once"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""

  # A USB host's start (a host present, no parked mark), the last channel kept: no look before or during its start.
  R="$T/usb"; unit "$R" "sta0 wlan0" "$HASH"; printf 'wpa_state=SCANNING\n' > "$R/wpa_status"; echo 6 > "$R/etc/ocbm/wifi_link_channel"
  echo 1 > "$R/tmp/host_present"; printf 'seen 6 -40\n' > "$R/wpa_find"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!
  n=0; while [ "$n" -lt 30 ] && ! grep -q "wpa sta0 disconnect" "$R/calls" 2>/dev/null; do n=$((n + 1)); sleep 0.1; done
  sleep 0.4
  check 'grep -q "wpa sta0 disconnect" "$R/calls" && ! grep -q "^wpa sta0 find" "$R/calls"' \
    "a USB host's start: no look, no scan until it runs, as before"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""

  # The seam's wlan lock held (the boot preload loading the driver), no interface yet: looked for every 0.2 s, the verb
  # not called behind the lock; called once the lock is gone.
  R="$T/lk"; unit "$R" "" "$HASH"; mkdir -p "$R/tmp/.radio_lock_wlan"; echo $$ > "$R/tmp/.radio_lock_wlan/pid"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!; sleep 1
  check '! grep -q "^radio_hal wifi_sta_up" "$R/calls" 2>/dev/null' "the wlan lock held (a driver load under way): the seam's verb not called behind it"
  rm -rf "$R/tmp/.radio_lock_wlan"
  n=0; while [ "$n" -lt 20 ] && ! grep -q "^radio_hal wifi_sta_up" "$R/calls" 2>/dev/null; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "^radio_hal wifi_sta_up" "$R/calls"' "  the lock gone: called"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""

  # Back-offs: what cannot change before the next boot, or for a while, is asked for and said once.
  R="$T/c"; unit "$R" "sta0" "$HASH"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.1 WIFI_STA_HAL="$S/hal3.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!; sleep 1.2; kill "$BG"; wait "$BG" 2>/dev/null; BG=""
  check '[ "$(grep -c "^radio_hal wifi_sta_up" "$R/calls")" = 1 ] && [ "$(grep -c "no station interface on this unit" "$R/tmp/box.log")" = 1 ] && ! grep -q "^wpa_supplicant" "$R/calls"' \
    "no station on this unit (the seam's 3): asked once, said once, no supplicant"
  R="$T/c1"; unit "$R" "sta0" "$HASH"; echo 1 > "$R/tmp/.radio_one_iface"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.1 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!; n=0; while [ "$n" -lt 20 ] && ! grep -qx "state=unsupported" "$R/tmp/wifi_link_state" 2>/dev/null; do n=$((n + 1)); sleep 0.1; done
  check 'grep -qx "state=unsupported" "$R/tmp/wifi_link_state" && [ ! -e "$R/calls" ]' \
    "  the seam's one-interface mark: not even asked, and state=unsupported for ocbmd"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""
  R="$T/c2"; unit "$R" "sta0 wlan0" "$HASH"; ln -sf "$S/wpa_fail" "$R/usr/sbin/ocbm-wpa_supplicant"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.1 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!; sleep 1.2; kill "$BG"; wait "$BG" 2>/dev/null; BG=""
  check '[ "$(grep -c "^wpa_supplicant" "$R/calls")" = 1 ] && [ "$(grep -c "did not start" "$R/tmp/box.log")" = 1 ]' \
    "a supplicant that does not start: tried again only 30 s later, said once"

  # Waits.
  R="$T/d"; unit "$R" "sta0 wlan0" "$HASH"; rm -f "$R/usr/sbin/ocbm-wpa_supplicant"
  env STUB_R="$R" WIFI_STA_ROOT="$R" WIFI_STA_TICK=0.2 WIFI_STA_HAL="$S/hal.sh" OCBM_TEST_PATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
    "$sh" "$KEEPER" >/dev/null 2>&1 &
  BG=$!; n=0; while [ "$n" -lt 20 ] && [ ! -s "$R/tmp/wifi_link_state" ]; do n=$((n + 1)); sleep 0.1; done
  check 'grep -q "no wpa_supplicant" "$R/tmp/box.log" && [ ! -e "$R/calls" ]' "no supplicant anywhere: it waits, starting nothing"
  check '[ "$(sed -n 4p "$R/tmp/wifi_link_state" 2>/dev/null)" = "seen=0 100" ]' "  and says no look will come (seen=0): no AP start waits for one"
  kill "$BG"; wait "$BG" 2>/dev/null; BG=""
  R="$T/e"; unit "$R" "sta0 wlan0" "$HASH"; rm -f "$R/usr/sbin/ocbm-wpa_supplicant" "$R/usr/sbin/ocbm-wpa_cli"
  mkdir -p "$R/tmp/bin"; ln -s "$S/wpa_supplicant" "$R/tmp/bin/wpa_supplicant"; keep "$R" "$sh"
  check 'grep -q "supplicant: $R/tmp/bin/wpa_supplicant" "$R/tmp/box.log"' "a bench copy in /tmp/bin is taken, no wpa_cli beside it"
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
  st='caps() { RADIO_BACKEND=mapped; return 0; }; wlan_driver_up() { return 0; }; log() { echo "log $*" >> "'"$R"'/calls"; }
      do_wifi_sta_up; echo "rc=$?"'
  mkdir -p "$R/tmp"; rm -f "$R/tmp/.radio_one_iface"; : > "$R/calls"; t0=$(date +%s)
  check '[ "$(hal "$R" "$sh" "$st")" = rc=3 ] && [ -e "$R/tmp/.radio_one_iface" ] && grep -q "one wireless interface on this driver" "$R/calls"' \
    "  wifi_sta_up says so (3) once the driver's probe has settled, and marks it for the boot"
  check '[ $(( $(date +%s) - t0 )) -le 3 ]' "  in about a second, not the 10 s it waited before"
  : > "$R/calls"
  check '[ "$(hal "$R" "$sh" "$st")" = rc=3 ] && [ ! -s "$R/calls" ]' "  and at once after that, asking the driver nothing"
  rm -f "$R/tmp/.radio_one_iface"
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
  # ADDENDUM 6: the station's channel the seam hands the AP layer (sta_channel), from the keeper's state, no fork.
  fch() { hal "$R" "$sh" 'sta_channel; echo "[$_fch]"'; }
  printf 'iface=sta0\nstate=joined\nchannel=149\n' > "$R/tmp/wifi_link_state"
  check '[ "$(fch)" = "[]" ]' "sta_channel: no link, a keeper's state left over: nothing to follow"
  printf 'key=%s\nssid_hex=6e6574\npsk=\n' "$(printf '11%.0s' $(seq 32))" > "$R/etc/ocbm/wifi_link"
  check '[ "$(fch)" = "[149]" ]' "  a link, the station joined on 149: 149"
  printf 'iface=sta0\nstate=joining\nchannel=0\n' > "$R/tmp/wifi_link_state"
  check '[ "$(fch)" = "[0]" ]' "  not on the hotspot: 0"
  printf 'iface=\nstate=unsupported\nchannel=0\n' > "$R/tmp/wifi_link_state"
  check '[ "$(fch)" = "[]" ]' "  no station on this unit: nothing"
  rm -f "$R/tmp/wifi_link_state"
  check '[ "$(fch)" = "[0]" ]' "  a link and no keeper state yet (the keeper starting): not on the hotspot, 0"
done
# The keeper's table of the channels an AP may use is radio_ap_up.sh's.
check '[ "$(grep "^ap_may_use()" "$KEEPER")" = "$(grep "^ap_may_use()" "$REPO/ccpa/rootfs/script/radio_ap_up.sh")" ]' \
  "the keeper's ap_may_use is radio_ap_up.sh's, character for character"
[ "$fails" -eq 0 ] && echo "wifi_sta_test: all passed" || { echo "wifi_sta_test: $fails FAILED"; exit 1; }
