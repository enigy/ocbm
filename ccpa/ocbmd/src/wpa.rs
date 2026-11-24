//! `ocbmd wpa [-p DIR] IFACE COMMAND [ARG...]`: one request to wpa_supplicant's control interface, its reply printed as
//! it came. The Wi-Fi link's station keeper (`/script/wifi_sta.sh`) talks to its supplicant through it, and the bench
//! by hand over the console (`ocbmd wpa sta0 status`, `scan`, `scan_results`, `list_networks`, `enable_network 0`): the
//! adapter carries no wpa_cli, about 68 KB of flash an update could not spare (24 Nov 2026).
//!
//! wpa_supplicant 2.11's UNIX control interface, as its own client library and server do it (`src/common/wpa_ctrl.c`
//! `wpa_ctrl_open2` / `wpa_ctrl_request`, `wpa_supplicant/ctrl_iface_unix.c`): a datagram socket at `DIR/IFACE`, DIR
//! being the configuration's `ctrl_interface` (the keeper writes `/var/run/wpa_supplicant`, the default here). The
//! client binds a socket of its own in /tmp, which the supplicant answers with `sendto`, connects to `DIR/IFACE` and
//! sends the request text; one datagram comes back: `OK\n`, `FAIL\n`, `UNKNOWN COMMAND\n` or the command's text, at
//! most 4096 B. A datagram that begins with `<` or `IFNAME=` is an event for an attached monitor, not the reply, and is
//! passed by. wpa_ctrl waits 10 s for the reply, and so does this.
//!
//! The request is the command's first word in capitals and the rest as given, one space between: wpa_cli's own
//! spelling for every command the keeper and the bench use (`status`, `scan`, `scan_results`, `list_networks`,
//! `enable_network 0`, `disconnect`, `reconnect`, `reconfigure`). Where wpa_cli spells one otherwise (`status verbose`
//! is its `STATUS-VERBOSE`), the control command itself is typed (`status-verbose`).
//!
//! Exit status: 0 a reply came, whatever it says (as wpa_cli); 1 no supplicant answered on `DIR/IFACE` within 10 s, or
//! the socket failed (said on stderr); 2 usage. Nothing goes to stdout but the reply. Run before ocbmd's
//! single-instance lock, as `jffs2-extract` is: it never touches the accessory, the daemon's files or its sockets.

use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The supplicant's control directory as the station keeper writes it (`ctrl_interface=`).
const CTRL_DIR: &str = "/var/run/wpa_supplicant";
/// Where the client's own socket is bound: wpa_ctrl's `CONFIG_CTRL_IFACE_CLIENT_DIR`.
const CLIENT_DIR: &str = "/tmp";
/// wpa_ctrl_request's wait for the reply.
const REPLY_WAIT: Duration = Duration::from_secs(10);
/// How long a send may wait on a full queue: wpa_ctrl_request retries one for 5 s.
const SEND_WAIT: Duration = Duration::from_secs(5);
/// The supplicant's request limit (`CTRL_IFACE_MAX_LEN`): a longer request is dropped unanswered. Also the reply
/// buffer's size.
const MAX_LEN: usize = 8192;

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
    eprintln!("usage: ocbmd wpa [-p CTRL_DIR] IFACE COMMAND [ARG...]   e.g. ocbmd wpa sta0 status");
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

/// The client socket's file, removed when the request is over.
struct Unlink(PathBuf);

impl Drop for Unlink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One request to the supplicant on `dir/iface` and its reply, the client's socket bound in `client_dir`.
fn ask(dir: &Path, iface: &str, req: &str, client_dir: &Path, wait: Duration) -> std::io::Result<Vec<u8>> {
    let me = client_dir.join(format!("ocbmd_wpa_{}", std::process::id()));
    // One an earlier run with this pid left (killed mid-request): wpa_ctrl removes its own the same way.
    let _ = std::fs::remove_file(&me);
    let sock = UnixDatagram::bind(&me)?;
    let _gone = Unlink(me);
    // Connected: only the supplicant's own datagrams are read.
    sock.connect(dir.join(iface))?;
    sock.set_write_timeout(Some(SEND_WAIT))?;
    sock.send(req.as_bytes())?;
    let end = Instant::now() + wait;
    let mut buf = vec![0u8; MAX_LEN];
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("no reply in {} s", wait.as_secs())));
        }
        sock.set_read_timeout(Some(left))?;
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        let r = &buf[..n];
        if r.starts_with(b"<") || r.starts_with(b"IFNAME=") {
            continue; // an event for an attached monitor (wpa_ctrl_request passes these by too)
        }
        return Ok(r.to_vec());
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
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
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
    }
}
