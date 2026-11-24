#!/bin/sh
# wifi_sta_dhcp.sh - udhcpc's hook for the Wi-Fi link's station (wifi_sta.sh): the address and its netmask, nothing
# else. No default route (the head unit is on the hotspot's own subnet, and nothing on the adapter goes further), no
# /etc/resolv.conf (busybox's default script rewrites it on flash at every renewal), and never an address on the
# adapter's own AP subnet or NCM's (192.168.43.x, 192.168.50.x): a hotspot handing one out would take them over.
# The log names the address's last octet only. WIFI_STA_ROOT: tools/wifi_sta_test.sh's scratch root.
BOXLOG=${WIFI_STA_ROOT:-}/tmp/box.log
case "$1" in
  deconfig)
    ifconfig "$interface" 0.0.0.0 2>/dev/null ;;
  bound|renew)
    case "$ip" in
      192.168.43.*|192.168.50.*)
        echo "[wifi_sta] $interface $1: an address on the adapter's own AP or NCM subnet - not taken" >> "$BOXLOG"
        ifconfig "$interface" 0.0.0.0 2>/dev/null
        exit 0 ;;
    esac
    ifconfig "$interface" "$ip" netmask "${subnet:-255.255.255.0}" up
    echo "[wifi_sta] $interface $1: ..${ip##*.}" >> "$BOXLOG" ;;
esac
exit 0
