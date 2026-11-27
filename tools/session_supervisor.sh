#!/bin/sh
# session_supervisor.sh — docs/carplay/02_SESSION_LIFECYCLE.md lifecycle state-machine ACTOR (box side). Launched at boot by
# ocbm_boot.sh and then IDLE-WAITS: post-boot the box does nothing to the phone until a HOST APP
# commands it. It reads ocbmd's presence signal /tmp/host_present and:
#
#   host PRESENT (1) -> go projection-ready + ARM:
#        projection_up.sh (iAP2 handshake -> Identified, if not already) THEN carplayd (which now
#        carries mDNS discovery in-process; the separate rx-connect binary was merged into it 2026-09-08).
#   host GONE   (0) -> TEARDOWN: kill carplayd -> HOLDING PATTERN (iap2d stays up).
#
# ocbmd supplies the one grace before it ever moves the flag: HEARTBEAT_GRACE=10s (beat loss), in
# ccpa/ocbmd/src/main.rs; a blip inside it never reaches here. A clean CT_STOP gets NO grace (changed
# 2026-09-03, was STOP_GRACE=5s) — it drops the flag at once so this loop runs a full teardown. Since
# we sample at 1 Hz and act on EDGES, ocbmd holds that 0 for REARM_HOLD=2s before honouring a
# re-SUBSCRIBE, so a quit->relaunch inside one tick cannot hide the GONE edge from us.
# NOTE: this file defines no heartbeat constant of its own — it is edge-triggered on the flag.
#
# P0 lifecycle hardening (docs/carplay/02_SESSION_LIFECYCLE.md,§2 — tasks #22,#23): the supervisor no longer treats "carplayd
# process alive" as healthy. It derives a real HEALTH signal from establishment milestones in
# carplayd.log (pair-verify OK -> RECORD = Apple's "session established" edge), publishes it to
# /tmp/session_healthy, and tracks STUCK counters (establishment stalls + presence flaps) that SURVIVE
# teardown and reset only after a session has been confirmed-established for a hold period. This closes
# the docs/carplay/02_SESSION_LIFECYCLE.md stall, where "ARMED + carplayd-alive" masked a session that never streamed and the one
# counter was wiped by teardown() on every GONE edge. The phone_reset escalation LADDER that ACTS on
# health=STUCK lands in task #24; here we detect, publish, bound the retry, and log the verdict.
#
# BENCH LEVER (2026-09-03): if /tmp/carplay_btmon exists (or CARPLAY_BTMON=1) AND `btmon` is on
# PATH, wireless_up() starts `btmon` for the wireless session's HCI traffic, prefixed "[btmon] "
# into /tmp/box.log, and wireless_down() kills it at teardown. Opt-in and harmless when btmon is
# absent (unknown on the CCPA rootfs) -- see btmon_start()/the "[b]tmon" pkill lines.
set -u
export PATH=${OCBM_TEST_PATH:+$OCBM_TEST_PATH:}/usr/sbin:/usr/bin:/sbin:/bin:$PATH

# tools/session_supervisor_test.sh sources this file for its functions alone (OCBM_SUP_FUNCS=1): no singleton lock,
# no traps, and it returns before the main loop.
if [ "${OCBM_SUP_FUNCS:-}" != 1 ]; then
# S2 singleton guard: mirrors the mkdir test-and-set + PID-liveness reclaim idiom in
# ccpa/rootfs/script/radio_hal.sh:84-99 (lock_take), so a second supervisor invocation (e.g. a
# stray manual relaunch) never runs concurrently with a live one and races it over /tmp state.
_sup_lock=/tmp/.sup_lock
while ! mkdir "$_sup_lock" 2>/dev/null; do
  _sup_holder=$(cat "$_sup_lock/pid" 2>/dev/null)
  if [ -n "$_sup_holder" ] && [ ! -d "/proc/$_sup_holder" ]; then
    rm -rf "$_sup_lock"; continue
  fi
  echo "[supervisor] already running pid=${_sup_holder:-unknown} — exiting" >> /tmp/box.log
  exit 0
done
# ocbmd sends SIGUSR1 when a host arrives, to cut the 1 s tick short (the main loop's wait). Set before the pid is
# published, so the signal never finds the default action (which would end this process). It records the wake too:
# one that lands while the tick is still working, outside its closing wait, skips that wait instead of being lost
# (the Mac's review of 376c4ce, SUP-WAKE-LOST).
woken=0
trap 'woken=1' USR1
echo $$ > "$_sup_lock/pid" 2>/dev/null
trap 'rm -rf "$_sup_lock" 2>/dev/null' EXIT
trap 'rm -rf "$_sup_lock" 2>/dev/null; trap - EXIT; exit 143' INT TERM HUP
fi

FLAG=${FLAG:-/tmp/host_present}
AL=/tmp/carplayd.log
HEALTHY=/tmp/session_healthy
STATE=/tmp/carplay_state          # canonical one-glance lifecycle verdict (task #26)
RING=/tmp/lifecycle.ndjson        # per-transition history ring, uptime-stamped (task #30)
PEER_PENDING=/tmp/peer_pending    # deferred peer-store mutation to apply at the idle boundary (#25)

# --- tunables (seconds / counts). Milestone-aware and deliberately generous so a slow cold pair-setup
# (the human "Allow CarPlay" tap can take tens of seconds, BEFORE pairing) is never falsely torn down.
# The fast presence-flap detector (below) catches the docs/carplay/02_SESSION_LIFECYCLE.md signature quickly regardless. See docs/carplay/02_SESSION_LIFECYCLE.md
# guardrail. Values are unvalidated against measured hardware latency — tune on the box.
ESTAB_CONNECT_GRACE=90   # ARMED -> must reach pair-verify within this (covers the human Allow tap)
ESTAB_STREAM_GRACE=30    # after pair-verify -> must reach RECORD within this (RECORD follows quickly)
CONFIRM_HOLD=15          # a session must stay established this long before STUCK counters are cleared
FLAP_N=5                 # presence 0->1 edges ...
FLAP_WINDOW=20           # ... within this window => flapping (the docs/carplay/02_SESSION_LIFECYCLE.md signature)
STALE_FLAG_N=3           # consecutive ticks (≈s) a "wireless" transport flag may persist with NO
                         # btd AND no carplayd alive before the stale-flag watchdog clears
                         # it (dual-transport self-heal of a crashed/SIGKILLed wireless session)
# escalation ladder (task #24 / docs/carplay/02_SESSION_LIFECYCLE.md):
L1_AT=2                  # establishment stalls before the first phone_reset (L1)
L1_MAX=2                 # max L1 (phone_reset) attempts before escalating to L2
L2_AT=4                  # establishment stalls that force L2 regardless of L1 count
L2_MAX=2                 # max L2 (full daemon restart) before parking IDLE (L3 reboot = task #28)
PROJ_AT=3                # consecutive projection bring-up failures before escalating to L1 (phone_reset)
L3_MAX_REBOOTS=3         # consecutive L3 reboots without a good session before parking (persistent budget)
REBOOT_BUDGET=/etc/ccpa_reboot_count   # persists across reboots (jffs2); a plain count, no RTC needed

armed=0
fails=0                  # consecutive carplayd-death re-ARMs, for backoff
proj_fails=0             # consecutive projection_up.sh failures (pre-ARM), for the #24 ladder fix
backoff_until=0          # epoch(uptime) seconds; while now < this, do not re-ARM
tick=0
stale_flag_ticks=0      # consecutive ticks the transport flag looked orphaned (stale-flag watchdog, C)
last_p=""               # previous DEFINITE presence read, for 0->1 edge detection

# per-session establishment state (reset on ARM / TEARDOWN)
saw_paired=0
saw_record=0
healthy=0
estab_deadline=0
established_since=0

# cross-session STUCK state — INTENTIONALLY survives teardown(); reset only on a confirmed-established
# session (docs/carplay/02_SESSION_LIFECYCLE.md; fixes the teardown() counter-wipe that made the docs/carplay/02_SESSION_LIFECYCLE.md flap uncountable).
stuck=0
stuck_fails=0
edge_ts=""              # space-list of recent present 0->1 edge timestamps (uptime secs)
edge_count=0
l1_tries=0             # phone_reset (L1) attempts this stuck episode
l2_tries=0             # full-daemon-restart (L2) attempts this stuck episode
stuck_reason=""        # last STUCK reason string, surfaced in /tmp/carplay_state
last_state=""          # signature of the last /tmp/carplay_state write (change-detection)
last_phase=""          # last published phase, for the transition ring (#30)

now() { cut -d. -f1 /proc/uptime; }
# The uptime without a fork, for the paths a host's arrival takes (each fork is about 15 ms on this CPU, and the
# armed go has about 150 ms in all): _up_s whole seconds, _up_ms the ms to the 10 ms /proc/uptime gives. Empty when
# unreadable.
SUP_PROC=${SUP_PROC:-/proc}   # tools/session_supervisor_test.sh points these at scratch trees
SUP_SYS=${SUP_SYS:-/sys}
uptime_now() {
  _up_raw=""; _up_s=""; _up_ms=""
  read -r _up_raw _up_idle 2>/dev/null <"$SUP_PROC/uptime"
  case "$_up_raw" in *.*) _up_s=${_up_raw%.*}; _up_ms="${_up_raw%.*}${_up_raw#*.}0" ;; esac
}

PHONE_FLAG=/tmp/phone_present     # phone-on-bus signal for ocbmd -> host SEV_PHONE_* (2026-07-12)
phone_flag_state=""
phone_waiting=0

# Is a GENUINE wired iPhone physically enumerated on the USB bus right now — Apple VID 05ac, NORMAL mode
# (before any role switch)? This is the raw bus probe projection_up.sh relies on (projection_up.sh:24),
# expressed against sysfs (the same 05ac idVendor match phone_on_bus has always used), and it is
# INDEPENDENT of /tmp/carplay_transport — so the preempt can see a real cable even while a stale flag
# still says "wireless". A wireless phone is on Wi-Fi and never on this bus, so this is ALWAYS false for a
# wireless-only session (the safety property preempt_wireless_for_wired relies on). Note: once a phone
# role-switches into projection it stops enumerating as 05ac (projection_up.sh:17), so this matches a
# FRESHLY-plugged cable — exactly the preempt trigger — not an already-projected wired session.
# Read with the shell's own `read`, not `grep` (16 Nov 2026): the armed go asks it, and a fork is about 15 ms here.
# The same answer: an idVendor holding 05ac anywhere in it.
wired_iphone_on_usb() {
  for _wiu_f in "$SUP_SYS"/bus/usb/devices/*/idVendor; do
    _wiu_v=""
    read -r _wiu_v 2>/dev/null <"$_wiu_f"
    case "$_wiu_v" in *05ac*) return 0 ;; esac
  done
  return 1
}

# Is an iPhone attached to the phone-facing port? TWO modes (the role switch flips them):
#   normal mode  — the iPhone is a 05ac USB DEVICE on the box's host controller (sysfs idVendor);
#   projected    — after the 0x51 role switch the iPhone is the USB HOST and the box is a gadget,
#                  so it VANISHES from the device list; presence = the phone-facing gadget
#                  (/sys/class/android_usb/android0, ci_hdrc.0) being CONFIGURED by it.
# Checking only the device list caused the 2026-07-12 deadlock: a projected phone read as "absent",
# the app showed a false "waiting for phone", and after a stall the gate blocked re-arm forever.
phone_on_bus() {
  # (B) Check the RAW USB bus for a genuine wired iPhone FIRST, so a live-wireless flag can never MASK a
  # real cable. This ordering is what unblinds the wired preempt: previously the `wireless_owns_session`
  # short-circuit below returned before the 05ac check ever ran, so a wired iPhone plugged in during (or
  # after a stale) wireless was structurally invisible. Flag-independent, so it still detects the cable
  # while the flag says "wireless".
  wired_iphone_on_usb && return 0
  # A live WIRELESS session means the phone IS present — just over Wi-Fi, not USB (#87). Without this,
  # the USB checks report "phone absent" during a wireless session, which ocbmd relays to the host app as
  # SEV_PHONE_ABSENT — making the app's watchdog blank the video / say "waiting for phone" on a perfectly
  # live wireless screen. (Kept below the wired check so it can no longer mask a real cable.)
  wireless_owns_session && return 0
  wired_phone_on_bus
}

# The wired half of phone_on_bus: a cable iPhone on the raw bus, or the phone-facing gadget CONFIGURED by
# a projected one. It never reads /tmp/carplay_transport, so a caller that has just read that flag itself
# (A2 and wireless_up's choke point: `! wireless_owns_session && wired_phone_on_bus`) gets ONE reading of
# it per decision. Both used to ask `phone_on_bus && ! wireless_owns_session`, which reads the flag twice:
# on the 2026-11-02 bench a phone switch cleared it between the two reads, so `phone_on_bus` answered true
# through its wireless clause and the negation passed as well. A2 then saw a wired session that did not
# exist, tore the radios down, restored them on the next tick, and that restart lost btd's switch hold.
wired_phone_on_bus() {
  wired_iphone_on_usb && return 0
  _wpb_s=""
  read -r _wpb_s 2>/dev/null <"$SUP_SYS/class/android_usb/android0/state"
  [ "$_wpb_s" = "CONFIGURED" ]
}

# Publish the phone-presence flag atomically on transitions (ocbmd mirrors it to the host app so
# "waiting for phone" is truthful and immediate, not a 20 s watchdog).
write_phone_flag() {  # $1 = 0|1
  [ "$1" = "$phone_flag_state" ] && return
  phone_flag_state="$1"
  printf '%s' "$1" > "$PHONE_FLAG.tmp" 2>/dev/null && mv "$PHONE_FLAG.tmp" "$PHONE_FLAG" 2>/dev/null
}

write_healthy() {  # $1 = 0|1 — atomic publish of the health signal for other consumers
  printf '%s' "$1" > "$HEALTHY.tmp" 2>/dev/null && mv "$HEALTHY.tmp" "$HEALTHY" 2>/dev/null
}

# write_state — publish the canonical one-glance lifecycle verdict /tmp/carplay_state (task #26).
# Edge-triggered: only rewrites when the meaningful signature changes (the uptime is excluded from the
# signature so a healthy session doesn't churn the file every tick), so overhead is a few writes per
# transition on the tmpfs, never per frame/tick. Read it with carplay-status.
write_state() {   # $1 = phase, $2 = reason
  _sig="$1|$armed|$healthy|$stuck|$saw_paired|$saw_record|$edge_count|$stuck_fails|$l1_tries|$l2_tries|$fails|${last_p:-?}|$2"
  [ "$_sig" = "$last_state" ] && return
  last_state="$_sig"
  {
    echo "phase=$1"
    echo "host_present=${last_p:-?}"
    echo "armed=$armed healthy=$healthy stuck=$stuck"
    echo "paired=$saw_paired record=$saw_record"
    echo "flaps=$edge_count stuck_fails=$stuck_fails l1=$l1_tries l2=$l2_tries fails=$fails"
    echo "reason=$2"
    echo "uptime=$(now)"
  } > "$STATE.tmp" 2>/dev/null && mv "$STATE.tmp" "$STATE" 2>/dev/null
}

# Kill any running session daemons (idempotent). iap2d/iAP2 are deliberately left up (holding pattern).
# True while a live WIRELESS session owns carplayd (carplayd writes /tmp/carplay_transport=wireless on
# its control connection). The wired supervisor must NOT touch carplayd in that case — the
# critical QC finding: the old unconditional kill SIGKILLed a healthy wireless projection the moment a
# USB phone appeared, and the establishment-stall ladder then escalated to REBOOTING the box at it.
CARPLAY_TRANSPORT_FLAG=${CARPLAY_TRANSPORT_FLAG:-/tmp/carplay_transport}
wireless_owns_session() {
  # `read`, not `$(cat ...)`: no fork (the armed go asks it). btd and carplayd write the flag as one word.
  _wos_t=""
  read -r _wos_t 2>/dev/null <"$CARPLAY_TRANSPORT_FLAG"
  [ "$_wos_t" = "wireless" ]
}

# A running carplayd is the WIRED arm's only while that arm is ARMED or carplayd says its peer is wired
# (its own carplay_transport write). Any other is the wireless one, idle once its session has ended:
# carplayd clears the transport flag then, so wireless_down, which spared every carplayd unless
# wireless_owns_session, left it running with no app (bench 2026-11-02: a carplayd from 11:16 outlived
# the 11:19 host-gone teardown, and its deleted binary held 1.1 MB through a stock restore).
wired_carplayd_live() {
  [ "$armed" = 1 ] || [ "$(cat /tmp/carplay_transport 2>/dev/null)" = "wired" ]
}

# The single projection OWNER (box_common::flags model, /tmp/projection_owner). Prefers the unified flag
# and falls back to the legacy wireless-only /tmp/carplay_transport, so it is byte-compatible with the
# existing wireless arbitration above.
# Claim/release the unified owner flag for a WIRED CARPLAY session, so `projection_owner()` (and
# ocbmd's CT_PROJ_MODE) can tell "idle" from "CarPlay is streaming".
claim_carplay_owner() { echo wired-cp > /tmp/projection_owner 2>/dev/null; }
# Only ever clears OUR token, so it can't stomp a wireless claim.
release_carplay_owner() {
  [ "$(cat /tmp/projection_owner 2>/dev/null)" = "wired-cp" ] && rm -f /tmp/projection_owner 2>/dev/null
  return 0
}

projection_owner() {
  o=$(cat /tmp/projection_owner 2>/dev/null)
  case "$o" in
    wired-cp)
      # Liveness self-heal: if the flag claims CarPlay owns the box but its daemons are gone (crash,
      # missed release), the claim is stale — clear it and fall through.
      #
      # `iap2d` ALONE, not `carplayd || iap2d` (F2, device-proven 2026-08-27). WIRED CarPlay IS the
      # Identified iAP2 link, and that link is iap2d; carplayd is NOT evidence of one. This very
      # function's caller deliberately leaves carplayd running when the host goes away
      # ("carplayd are wired-owned, left running"), and carplayd is shared with the
      # wireless arm — so an `||` here means a surviving carplayd keeps a dead session's `wired-cp`
      # claim alive FOREVER. Measured: unplug the iPhone mid-session -> iap2d exits, carplayd stays,
      # flag stays `wired-cp` with an EMPTY bus; the box then reported "wired CarPlay" to the app with
      # nothing plugged in.
      if pgrep iap2d >/dev/null 2>&1; then echo wired-cp; return; fi
      rm -f /tmp/projection_owner 2>/dev/null
      ;;
    wireless) echo "$o"; return ;;
  esac
  [ "$(cat /tmp/carplay_transport 2>/dev/null)" = "wireless" ] && { echo wireless; return; }
  echo ""
}

kill_session() {
  if wireless_owns_session; then
    echo "[sup] kill_session suppressed — a live wireless session owns carplayd"
    return 0
  fi
  pkill -f carplayd 2>/dev/null
  # brief SIGKILL fallback so a stuck carplayd can't double-bind :5000 on the next ARM
  sleep 1
  pkill -9 -f carplayd 2>/dev/null
  # A killed carplayd cannot clear the phone identity its session published; with no session the file
  # names no live phone, and a host connecting next must not be told one is (bench 2026-11-02).
  rm -f /tmp/phone_identity
  release_carplay_owner
}

# (A) Preempt a wireless-owned session when a GENUINE wired iPhone is physically on the USB bus. Called
# ONLY from the main loop's `wireless_owns_session && wired_iphone_on_usb` guard, so it can NEVER fire
# during a healthy wireless-ONLY session: a wireless phone is on Wi-Fi, not the USB bus, so
# wired_iphone_on_usb() (a raw 05ac sysfs probe, flag-independent) is false for it. SIGTERM
# btd so its own teardown_av_layer() (av.rs) clears the transport flag + AV_LAYER_UP latch
# and reaps the wireless carplayd, then WAIT (bounded ~5s) for the flag to actually clear
# before returning, so the caller's arm() takes the wired path cleanly. If btd is already
# gone (a stale flag with no owner), clear the flag directly so the wired takeover is never blocked.
preempt_wireless_for_wired() {
  echo "[sup] PREEMPT: genuine wired iPhone on USB while transport=wireless -> SIGTERM btd, switching to wired"
  # S3: bracket form, as :881/:914 already use, so this pkill doesn't also hit an unrelated wrapper
  # whose argv contains the literal "/usr/sbin/btd" (e.g. the radio_hal.sh bt_on child),
  # which would orphan it.
  pkill -f "[/]usr/sbin/btd" 2>/dev/null
  _i=0
  while [ "$_i" -lt 25 ]; do
    wireless_owns_session || { echo "[sup] PREEMPT: transport flag cleared -- proceeding to wired arm"; return 0; }
    _i=$((_i + 1)); sleep 0.2
  done
  echo "[sup] PREEMPT: flag still 'wireless' after ~5s wait -- clearing stale flag directly so wired can arm"
  rm -f /tmp/carplay_transport
}

arm() {
  if wireless_owns_session; then
    echo "[sup] ARM suppressed — a live wireless session owns carplayd (dual-transport, first-come-wins)"
    return 1
  fi
  echo "[sup] host PRESENT -> go projection-ready + ARM"
  /script/projection_up.sh; _pu=$?
  if [ "$_pu" = 2 ]; then
    # Exit 2 = the BOX is missing something (projection_up's prerequisite check), not a phone fault.
    # Propagated so the caller can refuse to count it toward the ladder — see the PROJ_ENV branch.
    echo "[sup] projection bring-up failed on a BOX MISCONFIGURATION — not arming, not escalating"
    return 2
  fi
  if [ "$_pu" != 0 ]; then
    echo "[sup] projection bring-up failed — not arming (will retry while present)"
    return 1
  fi
  kill_session   # idempotent: never leak a duplicate carplayd on re-ARM
  { true >"$AL"; } 2>/dev/null   # carplayd's in-process discovery thread logs here too (no separate rx-connect log since 2026-09-08). Not `:`, whose failed redirection would end the supervisor
  # docs/wireless/00_WIRELESS_CARPLAY.md: the wireless-metadata experiment (CARPLAY_WIRELESS_METADATA) is set ONLY at the wireless
  # spawn site (crates/vendor/wireless/src/av.rs::ensure_av_layer) — this is the WIRED launcher, and it
  # deliberately does NOT set that var: info.rs/session.rs's iAPChannelInfo/enabledFeatures echoes it
  # gates are NOT wireless-scoped (only events.rs's actual tunnel send is), so setting it here would
  # silently change the proven wired session's /info + SETUP-response bytes with zero live wired testing.
  # cornerMask experiment (Phase 1): OPT-IN via the on-box flag file /tmp/cornermask_test (tmpfs, so a
  # reboot disarms it — mirrors the /tmp/carplay_metadata lever philosophy). When present, arm the
  # SETUP `enabledFeatures`/viewAreas `cornerMasks` advertisement (CARPLAY_CORNERMASKS) and dump any
  # `topLeftCornerMask` the phone streams (CARPLAY_CORNERMASK_CAPTURE=/tmp). Absent = byte-identical to
  # the proven wired spawn. Re-arm after a reboot with: echo 1 > /tmp/cornermask_test
  CM=
  [ -f /tmp/cornermask_test ] && CM="CARPLAY_CORNERMASKS=1 CARPLAY_CORNERMASK_CAPTURE=/tmp"
  # logTransfer Tier-1 experiment (docs/carplay/04_CAPABILITIES_AND_CONFIG.md Half A): OPT-IN via /tmp/logtransfer_test, same tmpfs
  # philosophy as the cornermask flag above. Arms `logTransferInfo` in /info + the `logTransfer`
  # enabledFeatures echo. Absent = byte-identical to the proven wired spawn.
  LT=
  # SETUP_DUMP rides the same flag: each raw SETUP request plist lands at /tmp/setup_req.N, pinning
  # the feature-token array the iPhone actually proposes (the enabledFeatures intersection input —
  # a token we advertise but the phone never proposes can never appear negotiated; docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
  [ -f /tmp/logtransfer_test ] && LT="CARPLAY_LOGTRANSFER=1 CARPLAY_SETUP_DUMP=/tmp/setup_req"
  # mainBufferedAudio Phase-A (docs/carplay/04_CAPABILITIES_AND_CONFIG.md; docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4): the PRIMARY arm is now the pushed config's
  # enablesMainBufferedAudio (app default OFF, applied per connection by carplayd) — this
  # /tmp/mainbuffered_test flag is SUBORDINATE and reaches the wire only on the no-config /
  # parse-failure paths (it seeds the lever via the env; ANY parsed pushed config overwrites it in
  # both directions, so with the app connected it is inert). CARPLAY_SETUP_DUMP rides the flag so
  # the phone's SETUP request (does iOS request a buffered stream / move media off the realtime
  # stream?) is CAPTURED, not inferred. Absent = byte-identical to the proven wired spawn.
  # WARNING: advertising can silence media if iOS switches to a buffered stream we do not serve —
  # bench flag OR config toggle, remove/disable immediately on any audio dropout.
  MB=
  [ -f /tmp/mainbuffered_test ] && MB="CARPLAY_MAINBUFFERED=1 CARPLAY_SETUP_DUMP=/tmp/setup_req"
  # ONE process now: carplayd starts its own mDNS advertise/browse thread after it binds :5000
  # (ccpa/carplayd/src/discovery.rs). That ordering is why the separate launch is gone rather than
  # merely moved — rx-connect used to advertise before the RTSP port was listening.
  env OCBM_FWD_ENC=1 $CM $LT $MB setsid carplayd >>"$AL" 2>&1 &   # /usr/sbin/carplayd
  armed=1
  claim_carplay_owner   # tell the box that wired CarPlay owns the port
  # fresh establishment window for this session
  saw_paired=0; saw_record=0; healthy=0; established_since=0
  estab_deadline=$(( $(now) + ESTAB_CONNECT_GRACE ))
  write_healthy 0
  echo "[sup] ARMED (carplayd, discovery in-process) — awaiting pair-verify -> RECORD"
  return 0
}

teardown() {
  echo "[sup] host GONE -> TEARDOWN (holding pattern; iap2d/iAP2 stay up)"
  kill_session
  armed=0
  # per-session establishment state resets; the cross-session STUCK counters (stuck/stuck_fails/edge_ts/
  # fails) DELIBERATELY survive — they clear only on a confirmed-established session (docs/carplay/02_SESSION_LIFECYCLE.md). The
  # old `fails=0` here was the docs/carplay/02_SESSION_LIFECYCLE.md bug: every GONE edge wiped the only counter, so the flap was
  # never counted.
  saw_paired=0; saw_record=0; healthy=0; established_since=0
  write_healthy 0
}

# escalate — the recovery LADDER (task #24 / docs/carplay/02_SESSION_LIFECYCLE.md). Picks a rung by how many stalls/resets have
# accumulated this stuck episode; the counters survive teardown and clear only on a confirmed-established
# session (see the reset in the loop). L3 (reboot, under a persistent budget) is task #28.
escalate() {   # $1 = reason
  # TEST INHIBIT: while /tmp/no_escalate exists, the whole ladder (phone_reset -> ocbmd restart ->
  # REBOOT) is disabled. Bring-up testing produces exactly the signals the ladder is built to punish
  # — repeated app reconnects and sessions that never reach RECORD — and a reboot mid-experiment
  # destroys the state being measured. tmpfs, so it clears itself on the next boot.
  if [ -f /tmp/no_escalate ]; then
    echo "[sup] escalate INHIBITED ($1) — /tmp/no_escalate present (testing)"
    armed=0
    return
  fi
  # NEVER escalate (least of all REBOOT) while a live wireless session owns carplayd — the wired
  # establishment watchdog must not act on a wireless session it doesn't manage (critical QC finding).
  if wireless_owns_session; then
    echo "[sup] escalate suppressed ($1) — a live wireless session owns carplayd"
    armed=0
    return
  fi
  if [ "$l2_tries" -ge "$L2_MAX" ]; then
    # L1 + L2 exhausted -> L3: reboot — the ONLY proven deep-wedge recovery (see the 2026-07-10 USB-reset
    # investigation: no VBUS/controller/PHY reset re-enumerates a wedged iPhone). Gated by a PERSISTENT
    # consecutive-reboot budget so a permanently-broken box can't reboot-loop; the count clears on a
    # confirmed-good session (below). No RTC needed — a plain count in jffs2.
    rc=$(cat "$REBOOT_BUDGET" 2>/dev/null || echo 0)
    case "$rc" in ''|*[!0-9]*) rc=0 ;; esac
    if [ "$rc" -lt "$L3_MAX_REBOOTS" ]; then
      # FAIL CLOSED (audit N4): the budget is the ONLY thing bounding L3, and root jffs2 runs near
      # full, so an unchecked write here can silently leave the count unchanged -> unbounded reboots.
      # Read it back after sync and reboot only if it actually persisted; otherwise fall through to
      # the park-IDLE branch below. A reboot into a still-full filesystem is a loop, not a recovery.
      echo $((rc + 1)) > "$REBOOT_BUDGET" 2>/dev/null; sync
      if [ "$(cat "$REBOOT_BUDGET" 2>/dev/null)" = "$((rc + 1))" ]; then
        echo "[sup] health=STUCK reason=$1: L1/L2 exhausted -> ESCALATE L3 REBOOT (#$((rc + 1))/$L3_MAX_REBOOTS)"
        tail -c 262144 /tmp/box.log > /script/box_crash.log 2>/dev/null  # survives the reboot; ocbm_boot.sh streams it
        write_healthy 0; sync; reboot
        return
      fi
      echo "[sup] CRITICAL: reboot budget did not persist to $REBOOT_BUDGET (filesystem full?) — refusing L3 REBOOT"
    fi
    [ "$stuck" = 0 ] && echo "[sup] health=STUCK reason=$1: L3 reboot budget exhausted ($rc) — parking IDLE (needs manual intervention)"
    stuck=1; stuck_reason="$1"; write_healthy 0
    kill_session; armed=0
    backoff_until=$(( $(now) + 120 ))
    return
  fi
  if [ "$l1_tries" -lt "$L1_MAX" ] && [ "$stuck_fails" -lt "$L2_AT" ]; then
    # L1 — clean the phone-facing side (the automated power-cycle equivalent) and re-ARM.
    l1_tries=$((l1_tries + 1))
    echo "[sup] ESCALATE L1 (#$l1_tries): phone_reset then re-ARM ($1)"
    kill_session
    /script/phone_reset.sh >> /tmp/supervisor.log 2>&1
    armed=0; write_healthy 0
    backoff_until=$(( $(now) + 3 ))
  else
    # L2 — software power-cycle short of reboot: phone_reset + restart the OCBM control plane. The host
    # app must re-SUBSCRIBE afterwards. ocbmd restart is verified; if it fails to come back, only a
    # reboot recovers (task #28 adds the inittab respawn safety net + the L3 reboot rung).
    l2_tries=$((l2_tries + 1))
    echo "[sup] ESCALATE L2 (#$l2_tries): phone_reset + restart ocbmd ($1)"
    kill_session
    /script/phone_reset.sh >> /tmp/supervisor.log 2>&1
    restart_ocbmd_daemon
    armed=0; last_p=""; edge_ts=""; stuck_fails=0   # fresh episode after the big hammer
    write_healthy 0
    backoff_until=$(( $(now) + 8 ))
  fi
}

# restart_ocbmd_daemon — the L2 daemon-restart primitive (originally inlined in escalate()'s L2
# branch; extracted so the ocbmd-liveness wedge check below can reuse it verbatim rather than
# duplicating its subtleties). Kill, relaunch, verify. No session-state resets here — those are
# ladder-specific and stay in escalate().
restart_ocbmd_daemon() {
  # -x, not -f (audit N3), and BOTH spawn forms — exactly the `carplayd_alive` idiom below. `-f`
  # also matches the inittab RESPAWN WRAPPER (`{run_ocbmd.sh} /bin/sh /script/run_ocbmd.sh`):
  # killing it decapitates the thing that would bring ocbmd back, and matching it in the check
  # below certifies a DEAD daemon as alive.
  #
  # BOTH forms are load-bearing, NOT belt-and-braces: BusyBox 1.37 `pgrep/pkill -x` match against
  # the full `argv[0]`, not the basename (hardware-confirmed, see `wireless/src/av.rs`'s `running`).
  # Every real launch of this daemon uses the full path — inittab's `exec /usr/sbin/ocbmd` and
  # ocbm_boot.sh's `/usr/sbin/ocbmd &` — so a bare `-x ocbmd` alone would match NEITHER: the kill
  # would spare the live daemon and the relaunch below would make it a SECOND one, and the success
  # check would report CRITICAL against a daemon that had just started fine.
  pkill -x /usr/sbin/ocbmd 2>/dev/null; pkill -x ocbmd 2>/dev/null
  sleep 1
  pkill -9 -x /usr/sbin/ocbmd 2>/dev/null; pkill -9 -x ocbmd 2>/dev/null
  # Full path, matching inittab: the respawn wrapper polls `pgrep -f /usr/sbin/ocbmd` and is blind
  # to a bare-name `setsid ocbmd`, so the old spelling had it start a second daemon ~5 s later.
  #
  # stderr -> /tmp/box.log, NOT /tmp/ocbmd.log (2026-09-05). ocbmd streams a fixed list of files to
  # the host over CH_LOG (`LOG_SOURCES`, ccpa/ocbmd/src/main.rs) and /tmp/ocbmd.log is not on it,
  # so every ocbmd relaunched from here was INVISIBLE to the app for the rest of its life: on
  # 2026-09-05 this L2 fired ("ocbmd wedged (alive mtime stale >=1min, gadget CONFIGURED, pid=71)"),
  # the fresh ocbmd deleted /tmp/carplay_cfg.yaml at startup, carplayd_wl (left running) served the
  # reconnecting phone from compiled defaults (1920x720, H.264) while the app believed 2400x960/HEVC
  # was in force — and the app log carried ZERO box lines for the two minutes that decided it,
  # because everything the new ocbmd said went to a file nobody tails. The inittab respawn path
  # (ccpa/rootfs/script/run_ocbmd.sh) already appends to /tmp/box.log; this now matches it. `>>`
  # opens O_APPEND, which ocbmd's own box.log rotation relies on (see `log_rotate` there).
  setsid /usr/sbin/ocbmd >> /tmp/box.log 2>&1 &
  sleep 2
  if pgrep -x ocbmd >/dev/null 2>&1 || pgrep -x /usr/sbin/ocbmd >/dev/null 2>&1; then
    echo "[sup] L2: ocbmd restarted — host must re-SUBSCRIBE"
  else
    echo "[sup] CRITICAL: ocbmd did not restart — reboot required (L3 = task #28)"
  fi
}

# ocbmd_wedged — liveness escalation (bench finding: ocbmd stuck in an uninterruptible HCI ioctl
# during BT line-discipline teardown never exits, so the inittab respawn, the pid-lock singleton
# and the failover watchdog's first-120s window all do nothing; USB writes to the host stall until
# a manual power cycle). ocbmd (Rust side) touches /tmp/ocbmd_alive (mtime only) once per second
# from its dispatch loop. Contract: mtime stale while the ACCESSORY gadget (Mac/host-facing,
# android_usb_accessory — the OCBM control plane ocbmd owns, NOT the phone-facing android_usb) is
# CONFIGURED means ocbmd is wedged, not merely idle.
#
# Staleness check: BusyBox `find` on this box has no `-newermt`/fractional support (confirmed by
# the ONLY other stale-marker idiom in this repo, radio_hal.sh's lock_take() / radio_ap_up.sh's AP lock, both of
# which use integer `-mmin`). There is therefore no portable way to hit the 15s target precisely;
# `-mmin +0` (>=1 full minute stale) is the finest granularity available and is used here — this
# under-reacts by up to ~45s versus the 15s contract, traded for portability. Absent
# /tmp/ocbmd_alive is a no-op (old ocbmd builds never create it).
ACC_STATE=/sys/class/android_usb_accessory/android0/state
WEDGE_RATE_LIMIT=60     # do not re-escalate a wedge within this many seconds of the last one
wedge_escalate_ts=0     # epoch(uptime) of the last wedge-triggered L2, 0 = never
ocbmd_wedged() {
  [ -e /tmp/ocbmd_alive ] || return 1
  [ -n "$(find /tmp/ocbmd_alive -maxdepth 0 -mmin +0 2>/dev/null)" ] || return 1
  [ "$(cat "$ACC_STATE" 2>/dev/null)" = "CONFIGURED" ] || return 1
  [ -n "$(pidof ocbmd 2>/dev/null)" ] || return 1
  return 0
}

# The connect timeline's usb_edge_s (crates/box-common/src/timeline.rs): when the host-facing USB link last came up,
# as this supervisor saw it. Its UDC's state (ci_hdrc.1, docs/carplay/00_ARCHITECTURE.md) is read once a tick, with no
# fork (usb_observe), and an edge into "configured" from any other state is noted on the uptime clock; wireless_up
# stamps it beside sup_radios. Not known (empty) from the first reading, whose edge came before this supervisor did,
# nor once the state reads "suspended": this kernel's chipidea UDC still reads "suspended" after a resume
# (docs/carplay/02_SESSION_LIFECYCLE.md, "Empirical basis"), so a resume cannot be timed from it.
USB_UDC_STATE=/sys/class/udc/ci_hdrc.1/state
usb_class=""           # configured | suspended | down, as last read; empty before the first reading
usb_configured_at=""   # uptime ms of the last edge into "configured"; empty when not known
usb_observe() {
  _uo_s=""
  read -r _uo_s 2>/dev/null <"$USB_UDC_STATE"
  case "$_uo_s" in configured) _uo_c=configured ;; suspended) _uo_c=suspended ;; *) _uo_c=down ;; esac
  [ "$_uo_c" = "$usb_class" ] && return 0
  usb_configured_at=""
  if [ -n "$usb_class" ] && [ "$_uo_c" = configured ]; then
    _uo_up=""
    read -r _uo_up _uo_idle 2>/dev/null </proc/uptime
    case "$_uo_up" in *.*) usb_configured_at="${_uo_up%.*}${_uo_up#*.}0" ;; esac
  fi
  usb_class=$_uo_c
}

# apply_pending — at an idle boundary (present==0), apply any peer-store mutation that peer_store.sh
# deferred while a host was present (docs/carplay/02_SESSION_LIFECYCLE.md, #25). Runs restart-coupled: the store is only ever
# mutated while idle, and the next ARM's carplayd re-reads it — never the live disk<->memory divergence
# that caused the docs/carplay/02_SESSION_LIFECYCLE.md stall. Safe to call every idle tick (no-op without a pending file).
apply_pending() {
  [ -f "$PEER_PENDING" ] || return
  op=$(cat "$PEER_PENDING" 2>/dev/null)
  case "$op" in
    clear) rm -f /etc/carplay_peers.bin 2>/dev/null; sync
           echo "[sup] applied DEFERRED peer-store op at idle: clear (cold pair-setup next connect)" ;;
    *)     echo "[sup] unknown deferred peer-store op '$op' — ignored" ;;
  esac
  rm -f "$PEER_PENDING"
}

# Latch establishment milestones (grep-once; bound_logs may truncate the line later, so never rely on
# re-grepping — once latched it stays latched for this session). docs/wireless/00_WIRELESS_CARPLAY.md #1.3: transport-scoped — while
# a wireless session owns the box, read ITS log (/tmp/carplayd_wl.log), never the wired $AL. Grepping
# both unconditionally would let a stale, never-truncated wl.log falsely mark an unrelated WIRED session
# healthy (the exact cross-attribution bug per-transport logs were created to prevent).
#
# The wireless log is truncated only when btd starts, and holds every carplayd btd spawned since, so it is read
# after the last carplayd's spawn mark (btd's av::SPAWN_MARK) only. Mac bench 12 Nov: a phone that never joined
# latched the previous session's "pair-verify OK" and "RECORD done", with nothing connected.
WL_SPAWN_MARK="[av] === carplayd spawned for a new session ==="

# Does log $1 hold $2 (a fixed string)? For the wireless log, only after its last spawn mark.
milestone_in() {
  if [ "$1" = /tmp/carplayd_wl.log ]; then
    awk -v mark="$WL_SPAWN_MARK" -v want="$2" \
      'index($0, mark) { seen = 0 } index($0, want) { seen = 1 } END { exit !seen }' "$1" 2>/dev/null
  else
    grep -qF "$2" "$1" 2>/dev/null
  fi
}

scan_milestones() {
  if wireless_owns_session; then _ml=/tmp/carplayd_wl.log; else _ml="$AL"; fi
  if [ "$saw_paired" = 0 ] && milestone_in "$_ml" "pair-verify OK"; then
    saw_paired=1
    estab_deadline=$(( $(now) + ESTAB_STREAM_GRACE ))
    echo "[sup] milestone: pair-verify OK — control encrypted (RECORD grace ${ESTAB_STREAM_GRACE}s)"
  fi
  if [ "$saw_record" = 0 ] && milestone_in "$_ml" "RECORD done"; then
    saw_record=1; healthy=1; established_since=$(now); write_healthy 1
    echo "[sup] milestone: RECORD — session ESTABLISHED (health=1)"
  fi
}

# Prune edge_ts to the flap window; set edge_count.
prune_edges() {
  _cut=$(( $(now) - FLAP_WINDOW )); _out=""; edge_count=0
  for _t in $edge_ts; do
    if [ "$_t" -ge "$_cut" ]; then _out="$_out $_t"; edge_count=$((edge_count + 1)); fi
  done
  edge_ts="$_out"
}

# Keep the RAM-backed /tmp logs bounded on the 123 MB no-swap box (belt-and-suspenders; the per-frame
# churn was already removed). Best-effort in-place truncate to the tail when a log exceeds the cap.
bound_logs() {
  # docs/wireless/00_WIRELESS_CARPLAY.md #1.5: the wireless-side logs (av.rs's spawn_detached targets + btd's own log)
  # were omitted here — unbounded append on the 123 MB no-swap tmpfs across long-lived wireless sessions.
  for f in /tmp/ocbmd.log /tmp/iap2d.log /tmp/supervisor.log "$AL" \
           /tmp/carplayd_wl.log /tmp/wl.log; do
    [ -f "$f" ] || continue
    sz=$(wc -c < "$f" 2>/dev/null || echo 0)
    if [ "$sz" -gt 262144 ]; then
      tail -c 65536 "$f" > "$f.lr" 2>/dev/null && cat "$f.lr" > "$f" 2>/dev/null
      rm -f "$f.lr"
    fi
  done
  # transition ring: count-bounded (keep last 200 once it passes 400), NOT byte-tail — transitions are
  # rare so this stays tiny, and the flap ONSET (the diagnostic gold) is never sliced off mid-history.
  if [ -f "$RING" ]; then
    lc=$(wc -l < "$RING" 2>/dev/null || echo 0)
    if [ "$lc" -gt 400 ]; then tail -n 200 "$RING" > "$RING.lr" 2>/dev/null && cat "$RING.lr" > "$RING" 2>/dev/null; rm -f "$RING.lr"; fi
  fi
}

# --- App-driven wireless bring-up (2026-07-16) -------------------------------------------------------
# Wireless CarPlay radios are ON-DEMAND, driven by HOST-APP presence: brought up when the app is present
# AND its pushed config enables wireless (default true), torn down when the host goes away. WIRED is the
# unconditional always-on baseline (ocbmd + this supervisor spawn iap2d/carplayd on iPhone-USB-connect
# regardless) — this only ADDS the wireless option alongside it, so the box waits for whichever transport
# connects FIRST (dual-transport, first-come-wins; the wireless_owns_session guards above keep them from
# fighting). The heavy bring-up (~15 s: WiFi/BT module loads) runs DETACHED so it never blocks the 1 s
# loop, and each stage is internally idempotent (wlan_on/bt_on skip if already loaded; btd
# is pgrep-guarded). NEVER pipe a radio bring-up script to tail/head — a backgrounding daemon inheriting
# the console PTY wedges it; always redirect to a logfile.
WIRELESS_CFG=${CARPLAY_CFG_FILE:-/tmp/carplay_cfg.yaml}
# Default TRUE: only an explicit `wireless: false` in the host YAML disables it (wired-only). A missing
# file / missing key / `wireless: true` all mean enabled. Kept a raw grep so ocbmd + this shell need no
# YAML parser (carplayd's receiver ignores the key; the supervisor is its sole consumer).
wireless_enabled() { ! grep -qiE '^[[:space:]]*wireless:[[:space:]]*false' "$WIRELESS_CFG" 2>/dev/null; }
# App-commanded radio inhibit (ocbmd CT_RADIO, docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating): flag present = radios must be
# OFF now. ocbmd owns the flag lifecycle (set/cleared by host CT_RADIO; cleared on fresh SUBSCRIBE,
# app loss, and ocbmd startup) — this is an app-commanded surface, NOT an on-box lever.
RADIO_OFF_FLAG=${RADIO_OFF_FLAG:-/tmp/radio_off}
radio_off() { [ -f "$RADIO_OFF_FLAG" ]; }
# Hot-Handover (NON-STANDARD, opt-in): force a live wireless->wired switch when a cable is plugged into an
# ACTIVE wireless session. DEFAULT OFF (= "Standard", spec-conformant): Apple's R14G17 selects transport
# exactly ONCE at session start (wired-preferred) and never migrates a live session (CarPlayControlClient.c
# _CarPlayControllerCopyBestService); iOS keeps the running wireless session and treats the cable as
# charge-only ("Charge Only" USB mode; WWDC 2017-717 "do not interrupt a running session"). So this preempt
# is a deliberate extension, enabled ONLY by the host YAML `hot_handover: true`. Absent/anything-else => the
# standard "keep wireless on plug" behavior. See docs/ops/05_AUDITS.md and the transport-selection research 2026-08-01.
hot_handover_enabled() { grep -qiE '^[[:space:]]*hot_handover:[[:space:]]*true' "$WIRELESS_CFG" 2>/dev/null; }
wireless_running() { pgrep -f /usr/sbin/btd >/dev/null 2>&1; }
# A wireless_down teardown still running (its detached block): each block has its own marker, /tmp/.wl_teardown.<its
# start uptime>, holding that uptime, written here before the block starts; the block adds its pid as its first act and
# removes the marker as it ends, then wakes this loop. In flight while any block lives: one can wait up to 30 s on each
# radio_hal.sh lock, so no fixed age would do, and a second teardown must not hide a first one still running (the
# Mac's reviews of bbd29f3). 90 s caps one against a reused pid; before its pid is written, 15 s does.
# wireless_up waits on it, so a host that comes back within the teardown neither finds the SIGTERMed btd and skips
# its bring-up, nor starts a btd the teardown then kills with -9 (the Mac's review of 376c4ce, SUP-TEARDOWN-RACE).
WL_TEARDOWN_MARK=${WL_TEARDOWN_MARK:-/tmp/.wl_teardown}   # the teardown block below writes this path literally
teardown_in_flight() {
  _tif=1
  for _tf in "$WL_TEARDOWN_MARK".*; do
    case "$_tf" in *.new) continue ;; esac
    [ -f "$_tf" ] || continue
    if teardown_alive "$_tf"; then _tif=0; else rm -f "$_tf"; fi
  done
  return $_tif
}
teardown_alive() {  # $1 = one block's marker
  _ti=$(cat "$1" 2>/dev/null) || return 1
  _ts=${_ti%% *}
  case "$_ts" in ''|*[!0-9.]*) return 1 ;; esac
  _tage=$(( $(now) - ${_ts%%.*} ))
  if [ "$_ti" = "$_ts" ]; then
    [ "$_tage" -lt 15 ]   # the block has not written its pid yet
  else
    _tp=${_ti#* }
    case "$_tp" in ''|*[!0-9]*) return 1 ;; esac
    [ -d "/proc/$_tp" ] && [ "$_tage" -lt 90 ]
  fi
}
teardown_mark() { _td=$(cut -d" " -f1 /proc/uptime); echo "$_td" > "$WL_TEARDOWN_MARK.$_td"; }
# Is ANY part of the wireless stack still drawing power? The advertiser can be dead while hostapd is
# still beaconing and hci0 is still UP — which is exactly the state that made a wired session crawl
# (owner report 2026-08-11), so all three are checked, not just the daemon.
wireless_stack_up() {
  wireless_running && return 0
  pgrep -f "[h]ostapd" >/dev/null 2>&1 && return 0
  hciconfig hci0 2>/dev/null | grep -q "UP RUNNING" && return 0
  return 1
}
# Is ANY carplayd alive, in EITHER spawn form — the WIRED supervisor's bare-name `setsid carplayd`
# (argv[0]=="carplayd") or the wireless av.rs full-path `/usr/sbin/carplayd`? Exact-match probes for each
# form (deliberately NOT `pgrep -f carplayd`, which would false-match a transient `tail`/`grep` of an
# *carplayd*.log and wrongly report it alive). Used by the stale-flag watchdog to tell a truly-dead
# wireless session from a live one. (Over-reporting alive would only DELAY the watchdog, never mis-clear.)
carplayd_alive() { pgrep -x carplayd >/dev/null 2>&1 || pgrep -x /usr/sbin/carplayd >/dev/null 2>&1; }
# Pairing association model from the host YAML `pairing:` (default just_works — the proven CCPA posture).
# `numeric_comparison` selects SSP DisplayYesNo → the iPhone + box both show a 6-digit code to match.
# Passed to btd as BT_PAIRING_MODE (its ssp_agent reads the env).
# `numeric_comparison_interactive` additionally sets BT_SSP_INTERACTIVE=1: the box waits for the
# head unit's yes/no instead of confirming its own side at once. NOT reachable with iOS as the peer
# (docs/wireless/01_BT_AND_RADIO.md, 2026-09-04) — kept for other peers and bench work.
pairing_interactive() {
  if grep -qiE '^[[:space:]]*pairing:[[:space:]]*numeric_comparison_interactive' "$WIRELESS_CFG" 2>/dev/null; then
    echo 1
  else
    echo 0
  fi
}
pairing_mode() {
  if grep -qiE '^[[:space:]]*pairing:[[:space:]]*(numeric|numeric_comparison)' "$WIRELESS_CFG" 2>/dev/null; then
    echo numeric
  else
    echo just_works
  fi
}
# The GM bridge role (`wifi_ap: false`, OCBM 1.0.4 and earlier) wrote the vehicle hotspot into
# /etc/hostapd.conf and kept the adapter's own as /etc/hostapd.conf.stock. The role is gone, so the box's
# AP must be its own again: put the original back, once.
restore_stock_hostapd() {
  [ -f /etc/hostapd.conf.stock ] || return 0
  # The AP's configuration written ahead (radio_ap_up.sh PREP) no longer describes the file: write it again.
  rm -f /tmp/.radio_ap_prepared
  mv -f /etc/hostapd.conf.stock /etc/hostapd.conf && sync \
    && echo "[sup] put back the adapter's own /etc/hostapd.conf (the bridge role's copy is retired)"
}

# --- Armed Bluetooth between host sessions (the owner's decision, 16 Nov 2026) -------------------------------------
# Between host sessions Bluetooth may stay POWERED BUT INVISIBLE: page and inquiry scan off, no connection, nothing
# paging (docs/wireless/01_BT_AND_RADIO.md, "Between host sessions: armed"). With no host here, once the last one's
# teardown is over or the boot preload is done, this supervisor starts the NEXT host's btd early (OCBM_BTD_ARM=1,
# crates/vendor/wireless/src/armed.rs): it brings the controller up as a session's bring-up would but with no scan,
# and waits. The arm also writes the AP's configuration ahead (radio_hal.sh wifi_ap_prep): files only, no network,
# nothing beaconing. The host's arrival is then a go (wireless_go): the session's inputs in a file and SIGUSR1, then
# the AP from its configuration. btd proves the controller with one HCI round trip and answers; a failure, no answer
# or no btd falls back to today's path (go_watch: wireless_up fallback, bt_on and a fresh btd). Off: the owner's
# /script/radio_arm_off, or the bench hook /tmp/bench_arm_off; either restores the 2.6.0 behaviour (Bluetooth powered
# down between sessions) from the next host's departure.
BTD_ARMED_FILE=${BTD_ARMED_FILE:-/tmp/.btd_armed}      # the armed btd's pid, written by btd once armed
BTD_GO_FILE=${BTD_GO_FILE:-/tmp/.btd_go}               # the go: pid=<the armed btd> and the session's inputs
BTD_GO_RESULT=${BTD_GO_RESULT:-/tmp/.btd_go_result}    # btd's answer: "ok <uptime ms>" or "fail <why>"
BT_PHASE_FILE=${BT_PHASE_FILE:-/tmp/bt_phase}          # btd's phase mirror: a host session's, so none while armed
ARM_OFF_FILE=${ARM_OFF_FILE:-/script/radio_arm_off}    # the owner's off switch, on flash
ARM_BENCH_OFF=${ARM_BENCH_OFF:-/tmp/bench_arm_off}     # the bench's (box_common::bench_flag's rule)
STARTUP_MARKS=${STARTUP_MARKS:-/tmp/startup_marks}     # the connect timeline's record (box_common::timeline)
RADIO_LOCKS=${RADIO_LOCKS:-/tmp}                       # radio_hal.sh's locks: <here>/.radio_lock_<subsystem>
arm_pid=""      # the arm this supervisor started: its wrapper's pid, which becomes the armed btd's (exec, no fork)
arm_since=""    # the uptime (s) it started at
arm_next=1      # arm when no host is here: from the start (after the boot preload) and after each wireless session
go_pending=0    # a go went out and btd's answer is awaited (go_watch)
go_pid=""
go_since=""
# Radios a bring-up started (a go, or today's path), and no teardown block run since. wireless_down then never takes its
# early return, which looks only at processes and hci0: the go launches the AP's bring-up detached, so if its btd failed
# and the host left before hostapd was in the process list, nothing it looked at was up yet, and the AP came up with no
# host (the release review of 3b38d37, A1-2). A teardown block clears it.
wl_raised=0
# An AP bring-up (or teardown) under way: radio_hal.sh holds its wlan lock from before it starts anything to after.
ap_bringup_in_flight() { [ -d "$RADIO_LOCKS/.radio_lock_wlan" ]; }

# Is $1 a live btd? Its /proc stat's name and state, read with `read`: not a zombie waiting to be reaped.
proc_live_btd() {
  _plb_name=""; _plb_state=""
  read -r _plb_pid _plb_name _plb_state _plb_rest 2>/dev/null <"$SUP_PROC/$1/stat"
  [ "$_plb_name" = "(btd)" ] && [ -n "$_plb_state" ] && [ "$_plb_state" != Z ]
}

# Is the arm this supervisor started still there? A btd, or its wrapper (sh, before the exec) for 30 s at most.
arm_alive() {
  [ -n "$arm_pid" ] || return 1
  proc_live_btd "$arm_pid" && return 0
  _aa_state=""
  read -r _aa_pid _aa_name _aa_state _aa_rest 2>/dev/null <"$SUP_PROC/$arm_pid/stat"
  [ -n "$_aa_state" ] && [ "$_aa_state" != Z ] || return 1
  uptime_now
  [ -n "$_up_s" ] && [ -n "$arm_since" ] && [ $((_up_s - arm_since)) -lt 30 ]
}

# The armed btd ready for a go: it wrote its pid, the pid is this supervisor's arm, and it is a live btd. Sets _ap.
btd_armed_ready() {
  _ap=""
  [ -n "$arm_pid" ] || return 1
  read -r _ap 2>/dev/null <"$BTD_ARMED_FILE"
  [ -n "$_ap" ] && [ "$_ap" = "$arm_pid" ] && proc_live_btd "$_ap"
}

# Armed Bluetooth switched off: the owner's file, or the bench hook (there, and not reading "off").
arm_off() {
  [ -e "$ARM_OFF_FILE" ] && return 0
  [ -e "$ARM_BENCH_OFF" ] || return 1
  _ao=""; read -r _ao 2>/dev/null <"$ARM_BENCH_OFF"
  [ "$_ao" != off ]
}

# May the radios be armed now? Asked on ticks with no host here, cheapest first, so most ticks fork nothing.
arm_gate() {
  [ "$arm_next" = 1 ] && [ -z "$arm_pid" ] || return 1
  if arm_off; then
    arm_next=0
    echo "[sup] Bluetooth not armed between sessions: switched off ($ARM_OFF_FILE or $ARM_BENCH_OFF)"
    return 1
  fi
  # Attached already: an arm never attaches (at boot the preload does; without it, the first host's session does).
  [ -d "$SUP_SYS/class/bluetooth/hci0" ] || return 1
  # A seam bring-up under way (the boot preload holds both locks) goes first.
  [ -d "$RADIO_LOCKS/.radio_lock_bt" ] && return 1
  [ -d "$RADIO_LOCKS/.radio_lock_wlan" ] && return 1
  teardown_in_flight && return 1
  # A wired phone: the next host's session is wired, and the radios stay down for it (A2). Its Hot-Handover setting
  # cannot be asked: the config left with the host.
  wired_phone_on_bus && return 1
  wireless_running && return 1
  return 0
}

# Arm: the AP's configuration written ahead, then the next host's btd, armed. Detached, as a bring-up is; its body
# names no A/V daemon and no AP daemon (the teardown's patterns: comments are argv too), only btd, whose teardown
# pattern is meant to end an arm under way.
arm_wireless() {
  arm_next=0
  # As wireless_up's clean slate: a stray A/V daemon, not a wired one, is reaped before the wireless A/V log restarts.
  _ar=0
  if ! pgrep -x carplayd >/dev/null 2>&1; then _ar=1; pkill -f carplayd 2>/dev/null; fi
  restore_stock_hostapd
  # No phase while armed: a btd publishes one only once its host session has begun (after the go), so a phase file
  # here is a previous btd's (the teardown that leaves a wired carplayd running does not remove it). An update trial's
  # commit (ocbm_dispatch.sh, btd_session_up) counts a btd with a phase as one serving a host; this one serves none.
  rm -f "$BT_PHASE_FILE" 2>/dev/null
  uptime_now
  echo "[sup] no host: arming Bluetooth for the next one (powered, invisible) at uptime $_up_raw"
  env OCBM_BTD_ARM=1 OCBM_WL_REAPED="$_ar" OCBM_AV_NAME=carplayd OCBM_AV_LOG=/tmp/carplayd_wl.log setsid sh -c '
    # The AP configured ahead: its files only. No network and no beacon until a host is here.
    sh /script/radio_hal.sh wifi_ap_prep >/tmp/wlan_prep.log 2>&1
    if [ "$OCBM_WL_REAPED" = 1 ]; then
      _i=0
      while [ "$_i" -lt 25 ] && { pgrep -x "$OCBM_AV_NAME" || pgrep -x "/usr/sbin/$OCBM_AV_NAME"; } >/dev/null 2>&1; do
        _i=$((_i + 1)); sleep 0.2
      done
    fi
    { true >"$OCBM_AV_LOG"; } 2>/dev/null
    unset OCBM_BTD_WAIT   # an armed btd never waits for a handover, whatever the supervisor inherited
    exec /usr/sbin/btd </dev/null >>/tmp/wl.log 2>&1
  ' </dev/null >/dev/null 2>&1 &
  arm_pid=$!
  arm_since=$_up_s
}

# The arm, watched on ticks with no host here. Gone (the arm failed, a check gave the armed state up, or it crashed),
# it may have left Bluetooth powered: down through the seam, as a teardown leaves it. Not armed again until a wireless
# session has run, so the next host takes today's path.
arm_watch() {
  [ -n "$arm_pid" ] || return 0
  arm_alive && return 0
  echo "[sup] the armed btd ($arm_pid) has ended -- Bluetooth down as between sessions; the next host takes today's path"
  arm_pid=""
  wireless_down "the armed btd ended"
}

# An arm still under way when the host comes gets 3 s to finish arming: about a second from power-up to armed.
arm_wait_ready() {
  _aw=0
  while [ "$_aw" -lt 30 ] && ! btd_armed_ready && arm_alive; do _aw=$((_aw + 1)); sleep 0.1; done
  [ "$_aw" -gt 0 ] && echo "[sup] wireless_up waited $((_aw * 100)) ms for the arm under way"
  return 0
}

# The host's config, read once for a go: wireless (1 unless `wireless: false`), Hot-Handover (1 only on `true`), the
# pairing mode and whether it is interactive. The answers wireless_enabled, hot_handover_enabled, pairing_mode and
# pairing_interactive give, from one awk instead of four greps (tools/session_supervisor_test.sh holds them to it).
cfg_snapshot() {
  _cs_wireless=1; _cs_hot=0; _cs_mode=just_works; _cs_inter=0
  [ -f "$WIRELESS_CFG" ] || return 0
  set -- $(awk '{ l = tolower($0) }
    l ~ /^[[:space:]]*wireless:[[:space:]]*false/ { w = 1 }
    l ~ /^[[:space:]]*hot_handover:[[:space:]]*true/ { h = 1 }
    l ~ /^[[:space:]]*pairing:[[:space:]]*(numeric|numeric_comparison)/ { n = 1 }
    l ~ /^[[:space:]]*pairing:[[:space:]]*numeric_comparison_interactive/ { i = 1 }
    END { a = w ? 0 : 1; b = h ? 1 : 0; c = n ? "numeric" : "just_works"; d = i ? 1 : 0; print a, b, c, d }' \
    "$WIRELESS_CFG" 2>/dev/null)
  [ "$#" = 4 ] || return 0
  _cs_wireless=$1; _cs_hot=$2; _cs_mode=$3; _cs_inter=$4
}

# The bench's btmon capture (CARPLAY_BTMON or /tmp/carplay_btmon), as the bring-up's wrapper starts it.
btmon_lever() {
  { [ -f /tmp/carplay_btmon ] || [ "${CARPLAY_BTMON:-0}" = 1 ]; } || return 0
  if command -v btmon >/dev/null 2>&1; then
    echo "[sup] btmon lever armed -- capturing to /tmp/box.log"
    ( btmon 2>&1 | sed "s/^/[btmon] /" >> /tmp/box.log & )
  else
    echo "[sup] btmon lever armed but btmon not found on PATH -- skipping"
  fi
}

# The host's arrival with an armed btd: the go. wireless_up's guards first (the inhibit, `wireless: false`, a wired
# session owning the box), each of which puts the armed Bluetooth down instead of going; then the go, ahead of
# everything else, and the AP after it. Before the signal nothing forks but the config's one awk.
wireless_go() {  # $1 = the armed btd's pid
  if radio_off; then
    echo "[sup] radios inhibited (CT_RADIO off) — the armed Bluetooth goes down, no bring-up"
    wireless_down "radios inhibited (CT_RADIO off)"
    return
  fi
  cfg_snapshot
  if [ "$_cs_wireless" != 1 ]; then
    echo "[sup] wireless disabled by config (wireless: false) — wired-only; the armed Bluetooth goes down"
    wireless_down
    return
  fi
  if [ "$_cs_hot" != 1 ] && ! wireless_owns_session && wired_phone_on_bus; then
    echo "[sup] wireless_up SUPPRESSED — a WIRED session owns the box (Hot-Handover off); the armed Bluetooth goes down"
    wireless_down "WIRED session active + Hot-Handover OFF (radios yield to the cable)"
    # A2's rising edge, taken here (its state may be a past session's): the cable's going restores the stack.
    radios_yielded_to_wired=1; _a2_state=1
    return
  fi
  { true >"$BTD_GO_RESULT"; } 2>/dev/null   # no answer yet (a redirection, not rm: nothing forks before the signal; not `:`, whose failed redirection would end the supervisor)
  printf 'pid=%s\nBT_PAIRING_MODE=%s\nBT_SSP_INTERACTIVE=%s\n' "$1" "$_cs_mode" "$_cs_inter" 2>/dev/null >"$BTD_GO_FILE"
  usb_observe
  uptime_now
  # The connect timeline (box_common::timeline): the radios start now, and the host-facing USB link's last edge.
  if [ -n "$_up_ms" ]; then
    echo "t sup_radios $_up_ms" 2>/dev/null >>"$STARTUP_MARKS"
    [ -n "$usb_configured_at" ] && echo "t usb_edge $usb_configured_at" 2>/dev/null >>"$STARTUP_MARKS"
  fi
  arm_pid=""
  wl_raised=1
  if kill -USR1 "$1" 2>/dev/null; then
    go_pending=1; go_pid=$1; go_since=$_up_s
  fi
  # The AP from its configuration written at the arm (radio_hal.sh's fast path): hostapd is its first launch.
  ( setsid sh /script/radio_hal.sh wifi_ap_on >/tmp/wlan.log 2>&1 & )
  btmon_lever
  echo "[sup] host PRESENT + wireless enabled -> armed go (btd $1, pairing=$_cs_mode) at uptime $_up_raw"
  if [ "$go_pending" != 1 ]; then
    echo "[sup] armed go: btd $1 did not take the signal -- falling back to bt_on and a fresh btd"
    wireless_up fallback
  fi
}

# Stop a btd that is not to go on: SIGTERM, which an armed btd answers at once, then -9 after a second.
stop_btd() {
  proc_live_btd "$1" || return 0
  kill "$1" 2>/dev/null
  _sb=0
  while [ "$_sb" -lt 10 ] && proc_live_btd "$1"; do _sb=$((_sb + 1)); sleep 0.1; done
  if proc_live_btd "$1"; then
    kill -9 "$1" 2>/dev/null
    _sb=0
    while [ "$_sb" -lt 10 ] && proc_live_btd "$1"; do _sb=$((_sb + 1)); sleep 0.1; done
  fi
  proc_live_btd "$1" && echo "[sup] btd $1 survived SIGKILL (a kernel wait?)"
  return 0
}

# The go's answer, asked each tick until there is one: btd writes it within milliseconds (its proof is one round
# trip) and wakes this loop when it is a failure. A failure, no btd, or no answer within 3 s falls back to today's
# path: the armed btd stopped, then wireless_up's bring-up (bt_on, a fresh btd), the go's stamps and AP kept.
go_watch() {
  [ "$go_pending" = 1 ] || return 0
  _gw_r=""; read -r _gw_r 2>/dev/null <"$BTD_GO_RESULT"
  case "$_gw_r" in
    ok*)
      go_pending=0
      echo "[sup] armed go: btd $go_pid has the host's session ($_gw_r)"
      return 0
      ;;
    fail*) _gw_why="btd: ${_gw_r#fail }" ;;
    *)
      if proc_live_btd "$go_pid"; then
        uptime_now
        if [ -n "$_up_s" ] && [ -n "$go_since" ] && [ $((_up_s - go_since)) -lt 3 ]; then return 0; fi
        _gw_why="no answer from btd $go_pid in 3 s"
      else
        _gw_why="btd $go_pid is gone"
      fi
      ;;
  esac
  go_pending=0
  echo "[sup] armed go failed ($_gw_why) -- falling back to bt_on and a fresh btd"
  stop_btd "$go_pid"
  if [ "${p:-0}" = 1 ]; then wireless_up fallback; fi
}

# --- The cold boot: btd before Bluetooth is up (R3-5, 20 Nov 2026) ---------------------------------------------------
# A host that comes while the boot preload is attaching Bluetooth (the truck: most drives start with the adapter booting,
# the host at about 3 s) used to get its btd only after its bt_on, which waited for the preload to end: 1.6 to 1.9 s
# from hci0 answering to the first page on the dev adapter's bench of 70caa3c. With the preload running at the host's
# arrival, wireless_up's wrapper now starts btd first, waiting (OCBM_BTD_WAIT=1, crates/vendor/wireless/src/handover.rs),
# then bt_on beside it, then writes /tmp/.btd_bt_on_done once bt_on is over. btd goes on the preload's handover as hci0
# answers (radio_hal.sh, "The handover": the host's bt_on then touches nothing), or on that file: today's bring-up.
# OPT-IN, OFF BY DEFAULT (the owner's decision, 21 Nov 2026, after 2 of 6 boots with link trouble on the bench of
# 4a6ce9c against 0 of 9 without; radio_hal.sh, "OPT-IN"): only with the owner's /script/radio_handover_on or the
# bench's /tmp/bench_handover_on. Without either, btd still starts first while the preload runs, by btd_early (below,
# on by default since 2.7.1), and waits for that file; with btd_early off, any other start, and a failed armed go's
# fallback take today's wrapper: bt_on, then btd. The retired /script/radio_handover_off is read by nothing.
PRELOAD_STATE=${PRELOAD_STATE:-/tmp/radio_preload_state}            # radio_hal.sh's preload states, one per line
HANDOVER_ON_FILE=${HANDOVER_ON_FILE:-/script/radio_handover_on}     # the owner's opt-in, on flash
HANDOVER_BENCH_ON=${HANDOVER_BENCH_ON:-/tmp/bench_handover_on}      # the bench's (box_common::bench_flag's rule)
# The boot preload is under way: its last state is "running" (it writes another as it ends). No fork.
preload_running() {
  _prs=""
  while read -r _prw _prt; do _prs=$_prw; done 2>/dev/null <"$PRELOAD_STATE"
  [ "$_prs" = running ]
}
# The handover asked for: the owner's file, or the bench's set (there, and not reading "off").
handover_on() {
  [ -e "$HANDOVER_ON_FILE" ] && return 0
  [ -e "$HANDOVER_BENCH_ON" ] || return 1
  _hon=""; read -r _hon 2>/dev/null <"$HANDOVER_BENCH_ON"
  [ "$_hon" != off ]
}
# Round 4, item 6 (btd_early, 22 Nov 2026; docs/ops/02_TESTING.md, "Round 4 switches"): the same early btd while the
# boot preload runs, with the handover NOT asked for. Nothing is handed over (radio_hal.sh claims nothing without the
# handover's own opt-in), so btd waits for the file that says bt_on is over (crates/vendor/wireless/src/handover.rs,
# Go::BtOn) and brings Bluetooth up as today, DOWN/UP and all: only its exec and start (about 0.35 s from bt_on's end)
# move off the path. ON BY DEFAULT since 2.7.1 (the owner's decision of 22 Nov 2026). Off, 2.7.0's wrapper (bt_on, then
# btd), with "off" in /tmp/bench_btd_early or, for a cold boot, in /etc/ocbm/bench_btd_early on flash: the first of the
# two that exists decides, so an "off" in /tmp overrides the flash; any other text (empty, "on") is on. 0: on, 1: off;
# _beo is the file that decided, "" for the default.
BTD_EARLY_BENCH_ON=${BTD_EARLY_BENCH_ON:-/tmp/bench_btd_early}
BTD_EARLY_ON_FILE=${BTD_EARLY_ON_FILE:-/etc/ocbm/bench_btd_early}
btd_early_on() {
  for _beo in "$BTD_EARLY_BENCH_ON" "$BTD_EARLY_ON_FILE"; do
    [ -e "$_beo" ] || continue
    _beo_v=""; read -r _beo_v 2>/dev/null <"$_beo"
    [ "$_beo_v" != off ]; return
  done
  _beo=""; return 0
}

wireless_up() {
  # $1 = fallback: today's path after an armed go that failed (go_watch, wireless_go). The go's timeline stamps and
  # its AP went out already, so the bring-up skips its own (OCBM_GO_FALLBACK, in the wrapper below).
  _wu_fb=0
  [ "${1:-}" = fallback ] && _wu_fb=1
  if [ "$_wu_fb" = 0 ] && [ -n "$arm_pid" ]; then
    arm_wait_ready
    if btd_armed_ready; then
      wireless_go "$_ap"
      return
    fi
    if arm_alive; then
      echo "[sup] wireless_up: the arm is still under way after 3 s -- stopping it, bring-up deferred to the teardown's end"
      wireless_down "an arm that did not finish"
      wireless_rebring_at=$(( $(now) + 2 ))
      rebring_after_teardown=1
      return
    fi
    arm_pid=""   # the arm ended (it failed, or its checks gave the armed state up): today's path
  fi
  if radio_off; then
    echo "[sup] radios inhibited (CT_RADIO off) — skipping wireless bring-up"
    return
  fi
  if ! wireless_enabled; then
    echo "[sup] wireless disabled by config (wireless: false) — wired-only"
    wireless_running && wireless_down   # config flipped to false mid-present: honor it
    return
  fi
  # THE CHOKE POINT (owner directive 2026-08-11). Never raise the wireless stack while a WIRED session
  # owns the box and Hot-Handover is off. Guarding the CALLERS was not enough: FOUR paths reach here
  # during a live wired session — the host-presence 0->1 edge (:747), A2's own restore arm, the
  # deferred `wireless_rebring_at` timer (:721, armed by /tmp/wireless_restart or a CT_RADIO edge),
  # and the synthetic 0->1 edge a RESPAWNED supervisor manufactures from `last_p=""`. All four funnel
  # through this function, so the check belongs here.
  #
  # This is what actually delivers "wired => radios off". A2's teardown was already working — the
  # captured log shows it going quiet after two firings, which only happens when `wireless_stack_up`
  # reads false — and then a bring-up put hostapd, wlan0 and hci0 straight back.
  if ! hot_handover_enabled && ! wireless_owns_session && wired_phone_on_bus; then
    echo "[sup] wireless_up SUPPRESSED — a WIRED session owns the box (Hot-Handover off)"
    return
  fi
  # A teardown still running goes first (teardown_in_flight): up to 4 s here, then the bring-up is deferred to the
  # main loop's re-bring timer rather than started under it.
  _tw=0
  while teardown_in_flight && [ "$_tw" -lt 20 ]; do _tw=$((_tw + 1)); sleep 0.2; done
  if teardown_in_flight; then
    echo "[sup] wireless_up: the teardown is still running after 4 s -- bring-up deferred"
    wireless_rebring_at=$(( $(now) + 2 ))
    rebring_after_teardown=1
    return
  fi
  [ "$_tw" -gt 0 ] && echo "[sup] wireless_up waited $((_tw * 200)) ms for the teardown in flight"
  wireless_running && return   # idempotent guard (checked HERE, before the wrapper exists)
  # Clean slate before a fresh bring-up: reap any orphaned A/V children left by a prior session so the
  # new stack can NEVER latch onto / collide with a stale carplayd (the observed
  # "connection unsuccessful" after an app close+reopen). Safe here because this runs only on the
  # host-absent->present edge (app just (re)connected) with wireless enabled, and wlan_on/bt_on below
  # give the SIGTERM'd stragglers a full settle before btd spawns its own children.
  #
  # docs/wireless/00_WIRELESS_CARPLAY.md #1.4 (extended, review finding 2026-07-24): this pkill previously ran UNCONDITIONALLY, which
  # meant the CCPA-tab "Restart wireless"/Forget trigger (`wireless_down` then, ~4s later, THIS
  # function — see the main loop's `wireless_rebring_at` handling) could kill a live WIRED session's
  # carplayd exactly like the bug #1.4 already fixed inside `wireless_down` itself. Only reap
  # here if they are NOT a live wired session: either the transport flag already says "wireless" (a
  # stale, uncleaned prior wireless session — safe and correct to reap), or neither process is even
  # running (nothing to protect).
  _wl_reaped=0
  if wireless_owns_session || ! pgrep -x carplayd >/dev/null 2>&1; then
    _wl_reaped=1
    pkill -f carplayd 2>/dev/null
  else
    echo "[sup] wireless_up: carplayd is wired-owned — leaving it running, skipping the pre-bringup reap"
  fi
  mode=$(pairing_mode)
  restore_stock_hostapd
  usb_observe   # this instant's USB state, for the block's usb_edge stamp: the tick's reading may be a second old
  # The cold boot (above): btd first, waiting, while the boot preload attaches Bluetooth, when the handover is asked for.
  _wu_early=0
  if [ "$_wu_fb" = 0 ] && preload_running && handover_on; then _wu_early=1; fi
  # Round 4 (btd_early, above; on by default since 2.7.1): the early btd with the handover off; it waits for bt_on's end.
  # A file that decides says so; the default has the bring-up's own line below.
  if [ "$_wu_early" = 0 ] && [ "$_wu_fb" = 0 ] && preload_running; then
    if btd_early_on; then
      _wu_early=1
      [ -n "$_beo" ] && echo "[sup] [bench] round4: btd_early ($_beo): the handover is off - btd starts now and waits for bt_on to end, then brings Bluetooth up as today"
    else
      echo "[sup] [bench] round4: btd_early=off ($_beo): bt_on first, then btd, as in 2.7.0"
    fi
  fi
  if [ "$_wu_fb" = 1 ]; then
    echo "[sup] host PRESENT + wireless enabled -> CLEAN bring-up of Bluetooth after the failed armed go (detached, pairing=$mode) at uptime $(cut -d' ' -f1 /proc/uptime)"
  elif [ "$_wu_early" = 1 ]; then
    echo "[sup] host PRESENT + wireless enabled -> CLEAN bring-up of wireless stack (detached, pairing=$mode; the boot preload is attaching Bluetooth: btd starts now and waits for it) at uptime $(cut -d' ' -f1 /proc/uptime)"
  else
    echo "[sup] host PRESENT + wireless enabled -> CLEAN bring-up of wireless stack (detached, pairing=$mode) at uptime $(cut -d' ' -f1 /proc/uptime)"
  fi
  # Detached session: the Wi-Fi AP and the BT radio come up side by side, then the CarLink advertiser, each
  # redirected (not piped).
  # NO inner pgrep guard: the wrapper `sh -c` argv itself contains "/usr/sbin/btd", so an
  # inner `pgrep -f /usr/sbin/btd` self-matches the wrapper and SKIPS the launch (the bug
  # that left btd never started). The outer `wireless_running && return` above is the guard.
  # BT_PAIRING_MODE is inherited through setsid -> sh -c -> the exec'd btd. The body is single-quoted on
  # purpose, so values travel as env vars rather than by interpolating into the wrapper's argv.
  wl_raised=1
  env BT_PAIRING_MODE="$mode" BT_SSP_INTERACTIVE="$(pairing_interactive)" OCBM_WL_REAPED="$_wl_reaped" \
    OCBM_USB_EDGE="$usb_configured_at" OCBM_GO_FALLBACK="$_wu_fb" OCBM_BTD_EARLY="$_wu_early" setsid sh -c '
    # Radio bring-up goes through the chipset-neutral seam. It resolves this unit'"'"'s own
    # bring-up mapping at runtime, so the supervisor never names a chip, a module or an attach
    # helper. Exit codes: 0 converged / 1 failed / 2 already up / 3 unsupported on this variant.
    # Previously these were /script/wlan_on.sh and /script/bt_on.sh — the IW416 baseline'"'"'s own
    # scripts, which simply do not exist on a Realtek or Broadcom unit, so wireless bring-up
    # failed SILENTLY here (the calls sit in this detached wrapper, redirected, exit status
    # unread) while wired projection kept working and nothing looked wrong.
    # Side by side (the Mac connect-time brief of 13 Nov): Bluetooth is on the critical path, and the AP is not needed
    # until the 0x5703 reply from btd, so the AP no longer goes first (its 3.5 s held Bluetooth back on the 12 Nov bench).
    # radio_hal.sh keeps them apart where they meet: separate wlan and bt locks, the WLAN driver load under its own
    # lock (wlan_driver_up), and a descriptor file per caller (radio_detect.sh).
    # The outer subshell returns at once, so the AP bring-up is no child of the btd exec below (which would never reap it).
    # After a failed armed go (OCBM_GO_FALLBACK=1) the AP and these stamps went out with the go: Bluetooth only.
    #
    # BENCH LEVER (2026-09-03): opt-in btmon capture for this wireless session. Armed by either the
    # file flag or the env var -- the outer `env BT_PAIRING_MODE=... setsid sh -c` wrapper
    # above only lists the vars it OVERRIDES, so CARPLAY_BTMON, if exported by the caller, still
    # reaches here through the normal inherited environment. Whether the CCPA rootfs actually ships
    # btmon is unknown, so this must be a no-op (one log line, not a failure) when it is absent.
    btmon_capture() {
      if { [ -f /tmp/carplay_btmon ] || [ "${CARPLAY_BTMON:-0}" = 1 ]; }; then
        if command -v btmon >/dev/null 2>&1; then
          echo "[sup] btmon lever armed -- capturing to /tmp/box.log" >> /tmp/box.log
          # One sed per session (only runs when armed): btmon has no simple "prefix every line"
          # option of its own, and this is cheaper than parsing its output to add one.
          ( btmon 2>&1 | sed "s/^/[btmon] /" >> /tmp/box.log & )
        else
          echo "[sup] btmon lever armed but btmon not found on PATH -- skipping" >> /tmp/box.log
        fi
      fi
    }
    # docs/wireless/00_WIRELESS_CARPLAY.md #1.3 (review finding 2026-07-24): truncate the wireless health log HERE, immediately
    # before btd starts, not right after the (SIGTERM-only, kill-unconfirmed) reap above — truncating early
    # left a window where the OLD carplayd could still flush its buffered stdio (exit-time flush of an
    # O_APPEND-opened, fully-buffered log) and replay the PRIOR session'"'"'s "pair-verify OK"/"RECORD
    # done" lines back in after the truncate, exactly the bogus-instant-health bug this truncation
    # exists to prevent.
    #
    # That fix used to rely on wlan_on.sh + bt_on.sh taking ~15s, which gave the SIGTERM'"'"'d
    # stragglers a real settle window "for free". Behind the seam that assumption is gone in BOTH
    # directions: a variant with nothing to do, an already-converged fast path, or an honest
    # "unsupported" exit all return in milliseconds. So confirm the reap actually completed
    # instead of inferring it from elapsed time — this is strictly stronger than the old timing,
    # under which a SIGTERM-ignoring straggler survived even the 15s.
    #
    # Gated on OCBM_WL_REAPED: in the wired-owned skip branch a live wired carplayd is EXPECTED
    # and writes to its own log, not this one, so it must not burn the bound.
    av_settled() {
      if [ "$OCBM_WL_REAPED" = 1 ]; then
        _i=0
        while [ "$_i" -lt 25 ] && { pgrep -x carplayd || pgrep -x /usr/sbin/carplayd; } >/dev/null 2>&1; do
          _i=$((_i + 1)); sleep 0.2
        done
      fi
      { true > /tmp/carplayd_wl.log; } 2>/dev/null
    }
    if [ "$OCBM_GO_FALLBACK" != 1 ]; then
    echo "[sup] radios starting at uptime $(cut -d" " -f1 /proc/uptime)" >> /tmp/box.log
    # The connect timeline (crates/box-common/src/timeline.rs): the radios start now, and the host-facing USB link
    # last came up at OCBM_USB_EDGE (empty: not known). One append each, no fork; a failed one is ignored.
    _tl_up=""
    read -r _tl_up _tl_idle 2>/dev/null </proc/uptime
    case "$_tl_up" in *.*) echo "t sup_radios ${_tl_up%.*}${_tl_up#*.}0" 2>/dev/null >>/tmp/startup_marks ;; esac
    [ -n "$OCBM_USB_EDGE" ] && echo "t usb_edge $OCBM_USB_EDGE" 2>/dev/null >>/tmp/startup_marks
    ( sh /script/radio_hal.sh wifi_ap_on >/tmp/wlan.log 2>&1 & )
    else
    echo "[sup] radios after a failed armed go at uptime $(cut -d" " -f1 /proc/uptime): Bluetooth by bt_on and a fresh btd" >> /tmp/box.log
    fi
    # S5 (both btd starts below): O_APPEND (>>), not >. The in-place tail-truncate in bound_logs (~:551-555) relies on
    # the writer fd being O_APPEND so writes after truncation land at the new end-of-file; a
    # non-append fd keeps writing at its old offset, leaving a sparse hole and re-truncating to
    # nothing every tick.
    if [ "$OCBM_BTD_EARLY" = 1 ]; then
      # THE COLD BOOT (R3-5): the boot preload is attaching Bluetooth. btd starts first and waits for the controller
      # (OCBM_BTD_WAIT, crates/vendor/wireless/src/handover.rs), with the capture lever and the settled A/V log before
      # it as before an exec; bt_on runs beside it, and the file after it tells btd that bt_on is over. btd goes on
      # the preload handing it the controller as hci0 answers, or on that file (then as a btd started after bt_on).
      btmon_capture
      av_settled
      rm -f /tmp/.btd_bt_on_done
      OCBM_BTD_WAIT=1 /usr/sbin/btd </dev/null >>/tmp/wl.log 2>&1 &
      sh /script/radio_hal.sh bt_on 2>&1 | tee -a /tmp/box.log >/tmp/bt.log
      { true > /tmp/.btd_bt_on_done; } 2>/dev/null
      exit 0
    fi
    # bt.log stays the small truncated per-attempt status file that docs/wireless/01_BT_AND_RADIO.md
    # tells an operator to cat directly. This call is foreground (no trailing ampersand), so teeing
    # its combined stdout/stderr into the universal log costs nothing and changes no lifecycle.
    sh /script/radio_hal.sh bt_on 2>&1 | tee -a /tmp/box.log >/tmp/bt.log
    # btd starts once Bluetooth is up, not once the AP is (about 1-2 s of every connect on the Mac 13 Nov bench). Paging
    # and the iAP2 set-up come first; its 0x5702 reply, the first thing that needs the AP, waits while wifi_ap_on still
    # holds the wlan lock (wifi_handoff::wait_for_ap), so it never describes an AP that is not serving yet.
    btmon_capture
    av_settled
    unset OCBM_BTD_WAIT   # this btd never waits for a handover, whatever the supervisor inherited (21 Nov review I)
    exec /usr/sbin/btd </dev/null >>/tmp/wl.log 2>&1
  ' </dev/null >/dev/null 2>&1 &
}

wireless_down() {
  # $1 = why, for the log only. Default keeps the historical host-presence wording; the wired-takeover
  # caller passes its own so the log never claims the app went away when it did not.
  _wd_why="${1:-host GONE (or wireless disabled)}"
  # An arm or a go is over with this: the teardown below ends the btd behind either (an armed btd answers its SIGTERM
  # at once). A pid file it could not remove names nobody now.
  arm_pid=""; go_pending=0
  rm -f "$BTD_ARMED_FILE" 2>/dev/null
  # COMPLETE wireless-stack teardown, tied to app presence: the advertiser (btd) AND its
  # setsid-detached A/V children (carplayd). pkill-ing ONLY the parent orphans the children,
  # and the next app-connect bring-up then collides with those orphans ("Address in use" on the
  # advertiser / a stale AirPlay receiver) -> iOS reports "connection unsuccessful". No head-unit app
  # means no possible session (wired OR wireless), so reaping the whole A/V stack here is correct and
  # returns the box to the clean waiting state. Reap even if btd already died (the
  # detached children can outlive it).
  # `carplayd_alive` not `pgrep -x carplayd`: BusyBox `-x` matches the FULL invoked path, so a bare
  # `-x carplayd` is BLIND to the wireless spawn form `/usr/sbin/carplayd` (device-proven, docs/wireless/00_WIRELESS_CARPLAY.md).
  # A stranded wireless carplayd would otherwise trip this early return and never be reaped.
  # Not after a go or a bring-up that no teardown has followed (wl_raised), nor while an AP bring-up is under way: an AP
  # still coming up is none of the things below yet.
  if [ "$wl_raised" = 0 ] && ! ap_bringup_in_flight && ! wireless_running && ! carplayd_alive \
     && ! pgrep -x hostapd >/dev/null 2>&1 && ! { hciconfig hci0 2>/dev/null | grep -q "UP"; }; then
    return   # already fully down — processes dead AND radios actually powered off (docs/carplay/04_CAPABILITIES_AND_CONFIG.md: a
             # crashed stack can leave hostapd beaconing / hci0 UP with all three daemons gone;
             # the early return must not skip the radio teardown in that stranded state)
  fi
  if wireless_owns_session || ! wired_carplayd_live; then
    echo "[sup] $_wd_why -> tearing down COMPLETE wireless stack (advertiser + A/V children)"
    # Stop the CarLink advertiser + the A/V daemons + drop the AP (the WiFi beacon is the main drain).
    # BT end-state (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating): module stays ATTACHED (hciattach kept — re-attach is the
    # flaky part) but the radio is POWERED DOWN (noscan, then hci0 down at the end of this block) so
    # an app-less box drops ACLs and answers nothing. Since 16 Nov 2026 the main loop then arms it for the next host
    # once this block has ended (arm_gate): powered again, with no scan, no link and nothing paging.
    # SIGTERM first, brief settle, then SIGKILL only the A/V daemons (NOT the BT hci path
    # — heavy -9 on BT bring-up is what wedges the IW416 controller).
    # SELF-MATCH FIX 2026-08-01: the patterns MUST use the `[x]`-char-class form. This block runs as
    # `setsid sh -c '<this text>'`, so its own argv literally contains "/usr/sbin/btd",
    # "carplayd" — a plain `pkill -f /usr/sbin/btd` SIGKILLs THIS subshell
    # before it reaches wlan_off, orphaning the AP (hostapd kept broadcasting → iOS shows the phone still
    # on WiFi with CarPlay inactive). `[/]usr/...` matches the real daemon but NOT this text (verified:
    # busybox pgrep `[h]ostapd` matches, `[z]hostapd` doesn't). Same footgun the wireless_up guard notes.
    # COMMENTS INSIDE THE BLOCK ARE ARGV TOO (2026-10-20): a `# btmon ...` comment on the `[b]tmon`
    # line made that pkill SIGTERM this subshell right after btd/carplayd, so wlan_off and bt_off
    # never ran - hostapd kept beaconing and hci0 stayed up after every app disconnect. Never write
    # a pattern's plain word anywhere in the block.
    teardown_mark
    wl_raised=0
    env WL_TEARDOWN="$_td" setsid sh -c '
      # Its pid in its own marker: wireless_up waits while this block lives.
      echo "$WL_TEARDOWN $$" > "/tmp/.wl_teardown.$WL_TEARDOWN.new" && mv -f "/tmp/.wl_teardown.$WL_TEARDOWN.new" "/tmp/.wl_teardown.$WL_TEARDOWN"
      pkill -f "[/]usr/sbin/btd" 2>/dev/null
      pkill -f "[c]arplayd" 2>/dev/null
      pkill -f "[b]tmon" 2>/dev/null   # the 2026-09-03 HCI-capture bench lever, harmless if never armed
      sleep 1
      pkill -9 -f "[c]arplayd" 2>/dev/null
      pkill -9 -f "[/]usr/sbin/btd" 2>/dev/null
      pkill -9 -f "[b]tmon" 2>/dev/null
      # S4: the phase-mirror unlink moved IN HERE, after the backgrounded SIGKILL above, so a
      # still-live btd (killed -9 but not yet reaped) cannot re-publish a stale
      # /tmp/bt_phase after the synchronous outer unlink used to run (audit 3.3: last-write-wins,
      # no other unlink point).
      rm -f /tmp/bt_phase
      # The phone identity goes too: no session is left for it to name (bench 2026-11-02).
      rm -f /tmp/phone_identity
      sh /script/radio_hal.sh wifi_ap_off >/tmp/wlan_off.log 2>&1
      # docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating: POWER the BT radio off (page/inquiry dead, ACLs dropped), not just
      # noscan — an app-less box must not keep the iPhone BT-connected. Through the SEAM bt_off
      # verb, which is exactly this noscan+down and leaves the attach in place (NOT bt_off.sh, whose
      # rmmod path is the wedge-prone one); the next bring_up() does its proven down->up cycle.
      # NOTE: no apostrophes in this block - it is the body of a single-quoted setsid sh -c.
      # Raw `hciconfig hci0 noscan/down` here ran OUTSIDE the radio_hal.sh bt lock, so it could
      # land between a concurrent bt_on attach and its readback and present as a wedged
      # controller; it also hardcoded hci0 instead of the discovered device.
      sh /script/radio_hal.sh bt_off >/tmp/bt_off.log 2>&1
      # This teardown is over: its marker goes, and the loop is woken for a bring-up it deferred (teardown_in_flight).
      rm -f "/tmp/.wl_teardown.$WL_TEARDOWN"
      _sp=$(cat /tmp/.sup_lock/pid 2>/dev/null)
      [ -n "$_sp" ] && grep -q "[s]ession_supervisor" "/proc/$_sp/cmdline" 2>/dev/null && kill -USR1 "$_sp" 2>/dev/null
    ' </dev/null >/dev/null 2>&1 &
    # docs/wireless/00_WIRELESS_CARPLAY.md #1.2/#1.3: this is OUR session ending — clear the health state we own and the transport
    # flag ourselves (value-scoped, so it never fires against a wired session's "wired"/absent value),
    # rather than relying on the (possibly already-dying) child processes to clean up after themselves.
    saw_paired=0; saw_record=0; healthy=0; established_since=0; write_healthy 0
    [ "$(cat /tmp/carplay_transport 2>/dev/null)" = wireless ] && rm -f /tmp/carplay_transport
  else
    # docs/wireless/00_WIRELESS_CARPLAY.md #1.4: the running carplayd is the WIRED arm's (wired_carplayd_live) — never
    # kill it from here. Previously this function's pkill list had no such guard, so a CCPA-tab
    # "Restart wireless"/Forget action during a live WIRED session killed the wired carplayd too — a
    # real regression, not a wireless-only effect. Only the wireless-specific advertiser + radios are
    # ours to tear down in this case.
    echo "[sup] $_wd_why -> tearing down wireless advertiser + radios only (carplayd are wired-owned, left running)"
    # SELF-MATCH FIX 2026-08-01: `[/]usr/...` so this `sh -c` block doesn't SIGKILL itself before wlan_off
    # (see the COMPLETE-teardown branch above for the full rationale).
    teardown_mark
    wl_raised=0
    env WL_TEARDOWN="$_td" setsid sh -c '
      # Its pid in its own marker: wireless_up waits while this block lives.
      echo "$WL_TEARDOWN $$" > "/tmp/.wl_teardown.$WL_TEARDOWN.new" && mv -f "/tmp/.wl_teardown.$WL_TEARDOWN.new" "/tmp/.wl_teardown.$WL_TEARDOWN"
      pkill -f "[/]usr/sbin/btd" 2>/dev/null
      pkill -f "[b]tmon" 2>/dev/null   # the 2026-09-03 HCI-capture bench lever, harmless if never armed
      sleep 1
      pkill -9 -f "[/]usr/sbin/btd" 2>/dev/null
      pkill -9 -f "[b]tmon" 2>/dev/null
      sh /script/radio_hal.sh wifi_ap_off >/tmp/wlan_off.log 2>&1
      # docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating: power BT off here too, through the locked seam bt_off
      # verb (see the COMPLETE-teardown branch rationale).
      sh /script/radio_hal.sh bt_off >/tmp/bt_off.log 2>&1
      # This teardown is over: its marker goes, and the loop is woken for a bring-up it deferred (teardown_in_flight).
      rm -f "/tmp/.wl_teardown.$WL_TEARDOWN"
      _sp=$(cat /tmp/.sup_lock/pid 2>/dev/null)
      [ -n "$_sp" ] && grep -q "[s]ession_supervisor" "/proc/$_sp/cmdline" 2>/dev/null && kill -USR1 "$_sp" 2>/dev/null
    ' </dev/null >/dev/null 2>&1 &
  fi
}

[ "${OCBM_SUP_FUNCS:-}" = 1 ] && return 0   # sourced for its functions (tools/session_supervisor_test.sh)

write_healthy 0
echo "[sup] up; IDLE — gating the CarPlay session on $FLAG (waiting for a host app)"
WIRELESS_RESTART_FLAG=/tmp/wireless_restart   # ocbmd touches this on the CCPA tab's Restart-wireless / Forget
wireless_rebring_at=""                         # epoch(uptime) to re-bring-up after a requested restart
rebring_after_teardown=0   # that re-bring-up waits on a teardown: it goes as soon as none runs (its block wakes us)
wireless_was_owner=0    # tracks wireless_owns_session across ticks (see the reset check below)
last_r=0                # tracks the CT_RADIO inhibit flag across ticks (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating)

# Startup reconciliation (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating, gap G3): a respawned supervisor starts with
# last_p="" and never sees a 1->0 edge, so a crash between GONE and teardown would strand radios
# ON with no app. If the app is absent but any radio state is up, reconcile once now. No-op on a
# clean boot (boot chain raises no radios).
# An armed btd from before this supervisor (it was restarted): not this one's to go with, whatever the host. Down,
# and armed again by this supervisor once no host is here; a host here meanwhile takes today's path after it.
_left_armed=""
read -r _left_armed 2>/dev/null <"$BTD_ARMED_FILE"
if [ -n "$_left_armed" ] && proc_live_btd "$_left_armed"; then
  echo "[sup] startup: an armed btd ($_left_armed) from before this supervisor -> wireless_down (armed again with no host)"
  wireless_down "a restarted supervisor (an armed btd it did not start)"
elif [ "$(cat "$FLAG" 2>/dev/null)" != 1 ]; then
  if wireless_running || pgrep -x hostapd >/dev/null 2>&1 || hciconfig hci0 2>/dev/null | grep -q "UP"; then
    echo "[sup] startup reconciliation: app absent but radio state up -> wireless_down (stranded by a prior crash)"
    wireless_down
  fi
fi

while :; do
  # Read presence, acting only on a DEFINITE 0/1 — an empty/partial read (or missing file) keeps the
  # last state rather than flapping. (ocbmd writes the flag atomically, so this is defensive.)
  # With `read`, not `$(cat ...)`: the armed go counts its milliseconds from this read (ocbmd writes one character).
  woken=0
  p=""
  read -r p 2>/dev/null <"$FLAG"
  # A host just came: bring wireless up FIRST, before the rest of this tick's checks, each a fork on this CPU. They
  # put about 1.3 s between the host and radio_hal.sh's first line (the Mac's 13 Nov brief, item 1).
  wl_edge_done=0
  if [ "$p" = 1 ] && [ "${last_p:-}" != 1 ]; then
    uptime_now
    echo "[sup] host PRESENT seen at uptime $_up_raw"
    wireless_up; wl_edge_done=1
  fi
  go_watch      # an armed go's answer, while one is awaited (no fork)
  usb_observe   # the host-facing USB link, once a tick, after the host's edge (usb_observe: no fork)

  # docs/wireless/00_WIRELESS_CARPLAY.md #1.3 (extended, review finding 2026-07-24): `wireless_down`'s own branch already resets
  # saw_paired/saw_record/healthy when IT tears a wireless session down — but a wireless session can
  # ALSO end via `teardown_av_layer()` (crates/vendor/wireless/src/av.rs, called on preempt/shutdown),
  # which removes /tmp/carplay_transport directly without ever calling wireless_down. Without this
  # edge-detect, that second exit path left stale healthy=1/saw_record=1 latched — the supervisor would
  # keep publishing phase=STREAMING (and /tmp/session_healthy=1) for a session that already ended,
  # until happenstance triggered a wired arm() to reset them (never, if no USB phone is attached).
  # Checked every tick, transport-agnostic of WHICH exit path fired.
  if wireless_owns_session; then
    wireless_was_owner=1
  elif [ "$wireless_was_owner" = 1 ]; then
    wireless_was_owner=0
    echo "[sup] wireless session ended (transport flag cleared) -- resetting health state"
    saw_paired=0; saw_record=0; healthy=0; established_since=0; write_healthy 0
  fi

  # (C) Stale-flag watchdog (dual-transport self-heal): /tmp/carplay_transport is only ever cleared by
  # btd's teardown_av_layer() (av.rs) on a CLEAN SIGTERM. A wireless session that dies
  # WITHOUT that (crash / SIGKILL / OOM — panic=abort does no unwinding) leaves the flag stuck at
  # "wireless" forever, which suppresses the ENTIRE wired path (arm/kill_session/escalate all gate on
  # wireless_owns_session). If the flag says wireless but NEITHER btd NOR any carplayd is
  # alive for STALE_FLAG_N consecutive ticks, the owning session is provably gone — clear the orphaned
  # flag. Consecutive (not instant) so we never race a btd momentarily between claims or an
  # carplayd mid-(re)spawn. Requiring carplayd ALSO dead keeps this maximally conservative: it never
  # clears a flag while any carplayd could still be serving a screen (the preempt edge above handles a
  # live-but-superseded wireless session when a real wired cable appears).
  if wireless_owns_session && ! wireless_running && ! carplayd_alive; then
    stale_flag_ticks=$((stale_flag_ticks + 1))
    if [ "$stale_flag_ticks" -ge "$STALE_FLAG_N" ]; then
      echo "[sup] STALE transport flag: 'wireless' with no btd/carplayd for $stale_flag_ticks ticks -> clearing (self-heal, no reboot)"
      rm -f /tmp/carplay_transport
      stale_flag_ticks=0
    fi
  else
    stale_flag_ticks=0
  fi

  # CCPA-tab wireless restart (ocbmd sets the flag for Restart-wireless, Forget-all, Forget-device): tear
  # btd down now, then re-bring-up a few ticks later (once it's actually gone) if a host is
  # still present. This is how the app's box controls + a forget (reload with cleared keys) take effect.
  if [ -f "$WIRELESS_RESTART_FLAG" ]; then
    rm -f "$WIRELESS_RESTART_FLAG"
    echo "[sup] wireless restart requested (CCPA tab)"
    wireless_down
    wireless_rebring_at=$(( $(now) + 4 ))
    # With no host here (a forget ocbmd finished after its host left), the restart is a fresh arm once it is down.
    [ "${p:-0}" = 1 ] || arm_next=1
  fi
  # Deferred behind a teardown, it goes once none runs, never on its timer meanwhile: each firing blocked the tick ~4 s in
  # wireless_up and armed it again, so a long teardown left the loop asleep ~4 s of every 6, host edges and a wired
  # takeover late by as much (the Mac's 2.6.0 review, CS-5). The teardown's block wakes the loop (USR1) as it ends.
  rebring_due=0
  if [ -n "$wireless_rebring_at" ]; then
    if [ "$rebring_after_teardown" = 1 ]; then
      teardown_in_flight || rebring_due=1
    elif [ "$(now)" -ge "$wireless_rebring_at" ]; then
      rebring_due=1
    fi
  fi
  if [ "$rebring_due" = 1 ]; then
    wireless_rebring_at=""; rebring_after_teardown=0
    # This tick's read, not last_p: on a host edge last_p is still the old value until the edge block below, and a
    # deferral that came due this same tick was dropped (the Mac's review of bbd29f3).
    [ "${p:-${last_p:-0}}" = 1 ] && wireless_up
  fi

  # App-commanded radio inhibit edges (CT_RADIO -> /tmp/radio_off, docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating). Flag
  # appeared -> radios down NOW (wireless_down's docs/wireless/00_WIRELESS_CARPLAY.md #1.4 guard protects a wired-owned
  # carplayd). Flag cleared -> bring-up rides the existing 4 s wireless_rebring_at deferral, NOT a
  # direct wireless_up: (a) a quick off->on toggle must not race the off-edge's detached teardown
  # (same class the Restart-wireless deferral exists for), and (b) go_idle clears this flag and
  # host_present in the same instant, so an on-edge can arrive on the very tick the app went away
  # with last_p still stale — the deferred handler re-checks presence at fire time.
  if radio_off; then r=1; else r=0; fi
  if [ "$r" = 1 ] && [ "$last_r" != 1 ]; then
    echo "[sup] CT_RADIO off -> tearing radios down (app-commanded)"
    wireless_down
  fi
  if [ "$r" = 0 ] && [ "$last_r" = 1 ]; then
    echo "[sup] CT_RADIO on -> scheduling radio bring-up (deferred past teardown settle)"
    wireless_rebring_at=$(( $(now) + 4 ))
  fi
  last_r="$r"

  # presence 0->1 edge accounting (input to the flap detector) + app-driven wireless bring-up/teardown
  case "$p" in
    0|1)
      if [ "$p" = 1 ] && [ "$last_p" != 1 ]; then
        edge_ts="$edge_ts $(now)"
        [ "$wl_edge_done" = 1 ] || wireless_up   # host appeared (normally done at the top of the tick)
      fi
      if [ "$p" = 0 ] && [ "$last_p" = 1 ]; then
        # Armed Bluetooth for the next host once this teardown is over, when this host's session had wireless up (not
        # one that was wired-only, inhibited or `wireless: false`, which keeps Bluetooth down between sessions).
        if wireless_running; then arm_next=1; else arm_next=0; fi
        wireless_down    # host went away: idle the wireless radios (wired baseline stays ready)
      fi
      last_p="$p"
      ;;
  esac

  case "$p" in
    1)
      # Phone-presence gate (2026-07-12): with a host present, check the bus EVERY tick. No iPhone
      # = a legitimate WAIT state — do NOT attempt projection, do NOT count failures, and above all
      # do NOT escalate the ladder (phone_resets/L2 fired at an intentionally-absent phone were what
      # wedged the staged-timing tests). The moment the phone appears, arm IMMEDIATELY (clear any
      # backoff) — this is the plug-triggered retry that cuts plug→pixels to protocol time.
      if phone_on_bus; then
        phone_absent_ticks=0
        write_phone_flag 1
        if [ "$phone_waiting" = 1 ]; then
          phone_waiting=0
          proj_fails=0
          backoff_until=0
          echo "[sup] iPhone appeared on the bus — arming NOW"
        fi
      else
        # Debounce (3 ticks ≈ 3 s): the role-switch transition briefly reads as absent in BOTH modes
        # (device detached, gadget not yet CONFIGURED) — never flicker a false "waiting for phone".
        phone_absent_ticks=$(( ${phone_absent_ticks:-0} + 1 ))
        if [ "$phone_absent_ticks" -ge 3 ]; then
          write_phone_flag 0
          if [ "$armed" = 1 ]; then
            : # mid-session unplug: let the health/death paths handle teardown naturally
          else
            [ "$phone_waiting" != 1 ] && echo "[sup] host PRESENT, no iPhone attached — waiting for plug (no escalation)"
            phone_waiting=1
          fi
        fi
      fi
      # (A) Dual-transport preempt edge — GATED on the opt-in Hot-Handover lever (default OFF = Standard,
      # spec-conformant: a cable plugged into a live wireless session is left charge-only, wireless keeps
      # running). Only when the host enables `hot_handover: true` does a genuine wired iPhone on the USB bus
      # preempt a wireless session. The guard's wired_iphone_on_usb() is the RAW 05ac bus probe, NOT
      # wireless_owns_session (the flag that can lie), so this fires ONLY for a real cable and never during a
      # healthy wireless-only session. preempt_wireless_for_wired() returns only once the flag is clear, so
      # the wired arm path below then runs (wireless_owns_session is now false).
      if hot_handover_enabled && wireless_owns_session && wired_iphone_on_usb; then
        preempt_wireless_for_wired
      fi
      # (A2) STANDARD-MODE RADIO GATING — the mirror of (A), owner directive 2026-08-11.
      #
      # (A) stops a cable PREEMPTING a live wireless session when Hot-Handover is off. Nothing did the
      # reverse: `wireless_down` is gated on HOST PRESENCE ("no head-unit app means no possible
      # session"), so once a wireless session ended and a wired one armed, hostapd kept beaconing,
      # wlan0 stayed up and hci0 stayed powered for the whole wired session — observed on a 528 MHz
      # single core as "serious performance issues".
      #
      # EDGE-TRIGGERED, DELIBERATELY. Every other `wireless_down` caller in this file fires on an edge
      # (host 1->0, CT_RADIO, the restart flag, startup reconciliation). An earlier revision of this
      # block evaluated a LEVEL every tick, which is unsafe here: `wireless_down` is a detached
      # `setsid` doing `sleep 1` + `wlan_off.sh` (which `rmmod moal` / `rmmod mlan`), so it stays
      # in-flight for seconds while `wireless_stack_up` is still true. A level trigger therefore
      # spawns a new teardown every second and stacks concurrent rmmods on the same module — the
      # driver-wedge class `wireless_up`'s own `wireless_running && return` guard exists to avoid.
      #
      # PREDICATE: `phone_on_bus`, NOT `wired_iphone_on_usb`. The raw probe greps Apple VID 05ac and
      # its own comment says what that means: "once a phone role-switches into projection it stops
      # enumerating as 05ac ... so this matches a FRESHLY-plugged cable — exactly the preempt trigger
      # — not an already-projected wired session." That is the opposite of the window A2 needs, and
      # using it made A2 a self-cancelling oscillator: tear down on plug, then `projection_up.sh`
      # switches the gadget to 08e4, 05ac vanishes, the falling edge restores the radios for the rest
      # of the wired session. `phone_on_bus` was hardened in July for this same class of bug and
      # covers both modes. A2 asks its wired half, `wired_phone_on_bus`, after ONE read of the transport
      # flag: asking `phone_on_bus` read the flag a second time, and a switch clearing it in between made
      # A2 see a wired session that was not there (bench 2026-11-02; see `wired_phone_on_bus`).
      _a2_want=0
      if ! hot_handover_enabled && ! wireless_owns_session && wired_phone_on_bus; then
        _a2_want=1
      fi
      if [ "$_a2_want" != "${_a2_state:-0}" ]; then
        if [ "$_a2_want" = 1 ]; then
          # RISING edge: a wired session now owns the box. Claim the restore obligation ONLY if a
          # teardown actually ran — setting it unconditionally made A2 "restore" a stack it had never
          # touched, fighting the host-presence edge that owns bring-up.
          if wireless_stack_up; then
            wireless_down "WIRED session active + Hot-Handover OFF (radios yield to the cable)"
            radios_yielded_to_wired=1
          fi
        elif [ "${radios_yielded_to_wired:-0}" = 1 ]; then
          # FALLING edge: cable gone, or Hot-Handover was switched on. Test the guards BEFORE clearing
          # the flag — clearing first meant a restore could be dropped and the radios left down for
          # the rest of the app session with no way back short of an app reconnect.
          if wireless_enabled; then
            radios_yielded_to_wired=0
            echo "[sup] wired session ended -> restoring the wireless stack"
            wireless_up
          fi
        fi
        _a2_state="$_a2_want"
      fi
      if wireless_owns_session; then
        # docs/wireless/00_WIRELESS_CARPLAY.md #1.3: a live wireless session owns carplayd right now. `armed` stays 0 for the whole
        # duration (arm()/kill_session()/escalate() all suppress themselves against wireless), so
        # WITHOUT this branch the wired logic below would keep calling arm() — which returns 1 here —
        # churn proj_fails, and eventually fire escalate("projection-failure") against a session the
        # wired supervisor doesn't manage; the phase would also misreport ARMING/IDLE instead of
        # STREAMING for a perfectly healthy wireless session. Track ITS health independently instead:
        # no arm attempts, no escalation, just milestone scanning (transport-scoped inside
        # scan_milestones) so `healthy=1` — and therefore the STREAMING phase below — can actually be
        # reached for a wireless session.
        scan_milestones
      elif [ "$armed" = "0" ] && [ "$phone_waiting" = 1 ]; then
        : # waiting for the phone — publish phase below, skip arm/escalation entirely
      elif [ "$armed" = "0" ]; then
        if [ "$(now)" -ge "$backoff_until" ]; then
          arm; _armrc=$?
          if [ "$_armrc" = 0 ]; then
            proj_fails=0
          elif [ "$_armrc" = 2 ]; then
            # BOX MISCONFIGURED. Deterministic: it will fail identically on every retry, so counting
            # it toward PROJ_AT would march a healthy phone through phone_reset -> ocbmd restart ->
            # REBOOT and change nothing (measured 2026-08-27, two L1 resets for a missing binary).
            # Retry slowly so a fix pushed to the box is still picked up without a reboot, and say so
            # once a minute rather than every second.
            proj_fails=0
            if [ "$(now)" -ge "${env_fault_log_until:-0}" ]; then
              echo "[sup] NOT escalating: the box is misconfigured, not the phone — see the [proj] line above"
              env_fault_log_until=$(( $(now) + 60 ))
            fi
            backoff_until=$(( $(now) + 15 ))
          else
            # projection bring-up failed (e.g. iPhone not enumerating as 05ac after a disturbance).
            # Count it; after PROJ_AT in a row, escalate into the ladder so phone_reset re-enumerates
            # the iPhone (docs/carplay/02_SESSION_LIFECYCLE.md #1 — the gap that forced a manual reboot during the P0 deploy).
            proj_fails=$((proj_fails + 1))
            echo "[sup] projection bring-up failed (#$proj_fails)"
            if [ "$proj_fails" -ge "$PROJ_AT" ]; then
              escalate "projection-failure"
              proj_fails=0
            else
              backoff_until=$(( $(now) + 4 ))
            fi
          fi
        fi
      elif ! pgrep -x carplayd >/dev/null 2>&1 || ! pgrep -x iap2d >/dev/null 2>&1; then
        # either session daemon died while PRESENT -> re-ARM. Was three (carplayd,
        # iap2d); rx-connect is now a thread inside carplayd, so its death is carplayd's death.
        # (docs/wireless/00_WIRELESS_CARPLAY.md #1.1 hygiene: -x matches the process name, not -f's full-argv substring — avoids the
        # same false-match class already fixed in av.rs's Rust `running()`, e.g. a log tail/grep/editor
        # with "carplayd" in its own argv suppressing a needed re-ARM.)
        dead=""; for d in carplayd iap2d; do pgrep -x "$d" >/dev/null 2>&1 || dead="$dead $d"; done
        fails=$((fails + 1))
        back=$((fails * 5)); [ "$back" -gt 30 ] && back=30
        backoff_until=$(( $(now) + back ))
        echo "[sup]$dead exited while PRESENT -> re-ARM after ${back}s (fail #$fails)"
        armed=0
        write_healthy 0
      else
        # armed + carplayd alive: derive establishment health, detect a stall
        scan_milestones
        if [ "$saw_record" = 0 ] && [ "$(now)" -ge "$estab_deadline" ]; then
          stuck_fails=$((stuck_fails + 1))
          echo "[sup] ESTABLISHMENT STALL (#$stuck_fails): ARMED but no RECORD within grace (paired=$saw_paired)"
          if [ "$stuck_fails" -ge "$L1_AT" ]; then
            escalate "establishment-stall"
          else
            kill_session; armed=0; write_healthy 0   # soft re-ARM (below L1 threshold)
            backoff_until=$(( $(now) + 2 ))
          fi
        fi
      fi
      ;;
    0)
      [ "$armed" = "1" ] && teardown
      apply_pending   # idle boundary — apply any deferred peer-store mutation (#25)
      # Armed Bluetooth between host sessions: the arm watched, and started once its gate opens (arm_gate, cheapest
      # check first; the teardown's end wakes this loop for it).
      arm_watch
      arm_gate && arm_wireless
      ;;
    *) : ;;  # empty/unexpected read -> keep last state
  esac

  # --- presence-flap detector (the docs/carplay/02_SESSION_LIFECYCLE.md signature) -> escalation ladder ---
  prune_edges
  if [ "$edge_count" -ge "$FLAP_N" ] && [ "$(now)" -ge "$backoff_until" ]; then
    echo "[sup] flapping: $edge_count present-edges in ${FLAP_WINDOW}s"
    escalate "flapping"
  fi

  # --- counter reset: ONLY on a confirmed-established session held >= CONFIRM_HOLD (never in teardown) ---
  if [ "$healthy" = 1 ] && [ "$established_since" != 0 ] \
     && [ $(( $(now) - established_since )) -ge "$CONFIRM_HOLD" ]; then
    if [ "$stuck" != 0 ] || [ "$stuck_fails" != 0 ] || [ "$fails" != 0 ] \
       || [ "$l1_tries" != 0 ] || [ "$l2_tries" != 0 ]; then
      echo "[sup] session healthy ${CONFIRM_HOLD}s+ — clearing STUCK counters"
    fi
    stuck=0; stuck_reason=""; stuck_fails=0; fails=0; proj_fails=0; edge_ts=""; l1_tries=0; l2_tries=0
    # a confirmed-good session clears the persistent L3 reboot budget (the reboot cycle recovered)
    if [ -s "$REBOOT_BUDGET" ] && [ "$(cat "$REBOOT_BUDGET" 2>/dev/null)" != 0 ]; then
      rm -f "$REBOOT_BUDGET"; sync; echo "[sup] confirmed-good session — cleared L3 reboot budget"
    fi
    established_since=0   # one-shot clear so we don't re-log every tick while healthy
  fi

  # --- publish the canonical lifecycle verdict (#26) ---
  if   [ "$stuck" = 1 ];                                    then ph=STUCK;        rs="$stuck_reason"
  elif [ "$healthy" = 1 ];                                  then ph=STREAMING;    rs=""
  elif [ "$l1_tries" != 0 ] || [ "$l2_tries" != 0 ];       then ph=RECOVERING;   rs="self-heal l1=$l1_tries l2=$l2_tries"
  elif [ "$armed" = 1 ];                                    then ph=ESTABLISHING; rs="paired=$saw_paired"
  elif [ "$phone_waiting" = 1 ] && [ "${last_p:-0}" = 1 ]; then ph=WAITING_PHONE; rs="plug in the iPhone"
  elif [ "${last_p:-0}" = 1 ];                             then ph=ARMING;       rs=""
  else                                                          ph=IDLE;         rs=""
  fi
  # transition ring (#30): one line per PHASE change (never per tick), uptime-stamped for correlation;
  # count-bounded in bound_logs (NOT the lossy byte-tail, so a flap's ONSET survives).
  if [ "$ph" != "$last_phase" ]; then
    printf 't=%s %s -> %s reason=%s\n' "$(now)" "${last_phase:-BOOT}" "$ph" "$rs" >> "$RING" 2>/dev/null
    last_phase="$ph"
  fi
  write_state "$ph" "$rs"

  # ocbmd liveness escalation (rate-limited, independent of the phone-session ladder above — a
  # wedged ocbmd blocks the USB control plane regardless of phone/session state).
  if ocbmd_wedged; then
    _now=$(now)
    if [ $((_now - wedge_escalate_ts)) -ge "$WEDGE_RATE_LIMIT" ]; then
      wedge_escalate_ts=$_now
      _old_pid=$(pidof ocbmd 2>/dev/null)
      echo "[sup] ocbmd wedged (alive mtime stale >=1min, gadget CONFIGURED, pid=$_old_pid) — L2 restart" >> /tmp/box.log
      restart_ocbmd_daemon
      _waited=0
      while [ "$_waited" -lt 10 ]; do
        _new_pid=$(pidof ocbmd 2>/dev/null)
        case " $_new_pid " in
          *" $_old_pid "*) ;;
          *) _old_pid="" ;;
        esac
        [ -z "$_old_pid" ] && break
        sleep 1
        _waited=$((_waited + 1))
      done
      if [ -n "$_old_pid" ]; then
        echo "[sup] ocbmd unkillable (kernel wait) — L3 reboot" >> /tmp/box.log
        tail -c 262144 /tmp/box.log > /script/box_crash.log 2>/dev/null  # survives the reboot; ocbm_boot.sh streams it
        sync; reboot
      fi
    fi
  fi

  tick=$((tick + 1))
  [ $((tick % 30)) -eq 0 ] && bound_logs
  # 1 s, cut short by ocbmd's SIGUSR1 when a host arrives (the trap above): wait returns on a trapped signal, and one
  # that came while this tick worked skips the wait.
  if [ "$woken" != 1 ]; then
    sleep 1 & _tick_sleep=$!
    # A USR1 between the test above and the fork set woken but found no wait to cut: cut the sleep now.
    [ "$woken" = 1 ] && kill "$_tick_sleep" 2>/dev/null
    wait "$_tick_sleep" 2>/dev/null || kill "$_tick_sleep" 2>/dev/null
  fi
done
