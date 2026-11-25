//! `ocbmd wpa [-p DIR] IFACE COMMAND [ARG...]`: one request to wpa_supplicant's control interface, its reply printed as
//! it came. The Wi-Fi link's station keeper (`/script/wifi_sta.sh`) talks to its supplicant through it, and the bench
//! by hand over the console (`ocbmd wpa sta0 status`, `scan`, `scan_results`, `list_networks`, `enable_network 0`): the
//! adapter carries no wpa_cli, about 68 KB of flash an update could not spare (24 Nov 2026).
//!
//! wpa_supplicant 2.11's UNIX control interface, as its own client library and server do it (`src/common/wpa_ctrl.c`
//! `wpa_ctrl_open2` / `wpa_ctrl_request`, `wpa_supplicant/ctrl_iface_unix.c`): a datagram socket at `DIR/IFACE`, DIR
//! being the configuration's `ctrl_interface` (the keeper writes `/var/run/wpa_supplicant`, the default here). The
//! client binds a socket of its own, which the supplicant answers with `sendto` to the address it came from,
//! connects to `DIR/IFACE` and sends the request text; one datagram comes back: `OK\n`, `FAIL\n`, `UNKNOWN COMMAND\n`
//! or the command's text, at most 4096 B. A datagram that begins with `<` or `IFNAME=` is an event for an attached
//! monitor, not the reply, and is passed by. wpa_ctrl waits 10 s for the reply, and so does this.
//!
//! The client's socket (ADDENDUM 9, 24 Nov 2026: one `/tmp/ocbmd_wpa_<pid>` was found left behind, a request killed
//! mid-way): on Linux an abstract name, `\0ocbmd_wpa_<pid>`, which is no file, so nothing is left whatever ends the run
//! (SIGTERM, SIGKILL, a panic); elsewhere (the Mac's tests), or should the kernel refuse the name, a file in /tmp, as
//! wpa_ctrl binds, removed when the run ends by itself.
//!
//! The request is the command's first word in capitals and the rest as given, one space between: wpa_cli's own
//! spelling for every command the keeper and the bench use (`status`, `scan`, `scan_results`, `list_networks`,
//! `enable_network 0`, `disconnect`, `reconnect`, `reconfigure`). Where wpa_cli spells one otherwise (`status verbose`
//! is its `STATUS-VERBOSE`), the control command itself is typed (`status-verbose`).
//!
//! `find [FREQS] [quick]` (ADDENDUM 9): where the hotspot is now, for the keeper's looks. A monitor of the
//! supplicant's events for its few seconds (ATTACH), a scan of FREQS (SCAN's `freq=` list: `2437`, or ranges
//! `2412-2462,5180-5240`; none: every channel) or the one under way, awaited (at most 10 s), then the network's name
//! (GET_NETWORK 0 ssid) matched in the BSS table (`BSS RANGE=` pages: id, freq, level, age, ssid), what the look did
//! not hear left out. Prints `seen <channel> <dBm>` per match, the strongest first, or `seen 0`; then, when the
//! supplicant tried to join on the scan (not followed with `quick`), `event ...` lines: event names and numeric codes
//! only ([`mask`]). Nothing it prints comes from a BSSID or an SSID.
//!
//! Exit status: 0 a reply came, whatever it says (as wpa_cli), or `find`'s scan ended; 1 no supplicant answered on
//! `DIR/IFACE` within 10 s, the socket failed, or `find`'s scan failed or did not end (said on stderr); 2 usage.
//! Nothing goes to stdout but the reply. Run before ocbmd's single-instance lock, as `jffs2-extract` is: it never
//! touches the accessory, the daemon's files or its sockets.

use std::io::{Error, ErrorKind, Write};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The supplicant's control directory as the station keeper writes it (`ctrl_interface=`).
const CTRL_DIR: &str = "/var/run/wpa_supplicant";
/// Where a client socket that is a file is bound: wpa_ctrl's `CONFIG_CTRL_IFACE_CLIENT_DIR`.
const CLIENT_DIR: &str = "/tmp";
/// wpa_ctrl_request's wait for the reply.
const REPLY_WAIT: Duration = Duration::from_secs(10);
/// How long a send may wait on a full queue: wpa_ctrl_request retries one for 5 s.
const SEND_WAIT: Duration = Duration::from_secs(5);
/// The supplicant's request limit (`CTRL_IFACE_MAX_LEN`): a longer request is dropped unanswered. Also the reply
/// buffer's size.
const MAX_LEN: usize = 8192;
/// `find`: the longest wait for its scan's end (a scan of every channel, the radar ones listened to, takes a few s).
const SCAN_WAIT: Duration = Duration::from_secs(10);
/// `find`: a join the supplicant starts on the scan is followed while its events come at most this far apart...
const JOIN_QUIET: Duration = Duration::from_secs(1);
/// ...and for this long in all.
const JOIN_WATCH: Duration = Duration::from_secs(4);
/// `find`'s BSS table query: id, freq, level, age, ssid and the delimiter (wpa_ctrl.h `WPA_BSS_MASK_*`).
const BSS_MASK: &str = "0x21285";
/// The most events `find` prints (the last ones).
const MAX_EVENTS: usize = 8;

/// The subcommand, when `args` asks for it: the exit status. None: ocbmd runs as the daemon.
pub fn cli(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some("wpa") {
        return None;
    }
    let mut rest = &args[2..];
    let mut dir = CTRL_DIR;
    if rest.first().map(String::as_str) == Some("-p") {
        let Some(d) = rest.get(1) else { return Some(usage()) };
        dir = d;
        rest = &rest[2..];
    }
    let [iface, words @ ..] = rest else { return Some(usage()) };
    if words.is_empty() {
        return Some(usage());
    }
    if words[0] == "find" && words.len() <= 3 {
        // `quick`: no join followed after the scan (the keeper's first look, which an AP's start may wait for).
        let quick = words[1..].iter().any(|w| w == "quick");
        let freqs = words[1..].iter().find(|w| *w != "quick").map(String::as_str);
        return Some(match find(Path::new(dir), iface, freqs, quick, Path::new(CLIENT_DIR)) {
            Ok(f) => {
                let _ = std::io::stdout().write_all(render(&f).as_bytes());
                0
            }
            Err(e) => {
                eprintln!("ocbmd wpa: find on {dir}/{iface}: {e}");
                1
            }
        });
    }
    let req = request(words);
    if req.len() > MAX_LEN {
        eprintln!("ocbmd wpa: the request is longer than the supplicant takes ({MAX_LEN} B)");
        return Some(2);
    }
    Some(match ask(Path::new(dir), iface, &req, Path::new(CLIENT_DIR), REPLY_WAIT) {
        Ok(reply) => {
            let _ = std::io::stdout().write_all(&reply);
            0
        }
        Err(e) => {
            eprintln!("ocbmd wpa: no answer from {dir}/{iface}: {e}");
            1
        }
    })
}

fn usage() -> i32 {
    eprintln!("usage: ocbmd wpa [-p CTRL_DIR] IFACE COMMAND [ARG...] | find [FREQS] [quick]");
    2
}

/// The request text: the command's first word in capitals, the rest as given, one space between (wpa_cli's write_cmd).
fn request(words: &[String]) -> String {
    let mut r = words[0].to_ascii_uppercase();
    for w in &words[1..] {
        r.push(' ');
        r.push_str(w);
    }
    r
}

/// The client socket's file, where it has one, removed when the run is over.
struct Unlink(PathBuf);

impl Drop for Unlink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn other(what: &str) -> Error {
    Error::other(what.to_string())
}

/// A connection to the supplicant on `dir/iface`: the client's socket (see the module's doc), and the monitor events
/// that came while it waited for replies.
struct Conn {
    sock: UnixDatagram,
    _file: Option<Unlink>,
    buf: Vec<u8>,
    events: Vec<String>,
}

impl Conn {
    fn open(dir: &Path, iface: &str, client_dir: &Path) -> std::io::Result<Conn> {
        let name = format!("ocbmd_wpa_{}", std::process::id());
        #[cfg(target_os = "linux")]
        let bound = {
            use std::os::linux::net::SocketAddrExt;
            std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
                .and_then(|a| UnixDatagram::bind_addr(&a))
                .ok()
                .map(|s| (s, None))
        };
        #[cfg(not(target_os = "linux"))]
        let bound: Option<(UnixDatagram, Option<Unlink>)> = None;
        let (sock, _file) = match bound {
            Some(b) => b,
            None => {
                let me = client_dir.join(&name);
                // One an earlier run with this pid left (killed mid-request): wpa_ctrl removes its own the same way.
                let _ = std::fs::remove_file(&me);
                (UnixDatagram::bind(&me)?, Some(Unlink(me)))
            }
        };
        // Connected: only the supplicant's own datagrams are read.
        sock.connect(dir.join(iface))?;
        sock.set_write_timeout(Some(SEND_WAIT))?;
        Ok(Conn { sock, _file, buf: vec![0u8; MAX_LEN], events: Vec::new() })
    }

    /// The next datagram before `until`; None when none came.
    fn recv(&mut self, until: Instant) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.sock.set_read_timeout(Some(left))?;
            match self.sock.recv(&mut self.buf) {
                Ok(n) => return Ok(Some(self.buf[..n].to_vec())),
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// One request and its reply within `wait`; events that come meanwhile are kept (wpa_ctrl_request passes them by).
    fn ask(&mut self, req: &str, wait: Duration) -> std::io::Result<Vec<u8>> {
        self.sock.send(req.as_bytes())?;
        let end = Instant::now() + wait;
        while let Some(d) = self.recv(end)? {
            match event_text(&d) {
                Some(e) => self.events.push(e),
                None => return Ok(d),
            }
        }
        Err(Error::new(ErrorKind::TimedOut, format!("no reply in {} s", wait.as_secs())))
    }

    /// The next event before `until` (kept too); a stray reply is passed by.
    fn event(&mut self, until: Instant) -> std::io::Result<Option<String>> {
        while let Some(d) = self.recv(until)? {
            if let Some(e) = event_text(&d) {
                self.events.push(e.clone());
                return Ok(Some(e));
            }
        }
        Ok(None)
    }
}

/// A monitor's datagram (`<3>CTRL-EVENT-…`, or `IFNAME=sta0 <3>…`) as its text, the prefixes off; None: a reply.
fn event_text(d: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(d);
    let s = match s.strip_prefix("IFNAME=") {
        Some(r) => r.split_once(' ').map_or("", |x| x.1),
        None => &s,
    };
    let (_, text) = s.strip_prefix('<')?.split_once('>')?;
    Some(text.trim().to_string())
}

/// One request to the supplicant on `dir/iface` and its reply, a client's socket that is a file bound in `client_dir`.
fn ask(dir: &Path, iface: &str, req: &str, client_dir: &Path, wait: Duration) -> std::io::Result<Vec<u8>> {
    Conn::open(dir, iface, client_dir)?.ask(req, wait)
}

// ---- find -----------------------------------------------------------------------------------------------------------

/// `find`'s result: the network's sightings in the look as (channel, dBm), the strongest first, and the masked events.
#[derive(Debug, Default, PartialEq)]
struct Found {
    seen: Vec<(u32, i32)>,
    events: Vec<String>,
}

/// What `find` prints: `seen <channel> <dBm>` per sighting or `seen 0`, then `event <masked>` per event.
fn render(f: &Found) -> String {
    let mut o = if f.seen.is_empty() { "seen 0\n".to_string() } else { String::new() };
    for (ch, dbm) in &f.seen {
        o.push_str(&format!("seen {ch} {dbm}\n"));
    }
    for e in &f.events {
        o.push_str(&format!("event {e}\n"));
    }
    o
}

fn find(dir: &Path, iface: &str, freqs: Option<&str>, quick: bool, client_dir: &Path) -> std::io::Result<Found> {
    let t0 = Instant::now();
    let mut c = Conn::open(dir, iface, client_dir)?;
    if !c.ask("ATTACH", REPLY_WAIT)?.starts_with(b"OK") {
        return Err(other("the supplicant took no monitor"));
    }
    let found = look(&mut c, freqs, quick, t0);
    let _ = c.ask("DETACH", Duration::from_secs(1));
    found
}

/// A join's end, for a look that found the supplicant joining (FAIL-BUSY) and for the watch after a scan.
fn join_end(e: &str) -> bool {
    ["CONNECTED", "DISCONNECTED", "ASSOC-REJECT", "AUTH-REJECT", "SSID-TEMP-DISABLED"]
        .iter()
        .any(|p| e.strip_prefix("CTRL-EVENT-").is_some_and(|r| r.starts_with(p)))
}

fn look(c: &mut Conn, freqs: Option<&str>, quick: bool, t0: Instant) -> std::io::Result<Found> {
    let ssid = conf_ssid(&c.ask("GET_NETWORK 0 ssid", REPLY_WAIT)?).ok_or_else(|| other("no network 0"))?;
    let r = c.ask(&freqs.map_or("SCAN".to_string(), |f| format!("SCAN freq={f}")), REPLY_WAIT)?;
    // FAIL-BUSY: a scan under way (its end is awaited instead) or a join (its outcome, then the table it has).
    let busy = r.starts_with(b"FAIL-BUSY");
    if !busy && !r.starts_with(b"OK") {
        return Err(other(&format!("scan refused ({})", String::from_utf8_lossy(&r).trim())));
    }
    let until = Instant::now() + SCAN_WAIT;
    let mut ended = false;
    while let Some(e) = c.event(until)? {
        if e.starts_with("CTRL-EVENT-SCAN-FAILED") {
            return Err(other(&format!("the scan failed ({})", mask(&e).unwrap_or_default())));
        }
        if e.starts_with("CTRL-EVENT-SCAN-RESULTS") || (busy && join_end(&e)) {
            ended = true;
            break;
        }
    }
    if !ended {
        return Err(Error::new(ErrorKind::TimedOut, "no scan end in 10 s"));
    }
    let mut seen = sightings(c, &ssid, t0.elapsed().as_secs() + 2)?;
    seen.sort_by(|a, b| b.1.cmp(&a.1));
    // A join the supplicant starts on these results (it tries at once), followed to its outcome: CONNECTED, or quiet
    // after a failure. None tried within 300 ms, or `quick`: no wait.
    let watch_end = Instant::now() + if quick { Duration::ZERO } else { JOIN_WATCH };
    let tried = c.events.iter().any(|e| e.starts_with("Trying to associate"));
    let mut quiet = if tried { JOIN_QUIET } else { Duration::from_millis(300) };
    while let Some(e) = c.event((Instant::now() + quiet).min(watch_end))? {
        if e.starts_with("CTRL-EVENT-CONNECTED") {
            break;
        }
        if e.starts_with("Trying to associate") {
            quiet = JOIN_QUIET;
        }
    }
    let mut events: Vec<String> = Vec::new();
    for m in c.events.iter().filter_map(|e| mask(e)) {
        if events.last() != Some(&m) {
            events.push(m);
        }
    }
    let skip = events.len().saturating_sub(MAX_EVENTS);
    Ok(Found { seen, events: events.split_off(skip) })
}

/// The network's entries in the supplicant's BSS table no older than `fresh` s, as (channel, dBm), page by page (a
/// reply holds at most 4096 B: a page ends without `####` when more follow).
fn sightings(c: &mut Conn, ssid: &[u8], fresh: u64) -> std::io::Result<Vec<(u32, i32)>> {
    let mut seen = Vec::new();
    let mut from = 0u64;
    for _ in 0..16 {
        let page = c.ask(&format!("BSS RANGE={from}- MASK={BSS_MASK}"), REPLY_WAIT)?;
        let page = String::from_utf8_lossy(&page);
        let (mut id, mut freq, mut level, mut age, mut name) = (None, 0u32, 0i32, u64::MAX, None);
        let mut last = false;
        let mut next = None;
        for l in page.lines() {
            if l == "====" || l == "####" {
                if name.as_deref() == Some(ssid) && age <= fresh && chan_of(freq) != 0 {
                    seen.push((chan_of(freq), level));
                }
                next = id.map(|i: u64| i + 1);
                (id, freq, level, age, name) = (None, 0, 0, u64::MAX, None);
                last = l == "####";
            } else if let Some((k, v)) = l.split_once('=') {
                match k {
                    "id" => id = v.parse().ok(),
                    "freq" => freq = v.parse().unwrap_or(0),
                    "level" => level = v.parse().unwrap_or(0),
                    "age" => age = v.parse().unwrap_or(u64::MAX),
                    "ssid" => name = Some(unescape(v)),
                    _ => {}
                }
            }
        }
        match next {
            Some(n) if !last && n > from => from = n,
            _ => break,
        }
    }
    Ok(seen)
}

/// A frequency in MHz as its channel; 0 for one this radio's AP and station never use.
fn chan_of(f: u32) -> u32 {
    match f {
        2484 => 14,
        2412..=2472 => (f - 2407) / 5,
        5000..=5900 => (f - 5000) / 5,
        _ => 0,
    }
}

/// GET_NETWORK's `ssid`: `"<the bytes>"` or their hex (wpa_config_write_string); None for anything else (FAIL).
fn conf_ssid(r: &[u8]) -> Option<Vec<u8>> {
    let r = r.strip_suffix(b"\n").unwrap_or(r);
    if r.len() >= 2 && r[0] == b'"' && r[r.len() - 1] == b'"' {
        return Some(r[1..r.len() - 1].to_vec());
    }
    if r.is_empty() || r.len() % 2 == 1 {
        return None;
    }
    r.chunks(2).map(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()).collect()
}

/// An SSID as the supplicant prints it (printf_encode: `\"`, `\\`, `\e`, `\n`, `\r`, `\t`, `\xNN`) back to its bytes.
fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            let one = match b[i + 1] {
                b'e' => Some(0x1b),
                b'n' => Some(b'\n'),
                b'r' => Some(b'\r'),
                b't' => Some(b'\t'),
                b'"' | b'\\' => Some(b[i + 1]),
                _ => None,
            };
            if let Some(v) = one {
                out.push(v);
                i += 2;
                continue;
            }
            let hex = b.get(i + 2..i + 4).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
            if let (b'x', Some(v)) = (b[i + 1], hex) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// One event as the keeper may log it: its name and numeric codes, nothing that came from a BSSID or an SSID (a
/// quoted part goes before the arguments are read; only the arguments below, whose values are digits or capitals, are
/// kept). None: not one worth a line.
fn mask(e: &str) -> Option<String> {
    if let Some(rest) = e.strip_prefix("CTRL-EVENT-") {
        let (name, args) = rest.split_once(' ').unwrap_or((rest, ""));
        const EVENTS: [&str; 9] = [
            "ASSOC-REJECT", "AUTH-REJECT", "DISCONNECTED", "CONNECTED", "SSID-TEMP-DISABLED", "SSID-REENABLED",
            "NETWORK-NOT-FOUND", "SCAN-FAILED", "CHANNEL-SWITCH",
        ];
        const ARGS: [&str; 10] = [
            "status_code", "reason", "locally_generated", "auth_failures", "duration", "auth_type", "auth_transaction",
            "ret", "freq", "ch_width",
        ];
        if !EVENTS.contains(&name) {
            return None;
        }
        let mut out = name.to_ascii_lowercase();
        for t in unquoted(args).split_whitespace() {
            let Some((k, v)) = t.split_once('=') else { continue };
            let plain = v.bytes().all(|b| b.is_ascii_digit() || b == b'-' || b == b'_' || b.is_ascii_uppercase());
            if ARGS.contains(&k) && (1..=16).contains(&v.len()) && plain {
                out.push(' ');
                out.push_str(t);
            }
        }
        return Some(out);
    }
    if e.starts_with("Trying to associate") {
        // The last `freq=`: the event's own, after the quoted SSID.
        let digits = |r: &str| r.chars().take_while(char::is_ascii_digit).collect::<String>();
        let f = e.rsplit_once("freq=").map(|(_, r)| digits(r)).unwrap_or_default();
        return Some(if f.is_empty() { "trying".into() } else { format!("trying freq={f}") });
    }
    [
        ("Associated with", "associated"),
        ("Authentication with", "authentication timed out"),
        ("WPA: 4-Way Handshake failed", "4-way handshake failed (the key?)"),
    ]
    .iter()
    .find(|(p, _)| e.starts_with(p))
    .map(|(_, w)| w.to_string())
}

/// `s` without its quoted parts (`"..."` and `'...'`, a backslash escaping the next character inside one).
fn unquoted(s: &str) -> String {
    let (mut out, mut q, mut esc) = (String::new(), None, false);
    for c in s.chars() {
        match q {
            None if c == '"' || c == '\'' => q = Some(c),
            None => out.push(c),
            Some(_) if esc => esc = false,
            Some(_) if c == '\\' => esc = true,
            Some(qc) if c == qc => q = None,
            Some(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    /// A scratch directory of the test's own, short enough for a socket path (104 B on macOS).
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ocbmd_wpa_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A stand-in supplicant on `dir/sta0`: each request it reads is noted, `events` go first, then `reply`.
    fn supplicant(
        dir: &Path,
        events: &'static [&'static [u8]],
        reply: &'static [u8],
    ) -> std::thread::JoinHandle<Vec<u8>> {
        let s = UnixDatagram::bind(dir.join("sta0")).unwrap();
        std::thread::spawn(move || {
            let mut b = [0u8; MAX_LEN];
            let (n, from) = s.recv_from(&mut b).unwrap();
            let to = from.as_pathname().expect("the client binds a named socket, the one answered").to_path_buf();
            for e in events {
                s.send_to(e, &to).unwrap();
            }
            s.send_to(reply, &to).unwrap();
            b[..n].to_vec()
        })
    }

    #[test]
    fn the_request_is_wpa_clis_spelling() {
        assert_eq!(request(&words(&["status"])), "STATUS");
        assert_eq!(request(&words(&["scan_results"])), "SCAN_RESULTS");
        assert_eq!(request(&words(&["list_networks"])), "LIST_NETWORKS");
        assert_eq!(request(&words(&["enable_network", "0"])), "ENABLE_NETWORK 0");
        assert_eq!(request(&words(&["STATUS-VERBOSE"])), "STATUS-VERBOSE");
        assert_eq!(request(&words(&["status-verbose"])), "STATUS-VERBOSE");
        let args = words(&["set_network", "0", "ssid", "\"MiXed\""]);
        assert_eq!(request(&args), "SET_NETWORK 0 ssid \"MiXed\"", "arguments as given");
    }

    #[test]
    fn a_request_goes_to_dir_iface_and_its_reply_comes_back_past_monitor_events() {
        let dir = scratch("reply");
        let events: &[&[u8]] = &[b"<3>CTRL-EVENT-SCAN-STARTED ", b"IFNAME=sta0 <3>CTRL-EVENT-SCAN-RESULTS "];
        let sup = supplicant(&dir, events, b"wpa_state=COMPLETED\nfreq=5180\n");
        let got = ask(&dir, "sta0", "STATUS", &dir, Duration::from_secs(5)).unwrap();
        assert_eq!(got, b"wpa_state=COMPLETED\nfreq=5180\n");
        assert_eq!(sup.join().unwrap(), b"STATUS");
        assert!(!dir.join(format!("ocbmd_wpa_{}", std::process::id())).exists(), "the client's socket is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_supplicant_or_no_reply_is_an_error_not_a_reply() {
        let dir = scratch("none");
        assert!(ask(&dir, "sta0", "STATUS", &dir, Duration::from_secs(5)).is_err(), "no socket at dir/iface");
        // A socket that never answers: the wait ends it.
        let _mute = UnixDatagram::bind(dir.join("sta0")).unwrap();
        let t = Instant::now();
        let e = ask(&dir, "sta0", "STATUS", &dir, Duration::from_millis(300)).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_wpa_subcommand_is_taken_and_a_short_one_is_usage() {
        assert_eq!(cli(&words(&["ocbmd"])), None);
        assert_eq!(cli(&words(&["ocbmd", "jffs2-extract", "a", "b"])), None);
        assert_eq!(cli(&words(&["ocbmd", "wpa"])), Some(2));
        assert_eq!(cli(&words(&["ocbmd", "wpa", "sta0"])), Some(2));
        assert_eq!(cli(&words(&["ocbmd", "wpa", "-p"])), Some(2));
        assert_eq!(cli(&words(&["ocbmd", "wpa", "-p", "/nonexistent"])), Some(2));
        let none = words(&["ocbmd", "wpa", "-p", "/nonexistent", "sta0", "status"]);
        assert_eq!(cli(&none), Some(1), "nothing answers there");
        let find = words(&["ocbmd", "wpa", "-p", "/nonexistent", "sta0", "find"]);
        assert_eq!(cli(&find), Some(1), "find: nothing answers");
    }

    // The hotspot of these tests: a made-up name with an apostrophe from outside ASCII, as phones name theirs.
    const NAME: &str = "Example\u{2019}s Hotspot";
    const NAME_TXT: &str = "Example\\xe2\\x80\\x99s Hotspot"; // as the supplicant prints it (printf_encode)

    #[test]
    fn names_are_read_back_from_the_supplicants_two_spellings() {
        assert_eq!(unescape(NAME_TXT), NAME.as_bytes());
        assert_eq!(unescape(r#"a\"b\\c\ed\n\r\tz\x41"#), b"a\"b\\c\x1bd\n\r\tzA");
        assert_eq!(unescape(r"end\"), b"end\\", "a lone backslash at the end stays");
        assert_eq!(unescape(r"\xZZ"), b"\\xZZ", "no hex after \\x: as it is");
        let hex: String = NAME.bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(conf_ssid(format!("{hex}\n").as_bytes()).unwrap(), NAME.as_bytes(), "GET_NETWORK's hex form");
        assert_eq!(conf_ssid(b"\"a \"quoted\" one\"").unwrap(), b"a \"quoted\" one", "its quoted form, as it is");
        assert_eq!(conf_ssid(b"FAIL\n"), None);
        assert_eq!(conf_ssid(b"abc"), None);
        let chans = [2412, 2437, 2462, 2484, 5180, 5745].map(chan_of);
        assert_eq!(chans, [1, 6, 11, 14, 36, 149]);
        assert_eq!(chan_of(5955), 0, "6 GHz: never this radio's");
    }

    #[test]
    fn events_keep_their_names_and_codes_and_lose_every_name_and_address() {
        let cases = [
            ("CTRL-EVENT-ASSOC-REJECT bssid=02:11:22:33:44:55 status_code=1", Some("assoc-reject status_code=1")),
            (
                "CTRL-EVENT-AUTH-REJECT 02:11:22:33:44:55 auth_type=0 auth_transaction=2 status_code=17",
                Some("auth-reject auth_type=0 auth_transaction=2 status_code=17"),
            ),
            (
                "CTRL-EVENT-DISCONNECTED bssid=02:11:22:33:44:55 reason=3 locally_generated=1",
                Some("disconnected reason=3 locally_generated=1"),
            ),
            ("CTRL-EVENT-CONNECTED - Connection to 02:11:22:33:44:55 completed [id=0 id_str=]", Some("connected")),
            (
                concat!(
                    "CTRL-EVENT-SSID-TEMP-DISABLED id=0 ssid=\"reason=SECRET_NAME \\\" status_code=9\" ",
                    "auth_failures=2 duration=20 reason=CONN_FAILED"
                ),
                Some("ssid-temp-disabled auth_failures=2 duration=20 reason=CONN_FAILED"),
            ),
            ("CTRL-EVENT-SSID-REENABLED id=0 ssid=\"Example\"", Some("ssid-reenabled")),
            ("CTRL-EVENT-NETWORK-NOT-FOUND ", Some("network-not-found")),
            ("CTRL-EVENT-SCAN-FAILED ret=-16 retry=1", Some("scan-failed ret=-16")),
            (
                "CTRL-EVENT-CHANNEL-SWITCH freq=2437 ht_enabled=1 ch_offset=0 ch_width=20 MHz cf1=2437 cf2=0",
                Some("channel-switch freq=2437 ch_width=20"),
            ),
            ("Trying to associate with 02:11:22:33:44:55 (SSID='freq=1234 X' freq=2437 MHz)", Some("trying freq=2437")),
            ("Trying to associate with SSID 'Example'", Some("trying")),
            ("Associated with 02:11:22:33:44:55", Some("associated")),
            ("Authentication with 02:11:22:33:44:55 timed out.", Some("authentication timed out")),
            ("WPA: 4-Way Handshake failed - pre-shared key may be wrong", Some("4-way handshake failed (the key?)")),
            ("CTRL-EVENT-SCAN-RESULTS ", None),
            ("CTRL-EVENT-BSS-ADDED 3 02:11:22:33:44:55", None),
            ("WPA: Key negotiation completed with 02:11:22:33:44:55 [PTK=CCMP GTK=CCMP]", None),
        ];
        for (e, want) in cases {
            let got = mask(e);
            assert_eq!(got.as_deref(), want, "{e}");
            let g = got.unwrap_or_default();
            assert!(!g.contains("02:11") && !g.contains("Example") && !g.contains("SECRET"), "{g}");
        }
        assert_eq!(event_text(b"<3>CTRL-EVENT-SCAN-RESULTS ").as_deref(), Some("CTRL-EVENT-SCAN-RESULTS"));
        assert_eq!(event_text(b"IFNAME=sta0 <3>Associated with x").as_deref(), Some("Associated with x"));
        assert_eq!(event_text(b"OK\n"), None, "a reply");
    }

    /// A stand-in supplicant for `find`: it answers ATTACH, GET_NETWORK, SCAN (OK, or FAIL-BUSY when `busy`), the BSS
    /// pages and DETACH, sends the scan's events to the monitor, and after the last page the join's `join` events.
    fn finder(dir: &Path, busy: bool, pages: Vec<String>, join: Vec<String>) -> std::thread::JoinHandle<Vec<String>> {
        let s = UnixDatagram::bind(dir.join("sta0")).unwrap();
        std::thread::spawn(move || {
            let mut asked = Vec::new();
            let mut b = [0u8; MAX_LEN];
            let mut pages = pages.into_iter();
            loop {
                let (n, from) = s.recv_from(&mut b).unwrap();
                let to = from.as_pathname().unwrap().to_path_buf();
                let req = String::from_utf8_lossy(&b[..n]).into_owned();
                asked.push(req.clone());
                let send = |d: &str| {
                    s.send_to(d.as_bytes(), &to).unwrap();
                };
                match req.split(' ').next().unwrap() {
                    "ATTACH" => send("OK\n"),
                    "GET_NETWORK" => send(&NAME.bytes().map(|b| format!("{b:02x}")).collect::<String>()),
                    "SCAN" if busy => {
                        send("FAIL-BUSY\n");
                        send("<3>CTRL-EVENT-SCAN-RESULTS ");
                    }
                    "SCAN" => {
                        send("<3>CTRL-EVENT-SCAN-STARTED ");
                        send("OK\n");
                        send("<3>CTRL-EVENT-BSS-ADDED 0 02:11:22:33:44:55");
                        send("<3>CTRL-EVENT-SCAN-RESULTS ");
                    }
                    "BSS" => {
                        let p = pages.next().unwrap_or_default();
                        send(&p);
                        if p.ends_with("####\n") {
                            for e in &join {
                                send(e);
                            }
                        }
                    }
                    "DETACH" => {
                        send("OK\n");
                        return asked;
                    }
                    _ => send("UNKNOWN COMMAND\n"),
                }
            }
        })
    }

    fn entry(id: u32, freq: u32, level: i32, age: u32, name: &str) -> String {
        format!("id={id}\nfreq={freq}\nlevel={level}\nage={age}\nssid={name}\n====\n")
    }

    #[test]
    fn find_scans_the_given_channels_and_reports_where_the_network_was_heard_and_what_its_join_did() {
        let dir = scratch("find");
        // Two pages (the first cut where the reply buffer ended): another network, the hotspot heard now on 6 and
        // seen minutes ago on 149, another network named alike in part, then the table's last entry.
        let p1 = entry(4, 2412, -70, 0, "Other") + &entry(7, 2437, -40, 1, NAME_TXT);
        let p1 = p1 + &entry(9, 5745, -55, 300, NAME_TXT);
        let mut p2 = entry(10, 2462, -60, 0, "Example") + &entry(12, 5180, -80, 0, NAME_TXT);
        p2.truncate(p2.len() - 5);
        p2.push_str("####\n");
        let join = vec![
            format!("<3>Trying to associate with 02:11:22:33:44:55 (SSID='{NAME_TXT}' freq=2437 MHz)"),
            "<3>CTRL-EVENT-ASSOC-REJECT bssid=02:11:22:33:44:55 status_code=1".to_string(),
            format!(
                "<3>CTRL-EVENT-SSID-TEMP-DISABLED id=0 ssid=\"{NAME_TXT}\" {}",
                "auth_failures=1 duration=10 reason=CONN_FAILED"
            ),
        ];
        let sup = finder(&dir, false, vec![p1, p2], join);
        let t = Instant::now();
        let f = find(&dir, "sta0", Some("2412-2462,5180-5240,5745-5825"), false, &dir).unwrap();
        let asked = sup.join().unwrap();
        assert_eq!(f.seen, vec![(6, -40), (36, -80)], "heard in this look only, the strongest first");
        assert_eq!(f.events, vec![
            "trying freq=2437".to_string(),
            "assoc-reject status_code=1".to_string(),
            "ssid-temp-disabled auth_failures=1 duration=10 reason=CONN_FAILED".to_string(),
        ]);
        assert_eq!(asked[..3], ["ATTACH", "GET_NETWORK 0 ssid", "SCAN freq=2412-2462,5180-5240,5745-5825"]);
        let pages = ["BSS RANGE=0- MASK=0x21285", "BSS RANGE=10- MASK=0x21285", "DETACH"];
        assert_eq!(asked[3..], pages, "the next page from the id after the cut");
        assert!(t.elapsed() < Duration::from_secs(3), "the join followed until quiet, not to the 4 s bound");
        let out = render(&f);
        assert_eq!(out.lines().next(), Some("seen 6 -40"));
        assert!(!out.contains("Example") && !out.contains("02:11") && !out.contains("\\x"), "{out}");
        assert!(!dir.join(format!("ocbmd_wpa_{}", std::process::id())).exists(), "the client's socket is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quick_find_reports_what_it_heard_and_follows_no_join() {
        let dir = scratch("quick");
        let mut p = entry(3, 2437, -45, 0, NAME_TXT);
        p.truncate(p.len() - 5);
        p.push_str("####\n");
        let join = vec!["<3>Trying to associate with 02:11:22:33:44:55 (SSID='x' freq=2437 MHz)".to_string()];
        let sup = finder(&dir, false, vec![p], join);
        let t = Instant::now();
        let f = find(&dir, "sta0", Some("2437"), true, &dir).unwrap();
        sup.join().unwrap();
        assert_eq!(f, Found { seen: vec![(6, -45)], events: vec![] }, "heard on 6; the join not waited for");
        assert!(t.elapsed() < Duration::from_millis(900));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_on_a_busy_supplicant_reads_the_scan_under_way_and_says_seen_0_when_the_network_is_not_heard() {
        let dir = scratch("busy");
        let mut p = entry(1, 2412, -70, 0, "Other");
        p.truncate(p.len() - 5);
        p.push_str("####\n");
        let sup = finder(&dir, true, vec![p], vec![]);
        let f = find(&dir, "sta0", None, true, &dir).unwrap();
        let asked = sup.join().unwrap();
        assert_eq!(f, Found::default());
        assert_eq!(render(&f), "seen 0\n");
        assert_eq!(asked[2], "SCAN", "no channels given: every channel");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
