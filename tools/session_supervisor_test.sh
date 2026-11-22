#!/usr/bin/env bash
# session_supervisor_test.sh — the supervisor's armed-Bluetooth paths (16 Nov 2026), on the host: its functions are
# sourced (OCBM_SUP_FUNCS=1: no singleton lock, no traps, no main loop) under every POSIX shell found (sh, dash), and
# again under BusyBox's sh by tools/busybox_suites.sh. /proc, /sys and the /tmp files they read are scratch trees;
# every side effect (wireless_down, kill, setsid, env, pgrep, pkill) is a stub that writes what it was asked to a
# calls file, so nothing on the host is signalled, started or stopped.
#
#   tools/session_supervisor_test.sh
#
# Covered: the arm's gate and its off switches; the go (its guards, the go file, the timeline stamps, the signal, the
# AP launch); the go's answer and the fallback to today's path; an arm still under way at the host's arrival; the
# armed btd's end; the fork-free predicates against the grep and cat forms they replaced; the one-awk config read
# against the four functions it stands in for; the cold boot's early btd (R3-5: only when the handover is asked for,
# today's wrapper by default) and the wrapper itself.
set -u
export LC_ALL=C
HERE="$(cd "$(dirname "$0")" && pwd)"
SUP="$HERE/session_supervisor.sh"
T="$(mktemp -d "${TMPDIR:-/tmp}/session_supervisor_test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
fails=0
passes=0

# The prelude every case runs: scratch trees, the supervisor's functions, then stubs over its side effects.
cat > "$T/prelude.sh" <<'EOF'
R="$1"
export SUP_PROC="$R/proc" SUP_SYS="$R/sys" CARPLAY_CFG_FILE="$R/cfg.yaml" FLAG="$R/host_present"
export BTD_ARMED_FILE="$R/btd_armed" BTD_GO_FILE="$R/btd_go" BTD_GO_RESULT="$R/btd_go_result" BT_PHASE_FILE="$R/bt_phase"
export ARM_OFF_FILE="$R/radio_arm_off" ARM_BENCH_OFF="$R/bench_arm_off" STARTUP_MARKS="$R/startup_marks"
export RADIO_LOCKS="$R/locks" WL_TEARDOWN_MARK="$R/wl_teardown" CARPLAY_TRANSPORT_FLAG="$R/carplay_transport"
export RADIO_OFF_FLAG="$R/radio_off" PRELOAD_STATE="$R/radio_preload_state" HANDOVER_ON_FILE="$R/radio_handover_on"
export HANDOVER_BENCH_ON="$R/bench_handover_on" OCBM_SUP_FUNCS=1
export BTD_EARLY_BENCH_ON="$R/bench_btd_early" BTD_EARLY_ON_FILE="$R/etc_ocbm_bench_btd_early"
. "$2"
CALLS="$R/calls"
now() { echo "${FAKE_NOW:-100}"; }
wireless_down() { echo "wireless_down ${1:-}" >> "$CALLS"; arm_pid=""; go_pending=0; }
wireless_running() { [ "${FAKE_BTD_RUNNING:-0}" = 1 ]; }
pgrep() { return 1; }
pkill() { echo "pkill $*" >> "$CALLS"; }
kill() { echo "kill $*" >> "$CALLS"; return "${FAKE_KILL_RC:-0}"; }
setsid() { echo "setsid $*" >> "$CALLS"; }
env() {  # what it was asked, and the body after -c in $R/body
  printf 'env %s\n' "$*" >> "$CALLS"
  _e_next=0
  for _e_a in "$@"; do
    [ "$_e_next" = 1 ] && printf '%s\n' "$_e_a" > "$R/body"
    _e_next=0; [ "$_e_a" = -c ] && _e_next=1
  done
  return 0
}
sleep() { :; }
pass() { echo "PASS $*"; }
fail() { echo "FAIL $*"; }
check() { if eval "$1"; then pass "$2"; else fail "$2"; fi; }
called() { grep -qF -- "$1" "$CALLS" 2>/dev/null; }
before() { _b1=$(grep -nF -- "$1" "$CALLS" | head -1 | cut -d: -f1); _b2=$(grep -nF -- "$2" "$CALLS" | head -1 | cut -d: -f1)
           [ -n "$_b1" ] && [ -n "$_b2" ] && [ "$_b1" -lt "$_b2" ]; }
settle() { wait 2>/dev/null; return 0; }   # the stubs a backgrounded call ran (env ... &) have written
# A process in the scratch /proc: $1 pid, $2 name, $3 state.
proc() { mkdir -p "$SUP_PROC/$1"; echo "$1 ($2) $3 1 1 1" > "$SUP_PROC/$1/stat"; }
EOF

# A fresh scratch root: the uptime at 100.50 s, Bluetooth attached (hci0 there), nothing else.
fresh() {
  rm -rf "$1"; mkdir -p "$1/proc" "$1/sys/class/bluetooth/hci0" "$1/sys/bus/usb/devices" "$1/locks"
  echo "100.50 50.00" > "$1/proc/uptime"
  : > "$1/calls"
}

# Run case $2 (shell text, after the prelude) under shell $1 in a fresh root; count its PASS and FAIL lines.
run_case() {
  _rc_root="$T/root"
  fresh "$_rc_root"
  { cat "$T/prelude.sh"; printf '%s\n' "$2"; } > "$T/case.sh"
  _rc_out="$("$1" "$T/case.sh" "$_rc_root" "$SUP" 2>&1)"
  printf '%s\n' "$_rc_out" | sed -n 's/^PASS /  ok   /p; s/^FAIL /  FAIL /p'
  _rc_bad=$(printf '%s\n' "$_rc_out" | grep -c '^FAIL ')
  _rc_good=$(printf '%s\n' "$_rc_out" | grep -c '^PASS ')
  if [ "$_rc_good" -eq 0 ] && [ "$_rc_bad" -eq 0 ]; then
    echo "  FAIL the case ran no check: $(printf '%s\n' "$_rc_out" | tail -3)"
    _rc_bad=1
  fi
  fails=$((fails + _rc_bad)); passes=$((passes + _rc_good))
}

. "$HERE/test_shells.sh"
for sh in ${SHELLS:-$(distinct_shells sh dash)}; do
  command -v "$sh" >/dev/null 2>&1 || { echo "== $sh: not installed, skipped"; continue; }
  echo "== $sh: the supervisor's armed Bluetooth"

  run_case "$sh" '
uptime_now
check "[ \"\$_up_s\" = 100 ] && [ \"\$_up_ms\" = 100500 ]" "the uptime without a fork: 100.50 s is 100 s and 100500 ms"
echo "5.07 1.00" > "$SUP_PROC/uptime"; uptime_now
check "[ \"\$_up_ms\" = 5070 ]" "below 10 s too: 5070 ms"
rm "$SUP_PROC/uptime"; uptime_now
check "[ -z \"\$_up_ms\" ] && [ -z \"\$_up_s\" ]" "no uptime: empty, never a guess"
'

  run_case "$sh" '
# The four config functions and the one awk that stands in for them on the go, over the same YAML.
agree() {
  if wireless_enabled; then _w=1; else _w=0; fi
  if hot_handover_enabled; then _h=1; else _h=0; fi
  _m=$(pairing_mode); _i=$(pairing_interactive)
  cfg_snapshot
  [ "$_cs_wireless" = "$_w" ] && [ "$_cs_hot" = "$_h" ] && [ "$_cs_mode" = "$_m" ] && [ "$_cs_inter" = "$_i" ]
}
check "agree" "no config: the four functions and the snapshot agree (wireless on, Just Works)"
for yaml in "" "wireless: false" "  Wireless:   FALSE" "wireless: true" "wireless: falsehood" "# wireless: false" \
            "hot_handover: true" "Hot_Handover: True" "hot_handover: false" "pairing: numeric" "pairing:numeric_comparison" \
            "  pairing: numeric_comparison_interactive" "pairing: just_works" "pairing: Numeric_Comparison_Interactive"; do
  printf "screen: 1920x720\n%s\nother: 1\n" "$yaml" > "$CARPLAY_CFG_FILE"
  if agree; then pass "\"$yaml\": the snapshot agrees (wireless=$_w hot=$_h mode=$_m interactive=$_i)"
  else fail "\"$yaml\": the snapshot ($_cs_wireless $_cs_hot $_cs_mode $_cs_inter) is not the functions ($_w $_h $_m $_i)"; fi
done
printf "wireless: false\nhot_handover: true\npairing: numeric_comparison_interactive\n" > "$CARPLAY_CFG_FILE"
cfg_snapshot
check "[ \"\$_cs_wireless\$_cs_hot\$_cs_mode\$_cs_inter\" = 01numeric1 ]" "every key at once"
'

  run_case "$sh" '
# The predicates read with `read` now: the same answers as the grep and cat forms they replaced.
old_usb() { grep -q 05ac "$SUP_SYS"/bus/usb/devices/*/idVendor 2>/dev/null; }
old_bus() { old_usb && return 0; [ "$(cat "$SUP_SYS/class/android_usb/android0/state" 2>/dev/null)" = "CONFIGURED" ]; }
same() { a=0; b=0; c=0; d=0; wired_iphone_on_usb && a=1; old_usb && b=1; wired_phone_on_bus && c=1; old_bus && d=1
         [ "$a" = "$b" ] && [ "$c" = "$d" ]; }
check "same && ! wired_phone_on_bus" "an empty bus: no wired phone, as before"
mkdir -p "$SUP_SYS/bus/usb/devices/1-1" "$SUP_SYS/bus/usb/devices/usb1"
echo 1d6b > "$SUP_SYS/bus/usb/devices/usb1/idVendor"; echo 0bda > "$SUP_SYS/bus/usb/devices/1-1/idVendor"
check "same && ! wired_iphone_on_usb" "other vendors: none"
echo 05ac > "$SUP_SYS/bus/usb/devices/1-1/idVendor"
check "same && wired_iphone_on_usb && wired_phone_on_bus" "an iPhone (05ac) on the bus"
echo 1d6b > "$SUP_SYS/bus/usb/devices/1-1/idVendor"
mkdir -p "$SUP_SYS/class/android_usb/android0"; echo CONFIGURED > "$SUP_SYS/class/android_usb/android0/state"
check "same && ! wired_iphone_on_usb && wired_phone_on_bus" "a projected iPhone: the gadget CONFIGURED"
echo DISCONNECTED > "$SUP_SYS/class/android_usb/android0/state"
check "same && ! wired_phone_on_bus" "the gadget not configured: none"
check "! wireless_owns_session" "no transport flag: no wireless session"
printf wireless > "$CARPLAY_TRANSPORT_FLAG"; check "wireless_owns_session" "\"wireless\", written without a newline as btd and carplayd do"
printf wired > "$CARPLAY_TRANSPORT_FLAG"; check "! wireless_owns_session" "\"wired\": not a wireless session"
'

  run_case "$sh" '
proc 300 btd S; proc 301 btd Z; proc 302 sh S
check "proc_live_btd 300" "a running btd is live"
check "! proc_live_btd 301" "a zombie btd (exited, not yet reaped) is not"
check "! proc_live_btd 302" "another program is not"
check "! proc_live_btd 999" "no process is not"
arm_pid=300; echo 300 > "$BTD_ARMED_FILE"
check "btd_armed_ready && [ \"\$_ap\" = 300 ]" "armed: its file names this supervisor'"'"'s arm, a live btd"
echo 301 > "$BTD_ARMED_FILE"; check "! btd_armed_ready" "a file naming another pid is no armed btd of ours"
echo 300 > "$BTD_ARMED_FILE"; arm_pid=""; check "! btd_armed_ready" "no arm of ours: none, whatever the file says"
arm_pid=301; echo 301 > "$BTD_ARMED_FILE"; check "! btd_armed_ready" "a zombie never takes a go"
arm_pid=302; arm_since=95; check "arm_alive" "the arm wrapper (sh), 5 s in: still under way"
arm_since=60; check "! arm_alive" "the wrapper 40 s on: it is no arm any more"
arm_pid=300; arm_since=0; check "arm_alive" "the armed btd: alive whatever its age"
arm_pid=999; check "! arm_alive" "gone: not alive"
'

  run_case "$sh" '
check "! arm_off" "no switch: arming on"
: > "$ARM_BENCH_OFF"; check "arm_off" "the bench hook, empty: off"
echo off > "$ARM_BENCH_OFF"; check "! arm_off" "the bench hook reading off: on (Carlink cannot delete, so it writes off)"
echo on > "$ARM_BENCH_OFF"; check "arm_off" "the bench hook reading on: off"
rm "$ARM_BENCH_OFF"; : > "$ARM_OFF_FILE"; check "arm_off" "the owner'"'"'s switch on flash: off"
'

  run_case "$sh" '
# The gate opens with no host, an attached controller, no seam lock, no teardown, no wired phone and no btd.
arm_next=1; arm_pid=""
check "arm_gate" "everything in place: arm"
arm_next=0; check "! arm_gate" "not after a session without wireless (arm_next 0)"; arm_next=1
arm_pid=5; check "! arm_gate" "not while an arm of ours is there"; arm_pid=""
rmdir "$SUP_SYS/class/bluetooth/hci0"; check "! arm_gate" "not before Bluetooth is attached: an arm never attaches"
mkdir "$SUP_SYS/class/bluetooth/hci0"
mkdir "$RADIO_LOCKS/.radio_lock_bt"; check "! arm_gate" "not while a seam bring-up holds Bluetooth (the boot preload)"
rmdir "$RADIO_LOCKS/.radio_lock_bt"; mkdir "$RADIO_LOCKS/.radio_lock_wlan"; check "! arm_gate" "nor the WLAN lock"
rmdir "$RADIO_LOCKS/.radio_lock_wlan"
echo 99 > "$WL_TEARDOWN_MARK.99"; check "! arm_gate" "not while a teardown is in flight (the 2.6.0 ordering)"
rm "$WL_TEARDOWN_MARK.99"
mkdir -p "$SUP_SYS/bus/usb/devices/1-1"; echo 05ac > "$SUP_SYS/bus/usb/devices/1-1/idVendor"
check "! arm_gate" "not with a wired iPhone plugged in: the next session is wired, radios down"
rm "$SUP_SYS/bus/usb/devices/1-1/idVendor"
FAKE_BTD_RUNNING=1; check "! arm_gate" "not beside a btd already running"; FAKE_BTD_RUNNING=0
check "arm_gate && [ \"\$arm_next\" = 1 ]" "and open again once each is gone"
echo off > "$ARM_BENCH_OFF"; : > "$ARM_OFF_FILE"
check "! arm_gate && [ \"\$arm_next\" = 0 ]" "switched off: closed, and not asked again until a wireless session has run"
'

  run_case "$sh" '
arm_next=1
echo 7 > "$BT_PHASE_FILE"
arm_wireless; settle
check "[ \"\$arm_next\" = 0 ] && [ -n \"\$arm_pid\" ] && [ \"\$arm_since\" = 100 ]" "the arm: started once, its pid and time kept"
check "[ ! -e \"\$BT_PHASE_FILE\" ]" "a phase a previous btd left is gone before the armed one starts (none while armed)"
check "called \"env OCBM_BTD_ARM=1\"" "btd started armed (OCBM_BTD_ARM=1)"
check "called \"radio_hal.sh wifi_ap_prep\" && called \"exec /usr/sbin/btd\"" "the AP configured ahead, then btd"
check "[ -s \"\$R/body\" ] && ! grep -q -e carplayd -e hostapd \"\$R/body\" && grep -q /usr/sbin/btd \"\$R/body\"" \
  "its body names no A/V daemon and no AP daemon (the teardown patterns), only btd, whose pattern ends an arm under way"
check "called \"pkill -f carplayd\"" "a stray A/V daemon reaped first, as at a bring-up"
_u=$(grep -n "unset OCBM_BTD_WAIT" "$R/body" | cut -d: -f1); _x=$(grep -n "exec /usr/sbin/btd" "$R/body" | cut -d: -f1)
check "[ -n \"\$_u\" ] && [ \"\$_x\" = \"\$((_u + 1))\" ]" "an inherited OCBM_BTD_WAIT unset just before its btd starts (the 21 Nov review, I)"
'

  run_case "$sh" '
# The armed btd ends: Bluetooth goes down through the seam, and the next host takes today'"'"'s path.
proc 310 btd S; arm_pid=310
arm_watch
check "! called wireless_down && [ \"\$arm_pid\" = 310 ]" "alive: left alone"
echo "310 (btd) Z 1 1 1" > "$SUP_PROC/310/stat"
arm_watch
check "called \"wireless_down the armed btd ended\" && [ -z \"\$arm_pid\" ]" "ended (an exit or a crash): wireless_down, the arm forgotten"
'

  run_case "$sh" '
# The go: no fork before the signal but the config read; the inputs in the go file, the radios stamped, the AP after.
proc 320 btd S; arm_pid=320; echo 320 > "$BTD_ARMED_FILE"
printf "pairing: numeric_comparison_interactive\n" > "$CARPLAY_CFG_FILE"
usb_configured_at=88000
wireless_up; settle
check "[ \"\$(cat \"\$BTD_GO_FILE\")\" = \"\$(printf \"pid=320\nBT_PAIRING_MODE=numeric\nBT_SSP_INTERACTIVE=1\")\" ]" \
  "the go file: this btd, and the pairing inputs wireless_up gives a btd at the host'"'"'s arrival"
check "called \"kill -USR1 320\" && [ \"\$go_pending\" = 1 ] && [ \"\$go_pid\" = 320 ] && [ -z \"\$arm_pid\" ]" "SIGUSR1 to it; its answer awaited"
check "grep -qx \"t sup_radios 100500\" \"\$STARTUP_MARKS\"" "the timeline: sup_radios at the go"
check "called \"setsid sh /script/radio_hal.sh wifi_ap_on\" || { sleep_real=1; i=0; while [ \$i -lt 20 ] && ! called \"setsid sh /script/radio_hal.sh wifi_ap_on\"; do i=\$((i+1)); command sleep 0.05; done; called \"setsid sh /script/radio_hal.sh wifi_ap_on\"; }" \
  "the AP launched after the signal"
check "! called wireless_down && ! called \"OCBM_GO_FALLBACK=1\"" "no teardown, no fallback"
check "[ \"\$wl_raised\" = 1 ]" "the radios raised: no teardown may take its early return until one has run"
'

  run_case "$sh" '
# The go with its answer file unwritable (the 21 Nov review, B): its truncation was `:`, whose failed redirection ends
# the supervisor under dash and ash. The go goes on.
proc 321 btd S; arm_pid=321; echo 321 > "$BTD_ARMED_FILE"
BTD_GO_RESULT="$R/no_such_dir/btd_go_result"
wireless_up; settle
check "called \"kill -USR1 321\" && [ \"\$go_pending\" = 1 ]" "the answer file unwritable: the go still goes, the supervisor still runs"
'

  run_case "$sh" '
proc 330 btd S; arm_pid=330; echo 330 > "$BTD_ARMED_FILE"
: > "$RADIO_OFF_FLAG"
wireless_up; settle
check "called \"wireless_down radios inhibited\" && ! called \"kill -USR1\" && [ ! -e \"\$BTD_GO_FILE\" ]" \
  "CT_RADIO off at the arrival: the armed Bluetooth goes down, no go"
'

  run_case "$sh" '
proc 331 btd S; arm_pid=331; echo 331 > "$BTD_ARMED_FILE"
printf "wireless: false\n" > "$CARPLAY_CFG_FILE"
wireless_up; settle
check "called wireless_down && ! called \"kill -USR1\"" "wireless: false in the host'"'"'s config: down, no go"
'

  run_case "$sh" '
proc 332 btd S; arm_pid=332; echo 332 > "$BTD_ARMED_FILE"
mkdir -p "$SUP_SYS/bus/usb/devices/1-1"; echo 05ac > "$SUP_SYS/bus/usb/devices/1-1/idVendor"
_a2_state=0; radios_yielded_to_wired=0
wireless_up; settle
check "called \"wireless_down WIRED session active\" && ! called \"kill -USR1\"" "a wired session owns the box: down, no go"
check "[ \"\$radios_yielded_to_wired\" = 1 ] && [ \"\$_a2_state\" = 1 ]" "A2'"'"'s restore owed: the cable'"'"'s going brings the stack back"
'

  run_case "$sh" '
proc 333 btd S; arm_pid=333; echo 333 > "$BTD_ARMED_FILE"
mkdir -p "$SUP_SYS/bus/usb/devices/1-1"; echo 05ac > "$SUP_SYS/bus/usb/devices/1-1/idVendor"
printf "hot_handover: true\n" > "$CARPLAY_CFG_FILE"
wireless_up; settle
check "called \"kill -USR1 333\" && ! called wireless_down" "Hot-Handover on: the go, as wireless_up would bring it up"
'

  run_case "$sh" '
# btd gone between the check and the signal: today'"'"'s path, its stamps and AP already out with the go.
proc 334 btd S; arm_pid=334; echo 334 > "$BTD_ARMED_FILE"
FAKE_KILL_RC=1
wireless_up; settle
check "[ \"\$go_pending\" = 0 ] && called \"OCBM_GO_FALLBACK=1\"" "the signal failed: the fallback bring-up at once"
check "called \"BT_PAIRING_MODE=just_works\" && called \"exec /usr/sbin/btd\"" "the fallback is today'"'"'s wrapper (bt_on, a fresh btd)"
'

  run_case "$sh" '
# The release review of 3b38d37 (A1-2): after a go, a teardown runs even when nothing it looks at is up yet. The go
# launched the AP detached; with its btd failed and the host gone before hostapd showed, the early return left the AP
# coming up with no host. The real wireless_down here (sourced again over the stub), its commands stubbed.
. "$2"
hciconfig() { :; }
now() { echo 100; }
wireless_down; settle
check "! called \"WL_TEARDOWN=\"" "nothing raised and nothing up: the early return, as before"
wl_raised=1; : > "$CALLS"
wireless_down; settle
check "called \"WL_TEARDOWN=\" && [ \"\$wl_raised\" = 0 ]" "after a go with nothing up yet: the teardown runs, and it clears the flag"
: > "$CALLS"; wireless_down; settle
check "! called \"WL_TEARDOWN=\"" "a second, with nothing raised since: the early return again"
mkdir "$RADIO_LOCKS/.radio_lock_wlan"; : > "$CALLS"
wireless_down; settle
check "called \"WL_TEARDOWN=\"" "an AP bring-up under way (its wlan lock held): the teardown runs"
'

  run_case "$sh" '
# The go'"'"'s answer.
go_pending=1; go_pid=340; go_since=100; proc 340 btd S; p=1
wireless_up() { echo "wireless_up ${1:-}" >> "$CALLS"; }
stop_btd() { echo "stop_btd $1" >> "$CALLS"; }
go_watch
check "[ \"\$go_pending\" = 1 ] && ! called stop_btd" "no answer yet, btd alive, under 3 s: wait"
echo "ok 100520" > "$BTD_GO_RESULT"; go_watch
check "[ \"\$go_pending\" = 0 ] && ! called stop_btd && ! called wireless_up" "ok: the armed btd has the session"
go_pending=1; echo "fail no answer from the controller (timed out)" > "$BTD_GO_RESULT"; go_watch
check "[ \"\$go_pending\" = 0 ] && called \"stop_btd 340\" && called \"wireless_up fallback\"" "fail: btd stopped, today'"'"'s path"
: > "$CALLS"; rm -f "$BTD_GO_RESULT"; go_pending=1; echo "103.10 1" > "$SUP_PROC/uptime"; go_watch
check "called \"stop_btd 340\" && called \"wireless_up fallback\"" "no answer in 3 s: the fallback"
: > "$CALLS"; go_pending=1; echo "100.90 1" > "$SUP_PROC/uptime"; rm -r "$SUP_PROC/340"; go_watch
check "called \"wireless_up fallback\"" "btd gone without an answer: the fallback"
: > "$CALLS"; go_pending=1; p=0; go_watch
check "[ \"\$go_pending\" = 0 ] && ! called wireless_up" "the host gone meanwhile: no bring-up (its departure tears down)"
'

  run_case "$sh" '
# An arm still under way at the arrival: it gets its 3 s, and the go follows once it is armed.
proc 350 sh S; arm_pid=350; arm_since=99
sleep() { echo 350 > "$BTD_ARMED_FILE"; proc 350 btd S; }
wireless_up; settle
check "called \"kill -USR1 350\"" "armed during the wait: the go"
'

  run_case "$sh" '
proc 351 sh S; arm_pid=351; arm_since=99
wireless_up; settle
check "called \"wireless_down an arm that did not finish\" && [ \"\$rebring_after_teardown\" = 1 ]" \
  "still under way after 3 s: stopped, the bring-up deferred to the teardown'"'"'s end (today'"'"'s path)"
check "! called \"kill -USR1\"" "and no go to a btd that never armed"
'

  run_case "$sh" '
# The arm ended before the host came: today'"'"'s path, the arm forgotten.
arm_pid=360; arm_since=10
wireless_up; settle
check "[ -z \"\$arm_pid\" ] && called \"OCBM_GO_FALLBACK=0\" && ! called \"kill -USR1\"" "no armed btd: today'"'"'s bring-up"
check "[ \"\$wl_raised\" = 1 ]" "today'"'"'s bring-up raised the radios too: no early return until a teardown has run"
check "called \"OCBM_BTD_EARLY=0\"" "  no boot preload running: btd after bt_on, as before"
'

  run_case "$sh" '
# The cold boot (R3-5): the boot preload running, read off its last state with no fork.
check "! preload_running" "no state file (no preload this boot, or not started): not running"
echo "waiting 2.10" > "$PRELOAD_STATE"; check "! preload_running" "waiting for the self-test: not running (it stands down for a host)"
echo "running 2.30" >> "$PRELOAD_STATE"; check "preload_running" "running"
echo "done 7.51" >> "$PRELOAD_STATE"; check "! preload_running" "done: not running"
printf "stood_down 3.00\n" > "$PRELOAD_STATE"; check "! preload_running" "stood down: not running"
check "! handover_on" "no switch: the handover off (the default since 21 Nov)"
: > "$R/radio_handover_off"; check "! handover_on" "the retired /script/radio_handover_off: read by nothing, still off"
: > "$HANDOVER_BENCH_ON"; check "handover_on" "the bench hook, empty: on"
echo off > "$HANDOVER_BENCH_ON"; check "! handover_on" "the bench hook reading off: off"
rm "$HANDOVER_BENCH_ON"; : > "$HANDOVER_ON_FILE"; check "handover_on" "the owner'"'"'s opt-in on flash: on"
'

  run_case "$sh" '
# A host while the boot preload attaches Bluetooth. By default (21 Nov) today'"'"'s wrapper: bt_on, then btd, no early btd,
# whatever a retired off file says; btd first, waiting, only when the handover is asked for.
printf "waiting 2.10\nrunning 2.30\n" > "$PRELOAD_STATE"
wireless_up > "$R/sup.out"; settle
check "called \"OCBM_BTD_EARLY=0\" && ! grep -q \"btd starts now\" \"$R/sup.out\"" "the preload running, nothing asked for: today'"'"'s wrapper (OCBM_BTD_EARLY=0)"
: > "$CALLS"; : > "$R/radio_handover_off"
wireless_up > "$R/sup.out"; settle
check "called \"OCBM_BTD_EARLY=0\"" "  a leftover /script/radio_handover_off changes nothing"
: > "$CALLS"; : > "$HANDOVER_ON_FILE"
wireless_up > "$R/sup.out"; settle
check "called \"OCBM_BTD_EARLY=1\" && grep -q \"btd starts now and waits for it\" \"$R/sup.out\"" "the owner'"'"'s opt-in: btd early (OCBM_BTD_EARLY=1), said so"
rm "$HANDOVER_ON_FILE"; : > "$CALLS"; echo "on" > "$HANDOVER_BENCH_ON"
wireless_up; settle
check "called \"OCBM_BTD_EARLY=1\"" "the bench'"'"'s: btd early"
echo off > "$HANDOVER_BENCH_ON"; : > "$CALLS"
wireless_up; settle
check "called \"OCBM_BTD_EARLY=0\"" "the bench'"'"'s reading off: today'"'"'s wrapper"
: > "$HANDOVER_ON_FILE"; : > "$CALLS"
wireless_up fallback; settle
check "called \"OCBM_GO_FALLBACK=1\" && called \"OCBM_BTD_EARLY=0\"" "a failed armed go'"'"'s fallback: today'"'"'s wrapper, asked for or not"
rm "$HANDOVER_ON_FILE"; : > "$CALLS"; echo "done 7.51" >> "$PRELOAD_STATE"; : > "$HANDOVER_ON_FILE"
wireless_up; settle
check "called \"OCBM_BTD_EARLY=0\"" "the preload over: today'"'"'s wrapper, asked for or not"
'

  run_case "$sh" '
# Round 4, item 6 (bench btd_early): btd early while the boot preload runs, with the handover off. Off by default; its
# own file on flash or the bench'"'"'s in /tmp; never past the preload, nor on a failed go'"'"'s fallback.
printf "waiting 2.10\nrunning 2.30\n" > "$PRELOAD_STATE"
check "! btd_early_on" "no switch: off"
echo off > "$BTD_EARLY_BENCH_ON"; check "! btd_early_on" "the bench'"'"'s reading off: off"
: > "$BTD_EARLY_ON_FILE"; check "btd_early_on && [ \"\$_beo\" = \"$BTD_EARLY_ON_FILE\" ]" "on flash: on, and says which file"
wireless_up > "$R/sup.out"; settle
check "called \"OCBM_BTD_EARLY=1\" && grep -q \"\\[bench\\] round4: btd_early ($BTD_EARLY_ON_FILE): the handover is off - btd starts now and waits for bt_on to end\" \"$R/sup.out\"" \
  "the preload running: btd early (OCBM_BTD_EARLY=1), said so"
check "[ ! -e \"$HANDOVER_ON_FILE\" ] && [ ! -e \"$HANDOVER_BENCH_ON\" ]" "  and the handover still not asked for"
: > "$CALLS"; rm "$BTD_EARLY_ON_FILE"; echo on > "$BTD_EARLY_BENCH_ON"
wireless_up > "$R/sup.out"; settle
check "called \"OCBM_BTD_EARLY=1\" && grep -q \"round4: btd_early ($BTD_EARLY_BENCH_ON)\" \"$R/sup.out\"" "the bench'"'"'s in /tmp: the same"
: > "$CALLS"; : > "$HANDOVER_ON_FILE"
wireless_up > "$R/sup.out"; settle
check "called \"OCBM_BTD_EARLY=1\" && ! grep -q round4 \"$R/sup.out\"" "the handover asked for as well: its early btd, no round-4 line"
rm "$HANDOVER_ON_FILE"; : > "$CALLS"
wireless_up fallback; settle
check "called \"OCBM_BTD_EARLY=0\"" "a failed armed go'"'"'s fallback: today'"'"'s wrapper"
: > "$CALLS"; echo "done 7.51" >> "$PRELOAD_STATE"
wireless_up > "$R/sup.out"; settle
check "called \"OCBM_BTD_EARLY=0\" && ! grep -q round4 \"$R/sup.out\"" "the preload over: today'"'"'s wrapper"
'

  run_case "$sh" '
# The wrapper itself, run with its paths moved into the scratch root: stand-ins for radio_hal.sh (bt_on takes 0.4 s) and
# for btd (it says how it was started, and when it saw the file that says bt_on is over).
mkdir -p "$R/tmp" "$R/script" "$R/usr/sbin"
printf "#!/bin/sh\necho \"radio_hal \$1 start\" >> \"%s/calls\"\n[ \"\$1\" = bt_on ] && sleep 0.4\necho \"radio_hal \$1 end\" >> \"%s/calls\"\n" "$R" "$R" > "$R/script/radio_hal.sh"
printf "#!/bin/sh\necho \"btd start wait=\${OCBM_BTD_WAIT:-none} mode=\${BT_PAIRING_MODE:-}\" >> \"%s/calls\"\n_n=0\nwhile [ \$_n -lt 30 ] && [ ! -e \"%s/tmp/.btd_bt_on_done\" ]; do _n=\$((_n + 1)); sleep 0.1; done\n[ -e \"%s/tmp/.btd_bt_on_done\" ] && echo \"btd saw bt_on done\" >> \"%s/calls\"\n" "$R" "$R" "$R" "$R" > "$R/usr/sbin/btd"
chmod 755 "$R/script/radio_hal.sh" "$R/usr/sbin/btd"
printf "waiting 2.10\nrunning 2.30\n" > "$PRELOAD_STATE"
wireless_up; settle
sed -e "s#/tmp/#$R/tmp/#g; s#/proc/uptime#$R/proc/uptime#g; s#/script/#$R/script/#g; s#/usr/sbin/btd#$R/usr/sbin/btd#g" "$R/body" > "$R/body.run"
: > "$CALLS"; echo "100.50 1.00" > "$R/proc/uptime"
command env BT_PAIRING_MODE=just_works BT_SSP_INTERACTIVE=0 OCBM_WL_REAPED=0 OCBM_USB_EDGE= OCBM_GO_FALLBACK=0 \
  OCBM_BTD_EARLY=1 sh "$R/body.run"
i=0; while [ $i -lt 40 ] && ! called "btd saw bt_on done"; do i=$((i + 1)); command sleep 0.1; done
check "called \"btd start wait=1 mode=just_works\" && before \"btd start wait=1\" \"radio_hal bt_on end\"" \
  "early: btd started waiting (OCBM_BTD_WAIT=1, the session'"'"'s inputs), before bt_on was over"
check "before \"radio_hal bt_on end\" \"btd saw bt_on done\" && [ -e \"$R/tmp/.btd_bt_on_done\" ]" \
  "  the file that says bt_on is over, written once it is"
check "grep -qx \"t sup_radios 100500\" \"$R/tmp/startup_marks\"" "  the timeline: sup_radios before both"
: > "$CALLS"; rm -f "$R/tmp/.btd_bt_on_done"
command env BT_PAIRING_MODE=just_works BT_SSP_INTERACTIVE=0 OCBM_WL_REAPED=0 OCBM_USB_EDGE= OCBM_GO_FALLBACK=0 \
  OCBM_BTD_EARLY=0 sh "$R/body.run"
check "before \"radio_hal bt_on end\" \"btd start wait=none\" && ! called \"btd saw bt_on done\" && [ ! -e \"$R/tmp/.btd_bt_on_done\" ]" \
  "today'"'"'s: bt_on, then btd (no wait asked, no file)"
: > "$CALLS"
command env BT_PAIRING_MODE=just_works BT_SSP_INTERACTIVE=0 OCBM_WL_REAPED=0 OCBM_USB_EDGE= OCBM_GO_FALLBACK=0 \
  OCBM_BTD_EARLY=0 OCBM_BTD_WAIT=1 sh "$R/body.run"
check "called \"btd start wait=none\"" "  an OCBM_BTD_WAIT=1 the supervisor inherited: unset, this btd does not wait (the 21 Nov review, I)"
'
done
[ "$fails" -eq 0 ] && echo "session_supervisor_test: all passed ($passes checks)" || { echo "session_supervisor_test: $fails failed"; exit 1; }
