//! The Wi-Fi link: the OCBM session over TCP (OCBW v1, crate `ocbm-link`) on the adapter's own access point, beside
//! the USB accessory. Mode `ap` (ADDENDUM 24 of the Wi-Fi link plan, MGMT_WIFI_LINK version 2): the head unit joins
//! the AP the phone's CarPlay uses, and reaches ocbmd at the AP's own address.
//!
//! OFF unless [`LINK_FILE`] holds a link key in mode `ap`. With none, ocbmd is the USB daemon it was in 2.7.1: no
//! thread, no socket and no file of this module's is opened, and the only differences a host can see are the
//! `CAP_WIFI_LINK` bit, `MGMT_WIFI_LINK` answered, and `"wifi_link":{"mode":"off",..}` in MGMT_INFO.
//!
//! ON (a host set the link up with `MGMT_WIFI_LINK` v2, over whatever session it had):
//!
//! - the AP is up from power-up and between phone sessions ([`ApKeep`]), on the band the link keeps; v2's ACK carries
//!   the AP's name and passphrase ([`ap_network`]);
//! - a band changed while a link exists is ON TRIAL ([`BAND_TRIAL`], the owner's decision of 27 Nov 2026): the link
//!   file keeps the band before (`band_prev=`) until a Wi-Fi host completes the OCBW handshake while the AP runs the
//!   new band ([`ap_runs`]), and with none in time the box goes back to it by itself (main.rs `Daemon::band_back`);
//! - [`start_listener`]'s thread listens on the AP'S OWN ADDRESS on the AP's interface only, port 7020, as the radio
//!   seam publishes them, never on 0.0.0.0 or another interface. Handshakes run on threads of their own, at most
//!   [`MAX_HANDSHAKES`] at once and one per peer address, each within `ocbm_link::HANDSHAKE_TIMEOUT`, so a stranger on
//!   the AP costs only itself;
//! - an authenticated connection becomes a [`Link`]: a reader and a writer thread and two pipes, shaped like the USB
//!   relay's, so the poll loop reads and writes it as it does the accessory. The reader ends the link after
//!   `ocbm_link::DEAD_AFTER` (3 s) with nothing received; the writer sends a keepalive after
//!   `ocbm_link::KEEPALIVE_AFTER` (1 s) with nothing to send, and ends the link when one write has been stuck 3 s;
//! - ONE HOST AT A TIME (main.rs `Daemon::admit`): the first host whose HELLO the box takes holds it, over USB or
//!   Wi-Fi, until it leaves (CT_STOP, its link ends, or [`HOLD_SILENCE`] without a frame). A Wi-Fi host refused
//!   meanwhile has its connection closed at once, before any OCBM byte. USB ALWAYS WINS: a USB host's frame takes the
//!   box from a Wi-Fi holder at once (its link closes), and no USB frame is ever dropped.
//!
//! The mDNS advert on the AP is `mdns.rs`.
//!
//! THE HOTSPOT MODE IS RETIRED (27 Nov 2026, the owner's decision): MGMT_WIFI_LINK version 1, the adapter joining the
//! phone's hotspot as a client, cannot work on this radio (the IW416 joins no 2.4 GHz network as a client, ADDENDA 14
//! to 23 of the plan). A v1 is refused with [`STATUS_RETIRED`] and stores nothing, and a link file of that mode is
//! removed at ocbmd's start ([`start_link_conf`]). No station, no supplicant, no keeper.

use ocbm_link::{LinkKey, Session};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The link key, mode `ap` and the AP's band (`key=` / `mode=ap` / `band=` lines, and `band_prev=` while a band change
/// is on trial), 0600. On flash: kept across reboots and updates, removed by MGMT_WIFI_LINK's clear, OCBM_WIFI_RESET,
/// the go-back to stock and the stock restore (stock runs a passwordless telnetd on its AP).
#[cfg(not(test))]
pub const LINK_FILE: &str = "/etc/ocbm/wifi_link";
#[cfg(test)]
pub const LINK_FILE: &str = "/tmp/ocbmd_selftest_wifi_link";
/// What the retired hotspot mode kept beside [`LINK_FILE`] (its station keeper's: the hotspot's last channel, its
/// address, the station's own address): removed with an old link file at ocbmd's start, and at every store.
const RETIRED_BESIDE: [&str; 3] = ["_channel", "_bssid", "_sta"];
/// One start in plain USB mode, then gone: set when a USB host took the box on a driver that serializes reads and
/// writes ([`crate::AccIo::Direct`]), where the Wi-Fi side's USB reader would keep the loop's writes out.
#[cfg(not(test))]
pub const USB_ONCE_FLAG: &str = "/tmp/ocbmd_usb_once";
#[cfg(test)]
pub const USB_ONCE_FLAG: &str = "/tmp/ocbmd_selftest_usb_once";
/// The bench's interface lever: the listener binds this interface's address instead of the AP's (a wired bench over
/// NCM, say). Never the AP's subnet. Read at each look; "off" or empty reads as unset.
#[cfg(not(test))]
const BENCH_IF_FILE: &str = "/tmp/bench_wifi_link_if";
#[cfg(test)]
const BENCH_IF_FILE: &str = "/tmp/ocbmd_selftest_no_such_bench_wifi_link_if";
/// The AP's configuration, for its channel: [`AP_SESSION_CONF`] while the AP runs from it, else this flash file.
pub const HOSTAPD_CONF: &str = "/etc/hostapd.conf";
/// The configuration the AP runs from where it took mode `ap`'s band (radio_ap_up.sh `ap_band_conf`): hostapd.conf
/// with the band's channel, `hw_mode` and HT40 side, in tmpfs, there only while that AP is.
#[cfg(not(test))]
pub const AP_SESSION_CONF: &str = "/tmp/hostapd_session.conf";
#[cfg(test)]
pub const AP_SESSION_CONF: &str = "/tmp/ocbmd_selftest_no_such_hostapd_session.conf";
/// Mode `ap`: the radio seam's word on the AP once it serves (`result=ok`, `ap_iface=`, `ap_ip=`; gone with the AP).
#[cfg(not(test))]
const WLAN_STATE: &str = "/tmp/.radio_wlan_state";
#[cfg(test)]
const WLAN_STATE: &str = "/tmp/ocbmd_selftest_no_such_radio_wlan_state";
/// Mode `ap`: the band the running AP was started for, and its channel (radio_ap_up.sh: "<band> <channel>").
#[cfg(not(test))]
pub(crate) const AP_LINK_MARK: &str = "/tmp/.radio_ap_link";
#[cfg(test)]
pub(crate) const AP_LINK_MARK: &str = "/tmp/ocbmd_selftest_radio_ap_link";
/// An AP bring-up or teardown under way: the seam's wlan lock, the AP layer's own.
#[cfg(not(test))]
const AP_BUSY: [&str; 2] = ["/tmp/.radio_lock_wlan", "/tmp/.radio_ap_lock"];
#[cfg(test)]
const AP_BUSY: [&str; 2] = ["/tmp/ocbmd_selftest_no_such_lock_wlan", "/tmp/ocbmd_selftest_no_such_ap_lock"];
/// The root the AP's files are read under for v2's ACK ([`ap_network`]): "" on the adapter.
#[cfg(not(test))]
pub const AP_ROOT: &str = "";
#[cfg(test)]
pub const AP_ROOT: &str = "/tmp/ocbmd_selftest_aproot";

/// Handshakes under way at once, all peers together.
pub const MAX_HANDSHAKES: usize = 4;
/// Authenticated links whose host has not yet sent its HELLO, at once.
pub const MAX_CANDIDATES: usize = 4;
/// A host that holds the box and has sent nothing for this long no longer holds it against another host. The heartbeat
/// grace's figure: a subscribed host beats every second.
pub const HOLD_SILENCE: Duration = Duration::from_secs(10);
/// An authenticated link that sends no HELLO within this long is closed.
pub const HELLO_WITHIN: Duration = Duration::from_secs(5);
/// A band changed while a link exists is on trial this long from the change's apply (half a second after its ACK), or
/// from ocbmd's start when the trial was kept from before it (a power cut, or ocbmd started again: no clock survives a
/// power cut, so every start gives the new band the whole time): no Wi-Fi host completing the OCBW handshake on the new
/// band by then takes the AP back to the band before. Room for the AP's restart (seconds), the head unit to see its
/// network go and join it again on the new band (Android's scans while it has no network: tens of seconds, INFERRED)
/// and Carlink's next link (a search every few seconds), with margin; short enough that a head unit that sees no 5 GHz
/// network has the adapter back on 2.4 GHz within a drive's first minutes.
pub const BAND_TRIAL: Duration = Duration::from_secs(180);

/// The subnets the bench's interface lever must never name: the box's own AP's and NCM's (box-common `net`).
fn reserved_subnet(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 192 && o[1] == 168 && (o[2] == 43 || o[2] == 50)
}

// ---- the link file ------------------------------------------------------------------------------------------------

/// The band of the adapter's own AP in mode `ap`: v2's last byte, the link file's `band=`, MGMT_INFO's `ap_band`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// 2.4 GHz: channel 6, 20 MHz (many head units, the owner's included, see only 2.4 GHz).
    G24,
    /// 5 GHz: hostapd.conf's own channel when it is a non-DFS 5 GHz one (36 on the factory file), else 36.
    G5,
}

impl Band {
    /// v2's byte: 0x02 or 0x05.
    pub fn from_wire(b: u8) -> Option<Band> {
        match b {
            0x02 => Some(Band::G24),
            0x05 => Some(Band::G5),
            _ => None,
        }
    }

    /// "2.4" or "5", as the link file, the AP layer's mark and MGMT_INFO say it.
    pub fn name(self) -> &'static str {
        match self {
            Band::G24 => "2.4",
            Band::G5 => "5",
        }
    }

    /// The band a link file's or the AP layer's word names: "2.4" or "5", nothing else.
    fn named(v: &str) -> Option<Band> {
        match v.trim() {
            "2.4" => Some(Band::G24),
            "5" => Some(Band::G5),
            _ => None,
        }
    }

    /// `channel` is one of this band's: 1-14 at 2.4 GHz, 32 and up at 5 GHz.
    fn holds(self, channel: u32) -> bool {
        match self {
            Band::G24 => (1..=14).contains(&channel),
            Band::G5 => (32..=196).contains(&channel),
        }
    }
}

/// What [`LINK_FILE`] holds: a link in mode `ap`, the only mode since 27 Nov 2026.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkConf {
    pub key: LinkKey,
    /// The band of the adapter's AP, which the head unit joins (v2's last byte).
    pub band: Band,
    /// A band change on trial ([`BAND_TRIAL`]): the band before it, the last one a Wi-Fi host linked on or set up
    /// first, to go back to. None: nothing to undo (a first set-up, a band kept, or one gone back).
    pub band_prev: Option<Band>,
}

/// [`LINK_FILE`]'s text: `key=` (64 hex), `mode=ap` and `band=` (radio_hal.sh's `link_ap` reads the same lines), and
/// `band_prev=` while a band change is on trial (the shell reads none of it).
pub fn link_file_text(c: &LinkConf) -> String {
    let prev = c.band_prev.map(|b| format!("band_prev={}\n", b.name())).unwrap_or_default();
    format!(
        "# OCBM Wi-Fi link (MGMT_WIFI_LINK). Secret: never into a log, a backup or a stick.\nkey={}\nmode=ap\nband={}\n{prev}",
        c.key.to_hex(),
        c.band.name()
    )
}

/// The link a v2 sets up (`key`, `band`) over the one stored (`now`): a band changed while a link exists goes on trial
/// with the band it had (the one before the trial already running, if one is: the last band a host linked on); the
/// band before asked again ends the trial; the same band keeps whatever trial runs. A first set-up has nothing to undo.
pub fn next_conf(now: Option<&LinkConf>, key: LinkKey, band: Band) -> LinkConf {
    let band_prev = match now {
        None => None,
        Some(c) if c.band == band => c.band_prev,
        Some(c) if c.band_prev == Some(band) => None,
        Some(c) => Some(c.band_prev.unwrap_or(c.band)),
    };
    LinkConf { key, band, band_prev }
}

/// The AP layer's mark (`<band asked> <channel run>`, radio_ap_up.sh) says the AP runs `band`: started for it, and on
/// one of its channels (an AP whose channel hostapd refused runs its own, perhaps of the other band: not this one).
pub fn on_band(mark: &str, band: Band) -> bool {
    let mut w = mark.split_whitespace();
    w.next().and_then(Band::named) == Some(band) && w.next().and_then(|c| c.parse().ok()).is_some_and(|c| band.holds(c))
}

/// A Wi-Fi host that completes the OCBW handshake now does it on `band`: the AP runs it ([`on_band`]) and the listener
/// is the AP's (not the bench lever's interface).
pub fn ap_runs(band: Band) -> bool {
    bench_iface().is_none() && on_band(&std::fs::read_to_string(AP_LINK_MARK).unwrap_or_default(), band)
}

/// The file says mode `ap` (a `mode=ap` line). One that does not is of the retired hotspot mode: a hotspot's
/// (`ssid_hex=`, `psk=`), or the bench's key alone (round 1 and before).
fn mode_ap(text: &str) -> bool {
    text.lines().any(|l| l.strip_prefix("mode=").is_some_and(|v| v.trim() == "ap"))
}

/// The file's text read back: mode `ap`, a `key=` line of 64 hex digits, its `band=` (2.4 unless 5), and `band_prev=`
/// (2.4 or 5, and not the band itself; else none). Anything else is no link.
pub fn parse_link_file(text: &str) -> Option<LinkConf> {
    if !mode_ap(text) {
        return None;
    }
    let mut key = None;
    let (mut band, mut prev) = (Band::G24, None);
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("key=") {
            key = LinkKey::from_hex(v);
        } else if let Some(v) = l.strip_prefix("band=") {
            band = Band::named(v).unwrap_or(Band::G24);
        } else if let Some(v) = l.strip_prefix("band_prev=") {
            prev = Band::named(v);
        }
    }
    Some(LinkConf { key: key?, band, band_prev: prev.filter(|p| *p != band) })
}

/// The link as stored, or None (no file, one of the retired hotspot mode, or no valid key: then the link is off, and
/// the line says why).
pub fn read_link_conf(path: &str) -> Option<LinkConf> {
    let text = std::fs::read_to_string(path).ok()?;
    let c = parse_link_file(&text);
    if c.is_none() && mode_ap(&text) {
        eprintln!("[ocbmd] wifi: {path} has no valid key= line; the Wi-Fi link stays off");
    }
    c
}

/// The link at ocbmd's start: [`read_link_conf`]'s, after a link file of the retired hotspot mode (no `mode=ap`) is
/// removed, with what that mode kept beside it ([`RETIRED_BESIDE`]): such an adapter comes up as one with no link, its
/// next set-up in mode `ap` a first one. The second field: one was (the caller removes the kept session config, which
/// goes with a link). No file: one failed open, nothing else.
pub fn start_link_conf(path: &str) -> (Option<LinkConf>, bool) {
    let Ok(text) = std::fs::read_to_string(path) else { return (None, false) };
    if mode_ap(&text) {
        let c = parse_link_file(&text);
        if c.is_none() {
            eprintln!("[ocbmd] wifi: {path} has no valid key= line; the Wi-Fi link stays off");
        }
        return (c, false);
    }
    let gone = std::fs::remove_file(path).is_ok();
    remove_beside(path);
    eprintln!(
        "[ocbmd] wifi: {path} held a link of the retired hotspot mode (MGMT_WIFI_LINK v1): {}; no Wi-Fi link until it \
         is set up again (mode ap)",
        if gone { "removed, with the hotspot's kept channel and addresses" } else { "it could not be removed" }
    );
    (None, true)
}

/// The retired hotspot mode's files beside the link file, removed (none there: nothing).
fn remove_beside(path: &str) {
    for s in RETIRED_BESIDE {
        let _ = std::fs::remove_file(format!("{path}{s}"));
    }
}

// ---- MGMT_WIFI_LINK -----------------------------------------------------------------------------------------------

/// What `MGMT_WIFI_LINK` asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Provision {
    /// Remove the link: the box is a USB-only box again.
    Clear,
    /// Version 2: the link key, mode `ap` on this band (`band_prev` none: [`next_conf`] decides it against the link
    /// stored).
    Set(LinkConf),
}

/// MGMT_ACK's status to a version 1 body (the hotspot form, its key-only form included): the hotspot mode is retired,
/// nothing was stored or changed, and the same body will never be taken. 1 stays "refused" (malformed, not storable,
/// or the AP not nameable yet: try again in a moment).
pub const STATUS_RETIRED: u8 = 2;

/// Why a `MGMT_WIFI_LINK` body is not taken: its status, and the log's words.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Version 1, the retired hotspot mode: [`STATUS_RETIRED`].
    Retired,
    /// Status 1.
    Bad(&'static str),
}

impl Refusal {
    /// The MGMT_ACK status byte.
    pub fn status(&self) -> u8 {
        match self {
            Refusal::Retired => STATUS_RETIRED,
            Refusal::Bad(_) => 1,
        }
    }
}

/// `MGMT_WIFI_LINK`'s body (after the verb): empty = [`Provision::Clear`]; version 2, mode `ap`, `[ver 2][key 32][band
/// u8]` (35 B with the verb; band 0x02 or 0x05). Version 1 (the hotspot mode, whatever follows the version byte) is
/// [`Refusal::Retired`]; any other version, length or band is [`Refusal::Bad`].
pub fn parse_provision(body: &[u8]) -> Result<Provision, Refusal> {
    match body.first() {
        None => Ok(Provision::Clear),
        Some(1) => Err(Refusal::Retired),
        Some(2) => {
            let key: [u8; 32] = body.get(1..33).filter(|_| body.len() == 34).and_then(|k| k.try_into().ok()).ok_or(Refusal::Bad("a v2 that is not 35 B"))?;
            let band = Band::from_wire(body[33]).ok_or(Refusal::Bad("a v2 with an unknown band"))?;
            Ok(Provision::Set(LinkConf { key: LinkKey(key), band, band_prev: None }))
        }
        Some(_) => Err(Refusal::Bad("an unknown version")),
    }
}

// ---- mode `ap`: the AP the head unit joins ---------------------------------------------------------------------------

/// A WPA passphrase radio_ap_up.sh's `ap_passphrase` would keep: 8..=63 printable ASCII, and none of the defaults it
/// counts as missing (stock's 12345678, an older OCBM's "carplay" + the AP interface's 4 hex: any 4 hex here).
fn ap_psk_good(p: &str) -> bool {
    let known = p == "12345678" || p.strip_prefix("carplay").is_some_and(|s| s.is_empty() || (s.len() == 4 && s.bytes().all(|b| b.is_ascii_hexdigit())));
    (8..=63).contains(&p.len()) && p.bytes().all(|b| (b' '..=b'~').contains(&b)) && !known
}

/// The AP the head unit joins in mode `ap`, for v2's ACK: the adapter's name, which the AP takes as its ssid
/// (`/etc/carplay_ident`, radio_hal.sh's box_name, decided once), and the AP's WPA2 passphrase as radio_ap_up.sh keeps
/// it: hostapd.conf's when good; else, as its ap_passphrase would choose at the AP's next configuration, the one OCBM
/// kept (`/etc/ocbm/wifi_psk`), or a new random one, kept there for it first. Err (v2 refused,
/// nothing stored): no decided name yet, or the AP layer writing its configuration this very moment.
pub fn ap_network(root: &str) -> Result<(String, String), &'static str> {
    let read = |f: &str| std::fs::read_to_string(format!("{root}/{f}")).unwrap_or_default();
    let name = read("etc/carplay_ident").lines().next().unwrap_or("").trim().to_string();
    if !name.starts_with("CarLink-") || name.len() > 32 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err("no name decided yet");
    }
    if let Some(p) = read("etc/hostapd.conf").lines().find_map(|l| l.strip_prefix("wpa_passphrase=")).filter(|p| ap_psk_good(p)) {
        return Ok((name, p.to_string()));
    }
    if std::path::Path::new(&format!("{root}/tmp/.radio_ap_lock")).exists() {
        return Err("the AP layer is writing");
    }
    // As ap_passphrase reads it, `tr -cd ' -~' | head -c 63`. Not /etc/wifi_password, its next: a new one kept in
    // wifi_psk comes first for it.
    let kept: String = read("etc/ocbm/wifi_psk").chars().filter(|c| (' '..='~').contains(c)).take(63).collect();
    if ap_psk_good(&kept) {
        return Ok((name, kept));
    }
    let p = ocbm_link::hex(&ocbm_link::random32().map_err(|_| "no randomness")?)[..20].to_string();
    write_0600(&format!("{root}/etc/ocbm/wifi_psk"), &format!("{p}\n")).map_err(|_| "not kept")?;
    Ok((name, p))
}

/// `text` into `path`, 0600, through a synced temporary and a rename; the temporary goes on failure.
fn write_0600(path: &str, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = format!("{path}.tmp");
    let written = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Carry a set-up out on flash: [`LINK_FILE`] written 0600 through a synced temporary and a rename (`Some`), or removed
/// (`None`). Either way, once done, what the retired hotspot mode kept beside it goes too ([`RETIRED_BESIDE`]).
pub fn store(c: Option<&LinkConf>, path: &str) -> std::io::Result<()> {
    let done = match c {
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Some(c) => {
            if let Some(dir) = std::path::Path::new(path).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            write_0600(path, &link_file_text(c))
        }
    };
    if done.is_ok() {
        remove_beside(path);
    }
    done
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

/// The AP's own address on the AP's interface, port 7020, from the radio seam's word on the AP (`state`: `result=ok`,
/// `ap_iface=`, `ap_ip=`), and only while that interface holds that address (`addr_of`); never loopback, 0.0.0.0 or
/// NCM's subnet (ADDENDUM 24 item 2).
pub fn ap_bind(state: &str, addr_of: impl Fn(&str) -> Option<Ipv4Addr>) -> Option<(SocketAddrV4, String)> {
    let get = |k: &str| state.lines().find_map(|l| l.strip_prefix(k)).map(str::trim);
    let (iface, ip): (&str, Ipv4Addr) = (get("ap_iface=")?, get("ap_ip=")?.parse().ok()?);
    let o = ip.octets();
    if get("result=")? != "ok" || iface.is_empty() || ip.is_loopback() || ip.is_unspecified() || o[..3] == [192, 168, 50] {
        return None;
    }
    (addr_of(iface)? == ip).then(|| (SocketAddrV4::new(ip, ocbm_link::PORT), iface.to_string()))
}

/// Where the listener should be now, port 7020, and on which interface: the AP's ([`ap_bind`]), or the interface the
/// bench's lever names (`bench`), whose address must not lie on the AP's or NCM's subnet. None while there is none.
pub fn listen_addr(bench: Option<&str>) -> Option<(SocketAddrV4, String)> {
    let Some(iface) = bench else {
        return ap_bind(&std::fs::read_to_string(WLAN_STATE).unwrap_or_default(), |i| iface_v4(i).map(|(ip, _)| ip));
    };
    let (ip, _) = iface_v4(iface)?;
    if reserved_subnet(ip) || ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    Some((SocketAddrV4::new(ip, ocbm_link::PORT), iface.to_string()))
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
    /// The AP's address as it comes and goes, or the bench lever's interface's ([`listen_addr`]).
    Ap,
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

/// Start the listener's thread: it follows the AP's address (rebinding as it changes), accepts, and hands each
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
        Bind::Ap => {
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
            let bench = bench_iface();
            let want = listen_addr(bench.as_deref());
            if cur.as_ref().map(|(w, _)| w) != want.as_ref() {
                if let Some(((a, _), _)) = cur.take() {
                    eprintln!("[ocbmd] wifi: listener on {a} closed (its address went or changed)");
                }
                *shared.bound.lock().unwrap_or_else(|e| e.into_inner()) = None;
                if let Some((a, i)) = want {
                    match TcpListener::bind(a).and_then(|l| only_on(&l, &i).map(|()| l)) {
                        Ok(l) => {
                            let w = if bench.is_some() { "the bench lever's interface's" } else { "the AP's" };
                            eprintln!("[ocbmd] wifi: listening on {a} (OCBW v1, {w} address, from {i} only)");
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
                format!("a write stuck for {} s (the host is gone from the network?)", ocbm_link::DEAD_AFTER.as_secs())
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

/// The `"wifi_link"` object of MGMT_INFO in mode `ap` (ADDENDUM 24 item 9): `state` off (the AP not serving: no
/// address to listen on), up (the listener on the AP), linked (a Wi-Fi host holds the box); `ap_band` the link's;
/// `ap_channel` the channel the AP runs on, the band's once it has moved; `band_pending` (27 Nov 2026) true while a band
/// change is on trial: with no Wi-Fi host on `ap_band` within [`BAND_TRIAL`] the adapter goes back to the other band by
/// itself. No name, no key, no address.
pub fn ap_info_json(band: Band, linked: bool, listening: bool, ap_ch: u32, pending: bool) -> String {
    let state = if linked {
        "linked"
    } else if listening {
        "up"
    } else {
        "off"
    };
    format!(
        "{{\"mode\":\"ap\",\"state\":\"{state}\",\"ap_band\":\"{}\",\"ap_channel\":{ap_ch},\"band_pending\":{pending}}}",
        band.name()
    )
}

/// The `"wifi_link"` object of MGMT_INFO with no link (`mode` off), as round 1 sent it: `channel` was the retired
/// hotspot mode's, always 0 here. No name, no key, no address.
pub fn off_info_json(ap_ch: u32) -> String {
    format!("{{\"mode\":\"off\",\"state\":\"off\",\"channel\":0,\"ap_channel\":{ap_ch}}}")
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
    /// Mode `ap`: the AP kept up for the head unit.
    pub apk: ApKeep,
    /// A band change on trial (the link's `band_prev`): when this start's [`BAND_TRIAL`] began. None: none runs here.
    pub trial: Option<Instant>,
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
            apk: ApKeep::default(),
            trial: None,
        }
    }

    /// At ocbmd's start: a band change kept on trial from before this start (the link file's `band_prev`) gets the
    /// whole [`BAND_TRIAL`] again from `now` (no clock survives a power cut), said once.
    pub fn resume_trial(&mut self, now: Instant) {
        if let Some((band, prev)) = self.conf.as_ref().and_then(|c| c.band_prev.map(|p| (c.band, p))) {
            self.trial = Some(now);
            eprintln!(
                "[ocbmd] wifi: mode ap: the band's change to {} GHz is still on trial: back to {} GHz in {} s unless a \
                 Wi-Fi host links on {} GHz",
                band.name(),
                prev.name(),
                BAND_TRIAL.as_secs(),
                band.name()
            );
        }
    }

    /// Start taking Wi-Fi hosts with `conf`'s key: the listener and the advert, on the AP.
    pub fn start(&mut self, conf: &LinkConf) -> std::io::Result<()> {
        let shared = Shared::new(Some(conf.key.clone()));
        let l = start_listener(shared.clone(), Bind::Ap)?;
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
        let ap = ap_channel(&ap_conf_text(AP_SESSION_CONF, HOSTAPD_CONF));
        let listening = self.shared.as_ref().is_some_and(|s| s.bound().is_some());
        let linked = matches!(self.via, Some(Via::Wifi(_)));
        match self.conf.as_ref() {
            Some(c) => ap_info_json(c.band, linked, listening, ap, c.band_prev.is_some()),
            None => off_info_json(ap),
        }
    }
}

// ---- mode `ap`: the AP kept up ---------------------------------------------------------------------------------------

/// Mode `ap`: the AP is the head unit's network, up from power-up and between phone sessions (ADDENDUM 24 item 5).
/// The supervisor brings it up for a session as before, and in this mode the seam's teardown leaves it up
/// (radio_hal.sh); this keeps it there, a look every [`AP_LOOK`] while a link is set up (never otherwise):
/// raised when no hostapd runs, restarted on the band at once when the mode or the band changed ([`ApKeep::restart`]:
/// the phone's CarPlay drops and comes back), and restarted when it runs a configuration started for another band (or
/// none) while no phone's CarPlay session uses it, a few times a start at most; the band's going back after a trial
/// ([`ApKeep::back`]) takes the same rule, outside that count. Never over an AP bring-up or teardown under way, or a
/// set-up network.
#[derive(Default)]
pub struct ApKeep {
    next: Option<Instant>,
    /// The mode or the band changed: the AP restarts on the band at the next look.
    pub restart: bool,
    /// The band went back after its trial (main.rs `Daemon::band_back`): the AP restarts on it once no phone's CarPlay
    /// session uses it, whatever the fixes made this start; cleared once asked, or once the AP runs the band.
    pub back: bool,
    /// The last raise or restart asked of the seam.
    last: Option<Instant>,
    /// Restarts for a configuration of another band, this start ([`AP_FIXES`] at most).
    fixes: u32,
    /// Raises since an AP was last seen running: each waits twice the last (from [`AP_GAP`], 4 min at most), so a
    /// unit whose AP never comes up is not asked every 15 s for good.
    raises: u32,
}

/// Mode `ap`'s look, and the least gap between two of its raises or restarts (a seam verb takes a few seconds; one that
/// fails is not asked again at once), and the restarts for another band's configuration a start may make.
pub const AP_LOOK: Duration = Duration::from_secs(2);
const AP_GAP: Duration = Duration::from_secs(15);
const AP_FIXES: u32 = 3;

/// What mode `ap`'s look asks of the seam.
#[derive(Debug, PartialEq, Eq)]
pub enum ApAct {
    Wait,
    /// `radio_hal.sh wifi_ap_on`: no AP runs.
    Raise,
    /// `radio_hal.sh wifi_ap_restart`: down and up again on the band.
    Restart,
}

/// The look's decision: an AP bring-up or teardown under way or a set-up network (`busy`) waits; no hostapd: raise;
/// a change asked for a restart: restart; the AP started for another band (`started_for`, the AP layer's mark; None:
/// not started in mode `ap`) and no phone's CarPlay session on it: restart.
pub fn ap_decide(busy: bool, running: bool, asked: bool, started_for: Option<&str>, band: Band, carplay: bool) -> ApAct {
    if busy {
        ApAct::Wait
    } else if !running {
        ApAct::Raise
    } else if asked || (started_for != Some(band.name()) && !carplay) {
        ApAct::Restart
    } else {
        ApAct::Wait
    }
}

impl ApKeep {
    /// One look, for the link's `band` at `now`; the seam's verb it asks for, if any (the caller runs it).
    pub fn tick(&mut self, now: Instant, band: Band, set_up_ap: bool) -> Option<&'static str> {
        if self.next.is_some_and(|t| now < t) {
            return None;
        }
        self.next = Some(now + AP_LOOK);
        let busy = set_up_ap || AP_BUSY.iter().any(|p| std::path::Path::new(p).exists());
        let mark = std::fs::read_to_string(AP_LINK_MARK).unwrap_or_default();
        let carplay = box_common::flags::owner() == box_common::flags::ProjectionOwner::WirelessCp;
        let running = !busy && crate::running_procs(&["hostapd"])[0]; // the set-up network's counts too
        self.step(now, band, busy, running, &mark, carplay)
    }

    /// The look's decision on what it read (`mark`: the AP layer's, `carplay`: a phone's CarPlay session on the AP),
    /// and its pace: the verb to ask of the seam, if any, said in the box log.
    fn step(&mut self, now: Instant, band: Band, busy: bool, running: bool, mark: &str, carplay: bool) -> Option<&'static str> {
        let started_for = mark.split_whitespace().next();
        let act = ap_decide(busy, running, self.restart, started_for, band, carplay);
        if running {
            self.raises = 0;
            if started_for == Some(band.name()) {
                self.back = false; // the AP runs the band gone back to: nothing left to do for it
            }
        }
        let gap = if act == ApAct::Raise { AP_GAP * (1 << self.raises.saturating_sub(1).min(4)) } else { AP_GAP };
        let due = self.last.is_none_or(|t| now.duration_since(t) >= gap);
        let verb = match act {
            ApAct::Raise if due => {
                self.raises += 1;
                "wifi_ap_on"
            }
            ApAct::Restart if self.restart || (due && (self.back || self.fixes < AP_FIXES)) => {
                self.fixes += u32::from(!self.restart && !self.back);
                "wifi_ap_restart"
            }
            _ => return None,
        };
        let why = match (act, self.restart, self.back) {
            (ApAct::Raise, _, _) => "no AP up",
            (_, true, _) => "the mode or the band changed",
            (_, _, true) => "the band went back after its trial, no CarPlay on the AP",
            _ => "an AP of another band, no CarPlay on it",
        };
        eprintln!("[ocbmd] wifi: mode ap: {why}: {verb} on {} GHz", band.name());
        (self.restart, self.back) = (false, false);
        self.last = Some(now);
        Some(verb)
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

    /// A version 1 body (the retired hotspot form): `[1][key 32][ssid_len][ssid][psk_len][psk]`.
    fn v1(key: [u8; 32], ssid: &str, psk: &str) -> Vec<u8> {
        let mut b = vec![1];
        b.extend_from_slice(&key);
        b.push(ssid.len() as u8);
        b.extend_from_slice(ssid.as_bytes());
        b.push(psk.len() as u8);
        b.extend_from_slice(psk.as_bytes());
        b
    }

    /// The hotspot mode is retired (27 Nov 2026): every version 1 body, whatever it holds, well formed or not, is
    /// refused with status 2; the clear is taken as before.
    #[test]
    fn a_v1_body_is_the_retired_hotspot_mode_whatever_it_holds() {
        assert_eq!(parse_provision(&[]), Ok(Provision::Clear));
        let hash = "ab".repeat(32);
        let mut trailing = v1([3; 32], "net", &hash);
        trailing.push(0);
        for b in [
            v1([3; 32], "Example Hotspot", &hash),
            v1([3; 32], "net", "a good passphrase"),
            v1([3; 32], "net", ""),
            v1([3; 32], "", ""),
            v1([3; 32], "net", "short"),
            trailing,
            v1([3; 32], "net", &hash)[..40].to_vec(),
            vec![1],
        ] {
            assert_eq!(parse_provision(&b), Err(Refusal::Retired), "{} B", b.len());
        }
        assert_eq!(Refusal::Retired.status(), 2);
        assert_eq!((STATUS_RETIRED, Refusal::Bad("x").status()), (2, 1));
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    /// ADDENDUM 24, agent B's wire proposal as adopted (notes/carlink.md, notes/ocbm.md): v2 is mode `ap` on a band,
    /// 35 B with the verb exactly; anything else of version 2, and any version but 1 and 2, is refused with status 1.
    #[test]
    fn v2_is_mode_ap_on_its_band_and_v1_is_retired() {
        let v2_24 = unhex("0e02111111111111111111111111111111111111111111111111111111111111111102");
        let want = |band| Ok(Provision::Set(LinkConf { key: LinkKey([0x11; 32]), band, band_prev: None }));
        assert_eq!((v2_24.len(), parse_provision(&v2_24[1..])), (35, want(Band::G24)), "B's vector, 2.4 GHz");
        let mut v2_5 = v2_24.clone();
        v2_5[34] = 0x05;
        assert_eq!(parse_provision(&v2_5[1..]), want(Band::G5), "B's vector, 5 GHz");
        for bad in [0x00, 0x03, 0x24, 0xff] {
            v2_5[34] = bad;
            assert_eq!(parse_provision(&v2_5[1..]), Err(Refusal::Bad("a v2 with an unknown band")), "band {bad:#04x}");
        }
        let short = Err(Refusal::Bad("a v2 that is not 35 B"));
        assert_eq!(parse_provision(&v2_24[1..34]), short, "34 B");
        assert_eq!(parse_provision(&[&v2_24[1..], &[0][..]].concat()), short, "36 B");
        assert_eq!(parse_provision(&[2]), short);
        assert_eq!(parse_provision(&v1([0x11; 32], "", "")), Err(Refusal::Retired), "v1's key only: retired too");
        for other in [0, 3, 0xff] {
            assert_eq!(parse_provision(&[other]), Err(Refusal::Bad("an unknown version")), "version {other}");
        }
    }

    #[test]
    fn the_link_file_holds_mode_ap_and_its_band() {
        let c = LinkConf { key: LinkKey([7; 32]), band: Band::G5, band_prev: None };
        let t = link_file_text(&c);
        assert!(t.ends_with(&format!("\nkey={}\nmode=ap\nband=5\n", "07".repeat(32))), "{t}");
        assert_eq!(parse_link_file(&t), Some(c.clone()));
        let k = format!("key={}\n", "07".repeat(32));
        assert_eq!(parse_link_file(&format!("{k}mode=ap\n")).map(|c| c.band), Some(Band::G24), "no band: 2.4 GHz");
        assert_eq!(parse_link_file(&format!("{k}mode=ap\nband=24\n")).map(|c| c.band), Some(Band::G24), "an odd band: 2.4");
        let both = parse_link_file(&format!("{k}mode=ap\nband=5\nssid_hex=6e6574\npsk=x\n"));
        assert_eq!(both.map(|c| c.band), Some(Band::G5), "mode ap whatever else the file holds");
        assert_eq!(parse_link_file(&format!("{k}ssid_hex=6e6574\npsk=\n")), None, "the retired hotspot mode's: no link");
        assert_eq!(parse_link_file(&k), None, "the bench's key alone (v1): no link");
        assert_eq!(parse_link_file("key=short\nmode=ap\n"), None, "no valid key");
    }

    /// The owner's rule of 27 Nov 2026: a band changed while a link exists goes on trial with the band it had; a first
    /// set-up has nothing to undo; the same band again keeps the trial as it is (a new key too); the band before asked
    /// again ends it; a band that moves again keeps the band before the trial (the last one a host linked on).
    #[test]
    fn a_band_change_goes_on_trial_and_the_band_before_ends_it() {
        let (k1, k2) = (LinkKey([1; 32]), LinkKey([2; 32]));
        let c = |band, band_prev| LinkConf { key: LinkKey([1; 32]), band, band_prev };
        assert_eq!(next_conf(None, k1.clone(), Band::G5), c(Band::G5, None), "a first set-up: nothing to undo");
        let set = c(Band::G24, None);
        assert_eq!(next_conf(Some(&set), k1.clone(), Band::G24), set, "the same band: nothing moves");
        let trial = next_conf(Some(&set), k1.clone(), Band::G5);
        assert_eq!(trial, c(Band::G5, Some(Band::G24)), "2.4 -> 5: on trial, 2.4 kept to go back to");
        assert_eq!(next_conf(Some(&trial), k1.clone(), Band::G5), trial, "5 again: the trial as it is");
        assert_eq!(next_conf(Some(&trial), k2.clone(), Band::G5).band_prev, Some(Band::G24), "a new key, the same band");
        assert_eq!(next_conf(Some(&trial), k1.clone(), Band::G24), set, "back to 2.4 by the host: no trial");
        let kept = c(Band::G5, None);
        assert_eq!(next_conf(Some(&kept), k1, Band::G24), c(Band::G24, Some(Band::G5)), "5 kept, then 2.4: on trial back to 5");
    }

    #[test]
    fn the_link_file_keeps_the_band_on_trial() {
        let c = LinkConf { key: LinkKey([7; 32]), band: Band::G5, band_prev: Some(Band::G24) };
        let t = link_file_text(&c);
        assert!(t.ends_with("\nmode=ap\nband=5\nband_prev=2.4\n"), "{t}");
        assert_eq!(parse_link_file(&t), Some(c.clone()), "kept across a restart and a power cut: the file");
        let none = LinkConf { band_prev: None, ..c.clone() };
        assert!(!link_file_text(&none).contains("band_prev"), "no trial: no line");
        let k = format!("key={}\nmode=ap\n", "07".repeat(32));
        for (lines, want) in [("band=5\nband_prev=5\n", None), ("band=5\nband_prev=24\n", None), ("band=2.4\nband_prev=5\n", Some(Band::G5))] {
            assert_eq!(parse_link_file(&format!("{k}{lines}")).and_then(|c| c.band_prev), want, "{lines:?}");
        }
    }

    /// The AP layer's mark `<band asked> <channel run>`: a handshake counts for a band on trial only while the AP was
    /// started for that band and runs one of its channels.
    #[test]
    fn the_ap_layers_mark_says_whether_the_ap_runs_the_band() {
        assert!(on_band("5 36", Band::G5) && on_band("5 149\n", Band::G5) && on_band("2.4 6", Band::G24));
        assert!(!on_band("2.4 6", Band::G5), "the band before, the AP not moved yet");
        assert!(!on_band("2.4 36", Band::G24), "6 refused: its own 36, not 2.4 GHz");
        assert!(!on_band("5 6", Band::G5) && !on_band("", Band::G5) && !on_band("5", Band::G5) && !on_band("5 x", Band::G5));
    }

    /// The band gone back after its trial: the AP restarts on it once no phone's CarPlay session uses it (the rule for
    /// an AP of another band), whatever restarts of that kind this start made, and only once.
    #[test]
    fn the_band_gone_back_restarts_the_ap_once_no_carplay_uses_it() {
        let t = Instant::now();
        let mut k = ApKeep { fixes: AP_FIXES, back: true, ..ApKeep::default() };
        assert_eq!(k.step(t, Band::G24, false, true, "5 36", true), None, "a phone's CarPlay on the AP: it waits");
        assert!(k.back, "  still to do");
        assert_eq!(k.step(t + AP_LOOK, Band::G24, false, true, "5 36", false), Some("wifi_ap_restart"), "no CarPlay: restarted");
        assert!(!k.back && k.fixes == AP_FIXES, "asked once, and no fix counted");
        let mut k = ApKeep { back: true, ..ApKeep::default() };
        assert_eq!(k.step(t, Band::G24, false, true, "2.4 6", false), None, "the AP on the band already: nothing");
        assert!(!k.back);
        let mut k = ApKeep { fixes: AP_FIXES, ..ApKeep::default() };
        assert_eq!(k.step(t, Band::G24, false, true, "5 36", false), None, "another band with the fixes spent: as before");
    }

    /// A link file of the retired hotspot mode (a hotspot's, or the key alone) at ocbmd's start: removed, with what that
    /// mode kept beside it, and no link; a mode-ap file is the link, its neighbours left; no file is no link.
    #[test]
    fn an_old_hotspot_link_file_is_removed_at_the_start_with_its_leftovers() {
        let path = format!("/tmp/ocbmd_selftest_wifi_retire_{}", std::process::id());
        let beside = |s: &str| format!("{path}{s}");
        let k = format!("key={}\n", "0a".repeat(32));
        for old in [format!("{k}ssid_hex=4578616d706c65\npsk={}\n", "cd".repeat(32)), k.clone()] {
            std::fs::write(&path, &old).unwrap();
            for s in RETIRED_BESIDE {
                std::fs::write(beside(s), "x\n").unwrap();
            }
            assert_eq!(start_link_conf(&path), (None, true));
            assert!(!std::path::Path::new(&path).exists(), "the old link file gone");
            assert!(RETIRED_BESIDE.iter().all(|s| !std::path::Path::new(&beside(s)).exists()), "its leftovers gone");
        }
        std::fs::write(&path, format!("{k}mode=ap\nband=5\n")).unwrap();
        std::fs::write(beside("_channel"), "x\n").unwrap();
        assert_eq!(start_link_conf(&path), (Some(LinkConf { key: LinkKey([0x0a; 32]), band: Band::G5, band_prev: None }), false));
        assert!(std::path::Path::new(&path).exists() && std::path::Path::new(&beside("_channel")).exists(), "mode ap: kept");
        std::fs::write(&path, "key=short\nmode=ap\n").unwrap();
        assert_eq!(start_link_conf(&path), (None, false), "mode ap with no valid key: off, the file left as it is");
        assert!(std::path::Path::new(&path).exists());
        for p in [path.clone(), beside("_channel")] {
            let _ = std::fs::remove_file(p);
        }
        assert_eq!(start_link_conf(&path), (None, false), "no file");
    }

    fn ap_root(tag: &str) -> String {
        let r = format!("/tmp/ocbmd_selftest_apnet_{tag}_{}", std::process::id());
        let _ = std::fs::remove_dir_all(&r);
        std::fs::create_dir_all(format!("{r}/etc/ocbm")).unwrap();
        std::fs::create_dir_all(format!("{r}/tmp")).unwrap();
        r
    }

    /// v2's ACK names the AP as the AP layer runs it (radio_ap_up.sh's ap_passphrase), or the v2 is refused.
    #[test]
    fn ap_network_is_the_ap_the_ap_layer_keeps() {
        use std::os::unix::fs::PermissionsExt;
        let r = ap_root("net");
        let put = |f: &str, t: &str| std::fs::write(format!("{r}/{f}"), t).unwrap();
        put("etc/hostapd.conf", "interface=wlan0\nssid=CarLink-0000\nwpa_passphrase=example-pass\nchannel=36\n");
        assert!(ap_network(&r).is_err(), "no name decided yet");
        put("etc/carplay_ident", "ccpa-fe01\n");
        assert!(ap_network(&r).is_err(), "an older OCBM's name is derived again: not this one");
        put("etc/carplay_ident", "CarLink-0000\n");
        assert_eq!(ap_network(&r), Ok(("CarLink-0000".into(), "example-pass".into())), "hostapd.conf's, a good one");
        // Defaults ap_passphrase counts as missing: the one OCBM kept, which it takes first.
        put("etc/ocbm/wifi_psk", "0f3a9c41d27e88b05c6e\n");
        for known in ["12345678", "carplayfd6c", "carplay", "seven77"] {
            put("etc/hostapd.conf", &format!("ssid=CarLink-0000\nwpa_passphrase={known}\n"));
            assert_eq!(ap_network(&r).map(|n| n.1), Ok("0f3a9c41d27e88b05c6e".into()), "{known}: the kept one");
        }
        // Nothing good: a new one, kept for the AP layer (0600), which takes the kept one before /etc/wifi_password;
        // unless that layer is writing right now.
        put("etc/ocbm/wifi_psk", "12345678\n");
        put("etc/wifi_password", "a stock owner's\n");
        put("tmp/.radio_ap_lock", "");
        assert!(ap_network(&r).is_err(), "the AP layer configuring: refused, try again");
        let _ = std::fs::remove_file(format!("{r}/tmp/.radio_ap_lock"));
        let (_, p) = ap_network(&r).unwrap();
        assert!(p.len() == 20 && p.bytes().all(|b| b.is_ascii_hexdigit()), "{p}");
        assert_eq!(std::fs::read_to_string(format!("{r}/etc/ocbm/wifi_psk")).unwrap(), format!("{p}\n"));
        assert_eq!(std::fs::metadata(format!("{r}/etc/ocbm/wifi_psk")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(ap_network(&r).map(|n| n.1), Ok(p), "the same one again");
        let _ = std::fs::remove_dir_all(&r);
    }

    /// Mode `ap`'s listener: the seam's AP on its interface, and nothing else.
    #[test]
    fn mode_ap_listens_on_the_aps_address_on_its_interface_only() {
        let at = |want: &'static str| move |i: &str| (i == want).then(|| Ipv4Addr::new(192, 168, 43, 1));
        let ok = "result=ok\nbackend=mapped\nap_iface=wlan0\nap_ip=192.168.43.1\nwlan_mac=02:00:00:00:00:01\n";
        let want = Some((SocketAddrV4::new(Ipv4Addr::new(192, 168, 43, 1), ocbm_link::PORT), "wlan0".to_string()));
        assert_eq!(ap_bind(ok, at("wlan0")), want, "the AP's own address, from wlan0 only");
        assert_eq!(ap_bind(ok, at("sta0")), None, "the interface does not hold it (yet)");
        assert_eq!(ap_bind(&ok.replace("result=ok", "result=fail"), at("wlan0")), None, "an AP that did not start");
        assert_eq!(ap_bind("", at("wlan0")), None, "no AP (its state goes with it)");
        let ncm = ok.replace("192.168.43.1", "192.168.50.2");
        assert_eq!(ap_bind(&ncm, |_| Some(Ipv4Addr::new(192, 168, 50, 2))), None, "never NCM's subnet");
        let any = ok.replace("192.168.43.1", "0.0.0.0");
        assert_eq!(ap_bind(&any, |_| Some(Ipv4Addr::UNSPECIFIED)), None, "never 0.0.0.0");
        let moved = ok.replace("192.168.43.1", "10.0.0.1");
        assert_eq!(ap_bind(&moved, |_| Some(Ipv4Addr::new(10, 0, 0, 1))).map(|(a, _)| a.ip().octets()), Some([10, 0, 0, 1]), "/etc/wifi_ip's");
    }

    /// Mode `ap`'s object: mode, state, band, channel and (27 Nov 2026) whether a band change is on trial, the last key.
    #[test]
    fn mgmt_info_in_mode_ap_says_mode_state_band_channel_and_a_trial_only() {
        let j = |b, linked, listening, ch, pending| ap_info_json(b, linked, listening, ch, pending);
        assert_eq!(j(Band::G24, false, false, 36, false), "{\"mode\":\"ap\",\"state\":\"off\",\"ap_band\":\"2.4\",\"ap_channel\":36,\"band_pending\":false}");
        assert_eq!(j(Band::G24, false, true, 6, false), "{\"mode\":\"ap\",\"state\":\"up\",\"ap_band\":\"2.4\",\"ap_channel\":6,\"band_pending\":false}");
        assert_eq!(j(Band::G5, true, true, 36, false), "{\"mode\":\"ap\",\"state\":\"linked\",\"ap_band\":\"5\",\"ap_channel\":36,\"band_pending\":false}");
        assert_eq!(j(Band::G5, false, true, 6, true), "{\"mode\":\"ap\",\"state\":\"up\",\"ap_band\":\"5\",\"ap_channel\":6,\"band_pending\":true}", "5 GHz on trial, the AP not moved yet");
    }

    /// Mode `ap`'s look: raise, restart at once for a change, restart for another band's AP only with no CarPlay on
    /// it, and never over a bring-up under way.
    #[test]
    fn mode_aps_look_keeps_the_ap_up_on_its_band() {
        use ApAct::*;
        let d = |busy, running, asked, mark, carplay| ap_decide(busy, running, asked, mark, Band::G24, carplay);
        assert_eq!(d(true, false, true, None, false), Wait, "a bring-up or teardown under way, or a set-up network");
        assert_eq!(d(false, false, false, None, true), Raise, "no AP: raised, whatever runs");
        assert_eq!(d(false, true, true, Some("2.4"), true), Restart, "a change: at once, CarPlay or not");
        assert_eq!(d(false, true, false, Some("2.4"), false), Wait, "the band's AP");
        assert_eq!(d(false, true, false, Some("5"), false), Restart, "another band's, no CarPlay on it");
        assert_eq!(d(false, true, false, None, false), Restart, "one not started in mode ap");
        assert_eq!(d(false, true, false, None, true), Wait, "  but not under a phone's CarPlay");
        // ApKeep's pace (no AP runs on the test host): a look each 2 s, a raise each 15 s at most.
        let mut k = ApKeep::default();
        let t = Instant::now();
        assert_eq!(k.tick(t, Band::G24, false), Some("wifi_ap_on"));
        assert_eq!(k.tick(t + Duration::from_secs(1), Band::G24, false), None, "the next look is 2 s on");
        assert_eq!(k.tick(t + Duration::from_secs(3), Band::G24, false), None, "a raise not again within 15 s");
        assert_eq!(k.tick(t + Duration::from_secs(9), Band::G24, true), None, "a set-up network up: wait");
        k.restart = true;
        assert_eq!(k.tick(t + Duration::from_secs(17), Band::G24, false), Some("wifi_ap_on"), "no AP: a raise takes the band");
        assert!(!k.restart, "asked once");
        // No AP comes up: each raise waits twice the last (15, 30, 60 s ... 4 min).
        assert_eq!(k.tick(t + Duration::from_secs(40), Band::G24, false), None, "the third raise 30 s after the second");
        assert_eq!(k.tick(t + Duration::from_secs(48), Band::G24, false), Some("wifi_ap_on"));
        assert_eq!(k.tick(t + Duration::from_secs(100), Band::G24, false), None, "then 60 s");
        assert_eq!(k.tick(t + Duration::from_secs(108), Band::G24, false), Some("wifi_ap_on"));
    }

    #[test]
    fn the_link_file_is_stored_0600_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let path = format!("/tmp/ocbmd_selftest_wifi_store_{}", std::process::id());
        let c = LinkConf { key: LinkKey([9; 32]), band: Band::G24, band_prev: None };
        let kept = format!("{path}_bssid"); // the retired hotspot mode's kept address: any store takes it
        std::fs::write(&kept, "02:11:22:33:44:55\n").unwrap();
        store(Some(&c), &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read_link_conf(&path), Some(c));
        assert!(!std::path::Path::new(&kept).exists(), "a set-up: the retired mode's leftover gone");
        std::fs::write(&kept, "02:11:22:33:44:55\n").unwrap();
        store(None, &path).unwrap();
        assert!(!std::path::Path::new(&path).exists() && !std::path::Path::new(&kept).exists());
        store(None, &path).unwrap(); // removing nothing is fine
    }

    #[test]
    fn ap_channel_and_the_reserved_subnets_read_back() {
        assert_eq!(ap_channel("interface=wlan0\nchannel=36\nhw_mode=a\n"), 36);
        assert_eq!(ap_channel("ssid=x\n"), 0);
        assert!(reserved_subnet(Ipv4Addr::new(192, 168, 43, 10)) && reserved_subnet(Ipv4Addr::new(192, 168, 50, 2)));
        assert!(!reserved_subnet(Ipv4Addr::new(172, 20, 10, 2)));
    }

    /// `ap_channel` is the running AP's: its per-session configuration's where it took mode `ap`'s band.
    #[test]
    fn ap_channel_is_the_running_configurations() {
        let dir = format!("/tmp/ocbmd_selftest_ap_conf_{}", std::process::id());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (session, flash) = (format!("{dir}/hostapd_session.conf"), format!("{dir}/hostapd.conf"));
        std::fs::write(&flash, "interface=wlan0\nhw_mode=a\nchannel=36\n").unwrap();
        assert_eq!(ap_channel(&ap_conf_text(&session, &flash)), 36, "no session configuration: the flash file's");
        std::fs::write(&session, "interface=wlan0\nhw_mode=g\nchannel=6\n").unwrap();
        assert_eq!(ap_channel(&ap_conf_text(&session, &flash)), 6, "the AP on mode ap's 2.4 GHz: its session's");
        std::fs::remove_file(&session).unwrap();
        std::fs::remove_file(&flash).unwrap();
        assert_eq!(ap_channel(&ap_conf_text(&session, &flash)), 0, "neither: unknown");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No link: round 1's object, byte for byte (Carlink reads it so).
    #[test]
    fn mgmt_info_with_no_link_is_mode_off() {
        assert_eq!(off_info_json(36), "{\"mode\":\"off\",\"state\":\"off\",\"channel\":0,\"ap_channel\":36}");
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
