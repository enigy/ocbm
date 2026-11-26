# Bluetooth bring-up, pairing and the radio HAL

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 57, 51, 41, 40; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** the chipset-agnostic radio HAL → between host sessions: armed (Bluetooth powered, invisible) → the cold boot (the descriptor kept, the Bluetooth firmware beside the Wi-Fi driver) → accessory-initiated reconnect → the HCI event-mask regression → the bring-up race and its health check.

## Radio HAL — chipset-agnostic bring-up

<!-- absorbed: ../wireless/01_BT_AND_RADIO.md -->

Status: **wireless CarPlay demonstrated end-to-end on an RTL8822CS unit (2026-08-15)** — a
chipset with no backend in this repo — over a SoftAP and Bluetooth controller brought up
entirely from command lines extracted from the unit's own vendor dispatcher. Detection, mapping
extraction, driver load, BT attach with convergence, the owned SoftAP, and the supervisor wiring
are all landed and hardware-validated. Read §7 for what is still open; Broadcom remains untested
and is the known-hardest case.

### 1. The problem, stated precisely

CCPA ships with at least six WLAN/BT parts. Per the resources repo's
`01_Firmware_Architecture/device_variants_and_conversion.md`, **only the driver tarball for a
unit's own chip is in its rootfs** — there is no fallback set. Delete a variant's WLAN files and
its radios are gone until it is reflashed.

The project's owned bring-up scripts (`wlan_on.sh`, `bt_on.sh`, `attach_bluetooth.sh`,
`wlan_off.sh`) are hardcoded to NXP IW416: `/lib/firmware/nxp/iw416_ko.tar.gz`,
`insmod mlan.ko/moal.ko`, `fw_loader_linux` + `hciattach`.

**This is the problem as it stood before the seam landed.** `tools/session_supervisor.sh` called
those scripts by literal path at four sites. On a Realtek unit the files do not exist, so
wireless bring-up **failed silently** while wired projection kept working — invisible because
the calls run inside a detached `setsid sh -c` whose output is redirected and whose exit status
is never examined. Those four sites now call `radio_hal.sh` (`session_supervisor.sh` —
`wifi_ap_on` and `bt_on` inside `wireless_up`, `wifi_ap_off` at both `wireless_down` teardown
branches); the old paths survive only in comments recording why they changed. Since 13 Nov 2026 `wifi_ap_on`
runs in the background beside `bt_on` (Bluetooth is the critical path), and `btd` starts once `bt_on` is done. Its
0x5703 reply reads the AP's SSID and passphrase, so a 0x5702 that arrives while `wifi_ap_on` still holds the wlan lock
waits for it, up to 5 s (`wifi_handoff::wait_for_ap`); past that btd stays silent and the phone asks again. Since 16 Nov
2026 `radio_ap_up.sh` holds that lock until hostapd's own `AP-ENABLED` (2 s at most past the AP serving, then a
hostapd still running counts), not a fixed second after the AP serves: the reply waits for an AP that is up, never
for a timer. From the armed state (below) the AP's bring-up is a launch from its configuration written ahead. They share only the WLAN driver load, which
`wlan_driver_up` serialises under its own lock, and `radio_detect.sh` writes its descriptor through a
per-caller temporary file. `caps()` detects once per boot and reuses a descriptor that found wireless; since 20 Nov
2026 it takes the descriptor kept on flash instead while its key holds, and the driver tarball is unpacked once a boot
under its own lock ("The cold boot", below).
ocbmd logs each HELLO and a host's first frames after 5 s of silence with the uptime, to place a stalled open. They
are written before Carlink arms its log stream, so a drive log carries them only as thinned `[backfill]` replay: read
them from Save Logs' box log or `/tmp/box.log` over the console, before a reboot (tmpfs) loses them.
A `wireless_up` waits for a `wireless_down` teardown still running (`/tmp/.wl_teardown.<uptime>`, one per teardown
block, in flight while any lives; up to 4 s, then the main loop's re-bring timer, which the block's end wakes), so a host back within the teardown is neither skipped nor killed by it. btd's reconnect SDP
connect is non-blocking and polled in its own thread: once the page is answered (the link BT_CONNECTED, by
HCIGETCONNINFO; a link mid-page is BT_CONNECT, state 5 in `hcitool con`), the SDP channel gets 700 ms (bench lever
`/tmp/bench_sdp_after_acl_ms`); past it the connect is cut, a result of its own: the socket is closed and the link
disconnected, except a link that was up before the page or one a live session rides, which is kept. The walk pages
the phone once more, as after a reset, and a round counts at most one cut per phone toward a forgotten pairing.

#### Why "it's Linux, it should be generic" does not apply here

Genericity on a normal distro is manufactured by four things this platform lacks:

1. **In-tree drivers.** `88x2cs.ko` (Realtek SDIO), `bcmdhd.ko` (Broadcom FullMAC),
   `moal.ko`/`mlan.ko` (NXP) are out-of-tree vendor forks. Realtek's SDIO 8822 parts are still
   not mainline; `bcmdhd` is Broadcom's out-of-tree driver, not mainline `brcmfmac`.
2. **udev/modalias auto-binding.** Normally the device advertises an ID and udev loads the
   matching module unattended. There is no such rule for these vendor modules, so *something*
   must read the SDIO ID and choose. That something is shell.
3. **cfg80211/nl80211 as one API.** That layer is what makes `wlan0` mean the same thing
   everywhere. These drivers each invented their own naming knob instead — `if2name=`,
   `iface_name=`, `uap_name=`.
4. **A standard firmware loader.** Each vendor ships a tarball with its own layout, extracted to
   `/tmp` at boot.

Busybox is not the issue — it is a userspace toolbox and provides `ifconfig`/`ip` fine. The gap
is below it, in a 2014 kernel (3.14.52) with no in-tree support for any of these parts.

**The consequence that shapes the design:** once the driver is loaded and the controller
attached, everything above genuinely *is* generic — `hci0`, `hciconfig`, BlueZ mgmt, `hostapd`
all behave uniformly, and an audit of the Rust sources found **no hard chipset dependency
anywhere**. So the chipset-specific region is exactly the span where mainline's abstraction
would have applied and doesn't. The HAL is not inventing an abstraction; it is rebuilding the
missing one, at precisely that boundary.

### 2. The rule

**Never branch on a chipset whitelist for behaviour.** An unrecognised variant falls off the end
of a `case` and gets no path at all. This is already `tools/ncm_base_install.sh`'s governing
constraint and it applies here unchanged. A chip-name table for *logging* is fine; nothing may
act on it.

### 3. Adopt the vendor's mapping, not their mechanism

Every unit ships `/script/init_bluetooth_wifi.sh` — the vendor's own SDIO-ID dispatcher — plus
`attach_bluetooth.sh`. `ncm_base_install.sh` deliberately preserves them (`is_radio()` protects
them by explicit basename), and its comments already anticipate this seam: on non-IW416 units the
vendor scripts "are KEPT and become the on-demand radio path".

Separate the vendor's **observations** from their **choices** (CLAUDE.md doctrine):

* **Observations — adopt verbatim.** The per-chip `insmod` lines with their parameters, the
  attach helper with its baud and protocol, the firmware tarball path, and the *ordering
  constraints* (on 0xc822 and SD8987 the BT attach must wait for the WLAN driver or the chip
  wedges). These are fleet-deployed and working, and are not re-derivable by us for parts we do
  not own.
* **Choices — do not inherit.** Executing the dispatcher wholesale re-imports:
  - `/script/attach_bluetooth.sh &` — **fork-and-return**, which is exactly the uncoordinated
    double-bring-up docs/wireless/01_BT_AND_RADIO.md recorded fighting itself for 7+ minutes;
  - the Broadcom branch backgrounding `brcm_patchram_plus` the same way;
  - the SD8987 branch's `dmesg | grep "woal_request_fw failed" && reboot`, a reboot inside a
    radio bring-up that would bypass `session_supervisor`'s escalation ladder and its persistent
    reboot budget;
  - `tar -xvf … -C /` overlays written into the rootfs per boot;
  - a BT health check that is object existence, which a dead chip passes.

**The AP layer is always ours — never delegate it.** On a stripped box the vendor's
`start_bluetooth_wifi.sh` reads config through `riddleBoxCfg`, which the OCBM base removes. The
calls are *not* `test -e`-guarded, so with it gone that script seds an **empty `wpa_passphrase`
into `/etc/hostapd.conf`** — persistent, on flash — and falls back to `WLANIP=192.168.50.2`,
which is NCM's own address, then repoints the DHCP pool onto the management subnet. Its teardown
`killall`s every `udhcpd`, including NCM's. That is the control channel you are standing on.

### 4. The two scripts

#### `ccpa/rootfs/script/radio_detect.sh` — read-only, emits `/tmp/radio_caps`

Two phases, because the useful facts split cleanly by cost:

* **Cheap, pre-driver (milliseconds):** SDIO `(vendor, device, class)` via a **glob** of
  `/sys/bus/sdio/devices/*/` — never the vendor's hardcoded `mmc0:0001:1`, which encodes a host
  index and an RCA that are enumeration accidents. Firmware tree, `*_ko.tar.gz`, attach-helper
  presence, backend tier.
* **Post-load:** the interface name and MACs, obtained by **enumerating** `/sys/class/net/*` for
  `wireless`/`phy80211` — never by assuming `wlan0`.

The mapping is extracted with **no chipset table**: intersect the modules in *this unit's own
tarball* with *this unit's own dispatcher's* `insmod` lines. A part nobody anticipated still
resolves, because every unit ships a dispatcher that knows its silicon and a tarball that names
its modules.

Two extraction details that only testing revealed:

* Anchor the `insmod` match on `/tmp/` — the dispatcher's success/failure `echo` strings also
  contain `insmod <module>`, and a greedy `sed` picks the echo.
* Anchor the attach-command match at line start, or it matches the `if [ -e … ]` guard above it.

##### The SCO/HFP lines are extracted PER BRANCH, and that is not the same problem (2026-09-03)

`RADIO_BT_SCO_MTU_CMD` and `RADIO_BT_SCO_ROUTE_CMD` join the descriptor, carrying the vendor's own
post-attach SCO setup, which `bt_on` applies as the stock attach does. The insmod
and attach lines are discriminated *for free* — by which modules the unit's tarball holds and which
attach helper it ships. **The SCO lines are not.** They appear in several chipset branches with
different contents:

| branch | `scomtu` | routing command |
|---|---|---|
| `0xb822` / `0xc822` / `0xb733` (Realtek) | `hciconfig hci0 scomtu 240:32` | **none** — Realtek routes SCO over HCI by default |
| `0x4354` / `0x4335` (BCM4354) | none | none |
| `0x4358` / `0xaa31` (BCM4358) | `hciconfig hci0 scomtu 240:32` | `hcitool -i hci0 cmd 0x3f 0x1c 0x01 0x02 0x00 0x00 0x00` |
| `0x9149` / `0x9141` / `0x9159` (NXP) | `hciconfig hci0 scomtu 240:32` | `hcitool -i hci0 cmd 0x3f 0x1d 0x00` |

A whole-file `head -1` would hand one chip another chip's vendor-opaque HCI command — the
no-chipset-whitelist rule violated from the other side. So `attach_branch()` selects the branch by
**this unit's own SDIO id** against the dispatcher's own `if/elif` chain: still no table, still no
whitelist, and an id the dispatcher never mentions yields nothing, so the seam reports `unsupported`
honestly. Three details the extraction needs:

* **Count `if`/`fi` depth.** The NXP branches close an inner `if [ $configBurned -eq 0 ]` *before*
  their `scomtu` line, so "stop at the first `fi`" drops the very lines we came for.
* **A single-branch attach script is its own branch.** The owned IW416 rewrite has no `sdioCardID`
  dispatch at all; falling back to the whole file is correct there and only there.
* **Strip trailing comments.** The owned rewrite annotates inline
  (`hcitool … 0x3f 0x1d 0x00   # route SCO to HCI`) while the vendor comments the line above. `#`
  only starts a comment in the shell's *input* — the result of a variable expansion is never
  re-scanned — so a captured trailing comment reaches `hcitool` as four extra positional arguments.

The routing command is recognised by the **vendor's own annotation** ("route sco data to hci", same
line or the one above), never by matching raw opcodes, which would be a chipset table in disguise.

Verified against every branch of the stock 2025.10 `attach_bluetooth.sh` and against the owned
rewrite; an unknown SDIO id refuses both lines.

#### `ccpa/rootfs/script/radio_hal.sh` — the seam

Verbs `probe | status | wifi_ap_on | wifi_ap_off | wifi_ap_prep | bt_on | bt_off | preload`.
Exit `0` converged now · `1` ran and failed · `2` already converged · `3` unsupported on this
unit. Single-flight `mkdir` locks per subsystem; state published atomically to
`/tmp/.radio_wlan_state` and `/tmp/.radio_bt_state`. `wifi_ap_prep` (16 Nov 2026) writes the AP's
configuration ahead and raises nothing; the armed state below runs it.

Three contract clauses, each bought with a real failure:

1. **Convergent on return, never fork-and-return.** When an `_on` verb returns 0 the radio is up
   *and responsive* and nothing it started is still in flight.
2. **Responsiveness, not existence.** Convergence is a real HCI round-trip (read the local name
   back under a timeout), because `hci0` can exist — and even read `UP RUNNING` — with a chip
   that answers nothing.
3. **Never reboot, never touch session daemons.** A backend that believes only a reboot can
   recover says so with an exit code and lets the layer that owns reboot policy decide. The HAL
   never signals `carplayd`/`rx-connect`/`btd`/`ocbmd` — it cannot know whether a
   live session is wired-owned, which is what the supervisor's conditional reap (docs/wireless/00_WIRELESS_CARPLAY.md #1.4)
   exists to decide.

**Naming is load-bearing.** The supervisor's teardown runs in a detached `sh -c` whose own argv
its own `pkill` patterns can see — a bug this project already paid for once (2026-08-01: a plain
`pkill -f` killed the teardown subshell before it reached `wlan_off`, orphaning a still-beaconing
AP). So the script name and every verb must avoid `carplayd`, `rx-connect`, `btd`,
`ocbmd`, `hostapd`. Audited clear; re-audit before renaming a verb.

**The owned AP layer is invoked as `/script/radio_ap_up.sh`, never
`/script/start_bluetooth_wifi.sh`.** Every unit already ships a vendor file at that second path,
so testing it for existence proves the file is there, not that it is ours — and running the
vendor copy is the destructive case in §3.

**The AP's DHCP leases survive AP restarts and power cuts (2026-11-02).** `radio_ap_up.sh` (and the
IW416 overlay's `start_bluetooth_wifi.sh`) deleted `/var/lib/udhcpd.leases` at every AP start, as the
vendor script does. An iPhone rejoining a network it knows keeps its old address and asks for it again
(INIT-REBOOT); the AP's `udhcpd` (v0.9.9-pre) stays silent on that request from a client it has no
lease for (upstream udhcp's "else remain silent"), so on the bench the phone ran CarPlay on `.101`,
gave up asking, was handed `.100`, and the session died. That deletion was the whole bug, and both
scripts now keep the file. This `udhcpd` saves it at every ACK (`radio_ap_dhcp.log`: "save dhcp
record", then "AP connection, save dhcp lease file"; 24 bytes per lease, the remaining time stored),
and `/var/lib` is on the jffs2 root (only `/tmp` and `/var/run` are tmpfs), so the file is current
through an AP restart and a power cut alike, and a phone keeps its address from one drive to the
next. With a lease for the phone's MAC it ACKs a request for that address and NAKs any other. SIGTERM
writes nothing, and need not: the file is as of the last ACK.

**Since 2026-11-10 the AP's server is BusyBox's `udhcpd` (1.32.1, the one serving NCM), not the
vendor's.** A kept file can't help where no lease exists: the first session after a conversion, or after
a Restore and reinstall. There the phone's INIT-REBOOT request met the vendor server's silence, it ran
CarPlay on its old address for ~5 s, then DISCOVERed, and the session died when it was handed another
(the Mac's LIVI bench, 10 Nov 16:58; the probe bench at 17:39 got the same address back and survived).
BusyBox's server NAKs a REQUEST it has no lease for ("No, we don't have this IP for you"), so the phone
takes a fresh address within a second, before CarPlay starts. Both scripts start it as
`busybox udhcpd -f -a 250 /etc/udhcpd.conf` (`radio_ap_up.sh`, whose 250 ms ARP probe replaces the default 2 s that cost
1-1.9 s whenever a phone had no lease; the IW416 overlay without `-f` or `-a`) on the same file,
pool (`.100`-`.200`), lease (864000 s), interface and lease file, and set `auto_time 60`. BusyBox writes
leases on that timer and at SIGTERM, not at each ACK, and a drive ends in a power cut, so a lease
reaches flash within a minute of its ACK. Its lease file has another format: an 8-byte timestamp, then
36-byte records. A vendor-format file, or one older than 12 hours by the box's clock (including a clock
that restarted behind it), is ignored and rewritten; the NAK covers that case. Nothing in OCBM reads the
vendor server's `/tmp/.dhcp_record` or `PHONE_P2PGC_ADDR`; the seam's teardown finds it by its config path and,
since 16 Nov 2026, waits for it to go (up to 3 s, its lease file being written; then SIGKILL), no longer killing
`/var/run/udhcpd.pid`'s pid blind (a stale pidfile can name any process), and an AP `radio_ap_up.sh` starts never
counts the last one's server as its own ("The AP, written ahead", below). **The owned backend keeps the old exposure
at both ends** (the second review of `resident-bt`, item 6): the IW416 overlay's `wlan_off.sh` kills by that pidfile
and does not wait, and its `start_bluetooth_wifi.sh` (through `wlan_on.sh`) starts no server while any AP udhcpd is
listed, an exiting one included. Both are reference only, never installed: an installer-built unit takes
`backend=mapped` and never runs them.

### 5. Hardware-validated results (RTL8822CS, 2026-08-15)

Detection and mapping, on a chipset with **no repo backend**:

    RADIO_CHIP=realtek_rtl8822cs   SDIO vendor=0x024c device=0xc822 class=0x07
    RADIO_FW_TREE=/lib/firmware/rtlbt      KO=rtl8822_ko.tar.gz
    RADIO_WLAN_INSMOD="insmod /tmp/88x2cs.ko if2name=sta0;"
    RADIO_BT_LDISC_KO=rtk_hci_uart.ko
    RADIO_BT_ATTACH_CMD="rtk_hciattach -s 115200 ttymxc2 rtk_h5"
    RADIO_BT_AFTER_WLAN=1
    RADIO_BACKEND=mapped

Bring-up: `wlan0` appeared with a valid per-unit MAC (the address itself is not recorded here — same
rationale as commit 42f5052: this repo is public and a hardware address is trackable); `hci0`
converged in ~2 s and answered a real name read (`RTK_BT_4.2`, `UP RUNNING`, 57 events / 69
commands exchanged); the attach helper
survived the SSH session closing; `ncm0` and the management channel were untouched throughout.
`sta0` did **not** appear — `if2name=sta0` names the *secondary* interface, which requires
STA/P2P mode (no `wpa_supplicant` on this unit), so `wlan0` is genuinely primary on Realtek,
consistent with the vendor's BT wait being on `wlan0`.

#### Four defects found only by running it

1. **Presence ≠ ownership.** The first `wifi_ap_on` would have run the *vendor's*
   `start_bluetooth_wifi.sh`. Fixed by the unambiguous `radio_ap_up.sh` name. Verified after the
   fix: `hostapd.conf` still `wpa_passphrase=12345678`, `ncm0` still `192.168.50.2`.
2. **`UP RUNNING` is a lie.** A wedged controller reported `UP RUNNING` while every name read
   timed out — docs/wireless/01_BT_AND_RADIO.md reproduced live. The responsiveness check is what caught it.
3. **Reclaim the UART before re-attaching.** Attaching while the previous helper still held
   `ttymxc2` produced `OP_H5_SYNC Transmission timeout` → `Retransmission exhausts`. Correct
   recovery needs the helper killed, **the line-discipline module rmmod'd**, and the reset GPIO
   driven `1` → `0` (polarity matters; the other way is a no-op that looks right in the log),
   then re-insmod and re-attach, retried. With that, the HAL **recovers a wedged controller** —
   something the vendor path cannot do, since its existence-only check never notices.
4. **The probe is not a clean oracle.** Measured: this controller intermittently times out a
   name read, roughly one attempt in four, with the next second's attempt succeeding. This
   drives an asymmetry worth stating generally: **strict to declare success, reluctant to
   destroy.** A false negative on "is it already up?" does not merely misreport — it resets a
   working chip, which during a live session drops the phone. Decisions therefore use a spaced
   multi-attempt probe; poll loops keep the single-shot.

### 6. Where it installs, and why not in the OCBM installer

**The HAL belongs to the baseline conversion (`ncm_base_install.sh`), not to
`ocbm_install.sh`.** The baseline is where the vendor stack is stripped and the owned boot path
lands; the radio platform is a property of the *converted unit*, not of OCBM. OCBM merely
consumes it.

But `ocbm_install.sh --full` ships `session_supervisor.sh`, which is the HAL's *caller*. So the
OCBM installer must **assert** the seam exists on the target rather than assume it — which is
exactly the manifest cross-reference check §7 lists: for every shipped `*.sh`, extract its
absolute-path references and require each to be in the manifest, present on the target, or
explicitly declared optional. Run against the manifest **as it stood before the seam landed**, on a
Realtek unit, that check would have failed on `wlan_on.sh`, `bt_on.sh`, `wlan_off.sh` — catching the
real failure three phases before anything switched.

> **DO NOT READ THE NEXT SENTENCE AS "THIS CLASS OF FAILURE IS CLOSED." IT IS NOT, AND IT COST A
> FULL DAY ON 2026-08-28.** What was re-checked on 2026-08-16 is narrow: the supervisor no longer
> names `wlan_on.sh`/`bt_on.sh` (the old paths survive there only as a comment), and `--full` ships
> the seam. The *mechanism* that made those failures silent is untouched and is still live today:
>
> - `tools/session_supervisor.sh:792,794` (and again at `:865,894`) call
>   `sh /script/radio_hal.sh {wifi_ap_on,bt_on} >/tmp/{wlan,bt}.log 2>&1` inside a detached `setsid`
>   wrapper. **There is no `$?` check at any of those four sites.**
> - So if `/script/radio_hal.sh` or `/script/radio_detect.sh` is simply ABSENT from the target, the
>   shell writes `sh: /script/radio_hal.sh: not found` into `/tmp/bt.log` and the supervisor carries
>   on exactly as if the radios had converged.
> - **Nothing else notices.** The OCBM claim, `CT_HELLO_ACK`, the MFi relay (a real 945-byte
>   certificate and 128-byte signature), `CT_SUBSCRIBE` and `HOST_PRESENT` all still succeed,
>   because none of them touch the radio path. The box looks healthy from every angle the host can
>   see, and Bluetooth simply never exists.
>
> **If you are debugging "BT does nothing", do these two things first:**
> 1. `ls /script/radio_hal.sh /script/radio_detect.sh` on the target. A targeted `ocbm_push.sh` does
>    NOT install them — its default set is `ocbmd` + `btd` — so a box can hold a
>    supervisor that depends on files it never received.
> 2. `cat /tmp/bt.log /tmp/wlan.log`. That is the ONLY place the failure is written.
>
> Then read `CT_BOX_HEALTH` bit 0 (`BH_HCI_PRESENT`), which since 2026-08-29 means `hci0` is UP via
> `HCIGETDEVINFO`. A health of `0x50` (`btd|rootfs-ok`) with bit 0 clear is this fault's
> exact signature. Full account: `../ops/06_CORRECTIONS_LEDGER.md` `R-20W-5`.
>
> Two facts this document predates and does not otherwise mention: `hci_uart` is a **loadable
> module** on the CCPA, shipped inside `/lib/firmware/nxp/iw416_ko.tar.gz` — if nothing `insmod`s it
> the `n_hci` line discipline is never registered and `hciattach` fails `EINVAL`. And the box's
> `/tmp` is a **tmpfs on a USB-powered device**, so unplugging it to move it between bench and head
> unit IS a power loss: `/tmp/radio_caps` and any extracted `.ko` vanish on every move. `radio_hal.sh`
> now re-derives both, but any future step that caches into `/tmp` must assume it starts empty. (Since 20 Nov 2026 the
> descriptor is also kept on flash, `/etc/ocbm/kept_radio_caps`, and reused while its key holds; a missing, stale or
> damaged copy falls back to detecting, as here. "The cold boot", below.)

The narrow re-check: the supervisor names only `radio_hal.sh`, and `--full` now ships the seam. The
generic assertion is still unwritten — see §7, and note that `tools/ocbm_push.sh` now warns when the
supervisor is pushed to a box missing the seam, which is a guard at the point the mistake is made
rather than a check in an installer nobody ran.

That is the correct framing of what went wrong: **not a rule violation, a rule gap.** The WLAN
variant rule policed radio *files* in both directions but had no assertion over radio
*references inside non-radio files the installer ships*.

### 6b. Wireless CarPlay session, RTL8822CS, 2026-08-15

With the radios up through the seam and the macOS host app connected over OCBM:

    SUBSCRIBE sent (36848 B config) -> box session event: PRESENT
    phone ABSENT -> phone PRESENT (~15 s: association + handshake)
    [relay] RS_OPEN conn=1 wireless=true
    HEVC 1920x720, A/V decrypt video ok=690 fail=0, audio codec=1 48000Hz 2ch
    voice=33pps (Siri uplink live)
    100+ iAP2 metadata messages; three album artworks up to 141 KB
    steady state: 33-36 fps, jitter 24-30 ms, AVmon gaps=0 fails=0

**One transient worth chasing.** During establishment the video decoder produced bursts of
`Frame enqueued with decode failures — requesting keyframe to re-sync`, peaking at 136 in a
single second, with `hvcC` re-parsed repeatedly alongside them. It self-clears within ~90 s and
the session then runs cleanly, so it is a startup-latency annoyance rather than a session
killer. The shape — an IDR request following each failure, and a fresh `hvcC` parse following
each IDR — suggests the decoder session is being rebuilt against every re-sync request in a
brief feedback loop. Note the *decrypt* counters are clean (`fail=0`) throughout, so this is
strictly downstream of decryption.

**Why this session worked is also why it would not have on Broadcom.** `av.rs:63,66` pin
`WLAN_IFACE="wlan0"` and `AP_IP="192.168.43.1"`. On this Realtek unit both constants happen to
be correct — `wlan0` is primary and the owned AP layer serves 192.168.43.1 — so the Rust side
never noticed it was hardcoded. On a Broadcom unit the interface is `sta0` and `wlan0` does not
exist at all, so the identical session would fail with the radios up and perfectly healthy.
That makes §7's Rust item load-bearing risk rather than tidiness.

### 6c. What the box does with app-pushed config

The app pushes no Wi-Fi keys. Until OCBM 1.0.5 the supervisor's `apply_host_wifi_creds()` took
`wifi_ssid`/`wifi_pass`/`wifi_channel` for the GM bridge role (`wifi_ap:false`, where the box handed
the iPhone the vehicle's hotspot) and rebuilt `/etc/hostapd.conf` from a `.stock` snapshot; Carlink
never sent them. The role is gone, and the supervisor puts a bridge-role adapter's own
`/etc/hostapd.conf.stock` back once (`restore_stock_hostapd`). The box's AP is always its own:
`radio_ap_up.sh` names it `ccpa-<4hex>`, keeps the passphrase (writing one only when absent or <8
chars) and the channel (unless `/etc/wifi_use_24G` forces ch 6).

The box-AP SSID overwrite is long-standing — the old `wlan_on.sh` did the identical `sed` — but
it sits awkwardly against docs/carplay/04_CAPABILITIES_AND_CONFIG.md's doctrine that configurable CarPlay state is app-driven and
the box presents app-pushed config. In box-AP mode the box currently names its own AP and the
app cannot override it. Left as-is for now because changing it would alter behaviour on the
IW416 baseline too, but it is a doctrine gap, not a design decision.

**Bluetooth: the app pushes no name at all.** There is no `bt_name` key; the box self-derives
`ccpa-<4hex>`. Naming is a *box* concern, so an app that wants to brand the BT
identity currently cannot.

#### One box, one name — the intent, and why it is NOT yet achieved

Wi-Fi and Bluetooth must advertise the same `ccpa-<4hex>`, and deriving it per-radio does not
achieve that. The sources disagree: on the RTL8822CS unit the Wi-Fi MAC and the BT controller
address share no octets at all (Realtek programs no `bd_addr`, so the controller uses its own
efuse), so the two derivations yield different suffixes. Which name a radio picked would depend on
which radio happened to be up when it was asked. It could also change between boots, and the phone stores the name in its bonded record.

So the **shell** side resolves the identity once and persists it to `/etc/carplay_ident`:
`radio_hal.sh`'s `box_name()` writes it and passes it to `radio_ap_up.sh` as `RADIO_BOX_NAME`, and
`radio_ap_up.sh` re-reads the same file when invoked directly, so the SSID and the seam's BT name
come from one decision rather than two derivations. A placeholder is deliberately never persisted —
if every source fails the next call retries rather than freezing `ccpa-0000` into flash.

> **⚠️ CORRECTED: `/etc/carplay_ident` is NOT yet the single source of truth.** `radio_hal.sh` and
> `radio_ap_up.sh` are its only readers; `btd`, `bt_on.sh` and `ocbmd` each derive a
> name independently, and `btd` overwrites the advertised BT name every session — so
> the divergence §6c claims to have eliminated is still live. Closing it is tracked in §7, not here.
> Full detail and the correction history behind it:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-57-1`.

### 6d. What single-line extraction cannot represent — and why that was dangerous

The claim in §4 that "a part nobody anticipated still resolves" is **true only where the vendor
branch is closed-form**, and only the Realtek branches are. The NXP and Broadcom branches carry
shell variables resolved elsewhere in the dispatcher:

    insmod /tmp/moal.ko "mod_para=$nxpWiFiConfig"
    brcm_patchram_plus --patchram /lib/firmware/bcm/$bcmBTFirmware ... --bd_addr "$bcmBTMac" &

Captured verbatim those are not commands, only text shaped like one. And writing them into
`/tmp/radio_caps` made `. "$CAPS"` abort under `set -u` with **status 2** — which this seam
defines as *already converged*. Every verb died inside `caps()` before the backend switch,
reporting success, on **every NXP and Broadcom unit including the IW416 baseline**. Measured, not
theorised: sourcing a poisoned descriptor returns 2 on the box's own shell.

Fixed in two independent places, because either alone leaves a sharp edge: `radio_detect.sh`
refuses to emit a mapping it cannot faithfully execute (a trailing `&` is stripped; anything
still carrying `$`, a backtick or quotes becomes an empty mapping, so the seam reports
*unsupported* honestly), and `caps()` can no longer be killed by any descriptor. The usage-error
exit moved off 2, which it had been colliding with.

**So the honest scope of the design claim:** RTL8822BS/CS and RTL8733BS resolve on paper. NXP
resolves since 2026-10-18 for the one closed shape its branches use: a literal default assigned in
the unit's own SDIO branch, plus at most one alternative behind `if [ $gate -eq 1 ]` whose gate is a
one-line `test -e PATH`. Anything else still falls to the refusal above. Broadcom still resolves to
an honest "unsupported" rather than to a wrong command.

The gate's quoting is not cosmetic. Carlinkit's own 2025.10.15.1127 `init_bluetooth_wifi.sh` writes
`if [ $supportStaMode -eq 1 ]`; lvalen's NCM image rewrote it as `if [ "$supportStaMode" -eq 1 ]`.
Until 2026-10-28 only the quoted form resolved, which is the only text the IW416 baseline had ever
carried, so a factory IW416 unit (the one-file install's target) would have lost `moal.ko` and,
because BT attach waits for WLAN on this part, Bluetooth with it. Both forms now resolve, and
`tools/radio_detect_test.sh` runs both texts.

### 7. Not yet done / not yet proven

* Broadcom mappings are **refused, not resolved** (§6d): their branches assign the firmware and
  MAC variables in shapes the NXP resolver does not accept. Those units report `unsupported` —
  correct behaviour, but not working wireless.
* The `mapped` backend on a **factory** IW416 unit (Carlinkit's own scripts, no `wlan_on.sh`) is
  resolved on paper only: offline against the 2025.10.15.1127 text it yields both module lines,
  `hciattach /dev/ttymxc2 any 3000000 flow` and the `fw_loader_linux` preload. The one-file install's
  bench (M3) is its first run on hardware.
* `ocbm_install.sh`: the generic manifest cross-reference assertion (§6). The specific Realtek
  failure it would have caught is now closed — `--full` ships the seam alongside the supervisor
  (`ocbm_install.sh:158-160`) — but a future shipped script referencing a path the target lacks
  would still slip through.
* A reversible radio verification gate in the baseline, run while still on NCM.
* Rust parameterisation: `WLAN_IFACE`/`AP_IP` from the seam (`av.rs:63,66`), the identity chain
  (`box_identity.rs`'s `SOURCES` :10, which `hardware_id()` reads; `iap2-core/src/message.rs`'s `device_suffix()` :215 — a
  live bug on IW416 too, since `/sys/class/net/wlan0/address` is absent whenever the driver is
  unloaded, which under on-demand radios is most of the time). **Added 2026-08-16:** that same chain is why §6c's "one box, one name" is
  **not yet achieved**. `/etc/carplay_ident` is read only by `radio_hal.sh` and `radio_ap_up.sh`;
  `btd` (`main.rs` → `bt_bringup::bring_up`), `bt_on.sh` and `ocbmd`'s `bt_name_from()`
  each derive independently, and because the supervisor execs `btd` *after*
  `radio_hal.sh bt_on` the controller ends up advertising `CarLink-<suffix>` rather than the seam's
  `ccpa-<4hex>`.
  Closing it means the Rust side reading the ident file first — a behaviour change, so it is listed
  here rather than done. **31 Oct 2026, the dev adapter (IW416):** the iPhone listed `btd`'s
  `CarLink-fd6c`, `wlan0`'s suffix; the ident file and the SSID read `ccpa-fe01`, from `sta0`; and
  the app's pairing screen read `CarLink-0264`, because it asks before `wlan0` exists and
  `bt_name_from()` fell back to the serial. `bt_name_from()`'s caller now takes `wlan0`'s MAC from the
  one-file installer's `/etc/ocbm/addresses.txt` while `wlan0` is absent, so `MGMT_INFO`'s name is
  what `btd` advertises whenever `btd` brings Bluetooth up with `wlan0` present. Measured on the same
  adapter 1 Nov (installer 6affefc): `MGMT_INFO` read `CarLink-fd6c` with no `wlan0` and `hci0`
  down, and Carlink's pairing screen and Settings › Phones showed that name too. The seam's own name
  and the BT-only role still differ. **16 Nov (A2, branch `carplayd-early`):** the chain now matters to carplayd's
  start too. carplayd starts as btd's host session begins, and the identity it is handed (`box_identity::derive`)
  changes when `wlan0` appears; so that start waits for what derive reads first (`box_identity::derive_primary`, the
  same path, no other name: A2-4, the release review) to give the same identity on two looks 100 ms apart, 15 s at
  most, and otherwise leaves the start to the 0x5703, as before. A unit whose Wi-Fi interface is not `wlan0` never has
  that source, and keeps 2.6.x's start at the 0x5703. The 0x5703 derives the identity again and restarts a carplayd
  started with another one before it may dial (`av::expect_phone`). No interface name is added to the Rust side.
* The startup decode transient described in §6b.
* **Broadcom is entirely untested** and is the known-hardest case: `wlan0` there does not exist
  until it is explicitly created on top of `sta0` (`iw dev sta0 interface add wlan0 type
  managed`). No Broadcom hardware is available to this project. RTL8733BS and SD8987 are
  likewise unexercised — the seam resolves them on paper from their own dispatchers, which is
  the whole design claim, but that claim is only measured on 0xc822 so far.

---

## The Wi-Fi link's station beside the AP (branch `wifi-link`, 24 Nov 2026; not benched)

With a hotspot set up (an `ssid_hex=` line in `/etc/ocbm/wifi_link`, written by ocbmd for `MGMT_WIFI_LINK`) the adapter
also joins the phone's Personal Hotspot as a station, so a head unit on that hotspot reaches ocbmd over TCP
(docs/carplay/01_OCBM_PROTOCOL.md, "The Wi-Fi link"). Without the line nothing below runs.

- **Interfaces, from the unit's own driver configuration.** The IW416's stock `wifi_mod_para.conf` (2025.10.15.1127, read
  from the 18 Oct backup) says `drv_mode=3`, `sta_name=sta`, `uap_name=wlan`: one moal load makes `sta0`, the STA BSS,
  and `wlan0`, the uAP BSS. The AP of 2.7.1 takes the first wireless interface by name, `sta0`, which hostapd switches to
  the AP role, and leaves `wlan0` idle. In STA mode `radio_hal.sh` (`sta_mode`, `sta_iface`, `ap_iface`) gives the AP the
  wireless interface that is NOT named `sta*` and the station `sta*`: each BSS in its own role, and the layout the
  vendor's own STA mode uses (its supplicant on `sta0`). A unit whose driver makes one interface has no station beside
  its AP; the AP keeps it. That is known 1 s after the driver's first interface appears (`sta_settle`: a driver's
  interfaces come from one probe) and marked for the boot (`/tmp/.radio_one_iface`), so no path waits for a station
  again, `wifi_sta_up` answers 3 at once, and the keeper publishes `state=unsupported` (MGMT_INFO `wifi_link` state
  `"unsupported"`). An adapter with no link forks nothing for any of this (`sta_mode` tests the file first). The AP's
  teardown never downs the station's interface. That the IW416 firmware runs the two together on one channel is
  inferred, not measured.
- **Never `/usr/sbin/wpa_supplicant`.** The vendor dispatcher's gate (`test -e /usr/sbin/wpa_supplicant &&
  supportStaMode=1`) switches moal to `wifi_mod_para_p2psta.conf` (`drv_mode=5`, `wfd_name=wlan`: no uAP BSS at all), and
  `radio_detect.sh` follows that gate on every unit, set up or not. The stock image ships no supplicant (the factory
  survey of 27 Oct), so the keeper takes OCBM's own `/usr/sbin/ocbm-wpa_supplicant`, a unit's own stock copy, or a bench
  copy in `/tmp/bin`, and waits without one. It speaks to the supplicant's control socket
  (`/var/run/wpa_supplicant/<if>`) with `ocbmd wpa`, not wpa_cli, which the adapter no longer carries (24 Nov 2026:
  about 68 KB of flash); by hand over the console the same: `ocbmd wpa sta0 status` (or `scan`, `scan_results`,
  `list_networks`, `select_network 0`: wpa_cli's commands, the reply printed as wpa_cli prints it; `-p DIR` before the
  interface for another control directory), and `ocbmd wpa sta0 events 60` (below). OCBM's own is built by
  `tools/build_wpa_supplicant.sh` (wpa_supplicant 2.11 and libnl 3.12.0, static, nl80211 with WEXT as the fallback,
  WPA2-PSK only) from release archives a checkout fetches once into `scratchpad/wpa/dl`, each checked against a SHA-256
  pinned in the script, and the image places it when built (its licences as NOTICE files beside the payload).
- **WPA3-only hotspots are not supported in this version** (WPA2 and WPA2/WPA3 mixed are): the box receives the
  hotspot key as its PBKDF2 hash, which serves WPA2-PSK only, and the supplicant is built without SAE.
- **The keeper** (`/script/wifi_sta.sh`, started by `ocbm_boot.sh`): from power-up it keeps the station on the hotspot,
  with the address and netmask from its DHCP hook (`wifi_sta_dhcp.sh`: no default route, never the AP's or NCM's
  subnet), publishes `/tmp/wifi_link_state` for ocbmd and the AP layer (`iface=`, `state=`, `channel=`, then the last
  look's `seen=<channel> <uptime>` and, below, `elsewhere=`) and keeps the hotspot's last channel, joined or heard, in
  `/etc/ocbm/wifi_link_channel`, its address in `/etc/ocbm/wifi_link_bssid` (below) and the station's own address at
  its last start in `/etc/ocbm/wifi_link_sta` (said once a start: `the station's own address: the one it had at its last
  start`, or `new since its last start (a new device to the phone)`; the supplicant uses the interface's own address,
  2.11's `mac_addr` default, and the keeper sets none). Each is written 0600 when it changes and never logged.
  Driver load for it: the verb `wifi_sta_up`, under the wlan lock; while that lock is
  held (the boot preload loading the driver) it looks for its interface every 0.2 s instead of queueing behind it, 15 s
  at most, and waits for its supplicant's socket rather than a fixed second. A supplicant that does not start, or does
  not stay, is started again no sooner than 30 s after its last start and said once until it runs. With no supplicant or
  no station it publishes `seen=0`, so no AP start waits for a look that cannot come.
- **Scans on the one radio** (ADDENDUM 9 of the hand-off plan, 24 Nov 2026, for the looks). None while joined. Not
  joined: at each supplicant start with no AP up, and no host but the adapter's own session (M2), the *first look*: the
  hotspot's last channel alone (the supplicant's own first scan too, `initial_freq_list`; a fraction of a second where
  it was last time), then the channels an AP may use; its result is published at once. With a session starting (the AP
  up, or `/tmp/host_present` 1: M2's own session pages the bonded phones over Bluetooth before any AP) and not
  established (`/tmp/session_healthy` not 1, 40 s at most) no scan at all, so the page, the phone's join of the AP and
  the session's set-up never meet one; the one exception is the adapter's own session before its AP comes, when the
  first look heard the hotspot: scans and joins on that channel alone, so the station may join first. A USB host's start
  is never held up by a look. With the AP up the station scans AND joins on the AP's channel only (`scan_freq`,
  `freq_list`): one radio cannot join elsewhere while the AP runs, and each try off it took the radio from the phone
  (the evening bench: an authentication on 2437 beside the AP on 36, then the driver's `bss_start Fails`); a *look* over
  the channels an AP may use (no radar channel) each 60 s, or each 20 s while the adapter's own session has no host (its
  video parked), the first at once when its AP is not where the hotspot was last heard. With no AP up: every channel,
  and a look each 30 s (a wired session too). A look is `ocbmd wpa <if> find` (ccpa/ocbmd/src/wpa.rs): first the
  supplicant's own results (a sighting from its scan of the last 4 s, or a join under way: then no scan of ours, so a
  look never meets the join the supplicant starts on its own results), else a scan of the look's channels that probes
  for the hotspot by name (next item); where it heard the hotspot, from the BSS table (entries it did not hear, or on
  other channels, left out). The keeper says the channel and dBm when they change. The supplicant scans for itself
  (`disable_scan_offload=1`): wpa_supplicant 2.11 hands its scans to the driver's scheduled scan after three that found
  nothing (`wpa_supplicant_req_sched_scan`), and nothing on this radio has shown that the driver reports them. What it
  scans and joins on changes in place (ADDENDUM 11): its network's `scan_freq` and `freq_list` through SET_NETWORK,
  which leaves a join under way alone (a RECONFIGURE deauthenticates first); the adapter's own session's start pause
  (DISCONNECT) and the restart after 300 s not joined wait for a join under way to end.
- **The hotspot with its name hidden** (ADDENDUM 10 and 11 of the hand-off plan, 25 Nov 2026). An iPhone's Personal
  Hotspot leaves its name out of its beacons whenever the phone's hotspot screen is not open (locked, or another screen
  in front), also while a client is joined: a scan that names nothing lists it as a strong entry with an empty name,
  and only a probe that carries its name gets an answer with the name. The supplicant's own scans carry it
  (`scan_ssid=1`); so does every look now: `SCAN freq=<list> ssid <name in hex> ssid ` (wpa_supplicant 2.11's grammar,
  ctrl_iface.c `wpas_ctrl_scan`: the name, then the wildcard; a driver that takes one name per scan keeps the first). A
  look also knows the hotspot by its address, kept in `/etc/ocbm/wifi_link_bssid` at each join (STATUS `bssid=`) and
  when a look hears it by name: an entry with its name hidden (empty, or zeros) and that address, on the look's
  channels, is the hotspot (find's `seen 6 -39 hidden`; the keeper's `the hotspot heard on channel 6 (-39 dBm, its name
  hidden: known by its address)`). Heard by name under another address, with the kept one not heard in the same look,
  the new address is kept; a new set-up (MGMT_WIFI_LINK, ocbmd `wifi::store`), `OCBM_WIFI_RESET` and the go-back to
  stock remove it. The owner, 25 Nov 2026: an iPhone's hotspot address does not change; other phones and hotspot
  devices are not known.
  - **For owners:** the adapter remembers your hotspot's address so it can find and join it while the name is hidden
    (on an iPhone: whenever the Personal Hotspot screen is not open). This relies on the hotspot keeping one address, as
    an iPhone does. A phone or hotspot that changes its address has to be visible by name (its hotspot screen open) for
    the adapter to find it again; the adapter then remembers the new address. Setting the Wi-Fi link up again forgets
    the old one.
  - **No `bssid=` or `bssid_hint=` in the supplicant's network** (decided 25 Nov from the 2.11 source). With `ssid=`
    set, `bssid=` only narrows the choice to that address: the name check stays (events.c `wpa_scan_res_ok` relaxes it
    only for a network with no name), so it adds nothing for a hidden hotspot and would lock out an owner whose hotspot
    came back under another address. A network with `bssid=` and no name would match the hidden beacon, but its
    authentication and association carry the entry's empty name (sme.c takes the BSS's `ssid`), which no hotspot takes.
    `bssid_hint=` serves only a driver that picks the BSS itself (wpa_supplicant.c, the connect command's
    `bssid_hint`); this driver takes the supplicant's own SME (moal's `HostMlme` lines). So the supplicant joins by
    name, from the answer to its named probe, and the kept address serves the looks: `seen=`, `elsewhere=`, the AP's
    head start, and the retry below.
  - **Held back after failed joins:** 2.11 disables the network for a while after 4 failed attempts in a row
    (`wpas_connection_failed`: 10 s, then 20, 30, 60 from the 4th, 90 from the 6th, 120 from the 11th, 300 from the
    51st; `ssid-temp-disabled auth_failures=N duration=D reason=CONN_FAILED`). A look that hears the hotspot then sends
    SELECT_NETWORK, which clears that and has it try at once (ENABLE_NETWORK leaves an enabled network's back-off as
    it is), said once in 5 minutes: `the hotspot heard while the supplicant held it back after failed joins: it tries
    again now`. Not while the station is paused for a CarPlay start.
- **Its joins, as they happen** (ADDENDUM 11). Beside each supplicant the keeper runs a monitor of its control socket,
  `ocbmd wpa <if> events 0 /tmp/wifi_sta_misses` (the file: its count of unanswered authentications, ADDENDUM 13,
  below) (ATTACH and LEVEL 2: this build has no debug output, and its MSG_DEBUG `wpa_msg` lines, the
  SME's own 5 s timers among them, are all there is beyond the events), into the box log: `[wifi_sta] [up N]
  supplicant: <event>`, names and numeric codes only, never a name or an address. A join's steps carry the time since
  its authentication began (`authenticating freq=2437`, `trying freq=2437 (+2412 ms)`, `associated (+2450 ms)`,
  `connected (+2610 ms)`); a step that ends with no event of its own is said from STATUS (`no answer to the
  authentication (+2405 ms, now SCANNING)`: 2.11 says nothing when the driver ends one, sme.c
  `sme_event_auth_timed_out`). Within a minute each line comes once, then one line with the repeats (`again in the
  last 60 s: authenticating freq=2437 x5, no answer to the authentication x5`); the hotspot not found after a scan is
  left out (the looks say that). It ends with its supplicant (TERMINATING, or no PONG after 10 s of silence) and
  starts again with the next. By hand, `ocbmd wpa sta0 events 60` prints the same for 60 s. Being an `ocbmd` process
  that lives all along, `pidof ocbmd` counts it: ocbm_boot.sh's failover watchdog (`pgrep -f /usr/sbin/ocbmd`, as the
  respawn wrapper), the supervisor's L2 check (the full path) and the trial self-test (an ` wpa ` command line left
  out) look for the daemon without it; the supervisor's `pkill -x ocbmd` ends it, and the keeper starts it again.
- **The authentication's 2.4 s: what the stock driver does** (ADDENDUM 12 and 13 of the hand-off plan, 25 Nov 2026).
  The bench of 557b6b9: with no AP up, the driver's `wlan: HostMlme sta0 send auth to bssid ...`, then `wlan: HostMlme
  Auth received from ...` 2.41 s later every time, about 20 ms after the monitor's `no answer to the authentication
  (+2406 ms, now SCANNING)`. FOUND by disassembling the factory image's moal.ko (`iw416_ko.tar.gz`, driver
  `MXM5X--437.p20`; its symbols, relocations and strings; nothing run):
  - *Host MLME.* moal's `host_mlme` parameter is on by default (1) and the stock `wifi_mod_para.conf` leaves it. With it
    `woal_register_cfg80211` gives cfg80211 the `auth` and `assoc` operations and takes `connect` and `disconnect` away,
    so wpa_supplicant 2.11 runs its own SME (driver_nl80211_capa.c: the wiphy lists AUTHENTICATE): an
    NL80211_CMD_AUTHENTICATE per attempt. A connect command, even forced (`force_connect_cmd`), finds no `connect`
    and goes through cfg80211's own SME onto the same operation (Linux's cfg80211, not read from the unit's 3.14).
    **The card forces it** (ADDENDUM 15): woal_add_card, right after the command line and then the conf block were
    read (the block's `host_mlme=0` prints `host_mlme=disable`), takes the IW416's card table and, its
    `host_mlme_required` byte being 1, sets host MLME again. No parameter, conf line or /proc knob turns it off; only a
    driver copy with that byte changed (the bench's `sta_drvpatch`, below).
  - *The wait is a remain-on-channel.* `woal_cfg80211_authenticate` asks the firmware to stay on the network's channel
    for 2400 ms (the literal in its call to `woal_cfg80211_remain_on_channel_cfg`; 20 MHz, the channel's band, asked
    whenever no authentication is pending, whatever else runs on that channel), marks the authentication pending, and
    sends the frame it built as the firmware command TX_FRAME (0x0283: band, channel, the station's TxPD, the
    firmware's own rate, no transmit-status request), because this firmware says it takes frames by command
    (`cmd_tx_data`; the AP's own frames from hostapd go the same way). When the firmware says that stay has expired
    with the authentication still pending, moal calls `cfg80211_auth_timeout` (dmesg `wlan: HostMlme auth timeout`)
    and clears the pending flag and host MLME for the interface. The supplicant hears TIMED_OUT and moves on in
    silence (sme.c `sme_event_auth_timed_out`; its own authentication timer is 5 s and never comes first). Nothing
    sets the 2400: no module parameter, no supplicant key.
  - *The late answer is passed by.* The `Auth received` line is mlan's, the moment the firmware hands the phone's answer
    up. moal treats an authentication frame as the answer only while host MLME and the pending flag are set; after the
    expiry neither is, so no `Received auth frame` or `Drop auth frame` follows and the supplicant never sees it.
  - *Power save is off on the card.* The stock file's `ps_mode=2` and `auto_ds=2` apply to the card (the interface names
    of the same block, `sta0` and `wlan0`, do; `/sys/module/moal/parameters` shows the module's own defaults, not the
    card's), and moal refuses cfg80211's default power save on the station while `ps_mode` is 2.
  - *No reset from failed authentications on this firmware* (ADDENDUM 15 corrects ADDENDUM 13). moal does count the
    authentications it sends with none answered and calls `woal_reset_wifi(.., "auth timeout")` at the fifth
    (`WiFi Reset due to auth timeout cnt 5`, `Start to process hanging`, with `auto_fw_reload` the firmware loaded
    again), but only on its data path; the command path this firmware uses returns before the count. A second count
    does reset the radio: five deauthentications received while connected (`WiFi Reset due to EAPOL timeout cnt 5`).
  - *Why the answer comes only as the stay ends* (INFERRED; ADDENDA 12 to 18 of the hand-off plan). **It is 2.4 GHz
    only, and neither the phone, the AP beside the station nor the driver's mode**: the control of 25 Nov 18:10 (the
    hotspot moved to 149) authenticated in 7 ms and joined in 39 ms; a second network on ordinary access points failed
    alike on 2412 and 2437, and so did the client-only driver mode (`drv_mode=1`). Nothing the driver sends differs by
    band (ADDENDUM 15). What fails is the station's unicast exchange: the authentication in the stay, the firmware-built
    association after one answered in time (0xfffc, also with the bench's late copy), the firmware's own join (0xfffa);
    its broadcast probes and their answers work (its directed scans found the hidden hotspot), and so does the AP. The
    in-time answers at 2.4 GHz whose moment is known came with a channel event (24 Nov: the AP on 36 and the stay to
    2437; case a: hostapd starting on 6 during the stay; otherwise the stay's own end; two have no known event), and the
    same channel with the AP running joined at 5 GHz (36) but not at 2.4 GHz (6). Ranked (ADDENDUM 18): a 2.4 GHz-only
    hold of the station's unicast frames in the combo firmware, let go by a channel change, Bluetooth's coexistence
    arbiter being the chip's one 2.4-only part (its static set-up, not its traffic: the controller was idle in btd's
    armed state, page and inquiry scan off, nothing paging); the firmware's antenna choice; the module's own calibration
    file, shipped and never loaded; the station's 802.11d domain, sent band 0 then band 1 so that the last is the 5 GHz
    one (but 802.11d is off, and the firmware's join re-sends the 2.4 GHz one before it joins). The stay alone is not
    it: the association fails with no stay. docs/ops/02_TESTING.md has the no-flash steps that split these.
  - *What the host can set there* (ADDENDUM 18, read in the factory moal and mlan): no coexistence parameter or conf key
    (the driver's robust coex is one TLV, 0x021b, an external Bluetooth-request pin; the firmware's event 0x76 only
    tunes aggregation windows and scan times); `antcfg` (1 or 2 an antenna, 0xffff diversity), 0 in the stock file so
    the firmware's own default runs, read and set at run time in `/proc/mwlan/adapter0/config`; `cal_data_cfg` (none in
    the stock file, though `nxp/WlanCalData_ext.conf` is there) and `init_hostcmd_cfg` (raw host commands as sta0 is
    added), files the loader finds by a plain name in /lib/firmware, either ending the driver's load when it cannot be
    read; the power table's 2.4 GHz rows (CCK 16 dBm on 1-14); the firmware's OTP region, WW and not forced, so the
    driver writes US over it (`FW country code WW does not match with US`; channels 1-11 active either way). Worth a
    grep in any dmesg with MEVENT: `EVENT: BT coex wlan param update` (and `coex_scan=..`), `EVENT: Antenna Diversity`,
    `EVENT: Stop Tx`, `EVENT: TX_DATA_PAUSE`, `EVENT: MULTI_CHAN_INFO`, `EVENT: FW Debug Info`.
  - *Seen in dmesg* (the driver's default mask, `drvdbg` 0x207, shows MMSG and MERROR lines): `send auth to bssid`,
    mlan's `Auth received from`, `pending auth on going`, `Fail to configure remain on channel`, `HostMlme sta0: Set
    remain on Channel: with status=N`, `Fail to send packet status=N`, `bss_start Fails`, `WiFi Reset due to ...`,
    `Start to process hanging`. With the bench's `sta_drvdbg` (0x237: MCMND 0x10 and MEVENT 0x20 added) also
    `FW_REMAIN_ON_CHANNEL_EXPIRED cookie = ...` and `wlan: HostMlme auth timeout` (MEVENT), `HostMlme sta0: Received
    auth frame type = 0x0` (MEVENT), `Remain on Channel: chan=6, offset=.. width=..` and `wlan: HostMlme sta0 auth
    success` (MCMND). The mask is moal's `drvdbg`, writable at run time: `echo "drvdbg=0x237" >
    /proc/mwlan/adapter0/sta0/debug` sets moal's and its mlan's, `/sys/module/moal/parameters/drvdbg` moal's alone.
- **What the keeper and the AP layer do about it** (ADDENDUM 13, built 25 Nov on 557b6b9; not benched):
  - *The AP of the adapter's own session waits for a join under way* (`radio_ap_up.sh` `ap_join`): once the first look
    heard the hotspot, from the supplicant's STATUS every 0.2 s (`ocbmd wpa`), 6 s at most (the authentication alone
    takes 2.4 s): joined, the AP takes its channel; still under way at the bound, the AP starts beside it (said). With
    the bench's `sta_join_first` it waits instead for the station to be on the hotspot, up to that many seconds (10),
    its retries included. A USB host's AP never waits.
  - *No join begins in hostapd's start.* Not joined at the end of that wait, the AP layer stops the supplicant
    (DISCONNECT, never over a join under way) and writes `/tmp/.radio_ap_start` (its pid) until its start is over; the
    keeper takes that as its own pause (`the AP is starting: no scans and no joins until it is up`) and its start rules
    put the station back (the session running, 40 s at most; at once if no AP came).
  - *No fifth miss while the AP runs.* The monitor keeps the count of authentications in a row with no answer in
    `/tmp/wifi_sta_misses` (one more for each that ended unanswered or on the SME's 5 s timer, none again after any
    answer: the association tried, a refusal, a join; a refusal by the driver sent no frame and does not count); at 3
    the keeper tries no join while an AP is up (`3 authentications in a row unanswered: no joins while the AP runs (the
    driver resets the radio at 5)`), and goes on when the AP is down, where a reset drops no phone. The file lasts the
    boot, as the driver's count does (a reset the driver made with no AP up is not seen: the count stays high, so it
    errs toward holding). **Its premise does not hold on this firmware** (ADDENDUM 15, above: no reset from failed
    authentications), and the line's parenthesis repeats it; the guard now only holds joins back while the AP runs
    (`off` in `bench_sta_miss_guard` for the bench; whether to keep it is the owner's).
  - *The quick retry the plan asked for is 2.11's own:* its first three retries come 0.1 to 1 s after each failure, and
    the bench's case b shows them failing alike. Not built.
  - *Firmware MLME* (ADDENDUM 15: the bench's `sta_drvpatch` with the card's `host_mlme_required` byte changed, and
    `moal host_mlme=0` in `/etc/ocbm/bench_sta_modparam`, then a power-up; `host_mlme=0` alone is overridden by the
    card): moal keeps `connect` (`woal_cfg80211_connect`: the firmware's own join, no remain-on-channel, no
    host-built frame, no counter), and 2.11 then connects with NL80211_CMD_CONNECT by itself (`trying freq=..` with no
    `authenticating` before it; dmesg `wlan: Connected to bssid ... successfully`, or `can't find targe AP` when moal's
    own scan table lacks the hidden hotspot by name). Its cost is the AP's side: moal tests the same flag in its uAP
    start, its beacon filter, its management frame registration and transmit and its station add and delete, and
    without it says the AP's SME is the firmware's (`WIPHY_FLAG_HAVE_AP_SME`): so, INFERRED from those, the phone's
    authentication and association of the AP move into the firmware and hostapd learns of stations from the driver.
    Stock never ran so; not benched. The same one insmod at the boot preload (from the copies the two files make): no
    reload, nothing on a USB host's time to picture from the load itself. Changing it at run time would need the driver
    unloaded and loaded again (the AP and the station down for the driver's load, 3.8 s in the 24 Nov boot log beside
    the Bluetooth firmware's push, and Bluetooth's side under that unknown): not built.
  - *The bench's driver copies* (ADDENDUM 15; radio_hal.sh, off unless the file is there). `sta_modparam` puts its words
    for a module whose insmod names a parameter file (moal's `mod_para=`) into a copy of that file at `/tmp/fw`, inside
    its block after the file's own lines, and points the kernel firmware loader's path
    (`/sys/module/firmware_class/parameters/path`) there: moal reads the command line first and the block after it, so
    only there does a word the block names win. `sta_drvpatch` (`<module> <sha256> OFFSET:OLD:NEW...`) loads the module
    from a copy with those bytes changed, only when its file has that sha256 and the copy differs by exactly those
    bytes. The lines for moal 437.p20 (docs/ops/02_TESTING.md): *late*, the stay's expiry no longer ends a pending
    authentication and a new one is not refused for it, so the answer that reaches the driver 10 ms after the stay is
    taken; *nostay*, no stay at all, the frame going as the AP's own do (only with the AP on the hotspot's channel);
    *fwmlme*, the card's byte, for the firmware's own join above.
  - *The rest of the candidates, and their lines* (the monitor's beside dmesg): the answer taken and the association not
    answered (`trying freq=2437 (+N ms)` then `no answer to the association ...`, `assoc-reject status_code=N`; dmesg
    `bss_start Fails`, `Failed to connect`); the phone refusing (`auth-reject ... status_code=N`, 17: too many
    clients); the key (`associated` then `the key handshake timed out` or `4-way handshake failed (the key?)`); the
    driver refusing the step (`the driver refused the authentication`; dmesg `pending auth on going`, `Fail to
    configure remain on channel`, `bssid not found in scan list`: moal's own scan table lacks the network); a new
    device to the phone at each start (`the station's own address: new since its last start`); the back-off
    (`ssid-temp-disabled ... duration=D`, the look's `... it tries again now`).

- **The listener and the advert stay on the station's side.** The listener takes connections from the station's
  interface only (SO_BINDTODEVICE: under Linux's weak-host model the station's address alone would also answer on the
  AP's or USB's network). The mDNS socket is bound to the group, 224.0.0.251:5353, so no unicast datagram for carplayd's
  mdns-sd (its sockets: 0.0.0.0:5353, SO_REUSEPORT) can land in it.
- **Channels: the AP follows the hotspot** (ADDENDUM 6 of the hand-off plan, 24 Nov 2026; not benched). One radio, so
  one channel for the adapter's AP, its station and the head unit: the hotspot's. On the first bench (24 Nov, dev
  adapter) the station held the hotspot on 149 and the AP came up on `hostapd.conf`'s 36: the phone never joined the AP
  (4 CarPlay starts of 4, cut by btd's 90 s watchdog). With a session running on 36 the phone's hotspot was on 36 too
  (it follows the phone's own link) and the station joined beside the AP there. So at each AP start in STA mode the seam
  (`radio_hal.sh` `sta_channel`) hands the AP layer the channel the station is associated on, from the keeper's
  `/tmp/wifi_link_state` (no fork), and `radio_ap_up.sh` (`ap_follow`) starts hostapd there from a per-session
  configuration, `/tmp/hostapd_session.conf` (tmpfs, 0600): `hostapd.conf` with that `channel=`, its band's `hw_mode`
  and the HT40 side of its 40 MHz pair, any VHT centre left out. `hostapd.conf` on flash is never written for it; the
  per-session file goes at the AP's teardown and at the next start. btd's 0x5703 (the channel the phone is told) and
  MGMT_INFO's `ap_channel` read it while it is there, `hostapd.conf` otherwise. With no link nothing of this runs.
  - **Where an AP may start:** the US domain the unit's `hostapd.conf` asks for (`country_code=US` in the factory file
    of 28 Oct and the dev adapter's of 21 Oct: 802.11n, HT40+ on 36, VHT off) without radar detection: 1-11, 36-48
    (U-NII-1), 149-165 (U-NII-3). Never 52-144 (DFS), nor any other. HT40 on its pair's side: 36/40, 44/48, 149/153,
    157/161 (HT40+ on the lower channel, HT40- on the upper), 1-7 HT40+ and 8-11 HT40- at 2.4 GHz; 165 and 14 have no
    pair, 20 MHz there. In STA mode `/etc/wifi_use_24G` gives way: the AP goes to the station's band.
  - **At power-up, the adapter's own session** (ADDENDUM 9, the evening bench of 24 Nov on b860672): M2's AP came up at
    about 10 s on `hostapd.conf`'s 36 before the keeper had published anything (its supplicant started at 10.0 s, its
    first word was a second later), so the station never joined the hotspot on 2.4 GHz channel 6 and no Wi-Fi host could
    reach the adapter in the 180 s the session ran. Now, while the adapter's own session runs with no host
    (`/tmp/wifi_self_parked`, ocbmd's) and the station is not on the hotspot, `radio_ap_up.sh` (`ap_head`) waits up to
    4 s, while the keeper runs, for its first look, and starts the AP on the channel the look heard the hotspot on, else
    on the hotspot's last channel (`/etc/ocbm/wifi_link_channel`, where an AP may), else on its own; said in one line
    with the wait. Where the look heard it, it also waits for a join under way (ADDENDUM 13, above: 6 s at most) and
    holds the station still through hostapd's start: `STA mode, the adapter's own session: channel 6, heard there by
    the station's look (waited 300 ms for the station, then 2400 ms for its join, held still while hostapd starts)`.
    Any other start (a USB host's) waits for nothing and keeps its own channel when the station is not on the hotspot,
    as before.
  - **2.4 GHz** (the owner's head unit needs the hotspot's Maximize Compatibility, so 1-11 is the product's normal
    case): the session's configuration is `hw_mode=g`, 20 MHz (`ht_capab=[SHORT-GI-20]`, the factory file's own 2.4 GHz
    line: no 20/40 coexistence scan at hostapd's start, nothing on the channel next to the hotspot's) and no VHT line.
    The phone is told the channel in the 0x5703 as at 5 GHz (a channel number, from which iOS takes the band).
  - **Heard elsewhere while the AP runs:** two looks in a row that heard the hotspot on another channel an AP may use
    (the keeper's `elsewhere=`): the adapter's own session with no host attached starts again, once (ocbmd's re-arm: the
    phone leaves CarPlay and comes back, about a cold start), and its next AP takes that channel. A session a host is
    attached to is never moved.
  - **Not associated, or on a channel no AP may use** (a radar one): the AP keeps its own channel, as before, and the
    box log says why; the station stays joined, so MGMT_INFO shows `joined` with the two channels and Carlink its line.
    A followed channel hostapd refuses (it exits in its start-up: a domain with no AP there, say) costs one try: the AP
    then starts on its own channel, also said.
  - **During a session a host holds nothing moves.** The station joining after the AP is up finds the hotspot on the
    AP's channel (the phone's hotspot follows the phone's link, as benched). A station on another channel while the AP
    runs (it came up before the station joined) is said once by the keeper and reported in MGMT_INFO; the next AP start
    follows. What the radio does meanwhile (refuse the association, move the uAP, time-share the two channels) is for
    the bench.

## Between host sessions: armed (Bluetooth powered, invisible)

**Status: built and benched 16 Nov 2026** (branch `resident-bt` merged with `carplayd-early` into a local `round2`,
on the dev adapter, IW416). 12 wakes and a cold boot connected from the armed state (that boot a restart by `reboot`:
no cold boot for radio timing, its firmware downloads in it but the host up already, "The cold boot" below): `sup_radios` about 50 ms,
`bt_armed=1`, `page_start` about 100 ms and `ap_enabled` about 1.16 s after the host's arrival. With no host the armed
state read over the console as built: `hci0` `UP RUNNING` with neither `PSCAN` nor `ISCAN`, no link, no hostapd, udhcpd
or carplayd, the AP interface (`sta0`) down. Against `/tmp/bench_arm_off` arming saved about 1.0 s per connect. The
fallback, forced by freezing the armed btd (`kill -STOP`): `armed go failed (no answer from btd N in 3 s) -- falling
back`, `bt_armed=0`, and the connect completed by today's path. The bench plan:
[../ops/02_TESTING.md](../ops/02_TESTING.md), "The armed path".

**Open: one bad wake, of two delays.** The first wake about 90 s after an update was committed took 24.8 s, and two
delays made it up (the Mac's second reading of its log, 17 Nov). *A late join, its cause open:* hostapd's `AP-ENABLED`
came 0.53 s after its start and 4.5 s before the 0x5703, yet the phone authenticated about 7 s after the 0x5703
(`AP-STA-CONNECTED` 8.6 s after RFCOMM, 0.4 s on the other wakes). hostapd logs that before any DHCP, so a missing DHCP
server does not explain it; it may be the phone's. *No DHCP server, about 6.7 s:* no `udhcpd: started` and no
`sending ACK` for that session; the phone went on with its old IPv4 address, and its CarPlay service resolved 6.5 s
after it joined. Only this second delay has a likely cause, fixed since (the release review of `resident-bt`, A3-1;
"The AP, written ahead" below): the last AP's udhcpd, still writing its lease file to flash slowed by the commit's
garbage collection, was taken by the next AP start for its own DHCP server, and none was started. Neither repeated
after a reboot. Should it recur, capture right after it: `/tmp/wlan.log`, `/tmp/radio_ap_dhcp.log`, `/tmp/box.log`
(hostapd's lines for the phone's authentication and association, and the time of `[dispatch] update committed`),
`/tmp/startup_marks` and `df /`; for the late join, the phone's own log archive too (`idevicesyslog archive`, its
`wifid` lines; [../carplay/02_SESSION_LIFECYCLE.md](../carplay/02_SESSION_LIFECYCLE.md) §6), pulled on the Mac
afterwards.

The owner's decision of 16 Nov 2026: between host sessions Bluetooth may stay **powered but invisible**. "Invisible"
is hard: with no host here the controller has page scan and inquiry scan off, pages nobody and holds no connection,
so nothing can see it or connect to it and it reaches out to nobody. Until then the teardown left it attached and
powered down, and each connect paid `radio_hal.sh bt_on` (0.75 s) and btd's start and bring-up (0.16 s) before its
first page: `page_start` 1.17 s after the host on the dev adapter's 16 Nov wake bench (IW416). About 90% of this
owner's connects are wakes (the adapter stayed powered), so that is the path this is for.

**One fresh btd per host session still holds; the next host's starts early.** With no host here, once the last
teardown is over (or the boot preload is done), `tools/session_supervisor.sh` starts btd with `OCBM_BTD_ARM=1`
(`arm_gate`, `arm_wireless`). That btd (`crates/vendor/wireless/src/armed.rs`) runs `bt_bringup::arm` under the
seam's Bluetooth lock, writes its pid to `/tmp/.btd_armed`, and waits in `poll()` on a self-pipe its SIGUSR1 and
SIGTERM handlers write to. Nothing else of a btd's start runs before the host's go: the control port, `Control` (the
policy file, the switch hold it takes up), `/tmp/bt_phase`, the `[startup]` record and every session thread start
after it, as in a btd started at the host's arrival, so every per-process assumption (first-arm-wins caches,
per-session state) is unchanged. The gate, cheapest first: armed Bluetooth not switched off, a wireless session ran
since the last arm (or this is the supervisor's start), `hci0` attached (an arm never attaches: at boot the preload
does, without it the first host's session does), no seam lock (the preload holds both), no teardown in flight (the
2.6.0 ordering), no wired phone on the bus (its session would be wired, and the radios stay down for it), no btd.

**Invisible: the state, and its proof.** The arm, in order:
1. mgmt `SET_CONNECTABLE=0` while the controller is down (`ssp_agent::clear_connectable`): a flag change only, so the
   kernel's own power-on (its `powered_update_hci`, which writes Scan_Enable from those flags) enables no scan;
2. the DOWN→UP cycle a session's bring-up does (`hci_dev_do_open`: the SSP event mask), the controller reset on the
   way down, so its Scan_Enable is the spec default, none;
3. the identity writes (Simple Pairing, class, name, EIR), each waited for, as a session's;
4. Write_Scan_Enable `0x00`, waited for its Command Complete (`hci::write_scan_enable_waited`);
5. the proof: Read_Scan_Enable answers `0x00` from the chip itself (`hci::read_scan_enable`, not the kernel's
   PSCAN/ISCAN mirror) and HCIGETCONNLIST lists no connection (`hci::connections`); otherwise the arm fails.

Nothing pages (no reconnect thread), nothing listens (no SDP server, no RFCOMM listener, no SSP agent) and nothing
turns LE on. Every 180 s the armed btd checks again (one Read_Scan_Enable round trip and the connection list): a
controller found visible or holding a link is hidden again (scan none, each link disconnected), three times at most;
one that does not answer is asked again 2 s later, and only the fifth miss in a row gives the armed state up, as
`bt_responsive_confirmed` does. A check never resets anything: btd exits, and the supervisor (`arm_watch`) powers
Bluetooth down through the seam (`bt_off`), the between-sessions state of 2.6.0, so the next host takes today's path.
On a bench: `hciconfig hci0` reads `UP RUNNING` with neither `PSCAN` nor `ISCAN`; `hcitool con` lists nothing; a
phone that scans never finds the box, and a bonded one shows it "Not Connected"; `/tmp/wl.log` carries `[armed] armed as
"CarLink-…": Bluetooth powered, scan 0x00 (page and inquiry off), 0 connection(s), …` and no `[reconnect]`,
`[ssp-agent]` or `[sdp…]` line until `[armed] go: …`.

**The one exception: M3's set-up code** (branch `wifi-link`, 24 Nov 2026; not benched). On an adapter offering its
set-up network (docs/carplay/01_OCBM_PROTOCOL.md, "The Wi-Fi link"), ocbmd puts the set-up session's 4-digit code in
`/tmp/bt_setup_code` once the head unit's first record opens, removes it when that session ends for any reason, and
wakes the armed btd with SIGUSR1 after each change (the go's own wake; only a pid in `/tmp/.btd_armed` whose
`/proc/<pid>/comm` is `btd`). Woken with no go, btd writes the name `CarLink code NNNN` (local name and EIR, the class
and Simple Pairing as the arm wrote them) and Write_Scan_Enable `0x01`: inquiry scan only, so a phone's or a car's
search lists the box and nothing can page it (no connection, no bond: no SSP agent runs while armed); with no file, its
armed name and `0x00` again. The file is read at the arm too (a session that began while Bluetooth was not up yet: the
code shows once a btd arms), at every wake and at every check, and the checks hold the controller to `0x01` while the
code is up. A go while it is up passes the code's name as the arm's, so the session's bring-up rewrites the identity
(the name differs). A SIGTERM while it is up writes scan none before btd exits (the supervisor's `bt_off` follows). No
armed btd at all (the preload or the arm switched off, an arm that failed): the code is not shown, and ocbmd's line says
so (`set-up code on: no armed btd yet`). Nothing goes to flash; `/tmp` and the controller forget it at a power cut. On a
bench: `hciconfig hci0` reads `ISCAN` without `PSCAN`, `hciconfig hci0 name` the code's name, and `/tmp/wl.log` carries
`[armed] set-up code: the controller named for ocbmd's set-up session and discoverable …` and later
`[armed] set-up code gone: …`.

**The go.** ocbmd raises `/tmp/host_present` and wakes the supervisor. `wireless_up` finds its armed btd ready
(`btd_armed_ready`: the file names the btd it started, a live one, not a zombie) and calls `wireless_go`: the inhibit
(`/tmp/radio_off`), `wireless: false` and a wired session owning the box each put the armed Bluetooth down instead,
as each keeps today's bring-up from starting; otherwise the go file `/tmp/.btd_go` (`pid=`, `BT_PAIRING_MODE`,
`BT_SSP_INTERACTIVE`), the timeline stamps, SIGUSR1, and then the AP. No fork before the signal but one `awk` over
the host's config (`cfg_snapshot`, held to the four functions it stands in for by
`tools/session_supervisor_test.sh`). btd takes the inputs into its environment (no other thread runs yet), proves
the controller with one Read_Scan_Enable round trip (150 ms, a second try), answers `ok <uptime ms>` in
`/tmp/.btd_go_result`, and goes on as a btd started at the host's arrival. Its first session's bring-up is what the
arm left: the conflicting daemons stopped, the identity rewritten if the session's name differs from the arm's (it
derives from wlan0's MAC, which may have appeared since), and page plus inquiry scan on (`bring_up`'s last step; the
SSP agent narrows it to the pairing window at once, as at every session start). A later session in the same process
(a phone switch) runs `bring_up`'s DOWN→UP as before.

**The fallback is today's path.** A failed proof (`fail …`, and btd wakes the supervisor), no answer in 3 s, no btd,
or an arm still under way 3 s after the host came: the supervisor stops that btd and runs `wireless_up fallback`,
today's wrapper unchanged (`bt_on`, a fresh btd) except that it skips the AP launch and the timeline stamps, which
went out with the go. An arm under way at the host's arrival gets those 3 s to finish (it takes about one: the AP
written ahead, then the DOWN→UP); a host back within a teardown finds no armed btd (none is started while a teardown
runs) and takes today's path, the teardown first, as in 2.6.0. After a go, `wireless_down` never takes its early
return (nothing running) until a teardown has run (`wl_raised`, set by a go and by today's bring-up, cleared by each
teardown block), nor while an AP bring-up holds the wlan lock: the go launches the AP detached, so a btd that failed
its proof and a host gone before hostapd showed would otherwise have left the AP coming up with no host (the release
review's A1-2).

**The session's inputs, each as it reaches the btd that serves the host.**

| Input | Today's path (at the host's arrival) | Armed |
|---|---|---|
| `BT_PAIRING_MODE`, `BT_SSP_INTERACTIVE` (the host YAML's `pairing:`) | btd's environment, from `wireless_up` | the go file, read at the go into btd's environment before the SSP agent reads them |
| the A/V reap and the wireless A/V log's truncation (`OCBM_WL_REAPED`) | the wrapper, before btd | the arm's wrapper, before btd; nothing starts an A/V daemon between the arm and the go |
| `t sup_radios`, `t usb_edge` (the timeline) | the wrapper | the supervisor at the go |
| `CARPLAY_BTMON` / `/tmp/carplay_btmon` | the wrapper | the supervisor at the go (`btmon_lever`) |
| the supervisor's own environment (PATH, bench levers) | inherited | inherited at the arm, from the same supervisor process; a restarted supervisor tears an armed btd it did not start down and arms its own |
| btd's per-process state: control port, `Control`'s policy and switch hold, `/tmp/bt_phase` | at btd's start | after the go |
| the session's record, its name, the bonds, the pairing window | at the session's start | at the session's start, after the go |
| `/tmp/.radio_bt_warm` ("opened") | read by btd's first bring-up (spent unread by a btd that goes on the boot preload's handover, "The cold boot" 6, whose proof is the handover) | spent at the arm, unread: the arm opens the controller itself |

**Each case.** *The 30 s display-off cycle:* the teardown (≈2 s), then the arm (≈1 s), then the go. *Host back within
the teardown:* today's path, unchanged. *The boot preload:* the arm follows it once `hci0` is attached and its locks
are free; a self-test wait, the legacy first-boot trial (no preload) and a preload its guard switched off all leave
`hci0` unattached, so there is no arm until a host's session has attached it; a preload that starts after an arm
(its self-test passed late) finds the controller up, answering, and leaves it (its rc 2). *`/tmp/radio_off`:*
ocbmd clears it on every SUBSCRIBE and on the host's loss, so it is never set while armed; set at the arrival it puts
the armed Bluetooth down. *`wireless: false`:* down at the arrival; a session ended without wireless is not followed
by an arm. *A wired session* (Hot-Handover off): down at the arrival with A2's restore owed, so the cable's going
restores the stack; no arm while a wired phone is plugged in. *Restart wireless and Forget phone:* with the host here
they go through today's path (`wireless_down`, then today's bring-up); one that lands while no host is here re-arms.
*Pairing a new phone:* armed, the box is neither discoverable nor connectable; at the go the session's SSP agent
applies the pairing window as always (discoverable inside it, or while no phone is bonded). *An update:* it is
placed with a host here (a session btd) and restarts the adapter; the new version's boot preload then precedes its
arm. The update trial's commit (`ocbm_dispatch.sh` `btd_settles`) waits for a btd serving a host's session (a btd
process, its phase published in `/tmp/bt_phase`, none armed in `/tmp/.btd_armed`), never one only armed (corrected
16 Nov 2026, the release review's A1-3: the first build counted any btd process). An armed btd publishes no phase
before its go, and every arm removes one a previous btd left (the teardown that leaves a wired `carplayd` running
does not), so a phase never stands beside a btd only armed. *ocbmd
restarting:* with a host here it drops presence, a teardown, then an arm or today's path; with none here nothing
changes. *The supervisor restarting:* an armed btd it did not start is torn down at its start; it arms its own.
*btd crashing while armed:* its controller is left powered and scan-off; `arm_watch` sees the btd gone and powers it
down through the seam, and the next host takes today's path.

**The AP, written ahead (A3).** The 0.94 s between the radios starting and hostapd's launch on the 16 Nov wake bench
was ours: `radio_hal.sh` reading its descriptor, the driver step (already loaded), then `radio_ap_up.sh` deriving the
name and rewriting `hostapd.conf`, `udhcpd.conf` and `/etc/wifi_name` on flash, about 45 forks and 7 flash writes
before hostapd. The arm's wrapper now runs `radio_hal.sh wifi_ap_prep` (`radio_ap_up.sh PREP`: the configuration
only, nothing raised: no address, no hostapd, no udhcpd, no beacon), which leaves `/tmp/.radio_ap_prepared`
(`<interface> <name> <address> <channel> <2.4 GHz flag>`, then a checksum of each file the configuration lives in or is
written from). At the go, `wifi_ap_on`'s fast path skips the descriptor and the driver step, and `radio_ap_up.sh`
trusts the stamp while those files hold what they held (one `md5sum` over `hostapd.conf`, `udhcpd.conf`,
`carplay_ident`, `wifi_ip` and `wifi_name`), the 2.4 GHz flag is as it was and `/etc/carplay_ident` (or the seam) still
names the box the same: the address goes on the interface, then hostapd is the first thing launched. **By content,
never by date** (corrected 16 Nov 2026, the release review's A3-2): the adapter's clock reads 2020-01-02 until a host
sets it, so a stamp the boot's arm wrote was dated before files a 2026 clock had written, and the first build's date
comparison (`-nt`) never trusted it, so the fast path did nothing on the first connect after every power-up.
**The AP's DHCP server** (the release review's A3-1): BusyBox udhcpd writes its lease file to flash as it exits on
SIGTERM, so the teardown waits for it (`ap_dhcp_stop`: 3 s, then SIGKILL and 1 s more), and a start that launches
hostapd stops any AP DHCP server still listed, starts its own and keeps its pid: the convergence asks after that
process, a zombie not counted, through the confirmation. Before, a server still on its way out was taken for the new
AP's and none was started (the second delay of the bench's bad wake, above). The start's stop is shorter, 1 s and then
SIGKILL, 1.5 s at most (the second review of `resident-bt`, item 7): it holds the wlan lock, which btd's reply to a
0x5702 waits on for 5 s at most (past that the phone asks again), and the teardown has already given the last AP's
server its 3 s, so one still listed at a start is stuck or was never torn down. A server killed there loses its lease
write: the file keeps its last save (`auto_time`, at most a minute old) or, cut mid-write, the whole records before the
cut (BusyBox reads whole records only), and a phone whose lease went with it is NAKed and takes a fresh address within
a second. **The confirmation, by the start's own line** (the second review's item 3,
after A3-3's count): just before hostapd's launch the start writes `[radio] hostapd starting on <if>` to the box log,
and waits for an `AP-ENABLED` after it. ocbmd empties the box log at its cap with an `ftruncate` in place, the same
inode, so neither an inode nor a count taken at the launch tells; with the start's line gone the log was emptied after
the launch, and every `AP-ENABLED` in it is this hostapd's. A3-3's count, reset only when it fell, missed the line when
the log was emptied and the line written within one look (100 ms). One window is left and documented: the line written
and the log emptied before the next look; the wait then runs to its 2 s bound with the lock held, and a hostapd still
running counts, as before 16 Nov. Its own 0.86 s to `AP-ENABLED`
(`COUNTRY_UPDATE`, then `HT_SCAN` for `[HT40+]`, in its log) is the AP's, not ours: hostapd skips that scan only on
`noscan=1`, which a hostapd that lacks it refuses as an unknown item (and so fails to start), and this one is packed
so its version cannot be read off the file; it is left alone (see docs/ops/04_OPEN_ITEMS.md). Every `ap_configure`
write now happens only where a file differs (the arm runs it at every host departure), so a configuration written
by either path also stays current for the next. The fast path works without the arm too, whenever the stamp is
current.

**Off.** `/script/radio_arm_off` (the owner's, on flash) or the bench hook `/tmp/bench_arm_off` (`box_common::bench_flag`'s
rule; Carlink writes `off` to clear it) keeps Bluetooth powered down between sessions, as in 2.6.0, from the next host
departure on; an armed btd already running stays until then.

---

## The cold boot: the radios as early as the chip allows

**Status: 1 to 5 built 20 Nov 2026 on branch `cold-boot` (from OCBM 2.6.5, `801074f`) and benched the same day on
real power cycles** (the coordinating session's bench of `70caa3c` on the dev adapter in final mode, the host up before
the adapter had power, so it came at 4.2 to 5.0 s of uptime): together 3 of 3, both loads complete, `tries=1`,
`fallback=0`, `rc=0`, `btfw_ms` 2207 to 2239, no Wi-Fi driver error, CarPlay every time; `hci_up` 6.82 to 7.03 s
together against 9.23 and 9.38 with `/script/radio_together_off`; the first frame 13.8 to 15.4 s against 16.2 and
17.4. **6, the handover (R3-5), built the same day and benched on `4a6ce9c` on 21 Nov: it works as designed, but 2 of
its 6 boots had link trouble against 0 of 9 without it, the cause not established, so it is OFF BY DEFAULT** (the
owner's decision, 21 Nov: opt-in only, `/script/radio_handover_on`; see 6). Bench plan and revert conditions:
[../ops/02_TESTING.md](../ops/02_TESTING.md), "Cold boot: real power cycles".

**Why.** In the owner's truck the adapter loses power 15 to 30 minutes after the truck is switched off, so most drives
start with it booting: 22 of 26 display-on connects on 16-20 Nov found it just booted, and those reached the picture
about 19 s after display on (7 clean samples, 17.4 to 21.7 s) against about 7 s from an adapter still powered. The
host subscribes about 3 s into the boot, while the boot preload has just started (`[startup]`: "adapter up 3.0 s,
preload running"), and its `bt_on` and `wifi_ap_on` wait on the preload's locks: `bt_power` landed 7.4 to 8.6 s after
the host. Where the cold boot's time went (the dev adapter, one clean cold boot, uptime in s): `ocbm_boot.sh` 2.0;
the gadget ready 2.37, enumerated 2.57; the preload 2.75-2.89; `radio_detect.sh` 2.89-3.90 (0.75 to 0.8 s a run, timed
by hand); the Wi-Fi driver first 3.90-6.54 (insmod 4.08, firmware over 4.75, "WLAN FW is active" 6.00, `sta0` 6.54);
the Bluetooth firmware 6.58-9.35 (`fw_loader_linux`'s own timer about 2.2 s); `hciattach` to `hci0` answering
9.35-10.11; "preload: done rc=0 in 7.5 s" 10.44.

**A cold boot is a power cut, never a `reboot`.** A `reboot` was read on 20 Nov as keeping the IW416's firmware, but
the three restarts logged on 13 Nov (`[bench] reboot under a running host`, the connect-speed benches) each pushed the
Bluetooth firmware in full, 2.75 to 2.83 s from the push to the attach, and loaded the Wi-Fi driver in about 2.5 s: a
SoC restart resets the chip (22 Nov, from those logs). What the adapter cannot do without one is put the chip back into
its boot loader: reclaim+reset's gpio1 pulse did not (20 Nov, by hand: after it `hci0` answered within half a second,
and the Wi-Fi driver loaded in under 0.4 s, too quick for a download; that run's "the loader returned at once with no
output" was a result file left by the run before it). The benches before 20 Nov that said "a cold boot" (the armed path's,
above, among them) were restarts, with the downloads in them but the host already up: only removing power gives a
cold boot.

**1. The descriptor, kept across boots.** `/tmp` is empty at every power-up, so every boot ran `radio_detect.sh`
again, though its answer changes only when what it reads changes. `radio_hal.sh` `caps` now keeps the last descriptor
a detection gave that found wireless in `/etc/ocbm/kept_radio_caps`, without its post-load facts (interfaces,
addresses) and its time, and the next boot takes it (`caps_from_kept`) while `radio_detect.sh`'s own key reads what
it read then: `radio_detect_key`, at the head of that script so a new input there is a new term beside it, covers the
SDIO ids, the firmware trees, the helpers and bring-up scripts present, each file test the dispatcher's gates make, and
one md5 over the md5 of every file whose text or members are read (itself, the dispatcher, the attach script, the
driver tarball): one hash, so the key stays short in btd's one-line log of the descriptor, which ocbmd clips at 1024
bytes. Post-load facts are no input. The check is one awk, one grep and the md5s (the tarball's read the unpack right
after finds cached), against the detection's dozens of forks; the boot line's `caps_ms` measures it. Reused only as
`caps_check` reads it: `KEY=VALUE` lines nothing runs when sourced, the end mark last, wireless, a backend, an SDIO
id, and on the mapped backend an insmod and an attach mapping; anything else, or another key, is detected, and the
detection's result kept again. Written only after a detection and only where it differs (once per firmware or
`radio_detect.sh` version), as its own file, synced, renamed over the old, synced. Why `/etc/ocbm`: OCBM's own state
directory, which the one-file installer makes (a unit without it keeps nothing and detects every boot, as before);
updates keep it, and the key notices a new `radio_detect.sh`; a revert keeps it for a retry, whose boot the key checks;
the full way back to stock removes it, since its name is outside `is_radio()`'s `radio_*`. Not `/script`, where the
preload's flags are: `is_radio()` keeps every `radio_*` file there through every deletion unless a remover names it.
A start with a self-test (every trial start, an update's first) gains nothing from it: the self-test's own detection
fills `/tmp/radio_caps` first, and nothing is kept from it; a final adapter's starts are the ones it serves.
**A descriptor with no SDIO id is never final, and never kept** (the 21 Nov review, D and L). Started at about 2.0 s,
the preload could read the chip before the kernel has enumerated its SDIO card; the detection then names no device,
and its insmod and attach lines are not this unit's own. So the preload first waits for the card (`sdio_wait`: a look
every 0.1 s, 3 s at most, said either way), and `caps` takes `/tmp/radio_caps` as this boot's only when it found
wireless and names an SDIO device (`caps_final`): one that names none is detected again at the next verb. None of that
reaches the flash: `caps_check` wants an SDIO id, and `radio_detect_key` says `unkeepable` for a unit with no card, for
no `md5sum` output, and for the md5 of nothing (every input unreadable), so a key that read none of its inputs never
stands for a later boot's.

**2. The order: the Bluetooth firmware beside the Wi-Fi driver.** `RADIO_BT_AFTER_WLAN` is read off the whole attach
script, so another branch's wait loop set it on the IW416. In Carlinkit's 2025.10.15.1127 scripts only the 0xc822
branch and the SD8987 one (0x9149/0x9141) wait for the Wi-Fi interface; the IW416 branch (0x9159) pushes its firmware
with `fw_loader_linux` and attaches without waiting, and that unit's dispatcher starts the attach script in the
background before it loads the Wi-Fi driver. So the stock firmware loads the two side by side at every IW416 boot.
`radio_detect.sh` section 4d reads this unit's own branch, as the SCO lines are read: `RADIO_BT_TOGETHER=1` where the
whole file waits, this unit's branch does not, and that branch pushes its own firmware before an attach the script
could extract (on the mapped backend, with a Wi-Fi mapping). No chipset table: of every attach script this project
has read, the shape selects the IW416 branch alone, and an id the dispatcher never names has no branch, so 0.
**Conservative since 21 Nov** (the review's C: the first rule looked for one exact wait loop, and a wait before the
dispatch, an `until`, a helper function, a quoted path or a dispatcher that starts the attach after the Wi-Fi driver
all passed it): any sign that the attach waits for the Wi-Fi side is today's order. Besides the above, the dispatcher
must start the attach script in the background at its top level (in no function or `{` group), before it inserts any
of this unit's Wi-Fi modules, with nothing before that start that waits, sleeps, sources a file or names the Wi-Fi side;
this unit's branch must have no loop or sleep (`while`, `until`, `sleep`), source nothing, call none of the attach
script's own functions and name nothing of the Wi-Fi side; and the rest of the attach script (outside every
`sdioCardID` branch: before the dispatch, after it, its functions, its top level) must source nothing and name nothing
of the Wi-Fi side, its `sdioCardID` chains all closed. "The Wi-Fi side": `/sys/class/net`, an interface name
(`wlan<n>`, `sta<n>`, `uap<n>`, `p2p<n>`), `ifconfig`, `iwconfig`, `iw dev`, or one of this unit's own Wi-Fi modules by
name, in any quoting, comments aside. The stock top level's own wait (for the Bluetooth driver's unpack,
`/tmp/*hci_uart.ko`) names none. On Carlinkit's 2025.10.15.1127 texts, the only stock texts this project has (the
factory rootfs copy; the truck's 2025.02.25 text is unread), the rule gives together for 0x9159 alone, as before: 0 for
0x9149, 0x9141, 0xc822, 0xb822, 0xb733, 0x4354 and 0x4358, under sh and dash. The
dev adapter's bench (20 Nov, power really removed, the preload off, one trial each): Bluetooth first, no Wi-Fi driver
loaded: `fw_loader_linux` 2.48 s, `hci0` present 0.12 s and answering 0.43 s after the attach began, then the Wi-Fi
driver (insmod 2.13 s, `sta0` and `wlan0`, "WLAN FW is active"), `hci0` still answering, `wifi_ap_on` rc 0 with
AP-ENABLED; both started at one instant: the Wi-Fi driver done 2.00 s on, `fw_loader_linux` 2.72 s (its own 2467 ms),
`hci0` answering 3.24 s after the start, `wifi_ap_on` rc 0 afterwards and `hci0` still answering.

`radio_hal.sh` `bt_order` picks the order at each attach: `together` (`RADIO_BT_TOGETHER=1` and not switched off),
`wlan_first` (today's, where the attach waits) or `bt_alone` (today's, where it does not). Together
(`bt_attach_together`): an in-flight mark on flash, the modules unpacked, the line discipline, then at one instant
the Wi-Fi driver in a subshell of its own (under its lock, `wlan_driver_up`, its interface waited for) and the
firmware push, then the attach, which does not wait for the interface; the Wi-Fi driver's load is joined before the
verb returns (clause 1). Today's order (`bt_attach_mapped`) is the code of before, moved into a function, unchanged.

- **Off** (today's order back): the owner's `/script/radio_together_off` on flash, or the bench's
  `/tmp/bench_together_off` (`box_common::bench_flag`'s rule: set unless it reads `off`), read at each attach.
  `ocbm_boot.sh` logs the owner's or the guard's file at every boot. The bench's file is in `/tmp`, so a power cut
  clears it and the boot's own preload never sees it: a cold-boot A/B uses the owner's file.
- **The fallback:** a together attach whose `hci0` never answers marks the boot (`/tmp/.radio_together_fell_back`)
  and says so ("together gave no responsive hci0 - this boot's next attach is Wi-Fi first"). The tries left in that
  bring-up (a `bt_on` has four, the preload one) take today's order from its start: the attach helper stopped (no
  reset yet), the Wi-Fi interface first, the firmware pushed again, the attach; later attaches that boot are Wi-Fi first
  (`off=fallback` on the boot line). A preload's failure leaves it to the host's `bt_on`, which the mark sends Wi-Fi
  first.
- **A failed attach's helper is stopped** (the 21 Nov review A). The attach helper (`hciattach ... any`) outlives an
  attach that failed, holding the UART with a dead `hci0` registered: the host's `bt_on` then spent its confirmation on
  that `hci0`, pushed the firmware into a UART the helper still held, and made its attempt 1 a reclaim+reset (the line
  discipline's rmmod and the gpio1 pulse) with the Wi-Fi driver loaded, the step radio_detect.sh's preload note records
  as wedging the kernel (19 Oct). Now a preload whose attach failed with no tries left stops its own helper as it ends
  (SIGTERM, then -9: no reset, no rmmod), and a `bt_on` that finds this boot's fallback mark stops any helper still
  there before its firmware push, so its attempt 1 is a plain attach, as the in-process fallback's always was. The
  first half holds in every order and on every chipset of the mapped backend: 2.6.5 left the same helper after a failed
  Wi-Fi-first preload, so there a host's `bt_on` after a failed preload now begins with a plain push and attach where
  it began with a reclaim+reset, and its attempts 2 to 4 reclaim and reset as before. The second half is the IW416's
  alone, as together is.
- **No attach without firmware** (22 Nov 2026: a cold boot on the bench whose loader exited 255, whose attach went on
  against a chip with no firmware, and whose box then fell silent until a power cycle). `bt_fw_push` runs the loader
  under a bound (10 s, then TERM, and KILL 2 s later; it had none), and once more when it failed. The bound is a
  watcher that looks every 0.5 s and signals the loader only while that run's token is there; the token goes the moment
  the loader returns, whose code and time are its own, and the watcher is never waited for (the hardware test of
  22 Nov, BusyBox ash: a TERM to the old watcher, sent as a loader that failed at once returned, was lost, and the
  bring-up waited out its 12 s and logged a stop at the bound). A push is good when
  the loader exits 0 or says `Download Complete` (with no `Download Error`), whatever its code, the bound's stop after
  it included (the code logged: the 2.7.1 review); a failure logs the loader's code into box.log, and its last lines
  (an address in them masked) unless the bound stopped it, which says so instead. The loader runs in the background
  under the bt lock, so a signal to the bring-up (a teardown's TERM) stops it, and reaps it, before the lock goes
  (`bt_fw_stop`, from `lock_take`'s traps). Two failures attach nothing: no `hciattach`, no reclaim+reset, `rc=1`;
  in the together order the fallback mark and the guard's count as for any together attach that gave no responsive
  `hci0`, so the host's `bt_on` takes today's order with a push of its own. A good push leaves
  `/tmp/.radio_btfw_pushed`, and a failure after it that boot does not stop the attach: the chip runs that firmware,
  which a SoC restart or a power cut takes away (both empty `/tmp`) and reclaim+reset's gpio1 pulse does not (20 Nov),
  and a loader before it finds no boot loader: on the dev adapter (22 Nov) the real loader returned 255 after about
  4.05 s, its 4 s wait for one, and with the push's mark there the attach went on and `hci0` answered about 0.5 s
  later. That is the together
  fallback's push, a host's `bt_on` after a preload whose attach failed, and a try after a reset, where the attach
  always went on, failed loader or not. Each reclaim+reset is followed by a push before its attach, a loader run per
  reset that 2.7.0 did not make: a full push if the pulse took the firmware away, else (most likely, as the pulse did
  not take it on 20 Nov) that 4 s wait for a boot loader, then the attach. The boot line counts the runs
  (`btfw_tries`).
- **The guard**, as the preload's: a together attach is bracketed by `/script/.radio_together_inflight` (written and
  synced before it, removed after), so one that never finished (power lost or the box hung inside it) is found at the
  next attach; each failed or unfinished one counts in `/script/.radio_together_failed`, two in a row write the owner's
  file with `guard:` in it, and the order is Wi-Fi first until the owner removes it. One that converges clears the
  count. A together attach that gave Bluetooth but no Wi-Fi interface counts as well; one whose controller answered
  never counts, however a later name read goes (the 21 Nov review J). An install and an update lift the guard's file,
  its count and its mark (a new version tries again), never an owner's own file (as the armed path's switch), and so
  does an update's trial, once, at its first start, for this guard and the preload's alike: recorded in the update's
  journal (`guards_lifted` in `/etc/ocbm/update_pending`), so a package an older updater placed gets its lift there,
  and the rest of the trial counts as any boot does. Until 21 Nov that lift came at every start of a trial not yet kept
  (each passing self-test deletes the start count it was keyed on, so every start read as the first): neither guard
  could trip while a trial stayed open, at 2.6.5 too for the preload's. A revert and the restores remove both, the
  owner's too, since stock reads neither (the restore's lists name all three: `radio_*` is `is_radio()`'s). The off file goes in whole, written
  beside it as `/script/.radio_together_off.new`, synced and renamed in (a plain `>` cut by a power loss left an empty
  file, which every reader takes for the owner's). A mark, count or off file that cannot be written (a full flash)
  never stops a bring-up: the attach goes on, uncounted, and the log says so (the review's B: a `: > FILE` there used
  to end the whole script, dash with 2, this script's "already converged").

**3. Where the preload starts, and the stock GPIO set-up.** The dispatcher runs Carlinkit's `init_gpio.sh` beside
`ocbm_boot.sh` at every boot, and it pulses the Bluetooth reset line (gpio1) before setting the charge pins. Pulled
under a live Wi-Fi driver that line wedged the kernel (19 Oct, radio_detect.sh's preload note). Until 20 Nov the
preload started after the gadget, `ocbmd` and the supervisor and spent 0.8 s detecting before its first insmod, well
behind the pulse; started early with the descriptor kept, it no longer would be. So the dispatcher marks that
script's end (`OCBM_GPIO_DONE`, `/tmp/ocbm_gpio_done`, written whether or not the stock script is there), and
`ocbm_boot.sh` then starts the preload first, beside the gadget's set-up, about 0.8 s sooner. The preload reads its
descriptor and unpacks the modules (nothing touches the chip), then waits for the mark (`gpio_wait`, 3 s at most, said
either way), then the chip. Where nothing marks that end (another start script), or with `/script/radio_preload_late`
on flash (the bench's A/B, the owner's way back), it starts where it always did. That file is an owner's switch (the
21 Nov review G): installs and updates keep it, as the other owners' switches; a revert and the restores remove it.
**The cost, unmeasured:** on this one
core the preload's descriptor read and unpack share the CPU with the gadget's set-up, which may be ready a little later
than 2.37 s; the host's session waits on the radios, not on the link, so the radios first is the order that gives
that session its Bluetooth soonest: even after this change Bluetooth answers about 2.5 to 3 s after a host's
SUBSCRIBE at 3 s, so a link a little later costs the session nothing. The bench
measures both (docs/ops/02_TESTING.md, "Cold boot: real power cycles", step 6).

**4. The host's path: the trace.** Before (2.6.5): `ocbm_boot.sh` starts the preload after `ocbmd` and the supervisor;
`radio_hal.sh preload` takes the wlan lock, then the bt lock, and holds both through `do_preload`: `radio_detect.sh`,
the Wi-Fi driver first, the firmware, the attach. The host's SUBSCRIBE (about 3.0 s) wakes the supervisor
(`wireless_up`, no arm: `arm_gate` wants `hci0` and no seam lock), whose wrapper starts `wifi_ap_on` in the background
and `bt_on`; each waits in `lock_take` (a look every 0.5 s) until the preload ends (about 10.4 s); `bt_on` then finds
`hci0` up and answering (rc 2, `bt_power`), btd starts, and `wifi_ap_on` configures and launches the AP (its
configuration not written ahead at a cold boot). After: the preload starts at about 2.0 s with the kept descriptor;
at the GPIO mark (about 2.3 s) the Wi-Fi driver and the firmware start together; once `sta0` is up the preload's
Wi-Fi subshell lets the wlan lock go (`wlan_free`), so a waiting `wifi_ap_on` brings the AP up beside the firmware push,
as on today's path when a host comes before the preload; `hci0` answers, the Wi-Fi load is joined, and the preload
drops its locks at once, before its bookkeeping, only while they are still its own (`lock_drop_own`: its pid in the
lock, or no pid at all in one this process took and has not let go, where its pid write failed: the 21 Nov review K).
`lock_take` looks
every 0.2 s (no fork but the sleep). No double load: the modules are unpacked once (`ko_unpack`: its own lock and a
stamp, where each caller used to untar on a missing file, so two side by side could insmod half a file), the Wi-Fi
driver once (`wlan_driver_up`'s lock, `/proc/modules`), the firmware and the attach once (the bt lock). No deadlock:
every process takes them in one order, wlan then bt then the driver's then the unpack's, the Wi-Fi subshell only the
last two, and every wait is bounded. **The estimate (not a measurement):** `hci0` answering at about 5.8 to 6.4 s of
uptime (the bench's 3.24 s from both loads' start, which the GPIO mark at about 2.3 s and the descriptor and unpack
put at about 2.4 s, plus the CPU the host's own start-up takes then, plus up to 0.5 s of the attach's look), and
`bt_power` about 0.4 s later (the name and SCO set, the preload's locks let go, the host's `bt_on` confirming): about
3.2 to 3.9 s after a host at 3.0 s, against 7.4 to 8.6 s. The AP: the Wi-Fi driver up at about 4.4 to 4.9 s, the
host's `wifi_ap_on` on at once, hostapd about 1 s on (the configuration is not written ahead at a cold boot) and
AP-ENABLED about 0.9 s after that, 6.4 to 6.9 s, against about 12.6 s on the truck (17 Nov); well ahead of the phone's
0x5702, which comes after btd's start, the page and the iAP2 set-up, several seconds after `bt_power`. **The bench**
(20 Nov, above) put `hci0` answering at 6.82 to 7.03 s and `bt_power` 0.72 to 0.85 s after it: the trace's 0.4 s was
short, and `page_start` came 1.60 to 1.93 s after `hci0` answered (section 6, opt-in, takes that gap out). **The
truck, corrected (21 Nov):** those benches had the host at 4.2 to 5.0 s; with it at 3.3 s, as on the truck (the bench
of `4a6ce9c`, boot 10), the radio loads ran about 1 s slower (`wlan_start` 4030, `btfw_end` 6990, `hci_up` 8010), the
host's own start-up sharing the one core with them. So on the truck expect `hci_up` near 8 s and, by default (no
handover), a cold picture near 16.5 s of uptime (that boot: 16.85), against about 19 s on 2.6.5; the estimate above
for a host at 3.0 s (`hci0` at 5.8 to 6.4 s) was too early.

**From 2.7.1** (round 4, the owner's decisions of 22 Nov 2026; each on by default, with `off` in its file for 2.7.0's
path: [../ops/02_TESTING.md](../ops/02_TESTING.md), "Round 4 switches"): `lock_take` looks every 0.05 s and the attach
every 0.1 s (`page_trim`, `attach_poll`); with a host waiting, the preload sets no SCO lines and no name on its attach
(btd's bring-up resets the controller and writes the name) and clears the together guard's mark after its locks
(`page_trim`); and the supervisor starts btd at the host's arrival, waiting for `bt_on`'s end, with the handover not
asked for (`btd_early`). The trace and figures above are 2.7.0's path.

**5. The boot line** (`[radio-boot] v1`, one per boot): its keys are documented beside the timeline's in
[../ops/02_TESTING.md](../ops/02_TESTING.md), "The boot's radio line".

**6. The handover: a waiting host's session takes Bluetooth as `hci0` answers (R3-5; opt-in, off by default since
21 Nov).** Everything below runs only when the owner asks for it (the opt-in, and the open question, at the end of this
section); by default the cold path is section 4's, but for the first item below: since 2.7.1 btd starts at the host's
arrival and waits by default, by round 4's `btd_early` with no handover asked for, so its wait ends at `bt_on`'s end
and its bring-up keeps the DOWN/UP ([../ops/02_TESTING.md](../ops/02_TESTING.md), "Round 4 switches"). On the 20 Nov
bench the
first page went out 1.60 to 1.93 s after `hci0` answered, in three parts (ms since the host subscribed, from
`[timeline] v1`): `hci_up` to `bt_power` 0.72 to 0.85 s, the preload's name and SCO set-up (about 0.37 s on its own),
its bookkeeping, then the host's `bt_on` behind the bt lock (a look every 0.2 s) and its confirmation; `bt_power` to
`btd_start` 0.22 to 0.31 s, the wrapper starting btd only after `bt_on`; `btd_start` to `bt_up` 0.56 to 0.78 s, btd's
start and its DOWN/UP (`down +205 ms`, `up +520 ms`) on a controller the preload had just brought up. Each part goes:

- **btd waits, started at the host's arrival.** With the preload running when a host comes (its last state `running`
  in `/tmp/radio_preload_state`), `wireless_up`'s wrapper starts btd first with `OCBM_BTD_WAIT=1`, then the host's
  `bt_on` beside it, then writes `/tmp/.btd_bt_on_done` once `bt_on` is over (`tools/session_supervisor.sh`, "The
  cold boot: btd before Bluetooth is up"). btd (`crates/vendor/wireless/src/handover.rs`) does what a btd does at its
  start up to its first bring-up (the control port, `/tmp/bt_phase`, the session's record, the AP watch), then
  sleeps in `poll()` on an inotify watch of `/tmp` and a pipe SIGTERM writes to (a backstop look every 250 ms), until
  the preload's handover, that file (today's bring-up, as a btd started after `bt_on`), or SIGTERM. **The wait is
  bounded** (the 21 Nov review I): with neither after 30 s (`handover::BOUND`; an early wrapper killed before it wrote
  the file, say), and once the seam's bt lock has looked free for 1 s (a live holder's lock, or one being taken, keeps
  it waiting: a `bt_on`'s tries can take minutes), btd logs it and takes the full bring-up, its DOWN/UP and all. The
  other wrappers (today's, the armed one) unset `OCBM_BTD_WAIT` before they start btd, so only the early start waits.
  The rest of the
  session's start (carplayd and the certificate read, the radio descriptor's log, the name) comes after the wait, off
  the attach's CPU and once the Wi-Fi interface is in. Nothing pages or scans before the session's bring-up, as in
  any btd.
- **The claim, as `hci0` answers.** `radio_hal.sh` `bt_handover_claim`, in the preload's attach loop with a host here:
  `hciconfig hci0 up` on the device the attach registered, and `t bt_power` stamped with the answer, as a `bt_on`'s
  own attach stamps it. **The attach leaves the kernel's auto power-off armed**: `hci_register_dev` sets
  `HCI_AUTO_OFF`, and the open it queues (`hci_power_on`) closes the device again 2 s on unless something claims it
  (`net/bluetooth/hci_core.c`, v3.14.52: `:3037-3038`, `:2387-2417`); HCIDEVUP clears that bit and cancels the close,
  then finds the device up (`hci_dev_open` `:1956-1980`, `-EALREADY` at `:1890`, which `hciconfig up` takes for
  success). Carlinkit's own IW416 branch runs that `up` right after its `hciattach` (the factory rootfs's
  `attach_bluetooth.sh`); `radio_detect.sh` extracts only the attach line, and btd's DOWN/UP covered the difference
  until now (on the bench it came about 1.3 s after the answer, before the close). No name and no SCO lines here: btd
  writes the identity (Simple Pairing, class, name, EIR), and the vendor's SCO set-up never outlived btd's DOWN/UP
  (`apply_sco`), so the session's controller is set up as before.
- **The handover, once the Wi-Fi side is in.** `bt_handover_give`, after the together attach joins its Wi-Fi load (at
  once in today's orders: `wlan_first` has its interface already, `bt_alone` loads none), so btd's name, from wlan0's
  address, is the one it always had: `/tmp/.radio_bt_handover` (`<handed ms> <hci_up ms> <dev>`), then the bt lock goes
  while it is this preload's, before its bookkeeping. From there the controller is the host session's, and the preload
  touches it no more: no `bt_off` at its end, whatever the host does next (its session's teardown powers it down).
- **One owner at a time.** The host's `bt_on`, waiting on the bt lock, finds a handover written since this host came
  (ocbmd's origin, `/tmp/host_present_uptime`, to the shell's 10 ms) and touches nothing, since btd may be paging on
  it already: rc 2, `bt_power` again (the first stands; this one is for a record btd began late), and "opened" in
  `/tmp/.radio_bt_warm` for a btd that goes on its return. btd reads the same file by the same rule, so both judge
  alike; the origin changes only with a host's departure and return, which ocbmd holds at 0 for 2 s (`REARM_HOLD`),
  so the supervisor tears the session down between. A handover from before this host came is another host's and goes;
  every `bt_off` (a teardown's) removes it.
- **No DOWN/UP, on 2.6.0's proof of state.** btd's first bring-up from the handover (`bt_bringup::from_handover`):
  one Read_Scan_Enable round trip (the armed go's proof, 150 ms, two tries), then the identity and the scans with no
  DOWN/UP. The proof `just_opened` rests on holds: the controller was opened from DOWN just now (the registration's own
  `hci_dev_do_open`, which set the SSP event mask), nothing has reset it since, and the claim, like `bt_on`'s own
  `up` from DOWN, leaves nothing to close it. A controller that does not answer gets the DOWN/UP, as before; a later
  session in the same btd (a phone switch) does its DOWN/UP as before (the "opened" word is spent unread).
- **Opt-in (21 Nov):** the owner's `/script/radio_handover_on` on flash, or the bench's `/tmp/bench_handover_on`
  (`box_common::bench_flag`'s rule: set unless it reads `off`), read by the preload at its attach and by the supervisor
  at the host's arrival. With neither, nothing of the handover runs: no claim, no handover file, the host's `bt_on` and
  btd's DOWN/UP as in section 4. The early btd does, since 2.7.1, by `btd_early` (on by default; `off` in
  `/etc/ocbm/bench_btd_early` takes it out), and it waits for `bt_on`'s end; with it off too, the path is exactly the
  one `4a6ce9c` took with its old off file (the same hardware calls, the same wrapper, byte for byte; the one log line
  reads `handover: off (not asked for ...)`). `radio_hal.sh
  probe` and the boot line say `handover=on_owner`, `on_bench` or `off`. That old off file, `/script/radio_handover_off`
  (4a6ce9c's switch; the dev adapter has one), is retired: nothing reads it, and an update, an update's first start
  (for a package an older updater placed), a revert and the restores remove it. A revert and the restores remove the
  owner's opt-in too (`radio_*` is `is_radio()`'s, so the restore's lists name both); installs and updates keep it.
- **Everything else as before:** a host before the preload (it stands down; the host's `bt_on` attaches), after it
  (it powered down; `bt_on` powers up, "opened"), the armed wake path, a failed armed go's fallback, a self-test wait
  (`waiting`, not `running`), a preload that converged before the host came, a failed together attach (the host's
  `bt_on` takes the fallback order, btd goes on its end), and a second session in the same btd. Where the early btd
  started and no handover came, it goes on `bt_on`'s end as a btd started after it.

**The estimate, then the bench.** Estimated before the bench: from `hci0` answering, the claim (one `hciconfig` exec,
about 30 ms), the Wi-Fi join (none: `wlan_up` came first), the handover file, btd woken at once, its round trip, the
conflicting daemons and the identity writes (about 40 ms), the scans, the reconnect's page about 11 ms after `bt_up`:
`page_start` about 0.1 to 0.15 s after `hci0` answers. **The bench of `4a6ce9c`** (21 Nov, the coordinating session:
the dev adapter, real power cuts by hand, the host up first, the owner's phone beside the adapter throughout; uptime in
s, `page_start - bt_power` in ms):

| Boot | Handover | `page_start` | page − `bt_power` | phone answered after | first frame | |
|---|---|---|---|---|---|---|
| 1 | on | 7.16 | 95 | 0.31 s | 12.95 | clean |
| 2 | on | 6.96 | 101 | 1.83 s | 13.95 | clean |
| 3 | on | about 6.93 | about 100 | 3.0 s | 111.5 | answered late, no SDP channel within 0.7 s: cut; the re-page `CONNECT_FAILED 0x09`; the second bonded phone took the session, joined Wi-Fi 52 s later, never started CarPlay, and the 90 s rule ended it; then the first phone connected normally |
| 4 | on | 7.52 | 88 | 2.18 s | 55.5 | the SDP search took 0.93 s, then `RFCOMM connect to channel 1 failed: connect timed out` (8 s), four "answered, no SDP channel" cuts, a link lost with reason 0x01, 3 absent pages; connected at about 51 s (the second phone's Bluetooth off for this and later boots) |
| 5 | on | 7.09 | 98 | 0.41 s | 13.09 | clean |
| 6 | on | 7.09 | 110 | 1.80 s | 13.68 | clean |
| 7 | off | 8.91 | 1036 | 0.61 s | 14.37 | clean |
| 8 | off | 8.71 | 1045 | 1.32 s | 14.50 | clean |
| 9 | off | 8.53 | 946 | 2.21 s | 15.29 | clean |
| 10 | off | 9.60 | 1017 | 2.44 s | 16.85 | clean; the host at 3.3 s (the truck's timing), the loads about 1 s slower |

In every handover boot `handed - hci_up` was 30 to 40 ms, btd started before `bt_power`, and the controller stayed up:
the mechanism did what it was built to do, and the estimate held. With it on and a clean boot, the phone then waited
for the access point (`ap_enabled` at 8.5 to 9.5 s of uptime, `svc_seen` up to 1 s after `wifi_rsp`): the AP is the
next limit.

**OPEN: the handover, 2 of 6 boots with link trouble against 0 of 9 without, cause not established, off by default.**
The DOWN/UP path was clean in 9 of 9 (`70caa3c`'s 5 and these 4); the handover in 4 of 6. Suggestive, not proof: the
same "late answer, no SDP channel" cut happens on released 2.6.5 on the truck, and dmesg showed no Bluetooth error.
A proper test needs many real power cycles (tens per arm, alternating `/script/radio_handover_on` and none, one bonded
phone, the second phone's Bluetooth off), which by hand the owner cannot keep doing: a switched power supply or a
relay on the adapter's USB power would make it a bench run. The suspects: no second controller init after the attach
(the controller only ever saw the registration's `hci_dev_do_open`, where the DOWN/UP path adds a reset and a full
second init before the page); the page overlapping the access point's bring-up (with the handover the page goes out
about 1 s sooner, while hostapd is starting on the same IW416); or the phone (its Bluetooth state or the second bonded
phone). Each has a discriminating run: the handover with a DOWN/UP put back in btd (keeps the early start, removes the
first suspect); the handover with the page held until `ap_enabled` (the second); and the phone's own log over USB
(`idevicesyslog`, its `bluetoothd` lines) for the third.

**Not proven:** the together order on the truck's 2025.02.25 vendor scripts (the shape is read on the unit: the boot
line's `order=` and `radio_hal.sh probe` say which it took); the AP's bring-up beside the firmware push from the
preload (the bench brought it up after `hci0` answered; today's path does it beside the push when a host comes first);
what the early start costs the gadget's enumeration; the SDIO card being enumerated by the time the preload starts at
about 2.0 s (the self-test, which starts as early, has always found it); and every number in the estimates above. For
the handover (opt-in): why 2 of its 6 boots had link trouble (the OPEN question above); the kernel's auto power-off
on this unit (read from the v3.14.52 source, not observed: the bench's `hciconfig hci0` 2 to 3 s after an attach with
no host would show `DOWN`); a session with no DOWN/UP after a fresh attach pairing a new phone (the test of the event
mask); what btd's earlier start costs the attach; and the handover on any chipset but the IW416. From the 21 Nov
review's fixes, read off the code and run against stubs only: that a stopped helper takes its dead `hci0` with it and
leaves the chip a plain push and attach can bring back (A); how long the SDIO card takes to appear at a power-up (D;
`waited about N ms for the radio's SDIO card` in the box log says); and the handover wait's bound (I), which no bench
has reached.

---

## Accessory-initiated BT reconnect

<!-- absorbed: ../wireless/01_BT_AND_RADIO.md -->

Goal: the box should reconnect a **known/bonded** iPhone to wireless CarPlay on boot with **no user
interaction** — the behavior native head units and the stock Carlinkit firmware both have. This doc
records what was tried on hardware, what was eliminated with evidence, and the one path left to
implement (**Model B**). It is the authoritative pickup point; do not re-derive the dead-ends.

---

### ✅ RESOLVED 2026-08-01 — Model B works, device-proven end to end

> **⚠️ The "on boot, with zero user interaction" TRIGGER is superseded; the mechanism is not.**
> Radios power on only on app command and off on app command or app loss; once on, auto-connect to
> a bonded device is correct. The re-gating is CLOSED — the guard is on `wireless_up` itself, and
> the `wired_iphone_on_usb` and per-call-site strategies both failed. Full reasoning:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-51-1`–`R-51-3`.

The box now boots, autonomously pages the bonded phone, connects OUT, and brings up wireless CarPlay
with **zero user interaction**. Implemented in `crates/vendor/wireless/`: `rfcomm::connect_to` +
`sdp_client.rs` (new) + `reconnect.rs` (new) + `ssp_agent::bonded_addrs` + `main.rs` wiring. The
byte-pinned Identify and the accept path are untouched — purely additive.

**The one fact that unlocked it — the phone's iAP2 service UUID is NOT the accessory's.** Our
`sdp_server.rs` advertises the accessory-side iAP2 UUID `00000000-deca-fade-deca-deafdeca**caff**`.
Searching the *phone's* SDP server for that UUID returns an empty `35 00` — iOS does not expose it.
Browsing the phone's **full** SDP catalog (L2CAP-UUID `19 01 00` search, 1889 bytes) shows iOS
actually exposes its iAP2 endpoint as service **"Wireless iAP v2"** under UUID
**`02030302-1d19-415f-86f2-22a2106a0a77`** on **RFCOMM channel 1**. (Two red herrings sit next to it:
the accessory UUID ends `…caff`; iOS's *"Wireless iAP5"* service is `…deca**cafe**`, one byte off; the
real iAP2-v2 service is the unrelated `02030302…` UUID.) `sdp_client.rs` searches for that UUID,
reads channel 1, and `rfcomm::connect_to` opens it.

**iOS's exposed BR/EDR SDP catalog (device capture 2026-08-01), for reference:**

| Service | ServiceClass UUID128 | RFCOMM ch |
|---|---|---|
| **Wireless iAP v2** (this is the one) | `02030302-1d19-415f-86f2-22a2106a0a77` | **1** |
| Wireless iAP5 | `00000000-deca-fade-deca-deafdecacafe` | 1 |
| MAP MAS-iOS | (0x1132) | 2 |
| Phonebook (PBAP) | (0x112f) | 13 |
| Handsfree Gateway | (0x111f) | 8 |
| GATT / AAP Client / AVRCP / A2DP Source | — | — |

**Working flow captured on the box (`/tmp/wl.log`):**
```
[sdp-client] iAP2 RFCOMM channel on the phone = 1
[reconnect] RFCOMM connected OUT to the phone (ch 1) — starting iAP2 handshake
[bt-driver] SYN-ACK -- link up
[bt-driver] RX 0xAA00 -> CertSent ... AuthSuccess ... RX 0xAA05 -> Authenticated
[bt-driver] TX 0x1D01 IdentificationInformation (301 B, wireless transport)
[bt-driver] IdentifyAccept ... RX 0x1D02 -> Identified
[bt-driver] RX 0x5702 RequestAccessoryWiFiConfig -> replying 0x5703 (ssid="ccpa-b0df" ch=36 …)
[bt-driver] RX 0x4E0E DeviceTransportIdentifier: param0=<phone bdaddr>, param1=<phone UDID>
[av] started /usr/sbin/carplayd (detached) → transport=wireless → CarPlay active
```
Also learned, correcting two of this doc's eliminated-models notes: **an active SDP transaction TO the
phone holds the ACL** (Model A's idle L2CAP got reaped; a live ServiceSearchAttributeRequest does not),
and **the accessory drives everything** — iOS never self-initiates (accessoryd stays silent until the
outbound RFCOMM + iAP2 SYN arrives), which is why Models A and the bare page could never have worked.

Everything below is the pre-solution investigation, kept for the record.

---

### Ground truth
- The owner confirms native CarPlay AND the original Carlinkit CPC200-CCPA firmware autoconnect wireless
  on boot. So the feature is real and the mechanism is discoverable — per CLAUDE.md, "Carlinkit does X"
  means X can be made to work.
- Test rig this session: box BT bdaddr redacted (the box advertised `CarLink-b0df`); phone bdaddr and
  UDID redacted (iPhone18,4, iOS 27.0). Bond persists at
  `/etc/carplay/bt_link_keys` (25-byte record, JFFS2). iPhone connected to the Mac over USB with Apple's
  CarPlay/iAP2/BT/WiFi diagnostic profiles installed → `idevicesyslog -u <udid>` gives iOS's own view
  (this is the method that made the difference; keep using it).

  (Addresses redacted 2026-08-16, same rationale as commit 42f5052: this repo is public and a BT
  address is broadcast over the air and trackable.)

### What the working implementations do (stock Carlinkit + SpeedPlay, cited in the session research)
On boot, page the bonded phone by stored MAC, then **the accessory becomes the RFCOMM CLIENT and opens
the iAP2 channel TO the phone**, then drives a fresh iAP2 Identify (never a resume — CINEMO R25 §2.15).
Stock: `FastConnect`/`Bluetooth_ConnectStart` + "Page success | Outgoing page completed" +
`SDPToolSearchEnd` (SDP *client* search of the peer) + literal "BT paging and **RFCOMM connection to the
phone**". SpeedPlay `libCarplayJni.so`: `open_iap_rfcomm`, "start connect iap rfcomm", to `g_remote_addr`
(REMOTE_BTMAC). R14G17 SDK is Bonjour-only at this layer (2017 drop, silent on BT transport).

### Eliminated on hardware (do NOT retry these)
1. **Bare HCI page** (`hcitool cc` / raw Create_Connection). Page works — `DEVICE_CONNECTED`, iOS
   recognizes `SupS < WirelessiAP CarPlay >` and even starts to SDP-query the box — but the ACL is
   **channel-less and BlueZ idle-reaps it** (`DEVICE_DISCONNECTED reason=0x02`, local host) in ~seconds,
   before iOS finishes. iOS does **not** spontaneously start CarPlay from a bare inbound page.
2. **Model A: page + hold via an L2CAP connect to the phone's SDP PSM (0x0001).** Implemented
   `rfcomm::l2cap_connect` (raw `AF_BLUETOOTH`/`SOCK_SEQPACKET`/`BTPROTO_L2CAP`, `sockaddr_l2`,
   `SO_SNDTIMEO`); the connect pages + holds the ACL. iOS **accepts** the channel
   (`RecvConnectReq psm=1`), but then **sits completely idle for the full 12 s hold** — it never
   SDP-queries us, never opens RFCOMM to our server, never starts CarPlay — then the link drops when we
   release. **iOS will NOT self-initiate; the accessory must drive the connection. Model A is refuted.**
   (First cut also had a bug: `mark_connected` latched on our OWN outbound `DEVICE_CONNECTED` — same
   bdaddr — collapsing the hold to ~0 s; fixed to hold the full window, same negative result.)

### THE PATH LEFT — Model B (accessory drives RFCOMM to the phone)
The box must, per the stock firmware: (1) page the bonded phone (or let the RFCOMM connect page it
implicitly); (2) **SDP-query the phone** to discover its iAP2/EA RFCOMM service + channel — the box has
no `sdptool` (only `hcitool`/`hciconfig`), so implement a minimal **SDP client** over L2CAP PSM 0x0001
(the crate already hand-rolls an SDP *server* in `sdp_server.rs` — the encoding is symmetric); (3)
**RFCOMM-connect OUT to the phone's iAP2 channel** (add `rfcomm::connect_to(peer, channel)` mirroring
`open_listener`; the `l2cap_connect` scaffolding from this session — `sockaddr`/`connect`/`SO_SNDTIMEO`
pattern — is the reusable base); (4) hand the connected socket to the existing
`bt_driver::run(File, …)`, which already runs the full iAP2 Identify → 0xAA auth → 0x5702/0x5703 WiFi
handoff over any connected RFCOMM socket. Keep the server/accept-path 100% intact (needed for first-time
pairing and phone-initiated connects); Model B is purely additive. Do NOT touch the byte-pinned Identify
(`bt_driver.rs`/`message.rs`).

Open unknowns for the Model-B session (resolve with the iPhone-log rig):
- Does iOS expose an iAP2/EA **RFCOMM service** for the accessory to connect to on reconnect? The
  session's capture search was inconclusive. The SDP-client query in step (2) answers this directly —
  and if iOS exposes no such service, the stock "RFCOMM connection to the phone" may target a different
  service/UUID, or the whole flow may hinge on the SDP *search* itself keeping the ACL alive long enough
  for a different iOS action. Watch `idevicesyslog -p accessoryd -p carkitd` during each attempt.
- Whether an active SDP *transaction* (a real ServiceSearchAttributeRequest, vs. the idle channel that
  got reaped) is what holds the ACL and prompts iOS forward.

Design notes carried over: retry with bounded backoff (10→60 s), spawn only when bonds exist, keep the
accept-path as the fallback, and remember the "our own outbound connect fires `DEVICE_CONNECTED` for the
peer bdaddr" gotcha — the real connect-success signal is the **RFCOMM accept / iAP2 progress**, not the
mgmt `DEVICE_CONNECTED`.

### Status of the code
The Model-A `l2cap_connect` + reconnect scaffolding was **not committed** and is being reverted from the
working tree; the box is rolled back to the item-1 (committed) `btd`. Re-create the socket
scaffolding from this doc for the Model-B session.

---

## Remaining task list (as of 2026-08-01) — ordered

Landed & hardware-validated this session: audit remediation (Phase 0/1), the ocbmd OutQueue refactor,
the lost-command chain (C1/C4), wired↔wireless switching (item 1) + the Hot-Handover/Standard opt-in.

1. **Item 3 — V1 AVCC fast path + V4 host backpressure** (host Swift; biggest host-CPU win; no BT
   unknowns; testable on the live box).
2. **Item 4 — structural**: delete the dormant `Protocol/` layer (extract `TouchAction`/`CommandID`/LE
   helpers to `InputTypes.swift` first) + build the OCBM test suite (unblocked by the coordinator split)
   + add a TSan scheme.
3. **Deferred smaller items** (from docs/ops/05_AUDITS.md): eld-codec `AACENC_SBR_MODE=0` (ELD ASC, needs the phone);
   `CT_INPUT_NACK` protocol addition; `connect_seam` numeric-only (E); ocbmd `opt-level=2` (measure-first).
4. ~~**Item 2 (LAST) — accessory-initiated BT reconnect via Model B** (this doc).~~ **✅ DONE
   2026-08-01, device-proven** — see the RESOLVED banner at the top. SDP client + RFCOMM client +
   reconnect orchestrator landed; the phone's real iAP2-v2 UUID (`02030302-…`) on RFCOMM ch 1 was the
   unlock.

---

## HCI event-mask pairing regression

<!-- absorbed: ../wireless/01_BT_AND_RADIO.md -->

STATUS: FIXED in source and at HEAD, verified by review against the real kernel sources. Shipped in
`6425a7a` as part of the 2026-07-25 QC remediation batch (see §5 for the rest of that batch).
**Live-hardware confirmation: strong indirect evidence only — see the note below.**

> **⚠️ The "pending live-hardware confirmation" caveat is DOWNGRADED, not retired (2026-08-16).**
> docs/wireless/01_BT_AND_RADIO.md §6b's 2026-08-15 RTL8822CS session almost certainly included a fresh SSP pairing, which is
> inference from a working session rather than a logged pairing — and it exercised only the
> **daemon-side** half of the fix (`bt_bringup.rs`); the `attach_bluetooth.sh` half remains
> reviewed-not-retested on IW416. Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-41-1`.

### The symptom

From `SESSION_HANDOFF_2026-07-24.md`: the box becomes discoverable and stays so (`hciconfig hci0 name`
reads `'CarLink-b0df'` steadily, `btd` running, SDP/SSP bring-up clean in `/tmp/wl.log`).
The iPhone **sees** the device in Settings > Bluetooth and taps it. Every attempt reports **"Pairing
Unsuccessful."**

Evidence gathered at the time:
- `/tmp/wl.log` shows the phone connecting to the SDP PSM and browsing the "Wireless iAPv2" record —
  `ServiceSearchRequest` → `ServiceAttributeRequest`, three full cycles — then closing.
- **No pairing-related mgmt event is ever logged.** `ssp_agent.rs`'s event loop never fires; only its
  startup `SET_POWERED`/`BONDABLE`/`CONNECTABLE`/`SSP`/`IO_CAPABILITY` lines appear.
- `dmesg` shows zero BT ACL/pairing/auth events.

### Two pieces of that evidence were red herrings — correct the record

1. **"SDP browsed 3× then closed" is NORMAL.** The known-good 2026-07-14 hardware log
   (`scratchpad/boxlogs/wl.log`) shows the identical three-cycle browse-then-close before *every*
   successful session. The regression is entirely in what happens after the close: good = RFCOMM
   connect; broken = nothing.
2. **"No `EV_USER_CONFIRM_REQUEST`" proves nothing.** On this kernel a Just-Works bond with
   NoInputNoOutput is auto-accepted **in-kernel** — `hci_user_confirm_request_evt`
   (`hci_event.c:3144`) sends `HCI_OP_USER_CONFIRM_REPLY` itself and never calls
   `mgmt_user_confirm_request`. That event would not arrive even on a fully successful pairing. Only
   `NEW_LINK_KEY` would.

### Root cause

`ccpa/rootfs/script/attach_bluetooth.sh`'s `attach_bt()` ended with:

```sh
hcitool -i hci0 cmd 0x03 0x0003
```

OGF 0x03 / OCF 0x0003 = **HCI_Reset (0x0C03)**, issued on a raw socket behind the kernel's back, as the
*last* controller operation of the function.

HCI_Reset restores the controller's **spec-default event mask** (`0x00001FFFFFFFFFFF` — events
0x01–0x2D only). The SSP events live above that range:

| Event | Code |
|---|---|
| IO_Capability_Request | 0x31 |
| User_Confirmation_Request | 0x33 |
| User_Passkey_Request | 0x34 |
| Simple_Pairing_Complete | 0x36 |

These are enabled **only** by the kernel's init-time `Set_Event_Mask`, and — verified directly against
the v3.14.52 sources — that call is reachable by exactly one path:

- `hci_setup_event_mask` (`hci_core.c:1081`) sets `events[6] |= 0x01/0x04/0x08/0x20` iff
  `lmp_ssp_capable`.
- It is called only from `hci_init2_req` (`hci_core.c:1174`), reachable only via `__hci_init`
  (`:1364-1391`), reachable only from **`hci_dev_do_open`** (`:1852`) — i.e. an HCIDEVUP on a **DOWN**
  device.
- `hci_dev_reset` (`:2108-2151`) — what `hciconfig hci0 reset` invokes — purges queues and issues a
  bare reset. It does **not** re-init and never touches the event mask.
- `hci_dev_do_open` returns `-EALREADY` when `HCI_UP` is already set (`:1890-1891`), and BlueZ's
  `hciconfig up` swallows `EALREADY` and exits 0 — which is why `bt_bringup`'s existing
  `run(&[hci_dev, "up"])?` "succeeded" while re-initializing nothing.

**Net effect:** SDP and ACL keep working (their events are inside the default mask), the box looks
completely healthy, `sspmode` reads back Enabled, name/class/EIR/piscan are all applied — but when the
iPhone initiates fresh SSP pairing, the controller never delivers IO_Capability_Request to the host,
the kernel never replies, and LMP pairing times out phone-side. "Pairing Unsuccessful", with zero
evidence at the mgmt layer and zero in `dmesg`.

### Why it looked like a regression from a specific date

Bonded **reconnects** use Link_Key_Request (0x17), which *is* inside the default mask — so a
previously-bonded phone reconnects fine with a wiped mask. That is exactly what the 2026-07-14 log
shows. The 30-minute soak on 2026-07-17 (docs/wireless/00_WIRELESS_CARPLAY.md) evidently rode a bring-up ordering in which a kernel
re-init landed last.

docs/wireless/01_BT_AND_RADIO.md's `bt_on.sh` fix then made the bring-up sequence deterministic — and the raw reset became
reliably the final word. The pairing failure went from intermittent to every-time. It also retroactively
explains docs/wireless/00_WIRELESS_CARPLAY.md's "accumulated IW416 controller wedge... cleared by a power-cycle": a power cycle
forces a genuine cold `hci_dev_do_open`.

Supporting detail: line 21 also undid the two vendor commands immediately preceding it (SCO routing,
BLE power) — strong evidence it was a cargo-culted leftover rather than intentional.

### The fix

1. **`ccpa/rootfs/script/attach_bluetooth.sh`** — removed the trailing raw HCI_Reset entirely. Added
   `hciconfig hci0 down` + `up` after the `hciconfig hci0 reset` in the MAC-programming branch, since
   that reset has the same defect. (Bonus: the `up` makes the kernel re-read BD_ADDR, so
   `/sys/class/bluetooth/hci0/address` now reflects a freshly programmed MAC instead of a stale cache.)
2. **`crates/vendor/wireless/src/bt_bringup.rs`** — `bring_up()` now does a best-effort `down` before
   `up`, forcing `hci_dev_do_open` regardless of what the boot scripts did to the controller. This makes
   the daemon self-sufficient rather than dependent on script history.

#### Side effect — accepted

The controller is `hciattach`'d over UART, so it carries `HCI_QUIRK_RESET_ON_CLOSE` — the `down` makes
the kernel issue a real HCI_Reset on close, and the `up` re-reads `Read_Buffer_Size`. That discards
`attach_bluetooth.sh`'s SCO setup (`scomtu 240:32` plus the SCO-routing and BLE-power vendor
commands), which is deliberately not re-issued: CarPlay carries all audio — including telephony —
over the AirPlay/WiFi session, never over SCO/HFP, so nothing in the proven path depends on them.
(From 2026-09-03 to OCBM 1.0.5 btd re-applied the SCO lines after the `up` through `radio_hal.sh
sco_on`, for wireless Android Auto's Bluetooth call audio. Both are retired.)

Note also that `bring_up` runs **once per active session** (re-entered from the arbiter loop on every
wired→wireless handback), not once per process. The down/up is safe because within
`run_active_session` the mgmt socket, SDP listener and RFCOMM listener are all spawned *after* it
returns, the previous session's threads are joined *before* re-entry, and `noscan` blocks inbound ACLs
during the quiet period.

### Diagnostics added alongside (`ssp_agent.rs`)

The failure was undiagnosable from the box because the mgmt event loop's `_ => {}` silently dropped
every unrecognized event. Now:

- The catch-all logs event code, name, controller index, param length and hex params.
- Explicit arms log `DEVICE_CONNECTED` / `DEVICE_DISCONNECTED` / `CONNECT_FAILED` / `AUTH_FAILED` with
  bdaddr and reason/status. All byte offsets verified against v3.14.52 `mgmt.h`.
- `LOAD_LINK_KEYS` now reads and logs its completion status. This matters twice: the kernel rejects the
  whole load on the first record whose `addr_type != 0x00` (BDADDR_BREDR), **and** a rejected load also
  skips the kernel's own `hci_link_keys_clear`, leaving stale kernel keys that can then fail BR/EDR auth
  with no local explanation.
- Removed a `param_len < 6` pre-filter that discarded every short event *before* the match — part of why
  the loop appeared never to fire.
- Corrected the module doc, which claimed the agent "auto-accepts every pairing confirmation". On this
  kernel it never sees one; it is really the settings setup, the NEW_LINK_KEY persistence path, the
  Numeric-Comparison code publisher, and (now) the connection/auth event logger.

### SSP pairing design (current)

`ssp_agent.rs` (`crates/bt-common`) does more than the connection/auth logging above:

- **Bdaddr redaction.** Any bdaddr in a log line carries only its last two octets, never the full
  address.
- **Link-key load.** One line per persisted record on load: `..<tail> addr_type=<n>
  key_type=<name> (0x<hex>)`, named per the BlueZ mgmt "Link Key Type" values (`0x00` combination,
  `0x03` debug, `0x04` unauth_p192, `0x05` auth_p192, `0x06` changed, `0x07` unauth_p256, `0x08`
  auth_p256). `EV_NEW_LINK_KEY` is logged unconditionally, including `store_hint=0` (controller
  declines storage). `EV_AUTH_FAILED` (`[bdaddr 6][addr_type 1][status 1]`, status at param offset
  7) is a named arm, not just the hexdump fallback.
- **Rejection cap.** `ssp_agent` counts `EV_USER_CONFIRM_REQUEST`s per (bdaddr, connection); it
  still replies to every one, but after 3 rejections with no intervening `NEW_LINK_KEY` it logs
  once and publishes `BTP_PAIR_REJECTED` (`ocbm-proto`) via a callback hook `ssp_agent::run` takes
  (`bt-common` has no dependency on `ocbm-proto`/`bt_driver`; the wireless daemon wires the hook to
  `bt_driver::publish_bt_phase`). The counter resets on `DEVICE_DISCONNECTED` or `NEW_LINK_KEY`.
- **Connect hold.** `bt-common::rfcomm::connect_to` goes through `pairing_aware_connect`:
  connect non-blocking and poll. While `/tmp/pairing_code` is fresh for this attempt (mtime ≥ the
  attempt's start; `ssp_agent` writes it on `USER_CONFIRM_REQUEST`) the deadline stretches to
  `rfcomm::PAIRING_HOLD_SECS` (60 s) so the phone-side tap can land, latched for the rest of
  the attempt. When the rejection streak fires, `ssp_agent` also clears the code and writes
  `/tmp/pair_rejected`; `connect_to` sees it and returns `PermissionDenied` immediately.
  `reconnect.rs` maps that to `Attempt::PairRejected`: stop paging the remaining bonds, sleep
  `BACKOFF_MAX_SECS` (60 s), then clear the flag so the next attempt is judged on its own.
  `NEW_LINK_KEY` also clears the flag; `DEVICE_DISCONNECTED` clears the code.
- **A phone that answers and resets.** `Connection reset by peer` from the SDP query or the RFCOMM
  connect is `Attempt::Reset`: the phone took the page and then dropped the link, so it is there but
  not ready. `reconnect::walk` pages it once more `RESET_RETRY_SECS` (2 s) later, before the next phone
  in the call order (an app's Connect request or a session that comes up meanwhile ends the walk
  instead). Dev bench, 2 Nov: 32 s after a power cut the first-called iPhone accepted the page and
  reset it 4 s later, and the walk handed CarPlay to the second phone; `FIRST_RETRY_SECS` only helps
  when no phone in the round connects.
- **Bonding intent.** `connect_to` requests `BT_SECURITY_MEDIUM` (general bonding) on the outgoing
  socket (`rfcomm::request_bonding`) so a box-initiated connect's own side asks for a
  persistent key (`store_hint=1`); without it the kernel reported `store_hint=0` and a freshly
  authenticated key was never written, so the next boot would have prompted again. `ssp_agent`
  still honours whatever hint the kernel returns, as BlueZ does.
- **`SET_DISCOVERABLE`.** `bt_bringup`'s `hciconfig piscan` can be undone by mgmt's
  `SET_CONNECTABLE` rewriting Scan_Enable from its own flags. `ssp_agent` sends
  `SET_DISCOVERABLE=general` (no timeout) after `SET_CONNECTABLE` so a phone-initiated re-pair from
  Settings ▸ Bluetooth can still find the box.
- **`confirm_hint`.** `ssp_agent` honours the mgmt `confirm_hint` byte: `1` means the kernel
  resolved the pairing to Just-Works (auto-accept, no code), `0` means a human comparison is
  required — regardless of our own IO capability, as the SSP rules require. Just-Works is always
  auto-accepted, unaffected by anything below.
- **Numeric comparison: default is to confirm our side immediately.** In numeric mode
  (`confirm_hint == 0`) `ssp_agent` publishes the code to `/tmp/pairing_code` and replies
  `OP_USER_CONFIRM_REPLY` at once — this is the default and needs no app attached. Behind
  `BT_SSP_INTERACTIVE=1` (host YAML `pairing: numeric_comparison_interactive`, app Settings
  "Answer the pairing code in this app") the agent instead waits for a real answer from the head
  unit: it records a pending confirm `{bdaddr, addr_type, deadline = now + 55 s}`
  (`PAIR_CONFIRM_WAIT_SECS`, inside `PAIRING_HOLD_SECS` so the connect hold is still open when the
  answer lands) and replies to nothing until the macOS app sends `CT_PAIR_CONFIRM` (`0x1C`,
  `[accept u8]`, docs/carplay/01_OCBM_PROTOCOL.md) → ocbmd → btd's control port
  (`127.0.0.1:9115`, `{"cmd":"pair_answer","accept":true|false}`, `control.rs`). Pair →
  `OP_USER_CONFIRM_REPLY`; Cancel or no answer in 55 s → `OP_USER_CONFIRM_NEG_REPLY`, raising
  `/tmp/pair_rejected` and clearing the code so the connect hold aborts and the reconnect driver
  backs off instead of retrying into the same prompt. `DEVICE_DISCONNECTED` or `NEW_LINK_KEY` for
  that bdaddr drops the pending confirm; a repeat request from the same phone keeps the original
  deadline; a second phone's request supersedes the first (answered NO); an answer with nothing
  pending is dropped, never banked. The rejection cap above still fires at 3 confirms with no bond,
  but in this mode replies NO — the code is already off the screen. This lever is bench-only
  (`tools/session_supervisor.sh` must never set it outside of `pairing_interactive()`'s YAML read).

**Measured against iOS 27 (device captures, 2026-09-03/04):**

| Who initiates the connect | Who confirms | iOS result |
|---|---|---|
| Box (reconnect/re-pair) | Box confirms immediately (default) | iOS shows its code sheet, user confirms on the phone, key = `auth_p192` |
| Box (reconnect/re-pair) | Box waits for the head unit's own yes/no (`BT_SSP_INTERACTIVE=1`) | iOS returns `pairingComplete result:162` ~0.3 ms after its own confirm request — no sheet ever shown, even with Settings ▸ Bluetooth open |
| Phone, from Settings ▸ Bluetooth | — (Just-Works, iOS offers NoInputNoOutput) | Pairs immediately, key = `unauth_p192`, no code anywhere |

iOS will not hold a box-initiated numeric-comparison request open for a human on the accessory side
to answer, so the spec-literal both-humans flow is unreachable with iOS as the peer and lives only
behind the bench lever above. The one path where the human answers on the phone with the box
merely displaying a code is box-initiated + immediate self-confirm (row 1); the only path where the
user pairs from the phone's own Settings is Just-Works (row 3).

### If this does NOT fix it — the discriminating evidence

The new logging makes the next test conclusive rather than another guess:

- **Only `DEVICE_CONNECTED`/`DEVICE_DISCONNECTED`, nothing else** → still an event-mask problem.
- **`AUTH_FAILED` (0x0011)** → stale/mismatched link key. Check `/etc/carplay/bt_link_keys` and the
  iPhone's **Settings ▸ General ▸ CarPlay** list (CarPlay pairings persist there independently of the
  Bluetooth entry). Moving the key store aside and restarting `btd` clears both sides.
- **Nothing at all, not even 0x000B on tap** → event delivery itself is broken; the mask hypothesis
  gains further weight.

A zero-redeploy on-box probe also exists: `hciconfig hci0 down && hciconfig hci0 up`, then restart
`btd` and retry pairing.

**Do not build an HFP SDP record on the current evidence.** The standing hypothesis that the Class of
Device (`0x200408`, Audio/Video Hands-free) needs a matching HFP/A2DP SDP record — the box serves only
the iAP2 SPP record — cannot be the cause of a *regression*: that exact combination paired and ran a
30-minute session on 2026-07-14/17. It would require the phone's behavior to have changed.

### §5 — the rest of the 2026-07-25 batch

This document covers the blocker. The same batch also fixed, from a 12-agent code audit:

- **`server.rs`** — the pre-pair-verify plaintext accumulator was unbounded; any peer with network
  adjacency to `[::]:5000` could stream header-less bytes until allocation failed and `panic = "abort"`
  killed `carplayd`. Now capped like the encrypted path.
- **`carplayd/main.rs`** — its `LocalMfiSigner` was the only MFi chip user taking no
  `/tmp/carplay_mfi.lock`, while `iap2d`, `btd` and `mfi-i2c-local` all do. Both stateful
  sequences now hold a bounded (10 s) flock.
- **`ocbmd`** — frame splicing: a queue resting mid-frame under USB backpressure (the normal state for
  4K video) let the next `drain()` write a complete CTRL/audio frame into the middle of it. Fixed with a
  `Drain{Done,Partial,Blocked}` result and wire-ownership tracking.
- **`iap_tunnel.rs`/`events.rs`** — the tunnel established a full link session and then abandoned it:
  subscribes went out bare and inbound frames were never ACKed again. Now link-framed (built
  just-in-time so seq/ack are current at transmission) and serviced past Identify.
- Plus: per-stream RTCP flag, stereo RTP timestamp, `CT_SRC` clamp, `Kind::Acc` EOF handling,
  `conns.clear()` on teardown, the removal of two undeclared wired subscribes, and the wired params-6/7
  byte-pin test that had never existed.

---

## HISTORICAL — bring-up race and health check

<!-- absorbed: ../wireless/01_BT_AND_RADIO.md -->

STATUS: SHIPPED 2026-07-24. Written 2026-07-25 — the code comments in
`ccpa/rootfs/script/attach_bluetooth.sh` and `bt_on.sh` referenced "docs/wireless/01_BT_AND_RADIO.md" from the day they were
written, but the document was never actually created (flagged as a loose end in
`SESSION_HANDOFF_2026-07-24.md`). This closes that.

Both fixes are in the box's **shell bring-up scripts**, not in any Rust daemon. They are pre-existing
vendor-script defects that happened to surface while chasing an unrelated problem.

### Fix 1 — `attach_bluetooth.sh`: the retry loop had no real health check

`attach_bt()` loads `hci_uart`, pushes the NXP IW416 firmware over `/dev/ttymxc2` via
`fw_loader_linux`, attaches HCI with `hciattach`, and brings the interface up. The retry loop then
decided whether that worked by checking `hciconfig hci0`'s **exit code**.

That only proves the net-device *object* exists. `fw_loader_linux` can report `Download Error` on the
firmware push while `hciattach` and `hciconfig hci0 up` still succeed at the interface level — leaving
a chip that never answers a single HCI command. Every subsequent read/write then times out at the
kernel HCI layer (~2-3 s each), and the box runs for the rest of boot with a dead radio that looks
alive. Confirmed live via `dmesg`: repeated `hci0 command 0x0c56 / 0x0c24 / 0x0c13 / 0x0c14 tx
timeout`.

**Fix:** added `bt_responsive()`, which reads the local name back —
`timeout 5 hciconfig hci0 name | grep -q "Name:"` — using the exact readback pattern already proven in
`bt_on.sh`, bounded by `timeout` as a belt-and-suspenders on top of the kernel's own per-command HCI
timeout. The retry loop now uses that as its health check instead of the exit code.

### Fix 2 — `bt_on.sh`: two uncoordinated bring-ups fighting each other

`bt_on.sh` backgrounded `attach_bluetooth.sh` with `&` and then waited only for
`/sys/class/bluetooth/hci0` to *exist* (~100 ms) before returning. So `wireless_up()` immediately
exec'd `btd`, which runs its **own independent** Bluetooth bring-up — `killall
bluetoothDaemon hcid sdpd` plus its own `hciconfig` calls (`bt_bringup::bring_up`) — concurrently with
`attach_bluetooth.sh`'s still-in-flight retry loop.

The two uncoordinated bring-ups fought each other indefinitely. **Observed live: killed after 7+
minutes without ever converging.**

**Fix:** `bt_on.sh` now waits (up to 500 s) for `/tmp/.hciattach_done` — a flag `attach_bluetooth.sh`
already touched as its final statement and which nothing had ever read — before returning. So
`btd` cannot start until BT has genuinely converged.

### Result

After both fixes, BT bring-up was clean and fast on every subsequent test: the chip is responsive
within seconds and the two bring-ups no longer collide.

### Important consequence (see docs/wireless/01_BT_AND_RADIO.md)

Fix 2 made the bring-up **deterministic**. That is a good thing on its own, but it had a side effect
nobody predicted: it also made deterministic the *order* in which the controller's last configuration
write happened — and the last write was a raw `HCI_Reset` at the end of `attach_bt()` that wiped the
kernel's SSP event mask. Before this fix, the racy interleaving sometimes left a kernel re-init as the
final word, so fresh pairing sometimes worked; afterwards the raw reset always won, and fresh SSP
pairing broke every time.

In other words: **this fix did not cause the pairing regression, but it is what turned an intermittent
latent bug into a reproducible one.** That is a good trade — a reproducible bug is a fixable bug — and
the actual defect is documented and fixed in
`../wireless/01_BT_AND_RADIO.md`.

### Deployed

- `/script/attach_bluetooth.sh` — md5 `642b9a27786232fc8de6afc7e410a837`
- `/script/bt_on.sh` — md5 `2184dbfb23b3a64a52fc77da2bdd9013`

Both are plain shell pushed to `/script/`; no rebuild or UPX step.

> **⚠️ THE `attach_bluetooth.sh` MD5 ABOVE IS SUPERSEDED** (`bt_on.sh`'s is still current at HEAD).
> It records what this fix deployed on 2026-07-24; docs/wireless/01_BT_AND_RADIO.md changed the file the next day, so a box
> still matching `642b9a27…` is running the **pre-docs/wireless/01_BT_AND_RADIO.md** script. Such a box is not necessarily
> unable to pair — but push the current file rather than reason about which.
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-40-1`.
