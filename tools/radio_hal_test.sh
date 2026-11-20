#!/usr/bin/env bash
# radio_hal_test.sh — the radio seam's cold path (20 Nov 2026), on the host: the descriptor kept across boots
# (radio_hal.sh caps, radio_detect.sh's key), the order Bluetooth and the Wi-Fi driver come up in (together on a unit
# whose own attach branch waits for nothing, today's order on every other), its two switches, its fallback and its
# guard, a host's bring-up before, during and after the boot preload, the preload's wait for the stock GPIO set-up,
# the driver tarball unpacked once, and the boot line.
#
# radio_hal.sh and radio_detect.sh run as the adapter runs them, each verb a process of its own, from copies whose
# absolute paths are moved into a scratch root (as tools/radio_detect_test.sh does). The hardware is stood in for:
# insmod, fw_loader_linux, hciattach, hciconfig, the AP layer and the process tools are stubs, first on the scripts'
# PATH (OCBM_TEST_PATH), that act on the scratch /sys and /proc and write what they did, in order, to a calls file. A
# clock process moves the scratch /proc/uptime on. Under every POSIX shell found (sh, dash), and under BusyBox's sh by
# tools/busybox_suites.sh. Nothing on the host is loaded, attached or signalled but this run's own processes.
#
#   tools/radio_hal_test.sh
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/radio_hal_test.XXXXXX")"
fails=0
passes=0
ok()   { echo "  ok   $*"; passes=$((passes + 1)); }
bad()  { echo "  FAIL $*"; fails=$((fails + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }

# Every process a case left: the stubs' daemons (by their pid files) and the clock.
reap() {
  [ -n "${R:-}" ] || return 0
  rm -f "$R/clock.on"
  for f in "$R"/state/*.pid; do [ -e "$f" ] && kill -9 "$(cat "$f")" 2>/dev/null; done
  [ -n "${CLOCK:-}" ] && { kill "$CLOCK" 2>/dev/null; wait "$CLOCK" 2>/dev/null; }
  CLOCK=""
  return 0
}
trap 'reap; if [ -n "${RH_KEEP:-}" ]; then echo "kept: $T"; else rm -rf "$T"; fi' EXIT   # RH_KEEP=1: the scratch tree stays

# ---- the stubs -----------------------------------------------------------------------------------------------------
S="$T/stub"; mkdir -p "$S"
REAL_TAR="$(command -v tar)"
cat > "$S/insmod" <<'EOF'
#!/bin/sh
# insmod FILE [args]: the module listed in /proc/modules. moal brings its interface (sta0) up WLAN_SECS in (its firmware
# download and start), with state/moal.loading there meanwhile; hci_uart registers the n_hci line discipline.
R=$STUB_R
m=${1##*/}; m=${m%.ko}
echo "insmod $m start" >> "$R/calls"
case "$m" in
  moal) : > "$R/state/moal.loading"; sleep "${WLAN_SECS:-0.6}"
        mkdir -p "$R/sys/class/net/sta0/wireless"; echo 02:00:00:00:00:01 > "$R/sys/class/net/sta0/address"
        rm -f "$R/state/moal.loading" ;;
  hci_uart) echo "n_hci 15" >> "$R/proc/tty/ldiscs" ;;
esac
echo "$m 1000 0 - Live 0x0" >> "$R/proc/modules"
echo "insmod $m end" >> "$R/calls"
EOF
cat > "$S/rmmod" <<'EOF'
#!/bin/sh
R=$STUB_R
grep -v "^$1 " "$R/proc/modules" > "$R/proc/modules.x"; mv -f "$R/proc/modules.x" "$R/proc/modules"
if [ "$1" = hci_uart ]; then grep -v '^n_hci' "$R/proc/tty/ldiscs" > "$R/proc/tty/ldiscs.x"; mv -f "$R/proc/tty/ldiscs.x" "$R/proc/tty/ldiscs"; fi
echo "rmmod $1" >> "$R/calls"
EOF
cat > "$S/fw_loader_linux" <<'EOF'
#!/bin/sh
# The Bluetooth firmware push: FW_SECS long, noting whether the Wi-Fi interface was there and moal still loading.
R=$STUB_R
i=no; [ -e "$R/sys/class/net/sta0" ] && i=yes
w=no; [ -e "$R/state/moal.loading" ] && w=yes
echo "fw start iface=$i wlan_loading=$w" >> "$R/calls"
sleep "${FW_SECS:-0.8}"
echo "Download Complete"
echo "time:${FW_OWN_MS:-2467}"
echo "fw end" >> "$R/calls"
EOF
cat > "$S/hciattach" <<'EOF'
#!/bin/sh
# hciattach: hci0 registered, up and (unless ATTACH_DEAD names this attach, by its number this boot) answering; then it
# stays, as the daemon does, until killed, and its end takes hci0 with it, as the line discipline's close does.
R=$STUB_R
n=$(( $(cat "$R/state/attaches" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$R/state/attaches"
i=no; [ -e "$R/sys/class/net/sta0" ] && i=yes
echo "attach $n iface=$i" >> "$R/calls"
echo $$ > "$R/state/hciattach.pid"
gone() { rm -rf "$R/sys/class/bluetooth/hci0"; rm -f "$R/state/hci0.up" "$R/state/hci0.answers" "$R/state/hciattach.pid"; exit 0; }
trap gone TERM INT
sleep "${ATTACH_SECS:-0.1}"
mkdir -p "$R/sys/class/bluetooth/hci0"; echo 02:00:00:00:00:02 > "$R/sys/class/bluetooth/hci0/address"
case " ${ATTACH_DEAD:-} " in
  *" $n "*) # a chip that never answers: hci0 there, never up, and the helper gives up a second on
    echo "attach $n dead" >> "$R/calls"; sleep 1; gone ;;
esac
: > "$R/state/hci0.up"; : > "$R/state/hci0.answers"
while :; do sleep 0.1; done
EOF
cat > "$S/hciconfig" <<'EOF'
#!/bin/sh
R=$STUB_R
d=${1:-}
if [ -z "$d" ]; then
  [ -d "$R/sys/class/bluetooth/hci0" ] || exit 0
  if [ -e "$R/state/hci0.up" ]; then echo "hci0: UP RUNNING"; else echo "hci0: DOWN"; fi
  exit 0
fi
[ -d "$R/sys/class/bluetooth/$d" ] || exit 1
case "${2:-}" in
  "")   if [ -e "$R/state/$d.up" ]; then echo "$d: UP RUNNING"; else echo "$d: DOWN"; fi ;;
  up)   : > "$R/state/$d.up"; echo "hciconfig $d up" >> "$R/calls" ;;
  down) rm -f "$R/state/$d.up"; echo "hciconfig $d down" >> "$R/calls" ;;
  name) if [ $# -ge 3 ]; then echo "$3" > "$R/state/$d.name"; exit 0; fi
        [ -e "$R/state/$d.up" ] && [ -e "$R/state/$d.answers" ] || exit 1
        echo "	Name: '$(cat "$R/state/$d.name" 2>/dev/null || echo X)'" ;;
esac
exit 0
EOF
cat > "$S/killall" <<'EOF'
#!/bin/sh
R=$STUB_R
sig=TERM; [ "$1" = -9 ] && { sig=KILL; shift; }
echo "killall $1" >> "$R/calls"
p=$(cat "$R/state/$1.pid" 2>/dev/null) || exit 1
kill -s "$sig" "$p" 2>/dev/null
EOF
cat > "$S/pgrep" <<'EOF'
#!/bin/sh
R=$STUB_R
[ "$1" = -x ] && shift
p=$(cat "$R/state/$1.pid" 2>/dev/null) && kill -0 "$p" 2>/dev/null && { echo "$p"; exit 0; }
exit 1
EOF
cat > "$S/ps" <<'EOF'
#!/bin/sh
R=$STUB_R
for f in "$R"/state/*.pid; do
  [ -e "$f" ] || continue; p=$(cat "$f"); n=${f##*/}
  kill -0 "$p" 2>/dev/null && echo "$p root 0 S ${n%.pid}"
done
exit 0
EOF
cat > "$S/tar" <<EOF
#!/bin/sh
echo "tar \$*" | sed 's#[^ ]*/##g' >> "\$STUB_R/calls"
exec "$REAL_TAR" "\$@"
EOF
printf '#!/bin/sh\nexec "$@"\n' > "$S/setsid"
printf '#!/bin/sh\nshift\nexec "$@"\n' > "$S/timeout"
printf '#!/bin/sh\nshift 2\nexec "$@"\n' > "$S/chrt"
for b in hcitool set_wifi_mac ifconfig; do printf '#!/bin/sh\nexit 0\n' > "$S/$b"; done
chmod 755 "$S"/*
TESTPATH="$S${OCBM_TEST_PATH:+:$OCBM_TEST_PATH}"

# ---- a scratch unit ------------------------------------------------------------------------------------------------
# The IW416 shape the dev adapter and the truck have: Carlinkit's dispatcher and attach script by their structure (not
# their text), the driver tarball, the helpers, the box's name kept, /etc/ocbm there. Its paths carry the root, as the
# path-moved scripts look for them. $1: the SDIO device id (0x9159). $2: the IW416 branch's attach lines, when not the
# stock shape.
rewrite() {  # $1 source, $2 destination: the script with its absolute paths moved into $R ("-C /tmp" too)
  sed -e "s#/tmp/#@R@/tmp/#g; s# /tmp\([ ;]\)# @R@/tmp\1#g; s# /tmp\$# @R@/tmp#" \
      -e "s#/proc/uptime#@R@/proc/uptime#g; s#/proc/modules#@R@/proc/modules#g" \
      -e "s#/proc/tty/ldiscs#@R@/proc/tty/ldiscs#g; s#/sys/#@R@/sys/#g; s#/script/#@R@/script/#g; s#/etc/#@R@/etc/#g" \
      -e "s#/lib/firmware#@R@/lib/firmware#g; s#/usr/sbin/#@R@/usr/sbin/#g; s#@R@#$R#g" "$1" > "$2"
  chmod 755 "$2"
}
unit() {
  reap
  rm -rf "$R"
  mkdir -p "$R/tmp" "$R/state" "$R/proc/tty" "$R/sys/class/net" "$R/sys/class/bluetooth" "$R/sys/class/gpio/gpio1" \
           "$R/sys/bus/sdio/devices/mmc0:0001:1" "$R/script" "$R/etc/ocbm" "$R/lib/firmware/nxp" "$R/usr/sbin" "$R/ko"
  echo "100.00 1.00" > "$R/proc/uptime"; : > "$R/proc/modules"; echo "n_tty 0" > "$R/proc/tty/ldiscs"; : > "$R/calls"
  echo 0 > "$R/sys/class/gpio/gpio1/value"
  echo "${1:-0x9159}" > "$R/sys/bus/sdio/devices/mmc0:0001:1/device"
  echo 0x02df > "$R/sys/bus/sdio/devices/mmc0:0001:1/vendor"; echo 0x00 > "$R/sys/bus/sdio/devices/mmc0:0001:1/class"
  : > "$R/ko/mlan.ko"; : > "$R/ko/moal.ko"; : > "$R/ko/hci_uart.ko"
  (cd "$R/ko" && "$REAL_TAR" -czf "$R/lib/firmware/nxp/iw416_ko.tar.gz" hci_uart.ko mlan.ko moal.ko)
  : > "$R/lib/firmware/nxp/uartiw416_bt_v0.bin"
  for b in hciattach fw_loader_linux; do : > "$R/usr/sbin/$b"; chmod 755 "$R/usr/sbin/$b"; done
  echo CarLink-a1b2 > "$R/etc/carplay_ident"
  cat > "$R/script/init_bluetooth_wifi.sh" <<EOF
test -e $R/usr/sbin/wpa_supplicant && supportStaMode=1 || supportStaMode=0
sdioCardID=\`cat /sys/bus/sdio/devices/mmc0:0001:1/device\`
if [ "\$sdioCardID" == "0xc822" ]; then
	test -e $R/tmp/88x2cs.ko && insmod $R/tmp/88x2cs.ko if2name=sta0
elif [ "\$sdioCardID" == "0x9149" ] || [ "\$sdioCardID" == "0x9141" ]; then
	nxpWiFiConfig=nxp/wifi_mod_para.conf
	tar -xvf /lib/firmware/nxp/sd8987_ko.tar.gz -C /tmp
	test -e $R/tmp/mlan.ko && insmod $R/tmp/mlan.ko
	test -e $R/tmp/moal.ko && insmod $R/tmp/moal.ko mod_para=\$nxpWiFiConfig
elif [ "\$sdioCardID" == "0x9159" ]; then
	nxpWiFiConfig=nxp/wifi_mod_para.conf
	if [ \$supportStaMode -eq 1 ]; then
		nxpWiFiConfig=nxp/wifi_mod_para_p2psta.conf
	fi
	tar -xvf /lib/firmware/nxp/iw416_ko.tar.gz -C /tmp
	test -e $R/tmp/mlan.ko && insmod $R/tmp/mlan.ko
	test -e $R/tmp/moal.ko && insmod $R/tmp/moal.ko mod_para=\$nxpWiFiConfig
fi
EOF
  iw416="${2:-		fw_loader_linux /dev/ttymxc2 115200 1 /lib/firmware/nxp/uartiw416_bt_v0.bin 3000000
		if [ -e $R/usr/sbin/hciattach ]; then
			hciattach /dev/ttymxc2 any 3000000 flow
			hciconfig hci0 scomtu 240:32
			# route sco data to hci
			hcitool -i hci0 cmd 0x3f 0x1d 0x00
		fi}"
  cat > "$R/script/attach_bluetooth.sh" <<EOF
attach_bluetooth() {
	sdioCardID=\`cat /sys/bus/sdio/devices/mmc0:0001:1/device\`
	if [ "\$sdioCardID" == "0xb822" ] || [ "\$sdioCardID" == "0xc822" ]; then
		if [ "\$sdioCardID" == "0xc822" ]; then
			while [ ! -e $R/sys/class/net/wlan0/address ]; do
				sleep 0.1
			done
		fi
		rtk_hciattach -s 115200 ttymxc2 rtk_h5
	elif [ "\$sdioCardID" == "0x9149" ] || [ "\$sdioCardID" == "0x9141" ]; then
		while [ ! -e $R/sys/class/net/wlan0/address ]; do
			sleep 0.1
		done
		if [ -e $R/sys/class/net/wlan0/address ]; then
			hciattach /dev/ttymxc2 any 3000000 flow
			hciconfig hci0 scomtu 240:32
			# route sco data to hci
			hcitool -i hci0 cmd 0x3f 0x1d 0x00
		fi
	elif [ "\$sdioCardID" == "0x9159" ]; then
$iw416
	fi
}
EOF
  rewrite "$REPO/ccpa/rootfs/script/radio_hal.sh" "$R/script/radio_hal.sh"
  # radio_detect.sh, with one line more: each detection that runs (past the function-only return) says so.
  rewrite "$REPO/ccpa/rootfs/script/radio_detect.sh" "$R/script/radio_detect.sh.0"
  awk -v r="$R" '{ print } $0 == "[ \"${RADIO_DETECT_FUNCS:-}\" = 1 ] && return 0" { print "echo run >> \"" r "/state/detects\"" }' \
    "$R/script/radio_detect.sh.0" > "$R/script/radio_detect.sh"
  rm -f "$R/script/radio_detect.sh.0"; chmod 755 "$R/script/radio_detect.sh"
  cat > "$R/script/radio_ap_up.sh" <<'EOF'
#!/bin/sh
# The AP layer: notes Bluetooth's state as it starts, runs a stand-in hostapd, and says whether the wlan lock its caller
# holds is still there as it ends (a preload that ended meanwhile must not have taken it).
R=$STUB_R
h=down; [ -e "$R/state/hci0.up" ] && [ -e "$R/state/hci0.answers" ] && h=up
echo "ap start iface=${RADIO_WLAN_IF:-} hci=$h mode=${1:-}" >> "$R/calls"
[ "${1:-}" = PREP ] && exit 0
sh -c 'echo $$ > "$1/state/hostapd.pid"; trap "exit 0" TERM; while :; do sleep 0.1; done' sh "$R" </dev/null >/dev/null 2>&1 &
sleep "${AP_SECS:-0.3}"
if [ -d "$R/tmp/.radio_lock_wlan" ]; then echo "ap lock held" >> "$R/calls"; else echo "ap lock LOST" >> "$R/calls"; fi
echo "ap end" >> "$R/calls"
EOF
  chmod 755 "$R/script/radio_ap_up.sh"
  : > "$R/clock.on"
  ( n=0; while [ -e "$R/clock.on" ]; do n=$((n + 5)); printf '%d.%02d 1.00\n' $((100 + n / 100)) $((n % 100)) > "$R/proc/uptime.x"
      mv -f "$R/proc/uptime.x" "$R/proc/uptime"; sleep 0.05; done ) &
  CLOCK=$!
}
# A boot of the same unit: what a power cut wipes (/tmp, the kernel's state, the stubs' daemons) gone, flash kept.
reboot_unit() {
  for f in "$R"/state/*.pid; do [ -e "$f" ] && kill -9 "$(cat "$f")" 2>/dev/null; done
  rm -rf "$R/tmp" "$R/state" "$R/sys/class/net/sta0" "$R/sys/class/bluetooth/hci0"
  mkdir -p "$R/tmp" "$R/state"; : > "$R/proc/modules"; echo "n_tty 0" > "$R/proc/tty/ldiscs"; : > "$R/calls"
}
# A verb, as the supervisor runs it: its output in $R/out.<verb>, its exit code in $R/rc.<verb>.
hal() {
  env OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" RADIO_ATTACH_POLLS=3 "$SH" "$R/script/radio_hal.sh" "$@" </dev/null > "$R/out.$1" 2>&1
  echo $? > "$R/rc.$1"
}
rc() { cat "$R/rc.$1" 2>/dev/null; }
said() { grep -qF -- "$2" "$R/out.$1"; }
called() { grep -qF -- "$1" "$R/calls"; }
count() { grep -cF -- "$1" "$R/calls"; }
before() { _b1=$(grep -nF -- "$1" "$R/calls" | head -1 | cut -d: -f1); _b2=$(grep -nF -- "$2" "$R/calls" | head -1 | cut -d: -f1)
           [ -n "$_b1" ] && [ -n "$_b2" ] && [ "$_b1" -lt "$_b2" ]; }
waitfor() { _w=0; while ! eval "$1" && [ "$_w" -lt "${2:-100}" ]; do _w=$((_w + 1)); sleep 0.1; done; eval "$1"; }
cap() { sed -n "s/^RADIO_$1=//p" "$R/tmp/radio_caps" | tr -d '"'; }
line() { tail -1 "$R/tmp/radio_boot" 2>/dev/null; }
key() { line | tr ' ' '\n' | sed -n "s/^$1=//p"; }
inode() { ls -i "$1" 2>/dev/null | awk '{print $1}'; }

case_kept() {
  # ---- the descriptor kept across boots -----------------------------------------------------------------------------
  echo "== $SH: the descriptor, kept across boots"
  unit
  hal probe
  K="$R/etc/ocbm/kept_radio_caps"
  check '[ "$(rc probe)" = 0 ] && [ "$(cap CAPS_FROM)" = detected ] && [ "$(grep -c . "$R/state/detects")" = 1 ]' \
    "the first boot detects (no copy yet)"
  check '[ -s "$K" ] && [ "$(tail -1 "$K")" = RADIO_KEPT_END=1 ] && grep -q "^RADIO_DETECT_KEY=" "$K" && grep -qx "RADIO_BT_TOGETHER=1" "$K"' \
    "and keeps it on flash: its key, the order, the end mark last"
  check '! grep -qE "^RADIO_(WLAN_IF|WLAN_IFS|WLAN_MAC|HCI_DEV|BT_MAC|CAPS_FROM|DETECTED_AT)=" "$K"' \
    "without the boot's own facts (no interface, no address, no time)"
  i0=$(inode "$K"); reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = kept ] && [ ! -e "$R/state/detects" ] && said probe "descriptor=kept"' \
    "the next boot takes the kept copy: radio_detect.sh does not run"
  check 'said probe "the kept copy" && [ "$(cap WLAN_INSMOD)" = "insmod $R/tmp/mlan.ko;insmod $R/tmp/moal.ko mod_para=nxp/wifi_mod_para.conf;" ]' \
    "and the boot's descriptor holds the mapping a detection gives"
  check '[ "$(inode "$K")" = "$i0" ]' "nothing written to flash on a boot that reuses it"
  echo "# a reflash" >> "$R/script/init_bluetooth_wifi.sh"; reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = detected ] && [ "$(grep -c . "$R/state/detects")" = 1 ] && said probe "another key"' \
    "the vendor's dispatcher changed (a reflash): detected again"
  check '[ "$(inode "$K")" != "$i0" ] && [ "$(sed -n "s/^RADIO_DETECT_KEY=//p" "$K" | tr -d \")" = "$(cap DETECT_KEY)" ]' \
    "and the copy rewritten under the new key"
  i1=$(inode "$K"); reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = kept ] && [ "$(inode "$K")" = "$i1" ]' "then reused again"
  echo "# a newer radio_detect.sh" >> "$R/script/radio_detect.sh"; reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = detected ]' "radio_detect.sh itself changed (an update): detected again"
  # A copy that is not one: cut short (no end mark), a line that would run something, no mapping, no wireless.
  cp "$K" "$T/kept.good"
  sed '$d' "$T/kept.good" > "$K"; reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = detected ] && said probe "not one a boot may reuse" && [ "$(tail -1 "$K")" = RADIO_KEPT_END=1 ]' \
    "a copy cut short (a power cut inside a write): detected, and kept whole again"
  { sed '$d' "$T/kept.good"; echo "RADIO_X=\"\$(touch $R/state/ran)\""; echo RADIO_KEPT_END=1; } > "$K"; reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = detected ] && [ ! -e "$R/state/ran" ]' "a line that would run something: detected, and nothing ran"
  cp "$T/kept.good" "$K"; chmod 000 "$K"; reboot_unit; hal probe; chmod 644 "$K" 2>/dev/null
  if [ "$(id -u)" != 0 ]; then
    check '[ "$(cap CAPS_FROM)" = detected ]' "a copy that cannot be read: detected"
  fi
  sed 's/^RADIO_WLAN_INSMOD=.*/RADIO_WLAN_INSMOD=""/' "$T/kept.good" > "$K"; reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = detected ]' "a mapped copy with no insmod mapping: detected"
  sed 's/^RADIO_WIRELESS=.*/RADIO_WIRELESS=no/' "$T/kept.good" > "$K"; reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = detected ] && grep -qx RADIO_WIRELESS=yes "$K"' "one that found no wireless: detected"
  echo 0x9149 > "$R/sys/bus/sdio/devices/mmc0:0001:1/device"; reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = detected ] && [ "$(cap SDIO_DEVICE)" = 0x9149 ]' "another chip on the bus (the copy moved to another unit): detected"
  echo 0x9159 > "$R/sys/bus/sdio/devices/mmc0:0001:1/device"
  rm -rf "$R/etc/ocbm"; reboot_unit; hal probe; reboot_unit; hal probe
  check '[ "$(cap CAPS_FROM)" = detected ] && [ ! -e "$K" ]' "no /etc/ocbm (no one-file install): nothing kept, every boot detects, as before"
  mkdir -p "$R/etc/ocbm"; rm -f "$R/script/init_bluetooth_wifi.sh" "$R/script/attach_bluetooth.sh"; reboot_unit; hal probe
  check '[ ! -e "$K" ] && [ "$(cap WIRELESS)" = no ]' "a unit with no bring-up found: nothing kept"
}

case_order() {
  # ---- the order: together on the IW416 shape, today's elsewhere ----------------------------------------------------
  echo "== $SH: the order Bluetooth and the Wi-Fi driver come up in"
  unit
  WLAN_SECS=2.0 FW_SECS=0.3 hal bt_on
  check '[ "$(rc bt_on)" = 0 ] && said bt_on "radio order: together"' "IW416: bt_on brings Bluetooth up, together"
  check 'before "fw start" "insmod moal end" && before "insmod moal start" "fw end" && called "fw start iface=no"' \
    "the firmware pushed while the Wi-Fi driver loads (the two overlap)"
  check 'called "attach 1 iface=no"' "the attach does not wait for the Wi-Fi interface"
  check '[ -d "$R/sys/class/net/sta0" ] && called "insmod moal end"' \
    "and bt_on returns with the Wi-Fi driver's load over, Bluetooth up long before (nothing it started still in flight)"
  check '[ "$(count "insmod mlan start")" = 1 ] && [ "$(count "insmod moal start")" = 1 ] && [ "$(count "insmod hci_uart start")" = 1 ] && [ "$(count "tar -x")" = 1 ]' \
    "each module loaded once, the tarball unpacked once"
  check '[ ! -e "$R/script/.radio_together_inflight" ] && [ ! -e "$R/script/.radio_together_failed" ]' "converged: no mark, no count left"
  check '[ "$(key order)" = together ] && [ "$(key off)" = - ] && [ "$(key by)" = bt_on ] && [ "$(key rc)" = 0 ]' \
    "its boot line: by=bt_on order=together off=- rc=0"
  check '[ "$(key btfw)" = complete ] && [ "$(key btfw_ms)" = 2467 ] && [ "$(key tries)" = 1 ] && [ "$(key fallback)" = 0 ]' \
    "  btfw=complete btfw_ms=2467 (the loader's own word and time) tries=1 fallback=0"
  check '[ "$(key btfw_start)" -lt "$(key wlan_up)" ] && [ "$(key attach)" -lt "$(key wlan_up)" ] && [ "$(key hci_up)" -ge "$(key attach)" ]' \
    "  its stamps in order: the firmware and the attach both before the Wi-Fi interface"
  # Today's order, unchanged, on the same chip switched off, on a chip whose branch waits, and on one that never waits.
  unit; : > "$R/script/radio_together_off"
  WLAN_SECS=0.5 FW_SECS=0.3 hal bt_on
  check '[ "$(rc bt_on)" = 0 ] && before "insmod moal end" "fw start" && called "fw start iface=yes wlan_loading=no" && called "attach 1 iface=yes"' \
    "switched off by the owner: today's order, the Wi-Fi interface first, then the firmware, then the attach"
  check 'said bt_on "chipset requires the WLAN driver before BT attach" && [ "$(key order)" = wlan_first ] && [ "$(key off)" = owner ]' \
    "  said so, and the boot line: order=wlan_first off=owner"
  unit 0x9149
  WLAN_SECS=0.5 hal bt_on
  check '[ "$(rc bt_on)" = 0 ] && [ "$(cap BT_TOGETHER)" = 0 ] && before "insmod moal end" "attach 1" && called "attach 1 iface=yes" && ! called "fw start"' \
    "SD8987 (its branch waits, no firmware push): today's order exactly"
  check '[ "$(key order)" = wlan_first ] && [ "$(key off)" = - ]' "  order=wlan_first off=- (the part's own order)"
  unit 0x9159 "		fw_loader_linux /dev/ttymxc2 115200 1 /x.bin 3000000
		hciattach /dev/ttymxc2 any 3000000 flow"
  printf '\tfw_loader_linux /dev/ttymxc2 115200 1 /x.bin 3000000\n\thciattach /dev/ttymxc2 any 3000000 flow\n' > "$R/script/attach_bluetooth.sh"
  hal bt_on
  check '[ "$(rc bt_on)" = 0 ] && [ "$(cap BT_AFTER_WLAN)" = 0 ] && ! called "insmod moal" && [ "$(key order)" = bt_alone ]' \
    "an attach script that never waits: Bluetooth alone, no Wi-Fi driver, as before"
}

case_switches() {
  # ---- the switches --------------------------------------------------------------------------------------------------
  echo "== $SH: the switches"
  unit; echo "guard: 2 together attaches in a row failed" > "$R/script/radio_together_off"; hal probe
  check 'said probe "order=wlan_first off=guard"' "the guard's off file: Wi-Fi first, said as the guard's"
  rm "$R/script/radio_together_off"; : > "$R/tmp/bench_together_off"; hal probe
  check 'said probe "order=wlan_first off=bench"' "the bench's, empty: Wi-Fi first"
  echo on > "$R/tmp/bench_together_off"; hal probe
  check 'said probe "order=wlan_first off=bench"' "the bench's, reading on: Wi-Fi first"
  echo " off " > "$R/tmp/bench_together_off"; hal probe
  check 'said probe "order=together off=-"' "the bench's, reading off (Carlink cannot delete it): together"
  rm "$R/tmp/bench_together_off"; : > "$R/tmp/bench_together_off"
  WLAN_SECS=0.4 FW_SECS=0.2 hal bt_on
  check '[ "$(rc bt_on)" = 0 ] && before "insmod moal end" "fw start" && [ "$(key off)" = bench ] && said bt_on "for the bench"' \
    "a bring-up with the bench's: today's order, said so, off=bench"
}

case_fallback() {
  # ---- the fallback and the guard ------------------------------------------------------------------------------------
  echo "== $SH: the fallback, and the guard"
  unit
  ATTACH_DEAD=1 WLAN_SECS=0.4 FW_SECS=0.2 hal bt_on
  check '[ "$(rc bt_on)" = 0 ] && said bt_on "together gave no responsive hci0"' "a together attach whose hci0 never answers: said plainly"
  check 'called "killall hciattach" && before "attach 1 dead" "fw start iface=yes" && called "attach 2 iface=yes"' \
    "  then today's order for the tries left: its helper stopped, the firmware again after the interface, the attach"
  check '[ -e "$R/tmp/.radio_together_fell_back" ] && [ "$(cat "$R/script/.radio_together_failed")" = 1 ]' \
    "  the boot marked, one failure counted on flash"
  check '[ "$(key order)" = together ] && [ "$(key fallback)" = 1 ] && [ "$(key tries)" = 2 ] && [ "$(key rc)" = 0 ]' \
    "  its boot line: order=together fallback=1 tries=2 rc=0"
  killall_stub() { kill "$(cat "$R/state/hciattach.pid")" 2>/dev/null; waitfor '[ ! -d "$R/sys/class/bluetooth/hci0" ]' 30; }
  killall_stub; hal probe
  check 'said probe "order=wlan_first off=fallback"' "the next attach this boot: Wi-Fi first (the fallback)"
  # The preload's together failure: its one try fails, the host's bt_on takes today's order.
  reboot_unit; rm -f "$R/script/.radio_together_failed"
  ATTACH_DEAD=1 WLAN_SECS=0.3 FW_SECS=0.2 hal preload
  check '[ "$(rc preload)" = 1 ] && [ "$(key by)" = preload ] && [ "$(key rc)" = 1 ] && ! called "attach 2"' \
    "the preload's together attach fails: no second try there (its one), its boot line rc=1"
  ATTACH_DEAD=1 hal bt_on
  check '[ "$(rc bt_on)" = 0 ] && said bt_on "a together bring-up this boot gave no responsive hci0 (the fallback)" && called "attach 2 iface=yes"' \
    "the host's bt_on then: Wi-Fi first, and Bluetooth up"
  check '[ "$(grep -c "^\[radio-boot\] v1 " "$R/tmp/radio_boot")" = 2 ] && [ "$(key by)" = bt_on ] && [ "$(key off)" = fallback ] && [ "$(key rc)" = 0 ]' \
    "  a second boot line, only because the first failed: by=bt_on off=fallback rc=0"
  # The guard: two failures in a row switch together off; a success between them clears the count.
  reboot_unit; ATTACH_DEAD=1 WLAN_SECS=0.3 FW_SECS=0.2 hal bt_on
  check '[ -e "$R/script/radio_together_off" ] && grep -q "^guard: 2 together attaches in a row" "$R/script/radio_together_off"' \
    "a second failing boot in a row: switched off by its guard (the owner's file, with guard in it)"
  check 'said bt_on "together switched OFF" && [ "$(rc bt_on)" = 0 ]' "  said so, and Bluetooth still up (the fallback)"
  reboot_unit; hal probe
  check 'said probe "order=wlan_first off=guard"' "the next boot: Wi-Fi first, until the owner removes the file"
  rm "$R/script/radio_together_off"; reboot_unit; WLAN_SECS=0.3 FW_SECS=0.2 hal bt_on
  check '[ "$(key order)" = together ] && [ "$(rc bt_on)" = 0 ] && [ ! -e "$R/script/.radio_together_failed" ]' "removed: together again"
  reboot_unit; ATTACH_DEAD=1 WLAN_SECS=0.3 FW_SECS=0.2 hal bt_on
  reboot_unit; WLAN_SECS=0.3 FW_SECS=0.2 hal bt_on
  check '[ ! -e "$R/script/.radio_together_failed" ] && [ ! -e "$R/script/radio_together_off" ]' "a failure, then a success: the count cleared, nothing switched off"
  # A together attach that never finished (power lost, or a hang): its mark found at the next.
  reboot_unit; : > "$R/script/.radio_together_inflight"; WLAN_SECS=0.3 FW_SECS=0.2 hal bt_on
  check 'said bt_on "never finished" && [ "$(key order)" = together ] && [ ! -e "$R/script/.radio_together_failed" ]' \
    "one mark left by a boot that never finished: counted, together tried, and its success clears the count"
  reboot_unit; : > "$R/script/.radio_together_inflight"; echo 1 > "$R/script/.radio_together_failed"; hal bt_on
  check '[ "$(key order)" = wlan_first ] && [ "$(key off)" = guard ] && grep -q "never finished" "$R/script/radio_together_off"' \
    "two in a row: switched off before the attach, which takes today's order"
}

case_host() {
  # ---- a host before, during and after the boot preload --------------------------------------------------------------
  # As the supervisor brings a host's radios up (session_supervisor.sh wireless_up): wifi_ap_on in the background beside
  # a bt_on, once /tmp/host_present reads 1.
  host_bringup() { echo 1 > "$R/tmp/host_present"; ( hal wifi_ap_on & ); hal bt_on; waitfor '[ -e "$R/rc.wifi_ap_on" ]' 100; }
  echo "== $SH: a host during the boot preload"
  unit
  ( WLAN_SECS=0.8 FW_SECS=1.6 hal preload ) &
  pre=$!
  waitfor 'called "insmod moal start"' 50
  t0=$(date +%s)
  # The AP's bring-up outlasts the preload, so a preload that took the AP's lock as it ended would show (ap lock LOST).
  WLAN_SECS=0.8 FW_SECS=1.6 AP_SECS=2.5 host_bringup
  wait "$pre"; t1=$(date +%s)
  check '[ "$(rc preload)" = 0 ] && [ "$(rc bt_on)" = 2 ] && [ "$(rc wifi_ap_on)" = 0 ] && [ $((t1 - t0)) -lt 12 ]' \
    "the preload converges, the host's bt_on finds Bluetooth up (rc 2), its AP comes up, nothing deadlocks"
  check '[ "$(count "insmod moal start")" = 1 ] && [ "$(count "fw start")" = 1 ] && [ "$(count "attach ")" = 1 ] && [ "$(count "tar -x")" = 1 ]' \
    "no double load: one Wi-Fi driver load, one firmware push, one attach, one unpack"
  check 'before "insmod moal end" "ap start" && before "ap start" "fw end"' \
    "the AP starts once the Wi-Fi driver is up, while the firmware is still being pushed (the preload let its wlan lock go)"
  check 'called "ap lock held" && ! called "ap lock LOST"' "and the preload, ending meanwhile, left the AP bring-up's lock alone"
  check '! called "hciconfig hci0 down"' "a host is here: the preload leaves Bluetooth up for its session"
  check '[ "$(key by)" = preload ] && [ "$(key wlan_free)" != - ] && [ "$(key wlan_free)" -lt "$(key hci_up)" ]' \
    "  the boot line: by=preload, the wlan lock free (wlan_free) before hci0 answered"
  echo "== $SH: a host before the boot preload"
  unit
  WLAN_SECS=0.5 FW_SECS=0.5 host_bringup &
  hb=$!
  waitfor 'called "insmod mlan start"' 50
  hal preload; wait "$hb"
  check '[ "$(rc bt_on)" = 0 ] && [ "$(rc wifi_ap_on)" = 0 ] && [ "$(rc preload)" = 0 ] && said preload "a host is here - left to its session"' \
    "the host's bring-up first, side by side; the preload waits its locks, then stands down"
  check '[ "$(count "insmod moal start")" = 1 ] && [ "$(count "fw start")" = 1 ] && [ "$(count "attach ")" = 1 ] && called "ap lock held"' \
    "  one load of each, the AP's lock kept"
  check '[ "$(key by)" = bt_on ] && [ "$(key order)" = together ]' "  the boot line is the host's bt_on's"
  echo "== $SH: a host after the boot preload"
  unit
  WLAN_SECS=0.4 FW_SECS=0.4 hal preload
  check '[ "$(rc preload)" = 0 ] && called "hciconfig hci0 down" && grep -q "^done " "$R/tmp/radio_preload_state"' \
    "the preload alone: up, then powered down to the between-sessions state"
  host_bringup
  check '[ "$(rc bt_on)" = 0 ] && said bt_on "was only powered down" && [ "$(rc wifi_ap_on)" = 0 ]' "the host's bt_on powers it up; the AP comes up"
  check '[ "$(count "insmod moal start")" = 1 ] && [ "$(count "fw start")" = 1 ] && [ "$(count "attach ")" = 1 ]' "  nothing loaded twice"
  check '[ "$(grep -c "^\[radio-boot\]" "$R/tmp/radio_boot")" = 1 ] && [ "$(key by)" = preload ]' "  one boot line, the preload's"
}

case_gpio() {
  # ---- the preload's wait for the stock GPIO set-up --------------------------------------------------------------------
  echo "== $SH: the stock GPIO set-up first"
  unit
  ( env OCBM_GPIO_DONE="$R/tmp/gpio_done" OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" WLAN_SECS=0.3 FW_SECS=0.2 \
      "$SH" "$R/script/radio_hal.sh" preload </dev/null > "$R/out.preload" 2>&1; echo $? > "$R/rc.preload" ) &
  pre=$!
  waitfor '[ -e "$R/tmp/.radio_ko_unpacked" ]' 100; sleep 0.3
  check '! called "insmod" && ! called "fw start" && [ -e "$R/tmp/.radio_ko_unpacked" ] && [ "$(cap CAPS_FROM)" = detected ]' \
    "before the mark: the descriptor read and the modules unpacked, nothing loaded and nothing pushed"
  echo "gpio mark" >> "$R/calls"; : > "$R/tmp/gpio_done"
  wait "$pre"
  check '[ "$(rc preload)" = 0 ] && before "gpio mark" "insmod" && [ "$(key gpio)" != - ] && said preload "for the stock GPIO set-up"' \
    "after it: the chip's bring-up, and the boot line's gpio= stamped"
  unit
  ( env OCBM_GPIO_DONE="$R/tmp/never" OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" WLAN_SECS=0.2 FW_SECS=0.2 \
      "$SH" "$R/script/radio_hal.sh" preload </dev/null > "$R/out.preload" 2>&1; echo $? > "$R/rc.preload" )
  check '[ "$(rc preload)" = 0 ] && said preload "no end mark from the stock GPIO set-up in 3 s" && [ "$(key gpio)" = - ]' \
    "no mark in 3 s: it goes on, and says so"
}

case_line() {
  # ---- the boot line, composed -----------------------------------------------------------------------------------------
  echo "== $SH: the boot line"
  unit
  printf 'start 2050\ncaps kept\ncaps_ms 61\ngpio 2330\norder together\noff -\nwlan_start 2410\nwlan_up 4460\nwlan_free 4470\nbtfw_start 2420\nbtfw_end 4930\nbtfw complete\nbtfw_ms 2467\nattach 4940\nhci_up 5390\nstart 9999\n' \
    > "$R/tmp/radio_boot_marks"
  echo 3010 > "$R/tmp/host_present_uptime"
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" "$SH" -c '. "$1"; _bt_tries=1; boot_line preload 0' sh "$R/script/radio_hal.sh"
  want='[radio-boot] v1 by=preload start=2050 caps=kept caps_ms=61 gpio=2330 order=together off=- wlan_start=2410 wlan_up=4460 wlan_free=4470 btfw_start=2420 btfw_end=4930 btfw=complete btfw_ms=2467 attach=4940 hci_up=5390 tries=1 fallback=0 rc=0 host=3010'
  check '[ "$(line)" = "$want" ] && grep -qxF "$want" "$R/tmp/box.log"' "fixed keys in a fixed order, the first stamp of each key standing; in the box log too"
  check 'line | grep -qE "^\[radio-boot\] v1( [a-z_]+=[A-Za-z0-9_-]+)+$" && ! line | grep -qE "CarLink|([0-9a-f]{2}:){5}"' \
    "one grammar, no name and no address"
  : > "$R/tmp/radio_boot_marks"; rm -f "$R/tmp/host_present_uptime"
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" "$SH" -c '. "$1"; boot_line bt_on 0' sh "$R/script/radio_hal.sh"
  check '[ "$(grep -c . "$R/tmp/radio_boot")" = 1 ]' "after a line with rc=0, no other that boot"
  rm -f "$R/tmp/radio_boot"
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" "$SH" -c '. "$1"; boot_line bt_on 1' sh "$R/script/radio_hal.sh"
  check '[ "$(line)" = "[radio-boot] v1 by=bt_on start=- caps=- caps_ms=- gpio=- order=- off=- wlan_start=- wlan_up=- wlan_free=- btfw_start=- btfw_end=- btfw=- btfw_ms=- attach=- hci_up=- tries=0 fallback=0 rc=1 host=-" ]' \
    "nothing stamped: every key there, each -"
}

case_unpack() {
  # ---- the driver tarball, unpacked once -------------------------------------------------------------------------------
  echo "== $SH: the driver tarball, unpacked once"
  unit; hal probe; : > "$R/calls"
  jobs3=""
  for n in 1 2 3; do
    ( env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" "$SH" -c '. "$1"; caps >/dev/null; ko_unpack; echo "unpacked rc=$? mlan=$([ -e "$2/tmp/mlan.ko" ] && echo yes)" >> "$2/calls"' \
        sh "$R/script/radio_hal.sh" "$R" ) &
    jobs3="$jobs3 $!"
  done
  for j in $jobs3; do wait "$j"; done
  check '[ "$(count "tar -x")" = 1 ] && [ "$(count "unpacked rc=0 mlan=yes")" = 3 ]' "three at once: one unpack, and each finds the modules whole"
  rm "$R/tmp/moal.ko"
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" "$SH" -c '. "$1"; caps >/dev/null; ko_unpack' sh "$R/script/radio_hal.sh"
  check '[ "$(count "tar -x")" = 2 ] && [ -e "$R/tmp/moal.ko" ]' "a module gone since: unpacked again"
}

case_locks() {
  # ---- the locks -------------------------------------------------------------------------------------------------------
  echo "== $SH: the seam's locks"
  unit
  mkdir "$R/tmp/.radio_lock_bt"; sleep 30 & holder=$!; echo "$holder" > "$R/tmp/.radio_lock_bt/pid"
  ( sleep 1; kill "$holder" 2>/dev/null; rm -rf "$R/tmp/.radio_lock_bt"; echo "released" >> "$R/calls" ) &
  rel=$!
  t0=$(date +%s)
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" "$SH" -c '. "$1"; lock_take bt && echo "taken" >> "$2/calls"; lock_drop bt' \
    sh "$R/script/radio_hal.sh" "$R"
  t1=$(date +%s)
  wait "$rel"; wait "$holder" 2>/dev/null
  check 'before "released" "taken" && [ $((t1 - t0)) -le 3 ]' "a held lock is waited for, and taken once it goes"
  sh -c 'exit 0' & gone=$!; wait "$gone"
  mkdir "$R/tmp/.radio_lock_bt"; echo "$gone" > "$R/tmp/.radio_lock_bt/pid"
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" "$SH" -c '. "$1"; lock_take bt && echo "taken again" >> "$2/calls"; lock_drop bt' \
    sh "$R/script/radio_hal.sh" "$R" > "$R/out.lock" 2>&1
  check 'called "taken again" && grep -q "held by dead pid $gone" "$R/out.lock"' "one whose holder is gone is reclaimed"
  mkdir "$R/tmp/.radio_lock_wlan"; echo 4242 > "$R/tmp/.radio_lock_wlan/pid"
  env RADIO_HAL_FUNCS=1 OCBM_TEST_PATH="$TESTPATH" STUB_R="$R" "$SH" -c '. "$1"; lock_drop_own wlan' sh "$R/script/radio_hal.sh"
  check '[ -d "$R/tmp/.radio_lock_wlan" ]' "lock_drop_own leaves a lock another process holds"
}

case_boot() {
  # ---- where ocbm_boot.sh starts the preload ---------------------------------------------------------------------------
  # ocbm_boot.sh itself, its paths moved into $R and the stubs first on its PATH: a gadget whose sysfs is already
  # there, ocbmd, the supervisor and the preload (radio_hal.sh) stand-ins that write when they start.
  echo "== $SH: where ocbm_boot.sh starts the preload"
  boot_run() {  # $1: the dispatcher's mark asked for (1) or not (0)
    unit
    mkdir -p "$R/sys/class/android_usb_accessory/android0" "$R/dev" "$R/usr/sbin"
    : > "$R/sys/class/android_usb_accessory/android0/enable"; : > "$R/dev/usb_accessory"; : > "$R/tmp/g_android_accessory.ko"
    printf '#!/bin/sh\necho "ocbmd" >> "%s/calls"\n' "$R" > "$R/usr/sbin/ocbmd"
    printf '#!/bin/sh\necho "supervisor" >> "%s/calls"\n' "$R" > "$R/script/session_supervisor.sh"
    printf '#!/bin/sh\necho "preload $1 gpio=${OCBM_GPIO_DONE:-}" >> "%s/calls"\n' "$R" > "$R/script/radio_hal.sh"
    chmod 755 "$R/usr/sbin/ocbmd" "$R/script/session_supervisor.sh" "$R/script/radio_hal.sh"
    printf '#!/bin/sh\nexit 0\n' > "$S/pkill"; chmod 755 "$S/pkill"
    rewrite "$REPO/ccpa/rootfs/script/ocbm_boot.sh" "$R/ocbm_boot.0"
    sed -e "s#export PATH=/usr/sbin#export PATH=$S:/usr/sbin#" -e "s#/dev/usb_accessory#$R/dev/usb_accessory#g" \
      "$R/ocbm_boot.0" > "$R/script/ocbm_boot.sh"; chmod 755 "$R/script/ocbm_boot.sh"
    if [ "$1" = 1 ]; then gd="$R/tmp/ocbm_gpio_done"; else gd=""; fi
    env OCBM_GPIO_DONE="$gd" STUB_R="$R" "$SH" "$R/script/ocbm_boot.sh" </dev/null >/dev/null 2>&1
    waitfor 'called "preload" && called "supervisor" && grep -q "remote login off" "$R/tmp/box.log"' 100
  }
  # The order ocbm_boot.sh starts things in is its own box log's (the preload runs on in the background beside them).
  logged_before() { _l1=$(grep -nF -- "$1" "$R/tmp/box.log" | head -1 | cut -d: -f1); _l2=$(grep -nF -- "$2" "$R/tmp/box.log" | head -1 | cut -d: -f1)
                    [ -n "$_l1" ] && [ -n "$_l2" ] && [ "$_l1" -lt "$_l2" ]; }
  boot_run 1
  check 'logged_before "radio preload starting" "gadget module:" && called "preload preload gpio=$R/tmp/ocbm_gpio_done"' \
    "the dispatcher marks the GPIO set-up's end: the preload starts first, before the gadget and ocbmd, told where the mark is"
  check '[ "$(count "preload preload")" = 1 ]' "  once"
  boot_run 0
  check 'logged_before "armed functions=" "radio preload starting" && [ "$(count "preload preload")" = 1 ]' \
    "no mark (another start script): after ocbmd and the supervisor, as before"
  # The same unit started again with the dispatcher's mark and /script/radio_preload_late on flash.
  : > "$R/script/radio_preload_late"; : > "$R/calls"; : > "$R/tmp/box.log"
  env OCBM_GPIO_DONE="$R/tmp/ocbm_gpio_done" STUB_R="$R" "$SH" "$R/script/ocbm_boot.sh" </dev/null >/dev/null 2>&1
  waitfor 'called "preload" && grep -q "remote login off" "$R/tmp/box.log"' 100
  check 'logged_before "armed functions=" "radio preload starting" && grep -q "radio preload starts after ocbmd and the supervisor (.*radio_preload_late)" "$R/tmp/box.log"' \
    "/script/radio_preload_late: the old start, said so"
  echo "guard: 2 together attaches in a row failed" > "$R/script/radio_together_off"; : > "$R/calls"; : > "$R/tmp/box.log"
  env OCBM_GPIO_DONE="$R/tmp/ocbm_gpio_done" STUB_R="$R" "$SH" "$R/script/ocbm_boot.sh" </dev/null >/dev/null 2>&1
  waitfor 'grep -q "remote login off" "$R/tmp/box.log"' 100
  check 'grep -q "radio order: the Bluetooth firmware beside the Wi-Fi driver DISABLED by its guard (guard: 2" "$R/tmp/box.log"' \
    "a guard's off file: said at every boot"
  rm -f "$S/pkill"
}

. "$HERE/test_shells.sh"
for SH in ${SHELLS:-$(distinct_shells sh dash)}; do
  command -v "$SH" >/dev/null 2>&1 || { echo "== $SH: not installed, skipped"; continue; }
  R="$T/$(basename "$SH").root"
  for c in ${RH_ONLY:-kept order switches fallback host gpio line unpack locks boot}; do "case_$c"; done
  reap
done
[ "$fails" -eq 0 ] && echo "radio_hal_test: all passed ($passes checks)" || { echo "radio_hal_test: $fails failed"; exit 1; }
