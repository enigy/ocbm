//! The one phone this carplayd serves — the phone `btd` paged or accepted on Bluetooth.
//!
//! carplayd was written for one phone on the AP, and it shows in two places: the discovery loop dials
//! every `_carplay-ctrl` it resolves, and the accept thread hands `:5000` to the newest control
//! connection (`_HijackHTTPServerConnections`). The two-iPhone bench of 2026-11-01 (installer
//! `6affefc`, box `3d492a2`) showed what that does to a phone switch (`MGMT_SWITCH_DEVICE`): the phone
//! the switch dropped stays on the AP, the new carplayd dials it first, the requested phone's
//! connection then hijacks it, and the two take the session from each other until neither projects.
//!
//! So `btd` names the phone it is serving: at each Wi-Fi handoff (0x5703) it writes that phone's BR/EDR
//! address to `/tmp/carplay_expect_phone` (its `av::EXPECT_PHONE_FILE`), and passes that path to the
//! carplayd it spawns ([`EXPECT_FILE_ENV`]). A FILE, not the address in the environment: within one
//! `btd` session carplayd outlives a phone's Bluetooth link (the AV layer is latched), and the next phone
//! to hand off must not be refused by a carplayd that was told about the last one. The file is re-read at each decision, and
//! a new name forgets everything decided for the old one.
//!
//! With a phone named:
//! - discovery dials only the phone whose `_carplay-ctrl` TXT `id` is that address ([`PhoneGate::wants`],
//!   fix 1b); a TXT without an id is dialled as before; and not while that phone's session is live here
//!   ([`PhoneGate::serving`]), however its record's addresses change;
//! - a phase-1 SETUP whose `deviceID` is another phone is refused (HTTP 500, connection closed), before
//!   the session publishes who the phone is, and that peer address is refused for the rest of this
//!   name: its connections are dropped and discovery no longer dials it;
//! - while the named phone's session is up, a control connection from another address is dropped
//!   rather than preempting it; one from the named phone's own address still preempts, as before.
//!
//! No file, or no path in the environment (the wired supervisor's carplayd, or an older `btd`): no gate,
//! everything as before. A SETUP without a `deviceID` is let through: it cannot be told apart.

use std::collections::HashSet;
use std::net::{IpAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use receiver::session::{phase1_device_id, AtomicU64, SessionDelegate, SetupError};

/// The environment variable `btd` sets to the file that names the phone; absent = no gate.
pub const EXPECT_FILE_ENV: &str = "CARPLAY_EXPECT_PHONE_FILE";

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
}

#[derive(Default)]
struct State {
    /// The named phone's address, 12 lower-case hex digits; `None` = no gate.
    expected: Option<String>,
    refused: HashSet<IpAddr>,
    /// The session whose SETUP named the expected phone: its generation and peer address.
    serving: Option<(u64, IpAddr)>,
}

pub struct PhoneGate {
    file: Option<PathBuf>,
    state: Mutex<State>,
}

impl PhoneGate {
    /// The gate as `btd` set it up: on when [`EXPECT_FILE_ENV`] names a file.
    pub fn from_env() -> Self {
        Self::new(std::env::var_os(EXPECT_FILE_ENV).map(PathBuf::from))
    }

    pub fn new(file: Option<PathBuf>) -> Self {
        Self { file, state: Mutex::new(State::default()) }
    }

    /// Whether this carplayd gates at all: only the wireless spawn is handed the file's path.
    pub fn armed(&self) -> bool {
        self.file.is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The named phone now, read afresh; a different name than last time forgets every refusal and the
    /// session decided for the old one.
    fn expected_now(&self, st: &mut State) -> Option<String> {
        let now = self
            .file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| normalize(&s));
        if now != st.expected {
            if !st.refused.is_empty() || st.serving.is_some() {
                eprintln!("[carplayd] phone gate: the phone btd serves changed; earlier refusals forgotten");
            }
            st.refused.clear();
            st.serving = None;
            st.expected = now.clone();
        }
        now
    }

    /// A new control connection from `peer`, before it may preempt anything.
    pub fn on_connection(&self, peer: IpAddr) -> Admit {
        let mut st = self.lock();
        if self.expected_now(&mut st).is_none() {
            return Admit::Accept;
        }
        if st.refused.contains(&peer) {
            return Admit::Refused;
        }
        match st.serving {
            Some((_, served)) if served != peer => Admit::Holding(served),
            _ => Admit::Accept,
        }
    }

    /// A phase-1 SETUP on session `generation` from `peer`, naming `device_id`: whether it may go on.
    pub fn on_setup(
        &self,
        generation: u64,
        peer: IpAddr,
        device_id: Option<&str>,
    ) -> bool {
        let mut st = self.lock();
        let Some(expected) = self.expected_now(&mut st) else {
            return true;
        };
        match device_id.and_then(normalize) {
            None => {
                eprintln!("[carplayd] phone gate: SETUP from {peer} names no phone; letting it through");
                true
            }
            Some(id) if id == expected => {
                st.serving = Some((generation, peer));
                true
            }
            Some(_) => {
                st.refused.insert(peer);
                false
            }
        }
    }

    /// Session `generation` ended: if it was the named phone's, a newcomer may preempt again.
    pub fn session_ended(&self, generation: u64) {
        let mut st = self.lock();
        if st.serving.map(|(g, _)| g) == Some(generation) {
            st.serving = None;
        }
    }

    /// Whether discovery should leave `ip` alone: its phone was refused at SETUP under the current name.
    pub fn is_refused(&self, ip: IpAddr) -> bool {
        let mut st = self.lock();
        self.expected_now(&mut st).is_some() && st.refused.contains(&ip)
    }

    /// Whether discovery should dial a phone whose `_carplay-ctrl` TXT `id` is `txt_id`. The phone puts
    /// its BR/EDR address there (bench 2026-11-01: `id=68:a7:29:00:25:10`), the same address btd names,
    /// so another phone is left alone before anything is dialled (fix 1b). No gate, or no readable id:
    /// dial, and the SETUP check still applies.
    pub fn wants(&self, txt_id: Option<&str>) -> bool {
        let mut st = self.lock();
        match (self.expected_now(&mut st), txt_id.and_then(normalize)) {
            (Some(expected), Some(id)) => id == expected,
            _ => true,
        }
    }

    /// Whether the phone whose TXT `id` is `txt_id` already has its session here: it is the named phone,
    /// its SETUP was let through, and that session has not ended. Discovery leaves it alone then. On the
    /// 2026-11-02 bench a live phone's record gained two addresses mid-session; discovery dialled one of
    /// them, the iPhone answered 200 OK, and the session that was showing video ended 29 s later.
    pub fn serving(&self, txt_id: Option<&str>) -> bool {
        let mut st = self.lock();
        match (self.expected_now(&mut st), txt_id.and_then(normalize)) {
            (Some(expected), Some(id)) => id == expected && st.serving.is_some(),
            _ => false,
        }
    }
}

/// A BR/EDR address in any usual spelling (`AA:BB:..`, `aa-bb-..`, bare hex) as 12 lower-case hex
/// digits; `None` for anything else.
fn normalize(s: &str) -> Option<String> {
    let hex: String = s.trim().chars().filter(|c| !matches!(c, ':' | '-')).collect();
    (hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
}

/// The session delegate behind the gate: a phase-1 SETUP from another phone is refused before the inner
/// delegate sees it (so it never publishes that phone's identity), and its connection is closed.
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
            if !self.gate.on_setup(self.generation, self.peer, id.as_deref()) {
                eprintln!(
                    "[carplayd] phone gate: SETUP from {} names another phone than btd serves; refused, and {} \
                     is ignored until btd serves another phone",
                    self.peer, self.peer
                );
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

    const OWNER: &str = "68:A7:29:00:25:10";
    const OTHER: &str = "AA:BB:CC:00:6A:DE";

    fn gate_naming(phone: Option<&str>) -> (PhoneGate, PathBuf) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "carplayd_expect_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        match phone {
            Some(p) => std::fs::write(&path, p).unwrap(),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        (PhoneGate::new(Some(path.clone())), path)
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 168, 43, last])
    }

    #[test]
    fn the_phone_the_switch_dropped_is_refused_at_setup_and_stays_out() {
        // 2026-11-01 bench: the dropped phone (.100) was dialled first, then the requested one joined (.101).
        let (gate, path) = gate_naming(Some(OWNER));
        assert_eq!(gate.on_connection(ip(100)), Admit::Accept, "nothing known about .100 yet");
        assert!(!gate.on_setup(1, ip(100), Some(OTHER)), "another phone's SETUP is refused");
        assert!(gate.is_refused(ip(100)), "discovery leaves it alone");
        assert_eq!(gate.on_connection(ip(100)), Admit::Refused, "and its reconnects are dropped");
        assert_eq!(gate.on_connection(ip(101)), Admit::Accept);
        assert!(gate.on_setup(2, ip(101), Some("68-a7-29-00-25-10")), "the named phone, in any spelling");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_named_phones_session_is_not_preempted_from_another_address() {
        let (gate, path) = gate_naming(Some(OWNER));
        assert!(gate.on_setup(7, ip(101), Some(OWNER)));
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
        assert!(!gate.on_setup(1, ip(100), Some(OTHER)));
        std::fs::write(&path, OTHER).unwrap();
        assert!(!gate.is_refused(ip(100)));
        assert_eq!(gate.on_connection(ip(100)), Admit::Accept);
        assert!(gate.on_setup(2, ip(100), Some(OTHER)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn no_name_no_gate() {
        let (gate, _) = gate_naming(None);
        assert!(gate.armed(), "the wireless spawn, before btd has named a phone");
        assert!(gate.on_setup(1, ip(100), Some(OTHER)));
        assert_eq!(gate.on_connection(ip(101)), Admit::Accept);
        let wired = PhoneGate::new(None);
        assert!(!wired.armed());
        assert!(wired.on_setup(1, ip(100), Some(OTHER)));
        assert!(!wired.is_refused(ip(100)));
    }

    #[test]
    fn discovery_dials_only_the_named_phone_by_its_txt_id() {
        // 2026-11-01 bench: "_carplay-ctrl TXT: id=68:a7:29:00:25:10 srcvers=1005.13.1".
        let (gate, path) = gate_naming(Some(OWNER));
        assert!(gate.wants(Some("68:a7:29:00:25:10")), "the TXT spells it in lower case");
        assert!(!gate.wants(Some(OTHER)), "another phone is never dialled");
        assert!(gate.wants(None), "no id: dial, and the SETUP check decides");
        assert!(gate.wants(Some("not a mac")));
        std::fs::write(&path, OTHER).unwrap();
        assert!(gate.wants(Some(OTHER)), "follows the phone btd serves now");
        let _ = std::fs::remove_file(path);
        assert!(gate.wants(Some(OWNER)), "no name, no gate");
    }

    #[test]
    fn discovery_does_not_dial_the_named_phone_while_its_session_is_live() {
        // 2026-11-02 bench: the record changed to [fe80::.., .100, .101] mid-session and .100 was dialled.
        let (gate, path) = gate_naming(Some(OWNER));
        assert!(!gate.serving(Some(OWNER)), "no session yet: dial it");
        assert!(gate.on_setup(3, ip(101), Some(OWNER)));
        assert!(gate.serving(Some("68:a7:29:00:25:10")), "its session is live: leave it alone");
        assert!(!gate.serving(Some(OTHER)), "says nothing about another phone");
        assert!(!gate.serving(None), "nor about a TXT without an id");
        gate.session_ended(3);
        assert!(!gate.serving(Some(OWNER)), "once the session ends it may be dialled again");
        let _ = std::fs::remove_file(path);
        assert!(!PhoneGate::new(None).serving(Some(OWNER)), "no gate: as before");
    }

    #[test]
    fn a_setup_naming_no_phone_is_let_through() {
        let (gate, path) = gate_naming(Some(OWNER));
        assert!(gate.on_setup(1, ip(100), None));
        assert!(gate.on_setup(1, ip(100), Some("not a mac")));
        assert_eq!(gate.on_connection(ip(101)), Admit::Accept, "not marked as the named phone's session");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn addresses_normalize_however_they_are_spelled() {
        assert_eq!(normalize(" 68:A7:29:00:25:10\n").as_deref(), Some("68a729002510"));
        assert_eq!(normalize("68a729002510").as_deref(), Some("68a729002510"));
        assert_eq!(normalize("68:A7:29:00:25"), None);
        assert_eq!(normalize("ZZ:A7:29:00:25:10"), None);
    }
}
