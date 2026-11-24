//! The Wi-Fi link: the OCBM session over TCP on the phone's hotspot (OCBW v1, crate `ocbm-link`), beside the USB
//! accessory.
//!
//! OFF unless [`LINK_FILE`] holds a link key. With none, ocbmd is the USB daemon it was in 2.7.1: no thread, no
//! socket and no file of this module's is opened, and the only differences a host can see are the `CAP_WIFI_LINK`
//! bit, `MGMT_WIFI_LINK` answered, and `"wifi_link":{"state":"off",..}` in MGMT_INFO.
//!
//! ON (a host set the link up with `MGMT_WIFI_LINK`, over whatever session it had):
//!
//! - the station keeper (`/script/wifi_sta.sh`) keeps the station interface joined to the hotspot and publishes its
//!   state in [`STA_STATE`];
//! - [`start_listener`]'s thread listens on the STATION INTERFACE'S ADDRESS only, port 7020, never on the AP side
//!   (the phone's) and never on 0.0.0.0. Handshakes run on threads of their own, at most [`MAX_HANDSHAKES`] at once
//!   and one per peer address, each within `ocbm_link::HANDSHAKE_TIMEOUT`, so a stranger on the hotspot costs only
//!   itself;
//! - an authenticated connection becomes a [`Link`]: a reader and a writer thread and two pipes, shaped like the USB
//!   relay's, so the poll loop reads and writes it as it does the accessory. The reader ends the link after
//!   `ocbm_link::DEAD_AFTER` (3 s) with nothing received; the writer sends a keepalive after
//!   `ocbm_link::KEEPALIVE_AFTER` (1 s) with nothing to send, and ends the link when one write has been stuck 3 s;
//! - ONE HOST AT A TIME (main.rs `Daemon::admit`): the first host whose HELLO the box takes holds it, over USB or
//!   Wi-Fi, until it leaves (CT_STOP, its link ends, or [`HOLD_SILENCE`] without a frame). A Wi-Fi host refused
//!   meanwhile has its connection closed at once, before any OCBM byte. USB ALWAYS WINS: a USB host's frame takes the
//!   box from a Wi-Fi holder at once (its link closes), and no USB frame is ever dropped.
//!
//! The mDNS advert for the hotspot is `mdns.rs`.

use ocbm_link::{LinkKey, Session};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The link key and the hotspot (`key=` / `ssid=` / `psk=` lines), 0600. On flash: kept across reboots and updates,
/// removed by MGMT_WIFI_LINK's clear, the go-back to stock and the stock restore (stock runs a passwordless telnetd
/// on its AP, and the hashed hotspot key IS the hotspot's WPA key).
#[cfg(not(test))]
pub const LINK_FILE: &str = "/etc/ocbm/wifi_link";
#[cfg(test)]
pub const LINK_FILE: &str = "/tmp/ocbmd_selftest_wifi_link";
/// The station keeper's state (`wifi_sta.sh`): `iface=`, `state=joining|joined`, `channel=`. tmpfs.
#[cfg(not(test))]
pub const STA_STATE: &str = "/tmp/wifi_link_state";
#[cfg(test)]
pub const STA_STATE: &str = "/tmp/ocbmd_selftest_wifi_link_state";
/// One start in plain USB mode, then gone: set when a USB host took the box on a driver that serializes reads and
/// writes ([`crate::AccIo::Direct`]), where the Wi-Fi side's USB reader would keep the loop's writes out.
#[cfg(not(test))]
pub const USB_ONCE_FLAG: &str = "/tmp/ocbmd_usb_once";
#[cfg(test)]
pub const USB_ONCE_FLAG: &str = "/tmp/ocbmd_selftest_usb_once";
/// The bench's interface lever: the listener binds this interface's address instead of the station's (a wired bench
/// over NCM, say). Never the AP's. Read at each look; "off" or empty reads as unset.
#[cfg(not(test))]
const BENCH_IF_FILE: &str = "/tmp/bench_wifi_link_if";
#[cfg(test)]
const BENCH_IF_FILE: &str = "/tmp/ocbmd_selftest_no_such_bench_wifi_link_if";
/// The station keeper and its single-instance lock (its pid inside).
#[cfg(not(test))]
const KEEPER: &str = "/script/wifi_sta.sh";
#[cfg(test)]
const KEEPER: &str = "/tmp/ocbmd_selftest_no_such_wifi_sta.sh";
#[cfg(not(test))]
const KEEPER_LOCK_PID: &str = "/tmp/.wifi_sta_lock/pid";
#[cfg(test)]
const KEEPER_LOCK_PID: &str = "/tmp/ocbmd_selftest_no_such_wifi_sta_lock/pid";
/// The AP's configuration, for its channel: [`AP_SESSION_CONF`] while the AP runs from it, else this flash file.
pub const HOSTAPD_CONF: &str = "/etc/hostapd.conf";
/// The configuration the AP runs from where it followed the station's channel (radio_ap_up.sh, ADDENDUM 6 of the Wi-Fi
/// link plan): hostapd.conf with that channel, band and HT40 side, in tmpfs, there only while that AP is.
#[cfg(not(test))]
pub const AP_SESSION_CONF: &str = "/tmp/hostapd_session.conf";
#[cfg(test)]
pub const AP_SESSION_CONF: &str = "/tmp/ocbmd_selftest_no_such_hostapd_session.conf";

/// Handshakes under way at once, all peers together.
pub const MAX_HANDSHAKES: usize = 4;
/// Authenticated links whose host has not yet sent its HELLO, at once.
pub const MAX_CANDIDATES: usize = 4;
/// A host that holds the box and has sent nothing for this long no longer holds it against another host. The heartbeat
/// grace's figure: a subscribed host beats every second.
pub const HOLD_SILENCE: Duration = Duration::from_secs(10);
/// An authenticated link that sends no HELLO within this long is closed.
pub const HELLO_WITHIN: Duration = Duration::from_secs(5);

/// The subnets the station must never be on: the box's own AP's and NCM's (box-common `net`).
fn reserved_subnet(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 192 && o[1] == 168 && (o[2] == 43 || o[2] == 50)
}

// ---- the link file ------------------------------------------------------------------------------------------------

/// What [`LINK_FILE`] holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkConf {
    pub key: LinkKey,
    /// The hotspot's name; empty: the key only, no hotspot (the bench's interface lever).
    pub ssid: String,
    /// 64 hex digits (the PBKDF2 hash), or the passphrase, or empty (an open network).
    pub psk: String,
}

/// [`LINK_FILE`]'s text: `key=` (64 hex), and with a hotspot `ssid_hex=` (the name's bytes in hex, as the station
/// keeper hands them to wpa_supplicant: no quoting, and no hexdump needed on the adapter) and `psk=` (as received:
/// 64 hex digits, or a passphrase [`parse_provision`] checked, or empty).
pub fn link_file_text(c: &LinkConf) -> String {
    let mut t = format!(
        "# OCBM Wi-Fi link (MGMT_WIFI_LINK). Secret: never into a log, a backup or a stick.\nkey={}\n",
        c.key.to_hex()
    );
    if !c.ssid.is_empty() {
        t.push_str(&format!("ssid_hex={}\npsk={}\n", ocbm_link::hex(c.ssid.as_bytes()), c.psk));
    }
    t
}

/// The file's text read back: the `key=` line must be 64 hex digits; `ssid_hex=` and `psk=` are optional.
pub fn parse_link_file(text: &str) -> Option<LinkConf> {
    let mut key = None;
    let (mut ssid, mut psk) = (String::new(), String::new());
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("key=") {
            key = LinkKey::from_hex(v);
        } else if let Some(v) = l.strip_prefix("ssid_hex=") {
            let v = v.trim();
            let bytes: Option<Vec<u8>> = (0..v.len() / 2).map(|i| u8::from_str_radix(v.get(2 * i..2 * i + 2)?, 16).ok()).collect();
            ssid = bytes.and_then(|b| String::from_utf8(b).ok()).filter(|_| v.len() % 2 == 0)?;
        } else if let Some(v) = l.strip_prefix("psk=") {
            psk = v.to_string();
        }
    }
    Some(LinkConf { key: key?, ssid, psk })
}

/// The link as stored, or None (no file, or no valid key: then the link is off, and the line says why once).
pub fn read_link_conf(path: &str) -> Option<LinkConf> {
    let text = std::fs::read_to_string(path).ok()?;
    let c = parse_link_file(&text);
    if c.is_none() {
        eprintln!("[ocbmd] wifi: {path} has no valid key= line; the Wi-Fi link stays off");
    }
    c
}

// ---- MGMT_WIFI_LINK -----------------------------------------------------------------------------------------------

/// What `MGMT_WIFI_LINK` asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Provision {
    /// Remove the link: the box is a USB-only box again.
    Clear,
    Set(LinkConf),
}

/// A WPA key the supplicant's configuration can hold: 64 hex digits (the hash), or a passphrase of 8..=63 printable
/// ASCII without `"` (wpa_supplicant's quoted string would end there), or empty (an open network).
fn psk_storable(psk: &str) -> bool {
    psk.is_empty()
        || (psk.len() == 64 && psk.bytes().all(|b| b.is_ascii_hexdigit()))
        || ((8..=63).contains(&psk.len()) && psk.bytes().all(|b| (b' '..=b'~').contains(&b) && b != b'"'))
}

/// `MGMT_WIFI_LINK`'s body (after the verb): empty = [`Provision::Clear`], else
/// `[ver 1][key 32][ssid_len u8][ssid][psk_len u8][psk]`. Refuses anything the file or the supplicant could not hold.
pub fn parse_provision(body: &[u8]) -> Result<Provision, &'static str> {
    if body.is_empty() {
        return Ok(Provision::Clear);
    }
    if body[0] != 1 {
        return Err("an unknown version");
    }
    let key: [u8; 32] = body.get(1..33).ok_or("a short key")?.try_into().map_err(|_| "a short key")?;
    let sl = *body.get(33).ok_or("no ssid length")? as usize;
    let ssid = body.get(34..34 + sl).ok_or("a short ssid")?;
    let pl = *body.get(34 + sl).ok_or("no psk length")? as usize;
    let psk = body.get(35 + sl..35 + sl + pl).ok_or("a short psk")?;
    if body.len() != 35 + sl + pl {
        return Err("trailing bytes");
    }
    let ssid = std::str::from_utf8(ssid).map_err(|_| "an ssid that is not UTF-8")?;
    let psk = std::str::from_utf8(psk).map_err(|_| "a psk that is not UTF-8")?;
    if ssid.len() > 32 || ssid.chars().any(|c| c == '\n' || c == '\r' || c == '\0') {
        return Err("an ssid the file cannot hold");
    }
    if !psk_storable(psk) {
        return Err("a psk that is neither 64 hex digits nor an 8-63 character passphrase");
    }
    if ssid.is_empty() && !psk.is_empty() {
        return Err("a psk without an ssid");
    }
    Ok(Provision::Set(LinkConf { key: LinkKey(key), ssid: ssid.to_string(), psk: psk.to_string() }))
}

/// Carry a provision out on flash: [`LINK_FILE`] written 0600 through a synced temporary and a rename, or removed.
pub fn store(p: &Provision, path: &str) -> std::io::Result<()> {
    match p {
        Provision::Clear => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Provision::Set(c) => {
            use std::os::unix::fs::OpenOptionsExt;
            if let Some(dir) = std::path::Path::new(path).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let tmp = format!("{path}.tmp");
            let written = (|| {
                let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
                f.write_all(link_file_text(c).as_bytes())?;
                f.sync_all()
            })();
            if let Err(e) = written {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            std::fs::rename(&tmp, path)
        }
    }
}

// ---- the station keeper -------------------------------------------------------------------------------------------

/// Stop the running keeper (SIGTERM to the pid in its lock; it stops its supplicant and DHCP client itself), wait up
/// to 3 s for its lock to go, then, if `start`, start it again exactly as ocbm_boot.sh does, its lines into the box
/// log. On a thread of its own, so the poll loop never waits on it. Fire and forget for the new keeper: ocbmd ignores
/// SIGCHLD, so nothing may wait on the child.
pub fn keeper_restart(start: bool) {
    let _ = std::thread::Builder::new().name("wifi-keeper".into()).stack_size(64 * 1024).spawn(move || {
        if let Some(pid) = std::fs::read_to_string(KEEPER_LOCK_PID).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
            if pid > 1 {
                eprintln!("[ocbmd] wifi: stopping the station keeper (pid {pid})");
                // SAFETY: kill(2) on the pid the keeper wrote into its own lock.
                unsafe { libc::kill(pid, libc::SIGTERM) };
                let lock = std::path::Path::new(KEEPER_LOCK_PID).parent().map(|p| p.to_path_buf());
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline && lock.as_ref().is_some_and(|l| l.exists()) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        if !start {
            eprintln!("[ocbmd] wifi: station keeper stopped; no hotspot to join");
            return;
        }
        if !std::path::Path::new(KEEPER).exists() {
            eprintln!("[ocbmd] wifi: no {KEEPER}: no station keeper on this adapter");
            return;
        }
        let log = || std::fs::OpenOptions::new().append(true).create(true).open("/tmp/box.log").ok();
        let (out, err) = match (log(), log()) {
            (Some(o), Some(e)) => (std::process::Stdio::from(o), std::process::Stdio::from(e)),
            _ => (std::process::Stdio::null(), std::process::Stdio::null()),
        };
        match std::process::Command::new("setsid").args(["sh", KEEPER]).stdin(std::process::Stdio::null()).stdout(out).stderr(err).spawn() {
            Ok(_) => eprintln!("[ocbmd] wifi: station keeper started"),
            Err(e) => eprintln!("[ocbmd] wifi: could not start the station keeper ({e})"),
        }
    });
}

// ---- the station's state ------------------------------------------------------------------------------------------

/// The station as the keeper last published it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sta {
    pub iface: String,
    pub joined: bool,
    /// The hotspot's channel while joined, else 0.
    pub channel: u32,
    /// No station on this unit (one wireless interface, or no mapped backend): the keeper's `state=unsupported`.
    pub none: bool,
}

/// [`STA_STATE`]'s text: `iface=`, `state=joined|joining|unsupported`, `channel=`.
pub fn parse_sta_state(text: &str) -> Sta {
    let mut s = Sta::default();
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("iface=") {
            s.iface = v.trim().to_string();
        } else if let Some(v) = l.strip_prefix("state=") {
            s.joined = v.trim() == "joined";
            s.none = v.trim() == "unsupported";
        } else if let Some(v) = l.strip_prefix("channel=") {
            s.channel = v.trim().parse().unwrap_or(0);
        }
    }
    if !s.joined {
        s.channel = 0;
    }
    s
}

pub fn read_sta(path: &str) -> Option<Sta> {
    std::fs::read_to_string(path).ok().map(|t| parse_sta_state(&t))
}

/// The IPv4 address and netmask of `iface`, if it has one.
pub fn iface_v4(iface: &str) -> Option<(Ipv4Addr, Ipv4Addr)> {
    if iface.is_empty() {
        return None;
    }
    v4_where(|name, _| name == iface)
}

/// The netmask of the interface holding `ip`.
pub fn mask_of(ip: Ipv4Addr) -> Option<Ipv4Addr> {
    v4_where(|_, a| a == ip).map(|(_, m)| m)
}

/// The first IPv4 address (and its netmask) whose interface name and address pass `pick`.
fn v4_where(pick: impl Fn(&str, Ipv4Addr) -> bool) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list freed below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return None;
    }
    let mut found = None;
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` walks the list getifaddrs returned; each name is a C string.
        let ifa = unsafe { &*cur };
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_string_lossy();
        // SAFETY: a non-null ifa_addr points at a sockaddr; AF_INET ones (and their netmasks) are sockaddr_in.
        if !ifa.ifa_addr.is_null() && i32::from(unsafe { (*ifa.ifa_addr).sa_family }) == libc::AF_INET {
            let ip = Ipv4Addr::from(u32::from_be(unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) }.sin_addr.s_addr));
            if pick(&name, ip) {
                let mask = if ifa.ifa_netmask.is_null() {
                    Ipv4Addr::new(255, 255, 255, 0)
                } else {
                    Ipv4Addr::from(u32::from_be(unsafe { &*(ifa.ifa_netmask as *const libc::sockaddr_in) }.sin_addr.s_addr))
                };
                found = Some((ip, mask));
                break;
            }
        }
        cur = ifa.ifa_next;
    }
    // SAFETY: the list getifaddrs returned.
    unsafe { libc::freeifaddrs(head) };
    found
}

/// The channel the AP is configured for (hostapd.conf `channel=`), 0 when unreadable.
pub fn ap_channel(conf: &str) -> u32 {
    conf.lines().find_map(|l| l.strip_prefix("channel=")).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// The text of the configuration the AP runs from: `session`'s while it is there, else `flash`'s; "" when neither reads.
pub fn ap_conf_text(session: &str, flash: &str) -> String {
    std::fs::read_to_string(session).or_else(|_| std::fs::read_to_string(flash)).unwrap_or_default()
}

/// What the bench's interface lever names, if anything.
fn bench_iface() -> Option<String> {
    let v = std::fs::read_to_string(BENCH_IF_FILE).ok()?;
    let v = v.trim();
    (!v.is_empty() && v != "off").then(|| v.to_string())
}

/// Where the listener should be now: the station's address (or the bench lever's interface's), port 7020, and that
/// interface; None while there is none, or when it lies on the AP's or NCM's subnet.
pub fn listen_addr(sta: Option<&Sta>) -> Option<(SocketAddrV4, String)> {
    let iface = bench_iface().or_else(|| sta.filter(|s| s.joined).map(|s| s.iface.clone()))?;
    let (ip, _) = iface_v4(&iface)?;
    if reserved_subnet(ip) || ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    Some((SocketAddrV4::new(ip, ocbm_link::PORT), iface))
}

/// Take connections from `iface` only (SO_BINDTODEVICE): under Linux's weak-host model a socket bound to one
/// interface's address answers that address arriving on any interface, the AP's and USB's network included, so the
/// address alone does not keep a client of the box's own AP out (the review of 24 Nov, I). Nothing elsewhere (macOS,
/// the tests' host, has no such option and no such need here).
pub fn only_on(l: &TcpListener, iface: &str) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let name = std::ffi::CString::new(iface).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let len = name.as_bytes_with_nul().len() as libc::socklen_t;
        // SAFETY: SO_BINDTODEVICE on the listener's own socket, with the interface's NUL-terminated name.
        if unsafe { libc::setsockopt(l.as_raw_fd(), libc::SOL_SOCKET, libc::SO_BINDTODEVICE, name.as_ptr().cast(), len) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let _ = (l, iface);
    Ok(())
}

// ---- the state the threads share ------------------------------------------------------------------------------------

/// What the listener, the mDNS advert and the daemon share.
pub struct Shared {
    /// The link key new handshakes use; None stops the listener.
    pub key: Mutex<Option<LinkKey>>,
    /// Where the listener is bound now (the mDNS advert's address and port), None while it is not.
    pub bound: Mutex<Option<SocketAddrV4>>,
    /// Set to end the listener and the advert for good (the link was removed).
    pub stop: AtomicBool,
    /// While `crate::mono_ms()` is below this, a host holds the box ([`HOLD_SILENCE`] after its last frame): a new
    /// connection is closed before the box's hello, which Carlink reads as "busy", not as a wrong key (notes, 3b).
    pub busy_until_ms: AtomicU64,
    next_id: AtomicU64,
    handshakes: AtomicUsize,
    inflight: Mutex<Vec<IpAddr>>,
    refused: AtomicU64,
}

impl Shared {
    pub fn new(key: Option<LinkKey>) -> Arc<Self> {
        Arc::new(Shared {
            key: Mutex::new(key),
            bound: Mutex::new(None),
            stop: AtomicBool::new(false),
            busy_until_ms: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            handshakes: AtomicUsize::new(0),
            inflight: Mutex::new(Vec::new()),
            refused: AtomicU64::new(0),
        })
    }

    pub fn key(&self) -> Option<LinkKey> {
        self.key.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_key(&self, k: Option<LinkKey>) {
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = k;
    }

    pub fn bound(&self) -> Option<SocketAddrV4> {
        *self.bound.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A connection whose host proved the link key: the loop makes it a [`Link`].
pub struct Authed {
    pub id: u64,
    pub peer: SocketAddr,
    pub stream: TcpStream,
    pub session: Session,
}

/// Where the listener binds.
#[cfg_attr(not(test), allow(dead_code))]
pub enum Bind {
    /// The station's address as it comes and goes ([`listen_addr`]).
    Station,
    /// A fixed address (the tests: loopback, port 0).
    Fixed(SocketAddr),
}

/// The listener's end the loop holds: authenticated connections, and a pipe that is readable when one is waiting.
pub struct Listener {
    pub rx: mpsc::Receiver<Authed>,
    pub wake: File,
    /// The address bound, for [`Bind::Fixed`] (the tests read their port here).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fixed: Option<SocketAddr>,
}

impl Listener {
    /// Empty the wake pipe (the caller then takes every connection waiting in `rx`).
    pub fn drain_wake(&mut self) {
        let mut b = [0u8; 64];
        while matches!((&self.wake).read(&mut b), Ok(n) if n > 0) {}
    }
}

/// A short peer description for the box log: the last octet, as btd logs a station's address (`..7`).
pub fn peer_short(p: &SocketAddr) -> String {
    match p.ip() {
        IpAddr::V4(v) => format!("..{}", v.octets()[3]),
        IpAddr::V6(_) => "an IPv6 peer".to_string(),
    }
}

/// Start the listener's thread: it follows the station's address (rebinding as it changes), accepts, and hands each
/// connection to a handshake thread of its own.
pub fn start_listener(shared: Arc<Shared>, bind: Bind) -> std::io::Result<Listener> {
    let (tx, rx) = mpsc::channel();
    let (wake, wake_tx) = crate::cloexec_pipe()?;
    crate::set_nonblocking(&wake);
    crate::set_nonblocking(&wake_tx);
    let fixed = match &bind {
        Bind::Fixed(a) => {
            let l = TcpListener::bind(a)?;
            let got = l.local_addr()?;
            std::thread::Builder::new()
                .name("wifi-listen".into())
                .stack_size(96 * 1024)
                .spawn(move || listen_loop(shared, Some(l), tx, wake_tx))?;
            Some(got)
        }
        Bind::Station => {
            std::thread::Builder::new()
                .name("wifi-listen".into())
                .stack_size(96 * 1024)
                .spawn(move || listen_loop(shared, None, tx, wake_tx))?;
            None
        }
    };
    Ok(Listener { rx, wake, fixed })
}

fn listen_loop(shared: Arc<Shared>, fixed: Option<TcpListener>, tx: mpsc::Sender<Authed>, wake: File) {
    let fixed_mode = fixed.is_some();
    let mut cur: Option<((SocketAddrV4, String), TcpListener)> = None;
    let fixed_l = fixed;
    let mut last_look = Instant::now() - Duration::from_secs(10);
    let mut last_refusal_log = Instant::now() - Duration::from_secs(10);
    while !shared.stop.load(Ordering::Acquire) {
        if !fixed_mode && last_look.elapsed() >= Duration::from_secs(1) {
            last_look = Instant::now();
            let want = listen_addr(read_sta(STA_STATE).as_ref());
            if cur.as_ref().map(|(w, _)| w) != want.as_ref() {
                if let Some(((a, _), _)) = cur.take() {
                    eprintln!("[ocbmd] wifi: listener on {a} closed (the station's address went or changed)");
                }
                *shared.bound.lock().unwrap_or_else(|e| e.into_inner()) = None;
                if let Some((a, i)) = want {
                    match TcpListener::bind(a).and_then(|l| only_on(&l, &i).map(|()| l)) {
                        Ok(l) => {
                            eprintln!("[ocbmd] wifi: listening on {a} (OCBW v1, the station's address, from {i} only)");
                            *shared.bound.lock().unwrap_or_else(|e| e.into_inner()) = Some(a);
                            cur = Some(((a, i), l));
                        }
                        Err(e) => eprintln!("[ocbmd] wifi: could not listen on {a} from {i} ({e}); trying again"),
                    }
                }
            }
        }
        let l = match (fixed_l.as_ref(), cur.as_ref()) {
            (Some(l), _) => l,
            (None, Some((_, l))) => l,
            (None, None) => {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let mut pfd = libc::pollfd { fd: l.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd; a 500 ms bound so the address and the stop flag are looked at.
        if unsafe { libc::poll(&mut pfd, 1, 500) } <= 0 {
            continue;
        }
        let (s, peer) = match l.accept() {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[ocbmd] wifi: accept failed ({e})");
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let Some(key) = shared.key() else { continue };
        // Another host holds the box: closed before the box's hello (a wrong key is closed after the host's proof).
        if crate::mono_ms() < shared.busy_until_ms.load(Ordering::Acquire) {
            let n = shared.refused.fetch_add(1, Ordering::AcqRel) + 1;
            if last_refusal_log.elapsed() >= Duration::from_secs(5) {
                last_refusal_log = Instant::now();
                eprintln!("[ocbmd] wifi: refused a connection from {}: another host holds the box ({n} refused so far)", peer_short(&peer));
            }
            drop(s);
            continue;
        }
        // The caps: all peers together, and one at a time per peer address.
        let busy_peer = {
            let mut inf = shared.inflight.lock().unwrap_or_else(|e| e.into_inner());
            if inf.contains(&peer.ip()) || shared.handshakes.load(Ordering::Acquire) >= MAX_HANDSHAKES {
                true
            } else {
                inf.push(peer.ip());
                shared.handshakes.fetch_add(1, Ordering::AcqRel);
                false
            }
        };
        if busy_peer {
            let n = shared.refused.fetch_add(1, Ordering::AcqRel) + 1;
            if last_refusal_log.elapsed() >= Duration::from_secs(5) {
                last_refusal_log = Instant::now();
                eprintln!("[ocbmd] wifi: refused a connection from {} (handshakes busy; {n} refused so far)", peer_short(&peer));
            }
            drop(s);
            continue;
        }
        let id = shared.next_id.fetch_add(1, Ordering::AcqRel);
        let (tx, wake, sh) = (tx.clone(), wake.try_clone(), shared.clone());
        let spawned = std::thread::Builder::new().name("wifi-hs".into()).stack_size(64 * 1024).spawn(move || {
            let r = handshake(s, &key);
            {
                let mut inf = sh.inflight.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(i) = inf.iter().position(|a| *a == peer.ip()) {
                    inf.remove(i);
                }
            }
            sh.handshakes.fetch_sub(1, Ordering::AcqRel);
            match r {
                Ok((stream, session)) => {
                    if tx.send(Authed { id, peer, stream, session }).is_ok() {
                        if let Ok(mut w) = wake {
                            let _ = w.write(&[1]);
                        }
                    }
                }
                Err(e) => eprintln!("[ocbmd] wifi: refused {}: {e}", peer_short(&peer)),
            }
        });
        if spawned.is_err() {
            eprintln!("[ocbmd] wifi: no thread for a handshake; connection from {} dropped", peer_short(&peer));
            let mut inf = shared.inflight.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = inf.iter().position(|a| *a == peer.ip()) {
                inf.remove(i);
            }
            shared.handshakes.fetch_sub(1, Ordering::AcqRel);
        }
    }
    *shared.bound.lock().unwrap_or_else(|e| e.into_inner()) = None;
    eprintln!("[ocbmd] wifi: listener stopped (the link was removed)");
}

/// The box's side of one handshake, within the handshake limit.
fn handshake(s: TcpStream, key: &LinkKey) -> Result<(TcpStream, Session), ocbm_link::LinkError> {
    let nonce = ocbm_link::random32()?;
    let session = ocbm_link::box_handshake(&mut Deadline::new(&s, ocbm_link::HANDSHAKE_TIMEOUT), key, nonce)?;
    Ok((s, session))
}

/// One deadline for a whole exchange, never one per read: each read and write gets what is left of it, so a peer that
/// sends a byte every 4.9 s is cut at the limit, not minutes later (the review of 24 Nov, D). Both listeners' handshakes.
pub struct Deadline<'a>(&'a TcpStream, Instant);

impl<'a> Deadline<'a> {
    pub fn new(s: &'a TcpStream, within: Duration) -> Self {
        Deadline(s, Instant::now() + within)
    }
    fn left(&self) -> std::io::Result<Duration> {
        let left = self.1.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        Ok(left)
    }
}

impl Read for Deadline<'_> {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.0.set_read_timeout(Some(self.left()?))?;
        (&*self.0).read(b)
    }
}

impl Write for Deadline<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.set_write_timeout(Some(self.left()?))?;
        (&*self.0).write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---- a link ----------------------------------------------------------------------------------------------------

/// A link's counters, shared by its two threads and the loop: what each way carried and when, and whether the box shut
/// the socket first. For its end lines and the loop's 30 s line: from the box's log alone the 24 Nov attach bench could
/// not tell who ended a link, nor when either side had last heard the other (ADDENDUM 8 of the Wi-Fi link plan).
#[derive(Default)]
pub struct LinkStats {
    /// `crate::mono_ms()` at the link's start.
    since_ms: AtomicU64,
    in_records: AtomicU64,
    in_bytes: AtomicU64,
    /// `crate::mono_ms()` of the last record opened; 0: none yet.
    last_in_ms: AtomicU64,
    out_records: AtomicU64,
    out_keepalives: AtomicU64,
    out_bytes: AtomicU64,
    /// `crate::mono_ms()` of the last record written whole; 0: none yet.
    last_out_ms: AtomicU64,
    /// `crate::mono_ms()` when the write under way began; 0 between writes.
    write_since_ms: AtomicU64,
    /// The box shut the socket: [`SHUT_BY_LOOP`] or [`SHUT_BY_WRITER`], the first of them. 0: not the box, so an end of
    /// stream the reader reads is the host's.
    box_shut: AtomicU8,
}

/// [`Link::close`]: the loop ended the link.
const SHUT_BY_LOOP: u8 = 1;
/// The writer thread ended (its write failed or stuck, or the loop dropped the link's output) and shut the socket.
const SHUT_BY_WRITER: u8 = 2;

impl LinkStats {
    fn new() -> Arc<Self> {
        let s = LinkStats::default();
        s.since_ms.store(crate::mono_ms(), Ordering::Relaxed);
        Arc::new(s)
    }

    /// Note that the box shut the socket, unless it had already.
    fn shut(&self, by: u8) {
        let _ = self.box_shut.compare_exchange(0, by, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Both ways, the link's age and a write under way: "up 32210 ms; in 53 records, 58361 B, the last 120 ms ago; out
    /// 1412 records (31 keepalives), 474591 B, the last 40 ms ago".
    pub fn line(&self) -> String {
        let now = crate::mono_ms();
        let ago = |at: &AtomicU64| match at.load(Ordering::Relaxed) {
            0 => "never".to_string(),
            t => format!("{} ms ago", now.saturating_sub(t)),
        };
        let w = self.write_since_ms.load(Ordering::Relaxed);
        format!(
            "up {} ms; in {} records, {} B, the last {}; out {} records ({} keepalives), {} B, the last {}{}",
            now.saturating_sub(self.since_ms.load(Ordering::Relaxed)),
            self.in_records.load(Ordering::Relaxed),
            self.in_bytes.load(Ordering::Relaxed),
            ago(&self.last_in_ms),
            self.out_records.load(Ordering::Relaxed),
            self.out_keepalives.load(Ordering::Relaxed),
            self.out_bytes.load(Ordering::Relaxed),
            ago(&self.last_out_ms),
            if w == 0 { String::new() } else { format!("; a write under way for {} ms", now.saturating_sub(w)) },
        )
    }
}

/// What a link's end of stream was: after the box's own shutdown, that shutdown; otherwise the host's FIN.
fn eof_reason(box_shut: u8) -> &'static str {
    match box_shut {
        SHUT_BY_LOOP => "the box closed the link (the loop ended it)",
        SHUT_BY_WRITER => "the box closed the link (its writer ended)",
        _ => "the host closed the link",
    }
}

/// The socket's own view (Linux; nothing elsewhere): its TCP state (1 established; 8 close-wait: the peer's FIN came
/// first; 4/5 fin-wait: this side shut first; 7 closed: a reset), the segments not yet acknowledged, the
/// retransmissions, the RTT, how long since data last came in and went out and since the peer last acknowledged, and
/// the bytes the kernel still holds to send.
pub fn tcp_view(s: &TcpStream) -> String {
    #[cfg(target_os = "linux")]
    {
        let fd = s.as_raw_fd();
        // SAFETY: getsockopt(TCP_INFO) into a zeroed tcp_info with its size; the kernel fills what it has (3.14's
        // fields reach tcpi_total_retrans, the last read here).
        let mut ti: libc::tcp_info = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
        let info = (&mut ti as *mut libc::tcp_info).cast();
        if unsafe { libc::getsockopt(fd, libc::IPPROTO_TCP, libc::TCP_INFO, info, &mut len) } == 0 {
            let mut queued: libc::c_int = 0;
            // SAFETY: TIOCOUTQ (SIOCOUTQ) on a TCP socket: the bytes in its send queue, unsent or unacknowledged.
            let q = unsafe { libc::ioctl(fd, libc::TIOCOUTQ, &mut queued as *mut libc::c_int) } == 0;
            return format!(
                "; tcp state {}, unacked {}, retrans {}/{}, rtt {} us, data in {} ms ago, out {} ms ago, \
                 acked {} ms ago, queued {}",
                ti.tcpi_state,
                ti.tcpi_unacked,
                ti.tcpi_retransmits,
                ti.tcpi_total_retrans,
                ti.tcpi_rtt,
                ti.tcpi_last_data_recv,
                ti.tcpi_last_data_sent,
                ti.tcpi_last_ack_recv,
                if q { format!("{queued} B") } else { "?".to_string() },
            );
        }
    }
    let _ = s;
    String::new()
}

/// An authenticated host connection, as the poll loop holds it.
pub struct Link {
    pub id: u64,
    pub peer: SocketAddr,
    /// What the host sent, decrypted (non-blocking; EOF = the link ended).
    pub from_host: File,
    /// Frames for the host (non-blocking): the loop's output while this link holds the box (`Daemon::acc`), else
    /// kept here.
    pub to_host: Option<File>,
    /// Its frames until its HELLO is taken.
    pub reasm: ocbm_proto::Reassembler,
    /// Its HELLO was taken: it holds (or held) the box.
    pub admitted: bool,
    /// A set-up network session (setup.rs, M3): its own rules (two messages, 180 s in all), answered on the link itself.
    pub setup: bool,
    /// A set-up link's own frame sequence (it never takes the box's).
    pub seq: u32,
    pub since: Instant,
    /// Its first record has opened (its reader's word): the peer holds the keys its handshake made. A set-up link's
    /// code goes on the adapter's Bluetooth then (setup.rs).
    opened: Arc<AtomicBool>,
    /// [`Link::close`] has been called: its end is on the way (the loop sees its input's EOF a pass or so later).
    closed: std::cell::Cell<bool>,
    sock: TcpStream,
    /// What the link carried each way, and whether the box shut it ([`LinkStats`]).
    stats: Arc<LinkStats>,
}

impl Link {
    /// End the link now: both threads see the socket shut and end, closing their pipe ends.
    pub fn close(&self) {
        self.stats.shut(SHUT_BY_LOOP);
        self.closed.set(true);
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }

    /// The link's counters and its socket's view, for the loop's 30 s line.
    pub fn stats_line(&self) -> String {
        format!("{}{}", self.stats.line(), tcp_view(&self.sock))
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    /// A record from the peer has opened.
    pub fn opened(&self) -> bool {
        self.opened.load(Ordering::Acquire)
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.close();
    }
}

/// Start a link's two threads. The reader ends it after `DEAD_AFTER` with nothing received, a failed record, or EOF;
/// the writer sends a keepalive after `KEEPALIVE_AFTER` with nothing to send, and ends it on a write stuck
/// `DEAD_AFTER` or the loop closing its end.
pub fn start_link(a: Authed) -> std::io::Result<Link> {
    let Authed { id, peer, stream, session } = a;
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(ocbm_link::DEAD_AFTER))?;
    stream.set_write_timeout(Some(ocbm_link::DEAD_AFTER))?;
    let (from_host, host_bytes) = crate::cloexec_pipe()?;
    let (frames, to_host) = crate::cloexec_pipe()?;
    crate::set_nonblocking(&from_host);
    crate::set_nonblocking(&to_host);
    let Session { tx, rx } = session;
    let rs = stream.try_clone()?;
    let ws = stream.try_clone()?;
    let opened = Arc::new(AtomicBool::new(false));
    let first = opened.clone();
    let stats = LinkStats::new();
    let (rstats, wstats) = (stats.clone(), stats.clone());
    std::thread::Builder::new()
        .name("wifi-read".into())
        .stack_size(crate::RELAY_STACK)
        .spawn(move || link_reader(id, rs, rx, host_bytes, first, rstats))?;
    std::thread::Builder::new()
        .name("wifi-write".into())
        .stack_size(crate::RELAY_STACK)
        .spawn(move || link_writer(id, ws, tx, frames, wstats))?;
    Ok(Link {
        id,
        peer,
        from_host,
        to_host: Some(to_host),
        reasm: ocbm_proto::Reassembler::new(),
        admitted: false,
        setup: false,
        seq: 0,
        since: Instant::now(),
        opened,
        closed: std::cell::Cell::new(false),
        sock: stream,
        stats,
    })
}

fn link_reader(
    id: u64,
    mut s: TcpStream,
    mut rx: ocbm_link::Opener,
    mut host_bytes: File,
    opened: Arc<AtomicBool>,
    stats: Arc<LinkStats>,
) {
    let mut buf = vec![0u8; 32 * 1024];
    let (mut records, mut bytes) = (0u64, 0u64);
    let why = 'outer: loop {
        let n = match s.read(&mut buf) {
            Ok(0) => break eof_reason(stats.box_shut.load(Ordering::SeqCst)).to_string(),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                break format!("nothing from the host for {} s (dead link)", ocbm_link::DEAD_AFTER.as_secs())
            }
            Err(e) => break format!("read failed: {e}"),
        };
        rx.push(&buf[..n]);
        loop {
            match rx.next() {
                Ok(Some(plain)) => {
                    records += 1;
                    stats.in_records.store(records, Ordering::Relaxed);
                    stats.last_in_ms.store(crate::mono_ms(), Ordering::Relaxed);
                    if records == 1 {
                        opened.store(true, Ordering::Release);
                    }
                    if plain.is_empty() {
                        continue; // a keepalive
                    }
                    bytes += plain.len() as u64;
                    stats.in_bytes.store(bytes, Ordering::Relaxed);
                    if let Err(e) = host_bytes.write_all(&plain) {
                        break 'outer format!("the loop's pipe: {e}");
                    }
                }
                Ok(None) => break,
                Err(e) => break 'outer format!("{e}"),
            }
        }
    };
    // The socket's view before this side's shutdown below: a FIN from the host reads tcp state 8 (close-wait).
    eprintln!(
        "[ocbmd] wifi: link {id} reader ended: {why} ({records} records, {bytes} B in) at uptime {} s; {}{}",
        crate::uptime_text(),
        stats.line(),
        tcp_view(&s)
    );
    let _ = s.shutdown(std::net::Shutdown::Both);
    // `host_bytes` drops here: the loop reads EOF.
}

fn link_writer(id: u64, mut s: TcpStream, mut tx: ocbm_link::Sealer, frames: File, stats: Arc<LinkStats>) {
    let mut buf = vec![0u8; ocbm_link::MAX_RECORD];
    let mut out: Vec<u8> = Vec::with_capacity(4 + ocbm_link::MAX_RECORD + ocbm_link::TAG);
    let mut bytes = 0u64;
    let keepalive_ms = ocbm_link::KEEPALIVE_AFTER.as_millis() as libc::c_int;
    let why = loop {
        let mut pfd = libc::pollfd { fd: frames.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for a descriptor this thread owns.
        let r = unsafe { libc::poll(&mut pfd, 1, keepalive_ms) };
        out.clear();
        let keepalive = r == 0;
        if r == 0 {
            tx.seal_into(&mut out, &[]); // nothing to send for a second: the keepalive
        } else if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break format!("poll failed: {e}");
        } else {
            match (&frames).read(&mut buf) {
                Ok(0) => break "the loop closed its end".to_string(),
                Ok(n) => {
                    bytes += n as u64;
                    tx.seal_into(&mut out, &buf[..n]);
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => continue,
                Err(e) => break format!("pipe read failed: {e}"),
            }
        }
        stats.write_since_ms.store(crate::mono_ms(), Ordering::Relaxed);
        let wrote = s.write_all(&out);
        stats.write_since_ms.store(0, Ordering::Relaxed);
        if let Err(e) = wrote {
            break if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) {
                format!("a write stuck for {} s (the host is gone from the hotspot?)", ocbm_link::DEAD_AFTER.as_secs())
            } else {
                format!("send failed: {e}")
            };
        }
        stats.out_records.fetch_add(1, Ordering::Relaxed);
        if keepalive {
            stats.out_keepalives.fetch_add(1, Ordering::Relaxed);
        }
        stats.out_bytes.store(bytes, Ordering::Relaxed);
        stats.last_out_ms.store(crate::mono_ms(), Ordering::Relaxed);
    };
    eprintln!(
        "[ocbmd] wifi: link {id} writer ended: {why} ({bytes} B out) at uptime {} s; {}{}",
        crate::uptime_text(),
        stats.line(),
        tcp_view(&s)
    );
    // Before the shutdown: the reader's end of stream after it is this box's, and its line says so.
    stats.shut(SHUT_BY_WRITER);
    let _ = s.shutdown(std::net::Shutdown::Both);
    // `frames` drops here: the loop's writes fail (EPIPE / POLLERR).
}

/// The first frame of a not-yet-admitted link or transport: Some(nonce) when it is a CT_HELLO (0 = no nonce given).
pub fn hello_nonce(ch: u16, pl: &[u8]) -> Option<u32> {
    if ch != ocbm_proto::CH_CTRL || pl.first() != Some(&ocbm_proto::CT_HELLO) {
        return None;
    }
    Some(if pl.len() >= 6 { u32::from_le_bytes([pl[2], pl[3], pl[4], pl[5]]) } else { 0 })
}

/// A sink for the loop's output while no host holds the box (Wi-Fi mode): frames are written and gone, so nothing
/// queues and the A/V seams are not held back ("parked").
pub fn sink() -> File {
    std::fs::OpenOptions::new().write(true).open("/dev/null").expect("/dev/null")
}

/// The `"wifi_link"` object of MGMT_INFO: no name, no key, no address.
pub fn info_json(set_up: bool, linked: bool, sta: Option<&Sta>, listening: bool, ap_ch: u32) -> String {
    let (state, channel) = if !set_up {
        ("off", 0)
    } else if linked {
        ("linked", sta.map_or(0, |s| s.channel))
    } else if listening || sta.is_some_and(|s| s.joined) {
        ("joined", sta.map_or(0, |s| s.channel))
    } else if sta.is_some_and(|s| s.none) {
        ("unsupported", 0)
    } else {
        ("joining", 0)
    };
    format!("{{\"state\":\"{state}\",\"channel\":{channel},\"ap_channel\":{ap_ch}}}")
}

/// True once, if [`USB_ONCE_FLAG`] was set; removes it.
pub fn take_usb_once() -> bool {
    std::fs::remove_file(USB_ONCE_FLAG).is_ok()
}

pub fn set_usb_once() {
    let _ = std::fs::write(USB_ONCE_FLAG, b"1\n");
}

/// Which transport a host frame came over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Usb,
    /// A Wi-Fi link, by its id.
    Wifi(u64),
}

/// The daemon's Wi-Fi side (`Daemon::wifi`). Inert while `on` is false: then nothing here runs, and the daemon is
/// 2.7.1's USB daemon.
pub struct Side {
    /// A link key is set up and this start takes Wi-Fi hosts.
    pub on: bool,
    pub shared: Option<Arc<Shared>>,
    pub listener: Option<Listener>,
    /// Authenticated links: the one holding the box, and the ones whose HELLO has not come yet.
    pub links: Vec<Link>,
    /// The transport whose host holds the box; None: nobody, and the loop's output goes to a sink.
    pub via: Option<Via>,
    /// When the holder's last frame came (the [`HOLD_SILENCE`] rule).
    pub last_host_frame: Option<Instant>,
    /// When the listener was last told the box is held ([`Side::publish_busy`]).
    pub busy_published: Option<Instant>,
    /// USB's output while USB does not hold the box (None on a Direct unit, whose loop owns the device).
    pub usb_out: Option<File>,
    /// This unit's accessory I/O is the loop's own ([`crate::AccIo::Direct`]): a USB host taking the box restarts
    /// ocbmd for one plain USB start ([`USB_ONCE_FLAG`]), since the Wi-Fi side's USB reader would keep writes out.
    pub usb_direct: bool,
    /// USB's reader ended while USB did not hold the box: carry on over Wi-Fi, restart once idle.
    pub usb_gone: bool,
    /// A stored MGMT_WIFI_LINK to apply: when, the transport it came over, and the link as it was before.
    pub pending: Option<(Instant, Option<Via>, Option<LinkConf>)>,
    /// Restart ocbmd as soon as no host holds the box and no session is up.
    pub restart_when_idle: bool,
    /// The link as stored.
    pub conf: Option<LinkConf>,
}

impl Side {
    /// Tell the listener whether a host holds the box now: until `HOLD_SILENCE` after `last` (None: nobody).
    pub fn publish_busy(&mut self, last: Option<Instant>) {
        self.busy_published = last;
        if let Some(s) = self.shared.as_ref() {
            let until = last.map_or(0, |t| {
                let left = (t + HOLD_SILENCE).saturating_duration_since(Instant::now());
                crate::mono_ms() + left.as_millis() as u64
            });
            s.busy_until_ms.store(until, Ordering::Release);
        }
    }

    /// No link set up: 2.7.1's daemon.
    pub fn off() -> Side {
        Side {
            on: false,
            shared: None,
            listener: None,
            links: Vec::new(),
            via: None,
            last_host_frame: None,
            busy_published: None,
            usb_out: None,
            usb_direct: false,
            usb_gone: false,
            pending: None,
            restart_when_idle: false,
            conf: None,
        }
    }

    /// Start taking Wi-Fi hosts with `conf`'s key: the listener and the advert.
    pub fn start(&mut self, conf: &LinkConf) -> std::io::Result<()> {
        let shared = Shared::new(Some(conf.key.clone()));
        let l = start_listener(shared.clone(), Bind::Station)?;
        crate::mdns::start(shared.clone());
        self.shared = Some(shared);
        self.listener = Some(l);
        self.on = true;
        Ok(())
    }

    /// Stop taking Wi-Fi hosts: the listener and the advert end, every link closes.
    pub fn stop(&mut self) {
        if let Some(s) = self.shared.take() {
            s.set_key(None);
            s.stop.store(true, Ordering::Release);
        }
        self.listener = None;
        for l in &self.links {
            l.close();
        }
        self.links.clear();
        self.on = false;
    }

    /// The holder holds against another host: it has spoken within [`HOLD_SILENCE`].
    pub fn holder_fresh(&self, now: Instant) -> bool {
        self.via.is_some() && self.last_host_frame.is_some_and(|t| now.duration_since(t) < HOLD_SILENCE)
    }

    pub fn link_index(&self, id: u64) -> Option<usize> {
        self.links.iter().position(|l| l.id == id)
    }

    /// MGMT_INFO's `"wifi_link"` object.
    pub fn info(&self) -> String {
        let sta = read_sta(STA_STATE);
        let ap = ap_channel(&ap_conf_text(AP_SESSION_CONF, HOSTAPD_CONF));
        let listening = self.shared.as_ref().is_some_and(|s| s.bound().is_some());
        info_json(self.conf.is_some(), matches!(self.via, Some(Via::Wifi(_))), sta.as_ref(), listening, ap)
    }
}

/// A link for the daemon's tests, with no threads: `to_host`'s bytes come out of the returned pipe end, and the
/// returned stream is the host's end of its socket (it sees the link's close).
#[cfg(test)]
pub fn test_link(id: u64) -> (Link, File, TcpStream) {
    let l = TcpListener::bind(loopback0()).unwrap();
    let host = TcpStream::connect(l.local_addr().unwrap()).unwrap();
    let (sock, peer) = l.accept().unwrap();
    let (from_host, _host_bytes) = crate::cloexec_pipe().unwrap();
    let (out_read, to_host) = crate::cloexec_pipe().unwrap();
    crate::set_nonblocking(&from_host);
    crate::set_nonblocking(&out_read);
    let link = Link {
        id,
        peer,
        from_host,
        to_host: Some(to_host),
        reasm: ocbm_proto::Reassembler::new(),
        admitted: false,
        setup: false,
        seq: 0,
        since: Instant::now(),
        opened: Arc::default(),
        closed: std::cell::Cell::new(false),
        sock,
        stats: LinkStats::new(),
    };
    (link, out_read, host)
}

/// The tests' stand-in for a link's reader opening its first record.
#[cfg(test)]
pub fn test_opened(l: &Link) {
    l.opened.store(true, Ordering::Release);
}

/// An address for the tests' listener.
#[cfg(test)]
pub fn loopback0() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocbm_link::host_handshake;

    fn body(key: [u8; 32], ssid: &str, psk: &str) -> Vec<u8> {
        let mut b = vec![1];
        b.extend_from_slice(&key);
        b.push(ssid.len() as u8);
        b.extend_from_slice(ssid.as_bytes());
        b.push(psk.len() as u8);
        b.extend_from_slice(psk.as_bytes());
        b
    }

    #[test]
    fn provision_bodies_parse_and_refuse() {
        assert_eq!(parse_provision(&[]), Ok(Provision::Clear));
        let hash = "ab".repeat(32);
        let want = LinkConf { key: LinkKey([3; 32]), ssid: "Example Hotspot".into(), psk: hash.clone() };
        assert_eq!(parse_provision(&body([3; 32], "Example Hotspot", &hash)), Ok(Provision::Set(want.clone())));
        // The file it writes reads back the same.
        assert_eq!(parse_link_file(&link_file_text(&want)), Some(want));
        // A passphrase, an open network, the key only.
        assert!(parse_provision(&body([3; 32], "net", "a good passphrase")).is_ok());
        assert!(parse_provision(&body([3; 32], "net", "")).is_ok());
        assert!(parse_provision(&body([3; 32], "", "")).is_ok());
        // Refusals.
        assert!(parse_provision(&body([3; 32], "a\nkey=00", &hash)).is_err(), "a line break would forge a line");
        assert!(parse_provision(&body([3; 32], "net", "short")).is_err());
        assert!(parse_provision(&body([3; 32], "net", "has a \" quote")).is_err());
        assert!(parse_provision(&body([3; 32], "net", &"g".repeat(64))).is_err(), "64 characters must be hex");
        assert!(parse_provision(&body([3; 32], "", &hash)).is_err(), "a psk with no ssid");
        assert!(parse_provision(&body([3; 32], &"s".repeat(33), &hash)).is_err());
        assert!(parse_provision(&[2]).is_err());
        let mut trailing = body([3; 32], "net", &hash);
        trailing.push(0);
        assert!(parse_provision(&trailing).is_err());
        assert!(parse_provision(&body([3; 32], "net", &hash)[..40]).is_err());
    }

    #[test]
    fn the_link_file_is_stored_0600_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let path = format!("/tmp/ocbmd_selftest_wifi_store_{}", std::process::id());
        let c = LinkConf { key: LinkKey([9; 32]), ssid: "net".into(), psk: "cd".repeat(32) };
        store(&Provision::Set(c.clone()), &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read_link_conf(&path), Some(c));
        store(&Provision::Clear, &path).unwrap();
        assert!(!std::path::Path::new(&path).exists());
        store(&Provision::Clear, &path).unwrap(); // removing nothing is fine
        assert_eq!(parse_link_file("key=short\n"), None);
    }

    #[test]
    fn station_state_and_ap_channel_read_back() {
        let s = parse_sta_state("iface=sta0\nstate=joined\nchannel=149\n");
        assert_eq!(s, Sta { iface: "sta0".into(), joined: true, channel: 149, none: false });
        assert!(parse_sta_state("iface=\nstate=unsupported\nchannel=0\n").none, "no station on this unit");
        assert_eq!(parse_sta_state("iface=sta0\nstate=joining\nchannel=149\n").channel, 0, "no channel unless joined");
        assert_eq!(ap_channel("interface=wlan0\nchannel=36\nhw_mode=a\n"), 36);
        assert_eq!(ap_channel("ssid=x\n"), 0);
        assert!(reserved_subnet(Ipv4Addr::new(192, 168, 43, 10)) && reserved_subnet(Ipv4Addr::new(192, 168, 50, 2)));
        assert!(!reserved_subnet(Ipv4Addr::new(172, 20, 10, 2)));
    }

    /// ADDENDUM 6: `ap_channel` is the running AP's, its per-session configuration's where it followed the station.
    #[test]
    fn ap_channel_is_the_running_configurations() {
        let dir = format!("/tmp/ocbmd_selftest_ap_conf_{}", std::process::id());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (session, flash) = (format!("{dir}/hostapd_session.conf"), format!("{dir}/hostapd.conf"));
        std::fs::write(&flash, "interface=wlan0\nhw_mode=a\nchannel=36\n").unwrap();
        assert_eq!(ap_channel(&ap_conf_text(&session, &flash)), 36, "no session configuration: the flash file's");
        std::fs::write(&session, "interface=wlan0\nhw_mode=a\nchannel=149\n").unwrap();
        assert_eq!(ap_channel(&ap_conf_text(&session, &flash)), 149, "the AP followed the station: its session's");
        std::fs::remove_file(&session).unwrap();
        std::fs::remove_file(&flash).unwrap();
        assert_eq!(ap_channel(&ap_conf_text(&session, &flash)), 0, "neither: unknown");
        let _ = std::fs::remove_dir_all(&dir);
        // The hotspot on a radar channel and the AP on its own: joined, the two channels differ (Carlink's line).
        let dfs = Sta { iface: "sta0".into(), joined: true, channel: 100, none: false };
        assert_eq!(info_json(true, false, Some(&dfs), true, 36), "{\"state\":\"joined\",\"channel\":100,\"ap_channel\":36}");
    }

    #[test]
    fn mgmt_info_names_states_and_channels_only() {
        let joined = Sta { iface: "sta0".into(), joined: true, channel: 36, none: false };
        let none = Sta { none: true, ..Sta::default() };
        assert_eq!(info_json(true, false, Some(&none), false, 36), "{\"state\":\"unsupported\",\"channel\":0,\"ap_channel\":36}");
        assert_eq!(info_json(false, false, None, false, 36), "{\"state\":\"off\",\"channel\":0,\"ap_channel\":36}");
        assert_eq!(info_json(true, false, None, false, 36), "{\"state\":\"joining\",\"channel\":0,\"ap_channel\":36}");
        assert_eq!(info_json(true, false, Some(&joined), false, 36), "{\"state\":\"joined\",\"channel\":36,\"ap_channel\":36}");
        assert_eq!(info_json(true, true, Some(&joined), true, 44), "{\"state\":\"linked\",\"channel\":36,\"ap_channel\":44}");
    }

    #[test]
    fn hello_nonces_are_read_from_the_first_frame() {
        assert_eq!(hello_nonce(ocbm_proto::CH_CTRL, &[ocbm_proto::CT_HELLO, 1, 4, 3, 2, 1]), Some(0x0102_0304));
        assert_eq!(hello_nonce(ocbm_proto::CH_CTRL, &[ocbm_proto::CT_HELLO, 1]), Some(0));
        assert_eq!(hello_nonce(ocbm_proto::CH_CTRL, &[ocbm_proto::CT_HEARTBEAT]), None);
        assert_eq!(hello_nonce(ocbm_proto::CH_MGMT, &[ocbm_proto::CT_HELLO]), None);
    }

    fn read_for(f: &mut File, want: usize, secs: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut b = [0u8; 4096];
        while out.len() < want && Instant::now() < deadline {
            match f.read(&mut b) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&b[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        out
    }

    fn eof_within(f: &mut File, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut b = [0u8; 256];
        while Instant::now() < deadline {
            match f.read(&mut b) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        false
    }

    /// The listener on loopback: a wrong key is refused, the host's link carries both ways and sends keepalives, and
    /// a host that goes silent is dropped after DEAD_AFTER.
    #[test]
    fn a_link_relays_both_ways_keeps_alive_and_ends_on_silence() {
        let key = LinkKey([5u8; 32]);
        let shared = Shared::new(Some(key.clone()));
        let mut l = start_listener(shared.clone(), Bind::Fixed(loopback0())).unwrap();
        let addr = l.fixed.unwrap();

        let mut bad = TcpStream::connect(addr).unwrap();
        bad.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        assert!(host_handshake(&mut bad, &LinkKey([6u8; 32]), [1u8; 32]).is_err());

        let mut h = TcpStream::connect(addr).unwrap();
        h.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
        let mut sess = host_handshake(&mut h, &key, [2u8; 32]).unwrap();
        let a = l.rx.recv_timeout(Duration::from_secs(3)).expect("the authenticated link");
        l.drain_wake();
        let mut link = start_link(a).unwrap();

        h.write_all(&sess.tx.seal(b"HELLO frame bytes")).unwrap();
        let last_sent = Instant::now(); // the host sends nothing after this, not even a keepalive
        assert_eq!(read_for(&mut link.from_host, 17, 3), b"HELLO frame bytes");
        link.to_host.as_mut().unwrap().write_all(b"HELLO_ACK").unwrap();
        // The box's next records: the data, then keepalives once it has nothing to send.
        let (mut got, mut keepalives) = (Vec::new(), 0);
        let mut b = [0u8; 256];
        let deadline = Instant::now() + Duration::from_millis(2600);
        while Instant::now() < deadline {
            while let Some(p) = sess.rx.next().unwrap() {
                if p.is_empty() {
                    keepalives += 1;
                } else {
                    got.extend_from_slice(&p);
                }
            }
            if keepalives >= 2 {
                break;
            }
            let n = h.read(&mut b).unwrap();
            sess.rx.push(&b[..n]);
        }
        assert_eq!(got, b"HELLO_ACK");
        assert!(keepalives >= 2, "keepalives from an idle box: {keepalives}");
        // Its counters, which its end lines and the loop's 30 s line read (ADDENDUM 8 (b)).
        let st = &link.stats;
        assert_eq!((st.in_records.load(Ordering::Relaxed), st.in_bytes.load(Ordering::Relaxed)), (1, 17));
        assert!(st.out_records.load(Ordering::Relaxed) >= 3 && st.out_keepalives.load(Ordering::Relaxed) >= 2);
        assert_eq!(st.out_bytes.load(Ordering::Relaxed), 9);
        assert!(link.stats_line().contains("; in 1 records, 17 B, the last "), "{}", link.stats_line());

        // The host has been silent since its frame (no keepalive): the box drops it DEAD_AFTER after that, and the
        // loop's end reads EOF.
        assert!(eof_within(&mut link.from_host, 5), "a silent host's link should end");
        let took = last_sent.elapsed();
        assert!(took >= Duration::from_millis(2900) && took <= Duration::from_millis(4000), "ended {took:?} after the host's last record");
        shared.stop.store(true, Ordering::Release);
    }

    #[test]
    fn a_closed_host_reads_as_eof_at_once() {
        let key = LinkKey([4u8; 32]);
        let shared = Shared::new(Some(key.clone()));
        let l = start_listener(shared.clone(), Bind::Fixed(loopback0())).unwrap();
        let mut h = TcpStream::connect(l.fixed.unwrap()).unwrap();
        h.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let _sess = host_handshake(&mut h, &key, [2u8; 32]).unwrap();
        let mut link = start_link(l.rx.recv_timeout(Duration::from_secs(3)).unwrap()).unwrap();
        drop(h);
        assert!(eof_within(&mut link.from_host, 2));
        assert_eq!(link.stats.box_shut.load(Ordering::SeqCst), 0, "the host's close: the box shut nothing first");
        assert_eq!(eof_reason(0), "the host closed the link");
        shared.stop.store(true, Ordering::Release);
    }

    /// A link the box ends itself reads as the box's end, not as the host's (ADDENDUM 8 (b): its reader said "the host
    /// closed the link" after the box's own shutdown too).
    #[test]
    fn a_link_the_box_closes_is_told_from_one_the_host_closed() {
        let key = LinkKey([3u8; 32]);
        let shared = Shared::new(Some(key.clone()));
        let l = start_listener(shared.clone(), Bind::Fixed(loopback0())).unwrap();
        let mut h = TcpStream::connect(l.fixed.unwrap()).unwrap();
        h.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let _sess = host_handshake(&mut h, &key, [2u8; 32]).unwrap();
        let mut link = start_link(l.rx.recv_timeout(Duration::from_secs(3)).unwrap()).unwrap();
        link.close();
        assert!(eof_within(&mut link.from_host, 2));
        assert_eq!(link.stats.box_shut.load(Ordering::SeqCst), SHUT_BY_LOOP);
        assert_eq!(eof_reason(SHUT_BY_LOOP), "the box closed the link (the loop ended it)");
        // The writer's own end (a failed or stuck write, or the loop dropping the link's output) shuts it first too.
        link.stats.box_shut.store(0, Ordering::SeqCst);
        link.stats.shut(SHUT_BY_WRITER);
        link.stats.shut(SHUT_BY_LOOP);
        assert_eq!(link.stats.box_shut.load(Ordering::SeqCst), SHUT_BY_WRITER, "the first to shut it is the one named");
        assert_eq!(eof_reason(SHUT_BY_WRITER), "the box closed the link (its writer ended)");
        shared.stop.store(true, Ordering::Release);
    }

    /// A box another host holds closes a new connection before its hello (Carlink reads that as busy); a wrong key is
    /// closed only after the host's proof.
    #[test]
    fn a_busy_box_closes_before_its_hello_and_a_wrong_key_after_the_proof() {
        let key = LinkKey([3u8; 32]);
        let shared = Shared::new(Some(key.clone()));
        let l = start_listener(shared.clone(), Bind::Fixed(loopback0())).unwrap();
        let addr = l.fixed.unwrap();
        shared.busy_until_ms.store(crate::mono_ms() + 10_000, Ordering::Release);
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut hello = [0u8; ocbm_link::HELLO_LEN];
        let n = c.read(&mut hello).unwrap_or(0);
        assert_eq!(n, 0, "busy: EOF before any byte of the box's hello");
        shared.busy_until_ms.store(0, Ordering::Release);
        let mut w = TcpStream::connect(addr).unwrap();
        w.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        w.read_exact(&mut hello).unwrap();
        assert_eq!(&hello[..4], b"OCBW", "free: the box's hello");
        let mut reply = [0u8; ocbm_link::REPLY_LEN];
        reply[..4].copy_from_slice(b"OCBW");
        reply[4] = 1;
        w.write_all(&reply).unwrap(); // a proof with no key behind it
        let mut b = [0u8; 32];
        assert!(matches!(w.read(&mut b), Ok(0) | Err(_)), "a wrong proof: closed, no proof from the box");
        shared.stop.store(true, Ordering::Release);
    }

    /// A peer that drips one byte of its reply every 300 ms: in time for every read, so a per-read limit would hold its
    /// slot for 20 s. It is cut 5 s after it connected, all reads together.
    fn slow_drip(addr: SocketAddr) -> (TcpStream, Instant) {
        let s = TcpStream::connect(addr).unwrap();
        let t0 = Instant::now();
        let w = s.try_clone().unwrap();
        std::thread::spawn(move || {
            for _ in 0..ocbm_link::REPLY_LEN {
                if (&w).write_all(b"O").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        (s, t0)
    }

    /// When the box ended a connection, counted from `t0`: its EOF, read past whatever it sent first.
    fn cut_after(mut s: TcpStream, t0: Instant) -> Duration {
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut b = [0u8; 256];
        while matches!(s.read(&mut b), Ok(n) if n > 0) {}
        t0.elapsed()
    }

    /// A stranger dripping bytes from one address costs one slot, its own address's, and only for the handshake's 5 s
    /// in all: the paired host from another address gets in at once meanwhile. Two addresses on a dual-stack loopback
    /// listener (::1 and 127.0.0.1, which macOS has where it has no 127.0.0.2).
    #[test]
    fn a_slow_drip_stranger_is_cut_at_five_seconds_and_the_paired_host_gets_in_meanwhile() {
        let key = LinkKey([7u8; 32]);
        let shared = Shared::new(Some(key.clone()));
        let l = start_listener(shared.clone(), Bind::Fixed("[::]:0".parse().unwrap())).unwrap();
        let port = l.fixed.unwrap().port();
        let (stranger, t0) = slow_drip(SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), port));
        std::thread::sleep(Duration::from_millis(200));
        let mut h = TcpStream::connect(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)).unwrap();
        h.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let hs = Instant::now();
        assert!(host_handshake(&mut h, &key, [2u8; 32]).is_ok(), "the paired host, from another address");
        assert!(hs.elapsed() < Duration::from_secs(2), "at once, not after the stranger's slot");
        let took = cut_after(stranger, t0);
        assert!(took >= Duration::from_millis(4500) && took <= Duration::from_millis(6500), "the stranger cut after {took:?}");
        shared.stop.store(true, Ordering::Release);
    }
}
