//! The Wi-Fi transport: OCBM over TCP, beside the USB accessory.
//!
//! OFF unless [`WIFI_LINK_FILE`] holds a link key. With none, ocbmd is exactly the USB daemon it was.
//!
//! ON, ocbmd races the two transports: the USB relay threads start as usual, a TCP listener opens on
//! [`ocbm_link::PORT`], and the first host to send anything wins ([`wait_first_host`]):
//!
//! - **Wi-Fi first**: a host that completed the link handshake ([`ocbm_link::box_handshake`]) sent a
//!   record. The loop runs on the TCP pipes; the USB reader stays parked and its bytes are ignored.
//! - **USB first, relay driver**: the loop runs on the relay pipes, exactly as without Wi-Fi.
//! - **USB first, a driver that serializes reads and writes** ([`crate::AccIo::Direct`], the truck's):
//!   the parked relay reader holds the driver's mutex, so the loop can't write. ocbmd sets
//!   [`USB_ONCE_FLAG`] and exits; the respawn runs plain USB, and the host's own HELLO retry carries on.
//!
//! So USB always works: a box whose Wi-Fi is misconfigured is still reachable over the cable.
//!
//! The winner's end is the daemon's end: when the TCP host goes (EOF, a failed record, keepalive), its
//! relay thread closes the pipe and the loop's existing transport-loss path exits for respawn, which
//! opens the race again. No second host is taken while one holds the link.

use ocbm_link::{box_handshake, random32, LinkKey, MAX_RECORD, PORT};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::time::Duration;

/// `key=<64 hex>` (the link key). Its presence turns the Wi-Fi transport on. On flash: set once over
/// USB, kept across reboots and updates.
#[cfg(not(test))]
pub const WIFI_LINK_FILE: &str = "/etc/ocbm/wifi_link";
#[cfg(test)]
pub const WIFI_LINK_FILE: &str = "/tmp/ocbmd_selftest_wifi_link";

/// One start in plain USB mode, then gone: set when USB won the race on a serializing driver.
#[cfg(not(test))]
pub const USB_ONCE_FLAG: &str = "/tmp/ocbmd_usb_once";
#[cfg(test)]
pub const USB_ONCE_FLAG: &str = "/tmp/ocbmd_selftest_usb_once";

/// A stranger gets this long to finish the handshake before the listener moves on.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// A host that stops taking bytes (out of range, frozen) ends the link after this.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// The link key from `text` (the file's contents): the `key=` line.
pub fn parse_link_file(text: &str) -> Option<LinkKey> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("key="))
        .find_map(LinkKey::from_hex)
}

/// What `MGMT_WIFI_LINK` asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Provision {
    /// Remove [`WIFI_LINK_FILE`]: the box is a USB-only box again from its next start.
    Clear,
    /// Write it: the link key, and the hotspot (`ssid` empty = transport on but no STA, for a host that
    /// reaches the box another way). `psk` is the hotspot's WPA key as 64 hex digits, or its passphrase.
    Set { key: [u8; 32], ssid: String, psk: String },
}

/// `MGMT_WIFI_LINK`'s body (after the verb): empty = [`Provision::Clear`], else
/// `[ver 1][key 32][ssid_len u8][ssid][psk_len u8][psk]`. Refuses anything a config line can't hold.
pub fn parse_provision(body: &[u8]) -> Result<Provision, &'static str> {
    if body.is_empty() {
        return Ok(Provision::Clear);
    }
    if body[0] != 1 {
        return Err("unknown version");
    }
    let key: [u8; 32] = body.get(1..33).ok_or("short key")?.try_into().map_err(|_| "short key")?;
    let sl = *body.get(33).ok_or("no ssid length")? as usize;
    let ssid = body.get(34..34 + sl).ok_or("short ssid")?;
    let pl = *body.get(34 + sl).ok_or("no psk length")? as usize;
    let psk = body.get(35 + sl..35 + sl + pl).ok_or("short psk")?;
    if body.len() != 35 + sl + pl {
        return Err("trailing bytes");
    }
    let ssid = std::str::from_utf8(ssid).map_err(|_| "ssid not utf-8")?;
    let psk = std::str::from_utf8(psk).map_err(|_| "psk not utf-8")?;
    // One field per line in the file: no line breaks inside a value. An SSID is at most 32 bytes.
    if ssid.len() > 32 || ssid.contains(['\n', '\r']) || psk.contains(['\n', '\r']) || psk.len() > 64 {
        return Err("value not storable");
    }
    if !ssid.is_empty() && !(psk.is_empty() || psk.len() >= 8) {
        return Err("psk too short");
    }
    Ok(Provision::Set { key, ssid: ssid.to_string(), psk: psk.to_string() })
}

/// [`WIFI_LINK_FILE`]'s text for a [`Provision::Set`].
pub fn link_file_text(key: &[u8; 32], ssid: &str, psk: &str) -> String {
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let mut t = format!("# OCBM Wi-Fi transport (MGMT_WIFI_LINK)\nkey={hex}\n");
    if !ssid.is_empty() {
        t.push_str(&format!("ssid={ssid}\npsk={psk}\n"));
    }
    t
}

/// Carry out a provision: write (0600, atomically) or remove the file, then restart the STA keeper so a
/// new hotspot is joined now rather than at the next boot. The listener and the link key take effect at
/// ocbmd's next start (a USB host leaving, or the next boot).
pub fn apply_provision(p: &Provision) -> std::io::Result<()> {
    match p {
        Provision::Clear => match std::fs::remove_file(WIFI_LINK_FILE) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        },
        Provision::Set { key, ssid, psk } => {
            use std::os::unix::fs::OpenOptionsExt;
            let tmp = format!("{WIFI_LINK_FILE}.tmp");
            {
                let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
                f.write_all(link_file_text(key, ssid, psk).as_bytes())?;
                f.sync_all()?;
            }
            std::fs::rename(&tmp, WIFI_LINK_FILE)?;
        }
    }
    #[cfg(not(test))]
    restart_sta_keeper();
    Ok(())
}

/// The STA interface (wifi_sta.sh's `$STA_IF`; `sta` + 0 from the IW416's `sta_name=sta`).
#[cfg(not(test))]
fn sta_if() -> String {
    std::env::var("OCBMD_STA_IF").unwrap_or_else(|_| "sta0".to_string())
}

/// What the STA side runs, for [`stop_sta_side`]: which processes to stop, picked by pid or exact argv.
///
/// WHY NOT `pkill -f`. The first version restarted the keeper with one `sh -c "pkill -f '[w]ifi_sta.sh';
/// ...; exec sh /script/wifi_sta.sh"`. The `[w]` trick only keeps a pattern from matching its own text;
/// that shell's command line also named `wifi_sta.sh` in its `exec`, so the pkill killed the restart
/// itself: the old supplicant stayed on the old hotspot and no new keeper started, until a reboot (truck,
/// 2026-11-11: re-provisioning one hotspot -> another). Matching argv exactly, here in ocbmd, can't do that.
#[derive(Debug, PartialEq, Eq)]
pub enum StaProc {
    Keeper,
    Supplicant,
    Dhcp,
}

/// Which STA-side process a `/proc/<pid>/cmdline` (NUL-separated argv) is, if any, for interface `iface`.
pub fn sta_proc_kind(cmdline: &[u8], iface: &str) -> Option<StaProc> {
    let argv: Vec<&str> = cmdline
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .filter_map(|a| std::str::from_utf8(a).ok())
        .collect();
    let prog = argv.first()?.rsplit('/').next()?;
    let names_iface = argv.windows(2).any(|w| w[0] == "-i" && w[1] == iface) || argv.iter().any(|a| *a == format!("-i{iface}"));
    match prog {
        // The keeper is `sh /script/wifi_sta.sh` (or a bench copy): sh with the script as its first argument.
        "sh" | "ash" | "busybox" => argv.get(1).filter(|a| a.ends_with("/wifi_sta.sh") || **a == "wifi_sta.sh").map(|_| StaProc::Keeper),
        "wpa_supplicant" if names_iface => Some(StaProc::Supplicant),
        "udhcpc" if names_iface => Some(StaProc::Dhcp),
        _ => None,
    }
}

/// Every running STA-side process for `iface`: the keeper, its supplicant and its DHCP client.
#[cfg(not(test))]
fn sta_side_pids(iface: &str) -> Vec<(i32, StaProc)> {
    let me = std::process::id() as i32;
    let mut found = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else { return found };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        if let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) {
            if let Some(kind) = sta_proc_kind(&cmd, iface) {
                found.push((pid, kind));
            }
        }
    }
    found
}

/// Stop the keeper, its supplicant and its DHCP client (SIGTERM, then SIGKILL after ~2 s), and wait for
/// them to be gone, so a keeper started next finds no supplicant on the old hotspot (it would skip
/// starting its own) and no live lock holder. Bounded: at most ~2.5 s, on the provisioning path only.
#[cfg(not(test))]
fn stop_sta_side(iface: &str) {
    let victims = sta_side_pids(iface);
    if victims.is_empty() {
        return;
    }
    for (pid, kind) in &victims {
        eprintln!("[ocbmd] wifi: stopping the STA side's {kind:?} (pid {pid})");
        // SAFETY: kill(2) on a pid we just matched by its argv.
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    let gone = |pid: i32| !std::path::Path::new(&format!("/proc/{pid}")).exists()
        || std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|s| s.rsplit_once(')').map(|(_, r)| r.trim_start().starts_with('Z'))).unwrap_or(true);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline && !victims.iter().all(|(p, _)| gone(*p)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    for (pid, kind) in &victims {
        if !gone(*pid) {
            eprintln!("[ocbmd] wifi: {kind:?} (pid {pid}) ignored SIGTERM; SIGKILL");
            // SAFETY: as above.
            unsafe { libc::kill(*pid, libc::SIGKILL) };
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < deadline && !victims.iter().all(|(p, _)| gone(*p)) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Stop the STA side, then start the keeper again if the file names a hotspot: exactly as ocbm_boot.sh
/// starts it (`setsid sh /script/wifi_sta.sh >> /tmp/box.log 2>&1 </dev/null &`), so its lines reach
/// box.log. Clear (or no ssid=) stops it and starts nothing. Fire and forget for the new keeper: ocbmd
/// ignores SIGCHLD, so nothing may wait on the child.
#[cfg(not(test))]
fn restart_sta_keeper() {
    let iface = sta_if();
    stop_sta_side(&iface);
    let wants_sta = std::fs::read_to_string(WIFI_LINK_FILE).map(|t| t.lines().any(|l| l.starts_with("ssid="))).unwrap_or(false);
    if !wants_sta || !std::path::Path::new("/script/wifi_sta.sh").exists() {
        eprintln!("[ocbmd] wifi: STA side stopped; no hotspot to join");
        return;
    }
    let log = |_: ()| std::fs::OpenOptions::new().append(true).create(true).open("/tmp/box.log").ok();
    let (out, err) = match (log(()), log(())) {
        (Some(o), Some(e)) => (std::process::Stdio::from(o), std::process::Stdio::from(e)),
        _ => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    match std::process::Command::new("setsid")
        .args(["sh", "/script/wifi_sta.sh"])
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
    {
        Ok(_) => eprintln!("[ocbmd] wifi: STA keeper restarted for the new hotspot"),
        Err(e) => eprintln!("[ocbmd] wifi: could not start the STA keeper ({e})"),
    }
}

/// The link key, if the Wi-Fi transport is on.
pub fn link_key() -> Option<LinkKey> {
    let text = std::fs::read_to_string(WIFI_LINK_FILE).ok()?;
    let k = parse_link_file(&text);
    if k.is_none() {
        eprintln!("[ocbmd] wifi: {WIFI_LINK_FILE} has no valid key= line; Wi-Fi transport stays off");
    }
    k
}

/// True once, if [`USB_ONCE_FLAG`] was set; removes it.
pub fn take_usb_once() -> bool {
    std::fs::remove_file(USB_ONCE_FLAG).is_ok()
}

pub fn set_usb_once() {
    let _ = std::fs::write(USB_ONCE_FLAG, b"1\n");
}

/// Who spoke first.
#[derive(Debug, PartialEq, Eq)]
pub enum Winner {
    Usb,
    Tcp,
}

/// Wait until the USB relay's pipe or the TCP relay's pipe holds bytes, without reading them: whichever
/// wins, the loop then reads from the start. `tick` runs about once a second meanwhile (the liveness
/// stamp). USB wins a tie: the cable is the established path.
pub fn wait_first_host(usb: &File, tcp: &File, mut tick: impl FnMut()) -> Winner {
    loop {
        let mut fds = [
            libc::pollfd { fd: usb.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: tcp.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: two valid pollfds for descriptors the caller keeps open.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, 1000) };
        tick();
        if n <= 0 {
            continue;
        }
        if fds[0].revents != 0 {
            return Winner::Usb;
        }
        if fds[1].revents != 0 {
            return Winner::Tcp;
        }
    }
}

fn pipe() -> std::io::Result<(File, File)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: room for the two descriptors pipe(2) writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe(2) just created both, nothing else owns them.
    let ends = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    for f in [&ends.0, &ends.1] {
        // SAFETY: F_SETFD on a descriptor `ends` owns.
        unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok(ends)
}

fn nonblocking(f: &File) {
    // SAFETY: F_GETFL/F_SETFL on a descriptor `f` owns.
    unsafe {
        let fl = libc::fcntl(f.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(f.as_raw_fd(), libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

/// Keepalive: a host that vanished (the car drove off, the hotspot dropped) is noticed in ~11 s, so
/// the daemon respawns and listens again instead of holding a dead link.
fn keepalive(s: &TcpStream) {
    let fd = s.as_raw_fd();
    let on: libc::c_int = 1;
    let set = |level, opt, v: libc::c_int| {
        // SAFETY: setsockopt with a c_int value on a socket `s` owns.
        unsafe {
            libc::setsockopt(fd, level, opt, &v as *const _ as *const libc::c_void, std::mem::size_of::<libc::c_int>() as libc::socklen_t)
        }
    };
    set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, on);
    #[cfg(target_os = "linux")]
    {
        set(libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 5);
        set(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 2);
        set(libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3);
    }
}

/// Open the TCP side: a listener on `0.0.0.0:`[`PORT`] and a thread that takes the first host to
/// complete the handshake. Returns the loop's two ends, shaped like the USB relay's: `from_host`
/// (non-blocking read; EOF = the link ended) and `to_host` (non-blocking write; POLLERR = it ended).
pub fn start_tcp_transport(key: LinkKey) -> std::io::Result<(File, File)> {
    start_tcp_transport_on(key, SocketAddr::from(([0, 0, 0, 0], PORT))).map(|(a, b, _)| (a, b))
}

/// [`start_tcp_transport`] on a chosen address; also returns the bound one (tests use port 0).
pub fn start_tcp_transport_on(key: LinkKey, addr: SocketAddr) -> std::io::Result<(File, File, SocketAddr)> {
    let listener = TcpListener::bind(addr)?;
    let bound = listener.local_addr()?;
    let (from_host, host_bytes) = pipe()?;
    let (frames, to_host) = pipe()?;
    nonblocking(&from_host);
    nonblocking(&to_host);
    eprintln!("[ocbmd] wifi: listening on {bound} (OCBW v1)");
    std::thread::Builder::new()
        .name("wifi-accept".into())
        .stack_size(128 * 1024)
        .spawn(move || accept_one(listener, key, host_bytes, frames))?;
    Ok((from_host, to_host, bound))
}

/// Take hosts until one proves the key, then relay it for good. Strangers are logged and dropped.
fn accept_one(listener: TcpListener, key: LinkKey, host_bytes: File, frames: File) {
    loop {
        let (mut s, peer) = match listener.accept() {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[ocbmd] wifi: accept failed ({e})");
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let _ = s.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
        let _ = s.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
        let nonce = match random32() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[ocbmd] wifi: no randomness ({e}); refusing {peer}");
                continue;
            }
        };
        match box_handshake(&mut s, &key, nonce) {
            Ok(session) => {
                eprintln!("[ocbmd] wifi: host {peer} proved the link key");
                drop(listener); // one host holds the link; the next is taken after the respawn
                run_link(s, session, host_bytes, frames);
                return;
            }
            Err(e) => eprintln!("[ocbmd] wifi: refused {peer}: {e}"),
        }
    }
}

/// The two relay threads for an authenticated host. Each ends on the first failure, closing its pipe
/// end, which the loop takes as the transport's end.
fn run_link(s: TcpStream, session: ocbm_link::Session, mut host_bytes: File, mut frames: File) {
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(None);
    let _ = s.set_write_timeout(Some(SEND_TIMEOUT));
    keepalive(&s);
    let ocbm_link::Session { mut tx, mut rx } = session;
    let mut rs = match s.try_clone() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ocbmd] wifi: socket clone failed ({e}); dropping the link");
            return;
        }
    };
    let mut ws = s;
    let _ = std::thread::Builder::new()
        .name("wifi-read".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut buf = vec![0u8; 32 * 1024];
            let why = loop {
                let n = match rs.read(&mut buf) {
                    Ok(0) => break "host closed the connection".to_string(),
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => break format!("read failed: {e}"),
                };
                rx.push(&buf[..n]);
                let mut failed = None;
                loop {
                    match rx.next() {
                        Ok(Some(plain)) => {
                            if let Err(e) = host_bytes.write_all(&plain) {
                                failed = Some(format!("pipe write failed: {e}"));
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            failed = Some(format!("link error: {e}"));
                            break;
                        }
                    }
                }
                if let Some(w) = failed {
                    break w;
                }
            };
            eprintln!("[ocbmd] wifi reader stopped: {why}");
            let _ = rs.shutdown(std::net::Shutdown::Both);
            // host_bytes drops here: the loop reads EOF and exits for respawn.
        });
    let mut buf = vec![0u8; MAX_RECORD];
    let why = loop {
        let n = match frames.read(&mut buf) {
            Ok(0) => break "the loop closed its end".to_string(),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => break format!("pipe read failed: {e}"),
        };
        if let Err(e) = ws.write_all(&tx.seal(&buf[..n])) {
            break format!("send failed: {e}");
        }
    };
    eprintln!("[ocbmd] wifi writer stopped: {why}");
    let _ = ws.shutdown(std::net::Shutdown::Both);
    // `frames` drops here: the loop's writes fail (POLLERR) and it exits for respawn.
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocbm_link::host_handshake;

    fn provision_body(key: [u8; 32], ssid: &str, psk: &str) -> Vec<u8> {
        let mut b = vec![1];
        b.extend_from_slice(&key);
        b.push(ssid.len() as u8);
        b.extend_from_slice(ssid.as_bytes());
        b.push(psk.len() as u8);
        b.extend_from_slice(psk.as_bytes());
        b
    }

    #[test]
    fn provision_parsing() {
        assert_eq!(parse_provision(&[]), Ok(Provision::Clear));
        let psk = "ab".repeat(32);
        let p = parse_provision(&provision_body([3; 32], "Truck Wi-Fi", &psk)).unwrap();
        assert_eq!(p, Provision::Set { key: [3; 32], ssid: "Truck Wi-Fi".into(), psk: psk.clone() });
        // The file it writes parses back to the same key.
        let text = link_file_text(&[3; 32], "Truck Wi-Fi", &psk);
        assert_eq!(parse_link_file(&text).unwrap().0, [3; 32]);
        assert!(text.contains("ssid=Truck Wi-Fi\n"));
        // Refusals.
        assert!(parse_provision(&provision_body([3; 32], "a\nkey=evil", &psk)).is_err());
        assert!(parse_provision(&provision_body([3; 32], "net", "short")).is_err());
        assert!(parse_provision(&[2]).is_err());
        let mut trailing = provision_body([3; 32], "net", &psk);
        trailing.push(0);
        assert!(parse_provision(&trailing).is_err());
        // No hotspot: transport on, no STA.
        assert_eq!(
            parse_provision(&provision_body([3; 32], "", "")),
            Ok(Provision::Set { key: [3; 32], ssid: String::new(), psk: String::new() })
        );
        assert!(!link_file_text(&[3; 32], "", "").contains("ssid="));
    }

    fn argv(a: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        for s in a {
            v.extend_from_slice(s.as_bytes());
            v.push(0);
        }
        v
    }

    #[test]
    fn sta_side_processes_are_matched_by_argv_never_by_a_mention() {
        assert_eq!(sta_proc_kind(&argv(&["sh", "/script/wifi_sta.sh"]), "sta0"), Some(StaProc::Keeper));
        assert_eq!(sta_proc_kind(&argv(&["sh", "/tmp/wifi/wifi_sta.sh"]), "sta0"), Some(StaProc::Keeper));
        assert_eq!(
            sta_proc_kind(&argv(&["/usr/sbin/wpa_supplicant", "-D", "nl80211,wext", "-i", "sta0", "-c", "/tmp/wpa_sta.conf"]), "sta0"),
            Some(StaProc::Supplicant)
        );
        assert_eq!(sta_proc_kind(&argv(&["udhcpc", "-i", "sta0", "-b", "-p", "/var/run/udhcpc.sta0.pid"]), "sta0"), Some(StaProc::Dhcp));
        // The 2026-11-11 bug: a shell whose -c text merely NAMES the keeper is not the keeper.
        assert_eq!(
            sta_proc_kind(&argv(&["sh", "-c", "pkill -f '[w]ifi_sta.sh'; exec sh /script/wifi_sta.sh"]), "sta0"),
            None
        );
        // Another interface's supplicant (a future second radio), the AP's DHCP server, and strangers.
        assert_eq!(sta_proc_kind(&argv(&["wpa_supplicant", "-i", "wlan0", "-c", "x"]), "sta0"), None);
        assert_eq!(sta_proc_kind(&argv(&["udhcpd", "/etc/udhcpd.conf"]), "sta0"), None);
        assert_eq!(sta_proc_kind(&argv(&["/usr/sbin/hostapd", "/etc/hostapd.conf"]), "sta0"), None);
        assert_eq!(sta_proc_kind(b"", "sta0"), None);
    }

    #[test]
    fn link_file_parsing() {
        let hex = "ab".repeat(32);
        assert!(parse_link_file(&format!("# ocbm wifi\nkey={hex}\n")).is_some());
        assert!(parse_link_file("key=short").is_none());
        assert!(parse_link_file("").is_none());
    }

    fn read_all_nonblocking(f: &mut File, want: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut b = [0u8; 4096];
        while out.len() < want && std::time::Instant::now() < deadline {
            match f.read(&mut b) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&b[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        out
    }

    /// End to end on loopback: a stranger is refused, the real host is relayed both ways, and its leaving
    /// reads as EOF on the loop's end.
    #[test]
    fn tcp_transport_relays_an_authenticated_host_and_refuses_a_stranger() {
        let key = LinkKey([5u8; 32]);
        let (mut from_host, mut to_host, addr) =
            start_tcp_transport_on(key.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();

        // A stranger with the wrong key.
        let mut bad = TcpStream::connect(addr).unwrap();
        bad.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(host_handshake(&mut bad, &LinkKey([6u8; 32]), [1u8; 32]).is_err());

        // The real host.
        let mut h = TcpStream::connect(addr).unwrap();
        h.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut sess = host_handshake(&mut h, &key, [2u8; 32]).unwrap();

        // host -> loop
        h.write_all(&sess.tx.seal(b"HELLO frame bytes")).unwrap();
        assert_eq!(read_all_nonblocking(&mut from_host, 17), b"HELLO frame bytes");

        // loop -> host
        to_host.write_all(b"HELLO_ACK").unwrap();
        let mut got = Vec::new();
        let mut b = [0u8; 256];
        while sess.rx.next().unwrap().map(|p| got.extend_from_slice(&p)).is_none() {
            let n = h.read(&mut b).unwrap();
            sess.rx.push(&b[..n]);
        }
        assert_eq!(got, b"HELLO_ACK");

        // The host leaves: the loop's read end reaches EOF.
        drop(h);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut eof = false;
        while std::time::Instant::now() < deadline {
            match from_host.read(&mut b) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(eof, "the loop's end should read EOF once the host leaves");
    }

    #[test]
    fn first_host_wins_without_consuming_bytes() {
        let (usb_r, mut usb_w) = pipe().unwrap();
        let (tcp_r, _tcp_w) = pipe().unwrap();
        usb_w.write_all(b"x").unwrap();
        assert_eq!(wait_first_host(&usb_r, &tcp_r, || {}), Winner::Usb);
        // still readable: nothing was consumed
        let mut b = [0u8; 1];
        assert_eq!((&usb_r).read(&mut b).unwrap(), 1);
    }
}
