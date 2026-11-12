//! The Wi-Fi transport's discovery: a minimal mDNS/DNS-SD responder for `_ocbm._tcp` on the STA link.
//!
//! On the car's hotspot the box's address comes from DHCP, so the host finds it the standard way
//! (Android's `NsdManager`, macOS `dns-sd -B _ocbm._tcp`). The record set:
//!
//! ```text
//!   _services._dns-sd._udp.local  PTR  _ocbm._tcp.local
//!   _ocbm._tcp.local              PTR  ccpa-xxxx._ocbm._tcp.local
//!   ccpa-xxxx._ocbm._tcp.local    SRV  0 0 7020 ccpa-xxxx.local
//!   ccpa-xxxx._ocbm._tcp.local    TXT  "v=1" "id=<ocbm_link::box_id>"
//!   ccpa-xxxx.local               A    <the STA address>
//! ```
//!
//! `xxxx` is the STA interface MAC's last two bytes, as in the AP's SSID. `id` lets a host pick its own
//! box among several on one hotspot; it is an HMAC of the link key, so it says nothing about the key.
//!
//! WHY HAND-ROLLED. carplayd advertises `_airplay._tcp` with the `mdns-sd` crate; ocbmd taking that
//! crate too would cost ~200 KB of a rootfs with ~1 MB free, for five fixed records. This answers
//! questions that name one of those records (or ANY), announces on start and whenever the address
//! changes, and nothing else: no probing (the name carries the MAC), no known-answer suppression.
//!
//! SHARING :5353. `mdns-sd` binds it with SO_REUSEADDR + SO_REUSEPORT, and so does this, so the kernel
//! hands every multicast query to both. The socket joins the group on the STA interface only, and sends
//! there only, so nothing about `_ocbm._tcp` reaches the CarPlay AP's clients.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::os::unix::io::FromRawFd;
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

/// What the box advertises.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advert {
    /// `ccpa-xxxx`: both the instance label and the host label.
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
        format!("{}.local", self.name)
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

/// The full record set as one response (`ttl_scale` 0 = a goodbye).
pub fn response(a: &Advert, ttl_scale: u32) -> Vec<u8> {
    let mut out = vec![0, 0, 0x84, 0, 0, 0, 0, 5, 0, 0, 0, 0]; // id 0, QR|AA, 0 q, 5 answers
    let mut ptr = Vec::new();
    put_name(&mut ptr, SERVICE);
    put_rr(&mut out, META, T_PTR, CLASS_IN, 4500 * ttl_scale, &ptr);
    let mut inst = Vec::new();
    put_name(&mut inst, &a.instance());
    put_rr(&mut out, SERVICE, T_PTR, CLASS_IN, 4500 * ttl_scale, &inst);
    let mut srv = vec![0, 0, 0, 0];
    srv.extend_from_slice(&a.port.to_be_bytes());
    put_name(&mut srv, &a.host());
    put_rr(&mut out, &a.instance(), T_SRV, CLASS_IN | CACHE_FLUSH, 120 * ttl_scale, &srv);
    let mut txt = Vec::new();
    for kv in ["v=1".to_string(), format!("id={}", a.id)] {
        txt.push(kv.len() as u8);
        txt.extend_from_slice(kv.as_bytes());
    }
    put_rr(&mut out, &a.instance(), T_TXT, CLASS_IN | CACHE_FLUSH, 4500 * ttl_scale, &txt);
    put_rr(&mut out, &a.host(), T_A, CLASS_IN | CACHE_FLUSH, 120 * ttl_scale, &a.addr.octets());
    out
}

/// A name at `off` (labels and compression pointers), lowercased, and the offset after it in place.
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
        labels.push(String::from_utf8_lossy(msg.get(off + 1..off + 1 + len)?).into_owned());
        off += 1 + len;
    }
    None
}

/// Whether `msg` is a query with a question this advert answers.
pub fn asks_for_us(msg: &[u8], a: &Advert) -> bool {
    if msg.len() < 12 || msg[2] & 0x80 != 0 {
        return false; // too short, or a response
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]);
    let (inst, host) = (a.instance().to_ascii_lowercase(), a.host().to_ascii_lowercase());
    let mut off = 12;
    for _ in 0..qd {
        let Some((name, next)) = read_name(msg, off) else { return false };
        let Some(t) = msg.get(next..next + 2) else { return false };
        let qtype = u16::from_be_bytes([t[0], t[1]]);
        off = next + 4;
        let hit = match name.as_str() {
            n if n == SERVICE => matches!(qtype, T_PTR | T_ANY),
            n if n == META => matches!(qtype, T_PTR | T_ANY),
            n if n == inst => matches!(qtype, T_SRV | T_TXT | T_ANY),
            n if n == host => matches!(qtype, T_A | T_ANY),
            _ => false,
        };
        if hit {
            return true;
        }
    }
    false
}

/// IPv4 address of `iface`, if it has one.
fn iface_ipv4(iface: &str) -> Option<Ipv4Addr> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list we free below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return None;
    }
    let mut found = None;
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` walks the list getifaddrs returned.
        let ifa = unsafe { &*cur };
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_string_lossy();
        if name == iface && !ifa.ifa_addr.is_null() && unsafe { (*ifa.ifa_addr).sa_family } as i32 == libc::AF_INET {
            // SAFETY: an AF_INET sockaddr is a sockaddr_in.
            let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
            found = Some(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
            break;
        }
        cur = ifa.ifa_next;
    }
    // SAFETY: the list from getifaddrs.
    unsafe { libc::freeifaddrs(head) };
    found
}

/// `ccpa-xxxx` from `iface`'s MAC.
pub fn box_name(iface: &str) -> String {
    let mac = std::fs::read_to_string(format!("/sys/class/net/{iface}/address")).unwrap_or_default();
    let hex: String = mac.trim().chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() >= 4 {
        format!("ccpa-{}", hex[hex.len() - 4..].to_ascii_lowercase())
    } else {
        "ccpa".to_string()
    }
}

/// A :5353 socket shared with carplayd's (SO_REUSEADDR + SO_REUSEPORT), joined to the group on `ip`,
/// sending through it.
fn open_socket(ip: Ipv4Addr) -> std::io::Result<UdpSocket> {
    // SAFETY: plain socket(2). Close-on-exec so the console's shell never inherits it (no SOCK_CLOEXEC on
    // macOS, where the host-side tests build: set the flag after).
    #[cfg(target_os = "linux")]
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    #[cfg(not(target_os = "linux"))]
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(not(target_os = "linux"))]
    // SAFETY: F_SETFD on the fd just created.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    // SAFETY: the fd was just created and nothing else owns it.
    let sock = unsafe { UdpSocket::from_raw_fd(fd) };
    let one: libc::c_int = 1;
    for opt in [libc::SO_REUSEADDR, libc::SO_REUSEPORT] {
        // SAFETY: setsockopt with a c_int on our socket.
        unsafe {
            libc::setsockopt(fd, libc::SOL_SOCKET, opt, &one as *const _ as *const libc::c_void, std::mem::size_of::<libc::c_int>() as libc::socklen_t);
        }
    }
    let addr = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: PORT.to_be(),
        sin_addr: libc::in_addr { s_addr: 0 },
        sin_zero: [0; 8],
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
    };
    // SAFETY: bind to a sockaddr_in we built.
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
    sock.set_read_timeout(Some(Duration::from_secs(2)))?;
    Ok(sock)
}

/// Run the responder for `iface` for the life of the process (a thread of its own).
pub fn start(iface: String, port: u16, id: String) {
    let _ = std::thread::Builder::new()
        .name("mdns".into())
        .stack_size(128 * 1024)
        .spawn(move || run(&iface, port, &id));
}

fn run(iface: &str, port: u16, id: &str) {
    let dest = SocketAddrV4::new(GROUP, PORT);
    let mut current: Option<(Advert, UdpSocket)> = None;
    let mut buf = [0u8; 1500];
    let mut last_answer = Instant::now() - Duration::from_secs(10);
    let mut last_announce = Instant::now() - Duration::from_secs(600);
    loop {
        // Follow the STA address: none yet (DHCP still going), or a new lease.
        let ip = iface_ipv4(iface);
        let stale = match (&current, ip) {
            (Some((a, _)), Some(ip)) => a.addr != ip,
            (Some(_), None) => true,
            (None, Some(_)) => true,
            (None, None) => false,
        };
        if stale {
            if let Some((a, s)) = current.take() {
                let _ = s.send_to(&response(&a, 0), dest); // goodbye for the old address
            }
            if let Some(ip) = ip {
                match open_socket(ip) {
                    Ok(s) => {
                        let a = Advert { name: box_name(iface), addr: ip, port, id: id.to_string() };
                        eprintln!("[ocbmd] mdns: advertising {}.{SERVICE} at {ip}:{port} on {iface}", a.name);
                        // Announce twice, a second apart (RFC 6762 §8.3).
                        let _ = s.send_to(&response(&a, 1), dest);
                        std::thread::sleep(Duration::from_secs(1));
                        let _ = s.send_to(&response(&a, 1), dest);
                        last_announce = Instant::now();
                        current = Some((a, s));
                    }
                    Err(e) => eprintln!("[ocbmd] mdns: no socket on {iface} {ip} ({e}); retrying"),
                }
            }
        }
        let Some((a, s)) = current.as_ref() else {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };
        // A gentle re-announce: a host that joined the hotspot later still hears of us unasked.
        if last_announce.elapsed() >= Duration::from_secs(120) {
            let _ = s.send_to(&response(a, 1), dest);
            last_announce = Instant::now();
        }
        match s.recv_from(&mut buf) {
            Ok((n, _)) if asks_for_us(&buf[..n], a) && last_answer.elapsed() >= Duration::from_millis(500) => {
                let _ = s.send_to(&response(a, 1), dest);
                last_answer = Instant::now();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advert() -> Advert {
        Advert { name: "ccpa-1a2b".into(), addr: Ipv4Addr::new(192, 168, 0, 10), port: 7020, id: "cbb6a0d2a8710450".into() }
    }

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        put_name(&mut q, name);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    #[test]
    fn answers_only_its_own_names() {
        let a = advert();
        assert!(asks_for_us(&query("_ocbm._tcp.local", T_PTR), &a));
        assert!(asks_for_us(&query("_OCBM._tcp.local", T_PTR), &a)); // case-insensitive
        assert!(asks_for_us(&query("_services._dns-sd._udp.local", T_PTR), &a));
        assert!(asks_for_us(&query("ccpa-1a2b._ocbm._tcp.local", T_SRV), &a));
        assert!(asks_for_us(&query("ccpa-1a2b.local", T_A), &a));
        assert!(!asks_for_us(&query("_airplay._tcp.local", T_PTR), &a));
        assert!(!asks_for_us(&query("ccpa-1a2b.local", T_TXT), &a));
        let mut resp = query("_ocbm._tcp.local", T_PTR);
        resp[2] = 0x84; // a response, not a query
        assert!(!asks_for_us(&resp, &a));
    }

    #[test]
    fn compressed_question_names_are_followed() {
        // Q1 "_ocbm._tcp.local" PTR, Q2 "ccpa-1a2b" + pointer to Q1's name, SRV
        let mut q = vec![0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
        put_name(&mut q, "_ocbm._tcp.local");
        q.extend_from_slice(&[0, 12, 0, 1]);
        q.push(9);
        q.extend_from_slice(b"ccpa-1a2b");
        q.extend_from_slice(&[0xC0, 12]);
        q.extend_from_slice(&[0, 33, 0, 1]);
        let a = advert();
        assert_eq!(read_name(&q, 12 + 18 + 4).unwrap().0, "ccpa-1a2b._ocbm._tcp.local");
        assert!(asks_for_us(&q, &a));
    }

    #[test]
    fn response_carries_all_five_records() {
        let r = response(&advert(), 1);
        assert_eq!(&r[2..4], &[0x84, 0]);
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 5);
        let txt = b"\x03v=1\x13id=cbb6a0d2a8710450";
        assert!(r.windows(txt.len()).any(|w| w == txt));
        assert!(r.windows(4).any(|w| w == [192, 168, 0, 10]));
        assert!(r.windows(2).any(|w| w == 7020u16.to_be_bytes()));
    }
}
