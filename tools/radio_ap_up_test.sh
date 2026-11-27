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
# A stand-in AP DHCP server. $1: slow (exits 1 s after SIGTERM, as a lease file written to slow flash), quick (0.3 s
# after it) or deaf (ignores SIGTERM; only SIGKILL ends it).
case "$1" in slow) trap 'sleep 1; exit 0' TERM ;; quick) trap 'sleep 0.3; exit 0' TERM ;; deaf) trap '' TERM ;; esac
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
# Every stand-in so far ended and seen gone, so that one an earlier case failed to stop is no later case's.
standins_ended() {
  for p in $STANDINS; do kill -9 "$p" 2>/dev/null; done
  for p in $STANDINS; do _n=0; while ! gone "$p" && [ "$_n" -lt 20 ]; do _n=$((_n + 1)); sleep 0.1; done; done
  STANDINS=""
}
# ap_dhcp_stop from script $2 under shell $1 ($3: that script's functions-only switch); what it said in $T/dstop.out.
dstop() {
  env OCBM_TEST_PATH="$PSPATH" AP_DHCP_MATCH="$MATCH" "$3=1" "$1" -c '. "$1" && ap_dhcp_stop' sh "$2" > "$T/dstop.out" 2>&1
}
trap 'for p in $STANDINS; do kill -9 "$p" 2>/dev/null; done; rm -rf "$T"' EXIT

# The whole start, its commands stood in for (the second review of resident-bt, item 5): radio_ap_up.sh runs against a
# scratch adapter (AP_ROOT, AP_PROC, AP_BOXLOG) with these first on its PATH. Each keeps its state in $S. hostapd and
# the AP's udhcpd are processes, as on the adapter; setsid execs (so $! is the server's own pid, as there).
S="$T/stubs"; mkdir -p "$S"
printf '#!/bin/sh\nexec "$@"\n' > "$S/setsid"
cat > "$S/ifconfig" <<EOF
#!/bin/sh
# ifconfig IF ADDR ...: kept for IF; ifconfig IF: shown, as BusyBox's shows an address.
if [ \$# -ge 2 ]; then echo "\$2" > "$S/if.\$1"; exit 0; fi
a=\$(cat "$S/if.\$1" 2>/dev/null); [ -n "\$a" ] && echo "          inet addr:\$a  Mask:255.255.255.0"
exit 0
EOF
cat > "$S/pidof" <<EOF
#!/bin/sh
# pidof hostapd: the stand-in's, while it runs.
p=\$(cat "$S/hostapd.pid" 2>/dev/null)
[ -n "\$p" ] && kill -0 "\$p" 2>/dev/null && { echo "\$p"; exit 0; }
exit 1
EOF
cat > "$S/hostapd" <<EOF
#!/bin/sh
# A stand-in hostapd: its pid for pidof, its AP-ENABLED half a second in (its stdout is the box log), then running. It
# notes the configuration it was given, and refuses at once one on the channel $S/refuse names (ADDENDUM 6).
echo "\$1" >> "$S/hostapd.confs"
grep -qx "channel=\$(cat "$S/refuse" 2>/dev/null)" "\$1" 2>/dev/null && { echo "Could not set channel for kernel driver"; exit 1; }
echo \$\$ > "$S/hostapd.pid"
sleep 0.5; echo "sta0: AP-ENABLED "
while :; do sleep 0.1; done
EOF
cat > "$S/busybox" <<EOF
#!/bin/sh
# busybox udhcpd ...: a stand-in for the server the start launches, its /proc stat under AP_PROC. It notes whether the
# last AP's server (OLD_DHCP) still ran as it started; with $S/udhcpd.exit there it exits at once (as one that could not
# bind), its stat left a zombie's.
mkdir -p "\$AP_PROC/\$\$"; echo "\$\$ (busybox) S 1 \$\$" > "\$AP_PROC/\$\$/stat"; echo \$\$ > "$S/udhcpd.pid"
if [ -n "\${OLD_DHCP:-}" ] && kill -0 "\$OLD_DHCP" 2>/dev/null; then echo "old there"; else echo "old gone"; fi > "$S/udhcpd.start"
[ -e "$S/udhcpd.exit" ] && { echo "\$\$ (busybox) Z 1 \$\$" > "\$AP_PROC/\$\$/stat"; exit 1; }
while :; do sleep 0.1; done
EOF
cat > "$S/ps" <<EOF
#!/bin/sh
# ps, and with $S/ps.extra there a line more: a listing that names an AP DHCP server this start did not launch.
"$PSPATH/ps" "\$@"; cat "$S/ps.extra" 2>/dev/null; exit 0
EOF
chmod 755 "$S"/*
# radio_ap_up.sh (PREP when $2 says so) under shell $1 on adapter $R, OLD_DHCP passed on, and FOLLOW as the seam's
# RADIO_AP_CHANNEL when set (STA mode), BAND as its RADIO_AP_BAND (mode ap); its output in $R.out and its status in
# $R.rc. The servers and hostapd it leaves
# running are this run's to end (ended).
start_ap() {
  env ${FOLLOW+"RADIO_AP_CHANNEL=$FOLLOW"} ${BAND+"RADIO_AP_BAND=$BAND"} \
    OCBM_TEST_PATH="$S:$PSPATH${SED_SHIM:+:$SED_SHIM}" AP_ROOT="$R" AP_PROC="$R/proc" AP_BOXLOG="$R/tmp/box.log" \
    AP_DHCP_MATCH="$MATCH" RADIO_WLAN_IF=sta0 OLD_DHCP="${OLD_DHCP:-}" "$1" "$AP" ${2:-} > "$R.out" 2>&1
  echo $? > "$R.rc"
}
ended() {  # the stand-in hostapd and udhcpd a start left, ended, and their records gone
  for _f in "$S/hostapd.pid" "$S/udhcpd.pid"; do [ -s "$_f" ] && kill -9 "$(cat "$_f")" 2>/dev/null; done
  rm -f "$S/hostapd.pid" "$S/udhcpd.pid" "$S/udhcpd.start"
}
trap 'ended; for p in $STANDINS; do kill -9 "$p" 2>/dev/null; done; rm -rf "$T"' EXIT

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

  # The whole start (the second review of resident-bt, item 5): the stop on a launch, the server it starts and the pid
  # it keeps, so that a start which counted any AP DHCP server listed by config path (the A3-1 bug) fails here.
  echo "== $sh: the whole start, its commands stood in for: the last AP's DHCP server is never this one's"
  R="$T/ap/$TAG"; adapter "$R"; mkdir -p "$R/sys/class/net/sta0/wireless" "$R/proc"
  standins_ended; ended; rm -f "$S"/if.* "$S/udhcpd.exit" "$S/ps.extra"
  start_ap "$sh" PREP
  check '[ "$(cat "$R.rc")" = 0 ] && grep -q "configured ahead: ssid=CarLink-fd6c ip=192.168.43.1 ch=36 if=sta0 (nothing raised)" "$R.out" && [ ! -e "$S/hostapd.pid" ] && [ ! -e "$S/udhcpd.pid" ]' \
    "PREP, as the arm runs it: configured, and nothing raised"
  # The bench's bad wake: the last AP's server still over its lease file as the next AP starts.
  old=$(standin quick "$T/$TAG/etc/udhcpd.conf"); ncm=$(standin quick "$T/$TAG/tmp/udhcpd_ncm.conf")
  STANDINS="$STANDINS $old $ncm"; sleep 0.3
  OLD_DHCP=$old; start_ap "$sh"; OLD_DHCP=""
  check '[ "$(cat "$R.rc")" = 0 ] && grep -q "configured ahead (/tmp/.radio_ap_prepared)" "$R.out" && grep -q "AP up: ssid=CarLink-fd6c ip=192.168.43.1 ch=36 if=sta0 dhcp=yes" "$R.out"' \
    "the configuration written ahead trusted, and the AP up"
  check 'gone "$old" && ! grep -q "after SIGTERM" "$R.out" && [ "$(cat "$S/udhcpd.start" 2>/dev/null)" = "old gone" ] && kill -0 "$(cat "$S/udhcpd.pid")" 2>/dev/null' \
    "the last AP's server, still exiting: waited for, gone before this AP's own started, and that one serving"
  check '! gone "$ncm" && grep -qx "\[radio\] hostapd starting on sta0" "$R/tmp/box.log"' \
    "the NCM server untouched; the start's own line in the box log (its AP-ENABLED comes after it)"
  kill -9 "$ncm" 2>/dev/null
  # This AP's own server exits at once while the listing still names another (here a pid with no process, so the stop
  # has nothing to wait for): only the server this start launched counts. 12 s: the converge step runs to its bound.
  ended; : > "$S/udhcpd.exit"
  ph=$(sh -c 'echo $$')
  echo "$ph busybox udhcpd -f -a 250 $T/$TAG/etc/udhcpd.conf" > "$S/ps.extra"
  start_ap "$sh"
  check '[ "$(cat "$R.rc")" = 1 ] && grep -q "the AP DHCP server is not running (the one started here, pid $(cat "$S/udhcpd.pid"), exited)" "$R.out"' \
    "this AP's own server gone, another still listed: not up, and the one started here named"
  ended; rm -f "$S/udhcpd.exit" "$S/ps.extra"
  # The last AP's server deaf to SIGTERM at a start: SIGKILLed after the start's 1 s, not the teardown's 3 (the wlan
  # lock this holds is what btd's 0x5702 reply waits on, 5 s at most).
  old=$(standin deaf "$T/$TAG/etc/udhcpd.conf"); STANDINS="$STANDINS $old"; sleep 0.3
  OLD_DHCP=$old; start_ap "$sh"; OLD_DHCP=""
  check '[ "$(cat "$R.rc")" = 0 ] && grep -q "still there 1 s after SIGTERM" "$R.out" && gone "$old" && [ "$(cat "$S/udhcpd.start" 2>/dev/null)" = "old gone" ]' \
    "one deaf to SIGTERM: SIGKILLed after the start's own 1 s, then this AP's server, and the AP up"
  ended

  # ---- STA mode: the AP on the station's channel (ADDENDUM 6, 24 Nov 2026) ----------------------------------------
  echo "== $sh: STA mode: the AP follows the station's channel, never to a radar channel, hostapd.conf untouched"
  used=$(RADIO_AP_UP_FUNCS=1 "$sh" -c '. "$1"; c=0; while [ $c -le 200 ]; do ap_may_use $c && printf "%s " $c; c=$((c + 1)); done' sh "$AP")
  check '[ "$used" = "1 2 3 4 5 6 7 8 9 10 11 36 40 44 48 149 153 157 161 165 " ]' \
    "the channels an AP may start on (0-200): 1-11, 36-48, 149-165; no radar (DFS) channel, nothing else"
  sides=$(RADIO_AP_UP_FUNCS=1 "$sh" -c '. "$1"; for c in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 36 40 44 48 149 153 157 161 165; do
    ht40_side $c; printf "%s%s " $c "$_hs"; done' sh "$AP")
  check '[ "$sides" = "1+ 2+ 3+ 4+ 5+ 6+ 7+ 8- 9- 10- 11- 12- 13- 14 36+ 40- 44+ 48- 149+ 153- 157+ 161- 165 " ]' \
    "HT40's side, its pair's: 36/40, 44/48, 149/153, 157/161, 1-7 up, 8-13 down; none on 14 and 165"
  # ap_follow on the factory file's shape (country_code=US, 802.11n, HT40+ on 36), the AP's own channel 36: "ch _hrun".
  F="$T/follow"
  mkf() {
    rm -rf "$F" "$F.out"; mkdir -p "$F/etc" "$F/tmp"
    printf 'interface=wlan0\nssid=CarLink-0a1b\nwpa=3\nwpa_passphrase=%s\ndriver=nl80211\nieee80211n=1\n#ieee80211ac=1\ncountry_code=US\n#hw_mode=g\n#channel=11\nhw_mode=a\nchannel=36\n#ht_capab=[SHORT-GI-20][SHORT-GI-40][HT40-]\nht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]\n' \
      "$PW" > "$F/etc/hostapd.conf"
    cp "$F/etc/hostapd.conf" "$F.ref"; touch -t 202001010000 "$F/etc/hostapd.conf"; touch -t 202101010000 "$F.mark"
  }
  fol() {  # $1: the seam's RADIO_AP_CHANNEL (none given: unset, as with no link)
    env ${1+"RADIO_AP_CHANNEL=$1"} RADIO_AP_UP_FUNCS=1 AP_BOXLOG="$F/tmp/box.log" OCBM_TEST_PATH="${OCBM_TEST_PATH:-}" \
      "$sh" -c '. "$1"; ch=36; ap_follow "$2" > "$2.out" 2>&1; echo "$ch $_hrun"' sh "$AP" "$F"
  }
  SC="$F/tmp/hostapd_session.conf"
  others() { grep -v -e '^channel=' -e '^hw_mode=' -e '^ht_capab=' "$1"; }
  # 5 GHz: HT40 on its pair's side; 2.4 GHz (ADDENDUM 9: the truck's hotspot, Maximize Compatibility): 20 MHz, the
  # factory file's own 2.4 GHz line ([SHORT-GI-20] alone).
  while read -r fc hm hs gi; do   # "." for none
    [ "$hs" = . ] && hs=""; [ "${gi:-.}" = . ] && gi=""
    mkf; r=$(fol "$fc"); d="HT40$hs"; [ -n "$hs" ] || d="no HT40 (20 MHz)"
    check '[ "$r" = "$fc $SC" ] && grep -qx "channel=$fc" "$SC" && grep -qx "hw_mode=$hm" "$SC" && grep -qxF "ht_capab=[SHORT-GI-20]${gi:+[SHORT-GI-40]}${hs:+[HT40$hs]}" "$SC"' \
      "the station on $fc: the AP follows, hw_mode=$hm, $d"
    check 'cmp -s "$F/etc/hostapd.conf" "$F.ref" && [ -z "$(find "$F/etc" -type f -newer "$F.mark")" ] && [ "$(others "$SC")" = "$(others "$F.ref")" ]' \
      "  hostapd.conf on flash untouched; the session's every other line as it has it, the passphrase too"
  done <<'EOF'
149 a + 40
153 a - 40
157 a + 40
161 a - 40
165 a . 40
40 a - 40
44 a + 40
48 a - 40
1 g . .
6 g . .
11 g . .
EOF
  mkf; r=$(fol 6)
  check 'grep -q "the AP follows the station to channel 6 (hw_mode=g, 20 MHz)" "$F/tmp/box.log"' "  a 2.4 GHz follow says 20 MHz"
  mkf; printf 'ieee80211ac=1\nvht_capab=[SHORT-GI-80]\nvht_oper_chwidth=1\nvht_oper_centr_freq_seg0_idx=42\n' >> "$F/etc/hostapd.conf"
  r=$(fol 11)
  check '! grep -q -e "^ieee80211ac=" -e "^vht_" "$SC" && grep -qx "#ieee80211ac=1" "$SC" && grep -qx "ieee80211ac=1" "$F/etc/hostapd.conf"' \
    "  no VHT at 2.4 GHz (the flash file keeps its lines; a commented one stays commented)"

  # ADDENDUM 9: the adapter's own session with no host (ocbmd's parked mark), the station not on the hotspot: the AP
  # waits for the station's first look (HEAD_START), then takes the channel it heard, else the hotspot's last, else its
  # own. Any other start (a USB host's) never waits.
  echo "== $sh: STA mode, the adapter's own session: the AP waits for the station's look, then the hotspot's channel"
  park() {  # $1 the keeper's pid (a live one when not given)
    mkf; mkdir -p "$F/etc/ocbm" "$F/tmp/.wifi_sta_lock"; : > "$F/tmp/wifi_self_parked"; echo "${1:-$$}" > "$F/tmp/.wifi_sta_lock/pid"
  }
  st() { { echo iface=sta0; echo "state=$1"; echo "channel=$2"; [ -z "${3:-}" ] || echo "$3"; } > "$F/tmp/wifi_link_state"; }
  last_ch() { echo "$1" > "$F/etc/ocbm/wifi_link_channel"; }
  pfol() {  # $1 RADIO_AP_CHANNEL, $2 HEAD_START: "ch _hrun"
    env RADIO_AP_CHANNEL="$1" HEAD_START="$2" RADIO_AP_UP_FUNCS=1 AP_BOXLOG="$F/tmp/box.log" OCBM_TEST_PATH="${OCBM_TEST_PATH:-}" \
      "$sh" -c '. "$1"; ch=36; ap_follow "$2" > "$2.out" 2>&1; echo "$ch $_hrun"' sh "$AP" "$F"
  }
  logged() { grep -q "$1" "$F/tmp/box.log"; }
  park; st joining 0 "seen=6 12"; last_ch 149; r=$(pfol 0 40)
  check '[ "$r" = "6 $SC" ] && grep -qx "channel=6" "$SC" && grep -qx "hw_mode=g" "$SC" && logged "own session: channel 6, heard there by the station.s look (waited 0 ms"' \
    "the look heard the hotspot on 6: the AP takes 6 at once, 2.4 GHz (not the last channel, 149)"
  park; st joining 0 "seen=0 12"; last_ch 11; r=$(pfol 0 40)
  check '[ "$r" = "11 $SC" ] && logged "own session: channel 11, the hotspot.s last (not heard now)"' \
    "the look heard nothing: the hotspot's last channel, 11"
  park; st joining 0 "seen=0 12"; last_ch 100; r=$(pfol 0 40)
  check '[ "$r" = "36 $F/etc/hostapd.conf" ] && logged "the hotspot not heard and no last channel" && logged "not on the hotspot - the AP keeps its own channel 36"' \
    "  a last channel no AP may use (100): the AP's own, 36"
  park; last_ch 6; ( sleep 0.6; st joining 0 "seen=1 13" ) & r=$(pfol 0 40); wait
  check '[ "$r" = "1 $SC" ] && logged "channel 1, heard there by the station.s look (waited [4-9]00 ms"' \
    "no look yet: the AP waits for it (0.6 s), then takes what it heard (1)"
  park; last_ch 6; st joining 0; ( sleep 0.3; st joined 11 "seen=11 13" ) & r=$(pfol 0 40); wait
  check '[ "$r" = "11 $SC" ] && logged "channel 11, the station joined the hotspot there"' "  the station joins meanwhile: its channel"
  park; last_ch 6; r=$(pfol 0 5)
  check '[ "$r" = "6 $SC" ] && logged "channel 6, the hotspot.s last (not heard now) (waited 500 ms"' \
    "no look within HEAD_START (0.5 s here): the hotspot's last"
  park "$(sh -c 'echo $$')"; last_ch 6; r=$(pfol 0 40)
  check '[ "$r" = "6 $SC" ] && logged "(waited 0 ms"' "  no keeper running: no wait"
  park; rm -f "$F/tmp/wifi_self_parked"; last_ch 6; t0=$(date +%s); r=$(pfol 0 40)
  check '[ "$r" = "36 $F/etc/hostapd.conf" ] && ! logged "own session" && [ $(( $(date +%s) - t0 )) -le 1 ]' \
    "not the adapter's own session (a USB host's start, say): no wait, its own channel, as before"
  park; st joined 149; last_ch 6; r=$(pfol 149 40)
  check '[ "$r" = "149 $SC" ] && ! logged "own session"' "the station on the hotspot when the seam asked: followed, no look waited for"

  # ADDENDUM 13: the look heard the hotspot. A join under way is waited for, from the supplicant's STATUS (a stand-in
  # ocbmd's, its states one a request), up to sta_join_wait (6 s; off: none), or with sta_join_first until the station
  # is on the hotspot; not joined, the station is held still while hostapd starts (DISCONNECT, never over a join, and
  # the mark the keeper reads; sta_ap_hold off: not).
  echo "== $sh: STA mode, the adapter's own session: a join under way waited for, the station held still for hostapd"
  J="$T/joinbin"; JS="$T/joinst"; mkdir -p "$J"
  printf '#!/bin/sh\nshift\nexec "$@"\n' > "$J/timeout"
  cat > "$J/ocbmd" <<'EOF'
#!/bin/sh
# ocbmd wpa -p DIR IFACE status|disconnect: STATUS from the first line of $JS/states ("STATE [FREQ]", taken off while
# more follow; no file: no supplicant answers), every request noted in $JS/asked.
shift; echo "$4 $3" >> "$JS/asked"
[ "$4" = status ] || { echo OK; exit 0; }
[ -s "$JS/states" ] || exit 1
l=$(sed -n 1p "$JS/states")
[ -n "$(sed -n 2p "$JS/states")" ] && { sed 1d "$JS/states" > "$JS/states.x"; mv -f "$JS/states.x" "$JS/states"; }
set -- $l; echo "wpa_state=$1"; [ -n "${2:-}" ] && echo "freq=$2"
exit 0
EOF
  chmod 755 "$J"/*
  jcase() {  # the states, one a line; then the adapter's own session, the look heard the hotspot on 6
    park; st joining 0 "seen=6 12"; last_ch 11; rm -rf "$JS"; mkdir -p "$JS"
    [ $# = 0 ] || printf '%s\n' "$@" > "$JS/states"
  }
  pjoin() {  # ap_follow as pfol 0 40, the stand-in on its PATH: "ch _hrun"
    env RADIO_AP_CHANNEL=0 HEAD_START=40 RADIO_AP_UP_FUNCS=1 AP_BOXLOG="$F/tmp/box.log" JS="$JS" \
      OCBM_TEST_PATH="$J${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}" \
      "$sh" -c '. "$1"; ch=36; ap_follow "$2" > "$2.out" 2>&1; echo "$ch $_hrun"' sh "$AP" "$F"
  }
  asked() { grep -c "^$1 sta0$" "$JS/asked" 2>/dev/null; }
  held() { [ -s "$F/tmp/.radio_ap_start" ] && [ "$(asked disconnect)" = 1 ]; }
  jcase SCANNING; r=$(pjoin)
  check '[ "$r" = "6 $SC" ] && held && logged "channel 6, heard there by the station.s look (waited 0 ms for the station, held still while hostapd starts)"' \
    "no join under way: no wait, and the station held still for hostapd's start (DISCONNECT, the keeper's mark)"
  check '[ "$(asked status)" = 1 ]' "  one STATUS asked"
  jcase AUTHENTICATING AUTHENTICATING AUTHENTICATING SCANNING; r=$(pjoin)
  check '[ "$r" = "6 $SC" ] && held && logged "(waited 0 ms for the station, then 600 ms for its join, held still while hostapd starts)"' \
    "a join under way (0.6 s): waited for, then the hold"
  jcase AUTHENTICATING ASSOCIATING "COMPLETED 2462"; r=$(pjoin)
  check '[ "$r" = "11 $SC" ] && [ ! -e "$F/tmp/.radio_ap_start" ] && [ "$(asked disconnect)" = 0 ] && logged "channel 11, the station joined the hotspot there (waited 0 ms for the station, then 400 ms for its join)"' \
    "the join completes (on 11): the AP takes its channel, nothing held"
  jcase AUTHENTICATING; echo 1 > "$F/tmp/bench_sta_join_wait"; t0=$(date +%s); r=$(pjoin)
  check '[ "$r" = "6 $SC" ] && [ "$(asked disconnect)" = 0 ] && [ ! -e "$F/tmp/.radio_ap_start" ] && logged "then 1000 ms for its join, a join still under way: the AP starts beside it)" && [ $(( $(date +%s) - t0 )) -le 3 ]' \
    "  the bench's sta_join_wait 1: a join still under way after 1 s, the AP starts beside it (never a DISCONNECT over it)"
  jcase AUTHENTICATING; echo off > "$F/tmp/bench_sta_join_wait"; r=$(pjoin)
  check '[ "$(asked status)" = 1 ] && [ "$(asked disconnect)" = 0 ] && logged "(waited 0 ms for the station, a join still under way: the AP starts beside it)"' \
    "  sta_join_wait off: no wait (as before ADDENDUM 13)"
  jcase SCANNING SCANNING SCANNING AUTHENTICATING "COMPLETED 2437"; : > "$F/tmp/bench_sta_join_first"; r=$(pjoin)
  check '[ "$r" = "6 $SC" ] && [ "$(asked disconnect)" = 0 ] && logged "channel 6, the station joined the hotspot there (waited 0 ms for the station, then 800 ms for its join)"' \
    "the bench's sta_join_first: waited through the attempts' gaps until the station joined"
  jcase SCANNING; echo 1 > "$F/etc/ocbm/bench_sta_join_first"
  t0=$(date +%s); r=$(pjoin)
  check 'held && logged "then 1000 ms for its join, held still while hostapd starts)" && [ $(( $(date +%s) - t0 )) -le 3 ]' \
    "  sta_join_first 1 (on flash, for a cold boot), never joined: 1 s, then the hold"
  jcase SCANNING; echo off > "$F/tmp/bench_sta_ap_hold"; r=$(pjoin)
  check '[ "$(asked disconnect)" = 0 ] && [ ! -e "$F/tmp/.radio_ap_start" ] && logged "(waited 0 ms for the station)"' \
    "the bench's sta_ap_hold off: nothing held"
  jcase; r=$(pjoin)
  check '[ "$r" = "6 $SC" ] && [ ! -e "$F/tmp/.radio_ap_start" ] && [ "$(asked disconnect)" = 0 ] && logged "(waited 0 ms for the station)"' \
    "no supplicant answering: no wait, nothing held"
  jcase SCANNING; st joining 0 "seen=0 12"; r=$(pjoin)
  check '[ "$r" = "11 $SC" ] && [ ! -e "$JS/asked" ]' "the look did not hear the hotspot: no STATUS asked (the keeper holds the station itself)"
  mkf; r=$(fol 149)
  check '[ "$(ls -l "$SC" | cut -c1-10)" = "-rw-------" ] && [ ! -e "$SC.new" ]' "the session's configuration is 0600 (the AP's passphrase is in it)"
  check 'grep -q "the AP follows the station to channel 149 (hw_mode=a, HT40+)" "$F/tmp/box.log" && grep -q "follows the station to channel 149" "$F.out"' \
    "  and said, in the box log too"
  mkf; r=$(fol 36)
  check '[ "$r" = "36 $F/etc/hostapd.conf" ] && [ ! -e "$SC" ] && [ ! -s "$F.out" ]' \
    "the station on the AP's own channel (36): hostapd.conf as before, nothing written, nothing said"
  mkf; : > "$SC"; r=$(fol)
  check '[ "$r" = "36 $F/etc/hostapd.conf" ] && [ ! -e "$SC" ] && [ ! -s "$F.out" ] && [ ! -e "$F/tmp/box.log" ]' \
    "no Wi-Fi link (no channel given): as before, nothing said; a session's configuration left by an earlier start goes"
  mkf; r=$(fol 0)
  check '[ "$r" = "36 $F/etc/hostapd.conf" ] && [ ! -e "$SC" ] && grep -q "the station is not on the hotspot - the AP keeps its own channel 36" "$F/tmp/box.log"' \
    "the station not on the hotspot (0): the AP's own channel, said"
  for fc in 52 64 100 120 144; do
    mkf; r=$(fol "$fc")
    check '[ "$r" = "36 $F/etc/hostapd.conf" ] && [ ! -e "$SC" ] && grep -q "the station is on channel $fc, a radar (DFS) channel - the AP keeps its own channel 36" "$F/tmp/box.log" && cmp -s "$F/etc/hostapd.conf" "$F.ref"' \
      "the station on radar channel $fc: no AP there, the AP's own channel (36), and why, said"
  done
  mkf; r=$(fol 169)
  check '[ "$r" = "36 $F/etc/hostapd.conf" ] && [ ! -e "$SC" ] && grep -q "on channel 169, not one an AP may use - the AP keeps its own channel 36" "$F/tmp/box.log"' \
    "a channel outside the table (169): the AP's own, said"
  mkf; printf 'ieee80211ac=1\nvht_oper_chwidth=1\nvht_oper_centr_freq_seg0_idx=42\n' >> "$F/etc/hostapd.conf"; r=$(fol 157)
  check 'grep -qx "ieee80211ac=1" "$SC" && ! grep -q "^vht_oper_c" "$SC" && grep -q "^vht_oper_c" "$F/etc/hostapd.conf"' \
    "a VHT centre for 36's block is not carried to 157 (the flash file keeps it)"

  # The whole start, its commands stood in for: hostapd from the session's configuration, or, refused there, its own.
  R="$T/ap/$TAG"; adapter "$R"; mkdir -p "$R/sys/class/net/sta0/wireless" "$R/proc"
  standins_ended; ended; rm -f "$S"/if.* "$S/udhcpd.exit" "$S/ps.extra" "$S/hostapd.confs" "$S/refuse"
  start_ap "$sh" PREP; cp "$R/etc/hostapd.conf" "$R.flash"; : > "$S/hostapd.confs"
  start_ap "$sh"
  check '[ "$(cat "$R.rc")" = 0 ] && [ "$(cat "$S/hostapd.confs")" = "$R/etc/hostapd.conf" ] && [ ! -e "$R/tmp/hostapd_session.conf" ] && ! grep -q "STA mode" "$R.out" "$R/tmp/box.log"' \
    "no Wi-Fi link: hostapd from hostapd.conf, as before, and not a word of STA mode"
  ended; : > "$S/hostapd.confs"
  FOLLOW=149; start_ap "$sh"; unset FOLLOW
  check '[ "$(cat "$R.rc")" = 0 ] && grep -q "AP up: ssid=[^ ]* ip=192.168.43.1 ch=149 if=sta0 dhcp=yes" "$R.out" && [ "$(cat "$S/hostapd.confs")" = "$R/tmp/hostapd_session.conf" ]' \
    "the station on 149: hostapd from the session's configuration, the AP up on 149"
  check 'cmp -s "$R/etc/hostapd.conf" "$R.flash" && grep -qx "channel=36" "$R/etc/hostapd.conf" && grep -qx "ht_capab=\[HT40+\]" "$R/tmp/hostapd_session.conf"' \
    "  hostapd.conf on flash as it was (36); HT40+ for 149 in the session's"
  : > "$S/hostapd.confs"; FOLLOW=44; start_ap "$sh"; unset FOLLOW
  check '[ "$(cat "$R.rc")" = 0 ] && [ ! -s "$S/hostapd.confs" ] && grep -qx "channel=149" "$R/tmp/hostapd_session.conf" && grep -q "AP up: .* ch=149 " "$R.out"' \
    "a start while that AP runs: nothing moves (no hostapd started, its configuration kept), and it says ch=149"
  ended; : > "$S/hostapd.confs"; echo 149 > "$S/refuse"
  t0=$(date +%s); FOLLOW=149; start_ap "$sh"; unset FOLLOW; t1=$(date +%s)
  check '[ "$(cat "$R.rc")" = 0 ] && grep -q "hostapd did not start on channel 149 - the AP takes its own channel 36" "$R.out" && grep -q "AP up: ssid=[^ ]* ip=192.168.43.1 ch=36 if=sta0 dhcp=yes" "$R.out"' \
    "hostapd refuses 149 (a domain with no AP there): the AP comes up on its own channel, 36, and says so"
  check '[ "$(sed -n 1p "$S/hostapd.confs")" = "$R/tmp/hostapd_session.conf" ] && [ "$(sed -n 2p "$S/hostapd.confs")" = "$R/etc/hostapd.conf" ] && [ ! -e "$R/tmp/hostapd_session.conf" ] && grep -q "hostapd did not start on channel 149" "$R/tmp/box.log"' \
    "  the session's configuration first, then hostapd.conf; the session's gone; in the box log too"
  check '[ $((t1 - t0)) -le 5 ]' "  within a few seconds, not the converge step's 12 s bound"
  ended; rm -f "$S/refuse" "$S/hostapd.confs"
  : > "$S/hostapd.confs"; FOLLOW=6; start_ap "$sh"; unset FOLLOW
  check '[ "$(cat "$R.rc")" = 0 ] && grep -q "AP up: ssid=[^ ]* ip=192.168.43.1 ch=6 if=sta0 dhcp=yes" "$R.out" && grep -qx "hw_mode=g" "$R/tmp/hostapd_session.conf" && grep -qx "ht_capab=" "$R/tmp/hostapd_session.conf"' \
    "the station on 6 (2.4 GHz, the truck's hotspot): the AP up on 6, hw_mode=g, 20 MHz"
  ended; rm -f "$S/hostapd.confs"

  # ---- mode ap: the head unit joins this AP, on the link's band (ADDENDUM 24, 27 Nov 2026) -------------------------
  echo "== $sh: mode ap: the AP on the link's band (2.4 GHz: 6, 20 MHz), never DFS, hostapd.conf untouched"
  bnd() {  # $1 RADIO_AP_BAND, $2 the AP's own channel: "ch _hrun"
    env RADIO_AP_BAND="$1" RADIO_AP_UP_FUNCS=1 AP_BOXLOG="$F/tmp/box.log" OCBM_TEST_PATH="${OCBM_TEST_PATH:-}" \
      "$sh" -c '. "$1"; ch=$3; ap_band_conf "$2" > "$2.out" 2>&1; echo "$ch $_hrun"' sh "$AP" "$F" "$2"
  }
  mkf; r=$(bnd 2.4 36)
  check '[ "$r" = "6 $SC" ] && grep -qx "channel=6" "$SC" && grep -qx "hw_mode=g" "$SC" && grep -qxF "ht_capab=[SHORT-GI-20]" "$SC" && grep -q "mode ap: 2.4 GHz, channel 6 (hw_mode=g, 20 MHz)" "$F/tmp/box.log"' \
    "2.4 GHz: channel 6, hw_mode=g, 20 MHz (no HT40, no SHORT-GI-40), said"
  check 'cmp -s "$F/etc/hostapd.conf" "$F.ref" && [ -z "$(find "$F/etc" -type f -newer "$F.mark")" ] && [ "$(others "$SC")" = "$(others "$F.ref")" ]' \
    "  hostapd.conf on flash untouched; the session's every other line as it has it, the passphrase too"
  mkf; printf 'ieee80211ac=1\nvht_oper_chwidth=1\nvht_oper_centr_freq_seg0_idx=42\n' >> "$F/etc/hostapd.conf"; r=$(bnd 2.4 36)
  check '[ "$r" = "6 $SC" ] && ! grep -q -e "^ieee80211ac=" -e "^vht_" "$SC"' "  no VHT line carried to 2.4 GHz"
  mkf; r=$(bnd 5 36)
  check '[ "$r" = "36 $F/etc/hostapd.conf" ] && [ ! -e "$SC" ] && grep -q "mode ap: 5 GHz, channel 36 (its own)" "$F/tmp/box.log"' \
    "5 GHz on the AP's own 36: hostapd.conf as today, nothing written"
  mkf; r=$(bnd 5 149)
  check '[ "$r" = "149 $F/etc/hostapd.conf" ] && [ ! -e "$SC" ]' "5 GHz on its own 149 (no radar channel): kept"
  for own in 100 52 6; do
    mkf; r=$(bnd 5 "$own")
    check '[ "$r" = "36 $SC" ] && grep -qx "channel=36" "$SC" && grep -qx "hw_mode=a" "$SC" && grep -qxF "ht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]" "$SC"' \
      "5 GHz with the AP's own on $own (a radar channel, or 2.4 GHz): 36, HT40+"
  done
  # The whole start in mode ap: hostapd from the band's configuration, the band's mark once it is up; none without one.
  ended; rm -f "$S/refuse"; : > "$S/hostapd.confs"
  BAND=2.4; start_ap "$sh"; unset BAND
  check '[ "$(cat "$R.rc")" = 0 ] && grep -q "AP up: ssid=[^ ]* ip=192.168.43.1 ch=6 if=sta0 dhcp=yes" "$R.out" && [ "$(cat "$S/hostapd.confs")" = "$R/tmp/hostapd_session.conf" ] && [ "$(cat "$R/tmp/.radio_ap_link")" = "2.4 6" ]' \
    "mode ap, 2.4 GHz: hostapd from the session's configuration on 6, and the mark \"2.4 6\" for ocbmd"
  check 'cmp -s "$R/etc/hostapd.conf" "$R.flash"' "  hostapd.conf on flash as it was"
  : > "$S/hostapd.confs"; BAND=5; start_ap "$sh"; unset BAND
  check '[ "$(cat "$R.rc")" = 0 ] && [ ! -s "$S/hostapd.confs" ] && [ "$(cat "$R/tmp/.radio_ap_link")" = "2.4 6" ]' \
    "  a start while that AP runs: nothing moves, the mark as it was (ocbmd restarts it for another band)"
  ended; : > "$S/hostapd.confs"; echo 6 > "$S/refuse"
  BAND=2.4; start_ap "$sh"; unset BAND
  check '[ "$(cat "$R.rc")" = 0 ] && grep -q "mode ap: hostapd did not start on channel 6 - the AP takes its own channel 36" "$R.out" && [ "$(cat "$R/tmp/.radio_ap_link")" = "2.4 36" ]' \
    "hostapd refuses 6: the AP's own channel, said; the mark the band asked and the channel taken (no restart loop)"
  ended; rm -f "$S/refuse"; : > "$S/hostapd.confs"
  start_ap "$sh"
  check '[ "$(cat "$R.rc")" = 0 ] && [ ! -e "$R/tmp/.radio_ap_link" ] && [ "$(cat "$S/hostapd.confs")" = "$R/etc/hostapd.conf" ]' \
    "no band (no mode ap): hostapd.conf as before, and no mark"
  ended; rm -f "$S/hostapd.confs"

  # do_wifi_ap_off itself, its commands stood in for (the second review, item 5): the AP's server seen gone before it
  # returns, so that a teardown which killed and went on (2.6.1's) fails here.
  echo "== $sh: do_wifi_ap_off: the AP's DHCP server gone before it returns"
  D="$T/off"; rm -rf "$D"; mkdir -p "$D"; printf 'RADIO_WIRELESS=yes\nRADIO_BACKEND=mapped\n' > "$D/caps"
  standins_ended
  slow=$(standin slow "$T/$TAG/etc/udhcpd.conf"); ncm=$(standin slow "$T/$TAG/tmp/udhcpd_ncm.conf")
  STANDINS="$STANDINS $slow $ncm"; sleep 0.3
  env OCBM_TEST_PATH="$PSPATH" AP_DHCP_MATCH="$MATCH" RADIO_HAL_FUNCS=1 "$sh" -c '. "$1"
    _t_d=$2; CAPS="$_t_d/caps"; WLAN_STATE="$_t_d/wlan_state"
    killall() { echo "killall $*" >> "$_t_d/calls"; }
    ifconfig() { echo "ifconfig $*" >> "$_t_d/calls"; }
    wlan_iface() { echo sta0; }
    ap_running() { return 1; }
    do_wifi_ap_off; echo "rc=$?"' sh "$HAL" "$D" > "$D.out" 2>&1
  check 'grep -qx "rc=0" "$D.out" && gone "$slow" && ! grep -q SIGKILL "$D.out" && grep -q "wifi AP down" "$D.out"' \
    "a server a second over its lease file: gone when do_wifi_ap_off returns, waited for, not killed"
  check '! gone "$ncm" && grep -qx "killall hostapd" "$D/calls" && grep -qx "ifconfig sta0 down" "$D/calls"' \
    "the NCM server untouched; hostapd stopped and the interface down, as before"
  kill -9 "$ncm" 2>/dev/null

  # hostapd's AP-ENABLED: one after this start's own line in the box log (the second review of resident-bt, item 3). A
  # box log ocbmd emptied at its cap took that line with it, and every AP-ENABLED in it then is this hostapd's.
  echo "== $sh: hostapd's AP-ENABLED, after the start's own line, through a box log emptied at its cap"
  B="$T/box.log"; M="[radio] hostapd starting on sta0"
  seen() {  # "seen" or "waiting", the start's own line $1 (M when not given)
    env AP_BOXLOG="$B" RADIO_AP_UP_FUNCS=1 _ae_mark="${1-$M}" "$sh" -c '. "$1"; if ap_enabled_seen; then echo seen; else echo waiting; fi' sh "$AP"
  }
  printf '%s\nsta0: AP-ENABLED \n%s\nx\nsta0: AP-ENABLED \n%s\ny\n' "$M" "$M" "$M" > "$B"
  check '[ "$(seen)" = waiting ]' "two earlier starts and their AP-ENABLED, then this start's line: not yet"
  echo "sta0: AP-ENABLED " >> "$B"
  check '[ "$(seen)" = seen ]' "one after this start's own line: this hostapd's"
  printf 'x\nsta0: AP-ENABLED \n%s\n' "$M" > "$B"
  check '[ "$(seen)" = waiting ]' "one AP-ENABLED before the start's line (a count's baseline of 1): not yet"
  : > "$B"
  check '[ "$(seen)" = waiting ]' "the log emptied at its cap: waiting"
  echo "sta0: AP-ENABLED " >> "$B"
  check '[ "$(seen)" = seen ]' "emptied, then this hostapd's AP-ENABLED, the count back at that baseline: seen (the count missed it)"
  printf '[log] rotated 262200 bytes (not streamed)\nsta0: AP-ENABLED \n' > "$B"
  check '[ "$(seen)" = seen ]' "emptied by the idle rotation, its own line first: seen"
  rm -f "$B"
  check '[ "$(seen)" = waiting ]' "no box log at all: waiting"
  printf 'sta0: AP-ENABLED \n' > "$B"
  check '[ "$(seen "")" = waiting ]' "no line of its own (its write failed): never seen, the wait runs to its bound"
done
[ "$fails" -eq 0 ] && echo "radio_ap_up_test: all passed" || { echo "radio_ap_up_test: $fails failed"; exit 1; }
