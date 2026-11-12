//! ocbm-host — host-side OCBM client (rusb/libusb). Claims the accessory interface and
//! speaks OCBM v1 over the discovered bulk endpoints.
//!   ocbm-host hello   [vid pid]                  handshake (HELLO -> HELLO_ACK)
//!   ocbm-host console [vid pid]                  MODE_SELECT CONSOLE -> root-shell bridge
//!   ocbm-host settime [vid pid]                  push host wall clock -> box (no RTC battery)
//!   ocbm-host ncm     [vid pid]                  CH_MGMT ENTER_NCM -> box reboots into USB-NCM maintenance
//!                                                (ssh/scp at the NCM address). Return it with
//!                                                `rm /script/ncm_only; reboot` over ssh.
//!   ocbm-host push  [vid pid] <local> <remote> [mode-octal]   verified file deploy (default mode 755)
//!   ocbm-host pull  [vid pid] <remote> <local> [mode-octal]   verified file retrieval (box -> host)
//!   ocbm-host update  vid pid <package.tgz>       push an update package and send MGMT_UPDATE (trial + rollback)
//!   ocbm-host wifi-link vid pid set <ssid>|clear   provision the Wi-Fi transport (MGMT_WIFI_LINK): link key
//!                                                (~/.config/ocbm/link_key, made if absent) + hotspot; the
//!                                                password is read with echo off, only its WPA hash is sent
//!   ocbm-host <mode> tcp:<host>[:port] - [args]   any mode over the box's Wi-Fi transport (OCBW v1,
//!                                                port 7020), key from OCBM_LINK_KEY or ~/.config/ocbm/link_key
//! Every connection also auto-pushes the host clock right after the HELLO_ACK. Without one, nothing
//! else is sent and ocbm-host exits 1.


use ocbm_proto as p;
use rusb::{Context, Device, DeviceHandle, Direction, TransferType, UsbContext};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

/// What carries the OCBM byte stream: the USB accessory's bulk pair, or the Wi-Fi transport's TCP link
/// (OCBW v1: `ocbm-link`'s handshake and records; see `ccpa/ocbmd/src/wifi.rs`).
enum Wire {
    Usb {
        h: DeviceHandle<Context>,
        ep_in: u8,
        ep_out: u8,
    },
    Tcp {
        s: std::net::TcpStream,
        session: ocbm_link::Session,
    },
}

struct Link {
    wire: Wire,
    seq: u32,
    reasm: p::Reassembler,
}

impl Link {
    fn send(&mut self, ch: u16, flags: u8, pl: &[u8]) -> bool {
        let mut buf = vec![0u8; p::HDR_LEN + pl.len()];
        let n = match p::try_frame(&mut buf, ch, flags, self.seq, pl) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[ocbm] refusing oversized frame on ch {ch}: {} B > {} B max", e.len, e.max);
                return false; // seq NOT advanced: no frame went on the wire
            }
        };
        self.seq = self.seq.wrapping_add(1);
        match &mut self.wire {
            Wire::Usb { h, ep_out, .. } => h
                .write_bulk(*ep_out, &buf[..n], Duration::from_secs(2))
                .map(|w| w == n)
                .unwrap_or(false),
            Wire::Tcp { s, session } => {
                use std::io::Write;
                buf[..n]
                    .chunks(ocbm_link::MAX_RECORD)
                    .all(|c| s.write_all(&session.tx.seal(c)).is_ok())
            }
        }
    }
    fn recv(&mut self, out: &mut [u8], deadline: Instant) -> Option<(u16, u8, usize)> {
        loop {
            if let Some(x) = self.reasm.next(out) {
                return Some(x);
            }
            if Instant::now() >= deadline {
                return None;
            }
            let mut tmp = [0u8; 16384];
            match &mut self.wire {
                Wire::Usb { h, ep_in, .. } => {
                    if let Ok(n) = h.read_bulk(*ep_in, &mut tmp, Duration::from_millis(100)) {
                        if n > 0 {
                            self.reasm.push(&tmp[..n]);
                        }
                    }
                }
                Wire::Tcp { s, session } => {
                    use std::io::Read;
                    match s.read(&mut tmp) {
                        Ok(0) => {
                            eprintln!("[ocbm] wifi: the box closed the link");
                            std::process::exit(1);
                        }
                        Ok(n) => {
                            session.rx.push(&tmp[..n]);
                            loop {
                                match session.rx.next() {
                                    Ok(Some(plain)) => self.reasm.push(&plain),
                                    Ok(None) => break,
                                    Err(e) => {
                                        eprintln!("[ocbm] wifi: {e}; dropping the link");
                                        std::process::exit(1);
                                    }
                                }
                            }
                        }
                        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                        Err(e) => {
                            eprintln!("[ocbm] wifi: read failed ({e}); dropping the link");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
    }
}

/// The link key for `tcp:` targets: `OCBM_LINK_KEY` (64 hex), else `~/.config/ocbm/link_key`.
fn host_link_key() -> ocbm_link::LinkKey {
    let text = std::env::var("OCBM_LINK_KEY").ok().or_else(|| {
        let home = std::env::var("HOME").ok()?;
        std::fs::read_to_string(format!("{home}/.config/ocbm/link_key")).ok()
    });
    text.as_deref()
        .and_then(|t| t.lines().find_map(|l| ocbm_link::LinkKey::from_hex(l.trim().trim_start_matches("key="))))
        .unwrap_or_else(|| {
            eprintln!("[ocbm] wifi: no link key (set OCBM_LINK_KEY or ~/.config/ocbm/link_key)");
            std::process::exit(1);
        })
}

/// Connect to a box's Wi-Fi transport at `target` (`host` or `host:port`) and run the handshake.
fn open_tcp(target: &str) -> Link {
    use std::net::ToSocketAddrs;
    let with_port = if target.contains(':') { target.to_string() } else { format!("{target}:{}", ocbm_link::PORT) };
    let addr = with_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .unwrap_or_else(|| {
            eprintln!("[ocbm] wifi: can't resolve {with_port}");
            std::process::exit(1);
        });
    let mut s = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap_or_else(|e| {
        eprintln!("[ocbm] wifi: connect {addr}: {e}");
        std::process::exit(1);
    });
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let nonce = ocbm_link::random32().expect("randomness");
    let session = ocbm_link::host_handshake(&mut s, &host_link_key(), nonce).unwrap_or_else(|e| {
        eprintln!("[ocbm] wifi: handshake with {addr} failed: {e}");
        std::process::exit(1);
    });
    let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
    eprintln!("[ocbm] wifi: linked to {addr} (OCBW v1)");
    Link { wire: Wire::Tcp { s, session }, seq: 0, reasm: p::Reassembler::new() }
}

fn open(vid: u16, pid: u16) -> Link {
    let ctx = Context::new().expect("libusb init");
    let devs = ctx.devices().expect("device list");
    let dev: Device<Context> = devs
        .iter()
        .find(|d| {
            d.device_descriptor()
                .map(|dd| dd.vendor_id() == vid && (pid == 0 || dd.product_id() == pid))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            eprintln!("device {:04x} not found", vid);
            std::process::exit(1);
        });

    // Pick the interface that actually carries the bulk IN+OUT pair (not always IF0).
    let (mut ifnum, mut ep_in, mut ep_out) = (0u8, 0x81u8, 0x01u8);
    if let Ok(cfg) = dev.active_config_descriptor() {
        for iface in cfg.interfaces() {
            for desc in iface.descriptors() {
                let (mut bin, mut bout) = (None, None);
                for ep in desc.endpoint_descriptors() {
                    if ep.transfer_type() == TransferType::Bulk {
                        match ep.direction() {
                            Direction::In => bin = Some(ep.address()),
                            Direction::Out => bout = Some(ep.address()),
                        }
                    }
                }
                if let (Some(i), Some(o)) = (bin, bout) {
                    ifnum = desc.interface_number();
                    ep_in = i;
                    ep_out = o;
                }
            }
        }
    }

    let h = dev.open().unwrap_or_else(|e| {
        eprintln!("open: {}", e);
        std::process::exit(1);
    });
    let _ = h.set_auto_detach_kernel_driver(true);
    h.claim_interface(ifnum).unwrap_or_else(|e| {
        eprintln!("claim IF{}: {}", ifnum, e);
        std::process::exit(1);
    });
    eprintln!(
        "[ocbm] IF{} bulk IN=0x{:02x} OUT=0x{:02x}",
        ifnum, ep_in, ep_out
    );
    Link {
        wire: Wire::Usb { h, ep_in, ep_out },
        seq: 0,
        reasm: p::Reassembler::new(),
    }
}

/// This process's HOST INSTANCE NONCE — `CT_HELLO` bytes 2..6 (`ocbm_proto::CT_HELLO`,
/// docs/carplay/01_OCBM_PROTOCOL.md). The box compares it across reattaches to tell a relaunched
/// host from the same host reconnecting, so it must be stable for the life of the process and
/// differ between runs; 0 means "not supplied", hence the `max(1)`. It is NOT a capability word —
/// there is no host->box capability field, and sending one here (this used to send `0x13`) made
/// every run look like the same host, so the box could never latch `host_replaced`.
fn host_instance_nonce() -> u32 {
    static NONCE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *NONCE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        ((std::process::id() as u64 ^ nanos) as u32).max(1)
    })
}

/// HELLO, and wait for the HELLO_ACK. True when it came.
fn do_hello(link: &mut Link) -> bool {
    let mut hi = [0u8; 6];
    hi[0] = p::CT_HELLO;
    hi[1] = p::VERSION;
    hi[2..6].copy_from_slice(&host_instance_nonce().to_le_bytes());
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &hi);
    // REATTACH RESYNC: a prior session may have left kernel-buffered A/V/bulk frames ahead of our
    // HELLO_ACK that the box cannot retract. Drain frames until the ACK (or timeout) rather than
    // giving up on the first — the box also clears its software queues on HELLO. `out` is full-size
    // so a stale A/V frame doesn't truncate. Re-send HELLO once mid-wait in case the first was lost.
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + Duration::from_secs(3);
    let resend_at = Instant::now() + Duration::from_millis(1500);
    let mut stale = 0u32;
    let mut resent = false;
    while Instant::now() < deadline {
        // Bound each recv by the NEXT actionable moment (the resend, then the final deadline) rather
        // than the full deadline: `link.recv` blocks until ITS deadline when the link is silent, so a
        // single recv(&out, deadline) on a truly silent link (the lost-first-HELLO case) would consume
        // the whole 3 s and the mid-wait HELLO resend below would never fire — the exact case it exists
        // for (audit Fix #13). Slice to resend_at first, then to deadline; the resend runs after the
        // (short) recv returns.
        let slice_deadline = if resent { deadline } else { resend_at.min(deadline) };
        if let Some((ch, _, l)) = link.recv(&mut out, slice_deadline) {
            if ch == p::CH_CTRL && l >= 6 && out[0] == p::CT_HELLO_ACK {
                let caps = u32::from_le_bytes(out[2..6].try_into().unwrap());
                if stale > 0 {
                    eprintln!("[ocbm] drained {stale} stale frame(s) before HELLO_ACK (reattach)");
                }
                println!(
                    "HELLO_ACK: box v{} caps=0x{:08x} mode={}",
                    out[1],
                    caps,
                    if l >= 7 { out[6] } else { 0 }
                );
                return true;
            }
            stale += 1; // leftover frame from a prior session — keep draining toward the ACK
        }
        if !resent && Instant::now() >= resend_at {
            link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &hi);
            resent = true;
        }
    }
    eprintln!("no HELLO_ACK");
    false
}

/// Push the host wall clock to the box (CT_SETTIME) and consume the box's ack.
/// The box has no RTC battery, so its clock is bogus at boot; a valid clock is
/// required for TLS/AirPlay pairing. Best-effort: a box without the handler just
/// won't ack (times out fast). Returns true on a confirmed apply.
fn push_time(link: &mut Link) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut req = vec![p::CT_SETTIME];
    req.extend_from_slice(&now.to_le_bytes());
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &req);
    // Full-size buffer (the reassembler contract) + loop until the ack, filtering CH_CTRL/CT_SETTIME:
    // a leftover stale frame from reattach must not be mistaken for "no ack".
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + Duration::from_secs(1);
    while let Some((ch, _, l)) = link.recv(&mut out, deadline) {
        if ch == p::CH_CTRL && l >= 10 && out[0] == p::CT_SETTIME {
            let t = u64::from_le_bytes(out[1..9].try_into().unwrap());
            if out[9] == 0 {
                eprintln!("[ocbm] box clock set to {} (unix)", t);
                return true;
            }
            eprintln!("[ocbm] box rejected clock set (settimeofday failed)");
            return false;
        }
        // else: a stale/other frame — keep draining until the deadline
    }
    eprintln!("[ocbm] no CT_SETTIME ack (box handler absent?)");
    false
}

fn mode_install(link: &mut Link, what: &str) {
    let verb = match what {
        "info" => p::MGMT_GET_INFO,
        "confirm" => p::MGMT_INSTALL_CONFIRM,
        "stock" => p::MGMT_GO_STOCK,
        _ => {
            eprintln!("usage: ocbm-host install [vid pid] info|confirm|stock");
            std::process::exit(2);
        }
    };
    link.send(p::CH_MGMT, p::F_SOM | p::F_EOM, &[verb]);
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while Instant::now() < deadline {
        let Some((ch, _flags, l)) = link.recv(&mut out, deadline) else { break };
        if ch != p::CH_MGMT || l == 0 {
            continue; // clock ack / keepalive
        }
        if verb == p::MGMT_GET_INFO && out[0] == p::MGMT_INFO {
            println!("{}", String::from_utf8_lossy(&out[1..l]));
            return;
        }
        if l >= 3 && out[0] == p::MGMT_ACK && out[1] == verb {
            if out[2] == 0 {
                println!("install {what}: accepted");
                if verb == p::MGMT_INSTALL_CONFIRM {
                    println!("install confirm: unplug the adapter and plug it back in; the finish runs at its next start");
                }
                return;
            }
            eprintln!("install {what}: refused by the box (status {})", out[2]);
            std::process::exit(1);
        }
    }
    eprintln!("install {what}: no answer within 5 s");
    std::process::exit(1);
}

/// WPA's key from a passphrase: PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 32) (IEEE 802.11i H.4).
fn wpa_psk(passphrase: &[u8], ssid: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    type H = Hmac<sha1::Sha1>;
    let mut out = [0u8; 32];
    for (block, chunk) in out.chunks_mut(20).enumerate() {
        let mut m = <H as Mac>::new_from_slice(passphrase).expect("any key length");
        m.update(ssid);
        m.update(&(block as u32 + 1).to_be_bytes());
        let mut u = m.finalize().into_bytes();
        let mut t = u;
        for _ in 1..4096 {
            let mut m = <H as Mac>::new_from_slice(passphrase).expect("any key length");
            m.update(&u);
            u = m.finalize().into_bytes();
            t.iter_mut().zip(u.iter()).for_each(|(a, b)| *a ^= b);
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
    out
}

/// A line from stdin with the terminal's echo off (a password).
fn read_hidden(prompt: &str) -> String {
    use std::io::{BufRead, Write};
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    // SAFETY: termios get/set on stdin; restored before returning.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let tty = unsafe { libc::isatty(0) } == 1 && unsafe { libc::tcgetattr(0, &mut saved) } == 0;
    if tty {
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &quiet) };
    }
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    if tty {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved) };
        eprintln!();
    }
    line.trim_end_matches(['\r', '\n']).to_string()
}

/// The link key at ~/.config/ocbm/link_key, made (0600) if there is none.
fn host_link_key_or_new() -> [u8; 32] {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = format!("{}/.config/ocbm", std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    let path = format!("{dir}/link_key");
    if let Some(k) = std::fs::read_to_string(&path).ok().and_then(|t| ocbm_link::LinkKey::from_hex(t.trim())) {
        eprintln!("[ocbm] wifi: reusing the link key at {path}");
        return k.0;
    }
    let k = ocbm_link::random32().expect("randomness");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)
        .unwrap_or_else(|e| { eprintln!("[ocbm] wifi: can't write {path}: {e}"); std::process::exit(1) });
    use std::io::Write;
    let _ = writeln!(f, "{}", ocbm_link::LinkKey(k).to_hex());
    eprintln!("[ocbm] wifi: new link key -> {path}");
    k
}

/// `wifi-link set <ssid>` / `wifi-link clear`: provision the box's Wi-Fi transport (MGMT_WIFI_LINK). The
/// hotspot password is read with echo off and only its WPA hash is sent.
fn mode_wifi_link(link: &mut Link, what: &str, ssid: Option<&str>) {
    let body: Vec<u8> = match (what, ssid) {
        ("clear", _) => vec![p::MGMT_WIFI_LINK],
        ("set", Some(ssid)) => {
            let key = host_link_key_or_new();
            let psk = if ssid.is_empty() {
                String::new()
            } else {
                let pw = read_hidden(&format!("Password for \"{ssid}\": "));
                if !(8..=63).contains(&pw.len()) {
                    eprintln!("[ocbm] wifi: WPA2 passwords are 8-63 characters");
                    std::process::exit(2);
                }
                wpa_psk(pw.as_bytes(), ssid.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
            };
            let mut b = vec![p::MGMT_WIFI_LINK, 1];
            b.extend_from_slice(&key);
            b.push(ssid.len() as u8);
            b.extend_from_slice(ssid.as_bytes());
            b.push(psk.len() as u8);
            b.extend_from_slice(psk.as_bytes());
            b
        }
        _ => {
            eprintln!("usage: ocbm-host wifi-link <vid pid | tcp:<ip> -> set <ssid> | clear");
            std::process::exit(2);
        }
    };
    link.send(p::CH_MGMT, p::F_SOM | p::F_EOM, &body);
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Some((ch, _flags, l)) = link.recv(&mut out, deadline) else { break };
        if ch == p::CH_MGMT && l >= 3 && out[0] == p::MGMT_ACK && out[1] == p::MGMT_WIFI_LINK {
            if out[2] == 0 {
                println!("wifi-link {what}: done{}", if what == "set" { "; the box joins the hotspot now, and listens on Wi-Fi from ocbmd's next start" } else { "" });
                return;
            }
            eprintln!("wifi-link {what}: refused by the box (status {})", out[2]);
            std::process::exit(1);
        }
    }
    eprintln!("wifi-link {what}: no answer within 5 s (box without the Wi-Fi transport?)");
    std::process::exit(1);
}

/// `update <package.tgz>`: push an OCBM update package (tools/make_ocbm_update.sh) to /tmp/ocbm-update.tgz
/// and send MGMT_UPDATE with its SHA-256, as Carlink does. The box checks the package and installs it as a
/// trial that keeps the new version on a passing self-test and puts the old one back otherwise; it restarts.
fn mode_update(link: &mut Link, pkg: &str) {
    let data = std::fs::read(pkg).unwrap_or_else(|e| {
        eprintln!("update: can't read {pkg}: {e}");
        std::process::exit(2);
    });
    let sha: String = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect()
    };
    if !mode_push(link, pkg, "/tmp/ocbm-update.tgz", 0o644) {
        eprintln!("update: the push failed; nothing was started");
        std::process::exit(1);
    }
    let mut req = vec![p::MGMT_UPDATE];
    req.extend_from_slice(sha.as_bytes());
    link.send(p::CH_MGMT, p::F_SOM | p::F_EOM, &req);
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let Some((ch, _flags, l)) = link.recv(&mut out, deadline) else { break };
        if ch == p::CH_MGMT && l >= 3 && out[0] == p::MGMT_ACK && out[1] == p::MGMT_UPDATE {
            if out[2] == 0 {
                println!("update: accepted (sha256 {}); the box installs it as a trial and restarts", &sha[..16]);
                return;
            }
            eprintln!("update: refused by the box (status {}): no package, a different SHA-256, or no update script", out[2]);
            std::process::exit(1);
        }
    }
    eprintln!("update: no answer within 10 s");
    std::process::exit(1);
}

fn mode_ncm(link: &mut Link) {
    link.send(p::CH_MGMT, p::F_SOM | p::F_EOM, &[p::MGMT_ENTER_NCM]);
    // Same reassembler contract as push/pull: full-size buffer, and filter the CT_SETTIME chatter the
    // clock auto-push generates on every connect.
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while Instant::now() < deadline {
        let Some((ch, _flags, l)) = link.recv(&mut out, deadline) else { break };
        if ch == p::CH_CTRL {
            continue; // clock ack / keepalive
        }
        if ch == p::CH_MGMT && l >= 3 && out[0] == p::MGMT_ACK && out[1] == p::MGMT_ENTER_NCM {
            if out[2] == 0 {
                println!("ENTER_NCM accepted — the box is rebooting into NCM maintenance mode.");
                println!("It will NOT come back on USB-OCBM until returned with: rm /script/ncm_only; reboot");
                return;
            }
            eprintln!("ENTER_NCM refused by the box (status {}) — /script/ncm_only was not armed", out[2]);
            std::process::exit(1);
        }
    }
    eprintln!("ENTER_NCM: no ACK within 5 s — the box did not accept it; NOT assuming it rebooted");
    std::process::exit(1);
}

fn mode_console(link: &mut Link) {
    link.send(
        p::CH_CTRL,
        p::F_SOM | p::F_EOM,
        &[p::CT_MODE_SELECT, p::MODE_CONSOLE],
    );
    let orig = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        libc::tcgetattr(0, &mut t);
        t
    };
    let mut raw = orig;
    unsafe {
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(0, libc::TCSANOW, &raw);
    }
    eprintln!("[ocbm-rescue] CONSOLE bridged (Ctrl-] to quit)");
    // Reassembler::next requires out.len() >= MAX_PAYLOAD (it debug_asserts, and in release drops
    // any frame whose plen exceeds out.len()). Every other caller uses p::MAX_PAYLOAD; this was the
    // lone 16 KiB outlier, which would panic in debug or silently drop CONSOLE frames > 16 KiB.
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    'outer: loop {
        let mut fds = [libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        }];
        if unsafe { libc::poll(fds.as_mut_ptr(), 1, 5) } > 0 && (fds[0].revents & libc::POLLIN) != 0
        {
            let mut inb = [0u8; 1024];
            let k = unsafe { libc::read(0, inb.as_mut_ptr() as *mut libc::c_void, inb.len()) };
            if k <= 0 {
                break;
            }
            let k = k as usize;
            if inb[..k].contains(&0x1d) {
                break 'outer; // Ctrl-]
            }
            link.send(p::CH_CONSOLE, p::F_SOM | p::F_EOM, &inb[..k]);
        }
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(15)) {
            if ch == p::CH_CONSOLE {
                use std::io::Write;
                let _ = std::io::stdout().write_all(&out[..l]);
                let _ = std::io::stdout().flush();
            }
        }
    }
    unsafe { libc::tcsetattr(0, libc::TCSANOW, &orig) };
}

/// IP stream channel: OPEN a connection to a box-side target and print what it returns.
/// (First-cut validation; a full local-listener `forward` mode builds on this.)
fn recv_file_ack(link: &mut Link, secs: u64) -> Option<(u8, u32, u32)> {
    let mut out = vec![0u8; p::MAX_PAYLOAD]; // full-size per the reassembler contract
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some((ch, _, l)) = link.recv(&mut out, deadline) {
            if ch == p::CH_FILE && l >= 10 && out[0] == p::FILE_ACK {
                return Some((
                    out[1],
                    u32::from_le_bytes(out[2..6].try_into().unwrap()),
                    u32::from_le_bytes(out[6..10].try_into().unwrap()),
                ));
            }
        }
    }
    None
}

/// Pull a file FROM the box over CH_FILE (the mirror of push): send FILE_PULL(path), then read the
/// FILE_DATA sub-frames the box streams back until the terminal FILE_ACK, reassemble, verify the
/// end-to-end CRC-32 + size, and write `local`. Returns true on success. `mode` is applied to the
/// written file. A stalled/absent box CH_FILE handler → timeout → false (nothing written).
fn mode_pull(link: &mut Link, remote: &str, local: &str, mode: u32) -> bool {
    let mut req = vec![p::FILE_PULL];
    req.extend_from_slice(remote.as_bytes());
    if !link.send(p::CH_FILE, p::F_SOM | p::F_EOM, &req) {
        eprintln!("[pull] request send failed");
        return false;
    }
    let t0 = Instant::now();
    let mut data: Vec<u8> = Vec::new();
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    // Overall deadline is generous (a large binary over the console-shared link); each recv waits until
    // it, and a terminal FILE_ACK ends the loop early on success or error.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match link.recv(&mut out, deadline) {
            Some((ch, _, l)) if ch == p::CH_FILE && l >= 1 => match out[0] {
                p::FILE_DATA => data.extend_from_slice(&out[1..l]),
                p::FILE_ACK if l >= 10 => {
                    let status = out[1];
                    let rcrc = u32::from_le_bytes(out[2..6].try_into().unwrap());
                    let rsize = u32::from_le_bytes(out[6..10].try_into().unwrap());
                    if status != p::FILE_OK {
                        eprintln!("[pull] box refused (status {status}) — bad path / not found?");
                        return false;
                    }
                    let crc = p::crc32(&data);
                    if crc != rcrc || data.len() as u32 != rsize {
                        eprintln!(
                            "[pull] VERIFY FAILED: got crc=0x{:08x} size={} vs box crc=0x{:08x} size={}",
                            crc, data.len(), rcrc, rsize
                        );
                        return false;
                    }
                    if let Err(e) = std::fs::write(local, &data) {
                        eprintln!("[pull] write {local}: {e}");
                        return false;
                    }
                    let _ = std::fs::set_permissions(local, std::fs::Permissions::from_mode(mode));
                    let dt = t0.elapsed().as_secs_f64();
                    println!(
                        "[pull] {} -> {} : {} bytes, crc32=0x{:08x}, mode={:o}, {:.2}s ({:.1} Mbps)",
                        remote, local, rsize, rcrc, mode, dt,
                        rsize as f64 * 8.0 / 1e6 / dt.max(1e-6)
                    );
                    return true;
                }
                _ => {}
            },
            Some(_) => {} // other channel traffic — ignore
            None => {
                eprintln!("[pull] timeout ({} bytes received)", data.len());
                return false;
            }
        }
    }
}

/// Push a local file to the box over CH_FILE: OPEN -> stream DATA -> CLOSE(crc,size).
/// The box verifies the end-to-end CRC-32, chmods to `mode`, and atomically renames into
/// place — a reliable replacement for base64-over-UART deploys. Returns true on success.
fn mode_push(link: &mut Link, local: &str, remote: &str, mode: u32) -> bool {
    let data = match std::fs::read(local) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[push] read {}: {}", local, e);
            return false;
        }
    };
    let crc = p::crc32(&data);
    let size = data.len() as u32;

    // OPEN
    let mut open = vec![p::FILE_OPEN];
    open.extend_from_slice(&mode.to_le_bytes());
    open.extend_from_slice(remote.as_bytes());
    link.send(p::CH_FILE, p::F_SOM | p::F_EOM, &open);
    match recv_file_ack(link, 3) {
        Some((p::FILE_OK, _, _)) => {}
        Some((st, _, _)) => {
            eprintln!("[push] OPEN rejected (status {})", st);
            return false;
        }
        None => {
            eprintln!("[push] no OPEN ack (box CH_FILE handler absent?)");
            return false;
        }
    }

    // DATA — one sub-frame per chunk; type byte + chunk must fit one OCBM payload.
    let t0 = Instant::now();
    let chunk = p::MAX_PAYLOAD - 1;
    for part in data.chunks(chunk) {
        let mut d = Vec::with_capacity(1 + part.len());
        d.push(p::FILE_DATA);
        d.extend_from_slice(part);
        if !link.send(p::CH_FILE, p::F_SOM | p::F_EOM, &d) {
            eprintln!("[push] DATA send failed");
            return false;
        }
    }

    // CLOSE — box verifies crc+size, chmods, renames.
    let mut close = vec![p::FILE_CLOSE];
    close.extend_from_slice(&crc.to_le_bytes());
    close.extend_from_slice(&size.to_le_bytes());
    link.send(p::CH_FILE, p::F_SOM | p::F_EOM, &close);
    match recv_file_ack(link, 5) {
        Some((p::FILE_OK, rcrc, rsize)) => {
            let dt = t0.elapsed().as_secs_f64();
            println!(
                "[push] {} -> {} : {} bytes, crc32=0x{:08x}, mode={:o}, {:.2}s ({:.1} Mbps)",
                local,
                remote,
                rsize,
                rcrc,
                mode,
                dt,
                size as f64 * 8.0 / 1e6 / dt.max(1e-6)
            );
            true
        }
        Some((p::FILE_ERR_VERIFY, rcrc, rsize)) => {
            eprintln!(
                "[push] VERIFY FAILED: box got crc=0x{:08x} size={} vs host crc=0x{:08x} size={}",
                rcrc, rsize, crc, size
            );
            false
        }
        Some((st, _, _)) => {
            eprintln!("[push] CLOSE failed (status {})", st);
            false
        }
        None => {
            eprintln!("[push] no CLOSE ack");
            false
        }
    }
}

/// Decode a bridged ethernet frame into a one-line summary — enough to confirm the iPhone's
/// wired-CarPlay discovery (link-local IPv6: NDP/RS/RA + mDNS) is traversing OCBM to the Mac.
fn mode_session(link: &mut Link, secs: u64, cfg_path: Option<&str>) {
    let mut sub = vec![p::CT_SUBSCRIBE];
    match cfg_path.and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => {
            eprintln!(
                "[session] SUBSCRIBE with {} B config from {}",
                bytes.len(),
                cfg_path.unwrap()
            );
            sub.extend_from_slice(&bytes);
        }
        None => eprintln!("[session] SUBSCRIBE (no config)"),
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &sub);
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = if secs > 0 {
        Some(Instant::now() + Duration::from_secs(secs))
    } else {
        None
    };
    let mut next_hb = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        // drain any SESSION_EVENTs (short wait so heartbeats stay ~1/s)
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(250)) {
            if ch == p::CH_CTRL && l >= 2 && out[0] == p::CT_SESSION_EVENT {
                let ev = match out[1] {
                    x if x == p::SEV_HOST_PRESENT => "HOST_PRESENT",
                    x if x == p::SEV_HOST_GONE => "HOST_GONE",
                    _ => "?",
                };
                println!("[session] box event: {ev}");
            }
        }
        if Instant::now() >= next_hb {
            link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_HEARTBEAT]);
            next_hb = Instant::now() + Duration::from_secs(1);
        }
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
    println!("[session] STOP sent — clean exit");
}

/// `av` — receive the box's A/V over OCBM (CH_VIDEO / CH_MEDIA_AUDIO), count it, and save the video
/// stream to a file. Validates the box-session -> ocbmd -> OCBM -> host A/V path with a live iPhone.
fn mode_av(link: &mut Link, secs: u64) {
    eprintln!(
        "[av] receiving CH_VIDEO/CH_MEDIA_AUDIO{} — saving video to /tmp/carplay_video.bin",
        if secs > 0 {
            format!(" for {secs}s")
        } else {
            " (forever)".into()
        }
    );
    use std::io::Write;
    let mut vfile = std::fs::File::create("/tmp/carplay_video.bin").ok();
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let (mut vb, mut ab, mut vf, mut af) = (0u64, 0u64, 0u64, 0u64);
    let deadline = if secs > 0 {
        Some(Instant::now() + Duration::from_secs(secs))
    } else {
        None
    };
    let mut last = Instant::now();
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(300)) {
            match ch {
                p::CH_VIDEO => {
                    vf += 1;
                    vb += l as u64;
                    if let Some(f) = vfile.as_mut() {
                        let _ = f.write_all(&out[..l]);
                    }
                }
                p::CH_MEDIA_AUDIO => {
                    af += 1;
                    ab += l as u64;
                }
                _ => {}
            }
        }
        if last.elapsed() >= Duration::from_secs(2) {
            eprintln!("[av] video {vf} frm / {vb} B · audio {af} frm / {ab} B");
            last = Instant::now();
        }
    }
    println!("[av] total — video: {vf} frames / {vb} bytes (→ /tmp/carplay_video.bin) · audio: {af} frames / {ab} bytes");
}

/// `avdec` — COMMITTED-MODEL validation. The box runs with `OCBM_FWD_ENC=1`, so it forwards the
/// *encrypted* video + hands the per-stream key (NO on-box decrypt). This mode reassembles the seam
/// byte stream from CH_VIDEO, takes the handed key, and decrypts on the HOST — proving the committed
/// "forward encrypted + hand the session key" path end to end.
///
/// Wire (each message = `[u32 BE len][payload]`, reassembled across CH_VIDEO frames):
///   key    payload: `[SEAM_MAGIC "SEAV"][0x00][key.output 32B][scid 8B LE]`
///   frame  payload: `[SEAM_MAGIC "SEAV"][0x01][seq u64 LE][hdr 128B][body]`  — opcode = hdr[4]: 0 = encrypted VideoFrame, 1 = plaintext avcC.
/// Per VideoFrame the host reconstructs `nonce = [0,0,0,0] ++ seq_le64` from the WIRE seq (#678 — a
/// local counter desyncs permanently at the first gated/dropped frame), `aad = hdr`, `msg = body`.
/// The box's screen-seam resync marker ("SEAV"), byte-for-byte the receiver's `SEAM_MAGIC`. Every
/// forwarded screen message on CH_VIDEO/CH_ALT_VIDEO begins with it.
const SEAM_MAGIC: [u8; 4] = *b"SEAV";
/// Per-frame length ceiling for the screen seam (#651). Large enough for any single 4K IDR the box
/// forwards as one length-prefixed message, small enough to reject a garbled length prefix.
const SEAM_VID_MAX: usize = 4 * 1024 * 1024;

fn mode_avdec(link: &mut Link, secs: u64, cfg_path: Option<&str>) {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    use std::io::Write;
    eprintln!("[avdec] full host receiver — SUBSCRIBE + heartbeat + decrypt box A/V, then STOP");
    // Act as the live host receiver: SUBSCRIBE (arms the box session via the supervisor), HEARTBEAT to
    // stay present, drain + decrypt CH_VIDEO/CH_MEDIA_AUDIO, then STOP. One USB claim does the whole loop.
    let mut sub = vec![p::CT_SUBSCRIBE];
    if let Some(bytes) = cfg_path.and_then(|pp| std::fs::read(pp).ok()) {
        eprintln!("[avdec] SUBSCRIBE with {} B config", bytes.len());
        sub.extend_from_slice(&bytes);
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &sub);
    let mut next_hb = Instant::now() + Duration::from_secs(1);
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let mut buf: Vec<u8> = Vec::new(); // video seam reassembly
    let mut cipher: Option<ChaCha20Poly1305> = None;
    let (mut ok, mut fail, mut cfg) = (0u64, 0u64, 0u64);
    let mut plain_bytes = 0u64;
    let mut dec = std::fs::File::create("/tmp/carplay_dec.h264").ok();
    // Audio (CH_MEDIA_AUDIO): same seam framing; each 0x01 payload is a raw RTP packet. Per-packet the
    // nonce is IN the packet (no counter): nonce = [0,0,0,0] ++ payload trailing 8B, aad = pkt[4..12],
    // ct‖tag = payload[..len-8]  (payload = pkt[12..]). Key handed on the 0x00 message like video.
    let mut abuf: Vec<u8> = Vec::new();
    // Seam v2: every audio message is scid-tagged (media :9002 AND voice :9003 → CH_ALT_AUDIO share
    // the framing), so keys live in a per-scid map — concurrent streams can't clobber each other.
    let mut aciphers: std::collections::HashMap<u64, ChaCha20Poly1305> =
        std::collections::HashMap::new();
    let (mut aok, mut afail) = (0u64, 0u64);
    let deadline = if secs > 0 {
        Some(Instant::now() + Duration::from_secs(secs))
    } else {
        None
    };
    let mut last = Instant::now();
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(300)) {
            if ch == p::CH_VIDEO {
                buf.extend_from_slice(&out[..l]);
            } else if ch == p::CH_MEDIA_AUDIO || ch == p::CH_ALT_AUDIO {
                abuf.extend_from_slice(&out[..l]); // v2 framing is scid-tagged — one drain serves both
            } else if ch == p::CH_CTRL && l >= 2 && out[0] == p::CT_SESSION_EVENT {
                // Label all four event kinds (#637): the old two-way branch mislabeled the phone-presence
                // events (0x03/0x04) as "HOST_GONE".
                match out[1] {
                    p::SEV_HOST_PRESENT => eprintln!("[avdec] box session event: HOST_PRESENT"),
                    p::SEV_HOST_GONE => eprintln!("[avdec] box session event: HOST_GONE"),
                    p::SEV_PHONE_PRESENT => eprintln!("[avdec] box session event: PHONE_PRESENT"),
                    p::SEV_PHONE_ABSENT => eprintln!("[avdec] box session event: PHONE_ABSENT"),
                    other => eprintln!("[avdec] box session event: unknown 0x{other:02x}"),
                }
            }
        }
        if Instant::now() >= next_hb {
            link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_HEARTBEAT]);
            next_hb = Instant::now() + Duration::from_secs(1);
        }
        // Drain every complete length-prefixed message the reassembly buffer now holds.
        loop {
            if buf.len() < 4 {
                break;
            }
            let mlen = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            // Plausibility bound raised (#651): a single 4K IDR access unit routinely exceeds the old
            // 2×MAX_PAYLOAD (128 KiB) ceiling — the box sends the whole frame as ONE length-prefixed
            // message — so that bound was rejecting legitimate keyframes as "desync". SEAM_VID_MAX is a
            // generous per-frame cap that still catches a truly garbled length prefix.
            if mlen == 0 || mlen > SEAM_VID_MAX {
                buf.drain(0..1); // implausible length prefix: resync one byte (never wedge on desync)
                continue;
            }
            if buf.len() < 4 + mlen {
                if buf.len() > 2 * SEAM_VID_MAX {
                    buf.drain(0..1); // a plausible length that never completes — treat as desync, resync
                    continue;
                }
                break; // wait for the rest of the message
            }
            let msg = buf[4..4 + mlen].to_vec();
            buf.drain(0..4 + mlen);
            // Current box wire (#664): every screen message is `[SEAM_MAGIC "SEAV"][marker][…]` — the
            // magic prefix (+ an 8-byte seq on frames) that this replica used to ignore, parsing the
            // marker at msg[0]. Realign to the real layout; on a missing magic, resync one byte.
            if msg.len() < 5 || msg[0..4] != SEAM_MAGIC {
                continue; // no SEAM magic where a message boundary was expected — skip this message
            }
            match msg[4] {
                // KEY: [magic 4][0x00][key 32][scid 8 LE]
                0x00 if msg.len() >= 4 + 1 + 32 => {
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&msg[5..37]);
                    cipher = Some(ChaCha20Poly1305::new(Key::from_slice(&k)));
                    eprintln!("[avdec] received video key from box — decrypting on host");
                }
                // FRAME: [magic 4][0x01][seq u64 LE 8][hdr 128][body]
                0x01 if msg.len() >= 4 + 1 + 8 + 128 => {
                    let seq = u64::from_le_bytes(msg[5..13].try_into().unwrap());
                    let hdr = &msg[13..141];
                    let body = &msg[141..];
                    match hdr[4] {
                        0 => {
                            if let (Some(c), true) = (cipher.as_ref(), body.len() >= 16) {
                                // Nonce from the WIRE seq (#678), not a local per-frame counter: the
                                // ChaCha20 nonce must equal the iPhone's VideoFrame counter, and any
                                // dropped/gated frame on the box advances that counter — a local counter
                                // would desync permanently after the first gap, failing every Poly1305
                                // tag thereafter.
                                let mut nonce = [0u8; 12];
                                nonce[4..].copy_from_slice(&seq.to_le_bytes());
                                match c.decrypt(
                                    Nonce::from_slice(&nonce),
                                    Payload {
                                        msg: body,
                                        aad: hdr,
                                    },
                                ) {
                                    Ok(pt) => {
                                        ok += 1;
                                        plain_bytes += pt.len() as u64;
                                        if let Some(f) = dec.as_mut() {
                                            let _ = f.write_all(&pt);
                                        }
                                    }
                                    Err(_) => fail += 1,
                                }
                            }
                        }
                        1 => cfg += 1, // plaintext avcC/hvcC config (no decrypt)
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        // Audio: same [u32 len][marker][payload] framing on CH_MEDIA_AUDIO.
        loop {
            if abuf.len() < 4 {
                break;
            }
            let mlen = u32::from_be_bytes([abuf[0], abuf[1], abuf[2], abuf[3]]) as usize;
            if mlen == 0 || mlen > 2 * p::MAX_PAYLOAD {
                abuf.drain(0..1); // implausible length prefix: resync one byte
                continue;
            }
            if abuf.len() < 4 + mlen {
                if abuf.len() > 8 * p::MAX_PAYLOAD {
                    abuf.drain(0..1); // never-completing length: resync
                    continue;
                }
                break;
            }
            let mut msg = abuf[4..4 + mlen].to_vec();
            abuf.drain(0..4 + mlen);
            // Since 2026-09-03 the audio seam is magic-tagged like the video seam:
            // `[u32 BE len][SEAM_MAGIC "SEAV"][marker]…`. Strip it when present so this tool reads
            // both wires; a pre-magic box build still sends `[len][marker]`. (This debug receiver does
            // not implement the full scan-forward resync the macOS host does — it only needs to parse
            // a healthy stream.)
            if msg.len() >= 5 && msg[..4] == [0x53, 0x45, 0x41, 0x56] {
                msg.drain(0..4);
            }
            match msg[0] {
                // SEAM_KEY: [0x00][key 32][scid 8]
                0x00 if msg.len() >= 41 => {
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&msg[1..33]);
                    let scid = u64::from_le_bytes(msg[33..41].try_into().unwrap());
                    aciphers.insert(scid, ChaCha20Poly1305::new(Key::from_slice(&k)));
                    eprintln!("[avdec] received audio key (scid={scid}) — decrypting on host");
                }
                // SEAM_FORMAT: [0x02][scid 8][codec][rate u32 LE][ch][bits][audio_type]
                0x02 if msg.len() >= 17 => {
                    let scid = u64::from_le_bytes(msg[1..9].try_into().unwrap());
                    let codec = msg[9];
                    let rate = u32::from_le_bytes(msg[10..14].try_into().unwrap());
                    let (chn, bits, at) = (msg[14], msg[15], msg[16]);
                    eprintln!("[avdec] audio format scid={scid}: codec={codec} {rate}Hz {chn}ch {bits}bit atype={at}");
                }
                // SEAM_PKT: [0x01][scid 8][raw RTP datagram]
                0x01 if msg.len() >= 9 => {
                    let scid = u64::from_le_bytes(msg[1..9].try_into().unwrap());
                    let pkt = &msg[9..];
                    // MIN_AUDIO_PACKET = 12 (hdr) + 16 (tag) + 8 (nonce)
                    if let (Some(c), true) = (aciphers.get(&scid), pkt.len() >= 12 + 16 + 8) {
                        let aad = &pkt[4..12];
                        let payload = &pkt[12..];
                        let n = payload.len();
                        let mut nonce = [0u8; 12];
                        nonce[4..].copy_from_slice(&payload[n - 8..]);
                        let ct_and_tag = &payload[..n - 8];
                        match c.decrypt(
                            Nonce::from_slice(&nonce),
                            Payload {
                                msg: ct_and_tag,
                                aad,
                            },
                        ) {
                            Ok(_) => aok += 1,
                            Err(_) => afail += 1,
                        }
                    }
                }
                _ => {}
            }
        }
        if last.elapsed() >= Duration::from_secs(2) {
            eprintln!(
                "[avdec] video {ok} OK / {fail} fail · {cfg} cfg · {plain_bytes} B  |  audio {aok} OK / {afail} fail"
            );
            last = Instant::now();
        }
    }
    println!(
        "[avdec] COMMITTED forward-encrypted — video {ok} decrypted / {fail} failed / {cfg} config ({plain_bytes} B AVCC → /tmp/carplay_dec.h264) · audio {aok} decrypted / {afail} failed"
    );
    // Report per stream; only claim a stream "validated" if it actually decrypted frames with 0 failures
    // (avoids overstating validation when e.g. no audio was playing so no audio stream arrived).
    if ok > 0 && fail == 0 {
        println!("[avdec] ✓ video decrypted host-side, 0 failures");
    }
    if aok > 0 && afail == 0 {
        println!("[avdec] ✓ audio decrypted host-side, 0 failures");
    }
    if fail > 0 || afail > 0 {
        println!("[avdec] ✗ decrypt failures — video {fail}, audio {afail}");
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
    println!("[avdec] STOP sent — box tears the session down (holding pattern)");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("hello");
    let vid = u16::from_str_radix(args.get(2).map(String::as_str).unwrap_or("1314"), 16)
        .unwrap_or(0x1314);
    let pid = u16::from_str_radix(args.get(3).map(String::as_str).unwrap_or("2d00"), 16)
        .unwrap_or(0x2d00);

    // `tcp:<host>[:port]` in the vid slot (and anything, e.g. `-`, in the pid slot, so every mode's
    // later arguments keep their places) reaches the box over its Wi-Fi transport instead of USB.
    let mut link = match args.get(2).and_then(|a| a.strip_prefix("tcp:")) {
        Some(target) => open_tcp(target),
        None => open(vid, pid),
    };
    // Nothing but HELLO goes out until the HELLO_ACK: a trial's ocbmd takes a host's first other frame
    // as the proof it answered (the one-file install's answer_watch), so a clock push after "no
    // HELLO_ACK" vouched for a box that never answered (truck bench, 2 Nov).
    if !do_hello(&mut link) {
        std::process::exit(1);
    }
    // Auto-sync the box clock on every connect (the box has no RTC battery).
    push_time(&mut link);
    match mode {
        "settime" => {} // clock already pushed above; nothing more to do
        "console" => mode_console(&mut link),
        "ncm" => mode_ncm(&mut link),
        // `install info|confirm|stock`, or with the ids first as the usage says. The verb used to be
        // read from the fourth argument only, so `install stock` silently ran `info` (29 Oct).
        "install" => {
            let what = match args.get(2).map(String::as_str) {
                Some(w @ ("info" | "confirm" | "stock")) => w,
                _ => args.get(4).map(String::as_str).unwrap_or("info"),
            };
            mode_install(&mut link, what)
        }
        "update" => mode_update(&mut link, args.get(4).map(String::as_str).unwrap_or_else(|| {
            eprintln!("usage: ocbm-host update <vid pid | tcp:<ip> -> <package.tgz>");
            std::process::exit(2)
        })),
        "wifi-link" => mode_wifi_link(
            &mut link,
            args.get(4).map(String::as_str).unwrap_or(""),
            args.get(5).map(String::as_str),
        ),
        "av" => mode_av(
            &mut link,
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
        ),
        "avdec" => mode_avdec(
            &mut link,
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
            args.get(5).map(String::as_str),
        ),
        "session" => mode_session(
            &mut link,
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
            args.get(5).map(String::as_str),
        ),
        "push" => {
            let local = args.get(4).map(String::as_str).unwrap_or("");
            let remote = args.get(5).map(String::as_str).unwrap_or("");
            let mode = args
                .get(6)
                .and_then(|s| u32::from_str_radix(s, 8).ok())
                .unwrap_or(0o755);
            if local.is_empty() || remote.is_empty() {
                eprintln!("usage: ocbm-host push [vid pid] <local> <remote> [mode-octal]");
                std::process::exit(2);
            }
            if !mode_push(&mut link, local, remote, mode) {
                std::process::exit(1);
            }
        }
        "pull" => {
            let remote = args.get(4).map(String::as_str).unwrap_or("");
            let local = args.get(5).map(String::as_str).unwrap_or("");
            let mode = args
                .get(6)
                .and_then(|s| u32::from_str_radix(s, 8).ok())
                .unwrap_or(0o644);
            if remote.is_empty() || local.is_empty() {
                eprintln!("usage: ocbm-host pull [vid pid] <remote> <local> [mode-octal]");
                std::process::exit(2);
            }
            if !mode_pull(&mut link, remote, local, mode) {
                std::process::exit(1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

    #[test]
    fn wpa_psk_matches_ieee_80211i_vector() {
        let k: String = super::wpa_psk(b"password", b"IEEE").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(k, "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e");
    }

    /// The nonce must be stable within a process (a reattach is the SAME host) and never 0
    /// ("not supplied"), or the box's replacement detection reads it wrong in either direction.
    #[test]
    fn host_instance_nonce_is_stable_and_nonzero() {
        let n = super::host_instance_nonce();
        assert_ne!(n, 0);
        assert_eq!(n, super::host_instance_nonce());
    }
    // Encrypt an audio AU exactly as receiver_core's encrypt_audio_aad (packet = [12B hdr][ct‖tag][nonce8],
    // nonce = [0,0,0,0]‖nonce8, aad = hdr[4..12]), then decrypt with the exact logic mode_avdec uses for
    // CH_MEDIA_AUDIO. Proves the host audio-decrypt replica matches the box wire.
    #[test]
    fn audio_decrypt_replica_roundtrip() {
        let key = [0x11u8; 32];
        let au = b"hello carplay audio AU payload!".to_vec();
        let header: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let nonce8 = [0x22u8; 8];
        let mut enc_nonce = [0u8; 12];
        enc_nonce[4..].copy_from_slice(&nonce8);
        let ct_tag = ChaCha20Poly1305::new(Key::from_slice(&key))
            .encrypt(
                Nonce::from_slice(&enc_nonce),
                Payload {
                    msg: &au,
                    aad: &header[4..12],
                },
            )
            .unwrap();
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&header);
        pkt.extend_from_slice(&ct_tag);
        pkt.extend_from_slice(&nonce8);
        // host replica (mode_avdec CH_MEDIA_AUDIO branch)
        assert!(pkt.len() >= 12 + 16 + 8);
        let aad = &pkt[4..12];
        let payload = &pkt[12..];
        let n = payload.len();
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&payload[n - 8..]);
        let ct_and_tag = &payload[..n - 8];
        let pt = ChaCha20Poly1305::new(Key::from_slice(&key))
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ct_and_tag,
                    aad,
                },
            )
            .unwrap();
        assert_eq!(pt, au, "host audio decrypt must recover the original AU");
    }
}
