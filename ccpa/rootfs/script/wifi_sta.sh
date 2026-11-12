#!/bin/sh
########################
# wifi_sta.sh - the Wi-Fi transport's STA side: keep $STA_IF joined to the car's hotspot so the host
# can reach ocbmd over it (ccpa/ocbmd/src/wifi.rs). Started by ocbm_boot.sh when /etc/ocbm/wifi_link
# has an ssid= line; one instance; runs for the life of the box.
#
# WHY A KEEPER OF ITS OWN, NOT THE SUPERVISOR. The supervisor raises and drops the session's AP on
# host-presence edges. Over Wi-Fi the host can only arrive once this link is up, so the link has to
# exist before any session and outlive every one - including the teardown after a host leaves, when
# the next host needs it to come back. radio_hal.sh's STA mode keeps the session's AP off $STA_IF.
#
# /etc/ocbm/wifi_link (written over USB at provisioning, 0600):
#   key=<64 hex>    the link key (ocbmd)
#   ssid=<name>     the hotspot
#   psk=<64 hex>    its WPA key, already hashed (PBKDF2-SHA1(passphrase, ssid, 4096, 32)); a non-hex
#                   value is taken as the passphrase itself. The plaintext need never be stored.
#
# wpa_supplicant and wpa_cli come from the image (/usr/sbin) or a bench copy (/tmp/bin).
########################
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
L="[wifi_sta]"
CONF=/etc/ocbm/wifi_link
STA_IF=${RADIO_STA_IF:-sta0}
WPACONF=/tmp/wpa_sta.conf
CTRL=/var/run/wpa_supplicant
DHCP_PID=/var/run/udhcpc.$STA_IF.pid
LOG=/tmp/wifi_sta.log
# The udhcpc hook; overridable so a bench copy can run from /tmp.
DHCP_HOOK=${WIFI_STA_DHCP_HOOK:-/script/wifi_sta_dhcp.sh}

log() { echo "$L $*" >> /tmp/box.log; }

# Single instance: reclaim a lock whose holder is gone (radio_ap_up.sh's discipline).
LOCK=/tmp/.wifi_sta_lock
if ! mkdir "$LOCK" 2>/dev/null; then
  _holder=$(cat "$LOCK/pid" 2>/dev/null)
  if [ -n "$_holder" ] && [ -d "/proc/$_holder" ]; then echo "$L already running (pid $_holder)"; exit 0; fi
  rm -rf "$LOCK"; mkdir "$LOCK" 2>/dev/null || exit 1
fi
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT
trap 'rm -rf "$LOCK"; trap - EXIT; exit 143' INT TERM HUP

tool() { for _d in /usr/sbin /tmp/bin; do [ -x "$_d/$1" ] && { echo "$_d/$1"; return 0; }; done; return 1; }
WPA=$(tool wpa_supplicant) || { log "no wpa_supplicant in /usr/sbin or /tmp/bin - STA side off"; exit 1; }
CLI=$(tool wpa_cli) || { log "no wpa_cli - STA side off"; exit 1; }

ssid=$(sed -n 's/^ssid=//p' "$CONF" 2>/dev/null | head -1)
psk=$(sed -n 's/^psk=//p' "$CONF" 2>/dev/null | head -1)
[ -n "$ssid" ] || { log "no ssid= in $CONF - STA side off"; exit 0; }

# The SSID goes in as hex, so quotes or spaces in a hotspot's name can't break the config.
# hexdump, not od: the box's BusyBox od takes neither -A nor -t.
ssid_hex=$(printf '%s' "$ssid" | hexdump -v -e '/1 "%02x"')
case "$psk" in
  [0-9a-fA-F]*) [ "${#psk}" -eq 64 ] && [ -z "$(printf '%s' "$psk" | tr -d '0-9a-fA-F')" ] && psk_line="psk=$psk" || psk_line="psk=\"$psk\"" ;;
  "") psk_line="key_mgmt=NONE" ;;
  *) psk_line="psk=\"$psk\"" ;;
esac
umask 077
{
  echo "ctrl_interface=$CTRL"
  echo "update_config=0"
  echo "network={"
  echo "  ssid=$ssid_hex"
  echo "  $psk_line"
  echo "  scan_ssid=1"
  echo "}"
} > "$WPACONF"
umask 022
mkdir -p "$CTRL"
log "keeping $STA_IF on \"$ssid\""

wpa_up() { ps | grep -v grep | grep -q "wpa_supplicant .*-i $STA_IF"; }
dhcp_up() { [ -f "$DHCP_PID" ] && kill -0 "$(cat "$DHCP_PID" 2>/dev/null)" 2>/dev/null; }
sta_ip() { ifconfig "$STA_IF" 2>/dev/null | sed -n 's/.*inet addr:\([0-9.]*\).*/\1/p'; }

bad=0
last_state=""
while :; do
  if [ ! -e "/sys/class/net/$STA_IF" ]; then
    sh /script/radio_hal.sh wifi_sta_up >> /tmp/box.log 2>&1
    sleep 2; continue
  fi
  if ! wpa_up; then
    log "starting wpa_supplicant on $STA_IF"
    setsid "$WPA" -D nl80211,wext -i "$STA_IF" -c "$WPACONF" </dev/null >> "$LOG" 2>&1 &
    sleep 2
  fi
  state=$("$CLI" -i "$STA_IF" status 2>/dev/null | sed -n 's/^wpa_state=//p')
  [ "$state" != "$last_state" ] && { log "$STA_IF: ${state:-no answer}"; last_state=$state; }
  if [ "$state" = COMPLETED ]; then
    bad=0
    if ! dhcp_up; then
      # -b: keep running and renewing in the background. Our own hook: address and route only, no
      # /etc/resolv.conf rewrite on flash at every renewal.
      udhcpc -i "$STA_IF" -b -p "$DHCP_PID" -s "$DHCP_HOOK" >> "$LOG" 2>&1
    fi
  else
    bad=$((bad+1))
    # Two minutes without an association: start over (a hotspot that came back on another channel
    # or after a restart can leave the supplicant in a state it doesn't leave by itself).
    if [ "$bad" -ge 24 ]; then
      log "$STA_IF not associated for 2 min - restarting wpa_supplicant"
      for _p in $(ps | grep -v grep | grep "wpa_supplicant .*-i $STA_IF" | awk '{print $1}'); do kill "$_p" 2>/dev/null; done
      bad=0
    fi
  fi
  sleep 5
done
