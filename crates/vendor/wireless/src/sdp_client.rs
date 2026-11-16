//! Minimal, `bluetoothd`-less SDP *client* for Model-B accessory-initiated reconnect (docs/wireless/01_BT_AND_RADIO.md).
//!
//! On reconnect the box must discover which RFCOMM channel the bonded iPhone exposes its iAP2 service
//! on before it can `rfcomm::connect_to` it — the box has no `sdptool`. This is the symmetric peer of
//! `sdp_server.rs`: it L2CAP-connects to the phone's well-known SDP PSM 0x0001 (which in BlueZ
//! implicitly pages the phone, bringing the ACL up), sends a `ServiceSearchAttributeRequest` for the
//! iAP2 UUID, reassembles any continuation chunks, and pulls the RFCOMM channel out of the returned
//! `ProtocolDescriptorList`.
//!
//! Open unknown this module exists to ANSWER (docs/wireless/01_BT_AND_RADIO.md): whether iOS exposes an iAP2/EA RFCOMM service
//! at all on reconnect, and on what channel. So it always logs the raw response hex — if the channel
//! scan fails, the capture tells the next session exactly what iOS returned.

use std::io::{Read, Write};
use std::os::fd::FromRawFd;

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_L2CAP: libc::c_int = 0;
const SDP_PSM: u16 = 0x0001;

const SDP_ERROR_RSP: u8 = 0x01;
const SDP_SVC_SEARCH_ATTR_REQ: u8 = 0x06;
const SDP_SVC_SEARCH_ATTR_RSP: u8 = 0x07;

/// The PHONE-side iAP2 service UUID128 (`02030302-1d19-415f-86f2-22a2106a0a77`), named
/// "Wireless iAP v2" in iOS's SDP catalog — device-confirmed on 2026-08-01 by browsing the phone's
/// full record set (docs/wireless/01_BT_AND_RADIO.md). This is NOT the accessory-side UUID `sdp_server.rs` advertises
/// (`…decacaff`); the phone advertises its own iAP2 endpoint under this distinct UUID on RFCOMM ch 1.
/// (iOS also exposes "Wireless iAP5" under `…decacafe`, one byte off from the accessory UUID — a red
/// herring the first probe fell into.)
const IAP2_UUID128: [u8; 16] = [
    0x02, 0x03, 0x03, 0x02, 0x1d, 0x19, 0x41, 0x5f, 0x86, 0xf2, 0x22, 0xa2, 0x10, 0x6a, 0x0a, 0x77,
];

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    l2_psm: u16, // little-endian on the wire; this box is little-endian, so native == LE
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [sdp-client] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}

/// How long after the page is answered (the ACL is BT_CONNECTED) the SDP channel may take to open. All 83 good answers
/// on the Mac's benches opened it within 14-512 ms; all 10 answered-then-reset links never did, and the phone dropped
/// the link 4-6 s later (13 Nov brief, item 3). Past this, the connect is cut: a result of its own (`is_cut`), which the
/// reconnect loop pages once more and counts once a round, and which keeps a link that was up before the page. A bench
/// may set another ([`sdp_after_acl`]).
const SDP_AFTER_ACL_MS: u64 = 700;

/// How often a connect in flight looks at its socket and at the link.
const POLL_SLICE: std::time::Duration = std::time::Duration::from_millis(25);

/// How long the link a cut leaves may take to go after its Disconnect, before the caller pages again anyway.
const CUT_LINK_GONE: std::time::Duration = std::time::Duration::from_millis(1500);

/// The bench file for the SDP window (docs/ops/02_TESTING.md): whole ms, read through box_common::bench_text, so Carlink's
/// debug builds can set it like the other bench files and "off" (or anything not a number) is the default.
const BENCH_SDP_AFTER_ACL: &str = "/tmp/bench_sdp_after_acl_ms";

/// [`SDP_AFTER_ACL_MS`], or the env lever `BT_SDP_AFTER_ACL_MS`, or [`BENCH_SDP_AFTER_ACL`]: the Mac's reviews of 376c4ce
/// (F4) and of the 14 Nov pushes (B3). A window not the default says so, each page, as a [bench] line.
fn sdp_after_acl() -> std::time::Duration {
    let parse = |v: &str| v.trim().parse::<u64>().ok();
    let ms = box_common::lever("BT_SDP_AFTER_ACL_MS", "CARPLAY_SDP_AFTER_ACL_MS")
        .and_then(|v| parse(&v))
        .or_else(|| box_common::bench_text(BENCH_SDP_AFTER_ACL).and_then(|v| parse(&v)))
        .unwrap_or(SDP_AFTER_ACL_MS);
    if ms != SDP_AFTER_ACL_MS {
        log(&format!("[bench] SDP window {ms} ms after the answer, not {SDP_AFTER_ACL_MS} ({BENCH_SDP_AFTER_ACL} or BT_SDP_AFTER_ACL_MS)"));
    }
    std::time::Duration::from_millis(ms)
}

/// The pending error on a socket (SO_ERROR), which reading clears; 0 for none.
fn so_error(fd: libc::c_int) -> std::io::Result<libc::c_int> {
    let mut err: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: an int and its length, for a descriptor this module owns.
    let rc = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_ERROR, &mut err as *mut libc::c_int as *mut libc::c_void, &mut len) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(err)
}

/// O_NONBLOCK on or off.
fn set_nonblocking(fd: libc::c_int, on: bool) -> std::io::Result<()> {
    // SAFETY: plain fcntl flag reads and writes on a descriptor this module owns.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let want = if on { flags | libc::O_NONBLOCK } else { flags & !libc::O_NONBLOCK };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, want) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// What the reconnect loop lends a connect in flight: its shutdown, and whether a session is live (the accept path's,
/// or one the phone opened) whose link a cut must not drop.
pub struct Watch<'a> {
    pub shutdown: &'a std::sync::atomic::AtomicBool,
    pub session_active: &'a std::sync::atomic::AtomicBool,
}

/// The error of a [`cut`]: a reset, of its own kind, so the reconnect loop can count it apart from a phone that resets
/// the link itself ([`is_cut`]; the Mac's review of bbd29f3, A1).
#[derive(Debug)]
struct SdpCut;

impl std::fmt::Display for SdpCut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("answered, no SDP channel")
    }
}

impl std::error::Error for SdpCut {}

/// The error a [`cut`] returns.
pub(crate) fn cut_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionReset, SdpCut)
}

/// Whether `e` came from a [`cut`].
pub fn is_cut(e: &std::io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<SdpCut>())
}

/// What a connect in flight does next, from one look ([`wait_connected`]).
#[derive(Debug, PartialEq, Eq)]
enum Next {
    Wait,
    Connected,
    /// The connect failed with this errno: EHOSTDOWN for a page that timed out, ECONNRESET for a reset.
    Failed(i32),
    /// Closed with no error given.
    Closed,
    /// Answered, and no SDP channel within the window: [`cut`].
    Cut,
}

/// The decision of one look: `polled` is poll's revents and the socket's SO_ERROR when poll said something,
/// `answered_for` how long since the page was answered (None: not yet), `after_acl` the SDP channel's window.
fn next_step(polled: Option<(i16, i32)>, answered_for: Option<std::time::Duration>, after_acl: std::time::Duration) -> Next {
    if let Some((revents, err)) = polled {
        if err != 0 {
            return Next::Failed(err);
        }
        if revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Next::Closed;
        }
        if revents & libc::POLLOUT != 0 {
            return Next::Connected;
        }
    }
    match answered_for {
        Some(d) if d >= after_acl => Next::Cut,
        _ => Next::Wait,
    }
}

/// Wait for the non-blocking connect on `fd`, polled here in [`POLL_SLICE`]s, until `timeout` from `start` (the page) or
/// the loop's shutdown. Once the page is answered ([`crate::hci::AclInfo::answered`]) the SDP channel gets
/// [`sdp_after_acl`]; past it the connect is [`cut`]. Polled in this thread, not cut from another: a shutdown() from a
/// watch thread left the socket without an error, so connect() returned only at its 8 s timeout (the Mac's review of
/// 376c4ce, F2). `link_before`: why a link up before the page is to be left alone by a cut ([`before_page`]).
fn wait_connected(
    fd: std::os::fd::OwnedFd,
    peer: [u8; 6],
    start: std::time::Instant,
    timeout: std::time::Duration,
    link_before: Option<&'static str>,
    watch: &Watch<'_>,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::Ordering;
    let dev = crate::hci::dev_id(crate::HCI_DEV).ok();
    let after_acl = sdp_after_acl();
    let mut answered: Option<(std::time::Instant, u16)> = None;
    loop {
        if watch.shutdown.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "shutting down"));
        }
        let left = timeout.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the SDP connect timed out"));
        }
        let mut pfd = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
        // SAFETY: one pollfd for this socket.
        let r = unsafe { libc::poll(&mut pfd, 1, left.min(POLL_SLICE).as_millis().max(1) as libc::c_int) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        let polled = if r > 0 { Some((pfd.revents, so_error(fd.as_raw_fd())?)) } else { None };
        if let (None, None, Some(dev)) = (polled, answered, dev) {
            if let Some(acl) = crate::hci::acl_info(dev, peer).filter(|a| a.answered()) {
                answered = Some((std::time::Instant::now(), acl.handle));
                // The timeline's `acl`: the phone answered, as this 25 ms look first saw it.
                crate::startup::stamp("acl");
            }
        }
        match next_step(polled, answered.map(|(at, _)| at.elapsed()), after_acl) {
            Next::Wait => {}
            Next::Connected => return Ok(fd),
            Next::Failed(err) => return Err(std::io::Error::from_raw_os_error(err)),
            Next::Closed => {
                return Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "the SDP channel closed while connecting"));
            }
            Next::Cut => {
                let (at, handle) = answered.expect("a cut follows an answer");
                // Not a link this page brought up, or one a live session rides: close the socket and leave the link.
                // The kernel reuses an existing ACL for this page, so the phone's own (the accept path taking its
                // session mid-page, an SSP re-pair from Settings) would read as answered at once (the Mac's review, A2).
                let keep = link_before.or_else(|| watch.session_active.load(Ordering::Relaxed).then_some("a session is live"));
                return Err(cut(fd, dev.unwrap_or(0), peer, handle, (start, at), keep));
            }
        }
    }
}

/// A page answered, and no SDP channel in [`sdp_after_acl`]: close the socket here, disconnect the link it leaves and
/// wait (up to [`CUT_LINK_GONE`]) for it to go, so the caller's next page doesn't ride the same stuck link. With `keep`
/// (why), the link isn't this page's to drop: the socket is closed and nothing else. Its own kind of reset ([`is_cut`]).
fn cut(
    fd: std::os::fd::OwnedFd,
    dev: u16,
    peer: [u8; 6],
    handle: u16,
    (start, answered_at): (std::time::Instant, std::time::Instant),
    keep: Option<&str>,
) -> std::io::Error {
    let answered_ms = answered_at.duration_since(start).as_millis();
    let cut_ms = start.elapsed().as_millis();
    drop(fd);
    crate::startup::tally("sdp_cut");
    let error = cut_error();
    let head = format!(
        "answered at +{answered_ms} ms, but no SDP channel by +{cut_ms} ms ({} ms after the answer): cut",
        cut_ms.saturating_sub(answered_ms)
    );
    if let Some(why) = keep {
        log(&format!("{head}; {why}: socket closed, no Disconnect"));
        return error;
    }
    let sent = match crate::hci::disconnect(dev, handle) {
        Ok(()) => true,
        Err(e) => {
            log(&format!("Disconnect of the cut link not sent: {e}"));
            false
        }
    };
    let t = std::time::Instant::now();
    while t.elapsed() < CUT_LINK_GONE && crate::hci::acl_info(dev, peer).is_some() {
        std::thread::sleep(POLL_SLICE);
    }
    let gone = crate::hci::acl_info(dev, peer).is_none();
    note_left_link(peer, handle, !gone, sent);
    log(&format!(
        "{head}; the link {} {} ms after its Disconnect",
        if gone { "went" } else { "was still up" },
        t.elapsed().as_millis()
    ));
    error
}

/// The link a cut left behind (the Mac's review of 8a3a8b2, item 5): still up after its Disconnect, or its Disconnect
/// never sent. The next page to that phone would ride it and take it for the phone's own; it is told apart by its
/// handle, forgotten once a read finds no link to the phone or a page to it connects, and its Disconnect is sent again
/// only when it was never sent, and only by the page right after the cut (`fresh`), since handles are reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LeftLink {
    peer: [u8; 6],
    handle: u16,
    sent: bool,
    fresh: bool,
    /// When the cut left it: a resend is for the page right after, within [`LEFT_LINK_RESEND`], never a later session's.
    at: std::time::Instant,
}

/// How long after its cut a left link's Disconnect may be sent again, twice [`CUT_LINK_GONE`] (the Mac's review of round
/// 3): the page right after a cut comes within this; a link with that handle later is a new one.
const LEFT_LINK_RESEND: std::time::Duration = std::time::Duration::from_millis(3000);

/// Forget the left link: btd's bring-up resets the controller, so no handle from before it is valid. At the start of
/// the reconnect loop.
pub(crate) fn forget_left_link() {
    *LEFT_LINK.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Forget the left link for `peer` when a read finds no link to it (`acl`).
fn forget_gone(left: &mut Option<LeftLink>, peer: [u8; 6], acl: Option<crate::hci::AclInfo>) {
    if acl.is_none() {
        forget_peer(left, peer);
    }
}

/// Forget the left link if it is `peer`'s: one record serves every phone, and another phone's is not this one's to drop.
fn forget_peer(left: &mut Option<LeftLink>, peer: [u8; 6]) {
    if left.is_some_and(|l| l.peer == peer) {
        *left = None;
    }
}

/// How a page ended, for the left link's record ([`page_ended`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageEnd {
    /// The SDP channel came up.
    Connected,
    /// Answered, and no SDP channel: [`cut`] noted what it left.
    Cut,
    /// Any other failure.
    Failed,
}

/// The left link's record after a page to `peer` ended (`end`), `acl` reading the link to it now. A page that connected
/// rode or brought up the phone's link, so this phone's record goes (the Mac's 2.6.0 review, CS-1); one that failed
/// otherwise forgets it when no link is left, so the phone's own link later isn't taken for btd's (the review of round 3).
fn page_ended(left: &mut Option<LeftLink>, peer: [u8; 6], end: PageEnd, acl: impl FnOnce() -> Option<crate::hci::AclInfo>) {
    match end {
        PageEnd::Connected => forget_peer(left, peer),
        PageEnd::Cut => {}
        PageEnd::Failed => forget_gone(left, peer, acl()),
    }
}

static LEFT_LINK: std::sync::Mutex<Option<LeftLink>> = std::sync::Mutex::new(None);

/// What a page finds up to its phone before it starts ([`link_before_page`]).
#[derive(Debug, PartialEq, Eq)]
enum Before {
    Nothing,
    /// A link btd didn't leave: the phone's own, or a session's. A cut keeps it.
    PhonesLink,
    /// btd's own from the last cut; `resend`: its Disconnect was never sent, and this is the page right after.
    OwnLink { resend: bool },
}

/// The decision for the link `acl` up to `peer` before a page at `now`, given the last cut's `left` link, which it
/// updates: a record for this phone whose link is gone, or has another handle, is forgotten. Never a resend while a
/// session is live (`session_live`): its link may be this one (the Mac's review of round 3).
fn link_before_page(
    peer: [u8; 6],
    acl: Option<crate::hci::AclInfo>,
    left: &mut Option<LeftLink>,
    now: std::time::Instant,
    session_live: bool,
) -> Before {
    let mine = left.filter(|l| l.peer == peer);
    let out = match (acl, mine) {
        (None, Some(_)) => {
            *left = None;
            Before::Nothing
        }
        (None, None) => Before::Nothing,
        (Some(a), Some(l)) if l.handle == a.handle => Before::OwnLink {
            resend: !l.sent && l.fresh && now.saturating_duration_since(l.at) < LEFT_LINK_RESEND && !session_live,
        },
        (Some(_), Some(_)) => {
            *left = None;
            Before::PhonesLink
        }
        (Some(_), None) => Before::PhonesLink,
    };
    if let Some(l) = left.as_mut() {
        l.fresh = false;
    }
    out
}

/// Look at the link up to `peer` just before its page: the phone's own (kept by a cut, the reason returned), btd's own
/// from the last cut (logged as such, its Disconnect sent again when it never went out), or none.
fn before_page(dev: u16, peer: [u8; 6], session_live: bool) -> Option<&'static str> {
    let acl = crate::hci::acl_info(dev, peer);
    let mut left = LEFT_LINK.lock().unwrap_or_else(|e| e.into_inner());
    match (link_before_page(peer, acl, &mut left, std::time::Instant::now(), session_live), acl) {
        (Before::Nothing, _) | (Before::OwnLink { .. }, None) => None,
        (Before::PhonesLink, _) => Some("the link was up before the page"),
        (Before::OwnLink { resend }, Some(a)) => {
            log(&format!(
                "the link up before this page is btd's own from the last cut (handle 0x{:04x}, state {}){}",
                a.handle,
                a.state,
                if resend { ": its Disconnect was never sent, sending it now" } else { "" }
            ));
            if resend {
                if let Some(l) = left.as_mut() {
                    l.sent = true;
                }
                if crate::hci::disconnect(dev, a.handle).is_ok() {
                    let t = std::time::Instant::now();
                    while t.elapsed() < CUT_LINK_GONE && crate::hci::acl_info(dev, peer).is_some() {
                        std::thread::sleep(POLL_SLICE);
                    }
                }
                if crate::hci::acl_info(dev, peer).is_none() {
                    *left = None;
                    return None;
                }
            }
            // Still up: this page rides it, and a cut leaves it to the kernel rather than disconnect it again.
            Some("btd's own link from the last cut is still up")
        }
    }
}

/// Note the link a cut leaves: up after its Disconnect (`sent`), or with none sent; or gone (`up` false).
fn note_left_link(peer: [u8; 6], handle: u16, up: bool, sent: bool) {
    let mut left = LEFT_LINK.lock().unwrap_or_else(|e| e.into_inner());
    note_left(&mut left, peer, handle, up, sent, std::time::Instant::now());
}

/// [`note_left_link`] on `left`, at `at`: a link still up replaces the record; one that went forgets only this phone's,
/// so another phone's cut never wipes a record still up for the first (the Mac's 2.6.0 review, CS-1).
fn note_left(left: &mut Option<LeftLink>, peer: [u8; 6], handle: u16, up: bool, sent: bool, at: std::time::Instant) {
    if up {
        *left = Some(LeftLink { peer, handle, sent, fresh: true, at });
    } else {
        forget_peer(left, peer);
    }
}

/// L2CAP-connect to `peer`'s SDP PSM (implicitly pages the phone in BlueZ). `timeout_secs` bounds both
/// the connect and each subsequent read so a quiet phone can't wedge the reconnect thread. The connect itself is
/// non-blocking, watched by [`wait_connected`].
fn connect_sdp(peer: [u8; 6], timeout_secs: i64, watch: &Watch<'_>) -> std::io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let raw = unsafe {
        libc::socket(
            AF_BLUETOOTH as libc::c_int,
            libc::SOCK_SEQPACKET | crate::cloexec::SOCK_CLOEXEC,
            BTPROTO_L2CAP,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a fresh descriptor, owned from here: every early return closes it.
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    // tv_sec's width is target-dependent (i32 on armv7-musl, i64 on the dev host); `as _` infers the
    // field type from the struct literal so both targets build without naming the deprecated time_t alias.
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    tv.tv_sec = timeout_secs as _;
    for opt in [libc::SO_SNDTIMEO, libc::SO_RCVTIMEO] {
        let ret = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let addr = SockaddrL2 {
        l2_family: AF_BLUETOOTH,
        l2_psm: SDP_PSM,
        l2_bdaddr: peer,
        l2_cid: 0,
        l2_bdaddr_type: 0,
    };
    set_nonblocking(fd.as_raw_fd(), true)?;
    // A link to the phone already up (one it opened, or a session's): this page rides it, and a cut leaves it alone.
    // btd's own, left by its last cut, is told apart (before_page).
    let link_before = crate::hci::dev_id(crate::HCI_DEV)
        .ok()
        .and_then(|dev| before_page(dev, peer, watch.session_active.load(std::sync::atomic::Ordering::Relaxed)));
    // The timeline's `page_start`: the connect below is what pages the phone; the session's first page stands.
    // Before `start`, so the page's own clock does not carry the stamp's write.
    crate::startup::stamp("page_start");
    let start = std::time::Instant::now();
    let ret = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &addr as *const SockaddrL2 as *const libc::sockaddr,
            std::mem::size_of::<SockaddrL2>() as libc::socklen_t,
        )
    };
    let connected = if ret == 0 {
        Ok(fd)
    } else {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EINPROGRESS) {
            wait_connected(fd, peer, start, std::time::Duration::from_secs(timeout_secs.max(1) as u64), link_before, watch)
        } else {
            Err(e)
        }
    };
    // The left link's record for this phone after the page (page_ended).
    let end = match &connected {
        Ok(_) => PageEnd::Connected,
        Err(e) if is_cut(e) => PageEnd::Cut,
        Err(_) => PageEnd::Failed,
    };
    let acl = || crate::hci::dev_id(crate::HCI_DEV).ok().and_then(|dev| crate::hci::acl_info(dev, peer));
    page_ended(&mut LEFT_LINK.lock().unwrap_or_else(|p| p.into_inner()), peer, end, acl);
    let fd = connected?;
    // Connected with no answer seen first (a connect that completed at once, or no hci device to look at): the link
    // is up by now. The answer's own time, when it was seen, stands.
    crate::startup::stamp("acl");
    // Blocking again, for the SDP exchange and its SO_RCVTIMEO-bounded reads.
    set_nonblocking(fd.as_raw_fd(), false)?;
    Ok(std::fs::File::from(fd))
}

/// The iAP2 UUID128 search pattern: `DES { UUID128 iAP2 }`.
fn search_pattern_iap2() -> Vec<u8> {
    let mut p = Vec::with_capacity(19);
    p.push(0x35); // DES, 1-byte length follows
    p.push(0x11); // length = 17 (1 desc byte + 16 UUID bytes)
    p.push(0x1c); // UUID128 descriptor
    p.extend_from_slice(&IAP2_UUID128);
    p
}

/// The L2CAP UUID16 (0x0100) search pattern: `DES { UUID16 0x0100 }`. Every connectable BR/EDR
/// service's ProtocolDescriptorList contains L2CAP, so this matches essentially ALL of the peer's
/// SDP records — a browse-all, to enumerate what iOS actually exposes (docs/wireless/01_BT_AND_RADIO.md open unknown).
fn search_pattern_l2cap() -> Vec<u8> {
    vec![0x35, 0x03, 0x19, 0x01, 0x00]
}

/// Build a `ServiceSearchAttributeRequest` param block for `search_pattern`: max attr bytes 0xFFFF,
/// the full 0x0000..0xFFFF attribute range, and `cont` as the trailing continuation state.
fn build_ssa_request(search_pattern: &[u8], cont: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(search_pattern.len() + 12);
    p.extend_from_slice(search_pattern);
    // MaximumAttributeByteCount
    p.extend_from_slice(&0xFFFFu16.to_be_bytes());
    // AttributeIDList: DES { uint32 range 0x0000_FFFF }
    p.push(0x35);
    p.push(0x05);
    p.push(0x0a); // uint32 descriptor
    p.extend_from_slice(&0x0000_FFFFu32.to_be_bytes());
    // ContinuationState
    p.extend_from_slice(cont);
    p
}

/// Run one `ServiceSearchAttributeRequest` transaction over `sock` for `search_pattern`, reassembling
/// any continuation chunks. Returns the assembled attribute-list blob (may be empty `35 00`).
fn run_search(sock: &mut std::fs::File, search_pattern: &[u8], first_tid: u16) -> std::io::Result<Vec<u8>> {
    let mut attr_lists: Vec<u8> = Vec::new();
    let mut cont: Vec<u8> = vec![0x00];
    let mut tid = first_tid;
    let mut buf = [0u8; 4096];
    // Bound the continuation reassembly (audit B5): a peer returning endless non-zero continuation cookies
    // (or a zero-progress `count==0` each round) must not loop forever or grow `attr_lists` unbounded — the
    // one reassembly path in the crate that previously had no ceiling (contrast bt_driver's MAX_REASSEMBLY).
    const MAX_ROUNDS: usize = 64;
    const MAX_ATTR_BYTES: usize = 64 * 1024;
    let mut rounds = 0usize;
    loop {
        rounds += 1;
        if rounds > MAX_ROUNDS || attr_lists.len() > MAX_ATTR_BYTES {
            log(&format!("SDP reassembly exceeded bound (rounds={rounds}, bytes={}) — stopping", attr_lists.len()));
            break;
        }
        let params = build_ssa_request(search_pattern, &cont);
        let mut pdu = Vec::with_capacity(5 + params.len());
        pdu.push(SDP_SVC_SEARCH_ATTR_REQ);
        pdu.extend_from_slice(&tid.to_be_bytes());
        pdu.extend_from_slice(&(params.len() as u16).to_be_bytes());
        pdu.extend_from_slice(&params);
        sock.write_all(&pdu)?;

        let n = sock.read(&mut buf)?;
        if n < 5 {
            log(&format!("runt SDP response ({n} bytes)"));
            break;
        }
        let rpdu = buf[0];
        let plen = u16::from_be_bytes([buf[3], buf[4]]) as usize;
        if rpdu == SDP_ERROR_RSP {
            log(&format!("phone returned SDP_ERROR_RSP ({})", hex(&buf[5..n.min(5 + plen)])));
            break;
        }
        if rpdu != SDP_SVC_SEARCH_ATTR_RSP || 5 + plen > n {
            log(&format!("unexpected SDP response pdu=0x{rpdu:02x} plen={plen} n={n}"));
            break;
        }
        let params = &buf[5..5 + plen];
        if params.len() < 2 {
            break;
        }
        let count = u16::from_be_bytes([params[0], params[1]]) as usize;
        if 2 + count > params.len() {
            log("SDP response chunk length overruns PDU");
            break;
        }
        attr_lists.extend_from_slice(&params[2..2 + count]);
        let cs = &params[2 + count..];
        match cs.first() {
            Some(0) | None => break,
            Some(&len) => {
                let want = 1 + len as usize;
                if cs.len() < want {
                    log("truncated continuation state");
                    break;
                }
                cont = cs[..want].to_vec();
                tid = tid.wrapping_add(1);
            }
        }
    }
    Ok(attr_lists)
}

/// Scan an attribute-list blob for an RFCOMM `ProtocolDescriptorList` entry and return its channel.
/// The canonical encoding of "RFCOMM, channel N" is `19 00 03  08 NN` (UUID16 0x0003 = RFCOMM, then
/// a uint8 = the channel) — byte-identical to what `sdp_server.rs` itself emits (line ~62), so this
/// is the spec encoding, not a heuristic. Returns the first channel found.
fn scan_rfcomm_channel(blob: &[u8]) -> Option<u8> {
    let mut i = 0;
    while i + 4 < blob.len() {
        if blob[i] == 0x19 && blob[i + 1] == 0x00 && blob[i + 2] == 0x03 && blob[i + 3] == 0x08 {
            return Some(blob[i + 4]);
        }
        i += 1;
    }
    None
}

/// Pull attribute `0x0311 SupportedFeatures` out of an attribute-list blob.
///
/// What a bonded peer turned out to expose, from ONE SDP conversation.
///
/// One struct rather than several functions because every search MUST share one L2CAP channel: the
/// L2CAP connect is what pages the phone, and a second `query`-style call would page again and
/// spend most of its time on the connect.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Services {
    /// RFCOMM channel of the phone's "Wireless iAP v2" service — an iPhone.
    pub iap2: Option<u8>,
}

/// Discover what the bonded `peer` exposes, over ONE SDP channel: its iAP2 service (an iPhone). `Err`
/// only on connect/transport failure — a peer without one yields `Services::default()` with its raw
/// catalog logged.
/// Handles SDP continuation by re-requesting with the cookie.
pub fn query(peer: [u8; 6], timeout_secs: i64, watch: &Watch<'_>) -> std::io::Result<Services> {
    let mut disp = peer;
    disp.reverse(); // human-readable bdaddr for the log only
    let human = disp.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
    log(&format!("querying SDP on {human} (PSM 0x{SDP_PSM:04x})"));
    let mut sock = connect_sdp(peer, timeout_secs, watch)?;
    log("L2CAP SDP channel up (phone paged)");

    // (1) Targeted: the accessory-side iAP2 UUID. If iOS exposes an iAP2 RFCOMM service by this UUID,
    //     this pulls its channel directly.
    let iap2 = run_search(&mut sock, &search_pattern_iap2(), 1)?;
    log(&format!("iAP2-UUID search -> {} bytes: {}", iap2.len(), hex(&iap2)));
    if let Some(ch) = scan_rfcomm_channel(&iap2) {
        log(&format!("iAP2 RFCOMM channel on the phone = {ch}"));
        // The timeline's `sdp`: the phone's iAP2 channel is known.
        crate::startup::stamp("sdp");
        return Ok(Services { iap2: Some(ch) });
    }

    // (2) Browse-all (L2CAP UUID): dump the peer's ENTIRE SDP catalog so we can see what it DOES
    //     expose. Purely a diagnostic for a peer without the iAP2 service.
    let all = run_search(&mut sock, &search_pattern_l2cap(), 100)?;
    log(&format!("browse-all (L2CAP) -> {} bytes: {}", all.len(), hex(&all)));
    let mut chans = Vec::new();
    let mut i = 0;
    while i + 4 < all.len() {
        if all[i] == 0x19 && all[i + 1] == 0x00 && all[i + 2] == 0x03 && all[i + 3] == 0x08 {
            chans.push(all[i + 4]);
            i += 5;
        } else {
            i += 1;
        }
    }
    if chans.is_empty() {
        log("browse-all: the peer exposes NO RFCOMM service on BR/EDR — the accessory cannot connect OUT (redirects Model B; see docs/wireless/01_BT_AND_RADIO.md)");
    } else {
        log(&format!(
            "browse-all: the peer exposes RFCOMM channel(s) {chans:?} — but none under the iAP2 UUID; raw catalog above identifies the service UUIDs"
        ));
    }
    Ok(Services::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Item 5 (the Mac's review of 8a3a8b2): btd's own link from its last cut is told from the phone's by its handle,
    /// forgotten once there is no link, and its Disconnect is sent again only when it never went out, and only by the
    /// page right after the cut.
    #[test]
    fn a_link_left_by_the_last_cut_is_told_apart() {
        let peer = [1, 2, 3, 4, 5, 6];
        let other = [9, 2, 3, 4, 5, 6];
        let now = std::time::Instant::now();
        let acl = |handle| Some(crate::hci::AclInfo { handle, state: crate::hci::BT_CONNECTED });
        let left = |sent| Some(LeftLink { peer, handle: 0x0b, sent, fresh: true, at: now });

        let mut l = left(false);
        assert_eq!(link_before_page(peer, acl(0x0b), &mut l, now, false), Before::OwnLink { resend: true }, "never sent: resend");
        assert_eq!(link_before_page(peer, acl(0x0b), &mut l, now, false), Before::OwnLink { resend: false }, "only right after the cut");

        let mut l = left(true);
        assert_eq!(link_before_page(peer, acl(0x0b), &mut l, now, false), Before::OwnLink { resend: false }, "sent once: not again");

        // The Mac's 2.6.0 review, CS-2: never while a session is live, whose link it may be.
        let mut l = left(false);
        assert_eq!(link_before_page(peer, acl(0x0b), &mut l, now, true), Before::OwnLink { resend: false }, "a session is live");

        // Round 3's review: the resend is time-bound, so a later session's link with that handle never gets it.
        let mut l = left(false);
        let later = now + LEFT_LINK_RESEND;
        assert_eq!(link_before_page(peer, acl(0x0b), &mut l, later, false), Before::OwnLink { resend: false }, "too late: no resend");

        let mut l = left(false);
        assert_eq!(link_before_page(peer, acl(0x0c), &mut l, now, false), Before::PhonesLink, "another handle: the phone's own");
        assert_eq!(l, None, "and the record is forgotten");

        let mut l = left(false);
        assert_eq!(link_before_page(other, acl(0x0b), &mut l, now, false), Before::PhonesLink, "another phone's link");
        assert!(l.is_some(), "kept: it is this phone's record");
        assert_eq!(link_before_page(peer, None, &mut l, now, false), Before::Nothing);
        assert_eq!(l, None, "no link to the phone: forgotten");

        let mut l = None;
        assert_eq!(link_before_page(peer, acl(0x0b), &mut l, now, false), Before::PhonesLink, "no cut before it: the phone's");

        // A page that failed otherwise: a record whose link is gone is forgotten; one still up, or another phone's, kept.
        // A cut's stands: the cut noted it (the Mac's 2.6.0 review, CS-2: as connect_sdp ends a page).
        let mut l = left(true);
        page_ended(&mut l, peer, PageEnd::Failed, || acl(0x0b));
        assert!(l.is_some(), "the link still up");
        page_ended(&mut l, other, PageEnd::Failed, || None);
        assert!(l.is_some(), "another phone's page");
        page_ended(&mut l, peer, PageEnd::Cut, || None);
        assert!(l.is_some(), "a cut");
        page_ended(&mut l, peer, PageEnd::Failed, || None);
        assert_eq!(l, None, "failed, and no link left: forgotten");
    }

    /// The Mac's 2.6.0 review, CS-1: one record serves every phone, so another phone's cut whose link went leaves a record
    /// still up for the first, and a page that connects (riding the left link, or not) forgets its phone's record.
    #[test]
    fn the_left_link_is_one_phone_s_until_that_phone_connects() {
        let peer = [1, 2, 3, 4, 5, 6];
        let other = [9, 2, 3, 4, 5, 6];
        let now = std::time::Instant::now();
        let whose = |l: Option<LeftLink>| l.map(|l| (l.peer, l.handle));

        let mut l = None;
        note_left(&mut l, peer, 0x0b, true, false, now);
        assert_eq!(whose(l), Some((peer, 0x0b)), "a cut whose link stayed up: noted");
        note_left(&mut l, other, 0x0c, false, true, now);
        assert_eq!(whose(l), Some((peer, 0x0b)), "another phone's cut whose link went: the first phone's stands");
        page_ended(&mut l, other, PageEnd::Connected, || None);
        assert_eq!(whose(l), Some((peer, 0x0b)), "another phone's page connected");
        page_ended(&mut l, peer, PageEnd::Connected, || None);
        assert_eq!(l, None, "this phone's page connected: forgotten");

        note_left(&mut l, peer, 0x0b, true, false, now);
        note_left(&mut l, peer, 0x0b, false, true, now);
        assert_eq!(l, None, "its own next cut, whose link went: forgotten");
        note_left(&mut l, peer, 0x0b, true, false, now);
        note_left(&mut l, other, 0x0c, true, true, now);
        assert_eq!(whose(l), Some((other, 0x0c)), "another phone's cut whose link stayed up: the newer record");
    }

    /// wait_connected's decision (the Mac's review, A3): a page that timed out fails with EHOSTDOWN, an answer with no SDP
    /// channel past the window is a cut, and a writable socket is connected.
    #[test]
    fn a_connect_in_flight_fails_cuts_or_connects() {
        let w = std::time::Duration::from_millis(700);
        let ms = std::time::Duration::from_millis;
        assert_eq!(next_step(Some((libc::POLLERR | libc::POLLHUP, libc::EHOSTDOWN)), None, w), Next::Failed(libc::EHOSTDOWN));
        assert_eq!(next_step(Some((libc::POLLERR, libc::ECONNRESET)), Some(ms(300)), w), Next::Failed(libc::ECONNRESET));
        assert_eq!(next_step(Some((libc::POLLOUT, 0)), Some(ms(900)), w), Next::Connected, "connected beats a late cut");
        assert_eq!(next_step(Some((libc::POLLHUP, 0)), None, w), Next::Closed);
        assert_eq!(next_step(None, None, w), Next::Wait, "still paging");
        assert_eq!(next_step(None, Some(ms(699)), w), Next::Wait, "answered, inside the window");
        assert_eq!(next_step(None, Some(ms(700)), w), Next::Cut);
        assert!(is_cut(&cut_error()), "a cut is told from a reset");
        assert!(!is_cut(&std::io::Error::from_raw_os_error(libc::ECONNRESET)));
    }

    #[test]
    fn ssa_request_has_iap2_uuid_and_range() {
        let req = build_ssa_request(&search_pattern_iap2(), &[0x00]);
        // DES header + UUID128 descriptor + the 16 UUID bytes
        assert_eq!(&req[0..3], &[0x35, 0x11, 0x1c]);
        assert_eq!(&req[3..19], &IAP2_UUID128);
        // MaxAttributeByteCount 0xFFFF
        assert_eq!(&req[19..21], &[0xff, 0xff]);
        // AttributeIDList: DES uint32 0x0000FFFF
        assert_eq!(&req[21..28], &[0x35, 0x05, 0x0a, 0x00, 0x00, 0xff, 0xff]);
        // trailing continuation
        assert_eq!(req[28], 0x00);
    }

    #[test]
    fn services_default_is_nothing_at_all() {
        assert_eq!(Services::default().iap2, None);
    }

    #[test]
    fn an_empty_attribute_list_yields_no_channel() {
        assert_eq!(scan_rfcomm_channel(&[0x35, 0x00]), None);
    }

    #[test]
    fn l2cap_browse_pattern_is_uuid16_0100() {
        assert_eq!(search_pattern_l2cap(), vec![0x35, 0x03, 0x19, 0x01, 0x00]);
    }

    #[test]
    fn scan_finds_rfcomm_channel() {
        // The canonical RFCOMM descriptor as sdp_server emits it: 19 00 03 08 NN
        let blob = [0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x07];
        assert_eq!(scan_rfcomm_channel(&blob), Some(0x07));
    }

    #[test]
    fn scan_returns_none_without_rfcomm() {
        // L2CAP-only descriptor (UUID 0x0100), no RFCOMM
        let blob = [0x35, 0x03, 0x19, 0x01, 0x00];
        assert_eq!(scan_rfcomm_channel(&blob), None);
    }

    #[test]
    fn scan_picks_channel_after_full_pdl() {
        // Mirrors sdp_server's ProtocolDescriptorList: L2CAP then RFCOMM ch 1
        let blob = [
            0x35, 0x0c, 0x35, 0x03, 0x19, 0x01, 0x00, 0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x01,
        ];
        assert_eq!(scan_rfcomm_channel(&blob), Some(1));
    }
}
