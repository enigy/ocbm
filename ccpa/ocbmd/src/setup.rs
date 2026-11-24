//! M3: the set-up network. An adapter with no Wi-Fi link that no host has spoken to since power-up offers, 20 s after
//! ocbmd's start and for 10 minutes, a set-up network from its own AP (`CarLink-Setup-xxxx`, a public WPA2 passphrase,
//! client isolation: `radio_hal.sh setup_ap_on`), and listens on 192.168.43.1:7021 for the OCBS v1 exchange
//! (`ocbm_link::setup_box_handshake`: P-256 ECDH, then OCBW records). One client at a time: the handshake within 5 s in
//! all, then 60 s in all to MGMT_WIFI_LINK (Carlink shows the driver the network's name and waits for a tap first). Over
//! that session the box takes CT_HELLO and MGMT_WIFI_LINK and nothing else, answered on the set-up link itself
//! (main.rs `Daemon::setup_frame`): no info, no clock, no console, no file push, since trust is first-come. A set-up
//! link never holds the box and never counts as a host. The window ends after one set-up, at its 10 minutes, or at a
//! USB host's first frame; the set-up network goes, and the CarPlay AP's start stops it first in any case.
//!
//! Each exchange's two public points are kept ([`Setup::transcript`]) for a short confirmation code the owner has not
//! decided on yet (notes/ocbm.md, "M3 additions").

use ocbm_link::{Transcript, SETUP_PORT};
use std::fs::File;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The set-up network is offered this long after ocbmd's start (a USB host or the bridge has had its chance)...
pub const OFFER_AFTER: Duration = Duration::from_secs(20);
/// ...for this long.
pub const WINDOW: Duration = Duration::from_secs(600);
/// The exchange itself.
pub const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);
/// From the end of the exchange to MGMT_WIFI_LINK (Carlink's confirm tap comes first, 20 s at most).
pub const TO_LINK: Duration = Duration::from_secs(60);
/// The set-up AP's bring-up gets this long to say it is up.
const RAISE_LIMIT: Duration = Duration::from_secs(30);
/// radio_hal.sh's word that the set-up AP is up ("ok <iface>"), and removed when it goes.
#[cfg(not(test))]
const AP_MARK: &str = "/tmp/.radio_setup_ap";
#[cfg(test)]
const AP_MARK: &str = "/tmp/ocbmd_selftest_radio_setup_ap";
#[cfg(not(test))]
const HAL: &str = "/script/radio_hal.sh";
#[cfg(test)]
const HAL: &str = "/tmp/ocbmd_selftest_no_such_radio_hal.sh";
/// The set-up AP's hostapd configuration, there from its start's first step to its teardown's last.
#[cfg(not(test))]
const AP_CONF: &str = "/tmp/hostapd_setup.conf";
#[cfg(test)]
const AP_CONF: &str = "/tmp/ocbmd_selftest_hostapd_setup.conf";

/// A set-up network is up, or on its way up, by radio_hal.sh's files.
fn ap_up() -> bool {
    std::path::Path::new(AP_MARK).exists() || std::path::Path::new(AP_CONF).exists()
}

/// A set-up network this process did not raise must not stay up on its own (the review of 24 Nov, H): an earlier
/// ocbmd's, which exited or respawned with its window open, at this one's start; and any, once a host's HELLO is taken
/// ([`host_seen`]), whatever its session (a wired one, or one with wireless off, raises no CarPlay AP, whose start is
/// what takes a set-up network down otherwise).
pub fn orphan_down(why: &str) {
    if ap_up() {
        eprintln!("[ocbmd] setup: a set-up network is up {why}: taking it down");
        hal("setup_ap_off");
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    /// Not yet: offered at `since` + [`OFFER_AFTER`] if nothing has spoken by then.
    Waiting(Instant),
    /// The set-up AP is coming up since then.
    Raising(Instant),
    /// Up, the listener running, until then.
    Offered(Instant),
    /// Done for this start (a set-up, a host, the window, or never wanted).
    Over,
}

/// The set-up window's state in the daemon.
pub struct Setup {
    pub phase: Phase,
    pub listener: Option<crate::wifi::Listener>,
    /// The listener takes the next client only while this is false (one at a time).
    busy: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Each authenticated exchange's points, by link id (for the later code; nothing reads them yet).
    pub transcripts: Arc<Mutex<Vec<(u64, Transcript)>>>,
    /// A set-up link's MGMT_WIFI_LINK was stored: the window ends once it is applied.
    pub linked: bool,
}

impl Setup {
    /// Never offered (the link is set up, or the start is not one that may offer it).
    pub fn never() -> Setup {
        Setup { phase: Phase::Over, listener: None, busy: Arc::default(), stop: Arc::default(), transcripts: Arc::default(), linked: false }
    }

    pub fn waiting(now: Instant) -> Setup {
        Setup { phase: Phase::Waiting(now), ..Setup::never() }
    }

    pub fn active(&self) -> bool {
        matches!(self.phase, Phase::Raising(_) | Phase::Offered(_))
    }

    /// A set-up link has ended: the listener may take the next client.
    pub fn link_ended(&self) {
        self.busy.store(false, Ordering::Release);
    }

    /// The kept transcript of link `id`, if any (for the later short code; the tests read it).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn transcript(&self, id: u64) -> Option<Transcript> {
        self.transcripts.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(i, _)| *i == id).map(|(_, t)| t.clone())
    }

    /// One step of the window: offer it when due, start the listener once the AP is up, end it at its time.
    /// `quiet`: no host has spoken and no session holds the box. Returns a line for the log when the phase changed.
    pub fn tick(&mut self, now: Instant, quiet: bool) -> Option<String> {
        match self.phase {
            Phase::Waiting(since) if now.duration_since(since) >= OFFER_AFTER => {
                if !quiet {
                    self.phase = Phase::Over;
                    return Some("a host is here: no set-up network".into());
                }
                let _ = std::fs::remove_file(AP_MARK);
                hal("setup_ap_on");
                self.phase = Phase::Raising(now);
                Some("no Wi-Fi link and no host: raising the set-up network (CarLink-Setup-xxxx, 10 minutes)".into())
            }
            Phase::Raising(since) => {
                if std::fs::read_to_string(AP_MARK).is_ok_and(|m| m.starts_with("ok")) {
                    match self.start_listener(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 43, 1), SETUP_PORT))) {
                        Ok(()) => {
                            self.phase = Phase::Offered(now + WINDOW);
                            let _ = std::fs::remove_file(format!("{ROOT}{SETUP_ONCE}")); // a reset's leave is spent
                            return Some(format!("set-up network up; the set-up exchange on 192.168.43.1:{SETUP_PORT}, one client at a time"));
                        }
                        Err(e) => {
                            self.close();
                            return Some(format!("set-up listener could not start ({e}); set-up network taken down"));
                        }
                    }
                }
                if now.duration_since(since) >= RAISE_LIMIT {
                    self.close();
                    return Some("the set-up network did not come up (hostapd refused its configuration?); not offered".into());
                }
                None
            }
            Phase::Offered(until) if now >= until => {
                self.close();
                Some("the set-up window is over (10 minutes); set-up network taken down".into())
            }
            _ => None,
        }
    }

    /// End the window: the listener stops, the set-up AP goes.
    pub fn close(&mut self) {
        if matches!(self.phase, Phase::Over | Phase::Waiting(_)) {
            self.phase = Phase::Over;
            return;
        }
        self.stop.store(true, Ordering::Release);
        self.listener = None;
        hal("setup_ap_off");
        self.phase = Phase::Over;
    }

    /// The listener: one client at a time, the exchange within [`HANDSHAKE_LIMIT`].
    pub fn start_listener(&mut self, addr: SocketAddr) -> std::io::Result<()> {
        let l = TcpListener::bind(addr)?;
        let fixed = l.local_addr().ok();
        let (tx, rx) = mpsc::channel();
        let (wake, wake_tx) = crate::cloexec_pipe()?;
        crate::set_nonblocking(&wake);
        let (busy, stop, ts) = (self.busy.clone(), self.stop.clone(), self.transcripts.clone());
        std::thread::Builder::new()
            .name("setup-listen".into())
            .stack_size(96 * 1024)
            .spawn(move || listen(l, busy, stop, ts, tx, wake_tx))?;
        self.listener = Some(crate::wifi::Listener { rx, wake, fixed });
        Ok(())
    }
}

/// A radio_hal.sh verb, fire and forget (ocbmd ignores SIGCHLD: nothing may wait on the child); the box log has its
/// lines, and `setup_ap_on` leaves [`AP_MARK`] when the AP is up.
fn hal(verb: &str) {
    if !std::path::Path::new(HAL).exists() {
        return;
    }
    let log = || std::fs::OpenOptions::new().append(true).create(true).open("/tmp/box.log").ok();
    let (out, err) = match (log(), log()) {
        (Some(o), Some(e)) => (std::process::Stdio::from(o), std::process::Stdio::from(e)),
        _ => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let _ = std::process::Command::new("setsid").args(["sh", HAL, verb]).stdin(std::process::Stdio::null()).stdout(out).stderr(err).spawn();
}

/// The listener's thread: accepts, and while a client is in the exchange or holds a set-up link (`busy`) closes every
/// other at once. The exchange runs on a thread of its own, within [`HANDSHAKE_LIMIT`] in all, so a slow client never
/// keeps this thread from the stop flag or from closing the others.
fn listen(l: TcpListener, busy: Arc<AtomicBool>, stop: Arc<AtomicBool>, ts: Arc<Mutex<Vec<(u64, Transcript)>>>, tx: mpsc::Sender<crate::wifi::Authed>, wake: File) {
    let mut id = 1u64 << 32; // set-up links' ids, apart from the Wi-Fi listener's
    while !stop.load(Ordering::Acquire) {
        let mut pfd = libc::pollfd { fd: l.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd; a 500 ms bound so the stop flag is looked at.
        if unsafe { libc::poll(&mut pfd, 1, 500) } <= 0 {
            continue;
        }
        let Ok((s, peer)) = l.accept() else { continue };
        if busy.swap(true, Ordering::AcqRel) {
            eprintln!("[ocbmd] setup: refused {}: one client at a time", crate::wifi::peer_short(&peer));
            continue; // dropped: closed
        }
        id += 1;
        let (hb, hts, htx, hw) = (busy.clone(), ts.clone(), tx.clone(), wake.try_clone());
        let hs = move || {
            let r = ocbm_link::setup_keypair()
                .map_err(ocbm_link::LinkError::Io)
                .and_then(|(sk, pk)| ocbm_link::setup_box_handshake(&mut crate::wifi::Deadline::new(&s, HANDSHAKE_LIMIT), &sk, &pk));
            match r {
                Ok((session, t)) => {
                    hts.lock().unwrap_or_else(|e| e.into_inner()).push((id, t));
                    eprintln!("[ocbmd] setup: {} completed the set-up exchange (link {id})", crate::wifi::peer_short(&peer));
                    if htx.send(crate::wifi::Authed { id, peer, stream: s, session }).is_ok() {
                        if let Ok(mut w) = hw {
                            let _ = w.write(&[1]);
                        }
                        return;
                    }
                }
                Err(e) => eprintln!("[ocbmd] setup: refused {}: {e}", crate::wifi::peer_short(&peer)),
            }
            hb.store(false, Ordering::Release);
        };
        if std::thread::Builder::new().name("setup-hs".into()).stack_size(96 * 1024).spawn(hs).is_err() {
            eprintln!("[ocbmd] setup: no thread for the exchange; {} dropped", crate::wifi::peer_short(&peer));
            busy.store(false, Ordering::Release);
        }
    }
}

/// The set-up window may open at this start: not switched off by the owner (/etc/ocbm/wifi_setup_off) or the bench
/// (box_common::bench_switch("wifi_setup_off")), and the adapter never used ([`used_because`]).
pub fn wanted() -> bool {
    if std::path::Path::new("/etc/ocbm/wifi_setup_off").exists() {
        eprintln!("[ocbmd] setup: no set-up network (/etc/ocbm/wifi_setup_off)");
        return false;
    }
    if let Some((_, f)) = box_common::bench_switch("wifi_setup_off") {
        eprintln!("[ocbmd] [bench] setup: no set-up network ({f})");
        return false;
    }
    if let Some(why) = used_because(ROOT) {
        eprintln!("[ocbmd] setup: no set-up network: {why}");
        return false;
    }
    true
}

/// The set-up network's history on flash (the review of 24 Nov, A: never on an adapter that has been used, which a
/// stranger in range could otherwise provision). [`HOST_SEEN`]: a host's HELLO was taken here once, over USB or Wi-Fi
/// (a set-up client's never is), written once. [`SETUP_ONCE`]: OCBM_WIFI_RESET's leave (ocbm_dispatch.sh, which also
/// removes the link and HOST_SEEN): the next start may offer the set-up network whatever the history, and the leave
/// goes once the network is up or a host's HELLO is taken.
#[cfg(not(test))]
const ROOT: &str = "";
#[cfg(test)]
const ROOT: &str = "/tmp/ocbmd_selftest_no_such_root";
const HOST_SEEN: &str = "/etc/ocbm/host_seen";
const SETUP_ONCE: &str = "/etc/ocbm/wifi_setup_once";

/// Why this adapter may not offer the set-up network, under `root`; None: never used, or reset. Used means a host's
/// HELLO taken once ([`HOST_SEEN`]), a phone bonded (a Bluetooth bond or a CarPlay pairing), a host's config kept
/// (M2's), or an update recorded: an adapter updated from 2.7.1, which knew nothing of HOST_SEEN, has that record.
pub fn used_because(root: &str) -> Option<&'static str> {
    let has = |f: &str| std::path::Path::new(&format!("{root}{f}")).exists();
    let size = |f: &str| std::fs::metadata(format!("{root}{f}")).map_or(0, |m| m.len());
    if has(SETUP_ONCE) {
        None
    } else if has(HOST_SEEN) {
        Some("a host has used this adapter")
    } else if size("/etc/carplay/bt_link_keys") >= 25 || size("/etc/carplay_peers.bin") > 0 {
        Some("a phone is bonded to it")
    } else if has("/etc/ocbm/wifi_session.yaml") {
        Some("it keeps a host's config")
    } else if has("/etc/ocbm/update_result") || has("/etc/ocbm/update_pending") {
        Some("it was updated")
    } else {
        None
    }
}

/// A host's HELLO taken (main.rs `Daemon::handle`): [`HOST_SEEN`] written if it is not there, and a reset's leave
/// gone. Once a process.
pub fn host_seen() {
    static NOTED: AtomicBool = AtomicBool::new(false);
    if !NOTED.swap(true, Ordering::Relaxed) {
        note_host_at(ROOT);
        orphan_down("with a host here");
    }
}

fn note_host_at(root: &str) {
    let _ = std::fs::remove_file(format!("{root}{SETUP_ONCE}"));
    let f = format!("{root}{HOST_SEEN}");
    if !std::path::Path::new(&f).exists() {
        match std::fs::write(&f, b"1\n") {
            Ok(()) => eprintln!("[ocbmd] setup: a host's first HELLO on this adapter: no set-up network from now on ({HOST_SEEN})"),
            Err(e) => eprintln!("[ocbmd] setup: could not write {HOST_SEEN} ({e})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_waits_then_gives_up_when_a_host_spoke_or_the_ap_never_came() {
        let t0 = Instant::now();
        let mut s = Setup::waiting(t0);
        assert_eq!(s.tick(t0 + OFFER_AFTER - Duration::from_secs(1), true), None);
        assert!(s.tick(t0 + OFFER_AFTER, false).is_some());
        assert_eq!(s.phase, Phase::Over, "a host spoke: never offered");
        let mut s = Setup::waiting(t0);
        let _ = std::fs::remove_file(AP_MARK);
        assert!(s.tick(t0 + OFFER_AFTER, true).is_some());
        assert!(matches!(s.phase, Phase::Raising(_)));
        assert!(s.tick(t0 + OFFER_AFTER + RAISE_LIMIT, true).unwrap().contains("did not come up"));
        assert_eq!(s.phase, Phase::Over);
    }

    /// Who may be offered the set-up network: only an adapter with none of a host's traces, or one a reset left its
    /// leave on; a host's HELLO writes the flag once and takes the leave away.
    #[test]
    fn only_an_adapter_never_used_or_reset_may_offer_the_set_up_network() {
        let root = format!("/tmp/ocbmd_selftest_setup_root_{}", std::process::id());
        let _ = std::fs::remove_dir_all(&root);
        for d in ["/etc/ocbm", "/etc/carplay"] {
            std::fs::create_dir_all(format!("{root}{d}")).unwrap();
        }
        let put = |f: &str, b: &[u8]| std::fs::write(format!("{root}{f}"), b).unwrap();
        let gone = |f: &str| {
            let _ = std::fs::remove_file(format!("{root}{f}"));
        };
        assert_eq!(used_because(&root), None, "a fresh install");
        put("/etc/carplay/bt_link_keys", &[0; 24]);
        assert_eq!(used_because(&root), None, "no whole bond record");
        for (f, b) in [
            ("/etc/carplay/bt_link_keys", &[0u8; 25][..]),
            ("/etc/carplay_peers.bin", b"\x01i"),
            ("/etc/ocbm/wifi_session.yaml", b"cfg"),
            ("/etc/ocbm/update_result", b"committed"),
            ("/etc/ocbm/update_pending", b"placed"),
        ] {
            put(f, b);
            assert!(used_because(&root).is_some(), "{f}");
            put(SETUP_ONCE, b"reset\n");
            assert_eq!(used_because(&root), None, "{f}, but reset: the leave wins");
            gone(SETUP_ONCE);
            gone(f);
        }
        gone("/etc/carplay/bt_link_keys");
        put(SETUP_ONCE, b"reset\n");
        note_host_at(&root);
        assert_eq!(used_because(&root), Some("a host has used this adapter"), "the leave is gone with the first HELLO");
        assert!(!std::path::Path::new(&format!("{root}{SETUP_ONCE}")).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_set_up_network_on_its_way_up_or_up_is_seen_by_its_files() {
        let _ = std::fs::remove_file(AP_CONF);
        assert!(!ap_up() || std::path::Path::new(AP_MARK).exists());
        std::fs::write(AP_CONF, b"interface=wlan0\n").unwrap();
        assert!(ap_up(), "its configuration written: on its way up, or left by an ocbmd that is gone");
        let _ = std::fs::remove_file(AP_CONF);
    }

    fn listening() -> (Setup, SocketAddr) {
        let mut s = Setup::waiting(Instant::now());
        s.start_listener("127.0.0.1:0".parse().unwrap()).unwrap();
        s.phase = Phase::Offered(Instant::now() + WINDOW);
        let addr = s.listener.as_ref().unwrap().fixed.unwrap();
        (s, addr)
    }

    /// What the box sends before it closes, and when it closed (counted from `t0`).
    fn box_says(c: &mut std::net::TcpStream, t0: Instant) -> (Vec<u8>, Duration) {
        use std::io::Read;
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let (mut got, mut b) = (Vec::new(), [0u8; 128]);
        while let Ok(n @ 1..) = c.read(&mut b) {
            got.extend_from_slice(&b[..n]);
        }
        (got, t0.elapsed())
    }

    /// One client at a time, and the exchange within 5 s IN ALL: a client dripping its opening a byte every 300 ms
    /// (in time for every read) holds the one place for 5 s; another meanwhile is closed at once, not left waiting
    /// behind it; then a whole exchange goes through, its transcript kept, and the stop flag ends the listener.
    #[test]
    fn the_listener_takes_one_client_at_a_time_and_cuts_a_slow_one_at_five_seconds() {
        let (s, addr) = listening();
        let mut drip = std::net::TcpStream::connect(addr).unwrap();
        let t0 = Instant::now();
        let w = drip.try_clone().unwrap();
        std::thread::spawn(move || {
            for _ in 0..ocbm_link::SETUP_HELLO_LEN {
                if (&w).write_all(b"O").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        std::thread::sleep(Duration::from_millis(300));
        let mut other = std::net::TcpStream::connect(addr).unwrap();
        let t1 = Instant::now();
        let (said, closed) = box_says(&mut other, t1);
        assert!(said.is_empty() && closed < Duration::from_secs(1), "a second client closed at once ({closed:?})");
        let (said, cut) = box_says(&mut drip, t0);
        assert!(said.is_empty(), "nothing for an opening that never came");
        assert!(cut >= Duration::from_millis(4500) && cut <= Duration::from_millis(6500), "cut after {cut:?}");
        let (hs, hp) = ocbm_link::setup_keypair_from(&[0x11; 32]).unwrap();
        let mut c = std::net::TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let (_sess, t) = ocbm_link::setup_host_handshake(&mut c, &hs, &hp).expect("the place is free again");
        let a = s.listener.as_ref().unwrap().rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(s.transcript(a.id), Some(t), "the transcript kept for a later code");
        s.stop.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(700));
        assert!(std::net::TcpStream::connect(addr).is_err(), "the listener ended with the stop flag");
    }

    /// A point is read and checked before the box says anything: off the curve, compressed, or the magic wrong, the
    /// box closes with nothing sent; a point on the curve gets the box's 70 bytes.
    #[test]
    fn an_opening_with_a_bad_point_is_refused_after_it_is_parsed() {
        let (s, addr) = listening();
        let (_, good) = ocbm_link::setup_keypair_from(&[0x22; 32]).unwrap();
        let mut off_curve = good;
        off_curve[64] ^= 1; // y + 1: not on P-256
        let mut compressed = good;
        compressed[0] = 0x02;
        for (point, magic, want) in [(off_curve, *b"OCBS", 0), (compressed, *b"OCBS", 0), (good, *b"OCBW", 0), (good, *b"OCBS", ocbm_link::SETUP_HELLO_LEN)] {
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            let mut opening = magic.to_vec();
            opening.push(1);
            opening.extend_from_slice(&point);
            c.write_all(&opening).unwrap();
            if want > 0 {
                use std::io::Read;
                let mut theirs = [0u8; ocbm_link::SETUP_HELLO_LEN];
                c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                c.read_exact(&mut theirs).expect("the box's opening");
                assert_eq!(&theirs[..4], b"OCBS");
                let _ = s.listener.as_ref().unwrap().rx.recv_timeout(Duration::from_secs(3)).unwrap();
            } else {
                let (said, _) = box_says(&mut c, Instant::now());
                assert!(said.is_empty(), "closed with nothing sent: {:02x?}", &opening[..6]);
            }
            s.link_ended(); // the next client's turn (the daemon does this when a set-up link ends)
            std::thread::sleep(Duration::from_millis(100));
        }
        s.stop.store(true, Ordering::Release);
    }
}
