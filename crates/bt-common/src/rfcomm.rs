//! RFCOMM listener for the iAP2 data channel -- accepts the connection Phase A1 confirmed the
//! iPhone attempts (and the kernel correctly rejected with Disconnect Mode, since nothing was
//! listening) right after a successful SDP lookup. No `libbluetooth` dependency, matching
//! `sdp_server.rs`'s established approach: the L2CAP/RFCOMM sockaddr structs are hand-defined.

use std::io::Error;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, Ordering};

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_RFCOMM: libc::c_int = 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrRc {
    rc_family: libc::sa_family_t,
    rc_bdaddr: [u8; 6],
    rc_channel: u8,
}

fn open_listener(channel: u8) -> std::io::Result<std::fs::File> {
    // SOCK_CLOEXEC: this listener is open when bt_driver drives av::ensure_av_layer(), which
    // fork+execs carplayd SETSID-DETACHED TO OUTLIVE THIS PROCESS (av.rs:13). Without
    // CLOEXEC those daemons inherit the RFCOMM listening fd and hold the channel after this process
    // dies, so a supervisor restart re-binds onto a channel someone else still owns.
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_STREAM | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_RFCOMM,
        )
    };
    if fd < 0 {
        return Err(Error::last_os_error());
    }
    let addr = SockaddrRc {
        rc_family: AF_BLUETOOTH,
        rc_bdaddr: [0; 6],
        rc_channel: channel,
    };
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const SockaddrRc as *const libc::sockaddr,
            std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    if unsafe { libc::listen(fd, 1) } < 0 {
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // Matches sdp_server.rs's / ssp_agent.rs's own discipline: a receive timeout on the listening fd
    // so the shutdown flag actually gets checked between accepts instead of blocking forever.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
    timeout.tv_sec = 1;
    // Checked, like bind/listen above and the house pattern (bt_driver.rs, sdp_server.rs): an
    // unnoticed failure here would leave accept() blocking forever, so the shutdown flag is never
    // polled. The accept thread logs the error and retries.
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &timeout as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: fd is a freshly opened, bound, listening, exclusively-owned socket descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Connect OUT to a peer's RFCOMM `channel` (Model B, accessory-initiated reconnect — docs/wireless/01_BT_AND_RADIO.md).
///
/// Mirrors `open_listener`'s hand-rolled `AF_BLUETOOTH`/`SOCK_STREAM`/`BTPROTO_RFCOMM` socket but
/// `connect()`s to the bonded phone instead of binding+listening. In BlueZ a connect to a peer that
/// has no ACL implicitly pages it (Create Connection), so this both pages the phone and opens the
/// iAP2 DLC in one call — the bare-HCI-page dead-end (docs/wireless/01_BT_AND_RADIO.md §Eliminated) is not needed.
///
/// `peer` is the 6-byte bdaddr in BlueZ/mgmt little-endian order — exactly as stored in the link-key
/// record (`ssp_agent::bonded_addrs`), so it passes straight through with no byte reversal.
///
/// A blocking `connect()` on a quiet or absent phone would park the reconnect thread forever, so a
/// bounded `SO_SNDTIMEO` is set first (same discipline as `bt_driver`'s accepted socket): the connect
/// returns `EINPROGRESS`/`ETIMEDOUT` instead, and the orchestrator retries with backoff. The returned
/// socket is CLOEXEC for the same reason `open_listener` is — it is live across `av::ensure_av_layer`'s
/// fork+exec of the detached daemons.
pub fn connect_to(peer: [u8; 6], channel: u8, connect_timeout_secs: i64) -> std::io::Result<std::fs::File> {
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_STREAM | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_RFCOMM,
        )
    };
    if fd < 0 {
        return Err(Error::last_os_error());
    }
    // Bounded connect: SO_SNDTIMEO caps how long a blocking connect() parks on an unreachable peer.
    // tv_sec's width is target-dependent (i32 on armv7-musl, i64 on the dev host); `as _` infers the
    // field type from the struct literal so both targets build without naming the deprecated time_t alias.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
    // `.max(1)`: 0 means "no timeout" (unbounded blocking connect()) on Linux — the exact hang this
    // timeout exists to prevent.
    timeout.tv_sec = connect_timeout_secs.max(1) as _;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDTIMEO,
            &timeout as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        let e = Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // General bonding, so a bond formed on this box-initiated connect is persistent (store_hint=1).
    request_bonding(fd);
    let addr = SockaddrRc {
        rc_family: AF_BLUETOOTH,
        rc_bdaddr: peer,
        rc_channel: channel,
    };
    // Pairing-aware connect (2026-09-03): the connect is where SSP pairing runs when the phone
    // enforces authentication. Hold while a numeric-comparison prompt is outstanding, abort once the
    // phone has rejected — see [`pairing_aware_connect`]. SO_SNDTIMEO above still bounds
    // the later blocking writes.
    if let Err(e) = pairing_aware_connect(
        fd,
        &addr as *const SockaddrRc as *const libc::sockaddr,
        std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
        connect_timeout_secs,
    ) {
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: fd is a freshly opened, connected, exclusively-owned socket descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Block until one RFCOMM client connects on `channel`, or `shutdown` is set. Returns `Ok(None)` on
/// shutdown, `Ok(Some((file, peer)))` on an accepted connection ready for iAP2 traffic. `peer` is the
/// phone's bdaddr in mgmt little-endian order, the order the link-key store uses, so callers can
/// ask whether it is bonded (security plan 2026-10-21 H1: Wi-Fi details only for bonded phones).
pub fn accept_one(channel: u8, shutdown: &AtomicBool) -> std::io::Result<Option<(std::fs::File, [u8; 6])>> {
    let listener = open_listener(channel)?;
    let listen_fd = listener.as_raw_fd();
    while !shutdown.load(Ordering::Relaxed) {
        let mut ra: SockaddrRc = unsafe { std::mem::zeroed() };
        let mut ralen = std::mem::size_of::<SockaddrRc>() as libc::socklen_t;
        // accept4(SOCK_CLOEXEC), not accept(): the accepted DLC is live across
        // av::ensure_av_layer()'s fork+exec of the detached daemons — see open_listener above.
        let cfd = unsafe {
            crate::cloexec::accept_cloexec(
                listen_fd,
                &mut ra as *mut SockaddrRc as *mut libc::sockaddr,
                &mut ralen,
            )
        };
        if cfd < 0 {
            let e = Error::last_os_error();
            // ConnectionAborted/Interrupted are transient, same as sdp_server::run_services;
            // without this the caller's 1 s sleep-and-retry lands exactly
            // when a phone is retrying its connect.
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::Interrupted
            ) {
                continue;
            }
            return Err(e);
        }
        // SAFETY: cfd is a freshly accepted, exclusively-owned connected socket descriptor.
        return Ok(Some((unsafe { std::fs::File::from_raw_fd(cfd) }, ra.rc_bdaddr)));
    }
    Ok(None)
}

/// Pairing-aware connect wait (device-proven 2026-09-03). The L2CAP connect is where SSP pairing
/// happens when the phone enforces authentication: the kernel holds the connect until the phone's
/// user has answered the numeric-comparison prompt. A flat `connect_timeout_secs` tore that prompt
/// down before anyone could compare and tap, and every retry produced a NEW code (the owner saw the
/// app cycling codes). So while `/tmp/pairing_code` is fresh for THIS attempt (written by
/// `ssp_agent` on USER_CONFIRM_REQUEST, mtime >= our start) the deadline stretches to
/// [`PAIRING_HOLD_SECS`]; and once `ssp_agent` reports that the phone rejected the re-pair
/// (`/tmp/pair_rejected`, mtime >= our start) the wait ends at once with `PermissionDenied`, so the
/// caller backs off instead of the kernel re-requesting authentication every ~0.5 s.
pub const PAIRING_HOLD_SECS: u64 = 60;
/// Written by `ssp_agent` with the 6-digit code while a numeric-comparison confirm is outstanding.
pub const PAIRING_CODE_FLAG: &str = "/tmp/pairing_code";
/// Written by `ssp_agent` when the phone has rejected the re-pair repeatedly on one connection.
pub const PAIR_REJECTED_FLAG: &str = "/tmp/pair_rejected";

#[derive(Debug, PartialEq, Eq)]
enum WaitDecision {
    Wait,
    TimedOut,
    Rejected,
}

/// Pure decision for one poll tick of the connect wait. `base` is the caller's plain timeout.
fn connect_wait_decision(
    elapsed: std::time::Duration,
    base: std::time::Duration,
    code_fresh: bool,
    rejected_fresh: bool,
) -> WaitDecision {
    if rejected_fresh {
        return WaitDecision::Rejected;
    }
    let limit = if code_fresh {
        base.max(std::time::Duration::from_secs(PAIRING_HOLD_SECS))
    } else {
        base
    };
    if elapsed >= limit {
        WaitDecision::TimedOut
    } else {
        WaitDecision::Wait
    }
}

/// True when `path` exists and was modified at or after `since` (a flag left by an EARLIER attempt
/// must not steer this one).
fn flag_fresh(path: &str, since: std::time::SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t >= since)
        .unwrap_or(false)
}

/// `connect()` with a pairing-aware wait. Connects non-blocking, then polls
/// with [`connect_wait_decision`] each 250 ms: a plain `connect_timeout_secs` deadline, stretched to
/// [`PAIRING_HOLD_SECS`] while a numeric-comparison prompt is outstanding for this attempt, and cut
/// short with `PermissionDenied` once `ssp_agent` reports the phone rejected the re-pair. The socket
/// is returned to blocking mode on success.
fn pairing_aware_connect(
    fd: libc::c_int,
    addr: *const libc::sockaddr,
    addr_len: libc::socklen_t,
    connect_timeout_secs: i64,
) -> std::io::Result<()> {
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) };
    let rc = unsafe { libc::connect(fd, addr, addr_len) };
    if rc < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(e);
        }
        let start = std::time::Instant::now();
        // 1 s of slack so a flag written in the same tick as our start still counts as ours.
        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        let base = std::time::Duration::from_secs(connect_timeout_secs.max(1) as u64);
        let mut held_logged = false;
        // Latched once a prompt has been seen: the bond forming clears the code flag a moment BEFORE
        // the connect completes (device-proven 2026-09-03: NEW_LINK_KEY at +12 s, connect reported
        // "timed out" in the same tick because the hold fell back to the 8 s base), so the extended
        // limit must survive the flag going away for the rest of this attempt.
        let mut held = false;
        loop {
            let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
            let n = unsafe { libc::poll(&mut pfd, 1, 250) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n > 0 {
                let mut err: libc::c_int = 0;
                let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        &mut err as *mut libc::c_int as *mut libc::c_void,
                        &mut len,
                    )
                };
                if err != 0 {
                    return Err(std::io::Error::from_raw_os_error(err));
                }
                break;
            }
            let code_fresh = flag_fresh(PAIRING_CODE_FLAG, since);
            held |= code_fresh;
            let rejected = flag_fresh(PAIR_REJECTED_FLAG, since);
            match connect_wait_decision(start.elapsed(), base, held, rejected) {
                WaitDecision::Wait => {
                    if code_fresh && !held_logged {
                        eprintln!(
                            "[rfcomm] pairing prompt outstanding on the phone — holding the connect up to \
                             {PAIRING_HOLD_SECS}s for the user to confirm"
                        );
                        held_logged = true;
                    }
                }
                WaitDecision::TimedOut => {
                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"));
                }
                WaitDecision::Rejected => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "pairing rejected by the phone (re-pair on the iPhone)",
                    ));
                }
            }
        }
    }
    unsafe { libc::fcntl(fd, libc::F_SETFL, fl) };
    Ok(())
}

/// Ask the kernel for GENERAL BONDING on an outgoing BR/EDR socket (`BT_SECURITY_MEDIUM`).
/// Device-proven 2026-09-03: with no level set the box's own side of a box-initiated connect is
/// "no bonding", so a bond formed on it arrives as `NEW_LINK_KEY store_hint=0` (the kernel's
/// `hci_persistent_key` says not persistent) even though the phone did bond — the fresh
/// authenticated key was never written to disk and the next boot prompted again. MEDIUM maps to
/// `HCI_AT_GENERAL_BONDING`, which is what an accessory that expects to reconnect silently wants.
/// Best-effort: an old kernel refusing the option just keeps the previous behaviour.
fn request_bonding(fd: libc::c_int) {
    const SOL_BLUETOOTH: libc::c_int = 274;
    const BT_SECURITY: libc::c_int = 4;
    const BT_SECURITY_MEDIUM: u8 = 2;
    #[repr(C)]
    struct BtSecurity {
        level: u8,
        key_size: u8,
    }
    let sec = BtSecurity { level: BT_SECURITY_MEDIUM, key_size: 0 };
    let rc = unsafe {
        libc::setsockopt(
            fd,
            SOL_BLUETOOTH,
            BT_SECURITY,
            &sec as *const BtSecurity as *const libc::c_void,
            std::mem::size_of::<BtSecurity>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        eprintln!(
            "[rfcomm] BT_SECURITY=MEDIUM refused ({}) — bonds formed on this connect may not persist",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_wait_holds_for_the_pairing_prompt_and_aborts_on_rejection() {
        use std::time::Duration as D;
        let base = D::from_secs(8);
        assert_eq!(connect_wait_decision(D::from_secs(3), base, false, false), WaitDecision::Wait);
        assert_eq!(connect_wait_decision(D::from_secs(8), base, false, false), WaitDecision::TimedOut);
        // A fresh prompt stretches the wait to PAIRING_HOLD_SECS...
        assert_eq!(connect_wait_decision(D::from_secs(30), base, true, false), WaitDecision::Wait);
        assert_eq!(connect_wait_decision(D::from_secs(60), base, true, false), WaitDecision::TimedOut);
        // ...and a rejection ends it regardless of elapsed time or an outstanding prompt.
        assert_eq!(connect_wait_decision(D::from_secs(1), base, true, true), WaitDecision::Rejected);
        // A caller timeout longer than the hold is respected as-is.
        assert_eq!(connect_wait_decision(D::from_secs(70), D::from_secs(90), true, false), WaitDecision::Wait);
    }

    #[test]
    fn flag_fresh_ignores_files_older_than_the_attempt() {
        let dir = std::env::temp_dir().join(format!("rfcomm_flag_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("flag");
        let fs = f.to_str().unwrap();
        assert!(!flag_fresh(fs, std::time::SystemTime::now()));
        std::fs::write(&f, b"1").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        assert!(flag_fresh(fs, std::time::SystemTime::now() - std::time::Duration::from_secs(5)));
        assert!(!flag_fresh(fs, later));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
