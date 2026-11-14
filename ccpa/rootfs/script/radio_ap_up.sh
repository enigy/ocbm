#!/bin/sh
########################
# radio_ap_up.sh - the OWNED SoftAP layer. Chipset-neutral by construction: it loads no driver,
# names no module, reads no firmware tree. It configures hostapd + udhcpd on whatever interface
# the seam hands it, and returns only once the AP is actually up.
#
# WHY THIS FILE EXISTS UNDER THIS NAME. Every CCPA already ships a VENDOR file at
# /script/start_bluetooth_wifi.sh, so testing that path for existence proves the file is there,
# not that it is ours - and running the vendor copy on a stripped box is destructive:
#
#   * it reads config through `riddleBoxCfg`, which the OCBM base install removes, and the calls
#     are NOT existence-guarded. With it gone `passwd` comes back empty and the script seds an
#     EMPTY wpa_passphrase into /etc/hostapd.conf. That is persistent, on flash, and hostapd
#     refuses to start with a zero-length WPA passphrase - so the AP is broken for every future
#     bring-up, not just this one.
#   * its WLANIP default is 192.168.50.2 - the box's OWN NCM address - and it then repoints the
#     DHCP pool at 192.168.50.100-200, which is the pool the NCM link is handing out. On a unit
#     with no UART that is the control channel you are standing on.
#   * its teardown (`close_bluetooth_wifi.sh`) does `EnsureProcessKill udhcpd`, a killall that
#     takes the NCM DHCP server down with it.
#
# An unambiguous name is the guard. radio_hal.sh calls THIS path and explicitly refuses to fall
# back to the vendor one. Do not "simplify" that by pointing the seam at start_bluetooth_wifi.sh.
#
# WHAT IS CHIPSET-NEUTRAL HERE. The interface name arrives in RADIO_WLAN_IF (radio_hal.sh
# enumerates it - it is an insmod parameter, not a constant: Realtek if2name=sta0, Broadcom
# iface_name=sta, and on Broadcom wlan0 does not exist until something creates it on top of
# sta0). The device-unique SSID is derived from that interface's own MAC in sysfs rather than
# from the vendor's `set_wifi_mac` output, so no vendor helper is required.
#
# USAGE  RADIO_WLAN_IF=<iface> sh radio_ap_up.sh [AP]
# EXIT   0 AP converged   1 failed to converge
########################
set -u
export PATH=${OCBM_TEST_PATH:+$OCBM_TEST_PATH:}/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
L="[radio_ap_up]"

IF=${RADIO_WLAN_IF:-wlan0}
[ -e "/sys/class/net/$IF" ] || { echo "$L interface $IF does not exist - the driver must be up first"; exit 1; }
# AND IT MUST ACTUALLY BE A WIRELESS INTERFACE. Existence alone is not enough: this script
# unconditionally reconfigures whatever it is handed, so `RADIO_WLAN_IF=ncm0` would sail through
# the check above and then `ifconfig ncm0 192.168.43.1` - re-addressing the NCM management link
# out from under the only way into a unit with no UART, before hostapd ever gets a chance to
# fail. The seam always passes an enumerated wireless interface, but "the caller is careful" is
# exactly the guarantee this file refuses to accept from anyone else, so it does not accept it
# for itself either. A cfg80211/wext netdev is identifiable without knowing the driver.
if [ ! -e "/sys/class/net/$IF/wireless" ] && [ ! -e "/sys/class/net/$IF/phy80211" ]; then
  echo "$L $IF is not a wireless interface - refusing to reconfigure it"; exit 1
fi

# Single-flight. NOT the original's `[ -f lock ] && exit 0`: a crash between touch and rm left a
# stale file that suppressed every later bring-up until reboot. mkdir is atomic, and a lock older
# than 120s is treated as abandoned.
# Same discipline as radio_hal.sh's lock, and for the same reasons. Reclaim on the holder being
# GONE rather than on age; check the reclaim mkdir actually succeeded, because two contenders can
# both observe one stale lock and the loser would otherwise proceed unlocked and then delete the
# winner's lock on its way out; and make the signal trap EXIT, because in ash a trapped signal
# runs the handler and then RESUMES - releasing the lock while the bring-up carries on is worse
# than never having taken it.
LOCK=/tmp/.radio_ap_lock
if ! mkdir "$LOCK" 2>/dev/null; then
  _holder=$(cat "$LOCK/pid" 2>/dev/null)
  if { [ -n "$_holder" ] && [ ! -d "/proc/$_holder" ]; } \
     || { [ -z "$_holder" ] && [ -n "$(find "$LOCK" -maxdepth 0 -mmin +20 2>/dev/null)" ]; }; then
    echo "$L lock held by ${_holder:-no live process} - reclaiming"
    rm -rf "$LOCK"
    mkdir "$LOCK" 2>/dev/null || { echo "$L lost the reclaim race - refusing to stack"; exit 1; }
  else
    echo "$L another AP bring-up is in flight (pid ${_holder:-unknown}) - refusing to stack"; exit 1
  fi
fi
echo $$ > "$LOCK/pid" 2>/dev/null
trap 'rm -rf "$LOCK"' EXIT
trap 'rm -rf "$LOCK"; trap - EXIT; exit 143' INT TERM HUP

[ -f /etc/hostapd.conf ] || { echo "$L /etc/hostapd.conf missing"; exit 1; }

# ---- device-unique name ---------------------------------------------------------------------
# The box's one name, the one the iPhone lists on Bluetooth (btd's CarLink-<last 4 hex of wlan0's
# MAC>, else of the serial; radio_hal.sh box_name, which says why it is decided once and kept in
# /etc/carplay_ident). The seam resolves it and passes it in, so the SSID and the Bluetooth name come
# from ONE decision. The derivation below is only for a direct/bench invocation, and is box_name's.
#
# This overwrites ssid= in /etc/hostapd.conf, which is also what the box hands the phone in the
# iAP2 0x5703 handoff, so the two always name the same AP.
NAME="${RADIO_BOX_NAME:-}"
if [ -z "$NAME" ] && [ -s /etc/carplay_ident ]; then
  NAME=$(head -1 /etc/carplay_ident)
  case "$NAME" in CarLink-*) ;; *) NAME="" ;; esac   # an older OCBM's ccpa-<4 hex> is derived again
fi
if [ -z "$NAME" ]; then
  SUF=$(cat /sys/class/net/wlan0/address 2>/dev/null | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p')
  [ -n "$SUF" ] || SUF=$(sed -n 's/^wlan0=//p' /etc/ocbm/addresses.txt 2>/dev/null | head -1 | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p')
  if [ -n "$SUF" ]; then NAME="CarLink-$SUF"; echo "$NAME" > /etc/carplay_ident 2>/dev/null
  else
    SUF=$(cat /etc/serial_number 2>/dev/null | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p')
    NAME="CarLink${SUF:+-$SUF}"
  fi
fi
# The AP interface's own suffix, for the passphrase check below: an older fallback key was "carplay" and it.
SUF=$(cat "/sys/class/net/$IF/address" 2>/dev/null | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p')
sed -i "s/^ssid=.*/ssid=$NAME/" /etc/hostapd.conf
echo "$NAME" > /etc/wifi_name

# ---- passphrase: set only if MISSING, never blanked ------------------------------------------
# The vendor bug this guards against is unconditional rewriting from a config oracle that no
# longer exists. We only ever fill in an absent or too-short key, and never overwrite a good one.
# A well-known default counts as missing (security review C1, 2026-10-21): Carlinkit ships
# 12345678, and the old fallback below was derived from the AP's own MAC, which the AP broadcasts.
# The replacement is random and persisted once; the phone is handed it in the iAP2 0x5703 handoff
# at every connect, so nobody ever types it.
_known_psk() { case "$1" in 12345678|"carplay$SUF") return 0 ;; esac; return 1; }
PSK=$(sed -n 's/^wpa_passphrase=//p' /etc/hostapd.conf | head -1)
_known_psk "$PSK" && PSK=""
if [ "${#PSK}" -lt 8 ]; then
  # OCBM's own from before a go-back to stock first (it puts stock's 12345678 back): each phone knows it, and a new one
  # cost each phone about 6 s of PSK-MISMATCH retries at its first connect after the reinstall (the Mac, 13 Nov).
  # Which source it came from is logged, never the passphrase (the Mac's review, PSK-SOURCE-UNLOGGED).
  _psk_src="the one OCBM kept (/etc/ocbm/wifi_psk)"
  PSK=$(cat /etc/ocbm/wifi_psk 2>/dev/null | tr -cd '[:print:]' | head -c 63)
  _known_psk "$PSK" && PSK=""
  if [ "${#PSK}" -lt 8 ]; then
    _psk_src="/etc/wifi_password"
    PSK=$(cat /etc/wifi_password 2>/dev/null | tr -cd '[:print:]' | head -c 63)
    _known_psk "$PSK" && PSK=""
  fi
  if [ "${#PSK}" -lt 8 ]; then
    _psk_src="a new random one"
    PSK=$(head -c 64 /dev/urandom | md5sum | cut -c1-20)
  fi
  if grep -q '^wpa_passphrase=' /etc/hostapd.conf; then
    sed -i "s/^wpa_passphrase=.*/wpa_passphrase=$PSK/" /etc/hostapd.conf
  else
    echo "wpa_passphrase=$PSK" >> /etc/hostapd.conf
  fi
  echo "$L wpa_passphrase was absent, too short or a known default - set $_psk_src"
fi
# Kept where a go-back leaves it (/etc/ocbm stays on flash; ocbm_revert.sh), for the install after it. Not logged.
if [ -d /etc/ocbm ] && [ "$(cat /etc/ocbm/wifi_psk 2>/dev/null)" != "$PSK" ]; then
  ( umask 077; echo "$PSK" > /etc/ocbm/wifi_psk.new && mv -f /etc/ocbm/wifi_psk.new /etc/ocbm/wifi_psk ) 2>/dev/null
fi

# ---- channel + band ---------------------------------------------------------------------------
ch=$(sed -n 's/^channel=//p' /etc/hostapd.conf | head -1)
[ -e /etc/wifi_use_24G ] && ch=6
case "$ch" in ''|*[!0-9]*) ch=36 ;; esac
# hostapd of this vintage has no ACS and refuses to start with no channel, so it is always pinned.
if [ "$ch" -ge 1 ] && [ "$ch" -le 14 ]; then
  grep -q "^hw_mode=a" /etc/hostapd.conf && sed -i "s/^hw_mode=a/hw_mode=g/" /etc/hostapd.conf
else
  grep -q "^hw_mode=g" /etc/hostapd.conf && sed -i "s/^hw_mode=g/hw_mode=a/" /etc/hostapd.conf
fi
sed -i "s/^channel=.*/channel=${ch}/" /etc/hostapd.conf
# The AP must serve the interface we were handed, whatever it is called.
if grep -q '^interface=' /etc/hostapd.conf; then
  sed -i "s/^interface=.*/interface=$IF/" /etc/hostapd.conf
else
  sed -i "1i interface=$IF" /etc/hostapd.conf
fi

# ---- AP address -------------------------------------------------------------------------------
# HARD RULE: never 192.168.50.x. That is the NCM management subnet; colliding with it takes out
# the only way into a unit that has no UART. An override that lands there is refused, not obeyed.
# CANONICALISE BEFORE COMPARING. The first version tested `case $_w in 192.168.50.*) refuse`
# against an accept pattern of `[0-9]*.[0-9]*.[0-9]*.[0-9]*`. In shell globbing `[0-9]*` is "one
# digit followed by ANYTHING", so that accept pattern admits almost anything - and, worse, the
# refusal is a literal string match while the kernel parses the address numerically. inet_aton()
# reads a leading-zero octet as OCTAL and an 0x octet as HEX, so `192.168.062.1` and
# `192.168.0x32.1` both sail past the refusal and both resolve to 192.168.50.1 - the NCM
# management subnet, on a unit that may have no other way in. Comparing the text was never going
# to be sound; parse it into canonical octets first and compare THAT.
_ip_canon() {  # echoes the canonical dotted quad, or nothing if it is not a plain IPv4 literal
  case "$1" in *.*.*.*) ;; *) return 1 ;; esac
  IFS=. read -r _a _b _c _d <<EOF
$1
EOF
  for _o in "$_a" "$_b" "$_c" "$_d"; do
    case "$_o" in
      ''|*[!0-9]*) return 1 ;;          # empty, hex, octal-with-x, or trailing junk
      0|[1-9]|[1-9][0-9]|[1-9][0-9][0-9]) ;;   # no leading zeros => no octal reinterpretation
      *) return 1 ;;
    esac
    [ "$_o" -le 255 ] 2>/dev/null || return 1
  done
  echo "$_a.$_b.$_c.$_d"
}

WLANIP=192.168.43.1
if [ -e /etc/wifi_ip ]; then
  _w=$(_ip_canon "$(cat /etc/wifi_ip 2>/dev/null)")
  if [ -z "$_w" ]; then
    echo "$L /etc/wifi_ip is not a plain IPv4 address - ignoring it"
  else
    case "$_w" in
      192.168.50.*) echo "$L /etc/wifi_ip resolves to $_w - that is NCM's subnet; ignoring it" ;;
      *) WLANIP=$_w ;;
    esac
  fi
fi
base=${WLANIP%.*}
sed -i "s/192\.168\.[0-9]*\.100/${base}.100/; s/192\.168\.[0-9]*\.200/${base}.200/" /etc/udhcpd.conf 2>/dev/null
# Point udhcpd at the interface we are actually serving.
grep -q '^interface' /etc/udhcpd.conf 2>/dev/null && sed -i "s/^interface.*/interface	$IF/" /etc/udhcpd.conf
# THE AP'S DHCP SERVER IS BUSYBOX'S udhcpd (1.32.1, the one serving NCM), NOT THE VENDOR'S v0.9.9-pre.
# An iPhone rejoining a network it knows keeps its old address and asks for it again (INIT-REBOOT). The
# vendor's server stays silent on such a request from a client it has no lease for (upstream udhcp:
# "else remain silent"), so the phone ran CarPlay on its old address for ~5 s, then DISCOVERed, and the
# session died when it was handed another. Keeping the lease file (bench 2026-11-02) covered AP restarts
# and power cuts, but not the first session after a conversion, or after a Restore and reinstall, where
# no lease exists (the Mac, 10 Nov: the LIVI bench at 16:58 got a new address and dropped). BusyBox's
# udhcpd NAKs a REQUEST it has no lease for ("No, we don't have this IP for you"), and the phone takes
# a fresh address within a second, before CarPlay starts.
#
# Same file, same pool, lease and interface as before. Three keys BusyBox's server needs, set if absent:
# the lease file (on the jffs2 root, kept across AP restarts and power cuts) and the pidfile the
# teardown paths kill by, which the stock file names already, and auto_time. BusyBox writes leases on
# that timer and at exit, where the vendor's wrote them at every ACK, and a drive ends in a power cut.
# Its lease file has another format (a timestamp, then 36-byte records); a vendor-format file reads as
# a timestamp from far outside its 12-hour window, so BusyBox ignores it and rewrites it in its own. A
# file older than that window, or one the clock jumped past at boot, is ignored too: the NAK covers it.
_dhcp_key() {
  if grep -q "^[[:space:]]*$1[[:space:]]" /etc/udhcpd.conf 2>/dev/null; then
    sed -i "s|^[[:space:]]*$1[[:space:]].*|$1	$2|" /etc/udhcpd.conf
  else
    printf '%s\t%s\n' "$1" "$2" >> /etc/udhcpd.conf
  fi
}
_dhcp_key lease_file /var/lib/udhcpd.leases
_dhcp_key pidfile /var/run/udhcpd.pid
_dhcp_key auto_time 60

echo 1 > "/proc/sys/net/ipv6/conf/$IF/disable_ipv6" 2>/dev/null
ifconfig "$IF" "$WLANIP" netmask 255.255.255.0 mtu 1500 up || {
  echo "$L could not configure $IF"; exit 1; }

# ---- bring up ---------------------------------------------------------------------------------
test -e /tmp/bin/hostapd || cp /usr/sbin/hostapd /tmp/bin/hostapd 2>/dev/null
# Its output folds straight into the universal log. Not -B: daemonizing points stdout at /dev/null, so
# the station events after start-up (AP-STA-CONNECTED, AP-STA-DISCONNECTED, and the IEEE 802.11 lines
# that say why a phone left) never reached box.log, and the Mac's 12 Nov re-bench couldn't see why a
# fresh session's link was lost. It runs in the foreground under setsid instead, as udhcpd does below,
# appending (O_APPEND, for bound_logs' in-place truncate); the converge step waits for it.
echo "[radio] hostapd starting on $IF" >> /tmp/box.log
ps | grep -v grep | grep -qw hostapd || { setsid hostapd /etc/hostapd.conf </dev/null >>/tmp/box.log 2>&1 & }

# The AP's DHCP server, explicitly scoped. The stock script masked this behind a global udhcpd
# check, so the always-running NCM instance satisfied it and the AP's own server never started -
# the phone then associates and never gets an address. Match on the CONFIG PATH, not the process
# name: the NCM server is `busybox udhcpd -f /tmp/udhcpd_ncm.conf` and must never be mistaken for
# this one, nor killed with it.
#
# Redirect and detach. It runs in the foreground (-f, as NCM's does) under setsid, so left alone it
# would inherit the caller's stdout and hold the pipe open - which makes an ssh invocation of the seam
# hang long after the script itself has finished, and would break the HAL's convergent-on-return
# contract for any caller that reads our output. `busybox udhcpd -f /etc/udhcpd.conf` still matches the
# CONFIG-PATH patterns here and in radio_hal.sh, and it writes /var/run/udhcpd.pid for wlan_off.sh.
if ! ps | grep -v grep | grep -q "udhcpd .*etc/udhcpd.conf"; then
  # radio_ap_dhcp.log stays a small `>`-truncated status file: this udhcpd is a setsid-backgrounded
  # foreground process, so piping its own stdout to tee would change its lifecycle. Only the readback
  # below (on failure) is universal-logged.
  # -a 250: the ARP probe before each offer waits 250 ms, not the default 2 s. It cost 1-1.9 s whenever a phone had
  # no lease (the Mac's 13 Nov brief, item 10); a short probe still catches an address in use. BusyBox 1.32.1's udhcpd
  # takes -a (the adapter's, checked on the Mac's bench); /usr/sbin/udhcpd is the vendor's v0.9.9-pre and is not this.
  setsid busybox udhcpd -f -a 250 /etc/udhcpd.conf </dev/null >/tmp/radio_ap_dhcp.log 2>&1 &
fi

# ---- converge ---------------------------------------------------------------------------------
# Return only when the AP is really serving: the seam's contract is that a 0 means up, so a
# caller may start its advertiser immediately afterwards.
# DHCP IS PART OF CONVERGENCE, NOT A FOOTNOTE. The first version gated only on hostapd and the
# address, and merely COUNTED udhcpd in the closing log line. A DHCP server that died on a config
# error therefore still produced "AP up" and exit 0 - and the caller, trusting that, would start
# its advertiser. The phone then associates and never gets an address, which is verbatim the
# stock-script failure the udhcpd block above exists to fix. Anything the AP needs in order to
# actually serve a client belongs in the predicate.
ap_serving() {
  ps | grep -v grep | grep -qw hostapd || return 1
  ifconfig "$IF" 2>/dev/null | grep -q "inet addr:$WLANIP" || return 1
  ps | grep -v grep | grep -q "udhcpd .*etc/udhcpd.conf" || return 1
  return 0
}
n=0
while [ "$n" -lt 50 ]; do ap_serving && break; n=$((n+1)); sleep 0.2; done
# Re-test the FULL predicate after the loop: falling out of it on the bound is not success, and
# re-checking only hostapd here would let a missing address or a dead DHCP server report as up.
# A second on: in the foreground hostapd is in the process list before its start-up is through,
# where -B returned only after it, so one that refuses its config would otherwise pass the loop.
sleep 1
if ! ap_serving; then
  echo "$L AP did not converge (ssid=$NAME ch=$ch if=$IF):"
  ps | grep -v grep | grep -qw hostapd \
    || echo "$L   hostapd is not running"
  ifconfig "$IF" 2>/dev/null | grep -q "inet addr:$WLANIP" \
    || echo "$L   $IF does not hold $WLANIP"
  ps | grep -v grep | grep -q "udhcpd .*etc/udhcpd.conf" \
    || { echo "$L   the AP DHCP server is not running - a phone would associate and get no address"
         tail -3 /tmp/radio_ap_dhcp.log 2>/dev/null | sed "s|^|$L     |" | tee -a /tmp/box.log; }
  exit 1
fi
echo "$L AP up: ssid=$NAME ip=$WLANIP ch=$ch if=$IF dhcp=yes"
exit 0
