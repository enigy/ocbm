#!/usr/bin/env bash
# radio_ap_up_test.sh — radio_ap_up.sh's passphrase (ap_passphrase) against scratch adapters, on the host: the whole
# read and write, from hostapd.conf, the key OCBM kept and /etc/wifi_password to hostapd.conf and the kept copy.
# radio_ap_up.sh is sourced for its functions alone (RADIO_AP_UP_FUNCS=1) under every POSIX shell found (sh, dash),
# and tools/busybox_suites.sh runs this again under BusyBox's sh, tr and awk, the adapter's: its tr has no [:print:],
# and the hosts' do (the Mac's round 2, R1-1). No key is ever in what the script logs.
#
#   tools/radio_ap_up_test.sh
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
AP="$REPO/ccpa/rootfs/script/radio_ap_up.sh"
T="$(mktemp -d "${TMPDIR:-/tmp}/radio_ap_up_test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
fails=0
ok()   { echo "  ok   $*"; }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

KEPT='0f3a9c41d27e88b05c6e'   # the form a new random key takes: 20 hex digits
PW='a/b&c\d#e"f'\''g h'        # a stock owner's /etc/wifi_password: / & \ # " ' and an inner space
SP='Family car 1 '            # one that ends in a space

box() {  # $1 dir, $2 hostapd.conf's passphrase line ('' for none): a scratch adapter's /etc
  rm -rf "$1"; mkdir -p "$1/etc/ocbm"
  { echo "interface=wlan0"; echo "ssid=CarLink-fd6c"; [ -n "$2" ] && printf '%s\n' "$2"; echo "channel=36"; } > "$1/etc/hostapd.conf"
}
run_ap() {  # $1 shell, $2 dir: radio_ap_up.sh's ap_passphrase on it, as the adapter runs it; what it logs in $2.out
  RADIO_AP_UP_FUNCS=1 SUF=fd6c "$1" -c '. "$1" && ap_passphrase "$2"' sh "$AP" "$2" > "$2.out" 2>&1
}
key() { sed -n 's/^wpa_passphrase=//p' "$1/etc/hostapd.conf"; }   # every passphrase line: one is expected
kept() { cat "$1/etc/ocbm/wifi_psk" 2>/dev/null; }
said() { grep -qF -- "$2" "$1.out"; }
untold() { ! grep -qF -- "$2" "$1.out"; }   # the key is never logged
rest() { [ "$(grep -v '^wpa_passphrase=' "$1/etc/hostapd.conf")" = "$(printf 'interface=wlan0\nssid=CarLink-fd6c\nchannel=36')" ]; }

# ap_configure's cases (16 Nov 2026). A scratch adapter as a session's first AP start finds it: hostapd.conf on wlan0
# under an older name, udhcpd.conf's pool on NCM's subnet, the box's name kept, and the AP interface sta0.
adapter() {  # $1 dir
  rm -rf "$1"; mkdir -p "$1/etc/ocbm" "$1/tmp" "$1/sys/class/net/sta0"
  printf 'interface=wlan0\nssid=ccpa-fe01\nwpa_passphrase=%s\nhw_mode=a\nchannel=36\nht_capab=[HT40+]\n' "$PW" > "$1/etc/hostapd.conf"
  printf 'start 192.168.50.100\nend 192.168.50.200\ninterface wlan0\nlease_file /var/lib/udhcpd.leases\n' > "$1/etc/udhcpd.conf"
  echo "CarLink-fd6c" > "$1/etc/carplay_ident"
  echo "02:00:00:00:fe:01" > "$1/sys/class/net/sta0/address"
}
# ap_configure then ap_stamp under shell $1 on adapter $2 for interface $3, as PREP runs them; what it said in $2.out.
conf() {
  OCBM_TEST_PATH="${OCBM_TEST_PATH:-$SED_SHIM}" RADIO_AP_UP_FUNCS=1 IF="$3" \
    "$1" -c '. "$1" && ap_configure "$2" && ap_stamp "$2"' sh "$AP" "$2" > "$2.out" 2>&1
}
# ap_prepared_ok under shell $1 on adapter $2 for interface $3 (the seam's name for the box in $4): "NAME WLANIP ch".
prepared() {
  OCBM_TEST_PATH="${OCBM_TEST_PATH:-$SED_SHIM}" RADIO_AP_UP_FUNCS=1 IF="$3" RADIO_BOX_NAME="${4:-}" \
    "$1" -c '. "$1" && ap_prepared_ok "$2" && echo "$NAME $WLANIP $ch"' sh "$AP" "$2"
}
# Every file of adapter $1 back to 2020, and a mark at 2021 that nothing_newer measures against.
age() { find "$1/etc" -type f -exec touch -t 202001010000 {} +; touch -t 202101010000 "$1.mark"; }
nothing_newer() { [ -z "$(find "$1/etc" -type f -newer "$1.mark")" ]; }

# The AP's DHCP server stopped and seen gone (the release review of resident-bt, A3-1): stand-in servers, real
# processes, whose command lines carry this run's own tag, so nothing else on this host can match. A host's ps lists
# only its terminal's processes, BusyBox's all of them: a stand-in ps lists all (tools/busybox_suites.sh runs
# BusyBox's own). HAL is the seam; both scripts carry ap_dhcp_stop.
HAL="$REPO/ccpa/rootfs/script/radio_hal.sh"
TAG="ocbmtest$$"
MATCH="udhcpd .*$TAG/etc/udhcpd.conf"
mkdir -p "$T/fake" "$T/psbin" "$T/$TAG/etc" "$T/$TAG/tmp"
cat > "$T/fake/udhcpd" <<'EOF'
#!/bin/sh
# A stand-in AP DHCP server. $1: slow (exits 1 s after SIGTERM, as a lease file written to slow flash) or deaf (ignores
# SIGTERM; only SIGKILL ends it).
case "$1" in slow) trap 'sleep 1; exit 0' TERM ;; deaf) trap '' TERM ;; esac
while :; do sleep 0.1; done
EOF
chmod 755 "$T/fake/udhcpd"
cat > "$T/psbin/ps" <<'EOF'
#!/bin/sh
exec /bin/ps -A -o pid= -o command=
EOF
chmod 755 "$T/psbin/ps"
PSPATH="${OCBM_TEST_PATH:-$T/psbin}"
STANDINS=""
standin() {  # $1 slow|deaf, $2 config path: one stand-in server, detached (so it is reaped as it exits); its pid echoed
  ( "$T/fake/udhcpd" "$1" -f "$2" </dev/null >/dev/null 2>&1 & echo $! )
}
gone() { ! kill -0 "$1" 2>/dev/null; }
# ap_dhcp_stop from script $2 under shell $1 ($3: that script's functions-only switch); what it said in $T/dstop.out.
dstop() {
  env OCBM_TEST_PATH="$PSPATH" AP_DHCP_MATCH="$MATCH" "$3=1" "$1" -c '. "$1" && ap_dhcp_stop' sh "$2" > "$T/dstop.out" 2>&1
}
trap 'for p in $STANDINS; do kill -9 "$p" 2>/dev/null; done; rm -rf "$T"' EXIT

# BusyBox's and GNU sed edit in place with a bare -i; a BSD sed (a Mac's) takes -i with a suffix. On such a host the
# AP layer's sed -i is given a stand-in that does the same edit with this host's sed. tools/busybox_suites.sh and the
# cross builder run the real thing.
SED_SHIM=""
echo a > "$T/sedtest"
if [ -z "${OCBM_TEST_PATH:-}" ] && { ! sed -i 's/a/b/' "$T/sedtest" 2>/dev/null || [ "$(cat "$T/sedtest")" != b ]; }; then
  mkdir -p "$T/shim"
  _realsed="$(command -v sed)"
  cat > "$T/shim/sed" <<EOF
#!/bin/sh
if [ "\$1" = -i ]; then
  shift; _s=\$1; shift
  for _f in "\$@"; do "$_realsed" "\$_s" "\$_f" > "\$_f.shim" && cat "\$_f.shim" > "\$_f"; rm -f "\$_f.shim"; done
else
  exec "$_realsed" "\$@"
fi
EOF
  chmod 755 "$T/shim/sed"
  SED_SHIM="$T/shim"
  echo "== this host's sed takes -i with a suffix: the AP layer's in-place edits run through a stand-in"
fi

. "$HERE/test_shells.sh"
for sh in ${SHELLS:-$(distinct_shells sh dash)}; do
  command -v "$sh" >/dev/null 2>&1 || { echo "== $sh: not installed, skipped"; continue; }
  echo "== $sh: ap_passphrase"

  R="$T/kept"; box "$R" "wpa_passphrase=12345678"   # stock's default, as a go-back leaves it
  printf '%s\n' "$KEPT" > "$R/etc/ocbm/wifi_psk"; printf 'stockpass1\n' > "$R/etc/wifi_password"
  run_ap "$sh" "$R"
  check '[ "$(key "$R")" = "$KEPT" ] && said "$R" "set the one OCBM kept" && [ "$(kept "$R")" = "$KEPT" ]' \
    "a kept hex key goes back into hostapd.conf as it was, and stays kept"
  check 'untold "$R" "$KEPT" && rest "$R"' "the key isn't logged; the rest of hostapd.conf is as it was"

  R="$T/pw"; box "$R" "wpa_passphrase=12345678"
  printf '%s\n' "$PW" > "$R/etc/wifi_password"
  run_ap "$sh" "$R"
  check '[ "$(key "$R")" = "$PW" ] && said "$R" "set /etc/wifi_password" && [ "$(kept "$R")" = "$PW" ]' \
    "/etc/wifi_password with / & \\ # \" ' and an inner space: into hostapd.conf and the kept copy as it is"
  check 'untold "$R" "$PW" && rest "$R"' "the key isn't logged; the rest of hostapd.conf is as it was"

  R="$T/sp"; box "$R" ""   # no passphrase line at all
  printf '%s\n' "$SP" > "$R/etc/wifi_password"
  run_ap "$sh" "$R"
  check '[ "$(key "$R")" = "$SP" ] && [ "$(kept "$R")" = "$SP" ] && rest "$R"' "a key that ends in a space keeps it, on a line of its own"

  R="$T/good"; box "$R" "wpa_passphrase=$PW"; printf 'other-pass\n' > "$R/etc/wifi_password"
  run_ap "$sh" "$R"
  check '[ "$(key "$R")" = "$PW" ] && [ ! -s "$R.out" ] && [ "$(kept "$R")" = "$PW" ]' \
    "a good key in hostapd.conf is left alone, said nothing about, and kept"

  # The random key is md5sum's, which the adapter's BusyBox has and a Mac may not.
  if [ -n "${OCBM_TEST_PATH:-}" ] || command -v md5sum >/dev/null 2>&1; then
    R="$T/none"; box "$R" "wpa_passphrase=1234"; printf 'carplayfd6c\n' > "$R/etc/wifi_password"
    run_ap "$sh" "$R"
    check 'k="$(key "$R")"; [ "${#k}" = 20 ] && [ -z "$(printf %s "$k" | tr -d 0-9a-f)" ] && said "$R" "set a new random one" && [ "$(kept "$R")" = "$k" ]' \
      "too short, and a known default in /etc/wifi_password: a new random key, kept"
  else
    echo "  (skipped: a new random key needs md5sum, which this host lacks)"
  fi

  # ---- the AP's configuration (ap_configure), and the stamp the armed state's PREP leaves (16 Nov 2026) ----------
  echo "== $sh: ap_configure, the configuration written ahead, and its stamp"
  R="$T/cfg"; adapter "$R"
  conf "$sh" "$R" sta0
  check 'grep -qx "ssid=CarLink-fd6c" "$R/etc/hostapd.conf" && grep -qx "interface=sta0" "$R/etc/hostapd.conf" && grep -qx "channel=36" "$R/etc/hostapd.conf"' \
    "hostapd.conf: the box's name, the AP interface, its channel"
  check 'grep -q "^start[[:space:]]*192\.168\.43\.100" "$R/etc/udhcpd.conf" && grep -q "^end[[:space:]]*192\.168\.43\.200" "$R/etc/udhcpd.conf"' \
    "udhcpd.conf: the pool moved off NCM's 192.168.50 onto the AP's subnet"
  check 'grep -qx "interface	sta0" "$R/etc/udhcpd.conf" && grep -qx "lease_file	/var/lib/udhcpd.leases" "$R/etc/udhcpd.conf" && grep -qx "pidfile	/var/run/udhcpd.pid" "$R/etc/udhcpd.conf" && grep -qx "auto_time	60" "$R/etc/udhcpd.conf"' \
    "udhcpd.conf: the interface and BusyBox's three keys"
  check '[ "$(cat "$R/etc/wifi_name")" = CarLink-fd6c ] && [ "$(key "$R")" = "$PW" ]' "the name kept for the vendor's readers; a good key untouched"
  check '[ "$(head -1 "$R/tmp/.radio_ap_prepared")" = "sta0 CarLink-fd6c 192.168.43.1 36 0" ] && [ "$(sed -n 2p "$R/tmp/.radio_ap_prepared" | wc -w)" -eq 8 ]' \
    "the stamp: interface, name, address, channel, 2.4 GHz flag, then a checksum for each of the four watched files there"
  # Again, unchanged: nothing written (the arm runs this at every host departure, and /etc is flash).
  age "$R"
  conf "$sh" "$R" sta0
  check 'nothing_newer "$R"' "a second configuration with nothing changed writes no file"
  check '[ "$(grep -c . "$R/etc/udhcpd.conf")" = 6 ]' "and adds no key twice"
  # The stamp is trusted for the interface it names while what it was written from holds what it held: by content,
  # never by date. The release review of resident-bt (A3-2): the boot's arm writes the stamp under the 2020 clock the
  # adapter keeps until a host sets it, and the files carry dates a 2026 clock gave them.
  touch -t 202001020000 "$R/tmp/.radio_ap_prepared"; find "$R/etc" -type f -exec touch -t 202611161200 {} +
  check 'prepared "$sh" "$R" sta0 >"$R.p" && [ "$(cat "$R.p")" = "CarLink-fd6c 192.168.43.1 36" ]' \
    "files dated after the stamp, their contents as they were: trusted, the name, address and channel from it"
  check '! prepared "$sh" "$R" wlan0 >/dev/null' "not for another interface"
  check '! prepared "$sh" "$R" sta0 CarLink-0000 >/dev/null' "not when the seam names the box otherwise"
  cp -p "$R/etc/hostapd.conf" "$R/hostapd.keep"
  sed 's/^channel=36$/channel=40/' "$R/hostapd.keep" > "$R/etc/hostapd.conf"; touch -t 201901010000 "$R/etc/hostapd.conf"
  check '! prepared "$sh" "$R" sta0 >/dev/null' "not once hostapd.conf holds something else, however old its date"
  cp -p "$R/hostapd.keep" "$R/etc/hostapd.conf"
  check 'prepared "$sh" "$R" sta0 >/dev/null' "(trusted again with hostapd.conf as it was)"
  printf '10.1.2.1\n' > "$R/etc/wifi_ip"
  check '! prepared "$sh" "$R" sta0 >/dev/null' "not once /etc/wifi_ip is there"
  rm "$R/etc/wifi_ip"
  touch -t 202001010000 "$R/etc/wifi_use_24G"
  check '! prepared "$sh" "$R" sta0 >/dev/null' "not once the 2.4 GHz flag is there, however old"
  rm "$R/etc/wifi_use_24G"; echo "CarLink-beef" > "$R/etc/carplay_ident"
  check '! prepared "$sh" "$R" sta0 >/dev/null' "not once /etc/carplay_ident names the box otherwise"
  echo "CarLink-fd6c" > "$R/etc/carplay_ident"
  check 'prepared "$sh" "$R" sta0 >/dev/null' "(trusted again with the name as it was)"
  { echo "sta0 CarLink-fd6c 192.168.43.1 x 0"; sed -n 2p "$R/tmp/.radio_ap_prepared"; } > "$R.st"; mv "$R.st" "$R/tmp/.radio_ap_prepared"
  check '! prepared "$sh" "$R" sta0 >/dev/null' "not a stamp with a channel that is no number"
  head -1 "$R/tmp/.radio_ap_prepared" | sed 's/ x / 36 /' > "$R.st"; mv "$R.st" "$R/tmp/.radio_ap_prepared"
  check '! prepared "$sh" "$R" sta0 >/dev/null' "not a stamp of one line (an older one, without the checksums)"
  # A changed input is configured afresh: the 2.4 GHz flag moves the AP to channel 6 in g.
  : > "$R/etc/wifi_use_24G"
  conf "$sh" "$R" sta0
  check 'grep -qx "channel=6" "$R/etc/hostapd.conf" && grep -qx "hw_mode=g" "$R/etc/hostapd.conf" && [ "$(head -1 "$R/tmp/.radio_ap_prepared")" = "sta0 CarLink-fd6c 192.168.43.1 6 1" ]' \
    "the 2.4 GHz flag: channel 6, hw_mode g, and the stamp says so"
  check 'prepared "$sh" "$R" sta0 >/dev/null' "and that stamp is trusted"
  # Never 192.168.50.x: an /etc/wifi_ip that reads as NCM's subnet (octal here) is refused, as before.
  printf '192.168.062.1\n' > "$R/etc/wifi_ip"
  conf "$sh" "$R" sta0
  check 'grep -q "192\.168\.43\.100" "$R/etc/udhcpd.conf" && said "$R" "not a plain IPv4 address"' "an octal 192.168.062.1 is refused, the AP stays on .43"

  echo "== $sh: the AP's DHCP server, stopped and seen gone (both scripts), and this AP's own asked after"
  for which in "$HAL RADIO_HAL_FUNCS" "$AP RADIO_AP_UP_FUNCS"; do
    set -- $which; script=$1; hook=$2; name=$(basename "$script")
    slow=$(standin slow "$T/$TAG/etc/udhcpd.conf"); ncm=$(standin slow "$T/$TAG/tmp/udhcpd_ncm.conf")
    STANDINS="$STANDINS $slow $ncm"; sleep 0.3
    t0=$(date +%s); dstop "$sh" "$script" "$hook"; t1=$(date +%s)
    check 'gone "$slow" && ! grep -q SIGKILL "$T/dstop.out" && [ $((t1 - t0)) -le 3 ]' \
      "$name: a server that takes a second over its lease file is waited for, not killed"
    check '! gone "$ncm"' "$name: the NCM server (its own config) is never touched"
    kill -9 "$ncm" 2>/dev/null
    deaf=$(standin deaf "$T/$TAG/etc/udhcpd.conf"); STANDINS="$STANDINS $deaf"; sleep 0.3
    dstop "$sh" "$script" "$hook"
    check 'gone "$deaf" && grep -q "3 s after SIGTERM" "$T/dstop.out"' "$name: one still there 3 s after SIGTERM is SIGKILLed, and gone"
    dstop "$sh" "$script" "$hook"
    check '[ ! -s "$T/dstop.out" ]' "$name: none there: nothing to do, nothing said"
  done
  check 'echo "  321 root  1408 S  busybox udhcpd -f -a 250 /etc/udhcpd.conf" | grep -q "udhcpd .*etc/udhcpd.conf" && ! echo "  322 root  1408 S  busybox udhcpd -f /tmp/udhcpd_ncm.conf" | grep -q "udhcpd .*etc/udhcpd.conf"' \
    "the pattern both scripts use by default finds the AP's server and never the NCM one"
  check '[ "$(RADIO_AP_UP_FUNCS=1 "$sh" -c ". \"\$1\"; echo \"\$AP_DHCP_MATCH\"" sh "$AP")" = "udhcpd .*etc/udhcpd.conf" ] && [ "$(RADIO_HAL_FUNCS=1 "$sh" -c ". \"\$1\"; echo \"\$AP_DHCP_MATCH\"" sh "$HAL")" = "udhcpd .*etc/udhcpd.conf" ]' \
    "(and it is the default in both)"
  # This AP's own server, by the pid this start kept: alive through the confirmation, or the AP is not serving.
  mkdir -p "$T/proc/777"
  serving() { env OCBM_TEST_PATH="$PSPATH" AP_DHCP_MATCH="$1" AP_PROC="$T/proc" RADIO_AP_UP_FUNCS=1 _dhcpd="${2:-}" "$sh" -c '. "$1" && ap_dhcp_serving' sh "$AP"; }
  echo "777 (busybox) S 1 777" > "$T/proc/777/stat"
  check 'serving "$MATCH" 777' "the server this start launched, running: serving"
  echo "777 (busybox) Z 1 777" > "$T/proc/777/stat"
  check '! serving "$MATCH" 777' "it exited (a zombie this start has not reaped): not serving, whatever else is listed"
  rm "$T/proc/777/stat"
  check '! serving "$MATCH" 777' "gone altogether: not serving"
  echo "777 (setsid) Z 1 777" > "$T/proc/777/stat"; alive=$(standin slow "$T/$TAG/etc/udhcpd.conf"); STANDINS="$STANDINS $alive"; sleep 0.3
  check 'serving "$MATCH" 777 && ! serving "udhcpd .*nothing-$TAG" 777' "the pid setsid's, not the server's: asked by config path, as before"
  check 'serving "$MATCH" "" && ! serving "udhcpd .*nothing-$TAG" ""' "none launched by this start (an AP already up): by config path, as before"
  kill -9 "$alive" 2>/dev/null
done
[ "$fails" -eq 0 ] && echo "radio_ap_up_test: all passed" || { echo "radio_ap_up_test: $fails failed"; exit 1; }
