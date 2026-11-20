#!/bin/sh
########################
# radio_detect.sh - resolve THIS unit's WLAN/BT platform, read-only, and write a sourceable
# descriptor to /tmp/radio_caps. Run at boot before anything wants a radio, and again after a
# driver load (the post-load facts are the ones nobody can predict).
#
# WHY THIS EXISTS. CCPA ships at least six WLAN/BT parts (RTL8822BS/CS, RTL8733BS, BCM4354/4335,
# BCM4358, NXP SD8987, NXP IW416) and - per the resources repo's
# 01_Firmware_Architecture/device_variants_and_conversion.md - ONLY the driver tarball for a
# unit's own chip ships in its rootfs. There is no fallback set. So every layer above this one
# has to stop hardcoding one chip's names and paths, and instead ask.
#
# THE RULE THIS SCRIPT OBEYS (same as tools/ncm_base_install.sh): never branch on a chipset
# whitelist for BEHAVIOUR - an unlisted variant falls off the end of one and gets nothing. The
# chip-name table below is for HUMANS and logs only; nothing acts on it. Behaviour is derived
# from what the unit itself carries:
#
#   the SDIO id          - hardware ground truth, readable before any driver loads
#   /lib/firmware/<tree> - exactly one tree per unit, so the tree names the vendor
#   the attach helper    - rtk_hciattach XOR brcm_patchram_plus XOR hciattach+fw_loader_linux
#   /script/init_bluetooth_wifi.sh - the VENDOR's own per-SDIO-id dispatcher, which every unit
#                          ships and which knows that unit's exact insmod/attach lines
#
# The vendor dispatcher is the authority for the bottom half (load the driver, attach the
# controller) precisely because it encodes per-chip knowledge we cannot re-derive - e.g. on
# 0xc822 and SD8987 the BT attach must wait for wlan0 or the chip wedges. We own everything
# ABOVE the driver; see radio_hal.sh for why delegating the vendor's AP/network half is not
# merely inadequate but destructive on a stripped box.
#
# DO NOT make this script mutate anything. It must be safe to run at any time, in any state,
# including from a diagnostic shell mid-session. Loading a driver to "find out" is exactly the
# side effect callers rely on not happening.
########################
set -u

# ---- 0. the key a kept copy of this descriptor is checked against (20 Nov 2026) -----------------
# radio_hal.sh keeps the descriptor on flash (/etc/ocbm/kept_radio_caps) and, at a later boot, reuses it instead of
# running this script (0.75 to 0.8 s on the IW416, timed by hand on the dev adapter) while this key still reads what it
# read when the copy was made. The key is everything below reads, so A NEW INPUT BELOW MUST BE A NEW TERM HERE:
#   sdio=   the SDIO ids (vendor/device/class) of the first SDIO function, as section 1 finds it;
#   fw=     the firmware trees under /lib/firmware (section 2 picks one of them);
#   has=    which attach and preload helpers (section 3) and which bring-up scripts (section 4) are there;
#   gates=  each file test the dispatcher's gates make on this unit (`test -e PATH && var=1 || var=0`, resolve_var);
#   sums=   one md5 over the md5sum of every file whose text or members this script reads (each file's name and sum):
#           itself, the dispatcher, the attach script, and the driver tarballs (their member list names the modules).
#           One hash and not each file's, so the key stays short: btd logs the whole descriptor as one line, and ocbmd
#           clips a line at 1024 bytes (the truck's was 980 before this).
# The post-load facts (section 5) are no input: they describe the boot they were read in, and a kept copy has none.
# Built from the shell's own reads and globs plus one grep and one md5sum: a small part of a detection's cost.
# A key with a character a descriptor cannot carry is "unkeepable", and radio_hal.sh keeps nothing then.
# radio_hal.sh sources this file for the function alone (RADIO_DETECT_FUNCS=1), in a subshell.
radio_detect_key() {
  _rdk_sdio=none
  for _rdk_d in /sys/bus/sdio/devices/*/; do
    [ -e "$_rdk_d/device" ] || continue
    _rdk_v=""; _rdk_dv=""; _rdk_c=""
    read -r _rdk_dv 2>/dev/null <"$_rdk_d/device"
    read -r _rdk_v 2>/dev/null <"$_rdk_d/vendor"
    read -r _rdk_c 2>/dev/null <"$_rdk_d/class"
    _rdk_sdio="${_rdk_v:-}/${_rdk_dv:-}/${_rdk_c:-}"; break
  done
  _rdk_fw=""
  for _rdk_t in /lib/firmware/*/; do [ -d "$_rdk_t" ] && { _rdk_t=${_rdk_t%/}; _rdk_fw="$_rdk_fw,${_rdk_t##*/}"; }; done
  _rdk_has=""
  for _rdk_f in /usr/sbin/rtk_hciattach /usr/sbin/brcm_patchram_plus /usr/sbin/hciattach /usr/sbin/fw_loader_linux; do
    if [ -x "$_rdk_f" ]; then _rdk_has="${_rdk_has}1"; else _rdk_has="${_rdk_has}0"; fi
  done
  for _rdk_f in /script/wlan_on.sh /script/bt_on.sh /script/init_bluetooth_wifi.sh /script/attach_bluetooth.sh; do
    if [ -f "$_rdk_f" ]; then _rdk_has="${_rdk_has}1"; else _rdk_has="${_rdk_has}0"; fi
  done
  # Only the gate shape resolve_var reads: the dispatcher's other `test -e` lines name /tmp files that come and go.
  _rdk_gates=""; _rdk_next=0
  for _rdk_w in $(grep -oE 'test -e [^[:space:]]+ && [A-Za-z_][A-Za-z0-9_]*=1 \|\| [A-Za-z_][A-Za-z0-9_]*=0' \
                  /script/init_bluetooth_wifi.sh 2>/dev/null); do
    if [ "$_rdk_next" = 1 ]; then
      if [ -e "$_rdk_w" ]; then _rdk_gates="$_rdk_gates,$_rdk_w=1"; else _rdk_gates="$_rdk_gates,$_rdk_w=0"; fi
    fi
    _rdk_next=0; [ "$_rdk_w" = -e ] && _rdk_next=1
  done
  _rdk_sums=$(md5sum /script/radio_detect.sh /script/init_bluetooth_wifi.sh /script/attach_bluetooth.sh \
              /lib/firmware/*/*_ko.tar.gz 2>/dev/null | md5sum 2>/dev/null)
  set -f; set -- $_rdk_sums; set +f
  _rdk_key="v1 sdio=$_rdk_sdio fw=${_rdk_fw#,} has=$_rdk_has gates=${_rdk_gates#,} sums=${1:-}"
  case "$_rdk_key" in
    *[!A-Za-z0-9._/:=,+\ -]*) echo unkeepable ;;
    *) echo "$_rdk_key" ;;
  esac
}
[ "${RADIO_DETECT_FUNCS:-}" = 1 ] && return 0
OUT=${RADIO_CAPS:-/tmp/radio_caps}
# Taken before anything below reads its inputs: one that changes during this run then reads as a change later.
DETECT_KEY=$(radio_detect_key)

# ---- 1. hardware identity -----------------------------------------------------------------
# GLOB the sdio device node. The vendor dispatcher hardcodes mmc0:0001:1 (init_bluetooth_wifi.sh
# line 21); that path is an enumeration accident, not a guarantee, and a unit that enumerates
# differently would silently read nothing and fall into the "unknown chip" branch.
SDIO_NODE=""
for d in /sys/bus/sdio/devices/*/; do [ -e "$d/device" ] && { SDIO_NODE="${d%/}"; break; }; done
SDIO_DEV=""; SDIO_VEN=""; SDIO_CLS=""
if [ -n "$SDIO_NODE" ]; then
  SDIO_DEV=$(cat "$SDIO_NODE/device" 2>/dev/null)
  SDIO_VEN=$(cat "$SDIO_NODE/vendor" 2>/dev/null)
  SDIO_CLS=$(cat "$SDIO_NODE/class"  2>/dev/null)
fi

# Human-readable only. NOTHING below branches on CHIP - if this says "unknown" the unit still
# works, because every decision after this point comes from the filesystem, not this table.
case "$SDIO_DEV" in
  0xb822) CHIP=realtek_rtl8822bs ;;
  0xc822) CHIP=realtek_rtl8822cs ;;
  0xb733) CHIP=realtek_rtl8733bs ;;
  0x4354|0x4335) CHIP=broadcom_bcm4354 ;;
  0x4358|0xaa31) CHIP=broadcom_bcm4358 ;;
  0x9149|0x9141) CHIP=nxp_sd8987 ;;
  0x9159) CHIP=nxp_iw416 ;;
  "")     CHIP=no_sdio_radio ;;
  *)      CHIP=unknown ;;
esac

# ---- 2. the unit's own support set ---------------------------------------------------------
# Exactly one firmware tree ships per unit, so its name identifies the vendor without a table.
FW_TREE=""
for t in /lib/firmware/*/; do
  [ -d "$t" ] || continue
  case "${t%/}" in */rtlbt|*/nxp|*/bcm) FW_TREE="${t%/}"; break ;; esac
done
[ -n "$FW_TREE" ] || { for t in /lib/firmware/*/; do [ -d "$t" ] && { FW_TREE="${t%/}"; break; }; done; }
KO_TARBALL=$(ls "$FW_TREE"/*_ko.tar.gz 2>/dev/null | head -1)

# ---- 3. BT attach helper -------------------------------------------------------------------
# Most-specific first. Helper PRESENCE is discriminating; general binary presence is NOT - this
# Realtek unit ships /usr/sbin/wl (a Broadcom-flavoured tool) and supplicant configs with no
# supplicant binary, so never infer the chip from a non-attach binary.
BT_ATTACH=""
for h in rtk_hciattach brcm_patchram_plus hciattach; do
  [ -x "/usr/sbin/$h" ] && { BT_ATTACH=$h; break; }
done
BT_PRELOAD=""
[ -x /usr/sbin/fw_loader_linux ] && BT_PRELOAD=fw_loader_linux

# ---- 4. which bring-up path this unit actually carries -------------------------------------
# Resolution order, and the reason for it:
#   owned  - this unit got the project's own scripts (the IW416 baseline). Proven, keep using.
#   vendor - the unit's factory dispatcher survived the strip (ncm_base_install.sh's is_radio()
#            guard protects it by explicit basename). It is the only on-unit-correct source for
#            five of the six variants.
#   none   - no path. Report it loudly; wireless is unavailable. WIRED projection is unaffected,
#            it never touches a radio.
BACKEND=none
if [ -f /script/wlan_on.sh ] && [ -f /script/bt_on.sh ]; then
  BACKEND=owned
elif [ -f /script/init_bluetooth_wifi.sh ] && [ -f /script/attach_bluetooth.sh ]; then
  BACKEND=mapped
fi
# load_bluetooth_wifi.sh is deliberately NOT accepted as a dispatcher: despite the generic name
# it is Realtek-only (hardcodes rtk_hciattach and 88x2bs.ko), so treating it as one would hand a
# Broadcom unit a Realtek bring-up.

WIRELESS=no
[ "$BACKEND" != none ] && [ -n "$KO_TARBALL" ] && WIRELESS=yes

# ---- 4b. ADOPT THE VENDOR'S MAPPING, NOT THEIR MECHANISM -----------------------------------
# BACKEND=mapped means: this unit's own dispatcher is the authority for WHAT to run, and we
# extract those command lines here. It does NOT mean we execute the dispatcher. Running it
# wholesale would re-import the very defects the owned scripts exist to fix:
#   - it forks BT attach and returns (`/script/attach_bluetooth.sh &`), which is exactly the
#     uncoordinated double-bring-up docs/wireless/01_BT_AND_RADIO.md recorded fighting itself for 7+ minutes;
#   - the Broadcom branches background brcm_patchram_plus the same way;
#   - the SD8987 branch can `reboot` the box from inside a radio bring-up, bypassing the
#     supervisor's reboot budget;
#   - it untars overlays into `/` and its BT health check is object-existence, which a dead
#     chip passes (docs/wireless/01_BT_AND_RADIO.md again).
# The per-chip command lines are the vendor's OBSERVATIONS - fleet-deployed and working - so we
# take them verbatim. The surrounding control flow is their CHOICE, and we do not.
#
# The extraction carries no chipset table: we intersect the modules in THIS unit's own tarball
# with the dispatcher's own insmod lines. A part nobody anticipated still resolves, because the
# unit ships a dispatcher that knows its own silicon and a tarball that names its own modules.
# REFUSE TO EMIT WHAT WE CANNOT FAITHFULLY EXECUTE. Single-line extraction only works while a
# vendor branch is closed-form. The Realtek branches are; the NXP and Broadcom ones are NOT -
# they carry shell variables resolved elsewhere in the dispatcher:
#     insmod /tmp/moal.ko "mod_para=$nxpWiFiConfig"
#     brcm_patchram_plus --patchram /lib/firmware/bcm/$bcmBTFirmware ... --bd_addr "$bcmBTMac" &
# Captured verbatim, those are not commands, they are text that happens to look like one. Worse,
# writing them into the descriptor makes `. /tmp/radio_caps` abort under `set -u` - status 2,
# which radio_hal defines as "already converged". A mapping we cannot execute must therefore
# become NO mapping, so the seam reports "unsupported" honestly and the caller can act on it.
# A trailing `&` is stripped rather than rejected: backgrounding is the vendor's control-flow
# choice, and radio_hal supplies its own detached-and-converged discipline in its place.
safe_cmd() {  # stdin -> stdout, empty if the line cannot be executed as captured
  _c=$(cat)
  _c=$(echo "$_c" | sed 's/[[:space:]]*&[[:space:]]*$//')          # vendor's backgrounding
  case "$_c" in
    *'$'*|*'`'*|*'"'*|*"'"*) echo "" ;;   # unresolved substitution or quoting we cannot honour
    *) echo "$_c" ;;
  esac
}

BT_LDISC=""; WLAN_MODS=""
for m in $(tar tzf "$KO_TARBALL" 2>/dev/null | sed 's|.*/||' | grep '\.ko$'); do
  case "$m" in *hci_uart*) BT_LDISC=$m ;; *) WLAN_MODS="$WLAN_MODS $m" ;; esac
done
WLAN_MODS=${WLAN_MODS# }

# RESOLVE A DISPATCHER VARIABLE FROM THE DISPATCHER ITSELF, SCOPED TO THIS UNIT'S OWN BRANCH.
# The NXP branches pass the module parameter through a variable:
#     nxpWiFiConfig=nxp/wifi_mod_para.conf                        (default, in this unit's branch)
#     if [ "$supportStaMode" -eq 1 ]; then nxpWiFiConfig=...alt   (gated alternative, same branch)
#     test -e /usr/sbin/wpa_supplicant && supportStaMode=1 || supportStaMode=0   (the gate, file-wide)
# and safe_cmd rightly refuses `insmod /tmp/moal.ko "mod_para=$nxpWiFiConfig"` as captured. Refusing
# it dropped moal.ko entirely on IW416/SD8987 units: mlan.ko half-loaded, no interface appeared, and
# because BT attach waits for WLAN on these parts, Bluetooth died with it (2026-10-18, IW416 unit).
# This resolves ONLY that closed shape: a literal default assigned in THIS unit's branch (selected by
# its SDIO id with the same awk as attach_branch below), an optional alternative gated on `-eq 1` of a
# variable whose own definition is a single `test -e PATH` one-liner, evaluated on this unit. Anything
# else resolves to nothing and the line still falls to safe_cmd, which still refuses it. No chipset
# table: every name, path and value comes from the unit's own dispatcher.
dispatch_branch() {  # $1 file -> the lines of this unit's own sdioCardID branch
  [ -f "$1" ] && [ -n "$SDIO_DEV" ] || return 0
  awk -v id="$SDIO_DEV" '
    !inb {
      if ($0 ~ /sdioCardID/ && index($0, "\"" id "\"") > 0 && $0 ~ /^[ \t]*(el)?if[ \t]/) { inb=1; depth=0 }
      next
    }
    {
      if (depth == 0 && $0 ~ /^[ \t]*(elif|else|fi)([ \t]|$)/) exit
      if ($0 ~ /^[ \t]*if[ \t]/) depth++
      else if ($0 ~ /^[ \t]*fi([ \t]|$)/) depth--
      print
    }' "$1"
}
resolve_var() {  # $1 var  $2 file -> literal value, or nothing
  _v=$1; _f=$2
  _br=$(dispatch_branch "$_f")
  [ -n "$_br" ] || return 0
  # Literal assignments of the variable inside this branch, in order: no $, quotes, spaces or operators.
  _as=$(echo "$_br" | grep -E "^[[:space:]]*$_v=[A-Za-z0-9_./-]+[[:space:]]*$" | sed "s/^[[:space:]]*$_v=//; s/[[:space:]]*$//")
  _dflt=$(echo "$_as" | sed -n 1p); [ -n "$_dflt" ] || return 0
  _alt=$(echo "$_as" | sed -n 2p)
  [ -z "$(echo "$_as" | sed -n 3p)" ] || return 0          # three or more shapes: not closed-form
  [ -n "$_alt" ] || { echo "$_dflt"; return 0; }
  # The alternative must sit behind `if [ "$gate" -eq 1 ]`, and the gate must be a one-line file test.
  # Quotes optional: Carlinkit's own 2025.10.15.1127 dispatcher writes `if [ $supportStaMode -eq 1 ]`
  # (lvalen's NCM image quotes it), and missing that dropped moal.ko on factory IW416 units.
  _gate=$(echo "$_br" | grep -oE 'if \[ "?\$[A-Za-z_][A-Za-z0-9_]*"? -eq 1 \]' | head -1 | sed 's/.*\$//; s/[" ].*//')
  [ -n "$_gate" ] || return 0
  _test=$(grep -oE "test -e [^[:space:]]+ && $_gate=1 \|\| $_gate=0" "$_f" | head -1 | awk '{print $3}')
  [ -n "$_test" ] || return 0
  if [ -e "$_test" ]; then echo "$_alt"; else echo "$_dflt"; fi
}
# Substitute each `$var` in a captured line with its resolved literal. If any variable does not
# resolve the line is passed through untouched, for safe_cmd to refuse. A double-quoted token that
# becomes a single space-free word loses its quotes: they only existed to protect the expansion.
resolve_line() {  # stdin -> stdout
  _l=$(cat)
  for _var in $(echo "$_l" | grep -oE '\$[A-Za-z_][A-Za-z0-9_]*' | tr -d '$' | sort -u); do
    _val=$(resolve_var "$_var" /script/init_bluetooth_wifi.sh)
    [ -n "$_val" ] || { echo "$_l"; return 0; }
    _l=$(echo "$_l" | sed "s|\\\$$_var|$_val|g")
  done
  echo "$_l" | sed 's/"\([^" ]*\)"/\1/g'
}

# Anchor on "insmod /tmp/" - the dispatcher's success/failure echo strings also contain the word
# insmod followed by the module name, and a greedy sed without this anchor picks the echo.
WLAN_INSMOD=""
for m in $WLAN_MODS; do
  l=$(grep -h "insmod /tmp/$m" /script/init_bluetooth_wifi.sh 2>/dev/null \
      | grep -v '^[[:space:]]*#' | head -1 \
      | sed 's|.*\(insmod /tmp/[^&|;}]*\).*|\1|' | sed 's/[[:space:]]*$//' | resolve_line | safe_cmd)
  [ -n "$l" ] && WLAN_INSMOD="$WLAN_INSMOD$l
"
done
# Attach helper invocation, anchored at line start so `if [ -e ... ]` tests and echo lines can
# never be mistaken for the command itself.
BT_ATTACH_CMD=""
[ -n "$BT_ATTACH" ] && BT_ATTACH_CMD=$(grep -hE "^[[:space:]]*$BT_ATTACH " /script/attach_bluetooth.sh 2>/dev/null \
                       | head -1 | sed 's/^[[:space:]]*//' | safe_cmd)
# Ordering constraint: some parts wedge if BT attaches before the WLAN driver is up. The vendor
# encodes this as a wait loop inside the attach script; its presence is the signal. Read off the
# WHOLE file, so any branch's loop sets it; section 4d reads this unit's own branch (BT_TOGETHER).
BT_AFTER_WLAN=0
grep -qE 'while \[ ! -e /sys/class/net/' /script/attach_bluetooth.sh 2>/dev/null && BT_AFTER_WLAN=1

# ---- 4c. THE SCO SETUP THIS UNIT'S OWN DISPATCHER APPLIES ------------------------------------
# attach_bluetooth.sh sets SCO up after its own attach. Those lines are the vendor's OBSERVATIONS,
# per-chip and not re-derivable by us, so they are extracted here exactly like the insmod/attach
# mapping and applied by `radio_hal.sh bt_on`, as the stock attach does.
#
# SCOPE THE EXTRACTION TO THIS UNIT'S OWN BRANCH. Unlike the insmod and attach lines -- which are
# discriminated by the module tarball and by attach-helper presence -- the SCO lines appear in
# SEVERAL of the dispatcher's chipset branches with DIFFERENT contents:
#     0xb822/0xc822/0xb733 (Realtek):  scomtu only, no routing command at all
#     0x4358/0xaa31 (BCM4358):         hcitool -i hci0 cmd 0x3f 0x1c 0x01 0x02 0x00 0x00 0x00
#     0x9149/0x9141/0x9159 (NXP):      hcitool -i hci0 cmd 0x3f 0x1d 0x00
# A whole-file `head -1` would hand one chip another chip's vendor-opaque HCI command -- exactly
# the failure the no-chipset-whitelist rule exists to prevent, arrived at from the other side. So
# `attach_branch` selects the branch by THIS UNIT'S OWN SDIO id against the dispatcher's own
# if/elif chain: no table, no whitelist, and an id the dispatcher does not mention simply yields
# NOTHING (the seam then reports `unsupported`, honestly). A single-branch attach script -- the
# owned IW416 rewrite, which has no sdioCardID dispatch at all -- is its own branch.
attach_branch() {
  [ -f /script/attach_bluetooth.sh ] || return 0
  if ! grep -q 'sdioCardID' /script/attach_bluetooth.sh 2>/dev/null; then
    cat /script/attach_bluetooth.sh; return 0
  fi
  [ -n "$SDIO_DEV" ] || return 0
  # Nesting-aware: the branch ends at the next elif/else/fi AT ITS OWN LEVEL. Counting matters --
  # the NXP branches close an inner `if [ $configBurned -eq 0 ]` BEFORE their scomtu line, so a
  # naive "stop at the first fi" would drop the very lines we came for.
  awk -v id="$SDIO_DEV" '
    !inb {
      if ($0 ~ /sdioCardID/ && index($0, "\"" id "\"") > 0 && $0 ~ /^[ \t]*(el)?if[ \t]/) { inb=1; depth=0 }
      next
    }
    {
      if (depth == 0 && $0 ~ /^[ \t]*(elif|else|fi)([ \t]|$)/) exit
      if ($0 ~ /^[ \t]*if[ \t]/) depth++
      else if ($0 ~ /^[ \t]*fi([ \t]|$)/) depth--
      print
    }' /script/attach_bluetooth.sh
}

# Strip a TRAILING comment before anything else. The owned rewrite annotates these lines inline
# (`hcitool -i hci0 cmd 0x3f 0x1d 0x00              # route SCO to HCI`) while the vendor puts the
# comment on the line above. `#` is only a comment marker in the SHELL'S INPUT -- the result of a
# variable expansion is never re-scanned for one -- so a captured trailing comment would reach
# hcitool as four extra positional arguments, not as a comment.
decomment() { sed 's/[[:space:]]*#.*$//; s/[[:space:]]*$//'; }

_SCO_BRANCH=$(attach_branch)
# Firmware PRELOAD invocation, extracted exactly like the attach line (this unit's own branch,
# anchored at line start, decommented, safe_cmd-filtered). The NXP parts push the BT firmware over
# the UART with fw_loader_linux BEFORE hciattach:
#     fw_loader_linux /dev/ttymxc2 115200 1 /lib/firmware/nxp/uartiw416_bt_v0.bin 3000000
# radio_hal.sh has consumed RADIO_BT_PRELOAD_CMD since the seam existed, but nothing ever emitted
# it, so the preload was silently skipped on every NXP unit. Measured cost on an IW416 (2026-10-19):
# hciattach at 3 Mbaud against a controller with no firmware never answers, bt_on falls into its
# reclaim+reset path, and the reset GPIO it toggles belongs to the WLAN/BT COMBO chip -- pulled
# under a live mlan/moal driver, that wedged the kernel hard (no NCM, no telnet, power cycle only).
# Units without a preload helper leave this empty and are unaffected.
BT_PRELOAD_CMD=""
[ -n "$BT_PRELOAD" ] && BT_PRELOAD_CMD=$(echo "$_SCO_BRANCH" \
  | grep -E "^[[:space:]]*$BT_PRELOAD " | head -1 | sed 's/^[[:space:]]*//' | decomment | safe_cmd)
BT_SCO_MTU_CMD=$(echo "$_SCO_BRANCH" \
  | grep -E '^[[:space:]]*hciconfig[[:space:]]+[^[:space:]]+[[:space:]]+scomtu[[:space:]]' \
  | head -1 | sed 's/^[[:space:]]*//' | decomment | safe_cmd)
# The routing command is identified by the VENDOR'S OWN annotation ("route sco data to hci"),
# same line or the line above -- never by matching raw hcitool opcodes, which would be a chipset
# table in disguise. An unannotated variant yields nothing and the seam says so.
BT_SCO_ROUTE_CMD=$(echo "$_SCO_BRANCH" | awk '
  {
    if ($0 ~ /^[ \t]*hcitool[ \t].*[ \t]cmd[ \t]/ \
        && (tolower($0) ~ /#.*sco/ || tolower(prev) ~ /^[ \t]*#.*sco/)) {
      sub(/^[ \t]+/, ""); print; exit
    }
    prev = $0
  }' | decomment | safe_cmd)

# ---- 4d. WHETHER THIS UNIT'S OWN BRANCH WAITS FOR THE WI-FI DRIVER (20 Nov 2026) ----------------
# BT_AFTER_WLAN is read off the whole attach script, so one branch's wait loop sets it for every unit. In Carlinkit's
# 2025.10.15.1127 attach script only the 0xc822 branch and the SD8987 one (0x9149/0x9141) wait for the Wi-Fi interface;
# the IW416 branch (0x9159) pushes its Bluetooth firmware with fw_loader_linux and attaches without waiting, and that
# unit's dispatcher starts the attach script in the background BEFORE it loads the Wi-Fi driver. So the stock firmware
# loads the two side by side on every IW416 boot, and the dev adapter's bench did the same (20 Nov: both loads started
# at one instant, hci0 answering 3.24 s on, the AP up after). Read per branch like the SCO lines above (attach_branch):
# BT_TOGETHER=1 where the whole file waits but this unit's own branch does not, and that branch pushes its own firmware
# (a preload line) before an attach this script could extract, on the mapped backend with a Wi-Fi mapping. radio_hal.sh
# then loads the Wi-Fi driver beside the Bluetooth firmware and attaches without waiting for the interface; everywhere
# else (0 here) it keeps today's order. No chipset table: an id the dispatcher never names has no branch, so 0, and the
# shape below selects the IW416 branch alone in every attach script this project has read.
BT_TOGETHER=0
if [ "$BT_AFTER_WLAN" = 1 ] && [ "$BACKEND" = mapped ] && [ -n "$BT_PRELOAD_CMD" ] && [ -n "$BT_ATTACH_CMD" ] \
   && [ -n "$WLAN_INSMOD" ] && [ -n "$_SCO_BRANCH" ] \
   && ! echo "$_SCO_BRANCH" | grep -qE 'while \[ ! -e /sys/class/net/'; then
  BT_TOGETHER=1
fi

# ---- 5. post-load facts: enumerate, never assume -------------------------------------------
# The WLAN interface name is an INSMOD PARAMETER that differs per chip - Realtek passes
# if2name=sta0, Broadcom iface_name=sta, and on Broadcom STA units wlan0 does not exist until
# `iw dev sta0 interface add wlan0` runs. So the name is discovered here, not declared upstream.
# A cfg80211/wext netdev is identifiable without knowing the driver: it has wireless/ or
# phy80211/ under its sysfs node.
WLAN_IF=""; WLAN_IFS=""
for n in /sys/class/net/*/; do
  [ -e "$n/wireless" ] || [ -e "$n/phy80211" ] || continue
  i=$(basename "$n"); WLAN_IFS="$WLAN_IFS $i"
  [ -n "$WLAN_IF" ] || WLAN_IF=$i
done
WLAN_IFS=${WLAN_IFS# }
WLAN_MAC=""
[ -n "$WLAN_IF" ] && WLAN_MAC=$(cat "/sys/class/net/$WLAN_IF/address" 2>/dev/null)

HCI_DEV=""
for b in /sys/class/bluetooth/hci*/; do [ -d "$b" ] && { HCI_DEV=$(basename "${b%/}"); break; }; done
BT_MAC=""
[ -n "$HCI_DEV" ] && BT_MAC=$(cat "/sys/class/bluetooth/$HCI_DEV/address" 2>/dev/null)

# ---- 6. emit ------------------------------------------------------------------------------
# Written via tmp+mv so a reader can never see a half-file. Consumers MUST treat a missing or
# malformed descriptor as "absent" and fall back to their compiled defaults, never crash on it.
{
  echo "RADIO_CHIP=$CHIP"
  echo "RADIO_CAPS_FROM=detected"
  echo "RADIO_SDIO_NODE=$SDIO_NODE"
  echo "RADIO_SDIO_VENDOR=$SDIO_VEN"
  echo "RADIO_SDIO_DEVICE=$SDIO_DEV"
  echo "RADIO_SDIO_CLASS=$SDIO_CLS"
  echo "RADIO_FW_TREE=$FW_TREE"
  echo "RADIO_KO_TARBALL=$KO_TARBALL"
  echo "RADIO_BT_ATTACH=$BT_ATTACH"
  echo "RADIO_BT_PRELOAD=$BT_PRELOAD"
  echo "RADIO_BT_UART=/dev/ttymxc2"
  echo "RADIO_BACKEND=$BACKEND"
  echo "RADIO_WLAN_MODULES=\"$WLAN_MODS\""
  echo "RADIO_BT_LDISC_KO=$BT_LDISC"
  echo "RADIO_BT_ATTACH_CMD=\"$BT_ATTACH_CMD\""
  echo "RADIO_BT_PRELOAD_CMD=\"$BT_PRELOAD_CMD\""
  echo "RADIO_BT_AFTER_WLAN=$BT_AFTER_WLAN"
  echo "RADIO_BT_TOGETHER=$BT_TOGETHER"
  echo "RADIO_BT_SCO_MTU_CMD=\"$BT_SCO_MTU_CMD\""
  echo "RADIO_BT_SCO_ROUTE_CMD=\"$BT_SCO_ROUTE_CMD\""
  echo "RADIO_WLAN_INSMOD=\"$(echo "$WLAN_INSMOD" | sed '/^$/d' | tr '\n' ';')\""
  echo "RADIO_WIRELESS=$WIRELESS"
  echo "RADIO_WLAN_IF=$WLAN_IF"
  echo "RADIO_WLAN_IFS=\"$WLAN_IFS\""
  echo "RADIO_WLAN_MAC=$WLAN_MAC"
  echo "RADIO_HCI_DEV=$HCI_DEV"
  echo "RADIO_BT_MAC=$BT_MAC"
  echo "RADIO_DETECTED_AT=$(cut -d. -f1 /proc/uptime 2>/dev/null)"
  echo "RADIO_DETECT_KEY=\"$DETECT_KEY\""   # last: btd's log of this file is one line, and a clipped end loses least here
} > "$OUT.new.$$" 2>/dev/null && mv "$OUT.new.$$" "$OUT"

[ "${1:-}" = -q ] || cat "$OUT"
# Exit 0 always: detection is diagnostic. "No radio" is a finding, not a failure, and this runs
# on the boot path where a non-zero exit would be mistaken for something worth reacting to.
exit 0
