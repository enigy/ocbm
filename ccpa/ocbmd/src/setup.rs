//! M3: the set-up network. An adapter with no Wi-Fi link that no host has spoken to since power-up offers, 20 s after
//! ocbmd's start and for 10 minutes, a set-up network from its own AP (`CarLink-Setup-xxxx`, a public WPA2 passphrase,
//! client isolation, 2.4 GHz channel 6 for head units that see no 5 GHz: `radio_hal.sh setup_ap_on`), and listens on
//! 192.168.43.1:7021 for the OCBS v1 exchange
//! (`ocbm_link::setup_box_handshake`: P-256 ECDH, then OCBW records). One client at a time: the handshake within 5 s in
//! all, its first record within 3 s, then 180 s in all to MGMT_WIFI_LINK (the driver compares the set-up code first,
//! below). Over that session the box takes CT_HELLO and MGMT_WIFI_LINK and nothing else, answered on the set-up link
//! itself (main.rs `Daemon::setup_frame`): no info, no clock, no console, no file push, since trust is first-come. A
//! set-up link never holds the box and never counts as a host. The window ends after one set-up, at its 10 minutes (a
//! session in progress then may finish within its own 180 s: no new client, the set-up network up until it ends), at a
//! USB host's first frame, or after [`FAILED_LIMIT`] sessions that showed a code and set nothing up (then for this
//! power-up); the set-up network goes, and the CarPlay AP's start stops it first in any case.
//!
//! THE SET-UP CODE (ADDENDUM 5). It lets the driver see that Carlink is talking to THEIR adapter (not a neighbour's
//! adapter in its own set-up window, not a stale network); it is no proof against an attacker in radio range who also
//! fakes a Bluetooth name, and first-come trust stays the security model. When a set-up link's first record opens (the
//! head unit holds the exchange's keys), the code from the exchange's transcript ([`ocbm_link::setup_code`]) goes on the
//! adapter's Bluetooth: its four digits in `box_common::flags::SETUP_CODE` and SIGUSR1 to the armed btd, which names the
//! controller "CarLink code NNNN" (local name and EIR) and makes it discoverable, inquiry scan only (btd `armed.rs`: the
//! armed state's own wake, the supervisor's go's). When that session ends, for any reason, the file goes and the same
//! signal puts the normal name and invisibility back. Controller and tmpfs only: never /etc/carplay_ident or any file on
//! flash, nothing left by a power cut. A btd armed later (Bluetooth not up yet) reads the file when it arms; with no
//! armed btd at all the code is not shown, and the log says so.

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
/// From the end of the exchange to MGMT_WIFI_LINK, in all (ADDENDUM 5, was 60 s): the driver compares the code Carlink
/// shows with the adapter's name in a phone's or the car's Bluetooth list first (Carlink waits 150 s at most).
pub const TO_LINK: Duration = Duration::from_secs(180);
/// A set-up link whose first record has not opened this long after the exchange is closed (DECIDED here, notes/ocbm.md
/// 24 Nov: not in the plan). Carlink's first keepalive comes after 1 s, and the dead rule counts bytes, not records: a
/// peer dripping a record that never ends would otherwise hold the one place, no code shown, for TO_LINK.
pub const FIRST_RECORD_WITHIN: Duration = ocbm_link::DEAD_AFTER;
/// Set-up sessions in one window that showed a code and ended with no MGMT_WIFI_LINK stored: at this many the set-up
/// network closes for this power-up (ADDENDUM 5's abuse limit).
pub const FAILED_LIMIT: u32 = 5;
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
/// The set-up code for the armed btd (the module docs), and that btd's pid file. The tests' paths name no real btd.
#[cfg(not(test))]
pub(crate) const CODE_FILE: &str = box_common::flags::SETUP_CODE;
#[cfg(test)]
pub(crate) const CODE_FILE: &str = "/tmp/ocbmd_selftest_bt_setup_code";
#[cfg(not(test))]
const BTD_ARMED: &str = box_common::flags::BTD_ARMED;
#[cfg(test)]
const BTD_ARMED: &str = "/tmp/ocbmd_selftest_no_such_btd_armed";
/// [`FAILED_LIMIT`] reached: no set-up network again before the next power-up (tmpfs), whatever ocbmd's restarts.
#[cfg(not(test))]
pub(crate) const CLOSED_FOR_BOOT: &str = "/tmp/ocbmd_setup_closed";
#[cfg(test)]
pub(crate) const CLOSED_FOR_BOOT: &str = "/tmp/ocbmd_selftest_setup_closed";

/// A set-up network is up, or on its way up, by radio_hal.sh's files.
pub(crate) fn ap_up() -> bool {
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
    /// The 10 minutes are over with a set-up session in progress: no new client, the set-up network up until that
    /// session ends (within its own [`TO_LINK`]).
    Finishing,
    /// Done for this start (a set-up, a host, the window, the abuse limit, or never wanted).
    Over,
}

/// The set-up window's state in the daemon.
pub struct Setup {
    pub phase: Phase,
    pub listener: Option<crate::wifi::Listener>,
    /// The listener takes the next client only while this is false (one at a time).
    busy: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Each authenticated exchange's points, by link id: its set-up code's input.
    pub transcripts: Arc<Mutex<Vec<(u64, Transcript)>>>,
    /// A set-up link's MGMT_WIFI_LINK was stored: the window ends once it is applied.
    pub linked: bool,
    /// The set-up link whose code is on the adapter's Bluetooth now; None: the normal name.
    pub code_for: Option<u64>,
    /// This window's set-up sessions that showed a code and ended with nothing set up ([`FAILED_LIMIT`]).
    pub failed: u32,
}

impl Setup {
    /// Never offered (the link is set up, or the start is not one that may offer it).
    pub fn never() -> Setup {
        Setup {
            phase: Phase::Over,
            listener: None,
            busy: Arc::default(),
            stop: Arc::default(),
            transcripts: Arc::default(),
            linked: false,
            code_for: None,
            failed: 0,
        }
    }

    pub fn waiting(now: Instant) -> Setup {
        Setup { phase: Phase::Waiting(now), ..Setup::never() }
    }

    /// Taking clients: the set-up network coming up or up, within its 10 minutes.
    pub fn active(&self) -> bool {
        matches!(self.phase, Phase::Raising(_) | Phase::Offered(_))
    }

    /// The set-up network is up or on its way (taking clients, or a last session finishing).
    pub fn up(&self) -> bool {
        self.active() || self.phase == Phase::Finishing
    }

    /// A set-up link has ended: the listener may take the next client.
    pub fn link_ended(&self) {
        self.busy.store(false, Ordering::Release);
    }

    /// The kept transcript of link `id`, if any.
    pub fn transcript(&self, id: u64) -> Option<Transcript> {
        self.transcripts.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(i, _)| *i == id).map(|(_, t)| t.clone())
    }

    /// Set-up link `id`'s first record opened (it holds the exchange's keys): its code on the adapter's Bluetooth,
    /// replacing any other ([`bt_code`] logs it, never the code's inputs). False with no transcript kept for it.
    pub fn code_on(&mut self, id: u64) -> bool {
        let Some(t) = self.transcript(id) else { return false };
        self.code_for = Some(id);
        bt_code(Some(&ocbm_link::setup_code(&t)));
        true
    }

    /// The code off the adapter's Bluetooth, if it is on: the normal name and policy back.
    pub fn code_off(&mut self) {
        if self.code_for.take().is_some() {
            bt_code(None);
        }
    }

    /// One step of the window: offer it when due, start the listener once the AP is up, end it at its time.
    /// `quiet`: no host has spoken and no session holds the box. `in_session`: a set-up link is open, which may finish
    /// after the 10 minutes. Returns a line for the log when the phase changed.
    pub fn tick(&mut self, now: Instant, quiet: bool, in_session: bool) -> Option<String> {
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
                // "ok <the AP's interface>": the listener takes connections from that interface only.
                if let Some(m) = std::fs::read_to_string(AP_MARK).ok().filter(|m| m.starts_with("ok")) {
                    let iface = m[2..].trim();
                    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 43, 1), SETUP_PORT));
                    match self.start_listener(addr, Some(iface).filter(|i| !i.is_empty())) {
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
            Phase::Offered(until) if now >= until && in_session => {
                self.stop.store(true, Ordering::Release);
                self.listener = None;
                self.phase = Phase::Finishing;
                Some("the window is over: no new client; up for the session in progress".into())
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

    /// The listener: one client at a time, the exchange within [`HANDSHAKE_LIMIT`], from `iface` only when named.
    pub fn start_listener(&mut self, addr: SocketAddr, iface: Option<&str>) -> std::io::Result<()> {
        let l = TcpListener::bind(addr)?;
        if let Some(i) = iface {
            crate::wifi::only_on(&l, i)?;
        }
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
/// lines, and `setup_ap_on` leaves [`AP_MARK`] when the AP is up. Mode `ap`'s AP verbs too (wifi::ApKeep).
pub(crate) fn hal(verb: &str) {
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

/// The set-up code on the adapter's Bluetooth (`Some`: its four digits), or the normal name and policy back (`None`):
/// the file the armed btd reads, then its wake (the module docs). Logged here, never the digits or their inputs.
pub fn bt_code(code: Option<&str>) {
    let on = if code.is_some() { "on" } else { "off" };
    if let Err(e) = box_common::flags::put_setup_code(CODE_FILE, code) {
        eprintln!("[ocbmd] setup: set-up code {on}: {e}");
        return;
    }
    match armed_btd(BTD_ARMED, "/proc") {
        Some(pid) => {
            // SAFETY: a signal to the armed btd just identified, whose wake pipe takes SIGUSR1 (armed.rs).
            unsafe { libc::kill(pid, libc::SIGUSR1) };
            eprintln!("[ocbmd] setup: set-up code {on} Bluetooth (armed btd {pid})");
        }
        None => eprintln!("[ocbmd] setup: set-up code {on}: no armed btd yet"),
    }
}

/// The armed btd's pid, from its pid file `armed`, when `proc_root`/<pid>/comm names btd. Only an armed btd writes the
/// file, and only it takes SIGUSR1 (a btd started at a host's arrival has no handler, and the signal's default would end
/// it); a pid reused by another program is not woken. A zombie's signal goes nowhere.
fn armed_btd(armed: &str, proc_root: &str) -> Option<libc::pid_t> {
    let pid = std::fs::read_to_string(armed).ok()?.trim().parse::<libc::pid_t>().ok().filter(|p| *p > 1)?;
    (std::fs::read_to_string(format!("{proc_root}/{pid}/comm")).ok()?.trim() == "btd").then_some(pid)
}

/// A set-up code an earlier ocbmd left up (it exited or crashed with its session open) comes off at this one's start.
pub fn code_orphan_off() {
    if std::path::Path::new(CODE_FILE).exists() {
        bt_code(None);
    }
}

/// [`FAILED_LIMIT`] reached: no set-up network before the next power-up ([`wanted`]).
pub fn closed_for_boot() {
    let _ = std::fs::write(CLOSED_FOR_BOOT, b"1\n");
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
    if std::path::Path::new(CLOSED_FOR_BOOT).exists() {
        eprintln!("[ocbmd] setup: no set-up network until the next power-up (the abuse limit)");
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

/// Serializes the tests that write the set-up code's file and the abuse limit's (one path each for the process).
#[cfg(test)]
pub(crate) fn code_file_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code's file as the armed btd reads it (box_common::flags::setup_code): four digits while it is on, no file
    /// when it is off. No armed btd here (the tests' pid file names none), so nothing is signalled.
    #[test]
    fn the_set_up_code_is_put_in_its_file_and_taken_out() {
        let _c = code_file_lock();
        assert_eq!(armed_btd(BTD_ARMED, "/proc"), None, "the tests wake no process");
        bt_code(Some("2092"));
        let text = std::fs::read_to_string(CODE_FILE).unwrap();
        assert_eq!(box_common::flags::setup_code(&text), Some("2092"));
        bt_code(None);
        assert!(!std::path::Path::new(CODE_FILE).exists());
        let mut s = Setup::waiting(Instant::now());
        assert!(!s.code_on(7) && s.code_for.is_none(), "no transcript kept: no code");
        let (_, host_pub) = ocbm_link::setup_keypair_from(&[0x11; 32]).unwrap();
        let (_, box_pub) = ocbm_link::setup_keypair_from(&[0x22; 32]).unwrap();
        s.transcripts.lock().unwrap().push((7, Transcript { host_pub, box_pub }));
        assert!(s.code_on(7) && s.code_for == Some(7));
        assert_eq!(std::fs::read_to_string(CODE_FILE).unwrap().trim(), "2092", "the note's vector");
        s.code_off();
        assert!(s.code_for.is_none() && !std::path::Path::new(CODE_FILE).exists());
        std::fs::write(CODE_FILE, b"1234\n").unwrap();
        s.code_off();
        assert!(std::path::Path::new(CODE_FILE).exists(), "off only what this window put on");
        code_orphan_off();
        assert!(!std::path::Path::new(CODE_FILE).exists(), "an earlier ocbmd's code comes off at the start");
    }

    /// Only a btd named by the pid file is woken: not another program in a reused pid, init, or no file.
    #[test]
    fn only_the_armed_btd_is_woken() {
        let root = format!("/tmp/ocbmd_selftest_proc_{}", std::process::id());
        let _ = std::fs::remove_dir_all(&root);
        let comm = |pid: u32, text: &str| {
            std::fs::create_dir_all(format!("{root}/{pid}")).unwrap();
            std::fs::write(format!("{root}/{pid}/comm"), text).unwrap();
        };
        comm(300, "btd\n");
        comm(302, "sh\n");
        comm(303, "btdx\n");
        let armed = format!("{root}/armed");
        let pid_in = |p: &str| {
            std::fs::write(&armed, p).unwrap();
            armed_btd(&armed, &root)
        };
        assert_eq!(pid_in("300\n"), Some(300));
        assert_eq!(pid_in("302\n"), None, "another program");
        assert_eq!(pid_in("303\n"), None, "another name");
        assert_eq!(pid_in("304\n"), None, "no such process");
        assert_eq!(pid_in("1\n"), None);
        assert_eq!(pid_in("x\n"), None);
        let _ = std::fs::remove_file(&armed);
        assert_eq!(armed_btd(&armed, &root), None, "no pid file: no armed btd");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_window_waits_then_gives_up_when_a_host_spoke_or_the_ap_never_came() {
        let t0 = Instant::now();
        let mut s = Setup::waiting(t0);
        assert_eq!(s.tick(t0 + OFFER_AFTER - Duration::from_secs(1), true, false), None);
        assert!(s.tick(t0 + OFFER_AFTER, false, false).is_some());
        assert_eq!(s.phase, Phase::Over, "a host spoke: never offered");
        let mut s = Setup::waiting(t0);
        let _ = std::fs::remove_file(AP_MARK);
        assert!(s.tick(t0 + OFFER_AFTER, true, false).is_some());
        assert!(matches!(s.phase, Phase::Raising(_)));
        assert!(s.tick(t0 + OFFER_AFTER + RAISE_LIMIT, true, false).unwrap().contains("did not come up"));
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
        s.start_listener("127.0.0.1:0".parse().unwrap(), None).unwrap();
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
