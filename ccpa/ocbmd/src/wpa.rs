//! `ocbmd wpa [-p DIR] IFACE COMMAND [ARG...]`: one request to wpa_supplicant's control interface, its reply printed as
//! it came. The Wi-Fi link's station keeper (`/script/wifi_sta.sh`) talks to its supplicant through it, and the bench
//! by hand over the console (`ocbmd wpa sta0 status`, `scan`, `scan_results`, `list_networks`, `select_network 0`): the
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
//! `select_network 0`, `set_network 0 scan_freq 2437`, `disconnect`, `reconnect`). Where wpa_cli spells one otherwise
//! (`status verbose` is its `STATUS-VERBOSE`), the control command itself is typed (`status-verbose`).
//!
//! `find [FREQS] [KEPT]` (ADDENDUM 9; 11, 25 Nov 2026): where the hotspot is now, for the keeper's looks. FREQS is
//! SCAN's `freq=` list (`2437`, or ranges `2412-2462,5180-5240`; none: every channel), KEPT the file of the hotspot's
//! address the keeper keeps (`/etc/ocbm/wifi_link_bssid`). The hotspot is an entry of the supplicant's BSS table
//! (`BSS RANGE=` pages: id, address, freq, level, age, name) on a channel of FREQS, fresh, that carries the network's
//! name (GET_NETWORK 0 ssid), or whose name is hidden (empty, or zeros) and whose address is the kept one: an iPhone's
//! hotspot hides its name in its beacons whenever its hotspot screen is closed (the 25 Nov bench). First the table as
//! the supplicant has it: a sighting from its own scan of the last few seconds, or a join under way, answers the look
//! with no scan of ours (no scan meets the join it starts on those results). Else a scan of FREQS that probes for the
//! network by name (`ssid <hex>`, which a hidden hotspot answers) and for any (the wildcard), awaited (at most 10 s),
//! and the table again. Prints `seen <channel> <dBm>` per sighting, the strongest first, `hidden` after one known by
//! the kept address alone, or `seen 0`. When it sees the network by name and the kept address is not among the
//! sightings, it keeps the strongest named one's address in KEPT (0600; a new phone or a changed address is learnt
//! this way, and never locks the hotspot out: the supplicant joins by name). Nothing it prints comes from an address
//! or a name.
//!
//! `events SECS` (ADDENDUM 11): the supplicant's join events as they come, for SECS s (0: until it ends): [`events`].
//!
//! Exit status: 0 a reply came, whatever it says (as wpa_cli), `find`'s look ended, or `events` ran; 1 no supplicant
//! answered on `DIR/IFACE` within 10 s, the socket failed, or `find`'s scan failed or did not end (said on stderr);
//! 2 usage. Nothing goes to stdout but the reply. Run before ocbmd's single-instance lock, as `jffs2-extract` is: it
//! never touches the accessory, the daemon's files or its sockets.

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
/// `find`'s BSS table query: id, address, freq, level, age, name and the delimiter (wpa_ctrl.h `WPA_BSS_MASK_*`).
const BSS_MASK: &str = "0x21287";
/// `find`: a sighting this many s old in the supplicant's own results answers the look with no scan; while it joins,
/// [`JOIN_FRESH`] (its join's scan).
const RECENT: u64 = 4;
const JOIN_FRESH: u64 = 10;
/// `events`: repeats fold within this window; a join step under way is checked this often (STATUS); a quiet
/// supplicant is asked a PING this often, and has this long to answer it.
const WINDOW: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(250);
const PING_EVERY: Duration = Duration::from_secs(10);
const PING_WAIT: Duration = Duration::from_secs(2);
/// `events`: the most different lines shown in a window.
const MAX_SHOWN: usize = 16;
const HEX: &[u8; 16] = b"0123456789abcdef";

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
    let (path, client) = (Path::new(dir), Path::new(CLIENT_DIR));
    match words {
        [] => Some(usage()),
        // FREQS (digits, `-` and `,`), KEPT (a path); any other word is passed by (`quick`, the look's word until 11).
        [w, a @ ..] if w == "find" && a.len() <= 3 => {
            let freqs = a.iter().find(|w| w.starts_with(|c: char| c.is_ascii_digit())).map(String::as_str);
            let kept = a.iter().find(|w| w.starts_with('/')).map(String::as_str);
            Some(match find(path, iface, freqs, kept, client) {
                Ok(seen) => {
                    let _ = std::io::stdout().write_all(render(&seen).as_bytes());
                    0
                }
                Err(e) => {
                    eprintln!("ocbmd wpa: find on {dir}/{iface}: {e}");
                    1
                }
            })
        }
        [w, s] if w == "events" => {
            let Ok(secs) = s.parse() else { return Some(usage()) };
            Some(match events(path, iface, secs, client) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("ocbmd wpa: events on {dir}/{iface}: {e}");
                    1
                }
            })
        }
        _ => {
            let req = request(words);
            if req.len() > MAX_LEN {
                eprintln!("ocbmd wpa: the request is longer than the supplicant takes ({MAX_LEN} B)");
                return Some(2);
            }
            Some(match ask(path, iface, &req, client, REPLY_WAIT) {
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
    }
}

fn usage() -> i32 {
    eprintln!("usage: ocbmd wpa [-p DIR] IFACE COMMAND [ARG...] | find [FREQS] [KEPT] | events SECS");
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

    /// Whether an event came before `until` (kept in `events`); a stray reply is passed by.
    fn event(&mut self, until: Instant) -> std::io::Result<bool> {
        while let Some(d) = self.recv(until)? {
            if let Some(e) = event_text(&d) {
                self.events.push(e);
                return Ok(true);
            }
        }
        Ok(false)
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

/// STATUS's `wpa_state` ("?" when it has none).
fn state(status: &[u8]) -> &str {
    std::str::from_utf8(status).ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("wpa_state="))).unwrap_or("?")
}

/// A join under way: wpa_supplicant 2.11's states from AUTHENTICATING to GROUP_HANDSHAKE.
fn in_join(status: &[u8]) -> bool {
    matches!(state(status), "AUTHENTICATING" | "ASSOCIATING" | "ASSOCIATED" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE")
}

// ---- find -----------------------------------------------------------------------------------------------------------

/// A sighting: the channel, the level in dBm, and whether the hotspot was known by the kept address alone.
type Seen = (u32, i32, bool);

/// What `find` prints: `seen <channel> <dBm>[ hidden]` per sighting, or `seen 0`.
fn render(seen: &[Seen]) -> String {
    let mut o = if seen.is_empty() { "seen 0\n".to_string() } else { String::new() };
    for (ch, dbm, hidden) in seen {
        o.push_str(&format!("seen {ch} {dbm}{}\n", if *hidden { " hidden" } else { "" }));
    }
    o
}

fn find(
    dir: &Path,
    iface: &str,
    freqs: Option<&str>,
    kept: Option<&str>,
    client_dir: &Path,
) -> std::io::Result<Vec<Seen>> {
    let t0 = Instant::now();
    let mut c = Conn::open(dir, iface, client_dir)?;
    if !c.ask("ATTACH", REPLY_WAIT)?.starts_with(b"OK") {
        return Err(other("the supplicant took no monitor"));
    }
    let found = look(&mut c, freqs, kept, t0);
    let _ = c.ask("DETACH", Duration::from_secs(1));
    found
}

/// A join's end, for a look that found the supplicant busy (FAIL-BUSY).
fn join_end(e: &str) -> bool {
    ["CONNECTED", "DISCONNECTED", "ASSOC-REJECT", "AUTH-REJECT", "SSID-TEMP-DISABLED"]
        .iter()
        .any(|p| e.strip_prefix("CTRL-EVENT-").is_some_and(|r| r.starts_with(p)))
}

/// The look's SCAN (wpa_supplicant 2.11, ctrl_iface.c `wpas_ctrl_scan`): `freq=` takes the list (it is read up to its
/// last range); each `ssid ` term is a name in hex (even, at most 64 digits) up to the next space, an empty one the
/// wildcard, and the terms past what the driver takes in one scan are dropped from the end. So the network's name first
/// (the probe a hidden hotspot answers), then the wildcard (every network, the hotspot's beacon with its name hidden).
fn scan_request(freqs: Option<&str>, ssid: &[u8]) -> String {
    let mut r = String::from("SCAN ");
    if let Some(f) = freqs {
        r.push_str("freq=");
        r.push_str(f);
        r.push(' ');
    }
    r.push_str("ssid ");
    for b in ssid {
        r.push(char::from(HEX[usize::from(b >> 4)]));
        r.push(char::from(HEX[usize::from(b & 15)]));
    }
    r.push_str(" ssid ");
    r
}

fn look(c: &mut Conn, freqs: Option<&str>, kept_file: Option<&str>, t0: Instant) -> std::io::Result<Vec<Seen>> {
    let ssid = conf_ssid(&c.ask("GET_NETWORK 0 ssid", REPLY_WAIT)?).ok_or_else(|| other("no network 0"))?;
    let kept = kept_file.and_then(|p| std::fs::read_to_string(p).ok());
    let kept = kept.as_deref().map(str::trim).filter(|a| a.len() == 17);
    // The supplicant's own results first: what it heard in the last few s, or joins on, needs no scan of ours.
    let mut joining = in_join(&c.ask("STATUS", REPLY_WAIT)?);
    let mut t = table(c, &ssid, kept, if joining { JOIN_FRESH } else { RECENT }, freqs)?;
    if !joining && t.seen.is_empty() {
        // The scan's events from here on, those that came during the replies below included.
        let mut i = c.events.len();
        let r = c.ask(&scan_request(freqs, &ssid), REPLY_WAIT)?;
        // FAIL-BUSY: a scan under way (its end is awaited instead), or a join begun since (the table it joins from).
        let busy = r.starts_with(b"FAIL-BUSY");
        if !busy && !r.starts_with(b"OK") {
            return Err(other(&format!("scan refused ({})", String::from_utf8_lossy(&r).trim())));
        }
        joining = busy && in_join(&c.ask("STATUS", REPLY_WAIT)?);
        let until = Instant::now() + SCAN_WAIT;
        while !joining {
            if i == c.events.len() && !c.event(until)? {
                return Err(Error::new(ErrorKind::TimedOut, "no scan end in 10 s"));
            }
            let e = &c.events[i];
            i += 1;
            if e.starts_with("CTRL-EVENT-SCAN-FAILED") {
                return Err(other(&format!("the scan failed ({})", mask(e).unwrap_or_default())));
            }
            if e.starts_with("CTRL-EVENT-SCAN-RESULTS") || (busy && join_end(e)) {
                break;
            }
        }
        t = table(c, &ssid, kept, if joining { JOIN_FRESH } else { t0.elapsed().as_secs() + 2 }, freqs)?;
    }
    if let (Some(p), Some((_, a)), false) = (kept_file, &t.named, t.kept_heard) {
        keep_addr(p, a);
    }
    Ok(t.seen)
}

/// The hotspot in the BSS table: its sightings, the strongest one by name as (dBm, address), and whether the kept
/// address was among the sightings.
struct Table {
    seen: Vec<Seen>,
    named: Option<(i32, String)>,
    kept_heard: bool,
}

/// The network's entries no older than `fresh` s on a channel of `freqs` (this radio's, when none), by its name or,
/// the name hidden, by the `kept` address; page by page (a reply holds at most 4096 B: a page ends without `####` when
/// more follow).
fn table(c: &mut Conn, ssid: &[u8], kept: Option<&str>, fresh: u64, freqs: Option<&str>) -> std::io::Result<Table> {
    let mut t = Table { seen: Vec::new(), named: None, kept_heard: false };
    let mut from = 0u64;
    for _ in 0..16 {
        let page = c.ask(&format!("BSS RANGE={from}- MASK={BSS_MASK}"), REPLY_WAIT)?;
        let page = String::from_utf8_lossy(&page);
        let (mut id, mut freq, mut level, mut age, mut name) = (None, 0u32, 0i32, u64::MAX, None);
        let mut addr = String::new();
        let mut last = false;
        let mut next = None;
        for l in page.lines() {
            if l == "====" || l == "####" {
                let ch = chan_of(freq);
                let mine = kept.is_some_and(|k| k.eq_ignore_ascii_case(&addr));
                let named = name.as_deref() == Some(ssid);
                let hidden = !named && mine && name.as_deref().is_some_and(|n: &[u8]| n.iter().all(|&b| b == 0));
                if (named || hidden) && age <= fresh && ch != 0 && freqs.is_none_or(|f| in_freqs(freq, f)) {
                    t.kept_heard |= mine;
                    if named && t.named.as_ref().is_none_or(|(l, _)| level > *l) {
                        t.named = Some((level, addr.clone()));
                    }
                    // The strongest first (in place: a sort's code costs flash).
                    let at = t.seen.iter().position(|s| s.1 < level).unwrap_or(t.seen.len());
                    t.seen.insert(at, (ch, level, hidden));
                }
                next = id.map(|i: u64| i + 1);
                (id, freq, level, age, name) = (None, 0, 0, u64::MAX, None);
                addr.clear();
                last = l == "####";
            } else if let Some((k, v)) = l.split_once('=') {
                match k {
                    "id" => id = v.parse().ok(),
                    "bssid" => addr = v.to_string(),
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
    Ok(t)
}

/// `f` (MHz) within SCAN's `freq=` list: single frequencies and `a-b` ranges, comma-separated.
fn in_freqs(f: u32, list: &str) -> bool {
    list.split(',').any(|r| {
        let (a, b) = r.split_once('-').unwrap_or((r, r));
        a.parse().is_ok_and(|a: u32| a <= f) && b.parse().is_ok_and(|b: u32| f <= b)
    })
}

/// The hotspot's address kept in `path` (0600, through a temporary and a rename). Never printed.
fn keep_addr(path: &str, addr: &str) {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = format!("{path}.new");
    let w = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp);
    let w = w.and_then(|mut f| f.write_all(format!("{addr}\n").as_bytes()));
    if w.is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
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

/// One event as the keeper may log it: its name and numeric codes, nothing that came from an address or a name (a
/// quoted part goes before the arguments are read; only the arguments below, whose values are digits or capitals, are
/// kept). None: not one worth a line. The SME's own lines (sme.c) say its steps, and, at LEVEL 2 (wpa_msg MSG_DEBUG,
/// which this build's CONFIG_NO_STDOUT_DEBUG keeps), its 5 s timers.
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
            let (k, v) = t.split_once('=').unwrap_or((t, ""));
            let plain = v.bytes().all(|b| b.is_ascii_digit() || b == b'-' || b == b'_' || b.is_ascii_uppercase());
            // ASSOC-REJECT's bare `timeout` (no answer), or `timeout=` the step the driver's SME was at.
            let timeout = k == "timeout" && matches!(v, "" | "scan" | "auth" | "assoc");
            if timeout || (ARGS.contains(&k) && (1..=16).contains(&v.len()) && plain) {
                out.push(' ');
                out.push_str(t);
            }
        }
        return Some(out);
    }
    // A step's frequency: the last `freq=`, the event's own, after the quoted name.
    for (p, w) in [("SME: Trying to authenticate", "authenticating"), ("Trying to associate", "trying")] {
        if e.starts_with(p) {
            let digits = |r: &str| r.chars().take_while(char::is_ascii_digit).collect::<String>();
            let f = e.rsplit_once("freq=").map(|(_, r)| digits(r)).unwrap_or_default();
            return Some(if f.is_empty() { w.into() } else { format!("{w} freq={f}") });
        }
    }
    [
        ("Associated with", "associated"),
        // events.c wpa_supplicant_timeout: no key handshake within 10 s of the association.
        ("Authentication with", "the key handshake timed out"),
        ("WPA: 4-Way Handshake failed", "4-way handshake failed (the key?)"),
        // sme.c's own 5 s timers (sme_auth_timer, sme_assoc_timer), at LEVEL 2.
        ("SME: Authentication timeout", "no answer to the authentication in 5 s"),
        ("SME: Association timeout", "no answer to the association in 5 s"),
        ("SME: Authentication request to the driver failed", "the driver refused the authentication"),
        ("SME: Association request to the driver failed", "the driver refused the association"),
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

// ---- events ---------------------------------------------------------------------------------------------------------

/// `events SECS` (ADDENDUM 11, 25 Nov 2026): a monitor of the supplicant (ATTACH, LEVEL 2) for SECS s, or with 0 until
/// the supplicant ends (its CTRL-EVENT-TERMINATING, or no PONG to a PING after 10 s of silence). One line per event
/// [`mask`] keeps, `[wifi_sta] [up <s>] supplicant: <event>` (the keeper appends them to the box log as they come), the
/// hotspot not found (every scan while it is away) left out. A join's steps carry the time since its authentication
/// began, `(+N ms)`; a step that ends with no event of its own, as this build's supplicant ends one the driver timed
/// out (sme.c `sme_event_auth_timed_out` says nothing at MSG_INFO), is said with its time and the state it left for,
/// from STATUS every 250 ms while a step is under way. Bounded: within a minute each line once, then one line with how
/// often each came again.
fn events(dir: &Path, iface: &str, secs: u64, client_dir: &Path) -> std::io::Result<()> {
    let mut c = Conn::open(dir, iface, client_dir)?;
    if !c.ask("ATTACH", REPLY_WAIT)?.starts_with(b"OK") {
        return Err(other("the supplicant took no monitor"));
    }
    let _ = c.ask("LEVEL 2", REPLY_WAIT);
    let end = Instant::now() + Duration::from_secs(if secs == 0 { 1 << 31 } else { secs });
    let mut w = Watch { step: None, since: Instant::now(), keys: Vec::new(), again: Vec::new(), out: Vec::new() };
    let mut live = true;
    while live && Instant::now() < end {
        let wait = Instant::now() + if w.step.is_some() { POLL } else { PING_EVERY };
        if !c.event(wait.min(end))? && Instant::now() < end {
            if w.step.is_some() {
                let st = c.ask("STATUS", REPLY_WAIT)?;
                w.status(&st);
            } else {
                live = c.ask("PING", PING_WAIT).is_ok_and(|r| r.starts_with(b"PONG"));
            }
        }
        let evs = std::mem::take(&mut c.events);
        for e in &evs {
            live &= !e.starts_with("CTRL-EVENT-TERMINATING");
            w.event(e);
        }
        w.roll(!live || Instant::now() >= end);
        if !w.out.is_empty() {
            let up = std::fs::read_to_string("/proc/uptime").ok();
            let up = up.as_deref().and_then(|s| s.split(' ').next()).unwrap_or("?");
            for l in &w.out {
                live &= writeln!(std::io::stdout(), "[wifi_sta] [up {up}] supplicant: {l}").is_ok();
            }
            w.out.clear();
        }
    }
    let _ = c.ask("DETACH", Duration::from_secs(1));
    Ok(())
}

/// `events`' state: the join step under way (when its authentication began, which step), the window's lines and how
/// often each came again (two lists: no new generic code on flash), and the lines to print.
struct Watch {
    step: Option<(Instant, &'static str)>,
    since: Instant,
    keys: Vec<String>,
    again: Vec<u32>,
    out: Vec<String>,
}

impl Watch {
    fn event(&mut self, e: &str) {
        let Some(m) = mask(e) else { return };
        if m == "network-not-found" {
            return;
        }
        // A new attempt, or the network held back after a failure, while a step is under way: it ended unsaid.
        if m.starts_with("authenticating") || m.starts_with("ssid-temp-disabled") {
            self.silent("");
        }
        let line = match self.step {
            Some((t, _)) => format!("{m} (+{} ms)", t.elapsed().as_millis()),
            None => m.clone(),
        };
        const ENDS: [&str; 8] = [
            "connected", "auth-reject", "assoc-reject", "disconnected", "no answer", "the driver refused", "4-way",
            "the key handshake",
        ];
        self.step = if m.starts_with("authenticating") {
            Some((Instant::now(), "the authentication"))
        } else if m.starts_with("trying") {
            self.step.map(|(t, _)| (t, "the association"))
        } else if m.starts_with("associated") {
            self.step.map(|(t, _)| (t, "the key handshake"))
        } else if ENDS.iter().any(|p| m.starts_with(p)) {
            None
        } else {
            self.step
        };
        self.show(m, line);
    }

    /// STATUS while a step is under way: it has ended when the supplicant left the join's states (or after 20 s).
    fn status(&mut self, st: &[u8]) {
        if self.step.is_some_and(|(t, _)| !in_join(st) || t.elapsed() > Duration::from_secs(20)) {
            self.silent(state(st));
        }
    }

    /// The step under way ended with no event of its own (`now`: the state it left for, if known).
    fn silent(&mut self, now: &str) {
        if let Some((t, s)) = self.step.take() {
            let key = format!("no answer to {s}");
            let mut line = format!("{key} (+{} ms", t.elapsed().as_millis());
            if !now.is_empty() {
                line.push_str(", now ");
                line.push_str(now);
            }
            line.push(')');
            self.show(key, line);
        }
    }

    fn show(&mut self, key: String, line: String) {
        match self.keys.iter().position(|k| *k == key) {
            Some(i) => self.again[i] += 1,
            None if self.keys.len() < MAX_SHOWN => {
                self.keys.push(key);
                self.again.push(0);
                self.out.push(line);
            }
            None => {}
        }
    }

    /// At the window's end (or `now`): one line with the lines that came again in it, and a new window.
    fn roll(&mut self, now: bool) {
        if !now && self.since.elapsed() < WINDOW {
            return;
        }
        let mut a = String::new();
        for i in 0..self.keys.len() {
            if self.again[i] > 0 {
                a.push_str(if a.is_empty() { "" } else { ", " });
                a.push_str(&self.keys[i]);
                a.push_str(" x");
                a.push_str(&self.again[i].to_string());
            }
        }
        if !a.is_empty() {
            self.out.push(format!("again in the last {} s: {a}", self.since.elapsed().as_secs()));
        }
        self.keys.clear();
        self.again.clear();
        self.since = Instant::now();
    }
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
        assert_eq!(request(&words(&["select_network", "0"])), "SELECT_NETWORK 0");
        assert_eq!(request(&words(&["set_network", "0", "freq_list", "2437"])), "SET_NETWORK 0 freq_list 2437");
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
        let find = words(&["ocbmd", "wpa", "-p", "/nonexistent", "sta0", "find", "2437", "quick", "/etc/x"]);
        assert_eq!(cli(&find), Some(1), "find: nothing answers");
        assert_eq!(cli(&words(&["ocbmd", "wpa", "-p", "/nonexistent", "sta0", "events", "x"])), Some(2), "SECS");
        assert_eq!(cli(&words(&["ocbmd", "wpa", "-p", "/nonexistent", "sta0", "events", "1"])), Some(1));
    }

    // The hotspot of these tests: a made-up name with an apostrophe from outside ASCII, as phones name theirs.
    const NAME: &str = "Example\u{2019}s Hotspot";
    const NAME_TXT: &str = "Example\\xe2\\x80\\x99s Hotspot"; // as the supplicant prints it (printf_encode)
    // Made-up addresses (locally administered): the hotspot's, another network's.
    const ADDR: &str = "02:11:22:33:44:55";
    const OTHER: &str = "02:66:77:88:99:aa";

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

    /// ADDENDUM 11: the look's SCAN probes for the network by name (its hex, the grammar of 2.11's wpas_ctrl_scan),
    /// then for any network (the empty term), on the look's channels; `freq=`'s list is matched as the supplicant reads
    /// it.
    #[test]
    fn the_looks_scan_probes_for_the_hotspot_by_name_and_for_any() {
        assert_eq!(scan_request(Some("2437"), b"Ab\x00\xff"), "SCAN freq=2437 ssid 416200ff ssid ");
        assert_eq!(scan_request(None, b"x"), "SCAN ssid 78 ssid ", "no channels: every channel");
        let hex: String = NAME.bytes().map(|b| format!("{b:02x}")).collect();
        let r = scan_request(Some("2412-2462,5180-5240,5745-5825"), NAME.as_bytes());
        assert_eq!(r, format!("SCAN freq=2412-2462,5180-5240,5745-5825 ssid {hex} ssid "));
        assert_eq!(r.matches('=').count(), 1, "no other `key=` (bssid=, use_id=...) for the supplicant to find in it");
        let aps = "2412-2462,5180-5240,5745-5825";
        assert!(in_freqs(2437, aps) && in_freqs(2412, aps) && in_freqs(5825, aps) && in_freqs(5180, "5180"));
        assert!(!in_freqs(2467, aps) && !in_freqs(5500, aps) && !in_freqs(2437, "2412") && !in_freqs(2437, "x-y"));
    }

    #[test]
    fn events_keep_their_names_and_codes_and_lose_every_name_and_address() {
        let cases = [
            ("CTRL-EVENT-ASSOC-REJECT bssid=02:11:22:33:44:55 status_code=1", Some("assoc-reject status_code=1")),
            (
                "CTRL-EVENT-ASSOC-REJECT bssid=02:11:22:33:44:55 status_code=1 timeout=auth",
                Some("assoc-reject status_code=1 timeout=auth"),
            ),
            ("CTRL-EVENT-ASSOC-REJECT status_code=1 timeout", Some("assoc-reject status_code=1 timeout")),
            ("CTRL-EVENT-ASSOC-REJECT status_code=1 timeout=Example", Some("assoc-reject status_code=1")),
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
            (
                "SME: Trying to authenticate with 02:11:22:33:44:55 (SSID='freq=1 Example' freq=2437 MHz)",
                Some("authenticating freq=2437"),
            ),
            ("Trying to associate with 02:11:22:33:44:55 (SSID='freq=1234 X' freq=2437 MHz)", Some("trying freq=2437")),
            ("Trying to associate with SSID 'Example'", Some("trying")),
            ("Associated with 02:11:22:33:44:55", Some("associated")),
            ("Authentication with 02:11:22:33:44:55 timed out.", Some("the key handshake timed out")),
            ("WPA: 4-Way Handshake failed - pre-shared key may be wrong", Some("4-way handshake failed (the key?)")),
            ("SME: Authentication timeout", Some("no answer to the authentication in 5 s")),
            ("SME: Association timeout", Some("no answer to the association in 5 s")),
            ("SME: Authentication request to the driver failed", Some("the driver refused the authentication")),
            ("CTRL-EVENT-SCAN-RESULTS ", None),
            ("CTRL-EVENT-BSS-ADDED 3 02:11:22:33:44:55", None),
            ("Request association with 02:11:22:33:44:55", None),
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

    /// A stand-in supplicant for `find`: it answers ATTACH, GET_NETWORK, STATUS (`status`), SCAN (OK, or FAIL-BUSY when
    /// `busy`), the BSS pages and DETACH, and sends the scan's events to the monitor. `before` is the table before the
    /// look's scan, `after` after it, each one page or more.
    fn finder(
        dir: &Path,
        status: &'static str,
        busy: bool,
        before: Vec<String>,
        after: Vec<String>,
    ) -> std::thread::JoinHandle<Vec<String>> {
        let _ = std::fs::remove_file(dir.join("sta0")); // the last stand-in's, in a test that runs a few
        let s = UnixDatagram::bind(dir.join("sta0")).unwrap();
        std::thread::spawn(move || {
            let mut asked = Vec::new();
            let mut b = [0u8; MAX_LEN];
            let mut scanned = false;
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
                    "STATUS" => send(status),
                    "SCAN" if busy => {
                        send("FAIL-BUSY\n");
                        send("<3>CTRL-EVENT-SCAN-RESULTS ");
                        scanned = true;
                    }
                    "SCAN" => {
                        send("<3>CTRL-EVENT-SCAN-STARTED ");
                        send("OK\n");
                        send("<3>CTRL-EVENT-BSS-ADDED 0 02:11:22:33:44:55");
                        send("<3>CTRL-EVENT-SCAN-RESULTS ");
                        scanned = true;
                    }
                    "BSS" => {
                        let pages = if scanned { &after } else { &before };
                        let from: u64 = req.split(['=', '-']).nth(1).unwrap().parse().unwrap();
                        // The page that holds the id asked from (each page's first entry's id, as the test wrote it).
                        let p = pages.iter().find(|p| first_id(p) >= from).cloned().unwrap_or_else(|| "####\n".into());
                        send(&p);
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

    fn first_id(page: &str) -> u64 {
        page.lines().find_map(|l| l.strip_prefix("id=")).and_then(|v| v.parse().ok()).unwrap_or(u64::MAX)
    }

    fn entry(id: u32, addr: &str, freq: u32, level: i32, age: u32, name: &str) -> String {
        format!("id={id}\nbssid={addr}\nfreq={freq}\nlevel={level}\nage={age}\nssid={name}\n====\n")
    }

    /// The table's last page: its last delimiter is `####`.
    fn last(mut p: String) -> String {
        p.truncate(p.len() - 5);
        p + "####\n"
    }

    const APS: &str = "2412-2462,5180-5240,5745-5825";
    const IDLE: &str = "wpa_state=SCANNING\naddress=02:00:00:00:00:01\n";

    #[test]
    fn find_scans_by_name_and_reports_where_the_hotspot_was_heard_in_this_look_on_its_channels() {
        let dir = scratch("find");
        let kept = dir.join("kept");
        // Before the scan: the hotspot seen minutes ago only. After it, two pages (the first cut where the reply buffer
        // ended): another network, the hotspot heard now on 6, its old sighting on 149, a network named alike in part,
        // the hotspot on 36, and on a radar channel (outside the look's channels), then the table's last entry.
        let before = vec![last(entry(9, ADDR, 5745, -55, 300, NAME_TXT))];
        let p1 = entry(4, OTHER, 2412, -70, 0, "Other") + &entry(7, ADDR, 2437, -40, 1, NAME_TXT);
        let p1 = p1 + &entry(9, ADDR, 5745, -55, 300, NAME_TXT);
        let p2 = entry(10, OTHER, 2462, -60, 0, "Example") + &entry(12, OTHER, 5180, -80, 0, NAME_TXT);
        let p2 = last(p2 + &entry(13, OTHER, 5500, -30, 0, NAME_TXT));
        let sup = finder(&dir, IDLE, false, before, vec![p1, p2]);
        let t = Instant::now();
        let seen = find(&dir, "sta0", Some(APS), kept.to_str(), &dir).unwrap();
        let asked = sup.join().unwrap();
        assert_eq!(seen, vec![(6, -40, false), (36, -80, false)], "heard in this look, its channels, strongest first");
        let hex: String = NAME.bytes().map(|b| format!("{b:02x}")).collect();
        let scan = format!("SCAN freq={APS} ssid {hex} ssid ");
        let page0 = "BSS RANGE=0- MASK=0x21287";
        let want = ["ATTACH", "GET_NETWORK 0 ssid", "STATUS", page0, &scan, page0];
        assert_eq!(asked[..6], want, "the supplicant's own table first, then the scan by name");
        assert_eq!(asked[6..], ["BSS RANGE=10- MASK=0x21287", "DETACH"], "the next page from the id after the cut");
        assert!(t.elapsed() < Duration::from_secs(2), "no join waited for");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), format!("{ADDR}\n"), "the strongest named one's kept");
        let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&kept).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
        let out = render(&seen);
        assert_eq!(out, "seen 6 -40\nseen 36 -80\n");
        assert!(!dir.join(format!("ocbmd_wpa_{}", std::process::id())).exists(), "the client's socket is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ADDENDUM 10's FAULT 2: the hotspot's name hidden (its screen closed). Its beacon's entry, name empty or zeros,
    /// is the hotspot when its address is the kept one, on the look's channels; another hidden network is not.
    #[test]
    fn a_hidden_hotspot_is_known_by_its_kept_address() {
        let dir = scratch("hidden");
        let kept = dir.join("kept");
        std::fs::write(&kept, ADDR.to_ascii_uppercase() + "\n").unwrap();
        let p = entry(2, OTHER, 2412, -30, 0, "") + &entry(5, ADDR, 2437, -39, 0, "");
        let p = last(p + &entry(6, ADDR, 2462, -60, 0, "\\x00\\x00\\x00"));
        let sup = finder(&dir, IDLE, false, vec!["####\n".into()], vec![p]);
        let seen = find(&dir, "sta0", Some(APS), kept.to_str(), &dir).unwrap();
        sup.join().unwrap();
        assert_eq!(seen, vec![(6, -39, true), (11, -60, true)], "by the kept address, its name empty or zeros");
        assert_eq!(render(&seen), "seen 6 -39 hidden\nseen 11 -60 hidden\n");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), ADDR.to_ascii_uppercase() + "\n", "not rewritten");
        // No kept address: a hidden hotspot is not heard (as before ADDENDUM 11).
        let p = last(entry(5, ADDR, 2437, -39, 0, ""));
        let sup = finder(&dir, IDLE, false, vec!["####\n".into()], vec![p]);
        assert_eq!(find(&dir, "sta0", Some(APS), None, &dir).unwrap(), vec![]);
        sup.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The owner's rule (25 Nov): the kept address never locks the hotspot out. Heard by name under another address
    /// with the kept one not heard (a new phone, a changed address), the new one is kept; heard under both, the kept
    /// one stays (another device named alike nearby).
    #[test]
    fn a_hotspot_heard_by_name_under_another_address_replaces_the_kept_one() {
        let dir = scratch("moved");
        let kept = dir.join("kept");
        std::fs::write(&kept, format!("{ADDR}\n")).unwrap();
        let p = last(entry(3, OTHER, 2437, -45, 0, NAME_TXT));
        let sup = finder(&dir, IDLE, false, vec!["####\n".into()], vec![p]);
        assert_eq!(find(&dir, "sta0", Some("2437"), kept.to_str(), &dir).unwrap(), vec![(6, -45, false)]);
        sup.join().unwrap();
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), format!("{OTHER}\n"), "the new address kept");
        let near = ADDR.replace("55", "56"); // another device named alike nearby
        let p = last(entry(3, ADDR, 2437, -70, 0, "") + &entry(4, &near, 2437, -40, 0, NAME_TXT));
        std::fs::write(&kept, format!("{ADDR}\n")).unwrap();
        let sup = finder(&dir, IDLE, false, vec!["####\n".into()], vec![p]);
        assert_eq!(find(&dir, "sta0", None, kept.to_str(), &dir).unwrap(), vec![(6, -40, false), (6, -70, true)]);
        sup.join().unwrap();
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), format!("{ADDR}\n"), "the kept one heard too: it stays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ADDENDUM 11: a look never scans over a join, nor right after the supplicant's own scan heard the hotspot (the
    /// join it starts on those results): it answers from the supplicant's table.
    #[test]
    fn a_look_takes_the_supplicants_fresh_results_and_never_scans_over_a_join() {
        let dir = scratch("fresh");
        let fresh = last(entry(1, ADDR, 2437, -41, 2, NAME_TXT) + &entry(2, ADDR, 2462, -41, 2, NAME_TXT));
        let sup = finder(&dir, IDLE, false, vec![fresh], vec![]);
        assert_eq!(find(&dir, "sta0", Some("2437"), None, &dir).unwrap(), vec![(6, -41, false)], "its channel alone");
        assert!(!sup.join().unwrap().iter().any(|r| r.starts_with("SCAN")), "heard 2 s ago: no scan of ours");
        let stale = last(entry(1, ADDR, 2437, -41, 6, NAME_TXT));
        let sup = finder(&dir, "wpa_state=AUTHENTICATING\n", false, vec![stale], vec![]);
        assert_eq!(find(&dir, "sta0", None, None, &dir).unwrap(), vec![(6, -41, false)], "joining: the join's results");
        assert!(!sup.join().unwrap().iter().any(|r| r.starts_with("SCAN")), "a join under way: no scan of ours");
        let sup = finder(&dir, "wpa_state=4WAY_HANDSHAKE\n", false, vec!["####\n".into()], vec![]);
        assert_eq!(find(&dir, "sta0", None, None, &dir).unwrap(), vec![]);
        assert!(!sup.join().unwrap().iter().any(|r| r.starts_with("SCAN")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_on_a_busy_supplicant_reads_the_scan_under_way_and_says_seen_0_when_the_network_is_not_heard() {
        let dir = scratch("busy");
        let p = last(entry(1, OTHER, 2412, -70, 0, "Other"));
        let sup = finder(&dir, IDLE, true, vec!["####\n".into()], vec![p]);
        let seen = find(&dir, "sta0", None, None, &dir).unwrap();
        let asked = sup.join().unwrap();
        assert_eq!(seen, vec![]);
        assert_eq!(render(&seen), "seen 0\n");
        assert!(asked[4].starts_with("SCAN ssid "), "no channels given: every channel ({})", asked[4]);
        assert_eq!(asked[5], "STATUS", "busy: a join begun meanwhile? (no: the scan under way is awaited)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A join's story as `events` tells it: the steps with their times, a step that ends with no event of its own
    /// (the driver's timeout: this build's supplicant says nothing), repeats folded in a window, the hotspot's absence
    /// left out.
    #[test]
    fn events_tell_a_joins_steps_and_its_silent_end_and_fold_repeats() {
        let mut w = Watch { step: None, since: Instant::now(), keys: Vec::new(), again: Vec::new(), out: Vec::new() };
        w.event("CTRL-EVENT-NETWORK-NOT-FOUND ");
        assert!(w.out.is_empty(), "the hotspot away: the looks say so, not this");
        w.event("SME: Trying to authenticate with 02:11:22:33:44:55 (SSID='Example' freq=2437 MHz)");
        w.event("Trying to associate with 02:11:22:33:44:55 (SSID='Example' freq=2437 MHz)");
        assert!(w.step.is_some_and(|(_, s)| s == "the association"));
        w.status(b"wpa_state=ASSOCIATING\n");
        assert!(w.step.is_some(), "still joining");
        w.status(b"wpa_state=DISCONNECTED\n");
        assert!(w.step.is_none());
        assert_eq!(w.out[0], "authenticating freq=2437");
        assert!(w.out[1].starts_with("trying freq=2437 (+") && w.out[1].ends_with(" ms)"), "{}", w.out[1]);
        assert!(w.out[2].starts_with("no answer to the association (+"), "{}", w.out[2]);
        assert!(w.out[2].ends_with(" ms, now DISCONNECTED)"), "{}", w.out[2]);
        // A second attempt with no answer, ended by the next one and by the network held back: said once a window.
        w.event("SME: Trying to authenticate with 02:11:22:33:44:55 (SSID='Example' freq=2437 MHz)");
        w.event("SME: Trying to authenticate with 02:11:22:33:44:55 (SSID='Example' freq=2437 MHz)");
        w.event("CTRL-EVENT-SSID-TEMP-DISABLED id=0 ssid=\"Example\" auth_failures=1 duration=10 reason=CONN_FAILED");
        assert_eq!(w.out.len(), 5, "{:?}", w.out);
        assert!(w.out[3].starts_with("no answer to the authentication (+"), "{}", w.out[3]);
        assert_eq!(w.out[4], "ssid-temp-disabled auth_failures=1 duration=10 reason=CONN_FAILED");
        w.roll(true);
        let again = "again in the last 0 s: authenticating freq=2437 x2, no answer to the authentication x1";
        assert_eq!(w.out[5], again);
        // The supplicant's own 5 s timer, an explicit refusal and a join that completes: their own lines end the step.
        w.out.clear();
        w.event("SME: Trying to authenticate with 02:11:22:33:44:55 (SSID='Example' freq=2437 MHz)");
        w.event("SME: Authentication timeout");
        assert!(w.step.is_none() && w.out[1].starts_with("no answer to the authentication in 5 s"), "{:?}", w.out);
        w.event("SME: Trying to authenticate with 02:11:22:33:44:55 (SSID='Example' freq=2437 MHz)");
        w.event("CTRL-EVENT-AUTH-REJECT 02:11:22:33:44:55 auth_type=0 auth_transaction=2 status_code=17");
        assert!(w.step.is_none() && w.out[2].starts_with("auth-reject auth_type=0 auth_transaction=2 status_code=17"));
        w.event("SME: Trying to authenticate with 02:11:22:33:44:55 (SSID='Example' freq=2437 MHz)");
        w.event("Associated with 02:11:22:33:44:55");
        w.event("CTRL-EVENT-CONNECTED - Connection to 02:11:22:33:44:55 completed [id=0 id_str=]");
        assert!(w.step.is_none() && w.out.last().unwrap().starts_with("connected (+"), "{:?}", w.out);
        assert!(w.out.iter().all(|l| !l.contains("02:11") && !l.contains("Example")), "{:?}", w.out);
    }

    /// `events` as a run: it attaches at LEVEL 2, prints what it keeps, and ends at the supplicant's TERMINATING.
    #[test]
    fn events_attach_at_level_2_and_end_with_the_supplicant() {
        let dir = scratch("events");
        let s = UnixDatagram::bind(dir.join("sta0")).unwrap();
        let sup = std::thread::spawn(move || {
            let mut asked = Vec::new();
            let mut b = [0u8; MAX_LEN];
            while asked.len() < 3 {
                let (n, from) = s.recv_from(&mut b).unwrap();
                let to = from.as_pathname().unwrap().to_path_buf();
                asked.push(String::from_utf8_lossy(&b[..n]).into_owned());
                s.send_to(b"OK\n", &to).unwrap();
                if asked.len() == 2 {
                    s.send_to(b"<2>SME: Authentication timeout", &to).unwrap();
                    s.send_to(b"<3>CTRL-EVENT-TERMINATING ", &to).unwrap();
                }
            }
            asked
        });
        let t = Instant::now();
        events(&dir, "sta0", 30, &dir).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5), "ended with the supplicant, not at its 30 s");
        assert_eq!(sup.join().unwrap(), ["ATTACH", "LEVEL 2", "DETACH"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
