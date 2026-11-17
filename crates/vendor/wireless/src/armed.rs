//! btd armed: Bluetooth powered between host sessions but invisible (the owner's decision, 16 Nov 2026), so the
//! host's arrival is a "go" into today's session code rather than a power-up and a process start.
//!
//! One fresh btd per host session still holds: the supervisor starts this btd (`OCBM_BTD_ARM=1`) once the last host
//! has gone and its teardown is over, or after the boot preload, and it becomes the NEXT host's btd. Everything a btd
//! builds at its start for a session (its control port, its `Control`, the switch hold it takes up, its phase) is
//! built after the go, as in a btd the supervisor started at the host's arrival, so every per-process assumption
//! (first-arm-wins caches, per-session state) stays true. Only the controller's bring-up moved ahead.
//!
//! The protocol (`tools/session_supervisor.sh`, "Armed Bluetooth"):
//! - **Arm.** [`bt_bringup::arm`] under the seam's Bluetooth lock: the controller up as a session's bring-up leaves
//!   it, page and inquiry scan off, proved by the controller's own answer, no link. Then [`ARMED_FILE`] (this pid) and
//!   a wait in `poll()` on a self-pipe that SIGUSR1 and SIGTERM write to, with a timeout for the next check. Nothing
//!   pages, nothing listens, nothing polls.
//! - **Go.** The supervisor writes [`GO_FILE`] (`pid=` this btd, and the session's inputs, the values its
//!   `wireless_up` gives a btd's environment at the host's arrival) and sends SIGUSR1. btd takes the inputs into its
//!   environment, proves the controller with one real HCI round trip ([`GO_PROOF_TRIES`] at most, [`GO_PROOF_WAIT`]
//!   each), writes [`GO_RESULT`] `ok <uptime ms>` and returns: a host session begins. A failed proof writes
//!   `fail <why>`, wakes the supervisor and exits, and the supervisor falls back to today's path (`bt_on`, a fresh
//!   btd).
//! - **Checks.** Every [`CHECK_EVERY`], one real round trip and a look at the links ([`bt_bringup::audit`]). Strict to
//!   declare the controller well, reluctant to give it up: a miss is tried again [`CHECK_RETRY`] later, up to
//!   [`CHECK_TRIES`] in a row, and only then does btd exit, so the next host takes today's path. A check never resets
//!   anything: at worst btd exits, and the supervisor powers Bluetooth down through the seam (`bt_off`).

use std::io::{ErrorKind, Read as _};
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bt_bringup;

/// The environment variable the supervisor starts an armed btd with (`1`).
pub const ARM_ENV: &str = "OCBM_BTD_ARM";
/// This btd's pid while it is armed and ready for a go; removed at the go and on a clean exit.
pub const ARMED_FILE: &str = "/tmp/.btd_armed";
/// The go: `pid=<the armed btd>` and the session's inputs as `KEY=VALUE` lines, written before the signal.
pub const GO_FILE: &str = "/tmp/.btd_go";
/// The go's answer: `ok <uptime ms>` or `fail <why>`, read by the supervisor.
pub const GO_RESULT: &str = "/tmp/.btd_go_result";
/// radio_hal.sh's Bluetooth lock (`lock_take bt`): a directory holding its holder's pid.
const SEAM_BT_LOCK: &str = "/tmp/.radio_lock_bt";
/// The supervisor's pid (its singleton lock), woken when a go fails so the fallback starts at once.
const SUPERVISOR_PID_FILE: &str = "/tmp/.sup_lock/pid";

/// How often an armed controller is checked.
pub const CHECK_EVERY: Duration = Duration::from_secs(180);
/// A check that found no answer is tried again this soon...
pub const CHECK_RETRY: Duration = Duration::from_secs(2);
/// ...this many times in a row before the armed state is given up (radio_hal.sh's `bt_responsive_confirmed` bar: a
/// healthy controller misses about one read in four).
pub const CHECK_TRIES: u32 = 5;
/// A check that found the controller visible or holding a link hides it again, this many times in a row at most.
pub const HIDE_TRIES: u32 = 3;
/// How long a check's round trip may take.
const CHECK_WAIT: Duration = Duration::from_millis(500);
/// How long the go's round trip may take: a healthy controller answers in a few ms over the UART.
pub const GO_PROOF_WAIT: Duration = Duration::from_millis(150);
/// Round trips the go's proof may take: a second for the one miss in four.
pub const GO_PROOF_TRIES: u32 = 2;

/// Exit codes: the arm failed (nothing was ever armed), a check gave the armed state up, the go's proof failed.
pub const EXIT_ARM_FAILED: i32 = 75;
pub const EXIT_CHECK_FAILED: i32 = 76;
pub const EXIT_GO_FAILED: i32 = 77;

/// `@<unix_ms> [armed] …`, as btd's other lines (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG).
fn log(m: &str) {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    println!("@{ms} [armed] {m}");
}

/// Whether the supervisor started this btd armed.
pub fn requested() -> bool {
    std::env::var(ARM_ENV).as_deref() == Ok("1")
}

/// How the armed state ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A host's go, proved: a host session begins. `armed_name` is the name the arm wrote to the controller.
    Go { armed_name: String },
    /// Exit with this code (0: SIGTERM).
    Exit(i32),
}

// ---- The decisions, kept apart from the radio so they can be tested -----------------------------------------------

/// The session's inputs a go carries, each as `wireless_up` gives a btd's environment at the host's arrival.
#[derive(Debug, PartialEq, Eq)]
pub struct GoRequest {
    pub env: Vec<(&'static str, String)>,
}

/// The inputs, their defaults (the supervisor's for a config without the key) and the values they may take.
const GO_INPUTS: [(&str, &str, &[&str]); 2] =
    [("BT_PAIRING_MODE", "just_works", &["just_works", "numeric"]), ("BT_SSP_INTERACTIVE", "0", &["0", "1"])];

/// The go in `text` ([`GO_FILE`]) when it is addressed to `pid` (this btd): None for one with another pid or none (a
/// stale file, a spurious wake). Every input is set: one missing or not a value it may take is its default, as the
/// supervisor's own functions give it for a config without the key.
pub fn parse_go(text: &str, pid: u32) -> Option<GoRequest> {
    let mut to_us = false;
    let mut given: Vec<(&str, &str)> = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if key == "pid" {
            to_us = value.parse::<u32>().ok() == Some(pid);
        } else {
            given.push((key, value));
        }
    }
    if !to_us {
        return None;
    }
    let env = GO_INPUTS
        .iter()
        .map(|&(key, default, allowed)| {
            let v = given.iter().rev().find(|(k, _)| *k == key).map(|&(_, v)| v).filter(|v| allowed.contains(v));
            (key, v.unwrap_or(default).to_string())
        })
        .collect();
    Some(GoRequest { env })
}

/// What one armed check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// The controller answered: scan off, no link.
    Clean,
    /// It answered, but with a scan on or a link held.
    Visible,
    /// No answer in time.
    Silent,
}

/// What the armed state does after a check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Check again after this long.
    Wait(Duration),
    /// Hide the controller again ([`bt_bringup::hide`]) and check again after [`CHECK_RETRY`].
    Hide,
    /// Give the armed state up: exit, so the next host takes today's path.
    Exit(&'static str),
}

/// The checks' schedule: strict to declare the controller well, reluctant to give it up.
#[derive(Debug, Default)]
pub struct Checks {
    misses: u32,
    hides: u32,
}

impl Checks {
    pub fn after(&mut self, found: Found) -> Next {
        match found {
            Found::Clean => {
                self.misses = 0;
                self.hides = 0;
                Next::Wait(CHECK_EVERY)
            }
            Found::Silent => {
                self.misses += 1;
                if self.misses >= CHECK_TRIES {
                    Next::Exit("the controller did not answer")
                } else {
                    Next::Wait(CHECK_RETRY)
                }
            }
            Found::Visible => {
                self.misses = 0;
                self.hides += 1;
                if self.hides > HIDE_TRIES {
                    Next::Exit("the controller would not stay invisible")
                } else {
                    Next::Hide
                }
            }
        }
    }
}

/// The go's proof: up to [`GO_PROOF_TRIES`] round trips (`probe`), the first answer deciding. Ok with the scan mode
/// the controller read, Err with the last failure.
pub fn go_proof(mut probe: impl FnMut() -> std::io::Result<u8>) -> Result<u8, String> {
    let mut last = String::from("no round trip tried");
    for _ in 0..GO_PROOF_TRIES {
        match probe() {
            Ok(scan) => return Ok(scan),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

// ---- The armed state on the box ---------------------------------------------------------------------------------

/// Arm, then wait for the go, a check or SIGTERM: see the module docs. `name` is the session name the arm writes (the
/// go rewrites it when the session's differs).
pub fn run(shutdown: &Arc<AtomicBool>, hci_dev: &str, name: &str) -> Outcome {
    let rx = match wake_pipe() {
        Ok(rx) => rx,
        Err(e) => {
            log(&format!("no wake pipe ({e}) -- not arming; the host's arrival takes today's path"));
            return Outcome::Exit(EXIT_ARM_FAILED);
        }
    };
    let armed = match SeamLock::take(SEAM_BT_LOCK) {
        Ok(_lock) => bt_bringup::arm(hci_dev, name),
        Err(e) => Err(std::io::Error::new(e.kind(), format!("a radio bring-up holds Bluetooth ({e})"))),
    };
    if shutdown.load(Ordering::Relaxed) {
        log("SIGTERM while arming -- exiting");
        return Outcome::Exit(0);
    }
    let audit = match armed {
        Ok(a) => a,
        Err(e) => {
            log(&format!("not armed: {e} -- exiting; the host's arrival takes today's path"));
            return Outcome::Exit(EXIT_ARM_FAILED);
        }
    };
    let me = std::process::id();
    if let Err(e) = write_atomic(ARMED_FILE, &format!("{me}\n")) {
        log(&format!("{ARMED_FILE} not written ({e}) -- no go could find this btd; exiting"));
        return Outcome::Exit(EXIT_ARM_FAILED);
    }
    log(&format!(
        "armed as \"{name}\": Bluetooth powered, scan 0x{:02x} (page and inquiry off), {} connection(s), nothing paging \
         or listening; waiting for a host (checked every {} s)",
        audit.scan,
        audit.conns,
        CHECK_EVERY.as_secs()
    ));
    let mut checks = Checks::default();
    let mut next_check = Instant::now() + CHECK_EVERY;
    loop {
        let woke = wait(&rx, next_check.saturating_duration_since(Instant::now()));
        if shutdown.load(Ordering::Relaxed) {
            unarm(me);
            log("SIGTERM while armed -- exiting");
            return Outcome::Exit(0);
        }
        if woke {
            match std::fs::read_to_string(GO_FILE).ok().and_then(|t| parse_go(&t, me)) {
                Some(req) => return go(req, hci_dev, name),
                None => log("woken with no go for this btd -- still armed"),
            }
        }
        if Instant::now() < next_check {
            continue;
        }
        let found = match bt_bringup::audit(hci_dev, CHECK_WAIT) {
            Ok(a) if a.invisible() => Found::Clean,
            Ok(a) => {
                log(&format!("check: the controller reads scan 0x{:02x} with {} connection(s) -- hiding it again", a.scan, a.conns));
                Found::Visible
            }
            Err(e) => {
                log(&format!("check: no answer from the controller ({e})"));
                Found::Silent
            }
        };
        match checks.after(found) {
            Next::Wait(d) => next_check = Instant::now() + d,
            Next::Hide => {
                if let Err(e) = bt_bringup::hide(hci_dev) {
                    log(&format!("check: hiding failed ({e})"));
                }
                next_check = Instant::now() + CHECK_RETRY;
            }
            Next::Exit(why) => {
                unarm(me);
                log(&format!("{why} -- leaving the armed state; the host's arrival takes today's path"));
                return Outcome::Exit(EXIT_CHECK_FAILED);
            }
        }
    }
}

/// The go: the inputs into the environment, the proof, the answer. No other thread runs yet (the control port and the
/// session's threads start after the go), so setting the environment here races nothing.
fn go(req: GoRequest, hci_dev: &str, name: &str) -> Outcome {
    let t0 = Instant::now();
    let _ = std::fs::remove_file(ARMED_FILE);
    let _ = std::fs::remove_file(GO_FILE);
    for (key, value) in &req.env {
        std::env::set_var(key, value);
    }
    let proof = match crate::hci::dev_id(hci_dev) {
        Ok(dev) => go_proof(|| crate::hci::read_scan_enable(dev, GO_PROOF_WAIT)),
        Err(e) => Err(e.to_string()),
    };
    let inputs = req.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
    match proof {
        Ok(scan) => {
            let at = box_common::timeline::now_ms().map_or_else(|| "-".to_string(), |ms| ms.to_string());
            if let Err(e) = write_atomic(GO_RESULT, &format!("ok {at}\n")) {
                log(&format!("go: {GO_RESULT} not written ({e}) -- the supervisor will fall back after its wait"));
            }
            if scan != crate::hci::SCAN_NONE as u8 {
                log(&format!("go: the controller read scan 0x{scan:02x}: it was not invisible while armed"));
            }
            log(&format!("go: the controller answered in {} ms ({inputs}) -- a host session begins", t0.elapsed().as_millis()));
            Outcome::Go { armed_name: name.to_string() }
        }
        Err(e) => {
            let _ = write_atomic(GO_RESULT, &format!("fail no answer from the controller ({e})\n"));
            log(&format!("go: no answer from the controller ({e}) -- exiting; the supervisor falls back to bt_on and a fresh btd"));
            wake_supervisor(SUPERVISOR_PID_FILE);
            Outcome::Exit(EXIT_GO_FAILED)
        }
    }
}

/// [`ARMED_FILE`] removed when it still names this btd.
fn unarm(me: u32) {
    if std::fs::read_to_string(ARMED_FILE).is_ok_and(|s| s.trim().parse::<u32>().ok() == Some(me)) {
        let _ = std::fs::remove_file(ARMED_FILE);
    }
}

/// A self-pipe SIGUSR1 (the go) and SIGTERM write to, its read end non-blocking. SIGTERM's flag (main) stays the
/// word on shutdown; the pipe only wakes the wait.
fn wake_pipe() -> std::io::Result<UnixStream> {
    let (rx, tx) = UnixStream::pair()?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGUSR1, tx.try_clone()?)?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGTERM, tx)?;
    rx.set_nonblocking(true)?;
    Ok(rx)
}

/// Block until a signal wrote to `rx` or `timeout` passed: true when woken (the pipe drained).
fn wait(rx: &UnixStream, timeout: Duration) -> bool {
    let mut pfd = libc::pollfd { fd: rx.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let ms = timeout.as_millis().clamp(0, i32::MAX as u128) as libc::c_int;
    // SAFETY: one pollfd for a socket this function borrows.
    let r = unsafe { libc::poll(&mut pfd, 1, ms) };
    // A signal that lands in poll() interrupts it (EINTR) after its handler wrote the pipe: woken either way.
    let woke = r > 0 || (r < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted);
    if woke {
        let mut buf = [0u8; 64];
        while matches!((&*rx).read(&mut buf), Ok(n) if n > 0) {}
    }
    woke
}

/// Write `text` to `path` aside and rename it over, so a reader never sees half of it.
fn write_atomic(path: &str, text: &str) -> std::io::Result<()> {
    let tmp = format!("{path}.tmp.{}", std::process::id());
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A signal-0 probe: EPERM is a live process of another user.
fn pid_alive(pid: libc::pid_t) -> bool {
    // SAFETY: kill with signal 0 sends nothing; it only checks that the process exists.
    pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// Wake the supervisor (SIGUSR1, which it traps), as ocbmd's `wake_supervisor` does: only a process whose command line
/// names session_supervisor, so a recycled pid never takes a signal whose default action ends it.
fn wake_supervisor(pid_file: &str) {
    let Some(pid) = std::fs::read_to_string(pid_file).ok().and_then(|s| s.trim().parse::<libc::pid_t>().ok()) else {
        return;
    };
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    if pid > 0 && String::from_utf8_lossy(&cmdline).contains("session_supervisor") {
        // SAFETY: a signal to the supervisor just identified; it traps USR1.
        unsafe { libc::kill(pid, libc::SIGUSR1) };
    }
}

/// The seam's Bluetooth lock, held while the armed bring-up touches the controller, so it never runs beside a `bt_on`,
/// a `bt_off` or the boot preload (radio_hal.sh's `lock_take`: a directory, its holder's pid inside). A lock whose
/// holder is gone is reclaimed, as the shell does; a live holder, or one not yet named, means no arm now. Released on
/// drop; a btd that dies holding it leaves a dead pid, which the shell reclaims.
struct SeamLock(String);

impl SeamLock {
    fn take(path: &str) -> std::io::Result<SeamLock> {
        for _ in 0..2 {
            match std::fs::create_dir(path) {
                Ok(()) => {
                    std::fs::write(format!("{path}/pid"), format!("{}\n", std::process::id()))?;
                    return Ok(SeamLock(path.to_string()));
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let holder = std::fs::read_to_string(format!("{path}/pid")).ok().and_then(|s| s.trim().parse::<libc::pid_t>().ok());
                    match holder {
                        Some(pid) if !pid_alive(pid) => {
                            let _ = std::fs::remove_dir_all(path);
                        }
                        _ => {
                            let who = holder.map_or_else(|| "a holder not yet named".to_string(), |p| format!("pid {p}"));
                            return Err(std::io::Error::new(ErrorKind::WouldBlock, format!("{path} is held by {who}")));
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(ErrorKind::WouldBlock, format!("{path} was taken again as it was reclaimed")))
    }
}

impl Drop for SeamLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A go is the supervisor's for THIS btd: another pid's, or none, is a stale file or a spurious wake.
    #[test]
    fn a_go_is_taken_only_when_it_names_this_btd() {
        let go = "pid=4242\nBT_PAIRING_MODE=numeric\nBT_SSP_INTERACTIVE=1\n";
        assert_eq!(
            parse_go(go, 4242),
            Some(GoRequest { env: vec![("BT_PAIRING_MODE", "numeric".into()), ("BT_SSP_INTERACTIVE", "1".into())] })
        );
        assert_eq!(parse_go(go, 4243), None, "another btd's");
        assert_eq!(parse_go("BT_PAIRING_MODE=numeric\n", 4242), None, "no pid: no go");
        assert_eq!(parse_go("pid=x\n", 4242), None);
        assert_eq!(parse_go("", 4242), None);
    }

    /// The inputs as `wireless_up` gives them, and each one's default when it is missing or not a value it takes: the
    /// supervisor's own defaults for a config without the key (Just Works, no interactive SSP).
    #[test]
    fn every_input_is_set_a_bad_one_to_its_default() {
        let env = |text: &str| parse_go(text, 7).unwrap().env;
        let defaults = vec![("BT_PAIRING_MODE", "just_works".to_string()), ("BT_SSP_INTERACTIVE", "0".to_string())];
        assert_eq!(env("pid=7\n"), defaults, "nothing given: the defaults");
        assert_eq!(env("pid=7\nBT_PAIRING_MODE=bogus\nBT_SSP_INTERACTIVE=2\nOTHER=1\n"), defaults, "not values they take");
        assert_eq!(
            env(" pid = 7 \nBT_SSP_INTERACTIVE = 1\nBT_PAIRING_MODE=just_works\nBT_PAIRING_MODE=numeric\n"),
            vec![("BT_PAIRING_MODE", "numeric".to_string()), ("BT_SSP_INTERACTIVE", "1".to_string())],
            "spaces trimmed; the last of a key stands"
        );
    }

    /// Strict to declare success, reluctant to destroy: one miss is tried again 2 s later, and only the fifth in a row
    /// gives the armed state up; an answer clears the count.
    #[test]
    fn the_checks_give_up_only_after_five_misses_in_a_row() {
        let mut c = Checks::default();
        assert_eq!(c.after(Found::Clean), Next::Wait(CHECK_EVERY));
        for _ in 1..CHECK_TRIES {
            assert_eq!(c.after(Found::Silent), Next::Wait(CHECK_RETRY));
        }
        assert_eq!(c.after(Found::Clean), Next::Wait(CHECK_EVERY), "an answer before the fifth: well again");
        for _ in 1..CHECK_TRIES {
            assert_eq!(c.after(Found::Silent), Next::Wait(CHECK_RETRY));
        }
        assert!(matches!(c.after(Found::Silent), Next::Exit(_)), "the fifth miss in a row");
    }

    /// A controller that answers visible is hidden again, a few times; one that will not stay hidden is given up, and
    /// an invisible answer clears the count.
    #[test]
    fn a_visible_controller_is_hidden_again_then_given_up() {
        let mut c = Checks::default();
        for _ in 0..HIDE_TRIES {
            assert_eq!(c.after(Found::Visible), Next::Hide);
        }
        assert!(matches!(c.after(Found::Visible), Next::Exit(_)));
        let mut c = Checks::default();
        assert_eq!(c.after(Found::Visible), Next::Hide);
        assert_eq!(c.after(Found::Clean), Next::Wait(CHECK_EVERY));
        for _ in 0..HIDE_TRIES {
            assert_eq!(c.after(Found::Visible), Next::Hide, "the count started over");
        }
        let mut c = Checks::default();
        for _ in 1..CHECK_TRIES {
            assert_eq!(c.after(Found::Silent), Next::Wait(CHECK_RETRY));
        }
        assert_eq!(c.after(Found::Visible), Next::Hide, "an answer, visible or not, ends a run of misses");
        assert_eq!(c.after(Found::Silent), Next::Wait(CHECK_RETRY));
    }

    /// The go's fallback decision: the first answer proves the controller (whatever scan it reads); two misses send
    /// the host to today's path.
    #[test]
    fn the_go_proceeds_on_the_first_answer_and_falls_back_on_two_misses() {
        let mut tries = 0;
        assert_eq!(go_proof(|| { tries += 1; Ok(0) }), Ok(0));
        assert_eq!(tries, 1, "one round trip when it answers");
        let mut tries = 0;
        let flaky = go_proof(|| {
            tries += 1;
            if tries == 1 { Err(std::io::Error::new(ErrorKind::TimedOut, "no answer to HCI 0x0c19")) } else { Ok(2) }
        });
        assert_eq!(flaky, Ok(2), "one miss, then an answer (visible while armed: logged, still a go)");
        let mut tries = 0;
        let dead = go_proof(|| {
            tries += 1;
            Err(std::io::Error::new(ErrorKind::TimedOut, "no answer to HCI 0x0c19"))
        });
        assert_eq!(dead, Err("no answer to HCI 0x0c19".to_string()));
        assert_eq!(tries, GO_PROOF_TRIES, "fallback after GO_PROOF_TRIES misses");
        assert!(GO_PROOF_WAIT * GO_PROOF_TRIES <= Duration::from_millis(300), "a failed proof costs the fallback little");
    }

    /// The seam's lock, as radio_hal.sh's lock_take shares it: taken and released; refused while a live process holds
    /// it, or while it names no holder yet; reclaimed from a dead holder.
    #[test]
    fn the_seam_lock_is_shared_with_the_shell() {
        let dir = std::env::temp_dir().join(format!("armed_lock_{}", std::process::id()));
        let path = dir.to_str().unwrap().to_string();
        let _ = std::fs::remove_dir_all(&dir);
        {
            let _l = SeamLock::take(&path).expect("free");
            assert_eq!(std::fs::read_to_string(dir.join("pid")).unwrap().trim(), std::process::id().to_string());
            assert!(SeamLock::take(&path).is_err(), "held by a live process (this one)");
        }
        assert!(!dir.exists(), "released on drop");
        std::fs::create_dir(&dir).unwrap();
        assert!(SeamLock::take(&path).is_err(), "a lock not yet named: no arm now");
        std::fs::write(dir.join("pid"), "999999999\n").unwrap();
        let reclaimed = SeamLock::take(&path);
        assert!(reclaimed.is_ok(), "a dead holder's lock is reclaimed");
        drop(reclaimed);
        assert!(!dir.exists());
    }

    /// The wait wakes on a signal through the pipe at once, and otherwise sleeps out its timeout.
    #[test]
    fn the_wait_wakes_on_the_pipe_and_otherwise_times_out() {
        let (rx, tx) = UnixStream::pair().unwrap();
        rx.set_nonblocking(true).unwrap();
        let t = Instant::now();
        assert!(!wait(&rx, Duration::from_millis(30)), "nothing written: the timeout");
        assert!(t.elapsed() >= Duration::from_millis(25));
        use std::io::Write as _;
        (&tx).write_all(&[1, 1, 1]).unwrap();
        let t = Instant::now();
        assert!(wait(&rx, Duration::from_secs(5)), "written: woken");
        assert!(t.elapsed() < Duration::from_secs(1), "at once");
        assert!(!wait(&rx, Duration::from_millis(10)), "drained: the next wait sleeps");
    }
}
