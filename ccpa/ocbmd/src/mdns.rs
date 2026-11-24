//! The Wi-Fi link's discovery: a minimal mDNS / DNS-SD responder for `_ocbm._tcp`, on the station interface only.
//!
//! On the phone's hotspot the box's address comes from DHCP, so a host finds it the standard way (Android's
//! `NsdManager`, `dns-sd -B _ocbm._tcp` on a Mac). The record set, `<name>` being the box's name (CarLink-xxxx, the
//! one its AP and Bluetooth carry):
//!
//! ```text
//!   _services._dns-sd._udp.local  PTR  _ocbm._tcp.local
//!   _ocbm._tcp.local              PTR  <name>._ocbm._tcp.local
//!   <name>._ocbm._tcp.local       SRV  0 0 7020 <name lower case>.local
//!   <name>._ocbm._tcp.local       TXT  "v=1" "id=<ocbm_link::box_id>"
//!   <name lower case>.local       A    <the station's address>
//! ```
//!
//! `id` lets a host pick ITS box among several on one hotspot; it is an HMAC of the link key, so it says nothing
//! about the key. The instance name is for people only: Carlink matches on `id`.
//!
//! WHY HAND-ROLLED. carplayd advertises `_airplay._tcp` with the `mdns-sd` crate; ocbmd taking that crate too would
//! cost about 200 KB of flash for five fixed records. This answers questions naming one of them (or ANY), announces
//! when the address comes or changes, says goodbye when it goes, and nothing else: no probing (the name carries the
//! box's own suffix), no known-answer suppression.
//!
//! SHARING :5353 with carplayd's `mdns-sd`, whose sockets (0.11.5: one per interface) are bound to 0.0.0.0:5353 with
//! SO_REUSEADDR and SO_REUSEPORT. This one binds 224.0.0.251:5353 instead (with both options, to share the port): a
//! socket bound to the group address takes only datagrams sent to the group, so a unicast datagram to :5353 (a direct
//! query, a unicast reply to carplayd's own) never lands here, where with 0.0.0.0 and SO_REUSEPORT the kernel would
//! have spread unicast ones over every socket of the port, carplayd's and this one alike. Every multicast query still
//! reaches both. It joins the group on the station's address only, sends there only (replies go from the address the
//! route picks, the station's: a socket bound to a group address has no source address of its own), and on Linux sets
//! IP_MULTICAST_ALL to 0 so it hears only the groups it joined itself (by default a Linux socket hears a group joined
//! by ANY socket on ANY interface, the AP's included). A query from outside the station's subnet is not answered. So
//! nothing of `_ocbm._tcp` reaches the phone on the box's own AP.

use crate::wifi::Shared;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::unix::io::FromRawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
const SERVICE: &str = "_ocbm._tcp.local";
const META: &str = "_services._dns-sd._udp.local";

const T_A: u16 = 1;
const T_PTR: u16 = 12;
const T_TXT: u16 = 16;
const T_SRV: u16 = 33;
const T_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
const CACHE_FLUSH: u16 = 0x8000;
/// The box's name file (radio_hal.sh box_name).
#[cfg(not(test))]
const IDENT_FILE: &str = "/etc/carplay_ident";
#[cfg(test)]
const IDENT_FILE: &str = "/tmp/ocbmd_selftest_no_such_carplay_ident";

/// What the box advertises.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advert {
    /// The instance label: the box's name.
    pub name: String,
    pub addr: Ipv4Addr,
    pub port: u16,
    /// `ocbm_link::box_id`.
    pub id: String,
}

impl Advert {
    fn instance(&self) -> String {
        format!("{}.{SERVICE}", self.name)
    }
    fn host(&self) -> String {
        format!("{}.local", self.name.to_ascii_lowercase())
    }
}

/// The box's name as a DNS label: CarLink-xxxx from /etc/carplay_ident, else "CarLink"; letters, digits and '-'.
pub fn box_name() -> String {
    let n: String = std::fs::read_to_string(IDENT_FILE)
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(63)
        .collect();
    if n.is_empty() {
        "CarLink".to_string()
    } else {
        n
    }
}

fn put_name(out: &mut Vec<u8>, name: &str) {
    for label in name.split('.').filter(|l| !l.is_empty()) {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

fn put_rr(out: &mut Vec<u8>, name: &str, rtype: u16, class: u16, ttl: u32, rdata: &[u8]) {
    put_name(out, name);
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
}

/// The whole record set as one response. `goodbye` sets every TTL to 0. `legacy` is a unicast answer to a one-shot
/// query from a port other than 5353 (RFC 6762 §6.7): its id and its questions echoed, all of them and counted as many
/// in QDCOUNT, no cache-flush bit, TTLs of 10 s.
pub fn response(a: &Advert, goodbye: bool, legacy: Option<(u16, Questions)>) -> Vec<u8> {
    let (id, qd) = legacy.map_or((0, 0), |(id, (n, _))| (id, n));
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0x84, 0]); // QR | AA
    out.extend_from_slice(&qd.to_be_bytes());
    out.extend_from_slice(&5u16.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    if let Some((_, (_, q))) = legacy {
        out.extend_from_slice(q);
    }
    let ttl = |long: u32| if goodbye { 0 } else if legacy.is_some() { long.min(10) } else { long };
    let flush = if legacy.is_some() { 0 } else { CACHE_FLUSH };
    let mut ptr = Vec::new();
    put_name(&mut ptr, SERVICE);
    put_rr(&mut out, META, T_PTR, CLASS_IN, ttl(4500), &ptr);
    let mut inst = Vec::new();
    put_name(&mut inst, &a.instance());
    put_rr(&mut out, SERVICE, T_PTR, CLASS_IN, ttl(4500), &inst);
    let mut srv = vec![0, 0, 0, 0];
    srv.extend_from_slice(&a.port.to_be_bytes());
    put_name(&mut srv, &a.host());
    put_rr(&mut out, &a.instance(), T_SRV, CLASS_IN | flush, ttl(120), &srv);
    let mut txt = Vec::new();
    for kv in ["v=1".to_string(), format!("id={}", a.id)] {
        txt.push(kv.len() as u8);
        txt.extend_from_slice(kv.as_bytes());
    }
    put_rr(&mut out, &a.instance(), T_TXT, CLASS_IN | flush, ttl(4500), &txt);
    put_rr(&mut out, &a.host(), T_A, CLASS_IN | flush, ttl(120), &a.addr.octets());
    out
}

/// A name at `off` (labels and compression pointers), lower case, and the offset after it in place.
fn read_name(msg: &[u8], mut off: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    for _ in 0..64 {
        let len = *msg.get(off)? as usize;
        if len == 0 {
            return Some((labels.join(".").to_ascii_lowercase(), end.unwrap_or(off + 1)));
        }
        if len & 0xC0 == 0xC0 {
            let ptr = ((len & 0x3F) << 8) | *msg.get(off + 1)? as usize;
            end.get_or_insert(off + 2);
            off = ptr;
            continue;
        }
        if len & 0xC0 != 0 {
            return None;
        }
        labels.push(String::from_utf8_lossy(msg.get(off + 1..off + 1 + len)?).into_owned());
        off += 1 + len;
    }
    None
}

/// A query's question section: how many questions, and their bytes (for a legacy answer to echo whole).
pub type Questions<'m> = (u16, &'m [u8]);

/// Whether `msg` is a query with a question this advert answers; Some(its whole question section) when it is. A
/// question that does not parse ends it: None.
pub fn asks_for_us<'m>(msg: &'m [u8], a: &Advert) -> Option<Questions<'m>> {
    if msg.len() < 12 || msg[2] & 0x80 != 0 {
        return None; // too short, or a response
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]);
    let (inst, host) = (a.instance().to_ascii_lowercase(), a.host());
    let mut off = 12;
    let mut hit = false;
    for _ in 0..qd {
        let (name, next) = read_name(msg, off)?;
        let t = msg.get(next..next + 4)?;
        let qtype = u16::from_be_bytes([t[0], t[1]]);
        off = next + 4;
        hit |= match name.as_str() {
            n if n == SERVICE || n == META => matches!(qtype, T_PTR | T_ANY),
            n if n == inst => matches!(qtype, T_SRV | T_TXT | T_ANY),
            n if n == host => matches!(qtype, T_A | T_ANY),
            _ => false,
        };
    }
    Some((qd, msg.get(12..off)?)).filter(|_| hit)
}

fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, mask: Ipv4Addr) -> bool {
    u32::from(a) & u32::from(mask) == u32::from(b) & u32::from(mask)
}

/// A :5353 socket shared with carplayd's, joined to the group on `ip` only, sending through it.
fn open_socket(ip: Ipv4Addr) -> std::io::Result<UdpSocket> {
    // SAFETY: plain socket(2); close-on-exec set right after (no SOCK_CLOEXEC on macOS, where the tests build).
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the fd was just created and nothing else owns it.
    let sock = unsafe { UdpSocket::from_raw_fd(fd) };
    let one: libc::c_int = 1;
    let set = |level: libc::c_int, opt: libc::c_int, v: libc::c_int| {
        // SAFETY: setsockopt with a c_int on the socket above.
        unsafe { libc::setsockopt(fd, level, opt, &v as *const _ as *const libc::c_void, std::mem::size_of::<libc::c_int>() as libc::socklen_t) }
    };
    // SAFETY: F_SETFD on the fd above.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    set(libc::SOL_SOCKET, libc::SO_REUSEADDR, one);
    set(libc::SOL_SOCKET, libc::SO_REUSEPORT, one);
    #[cfg(target_os = "linux")]
    set(libc::IPPROTO_IP, libc::IP_MULTICAST_ALL, 0);
    let addr = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: PORT.to_be(),
        sin_addr: libc::in_addr { s_addr: u32::from(GROUP).to_be() }, // the group's datagrams only (the header)
        sin_zero: [0; 8],
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
    };
    // SAFETY: bind to the sockaddr_in built above.
    if unsafe { libc::bind(fd, &addr as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    sock.join_multicast_v4(&GROUP, &ip)?;
    let iface = libc::in_addr { s_addr: u32::from(ip).to_be() };
    // SAFETY: IP_MULTICAST_IF takes an in_addr.
    unsafe {
        libc::setsockopt(fd, libc::IPPROTO_IP, libc::IP_MULTICAST_IF, &iface as *const _ as *const libc::c_void, std::mem::size_of::<libc::in_addr>() as libc::socklen_t);
    }
    sock.set_multicast_ttl_v4(255)?;
    sock.set_multicast_loop_v4(false)?;
    sock.set_read_timeout(Some(Duration::from_secs(1)))?;
    Ok(sock)
}

/// Run the responder for the life of the link (a thread of its own; it ends when `shared.stop` is set).
pub fn start(shared: Arc<Shared>) {
    let _ = std::thread::Builder::new().name("mdns".into()).stack_size(96 * 1024).spawn(move || run(&shared));
}

fn run(shared: &Shared) {
    let dest = SocketAddrV4::new(GROUP, PORT);
    let mut current: Option<(Advert, Ipv4Addr, UdpSocket)> = None;
    let mut buf = [0u8; 1500];
    let mut last_answer = Instant::now() - Duration::from_secs(10);
    let mut last_announce = Instant::now();
    while !shared.stop.load(Ordering::Acquire) {
        // Follow the listener: its address is the advert's (none until the station has one).
        let want = match (shared.bound(), shared.key()) {
            (Some(a), Some(k)) => Some(Advert { name: box_name(), addr: *a.ip(), port: a.port(), id: ocbm_link::box_id(&k) }),
            _ => None,
        };
        if current.as_ref().map(|(a, _, _)| a) != want.as_ref() {
            if let Some((a, _, s)) = current.take() {
                let _ = s.send_to(&response(&a, true, None), dest); // goodbye for the old record set
            }
            if let Some(a) = want {
                let mask = crate::wifi::mask_of(a.addr).unwrap_or(Ipv4Addr::new(255, 255, 255, 0));
                match open_socket(a.addr) {
                    Ok(s) => {
                        eprintln!("[ocbmd] mdns: advertising {}.{SERVICE} at {}:{} on the station's link", a.name, a.addr, a.port);
                        // Announce twice, a second apart (RFC 6762 §8.3).
                        let _ = s.send_to(&response(&a, false, None), dest);
                        std::thread::sleep(Duration::from_secs(1));
                        let _ = s.send_to(&response(&a, false, None), dest);
                        last_announce = Instant::now();
                        current = Some((a, mask, s));
                    }
                    Err(e) => {
                        eprintln!("[ocbmd] mdns: no socket on {} ({e}); trying again", a.addr);
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
            }
        }
        let Some((a, mask, s)) = current.as_ref() else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        // A gentle re-announce: a host that joined the hotspot later hears of the box unasked.
        if last_announce.elapsed() >= Duration::from_secs(120) {
            let _ = s.send_to(&response(a, false, None), dest);
            last_announce = Instant::now();
        }
        if let Ok((n, SocketAddr::V4(src))) = s.recv_from(&mut buf) {
            if !same_subnet(*src.ip(), a.addr, *mask) {
                continue; // not from the hotspot: never answered
            }
            if let Some(q) = asks_for_us(&buf[..n], a) {
                if src.port() != PORT {
                    let id = u16::from_be_bytes([buf[0], buf[1]]);
                    let _ = s.send_to(&response(a, false, Some((id, q))), src);
                } else if last_answer.elapsed() >= Duration::from_millis(500) {
                    let _ = s.send_to(&response(a, false, None), dest);
                    last_answer = Instant::now();
                }
            }
        }
    }
    if let Some((a, _, s)) = current.take() {
        let _ = s.send_to(&response(&a, true, None), dest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advert() -> Advert {
        Advert { name: "CarLink-1a2b".into(), addr: Ipv4Addr::new(172, 20, 10, 5), port: 7020, id: "cbb6a0d2a8710450".into() }
    }

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        put_name(&mut q, name);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    #[test]
    fn answers_only_its_own_names() {
        let a = advert();
        assert!(asks_for_us(&query("_ocbm._tcp.local", T_PTR), &a).is_some());
        assert!(asks_for_us(&query("_OCBM._tcp.local", T_PTR), &a).is_some(), "case-insensitive");
        assert!(asks_for_us(&query("_services._dns-sd._udp.local", T_PTR), &a).is_some());
        assert!(asks_for_us(&query("CarLink-1a2b._ocbm._tcp.local", T_SRV), &a).is_some());
        assert!(asks_for_us(&query("carlink-1a2b.local", T_A), &a).is_some());
        assert!(asks_for_us(&query("_airplay._tcp.local", T_PTR), &a).is_none());
        assert!(asks_for_us(&query("carlink-1a2b.local", T_TXT), &a).is_none());
        let mut resp = query("_ocbm._tcp.local", T_PTR);
        resp[2] = 0x84; // a response, not a query
        assert!(asks_for_us(&resp, &a).is_none());
    }

    #[test]
    fn compressed_question_names_are_followed() {
        // Q1 "_ocbm._tcp.local" PTR, Q2 "CarLink-1a2b" + a pointer to Q1's name, SRV
        let mut q = vec![0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
        put_name(&mut q, "_ocbm._tcp.local");
        q.extend_from_slice(&[0, 12, 0, 1]);
        q.push(12);
        q.extend_from_slice(b"CarLink-1a2b");
        q.extend_from_slice(&[0xC0, 12]);
        q.extend_from_slice(&[0, 33, 0, 1]);
        assert_eq!(read_name(&q, 12 + 18 + 4).unwrap().0, "carlink-1a2b._ocbm._tcp.local");
        let qs = asks_for_us(&q, &advert()).unwrap();
        assert_eq!(qs, (2, &q[12..]), "the whole question section and its count, for a legacy answer to echo");
        // Echoed whole in a legacy answer, and counted as two (it said one: the second question read as an answer).
        let r = response(&advert(), false, Some((7, qs)));
        assert_eq!(u16::from_be_bytes([r[4], r[5]]), 2);
        assert_eq!(&r[12..q.len()], &q[12..]);
    }

    #[test]
    fn a_pointer_loop_or_a_bad_label_ends_the_parse() {
        let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        q.extend_from_slice(&[0xC0, 12]); // points at itself
        assert!(read_name(&q, 12).is_none());
        assert!(asks_for_us(&q, &advert()).is_none());
        let bad = [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0x40, b'x'];
        assert!(read_name(&bad, 12).is_none());
    }

    #[test]
    fn the_response_carries_all_five_records() {
        let r = response(&advert(), false, None);
        assert_eq!(&r[2..4], &[0x84, 0]);
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 5);
        let txt = b"\x03v=1\x13id=cbb6a0d2a8710450";
        assert!(r.windows(txt.len()).any(|w| w == txt));
        assert!(r.windows(4).any(|w| w == [172, 20, 10, 5]));
        assert!(r.windows(2).any(|w| w == 7020u16.to_be_bytes()));
        assert!(r.windows(13).any(|w| w == b"\x0ccarlink-1a2b"), "the host label in lower case");
        let bye = response(&advert(), true, None);
        assert_eq!(bye.len(), r.len());
        assert_ne!(bye, r, "a goodbye's TTLs are 0");
    }

    #[test]
    fn a_legacy_query_is_answered_with_its_id_and_question() {
        let q = query("_ocbm._tcp.local", T_PTR);
        let qs = asks_for_us(&q, &advert()).unwrap();
        let r = response(&advert(), false, Some((0x1234, qs)));
        assert_eq!(&r[..2], &[0x12, 0x34]);
        assert_eq!(u16::from_be_bytes([r[4], r[5]]), 1, "the question echoed");
        assert_eq!(&r[12..12 + qs.1.len()], qs.1);
    }

    #[test]
    fn subnets_are_compared_under_the_mask() {
        let m = Ipv4Addr::new(255, 255, 255, 240);
        assert!(same_subnet(Ipv4Addr::new(172, 20, 10, 5), Ipv4Addr::new(172, 20, 10, 2), m));
        assert!(!same_subnet(Ipv4Addr::new(192, 168, 43, 100), Ipv4Addr::new(172, 20, 10, 2), m));
    }
}
