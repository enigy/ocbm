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
# USAGE  RADIO_WLAN_IF=<iface> sh radio_ap_up.sh [AP|PREP]
#        AP (the default) brings the AP up. PREP writes its configuration only, raising nothing, and the stamp a
#        later AP start trusts (radio_hal.sh wifi_ap_prep, the armed state between host sessions).
#        RADIO_AP_CHANNEL: STA mode's station channel, for the AP to follow (ap_follow).
# EXIT   0 AP converged (PREP: configured)   1 failed to converge
########################
set -u
export PATH=${OCBM_TEST_PATH:+$OCBM_TEST_PATH:}/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
L="[radio_ap_up]"

# ---- passphrase: set only if MISSING, never blanked ------------------------------------------
# The vendor bug this guards against is unconditional rewriting from a config oracle that no
# longer exists. We only ever fill in an absent or too-short key, and never overwrite a good one.
# A well-known default counts as missing (security review C1, 2026-10-21): Carlinkit ships
# 12345678, and the old fallback below was derived from the AP's own MAC, which the AP broadcasts.
# The replacement is random and persisted once; the phone is handed it in the iAP2 0x5703 handoff
# at every connect, so nobody ever types it.
# A function of the root it works under ($1, empty on the adapter), defined before anything runs, so
# tools/radio_ap_up_test.sh sources this file for it alone (RADIO_AP_UP_FUNCS=1) and runs the whole
# read and write against a scratch tree, under BusyBox too (the Mac's round 2, R1-1). Called below,
# where the name is set; SUF is the AP interface's suffix by then.
_known_psk() { case "$1" in 12345678|"carplay$SUF") return 0 ;; esac; return 1; }
ap_passphrase() {
  _hc="$1/etc/hostapd.conf"
  PSK=$(sed -n 's/^wpa_passphrase=//p' "$_hc" | head -1)
  _known_psk "$PSK" && PSK=""
  if [ "${#PSK}" -lt 8 ]; then
    # OCBM's own from before a go-back to stock first (it puts stock's 12345678 back): each phone knows it, and a new one
    # cost each phone about 6 s of PSK-MISMATCH retries at its first connect after the reinstall (the Mac, 13 Nov).
    # Which source it came from is logged, never the passphrase (the Mac's review, PSK-SOURCE-UNLOGGED).
    # tr keeps ' ' to '~', the printable ASCII a WPA passphrase is made of, as a range: BusyBox 1.32.1's tr has no
    # [:print:] and took it for the letters [ : p r i n t ], so a kept hex key came out empty and /etc/wifi_password was
    # cut to those letters (the Mac's round 2, R1-1).
    _psk_src="the one OCBM kept (/etc/ocbm/wifi_psk)"
    PSK=$(cat "$1/etc/ocbm/wifi_psk" 2>/dev/null | tr -cd ' -~' | head -c 63)
    _known_psk "$PSK" && PSK=""
    if [ "${#PSK}" -lt 8 ]; then
      _psk_src="/etc/wifi_password"
      PSK=$(cat "$1/etc/wifi_password" 2>/dev/null | tr -cd ' -~' | head -c 63)
      _known_psk "$PSK" && PSK=""
    fi
    if [ "${#PSK}" -lt 8 ]; then
      _psk_src="a new random one"
      PSK=$(head -c 64 /dev/urandom | md5sum | cut -c1-20)
    fi
    # The key reaches awk through its environment, never sed's replacement or awk -v, which take '\' (and sed '/' and
    # '&'): a printable ASCII passphrase, a kept one or /etc/wifi_password's, is written as it was read. Read back; a
    # line that doesn't hold it is logged as a failure, without the key (the Mac's 2.6.0 review, CS-4).
    cp -p "$_hc" "$_hc.new" 2>/dev/null   # its mode, as sed -i kept it
    OCBM_PSK="$PSK" awk '/^wpa_passphrase=/ { print "wpa_passphrase=" ENVIRON["OCBM_PSK"]; seen = 1; next }
      { print }
      END { if (!seen) print "wpa_passphrase=" ENVIRON["OCBM_PSK"] }' "$_hc" > "$_hc.new" \
      && mv -f "$_hc.new" "$_hc"
    rm -f "$_hc.new"
    if [ "$(sed -n 's/^wpa_passphrase=//p' "$_hc" | head -1)" = "$PSK" ]; then
      echo "$L wpa_passphrase was absent, too short or a known default - set $_psk_src"
    else
      echo "$L wpa_passphrase was absent, too short or a known default - FAILED to set $_psk_src: hostapd.conf does not hold it"
    fi
  fi
  # Kept where a go-back leaves it (/etc/ocbm stays on flash; ocbm_revert.sh), for the install after it. Not logged.
  # printf, not echo: an echo may take a '\' in it as an escape.
  if [ -d "$1/etc/ocbm" ] && [ "$(cat "$1/etc/ocbm/wifi_psk" 2>/dev/null)" != "$PSK" ]; then
    ( umask 077; printf '%s\n' "$PSK" > "$1/etc/ocbm/wifi_psk.new" && mv -f "$1/etc/ocbm/wifi_psk.new" "$1/etc/ocbm/wifi_psk" ) 2>/dev/null
  fi
}

# ---- the AP address: never 192.168.50.x ---------------------------------------------------------
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

# One udhcpd.conf key ($2) set to $3 in the file $1: added when absent, rewritten when it holds anything else.
_dhcp_key() {
  if grep -q "^[[:space:]]*$2[[:space:]]" "$1" 2>/dev/null; then
    grep "^[[:space:]]*$2[[:space:]]" "$1" | grep -qvxF "$2	$3" \
      && sed -i "s|^[[:space:]]*$2[[:space:]].*|$2	$3|" "$1"
  else
    printf '%s\t%s\n' "$2" "$3" >> "$1"
  fi
  return 0
}

# ---- the AP's configuration (radio_ap_up.sh's work before anything is raised) --------------------
# A function of the root it works under ($1, empty on the adapter) since 16 Nov 2026, as ap_passphrase is: the arm
# writes it ahead (PREP, with no host here: files only), and tools/radio_ap_up_test.sh runs it against scratch
# adapters. It configures the interface IF. Each file is rewritten only where it holds something else: the arm runs
# this at every host departure, and /etc is flash; the result is what the unconditional writes before it left. Sets
# NAME, SUF, ch and WLANIP.
ap_configure() {
  _hc="$1/etc/hostapd.conf"
  _uc="$1/etc/udhcpd.conf"
  # ---- device-unique name ---------------------------------------------------------------------
  # The box's one name, the one the iPhone lists on Bluetooth (btd's CarLink-<last 4 hex of wlan0's
  # MAC>, else of the serial; radio_hal.sh box_name, which says why it is decided once and kept in
  # /etc/carplay_ident). The seam resolves it and passes it in, so the SSID and the Bluetooth name come
  # from ONE decision. The derivation below is only for a direct/bench invocation, and is box_name's.
  #
  # This overwrites ssid= in /etc/hostapd.conf, which is also what the box hands the phone in the
  # iAP2 0x5703 handoff, so the two always name the same AP.
  NAME="${RADIO_BOX_NAME:-}"
  if [ -z "$NAME" ] && [ -s "$1/etc/carplay_ident" ]; then
    NAME=$(head -1 "$1/etc/carplay_ident")
    case "$NAME" in CarLink-*) ;; *) NAME="" ;; esac   # an older OCBM's ccpa-<4 hex> is derived again
  fi
  if [ -z "$NAME" ]; then
    SUF=$(cat "$1/sys/class/net/wlan0/address" 2>/dev/null | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p')
    [ -n "$SUF" ] || SUF=$(sed -n 's/^wlan0=//p' "$1/etc/ocbm/addresses.txt" 2>/dev/null | head -1 | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p')
    if [ -n "$SUF" ]; then NAME="CarLink-$SUF"; echo "$NAME" > "$1/etc/carplay_ident" 2>/dev/null
    else
      SUF=$(cat "$1/etc/serial_number" 2>/dev/null | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p')
      NAME="CarLink${SUF:+-$SUF}"
    fi
  fi
  # The AP interface's own suffix, for the passphrase check below: an older fallback key was "carplay" and it.
  SUF=$(cat "$1/sys/class/net/$IF/address" 2>/dev/null | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p')
  grep '^ssid=' "$_hc" | grep -qvxF "ssid=$NAME" && sed -i "s/^ssid=.*/ssid=$NAME/" "$_hc"
  _wn=""; read -r _wn 2>/dev/null <"$1/etc/wifi_name"
  [ "$_wn" = "$NAME" ] || echo "$NAME" > "$1/etc/wifi_name"

  # ---- passphrase (ap_passphrase, above) -------------------------------------------------------
  ap_passphrase "$1"

  # ---- channel + band ---------------------------------------------------------------------------
  ch=$(sed -n 's/^channel=//p' "$_hc" | head -1)
  [ -e "$1/etc/wifi_use_24G" ] && ch=6
  case "$ch" in ''|*[!0-9]*) ch=36 ;; esac
  # hostapd of this vintage has no ACS and refuses to start with no channel, so it is always pinned.
  if [ "$ch" -ge 1 ] && [ "$ch" -le 14 ]; then
    grep -q "^hw_mode=a" "$_hc" && sed -i "s/^hw_mode=a/hw_mode=g/" "$_hc"
  else
    grep -q "^hw_mode=g" "$_hc" && sed -i "s/^hw_mode=g/hw_mode=a/" "$_hc"
  fi
  grep '^channel=' "$_hc" | grep -qvxF "channel=$ch" && sed -i "s/^channel=.*/channel=${ch}/" "$_hc"
  # The AP must serve the interface we were handed, whatever it is called.
  if grep -q '^interface=' "$_hc"; then
    grep '^interface=' "$_hc" | grep -qvxF "interface=$IF" && sed -i "s/^interface=.*/interface=$IF/" "$_hc"
  else
    sed -i "1i interface=$IF" "$_hc"
  fi

  # ---- AP address (_ip_canon, above) -------------------------------------------------------------
  WLANIP=192.168.43.1
  if [ -e "$1/etc/wifi_ip" ]; then
    _w=$(_ip_canon "$(cat "$1/etc/wifi_ip" 2>/dev/null)")
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
  # The pool onto the AP's subnet, when a pool line names another.
  grep "192\.168\.[0-9]*\.[12]00" "$_uc" 2>/dev/null | grep -qv "$base\.[12]00" \
    && sed -i "s/192\.168\.[0-9]*\.100/${base}.100/; s/192\.168\.[0-9]*\.200/${base}.200/" "$_uc" 2>/dev/null
  # Point udhcpd at the interface we are actually serving.
  grep '^interface' "$_uc" 2>/dev/null | grep -qvxF "interface	$IF" && sed -i "s/^interface.*/interface	$IF/" "$_uc"
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
  _dhcp_key "$_uc" lease_file /var/lib/udhcpd.leases
  _dhcp_key "$_uc" pidfile /var/run/udhcpd.pid
  _dhcp_key "$_uc" auto_time 60
  return 0
}

# ---- the configuration written ahead: its stamp ---------------------------------------------------
# Two lines in $1/tmp/.radio_ap_prepared (tmpfs: a power cut forgets it), written after ap_configure:
# "<interface> <name> <address> <channel> <2.4 GHz flag 0|1>", then the checksums of the files the configuration lives
# in and is written from (ap_sums). A later start on the same interface trusts it, and skips ap_configure, while those
# files hold what they held, the 2.4 GHz flag is as it was, and the box's name is still the one it holds. On success
# sets NAME, WLANIP and ch from the stamp.
# BY CONTENT, NEVER BY DATE (the release review of resident-bt, A3-2). The adapter's clock reads 2020-01-02 until a host
# sets it, so a stamp the boot's arm wrote was dated before files a 2026 clock had written, a date comparison never
# trusted it, and the fast path did nothing on the first connect after every power-up. One md5sum over five small files
# is the one fork this costs a start. The passphrase's sources (/etc/ocbm/wifi_psk, /etc/wifi_password) are not
# watched: ap_passphrase reads them only for a hostapd.conf without a good key, and hostapd.conf is watched.
AP_PREPARED=/tmp/.radio_ap_prepared
AP_WATCHED="hostapd.conf udhcpd.conf carplay_ident wifi_ip wifi_name"
# The watched files' checksums under root $1, on one line, into _ap_sums. A file that is not there has none, so its
# coming or going changes the line too.
ap_sums() {
  _ap_raw=$(cd "$1/etc" 2>/dev/null && md5sum $AP_WATCHED 2>/dev/null)
  set -- $_ap_raw
  _ap_sums="$*"
}
ap_stamp() {
  _s24=0; [ -e "$1/etc/wifi_use_24G" ] && _s24=1
  ap_sums "$1"
  { echo "$IF $NAME $WLANIP $ch $_s24"; echo "$_ap_sums"; } 2>/dev/null >"$1$AP_PREPARED"
  return 0
}
ap_prepared_ok() {
  _st="$1$AP_PREPARED"
  _p_if=""; _p_name=""; _p_ip=""; _p_ch=""; _p_24=""; _p_sums=""
  { read -r _p_if _p_name _p_ip _p_ch _p_24 _p_rest; read -r _p_sums; } 2>/dev/null <"$_st"
  [ -n "$_p_24" ] && [ -n "$_p_sums" ] && [ "$_p_if" = "$IF" ] || return 1
  case "$_p_ch" in ''|*[!0-9]*) return 1 ;; esac
  _p_24now=0; [ -e "$1/etc/wifi_use_24G" ] && _p_24now=1
  [ "$_p_24" = "$_p_24now" ] || return 1
  [ -f "$1/etc/hostapd.conf" ] || return 1
  # The name: the seam's when it passes one, else the one /etc/carplay_ident holds (a name it would derive afresh, as
  # with no file, is not trusted from the stamp).
  _p_id=""; read -r _p_id 2>/dev/null <"$1/etc/carplay_ident"
  [ "$_p_name" = "${RADIO_BOX_NAME:-$_p_id}" ] || return 1
  ap_sums "$1"
  [ "$_ap_sums" = "$_p_sums" ] || return 1
  NAME=$_p_name; WLANIP=$_p_ip; ch=$_p_ch
  return 0
}
# ---- the AP's DHCP server: the last AP's never counts as this one's ------------------------------
# The release review of resident-bt (A3-1), the likely cause of the second of the 16 Nov bench's bad wake's two delays
# (no "sending ACK" for about 6.7 s, the phone on its old address; the first, the phone's late join, is open): BusyBox
# udhcpd writes its lease file (/var/lib/udhcpd.leases, on the jffs2 root) as it exits on SIGTERM, and with flash slow
# (garbage collection just after an update's commit removed its .prev binaries) the last AP's server was still in the
# process list when the next AP started. That start took it for its own and started none, and when it had gone the AP
# had no DHCP. radio_hal.sh's teardown now waits for it (its own copy of ap_dhcp_stop), and a start that launches
# hostapd stops any left and keeps the pid of the one it starts. Found by config path, never by name: the NCM server,
# `busybox udhcpd -f /tmp/udhcpd_ncm.conf`, is never one of them. AP_DHCP_MATCH and AP_PROC are test hooks.
AP_DHCP_MATCH=${AP_DHCP_MATCH:-udhcpd .*etc/udhcpd.conf}
AP_PROC=${AP_PROC:-/proc}
ap_dhcp_pids() { ps 2>/dev/null | grep -v grep | grep "$AP_DHCP_MATCH" | awk '{print $1}'; }
ap_pids_live() { for _apl in "$@"; do kill -0 "$_apl" 2>/dev/null && return 0; done; return 1; }
# SIGTERM to every AP DHCP server, then up to $1 looks 0.2 s apart for them to go (their lease file: 15 when not given,
# 3 s), then SIGKILL and up to $2 looks 0.1 s apart (10, 1 s). 1 when one survived even that (a kernel wait): a server
# started now may not bind. The start gives 5 and 5, 1.5 s at most (the second review of resident-bt, item 7): this
# wait holds the wlan lock, which btd's reply to the phone's 0x5702 waits on for 5 s at most (wifi_handoff::AP_WAIT),
# and the teardown has given the last AP's server its 3 s already; one still there at a start is stuck or was never
# torn down.
ap_dhcp_stop() {
  _adt=${1:-15}; _adk=${2:-10}
  _ads=$(ap_dhcp_pids)
  [ -n "$_ads" ] || return 0
  for _adp in $_ads; do kill "$_adp" 2>/dev/null; done
  _adn=0; while [ "$_adn" -lt "$_adt" ] && ap_pids_live $_ads; do _adn=$((_adn + 1)); sleep 0.2; done
  ap_pids_live $_ads || return 0
  echo "$L the last AP's DHCP server is still there $((_adt / 5)) s after SIGTERM (its lease file on flash?) - SIGKILL"
  for _adp in $_ads; do kill -9 "$_adp" 2>/dev/null; done
  _adn=0; while [ "$_adn" -lt "$_adk" ] && ap_pids_live $_ads; do _adn=$((_adn + 1)); sleep 0.1; done
  ap_pids_live $_ads || return 0
  echo "$L the last AP's DHCP server survived SIGKILL (a kernel wait?): this AP's may not bind"
  return 1
}
# This AP's DHCP server serving: the one this start launched ($_dhcpd), alive (its /proc stat not a zombie this shell
# has not reaped) at each look, through the confirmation; or, when this start launched none, any by config path.
ap_dhcp_serving() {
  if [ -n "${_dhcpd:-}" ]; then
    _adx=""; _adx_name=""
    read -r _adx_pid _adx_name _adx _adx_rest 2>/dev/null <"$AP_PROC/$_dhcpd/stat"
    # $! is the server's own pid (setsid does not fork for a job that leads no process group, which ours never do);
    # were it setsid's, ask by config path as before rather than fail an AP that serves.
    [ "$_adx_name" = "(setsid)" ] && { [ -n "$(ap_dhcp_pids)" ]; return; }
    [ -n "$_adx" ] && [ "$_adx" != Z ]
  else
    [ -n "$(ap_dhcp_pids)" ]
  fi
}
# ---- hostapd's own word ---------------------------------------------------------------------------
# Its AP-ENABLED: one after this start's own line in the box log, "[radio] hostapd starting on <if>" (_ae_mark),
# written just before hostapd's launch. By that line, not by a count: ocbmd empties the box log at its cap with an
# ftruncate in place (the same inode, so an inode says nothing either), and a count taken at the launch missed this
# hostapd's line when the log was emptied and the line written within one look (the second review of resident-bt,
# item 3; the release review's A3-3 had reset the count only when it fell). With this start's line gone the log was
# emptied after the launch, so every AP-ENABLED in it is this hostapd's. One window is left: the line written and the
# log emptied before the next look (100 ms). The wait then runs to its 2 s bound holding the wlan lock btd's 0x5703
# reply waits on, and a hostapd still running counts, as before 16 Nov. No line of its own (the write failed): never
# seen, the bound. One fork a look, as the count was. AP_BOXLOG is a test hook; hostapd writes the box log itself.
AP_BOXLOG=${AP_BOXLOG:-/tmp/box.log}
ap_enabled_seen() {
  [ -n "${_ae_mark:-}" ] || return 1
  _ae_out=$(grep -F -e "$_ae_mark" -e "AP-ENABLED" "$AP_BOXLOG" 2>/dev/null)
  case "$_ae_out" in *"$_ae_mark"*) _ae_out=${_ae_out##*"$_ae_mark"} ;; esac
  case "$_ae_out" in *AP-ENABLED*) return 0 ;; esac
  return 1
}
# ---- STA mode: the AP on the station's channel (ADDENDUM 6, 24 Nov 2026) --------------------------------------------
# One radio. The bench's AP on 36 beside the station on the hotspot's 149 was never joined. RADIO_AP_CHANNEL is the
# seam's ("" no link, 0 not joined: the own channel, as before). ap_may_use: the US domain hostapd.conf asks for
# (country_code=US), no DFS: 1-11, 36-48, 149-165. ht40_side: its pair's side (_hs); none on 165 and 14.
ap_may_use() { case "$1" in [1-9]|1[01]|36|40|44|48|149|153|157|161|165) return 0 ;; esac; return 1; }
ht40_side() { _hs=""; case "$1" in [1-7]|36|44|149|157) _hs=+ ;; [89]|1[0-3]|40|48|153|161) _hs=- ;; esac; }
say() { echo "$L $*"; echo "$L $*" 2>/dev/null >>"$AP_BOXLOG"; }   # the box log too
# hostapd's configuration (_hrun) under root $1: AP_SESSION (tmpfs, 0600: hostapd.conf with the station's channel, band
# and HT40 side) when it follows, else hostapd.conf, never written for it. Sets ch; _own: the AP's own.
AP_SESSION=/tmp/hostapd_session.conf
ap_follow() {
  _hrun="$1/etc/hostapd.conf"; _own=$ch; _fc=${RADIO_AP_CHANNEL:-}
  rm -f "$1$AP_SESSION"
  case "$_fc" in
    ''|"$ch") return 0 ;;
    0) say "STA mode: the station is not on the hotspot - the AP keeps its own channel $ch"; return 0 ;;
  esac
  if ! ap_may_use "$_fc"; then
    _w="not one an AP may use"
    case "$_fc" in 5[2-9]|6[0-4]|1[0-3][0-9]|14[0-4]) _w="a radar (DFS) channel" ;; esac
    say "STA mode: the station is on channel $_fc, $_w - the AP keeps its own channel $ch"; return 0
  fi
  _hm=a; [ "$_fc" -le 14 ] && _hm=g; ht40_side "$_fc"
  if ( umask 077; sed -e "s/^channel=.*/channel=$_fc/; s/^hw_mode=.*/hw_mode=$_hm/; /^vht_oper_c/d" \
         -e "/^ht_capab=/s/\[HT40[-+]\]/${_hs:+[HT40$_hs]}/" "$_hrun" > "$1$AP_SESSION.new" ) \
     && mv -f "$1$AP_SESSION.new" "$1$AP_SESSION"; then
    say "STA mode: the AP follows the station to channel $_fc (hw_mode=$_hm${_hs:+, HT40$_hs})"
    ch=$_fc; _hrun="$1$AP_SESSION"
  else
    rm -f "$1$AP_SESSION.new"; say "STA mode: channel $_fc not written - the AP keeps its own channel $ch"
  fi
}
[ "${RADIO_AP_UP_FUNCS:-}" = 1 ] && return 0

# The root the start works under, "" on the adapter, beside AP_PROC (above): a test hook. tools/radio_ap_up_test.sh runs
# the whole start against a scratch adapter with its commands stood in for (the second review of resident-bt, item 5).
AP_ROOT=${AP_ROOT:-}

# The connect timeline's ap_start and ap_up (crates/box-common/src/timeline.rs): `t <key> <uptime ms>` appended to btd's
# record. One append, no fork; a failed one is ignored.
tl_stamp() {
  _tls_up=""
  read -r _tls_up _tls_idle 2>/dev/null <"$AP_PROC/uptime"
  case "$_tls_up" in *.*) echo "t $1 ${_tls_up%.*}${_tls_up#*.}0" 2>/dev/null >>"$AP_ROOT/tmp/startup_marks" ;; esac
  return 0
}

IF=${RADIO_WLAN_IF:-wlan0}
[ -e "$AP_ROOT/sys/class/net/$IF" ] || { echo "$L interface $IF does not exist - the driver must be up first"; exit 1; }
# AND IT MUST ACTUALLY BE A WIRELESS INTERFACE. Existence alone is not enough: this script
# unconditionally reconfigures whatever it is handed, so `RADIO_WLAN_IF=ncm0` would sail through
# the check above and then `ifconfig ncm0 192.168.43.1` - re-addressing the NCM management link
# out from under the only way into a unit with no UART, before hostapd ever gets a chance to
# fail. The seam always passes an enumerated wireless interface, but "the caller is careful" is
# exactly the guarantee this file refuses to accept from anyone else, so it does not accept it
# for itself either. A cfg80211/wext netdev is identifiable without knowing the driver.
if [ ! -e "$AP_ROOT/sys/class/net/$IF/wireless" ] && [ ! -e "$AP_ROOT/sys/class/net/$IF/phy80211" ]; then
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
LOCK=$AP_ROOT/tmp/.radio_ap_lock
if ! mkdir "$LOCK" 2>/dev/null; then
  _holder=$(cat "$LOCK/pid" 2>/dev/null)
  if { [ -n "$_holder" ] && [ ! -d "$AP_PROC/$_holder" ]; } \
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

[ -f "$AP_ROOT/etc/hostapd.conf" ] || { echo "$L /etc/hostapd.conf missing"; exit 1; }

# ---- configure, or trust the configuration written ahead ------------------------------------------
# AP (the default): the AP's bring-up. PREP (the armed state between host sessions, radio_hal.sh wifi_ap_prep): the
# configuration only, then the stamp, and nothing raised: no address, no hostapd, no udhcpd, no beacon.
MODE=${1:-AP}
if [ "$MODE" != PREP ] && ap_prepared_ok "$AP_ROOT"; then
  echo "$L configured ahead ($AP_PREPARED): ssid=$NAME ip=$WLANIP ch=$ch if=$IF - launching"
else
  ap_configure "$AP_ROOT"
  ap_stamp "$AP_ROOT"
  # The binary staged where the vendor layout keeps it, as before (a copy in tmpfs, once per boot).
  test -e "$AP_ROOT/tmp/bin/hostapd" || cp "$AP_ROOT/usr/sbin/hostapd" "$AP_ROOT/tmp/bin/hostapd" 2>/dev/null
  if [ "$MODE" = PREP ]; then
    echo "$L configured ahead: ssid=$NAME ip=$WLANIP ch=$ch if=$IF (nothing raised)"
    exit 0
  fi
fi

echo 1 2>/dev/null > "$AP_PROC/sys/net/ipv6/conf/$IF/disable_ipv6"   # quiet where there is none (2> first)
ifconfig "$IF" "$WLANIP" netmask 255.255.255.0 mtu 1500 up || {
  echo "$L could not configure $IF"; exit 1; }

# ---- bring up ---------------------------------------------------------------------------------
test -e "$AP_ROOT/tmp/bin/hostapd" || cp "$AP_ROOT/usr/sbin/hostapd" "$AP_ROOT/tmp/bin/hostapd" 2>/dev/null
# Its output folds straight into the universal log. Not -B: daemonizing points stdout at /dev/null, so
# the station events after start-up (AP-STA-CONNECTED, AP-STA-DISCONNECTED, and the IEEE 802.11 lines
# that say why a phone left) never reached box.log, and the Mac's 12 Nov re-bench couldn't see why a
# fresh session's link was lost. It runs in the foreground under setsid instead, as udhcpd does below,
# appending (O_APPEND, for bound_logs' in-place truncate); the converge step waits for it.
# Running already is asked of pidof (one fork, where `ps | grep | grep` took three ahead of the launch); it names a
# process by its own name, so an editor holding /etc/hostapd.conf open no longer counts as the AP.
# This start's own line, before the launch: the converge step waits for an AP-ENABLED after it (ap_enabled_seen).
_ae_mark="[radio] hostapd starting on $IF"
echo "$_ae_mark" 2>/dev/null >> "$AP_BOXLOG" || _ae_mark=""
_launched=0; _hapd=""; _dhcpd=""; _hrun="$AP_ROOT/etc/hostapd.conf"
if ! pidof hostapd >/dev/null 2>&1; then
  ap_follow "$AP_ROOT"
  setsid hostapd "$_hrun" </dev/null >>"$AP_BOXLOG" 2>&1 &
  _hapd=$!
  tl_stamp ap_start
  _launched=1
elif [ -f "$AP_ROOT$AP_SESSION" ]; then
  ch=$(sed -n 's/^channel=//p' "$AP_ROOT$AP_SESSION")   # up already, where it followed
fi

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
# A new AP (hostapd launched just above): any AP DHCP server still listed is the last AP's on its way out, never this
# one's (ap_dhcp_stop, above, given 1.5 s at most here). An AP already up (hostapd running) keeps the one it has, as
# before.
_dhcp_new=0
if [ "$_launched" = 1 ]; then
  ap_dhcp_stop 5 5
  _dhcp_new=1
elif [ -z "$(ap_dhcp_pids)" ]; then
  _dhcp_new=1
fi
if [ "$_dhcp_new" = 1 ]; then
  # radio_ap_dhcp.log stays a small `>`-truncated status file: this udhcpd is a setsid-backgrounded
  # foreground process, so piping its own stdout to tee would change its lifecycle. Only the readback
  # below (on failure) is universal-logged.
  # -a 250: the ARP probe before each offer waits 250 ms, not the default 2 s. It cost 1-1.9 s whenever a phone had
  # no lease (the Mac's 13 Nov brief, item 10); a short probe still catches an address in use. BusyBox 1.32.1's udhcpd
  # takes -a (the adapter's, checked on the Mac's bench); /usr/sbin/udhcpd is the vendor's v0.9.9-pre and is not this.
  setsid busybox udhcpd -f -a 250 "$AP_ROOT/etc/udhcpd.conf" </dev/null >"$AP_ROOT/tmp/radio_ap_dhcp.log" 2>&1 &
  _dhcpd=$!
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
  ap_dhcp_serving || return 1
  return 0
}
# The hostapd launched here has exited: its /proc entry is a zombie this shell has not reaped (ps and pidof still list
# it by name, so ap_serving alone would call it serving). Anything else, its being there included, is not "exited".
hapd_exited() {
  _hx_name=""; _hx_state=""
  read -r _hx_pid _hx_name _hx_state _hx_rest 2>/dev/null <"$AP_PROC/$_hapd/stat"
  [ "$_hx_name" = "(hostapd)" ] && [ "$_hx_state" = Z ]
}
# ap_follow's AP refused: its hostapd gone in its start-up (a zombie, or reaped). Never so for another AP.
hapd_refused() { [ "$_hrun" = "$AP_ROOT$AP_SESSION" ] && { hapd_exited || ! kill -0 "$_hapd" 2>/dev/null; }; }
ap_converge() {
  n=0
  # The first pass is the timeline's ap_up: the AP serving, before the confirmation below.
  while [ "$n" -lt 50 ]; do ap_serving && { tl_stamp ap_up; break; }; hapd_refused && break; n=$((n+1)); sleep 0.2; done
  # CONFIRMED ON HOSTAPD'S OWN WORD (16 Nov 2026), not on a second. In the foreground hostapd is in the process list
  # before its start-up is through, so the loop above passes a hostapd that then refuses its configuration; this used
  # to sleep a fixed second and look again. Its AP-ENABLED, in the box log after this start's own line, says it took
  # the configuration and the driver started the AP: a stronger answer, and sooner (about 0.65 s after ap_up on the
  # bench, where the second ended 1 s after it). The seam's wlan lock is held until this returns, and btd's 0x5703
  # reply to the phone waits on that lock (wifi_handoff::wait_for_ap), so a fixed second was a second the reply could
  # wait for nothing. Up to 2 s; with no AP-ENABLED by then a hostapd still running counts, as it did after the second.
  # One not launched here (running already) is confirmed as before.
  _ae_ms=""
  if [ "$_launched" = 1 ]; then
    n=0
    while [ "$n" -lt 20 ] && ! hapd_exited; do
      ap_enabled_seen && { _ae_ms=$((n * 100)); break; }
      hapd_refused && break
      n=$((n+1)); sleep 0.1
    done
  else
    sleep 1
  fi
}
ap_converge
# Refused there (a domain with no AP on it, say): the AP's own channel, as before.
if hapd_refused; then
  say "STA mode: hostapd did not start on channel $ch - the AP takes its own channel $_own"
  rm -f "$_hrun"; ch=$_own; _hrun="$AP_ROOT/etc/hostapd.conf"
  [ -n "$_ae_mark" ] && echo "$_ae_mark" 2>/dev/null >> "$AP_BOXLOG"
  setsid hostapd "$_hrun" </dev/null >>"$AP_BOXLOG" 2>&1 &
  _hapd=$!
  ap_converge
fi
# Re-test the FULL predicate after the loop: falling out of it on the bound is not success, and
# re-checking only hostapd here would let a missing address or a dead DHCP server report as up.
if ! ap_serving || { [ "$_launched" = 1 ] && hapd_exited; }; then
  echo "$L AP did not converge (ssid=$NAME ch=$ch if=$IF):"
  { [ "$_launched" = 1 ] && hapd_exited; } && echo "$L   hostapd exited during its start-up"
  ps | grep -v grep | grep -qw hostapd \
    || echo "$L   hostapd is not running"
  ifconfig "$IF" 2>/dev/null | grep -q "inet addr:$WLANIP" \
    || echo "$L   $IF does not hold $WLANIP"
  ap_dhcp_serving \
    || { echo "$L   the AP DHCP server is not running${_dhcpd:+ (the one started here, pid $_dhcpd, exited)} - a phone would associate and get no address"
         tail -3 "$AP_ROOT/tmp/radio_ap_dhcp.log" 2>/dev/null | sed "s|^|$L     |" | tee -a "$AP_BOXLOG"; }
  exit 1
fi
if [ -n "$_ae_ms" ]; then
  echo "$L AP up: ssid=$NAME ip=$WLANIP ch=$ch if=$IF dhcp=yes (AP-ENABLED ${_ae_ms} ms after it was serving)"
elif [ "$_launched" = 1 ]; then
  echo "$L AP up: ssid=$NAME ip=$WLANIP ch=$ch if=$IF dhcp=yes (hostapd running; no AP-ENABLED from it in 2 s)"
else
  echo "$L AP up: ssid=$NAME ip=$WLANIP ch=$ch if=$IF dhcp=yes"
fi
exit 0
