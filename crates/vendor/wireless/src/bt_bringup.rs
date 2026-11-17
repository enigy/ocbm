//! Raw Bluetooth Class-of-Device/name/EIR bring-up, porting `carlink_linux`'s proven
//! `carplay_bt_raw_up.sh` (live-verified against a real iPhone by that project) via `hciconfig` --
//! confirmed present on this Pi's Raspberry Pi OS `bluez` build despite upstream deprecating it
//! elsewhere, so shelling out here matches a proven reference exactly rather than hand-rolling raw
//! HCI ioctls for this phase.
//!
//! `bluetoothd` MUST be masked (not just stopped) before this runs -- it otherwise fights the raw
//! CoD/EIR settings and answers its own (empty) SDP. That requirement is the load-bearing part and
//! stands on its own.
//!
//! CORRECTED 2026-08-11: this comment used to justify it by analogy -- "the wired mDNS path already
//! masks `avahi-daemon` for the identical reason -- see docs/ops/04_OPEN_ITEMS.md". NEITHER
//! half survives checking. That handoff contains no avahi content, and `avahi` appears NOWHERE in
//! this repo. The precedent was asserted, not established. Removed rather than repointed, because
//! carrying a citation to a source that does not contain the claim is worse than carrying no
//! citation at all.

use std::process::Command;

/// Class of Device: Major Service "Audio" + Major Device "Audio/Video" + Minor "Hands-free" --
/// the exact value the reference project's own comment identifies as "the CCPA value" (i.e. what a
/// real CarPlay dongle presents, which is what iOS's CarPlay accessory filter looks for).
pub const CLASS_OF_DEVICE: u32 = 0x200408;

/// The iAP2 service UUID (128-bit) a CarPlay-capable accessory advertises, and the CarPlay
/// discovery marker UUID, byte-exact from the reference project's own capture-verified EIR.
const IAP2_SERVICE_UUID: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0xde, 0xca, 0xfa, 0xde, 0xde, 0xca, 0xde, 0xaf, 0xde, 0xca, 0xca, 0xff,
];
const CARPLAY_MARKER_UUID: [u8; 16] = [
    0xd3, 0x1f, 0xbf, 0x50, 0x5d, 0x57, 0x27, 0x97, 0xa2, 0x40, 0x41, 0xcd, 0x48, 0x43, 0x88, 0xec,
];

/// Build the Extended Inquiry Response data `hciconfig <dev> inqdata` expects: a sequence of
/// `[len][type][data...]` AD structures -- Complete Local Name (type 0x09) + two 128-bit Service
/// UUID lists (type 0x06 incomplete, 0x07 complete), matching the reference's exact AD layout.
pub fn eir_bytes(name: &str) -> Vec<u8> {
    let mut eir = Vec::new();

    // AD: Complete Local Name — NUL-terminated. The working carlink_linux EIR (byte-identical to a
    // genuine CCPA's /etc/bluetooth/eir_info) appends a trailing 0x00 and its length byte counts it
    // (`0f 09 <name> 00` for a 13-char name). Reproduce that exactly.
    //
    // Truncated to fit the AD length byte rather than cast to it: a name long enough to wrap
    // `len + 2` past 255 would declare a length that disagrees with the bytes that follow, and the
    // controller parses the rest of the EIR from that number. `hci::write_eir` rejects > 240 bytes
    // total, so the cut is at the smaller of the two limits.
    // 240 total, less this AD's len+type+NUL (3) and the two 18-byte UUID ADs below.
    const MAX_NAME: usize = 240 - 3 - 2 * 18;
    let mut end = name.len().min(MAX_NAME);
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    let name_bytes = &name.as_bytes()[..end];
    eir.push((name_bytes.len() + 2) as u8); // type byte + name bytes + trailing NUL
    eir.push(0x09);
    eir.extend_from_slice(name_bytes);
    eir.push(0x00);

    // AD: 128-bit Service UUID list (incomplete), the iAP2 service.
    eir.push(1 + 16);
    eir.push(0x06);
    eir.extend_from_slice(&IAP2_SERVICE_UUID);

    // AD: 128-bit Service UUID list (complete), the CarPlay discovery marker.
    eir.push(1 + 16);
    eir.push(0x07);
    eir.extend_from_slice(&CARPLAY_MARKER_UUID);

    eir
}

fn eir_hex(name: &str) -> String {
    eir_bytes(name).iter().map(|b| format!("{b:02x}")).collect()
}

fn run(args: &[&str]) -> std::io::Result<()> {
    run_with(Command::new("hciconfig"), args)
}

/// BusyBox `chrt`, present on the CCPA rootfs.
const CHRT: &str = "/usr/bin/chrt";

/// `hciconfig down` / `up` at SCHED_FIFO 1 where `chrt` exists, plain `hciconfig` otherwise.
///
/// Both are ioctls that wait in the kernel's `__hci_req_sync`, the path that loses its wake-up on
/// the CCPA's 3.14 kernel when the controller answers before the caller is on the wait queue (the
/// 10 s piscan stall, `hci::write_scan_enable`; seen once on `up` as well, 2026-10-20). Unlike scan
/// enable these cannot go raw - the DOWN->UP cycle exists for what `hci_dev_do_open` does. But the
/// box has ONE core, and the command is only sent by the HCI workqueue, which cannot preempt a
/// real-time caller: the caller is always on the wait queue before the command even leaves.
fn run_rt(args: &[&str]) -> std::io::Result<()> {
    if !std::path::Path::new(CHRT).exists() {
        return run(args);
    }
    let mut cmd = Command::new(CHRT);
    cmd.args(["-f", "1", "hciconfig"]);
    run_with(cmd, args)
}

fn run_with(mut cmd: Command, args: &[&str]) -> std::io::Result<()> {
    // Captured (not inherited) so a failure's stderr lands in the SAME log line as the argv and exit
    // status -- `hciconfig`'s own stderr is a few words with no context, and split across a raw
    // process's inherited stdio it is unattributable once other daemons are logging concurrently.
    let out = cmd.args(args).output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("[btd] hciconfig {args:?} failed: status={} stderr={stderr:?}", out.status);
        return Err(std::io::Error::other(format!(
            "hciconfig {args:?} failed: {}", out.status
        )));
    }
    Ok(())
}

/// Stop the box's stock BT daemons that would fight this raw stack. On the IW416 box, `bt_on.sh`
/// brings `hci0` up but also starts `hcid` (and possibly `bluetoothDaemon`), which runs its own SDP
/// server on L2CAP PSM 1 and would answer the iPhone's SDP with no iAP2 record -- the exact failure
/// the PoC documents for `bluetoothd`. Best-effort `killall` (ignore "no such process"); neither is
/// respawn-protected, so this is enough. This is the ccpa_custom analog of the PoC masking
/// `bluetoothd` via systemd.
fn stop_conflicting_daemons() {
    // `hfpd` is in this list because `bluetoothDaemon` starts it: it is the vendor's HFP/HSP
    // implementation, a second Bluetooth stack talking to the phone. It is also in `bt_off.sh` and
    // the installer kill list.
    // A SIGTERM to each by its /proc comm, as `killall` sends, without a child process per name.
    let names = ["hcid", "bluetoothDaemon", "sdpd", "hfpd"];
    let Ok(procs) = std::fs::read_dir("/proc") else { return };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<libc::pid_t>().ok()) else { continue };
        let comm = std::fs::read_to_string(p.path().join("comm")).unwrap_or_default();
        // comm is cut at 15 bytes: "bluetoothDaemon" is exactly 15.
        if names.contains(&comm.trim()) {
            // SAFETY: a signal to a pid just read from /proc; a pid that exited meanwhile is ESRCH.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

/// Is the HCI UART line discipline registered with the kernel?
///
/// `hci_uart` is a LOADABLE MODULE on the CCPA (RFCOMM/SCO/L2CAP and the BT core are built in; only
/// this one is not), shipped inside `/lib/firmware/nxp/iw416_ko.tar.gz`. Something has to extract and
/// `insmod` it before `hciattach` runs -- on this box that is `radio_hal.sh`, which the supervisor
/// invokes with its exit status unread.
///
/// When the module is absent, `hciattach`'s `ioctl(TIOCSETD, N_HCI)` fails `EINVAL` and prints
/// "Can't set line discipline: Invalid argument" -- three steps downstream of the real cause, and
/// directly beneath a flawless firmware download. That combination cost a full session on
/// 2026-08-28: no `hci0`, no pairing, no CarPlay, and every layer above still reporting success.
///
/// `/proc/tty/ldiscs` answers it in one read, so check before walking into it. See ccpa_custom
/// `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5.
fn hci_ldisc_registered() -> bool {
    std::fs::read_to_string("/proc/tty/ldiscs")
        .map(|s| s.lines().any(|l| l.split_whitespace().next() == Some("n_hci")))
        .unwrap_or(true) // unreadable /proc: do not block bring-up on a diagnostic
}

/// One timed step of [`bring_up`]: an `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md
/// CH_LOG, so the host's drive log keeps the real time) plus the time since bring-up began.
///
/// Added 2026-10-20: a GM truck drive log showed 11.7 s from btd starting to "discoverable" on a
/// warm reconnect (radio_hal had just reported hci0 converged), the largest single cost between the
/// host's SUBSCRIBE and the first video frame. Nothing inside bring-up was timed, so which step
/// spends it (the DOWN->UP controller reset or the `hciconfig` calls) could not be told apart.
fn stamp(t0: std::time::Instant, what: &str) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    eprintln!("@{now_ms} [bt-bringup] {what} (+{} ms)", t0.elapsed().as_millis());
}

/// Bring the controller up as a discoverable/connectable CarPlay-style accessory: set class, name,
/// EIR, then enable page+inquiry scan (discoverable + connectable, no timeout).
///
/// Since 16 Nov 2026 in two steps, [`prepare`] and [`open_scans`], so the armed state (between host sessions,
/// [`arm`]) can run the first ahead of the host and the second at its go ([`open_from_armed`]). This path runs both
/// at once, exactly as before.
pub fn bring_up(hci_dev: &str, name: &str, just_opened: bool) -> std::io::Result<()> {
    let t0 = std::time::Instant::now();
    prepare(hci_dev, name, just_opened, t0)?;
    open_scans(hci_dev, name, t0)
}

/// Everything [`bring_up`] does before the scan mode: the conflicting daemons stopped, the DOWN->UP cycle (unless
/// `just_opened`), and the identity writes (Simple Pairing, class, name, EIR).
fn prepare(hci_dev: &str, name: &str, just_opened: bool, t0: std::time::Instant) -> std::io::Result<()> {
    stamp(t0, "start");
    // Name the real fault at its source rather than letting it surface as an unexplained EINVAL.
    // Deliberately a LOG, not an early return: this is diagnostic, the controller may already be
    // attached by some path we do not model, and refusing to proceed on a heuristic would be worse
    // than the silence it replaces.
    if !hci_ldisc_registered() {
        eprintln!(
            "[bt-bringup] WARNING: no n_hci line discipline registered — hciattach cannot create \
             {hci_dev} and BT will not come up. hci_uart is a loadable module here; check that \
             /script/radio_hal.sh and /script/radio_detect.sh are installed (ccpa_custom \
             docs/ops/06_CORRECTIONS_LEDGER.md R-20W-5)."
        );
    }
    stop_conflicting_daemons(); // box: free L2CAP PSM 1 (SDP) from the stock hcid before we bind it
    stamp(t0, "conflicting daemons stopped");

    // docs/wireless/01_BT_AND_RADIO.md: force a DOWN->UP cycle, not a bare `up`. The kernel sends its Set_Event_Mask (which is
    // what enables the SSP events IO_Capability_Request 0x31 / User_Confirmation_Request 0x33 /
    // User_Passkey_Request 0x34 / Simple_Pairing_Complete 0x36) ONLY from `hci_dev_do_open`, i.e. an
    // HCIDEVUP on a DOWN device -- verified against the v3.14.52 sources: `hci_setup_event_mask`
    // (hci_core.c:1081) is reachable only via `hci_init2_req` -> `__hci_init` -> `hci_dev_do_open`,
    // `hci_dev_reset` (hci_core.c:2108) does NOT re-init, and `hci_dev_do_open` returns -EALREADY when
    // HCI_UP is set (which `hciconfig up` swallows, exiting 0). So any earlier raw HCI_Reset (the bug
    // removed from attach_bluetooth.sh) or `hciconfig reset` leaves the controller on the spec-default
    // mask -- SDP and ACL keep working (their events are below 0x2D), and bonded RECONNECTS keep
    // working (Link_Key_Request 0x17 is also below it), but fresh SSP pairing silently never reaches
    // the host and the phone reports "Pairing Unsuccessful". Doing this here makes the daemon
    // self-sufficient regardless of what the boot scripts did to the controller beforehand. `down` is
    // best-effort (already-down returns 0); only the `up` is load-bearing.
    //
    // SAFETY (corrected 2026-07-25 -- an earlier version of this comment wrongly claimed "runs once at
    // startup"): `bring_up` runs once per ACTIVE SESSION, re-entered from `main.rs`'s arbiter loop on
    // every wired->wireless handback. The real guarantee is ordering within `run_active_session`: the
    // SSP mgmt socket, the SDP listener and the RFCOMM listener are all spawned AFTER this returns,
    // and the previous session's threads are joined BEFORE re-entry -- so the down/up never hits a
    // bound socket or a live listener. During the quiet period `go_quiet`'s `noscan` prevents any new
    // inbound ACL, so no in-use connection can be severed either.
    //
    // KNOWN SIDE EFFECT: this controller is `hciattach`'d over UART, so it carries
    // HCI_QUIRK_RESET_ON_CLOSE -- the `down` makes the kernel issue a real HCI_Reset on close, and the
    // `up` re-reads Read_Buffer_Size. That discards `attach_bluetooth.sh`'s SCO setup, which is
    // harmless: CarPlay carries all audio, telephony included, over the AirPlay/Wi-Fi session, never
    // over SCO.
    //
    // ONE CYCLE, NOT TWO (the Mac's 13 Nov brief, item 1: about 0.5 s of every connect). `just_opened`: radio_hal.sh's
    // bt_on opened this controller from DOWN moments before this daemon started (`/tmp/.radio_bt_warm` reads
    // "opened"), which is the same `hci_dev_do_open` and the same event mask, after the same reset on close. Only the
    // first bring-up of a btd may trust it: a later one follows a session.
    if just_opened {
        stamp(t0, "opened by radio_hal.sh just now: no down/up");
    } else {
        let _ = run_rt(&[hci_dev, "down"]);
        stamp(t0, "down");
        run_rt(&[hci_dev, "up"])?;
        stamp(t0, "up");
    }
    // Force Simple Pairing mode ON at the HCI level (HCI Write_Simple_Pairing_Mode). The NXP IW416
    // firmware defaults SSP OFF and the mgmt SET_SSP path can be rejected silently, leaving "Simple
    // Pairing mode: Disabled" → pairing degrades to legacy PIN → iOS refuses the iAP2/CarPlay handshake
    // (SDP loops, RFCOMM never opens). This deterministically flips the readback; SSP is required for
    // BOTH Just-Works and Numeric Comparison. Non-fatal if the build's hciconfig lacks `sspmode`.
    // All four as raw commands on one socket ([`crate::hci::write_identity`]), not four hciconfig children; hciconfig
    // only if that fails.
    if let Err(e) = crate::hci::write_identity(crate::hci::dev_id(hci_dev)?, CLASS_OF_DEVICE, name, &eir_bytes(name)) {
        eprintln!("[bt-bringup] raw identity writes failed ({e}) -- by hciconfig");
        let _ = run(&[hci_dev, "sspmode", "1"]);
        run(&[hci_dev, "class", &format!("0x{CLASS_OF_DEVICE:06x}")])?;
        run(&[hci_dev, "name", name])?;
        run(&[hci_dev, "inqdata", &eir_hex(name)])?;
    }
    stamp(t0, "sspmode, class, name, EIR");
    Ok(())
}

/// [`bring_up`]'s last step: page and inquiry scan on, as a session starts. The SSP agent narrows it to the pairing
/// window's state moments later (`ssp_agent::apply_pairing_state`: page scan only, unless the window is open or no
/// phone is bonded), as it always has.
fn open_scans(hci_dev: &str, name: &str, t0: std::time::Instant) -> std::io::Result<()> {
    // NOT `hciconfig piscan`: its HCISETSCAN ioctl loses its wake-up on this 3.14 kernel about one
    // bring-up in three and sleeps the full 10 s request timeout (`hci::write_scan_enable`).
    crate::hci::write_scan_enable(crate::hci::dev_id(hci_dev)?, crate::hci::SCAN_PAGE_AND_INQUIRY)?;
    stamp(t0, "class, name, EIR, piscan set");
    eprintln!("[btd] HCI bring-up OK dev={hci_dev} name={name}");
    Ok(())
}

// ---- The armed state: Bluetooth powered between host sessions, but invisible (owner decision, 16 Nov 2026) ----

/// How long the armed bring-up waits for each command it must know the controller took.
const ARMED_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// What an armed controller looks like from the inside: its own scan mode ([`crate::hci::read_scan_enable`]) and the
/// connections the kernel holds on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Audit {
    pub scan: u8,
    pub conns: usize,
}

impl Audit {
    /// Nothing can see it (no inquiry scan), nothing can connect to it (no page scan), and it holds no link.
    pub fn invisible(self) -> bool {
        self.scan == crate::hci::SCAN_NONE as u8 && self.conns == 0
    }
}

/// One real round trip to the controller and a look at its links: [`Audit`]. An error when the controller does not
/// answer within `wait`.
pub fn audit(hci_dev: &str, wait: std::time::Duration) -> std::io::Result<Audit> {
    let dev = crate::hci::dev_id(hci_dev)?;
    let scan = crate::hci::read_scan_enable(dev, wait)?;
    let conns = crate::hci::connections(dev)?.len();
    Ok(Audit { scan, conns })
}

/// Make an armed controller invisible again after an [`audit`] found it otherwise: scan "none", waited for, and every
/// link it holds disconnected. Nothing in the armed state turns a scan on or makes a link, so this is a repair of
/// something outside btd, and the next audit says whether it held.
pub fn hide(hci_dev: &str) -> std::io::Result<()> {
    let dev = crate::hci::dev_id(hci_dev)?;
    crate::hci::write_scan_enable_waited(dev, crate::hci::SCAN_NONE, ARMED_WAIT)?;
    for c in crate::hci::connections(dev)? {
        let _ = crate::hci::disconnect(dev, c.handle);
    }
    Ok(())
}

/// The armed bring-up: the controller up and set up as a session's [`bring_up`] leaves it (its DOWN->UP cycle, so the
/// kernel's event mask is set, and the identity writes), but with page and inquiry scan OFF, and proved so by the
/// controller's own answer. Nothing pages and nothing listens: no SDP server, no RFCOMM listener, no SSP agent until
/// a host's go ([`open_from_armed`]).
///
/// Order is the invisibility argument. mgmt's connectable flag goes off first, while the controller is down (a flag
/// change only), so the kernel's own power-on writes no scan; the controller is reset on the way down and up (its
/// scan default is none); "none" is then written and waited for; and [`audit`] reads it back from the chip and finds
/// no link. An error at any step fails the arm: btd exits, and the next host takes today's path.
pub fn arm(hci_dev: &str, name: &str) -> std::io::Result<Audit> {
    let t0 = std::time::Instant::now();
    let dev = crate::hci::dev_id(hci_dev)?;
    if let Err(e) = crate::ssp_agent::clear_connectable(dev) {
        eprintln!("[bt-bringup] armed: mgmt connectable not cleared ({e}) -- the scan is still written off and read back");
    }
    prepare(hci_dev, name, false, t0)?;
    crate::hci::write_scan_enable_waited(dev, crate::hci::SCAN_NONE, ARMED_WAIT)?;
    let seen = audit(hci_dev, ARMED_WAIT)?;
    stamp(t0, &format!("armed: scan 0x{:02x}, {} connection(s)", seen.scan, seen.conns));
    if !seen.invisible() {
        return Err(std::io::Error::other(format!(
            "the controller reads scan 0x{:02x} with {} connection(s) after \"none\" was written",
            seen.scan, seen.conns
        )));
    }
    Ok(seen)
}

/// From the armed state into a session, at the host's go: what [`bring_up`] does that the arm left for now. The
/// conflicting daemons stopped again, the identity rewritten when the session's name is not the one the arm wrote
/// (`armed_name`: the name derives from wlan0's MAC, which may have appeared since), then [`open_scans`].
pub fn open_from_armed(hci_dev: &str, name: &str, armed_name: &str) -> std::io::Result<()> {
    let t0 = std::time::Instant::now();
    stamp(t0, "from armed: start");
    stop_conflicting_daemons();
    if name != armed_name {
        eprintln!("[bt-bringup] from armed: the name is {name} now, not {armed_name} -- rewriting the identity");
        let dev = crate::hci::dev_id(hci_dev)?;
        if let Err(e) = crate::hci::write_identity(dev, CLASS_OF_DEVICE, name, &eir_bytes(name)) {
            eprintln!("[bt-bringup] raw identity writes failed ({e}) -- by hciconfig");
            run(&[hci_dev, "name", name])?;
            run(&[hci_dev, "inqdata", &eir_hex(name)])?;
        }
    }
    open_scans(hci_dev, name, t0)
}

/// Stop being discoverable/connectable -- used when the arbiter preempts this session in favor of
/// wired, or on shutdown. Does not power the controller off (a future phase reuses it for RFCOMM).
pub fn go_quiet(hci_dev: &str) -> std::io::Result<()> {
    // Raw, not `hciconfig noscan`, for the same lost-wake-up reason as bring_up's scan enable.
    crate::hci::write_scan_enable(crate::hci::dev_id(hci_dev)?, crate::hci::SCAN_NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eir_starts_with_complete_local_name() {
        let eir = eir_bytes("CarLink");
        assert_eq!(eir[0], 9); // type byte + "CarLink" (7 bytes) + trailing NUL
        assert_eq!(eir[1], 0x09);
        assert_eq!(&eir[2..9], b"CarLink");
        assert_eq!(eir[9], 0x00); // NUL terminator, counted in the length above
    }

    /// The AD length byte must always describe the bytes that actually follow it: a name long
    /// enough to overflow it would desync the controller's parse of the rest of the EIR.
    #[test]
    fn an_overlong_name_is_truncated_not_wrapped() {
        let eir = eir_bytes(&"x".repeat(400));
        assert_eq!(eir[0] as usize, MAX_NAME_FOR_TEST + 2);
        assert_eq!(eir[1], 0x09);
        assert_eq!(eir[2 + MAX_NAME_FOR_TEST], 0x00, "NUL must follow the truncated name");
        assert!(eir.len() <= 240, "EIR is {} bytes", eir.len());
    }

    /// Mirrors `eir_bytes`'s own cap; kept here so a change to one fails the other.
    const MAX_NAME_FOR_TEST: usize = 240 - 3 - 2 * 18;

    #[test]
    fn eir_contains_both_service_uuids() {
        let eir = eir_bytes("CarLink");
        // iAP2 service UUID AD starts right after the name AD (2-byte header + 7-byte name + 1 NUL).
        let iap2_start = 2 + 7 + 1;
        assert_eq!(eir[iap2_start], 17); // 1 (type) + 16 (UUID)
        assert_eq!(eir[iap2_start + 1], 0x06);
        assert_eq!(&eir[iap2_start + 2..iap2_start + 18], &IAP2_SERVICE_UUID);

        let marker_start = iap2_start + 18;
        assert_eq!(eir[marker_start], 17);
        assert_eq!(eir[marker_start + 1], 0x07);
        assert_eq!(
            &eir[marker_start + 2..marker_start + 18],
            &CARPLAY_MARKER_UUID
        );
    }

    #[test]
    fn eir_hex_is_lowercase_no_separators() {
        let hex = eir_hex("CarLink");
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(hex.len(), eir_bytes("CarLink").len() * 2);
    }
}
