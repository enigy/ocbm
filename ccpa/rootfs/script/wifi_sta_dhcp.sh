#!/bin/sh
# udhcpc hook for the Wi-Fi transport's STA link (wifi_sta.sh): set the address and the default route,
# and nothing else. busybox's default script also rewrites /etc/resolv.conf, which is on flash, at
# every renewal; the box resolves nothing over this link, so that write is skipped.
case "$1" in
  deconfig)
    ifconfig "$interface" 0.0.0.0 2>/dev/null ;;
  bound|renew)
    ifconfig "$interface" "$ip" netmask "${subnet:-255.255.255.0}" up
    route del default dev "$interface" 2>/dev/null
    for _gw in $router; do route add default gw "$_gw" dev "$interface" 2>/dev/null && break; done
    echo "[wifi_sta] $interface $1: $ip via ${router:-none}" >> /tmp/box.log ;;
esac
exit 0
