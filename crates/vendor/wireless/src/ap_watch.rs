//! The connect timeline's AP events (`box_common::timeline`), each stamped as its line lands: hostapd's own
//! `AP-ENABLED` (`t ap_enabled`) and `AP-STA-CONNECTED` (`t sta_assoc`), which it writes into `/tmp/box.log`
//! (`radio_ap_up.sh` runs it in the foreground into that file), and udhcpd's `sending ACK to` (`t dhcp_ack`), which
//! it writes into `/tmp/radio_ap_dhcp.log`. Those lines carry no time of their own.
//!
//! One thread per btd session, from the session's start until it has seen the phone's association and lease, a
//! minute has passed, or the session ends. Event-driven: inotify on the two files and the session's end through a
//! pipe; nothing polls, and between sessions nothing runs. It only reads those two files: hostapd's and udhcpd's
//! configuration and start-up are never touched.
//!
//! The first matching line of the session is the one stamped. Neither line can tell the phone btd serves from another
//! on the AP: the phone joins the AP under a private Wi-Fi address, not the one its 0x4E0E names (the 12 Nov bench's
//! iPhone showed two different addresses), and udhcpd names only an IP. A station's address is never kept: its last
//! two octets, as `..XX:YY`, are the most that reaches btd's log.

// Off Linux (a developer's host) there is no inotify: the watch does nothing there, and only the tests use the rest.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;

/// The file hostapd writes into (`radio_ap_up.sh`).
const BOX_LOG: &str = "/tmp/box.log";
/// The file udhcpd writes into (`radio_ap_up.sh`).
const DHCP_LOG: &str = "/tmp/radio_ap_dhcp.log";
/// How long a session's watch lasts at most.
const WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// The most read from a file at one wake; the rest at the next.
const READ_MAX: u64 = 64 * 1024;
/// The longest line kept whole; a longer one is cut (it is no line of hostapd's or udhcpd's).
const LINE_MAX: usize = 4096;

/// `@<unix_ms> [ap-watch] …`, as btd's other lines (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG).
fn log(m: &str) {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    println!("@{ms} [ap-watch] {m}");
}

/// What a line says, for the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    /// hostapd: the AP is enabled (`<iface>: AP-ENABLED`).
    ApEnabled,
    /// hostapd: a station associated and completed the key handshake (`<iface>: AP-STA-CONNECTED <address>`).
    StaConnected,
    /// udhcpd: a lease acknowledged (`udhcpd: sending ACK to <ip>`).
    DhcpAck,
}

impl Event {
    /// Its key in the timeline.
    fn key(self) -> &'static str {
        match self {
            Event::ApEnabled => "ap_enabled",
            Event::StaConnected => "sta_assoc",
            Event::DhcpAck => "dhcp_ack",
        }
    }
}

/// Whose file a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Writer {
    Hostapd,
    Udhcpd,
}

/// The event `line` from `writer`'s file tells, if any.
fn event(writer: Writer, line: &str) -> Option<Event> {
    match writer {
        Writer::Hostapd if line.contains("AP-STA-CONNECTED ") => Some(Event::StaConnected),
        Writer::Hostapd if line.contains("AP-ENABLED") => Some(Event::ApEnabled),
        Writer::Udhcpd if line.contains("sending ACK to ") => Some(Event::DhcpAck),
        _ => None,
    }
}

/// The last two octets of the station an `AP-STA-CONNECTED` line names, as `..XX:YY`: all of its address btd's log
/// may show.
fn station_tail(line: &str) -> Option<String> {
    let addr = line.split("AP-STA-CONNECTED ").nth(1)?.split_whitespace().next()?;
    let shaped = addr.len() == 17 && addr.bytes().enumerate().all(|(i, b)| if i % 3 == 2 { b == b':' } else { b.is_ascii_hexdigit() });
    shaped.then(|| format!("..{}", &addr[12..]))
}

/// One file followed from where it stood when the watch began: the lines appended since; from its start again when
/// it shrank (truncated, or emptied at its cap) or is another file now (replaced, or created since).
struct Follow {
    path: String,
    off: u64,
    ino: u64,
    partial: Vec<u8>,
}

impl Follow {
    /// `path` from its end now, or from its start once it exists.
    fn new(path: &str) -> Follow {
        let (off, ino) = std::fs::metadata(path).map_or((0, 0), |m| (m.len(), m.ino()));
        Follow { path: path.to_string(), off, ino, partial: Vec::new() }
    }

    /// The complete lines appended since the last call; a line still being written waits for the next.
    fn lines(&mut self) -> Vec<String> {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return Vec::new() };
        let Ok(m) = f.metadata() else { return Vec::new() };
        if m.ino() != self.ino || m.len() < self.off {
            self.ino = m.ino();
            self.off = 0;
            self.partial.clear();
        }
        let want = m.len().saturating_sub(self.off).min(READ_MAX);
        if want == 0 || f.seek(SeekFrom::Start(self.off)).is_err() {
            return Vec::new();
        }
        let mut chunk = Vec::new();
        let Ok(n) = f.take(want).read_to_end(&mut chunk) else { return Vec::new() };
        self.off += n as u64;
        let mut out = Vec::new();
        for piece in chunk.split_inclusive(|&b| b == b'\n') {
            let room = LINE_MAX.saturating_sub(self.partial.len());
            let body = piece.strip_suffix(b"\n").unwrap_or(piece);
            self.partial.extend_from_slice(&body[..body.len().min(room)]);
            if piece.ends_with(b"\n") {
                out.push(String::from_utf8_lossy(&self.partial).into_owned());
                self.partial.clear();
            }
        }
        out
    }
}

/// What a session's watch saw, for its closing line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Seen {
    ap_enabled: bool,
    /// The station's tail (`..XX:YY`), or "" when its line named none.
    sta: Option<String>,
    dhcp_ack: bool,
}

impl Seen {
    /// Note one line; the event it is, when it is one this watch had not seen yet (the first stands).
    fn note(&mut self, writer: Writer, line: &str) -> Option<Event> {
        let e = event(writer, line)?;
        let new = match e {
            Event::ApEnabled => !std::mem::replace(&mut self.ap_enabled, true),
            Event::StaConnected => {
                let first = self.sta.is_none();
                if first {
                    self.sta = Some(station_tail(line).unwrap_or_default());
                }
                first
            }
            Event::DhcpAck => !std::mem::replace(&mut self.dhcp_ack, true),
        };
        new.then_some(e)
    }

    /// Whether the phone is on the AP: associated and leased. Its AP was enabled before that, so nothing is left.
    fn done(&self) -> bool {
        self.sta.is_some() && self.dhcp_ack
    }

    /// The watch's closing line: why it ended, after how long, and what it saw.
    fn summary(&self, why: &str, secs: f64) -> String {
        let yes = |b: bool| if b { "seen" } else { "not seen" };
        let sta = match &self.sta {
            Some(t) if !t.is_empty() => format!("seen ({t})"),
            Some(_) => "seen".to_string(),
            None => "not seen".to_string(),
        };
        format!(
            "ended ({why}) after {secs:.1} s: AP-ENABLED {}, AP-STA-CONNECTED {sta}, DHCP ACK {}",
            yes(self.ap_enabled),
            yes(self.dhcp_ack)
        )
    }
}

/// A session's watch; dropping it ends the watch (the session's end) and waits for its thread.
pub struct ApWatch {
    /// The write end of the stop pipe: closing it wakes the thread.
    stop: Option<std::os::fd::OwnedFd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ApWatch {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Start this session's watch. On a host without inotify it does nothing.
pub fn start() -> ApWatch {
    #[cfg(target_os = "linux")]
    {
        linux::start()
    }
    #[cfg(not(target_os = "linux"))]
    {
        ApWatch { stop: None, thread: None }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    pub(super) fn start() -> ApWatch {
        let none = ApWatch { stop: None, thread: None };
        let mut ends = [0 as libc::c_int; 2];
        // SAFETY: a two-int array for pipe2 to fill.
        if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            log(&format!("no watch this session: pipe2 failed ({})", std::io::Error::last_os_error()));
            return none;
        }
        // SAFETY: two descriptors pipe2 just opened, owned from here.
        let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
        match std::thread::Builder::new().name("ap-watch".into()).spawn(move || watch(rd)) {
            Ok(t) => ApWatch { stop: Some(wr), thread: Some(t) },
            Err(e) => {
                log(&format!("no watch this session: {e}"));
                none
            }
        }
    }

    /// A watch on `path` in `ino` for writes (IN_MODIFY covers a truncation too); a file not there yet fails, and is
    /// asked for again at the next wake. Asking again for a file already watched changes nothing.
    fn add(ino: &OwnedFd, path: &str) {
        if let Ok(c) = std::ffi::CString::new(path) {
            // SAFETY: a NUL-terminated path for inotify_add_watch on our own descriptor.
            unsafe { libc::inotify_add_watch(ino.as_raw_fd(), c.as_ptr(), libc::IN_MODIFY) };
        }
    }

    fn watch(stop: OwnedFd) {
        let started = std::time::Instant::now();
        // SAFETY: a plain inotify_init1; the descriptor is owned below.
        let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if raw < 0 {
            log(&format!("no watch this session: inotify_init1 failed ({})", std::io::Error::last_os_error()));
            return;
        }
        // SAFETY: the descriptor inotify_init1 just returned.
        let ino = unsafe { OwnedFd::from_raw_fd(raw) };
        // Where the files stand first, then the watches, then one read: a line between the two is read then, not lost.
        let mut hostapd = Follow::new(BOX_LOG);
        let mut udhcpd = Follow::new(DHCP_LOG);
        add(&ino, BOX_LOG);
        add(&ino, DHCP_LOG);
        let mut seen = Seen::default();
        let mut why = "a minute passed";
        let mut events = [0u8; 4096];
        loop {
            let at = box_common::timeline::now_ms();
            for (writer, follow) in [(Writer::Hostapd, &mut hostapd), (Writer::Udhcpd, &mut udhcpd)] {
                for line in follow.lines() {
                    if let (Some(e), Some(ms)) = (seen.note(writer, &line), at) {
                        crate::startup::stamp_at(e.key(), ms);
                    }
                }
            }
            if seen.done() {
                why = "the phone is on the AP";
                break;
            }
            let left = WINDOW.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            let mut fds = [
                libc::pollfd { fd: ino.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: stop.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            // SAFETY: two pollfds of our own descriptors.
            let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, left.as_millis().min(i32::MAX as u128) as libc::c_int) };
            if r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                why = "poll failed";
                break;
            }
            if fds[1].revents != 0 {
                why = "the session ended";
                break;
            }
            // The events themselves say nothing the files don't: drain them, then read both files.
            // SAFETY: reads into our own buffer from our own non-blocking descriptor, until it has no more.
            while unsafe { libc::read(ino.as_raw_fd(), events.as_mut_ptr() as *mut libc::c_void, events.len()) } > 0 {}
            // The DHCP log is created when udhcpd starts, and either file may have been replaced since.
            add(&ino, BOX_LOG);
            add(&ino, DHCP_LOG);
        }
        log(&seen.summary(why, started.elapsed().as_secs_f64()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lines_the_watch_takes() {
        let h = Writer::Hostapd;
        assert_eq!(event(h, "sta0: AP-ENABLED "), Some(Event::ApEnabled));
        assert_eq!(event(h, "sta0: AP-STA-CONNECTED 02:00:00:00:00:0a"), Some(Event::StaConnected));
        assert_eq!(event(h, "sta0: STA 02:00:00:00:00:0a IEEE 802.11: associated (aid 1)"), None, "not yet keyed");
        assert_eq!(event(h, "sta0: AP-STA-DISCONNECTED 02:00:00:00:00:0a"), None);
        assert_eq!(event(h, "sta0: AP-DISABLED "), None);
        assert_eq!(event(h, "udhcpd: sending ACK to 192.168.43.100"), None, "udhcpd's line, in hostapd's file");
        let u = Writer::Udhcpd;
        assert_eq!(event(u, "udhcpd: sending ACK to 192.168.43.100"), Some(Event::DhcpAck));
        assert_eq!(event(u, "udhcpd: sending OFFER to 192.168.43.100"), None);
        assert_eq!(event(u, "udhcpd: sending NAK to 192.168.43.100"), None);
        assert_eq!(event(u, "sta0: AP-ENABLED "), None);
        assert_eq!(Event::StaConnected.key(), "sta_assoc");
    }

    #[test]
    fn a_station_is_told_by_its_last_two_octets_only() {
        assert_eq!(station_tail("sta0: AP-STA-CONNECTED 02:00:00:00:00:0a").as_deref(), Some("..00:0a"));
        assert_eq!(station_tail("wlan0: AP-STA-CONNECTED 02:00:00:00:00:0A p2p_dev_addr=x").as_deref(), Some("..00:0A"));
        assert_eq!(station_tail("sta0: AP-STA-CONNECTED garbage"), None);
        assert_eq!(station_tail("sta0: AP-STA-CONNECTED"), None);
        assert_eq!(station_tail("sta0: AP-STA-CONNECTED 02:00:00:00:00:0ax"), None);
    }

    /// The first of each stands, and the watch is done once the phone is associated and leased.
    #[test]
    fn the_first_of_each_stands_and_the_lease_ends_the_watch() {
        let mut seen = Seen::default();
        assert_eq!(seen.note(Writer::Hostapd, "sta0: AP-ENABLED "), Some(Event::ApEnabled));
        assert_eq!(seen.note(Writer::Hostapd, "sta0: AP-ENABLED "), None, "the first stands");
        assert_eq!(seen.note(Writer::Hostapd, "sta0: AP-STA-CONNECTED 02:00:00:00:00:0a"), Some(Event::StaConnected));
        assert_eq!(seen.note(Writer::Hostapd, "sta0: AP-STA-CONNECTED 02:00:00:00:00:0b"), None);
        assert!(!seen.done(), "no lease yet");
        assert_eq!(seen.note(Writer::Udhcpd, "udhcpd: sending ACK to 192.168.43.100"), Some(Event::DhcpAck));
        assert!(seen.done());
        assert_eq!(
            seen.summary("the phone is on the AP", 23.44),
            "ended (the phone is on the AP) after 23.4 s: AP-ENABLED seen, AP-STA-CONNECTED seen (..00:0a), DHCP ACK seen"
        );
        let mut late = Seen::default();
        late.note(Writer::Hostapd, "sta0: AP-STA-CONNECTED ?");
        late.note(Writer::Udhcpd, "udhcpd: sending ACK to 192.168.43.100");
        assert!(late.done(), "AP-ENABLED before the session began: not waited for");
        assert_eq!(
            late.summary("the phone is on the AP", 3.0),
            "ended (the phone is on the AP) after 3.0 s: AP-ENABLED not seen, AP-STA-CONNECTED seen, DHCP ACK seen"
        );
        assert_eq!(
            Seen::default().summary("a minute passed", 60.0),
            "ended (a minute passed) after 60.0 s: AP-ENABLED not seen, AP-STA-CONNECTED not seen, DHCP ACK not seen"
        );
    }

    /// A file followed from where it stood: the lines appended since, a line still being written at the next read,
    /// and from its start again once it shrank, was replaced, or appeared.
    #[test]
    fn a_file_is_followed_across_truncation_and_replacement() {
        let dir = std::env::temp_dir().join(format!("btd_ap_watch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("box.log");
        let p = path.to_str().unwrap();
        let append = |s: &str| {
            use std::io::Write as _;
            std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap().write_all(s.as_bytes()).unwrap();
        };

        let mut f = Follow::new(p);
        assert!(f.lines().is_empty(), "no file yet");
        append("before the watch\n");
        assert_eq!(f.lines(), ["before the watch"], "created since: read from its start");

        append("before the second watch\n");
        let mut f = Follow::new(p);
        append("sta0: AP-ENA");
        assert!(f.lines().is_empty(), "only what came after it began, and a line still being written waits");
        append("BLED \nsta0: AP-STA-CONNECTED 02:00:00:00:00:0a\n");
        assert_eq!(f.lines(), ["sta0: AP-ENABLED ", "sta0: AP-STA-CONNECTED 02:00:00:00:00:0a"]);
        assert!(f.lines().is_empty());

        std::fs::OpenOptions::new().write(true).truncate(true).open(&path).unwrap();
        append("udhcpd: started\n");
        assert_eq!(f.lines(), ["udhcpd: started"], "truncated (udhcpd's `>`, ocbmd's cap): from the start");

        std::fs::remove_file(&path).unwrap();
        append("a new file, longer than the old one was by now: from its start all the same\n");
        assert_eq!(f.lines(), ["a new file, longer than the old one was by now: from its start all the same"]);

        append(&format!("{}\nnext\n", "x".repeat(LINE_MAX + 10)));
        let got = f.lines();
        assert_eq!((got[0].len(), got[1].as_str()), (LINE_MAX, "next"), "a long line is cut, the next one whole");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_watch_ends_when_it_is_dropped() {
        let t = std::time::Instant::now();
        drop(start());
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "the session's end wakes the thread at once");
    }
}
