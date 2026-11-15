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
done
[ "$fails" -eq 0 ] && echo "radio_ap_up_test: all passed" || { echo "radio_ap_up_test: $fails failed"; exit 1; }
