#!/bin/sh
########################
# radio_hal.sh - the chipset-neutral radio seam. Everything above this line (session_supervisor,
# the Rust daemons) asks for "wifi up" / "bt up" and never names a chip, a module, a firmware
# path or an attach helper.
#
# VERBS   probe | status | wifi_ap_on | wifi_ap_off | wifi_ap_prep | wifi_sta_up | setup_ap_on | setup_ap_off | bt_on |
#         bt_off | preload
# EXIT    0 converged now   1 ran and failed   2 already converged   3 unsupported on this unit
#
# wifi_ap_prep (16 Nov 2026) writes the AP's configuration ahead, with no host here and nothing raised: the files
# radio_ap_up.sh would write at the AP's bring-up, and a stamp that lets the next wifi_ap_on go straight to launching
# it. The supervisor runs it as it arms Bluetooth between host sessions (session_supervisor.sh, "Armed Bluetooth").
#
# ---------------------------------------------------------------------------------------------
# WHY THE VERB AND SCRIPT NAMES LOOK LIKE THIS. session_supervisor's teardown runs inside a
# detached `sh -c` whose own argv is visible to its own pkill patterns. That is a real bug this
# project already paid for once (2026-08-01: a plain `pkill -f` SIGKILLed the teardown subshell
# before it reached wlan_off, orphaning hostapd, which kept beaconing). So the name of this
# script and every verb MUST avoid the substrings the supervisor pattern-matches on:
# carplayd, btd, ocbmd, hostapd. Do not rename a verb without
# re-checking every pgrep/pkill pattern in session_supervisor.sh.
#
# THREE CONTRACT CLAUSES, each bought with a real failure:
#
#   1. CONVERGENT ON RETURN, NEVER FORK-AND-RETURN. When an `_on` verb returns 0, the radio is
#      up AND RESPONSIVE, and no work this verb started is still in flight. The vendor's own
#      dispatcher violates this (`/script/attach_bluetooth.sh &`, and the Broadcom branch
#      backgrounds brcm_patchram_plus) - that is precisely the uncoordinated double-bring-up
#      docs/wireless/01_BT_AND_RADIO.md recorded fighting itself for 7+ minutes on real hardware, because the caller
#      exec'd its advertiser while attach was still mid-retry. Callers rely on the return.
#
#   2. RESPONSIVENESS, NOT EXISTENCE. `hci0` can appear ~100ms after attach with firmware that
#      never loaded, and every later command then times out (docs/wireless/01_BT_AND_RADIO.md). Convergence is a real HCI
#      round-trip - read the local name back under a timeout - not a node existence test. The
#      vendor's check is `hciconfig hci0` existence, which a dead chip passes.
#
#   3. NEVER REBOOT, NEVER TOUCH SESSION DAEMONS. The vendor SD8987 branch can reboot the box
#      from inside a radio bring-up; that would bypass session_supervisor's escalation ladder
#      and its persistent reboot budget. A backend that believes only a reboot can recover says
#      so with an exit code and lets the layer that owns reboot policy decide. Likewise this
#      script never signals carplayd / btd / ocbmd: it cannot know
#      whether a live session is wired-owned, and the supervisor's conditional-reap logic
#      (docs/wireless/00_WIRELESS_CARPLAY.md #1.4) exists exactly to make that distinction.
#
# WHAT WE TAKE FROM CARLINKIT AND WHAT WE DO NOT. radio_detect.sh extracts this unit's own
# insmod/attach command lines from its own vendor dispatcher. Those lines are the vendor's
# OBSERVATIONS - fleet-deployed, working, and not re-derivable by us for parts we do not own -
# so we run them verbatim. Their surrounding control flow is their CHOICE, and it carries the
# defects above, so we supply our own. We never execute init_bluetooth_wifi.sh wholesale.
#
# THE AP LAYER IS ALWAYS OURS. Never delegate it. On a stripped box the vendor's
# start_bluetooth_wifi.sh reads config through riddleBoxCfg, which the OCBM base removes; with
# it gone that script seds an EMPTY wpa_passphrase into /etc/hostapd.conf - persistent, on
# flash - and falls back to WLANIP=192.168.50.2, which is NCM's own address, then repoints the
# DHCP pool onto the NCM subnet. Its teardown killalls every udhcpd, including NCM's. That is
# the control channel you are standing on.
########################
set -u
export PATH=${OCBM_TEST_PATH:+$OCBM_TEST_PATH:}/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH

VERB="${1:-}"
CAPS=/tmp/radio_caps
WLAN_STATE=/tmp/.radio_wlan_state
BT_STATE=/tmp/.radio_bt_state
BT_WARM=/tmp/.radio_bt_warm
DETECT=/script/radio_detect.sh
KEPT_CAPS=/etc/ocbm/kept_radio_caps        # the descriptor kept across boots (caps, "The descriptor, kept")
KO_STAMP=/tmp/.radio_ko_unpacked           # this unit's driver tarball is unpacked in /tmp (ko_unpack)
BOOT_MARKS=/tmp/radio_boot_marks           # the boot line's stamps so far (boot_mark, boot_line)
BOOT_LINE=/tmp/radio_boot                  # the boot line(s) written this boot, for a console
BOXLOG=/tmp/box.log
LOGP="[radio_hal]"

# Uptime (seconds since boot, /proc/uptime) on every line: the box clock is bogus until the host's
# CT_SETTIME, so cold-boot radio bring-up (firmware preload, hciattach, convergence) is otherwise
# untimeable in a drive log (Carlink3, 2026-10-20). Plain text after the prefix, so `grep` on the
# message words still matches. Read with `read`, not `cut`: a fork per line was about 15 ms each on the
# path a host's arrival waits on (16 Nov 2026).
log() {
  _log_up=""
  read -r _log_up _log_idle 2>/dev/null </proc/uptime
  echo "$LOGP [up ${_log_up}] $*"
}

# The uptime in whole ms (_um) and whole s (_us), no fork; both empty when unreadable. /proc/uptime has two decimals.
up_ms() {
  _um=""; _us=""; _um_r=""
  read -r _um_r _um_i 2>/dev/null </proc/uptime
  case "$_um_r" in *.*) _us=${_um_r%.*}; _um="${_um_r%.*}${_um_r#*.}0" ;; esac
}

# The boot line's stamps (R3-4, 20 Nov 2026): "<key> <value>" lines, the first of each key standing (boot_line).
# boot_mark stamps the uptime ms now, boot_note a value. One append each, no fork; a failed one is ignored.
boot_mark() { up_ms; [ -n "$_um" ] && echo "$1 $_um" 2>/dev/null >>"$BOOT_MARKS"; return 0; }
boot_note() { echo "$1 $2" 2>/dev/null >>"$BOOT_MARKS"; return 0; }

# A pid that still runs: kill -0 is the shell's own (a zombie counts, as /proc/<pid> did).
pid_live() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }

# An empty file written, or one truncated, without `:`: a special built-in, whose failed redirection ends a
# non-interactive POSIX shell (dash exits 2, which this script defines as "already converged"; BusyBox ash exits 1),
# so a full flash or a missing directory under `: > FILE` stopped a bring-up dead (the 21 Nov reviews, B). `true` is a
# regular built-in: its failure is only a status, and the braces keep the shell's message out of the logs. No fork.
# 0: written; 1: not, which the caller logs where it matters. Nothing here stops a bring-up.
write_empty() { { true >"$1"; } 2>/dev/null; }

# Single-flight per subsystem. mkdir is the portable atomic test-and-set on busybox (no flock
# dependency). A concurrent second bring-up must not run - the supervisor can manufacture
# overlapping edges (a respawn synthesizes a host-present edge), and stacking two insmod/rmmod
# sequences on one module is the driver-wedge class this guards against.
#
# A lock MUST be reclaimable. An earlier version had no staleness handling and no trap: a killed
# invocation (an ssh session timing out mid-bring-up is enough) left the directory behind and
# every later call refused to run, so wireless could never come up again until reboot. A lock is
# an optimisation against stacking, never a way to permanently disable a radio.
# RECLAIM ON LIVENESS, NOT ON AGE. The first version aged locks out after 2 minutes, which is
# SHORTER THAN A LEGITIMATE bt_on: against a present-but-wedged controller the convergence poll
# calls a `timeout 5` probe up to 30 times per attempt, across 4 attempts, so a genuine bring-up
# can hold the lock for many minutes. A concurrent caller would then declare the LIVE holder
# stale, delete its lock, and start a second attach against the same UART - which is exactly the
# OP_H5_SYNC collision the lock exists to prevent, manufactured by the lock itself.
#
# So the holder records its PID and reclaim requires the holder to be GONE. The age check stays
# only as a backstop for a PID that died and was recycled onto an unrelated process, and it is
# now generous enough that it can never fire against a working bring-up.
# THE WAIT, LOOKED AT EVERY 0.2 s (20 Nov 2026). A host that comes while the boot preload holds both locks waits here
# for each (its wifi_ap_on for wlan, its bt_on for bt), and a look every 0.5 s put up to half a second between the
# preload letting go and the host's bring-up going on. Between looks only the sleep forks (the shell's own test sees
# the lock go; the pid is read with `read`), so five looks a second cost no more than the two of before, which forked
# cat and mkdir each time. Still 30 s at most, and a lock is still reclaimed only when its holder is gone. Since 2.7.1
# every 0.05 s by default (round 4's page_trim, below).
lock_take() {  # $1 = subsystem
  _lk="/tmp/.radio_lock_$1"; _n=0
  # Round 4, item 6 (page_trim, on by default since 2.7.1): a look every 0.05 s while it waits (20 a round, 600 in all:
  # still 30 s at most), so a host's bt_on goes on within 50 ms of the boot preload letting the lock go, not 200. Off
  # (2.7.0's): the 0.2 s look. _lkt: the line's tag when a file turned it on.
  _lks=0.2; _lkr=5; _lkm=150; _lkt=""
  if r4_on page_trim; then _lks=0.05; _lkr=20; _lkm=600; _lkt=$_r4t; fi
  while ! mkdir "$_lk" 2>/dev/null; do
    _holder=""; read -r _holder 2>/dev/null <"$_lk/pid"
    if [ -n "$_holder" ] && ! pid_live "$_holder"; then
      log "lock $1 held by dead pid $_holder - reclaiming"
      rm -rf "$_lk"; continue
    fi
    if [ -z "$_holder" ] && [ -n "$(find "$_lk" -maxdepth 0 -mmin +20 2>/dev/null)" ]; then
      log "lock $1 has no holder and is >20min old - reclaiming"
      rm -rf "$_lk"; continue
    fi
    [ "$_n" -ge "$_lkm" ] && {
      log "lock $1 busy >30s (held by pid ${_holder:-unknown}) - refusing to stack bring-ups"; return 1; }
    # At least one sleep a round (a lock path that is no directory must not spin), up to five while it stays.
    _lkw=0
    while :; do sleep "$_lks"; _lkw=$((_lkw + 1)); [ "$_lkw" -lt "$_lkr" ] && [ -d "$_lk" ] || break; done
    _n=$((_n + _lkw))
  done
  [ "$_lks" = 0.05 ] && [ "$_n" -gt 0 ] && log "${_lkt}the $1 lock taken after about $((_n * 50)) ms of waiting, looked at every 0.05 s"
  echo $$ > "$_lk/pid" 2>/dev/null
  lock_mine "$1" 1   # taken by this process, whether or not its pid could be written (lock_drop_own)
  # TWO traps, deliberately. In ash a trapped signal runs the handler and then RESUMES the
  # script: a single combined trap would release the lock on SIGTERM while the bring-up carried
  # on running, so the next caller would take the lock and stack a second attach on top of a
  # still-live one. The signal trap must therefore also exit. Both stop a Bluetooth firmware loader in
  # flight before the lock goes (bt_fw_stop: it holds the UART); the signal trap ignores a second
  # signal meanwhile, so the stop ends before the lock does (2.7.1 review).
  trap 'bt_fw_stop; rm -rf "/tmp/.radio_lock_'"$1"'" 2>/dev/null' EXIT
  trap 'trap "" INT TERM HUP; bt_fw_stop; rm -rf "/tmp/.radio_lock_'"$1"'" 2>/dev/null; trap - EXIT; exit 143' INT TERM HUP
  return 0
}
lock_drop() { trap - EXIT INT TERM HUP; rm -rf "/tmp/.radio_lock_$1" 2>/dev/null; lock_mine "$1" 0; }
# Which locks this process took and still holds (lock_take sets it, the drops clear it): _lkm_wlan, _lkm_bt.
lock_mine() { case "$1" in wlan) _lkm_wlan=$2 ;; bt) _lkm_bt=$2 ;; esac; }
lock_is_mine() { case "$1" in wlan) [ "${_lkm_wlan:-0}" = 1 ] ;; bt) [ "${_lkm_bt:-0}" = 1 ] ;; *) return 1 ;; esac; }
# Drop $1's lock only while this process holds it: its pid in it, or (its pid write failed, 21 Nov review K) no pid in
# it at all and this process took it and has not let it go. The boot preload lets its locks go before it ends
# (do_preload, bt_attach_together, bt_handover_give), and the lock by then may be a host's bring-up's: never remove
# that one, even one whose own pid write failed.
lock_drop_own() {
  _ldo=""; read -r _ldo 2>/dev/null <"/tmp/.radio_lock_$1/pid"
  if [ "$_ldo" = "$$" ] || { [ -z "$_ldo" ] && lock_is_mine "$1"; }; then rm -rf "/tmp/.radio_lock_$1" 2>/dev/null; fi
  lock_mine "$1" 0
  return 0
}

# Atomic so a reader can never see a half-written descriptor. Consumers (the Rust daemons) must
# treat a missing or malformed file as "absent" and fall back to their compiled defaults - this
# file is an override channel, never a hard dependency.
publish() {  # $1 = state file, rest = KEY=VALUE...
  _f=$1; shift
  { for kv in "$@"; do echo "$kv"; done; } > "$_f.new" 2>/dev/null && mv "$_f.new" "$_f"
}

# =============================================================================================
# THE DESCRIPTOR, KEPT ACROSS BOOTS (20 Nov 2026)
# =============================================================================================
# radio_detect.sh's answer changes only when what it reads changes: the SDIO ids, the firmware trees and the driver
# tarball, the helpers and bring-up scripts present, the vendor's dispatcher and attach script, and its own text. /tmp
# is empty at every power-up, so every boot ran it again: 0.75 to 0.8 s on the IW416, on the boot preload's path.
#
# KEPT_CAPS holds the last descriptor a detection gave that found wireless, its post-load facts (the interfaces and
# addresses of the boot it ran in) and its time left out. A boot reuses it while radio_detect.sh's key (its
# radio_detect_key: each of those inputs, the files by md5) reads what the copy was made under; otherwise the boot
# detects as before and keeps the result.
#   WHERE: /etc/ocbm, OCBM's own state directory on the jffs2 root. The one-file installer makes it, and only a unit it
#   installed has one (backend=mapped): elsewhere nothing is kept and every boot detects, as before. Updates keep it
#   (installed_version and the journal live there) and the key notices a new radio_detect.sh; a revert to stock keeps it
#   for a retry, whose boot the key checks again; the full way back to stock removes it, whole or as an extra of /etc,
#   since its name is not one is_radio() keeps. Not /script, where the preload's flags are: is_radio() protects every
#   radio_* file there from deletion, so each needs naming in every remover (the Mac's 13 Nov), and the vendor's own
#   scripts beside them change with a reflash.
#   WRITTEN only after a detection and only where it differs from the copy there (once per firmware or radio_detect.sh
#   version), as a file of its own beside it, synced, renamed over it, synced: a power cut leaves the old copy or the
#   new, and a stray candidate at worst.
#   READ BACK only as caps_check reads it: KEY=VALUE lines that nothing runs when sourced, the end mark last (a copy cut
#   short never ends with it), wireless, a backend and an SDIO id, and on the mapped backend an insmod and an attach
#   mapping. Anything else, or another key, is detected.
# The check costs one awk over the copy, the key of radio_detect.sh (one grep, one md5sum, whose read of the tarball
# the unpack right after finds cached) and one rename, against the dozens of forks of a detection.
caps_key_now() { ( RADIO_DETECT_FUNCS=1; . "$DETECT" && radio_detect_key ) 2>/dev/null; }

caps_check() {  # $1 a descriptor, $2 where its lines go ("" none): prints its key when it is one a boot may reuse
  awk -v out="$2" '
    /^RADIO_KEPT_END=1$/ { end = NR; next }
    {
      if ($0 !~ /^RADIO_[A-Z0-9_]+=/) { bad = 1; next }
      k = $0; sub(/=.*/, "", k); val = $0; sub(/^[^=]*=/, "", val)
      if (val !~ /^"[^"$`\\]*"$/ && val !~ /^[A-Za-z0-9_.\/:,+-]*$/) { bad = 1; next }
      s[k] = val
      if (out != "") print >> out
    }
    END {
      key = s["RADIO_DETECT_KEY"]; gsub(/"/, "", key)
      if (bad || end != NR || key == "" || key == "unkeepable") exit 1
      if (s["RADIO_WIRELESS"] != "yes" || s["RADIO_SDIO_DEVICE"] == "") exit 1
      b = s["RADIO_BACKEND"]
      if (b != "mapped" && b != "owned") exit 1
      if (b == "mapped" && (s["RADIO_WLAN_INSMOD"] == "\"\"" || s["RADIO_WLAN_INSMOD"] == "")) exit 1
      if (b == "mapped" && (s["RADIO_BT_ATTACH_CMD"] == "\"\"" || s["RADIO_BT_ATTACH_CMD"] == "")) exit 1
      print key
    }' "$1" 2>/dev/null
}

# This boot's descriptor from the kept copy, when it may be: 0 with /tmp's in place, 1 to detect.
caps_from_kept() {
  [ -s "$KEPT_CAPS" ] || return 1
  _ck_new="$CAPS.kept.$$"
  # Where it came from and when first (btd logs the descriptor as one line, clipped at 1024 bytes), then the copy's lines.
  up_ms
  { echo "RADIO_CAPS_FROM=kept"; echo "RADIO_DETECTED_AT=$_us"; } 2>/dev/null >"$_ck_new" || { rm -f "$_ck_new"; return 1; }
  if ! _ck_key=$(caps_check "$KEPT_CAPS" "$_ck_new") || [ -z "$_ck_key" ]; then
    rm -f "$_ck_new"
    log "descriptor: the kept copy ($KEPT_CAPS) is not one a boot may reuse (cut short, or a line it cannot carry) - detecting"
    return 1
  fi
  _ck_now=$(caps_key_now)
  if [ "$_ck_now" != "$_ck_key" ]; then
    rm -f "$_ck_new"
    log "descriptor: the kept copy was made under another key (this unit's firmware, its vendor scripts or radio_detect.sh changed) - detecting"
    return 1
  fi
  mv -f "$_ck_new" "$CAPS" 2>/dev/null || { rm -f "$_ck_new"; return 1; }
  log "descriptor: the kept copy ($KEPT_CAPS), its key unchanged"
  return 0
}

# After a detection: keep what it found, where it differs from the copy there.
caps_keep() {
  [ -d "${KEPT_CAPS%/*}" ] || return 0
  _kp_new="$KEPT_CAPS.new.$$"
  awk '/^RADIO_(WLAN_IF|WLAN_IFS|WLAN_MAC|HCI_DEV|BT_MAC|CAPS_FROM|DETECTED_AT)=/ { next } { print }
       END { print "RADIO_KEPT_END=1" }' "$CAPS" 2>/dev/null >"$_kp_new" || { rm -f "$_kp_new"; return 0; }
  if [ -z "$(caps_check "$_kp_new" "")" ]; then
    rm -f "$_kp_new"; log "descriptor: this one is not kept (no wireless, no mapping, or no key it can carry)"; return 0
  fi
  if cmp -s "$_kp_new" "$KEPT_CAPS" 2>/dev/null; then rm -f "$_kp_new"; return 0; fi
  sync
  if mv -f "$_kp_new" "$KEPT_CAPS" 2>/dev/null; then
    # Strays a power cut left, never the candidate of a writer still running (wifi_ap_on and bt_on detect side by side).
    for _kp_s in "$KEPT_CAPS".new.*; do [ -e "$_kp_s" ] && ! pid_live "${_kp_s##*.}" && rm -f "$_kp_s"; done
    sync
    log "descriptor: kept on flash ($KEPT_CAPS) for the next boots"
  else
    rm -f "$_kp_new"; log "descriptor: could not keep it on flash ($KEPT_CAPS)"
  fi
  return 0
}

# The descriptor at $CAPS is final for this boot: it found wireless and names an SDIO device. One made before the
# radio's SDIO card was enumerated names none (the boot preload now looks at about 2.0 s of uptime), and its insmod and
# attach lines are then not this unit's own: never final, so the next verb detects again (the 21 Nov review D; caps_check
# never keeps one on flash either). No fork.
caps_final() {
  _cfw=0; _cfs=0
  while IFS= read -r _cfl; do
    case "$_cfl" in RADIO_WIRELESS=yes) _cfw=1 ;; RADIO_SDIO_DEVICE=?*) _cfs=1 ;; esac
  done 2>/dev/null <"$CAPS"
  [ "$_cfw" = 1 ] && [ "$_cfs" = 1 ]
}

caps() {
  # Once per boot: /tmp starts empty at every power-up, and what this script reads from the descriptor (the chip, its
  # driver and attach lines) cannot change within one. Detection costs 1.0 s alone and 1.6 s for two at once (the
  # Mac's 13 Nov bench), so a boot first takes the kept copy (caps_from_kept), and detects only when it may not. A
  # descriptor that found no wireless, or no SDIO device (caps_final), is taken again. Its post-load facts (interface,
  # MACs) go stale; only btd's start-up log reads them, and this script enumerates its own (wlan_iface, hci_dev).
  if ! caps_final; then
    up_ms; _caps_t0=$_um
    if caps_from_kept; then
      boot_note caps kept
    elif [ -x "$DETECT" ]; then
      sh "$DETECT" -q >/dev/null 2>&1
      boot_note caps detected
      caps_keep
    fi
    up_ms; [ -n "$_caps_t0" ] && [ -n "$_um" ] && boot_note caps_ms $((_um - _caps_t0))
  fi
  [ -f "$CAPS" ] || { log "no descriptor at $CAPS and radio_detect.sh is missing"; return 1; }

  # A MALFORMED DESCRIPTOR MUST NEVER ABORT THIS SCRIPT. Measured: an extracted vendor line
  # carrying an unexpanded shell variable - e.g. the NXP branch's
  #     insmod /tmp/moal.ko "mod_para=$nxpWiFiConfig"
  # - makes `.` fail under `set -u` with status **2** in ash/dash, and 2 is this script's
  # "already converged" code. The verb would exit before the backend switch, reporting SUCCESS,
  # and the caller would start its advertiser over radios that were never brought up. That is
  # the silent-failure class this whole seam exists to eliminate, so it is not acceptable to
  # merely "usually" avoid it: source defensively and then require what we depend on.
  set +u
  . "$CAPS" 2>/dev/null
  set -u
  # Every key this script consumes gets a defined default, so a descriptor written by an older
  # or partially-failed detect can degrade to "unsupported" instead of killing the shell.
  : "${RADIO_BACKEND:=none}"   "${RADIO_WIRELESS:=no}"      "${RADIO_CHIP:=unknown}"
  : "${RADIO_SDIO_DEVICE:=}"   "${RADIO_SDIO_VENDOR:=}"     "${RADIO_FW_TREE:=}"
  : "${RADIO_KO_TARBALL:=}"    "${RADIO_WLAN_MODULES:=}"    "${RADIO_WLAN_INSMOD:=}"
  : "${RADIO_BT_LDISC_KO:=}"   "${RADIO_BT_ATTACH_CMD:=}"   "${RADIO_BT_ATTACH:=}"
  : "${RADIO_BT_PRELOAD:=}"    "${RADIO_BT_PRELOAD_CMD:=}"  "${RADIO_BT_AFTER_WLAN:=0}"
  : "${RADIO_BT_SCO_MTU_CMD:=}"  "${RADIO_BT_SCO_ROUTE_CMD:=}"
  : "${RADIO_BT_UART:=/dev/ttymxc2}" "${RADIO_BT_TOGETHER:=0}" "${RADIO_CAPS_FROM:=}"
  return 0
}

# This unit's driver tarball unpacked into /tmp once a boot, by whichever bring-up needs it first, and never twice at
# once (20 Nov 2026). Each caller used to test for one module file and untar when it was missing, so two side by side
# (wifi_ap_on and bt_on at a host's arrival; the Wi-Fi driver and the Bluetooth firmware loaded together) could find a
# module tar was still writing and insmod half a file. Its own small lock and a stamp once the tarball is out; a
# caller that finds the lock waits for the stamp (20 s at most, then unpacks itself rather than stall a bring-up).
ko_unpack() {
  if [ -e "$KO_STAMP" ]; then
    _kuok=1
    for _kum in $RADIO_WLAN_MODULES $RADIO_BT_LDISC_KO; do [ -e "/tmp/$_kum" ] || _kuok=0; done
    [ "$_kuok" = 1 ] && return 0
    rm -f "$KO_STAMP"   # a module gone since: unpacked again
  fi
  [ -n "$RADIO_KO_TARBALL" ] || return 1
  _kul=/tmp/.radio_lock_ko; _kun=0; _kuown=1
  while ! mkdir "$_kul" 2>/dev/null; do
    _kuh=""; read -r _kuh 2>/dev/null <"$_kul/pid"
    if [ -n "$_kuh" ] && ! pid_live "$_kuh"; then rm -rf "$_kul"; continue; fi
    _kun=$((_kun + 1)); [ "$_kun" -ge 100 ] && { log "driver unpack lock busy >20 s - unpacking anyway"; _kuown=0; break; }
    sleep 0.2
    [ -e "$KO_STAMP" ] && [ ! -d "$_kul" ] && return 0
  done
  [ "$_kuown" = 1 ] && echo $$ > "$_kul/pid" 2>/dev/null
  if [ ! -e "$KO_STAMP" ]; then
    { tar -xzf "$RADIO_KO_TARBALL" -C /tmp 2>/dev/null || tar -xf "$RADIO_KO_TARBALL" -C /tmp 2>/dev/null; } \
      && write_empty "$KO_STAMP"
  fi
  [ "$_kuown" = 1 ] && rm -rf "$_kul" 2>/dev/null
  [ -e "$KO_STAMP" ]
}

# Enumerate the wireless interface rather than assuming a name. The name is an insmod PARAMETER
# that differs per chip (Realtek if2name=sta0, Broadcom iface_name=sta), and on Broadcom wlan0
# does not exist until something explicitly creates it on top of sta0. A cfg80211/wext netdev is
# identifiable without knowing the driver: it carries wireless/ or phy80211/ in sysfs.
#
# NOTE THE VARIABLE NAMES. These helpers are called from inside caller loops, and sh has no
# function scope, so a bare or short loop variable here silently overwrites the caller's. That is
# not theoretical: wlan_iface() once used `_n`, the BT wait loop used `_n` as its counter, and
# calling the helper replaced the counter with a sysfs path — `_n=$((_n+1))` then failed with an
# arithmetic syntax error. It only ever reached that line when NO interface existed (otherwise
# the caller's `&& break` fired first). Keep these names distinctive.
wlan_iface() {
  for _wif_d in /sys/class/net/*/; do
    [ -e "$_wif_d/wireless" ] || [ -e "$_wif_d/phy80211" ] || continue
    basename "$_wif_d"; return 0
  done
  return 1
}

# ---- the Wi-Fi link's station (24 Nov 2026; /script/wifi_sta.sh, ocbmd src/wifi.rs) ----------------------------------
# With a hotspot in /etc/ocbm/wifi_link (an ssid_hex= line) the adapter also joins the phone's hotspot as a station. One
# radio, and two interfaces from one driver load: on the IW416 its stock wifi_mod_para.conf (drv_mode=3) names the STA
# BSS sta0 (sta_name=sta) and the uAP BSS wlan0 (uap_name=wlan), and stock's own STA mode (when a supplicant exists)
# runs its station on sta0. So in STA mode the station keeps the interface named sta* and the AP takes the OTHER
# wireless interface: each BSS in its own role. Without the line nothing here changes: the AP takes the first
# interface, as before (sta0 on the IW416, by name order). A unit whose driver makes one interface only has no station
# beside the AP: the AP keeps it, and the keeper says so. RADIO_STA_IF names the station's interface outright.
WIFI_LINK_FILE=/etc/ocbm/wifi_link
# The shell's own test first: an adapter with no link forks nothing more than 2.7.1 on any path that asks.
sta_mode() { [ -f "$WIFI_LINK_FILE" ] && grep -q '^ssid_hex=' "$WIFI_LINK_FILE" 2>/dev/null; }
sta_iface() {
  if [ -n "${RADIO_STA_IF:-}" ]; then [ -e "/sys/class/net/$RADIO_STA_IF" ] && { echo "$RADIO_STA_IF"; return 0; }; return 1; fi
  _sti_s=""; _sti_o=""
  for _sti_d in /sys/class/net/*/; do
    [ -e "$_sti_d/wireless" ] || [ -e "$_sti_d/phy80211" ] || continue
    _sti_n=$(basename "$_sti_d")
    case "$_sti_n" in sta*) [ -z "$_sti_s" ] && _sti_s=$_sti_n ;; *) _sti_o=$_sti_n ;; esac
  done
  [ -n "$_sti_s" ] && [ -n "$_sti_o" ] && { echo "$_sti_s"; return 0; }
  return 1
}
# STA mode, the driver's first interface there: is a station's beside it? A driver's interfaces come from one probe, so a
# second is there within 1 s of the first; none by then is a one-interface driver, marked for the boot (ONE_IF), so no
# path waits for a station again and the keeper stops asking. 1: none.
ONE_IF=/tmp/.radio_one_iface
sta_settle() {
  [ -e "$ONE_IF" ] && return 1
  _ss=0; while [ "$_ss" -lt 10 ]; do sta_iface >/dev/null 2>&1 && return 0; _ss=$((_ss+1)); sleep 0.1; done
  echo 1 > "$ONE_IF"; log "one wireless interface on this driver: no station beside the AP, so no Wi-Fi link here"; return 1
}
# The AP's interface: in STA mode the wireless interface that is not the station's (when there is one), else wlan_iface.
ap_iface() {
  sta_mode || { wlan_iface; return; }
  _apf_s=$(sta_iface) || { wlan_iface; return; }
  for _apf_d in /sys/class/net/*/; do
    [ -e "$_apf_d/wireless" ] || [ -e "$_apf_d/phy80211" ] || continue
    _apf_n=$(basename "$_apf_d")
    [ "$_apf_n" = "$_apf_s" ] && continue
    echo "$_apf_n"; return 0
  done
  wlan_iface
}

hci_dev() {
  for _hcid_d in /sys/class/bluetooth/hci*/; do
    [ -d "$_hcid_d" ] && { basename "${_hcid_d%/}"; return 0; }
  done
  return 1
}

# Clause 2: a real HCI round-trip under a timeout. `hciconfig <dev>` merely existing proves
# nothing - the whole point. Single-shot: used inside the convergence poll, where a fast
# negative is what we want because we are about to poll again anyway.
bt_responsive() {  # $1 = hci dev
  timeout 5 hciconfig "$1" name 2>/dev/null | grep -q "Name:"
}
# Kernel flags only (HCIGETDEVINFO, what `hciconfig <dev>` prints): never reaches the chip, so it
# answers instantly even against a wedged controller. Says nothing about responsiveness - that is
# still bt_responsive's job - only whether a name read can possibly succeed.
bt_powered_up() {  # $1 = hci dev
  timeout 5 hciconfig "$1" 2>/dev/null | grep -q "UP RUNNING"
}
# `hciconfig <dev> up|down` at SCHED_FIFO 1 where chrt exists. Both wait in the kernel's
# __hci_req_sync, which on the CCPA's single-core 3.14 kernel can lose its wake-up and sleep the
# full 10 s timeout; a real-time caller is always waiting before the HCI workqueue can even send
# the command (btd's bt_bringup::run_rt has the measurements).
hci_rt() {
  if command -v chrt >/dev/null 2>&1; then chrt -f 1 hciconfig "$@"; else hciconfig "$@"; fi
}

# Confirmed variant - used for DECISIONS, never inside a poll loop.
#
# MEASURED on an RTL8822CS: a healthy, attached controller intermittently times out a name read
# - about one attempt in four, with the next attempts a second later succeeding. So the probe is
# not a clean oracle, and the two questions we ask it deserve DIFFERENT bars:
#
#   "did my attach succeed?"        -> strict. One real round-trip, poll until it answers.
#   "is this radio already up?"     -> reluctant. A false negative here does not merely misreport;
#                                      it sends us into rmmod + GPIO reset + re-attach against a
#                                      working chip, which during a live session drops the phone.
#
# Hence: be strict to declare success, reluctant to destroy. Five spaced attempts cost up to a
# few tens of seconds in the rare all-miss case, and only on the decision path.
bt_responsive_confirmed() {  # $1 = hci dev
  _t=0
  while [ "$_t" -lt 5 ]; do
    bt_responsive "$1" && return 0
    _t=$((_t+1)); [ "$_t" -lt 5 ] && sleep 2
  done
  return 1
}

ap_running() { pgrep -x hostapd >/dev/null 2>&1 || ps | grep -v grep | grep -qw hostapd; }

# THE BOX IDENTITY. Wi-Fi and Bluetooth MUST advertise the same name, and the only way to
# guarantee that is to decide it ONCE and persist it.
#
# Deriving it per-radio does not work, and the failure is not hypothetical. The natural sources
# disagree: on the validated unit the WLAN MAC and the BT controller address share no octets at
# all (Realtek sets no bd_addr, so BT uses its own chip efuse). So which name a radio gets would
# depend on which radio happened to be up when it was asked. Worse, the answer could change
# between boots, and the phone stores the name in its bonded record.
#
# The name is the one the iPhone lists on Bluetooth: btd's, iap2-core's accessory_name,
# CarLink-<last 4 hex of wlan0's MAC>, else of the serial. The AP takes it too (Scott, 12 Nov: the
# dev adapter's Wi-Fi said ccpa-fe01, from the AP interface sta0's MAC, beside Bluetooth's
# CarLink-fd6c). Before the radios are up, wlan0's MAC comes from the one-file installer's record
# (/etc/ocbm/addresses.txt), as ocbmd's name does.
#
# So: the first derivation from wlan0's MAC wins and is written to /etc/carplay_ident; everything
# after reads that file. One small flash write, once per box. A name an older OCBM persisted
# (ccpa-<4 hex>) is derived again; the serial's fallback is not persisted, so a later call with
# wlan0's MAC known decides.
IDENT_FILE=/etc/carplay_ident
BOX_BRAND=CarLink
# The last 4 hex digits of stdin, lowercase (iap2-core's suffix_from). Empty with fewer than 4.
hex_suffix() { tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed -n 's/.*\(....\)$/\1/p'; }
box_name() {  # $1 = hci dev, unused (kept for callers): the name never comes from the BT address
  if [ -s "$IDENT_FILE" ]; then
    _id=$(head -1 "$IDENT_FILE")
    case "$_id" in "$BOX_BRAND"-*) echo "$_id"; return 0 ;; esac
  fi
  _sfx=$(cat /sys/class/net/wlan0/address 2>/dev/null | hex_suffix)
  [ -n "$_sfx" ] || _sfx=$(sed -n 's/^wlan0=//p' /etc/ocbm/addresses.txt 2>/dev/null | head -1 | hex_suffix)
  if [ -n "$_sfx" ]; then
    echo "$BOX_BRAND-$_sfx" > "$IDENT_FILE" 2>/dev/null
    echo "$BOX_BRAND-$_sfx"
    return 0
  fi
  _sfx=$(cat /etc/serial_number 2>/dev/null | hex_suffix)
  if [ -n "$_sfx" ]; then echo "$BOX_BRAND-$_sfx"; else echo "$BOX_BRAND"; fi
}

# Name the controller. The owned bt_on.sh has always done this; the mapped path did not, which
# left a non-IW416 unit advertising its chip default (an RTL8822CS came up as "RTK_BT_4.2") while
# its SoftAP was correctly named ccpa-<4hex> — one box, two identities.
#
# Two mechanisms, both needed: /etc/.custom_bluetooth_name is the flag bluetoothDaemon honours
# when it applies a custom name (without it the daemon BLANKS the adapter name during init), and
# the direct hciconfig set covers the case where the daemon is not managing us. The retry exists
# because the daemon can blank the name AFTER our first set — assert until it sticks.
bt_set_name() {  # $1 = hci dev
  _bn=$(box_name "$1")
  echo "$_bn" > /etc/.custom_bluetooth_name 2>/dev/null
  echo "$_bn" > /etc/bluetooth_name 2>/dev/null
  _n=0
  # Read back at once and wait only before a retry: with no stock daemon to blank it (OCBM mode) the first set sticks,
  # and the 0.5 s before every read-back was paid on each fresh attach (the Mac's 13 Nov brief, item 6).
  while [ "$_n" -lt 10 ]; do
    hciconfig "$1" name "$_bn" 2>/dev/null
    _cur=$(timeout 5 hciconfig "$1" name 2>/dev/null | sed -n "s/.*Name: '\(.*\)'.*/\1/p")
    [ "$_cur" = "$_bn" ] && break
    _n=$((_n+1))
    sleep 0.5
  done
  log "BT name=$_bn (asserted after $_n retries, now '${_cur:-unknown}')"
}

# The two name files bt_set_name writes, each only where it would change: round 4's page trim sets no name on the
# controller, and a boot whose name they hold already writes nothing to flash.
bt_name_files() {
  _bnf=$(box_name)
  for _bnf_f in /etc/.custom_bluetooth_name /etc/bluetooth_name; do
    _bnf_c=""; read -r _bnf_c 2>/dev/null <"$_bnf_f"
    [ "$_bnf_c" = "$_bnf" ] || echo "$_bnf" > "$_bnf_f" 2>/dev/null
  done
  return 0
}

# Load THIS unit's WLAN driver and nothing else - no AP, no addressing. Factored out of
# wifi_ap_on because bt_on needs it too: on the parts whose vendor script makes BT attach wait
# for the WLAN interface, a bt_on with no wifi_ap_on before it (preload, a direct call) would wait
# for something nobody creates. See do_bt_on.
# Returns 0 if the driver is loaded (or was already), 1 if there is nothing to load.
wlan_driver_up() {
  [ -n "$RADIO_WLAN_INSMOD" ] || { log "no WLAN insmod mapping for this unit"; return 1; }
  # wifi_ap_on and bt_on run side by side at host_present, and both may load the driver: one at a time, so they
  # never both insmod a module (or untar the modules) at once. Its own small lock, never the wlan or bt one, and
  # reclaimed when its holder is gone; after 30 s it goes ahead unlocked rather than stall a bring-up.
  _wdl=/tmp/.radio_lock_wlandrv; _wdn=0; _wdown=1
  while ! mkdir "$_wdl" 2>/dev/null; do
    _wdh=""; read -r _wdh 2>/dev/null <"$_wdl/pid"
    if [ -n "$_wdh" ] && ! pid_live "$_wdh"; then rm -rf "$_wdl"; continue; fi
    _wdn=$((_wdn+1)); [ "$_wdn" -ge 150 ] && { log "wlan driver lock busy >30 s - loading anyway"; _wdown=0; break; }
    sleep 0.2
  done
  [ "$_wdown" = 1 ] && echo $$ > "$_wdl/pid" 2>/dev/null
  wlan_driver_load; _wdr=$?
  [ "$_wdown" = 1 ] && rm -rf "$_wdl" 2>/dev/null
  return $_wdr
}

wlan_driver_load() {
  ko_unpack || log "driver tarball ${RADIO_KO_TARBALL:-<none>} did not unpack"
  log "wlan driver: $RADIO_WLAN_INSMOD"
  _wdl_loaded=0   # a module inserted here: the caller that waits for the interface stamps wlan_up
  _old=$IFS; IFS=';'
  for _cmd in $RADIO_WLAN_INSMOD; do
    [ -n "$_cmd" ] || continue
    IFS=$_old
    # Idempotent: a module already in /proc/modules is success, not the "File exists" error
    # insmod would print. Both verbs must be safe to call repeatedly - the supervisor can invoke
    # them on any host-presence edge, and a driver already loaded is the common case.
    _mod=$(echo "$_cmd" | sed 's|.*/tmp/||; s/ .*//; s/\.ko$//')
    if [ -n "$_mod" ] && grep -q "^$_mod " /proc/modules 2>/dev/null; then
      log "  $_mod already loaded"
    else
      boot_mark wlan_start   # the boot line: the first module this boot loads (the first stamp stands)
      _wdl_loaded=1
      $_cmd 2>&1 | sed "s|^|$LOGP   |"
    fi
    IFS=';'
  done
  IFS=$_old
  # set_wifi_mac is the vendor's per-unit MAC applier and is kept by the base install's radio
  # guard on every variant. Harmless where absent.
  command -v set_wifi_mac >/dev/null 2>&1 && set_wifi_mac >/dev/null 2>&1
  return 0
}

# ---------------------------------------------------------------------------------------------
# SCO. Apply THIS UNIT'S OWN post-attach SCO setup at bt_on, as the vendor's attach script does.
# Nothing on the box carries audio over SCO (CarPlay's calls ride the AirPlay session), so this only
# keeps bt_on doing what the stock attach does; btd's DOWN->UP cycle later discards it, harmlessly.
#
# The commands are the VENDOR'S, extracted per-branch by radio_detect.sh -- never composed here.
# On this unit's Realtek branch the vendor sets ONLY the SCO MTU and no routing command at all
# (Realtek routes SCO over HCI by default); on NXP and BCM4358 there is also a vendor-opaque
# routing command. Emitting one chip's routing command on another chip is precisely the failure
# the no-chipset-whitelist rule exists to prevent, so a branch we cannot resolve yields NO mapping
# and this verb reports `unsupported` (3) rather than guessing.
#
# The vendor writes `hci0` literally. Retarget it at the device we actually enumerated rather than
# firing a command at a node that may not exist -- the ONLY edit made to a captured line, and it is
# logged when it happens.
apply_sco() {  # $1 = hci dev; echoes what it did. 0 = applied, 3 = nothing to apply
  _sd=$1; _did=0
  for _sc in sco_mtu sco_route; do
    case "$_sc" in
      sco_mtu)   _cmd=$RADIO_BT_SCO_MTU_CMD ;;
      sco_route) _cmd=$RADIO_BT_SCO_ROUTE_CMD ;;
    esac
    [ -n "$_cmd" ] || continue
    case "$_cmd" in
      *" hci0"*|*" hci0")
        [ "$_sd" = hci0 ] || { log "retargeting the vendor $_sc line from hci0 to $_sd"
                               _cmd=$(echo "$_cmd" | sed "s/ hci0/ $_sd/g"); } ;;
    esac
    log "$_sc: $_cmd"
    $_cmd >/dev/null 2>&1 || log "  $_sc returned non-zero (non-fatal)"
    _did=1
  done
  [ "$_did" = 1 ] || return 3
  return 0
}

# =============================================================================================
# WLAN
# =============================================================================================
# The AP's configuration written ahead (wifi_ap_prep; radio_ap_up.sh PREP leaves this stamp) for an interface that is
# still there. Cheap on purpose, no fork: radio_ap_up.sh checks the rest (nothing it read changed since) before it
# trusts it. Sets _api, the interface.
AP_PREPARED=/tmp/.radio_ap_prepared
ap_prepared_iface() {
  _api=""
  read -r _api _api_rest 2>/dev/null <"$AP_PREPARED"
  [ -n "$_api" ] || return 1
  [ -e "/sys/class/net/$_api/wireless" ] || [ -e "/sys/class/net/$_api/phy80211" ]
}

# Write the AP's configuration ahead, raising nothing: no address, no hostapd, no udhcpd. The armed state between host
# sessions runs it, so the host's arrival launches the AP at once (session_supervisor.sh, "Armed Bluetooth"). Only for
# the owned AP layer (backend=mapped), and only with the WLAN driver's interface there: the boot preload loads it on
# a part whose Bluetooth needs it, and a session leaves it loaded. Without either, the AP is configured at its
# bring-up, as before.
do_wifi_ap_prep() {
  caps || { log "AP prep: no descriptor - nothing prepared"; return 3; }
  [ "$RADIO_BACKEND" = mapped ] || {
    log "AP prep: backend=$RADIO_BACKEND brings its AP up with its own scripts - nothing prepared"; return 3; }
  _i=$(ap_iface 2>/dev/null) || {
    rm -f "$AP_PREPARED"; log "AP prep: no WLAN interface - the AP is configured at its bring-up"; return 3; }
  [ -x /script/radio_ap_up.sh ] || { log "AP prep: the owned AP layer (/script/radio_ap_up.sh) is not installed"; return 1; }
  if ap_running; then
    log "AP prep: an AP is running on $_i - its configuration is left as it is"; return 2
  fi
  RADIO_WLAN_IF="$_i" RADIO_BOX_NAME="$(box_name)" sh /script/radio_ap_up.sh PREP 2>&1 | sed "s/^/$LOGP   /"
  if ap_prepared_iface && [ "$_api" = "$_i" ]; then
    log "AP configured ahead on $_i (nothing raised: no address, no AP, no DHCP)"
    return 0
  fi
  log "AP prep did not finish on $_i - the AP is configured at its bring-up"
  return 1
}

do_wifi_ap_on() {
  # M3: the CarPlay AP wins over a set-up network still up (ocbmd ends the window at a host's first frame anyway).
  [ -e "$SETUP_MARK" ] && { log "a set-up network is up: down first for the CarPlay AP"; do_setup_ap_off; }
  # FAST PATH (16 Nov 2026). The configuration was written ahead (wifi_ap_prep) and the WLAN driver is loaded (its
  # interface is there): straight to the owned AP layer, whose first launch is then hostapd. No descriptor read, no
  # driver step, no configuration writes: about 0.9 s of every connect went there before hostapd started.
  if ap_prepared_iface && [ -x /script/radio_ap_up.sh ] && { ! sta_mode || [ "$_api" = "$(ap_iface 2>/dev/null)" ]; }; then
    log "AP configured ahead on $_api - launching it"
    ONDEMAND_WIFI=1 RADIO_WLAN_IF="$_api" sh /script/radio_ap_up.sh AP 2>&1 | sed "s/^/$LOGP   /"
    _n=0; while [ "$_n" -lt 50 ]; do ap_running && break; _n=$((_n+1)); sleep 0.2; done
    if ap_running; then
      # The address the AP layer configured, from its stamp (rewritten if it configured afresh), and the MAC, both
      # with `read`: this lock is what btd's 0x5703 reply waits on.
      _api_ip=""; _api_mac=""
      read -r _api _api_name _api_ip _api_rest 2>/dev/null <"$AP_PREPARED"
      read -r _api_mac 2>/dev/null <"/sys/class/net/$_api/address"
      publish "$WLAN_STATE" "result=ok" "backend=mapped" "ap_iface=$_api" "ap_ip=$_api_ip" "wlan_mac=$_api_mac"
      log "AP converged on $_api"
      return 0
    fi
    publish "$WLAN_STATE" "result=fail" "backend=mapped" "ap_iface=$_api" "detail=ap-did-not-start"
    log "AP did not start on $_api"
    return 1
  fi
  caps || { publish "$WLAN_STATE" "result=unsupported" "detail=no-descriptor"; return 3; }

  if ap_running && wlan_iface >/dev/null 2>&1; then
    _i=$(ap_iface)
    log "already converged (AP running on $_i)"
    publish "$WLAN_STATE" "result=ok" "backend=$RADIO_BACKEND" "chipset=$RADIO_SDIO_DEVICE" \
            "ap_iface=$_i" "ap_ip=$(ap_ip_of "$_i")" "wlan_mac=$(cat /sys/class/net/$_i/address 2>/dev/null)"
    return 2
  fi

  case "$RADIO_BACKEND" in
    owned)
      # The IW416 baseline's own scripts. exec keeps the process tree, signals and exit code
      # identical to calling wlan_on.sh directly, so this path is a no-op refactor for that unit.
      log "backend=owned -> /script/wlan_on.sh"
      sh /script/wlan_on.sh; _rc=$?
      _i=$(wlan_iface 2>/dev/null) || _i=""
      [ "$_rc" = 0 ] && ap_running \
        && publish "$WLAN_STATE" "result=ok" "backend=owned" "ap_iface=${_i:-wlan0}" \
                   "ap_ip=$(ap_ip_of "${_i:-wlan0}")" "wlan_mac=$(cat /sys/class/net/${_i:-wlan0}/address 2>/dev/null)" \
        || publish "$WLAN_STATE" "result=fail" "backend=owned" "detail=wlan_on.sh rc=$_rc"
      return $_rc
      ;;
    mapped)
      [ -n "$RADIO_WLAN_INSMOD" ] || {
        log "no insmod mapping could be extracted for chipset $RADIO_SDIO_DEVICE"
        publish "$WLAN_STATE" "result=unsupported" "chipset=$RADIO_SDIO_DEVICE" "detail=no-mapping"
        return 3; }
      # Stage this unit's own modules. The tarball is the unit's, extracted to tmpfs - we never
      # write to flash here (the vendor path untars overlays into / on some variants; that is
      # provisioning, not bring-up, and it does not belong in a per-session call).
      ko_unpack || log "driver tarball ${RADIO_KO_TARBALL:-<none>} did not unpack"
      wlan_driver_up || {
        publish "$WLAN_STATE" "result=fail" "backend=mapped" "chipset=$RADIO_SDIO_DEVICE" "detail=driver-load"
        return 1; }
      # Converge on the interface appearing, discovered by enumeration; in STA mode a station's beside it, 1 s at most.
      _n=0; while [ "$_n" -lt 100 ]; do wlan_iface >/dev/null 2>&1 && break; _n=$((_n+1)); sleep 0.1; done
      sta_mode && sta_settle
      _i=$(ap_iface 2>/dev/null) || {
        log "driver loaded but no wireless interface appeared"
        publish "$WLAN_STATE" "result=fail" "backend=mapped" "chipset=$RADIO_SDIO_DEVICE" "detail=no-iface"
        return 1; }
      [ "${_wdl_loaded:-0}" = 1 ] && boot_mark wlan_up
      log "wlan interface $_i up (mac $(cat /sys/class/net/$_i/address 2>/dev/null))"

      # The AP layer is OURS on every variant - see the header. It carries no chipset content;
      # RADIO_WLAN_IF tells it which interface to configure, defaulting to wlan0 so the IW416
      # unit's behaviour is unchanged when this variable is absent.
      #
      # DELIBERATELY /script/radio_ap_up.sh AND NOT /script/start_bluetooth_wifi.sh. Every unit
      # already ships a VENDOR file at that second path, so testing it for existence proves the
      # file is there, not that it is ours - and running the vendor's copy on a stripped box is
      # destructive: with riddleBoxCfg removed it seds an empty wpa_passphrase into
      # /etc/hostapd.conf (persistent, on flash) and defaults WLANIP to 192.168.50.2, which is
      # NCM's own address, then repoints the DHCP pool onto the management subnet. An
      # unambiguous name is the guard: presence at this path means it is the owned layer.
      [ -x /script/radio_ap_up.sh ] || {
        log "driver is up but the owned AP layer (/script/radio_ap_up.sh) is not installed"
        log "  NOT falling back to /script/start_bluetooth_wifi.sh - that is the vendor's copy and"
        log "  on a stripped box it corrupts hostapd.conf and collides with the NCM subnet"
        publish "$WLAN_STATE" "result=fail" "backend=mapped" "ap_iface=$_i" "detail=no-owned-ap-layer"
        return 1; }
      # RADIO_BOX_NAME is resolved HERE, not inside the AP layer, so the SSID and the Bluetooth
      # name provably come from one decision rather than two derivations that can disagree.
      ONDEMAND_WIFI=1 RADIO_WLAN_IF="$_i" RADIO_BOX_NAME="$(box_name)" \
        sh /script/radio_ap_up.sh AP 2>&1 | sed "s/^/$LOGP   /"
      _n=0; while [ "$_n" -lt 50 ]; do ap_running && break; _n=$((_n+1)); sleep 0.2; done
      if ap_running; then
        publish "$WLAN_STATE" "result=ok" "backend=mapped" "chipset=$RADIO_SDIO_DEVICE" \
                "ap_iface=$_i" "ap_ip=$(ap_ip_of "$_i")" "wlan_mac=$(cat /sys/class/net/$_i/address 2>/dev/null)"
        log "AP converged on $_i"
        return 0
      fi
      publish "$WLAN_STATE" "result=fail" "backend=mapped" "ap_iface=$_i" "detail=ap-did-not-start"
      log "AP did not start on $_i"
      return 1
      ;;
    *)
      log "NO BACKEND for wifi on this unit - wireless unavailable (wired projection unaffected)"
      publish "$WLAN_STATE" "result=unsupported" "chipset=${RADIO_SDIO_DEVICE:-unknown}" "detail=no-backend"
      return 3
      ;;
  esac
}

ap_ip_of() { ifconfig "$1" 2>/dev/null | sed -n 's/.*inet addr:\([0-9.]*\).*/\1/p' | head -1; }

# The AP's DHCP server(s), by config path (never the NCM server, `busybox udhcpd -f /tmp/udhcpd_ncm.conf`), and their
# stop: SIGTERM, up to 3 s for them to go, then SIGKILL and up to 1 s more. BusyBox udhcpd writes its lease file
# (/var/lib/udhcpd.leases, on the jffs2 root) as it exits on SIGTERM; with flash slow (garbage collection just after an
# update's commit removed its .prev binaries) the server outlived the teardown, the next AP start took it for its own and
# started none, and once it had gone the AP had no DHCP (the release review of resident-bt, A3-1; radio_ap_up.sh keeps
# the same functions for its start, whose stop takes 1.5 s at most). AP_DHCP_MATCH is a test hook.
AP_DHCP_MATCH=${AP_DHCP_MATCH:-udhcpd .*etc/udhcpd.conf}
ap_dhcp_pids() { ps 2>/dev/null | grep -v grep | grep "$AP_DHCP_MATCH" | awk '{print $1}'; }
ap_pids_live() { for _apl in "$@"; do kill -0 "$_apl" 2>/dev/null && return 0; done; return 1; }
ap_dhcp_stop() {
  _ads=$(ap_dhcp_pids)
  [ -n "$_ads" ] || return 0
  for _adp in $_ads; do kill "$_adp" 2>/dev/null; done
  _adn=0; while [ "$_adn" -lt 15 ] && ap_pids_live $_ads; do _adn=$((_adn + 1)); sleep 0.2; done
  ap_pids_live $_ads || return 0
  log "the AP's DHCP server is still there 3 s after SIGTERM (its lease file on flash?) - SIGKILL"
  for _adp in $_ads; do kill -9 "$_adp" 2>/dev/null; done
  _adn=0; while [ "$_adn" -lt 10 ] && ap_pids_live $_ads; do _adn=$((_adn + 1)); sleep 0.1; done
  ap_pids_live $_ads || return 0
  log "the AP's DHCP server survived SIGKILL (a kernel wait?)"
  return 1
}

# ---- M3: the set-up network (ocbmd src/setup.rs) --------------------------------------------------------------------
# An adapter never used (ocbmd decides) offers, for a while, a set-up network from its one AP: SSID CarLink-Setup-xxxx
# (the box's own 4 hex), the public WPA2 passphrase "carlinksetup", 192.168.43.1/24 with DHCP, and CLIENT ISOLATION
# (ap_isolate=1: a second client cannot reach the first, so it cannot stand between the head unit and the box). Its
# configurations live in /tmp, hostapd's made from the CarPlay AP's: the flash ones are never written. A hostapd that
# refuses ap_isolate refuses the whole configuration, and then no set-up network is offered (the marker below never
# says ok). The CarPlay AP's start (do_wifi_ap_on) stops a set-up AP first.
SETUP_MARK=/tmp/.radio_setup_ap
SETUP_HAPD=/tmp/hostapd_setup.conf
SETUP_DHCP=/tmp/udhcpd_setup.conf
setup_dhcp_pids() { ps 2>/dev/null | grep -v grep | grep "udhcpd .*udhcpd_setup.conf" | awk '{print $1}'; }
do_setup_ap_on() {
  rm -f "$SETUP_MARK"
  ap_running && { log "setup_ap_on: an AP is running - no set-up network"; return 1; }
  caps || return 3
  [ "$RADIO_BACKEND" = mapped ] || { log "setup_ap_on: backend ${RADIO_BACKEND:-none} - no set-up network"; return 3; }
  wlan_driver_up || return 1
  _n=0; while [ "$_n" -lt 100 ] && ! wlan_iface >/dev/null 2>&1; do _n=$((_n+1)); sleep 0.1; done
  _si=$(ap_iface 2>/dev/null) || { log "setup_ap_on: no wireless interface"; return 1; }
  _ssfx=$(box_name | sed -n 's/^CarLink-\(....\)$/\1/p'); [ -n "$_ssfx" ] || _ssfx=0000
  [ -f /etc/hostapd.conf ] || { log "setup_ap_on: no /etc/hostapd.conf to start from"; return 1; }
  sed -e '/^interface=/d; /^ssid=/d; /^wpa_passphrase=/d; /^ap_isolate=/d; /^ctrl_interface/d' /etc/hostapd.conf > "$SETUP_HAPD.new" || return 1
  printf 'interface=%s\nssid=CarLink-Setup-%s\nwpa_passphrase=carlinksetup\nap_isolate=1\n' "$_si" "$_ssfx" >> "$SETUP_HAPD.new"
  mv -f "$SETUP_HAPD.new" "$SETUP_HAPD"
  # Its own pool on its own subnet, never /etc/udhcpd.conf's: a custom /etc/wifi_ip moves that one (radio_ap_up.sh), and
  # the set-up address stays 192.168.43.1, where Carlink looks for it.
  printf 'start\t192.168.43.100\nend\t192.168.43.200\ninterface\t%s\nlease_file\t/tmp/udhcpd_setup.leases\npidfile\t/tmp/udhcpd_setup.pid\nopt\tsubnet\t255.255.255.0\n' "$_si" > "$SETUP_DHCP"
  ifconfig "$_si" 192.168.43.1 netmask 255.255.255.0 up || return 1
  setsid hostapd "$SETUP_HAPD" </dev/null >>"$BOXLOG" 2>&1 &
  setsid busybox udhcpd -f "$SETUP_DHCP" </dev/null >/dev/null 2>&1 &
  _n=0; while [ "$_n" -lt 25 ] && ! ap_running; do _n=$((_n+1)); sleep 0.2; done
  sleep 1
  if ap_running && [ -n "$(setup_dhcp_pids)" ]; then
    echo "ok $_si" > "$SETUP_MARK"
    log "set-up network up on $_si (CarLink-Setup-$_ssfx, client isolation on)"
    return 0
  fi
  log "set-up network did not come up on $_si (hostapd refused its configuration?)"
  do_setup_ap_off
  return 1
}
do_setup_ap_off() {
  [ -e "$SETUP_MARK" ] || [ -e "$SETUP_HAPD" ] || return 2
  _so=$(sed -n 's/^interface=//p' "$SETUP_HAPD" 2>/dev/null | head -1)
  killall hostapd 2>/dev/null
  for _sp in $(setup_dhcp_pids); do kill "$_sp" 2>/dev/null; done
  _n=0; while [ "$_n" -lt 25 ] && { ap_running || [ -n "$(setup_dhcp_pids)" ]; }; do _n=$((_n+1)); sleep 0.2; done
  [ -n "$_so" ] && ifconfig "$_so" 0.0.0.0 2>/dev/null
  rm -f "$SETUP_MARK" "$SETUP_HAPD" "$SETUP_DHCP" /tmp/udhcpd_setup.leases /tmp/udhcpd_setup.pid
  log "set-up network down"
  return 0
}

# The Wi-Fi link's station (wifi_sta.sh): this unit's WLAN driver loaded, and the station's interface (sta_iface: one
# beside the AP's) up. Never an AP, an address or a scan. 2: already there; 3: no station on this unit (no mapped
# backend, no STA mode, one interface: sta_settle), which the keeper takes as final for the boot.
do_wifi_sta_up() {
  sta_mode || { log "wifi_sta_up: no hotspot in $WIFI_LINK_FILE"; return 3; }
  [ -e "$ONE_IF" ] && return 3
  _wsu=$(sta_iface 2>/dev/null) && { ifconfig "$_wsu" up 2>/dev/null; return 2; }
  caps || return 3
  [ "$RADIO_BACKEND" = mapped ] || { log "wifi_sta_up: backend ${RADIO_BACKEND:-none} has no station path"; return 3; }
  wlan_driver_up || return 1
  _n=0; while [ "$_n" -lt 100 ] && ! wlan_iface >/dev/null 2>&1; do _n=$((_n+1)); sleep 0.1; done
  sta_settle || return 3
  _wsu=$(sta_iface 2>/dev/null) || return 1
  ifconfig "$_wsu" up 2>/dev/null
  log "wifi_sta_up: station interface $_wsu up"
  return 0
}

do_wifi_ap_off() {
  caps || true
  # Chipset-neutral teardown. Deliberately NO module unload: whether rmmod is safe (and whether
  # it disturbs BT on a combo part) is chipset policy, and the supervisor's proven end-state for
  # BT is "attached but powered down" rather than unloaded.
  if [ "${RADIO_BACKEND:-none}" = owned ] && [ -x /script/wlan_off.sh ]; then
    log "backend=owned -> /script/wlan_off.sh"
    sh /script/wlan_off.sh; _rc=$?
    publish "$WLAN_STATE" "result=ok" "backend=owned" "detail=wlan_off rc=$_rc"
    return 0
  fi
  killall hostapd 2>/dev/null
  # Kill ONLY the AP's DHCP server. A killall udhcpd here would take down the NCM one serving
  # the management link - on a box with no UART that is the only way in, and it is exactly what
  # the vendor's close_bluetooth_wifi.sh does. Identify it by its CONFIG PATH, which is
  # unambiguous: the NCM server runs `busybox udhcpd -f /tmp/udhcpd_ncm.conf`.
  # And see it GONE (ap_dhcp_stop): one still writing its lease file was the next AP's DHCP server
  # that never was. Its pidfile's pid is no longer killed blind: the server is found by its config path
  # as above, and a stale pidfile can name any process.
  ap_dhcp_stop
  if sta_mode; then
    # The interface the AP ran on; the station's (an AP raised there before the link was set up) only loses the AP's
    # address: the station keeper brings it up for the hotspot.
    _i=$(sed -n 's/^interface=//p' /etc/hostapd.conf 2>/dev/null | head -1); [ -n "$_i" ] || _i=$(ap_iface 2>/dev/null)
    if [ -n "$_i" ] && [ "$_i" = "$(sta_iface 2>/dev/null)" ]; then ifconfig "$_i" 0.0.0.0 2>/dev/null
    elif [ -n "$_i" ]; then ifconfig "$_i" down 2>/dev/null; fi
  else
    _i=$(wlan_iface 2>/dev/null) && ifconfig "$_i" down 2>/dev/null
  fi
  _n=0; while [ "$_n" -lt 25 ] && ap_running; do _n=$((_n+1)); sleep 0.2; done
  ap_running && { log "hostapd still running after teardown"; publish "$WLAN_STATE" "result=fail" "detail=ap-survived"; return 1; }
  rm -f "$WLAN_STATE"
  log "wifi AP down"
  return 0
}

# =============================================================================================
# BOOT PRELOAD (on by default since 13 Nov 2026, off with /script/radio_preload_off; started by ocbm_boot.sh)
# =============================================================================================
# Load this unit's WLAN driver and attach BT, then power BT down: exactly the state a teardown
# leaves between sessions (drivers loaded, attached, hci0 DOWN, no AP - nothing a phone can see).
# The supervisor then arms Bluetooth powered but invisible for the next host unless /script/radio_arm_off
# (session_supervisor.sh, "Armed Bluetooth"); this preload is what attaches it for that at boot.
# The first app connect after power-up then skips the driver and firmware load (~7 s measured
# 2026-10-20, IW416), done here while the box waits for the head unit to boot. A deliberate,
# owner-approved exception to docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 3 - see there.
# Opt-in until Scott made it the default (13 Nov, the connect-time brief): nothing ever set the opt-in flag,
# so it never ran. On a boot with a self-test (a trial, or an update's), it waits for the self-test to pass (ocbm_boot.sh).
#
# Three guards, two against a boot-time wedge on a unit with no UART:
#  * ONE attach attempt (RADIO_BT_MAX_TRIES=1): never reclaim+reset, the GPIO reset under a live
#    WLAN driver being the wedge-prone step. A failure is left to the app-driven bt_on, exactly as
#    if this had not run.
#  * An in-flight marker on flash. A boot that finds it knows the previous preload never finished
#    (hung, or power lost inside it) and counts it; PRELOAD_GUARD_AFTER in a row DISABLE the preload
#    by writing the off flag with "guard" in it, so a unit that wedges here costs two bad boots, not
#    one in two. Not one: a power cut inside the 7 s window is no wedge (the Mac's dev adapter, 13 Nov,
#    latched off overnight), and on a truck the engine crank falls just after the ignition comes on.
#    A preload that finishes clears the count. An owner's own flag is empty. Installs, updates and
#    every way back to stock remove all three files, and ocbmd reports the state to Carlink (box info
#    "preload"). ocbm_boot.sh logs a disabled preload at every boot.
#  * A session owns the radios once a host is here. A Carlink already plugged in when the adapter
#    boots (as after an update) can reach bt_on before this does: the preload then stands down, and
#    it never powers off a controller it did not bring up itself (the Mac's 13 Nov review). A host that
#    comes while this attaches is handed the controller as hci0 answers (the handover, R3-5, below), and
#    this preload never powers that one off either: it is the host session's.
HOST_FLAG=/tmp/host_present
host_here() { [ "$(cat "$HOST_FLAG" 2>/dev/null)" = 1 ]; }
PRELOAD_OFF=/script/radio_preload_off
PRELOAD_MARK=/script/.radio_preload_inflight
PRELOAD_UNFINISHED=/script/.radio_preload_unfinished
PRELOAD_GUARD_AFTER=2
# Each state the preload enters, with the uptime, for btd's [startup] record (crates/vendor/wireless/src/startup.rs).
preload_state() { echo "$1 $(cut -d' ' -f1 /proc/uptime)" >> /tmp/radio_preload_state; }
do_preload() {
  if [ -e "$PRELOAD_MARK" ]; then
    _pn=$(cat "$PRELOAD_UNFINISHED" 2>/dev/null); case "$_pn" in ''|*[!0-9]*) _pn=0 ;; esac
    _pn=$((_pn + 1)); rm -f "$PRELOAD_MARK"
    if [ "$_pn" -ge "$PRELOAD_GUARD_AFTER" ]; then
      echo "guard: $_pn unfinished in a row" > "$PRELOAD_OFF"; rm -f "$PRELOAD_UNFINISHED"; sync
      preload_state off_guard
      log "preload: $_pn boots in a row never finished it - preload DISABLED (rm $PRELOAD_OFF to re-enable)"
      return 1
    fi
    echo "$_pn" > "$PRELOAD_UNFINISHED"; sync
    log "preload: the previous boot's preload never finished ($_pn of $PRELOAD_GUARD_AFTER in a row before it is switched off)"
  fi
  if host_here; then
    log "preload: a host is here - left to its session"
    preload_state stood_down
    return 0
  fi
  preload_state running
  boot_mark start
  touch "$PRELOAD_MARK"; sync
  _p0=$(cut -d' ' -f1 /proc/uptime)
  # Before the chip (20 Nov 2026): this unit's descriptor (the kept copy, caps) and its modules unpacked, neither of
  # which touches it, while the stock GPIO set-up may still be pulsing the Bluetooth reset line; then that set-up's end
  # (gpio_wait), then the chip. ocbm_boot.sh starts this beside the gadget for it, where the dispatcher marks that end.
  sdio_wait
  caps && ko_unpack
  gpio_wait
  RADIO_BT_MAX_TRIES=1
  _bt_handed=0
  do_bt_on; _pr=$?
  # A FAILED ATTACH'S HELPER (21 Nov review A). The helper (hciattach ... any) stays up after its attach failed, holding
  # the UART with hci0 registered and dead: the host's bt_on then spent its confirmation on that hci0 (about 18 s),
  # pushed the firmware into a UART the helper still held, and its first attempt was a reclaim+reset (rmmod hci_uart and
  # the gpio1 pulse) with the Wi-Fi driver loaded, the step radio_detect.sh's preload note records as wedging the kernel.
  # So the preload stops its own helper (bt_helper_stop: SIGTERM, then -9 on the helper; no reset, no rmmod), which takes
  # hci0 with it. In either order: 2.6.5 left the same helper after a failed Wi-Fi-first preload.
  # No firmware pushed (_bt_nofw): no attach was started, so there is no helper to stop.
  if [ "$_pr" = 1 ] && [ "${_bt_attached:-0}" = 1 ] && [ "${_bt_nofw:-0}" != 1 ]; then
    log "preload: the attach failed - its helper stopped (no reset, no rmmod): the host's bring-up starts on a free UART"
    bt_helper_stop
  fi
  # Attached by this preload: power it down to the between-sessions state. Not when it found the controller up
  # (rc 2: someone else's), not once a host has come (its session is waiting on this lock for the radio), and never
  # one handed to a host's session (bt_handover_give): that session's teardown powers it down, whatever the host does.
  if [ "$_pr" = 0 ] && [ "$_bt_handed" != 1 ] && ! host_here; then do_bt_off; fi
  # The seam's part is over: the locks go now, not after the bookkeeping below (a sync and a few forks), so a host's
  # bring-up waiting on them goes on at once. Only while they are still this preload's (lock_drop_own).
  trap - EXIT INT TERM HUP; lock_drop_own bt; lock_drop_own wlan
  # Round 4's page trim: the together guard's mark, left for here by a converged attach a host was waiting on; one sync.
  if [ "${_bt_mark_late:-0}" = 1 ]; then
    rm -f "$TOGETHER_MARK" "$TOGETHER_FAILS"
    log "the together guard's mark cleared after the locks went"
  fi
  rm -f "$PRELOAD_MARK" "$PRELOAD_UNFINISHED"; sync
  # "done" only when Bluetooth came up (rc 0, or 2: already up); a failed attach says so, e.g. done_rc1, so the
  # [startup] line never reads "preload done" after one (the Mac's review, PRELOAD-DONE-ANY-RC).
  case "$_pr" in 0|2) preload_state done ;; *) preload_state "done_rc$_pr" ;; esac
  # The duration from awk's own variables: the program inside nested quotes was taken apart by bash 3.2's sh (the Mac's,
  # tools/radio_hal_test.sh), which then left an awk reading its stdin. BusyBox ash and dash read it either way.
  _p1=$(cut -d' ' -f1 /proc/uptime)
  _pd=$(awk -v a="$_p1" -v b="$_p0" 'BEGIN { printf "%.1f", a - b }' </dev/null)
  log "preload: done rc=$_pr in $_pd s, wlan=$(wlan_iface 2>/dev/null || echo none) bt=$(hciconfig 2>/dev/null | grep -oE 'UP RUNNING|DOWN' | head -1)"
  [ "${_bt_attached:-0}" = 1 ] && boot_line preload "$_pr"
  return $_pr
}

# THE SDIO CARD FIRST (21 Nov 2026, review D). Started early, the preload reads the descriptor at about 2.0 s of uptime,
# and a radio whose SDIO card is not enumerated yet gives one that names no device, whose module and attach lines are
# then not this unit's own. Up to 3 s for the card's node, a look every 0.1 s, said either way; past that the preload
# goes on, and caps_final has the next verb detect again.
sdio_seen() { for _sdn in /sys/bus/sdio/devices/*/device; do [ -e "$_sdn" ] && return 0; done; return 1; }
sdio_wait() {
  _sdw=0
  while ! sdio_seen && [ "$_sdw" -lt 30 ]; do _sdw=$((_sdw + 1)); sleep 0.1; done
  if sdio_seen; then
    [ "$_sdw" -gt 0 ] && log "preload: waited about $((_sdw * 100)) ms for the radio's SDIO card"
  else
    log "preload: no SDIO card in 3 s - the descriptor names no device, and the next bring-up detects again"
  fi
  return 0
}

# THE STOCK GPIO SET-UP FIRST (20 Nov 2026). The dispatcher runs Carlinkit's init_gpio.sh beside ocbm_boot.sh at every
# boot, and it pulses the Bluetooth reset line (gpio1) before it sets the charge pins. The pulse is no reset of the
# chip's firmware (the bench, 20 Nov), but pulled under a live Wi-Fi driver that line wedged the kernel (19 Oct). Until
# 20 Nov the preload started after the gadget, ocbmd and the supervisor, and spent 0.8 s detecting before its first
# insmod, which kept it well behind the pulse; started early and with the descriptor kept, it no longer is. So the
# dispatcher marks that script's end (OCBM_GPIO_DONE) and the preload waits for it before it touches the chip, 3 s at
# most, then goes on and says so. A start without the mark (another start script) has no wait: ocbm_boot.sh then
# starts the preload where it always did.
gpio_wait() {
  [ -n "${OCBM_GPIO_DONE:-}" ] || return 0
  _gpw=0
  while [ ! -e "$OCBM_GPIO_DONE" ] && [ "$_gpw" -lt 30 ]; do _gpw=$((_gpw + 1)); sleep 0.1; done
  if [ -e "$OCBM_GPIO_DONE" ]; then
    boot_mark gpio
    [ "$_gpw" -gt 0 ] && log "preload: waited about $((_gpw * 100)) ms for the stock GPIO set-up (its Bluetooth reset pulse)"
  else
    log "preload: no end mark from the stock GPIO set-up in 3 s ($OCBM_GPIO_DONE) - going on"
  fi
  return 0
}

# =============================================================================================
# THE BOOT LINE (R3-4, 20 Nov 2026)
# =============================================================================================
# One line per boot for the drive log, once the first bring-up that attached Bluetooth is over (the preload's, or a
# host's bt_on when no preload attached it), and one more only when that one failed and a later one attached: the
# times of the cold path, the order it took and where the descriptor came from. Its own line and its own tag, so
# nothing that parses [startup] or [timeline] changes; fixed keys in a fixed order, each the uptime in whole ms or a
# word, `-` for one not stamped; no name, no address. Into the box log (not through log(): no prefix, and not to
# stdout, which bt_on's caller copies into bt.log as well, so the drive log would carry it twice), and into
# /tmp/radio_boot for a console. The keys are documented in docs/ops/02_TESTING.md, "The boot's radio line".
boot_line() {  # $1 = by (preload | bt_on), $2 = its exit code
  grep -q ' rc=0 ' "$BOOT_LINE" 2>/dev/null && return 0
  _bl_h=""; read -r _bl_h 2>/dev/null </tmp/host_present_uptime
  case "$_bl_h" in ''|0|*[!0-9]*) _bl_h=- ;; esac
  boot_note tries "${_bt_tries:-0}"
  boot_note btfw_tries "${_bt_fw_runs:--}"   # the loader's runs (bt_fw_push; one more after each reclaim+reset)
  _bl=$(awk -v by="$1" -v rc="$2" -v host="$_bl_h" '
    NF >= 2 && !($1 in s) { s[$1] = $2 }
    END {
      n = split("start caps caps_ms gpio order off wlan_start wlan_up wlan_free btfw_start btfw_end btfw btfw_ms btfw_tries attach hci_up handover handed tries fallback", k, " ")
      line = "[radio-boot] v1 by=" by
      for (i = 1; i <= n; i++) {
        val = "-"
        if (k[i] == "fallback") val = 0
        if (k[i] in s) val = s[k[i]]
        line = line " " k[i] "=" val
      }
      print line " rc=" rc " host=" host
    }' "$BOOT_MARKS" 2>/dev/null)
  [ -n "$_bl" ] || return 0
  echo "$_bl" 2>/dev/null >>"$BOXLOG"
  echo "$_bl" 2>/dev/null >>"$BOOT_LINE"
  rm -f "$BOOT_MARKS"   # a later line starts afresh
  return 0
}

# =============================================================================================
# BLUETOOTH
# =============================================================================================
# The connect timeline's bt_power (crates/box-common/src/timeline.rs): bt_on has the controller up and answering. For
# the bt_on verb only (the boot preload shares do_bt_on, and is no connect), save the one boot preload that hands the
# controller to a waiting host (bt_handover_claim): it stamps hci0's answer, as a bt_on's own attach does. One append,
# no fork; a failed one is ignored.
bt_power_stamp() {
  [ "$VERB" = bt_on ] || return 0
  _bps_up=""
  read -r _bps_up _bps_idle 2>/dev/null </proc/uptime
  case "$_bps_up" in *.*) echo "t bt_power ${_bps_up%.*}${_bps_up#*.}0" 2>/dev/null >>/tmp/startup_marks ;; esac
  return 0
}

# ---- The order Bluetooth and the Wi-Fi driver come up in (20 Nov 2026) ---------------------------------------------
# On a part whose vendor attach script waits for the Wi-Fi interface (RADIO_BT_AFTER_WLAN=1) the attach loaded the Wi-Fi
# driver first and waited for its interface: on the dev adapter's cold boot (20 Nov) 2.6 s, then 2.8 s of Bluetooth
# firmware and 0.8 s of attach. That flag is read off the whole attach script, and on the IW416 it comes from other
# parts' branches: its own branch pushes its firmware and attaches with no wait, and its stock firmware loads both at
# once at every boot (radio_detect.sh, section 4d, RADIO_BT_TOGETHER=1). The dev adapter's bench did the same on real
# power cycles (20 Nov: hci0 answering 3.24 s after both loads started at one instant, the AP up afterwards). There
# the two now run side by side ("together"); every other part keeps today's order exactly ("wlan_first" where its
# attach waits, "bt_alone" where it does not).
#   OFF, today's order back: the owner's /script/radio_together_off on flash, or the bench's /tmp/bench_together_off
#   (box_common::bench_flag's rule: set unless it reads "off"; Carlink writes "off" to clear it). Read at each attach.
#   THE FALLBACK: a together attach whose hci0 never answers marks the boot (TOGETHER_FELL), and the next attempt, in
#   the same bring-up (bt_on's tries after the first) or the next one, is Wi-Fi first; the log says so, and the boot
#   line (off=fallback, fallback=1).
#   THE GUARD, as the preload's: a together attach is bracketed on flash (TOGETHER_MARK, written before it and removed
#   after), so one that never finished (power lost or the box hung inside it) is found at the next attach; each failed
#   or unfinished one counts (TOGETHER_FAILS), and TOGETHER_GUARD_AFTER in a row switch the order off, by writing the
#   owner's file with "guard" in it, until the owner removes it. One that converges clears the count. An install, an
#   update and an update's first start lift the guard's file and its count (a new version tries again), never the
#   owner's own file; a revert and the restores remove both, since stock reads neither. A mark, count or off file that
#   cannot be written (a full flash) never stops a bring-up: the attach goes on, uncounted (21 Nov review B).
TOGETHER_OFF=/script/radio_together_off
TOGETHER_BENCH_OFF=/tmp/bench_together_off
TOGETHER_MARK=/script/.radio_together_inflight
TOGETHER_FAILS=/script/.radio_together_failed
TOGETHER_FELL=/tmp/.radio_together_fell_back
TOGETHER_GUARD_AFTER=2

# A bench flag: there, and not reading "off" (an empty file is set). The first line, with the shell's own read.
bench_flag() { [ -e "$1" ] || return 1; _bfl=""; read -r _bfl 2>/dev/null <"$1"; [ "$_bfl" != off ]; }

# ---- Round 4's switches here (22 Nov 2026; docs/ops/02_TESTING.md, "Round 4 switches") -------------------------------
# attach_poll and page_trim, both ON BY DEFAULT since 2.7.1 (the owner's decision of 22 Nov 2026). Each goes back to
# 2.7.0's path, exactly, with "off" in /tmp/bench_<name> (a reboot clears it) or, for a cold boot, whose /tmp is empty,
# in /etc/ocbm/bench_<name> on flash (OCBM's own state directory: updates keep it, the full way back to stock removes it
# whole, so no remover names it). The first of the two that exists decides, so an "off" in /tmp overrides the flash
# (box_common::bench_override's rule); any other text, an empty file or "on", is on. 0: on, 1: off. _r4f: the file that
# decided, "" for the default; _r4t: "[bench] round4: <name> (<file>): " when a file turned it on, the tag its lines
# carry then, else "" (the default's lines are plain). Its "off" says so where it would have acted.
r4_on() {
  _r4t=""
  for _r4f in "/tmp/bench_$1" "/etc/ocbm/bench_$1"; do
    [ -e "$_r4f" ] || continue
    _r4v=""; read -r _r4v 2>/dev/null <"$_r4f"
    [ "$_r4v" = off ] && return 1
    _r4t="[bench] round4: $1 ($_r4f): "; return 0
  done
  _r4f=""; return 0
}

# The order the next attach takes: _bto = together | wlan_first | bt_alone, and _bto_off = why a part that could take
# together does not (owner | guard | bench | fallback), else "-". $1 = peek: count nothing (the probe verb).
bt_order() {
  _bto_off=-; _bto_l=""
  if [ "${RADIO_BT_AFTER_WLAN:-0}" != 1 ]; then _bto=bt_alone; return 0; fi
  _bto=wlan_first
  [ "${RADIO_BT_TOGETHER:-0}" = 1 ] || return 0
  if [ -e "$TOGETHER_OFF" ]; then
    read -r _bto_l 2>/dev/null <"$TOGETHER_OFF"
    case "$_bto_l" in guard*) _bto_off=guard ;; *) _bto_off=owner ;; esac
    return 0
  fi
  if bench_flag "$TOGETHER_BENCH_OFF"; then _bto_off=bench; return 0; fi
  if [ -e "$TOGETHER_FELL" ]; then _bto_off=fallback; return 0; fi
  if [ -e "$TOGETHER_MARK" ] && [ "${1:-}" != peek ]; then
    if ! together_count "never finished, power lost or the box hung inside it"; then
      _bto_off=guard; read -r _bto_l 2>/dev/null <"$TOGETHER_OFF"; return 0
    fi
  fi
  _bto=together
}

# One together attach that failed or never finished ($1 says which). 0: counted; 1: that made TOGETHER_GUARD_AFTER in a
# row, and the order is switched off.
# The off file goes in whole or not at all (21 Nov review F): written beside it, synced, renamed over it, synced, as the
# kept descriptor is. A plain `>` cut by a power loss left it empty, which every reader takes for the owner's own file,
# and installs and updates never lift that. A stray $TOGETHER_OFF_NEW at worst (a dot file, so the restores' extras
# pass removes it; the next trip writes it again). Neither the off file nor the count stops a bring-up when it cannot
# be written: the failure goes uncounted, the order stays as it was, and the log says so.
TOGETHER_OFF_NEW=/script/.radio_together_off.new
together_count() {
  _tgc=""; read -r _tgc 2>/dev/null <"$TOGETHER_FAILS"; case "$_tgc" in ''|*[!0-9]*) _tgc=0 ;; esac
  _tgc=$((_tgc + 1)); rm -f "$TOGETHER_MARK"
  if [ "$_tgc" -ge "$TOGETHER_GUARD_AFTER" ]; then
    if { echo "guard: $_tgc together attaches in a row failed or never finished, the last one $1" >"$TOGETHER_OFF_NEW" \
         && sync && mv -f "$TOGETHER_OFF_NEW" "$TOGETHER_OFF"; } 2>/dev/null; then
      rm -f "$TOGETHER_FAILS"; sync
      log "radio order: $_tgc together attaches in a row failed or never finished, the last one $1 - together switched OFF, Wi-Fi first from now on (rm $TOGETHER_OFF to try it again)"
      return 1
    fi
    rm -f "$TOGETHER_OFF_NEW" 2>/dev/null
    log "radio order: $_tgc together attaches in a row failed or never finished, but the guard's off file could not be written ($TOGETHER_OFF) - the order stays, uncounted"
    return 0
  fi
  if { echo "$_tgc" >"$TOGETHER_FAILS"; } 2>/dev/null; then sync
  else log "radio order: the guard's count could not be written ($TOGETHER_FAILS) - this one goes uncounted"; fi
  log "radio order: a together attach $1 ($_tgc of $TOGETHER_GUARD_AFTER in a row before together is switched off)"
  return 0
}

# ---- The handover: a waiting host's session takes Bluetooth as hci0 answers (R3-5, 20 Nov 2026) ----------------------
# A host that came while the boot preload attached Bluetooth (the truck's cold start: the host at about 3 s, hci0 at
# about 6.8) waited, on the dev adapter's bench of 70caa3c (20 Nov), 1.6 to 1.9 s from hci0 answering to its first
# page: this preload's SCO and name set-up and its bookkeeping, the host's bt_on behind the bt lock (a look every 0.2 s
# and its confirmation), btd's start after bt_on, and btd's DOWN/UP. When the handover is asked for (OPT-IN, below), with
# a host here as the preload's attach converges, the preload hands the controller over at once, and the host's btd,
# started beside its bt_on and waiting (session_supervisor.sh wireless_up, OCBM_BTD_EARLY;
# crates/vendor/wireless/src/handover.rs), pages straight away:
#   1. THE CLAIM (bt_handover_claim, as hci0 answers): `hciconfig <dev> up` on the device the attach registered. The
#      attach leaves the kernel's auto power-off armed: hci_register_dev sets HCI_AUTO_OFF, and the open it queues
#      (hci_power_on) closes the device again 2 s later unless something claims it (net/bluetooth/hci_core.c, v3.14.52:
#      hci_register_dev :3037-3038, hci_power_on :2387-2417). HCIDEVUP clears that bit and cancels the close, then
#      finds the device up (hci_dev_open :1956-1980, hci_dev_do_open's -EALREADY :1890, which hciconfig takes for
#      success). Carlinkit's own IW416 branch runs that `up` right after its hciattach; radio_detect.sh does not extract
#      it, and btd's DOWN/UP covered it until now. bt_power is stamped with hci0's answer, as a bt_on's own attach
#      stamps it. No SCO lines and no name here: btd writes the identity (Simple Pairing, class, name, EIR), and the
#      vendor's SCO set-up never outlived btd's DOWN/UP (apply_sco), so the session's controller is set up as before.
#   2. THE HANDOVER (bt_handover_give, once the Wi-Fi interface this attach brought up beside it is in, so btd's name,
#      from wlan0's address, is the one it always had): $HANDOVER, "<handed ms> <hci_up ms> <dev>", then the bt lock
#      goes, while it is this preload's, before the rest of its bookkeeping. From here the controller is the host
#      session's, and this preload touches it no more (no bt_off at its end, whatever the host does next).
#   3. THE HOST'S SIDE (bt_handed): a bt_on that finds a handover written since this host came (ocbmd's origin,
#      /tmp/host_present_uptime) touches nothing, as btd may be paging on it already, and returns 2, with "opened" in
#      $BT_WARM for a btd that goes on its return. btd goes on whichever comes first: the handover, proved there with
#      one round trip and brought up with no DOWN/UP (2.6.0's "opened" proof of state: a controller opened from DOWN
#      just now, its event mask set by that open, nothing reset since and, the claim standing in for bt_on's own `up`,
#      nothing to close it); or the wrapper's word that bt_on is over, today's bring-up.
#   OPT-IN, OFF BY DEFAULT (the owner's decision, 21 Nov 2026). On the dev adapter's bench of 4a6ce9c (real power cuts,
#   the host up first) the handover did what it was built to do (handed 30 to 40 ms after hci_up, the page about 100 ms
#   after it) but 2 of its 6 boots had link trouble (a late answer cut for no SDP channel; an RFCOMM connect that timed
#   out), against 0 of 9 with the DOWN/UP kept (70caa3c's 5 and 4a6ce9c's 4 with it off): suggestive, not proof, the
#   cause not found (docs/wireless/01_BT_AND_RADIO.md, "The cold boot", 6). So the handover runs only when asked for:
#   the owner's /script/radio_handover_on on flash, or the bench's /tmp/bench_handover_on (bench_flag's rule: set
#   unless it reads "off"), read here at the attach and by the supervisor at the host's arrival. With neither, nothing
#   of the handover runs: no claim, no handover file, btd's DOWN/UP. The supervisor still starts btd early (its
#   btd_early, on by default since 2.7.1), and that btd waits for bt_on's end; with btd_early off too, the cold path is
#   4a6ce9c's with its old off file. That off file, /script/radio_handover_off, is retired: nothing reads it, and
#   updates, an update's first start, reverts and the restores remove it. A teardown's bt_off removes $HANDOVER: it was
#   for the session that ends.
HANDOVER=/tmp/.radio_bt_handover
HANDOVER_ON=/script/radio_handover_on
HANDOVER_BENCH_ON=/tmp/bench_handover_on
# Whether the handover is asked for, and by whom: _hov = on_owner, on_bench, or off (not opted in).
handover_state() {
  if [ -e "$HANDOVER_ON" ]; then _hov=on_owner; elif bench_flag "$HANDOVER_BENCH_ON"; then _hov=on_bench; else _hov=off; fi
}

# The boot preload's attach answered ($1 the device, $2 the uptime ms of the answer): with a host here and the handover
# asked for, the claim and bt_power. 0: claimed, the handover due (bt_handover_give); 1: not, today's path (SCO, name,
# and the host's bt_on after this preload).
bt_handover_claim() {
  [ "$VERB" = preload ] || return 1
  _bhc_h=""; read -r _bhc_h 2>/dev/null <"$HOST_FLAG"
  [ "$_bhc_h" = 1 ] && [ -n "$2" ] || return 1
  handover_state
  if [ "$_hov" = off ]; then
    log "handover: off (not asked for: $HANDOVER_ON or $HANDOVER_BENCH_ON) - the host's bt_on takes $1 after this preload"
    return 1
  fi
  if ! hci_rt "$1" up 2>/dev/null; then
    log "handover: hciconfig $1 up failed - no handover; the host's bt_on takes it after this preload"
    return 1
  fi
  echo "t bt_power $2" 2>/dev/null >>/tmp/startup_marks
  _bt_handover_due="$2 $1"
  return 0
}

# The handover itself, once the attach's Wi-Fi side is in (bt_attach_together's join; at once in today's order). A no-op
# unless bt_handover_claim made one due. Sets _bt_handed.
bt_handover_give() {
  [ -n "${_bt_handover_due:-}" ] || return 0
  set -- $_bt_handover_due; _bt_handover_due=""
  up_ms
  if [ -n "$_um" ] && echo "$_um $1 $2" 2>/dev/null >"$HANDOVER"; then
    boot_note handed "$_um"
    _bt_handed=1
    trap - EXIT INT TERM HUP; lock_drop_own bt
    log "handover: $2 is the host's session's from uptime $_um ms (it answered at $1 ms; claimed, no SCO or name set: btd writes the identity) - the bt lock goes now"
  else
    log "handover: $HANDOVER not written - the host's bt_on takes Bluetooth after this preload"
  fi
  _bhg_mac=""; read -r _bhg_mac 2>/dev/null <"/sys/class/bluetooth/$2/address"
  publish "$BT_STATE" "result=ok" "backend=mapped" "chipset=$RADIO_SDIO_DEVICE" "hci_dev=$2" "bt_mac=$_bhg_mac"
  return 0
}

# The host's side: a handover written since this host came (the shell's uptime is 10 ms coarse, the origin to the ms),
# to a device that is there. 0: handed (_bhd_at, _bhd_hu, _bhd_dev); 1: none, or one from before this host came, which
# goes (another host's, whose teardown did not remove it).
bt_handed() {
  [ -e "$HANDOVER" ] || return 1
  _bhd_at=""; _bhd_hu=""; _bhd_dev=""; _bhd_o=""
  read -r _bhd_at _bhd_hu _bhd_dev _bhd_rest 2>/dev/null <"$HANDOVER"
  read -r _bhd_o 2>/dev/null </tmp/host_present_uptime
  case "$_bhd_at" in ''|*[!0-9]*) _bhd_at="" ;; esac
  case "$_bhd_o" in ''|*[!0-9]*) _bhd_o="" ;; esac
  if [ -n "$_bhd_at" ] && [ -n "$_bhd_o" ] && [ $((_bhd_at + 10)) -gt "$_bhd_o" ] && [ -n "$_bhd_dev" ] \
     && [ -d "/sys/class/bluetooth/$_bhd_dev" ]; then
    return 0
  fi
  rm -f "$HANDOVER"
  log "a handover from before this host came (or unreadable) - removed; Bluetooth comes up as before"
  return 1
}

# Stop the attach helper still holding the UART (SIGTERM, then -9: never the controller itself). The first half of
# bt_reclaim_reset; the fallback stops only this, today's later attempts go on to the line discipline and the reset.
bt_helper_stop() {
  _helper=${RADIO_BT_ATTACH_CMD%% *}
  killall "$_helper" 2>/dev/null
  _k=0; while [ "$_k" -lt 20 ] && { pgrep -x "$_helper" >/dev/null 2>&1 || ps | grep -v grep | grep -qw "$_helper"; }; do
    _k=$((_k+1)); sleep 0.25
  done
  # ESCALATE. A helper that ignored SIGTERM still owns /dev/ttymxc2, and continuing to
  # rmmod the line discipline and re-attach underneath it recreates the very H5 collision
  # this function exists to clear. The owned bt_off path already treats -9 on the helper as
  # safe; only the CONTROLLER must never be SIGKILLed mid-bring-up, not the attach process.
  if pgrep -x "$_helper" >/dev/null 2>&1 || ps | grep -v grep | grep -qw "$_helper"; then
    log "  $_helper ignored SIGTERM - escalating to -9 before touching the line discipline"
    killall -9 "$_helper" 2>/dev/null
    _k=0; while [ "$_k" -lt 12 ] && { pgrep -x "$_helper" >/dev/null 2>&1 || ps | grep -v grep | grep -qw "$_helper"; }; do
      _k=$((_k+1)); sleep 0.25
    done
  fi
}

# NEVER launch an attach while another helper still owns the UART. Learned on hardware:
# a controller can sit at `UP RUNNING` and still be wedged (name read times out), so we
# correctly decide to re-attach - but if the previous helper is alive it still holds
# /dev/ttymxc2, and the second attach's H5 SYNC collides with the first's link state:
#   Realtek Bluetooth WARN: OP_H5_SYNC Transmission timeout
#   Realtek Bluetooth ERROR: Retransmission exhausts
# which leaves the chip deader than it started. A stale helper is not a reason to skip the
# attach - it is a reason to reclaim the port FIRST. This is the same reclaim the owned
# attach path does with `killall hciattach` before re-attaching.
# RECLAIM-RESET-ATTACH, retried. Three lessons, each paid for on hardware:
#
#  1. Never attach while another helper owns the UART. A controller can sit at `UP RUNNING`
#     and still be wedged (name read times out), so re-attaching is right - but the stale
#     helper still holds /dev/ttymxc2 and the new H5 SYNC collides with the old link state
#     ("OP_H5_SYNC Transmission timeout" -> "Retransmission exhausts"), leaving the chip
#     deader than before.
#  2. Killing the helper is not enough: the line-discipline MODULE holds the stale H5 state.
#     Both the vendor's reset_bluetooth() and our own reset_bt() rmmod it before the reset,
#     and a reset without that step does not revive the chip - measured.
#  3. The reset line is asserted HIGH then released LOW (1 -> 0). Both implementations agree;
#     driving it the other way is a no-op that looks like a reset in the log.
#
# One attempt is not enough either - the owned path loops reset+attach+responsiveness, so
# this does too. Everything here is derived from the mapping, so no chip name appears.
bt_reclaim_reset() {
  bt_helper_stop
  _ldisc=${RADIO_BT_LDISC_KO%.ko}
  [ -n "$_ldisc" ] && grep -q "^$_ldisc " /proc/modules 2>/dev/null && { sleep 1; rmmod "$_ldisc" 2>/dev/null; }
  if [ -e /sys/class/gpio/gpio1/value ]; then
    echo 1 > /sys/class/gpio/gpio1/value 2>/dev/null; sleep 0.2
    echo 0 > /sys/class/gpio/gpio1/value 2>/dev/null; sleep 0.5
  fi
  [ -n "$RADIO_BT_LDISC_KO" ] && [ -e "/tmp/$RADIO_BT_LDISC_KO" ] \
    && { grep -q "^$_ldisc " /proc/modules 2>/dev/null || insmod "/tmp/$RADIO_BT_LDISC_KO" 2>/dev/null; }
}

# The line discipline the attach needs, from this unit's modules, and loud when it is not there.
bt_ldisc_ready() {
  # LAST-RESORT LDISC RECOVERY. Everything above depends on the DESCRIPTOR having named the
  # module, and the descriptor lives at /tmp/radio_caps -- on a tmpfs that every power loss
  # wipes. The box is USB-powered, so unplugging it to move it between the bench and the head
  # unit IS a power loss: each move starts from an empty /tmp. `caps()` re-derives the
  # descriptor (the kept copy, or /script/radio_detect.sh), so if that script is absent, unreadable, or
  # its extraction yields nothing, RADIO_BT_LDISC_KO comes back EMPTY -- and this whole block
  # then no-ops. The supervisor invokes us as `sh /script/radio_hal.sh bt_on 2>&1 | tee -a
  # /tmp/box.log >/tmp/bt.log` inside a detached wrapper and never reads the exit status, so
  # the failure is completely
  # silent: OCBM, MFi and CT_SUBSCRIBE all succeed, and Bluetooth simply never exists.
  #
  # Diagnosed on hardware 2026-08-28: no hci0, no pairing, and the only visible symptom was
  # hciattach reporting "Can't set line discipline: Invalid argument" -- three steps downstream
  # of the real cause, and directly beneath a flawless firmware download.
  #
  # So: if the descriptor did not name a line discipline, find one in the tarball ourselves.
  # This is deliberately independent of radio_detect.sh -- a recovery path that depends on the
  # thing that failed is not a recovery path.
  if [ -z "$RADIO_BT_LDISC_KO" ] || [ ! -e "/tmp/$RADIO_BT_LDISC_KO" ]; then
    for _cand in /tmp/*hci_uart*.ko; do
      [ -e "$_cand" ] || continue
      RADIO_BT_LDISC_KO=${_cand##*/}
      log "descriptor named no BT line discipline - recovered $RADIO_BT_LDISC_KO from $RADIO_KO_TARBALL"
      break
    done
  fi

  if [ -n "$RADIO_BT_LDISC_KO" ] && [ -e "/tmp/$RADIO_BT_LDISC_KO" ]; then
    grep -q "^${RADIO_BT_LDISC_KO%.ko} " /proc/modules 2>/dev/null \
      || insmod "/tmp/$RADIO_BT_LDISC_KO" 2>/dev/null
  fi

  # And SAY SO if the discipline still is not registered. hciattach cannot create hci0 without
  # it, so continuing quietly here is what turned a one-line fault into a multi-hour hunt.
  if ! grep -q "^n_hci" /proc/tty/ldiscs 2>/dev/null; then
    log "FATAL: no n_hci line discipline registered (ldisc_ko='${RADIO_BT_LDISC_KO:-<unset>}',"
    log "       tarball='${RADIO_KO_TARBALL:-<unset>}'). hciattach WILL fail EINVAL and no hci0"
    log "       will appear. Check that /script/radio_detect.sh exists and that the tarball"
    log "       contains an hci_uart module."
  fi
}

# The firmware this unit's branch pushes over the UART before its attach, where it has one (the NXP parts), timed for
# the boot line, with what the loader said: its verdict, and its own time. The first run's stamps stand.
#
# BOUNDED (22 Nov 2026): a push takes 2.2 to 3.2 s (every boot line logged), and the loader waits 4 s for the chip's
# boot loader (its own "Timeout: 4000"); one still running after RADIO_BTFW_WAIT seconds (10; a test hook) is sent TERM,
# and KILL 2 s later, by its watcher (bt_fw_watch, a look every 0.5 s). It ran unbounded before, under the bt lock.
#
# Its code: 0 pushed, with BT_FW_PUSHED written (bt_fw_push): the loader exited 0, or said "Download Complete" (and no
# "Download Error") whatever its code, a stop at the bound after it included (the code logged); 1 not, the loader having
# failed (its code and its last lines go to the log, any address in them masked) or been stopped at the bound (its
# code, and that it was stopped). The bench of 22 Nov (a cold boot, by hand): the loader exited 255, the attach went on
# against a chip with no firmware, and the box went silent.
RADIO_BTFW_WAIT=${RADIO_BTFW_WAIT:-10}
case "$RADIO_BTFW_WAIT" in ''|*[!0-9]*) RADIO_BTFW_WAIT=10 ;; esac
# A firmware push that succeeded this boot (bt_fw_push). In /tmp, so a SoC restart or a power cut, which put the chip
# back in its boot loader, take it away with the firmware.
BT_FW_PUSHED=/tmp/.radio_btfw_pushed
# A BR/EDR address in the loader's lines, masked: it prints none that we know of (chip id, file, baud rates, sizes).
MAC_MASK='s/[0-9A-Fa-f][0-9A-Fa-f]\(:[0-9A-Fa-f][0-9A-Fa-f]\)\{5\}/<addr>/g'
bt_fw_preload() {
  [ -n "$RADIO_BT_PRELOAD" ] && [ -n "${RADIO_BT_PRELOAD_CMD:-}" ] || return 0
  log "firmware preload: $RADIO_BT_PRELOAD_CMD"
  _bt_fw_runs=$((${_bt_fw_runs:-0} + 1))
  boot_mark btfw_start; _bfp_t0=${_um:-}
  # _bfp_live, _bfp_w, _bfp_tok: the loader, its watcher and the run's token while they run, for a trapped signal to
  # stop first (bt_fw_stop). The token is this run's own (a lost-kill watcher of the run before must not take it).
  _bfp_w=""; _bfp_tok=/tmp/.radio_btfw_run.$$.$_bt_fw_runs
  write_empty "$_bfp_tok" || _bfp_tok=""
  $RADIO_BT_PRELOAD_CMD >/tmp/radio_bt_fw.log 2>&1 &
  _bfp_pid=$!; _bfp_live=$_bfp_pid
  bt_fw_watch "$_bfp_pid" "$_bfp_tok" $((RADIO_BTFW_WAIT * 1000)) "$_bfp_t0" </dev/null >/dev/null 2>&1 &
  _bfp_w=$!
  # The loader's code and time are its own, taken as it returns, and the watcher is never waited for (the hardware test
  # of 22 Nov, BusyBox ash: a TERM to the watcher, sent as a loader that failed at once returned, came before the fresh
  # subshell had reset its parent's trapped TERM and was lost; the parent then waited out the watcher's 12 s and took
  # them for a stop at the bound). Its token goes at once, and the kill only hurries it: one that missed the kill ends
  # at its next look, signalling nothing. With no token (it could not be written) only a KILL ends it.
  wait "$_bfp_pid"; _bfp_rc=$?; _bfp_live=""
  boot_mark btfw_end; _bfp_ms=$(( ${_um:-0} - ${_bfp_t0:-0} ))
  if [ -n "$_bfp_tok" ]; then rm -f "$_bfp_tok"; kill "$_bfp_w" 2>/dev/null; else kill -KILL "$_bfp_w" 2>/dev/null; fi
  _bfp_w=""
  set -- $(awk '/Download Complete/ { c = 1 } /Download Error/ { e = 1 }
    match($0, /time:[ ]*[0-9]+/) { t = substr($0, RSTART, RLENGTH); sub(/time:[ ]*/, "", t) }
    END { v = "other"; if (c) v = "complete"; if (e) v = "error"; if (t == "") t = "-"; print v " " t }' \
    /tmp/radio_bt_fw.log 2>/dev/null)
  _bfp_v=${1:-other}
  boot_note btfw "$_bfp_v"; boot_note btfw_ms "${2:--}"
  # The loader's own word stands over its code (the 2.7.1 review): "Download Complete" with no "Download Error" is the
  # firmware pushed, whatever code followed it, from a loader the bound stopped after it said so too; the code is logged.
  if [ "$_bfp_rc" != 0 ] && [ "$_bfp_v" = complete ]; then
    if [ "$_bfp_ms" -ge $((RADIO_BTFW_WAIT * 1000)) ]; then
      log "  firmware preload stopped at the bound (exit $_bfp_rc) after it said Download Complete - taken as pushed"
    else
      log "  firmware preload exited $_bfp_rc after it said Download Complete - taken as pushed"
    fi
    _bfp_rc=0
  fi
  if [ "$_bfp_rc" = 0 ]; then
    # A builtin's redirection, no fork. Unwritten (a full /tmp), a later failed push stops the attach.
    write_empty "$BT_FW_PUSHED" || log "  the push's mark could not be written ($BT_FW_PUSHED)"
    return 0
  fi
  log "  firmware preload exited $_bfp_rc"
  if [ "$_bfp_ms" -ge $((RADIO_BTFW_WAIT * 1000)) ]; then
    log "  the loader had not returned after $RADIO_BTFW_WAIT s - stopped"
    return 1
  fi
  log "  the loader's last lines:"
  tail -5 /tmp/radio_bt_fw.log 2>/dev/null | cut -c1-160 | sed -e "$MAC_MASK" -e "s/^/$LOGP   /"
  return 1
}

# The loader's watcher (bt_fw_preload's bound, bt_fw_stop's stop): TERM to pid $1 once $3 ms have passed since $4 (ms
# of uptime; "" counts its own looks instead), and KILL 2 s after that, each only while the run's token $2 is there
# ("": none could be written, then always). It looks every 0.5 s and ends as soon as the token goes, which its parent
# removes the moment the loader returns: so a watcher whose own kill was lost signals no pid after that (none taken by
# another process since) and is gone within a look. Never waited for (the hardware test of 22 Nov). Its sleeps fork,
# nothing else.
bt_fw_watch() {  # $1 pid, $2 token, $3 after_ms, $4 start_ms
  _bfw_at=$3; _bfw_sig=TERM; _bfw_n=0
  while [ -z "$2" ] || [ -e "$2" ]; do
    up_ms
    if [ -n "$4" ] && [ -n "$_um" ]; then _bfw_e=$((_um - $4)); else _bfw_e=$((_bfw_n * 500)); fi
    if [ "$_bfw_e" -ge "$_bfw_at" ]; then
      { [ -z "$2" ] || [ -e "$2" ]; } && kill -"$_bfw_sig" "$1" 2>/dev/null
      [ "$_bfw_sig" = KILL ] && return 0
      _bfw_sig=KILL; _bfw_at=$((_bfw_at + 2000))
    fi
    _bfw_n=$((_bfw_n + 1)); sleep 0.5
  done
}

# The loader in flight (bt_fw_preload), stopped before a trapped signal lets the bt lock go (lock_take's traps; the
# 2.7.1 review): it runs in the background, so the wait for it ends at the signal, and it would go on holding the UART
# that the lock's next holder pushes to or attaches on. The bound's watcher first, told by its token and stopped with
# KILL (the trap ignores TERM by then, and so do the children it starts), whether or not a loader still runs; then the
# loader: TERM, and KILL 2 s later from a watcher of this stop's own, the loader reaped before the lock goes. That
# watcher is told by its token too, never waited for.
bt_fw_stop() {
  [ -n "${_bfp_tok:-}" ] && rm -f "$_bfp_tok"
  [ -n "${_bfp_w:-}" ] && kill -KILL "$_bfp_w" 2>/dev/null
  _bfp_w=""
  [ -n "${_bfp_live:-}" ] || return 0
  up_ms; _bfs_tok=/tmp/.radio_btfw_stop.$$
  write_empty "$_bfs_tok" || _bfs_tok=""
  bt_fw_watch "$_bfp_live" "$_bfs_tok" 0 "$_um" </dev/null >/dev/null 2>&1 &
  _bfs_w=$!
  wait "$_bfp_live" 2>/dev/null
  if [ -n "$_bfs_tok" ]; then rm -f "$_bfs_tok"; else kill -KILL "$_bfs_w" 2>/dev/null; fi
  _bfp_live=""
}

# The firmware push (22 Nov 2026): the loader once, and once more when it failed. 0: pushed (or none to push); 1: not,
# and the caller attaches nothing, with no hciattach and no reclaim+reset: an attach against a chip left without its
# firmware is what preceded the silent box of the 22 Nov bench. _bt_nofw says so to the preload's end (no helper to
# stop), and the boot line counts the runs (btfw_tries).
#
# A failure after a push that succeeded earlier this boot (BT_FW_PUSHED) is no reason to stop: the chip still runs that
# firmware, which only a SoC restart or a power cut takes away (both empty /tmp; the 13 Nov restarts each pushed in
# full), not reclaim+reset's gpio1 pulse (20 Nov, by hand: after it hci0 answered within half a second, and the Wi-Fi
# driver loaded in under 0.4 s, too quick for a download). A loader before such a chip finds no boot loader: measured
# on the dev adapter on 22 Nov 2026, the real loader returns 255 after about 4.05 s (its 4 s wait for one), and with
# the push's mark there the attach goes on and hci0 answers about 0.5 s later (that 20 Nov run's "at once, no output"
# was a result file left by the run before it). So the attach goes on, as it always has there: the together
# fallback's, a host's bt_on after the preload's attach failed, a try after a reset.
bt_fw_push() {
  _bt_nofw=0
  bt_fw_preload && return 0
  if [ -e "$BT_FW_PUSHED" ]; then
    log "  the chip runs the firmware pushed earlier this boot - the attach goes on"
    return 0
  fi
  log "  the firmware push failed - pushing it once more"
  bt_fw_preload && return 0
  _bt_nofw=1
  log "  the firmware push failed twice - no attach, and no reset after it"
  publish "$BT_STATE" "result=fail" "backend=mapped" "chipset=$RADIO_SDIO_DEVICE" "detail=no-firmware"
  return 1
}

# Attach and converge, $1 tries. Clause 1: the attach runs detached so a helper that blocks cannot hang this verb, but
# we DO NOT return until it converges. That is the whole difference from the vendor path, which forks the attach and
# returns immediately. RADIO_BT_MAX_TRIES: the boot preload passes 1, so it never reaches reclaim+reset (attempt 1 only
# reclaims a helper that is already running, which at boot none is). RADIO_ATTACH_POLLS is a test hook
# (tools/radio_hal_test.sh): the looks an attempt gets to converge at 0.5 s apart (30); attach_poll, below, makes them
# 0.1 s apart and five times as many.
RADIO_ATTACH_POLLS=${RADIO_ATTACH_POLLS:-30}
case "$RADIO_ATTACH_POLLS" in ''|*[!0-9]*) RADIO_ATTACH_POLLS=30 ;; esac
# Whether an attempt of bt_attach_loop looks again: while its uptime since the attach (_bal_t0) is under _bal_ms, with
# attach_poll; else, or with no uptime to read, while its looks (_n) are under _bal_n, 2.7.0's rule. No fork.
bt_attach_looks_on() {
  if [ -n "$_bal_ms" ] && [ -n "$_bal_t0" ]; then
    up_ms
    [ -n "$_um" ] && { [ $((_um - _bal_t0)) -lt "$_bal_ms" ]; return; }
  fi
  [ "$_n" -lt "$_bal_n" ]
}
bt_attach_loop() {
  _helper=${RADIO_BT_ATTACH_CMD%% *}
  _try=0; _d=""; _bal_ok=0
  # Round 4, item 8 (attach_poll, on by default since 2.7.1): a look every 0.1 s, not 0.5, as hci0 comes up, which put
  # hci_up up to half a second after hci0 answered. A look with no hci0 forks one subshell (the glob's), and one with
  # hci0 there but not up fails fast (ENETDOWN); but against a controller up and wedged each look's name read takes its
  # 5 s timeout (bt_responsive), so the attempt is bounded by the uptime (_bal_ms, 2.7.0's RADIO_ATTACH_POLLS looks of
  # 0.5 s: 15 s), not by a count of looks, which would make it up to five times 2.7.0's under the bt lock (the 2.7.1
  # review). With no uptime to read, five times the looks, as before. Off: 2.7.0's 0.5 s look, by its count.
  _bal_s=0.5; _bal_n=$RADIO_ATTACH_POLLS; _bal_ms=""
  if r4_on attach_poll; then
    _bal_s=0.1; _bal_n=$((RADIO_ATTACH_POLLS * 5)); _bal_ms=$((RADIO_ATTACH_POLLS * 500))
    [ -n "$_r4t" ] && log "${_r4t}hci0 looked at every 0.1 s as the attach converges, not 0.5 s"
  else
    log "[bench] round4: attach_poll=off ($_r4f): hci0 looked at every 0.5 s as the attach converges, as in 2.7.0"
  fi
  while [ "$_try" -lt "$1" ]; do
    _try=$((_try+1))
    if [ "$_try" -gt 1 ] || pgrep -x "$_helper" >/dev/null 2>&1 || ps | grep -v grep | grep -qw "$_helper"; then
      log "reclaim+reset (attempt $_try)"; bt_reclaim_reset
      # A push after each reset, before its attach (22 Nov 2026). With none (bt_fw_push: two failures and no push earlier
      # this boot) this attempt attaches nothing, and none follows: no further reset.
      bt_fw_push || { log "  attempt $_try attaches nothing: no firmware after its reset"; return 1; }
    fi
    _bt_tries=$((${_bt_tries:-0} + 1))
    log "attach attempt $_try: $RADIO_BT_ATTACH_CMD"
    boot_mark attach
    setsid $RADIO_BT_ATTACH_CMD </dev/null >/tmp/radio_bt_attach.log 2>&1 &
    _n=0; up_ms; _bal_t0=$_um
    while bt_attach_looks_on; do
      _d=$(hci_dev 2>/dev/null) && bt_responsive "$_d" && { _bal_ok=1; break; }
      _n=$((_n+1)); sleep "$_bal_s"
    done
    # One last look once the looks ran out. An answer above stands: two further name reads (a healthy RTL8822CS missed
    # about one in four, bt_responsive_confirmed) used to undo it, and a preload's one try then counted toward the
    # together guard, or a bt_on went on to reclaim+reset a working chip (the 21 Nov review J).
    if [ "$_bal_ok" = 0 ]; then
      _d=$(hci_dev 2>/dev/null) || _d=""
      [ -n "$_d" ] && bt_responsive "$_d" && _bal_ok=1
    fi
    [ "$_bal_ok" = 1 ] && break
    log "  attempt $_try did not converge"
  done
  if [ "$_bal_ok" = 1 ]; then
    boot_mark hci_up
    # A host waiting on the boot preload: its session takes the controller (the handover, above), with no SCO or name
    # set here; bt_handover_give hands it over once the Wi-Fi side is in, and publishes the state.
    if bt_handover_claim "$_d" "$_um"; then
      log "BT converged: $_d responsive on attempt $_try - claimed for the host's session"
      return 0
    fi
    bt_power_stamp
    # Round 4, item 6 (page_trim, on by default since 2.7.1), a host waiting on this preload: no SCO lines and no name set
    # here (0.39 s of hci_up to the first page). The host's session resets the controller before it uses it (btd's
    # DOWN/UP), and btd writes the name, class, Simple Pairing and EIR itself (bt_bringup.rs prepare); the vendor's SCO
    # set-up never outlived that reset (apply_sco), as the handover's claim already relies on. The name files only where
    # they change. Off (2.7.0's): both set, as with no host.
    _bal_trim=0
    if [ "$VERB" = preload ] && host_here; then
      if r4_on page_trim; then _bal_trim=1
      else log "[bench] round4: page_trim=off ($_r4f): a host is waiting - SCO lines and the name set on $_d, as in 2.7.0"; fi
    fi
    if [ "$_bal_trim" = 1 ]; then
      log "${_r4t}a host is waiting - no SCO lines and no name set on $_d (btd's bring-up resets the controller and writes the name)"
      bt_name_files
    else
      # Vendor SCO tuning where the mapping carried it; non-fatal. Was a HARDCODED
      # `scomtu 240:32` despite this comment -- which is the Realtek/NXP value and nobody
      # else's, and which skipped the routing command the NXP and BCM4358 branches need.
      # Now the extracted per-branch mapping.
      apply_sco "$_d" >/dev/null 2>&1 || log "no SCO mapping for this chipset"
      bt_set_name "$_d"
    fi
    publish "$BT_STATE" "result=ok" "backend=mapped" "chipset=$RADIO_SDIO_DEVICE" "hci_dev=$_d" \
            "bt_mac=$(cat /sys/class/bluetooth/$_d/address 2>/dev/null)"
    log "BT converged: $_d responsive on attempt $_try"
    return 0
  fi
  log "BT attach did not converge in 60s (device ${_d:-absent}); last log:"
  # radio_bt_attach.log stays a small `>`-truncated status file: it is written by a
  # setsid-backgrounded attach daemon, and piping that daemon's own stdout would change its
  # lifecycle. Only this readback (the tail below) is universal-logged, via `tee -a`.
  tail -5 /tmp/radio_bt_attach.log 2>/dev/null | sed "s/^/$LOGP   /" | tee -a /tmp/box.log
  publish "$BT_STATE" "result=fail" "backend=mapped" "chipset=$RADIO_SDIO_DEVICE" "detail=no-convergence"
  return 1
}

# Today's order, $1 tries: the Wi-Fi driver first where this unit's attach waits for it, then the line discipline, the
# firmware, the attach.
bt_attach_mapped() {
  # Ordering constraint, discovered not assumed: some parts wedge if BT attaches before the
  # WLAN driver is up (the vendor encodes this as a wait loop inside its attach script, and
  # radio_detect.sh reports its presence). Honour it rather than re-deriving which chips care.
  # LOAD the WLAN driver, do not merely wait for it. The constraint these chips impose is
  # "the WLAN driver must be up before BT attaches" - on 0xc822 because attaching first can
  # make WiFi fail to start, on SD8987 because it hangs the chip outright, which is why the
  # vendor there GUARDS the attach on the interface existing rather than proceeding.
  #
  # The previous version only polled for an interface and then attached anyway, quoting the
  # vendor's own timeout behaviour. That excuse does not survive a bt_on with no wifi_ap_on
  # before it: nothing loads the driver and the poll is guaranteed to time out - we would
  # attach into the exact ordering the constraint exists to forbid.
  #
  # Loading the driver is not an AP: no addressing, no hostapd. If it cannot be loaded we fail
  # honestly rather than wedge the chip.
  if [ "${RADIO_BT_AFTER_WLAN:-0}" = 1 ] && ! wlan_iface >/dev/null 2>&1; then
    log "chipset requires the WLAN driver before BT attach - loading it (driver only, no AP)"
    if wlan_driver_up; then
      _n=0; while [ "$_n" -lt 120 ]; do wlan_iface >/dev/null 2>&1 && break; _n=$((_n+1)); sleep 0.1; done
    fi
    if ! wlan_iface >/dev/null 2>&1; then
      log "  WLAN interface never appeared - refusing to attach BT into a documented wedge"
      publish "$BT_STATE" "result=fail" "backend=mapped" "chipset=$RADIO_SDIO_DEVICE" \
              "detail=wlan-required-but-unavailable"
      return 1
    fi
    [ "${_wdl_loaded:-0}" = 1 ] && boot_mark wlan_up
    log "  WLAN interface $(wlan_iface) up - BT attach may proceed"
  fi

  ko_unpack || log "driver tarball ${RADIO_KO_TARBALL:-<none>} did not unpack"
  bt_ldisc_ready
  # No firmware pushed (bt_fw_push, twice): no attach, so none of the reclaim+reset tries either.
  if bt_fw_push; then
    bt_attach_loop "$1"; _bam_rc=$?
  else
    _bam_rc=1
  fi
  bt_handover_give   # nothing left to join: wlan_first brought the Wi-Fi interface up first, bt_alone brings none
  return $_bam_rc
}

# Together (the IW416, RADIO_BT_TOGETHER=1): the Wi-Fi driver loads in the background while the firmware is pushed and
# the controller attached, and the attach does not wait for its interface. One attempt: its failure is the fallback.
bt_attach_together() {
  log "radio order: together - the Bluetooth firmware beside the Wi-Fi driver, attached without waiting for its interface (this unit's own attach branch waits for nothing; Wi-Fi first again with $TOGETHER_OFF or $TOGETHER_BENCH_OFF)"
  # A mark that cannot be written (a full flash) never stops the attach: it goes on, and were it never to finish it
  # would go uncounted (the guard, above).
  if write_empty "$TOGETHER_MARK"; then sync
  else log "radio order: the in-flight mark could not be written ($TOGETHER_MARK) - this together attach goes on, uncounted by the guard should it never finish"; fi
  ko_unpack || log "driver tarball ${RADIO_KO_TARBALL:-<none>} did not unpack"
  # The line discipline first, then both loads at one instant, as the bench ran them (bt_parallel.sh, 20 Nov).
  bt_ldisc_ready
  # The Wi-Fi driver under its own lock (wlan_driver_up), its interface waited for, in a subshell of its own. Once the
  # interface is there a preload lets its wlan lock go, while this one still holds Bluetooth: a host's AP bring-up
  # waiting on it starts beside the firmware load, as on today's path when a host comes before the preload.
  # Its exit code: 0 the interface came, 1 it did not; 2 more when it let the preload's wlan lock go (lock_mine).
  (
    _btw_rc=1; _btw_freed=0
    if wlan_driver_up; then
      _n=0; while [ "$_n" -lt 120 ]; do wlan_iface >/dev/null 2>&1 && break; _n=$((_n+1)); sleep 0.1; done
      if _btw_i=$(wlan_iface 2>/dev/null); then
        _btw_rc=0
        [ "${_wdl_loaded:-0}" = 1 ] && boot_mark wlan_up
        log "  the Wi-Fi driver is up ($_btw_i), beside the Bluetooth firmware"
      else
        log "  the Wi-Fi driver gave no interface"
      fi
    fi
    if [ "$VERB" = preload ]; then
      _btw_h=""; read -r _btw_h 2>/dev/null </tmp/.radio_lock_wlan/pid
      if [ "$_btw_h" = "$$" ]; then
        rm -rf /tmp/.radio_lock_wlan 2>/dev/null; _btw_freed=1
        boot_mark wlan_free
        log "  the preload lets the wlan lock go: an AP bring-up may start"
      fi
    fi
    exit $((_btw_rc + 2 * _btw_freed))
  ) &
  _btw_job=$!
  # No firmware pushed (bt_fw_push, twice): no attach, and the fallback, its mark and the guard's count as for any
  # together attach that gave no responsive hci0 (below).
  if bt_fw_push; then
    bt_attach_loop 1; _btw_bt=$?
  else
    _btw_bt=1
  fi
  # Clause 1: nothing this verb started still in flight when it returns. The Wi-Fi driver has been the shorter of the
  # two (2.0 s against 3.24 on the bench), so this rarely waits.
  wait "$_btw_job"; _btw_wl=$?
  # The wlan lock that subshell let go is no longer this process's: a later holder's must stay (lock_drop_own).
  case "$_btw_wl" in 2|3) lock_mine wlan 0; _btw_wl=$((_btw_wl - 2)) ;; 0|1) ;; *) _btw_wl=1 ;; esac
  # A controller claimed for a waiting host goes to its session now that the Wi-Fi interface is in too (btd's name).
  bt_handover_give
  if [ "$_btw_bt" = 0 ]; then
    if [ "$_btw_wl" = 0 ]; then
      # Round 4, item 6 (page_trim, on by default since 2.7.1), a host waiting on this preload: the guard's mark and count
      # go once the locks have (do_preload), so their flash sync is off the path the host's bt_on waits on. A power cut
      # in between counts this attach as unfinished: more careful, never less. Off (2.7.0's): cleared here.
      if [ "$VERB" = preload ] && host_here && r4_on page_trim; then
        _bt_mark_late=1
      else
        rm -f "$TOGETHER_MARK" "$TOGETHER_FAILS"; sync
      fi
    else
      together_count "gave Bluetooth but no Wi-Fi interface" || :
    fi
    return 0
  fi
  write_empty "$TOGETHER_FELL" || log "radio order: the fallback mark could not be written ($TOGETHER_FELL) - a later attach this boot may try together again"
  log "radio order: together gave no responsive hci0 - this boot's next attach is Wi-Fi first"
  together_count "gave no responsive hci0" || :
  return 1
}

do_bt_on() {
  _bt_attached=0   # set once this bring-up attaches (the mapped path): its caller then writes the boot line
  caps || { publish "$BT_STATE" "result=unsupported" "detail=no-descriptor"; return 3; }
  # A controller the boot preload handed to this host's session (the handover, above): nothing to bring up, and nothing
  # touched, since btd may be paging on it already. bt_power again with hci0's answer (the first stands: this one is
  # for a record btd began after the preload's stamp). "opened" for a btd that goes on this verb's return.
  if [ "$VERB" = bt_on ] && bt_handed; then
    log "handed over by the boot preload: $_bhd_dev answered at uptime $_bhd_hu ms and is this host's session's since $_bhd_at ms - nothing to bring up, nothing touched"
    case "$_bhd_hu" in ''|*[!0-9]*) ;; *) echo "t bt_power $_bhd_hu" 2>/dev/null >>/tmp/startup_marks ;; esac
    echo opened 2>/dev/null >"$BT_WARM"
    return 2
  fi
  # Whether this bring-up found the controller warm (attached, at most powered down): btd then pages at once, with no
  # settle, since nothing was just attached (the Mac's 13 Nov connect-time brief). A full attach or a failure leaves none.
  rm -f "$BT_WARM"

  _d=$(hci_dev 2>/dev/null) || _d=""
  # Only a POWERED-UP adapter can pass the confirmation: on a DOWN one every name read fails
  # instantly with ENETDOWN, so the confirmation's 5 tries x 2 s sleeps were 8 s of pure waiting
  # before the power-up branch below - paid on every session that starts from the supervisor's
  # between-sessions state (measured 2026-10-20: bt_on 9.5 s from DOWN vs 0.4 s for the power-up).
  if [ -n "$_d" ] && bt_powered_up "$_d" && bt_responsive_confirmed "$_d"; then
    bt_power_stamp
    log "already converged ($_d responsive)"
    write_empty "$BT_WARM"
    publish "$BT_STATE" "result=ok" "backend=$RADIO_BACKEND" "hci_dev=$_d" \
            "bt_mac=$(cat /sys/class/bluetooth/$_d/address 2>/dev/null)"
    return 2
  fi

  # POWER IT UP BEFORE CONCLUDING IT IS WEDGED. The supervisor's deliberate teardown end state is
  # "attached but powered down" (`hciconfig <dev> down`) - chosen precisely because re-attach is
  # the wedge-prone half. But a name read on a DOWN adapter fails immediately with ENETDOWN: it
  # never reaches the chip. So the check above misses on every teardown, and the code below would
  # then run the full destructive recovery - kill the helper, rmmod the line discipline, toggle
  # the reset GPIO, re-attach - against a perfectly healthy controller, once per session cycle,
  # when a single `hciconfig up` was the entire fix. Today's testing was all cold bring-up, which
  # never exercises this path.
  if [ -n "$_d" ]; then
    _bt_was_down=1
    if bt_powered_up "$_d"; then
      log "$_d present but not answering - trying to power it up before assuming it is wedged"
      _bt_was_down=0
    else
      log "$_d powered down (between-sessions state) - powering it up"
    fi
    hci_rt "$_d" up 2>/dev/null
    if bt_responsive_confirmed "$_d"; then
      bt_power_stamp
      log "$_d was only powered down - up and responsive, no reset needed"
      # "opened": this `up` opened a DOWN device, so the kernel set its event mask just now, and btd's first bring-up
      # can skip its own down/up (bt_bringup.rs; the Mac's 13 Nov brief, item 1).
      if [ "$_bt_was_down" = 1 ]; then echo opened > "$BT_WARM"; else write_empty "$BT_WARM"; fi
      publish "$BT_STATE" "result=ok" "backend=$RADIO_BACKEND" "hci_dev=$_d" \
              "bt_mac=$(cat /sys/class/bluetooth/$_d/address 2>/dev/null)"
      return 0
    fi
    log "$_d still unresponsive after power-up - recovery is warranted"
  fi

  case "$RADIO_BACKEND" in
    owned)
      log "backend=owned -> /script/bt_on.sh"
      sh /script/bt_on.sh; _rc=$?
      _d=$(hci_dev 2>/dev/null) || _d=""
      if [ -n "$_d" ] && bt_responsive_confirmed "$_d"; then
        bt_power_stamp
        publish "$BT_STATE" "result=ok" "backend=owned" "hci_dev=$_d" \
                "bt_mac=$(cat /sys/class/bluetooth/$_d/address 2>/dev/null)"
        return 0
      fi
      publish "$BT_STATE" "result=fail" "backend=owned" "detail=bt_on.sh rc=$_rc, not responsive"
      return 1
      ;;
    mapped)
      [ -n "$RADIO_BT_ATTACH_CMD" ] || {
        log "no attach mapping could be extracted for chipset $RADIO_SDIO_DEVICE"
        publish "$BT_STATE" "result=unsupported" "chipset=$RADIO_SDIO_DEVICE" "detail=no-mapping"
        return 3; }
      _bt_attached=1
      boot_mark start
      bt_order
      boot_note order "$_bto"; boot_note off "$_bto_off"
      handover_state; boot_note handover "$_hov"   # asked for or not, as the attach starts (the boot line)
      case "$_bto_off" in
        owner)       log "radio order: Wi-Fi first - together is switched off by the owner ($TOGETHER_OFF)" ;;
        guard)       log "radio order: Wi-Fi first - together is switched off by its guard ($TOGETHER_OFF: $_bto_l)" ;;
        bench)       log "radio order: Wi-Fi first - together is switched off for the bench ($TOGETHER_BENCH_OFF)" ;;
        fallback)    log "radio order: Wi-Fi first - a together bring-up this boot gave no responsive hci0 (the fallback)" ;;
      esac
      if [ "$_bto" = together ]; then
        bt_attach_together && return 0
        # THE FALLBACK. The tries left (bt_on has four, the boot preload one) take today's order, from its start: the
        # together attempt's helper stopped so the UART is free (no reset, no rmmod), the Wi-Fi interface first, the
        # firmware pushed again, then the attach, whose attempt 1 finds no helper and is a plain attach. A preload's
        # failure leaves it to the host's bt_on, which the boot's mark sends Wi-Fi first: the preload stops its helper
        # as it ends (do_preload), and that bt_on stops any it still finds before its firmware push (below).
        _bt_left=$(( ${RADIO_BT_MAX_TRIES:-4} - 1 ))
        [ "$_bt_left" -ge 1 ] || return 1
        boot_note fallback 1
        log "radio order: Wi-Fi first for the $_bt_left tries left (the fallback)"
        bt_helper_stop
        bt_attach_mapped "$_bt_left"
        return $?
      fi
      # The fallback from an earlier bring-up (21 Nov review A): this boot's together attach failed in another process
      # (the boot preload), whose helper, were it still there, would hold the UART. Stopped first, as the fallback
      # above does (no reset, no rmmod), so the firmware push reaches the chip and attempt 1 is a plain attach.
      [ "$_bto_off" = fallback ] && bt_helper_stop
      bt_attach_mapped "${RADIO_BT_MAX_TRIES:-4}"
      return $?
      ;;
    *)
      log "NO BACKEND for bt on this unit - wireless unavailable (wired projection unaffected)"
      publish "$BT_STATE" "result=unsupported" "chipset=${RADIO_SDIO_DEVICE:-unknown}" "detail=no-backend"
      return 3
      ;;
  esac
}

do_bt_off() {
  # End state: powered down but STILL ATTACHED. Never rmmod the line discipline and never -9 the
  # attach helper - re-attach is the wedge-prone half, which is why the supervisor stops at
  # `hciconfig down` rather than calling bt_off.sh.
  rm -f "$HANDOVER"   # a handover was for the session this ends
  _d=$(hci_dev 2>/dev/null) || { rm -f "$BT_STATE"; return 0; }
  # Raw Write_Scan_Enable(0x00), not `hciconfig noscan`: that ioctl can sleep its full 10 s
  # timeout on the CCPA's 3.14 kernel (lost wake-up, btd's hci::write_scan_enable), and this
  # runs under the bt lock, so the next session's bt_on would wait behind it.
  timeout 5 hcitool -i "$_d" cmd 0x03 0x001a 0x00 >/dev/null 2>&1
  # End every ACL with a real HCI Disconnect before powering down. `down` resets the controller,
  # and a reset is not a disconnect: the phone is not told the link ended and may only learn it
  # from its supervision timeout. Once this teardown actually ran (2026-10-20), the next session's
  # first page failed in 3 of 8 reconnects, each costing the reconnect loop's 10 s backoff.
  for _a in $(timeout 3 hcitool -i "$_d" con 2>/dev/null | awk '/ACL/ {print $3}'); do
    timeout 3 hcitool -i "$_d" dc "$_a" 2>/dev/null && log "BT disconnected $_a before power-down"
  done
  hci_rt "$_d" down   2>/dev/null
  rm -f "$BT_STATE"
  log "BT $_d powered down (attach left in place)"
  return 0
}

# =============================================================================================
[ "${RADIO_HAL_FUNCS:-}" = 1 ] && return 0   # sourced for its functions (tools/radio_ap_up_test.sh)
case "$VERB" in
  probe)
    caps || exit 3
    echo "chipset=$RADIO_SDIO_DEVICE vendor=$RADIO_SDIO_VENDOR chip=$RADIO_CHIP backend=$RADIO_BACKEND wireless=$RADIO_WIRELESS"
    echo "wlan_insmod=\"$RADIO_WLAN_INSMOD\" bt_attach=\"$RADIO_BT_ATTACH_CMD\" bt_after_wlan=$RADIO_BT_AFTER_WLAN bt_together=$RADIO_BT_TOGETHER"
    # The order the next attach would take, the descriptor's source (the kept copy, or a detection), and whether a boot
    # preload would hand Bluetooth to a waiting host: on_owner or on_bench (asked for by that file), or off (the default).
    bt_order peek
    handover_state
    echo "order=$_bto off=$_bto_off descriptor=${RADIO_CAPS_FROM:-unknown} handover=$_hov"
    [ "$RADIO_WIRELESS" = yes ] && exit 0 || exit 3 ;;
  status)
    caps || exit 3
    _i=$(wlan_iface 2>/dev/null) || _i=""
    _d=$(hci_dev 2>/dev/null)    || _d=""
    # Three-valued per radio: absent (no driver) / down / up. A cold box is "absent", which is
    # normal on this platform - radios load on demand - and must never read as failure.
    echo "wlan=$([ -z "$_i" ] && echo absent || { ap_running && echo up || echo down; }) iface=${_i:-none}"
    echo "bt=$([ -z "$_d" ] && echo absent || { bt_responsive_confirmed "$_d" && echo up || echo down; }) hci=${_d:-none}"
    [ -f "$WLAN_STATE" ] && sed 's/^/wlan_state: /' "$WLAN_STATE"
    [ -f "$BT_STATE" ]   && sed 's/^/bt_state:   /' "$BT_STATE"
    exit 0 ;;
  wifi_ap_on)  lock_take wlan || exit 1; do_wifi_ap_on;  _r=$?; lock_drop wlan; exit $_r ;;
  wifi_ap_off) lock_take wlan || exit 1; do_wifi_ap_off; _r=$?; lock_drop wlan; exit $_r ;;
  wifi_ap_prep) lock_take wlan || exit 1; do_wifi_ap_prep; _r=$?; lock_drop wlan; exit $_r ;;
  bt_on)       lock_take bt   || exit 1; do_bt_on;       _r=$?; lock_drop bt
               [ "${_bt_attached:-0}" = 1 ] && boot_line bt_on "$_r"   # a bt_on that attached (no preload did)
               exit $_r ;;
  bt_off)      lock_take bt   || exit 1; do_bt_off;      _r=$?; lock_drop bt;   exit $_r ;;
  # The Wi-Fi link's station (wifi_sta.sh): the WLAN driver loaded and the station's interface up, nothing else (no AP,
  # no address). Under the wlan lock, so it never meets a session's wifi_ap_on or the boot preload.
  wifi_sta_up) lock_take wlan || exit 1; do_wifi_sta_up; _r=$?; lock_drop wlan; exit $_r ;;
  # M3's set-up network (ocbmd src/setup.rs), under the wlan lock like the CarPlay AP.
  setup_ap_on)  lock_take wlan || exit 1; do_setup_ap_on;  _r=$?; lock_drop wlan; exit $_r ;;
  setup_ap_off) lock_take wlan || exit 1; do_setup_ap_off; _r=$?; lock_drop wlan; exit $_r ;;
  # Both locks, wlan first (the supervisor's own order, one at a time, so no deadlock): bt_on loads
  # the WLAN driver, which an app-driven wifi_ap_on arriving mid-preload must not race. Each lock may have gone
  # earlier (do_preload, and the Wi-Fi side of a together attach), so each goes here only while it is still this one's.
  preload)     lock_take wlan || exit 1; lock_take bt || { lock_drop wlan; exit 1; }
               do_preload; _r=$?; trap - EXIT INT TERM HUP; lock_drop_own bt; lock_drop_own wlan; exit $_r ;;
  *) echo "$LOGP usage: radio_hal.sh probe|status|wifi_ap_on|wifi_ap_off|wifi_ap_prep|wifi_sta_up|setup_ap_on|setup_ap_off|bt_on|bt_off|preload" >&2; exit 64 ;;
esac
