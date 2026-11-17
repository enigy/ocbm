//! The one phone this carplayd serves — the phone `btd` paged or accepted on Bluetooth.
//!
//! carplayd was written for one phone on the AP, and it shows in two places: the discovery loop dials
//! every `_carplay-ctrl` it resolves, and the accept thread hands `:5000` to the newest control
//! connection (`_HijackHTTPServerConnections`). The two-iPhone bench of 2026-11-01 (installer
//! `6affefc`, box `3d492a2`) showed what that does to a phone switch (`MGMT_SWITCH_DEVICE`): the phone
//! the switch dropped stays on the AP, the new carplayd dials it first, the requested phone's
//! connection then hijacks it, and the two take the session from each other until neither projects.
//!
//! So `btd` names the phone it is serving in `/tmp/carplay_expect_phone` (its `av::EXPECT_PHONE_FILE`), and passes
//! that path to the carplayd it spawns ([`EXPECT_FILE_ENV`]). A FILE, not the address in the environment: carplayd
//! outlives a phone's Bluetooth link (since A2 it starts with btd's host session, before any phone is named), and the
//! next phone must not be refused by a carplayd that was told about the last one. One line: the phone's BR/EDR
//! address, then ` wifi` once btd has sent that phone its 0x5703 (its Wi-Fi details) in this naming. btd names a
//! bonded phone when its Bluetooth session starts, while no CarPlay session is up (no mark yet; the same phone back
//! loses the mark of its earlier session unless its own CarPlay session is up or connecting, A2-2), and again at its
//! 0x5703 (with the mark); it puts the earlier name back, mark and all, if that session ends without a 0x5703 (the
//! Mac's 15 Nov bench: a phone that knows the AP was on it before its 0x5703, and was refused). The file is re-read at
//! each decision, and discovery is woken when it changes (inotify, `watch.rs`). A new name forgets the refusals decided
//! for the old one; the old phone's live session stops holding, and holds again if the name comes back to it.
//!
//! With the gate armed (the wireless spawn):
//! - no phone named (no file): carplayd serves and dials nobody ([`Admit::Unnamed`], [`Dial::Unnamed`]);
//! - discovery dials only the phone whose `_carplay-ctrl` TXT `id` is the named one (fix 1b); a TXT without an id is
//!   dialled as before; never while that phone's session is live here ([`Dial::Live`]); and only once btd has sent
//!   that phone its 0x5703 ([`Dial::Waiting`] until the mark), unless the bench lever [`BENCH_DIAL_EARLY`] is set;
//! - a phase-1 SETUP whose `deviceID` is another phone is refused (HTTP 500, connection closed), before
//!   the session publishes who the phone is, and that peer address is refused for the rest of this
//!   name: its connections are dropped and discovery no longer dials it;
//! - while the named phone's session is up, a control connection from another address is dropped
//!   rather than preempting it; one from the named phone's own address still preempts, as before.
//!
//! No path in the environment (the wired supervisor's carplayd): no gate, everything as before. A SETUP without a
//! `deviceID` is let through while a phone is named: it cannot be told apart.

use std::collections::HashSet;
use std::net::{IpAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use receiver::session::{phase1_device_id, AtomicU64, SessionDelegate, SetupError};

/// The environment variable `btd` sets to the file that names the phone; absent = no gate.
pub const EXPECT_FILE_ENV: &str = "CARPLAY_EXPECT_PHONE_FILE";

/// The name file's mark: btd sent the named phone its 0x5703 (`av::WIFI_MARK` in btd).
const WIFI_MARK: &str = "wifi";

/// Bench lever (docs/ops/02_TESTING.md, "Bench hooks"): dial the named phone at its first resolve, before btd has sent
/// it its 0x5703, for one experiment (does the phone accept it?). Set unless absent or "off" (`box_common::bench_flag`);
/// nothing in a release writes it, and a reboot clears it.
pub const BENCH_DIAL_EARLY: &str = "/tmp/bench_dial_early";

/// What the accept thread does with a new control connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// Serve it, preempting the current session as before.
    Accept,
    /// Drop it: its phone was refused at SETUP.
    Refused,
    /// Drop it: the named phone's session (from this address) holds, and a newcomer from another
    /// address does not preempt it.
    Holding(IpAddr),
    /// Drop it: btd has named no phone yet.
    Unnamed,
}

/// What a phase-1 SETUP may do ([`PhoneGate::on_setup`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// Go on.
    Allow,
    /// Refused: it names another phone than btd's, and its address is refused for this name.
    Elsewhere,
    /// Refused: btd has named no phone.
    Unnamed,
}

/// What discovery does with a phone's `_carplay-ctrl` record ([`PhoneGate::dial`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dial {
    /// Dial it.
    Now,
    /// Dial it: the named phone before its 0x5703, dialled by the bench lever ([`BENCH_DIAL_EARLY`]).
    Early,
    /// The named phone, its session already live here (bench 2026-11-02): never dialled again.
    Live,
    /// Another phone than btd names (fix 1b): held for when btd names it.
    Elsewhere,
    /// No phone named yet: held for when one is.
    Unnamed,
    /// The named phone, before btd has sent it its 0x5703: held until it has.
    Waiting,
}

impl Dial {
    /// Whether it is dialled now.
    pub fn now(self) -> bool {
        matches!(self, Dial::Now | Dial::Early)
    }
}

/// The name file as read: the named phone, 12 lower-case hex digits, and whether btd sent it its 0x5703.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub phone: String,
    pub wifi: bool,
}

impl Named {
    /// The file's text read; `None` when it names no phone.
    fn parse(s: &str) -> Option<Named> {
        let mut words = s.split_whitespace();
        let phone = normalize(words.next()?)?;
        Some(Named { phone, wifi: words.any(|w| w == WIFI_MARK) })
    }
}

#[derive(Default)]
struct State {
    /// The named phone and its mark as last read; `None` = none.
    expected: Option<Named>,
    /// Moved on each time the named phone changes (to another, or to none) and each time the same phone loses its mark
    /// (named afresh, A2-2), whoever read it: so discovery can tell a dial made under this naming from one made under
    /// an earlier naming of the same phone (carplayd-3), and a naming it has not acted on from the one it has, even when
    /// the file reads the same ([`PhoneGate::naming`]).
    epoch: u64,
    refused: HashSet<IpAddr>,
    /// The session whose SETUP named the expected phone: its generation, peer address and phone. Kept across a
    /// name change until the session ends: it holds only while its phone is the one named, and is never dialled
    /// again, so a name put back to that phone finds its session as it was (the Mac's 15 Nov bench, R4).
    serving: Option<(u64, IpAddr, String)>,
    /// The control connections being served or set up now, by generation (carplayd-1's retries wait for none).
    open: HashSet<u64>,
}

pub struct PhoneGate {
    file: Option<PathBuf>,
    bench: PathBuf,
    state: Mutex<State>,
}

impl PhoneGate {
    /// The gate as `btd` set it up: on when [`EXPECT_FILE_ENV`] names a file.
    pub fn from_env() -> Self {
        Self::new(std::env::var_os(EXPECT_FILE_ENV).map(PathBuf::from))
    }

    pub fn new(file: Option<PathBuf>) -> Self {
        Self::with_bench(file, PathBuf::from(BENCH_DIAL_EARLY))
    }

    /// With the bench lever at `bench` (a test's own).
    pub fn with_bench(file: Option<PathBuf>, bench: PathBuf) -> Self {
        Self { file, bench, state: Mutex::new(State::default()) }
    }

    /// Whether this carplayd gates at all: only the wireless spawn is handed the file's path.
    pub fn armed(&self) -> bool {
        self.file.is_some()
    }

    /// The name file, for the watch that wakes discovery when it changes.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The name file now, read afresh; another phone than last time (or none) forgets every refusal decided for the
    /// old one and moves the epoch on, and so does the same phone named afresh: its mark gone, as btd names a phone
    /// back on Bluetooth without the mark of its earlier session (A2-2). The live session is kept (`State::serving`).
    fn expected_now(&self, st: &mut State) -> Option<Named> {
        let now = self.file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| Named::parse(&s));
        if now.as_ref().map(|n| &n.phone) != st.expected.as_ref().map(|n| &n.phone) {
            if !st.refused.is_empty() {
                eprintln!("[carplayd] phone gate: the phone btd serves changed; earlier refusals forgotten");
            }
            st.refused.clear();
            st.epoch += 1;
        } else if st.expected.as_ref().is_some_and(|n| n.wifi) && now.as_ref().is_some_and(|n| !n.wifi) {
            st.epoch += 1;
        }
        st.expected = now.clone();
        now
    }

    /// The phone btd names now (12 lower-case hex digits), read afresh; `None` = none. (Discovery reads
    /// [`PhoneGate::naming`] at each wake, for the mark and the epoch too.)
    #[cfg(test)]
    pub fn named(&self) -> Option<String> {
        self.naming().0.map(|n| n.phone)
    }

    /// The phone btd names now and its 0x5703 mark, read afresh, and the naming's epoch, from one read. Discovery acts
    /// on a naming by its epoch: the file can read as it did at discovery's last look after another naming in between
    /// that the gate read (the same phone named afresh and its mark back, another phone named and the first put back),
    /// and that is a new naming all the same (A2-1, the second review).
    pub fn naming(&self) -> (Option<Named>, u64) {
        let mut st = self.lock();
        let now = self.expected_now(&mut st);
        (now, st.epoch)
    }

    /// The naming's epoch as of the last read ([`State::epoch`]).
    pub fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    /// A new control connection from `peer`, before it may preempt anything.
    pub fn on_connection(&self, peer: IpAddr) -> Admit {
        if !self.armed() {
            return Admit::Accept;
        }
        let mut st = self.lock();
        let Some(expected) = self.expected_now(&mut st) else {
            return Admit::Unnamed;
        };
        if st.refused.contains(&peer) {
            return Admit::Refused;
        }
        // Only the named phone's session holds: after another phone's 0x5703 that phone takes over, as before.
        match &st.serving {
            Some((_, served, phone)) if *served != peer && expected.phone == *phone => Admit::Holding(*served),
            _ => Admit::Accept,
        }
    }

    /// The accept thread let control connection `generation` through: open until [`PhoneGate::session_ended`].
    pub fn connection_opened(&self, generation: u64) {
        self.lock().open.insert(generation);
    }

    /// Whether a control connection is being served or set up now.
    pub fn connection_open(&self) -> bool {
        !self.lock().open.is_empty()
    }

    /// A phase-1 SETUP on session `generation` from `peer`, naming `device_id`: whether it may go on.
    pub fn on_setup(&self, generation: u64, peer: IpAddr, device_id: Option<&str>) -> Setup {
        if !self.armed() {
            return Setup::Allow;
        }
        let mut st = self.lock();
        let Some(expected) = self.expected_now(&mut st) else {
            return Setup::Unnamed;
        };
        match device_id.and_then(normalize) {
            None => {
                eprintln!("[carplayd] phone gate: SETUP from {peer} names no phone; letting it through");
                Setup::Allow
            }
            Some(id) if id == expected.phone => {
                st.serving = Some((generation, peer, id));
                Setup::Allow
            }
            Some(_) => {
                st.refused.insert(peer);
                Setup::Elsewhere
            }
        }
    }

    /// Session `generation` ended (or was superseded before it was served): if it was the named phone's, a newcomer may
    /// preempt again.
    pub fn session_ended(&self, generation: u64) {
        let mut st = self.lock();
        st.open.remove(&generation);
        if st.serving.as_ref().map(|(g, _, _)| *g) == Some(generation) {
            st.serving = None;
        }
    }

    /// Whether discovery should leave `ip` alone: its phone was refused at SETUP under the current name.
    pub fn is_refused(&self, ip: IpAddr) -> bool {
        let mut st = self.lock();
        self.expected_now(&mut st).is_some() && st.refused.contains(&ip)
    }

    /// What discovery does with a `_carplay-ctrl` whose TXT `id` is `txt_id` ([`Dial`]), and the naming's epoch, from one
    /// read of the name file. The phone puts its BR/EDR address in its TXT `id` (bench 2026-11-01: lower case with
    /// colons), the same address btd names, so another phone is left alone before anything is dialled (fix 1b). No gate:
    /// dial, as before.
    pub fn dial(&self, txt_id: Option<&str>) -> (Dial, u64) {
        if !self.armed() {
            return (Dial::Now, 0);
        }
        let mut st = self.lock();
        let Some(expected) = self.expected_now(&mut st) else {
            return (Dial::Unnamed, st.epoch);
        };
        let id = txt_id.and_then(normalize);
        let verdict = match &id {
            Some(id) if *id != expected.phone => Dial::Elsewhere,
            Some(id) if st.serving.as_ref().is_some_and(|(_, _, phone)| phone == id) => Dial::Live,
            _ if expected.wifi => Dial::Now,
            _ if box_common::bench_flag(&self.bench.to_string_lossy()) => Dial::Early,
            _ => Dial::Waiting,
        };
        (verdict, st.epoch)
    }

    /// Whether discovery may dial a phone whose `_carplay-ctrl` TXT `id` is `txt_id` at all, whatever its 0x5703: the
    /// named phone's (or a TXT without a readable id) while one is named. No gate: yes. ([`PhoneGate::dial`] decides;
    /// this is its first half, for the tests.)
    #[cfg(test)]
    pub fn wants(&self, txt_id: Option<&str>) -> bool {
        !matches!(self.dial(txt_id).0, Dial::Elsewhere | Dial::Unnamed)
    }

    /// Whether the phone whose TXT `id` is `txt_id` already has its session here: its SETUP was let through
    /// as the named phone's, and that session has not ended. Discovery leaves it alone then. On the
    /// 2026-11-02 bench a live phone's record gained two addresses mid-session; discovery dialled one of
    /// them, the iPhone answered 200 OK, and the session that was showing video ended 29 s later. It holds
    /// across a name change, so a name put back to that phone (R4) does not dial it again. ([`PhoneGate::dial`] gives
    /// it as [`Dial::Live`]; this is for the tests.)
    #[cfg(test)]
    pub fn serving(&self, txt_id: Option<&str>) -> bool {
        let mut st = self.lock();
        match (self.expected_now(&mut st), txt_id.and_then(normalize)) {
            (Some(_), Some(id)) => st.serving.as_ref().is_some_and(|(_, _, phone)| *phone == id),
            _ => false,
        }
    }
}

/// A BR/EDR address in any usual spelling (`AA:BB:..`, `aa-bb-..`, bare hex) as 12 lower-case hex
/// digits; `None` for anything else.
pub(crate) fn normalize(s: &str) -> Option<String> {
    let hex: String = s.trim().chars().filter(|c| !matches!(c, ':' | '-')).collect();
    (hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
}

/// The session delegate behind the gate: a phase-1 SETUP from another phone, or with no phone named, is refused before
/// the inner delegate sees it (so it never publishes that phone's identity), and its connection is closed.
pub struct GatedSession {
    inner: Box<dyn SessionDelegate>,
    gate: Arc<PhoneGate>,
    generation: u64,
    peer: IpAddr,
    stream: Option<TcpStream>,
}

impl GatedSession {
    pub fn new(
        inner: Box<dyn SessionDelegate>,
        gate: Arc<PhoneGate>,
        generation: u64,
        peer: IpAddr,
        stream: Option<TcpStream>,
    ) -> Self {
        Self { inner, gate, generation, peer, stream }
    }
}

impl SessionDelegate for GatedSession {
    fn on_paired(&mut self, shared_secret: [u8; 32]) {
        self.inner.on_paired(shared_secret)
    }

    fn setup(&mut self, request_plist: &[u8]) -> Result<Vec<u8>, SetupError> {
        if let Some(id) = phase1_device_id(request_plist) {
            let refused = match self.gate.on_setup(self.generation, self.peer, id.as_deref()) {
                Setup::Allow => None,
                Setup::Elsewhere => Some(format!(
                    "names another phone than btd serves; refused, and {} is ignored until btd serves another phone",
                    self.peer
                )),
                Setup::Unnamed => Some("came before btd named any phone; refused".to_string()),
            };
            if let Some(why) = refused {
                eprintln!("[carplayd] phone gate: SETUP from {} {why}", self.peer);
                if let Some(s) = &self.stream {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
                return Err(SetupError);
            }
        }
        self.inner.setup(request_plist)
    }

    fn record(&mut self) -> Vec<u8> {
        self.inner.record()
    }

    fn command(&mut self, request_plist: &[u8]) -> Vec<u8> {
        self.inner.command(request_plist)
    }

    fn stream_message(&mut self, stream: i64, body: &[u8]) -> Option<Vec<u8>> {
        self.inner.stream_message(stream, body)
    }

    fn teardown(&mut self, request_plist: &[u8]) {
        self.inner.teardown(request_plist)
    }

    fn last_activity(&self) -> Option<Arc<AtomicU64>> {
        self.inner.last_activity()
    }

    fn set_resolution(&mut self, w: i64, h: i64, fps: i64) {
        self.inner.set_resolution(w, h, fps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "02:00:00:00:00:0A";
    const OTHER: &str = "AA:BB:CC:00:6A:DE";

    fn scratch(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "carplayd_{tag}_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    /// A gate whose file names `phone` with the 0x5703 mark (as btd writes it at a handoff), and a bench lever of its
    /// own, off.
    fn gate_naming(phone: Option<&str>) -> (PhoneGate, PathBuf) {
        let path = scratch("expect");
        match phone {
            Some(p) => std::fs::write(&path, format!("{p} wifi")).unwrap(),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        (PhoneGate::with_bench(Some(path.clone()), scratch("bench")), path)
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 168, 43, last])
    }

    #[test]
    fn the_phone_the_switch_dropped_is_refused_at_setup_and_stays_out() {
        // 2026-11-01 bench: the dropped phone (.100) was dialled first, then the requested one joined (.101).
        let (gate, path) = gate_naming(Some(OWNER));
        assert_eq!(gate.on_connection(ip(100)), Admit::Accept, "nothing known about .100 yet");
        assert_eq!(gate.on_setup(1, ip(100), Some(OTHER)), Setup::Elsewhere, "another phone's SETUP is refused");
        assert!(gate.is_refused(ip(100)), "discovery leaves it alone");
        assert_eq!(gate.on_connection(ip(100)), Admit::Refused, "and its reconnects are dropped");
        assert_eq!(gate.on_connection(ip(101)), Admit::Accept);
        assert_eq!(gate.on_setup(2, ip(101), Some("02-00-00-00-00-0a")), Setup::Allow, "the named phone, in any spelling");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_named_phones_session_is_not_preempted_from_another_address() {
        let (gate, path) = gate_naming(Some(OWNER));
        assert_eq!(gate.on_setup(7, ip(101), Some(OWNER)), Setup::Allow);
        assert_eq!(gate.on_connection(ip(102)), Admit::Holding(ip(101)), "a newcomer elsewhere does not hijack it");
        assert_eq!(gate.on_connection(ip(101)), Admit::Accept, "its own reconnect still preempts, as before");
        gate.session_ended(6);
        assert_eq!(gate.on_connection(ip(102)), Admit::Holding(ip(101)), "only its own session's end counts");
        gate.session_ended(7);
        assert_eq!(gate.on_connection(ip(102)), Admit::Accept);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_new_name_forgets_what_was_decided_for_the_old_one() {
        // One btd session, carplayd latched: the owner leaves, the other phone hands off next.
        let (gate, path) = gate_naming(Some(OWNER));
        assert_eq!(gate.on_setup(1, ip(100), Some(OTHER)), Setup::Elsewhere);
        std::fs::write(&path, format!("{OTHER} wifi")).unwrap();
        assert!(!gate.is_refused(ip(100)));
        assert_eq!(gate.on_connection(ip(100)), Admit::Accept);
        assert_eq!(gate.on_setup(2, ip(100), Some(OTHER)), Setup::Allow);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn before_any_phone_is_named_an_armed_carplayd_serves_and_dials_nobody() {
        // A2: carplayd starts with btd's host session, before a phone's Bluetooth session names it.
        let (gate, path) = gate_naming(None);
        assert!(gate.armed(), "the wireless spawn, before btd has named a phone");
        assert_eq!(gate.on_connection(ip(101)), Admit::Unnamed, "a phone that comes by itself is dropped");
        assert_eq!(gate.on_setup(1, ip(100), Some(OTHER)), Setup::Unnamed);
        assert_eq!(gate.on_setup(1, ip(100), None), Setup::Unnamed, "even one that names no phone");
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Unnamed);
        assert_eq!(gate.dial(None).0, Dial::Unnamed, "nor a TXT without an id");
        assert!(!gate.wants(Some(OWNER)));
        assert!(!gate.is_refused(ip(100)), "nothing refused for later");
        std::fs::write(&path, OWNER).unwrap();
        assert_eq!(gate.on_connection(ip(101)), Admit::Accept, "named: its own connection is served");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_wired_carplayd_has_no_gate() {
        let wired = PhoneGate::new(None);
        assert!(!wired.armed());
        assert_eq!(wired.on_setup(1, ip(100), Some(OTHER)), Setup::Allow);
        assert_eq!(wired.on_connection(ip(101)), Admit::Accept);
        assert!(!wired.is_refused(ip(100)));
        assert_eq!(wired.dial(Some(OTHER)).0, Dial::Now, "dials as before");
        assert!(!wired.serving(Some(OWNER)), "no gate: as before");
    }

    #[test]
    fn the_dial_waits_for_the_named_phones_0x5703() {
        // A2's ordering: the name comes as its Bluetooth session starts, the mark once btd has sent its 0x5703.
        let (gate, path) = gate_naming(None);
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Unnamed, "not named");
        std::fs::write(&path, OWNER).unwrap();
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Waiting, "named, no 0x5703 yet: held");
        assert_eq!(gate.dial(None).0, Dial::Waiting, "a TXT without an id waits with it");
        assert_eq!(gate.dial(Some(OTHER)).0, Dial::Elsewhere, "another phone, whatever the mark");
        assert!(gate.wants(Some(OWNER)), "it is the phone to dial, once its 0x5703 is sent");
        std::fs::write(&path, format!("{OWNER} wifi")).unwrap();
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Now, "named and its 0x5703 sent: dial");
        assert_eq!(gate.dial(None).0, Dial::Now);
        assert_eq!(gate.dial(Some(OTHER)).0, Dial::Elsewhere);
        assert_eq!(gate.on_setup(3, ip(101), Some(OWNER)), Setup::Allow);
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Live, "its session is live: never again");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_bench_lever_dials_the_named_phone_before_its_0x5703_only() {
        let (gate, path) = gate_naming(None);
        let lever = gate.bench.clone();
        std::fs::write(&lever, "").unwrap();
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Unnamed, "the lever names nobody");
        std::fs::write(&path, OWNER).unwrap();
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Early, "named, no 0x5703: dialled under the lever");
        assert!(gate.dial(Some(OWNER)).0.now());
        assert_eq!(gate.dial(Some(OTHER)).0, Dial::Elsewhere, "never another phone");
        std::fs::write(&lever, "off\n").unwrap();
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Waiting, "\"off\" is off");
        std::fs::write(&lever, "on").unwrap();
        std::fs::write(&path, format!("{OWNER} wifi")).unwrap();
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Now, "with the mark it is no early dial");
        let _ = std::fs::remove_file(&lever);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_rename_mid_wait_moves_the_epoch_and_the_mark_goes_with_the_naming() {
        let (gate, path) = gate_naming(None);
        std::fs::write(&path, OWNER).unwrap();
        let (_, e1) = gate.dial(Some(OWNER));
        std::fs::write(&path, format!("{OWNER} wifi")).unwrap();
        let (v, e2) = gate.dial(Some(OWNER));
        assert_eq!((v, e2), (Dial::Now, e1), "the mark arriving is the same naming");
        std::fs::write(&path, OTHER).unwrap();
        let (v, e3) = gate.dial(Some(OTHER));
        assert_eq!(v, Dial::Waiting, "another phone named while the first waited: it waits for its own 0x5703");
        assert!(e3 > e2, "a new naming");
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Elsewhere, "and the first is held");
        std::fs::write(&path, format!("{OWNER} wifi")).unwrap();
        let (v, e4) = gate.dial(Some(OWNER));
        assert_eq!(v, Dial::Now, "put back with its mark (R4)");
        assert!(e4 > e3);
        std::fs::write(&path, OWNER).unwrap();
        let (v, e5) = gate.dial(Some(OWNER));
        assert_eq!(v, Dial::Waiting, "A2-2: the same phone back on Bluetooth, named afresh without its mark");
        assert!(e5 > e4, "a new naming of the same phone");
        std::fs::write(&path, format!("{OWNER} wifi")).unwrap();
        let (v, e6) = gate.dial(Some(OWNER));
        assert_eq!((v, e6), (Dial::Now, e5), "its new 0x5703: the same naming");
        let _ = std::fs::remove_file(&path);
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Unnamed, "the file removed: nobody");
        assert!(gate.epoch() > e6);
    }

    #[test]
    fn discovery_dials_only_the_named_phone_by_its_txt_id() {
        // 2026-11-01 bench: "_carplay-ctrl TXT: id=<its address, lower case> srcvers=1005.13.1".
        let (gate, path) = gate_naming(Some(OWNER));
        assert!(gate.wants(Some("02:00:00:00:00:0a")), "the TXT spells it in lower case");
        assert!(!gate.wants(Some(OTHER)), "another phone is never dialled");
        assert!(gate.wants(None), "no id: dial, and the SETUP check decides");
        assert!(gate.wants(Some("not a mac")));
        std::fs::write(&path, format!("{OTHER} wifi")).unwrap();
        assert!(gate.wants(Some(OTHER)), "follows the phone btd serves now");
        let _ = std::fs::remove_file(path);
        assert!(!gate.wants(Some(OWNER)), "no name: nobody");
    }

    #[test]
    fn discovery_does_not_dial_the_named_phone_while_its_session_is_live() {
        // 2026-11-02 bench: the record changed to [fe80::.., .100, .101] mid-session and .100 was dialled.
        let (gate, path) = gate_naming(Some(OWNER));
        assert!(!gate.serving(Some(OWNER)), "no session yet: dial it");
        assert_eq!(gate.on_setup(3, ip(101), Some(OWNER)), Setup::Allow);
        assert!(gate.serving(Some("02:00:00:00:00:0a")), "its session is live: leave it alone");
        assert!(!gate.serving(Some(OTHER)), "says nothing about another phone");
        assert!(!gate.serving(None), "nor about a TXT without an id");
        gate.session_ended(3);
        assert!(!gate.serving(Some(OWNER)), "once the session ends it may be dialled again");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_live_session_is_kept_across_a_name_and_its_put_back() {
        // The Mac's 15 Nov bench, R4: btd names another phone, then puts the earlier name back.
        let (gate, path) = gate_naming(Some(OWNER));
        assert_eq!(gate.on_setup(4, ip(101), Some(OWNER)), Setup::Allow);
        std::fs::write(&path, OTHER).unwrap();
        assert!(!gate.serving(Some(OTHER)), "the other phone has no session here");
        assert!(gate.serving(Some(OWNER)), "the owner's session is still live: never dialled again");
        assert_eq!(gate.on_connection(ip(102)), Admit::Accept, "with another phone named it does not hold, as before");
        std::fs::write(&path, format!("{OWNER} wifi")).unwrap();
        assert_eq!(gate.named().as_deref(), Some("02000000000a"));
        assert!(gate.serving(Some(OWNER)), "named again: still not dialled again");
        assert_eq!(gate.dial(Some(OWNER)).0, Dial::Live);
        assert_eq!(gate.on_connection(ip(102)), Admit::Holding(ip(101)), "and it holds again");
        gate.session_ended(4);
        assert!(!gate.serving(Some(OWNER)));
        let _ = std::fs::remove_file(path);
        assert_eq!(gate.named(), None, "no file, no phone");
    }

    #[test]
    fn a_setup_naming_no_phone_is_let_through_while_one_is_named() {
        let (gate, path) = gate_naming(Some(OWNER));
        assert_eq!(gate.on_setup(1, ip(100), None), Setup::Allow);
        assert_eq!(gate.on_setup(1, ip(100), Some("not a mac")), Setup::Allow);
        assert_eq!(gate.on_connection(ip(101)), Admit::Accept, "not marked as the named phone's session");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_connections_are_counted_until_they_end() {
        let (gate, path) = gate_naming(Some(OWNER));
        assert!(!gate.connection_open());
        gate.connection_opened(5);
        gate.connection_opened(6);
        gate.session_ended(5);
        assert!(gate.connection_open(), "6 is still open");
        gate.session_ended(6);
        assert!(!gate.connection_open());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_name_file_reads_as_btd_writes_it() {
        assert_eq!(Named::parse("02:00:00:00:00:0A"), Some(Named { phone: "02000000000a".into(), wifi: false }));
        assert_eq!(Named::parse("02:00:00:00:00:0A wifi\n"), Some(Named { phone: "02000000000a".into(), wifi: true }));
        assert_eq!(Named::parse(" 02000000000A  wifi "), Some(Named { phone: "02000000000a".into(), wifi: true }));
        assert_eq!(Named::parse("02:00:00:00:00:0A later"), Some(Named { phone: "02000000000a".into(), wifi: false }));
        assert_eq!(Named::parse("wifi"), None);
        assert_eq!(Named::parse(""), None);
    }

    #[test]
    fn addresses_normalize_however_they_are_spelled() {
        assert_eq!(normalize(" 02:00:00:00:00:0A\n").as_deref(), Some("02000000000a"));
        assert_eq!(normalize("02000000000A").as_deref(), Some("02000000000a"));
        assert_eq!(normalize("02:00:00:00:00"), None);
        assert_eq!(normalize("ZZ:00:00:00:00:0A"), None);
    }
}
