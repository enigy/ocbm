#!/bin/sh
########################
# wifi_sta.sh - the Wi-Fi link's station keeper: keep the station interface joined to the phone's hotspot so a host
# can reach ocbmd over it (ccpa/ocbmd/src/wifi.rs), from power-up and for the life of the adapter. Started by
# ocbm_boot.sh, and again by ocbmd when MGMT_WIFI_LINK changes the hotspot, only while /etc/ocbm/wifi_link has an
# ssid_hex= line. One instance (its lock holds its pid; SIGTERM stops it, its supplicant and its DHCP client).
#
# WHY A KEEPER OF ITS OWN. The supervisor raises and drops the session's AP on host-presence edges. The hotspot may be
# on before power-up, come on at the Bluetooth connect, come on late or by hand, so the station link has to be looked
# for from power-up, whatever the session does, and outlive every session.
#
# ONE RADIO. The station and the session's AP share the unit's one radio and its channel. radio_hal.sh's STA mode puts
# the AP on the other wireless interface of the same driver load (the IW416's drv_mode=3 makes sta0, its STA BSS, and
# wlan0, its uAP BSS); this keeper takes the station's (RADIO_STA_IF, else the wireless interface named sta*, only when
# another wireless interface exists beside it). It never touches the AP's interface, and waits while an AP raised
# before the link was set up still runs on the station's.
#
# WHAT IT SCANS, AND WHEN (no scan storms while a phone joins; docs/wireless/01_BT_AND_RADIO.md). Joined: nothing (no
# background scan). Not joined:
#   - no session (no AP up, no host present): wpa_supplicant's own scans of every channel;
#   - a session starting (the AP up, or a host present: M2's Bluetooth paging comes before any AP) and not yet
#     established (/tmp/session_healthy not 1), for up to START_QUIET seconds: NO scan (the supplicant's DISCONNECT),
#     so the phone's page, its join of the AP and the session's set-up never meet one;
#   - then, with the AP up: its channel only (scan_freq), and one scan of every channel each FULL_SCAN_EVERY seconds;
#     with no AP (a wired session): every channel.
# CHANNELS (ADDENDUM 6, 24 Nov 2026). The AP starts on the station's channel where an AP may (radio_ap_up.sh), so the
# two differ only when the station joined after the AP came up, or on a channel no AP may use (a radar one): said
# plainly while the AP runs, once a change. Nothing moves the AP during a session; its next start follows.
#
# /etc/ocbm/wifi_link (ocbmd writes it, 0600): key= (ocbmd's), ssid_hex= (the hotspot's name in hex), psk= (64 hex
# digits: the PBKDF2 hash, worked out on the head unit; or the passphrase; or empty: an open network).
# The hotspot's last channel is kept in /etc/ocbm/wifi_link_channel (written when it changes) for the AP's channel at
# a later boot (M2's design note); the state for ocbmd in /tmp/wifi_link_state.
#
# The supplicant: OCBM's own drop-in, /usr/sbin/ocbm-wpa_supplicant, never at /usr/sbin/wpa_supplicant (the vendor's
# dispatcher reads THAT path's existence as "STA mode" and loads another driver mode, on every unit, set up or not);
# else a unit's own stock copy at /usr/sbin; else a bench copy in /tmp/bin. The stock 2025.10.15.1127 image ships none
# (the factory survey, 27 Oct). Without one the keeper waits, one line a minute. Its control socket (ctrl_interface
# below) is spoken to by `ocbmd wpa` (ccpa/ocbmd/src/wpa.rs), not wpa_cli, which the adapter no longer carries (24 Nov
# 2026: its 68 KB of flash). Called by its bare name: the respawn wrapper's `pgrep -f /usr/sbin/ocbmd` (run_ocbmd.sh)
# never takes one of these short runs for the daemon; `pidof ocbmd` does see one while it lasts (a few ms, at most the
# 10 s reply wait).
#
# Test hooks (tools/wifi_sta_test.sh): WIFI_STA_ROOT (a scratch root for every path below), WIFI_STA_TICK,
# WIFI_STA_ONCE (one pass, then exit), RADIO_STA_IF.
########################
set -u
export PATH=${OCBM_TEST_PATH:+$OCBM_TEST_PATH:}/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
L="[wifi_sta]"
R=${WIFI_STA_ROOT:-}
CONF=$R/etc/ocbm/wifi_link
CHANNEL_MEMO=$R/etc/ocbm/wifi_link_channel
STATE=$R/tmp/wifi_link_state
WPACONF=$R/tmp/wpa_sta.conf
CTRL=$R/var/run/wpa_supplicant
WPA_PID=$R/var/run/wpa_sta.pid
DHCP_PID=$R/var/run/udhcpc_sta.pid
WPALOG=$R/tmp/wifi_sta.log
BOXLOG=$R/tmp/box.log
HOSTAPD_CONF=$R/etc/hostapd.conf
HEALTHY=$R/tmp/session_healthy
PRESENT=$R/tmp/host_present
ONE_IF=$R/tmp/.radio_one_iface
SYS=$R/sys
LOCK=$R/tmp/.wifi_sta_lock
DHCP_HOOK=${WIFI_STA_DHCP_HOOK:-$R/script/wifi_sta_dhcp.sh}
HAL=${WIFI_STA_HAL:-$R/script/radio_hal.sh}
TICK=${WIFI_STA_TICK:-2}
START_QUIET=40          # s: the longest a CarPlay start keeps the keeper from scanning
FULL_SCAN_EVERY=60      # s: a scan of every channel while a session runs and the station is not joined
RESTART_AFTER=300       # s without joining (not paused): the supplicant starts afresh

log() { _lu=""; read -r _lu _li 2>/dev/null <"$R/proc/uptime"; printf '%s [up %s] %s\n' "$L" "${_lu:-?}" "$*" >> "$BOXLOG"; }
up_s() { _us=""; read -r _us _ui 2>/dev/null <"$R/proc/uptime"; echo "${_us%.*}"; }

# ---- one instance ----------------------------------------------------------------------------------------------
if ! mkdir "$LOCK" 2>/dev/null; then
  _holder=""; read -r _holder 2>/dev/null <"$LOCK/pid"
  if [ -n "$_holder" ] && kill -0 "$_holder" 2>/dev/null; then echo "$L already running (pid $_holder)"; exit 0; fi
  rm -rf "$LOCK"; mkdir "$LOCK" 2>/dev/null || exit 1
fi
echo $$ > "$LOCK/pid"

pid_of() { _p=""; read -r _p 2>/dev/null <"$1"; [ -n "$_p" ] && kill -0 "$_p" 2>/dev/null && echo "$_p"; }
stop_pidfile() {  # $1 pid file: SIGTERM, up to 2 s, then SIGKILL
  _sp=$(pid_of "$1") || { rm -f "$1"; return 0; }
  kill "$_sp" 2>/dev/null
  _n=0; while [ "$_n" -lt 20 ] && kill -0 "$_sp" 2>/dev/null; do _n=$((_n + 1)); sleep 0.1; done
  kill -0 "$_sp" 2>/dev/null && kill -9 "$_sp" 2>/dev/null
  rm -f "$1"
}
IF=""
cleanup() {
  stop_pidfile "$DHCP_PID"
  stop_pidfile "$WPA_PID"
  [ -n "$IF" ] && ifconfig "$IF" 0.0.0.0 2>/dev/null
  rm -f "$STATE" "$WPACONF" "$WPACONF.new"
  rm -rf "$LOCK"
}
trap 'cleanup' EXIT
trap 'log "stopping"; cleanup; trap - EXIT; exit 143' INT TERM HUP

# ---- what it keeps ---------------------------------------------------------------------------------------------
ssid_hex=$(sed -n 's/^ssid_hex=//p' "$CONF" 2>/dev/null | head -1)
psk=$(sed -n 's/^psk=//p' "$CONF" 2>/dev/null | head -1)
case "$ssid_hex" in ''|*[!0-9a-fA-F]*) log "no hotspot in $CONF - nothing to keep"; exit 0 ;; esac

supplicant() {  # the path to use
  for _tc in /usr/sbin/ocbm-wpa_supplicant /usr/sbin/wpa_supplicant /tmp/bin/wpa_supplicant; do
    [ -x "$R$_tc" ] && { echo "$R$_tc"; return 0; }
  done
  return 1
}

# The station's interface: RADIO_STA_IF, else the wireless interface named sta*, only when another wireless interface
# exists beside it (the AP's). With one interface there is no station beside the AP.
sta_if() {
  [ -n "${RADIO_STA_IF:-}" ] && { [ -e "$SYS/class/net/$RADIO_STA_IF" ] && echo "$RADIO_STA_IF"; return; }
  _sti_s=""; _sti_o=""
  for _sti_d in "$SYS"/class/net/*/; do
    [ -e "$_sti_d/wireless" ] || [ -e "$_sti_d/phy80211" ] || continue
    _sti_n=$(basename "$_sti_d")
    case "$_sti_n" in sta*) [ -z "$_sti_s" ] && _sti_s=$_sti_n ;; *) _sti_o=$_sti_n ;; esac
  done
  [ -n "$_sti_s" ] && [ -n "$_sti_o" ] && echo "$_sti_s"
}

ap_up() { pidof hostapd >/dev/null 2>&1; }
# The AP's channel: the configuration it runs from, the session's where it followed the station (radio_ap_up.sh).
AP_SESSION=$R/tmp/hostapd_session.conf
ap_channel() {
  _apf=$HOSTAPD_CONF; [ -f "$AP_SESSION" ] && _apf=$AP_SESSION
  _apc=$(sed -n 's/^channel=//p' "$_apf" 2>/dev/null | head -1)
  case "$_apc" in ''|*[!0-9]*) echo 0 ;; *) echo "$_apc" ;; esac
}
ap_iface_conf() { sed -n 's/^interface=//p' "$HOSTAPD_CONF" 2>/dev/null | head -1; }
freq_of() {  # $1 channel -> MHz
  if [ "$1" -ge 1 ] && [ "$1" -le 13 ]; then echo $((2407 + 5 * $1)); elif [ "$1" = 14 ]; then echo 2484
  elif [ "$1" -ge 32 ]; then echo $((5000 + 5 * $1)); else echo 0; fi
}
chan_of() {  # $1 MHz -> channel
  case "$1" in
    2484) echo 14 ;;
    24[0-9][0-9]) echo $(( ($1 - 2407) / 5 )) ;;
    5[0-9][0-9][0-9]) echo $(( ($1 - 5000) / 5 )) ;;
    *) echo 0 ;;
  esac
}

write_conf() {  # $1 = "" (every channel) or the one frequency to scan
  _old_umask=$(umask); umask 077
  {
    printf 'ctrl_interface=%s\n' "$CTRL"
    printf 'update_config=0\n'
    printf 'ap_scan=1\n'
    printf 'network={\n'
    printf '\tssid=%s\n' "$ssid_hex"
    if [ -z "$psk" ]; then printf '\tkey_mgmt=NONE\n'
    elif [ "${#psk}" = 64 ] && [ -z "$(printf '%s' "$psk" | tr -d '0-9a-fA-F')" ]; then printf '\tpsk=%s\n' "$psk"
    else printf '\tpsk="%s"\n' "$psk"; fi
    printf '\tscan_ssid=1\n'
    [ -n "$1" ] && printf '\tscan_freq=%s\n' "$1"
    printf '}\n'
  } > "$WPACONF.new" && mv -f "$WPACONF.new" "$WPACONF"
  umask "$_old_umask"
  conf_freq=$1
}

publish() {  # $1 joined|joining|unsupported, $2 channel; the lines in the order radio_hal.sh's sta_channel reads them
  { echo "iface=$IF"; echo "state=$1"; echo "channel=$2"; } > "$STATE.new" 2>/dev/null && mv -f "$STATE.new" "$STATE"
}

# One request to the supplicant's control socket, its reply on stdout (nothing when none came).
cli() { ocbmd wpa -p "$CTRL" "$IF" "$@" 2>/dev/null; }

log "keeping the station on the hotspot set up in $CONF"
mkdir -p "$CTRL" "$R/var/run" 2>/dev/null
WPA=""; said_tools=0; said_ap_on_sta=0; none=0; wpa_down=0; wpa_next=0
last_ch=""; last_mismatch=""; joined_at=""; down_since=$(up_s)
paused=0; ap_since=""; last_full=0; conf_freq="x"
while :; do
  # No station on this unit (below): nothing changes before the next boot.
  [ "$none" = 1 ] && { [ -n "${WIFI_STA_ONCE:-}" ] && exit 0; sleep "$TICK"; continue; }
  now=$(up_s)
  # The supplicant: a drop-in or a bench copy can come at any time.
  if [ -z "$WPA" ]; then
    WPA=$(supplicant) || WPA=""
    if [ -z "$WPA" ]; then
      [ $((said_tools % 30)) = 0 ] && log "no wpa_supplicant (OCBM's drop-in /usr/sbin/ocbm-wpa_supplicant, the unit's own, or a bench copy in /tmp/bin): no station link yet"
      said_tools=$((said_tools + 1)); [ -n "${WIFI_STA_ONCE:-}" ] && exit 0; sleep "$TICK"; continue
    fi
    log "supplicant: $WPA"
  fi
  # The interface: the driver loads with the boot preload or a session; with no station's interface, the seam loads it
  # and says whether this unit has one (radio_hal.sh wifi_sta_up). Its 3, no station here (one wireless interface, its
  # mark ONE_IF, or no mapped backend), holds for the boot: said once, published for ocbmd, not asked again.
  if [ -z "$IF" ]; then
    IF=$(sta_if)
    if [ -z "$IF" ]; then
      if [ -e "$ONE_IF" ]; then rc=3; else sh "$HAL" wifi_sta_up >> "$BOXLOG" 2>&1; rc=$?; fi
      if [ "$rc" = 3 ]; then
        none=1; publish unsupported 0
        log "no station interface on this unit (one wireless interface, or no mapped backend): no Wi-Fi link here"
      fi
      [ -n "${WIFI_STA_ONCE:-}" ] && exit 0; sleep "$TICK"; continue
    fi
    log "station interface: $IF"
  fi
  # The interface gone (the driver unloaded): its supplicant and DHCP client with it; looked for again.
  if [ ! -e "$SYS/class/net/$IF" ]; then
    log "$IF is gone: the station starts again when an interface is back"
    stop_pidfile "$DHCP_PID"; stop_pidfile "$WPA_PID"; IF=""; joined_at=""; publish joining 0 2>/dev/null
    [ -n "${WIFI_STA_ONCE:-}" ] && exit 0; sleep "$TICK"; continue
  fi
  # An AP raised before the link was set up may still run on the station's interface: it goes at the session's end.
  if ap_up && [ "$(ap_iface_conf)" = "$IF" ]; then
    [ "$said_ap_on_sta" = 0 ] && log "the AP runs on $IF (raised before the link was set up): the station waits for it to go"
    said_ap_on_sta=1; [ -n "${WIFI_STA_ONCE:-}" ] && exit 0; sleep "$TICK"; continue
  fi
  said_ap_on_sta=0
  # The supplicant: started, and again no sooner than 30 s after its last start; one that does not start or does not
  # stay is said once, until it runs.
  if ! pid_of "$WPA_PID" >/dev/null && [ "$now" -ge "$wpa_next" ]; then
    wpa_next=$((now + 30))
    [ "$conf_freq" = x ] && write_conf ""
    ifconfig "$IF" 0.0.0.0 up 2>/dev/null
    [ "$wpa_down" = 1 ] || log "starting the supplicant on $IF"
    "$WPA" -B -P "$WPA_PID" -D nl80211,wext -i "$IF" -c "$WPACONF" >> "$WPALOG" 2>&1 && sleep 1
    paused=0
  fi
  if ! pid_of "$WPA_PID" >/dev/null; then
    [ "$wpa_down" = 1 ] || log "the supplicant did not start, or did not stay (/tmp/wifi_sta.log): again every 30 s"
    wpa_down=1; [ -n "${WIFI_STA_ONCE:-}" ] && exit 0; sleep "$TICK"; continue
  fi
  wpa_down=0
  st=$(cli status)
  wstate=$(echo "$st" | sed -n 's/^wpa_state=//p')
  freq=$(echo "$st" | sed -n 's/^freq=//p')
  if [ "$wstate" = COMPLETED ]; then
    ch=$(chan_of "${freq:-0}")
    [ -n "$joined_at" ] || { joined_at=$now; log "$IF joined the hotspot on channel $ch"; }
    pid_of "$DHCP_PID" >/dev/null || udhcpc -i "$IF" -b -R -p "$DHCP_PID" -s "$DHCP_HOOK" >> "$WPALOG" 2>&1
    publish joined "$ch"
    if [ "$ch" != "$last_ch" ] && [ "$ch" != 0 ]; then
      last_ch=$ch
      _kept=""; read -r _kept 2>/dev/null <"$CHANNEL_MEMO"
      [ "$_kept" = "$ch" ] || { echo "$ch" > "$CHANNEL_MEMO.new" 2>/dev/null && mv -f "$CHANNEL_MEMO.new" "$CHANNEL_MEMO"; }
    fi
    # An AP running on another channel (it came up before the station joined, or the hotspot's is no AP's): said once a
    # change. It stays for its session; its next start follows where an AP may (ADDENDUM 6).
    mm=""; ap_up && mm="$ch/$(ap_channel)"
    if [ "$mm" != "$last_mismatch" ]; then
      last_mismatch=$mm; apc=${mm#*/}
      [ -n "$mm" ] && [ "$apc" != 0 ] && [ "$ch" != "$apc" ] && \
        log "the hotspot is on channel $ch and the adapter's AP on $apc: one radio serves both; the AP stays for this" \
            "session, its next start follows the hotspot where an AP may (no radar channel)"
    fi
    down_since=""
  else
    if [ -n "$joined_at" ]; then
      log "$IF left the hotspot (${wstate:-no answer})"; joined_at=""
      stop_pidfile "$DHCP_PID"; ifconfig "$IF" 0.0.0.0 2>/dev/null
    fi
    [ -n "$down_since" ] || { down_since=$now; log "$IF looking for the hotspot"; }
    publish joining 0
    # Scans around a session (the header): one is starting with the AP up or a host present.
    up=0; ap_up && up=1
    hp=""; read -r hp 2>/dev/null <"$PRESENT"
    healthy=""; read -r healthy 2>/dev/null <"$HEALTHY"
    if [ "$up" = 1 ] || [ "$hp" = 1 ]; then [ -n "$ap_since" ] || ap_since=$now; else ap_since=""; fi
    if [ -n "$ap_since" ] && [ "$healthy" != 1 ] && [ $((now - ap_since)) -lt "$START_QUIET" ]; then
      [ "$paused" = 1 ] || { cli disconnect >/dev/null; paused=1; log "CarPlay is starting: no scans until it runs (${START_QUIET} s at most)"; }
    elif [ "$up" = 1 ]; then
      f=$(freq_of "$(ap_channel)")
      [ "$f" != 0 ] && [ "$conf_freq" != "$f" ] && { write_conf "$f"; cli reconfigure >/dev/null; }
      if [ "$paused" = 1 ]; then cli reconnect >/dev/null; paused=0; last_full=$now; log "scanning the AP's channel, and every channel each ${FULL_SCAN_EVERY} s"; fi
      [ $((now - last_full)) -ge "$FULL_SCAN_EVERY" ] && { cli scan >/dev/null; last_full=$now; }
    else
      [ "$conf_freq" != "" ] && { write_conf ""; cli reconfigure >/dev/null; }
      [ "$paused" = 1 ] && { cli reconnect >/dev/null; paused=0; }
    fi
    # Stuck where it does not leave by itself (a hotspot back on another channel, say): start afresh, not paused.
    if [ "$paused" = 0 ] && [ $((now - down_since)) -ge "$RESTART_AFTER" ]; then
      log "$IF not joined for ${RESTART_AFTER} s: the supplicant starts afresh"
      stop_pidfile "$WPA_PID"; down_since=$now
    fi
  fi
  [ -n "${WIFI_STA_ONCE:-}" ] && exit 0
  sleep "$TICK"
done
