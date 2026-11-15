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

use crate::phone_gate::PhoneGate;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV6, TcpStream};
use std::time::{Duration, Instant};

const PORT: u16 = 5000;

/// The phone's CarPlay control service, which discovery browses and dials.
const CTRL_SERVICE: &str = "_carplay-ctrl._tcp.local.";

/// How long the browse loop waits for an mDNS event before it reads the phone btd names again ([`Held`]).
const NAME_CHECK: Duration = Duration::from_secs(1);

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

/// The interface holding the IPv4 `addr`, if any. Wireless carplayd advertises on the AP gateway
/// (`RX_ADDR`), and the interface holding that address is the AP's, whatever the chip's bring-up named it:
/// btd passes `RX_IFACE=wlan0`, but on the 2026-11-02 bench unit the AP was `sta0`, so link-local dials
/// carried `wlan0`'s scope and failed with "Network unreachable", ten rounds at a time.
fn iface_holding(addr: Ipv4Addr) -> Option<String> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `ifap` with a list this function frees below; every node is read only
    // between the two calls, and `ifa_addr` is checked for null and AF_INET before it is read as IPv4.
    unsafe {
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut found = None;
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null() && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                if Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)) == addr {
                    found = Some(CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned());
                    break;
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        found
    }
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

/// A resolved `_carplay-ctrl` record, as discovery keeps it.
#[derive(Debug, Clone)]
struct Ctrl {
    fullname: String,
    /// The TXT `id`: the phone's BR/EDR address (2026-11-01 bench), the same address btd names.
    id: Option<String>,
    addrs: Vec<IpAddr>,
    port: u16,
}

/// What discovery does with a resolved record, by the phone gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Dial,
    /// Another phone than btd serves (fix 1b): not dialled, and held ([`Held`]).
    Elsewhere,
    /// The phone btd serves, its session already live here (bench 2026-11-02).
    Live,
}

fn verdict(gate: &PhoneGate, id: Option<&str>) -> Verdict {
    if !gate.wants(id) {
        Verdict::Elsewhere
    } else if gate.serving(id) {
        Verdict::Live
    } else {
        Verdict::Dial
    }
}

/// The records left alone because btd named another phone when they resolved, and the name discovery last
/// acted on. The Mac's 15 Nov bench: a phone that knows the AP joins it within ~1.5 s of its Bluetooth link,
/// before its 0x5703 names it, so its record resolved while the last phone was still named and was refused.
/// Nothing looked again when the name changed, and its dial waited for the browser's own re-query, 84 s
/// later; its record had been removed meanwhile. So a name change dials the held records the gate now lets
/// through, and asks `_carplay-ctrl` again for a record that went away (R1).
struct Held {
    named: Option<String>,
    records: HashMap<String, Ctrl>,
}

impl Held {
    fn new(gate: &PhoneGate) -> Self {
        Self { named: gate.named(), records: HashMap::new() }
    }

    fn keep(&mut self, c: Ctrl) {
        self.records.insert(c.fullname.clone(), c);
    }

    fn forget(&mut self, fullname: &str) {
        self.records.remove(fullname);
    }

    /// Whether the phone btd names changed since the last call.
    fn renamed(&mut self, gate: &PhoneGate) -> bool {
        let now = gate.named();
        if now == self.named {
            return false;
        }
        self.named = now;
        true
    }

    /// The held records the gate lets through now, taken out to be dialled: never one whose session is
    /// live here, nor another phone's.
    fn due(&mut self, gate: &PhoneGate) -> Vec<Ctrl> {
        let due: Vec<String> = self
            .records
            .values()
            .filter(|c| verdict(gate, c.id.as_deref()) == Verdict::Dial)
            .map(|c| c.fullname.clone())
            .collect();
        due.iter().filter_map(|k| self.records.remove(k)).collect()
    }
}

/// Dials the phones' control ports, and remembers what it dialled.
struct Dialer {
    dev_dec: u64,
    iface: String,
    adv_addr: String,
    dialed: HashSet<String>,
    /// The addresses dialed for each phone's service, so a phone that leaves forgets only its own.
    dialed_by: HashMap<String, Vec<String>>,
}

impl Dialer {
    /// Scope for IPv6 link-local dials, resolved NOW rather than once at startup: the interface holding
    /// the advertised AP address, else RX_IFACE.
    fn scope(&self) -> (String, u32) {
        let ap_iface = self.adv_addr.parse::<Ipv4Addr>().ok().and_then(iface_holding);
        let scope_iface = ap_iface.unwrap_or_else(|| self.iface.clone());
        let scope = iface_scope(&scope_iface);
        (scope_iface, scope)
    }

    fn dial(&mut self, gate: &PhoneGate, c: &Ctrl, scope: u32) {
        // Dial the resolved ADDRESS(es), not the hostname (box has no .local resolver). Mark a
        // peer dialed only on SUCCESS, retrying transient failures with a short backoff (the
        // source address can lag the mDNS resolve by a few seconds right after association).
        // Interleave addresses per attempt round (audit Fix #12): try EACH resolved address once
        // per pass, THEN sleep — so a stale/unreachable address listed first (e.g. an IPv6
        // link-local that lags association) can't burn its whole ~40s retry budget before the
        // good address is tried, and can't block this browse thread from processing queued mDNS
        // events (incl. ServiceRemoved) for that long. One accepted dial is enough — the iPhone
        // opens RTSP off it (we don't dial the other family too).
        'rounds: for attempt in 1..=10u32 {
            // The phone btd names can change while this retries (a Bluetooth session's start, its put-back, a
            // 0x5703): stop dialling a phone that is no longer it, or whose session came up meanwhile.
            if attempt > 1 && verdict(gate, c.id.as_deref()) != Verdict::Dial {
                println!("[rx] {}: not the phone to dial any more; stopped", c.fullname);
                break 'rounds;
            }
            let mut tried = false;
            for ip in &c.addrs {
                let sa = dialable(*ip, c.port, scope);
                if self.dialed.contains(&sa.to_string()) {
                    continue;
                }
                // A phone the gate refused at SETUP: the one a switch dropped, still on the AP.
                // Not marked dialed, so a carplayd later told to serve that phone still dials it.
                if gate.is_refused(*ip) {
                    println!("[rx] {sa}: its phone was refused at SETUP; not dialling it");
                    continue;
                }
                tried = true;
                if connect_out(sa, self.dev_dec) {
                    self.dialed.insert(sa.to_string());
                    self.dialed_by.entry(c.fullname.clone()).or_default().push(sa.to_string());
                    break 'rounds;
                }
            }
            // Every resolved address is already dialed (mDNS re-announce / TTL refresh of a
            // connected peer) or none resolved — nothing to attempt. Fall through instantly like
            // the pre-fix address-major loop did, instead of sleeping 10 rounds for nothing (audit
            // Fix #12 v2, regression caught by the gate: without this a repeat ServiceResolved of
            // an already-connected phone would stall this browse thread ~10s).
            if !tried {
                break 'rounds;
            }
            println!("[rx] connect-out round {attempt}/10 — no address accepted, retrying in 1s");
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// The iPhone's control service went away (Wi-Fi drop / sleep). Forget the addresses dialed for
    /// it so it gets re-dialed when it reappears — otherwise `dialed` pins the old address forever
    /// and the phone can never reconnect without restarting the daemon. ONLY its own: this was
    /// the whole set while the box was taken for single-phone, and with a second phone on the AP
    /// (the 2026-11-01 switch bench) one leaving would make discovery dial the other again.
    fn removed(&mut self, fullname: &str) {
        if let Some(addrs) = self.dialed_by.remove(fullname) {
            println!("[rx] _carplay-ctrl removed ({fullname}) — forgetting its {} dialed address(es)", addrs.len());
            for a in addrs {
                self.dialed.remove(&a);
            }
        }
    }
}

fn on_event(ev: ServiceEvent, gate: &PhoneGate, dialer: &mut Dialer, held: &mut Held) {
    match ev {
        ServiceEvent::ServiceResolved(info) => {
            let (scope_iface, scope) = dialer.scope();
            let port = info.get_port();
            let host = info.get_hostname().trim_end_matches('.').to_string();
            let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
            println!(
                "[rx] resolved _carplay-ctrl {} -> {host}:{port} addrs={addrs:?} scope={scope_iface}({scope})",
                info.get_fullname()
            );
            // What the phone puts in its TXT record. Its `id` is its BR/EDR address (2026-11-01 bench),
            // so another phone than the one btd serves is never dialled (fix 1b). Not marked dialed, and
            // held: when btd names that phone, it is dialled from here at once (the Mac's 15 Nov bench).
            // The phone gate still refuses another phone at SETUP, for a TXT without an id.
            let txt: Vec<String> =
                info.get_properties().iter().map(|p| format!("{}={}", p.key(), p.val_str())).collect();
            println!("[rx] _carplay-ctrl TXT: {}", txt.join(" "));
            let id = info
                .get_properties()
                .iter()
                .find(|p| p.key().eq_ignore_ascii_case("id"))
                .map(|p| p.val_str().to_string());
            let c = Ctrl { fullname: info.get_fullname().to_string(), id, addrs, port };
            match verdict(gate, c.id.as_deref()) {
                Verdict::Elsewhere => {
                    println!(
                        "[rx] {}: TXT id {} is another phone than btd serves; not dialling it",
                        c.fullname,
                        c.id.as_deref().unwrap_or_default()
                    );
                    held.keep(c);
                }
                // The phone btd serves, with its session already live here: a record that changed
                // addresses mid-session is the same phone, and dialling it again ends the session that
                // is showing video (bench 2026-11-02: dialled at .101, the record grew .100, a second
                // connect to .100 got 200 OK, and the session was torn down 29 s after video started).
                Verdict::Live => {
                    held.forget(&c.fullname);
                    println!("[rx] {}: its session is live here; not dialling it again at {:?}", c.fullname, c.addrs);
                }
                Verdict::Dial => {
                    held.forget(&c.fullname);
                    dialer.dial(gate, &c, scope);
                }
            }
        }
        ServiceEvent::ServiceRemoved(_ty, fullname) => {
            held.forget(&fullname);
            dialer.removed(&fullname);
        }
        _ => {}
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
    mdns.register(svc)?;
    println!("[rx] advertised _airplay._tcp 'CarPlay' :{PORT}");

    let mut browse = mdns.browse(CTRL_SERVICE)?;
    let mut dialer = Dialer { dev_dec, iface, adv_addr, dialed: HashSet::new(), dialed_by: HashMap::new() };
    let mut held = Held::new(gate);
    loop {
        // Woken once a second without an event, to read the name btd wrote (`Held`).
        match browse.recv_timeout(NAME_CHECK) {
            Ok(ev) => on_event(ev, gate, &mut dialer, &mut held),
            Err(_) if browse.is_disconnected() => break,
            Err(_) => {}
        }
        if !held.renamed(gate) {
            continue;
        }
        // The phone btd serves changed (the Mac's 15 Nov bench, R1). Ask `_carplay-ctrl` again: a fresh browse
        // queries at once (and again 1 s, 2 s, ... later, as any new browse does), so a phone on the AP whose
        // record went away answers within about a second; a record mdns-sd still holds comes back from its
        // cache, and one already dialled or live is left alone as ever. The old channel is read to its end
        // (`SearchStopped`) before the new one, so a removal already sent there is not lost.
        println!("[rx] the phone btd serves changed: asking _carplay-ctrl again");
        match mdns.stop_browse(CTRL_SERVICE) {
            Ok(()) => {
                // mdns-sd refuses a command only while its queue is full (`Error::Again`): tried for about a
                // second, and still refused, discovery is down, as when the first browse fails.
                let mut tries = 0;
                let fresh = loop {
                    match mdns.browse(CTRL_SERVICE) {
                        Ok(r) => break r,
                        Err(e) if tries < 10 => {
                            tries += 1;
                            println!("[rx] browse _carplay-ctrl again: {e}; retrying");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        Err(e) => return Err(e.into()),
                    }
                };
                let old = std::mem::replace(&mut browse, fresh);
                let until = Instant::now() + Duration::from_secs(1);
                let mut left = Vec::new();
                while let Ok(ev) = old.recv_deadline(until) {
                    if matches!(ev, ServiceEvent::SearchStopped(_)) {
                        break;
                    }
                    left.push(ev);
                }
                for ev in left {
                    on_event(ev, gate, &mut dialer, &mut held);
                }
                // The new browse's first answers, from mdns-sd's cache, before the held records: a record it
                // resolves again is dialled once, from there, and no longer held.
                while let Ok(ev) = browse.recv_timeout(Duration::from_millis(300)) {
                    on_event(ev, gate, &mut dialer, &mut held);
                }
            }
            Err(e) => println!("[rx] could not ask _carplay-ctrl again ({e}); dialling what it resolved before"),
        }
        // A record that resolved while another phone was named, and is still there.
        for c in held.due(gate) {
            println!("[rx] {}: resolved while btd named another phone; dialling it now", c.fullname);
            let (_, scope) = dialer.scope();
            dialer.dial(gate, &c, scope);
        }
    }
    // The browse channel only closes when mdns-sd's daemon thread has exited (0.11.5 delivers
    // ServiceFound/Resolved/Removed with a BLOCKING `send`, never dropping the querier on a full
    // channel, and nothing here ever calls `shutdown()`; `stop_browse` above is followed by a new
    // browse at once), so this is "mDNS died mid-session" — an Err, so `start` treats it exactly
    // like a construction failure rather than ending quietly.
    Err("browse channel closed — the mDNS daemon thread exited".into())
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
        assert_eq!(iface_holding(Ipv4Addr::LOCALHOST).as_deref(), Some("lo"));
        assert_eq!(iface_holding(Ipv4Addr::new(192, 0, 2, 77)), None, "an address no interface holds");
    }

    // The Mac's 15 Nov bench: phone A was named, phone B had handed off before.
    const PHONE_A: &str = "68:a7:29:00:25:10";
    const PHONE_B: &str = "aa:bb:cc:00:6a:de";

    fn gate_naming(phone: &str) -> (PhoneGate, std::path::PathBuf) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "discovery_expect_test_{}_{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, phone).unwrap();
        (PhoneGate::new(Some(path.clone())), path)
    }

    fn ctrl(fullname: &str, id: &str, last: u8) -> Ctrl {
        Ctrl {
            fullname: fullname.to_string(),
            id: Some(id.to_string()),
            addrs: vec![IpAddr::from([192, 168, 43, last])],
            port: 7000,
        }
    }

    fn names(due: &[Ctrl]) -> Vec<&str> {
        due.iter().map(|c| c.fullname.as_str()).collect()
    }

    #[test]
    fn a_phone_refused_before_btd_named_it_is_dialled_once_it_is_named() {
        // 10:54:26.243 B's record resolved while the file named A; B's 0x5703 named it 0.3 s later.
        let (gate, path) = gate_naming(PHONE_A);
        let mut held = Held::new(&gate);
        let b = ctrl("B._carplay-ctrl._tcp.local.", PHONE_B, 101);
        assert_eq!(verdict(&gate, b.id.as_deref()), Verdict::Elsewhere);
        held.keep(b);
        assert!(!held.renamed(&gate), "the same name: nothing asked again");
        assert!(held.due(&gate).is_empty(), "nothing dialled while A is named");
        std::fs::write(&path, PHONE_B).unwrap();
        assert!(held.renamed(&gate), "B named: ask again");
        assert_eq!(names(&held.due(&gate)), ["B._carplay-ctrl._tcp.local."], "and dial the record held for it");
        assert!(!held.renamed(&gate), "the name unchanged since");
        assert!(held.due(&gate).is_empty(), "so nothing is dialled twice");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_record_removed_meanwhile_is_asked_for_again_not_remembered() {
        // 10:54:26.745 "_carplay-ctrl removed": the fresh resolve the browse asks for is dialled.
        let (gate, path) = gate_naming(PHONE_A);
        let mut held = Held::new(&gate);
        held.keep(ctrl("B._carplay-ctrl._tcp.local.", PHONE_B, 101));
        held.forget("B._carplay-ctrl._tcp.local.");
        std::fs::write(&path, PHONE_B).unwrap();
        assert!(held.renamed(&gate), "asked again");
        assert!(held.due(&gate).is_empty(), "no record kept for it");
        assert_eq!(verdict(&gate, Some(PHONE_B)), Verdict::Dial, "the fresh resolve is dialled");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_live_session_is_not_dialled_again_when_its_name_comes_back() {
        // R4: A's session is live; btd names B early, B's session ends without a 0x5703, A is put back.
        let (gate, path) = gate_naming(PHONE_A);
        let mut held = Held::new(&gate);
        assert!(gate.on_setup(1, IpAddr::from([192, 168, 43, 100]), Some(PHONE_A)));
        assert_eq!(verdict(&gate, Some(PHONE_A)), Verdict::Live);
        std::fs::write(&path, PHONE_B).unwrap();
        assert!(held.renamed(&gate));
        held.keep(ctrl("A._carplay-ctrl._tcp.local.", PHONE_A, 100));
        held.keep(ctrl("C._carplay-ctrl._tcp.local.", "02:00:00:00:00:0c", 102));
        std::fs::write(&path, PHONE_A).unwrap();
        assert!(held.renamed(&gate));
        assert!(held.due(&gate).is_empty(), "A's session is live, and C is another phone");
        assert_eq!(verdict(&gate, Some(PHONE_A)), Verdict::Live, "nor is a fresh resolve of A dialled");
        gate.session_ended(1);
        assert_eq!(verdict(&gate, Some(PHONE_A)), Verdict::Dial, "once its session ends it may be");
        let _ = std::fs::remove_file(path);
    }
}
