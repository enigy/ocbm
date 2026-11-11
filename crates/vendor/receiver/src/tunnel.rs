//! Route A's tunnel to the phone's proxy. Once video is offered, the phone sends `setProxyParameters`: a TLS 1.3
//! pre-shared-key proxy on its own address (carplayd forwards that command to the host like every other). With
//! it, the host fetches the video the phone hands off over the phone's network instead of its own, as LIVI's
//! receiver does. The host runs the TLS and the proxy's CONNECT; the box only carries the bytes: host->box as
//! `INPUT_TUNNEL` sub-frames `[op][id][data]`, box->host on the `:9006` seam (ocbm-proto `CH_TUNNEL`), framed
//! `[u32 BE "TUNL"][u32 BE len][op][id][data]`.
//!
//! A tunnel reaches only the phone: its address must be the phone's own (this session's control connection
//! came from it) or a link-local IPv6 one on the phone's link, never anything else on the box's network.
//! Behind the AirPlay video probe's `video` switch, as the rest of route A is.

use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV6, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// ocbmd's seam for the tunnels' bytes to the host (ocbm-proto `CH_TUNNEL`).
const SEAM_PORT: u16 = 9006;
const MAGIC: [u8; 4] = *b"TUNL";
// ocbm-proto's TUNNEL_* ops (the receiver doesn't link that crate).
const OPEN: u8 = 0x01;
const DATA: u8 = 0x02;
const CLOSE: u8 = 0x03;
const OPENED: u8 = 0x04;
/// One read from the phone, and so the most data one frame carries.
const CHUNK: usize = 16 * 1024;

/// This session's phone, as its control connection came from (with its IPv6 scope). Set per session.
static PHONE: Mutex<Option<SocketAddr>> = Mutex::new(None);
/// The open tunnels: id, generation (an id is reused once its tunnel is gone) and the socket.
static TUNNELS: Mutex<Vec<(u8, u64, TcpStream)>> = Mutex::new(Vec::new());
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The one connection to the seam, shared by every tunnel; a frame is written whole under this lock.
static SEAM: Mutex<Option<TcpStream>> = Mutex::new(None);

/// The phone of the session that just connected: the one address a tunnel may reach.
pub fn set_phone(addr: SocketAddr) {
    *crate::plock(&PHONE) = Some(addr);
}

/// One `INPUT_TUNNEL` sub-frame from the host, without its type byte. False when it was refused.
pub fn on_input(pl: &[u8]) -> bool {
    let [op, id, data @ ..] = pl else {
        eprintln!("[tunnel] a {} B sub-frame with no op or id — dropped", pl.len());
        return false;
    };
    match *op {
        OPEN => open(*id, data),
        DATA => write(*id, data),
        CLOSE => {
            if let Some((_, s)) = take(*id, None) {
                let _ = s.shutdown(Shutdown::Both);
                eprintln!("[tunnel] {id}: closed (the host closed it)");
            }
            true
        }
        other => {
            eprintln!("[tunnel] {id}: op 0x{other:02x} is not one the box knows — dropped");
            false
        }
    }
}

/// The session ended: its tunnels end with it (the phone's proxy offer was this session's).
pub fn close_all() {
    let all = std::mem::take(&mut *crate::plock(&TUNNELS));
    for (id, _, s) in all {
        let _ = s.shutdown(Shutdown::Both);
        eprintln!("[tunnel] {id}: closed (the session ended)");
        emit(CLOSE, id, b"the session ended");
    }
}

fn open(id: u8, data: &[u8]) -> bool {
    if !crate::probe::switches().video {
        refuse(id, "route A is off");
        return false;
    }
    let Some((port, host)) = data.split_first_chunk::<2>().map(|(p, h)| (u16::from_be_bytes(*p), h)) else {
        refuse(id, "no port");
        return false;
    };
    let phone = *crate::plock(&PHONE);
    let to = match target(&String::from_utf8_lossy(host), port, phone) {
        Ok(to) => to,
        Err(why) => {
            refuse(id, &why);
            return false;
        }
    };
    if crate::plock(&TUNNELS).iter().any(|(t, _, _)| *t == id) {
        refuse(id, "that id is open already");
        return false;
    }
    // Connect off the input thread: a silent proxy would hold every touch for the connect's timeout.
    let spawned = std::thread::Builder::new().name(format!("tunnel-{id}")).spawn(move || run(id, to));
    if spawned.is_err() {
        refuse(id, "no thread for it");
        return false;
    }
    true
}

/// One tunnel's life: connect, tell the host, then carry the phone's bytes to it until either side ends.
fn run(id: u8, to: SocketAddr) {
    let stream = match TcpStream::connect_timeout(&to, Duration::from_secs(5)) {
        Ok(s) => s,
        Err(e) => {
            refuse(id, &format!("connect failed ({:?})", e.kind()));
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(writer) = stream.try_clone() else {
        refuse(id, "no second handle on the socket");
        return;
    };
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    crate::plock(&TUNNELS).push((id, generation, writer));
    eprintln!("[tunnel] {id}: open to the phone's proxy");
    if !emit(OPENED, id, &[]) {
        end(id, generation, "the seam is gone");
        return;
    }
    let mut stream = stream;
    let mut buf = vec![0u8; CHUNK];
    let why = loop {
        match stream.read(&mut buf) {
            Ok(0) => break "the phone closed it".to_string(),
            Ok(n) => {
                if !emit(DATA, id, &buf[..n]) {
                    break "the seam is gone".to_string();
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => break format!("read failed ({:?})", e.kind()),
        }
    };
    end(id, generation, &why);
}

/// The host's bytes for tunnel `id`. Written outside the tunnels' lock, so one slow tunnel holds no other.
fn write(id: u8, data: &[u8]) -> bool {
    let found = crate::plock(&TUNNELS)
        .iter()
        .find(|(t, _, _)| *t == id)
        .and_then(|(_, g, s)| s.try_clone().ok().map(|s| (*g, s)));
    let Some((generation, mut s)) = found else {
        eprintln!("[tunnel] {id}: {} B for a tunnel that isn't open — dropped", data.len());
        return false;
    };
    if let Err(e) = s.write_all(data) {
        end(id, generation, &format!("write failed ({:?})", e.kind()));
        return false;
    }
    true
}

/// Tunnel `id` (that generation) ended on the box's side: forget it and tell the host, unless the host closed
/// it first, or a newer tunnel has its id now.
fn end(id: u8, generation: u64, why: &str) {
    if let Some((_, s)) = take(id, Some(generation)) {
        let _ = s.shutdown(Shutdown::Both);
        eprintln!("[tunnel] {id}: closed ({why})");
        emit(CLOSE, id, why.as_bytes());
    }
}

/// Remove tunnel `id` (any generation, or only `generation`), returning its socket.
fn take(id: u8, generation: Option<u64>) -> Option<(u64, TcpStream)> {
    let mut t = crate::plock(&TUNNELS);
    let i = t.iter().position(|(x, g, _)| *x == id && generation.map_or(true, |want| *g == want))?;
    let (_, g, s) = t.swap_remove(i);
    Some((g, s))
}

fn refuse(id: u8, why: &str) {
    eprintln!("[tunnel] {id}: refused ({why})");
    emit(CLOSE, id, why.as_bytes());
}

/// Where a tunnel to `host` (the proxyUrl's host) may go: the phone's own address with its scope, or a
/// link-local IPv6 one on the phone's link. Anything else is refused, naming kinds of address, never one.
fn target(host: &str, port: u16, phone: Option<SocketAddr>) -> Result<SocketAddr, String> {
    let phone = phone.ok_or("no phone session")?;
    if port == 0 {
        return Err("port 0".into());
    }
    let bare = host.trim();
    let bare = bare.strip_prefix("https://").or_else(|| bare.strip_prefix("http://")).unwrap_or(bare);
    let bare = bare.strip_prefix('[').unwrap_or(bare);
    let bare = bare.split([']', '%', '/']).next().unwrap_or("");
    let ip = canonical(bare.parse::<IpAddr>().map_err(|_| "the proxy's host is not an IP address")?);
    if ip == canonical(phone.ip()) {
        return Ok(match (ip, phone) {
            (IpAddr::V6(v6), SocketAddr::V6(p)) => SocketAddr::V6(SocketAddrV6::new(v6, port, 0, p.scope_id())),
            (ip, _) => SocketAddr::new(ip, port),
        });
    }
    match (ip, phone) {
        (IpAddr::V6(v6), SocketAddr::V6(p)) if link_local(&v6) && p.scope_id() != 0 => {
            Ok(SocketAddr::V6(SocketAddrV6::new(v6, port, 0, p.scope_id())))
        }
        _ => Err(format!("not the phone's address (the proxy's is {}, the phone's {})", kind(ip), kind(phone.ip()))),
    }
}

/// An IPv4-mapped IPv6 address as the IPv4 one, so a dual-stack socket's peer compares with a plain one.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

fn link_local(ip: &Ipv6Addr) -> bool {
    ip.segments()[0] & 0xffc0 == 0xfe80
}

fn kind(ip: IpAddr) -> &'static str {
    match canonical(ip) {
        IpAddr::V4(_) => "IPv4",
        IpAddr::V6(v6) if link_local(&v6) => "link-local IPv6",
        IpAddr::V6(_) => "IPv6",
    }
}

/// One message for the host on the seam, `[u32 BE "TUNL"][u32 BE len][op][id][data]`.
fn frame(op: u8, id: u8, data: &[u8]) -> Vec<u8> {
    let len = u32::try_from(2 + data.len()).unwrap_or(u32::MAX);
    let mut f = Vec::with_capacity(10 + data.len());
    f.extend_from_slice(&MAGIC);
    f.extend_from_slice(&len.to_be_bytes());
    f.push(op);
    f.push(id);
    f.extend_from_slice(data);
    f
}

/// Send one message to the host. False when the seam couldn't take it: the seam is then dropped, since a torn
/// frame can't be resumed, and the next message connects again.
fn emit(op: u8, id: u8, data: &[u8]) -> bool {
    let f = frame(op, id, data);
    let mut seam = crate::plock(&SEAM);
    if seam.is_none() {
        let addr = SocketAddr::from(([127, 0, 0, 1], SEAM_PORT));
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(s) => {
                // ocbmd reads this seam only once the host has taken the last chunk, so a write waits while
                // the host is slow; ten seconds of that and the host is gone.
                let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
                let _ = s.set_nodelay(true);
                *seam = Some(s);
            }
            Err(e) => {
                eprintln!("[tunnel] the seam :{SEAM_PORT} isn't there ({e})");
                return false;
            }
        }
    }
    let sent = seam.as_mut().is_some_and(|s| s.write_all(&f).is_ok());
    if !sent {
        *seam = None;
        eprintln!("[tunnel] a write to the seam failed: it is dropped");
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v6_phone() -> Option<SocketAddr> {
        Some(SocketAddr::V6(SocketAddrV6::new("fe80::1c2b:3d4e:5f60:7182".parse().unwrap(), 7000, 0, 3)))
    }

    #[test]
    fn a_tunnel_reaches_the_phone_with_its_scope() {
        let to = target("https://fe80::1c2b:3d4e:5f60:7182", 443, v6_phone()).unwrap();
        assert_eq!(to, SocketAddr::V6(SocketAddrV6::new("fe80::1c2b:3d4e:5f60:7182".parse().unwrap(), 443, 0, 3)));
        assert_eq!(target("[fe80::1c2b:3d4e:5f60:7182%7]", 443, v6_phone()).unwrap(), to, "brackets and a scope");
        let other = target("fe80::9", 8443, v6_phone()).unwrap();
        assert_eq!(other.port(), 8443);
        assert!(matches!(other, SocketAddr::V6(a) if a.scope_id() == 3), "another link-local, on the phone's link");
        let v4: Option<SocketAddr> = Some("192.168.50.101:7000".parse().unwrap());
        assert_eq!(target("192.168.50.101", 8443, v4).unwrap(), "192.168.50.101:8443".parse().unwrap());
        let mapped: Option<SocketAddr> = Some("[::ffff:192.168.50.101]:7000".parse().unwrap());
        assert_eq!(target("192.168.50.101", 8443, mapped).unwrap(), "192.168.50.101:8443".parse().unwrap());
    }

    #[test]
    fn a_tunnel_never_reaches_past_the_phone() {
        let refused = target("192.168.50.1", 22, v6_phone()).unwrap_err();
        assert!(refused.contains("not the phone's address"), "{refused}");
        assert!(!refused.contains("192.168"), "the refusal names kinds of address, never one: {refused}");
        let v4: Option<SocketAddr> = Some("192.168.50.101:7000".parse().unwrap());
        assert!(target("192.168.50.1", 22, v4).is_err(), "the box's own address");
        assert!(target("fe80::9", 22, v4).is_err(), "no scope to reach a link-local one with");
        assert!(target("2001:db8::1", 443, v6_phone()).is_err());
        assert!(target("example.com", 443, v6_phone()).is_err());
        assert!(target("fe80::1c2b:3d4e:5f60:7182", 0, v6_phone()).is_err());
        assert!(target("fe80::1c2b:3d4e:5f60:7182", 443, None).is_err());
    }

    #[test]
    fn a_message_for_the_host_is_the_metadata_seams_shape() {
        assert_eq!(frame(DATA, 7, b"abc"), [b'T', b'U', b'N', b'L', 0, 0, 0, 5, DATA, 7, b'a', b'b', b'c']);
        assert_eq!(frame(OPENED, 1, &[]), [b'T', b'U', b'N', b'L', 0, 0, 0, 2, OPENED, 1]);
    }

    #[test]
    fn a_short_or_unknown_sub_frame_is_refused() {
        assert!(!on_input(&[]));
        assert!(!on_input(&[OPEN]));
        assert!(!on_input(&[0x7f, 1]));
        assert!(!on_input(&[DATA, 200, 1, 2]), "no such tunnel");
    }
}
