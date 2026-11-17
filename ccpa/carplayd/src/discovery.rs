//! CarPlay receiver discovery + connect-out — advertise `_airplay._tcp`, browse `_carplay-ctrl._tcp`,
//! and dial the iPhone's control port so it turns around and opens RTSP to our `:5000`.
//!
//! WAS A SEPARATE BINARY (`rx-connect`), merged here 2026-09-08. The two were never independent:
//! the supervisor launched them on consecutive lines and `kill_session` killed them together, and
//! `btd`'s wireless path spawned and pkilled them as a pair. What forced the merge is not lifecycle
//! but IDENTITY — the advertised `deviceid`/`pi` MUST equal the ones the pairing server verifies
//! with, or pair-verify fails. Two processes deriving the same values independently from the same
//! env vars is a correctness hazard with no upside; here they are function calls against the same
//! `OnceLock`s, so they cannot disagree.
//!
//! Merging also dropped tokio entirely. Everything below is a connect with a timeout, a write, a
//! read with a timeout, and a sleep — `std::net` does all four, and `mdns-sd` is sync already
//! (flume + polling + socket2, no async runtime). One fewer ~307 KB musl/std floor, one fewer
//! process in the double-spawn race, and one heap arena gone.
//!
//! Bearer selection is unchanged and still env-driven, so ONE build serves both:
//!   RX_IFACE  (default "ncm0")  — interface whose index scopes IPv6 link-local dials, unless another
//!                                 interface holds RX_ADDR (below).
//!   RX_ADDR   (default unset)   — explicit advertised address; unset means addr-auto (all
//!                                 interface addresses). Wireless AP: RX_IFACE=wlan0
//!                                 RX_ADDR=192.168.43.1. The interface holding it scopes link-local
//!                                 dials: btd's `wlan0` is not the AP's name on every chip.
//!
//! STARTED EARLY (A2). The wireless carplayd starts as btd's host session begins, before any phone is named and often
//! before the AP has its address, so that it is advertising and browsing when the phone joins. Three things follow:
//! - mdns-sd reads the host's interfaces when its daemon starts and then only every 30 s (`IP_CHECK_INTERVAL_MILLIS`),
//!   so an AP address that comes later is told to it at once ([`Browser::interfaces_changed`], from the address watch:
//!   it re-reads them and announces `_airplay._tcp` again), and the browse is started again so its first query goes
//!   out there ([`Net`]);
//! - the loop sleeps until mDNS, btd's name file (inotify) or an address change wakes it: nothing polls;
//! - the phone btd names is dialled only once btd has sent it its 0x5703 (the name file's mark, `phone_gate::Dial`):
//!   its record is held until then, and dialled at once when the mark comes. Before any phone is named, nobody is.
//!
//! FINDING THE PHONE. The loop relies on the phone's own announcement and the browse's own queries, and sends no extra
//! ones: mdns-sd caches an unsolicited announcement and resolves it with no query outstanding (`handle_response` takes
//! the PTR, SRV and TXT answers and the A/AAAA additionals in one packet), a phone joining a network announces its
//! records (twice, RFC 6762 §8.3) once it has probed them, and it answers nothing for them while it probes (§8.1),
//! which a query cadence would only spend packets on. A browse started again (an address came up, the name changed)
//! replays mdns-sd's cache and queries at once, then 1, 2, 4 ... s later.

mod watch;

use crate::phone_gate::{Dial, PhoneGate};
use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::CStr;
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV6, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PORT: u16 = 5000;

/// The phone's CarPlay control service, which discovery browses and dials.
const CTRL_SERVICE: &str = "_carplay-ctrl._tcp.local.";

fn mac_to_dec(mac: &str) -> u64 {
    mac.split(':').fold(0u64, |v, p| {
        (v << 8) | u64::from_str_radix(p, 16).unwrap_or(0)
    })
}

/// Dial the iPhone's CarPlay-control port: `GET /ctrl-int/1/connect` → the iPhone turns around and
/// opens RTSP to our advertised `:5000`. Returns whether the dial was accepted, so a transient
/// failure is retried rather than deduping the peer forever.
fn connect_out(addr: SocketAddr, dev_dec: u64) -> bool {
    // Bound the connect: a stale/unroutable address — a phone addr that lagged the mDNS resolve, or
    // an IPv6 link-local whose scope has since changed — must not block this single browse thread
    // for the OS default connect timeout (minutes), which would stall ALL discovery. 3s is generous
    // on a local AP subnet. `connect_timeout` is the std equivalent of the tokio timeout this
    // replaced; it needs a single SocketAddr, which is exactly what `dialable` produces.
    let mut s = match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
        Ok(s) => s,
        Err(e) => {
            println!("[rx] connect-out {addr} failed: {e}");
            return false;
        }
    };
    let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let req = format!(
        "GET /ctrl-int/1/connect HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: AirPlay/320.17\r\nAirPlay-Receiver-Device-ID: {dev_dec}\r\nConnection: keep-alive\r\n\r\n"
    );
    // Check the write: a silently-dropped request is not a successful dial — returning true here
    // would mark the peer dialed forever without the iPhone ever being asked to open RTSP.
    if let Err(e) = s.write_all(req.as_bytes()) {
        println!("[rx] connect-out {addr} write failed: {e}");
        return false;
    }
    receiver::startup::stage(receiver::startup::Stage::Dial); // the connect timeline's `dial`: the first one sent
    println!("[rx] connect-out GET /ctrl-int/1/connect -> {addr} (devid={dev_dec})");
    let mut buf = [0u8; 1024];
    match s.read(&mut buf) {
        Ok(n) if n > 0 => {
            let resp = String::from_utf8_lossy(&buf[..n]);
            // Truncate on the BYTE buffer before lossy-decoding (audit #6): byte-indexing the decoded
            // `String` at 160 can land mid-UTF-8-sequence and panic (daemon-fatal under panic="abort").
            // Slicing `buf` first lets from_utf8_lossy place the char boundary safely.
            println!(
                "[rx] connect-out resp {n}B: {}",
                String::from_utf8_lossy(&buf[..n.min(160)])
            );
            // Honor the HTTP status: the genuine accept is `HTTP/1.1 200 OK`. An explicit non-2xx
            // means the iPhone refused — DON'T mark the peer dialed, let the retry loop try again. A
            // successful write with no/partial response is treated as tentatively-accepted (the iPhone
            // can open RTSP asynchronously without a body), preserving the proven working path.
            match resp.split_whitespace().nth(1) {
                Some(code) if code.starts_with('2') => true,
                Some(code) => {
                    println!("[rx] connect-out {addr} refused (status {code}) — will retry");
                    false
                }
                None => true, // no parseable status line; don't over-tighten the working async case
            }
        }
        // Empty response, or the 2s read timeout expiring: the write succeeded, so treat it as
        // tentatively accepted exactly as the tokio version did (a read timeout arrived there as
        // `Err(_)` from `tokio::time::timeout`; here it is `WouldBlock`/`TimedOut` from the socket).
        Ok(_) => true,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => true,
        Err(e) => {
            println!("[rx] connect-out {addr} read failed: {e}");
            false
        }
    }
}

/// Resolve an interface name to its scope id. Called per-dial (not once at startup): on wireless the
/// AP interface (wlan0) can come up AFTER this thread starts, so a startup lookup would cache a
/// permanent 0 and break every IPv6 link-local dial.
fn iface_scope(iface: &str) -> u32 {
    let c = match std::ffi::CString::new(iface) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    unsafe { libc::if_nametoindex(c.as_ptr()) }
}

/// The host's addresses, as `getifaddrs` lists them: each interface's name and address, IPv4 and IPv6. None when it
/// fails.
fn if_addrs() -> Option<Vec<(String, IpAddr, libc::c_uint)>> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `ifap` with a list this function frees below; every node is read only
    // between the two calls, and `ifa_addr` is checked for null and its family before it is read as one.
    unsafe {
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut out = Vec::new();
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null() {
                let family = i32::from((*ifa.ifa_addr).sa_family);
                let ip = if family == libc::AF_INET {
                    let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))))
                } else if family == libc::AF_INET6 {
                    let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    Some(IpAddr::from(sin6.sin6_addr.s6_addr))
                } else {
                    None
                };
                if let Some(ip) = ip {
                    let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                    out.push((name, ip, ifa.ifa_flags));
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        Some(out)
    }
}

/// The interface holding the IPv4 `addr`, if any. Wireless carplayd advertises on the AP gateway
/// (`RX_ADDR`), and the interface holding that address is the AP's, whatever the chip's bring-up named it:
/// btd passes `RX_IFACE=wlan0`, but on the 2026-11-02 bench unit the AP was `sta0`, so link-local dials
/// carried `wlan0`'s scope and failed with "Network unreachable", ten rounds at a time.
fn iface_holding(addr: Ipv4Addr) -> Option<String> {
    if_addrs()?.into_iter().find(|(_, ip, _)| *ip == IpAddr::V4(addr)).map(|(name, _, _)| name)
}

/// Every address on an interface that is up, but the loopback's: what mdns-sd can send and hear on. An entry new to it
/// is an interface mdns-sd may not have yet ([`Net`]).
fn up_addresses() -> BTreeSet<(String, IpAddr)> {
    if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, ip, flags)| flags & libc::IFF_UP as libc::c_uint != 0 && !ip.is_loopback())
        .map(|(name, ip, _)| (name, ip))
        .collect()
}

/// Turn a resolved mDNS IP into a dialable SocketAddr. The box has no `.local` resolver, so we dial
/// the ADDRESS, not the hostname; an IPv6 link-local (fe80::) needs the receiving interface's scope
/// id. IPv4 (the wireless AP case) needs no scope.
fn dialable(ip: IpAddr, port: u16, scope: u32) -> SocketAddr {
    match ip {
        IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80 => {
            SocketAddr::V6(SocketAddrV6::new(v6, port, 0, scope))
        }
        _ => SocketAddr::new(ip, port),
    }
}

/// Spawn the advertise + browse loop. Called from `run_pairing_server` AFTER `:5000` is bound —
/// deliberately, and a behaviour improvement over the two-process arrangement: `rx-connect` could
/// (and did) advertise a receiver whose RTSP port was not yet listening, so an iPhone that dialled
/// in that window got a connection refused on the turn-around.
///
/// A discovery failure is FATAL to the process, deliberately. Both spawners assume "discovery is a
/// thread inside carplayd, so its death is carplayd's death" and watch ONLY the process:
/// `btd`'s AV latch (`av.rs`, `pid_alive` on the recorded carplayd pid — the rx-connect pid it
/// used to check alongside was removed with the merge) and the wired supervisor's
/// "session daemon died while PRESENT -> re-ARM" arm (`session_supervisor.sh`). If this thread
/// merely ended, carplayd would keep serving `:5000` that no phone can find: every 0x5702 retry
/// would read `ap_ok == true` and log "AV layer already existing — nothing to do", the supervisor
/// would see carplayd alive, and nothing would ever respawn it — the phone joins the AP and
/// projection never starts, with no error anywhere but this log. Exiting makes the assumption
/// true by construction: alive == advertising. Same policy as `start_input_listener`'s bind
/// failure in main.rs ("exiting IS the repair, not a crash loop" — both spawners back off).
///
/// The thread (rather than inline) is for the blocking browse/dial loop, not for isolation.
pub fn start(device_id: String, pairing_identity: String, gate: std::sync::Arc<crate::phone_gate::PhoneGate>) {
    std::thread::spawn(move || {
        if let Err(e) = run(&device_id, &pairing_identity, &gate) {
            eprintln!("[rx] discovery failed: {e}");
            eprintln!(
                "[carplayd] FATAL: mDNS discovery is down — a receiver no phone can find is not a \
                 working receiver; exiting so btd / the supervisor respawn this daemon"
            );
            std::process::exit(1);
        }
    });
}

/// Rounds a dial gets from a record that is news: new, or changed since discovery last saw it. The source address can
/// lag the resolve right after association.
const FRESH_ROUNDS: u32 = 10;

/// Rounds a dial gets from a record discovery has seen as it is (mdns-sd's cache replaying it as a browse starts: a
/// name change, an address that came up), or one held from before. Such a record may be a phone's that left without a
/// goodbye and is still cached (~120 s), and its dead address must not hold this thread ten rounds before the fresh
/// resolves are read (the Mac's review of 2769b69). A dial from it that misses is tried again ([`Retries`],
/// carplayd-1).
const RENAME_ROUNDS: u32 = 2;

/// A dial that tried and missed is tried again this long after, then twice as long after each miss, [`RETRIES`] times
/// (carplayd-1: such a dial waited for btd's 90 s backstop, `Control::carplay_never_reached`).
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRIES: u32 = 3;

/// A new browse's first answers are read until none has come for [`QUIET`], or [`REPLAY_MAX`] at most; an old browse's
/// channel is read to its end for [`DRAIN`] at most.
const QUIET: Duration = Duration::from_millis(300);
const REPLAY_MAX: Duration = Duration::from_millis(1500);
const DRAIN: Duration = Duration::from_secs(1);

/// Without the address watch (no netlink socket): the interfaces are looked at again this often, until the advertised
/// address is on one that is up, for [`FALLBACK_FOR`] at most; mdns-sd's own 30 s check does the rest.
const FALLBACK_STEP: Duration = Duration::from_secs(1);
const FALLBACK_FOR: Duration = Duration::from_secs(300);

/// Without the name file's watch (no inotify): the name is read again this often, as 2.6.0 did.
const NAME_POLL: Duration = Duration::from_secs(1);

/// A resolved `_carplay-ctrl` record, as discovery keeps it.
#[derive(Debug, Clone)]
struct Ctrl {
    fullname: String,
    /// The TXT `id`: the phone's BR/EDR address (2026-11-01 bench), the same address btd names.
    id: Option<String>,
    addrs: Vec<IpAddr>,
    port: u16,
}

impl Ctrl {
    fn who(&self) -> String {
        who(&self.fullname, self.id.as_deref())
    }

    /// Whether `other` is this record as it was: the same instance, id, port and addresses (in any order).
    fn same(&self, other: &Ctrl) -> bool {
        let addrs = |c: &Ctrl| c.addrs.iter().copied().collect::<BTreeSet<IpAddr>>();
        self.fullname == other.fullname && self.id == other.id && self.port == other.port && addrs(self) == addrs(other)
    }
}

/// A record in the log: its instance as a short hash, since an instance is usually its owner's name for the
/// phone and the box log reaches Carlink's drive log, and its TXT id as a tail (`..XX:YY`, as btd's lines;
/// carplayd-5).
fn who(fullname: &str, id: Option<&str>) -> String {
    format!("{} ({})", crate::short_tag(fullname.as_bytes()), id.map_or_else(|| "no id".to_string(), id_tail))
}

/// The last two octets of a TXT `id` in btd's spelling (`..XX:YY`); `id ?` for one that is not an address.
fn id_tail(id: &str) -> String {
    match crate::phone_gate::normalize(id) {
        Some(hex) => format!("..{}:{}", hex[8..10].to_ascii_uppercase(), hex[10..].to_ascii_uppercase()),
        None => "id ?".to_string(),
    }
}

/// A TXT record for the log: a value that is an address shows as its tail.
fn txt_line<'a>(props: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let shown: Vec<String> = props
        .map(|(k, v)| match crate::phone_gate::normalize(v) {
            Some(_) => format!("{k}={}", id_tail(v)),
            None => format!("{k}={v}"),
        })
        .collect();
    shown.join(" ")
}

/// How the name file changed since discovery last acted on it ([`Held::check`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    None,
    /// Another phone named, or none.
    Renamed,
    /// The same phone, and btd has now sent it its 0x5703: its held records may be dialled.
    Cleared,
}

/// The records left alone for now, and the name discovery last acted on. The Mac's 15 Nov bench: a phone that knows
/// the AP joins it within ~1.5 s of its Bluetooth link, before its 0x5703 names it, so its record resolved while the
/// last phone was still named and was refused. Nothing looked again when the name changed, and its dial waited for the
/// browser's own re-query, 84 s later; its record had been removed meanwhile. So a name change asks `_carplay-ctrl`
/// afresh and dials the held records the gate now lets through (R1, [`rename`]). Since A2 a record is held also while
/// no phone is named, and while the named phone's 0x5703 has not been sent ([`Dial::Waiting`]): its mark dials them.
struct Held {
    named: Option<String>,
    /// Whether btd had sent the named phone its 0x5703 when discovery last acted on the name.
    wifi: bool,
    /// Each held record, and the rounds its dial gets.
    records: HashMap<String, (Ctrl, u32)>,
    /// Each record as discovery last saw it resolved, until it is removed (carplayd-2, [`rounds_for`]).
    seen: HashMap<String, Ctrl>,
}

impl Held {
    fn new(gate: &PhoneGate) -> Self {
        let now = gate.named_state();
        Self {
            wifi: now.as_ref().is_some_and(|n| n.wifi),
            named: now.map(|n| n.phone),
            records: HashMap::new(),
            seen: HashMap::new(),
        }
    }

    fn keep(&mut self, c: Ctrl, rounds: u32) {
        self.records.insert(c.fullname.clone(), (c, rounds));
    }

    fn forget(&mut self, fullname: &str) {
        self.records.remove(fullname);
    }

    /// How the name changed since the last call ([`Change`]).
    fn check(&mut self, gate: &PhoneGate) -> Change {
        let now = gate.named_state();
        let wifi = now.as_ref().is_some_and(|n| n.wifi);
        let phone = now.map(|n| n.phone);
        if phone != self.named {
            self.named = phone;
            self.wifi = wifi;
            return Change::Renamed;
        }
        let cleared = wifi && !self.wifi;
        self.wifi = wifi;
        if cleared {
            Change::Cleared
        } else {
            Change::None
        }
    }

    /// The held records the gate lets dial now, taken out to be dialled: never one whose session is live here, nor
    /// another phone's, nor the named phone's before its 0x5703.
    fn due(&mut self, gate: &PhoneGate) -> Vec<(Ctrl, u32)> {
        let due: Vec<String> = self
            .records
            .values()
            .filter(|(c, _)| gate.dial(c.id.as_deref()).0.now())
            .map(|(c, _)| c.fullname.clone())
            .collect();
        due.iter().filter_map(|k| self.records.remove(k)).collect()
    }

    /// carplayd-2: the rounds a resolved record's dial gets, and the record noted as seen. One discovery has seen as it is
    /// (mdns-sd sends nothing for a record that did not change, so this is its cache replayed as a browse starts) gets
    /// [`RENAME_ROUNDS`]; news gets [`FRESH_ROUNDS`], wherever it is read, a name change's replay window included.
    fn rounds_for(&mut self, c: &Ctrl) -> u32 {
        let rounds = if self.seen.get(&c.fullname).is_some_and(|p| p.same(c)) { RENAME_ROUNDS } else { FRESH_ROUNDS };
        self.seen.insert(c.fullname.clone(), c.clone());
        rounds
    }
}

/// How a dial went ([`Dialer::dial`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialled {
    Accepted,
    /// It tried every round and no address accepted it: tried again later ([`Retries`]).
    Missed,
    /// Nothing to try (every address dialled already, or refused at SETUP), or its phone stopped being the one to dial.
    Nothing,
}

/// Dials the phones' control ports, and remembers what it dialled.
struct Dialer {
    iface: String,
    adv_addr: String,
    dialed: HashSet<String>,
    /// The addresses dialed for each phone's service, each with the naming's epoch it was dialed under, and its TXT id:
    /// a phone that leaves forgets only its own, and a name change forgets the newly named phone's from earlier namings
    /// ([`Dialer::forget_phone`]).
    dialed_by: HashMap<String, (Option<String>, Vec<(String, u64)>)>,
    /// `connect_out` with this receiver's device id; a test's fake.
    connect: Box<dyn FnMut(SocketAddr) -> bool>,
    /// Between rounds: a second, none in tests.
    pause: Duration,
}

impl Dialer {
    fn new(dev_dec: u64, iface: String, adv_addr: String) -> Self {
        Self {
            iface,
            adv_addr,
            dialed: HashSet::new(),
            dialed_by: HashMap::new(),
            connect: Box::new(move |sa| connect_out(sa, dev_dec)),
            pause: Duration::from_secs(1),
        }
    }

    /// Scope for IPv6 link-local dials, resolved NOW rather than once at startup: the interface holding
    /// the advertised AP address, else RX_IFACE.
    fn scope(&self) -> (String, u32) {
        let ap_iface = self.adv_addr.parse::<Ipv4Addr>().ok().and_then(iface_holding);
        let scope_iface = ap_iface.unwrap_or_else(|| self.iface.clone());
        let scope = iface_scope(&scope_iface);
        (scope_iface, scope)
    }

    /// Dial `c` for up to `rounds` rounds.
    fn dial(&mut self, gate: &PhoneGate, c: &Ctrl, scope: u32, rounds: u32) -> Dialled {
        // Dial the resolved ADDRESS(es), not the hostname (box has no .local resolver). Mark a
        // peer dialed only on SUCCESS, retrying transient failures with a short backoff (the
        // source address can lag the mDNS resolve by a few seconds right after association).
        // Interleave addresses per attempt round (audit Fix #12): try EACH resolved address once
        // per pass, THEN sleep — so a stale/unreachable address listed first (e.g. an IPv6
        // link-local that lags association) can't burn its whole ~40s retry budget before the
        // good address is tried, and can't block this browse thread from processing queued mDNS
        // events (incl. ServiceRemoved) for that long. One accepted dial is enough — the iPhone
        // opens RTSP off it (we don't dial the other family too).
        for attempt in 1..=rounds {
            let mut tried = false;
            for ip in &c.addrs {
                // The phone btd names can change while this dials (a Bluetooth session's start, its put-back,
                // a 0x5703): never dial one that is no longer it, or whose session came up meanwhile. The epoch is
                // the naming this dial is made under (carplayd-3).
                let (verdict, epoch) = gate.dial(c.id.as_deref());
                if !verdict.now() {
                    println!("[rx] {}: not the phone to dial any more; stopped", c.who());
                    return Dialled::Nothing;
                }
                let sa = dialable(*ip, c.port, scope);
                if self.dialed.contains(&sa.to_string()) {
                    println!("[rx] {}: already dialled at {sa}; not again", c.who());
                    continue;
                }
                // A phone the gate refused at SETUP: the one a switch dropped, still on the AP.
                // Not marked dialed, so a carplayd later told to serve that phone still dials it.
                if gate.is_refused(*ip) {
                    println!("[rx] {sa}: its phone was refused at SETUP; not dialling it");
                    continue;
                }
                tried = true;
                if (self.connect)(sa) {
                    self.dialed.insert(sa.to_string());
                    let by = self.dialed_by.entry(c.fullname.clone()).or_insert_with(|| (c.id.clone(), Vec::new()));
                    by.1.push((sa.to_string(), epoch));
                    return Dialled::Accepted;
                }
            }
            // Every resolved address is already dialed (mDNS re-announce / TTL refresh of a
            // connected peer) or none resolved — nothing to attempt. Fall through instantly like
            // the pre-fix address-major loop did, instead of sleeping 10 rounds for nothing (audit
            // Fix #12 v2, regression caught by the gate: without this a repeat ServiceResolved of
            // an already-connected phone would stall this browse thread ~10s).
            if !tried {
                return Dialled::Nothing;
            }
            if attempt < rounds {
                println!("[rx] connect-out round {attempt}/{rounds} — no address accepted, retrying in 1s");
                std::thread::sleep(self.pause);
            }
        }
        println!("[rx] {}: no address accepted in {rounds} round(s)", c.who());
        Dialled::Missed
    }

    /// The iPhone's control service went away (Wi-Fi drop / sleep). Forget the addresses dialed for
    /// it so it gets re-dialed when it reappears — otherwise `dialed` pins the old address forever
    /// and the phone can never reconnect without restarting the daemon. ONLY its own: this was
    /// the whole set while the box was taken for single-phone, and with a second phone on the AP
    /// (the 2026-11-01 switch bench) one leaving would make discovery dial the other again.
    fn removed(&mut self, fullname: &str) {
        if let Some((id, addrs)) = self.dialed_by.remove(fullname) {
            println!(
                "[rx] _carplay-ctrl removed ({}) — forgetting its {} dialed address(es)",
                who(fullname, id.as_deref()),
                addrs.len()
            );
            for (a, _) in addrs {
                self.dialed.remove(&a);
            }
        }
    }

    /// The phone btd now names may be dialled again at an address dialled under an earlier naming of it: mdns-sd sends
    /// nothing for an unchanged record, and `dialed` shrank only on its removal (carplayd-4). Only dials from before
    /// this naming (`epoch`, the gate's for the naming just seen): one made moments earlier, as the name changed and
    /// before discovery acted on it, is this naming's, and is not made again (carplayd-3). A live session is still
    /// never dialled ([`Dial::Live`]).
    fn forget_phone(&mut self, named: &str, epoch: u64) {
        for (id, addrs) in self.dialed_by.values_mut() {
            if id.as_deref().and_then(crate::phone_gate::normalize).as_deref() != Some(named) {
                continue;
            }
            let dialed = &mut self.dialed;
            addrs.retain(|(a, e)| {
                let earlier = *e < epoch;
                if earlier {
                    dialed.remove(a);
                }
                !earlier
            });
        }
        self.dialed_by.retain(|_, (_, addrs)| !addrs.is_empty());
    }
}

/// A dial that missed, to be tried again ([`Retries`]).
#[derive(Debug, Clone)]
struct Retry {
    c: Ctrl,
    /// Tries left.
    left: u32,
    gap: Duration,
    at: Instant,
}

/// carplayd-1: the dials that tried and missed, tried again [`RETRIES`] times, [`RETRY_FIRST`] after the miss and twice
/// as long after each next one, while their record stands (a removal or a fresh resolve of it drops the retry) and its
/// phone is still the one to dial. Not while a control connection is open: that may be the phone's own, and a second
/// dial would have it preempt itself.
struct Retries {
    list: Vec<Retry>,
    first: Duration,
}

impl Retries {
    fn new(first: Duration) -> Self {
        Self { list: Vec::new(), first }
    }

    fn add(&mut self, c: Ctrl) {
        self.drop_for(&c.fullname);
        self.list.push(Retry { c, left: RETRIES, gap: self.first, at: Instant::now() + self.first });
    }

    /// `r`, which missed again (or waited for an open connection), once more if it has tries left.
    fn again(&mut self, mut r: Retry) {
        if r.left > 1 {
            r.left -= 1;
            r.gap *= 2;
            r.at = Instant::now() + r.gap;
            self.list.push(r);
        }
    }

    fn drop_for(&mut self, fullname: &str) {
        self.list.retain(|r| r.c.fullname != fullname);
    }

    fn next(&self) -> Option<Instant> {
        self.list.iter().map(|r| r.at).min()
    }

    fn due(&mut self, now: Instant) -> Vec<Retry> {
        let (due, rest) = std::mem::take(&mut self.list).into_iter().partition(|r| r.at <= now);
        self.list = rest;
        due
    }
}

/// Whether an interface or address came up that mdns-sd may not have: the wireless carplayd starts early, often before
/// the AP has its address (A2), and mdns-sd reads the host's interfaces only at its start and every 30 s. The address
/// watch (netlink, `watch.rs`) marks `dirty` and wakes the loop; then the addresses on interfaces that are up are read,
/// and one not there at the last look is news. A link that comes up keeps the address it had while down: mdns-sd may
/// have had no socket for it, since it cannot send there while down, so it counts too. Without the watch, the
/// addresses are looked at every [`FALLBACK_STEP`] until the advertised address is up, for [`FALLBACK_FOR`] at most.
/// The wired carplayd (no advertised address) has none of this.
struct Net {
    on: bool,
    dirty: Arc<AtomicBool>,
    watched: bool,
    last: BTreeSet<(String, IpAddr)>,
    snapshot: Box<dyn FnMut() -> BTreeSet<(String, IpAddr)>>,
    adv: Option<IpAddr>,
    fallback_until: Option<Instant>,
}

impl Net {
    /// `adv`: the advertised address; `watched`: the address watch is up, marking `dirty`. The first look is now, before
    /// mdns-sd's daemon reads the interfaces, so an address that comes between the two is news.
    fn new(
        adv: Option<IpAddr>,
        watched: bool,
        dirty: Arc<AtomicBool>,
        mut snapshot: Box<dyn FnMut() -> BTreeSet<(String, IpAddr)>>,
    ) -> Self {
        let last = snapshot();
        let mut net = Net { on: adv.is_some(), dirty, watched, last, snapshot, adv, fallback_until: None };
        if net.on && !watched && !net.adv_up() {
            net.fallback_until = Some(Instant::now() + FALLBACK_FOR);
        }
        net
    }

    /// The wired carplayd's: nothing watched, nothing looked at.
    fn off() -> Self {
        Net {
            on: false,
            dirty: Arc::new(AtomicBool::new(false)),
            watched: false,
            last: BTreeSet::new(),
            snapshot: Box::new(BTreeSet::new),
            adv: None,
            fallback_until: None,
        }
    }

    fn adv_up(&self) -> bool {
        self.adv.is_some_and(|a| self.last.iter().any(|(_, ip)| *ip == a))
    }

    /// Whether an address came up since the last look ([`Net`]).
    fn changed(&mut self) -> bool {
        if !self.on || (self.watched && !self.dirty.swap(false, Ordering::AcqRel)) {
            return false;
        }
        if !self.watched && self.fallback_until.is_none() {
            return false;
        }
        let now = (self.snapshot)();
        let news = now.iter().any(|a| !self.last.contains(a));
        self.last = now;
        if self.fallback_until.is_some_and(|t| self.adv_up() || Instant::now() >= t) {
            self.fallback_until = None;
        }
        news
    }

    /// How long the loop may sleep for this: the fallback's step while it lasts.
    fn wait(&self) -> Option<Duration> {
        self.fallback_until.map(|_| FALLBACK_STEP)
    }
}

/// What a browse's channel gave: an event, a wake from elsewhere (the name file, an address), nothing within the wait,
/// or nothing ever again (closed).
enum Recv {
    Event(ServiceEvent),
    Woken,
    Quiet,
    Gone,
}

/// A `_carplay-ctrl` browse's channel and the loop's wakes, read together, with a wait (`None`: until one comes).
type Events = Box<dyn FnMut(Option<Duration>) -> Recv>;

/// Where discovery starts and stops its `_carplay-ctrl` browse, and tells mdns-sd the interfaces changed: mdns-sd, or a
/// test's fake.
trait Browser {
    fn start(&self) -> Result<Events, String>;
    fn stop(&self) -> Result<(), String>;
    /// An interface or address came up ([`Net`]): mdns-sd reads the host's interfaces again now, not at its 30 s check
    /// (`enable_interface` makes it apply its selections to them at once, every one selected), and `_airplay._tcp` is
    /// announced again, on the new one too (`register` again: two announcements, a second apart, RFC 6762 §8.3).
    fn interfaces_changed(&self);
}

struct Mdns<'a> {
    daemon: &'a ServiceDaemon,
    svc: ServiceInfo,
    wakes: flume::Receiver<()>,
}

impl Browser for Mdns<'_> {
    fn start(&self) -> Result<Events, String> {
        let rx = self.daemon.browse(CTRL_SERVICE).map_err(|e| e.to_string())?;
        let wakes = self.wakes.clone();
        Ok(Box::new(move |wait: Option<Duration>| {
            let select = flume::Selector::new()
                .recv(&rx, |r| match r {
                    Ok(ev) => Recv::Event(ev),
                    Err(_) => Recv::Gone,
                })
                .recv(&wakes, |_| Recv::Woken);
            match wait {
                None => select.wait(),
                Some(d) => select.wait_timeout(d).unwrap_or(Recv::Quiet),
            }
        }))
    }

    fn stop(&self) -> Result<(), String> {
        self.daemon.stop_browse(CTRL_SERVICE).map_err(|e| e.to_string())
    }

    fn interfaces_changed(&self) {
        if let Err(e) = self.daemon.enable_interface(IfKind::All) {
            println!("[rx] mDNS: reading the interfaces again: {e}");
        }
        if let Err(e) = self.daemon.register(self.svc.clone()) {
            println!("[rx] mDNS: announcing _airplay._tcp again: {e}");
        }
    }
}

/// Start the browse, tried for about a second. mdns-sd's `send_cmd` queues a command with `try_send`, then
/// wakes its daemon: it reports an error when the queue is full (`Error::Again`) or the wake fails, the latter
/// with the command queued all the same. Still refused after a second, its daemon is gone, and discovery is
/// down, as when the first browse fails.
fn browse_again(b: &dyn Browser) -> Result<Events, String> {
    let mut tries = 0;
    loop {
        match b.start() {
            Ok(events) => return Ok(events),
            Err(e) if tries < 10 => {
                tries += 1;
                println!("[rx] browse _carplay-ctrl: {e}; retrying");
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Discovery's state between events: the gate, the dialer, the held records, the retries, the interfaces.
struct Loop<'a> {
    gate: &'a PhoneGate,
    dialer: &'a mut Dialer,
    held: &'a mut Held,
    retries: &'a mut Retries,
    net: &'a mut Net,
    /// The name file is read again every [`NAME_POLL`] (no watch on it).
    poll_name: bool,
}

impl Loop<'_> {
    /// Dial `c` for `rounds`, and try it again later when it missed ([`Retries`]).
    fn dial(&mut self, c: &Ctrl, rounds: u32) -> Dialled {
        let (_, scope) = self.dialer.scope();
        let how = self.dialer.dial(self.gate, c, scope, rounds);
        if how == Dialled::Missed {
            self.retries.add(c.clone());
        }
        how
    }

    /// One event from the browse; returns the instance a resolve was for.
    fn on_event(&mut self, ev: ServiceEvent) -> Option<String> {
        match ev {
            ServiceEvent::ServiceResolved(info) => {
                let (scope_iface, scope) = self.dialer.scope();
                let port = info.get_port();
                let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                // What the phone puts in its TXT record. Its `id` is its BR/EDR address (2026-11-01 bench),
                // so another phone than the one btd serves is never dialled (fix 1b). Not marked dialed, and
                // held: when btd names that phone, it is dialled from here at once (the Mac's 15 Nov bench).
                // The phone gate still refuses another phone at SETUP, for a TXT without an id.
                let id = info
                    .get_properties()
                    .iter()
                    .find(|p| p.key().eq_ignore_ascii_case("id"))
                    .map(|p| p.val_str().to_string());
                let c = Ctrl { fullname: info.get_fullname().to_string(), id, addrs, port };
                println!(
                    "[rx] resolved _carplay-ctrl {} :{port} addrs={:?} scope={scope_iface}({scope})",
                    c.who(),
                    c.addrs
                );
                println!(
                    "[rx] _carplay-ctrl TXT: {}",
                    txt_line(info.get_properties().iter().map(|p| (p.key(), p.val_str())))
                );
                let rounds = self.held.rounds_for(&c);
                self.retries.drop_for(&c.fullname);
                match self.gate.dial(c.id.as_deref()).0 {
                    Dial::Elsewhere => {
                        println!("[rx] {}: another phone than btd serves; not dialling it", c.who());
                        self.held.keep(c.clone(), rounds);
                    }
                    Dial::Unnamed => {
                        println!("[rx] {}: btd names no phone yet; not dialling it", c.who());
                        self.held.keep(c.clone(), rounds);
                    }
                    Dial::Waiting => {
                        // The phone btd names, visible now: the connect timeline's `svc_seen`, though it waits for dial_ok.
                        receiver::startup::stage(receiver::startup::Stage::SvcSeen);
                        println!("[rx] {}: the phone btd names; dialled once btd has sent it its 0x5703", c.who());
                        self.held.keep(c.clone(), rounds);
                    }
                    // The phone btd serves, with its session already live here: a record that changed
                    // addresses mid-session is the same phone, and dialling it again ends the session that
                    // is showing video (bench 2026-11-02: dialled at .101, the record grew .100, a second
                    // connect to .100 got 200 OK, and the session was torn down 29 s after video started).
                    Dial::Live => {
                        self.held.forget(&c.fullname);
                        println!("[rx] {}: its session is live here; not dialling it again at {:?}", c.who(), c.addrs);
                    }
                    verdict @ (Dial::Now | Dial::Early) => {
                        // The connect timeline's `svc_seen`: the record of the phone btd names, the first to resolve.
                        receiver::startup::stage(receiver::startup::Stage::SvcSeen);
                        self.held.forget(&c.fullname);
                        if verdict == Dial::Early {
                            println!("[bench] dial_early: dialling {} before btd has sent it its 0x5703", c.who());
                        }
                        self.dial(&c, rounds);
                    }
                }
                Some(c.fullname)
            }
            ServiceEvent::ServiceRemoved(_ty, fullname) => {
                self.held.forget(&fullname);
                self.held.seen.remove(&fullname);
                self.dialer.removed(&fullname);
                self.retries.drop_for(&fullname);
                None
            }
            _ => None,
        }
    }

    /// How long the loop may sleep: until the next retry, the fallback's next look, or the name's next read; `None`,
    /// until something wakes it.
    fn wait(&self) -> Option<Duration> {
        let retry = self.retries.next().map(|t| t.saturating_duration_since(Instant::now()));
        let name = self.poll_name.then_some(NAME_POLL);
        [retry, self.net.wait(), name].into_iter().flatten().min()
    }

    /// The browse stopped and started again. In order:
    /// - the browse is stopped and started again, even when the stop was refused (a refused stop can still have been
    ///   queued, and it closes the channel at the daemon's next wake: carplayd-3 of 2.6.0's review);
    /// - the old channel is read to its end first, so a removal already sent there is not lost;
    /// - the new browse's first answers are read: mdns-sd's cache, replayed as it starts, and the answers to its query,
    ///   which it sends at once and again 1 s, 2 s, ... later. Each is dialled by the rounds its news earns
    ///   ([`Held::rounds_for`]).
    ///
    /// Returns the new browse and the instances it resolved.
    fn rebrowse(&mut self, b: &dyn Browser, mut old: Events) -> Result<(Events, HashSet<String>), String> {
        if let Err(e) = b.stop() {
            println!("[rx] stop _carplay-ctrl browse: {e}; browsing again all the same");
        }
        let mut events = browse_again(b)?;
        let until = Instant::now() + DRAIN;
        let mut left = Vec::new();
        loop {
            let wait = until.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                break;
            }
            match old(Some(wait)) {
                Recv::Event(ServiceEvent::SearchStopped(_)) | Recv::Gone | Recv::Quiet => break,
                Recv::Woken => {}
                Recv::Event(ev) => left.push(ev),
            }
        }
        let mut resolved = HashSet::new();
        for ev in left {
            resolved.extend(self.on_event(ev));
        }
        let until = Instant::now() + REPLAY_MAX;
        let mut quiet = Instant::now() + QUIET;
        loop {
            let wait = quiet.min(until).saturating_duration_since(Instant::now());
            if wait.is_zero() {
                break;
            }
            match events(Some(wait)) {
                Recv::Event(ServiceEvent::SearchStopped(_)) | Recv::Gone => {
                    events = browse_again(b)?;
                    break;
                }
                Recv::Event(ev) => {
                    resolved.extend(self.on_event(ev));
                    quiet = Instant::now() + QUIET;
                }
                Recv::Woken => {}
                Recv::Quiet => break,
            }
        }
        Ok((events, resolved))
    }

    /// The phone btd serves changed (the Mac's 15 Nov bench, R1). In order:
    /// - the newly named phone's addresses dialled under an earlier naming of it are forgotten, so it is dialled again
    ///   (carplayd-4), but not one dialled under this naming (carplayd-3);
    /// - the held records it now lets dial are taken, and the rest let go: the replay holds again whatever is still
    ///   cached (2.6.0's carplayd-1 review);
    /// - the browse starts again ([`Loop::rebrowse`]);
    /// - a taken record the cache did not resolve again is dialled for [`RENAME_ROUNDS`].
    fn rename(&mut self, b: &dyn Browser, old: Events) -> Result<Events, String> {
        println!("[rx] the phone btd serves changed: asking _carplay-ctrl again");
        receiver::startup::naming_changed();
        if self.held.wifi {
            // Named with its 0x5703 already sent (at a handoff that named it, or put back): its dial may go now.
            receiver::startup::dial_ok();
        }
        if let Some(named) = self.held.named.clone() {
            self.dialer.forget_phone(&named, self.gate.epoch());
        }
        let mut due = self.held.due(self.gate);
        self.held.records.clear();
        let (events, resolved) = self.rebrowse(b, old)?;
        due.retain(|(c, _)| !resolved.contains(&c.fullname));
        for (c, _) in due {
            println!("[rx] {}: resolved while btd named another phone; dialling it now", c.who());
            // `svc_seen` for a record held from before its phone was named: it is the named phone's from now.
            receiver::startup::stage(receiver::startup::Stage::SvcSeen);
            self.dial(&c, RENAME_ROUNDS);
        }
        Ok(events)
    }

    /// btd has sent the phone it names its 0x5703 (the name file's mark): the connect timeline's `dial_ok`, and the
    /// records held for that phone are dialled now, each by its rounds.
    fn cleared(&mut self) {
        receiver::startup::dial_ok();
        let due = self.held.due(self.gate);
        println!("[rx] btd has sent the phone it names its 0x5703: {} record(s) of it to dial", due.len());
        for (c, rounds) in due {
            self.dial(&c, rounds);
        }
    }

    /// The retries that are due ([`Retries`]).
    fn retry(&mut self) {
        for r in self.retries.due(Instant::now()) {
            match self.gate.dial(r.c.id.as_deref()).0 {
                Dial::Live => continue,
                v if !v.now() => {
                    // Not the phone to dial now: held, for when it is.
                    self.held.keep(r.c, RENAME_ROUNDS);
                    continue;
                }
                _ => {}
            }
            if self.gate.connection_open() {
                println!("[rx] {}: a control connection is open; its retry waits", r.c.who());
                self.retries.again(r);
                continue;
            }
            println!("[rx] {}: dialling again after a dial that missed ({} more after this)", r.c.who(), r.left - 1);
            let (_, scope) = self.dialer.scope();
            if self.dialer.dial(self.gate, &r.c, scope, RENAME_ROUNDS) == Dialled::Missed {
                self.retries.again(r);
            }
        }
    }
}

/// Discovery's loop: each event; then, whatever woke it, an address that came up ([`Net`]), the name btd wrote
/// ([`Held::check`]: a rename, or the 0x5703 mark), and the retries due. A browse that stops or whose channel closes
/// is started again: only a browse mdns-sd will not start ends discovery, with its error.
fn browse_loop(b: &dyn Browser, mut lp: Loop<'_>) -> String {
    let mut events = match browse_again(b) {
        Ok(e) => e,
        Err(e) => return e,
    };
    loop {
        match events(lp.wait()) {
            Recv::Event(ServiceEvent::SearchStopped(_)) | Recv::Gone => {
                println!("[rx] the _carplay-ctrl browse ended; browsing again");
                std::thread::sleep(Duration::from_millis(100));
                events = match browse_again(b) {
                    Ok(e) => e,
                    Err(e) => return e,
                };
            }
            Recv::Event(ev) => {
                lp.on_event(ev);
            }
            Recv::Woken | Recv::Quiet => {}
        }
        if lp.net.changed() {
            println!("[rx] an interface or address came up: mDNS reads them again, announces again and asks again");
            b.interfaces_changed();
            events = match lp.rebrowse(b, events) {
                Ok((e, _)) => e,
                Err(e) => return e,
            };
        }
        match lp.held.check(lp.gate) {
            Change::Renamed => {
                events = match lp.rename(b, events) {
                    Ok(e) => e,
                    Err(e) => return e,
                }
            }
            Change::Cleared => lp.cleared(),
            Change::None => {}
        }
        lp.retry();
    }
}

fn run(
    dev_id: &str,
    pi: &str,
    gate: &crate::phone_gate::PhoneGate,
) -> Result<(), Box<dyn std::error::Error>> {
    let dev_dec = mac_to_dec(dev_id);
    let iface = std::env::var("RX_IFACE").unwrap_or_else(|_| "ncm0".to_string());
    let adv_addr = std::env::var("RX_ADDR").unwrap_or_default(); // "" => addr-auto
    println!(
        "[rx] device id {dev_id} = {dev_dec} (dec); pi {pi}; iface={iface} adv_addr={}",
        if adv_addr.is_empty() {
            "addr-auto"
        } else {
            &adv_addr
        }
    );

    // What wakes the loop besides mDNS (watch.rs): btd's name file, and on the wireless bearer (an advertised address)
    // the host's links and addresses. Both watches before the first look at the interfaces, and that before mdns-sd's
    // daemon reads them, so nothing that comes up in between is missed.
    let (wake, wakes) = flume::unbounded::<()>();
    let name_watched = gate.file().is_some_and(|f| watch::name_file(f, wake.clone()));
    if gate.armed() && !name_watched {
        println!("[rx] no watch on btd's name file: reading it every {} s instead", NAME_POLL.as_secs());
    }
    let mut net = match adv_addr.parse::<IpAddr>() {
        Ok(adv) => {
            let dirty = Arc::new(AtomicBool::new(false));
            let watched = watch::addresses(dirty.clone(), wake.clone());
            if !watched {
                println!("[rx] no watch on the addresses: looking at them every {} s until {adv} is up", FALLBACK_STEP.as_secs());
            }
            Net::new(Some(adv), watched, dirty, Box::new(up_addresses))
        }
        Err(_) => Net::off(),
    };

    // mDNS advertise _airplay._tcp (CarPlay TXT) + browse _carplay-ctrl.
    let mdns = ServiceDaemon::new()?;
    let mut props: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("deviceid", dev_id),
        // features = the genuine CCPA head unit's live Bonjour value. iOS reads this TXT at DISCOVERY
        // to classify the service; the HIGH word 0x61 = Car(bit32) | CarPlayControl(bit37) |
        // HKPairingAndEncrypt(bit38). The Car bit marks us a *CarPlay* receiver so the iPhone opens
        // RTSP back to :5000 (dropping it → iPhone answers the connect-out 200 OK but never opens RTSP,
        // so no session/video). Must equal /info features (info.rs).
        ("features", "0x44440B80,0x61"),
        ("flags", "0x4"),
        ("model", "CarLink-mac-1.0"),
        ("protovers", "1.0"),
        ("pi", pi),
        ("srcvers", "320.17"),
    ] {
        props.insert(k.to_string(), v.to_string());
    }
    // Advertise with an explicit address when RX_ADDR is set (wireless AP: the wlan0 IPv4 the iPhone
    // can reach); otherwise addr-auto publishes every interface address (wired default).
    let svc = ServiceInfo::new(
        "_airplay._tcp.local.",
        "CarPlay",
        "ncmcarplay.local.",
        adv_addr.as_str(),
        PORT,
        props,
    )?;
    let svc = if adv_addr.is_empty() {
        svc.enable_addr_auto()
    } else {
        svc
    };
    mdns.register(svc.clone())?;
    println!("[rx] advertised _airplay._tcp 'CarPlay' :{PORT}");

    let mut dialer = Dialer::new(dev_dec, iface, adv_addr);
    let mut held = Held::new(gate);
    if held.wifi {
        // Started with its phone already named and handed off (the 0x5703's own start, when the early one did not).
        receiver::startup::dial_ok();
    }
    let mut retries = Retries::new(RETRY_FIRST);
    let browser = Mdns { daemon: &mdns, svc, wakes };
    let lp = Loop {
        gate,
        dialer: &mut dialer,
        held: &mut held,
        retries: &mut retries,
        net: &mut net,
        poll_name: gate.armed() && !name_watched,
    };
    let why = browse_loop(&browser, lp);
    drop(wake);
    // A browse that ends is started again; only one mdns-sd will not start ends here, which means its daemon
    // thread has exited ("mDNS died mid-session") — an Err, so `start` treats it exactly like a construction
    // failure rather than ending quietly.
    Err(format!("the _carplay-ctrl browse would not start again ({why}) — the mDNS daemon is down").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_to_dec_matches_the_advertised_device_id() {
        // The value the box has always advertised, kept as a regression anchor across the merge:
        // this decimal is what goes out in `AirPlay-Receiver-Device-ID` and what the iPhone echoes.
        assert_eq!(mac_to_dec("AA:BB:CC:DD:EE:01"), 187_723_572_702_721);
        assert_eq!(mac_to_dec("00:00:00:00:00:00"), 0);
        // A malformed octet contributes 0 rather than panicking (daemon-fatal under panic="abort").
        assert_eq!(mac_to_dec("ZZ:00:00:00:00:01"), 1);
    }

    #[test]
    fn ipv6_link_local_dials_carry_the_interface_scope() {
        let v6: IpAddr = "fe80::1".parse().unwrap();
        match dialable(v6, 7000, 42) {
            SocketAddr::V6(a) => {
                assert_eq!(a.scope_id(), 42, "link-local dial without a scope is unroutable");
                assert_eq!(a.port(), 7000);
            }
            other => panic!("expected V6, got {other:?}"),
        }
        // A global v6 address takes no scope...
        let g: IpAddr = "2001:db8::1".parse().unwrap();
        assert!(matches!(dialable(g, 7000, 42), SocketAddr::V6(a) if a.scope_id() == 0));
        // ...and IPv4 (the wireless AP bearer) never does.
        let v4: IpAddr = "192.168.43.10".parse().unwrap();
        assert!(matches!(dialable(v4, 7000, 42), SocketAddr::V4(_)));
    }

    #[test]
    fn the_interface_holding_an_address_is_found_by_the_address() {
        let lo = iface_holding(Ipv4Addr::LOCALHOST).expect("the loopback holds 127.0.0.1");
        assert!(lo.starts_with("lo"), "{lo}: lo on Linux, lo0 elsewhere");
        assert_eq!(iface_holding(Ipv4Addr::new(192, 0, 2, 77)), None, "an address no interface holds");
        assert!(!up_addresses().iter().any(|(_, ip)| ip.is_loopback()), "the loopback is no interface mdns-sd uses");
    }

    // The Mac's 15 Nov bench: phone A was named, phone B had handed off before.
    const PHONE_A: &str = "02:00:00:00:00:0a";
    const PHONE_B: &str = "02:00:00:00:00:0b";
    const PHONE_C: &str = "02:00:00:00:00:0c";

    fn scratch(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "discovery_{tag}_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    /// A gate whose name file holds `text` (`"<phone> wifi"` as btd writes it at a 0x5703, `"<phone>"` at a Bluetooth
    /// session's start, `""` for none), and a bench lever of its own, off.
    fn gate_with(text: &str) -> (PhoneGate, std::path::PathBuf) {
        let path = scratch("expect");
        if text.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            std::fs::write(&path, text).unwrap();
        }
        (PhoneGate::with_bench(Some(path.clone()), scratch("bench")), path)
    }

    /// The name as btd writes it at the phone's 0x5703.
    fn handed(phone: &str) -> String {
        format!("{phone} wifi")
    }

    fn gate_naming(phone: &str) -> (PhoneGate, std::path::PathBuf) {
        gate_with(&handed(phone))
    }

    fn ctrl(fullname: &str, id: &str, last: u8) -> Ctrl {
        Ctrl {
            fullname: fullname.to_string(),
            id: Some(id.to_string()),
            addrs: vec![IpAddr::from([192, 168, 43, last])],
            port: 7000,
        }
    }

    fn names(due: &[(Ctrl, u32)]) -> Vec<&str> {
        due.iter().map(|(c, _)| c.fullname.as_str()).collect()
    }

    #[test]
    fn a_phone_refused_before_btd_named_it_is_dialled_once_it_is_named() {
        // 10:54:26.243 B's record resolved while the file named A; B's 0x5703 named it 0.3 s later.
        let (gate, path) = gate_naming(PHONE_A);
        let mut held = Held::new(&gate);
        let b = ctrl("B._carplay-ctrl._tcp.local.", PHONE_B, 101);
        assert_eq!(gate.dial(b.id.as_deref()).0, Dial::Elsewhere);
        held.keep(b, FRESH_ROUNDS);
        assert_eq!(held.check(&gate), Change::None, "the same name: nothing asked again");
        assert!(held.due(&gate).is_empty(), "nothing dialled while A is named");
        std::fs::write(&path, handed(PHONE_B)).unwrap();
        assert_eq!(held.check(&gate), Change::Renamed, "B named: ask again");
        assert_eq!(names(&held.due(&gate)), ["B._carplay-ctrl._tcp.local."], "and dial the record held for it");
        assert_eq!(held.check(&gate), Change::None, "the name unchanged since");
        assert!(held.due(&gate).is_empty(), "so nothing is dialled twice");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_record_removed_meanwhile_is_asked_for_again_not_remembered() {
        // 10:54:26.745 "_carplay-ctrl removed": the fresh resolve the browse asks for is dialled.
        let (gate, path) = gate_naming(PHONE_A);
        let mut held = Held::new(&gate);
        held.keep(ctrl("B._carplay-ctrl._tcp.local.", PHONE_B, 101), FRESH_ROUNDS);
        held.forget("B._carplay-ctrl._tcp.local.");
        std::fs::write(&path, handed(PHONE_B)).unwrap();
        assert_eq!(held.check(&gate), Change::Renamed, "asked again");
        assert!(held.due(&gate).is_empty(), "no record kept for it");
        assert_eq!(gate.dial(Some(PHONE_B)).0, Dial::Now, "the fresh resolve is dialled");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_live_session_is_not_dialled_again_when_its_name_comes_back() {
        // R4: A's session is live; btd names B early, B's session ends without a 0x5703, A is put back.
        let (gate, path) = gate_naming(PHONE_A);
        let mut held = Held::new(&gate);
        assert_eq!(gate.on_setup(1, IpAddr::from([192, 168, 43, 100]), Some(PHONE_A)), crate::phone_gate::Setup::Allow);
        assert_eq!(gate.dial(Some(PHONE_A)).0, Dial::Live);
        std::fs::write(&path, PHONE_B).unwrap();
        assert_eq!(held.check(&gate), Change::Renamed);
        held.keep(ctrl("A._carplay-ctrl._tcp.local.", PHONE_A, 100), FRESH_ROUNDS);
        held.keep(ctrl("C._carplay-ctrl._tcp.local.", PHONE_C, 102), FRESH_ROUNDS);
        std::fs::write(&path, handed(PHONE_A)).unwrap();
        assert_eq!(held.check(&gate), Change::Renamed);
        assert!(held.due(&gate).is_empty(), "A's session is live, and C is another phone");
        assert_eq!(gate.dial(Some(PHONE_A)).0, Dial::Live, "nor is a fresh resolve of A dialled");
        gate.session_ended(1);
        assert_eq!(gate.dial(Some(PHONE_A)).0, Dial::Now, "once its session ends it may be");
        let _ = std::fs::remove_file(path);
    }

    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::rc::Rc;

    /// What a fake browse's channel gives when read.
    enum Step {
        Ev(ServiceEvent),
        Quiet,
        /// btd writes this text into the name file, and the watch wakes the loop.
        Name(String),
        /// btd removes the name file.
        Unname,
        /// An interface or address comes up: the address watch wakes the loop, and this is the new look.
        Up(&'static str, &'static str),
    }

    /// mdns-sd, scripted: each start hands out the next channel, and none once they run out (its daemon gone).
    /// A stop closes the live channel, as a stop mdns-sd queued does at its next wake, refused or not.
    struct FakeBrowse {
        channels: RefCell<VecDeque<Vec<Step>>>,
        live: RefCell<Option<Rc<Cell<bool>>>>,
        starts: Cell<u32>,
        interfaces: Rc<Cell<u32>>,
        stop_refused: bool,
        file: PathBuf,
        /// The host's addresses as the address watch's look sees them.
        addrs: Rc<RefCell<BTreeSet<(String, IpAddr)>>>,
        dirty: Arc<AtomicBool>,
    }

    impl FakeBrowse {
        fn new(file: &std::path::Path, stop_refused: bool, channels: Vec<Vec<Step>>) -> Self {
            Self {
                channels: RefCell::new(channels.into()),
                live: RefCell::new(None),
                starts: Cell::new(0),
                interfaces: Rc::new(Cell::new(0)),
                stop_refused,
                file: file.to_path_buf(),
                addrs: Rc::new(RefCell::new(BTreeSet::new())),
                dirty: Arc::new(AtomicBool::new(false)),
            }
        }

        /// The address watch over this fake's addresses, for an advertised `adv`.
        fn net(&self, adv: &str) -> Net {
            let addrs = self.addrs.clone();
            Net::new(adv.parse().ok(), true, self.dirty.clone(), Box::new(move || addrs.borrow().clone()))
        }
    }

    impl Browser for FakeBrowse {
        fn start(&self) -> Result<Events, String> {
            let steps = self.channels.borrow_mut().pop_front().ok_or_else(|| "the daemon is gone".to_string())?;
            self.starts.set(self.starts.get() + 1);
            let stopped = Rc::new(Cell::new(false));
            *self.live.borrow_mut() = Some(stopped.clone());
            let mut steps: VecDeque<Step> = steps.into();
            let file = self.file.clone();
            let addrs = self.addrs.clone();
            let dirty = self.dirty.clone();
            Ok(Box::new(move |_: Option<Duration>| {
                if stopped.get() {
                    return Recv::Gone;
                }
                match steps.pop_front() {
                    Some(Step::Ev(ev)) => Recv::Event(ev),
                    Some(Step::Quiet) => Recv::Quiet,
                    Some(Step::Name(n)) => {
                        std::fs::write(&file, n).unwrap();
                        Recv::Woken
                    }
                    Some(Step::Unname) => {
                        let _ = std::fs::remove_file(&file);
                        Recv::Woken
                    }
                    Some(Step::Up(name, ip)) => {
                        addrs.borrow_mut().insert((name.to_string(), ip.parse().unwrap()));
                        dirty.store(true, Ordering::Release);
                        Recv::Woken
                    }
                    None => Recv::Gone,
                }
            }))
        }

        fn stop(&self) -> Result<(), String> {
            if let Some(s) = self.live.borrow().as_ref() {
                s.set(true);
            }
            if self.stop_refused {
                Err("sending on a full channel (Again)".to_string())
            } else {
                Ok(())
            }
        }

        fn interfaces_changed(&self) {
            self.interfaces.set(self.interfaces.get() + 1);
        }
    }

    fn resolved_ev(instance: &str, id: &str, last: u8) -> ServiceEvent {
        let props: HashMap<String, String> = [("id".to_string(), id.to_string())].into_iter().collect();
        let ip = format!("192.168.43.{last}");
        let info = ServiceInfo::new(CTRL_SERVICE, instance, "phone.local.", ip.as_str(), 7000, props).unwrap();
        ServiceEvent::ServiceResolved(info)
    }

    fn resolved(instance: &str, id: &str, last: u8) -> Step {
        Step::Ev(resolved_ev(instance, id, last))
    }

    fn removed(instance: &str) -> Step {
        Step::Ev(ServiceEvent::ServiceRemoved(CTRL_SERVICE.into(), format!("{instance}.{CTRL_SERVICE}")))
    }

    fn at(last: u8) -> SocketAddr {
        SocketAddr::from(([192, 168, 43, last], 7000))
    }

    /// A dialer that records each connect; `accept` decides it.
    fn dialer(dials: &Rc<RefCell<Vec<SocketAddr>>>, accept: impl Fn(SocketAddr) -> bool + 'static) -> Dialer {
        let seen = dials.clone();
        Dialer {
            iface: "lo".to_string(),
            adv_addr: String::new(),
            dialed: HashSet::new(),
            dialed_by: HashMap::new(),
            connect: Box::new(move |sa| {
                seen.borrow_mut().push(sa);
                accept(sa)
            }),
            pause: Duration::ZERO,
        }
    }

    /// Run the loop over `fake` until its channels run out. Retries come `retry_first` after a miss (a long time: never,
    /// in a test that is not about them).
    fn run_loop(fake: &FakeBrowse, gate: &PhoneGate, d: &mut Dialer, net: &mut Net, retry_first: Duration) -> String {
        let mut held = Held::new(gate);
        let mut retries = Retries::new(retry_first);
        let lp = Loop { gate, dialer: d, held: &mut held, retries: &mut retries, net, poll_name: false };
        browse_loop(fake, lp)
    }

    const NEVER: Duration = Duration::from_secs(3600);

    #[test]
    fn a_refused_stop_or_an_ended_browse_never_ends_discovery() {
        // 2.6.0's carplayd-3: the stop was refused but queued, so the old channel closed; and the live browse stopped.
        let (gate, path) = gate_naming(PHONE_A);
        let fake = FakeBrowse::new(
            &path,
            true,
            vec![
                vec![Step::Name(handed(PHONE_B))],
                vec![
                    resolved("B", PHONE_B, 101),
                    Step::Quiet,
                    Step::Ev(ServiceEvent::SearchStopped(CTRL_SERVICE.into())),
                ],
                vec![],
            ],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| true);
        let why = run_loop(&fake, &gate, &mut d, &mut Net::off(), NEVER);
        assert_eq!(why, "the daemon is gone", "only a browse that will not start ends it");
        assert_eq!(fake.starts.get(), 3, "browsed again after the refused stop, and after the browse stopped");
        assert_eq!(*dials.borrow(), [at(101)], "B dialled from the new browse");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_rename_dials_a_cached_record_two_rounds_and_lets_the_held_ones_go() {
        // 2.6.0's carplayd-1 review: B left 89 s before it was named; the cache still has its old address (.150), and
        // its fresh resolve (.101) must not wait ten rounds behind it. C was held and is no longer cached.
        let (gate, path) = gate_naming(PHONE_A);
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![
                vec![resolved("B", PHONE_B, 150), resolved("C", PHONE_C, 160), Step::Name(handed(PHONE_B))],
                vec![resolved("B", PHONE_B, 150), Step::Quiet, resolved("B", PHONE_B, 101), Step::Quiet],
            ],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |sa| sa != at(150));
        let mut held = Held::new(&gate);
        let mut retries = Retries::new(NEVER);
        let mut net = Net::off();
        let lp = Loop { gate: &gate, dialer: &mut d, held: &mut held, retries: &mut retries, net: &mut net, poll_name: false };
        browse_loop(&fake, lp);
        assert_eq!(*dials.borrow(), [at(150), at(150), at(101)], "two rounds from the replay, then the fresh one");
        assert!(held.records.is_empty(), "C, not resolved again, is not held over");
        assert!(retries.list.is_empty(), "the fresh resolve took over the replay's miss");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_dial_stops_before_the_next_address_once_its_phone_is_not_named() {
        let (gate, path) = gate_naming(PHONE_B);
        let dials = Rc::new(RefCell::new(Vec::new()));
        let file = path.clone();
        let mut d = dialer(&dials, move |_| {
            std::fs::write(&file, handed(PHONE_C)).unwrap(); // btd names C while B's first address is tried
            false
        });
        let mut b = ctrl("B", PHONE_B, 150);
        b.addrs.push(IpAddr::from([192, 168, 43, 151]));
        assert_eq!(d.dial(&gate, &b, 0, RENAME_ROUNDS), Dialled::Nothing);
        assert_eq!(*dials.borrow(), [at(150)], ".151 is never dialled");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_phone_dialled_before_is_dialled_again_when_it_is_named_again() {
        // carplayd-4: B was dialled at .101 under its name, A was named, then B again with the same record.
        let (gate, path) = gate_naming(PHONE_B);
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![
                vec![resolved("B", PHONE_B, 101), Step::Name(handed(PHONE_A))],
                vec![resolved("B", PHONE_B, 101), Step::Quiet, Step::Name(handed(PHONE_B))],
                vec![Step::Quiet, resolved("B", PHONE_B, 101), Step::Quiet],
            ],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| true);
        run_loop(&fake, &gate, &mut d, &mut Net::off(), NEVER);
        assert_eq!(*dials.borrow(), [at(101), at(101)], "again once named again; its re-announce after that is not");
        let _ = std::fs::remove_file(path);
    }

    /// A2's dial decision, through the loop: before any phone is named nobody is dialled; the named phone's record waits
    /// for its 0x5703 and is dialled the moment the mark comes, with no further mDNS event; another phone never is.
    #[test]
    fn the_named_phone_is_dialled_when_its_0x5703_is_sent_and_not_before() {
        let (gate, path) = gate_with("");
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![
                vec![
                    resolved("B", PHONE_B, 101),
                    resolved("C", PHONE_C, 102),
                    Step::Quiet,
                    Step::Name(PHONE_B.to_string()),
                ],
                vec![resolved("B", PHONE_B, 101), resolved("C", PHONE_C, 102), Step::Quiet, Step::Name(handed(PHONE_B))],
            ],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let file = path.clone();
        let mut d = dialer(&dials, move |_| {
            assert!(std::fs::read_to_string(&file).unwrap().ends_with(" wifi"), "never dialled before its 0x5703");
            true
        });
        run_loop(&fake, &gate, &mut d, &mut Net::off(), NEVER);
        assert_eq!(*dials.borrow(), [at(101)], "B at its 0x5703, from its held record; C never");
        let _ = std::fs::remove_file(path);
    }

    /// The bench lever dials the named phone at its first resolve, before its 0x5703; still never another phone, and
    /// never before a phone is named.
    #[test]
    fn the_bench_lever_dials_at_the_first_resolve() {
        let path = scratch("expect");
        let _ = std::fs::remove_file(&path);
        let lever = scratch("lever");
        std::fs::write(&lever, "").unwrap();
        let gate = PhoneGate::with_bench(Some(path.clone()), lever.clone());
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![
                vec![resolved("B", PHONE_B, 101), Step::Name(PHONE_B.to_string())],
                vec![resolved("B", PHONE_B, 101), resolved("C", PHONE_C, 102), Step::Quiet, Step::Name(handed(PHONE_B))],
            ],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| true);
        run_loop(&fake, &gate, &mut d, &mut Net::off(), NEVER);
        assert_eq!(*dials.borrow(), [at(101)], "once, as soon as it is named, before its 0x5703; not again at it");
        let _ = std::fs::remove_file(lever);
        let _ = std::fs::remove_file(path);
    }

    /// A rename while the named phone waits for its 0x5703: the first phone's record is not dialled (another phone is
    /// named now), the second waits for its own mark, and a word in the file that is not the mark clears nothing.
    #[test]
    fn a_rename_mid_wait_waits_for_the_new_phones_own_0x5703() {
        let (gate, path) = gate_with(PHONE_A);
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![
                vec![resolved("A", PHONE_A, 100), resolved("B", PHONE_B, 101), Step::Quiet, Step::Name(PHONE_B.to_string())],
                vec![
                    resolved("A", PHONE_A, 100),
                    resolved("B", PHONE_B, 101),
                    Step::Quiet,
                    Step::Name(format!("{PHONE_B} later")),
                    Step::Name(handed(PHONE_B)),
                ],
            ],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| true);
        run_loop(&fake, &gate, &mut d, &mut Net::off(), NEVER);
        assert_eq!(*dials.borrow(), [at(101)], "only B, only at its own 0x5703");
        let _ = std::fs::remove_file(path);
    }

    /// No phone named any more (a put-back to none) while a record waits: it is not dialled.
    #[test]
    fn a_name_removed_while_waiting_dials_nobody() {
        let (gate, path) = gate_with(PHONE_B);
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![vec![resolved("B", PHONE_B, 101), Step::Unname], vec![resolved("B", PHONE_B, 101), Step::Quiet]],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| true);
        run_loop(&fake, &gate, &mut d, &mut Net::off(), NEVER);
        assert!(dials.borrow().is_empty());
        let _ = std::fs::remove_file(path);
    }

    /// A2, carplayd started before the AP: when the AP's address comes up, mdns-sd is told to read the interfaces again
    /// and announce again, and the browse starts again so its query goes out there; an address it had already is no
    /// news, and neither is one going away.
    #[test]
    fn an_address_that_comes_up_after_the_start_is_announced_and_browsed_on() {
        let (gate, path) = gate_naming(PHONE_B);
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![
                vec![Step::Quiet, Step::Up("wlan0", "192.168.43.1")],
                vec![Step::Quiet, Step::Up("wlan0", "192.168.43.1"), resolved("B", PHONE_B, 101), Step::Quiet],
                vec![],
            ],
        );
        let mut net = fake.net("192.168.43.1");
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| true);
        run_loop(&fake, &gate, &mut d, &mut net, NEVER);
        assert_eq!(fake.interfaces.get(), 1, "told once: the second look found nothing new");
        assert_eq!(fake.starts.get(), 3, "browsed again at the address, and after the second channel ended");
        assert_eq!(*dials.borrow(), [at(101)], "and the phone found there is dialled");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_address_watch_reports_only_an_address_newly_up() {
        let addrs = Rc::new(RefCell::new(BTreeSet::new()));
        let dirty = Arc::new(AtomicBool::new(false));
        let a = addrs.clone();
        let mut net = Net::new("192.168.43.1".parse().ok(), true, dirty.clone(), Box::new(move || a.borrow().clone()));
        assert!(!net.changed(), "nothing marked");
        addrs.borrow_mut().insert(("wlan0".into(), "192.168.43.1".parse().unwrap()));
        assert!(!net.changed(), "not marked by the watch: not looked at");
        dirty.store(true, Ordering::Release);
        assert!(net.changed(), "the AP's address");
        dirty.store(true, Ordering::Release);
        assert!(!net.changed(), "the same again");
        addrs.borrow_mut().clear();
        dirty.store(true, Ordering::Release);
        assert!(!net.changed(), "gone: nothing to announce on");
        addrs.borrow_mut().insert(("wlan0".into(), "fe80::1".parse().unwrap()));
        dirty.store(true, Ordering::Release);
        assert!(net.changed(), "a link-local address too");
        assert_eq!(net.wait(), None, "with the watch nothing is looked at on a timer");

        // Without the watch: a look every FALLBACK_STEP until the advertised address is up.
        let addrs = Rc::new(RefCell::new(BTreeSet::new()));
        let a = addrs.clone();
        let mut fallback = Net::new("192.168.43.1".parse().ok(), false, dirty, Box::new(move || a.borrow().clone()));
        assert_eq!(fallback.wait(), Some(FALLBACK_STEP));
        assert!(!fallback.changed());
        addrs.borrow_mut().insert(("sta0".into(), "192.168.43.1".parse().unwrap()));
        assert!(fallback.changed(), "found on the next look");
        assert_eq!(fallback.wait(), None, "and the looks end");
        assert!(!Net::off().changed(), "the wired carplayd looks at nothing");
    }

    /// carplayd-1: a dial that tried every round and missed is tried again later, while its record stands; a removal
    /// ends the retries.
    #[test]
    fn a_dial_that_missed_is_tried_again() {
        let (gate, path) = gate_naming(PHONE_B);
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![vec![resolved("B", PHONE_B, 101), Step::Quiet, Step::Quiet, Step::Quiet, removed("B"), Step::Quiet, Step::Quiet]],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let tries = Rc::new(Cell::new(0));
        let t = tries.clone();
        let mut d = dialer(&dials, move |_| {
            t.set(t.get() + 1);
            t.get() > FRESH_ROUNDS as usize + RENAME_ROUNDS as usize
        });
        run_loop(&fake, &gate, &mut d, &mut Net::off(), Duration::ZERO);
        let n = dials.borrow().len();
        assert_eq!(n, FRESH_ROUNDS as usize + 2 * RENAME_ROUNDS as usize - 1, "the fresh rounds, a retry that missed, one that got through");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_retry_waits_while_a_control_connection_is_open_and_ends_with_its_record() {
        let (gate, path) = gate_naming(PHONE_B);
        let mut retries = Retries::new(Duration::ZERO);
        retries.add(ctrl("B", PHONE_B, 101));
        gate.connection_opened(9);
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| true);
        let mut held = Held::new(&gate);
        let mut net = Net::off();
        let mut lp = Loop { gate: &gate, dialer: &mut d, held: &mut held, retries: &mut retries, net: &mut net, poll_name: false };
        lp.retry();
        assert!(dials.borrow().is_empty(), "the open connection may be the phone's own");
        assert_eq!(lp.retries.list.len(), 1, "it waits");
        lp.gate.session_ended(9);
        lp.retries.list[0].at = Instant::now();
        lp.retry();
        assert_eq!(*dials.borrow(), [at(101)], "dialled once the connection is gone");
        lp.retries.add(ctrl("B", PHONE_B, 101));
        lp.retries.drop_for("B");
        assert!(lp.retries.next().is_none(), "a removal or a fresh resolve drops it");
        let _ = std::fs::remove_file(path);
    }

    /// carplayd-2: a record that is news, read in a rename's replay window, gets the fresh rounds; the same record read
    /// again as it was gets the capped ones.
    #[test]
    fn a_fresh_resolve_in_the_replay_window_gets_the_fresh_rounds() {
        let (gate, path) = gate_naming(PHONE_A);
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![
                vec![resolved("B", PHONE_B, 150), Step::Name(handed(PHONE_B))],
                // The replay: B as seen (.150), then B's fresh resolve at .101 inside the same window.
                vec![resolved("B", PHONE_B, 150), resolved("B", PHONE_B, 101), Step::Quiet],
            ],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| false);
        run_loop(&fake, &gate, &mut d, &mut Net::off(), NEVER);
        let at150 = dials.borrow().iter().filter(|&&sa| sa == at(150)).count();
        let at101 = dials.borrow().iter().filter(|&&sa| sa == at(101)).count();
        assert_eq!((at150, at101), (RENAME_ROUNDS as usize, FRESH_ROUNDS as usize), "replayed: capped; news: fresh");
        let _ = std::fs::remove_file(path);
    }

    /// carplayd-3: btd names B; before discovery acts on the change, B's fresh resolve is dialled under the new name.
    /// The rename that follows forgets only B's dials from an earlier naming, so B is not dialled a second time.
    #[test]
    fn a_dial_made_under_the_new_name_is_not_repeated_by_its_rename() {
        let (gate, path) = gate_naming(PHONE_A);
        let fake = FakeBrowse::new(
            &path,
            false,
            vec![vec![Step::Quiet], vec![resolved("B", PHONE_B, 101), Step::Quiet]],
        );
        let dials = Rc::new(RefCell::new(Vec::new()));
        let mut d = dialer(&dials, |_| true);
        let mut held = Held::new(&gate);
        // The file names B (handed off) before the loop has looked: B's resolve is dialled as it comes.
        std::fs::write(&path, handed(PHONE_B)).unwrap();
        let mut retries = Retries::new(NEVER);
        let mut net = Net::off();
        let mut lp = Loop { gate: &gate, dialer: &mut d, held: &mut held, retries: &mut retries, net: &mut net, poll_name: false };
        lp.on_event(resolved_ev("B", PHONE_B, 101));
        assert_eq!(*dials.borrow(), [at(101)]);
        browse_loop(&fake, lp);
        assert_eq!(*dials.borrow(), [at(101)], "the rename's replay finds it dialled under this naming");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_log_shows_a_phone_by_its_tail_and_a_hash() {
        // carplayd-5: an instance is usually its owner's name for the phone.
        let who = who("Owner's iPhone._carplay-ctrl._tcp.local.", Some(PHONE_B));
        assert!(who.starts_with("sha256:") && who.ends_with("(..00:0B)"), "{who}");
        assert!(!who.contains("Owner"));
        assert_eq!(id_tail("not an address"), "id ?");
        assert_eq!(txt_line([("id", PHONE_B), ("srcvers", "1005.13.1")].into_iter()), "id=..00:0B srcvers=1005.13.1");
    }

    #[test]
    fn a_record_is_the_same_whatever_the_order_of_its_addresses() {
        let mut a = ctrl("B", PHONE_B, 101);
        a.addrs.push(IpAddr::from([192, 168, 43, 100]));
        let mut b = a.clone();
        b.addrs.reverse();
        assert!(a.same(&b));
        b.addrs.pop();
        assert!(!a.same(&b), "an address fewer is news");
        let mut c = a.clone();
        c.id = None;
        assert!(!a.same(&c), "its id gone is news");
    }
}
