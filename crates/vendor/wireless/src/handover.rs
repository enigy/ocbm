//! A btd started before Bluetooth is up: a cold boot with a host waiting (R3-5, 20 Nov 2026).
//!
//! On the truck most drives start with the adapter booting, and the host subscribes while the boot preload is still
//! attaching the controller (docs/wireless/01_BT_AND_RADIO.md, "The cold boot"). Until now its session waited for the
//! preload to end, then for its own `bt_on` behind the seam's lock, then for btd's start and btd's DOWN/UP: on the dev
//! adapter's bench of 20 Nov (`70caa3c`, real power cycles) the first page went out 1.6 to 1.9 s after hci0 answered.
//!
//! Opt-in since 21 Nov (`/script/radio_handover_on`; off by default): the supervisor starts this btd at the host's arrival
//! while the boot preload runs (`OCBM_BTD_WAIT=1`, `wireless_up`), beside the host's `bt_on`. It does what a btd does at its start up to
//! its first bring-up (the control port, the session's record, the AP watch), then [`wait`]s for the first of:
//! - **the preload's handover** ([`HANDOVER`], written by `radio_hal.sh` `bt_handover_give`): the controller this
//!   boot's attach registered, claimed by an HCIDEVUP (the kernel's auto power-off cancelled) and handed to the host
//!   that came. `bt_bringup::from_handover` proves it with one round trip and brings it up with no DOWN/UP;
//! - **`bt_on` over** ([`BT_ON_DONE`], written by the supervisor's wrapper whatever `bt_on` returned): today's bring-up,
//!   as in a btd started after `bt_on`;
//! - **SIGTERM**: the host left, or a teardown.
//!
//! A handover counts only when it was written since this host came (ocbmd's origin, `box_common::timeline::ORIGIN`):
//! one from before is another host's, whose teardown (`bt_off`) should have removed it. The host's `bt_on` reads the
//! same file by the same rule (`radio_hal.sh` `bt_handed`) and, finding it, touches nothing, so the controller has one
//! owner at a time: the preload until it writes the file, this btd after.
//!
//! Nothing here pages or scans: the session's bring-up does, after the wait, as in any btd. Event-driven: the wait
//! sleeps in `poll()` on an inotify watch of `/tmp` and a pipe SIGTERM writes to, with a backstop look every
//! [`BACKSTOP`].

use std::io::Read as _;
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The environment variable the supervisor starts a waiting btd with (`1`).
pub const WAIT_ENV: &str = "OCBM_BTD_WAIT";
/// The boot preload's handover: `<handed ms> <hci_up ms> <dev>`, uptimes in ms (`radio_hal.sh` `bt_handover_give`).
pub const HANDOVER: &str = "/tmp/.radio_bt_handover";
/// Written by the supervisor's wrapper once the host's `bt_on` is over, whatever it returned.
pub const BT_ON_DONE: &str = "/tmp/.btd_bt_on_done";
/// Where both are written: the directory the wait watches.
const DIR: &str = "/tmp";
/// The shell's uptime is `/proc/uptime`'s, to 10 ms; ocbmd's origin is to the ms. A handover written in the same 10 ms
/// as the origin reads up to 9 ms before it.
const SHELL_STEP_MS: u64 = 10;
/// The longest the wait sleeps between two looks with `/tmp` watched: a backstop only, as each write there wakes it.
pub const BACKSTOP: Duration = Duration::from_millis(250);
/// Between two looks with no watch (no inotify: a developer's host, or inotify refused): the looks are all there is.
pub const TICK: Duration = Duration::from_millis(20);

/// `@<unix_ms> [handover] …`, as btd's other lines (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG).
fn log(m: &str) {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    println!("@{ms} [handover] {m}");
}

/// Whether the supervisor started this btd to wait for the controller.
pub fn requested() -> bool {
    std::env::var(WAIT_ENV).as_deref() == Ok("1")
}

/// The preload's handover, as this btd takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handover {
    /// When the preload handed the controller over (uptime ms).
    pub handed_ms: u64,
    /// When hci0 first answered the preload's attach (uptime ms): the timeline's `bt_power` on this path.
    pub hci_up_ms: u64,
}

/// The handover `text` ([`HANDOVER`]) for this host (`origin`, the timeline's) and device `dev`: None when it is none
/// (malformed), another device's, from before this host came, or when there is no origin to judge it against.
pub fn parse(text: &str, origin: Option<u64>, dev: &str) -> Option<Handover> {
    let mut words = text.split_whitespace();
    let handed_ms: u64 = words.next()?.parse().ok()?;
    let hci_up_ms: u64 = words.next()?.parse().ok()?;
    if words.next()? != dev {
        return None;
    }
    (handed_ms.saturating_add(SHELL_STEP_MS) > origin?).then_some(Handover { handed_ms, hci_up_ms })
}

/// How the wait ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Go {
    /// The preload handed the controller to this host's session.
    Handover(Handover),
    /// The host's `bt_on` is over: today's bring-up.
    BtOn,
    /// SIGTERM.
    Exit,
}

/// The decision from what is there now: a handover for this host first (it is the sooner and asks less of the
/// controller), then `bt_on`'s end; None while neither is.
pub fn decide(handover: Option<&str>, origin: Option<u64>, bt_on_done: bool, dev: &str) -> Option<Go> {
    if let Some(h) = handover.and_then(|t| parse(t, origin, dev)) {
        return Some(Go::Handover(h));
    }
    bt_on_done.then_some(Go::BtOn)
}

/// The files a wait reads (the box's in [`wait`], a test's own elsewhere).
pub struct Files<'a> {
    pub handover: &'a str,
    pub bt_on_done: &'a str,
    pub origin: &'a str,
}

const BOX_FILES: Files<'static> = Files { handover: HANDOVER, bt_on_done: BT_ON_DONE, origin: box_common::timeline::ORIGIN };

/// One look at the files.
fn look(files: &Files, dev: &str) -> Option<Go> {
    let handover = std::fs::read_to_string(files.handover).ok();
    let origin = box_common::timeline::read_origin(files.origin);
    decide(handover.as_deref(), origin, std::path::Path::new(files.bt_on_done).exists(), dev)
}

/// Wait for the controller (the module docs), on the box's files; `dev` is the controller (`hci0`).
pub fn wait(shutdown: &AtomicBool, dev: &str) -> Go {
    let t0 = Instant::now();
    let wake = match term_pipe() {
        Ok(rx) => Some(rx),
        Err(e) => {
            log(&format!("no SIGTERM pipe ({e}): a SIGTERM is seen at the next look"));
            None
        }
    };
    let watch = Watch::new(DIR);
    let every = if watch.is_some() { BACKSTOP } else { TICK };
    let go = wait_on(&BOX_FILES, shutdown, dev, wake.as_ref(), watch.as_ref(), every);
    let waited = t0.elapsed().as_millis();
    match go {
        Go::Handover(h) => log(&format!(
            "the boot preload handed {dev} over (it answered at uptime {} ms, handed over at {} ms) after {waited} ms \
             here: the session's bring-up from it",
            h.hci_up_ms, h.handed_ms
        )),
        Go::BtOn => log(&format!("bt_on is over after {waited} ms here, with no handover: today's bring-up")),
        Go::Exit => log(&format!("SIGTERM after {waited} ms waiting for the controller")),
    }
    go
}

/// The wait itself: a look, then a sleep until a write in the watched directory, a signal through `wake`, or `every`.
fn wait_on(files: &Files, shutdown: &AtomicBool, dev: &str, wake: Option<&UnixStream>, watch: Option<&Watch>, every: Duration) -> Go {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Go::Exit;
        }
        if let Some(go) = look(files, dev) {
            return go;
        }
        sleep_until_woken(wake, watch, every);
    }
}

/// Sleep in `poll()` on `wake` and `watch` for `every` at most, then drain whichever woke it.
fn sleep_until_woken(wake: Option<&UnixStream>, watch: Option<&Watch>, every: Duration) {
    let mut fds: Vec<libc::pollfd> = Vec::with_capacity(2);
    if let Some(w) = wake {
        fds.push(libc::pollfd { fd: w.as_raw_fd(), events: libc::POLLIN, revents: 0 });
    }
    if let Some(w) = watch {
        fds.push(libc::pollfd { fd: w.fd(), events: libc::POLLIN, revents: 0 });
    }
    let ms = every.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
    // SAFETY: pollfds of descriptors this function borrows; none at all is a plain sleep. A signal ends it early (EINTR).
    unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
    if let Some(w) = wake {
        let mut buf = [0u8; 64];
        while matches!((&*w).read(&mut buf), Ok(n) if n > 0) {}
    }
    if let Some(w) = watch {
        w.drain();
    }
}

/// A pipe SIGTERM writes to (main's flag stays the word on shutdown; this only wakes the wait), its read end
/// non-blocking.
fn term_pipe() -> std::io::Result<UnixStream> {
    let (rx, tx) = UnixStream::pair()?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGTERM, tx)?;
    rx.set_nonblocking(true)?;
    Ok(rx)
}

/// An inotify watch for files closed after writing, or moved, into a directory. None off Linux, or when refused.
#[cfg(target_os = "linux")]
struct Watch(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl Watch {
    fn new(dir: &str) -> Option<Watch> {
        use std::os::fd::FromRawFd as _;
        // SAFETY: a plain inotify_init1; the descriptor is owned below.
        let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if raw < 0 {
            return None;
        }
        // SAFETY: the descriptor inotify_init1 just returned.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let path = std::ffi::CString::new(dir).ok()?;
        // SAFETY: a NUL-terminated path for inotify_add_watch on our own descriptor.
        let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO) };
        (wd >= 0).then_some(Watch(fd))
    }

    fn fd(&self) -> libc::c_int {
        self.0.as_raw_fd()
    }

    /// The events say nothing the files don't: read them all away.
    fn drain(&self) {
        let mut buf = [0u8; 4096];
        // SAFETY: reads into our own buffer from our own non-blocking descriptor, until it has no more.
        while unsafe { libc::read(self.fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) } > 0 {}
    }
}

#[cfg(not(target_os = "linux"))]
struct Watch;

#[cfg(not(target_os = "linux"))]
impl Watch {
    fn new(_dir: &str) -> Option<Watch> {
        None
    }

    fn fd(&self) -> libc::c_int {
        -1
    }

    fn drain(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handover is this host's when it was written since the host came, to the shell's 10 ms: one from before, one
    /// for another device, a malformed one or one with no origin to judge it by is none.
    #[test]
    fn a_handover_counts_only_for_the_host_that_came_before_it() {
        let h = Handover { handed_ms: 6850, hci_up_ms: 6820 };
        assert_eq!(parse("6850 6820 hci0\n", Some(4993), "hci0"), Some(h), "the bench's boot 1: the host at 4993");
        assert_eq!(parse("6850 6820 hci0", Some(6850), "hci0"), Some(h), "handed over as the host came");
        assert_eq!(parse("6850 6820 hci0", Some(6859), "hci0"), Some(h), "the shell's 10 ms: the same instant");
        assert_eq!(parse("6850 6820 hci0", Some(6860), "hci0"), None, "10 ms or more before the host: another's");
        assert_eq!(parse("6850 6820 hci0", Some(90_000), "hci0"), None, "an earlier host's");
        assert_eq!(parse("6850 6820 hci0", None, "hci0"), None, "no origin: nothing to judge it by");
        assert_eq!(parse("6850 6820 hci1", Some(4993), "hci0"), None, "another device");
        assert_eq!(parse("6850 6820", Some(4993), "hci0"), None, "no device");
        assert_eq!(parse("", Some(4993), "hci0"), None, "empty: a write under way");
        assert_eq!(parse("x 6820 hci0", Some(4993), "hci0"), None);
        assert_eq!(parse("6850 -1 hci0", Some(4993), "hci0"), None);
        assert_eq!(parse(&format!("{} 1 hci0", u64::MAX), Some(4993), "hci0").map(|h| h.hci_up_ms), Some(1), "no overflow");
    }

    /// The handover first, then bt_on's end; nothing while neither is there.
    #[test]
    fn the_handover_goes_before_bt_ons_end() {
        let fresh = Some("6850 6820 hci0");
        let stale = Some("1000 990 hci0");
        let h = Go::Handover(Handover { handed_ms: 6850, hci_up_ms: 6820 });
        assert_eq!(decide(fresh, Some(4993), false, "hci0"), Some(h));
        assert_eq!(decide(fresh, Some(4993), true, "hci0"), Some(h), "both there: the handover");
        assert_eq!(decide(stale, Some(4993), true, "hci0"), Some(Go::BtOn), "another host's handover: bt_on's end");
        assert_eq!(decide(stale, Some(4993), false, "hci0"), None, "another host's handover, bt_on still running: wait");
        assert_eq!(decide(None, Some(4993), false, "hci0"), None);
        assert_eq!(decide(None, None, true, "hci0"), Some(Go::BtOn), "bt_on's end needs no origin");
    }

    struct Scratch {
        dir: std::path::PathBuf,
        handover: String,
        done: String,
        origin: String,
    }

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("btd_handover_{tag}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let p = |n: &str| dir.join(n).to_str().unwrap().to_string();
            Scratch { handover: p("handover"), done: p("done"), origin: p("origin"), dir }
        }

        fn files(&self) -> Files<'_> {
            Files { handover: &self.handover, bt_on_done: &self.done, origin: &self.origin }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The wait takes the handover as it is written (here by a thread 100 ms on), and does not take one from before
    /// the host came, waiting on for bt_on's end instead.
    #[test]
    fn the_wait_goes_on_the_handover_or_on_bt_ons_end() {
        let s = Scratch::new("go");
        std::fs::write(&s.origin, "4993\n").unwrap();
        let stop = AtomicBool::new(false);
        let handover = s.handover.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(handover, "6850 6820 hci0\n").unwrap();
        });
        let t = Instant::now();
        let go = wait_on(&s.files(), &stop, "hci0", None, None, TICK);
        writer.join().unwrap();
        assert_eq!(go, Go::Handover(Handover { handed_ms: 6850, hci_up_ms: 6820 }));
        assert!(t.elapsed() >= Duration::from_millis(90) && t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());

        std::fs::write(&s.handover, "1000 990 hci0\n").unwrap();
        let done = s.done.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(done, "").unwrap();
        });
        let go = wait_on(&s.files(), &stop, "hci0", None, None, TICK);
        writer.join().unwrap();
        assert_eq!(go, Go::BtOn, "a handover from before the host came: bt_on's end");
    }

    /// SIGTERM's flag ends the wait at the next wake; the pipe wakes it at once, long before its backstop.
    #[test]
    fn a_sigterm_ends_the_wait() {
        let s = Scratch::new("term");
        let stop = AtomicBool::new(true);
        assert_eq!(wait_on(&s.files(), &stop, "hci0", None, None, TICK), Go::Exit, "already set: at once");

        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (rx, tx) = UnixStream::pair().unwrap();
        rx.set_nonblocking(true).unwrap();
        let flag = stop.clone();
        let signaller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
            use std::io::Write as _;
            (&tx).write_all(&[1]).unwrap();
        });
        let t = Instant::now();
        let go = wait_on(&s.files(), &stop, "hci0", Some(&rx), None, Duration::from_secs(30));
        signaller.join().unwrap();
        assert_eq!(go, Go::Exit);
        assert!(t.elapsed() < Duration::from_secs(5), "woken by the pipe, not the 30 s backstop: {:?}", t.elapsed());
    }

    /// A watch of the directory wakes the wait as a file there is closed after writing (Linux; elsewhere there is none
    /// and the looks are every TICK).
    #[test]
    fn a_write_in_the_watched_directory_wakes_the_wait() {
        let s = Scratch::new("watch");
        std::fs::write(&s.origin, "4993\n").unwrap();
        let Some(watch) = Watch::new(s.dir.to_str().unwrap()) else {
            assert!(!cfg!(target_os = "linux"), "inotify refused on Linux");
            return;
        };
        let stop = AtomicBool::new(false);
        let done = s.done.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(done, "").unwrap();
        });
        let t = Instant::now();
        let go = wait_on(&s.files(), &stop, "hci0", None, Some(&watch), Duration::from_secs(30));
        writer.join().unwrap();
        assert_eq!(go, Go::BtOn);
        assert!(t.elapsed() < Duration::from_secs(5), "woken by the watch, not the 30 s backstop: {:?}", t.elapsed());
    }

    /// Only `OCBM_BTD_WAIT=1` makes a btd wait (no other test reads this variable, so setting it here races nothing).
    #[test]
    fn only_the_supervisors_word_starts_a_waiting_btd() {
        assert!(HANDOVER.starts_with(DIR) && BT_ON_DONE.starts_with(DIR), "both in the watched directory");
        assert!(BACKSTOP > TICK);
        std::env::remove_var(WAIT_ENV);
        assert!(!requested(), "unset: today's btd");
        std::env::set_var(WAIT_ENV, "0");
        assert!(!requested());
        std::env::set_var(WAIT_ENV, "1");
        assert!(requested(), "the supervisor's early start");
        std::env::remove_var(WAIT_ENV);
    }
}
