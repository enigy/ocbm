//! The AirPlay video probe (branch `airplay-video-probe`, the Mac's 10 Nov brief on Scott's OK): two
//! bench switches that decide between route A and route C before anything is built. Route A offers
//! AirPlay video inside the CarPlay session, as an automaker can from iOS 26. Route C advertises an
//! ordinary AirPlay receiver beside CarPlay. Nothing plays and Carlink doesn't change: each switch only
//! offers, and logs what the phone does next.
//!
//! Both are OFF unless [`FLAG_FILE`] names them. carplayd reads it once, at start, and every wireless
//! session spawns a fresh carplayd, so a changed file takes effect at the next phone connection. With
//! no file the box behaves exactly as without this module. The file holds words in any order; `#`
//! starts a comment that runs to the end of the line:
//!
//! ```text
//! video      route A: echo `videoPlayback` in the phase-1 SETUP when the phone proposes it, add
//!            LIVI item 5's `videoPlaybackInfo` and `playbackCapabilities` to /info, tell the phone
//!            setVideoPlaybackAllowed(true) once at RECORD, and log the DataStreams and requests that
//!            follow
//! bits       with video: `featuresEx` also carries bits 0 and 49, the bits macOS's AirPlay sender
//!            tests for video [I]
//! noallow    with video: leave setVideoPlaybackAllowed out
//! receiver   route C: a second, ordinary `_airplay._tcp` service, "Carlink Video", whose listener
//!            logs the first requests of each connection and closes it
//! txt:K=V    with receiver: that service's TXT key K becomes V (`txt:K=` drops K)
//! ```
//!
//! Every line written here starts `[probe]`, in carplayd's log (`/tmp/carplayd_wl.log` for a wireless
//! session). Keys, value types and sizes only: no URL past its host, no titles, names or other values a
//! phone might put in a play request. The exceptions are protocol identifiers: a message's own `type`,
//! a DataStream's client type, and the header values in [`SHOWN_HEADERS`].

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use plist::{Dictionary, Value};
use rtsp::message::{Request, Response};

/// The probe's switches, on the adapter. `/etc` is on flash, so they survive a reboot until removed.
pub const FLAG_FILE: &str = "/etc/ocbm/airplay_probe";

/// More than any switch file needs; a larger file is read this far.
const FLAG_FILE_MAX: u64 = 4096;

/// The switches [`FLAG_FILE`] names. The default is all off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Switches {
    /// Route A: video offered inside the CarPlay session.
    pub video: bool,
    /// With `video`: [`VIDEO_BITS`] ORed into `featuresEx`.
    pub bits: bool,
    /// With `video`: setVideoPlaybackAllowed(true) at RECORD. On with `video` unless `noallow`.
    pub allow: bool,
    /// Route C: an ordinary AirPlay receiver beside CarPlay.
    pub receiver: bool,
    /// With `receiver`: TXT keys set (`Some`) or dropped (`None`) over [`receiver_txt`]'s defaults.
    pub txt: Vec<(String, Option<String>)>,
    /// Words this build doesn't know, for the start line.
    pub unknown: Vec<String>,
}

impl Switches {
    /// Parse the switch file's text. Unknown words are kept for the log, never fatal.
    pub fn parse(text: &str) -> Switches {
        let mut s = Switches::default();
        let mut noallow = false;
        let words = text.lines().flat_map(|l| l.split('#').next().unwrap_or("").split_whitespace());
        for w in words {
            match w {
                "video" => s.video = true,
                "bits" => s.bits = true,
                "noallow" => noallow = true,
                "receiver" => s.receiver = true,
                _ => match w.strip_prefix("txt:").and_then(|kv| kv.split_once('=')) {
                    Some((k, v)) if txt_key_ok(k) && txt_value_ok(v) => {
                        s.txt.retain(|(old, _)| old != k);
                        s.txt.push((k.to_string(), (!v.is_empty()).then(|| v.to_string())));
                    }
                    _ => s.unknown.push(w.chars().take(40).collect()),
                },
            }
        }
        s.allow = s.video && !noallow;
        s
    }

    /// One line for the log: what is on.
    pub fn describe(&self) -> String {
        let mut on = Vec::new();
        if self.video {
            on.push(format!(
                "video (featuresEx {}, setVideoPlaybackAllowed {})",
                if self.bits { "with bits 0 and 49" } else { "as /info features" },
                if self.allow { "at RECORD" } else { "left out" }
            ));
        }
        if self.receiver {
            on.push(format!("receiver (TXT changes {:?})", self.txt));
        }
        if on.is_empty() {
            on.push("nothing".into());
        }
        let mut line = format!("on: {}", on.join("; "));
        if !self.unknown.is_empty() {
            line.push_str(&format!("; not understood: {:?}", self.unknown));
        }
        line
    }
}

/// A TXT key the switch file may set: a short word.
fn txt_key_ok(k: &str) -> bool {
    !k.is_empty() && k.len() <= 16 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A TXT value the switch file may set: printable ASCII, no spaces (the file splits on them).
fn txt_value_ok(v: &str) -> bool {
    v.len() <= 128 && v.chars().all(|c| c.is_ascii_graphic())
}

/// This process's switches, read from [`FLAG_FILE`] on first use (carplayd's `main` makes that first
/// use, so the file is read when carplayd starts). No file: all off, and nothing is logged.
pub fn switches() -> &'static Switches {
    static S: OnceLock<Switches> = OnceLock::new();
    S.get_or_init(|| {
        let mut text = String::new();
        match std::fs::File::open(FLAG_FILE) {
            Ok(f) => {
                if let Err(e) = f.take(FLAG_FILE_MAX).read_to_string(&mut text) {
                    eprintln!("[probe] {FLAG_FILE} unreadable ({e}): everything stays off");
                    return Switches::default();
                }
                let s = Switches::parse(&text);
                eprintln!("[probe] {FLAG_FILE}: {}", s.describe());
                s
            }
            Err(_) => Switches::default(),
        }
    })
}

/// One `[probe]` line. The first 400 go out whole, then one in 100 with its number, so a chatty
/// channel can't fill the tmpfs log.
pub fn line(msg: std::fmt::Arguments) {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    if n <= 400 {
        eprintln!("[probe] {msg}");
    } else if n % 100 == 0 {
        eprintln!("[probe] {msg}  (probe line {n}: past 400, one in 100 is logged)");
    }
}

// ---- Route A: the SETUP echo, the /info keys, setVideoPlaybackAllowed ----

/// Bits 0 and 49: what macOS 27's AirPlay sender tests a receiver's features for before it offers
/// video (the Mac's 10 Nov reading). None of the three receivers it read sets them; on CarPlay, [I].
pub const VIDEO_BITS: u64 = 1 | (1 << 49);

/// `featuresEx`: the feature bits as bytes, low byte first, trailing zero bytes dropped, then Base64.
/// That's CarPlaySDK's `APSFeaturesCopyStringRepresentation` form, and what LIVI sends as "the
/// features again".
pub fn features_ex(features: u64) -> String {
    let bytes = features.to_le_bytes();
    let used = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    base64(&bytes[..used])
}

/// Standard Base64 (RFC 4648 §4) with padding. The box crates carry no encoder, and this is all of one.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Route A's /info keys, in LIVI item 5's shapes: `videoPlaybackInfo` holds `videoPlaybackAllowed`
/// (true: the bench is parked) and `featuresEx` (`features`, plus [`VIDEO_BITS`] with `bits`), and
/// `playbackCapabilities` says no FairPlay secure stop and no UI for audio-only content.
/// `extendedFeatures` and `features` stay as they are.
pub fn info_keys(features: u64, bits: bool) -> [(&'static str, Value); 2] {
    let features = if bits { features | VIDEO_BITS } else { features };
    let mut info = Dictionary::new();
    info.insert("videoPlaybackAllowed".into(), Value::Boolean(true));
    info.insert("featuresEx".into(), Value::String(features_ex(features)));
    let mut caps = Dictionary::new();
    caps.insert("supportsFPSSecureStop".into(), Value::Boolean(false));
    caps.insert("supportsUIForAudioOnlyContent".into(), Value::Boolean(false));
    [("videoPlaybackInfo", Value::Dictionary(info)), ("playbackCapabilities", Value::Dictionary(caps))]
}

/// Does the phone's phase-1 SETUP `features` list carry `word`?
pub fn proposes(req: &Dictionary, word: &str) -> bool {
    req.get("features")
        .and_then(Value::as_array)
        .is_some_and(|words| words.iter().any(|w| w.as_string() == Some(word)))
}

/// Should this phase-1 SETUP echo `videoPlayback`? Only with route A on, and only when the phone
/// proposed it: an echo of a word the phone didn't propose would be a feature it never asked for.
pub fn echoes_video(s: &Switches, req: Option<&Dictionary>) -> bool {
    s.video && req.is_some_and(|d| proposes(d, "videoPlayback"))
}

/// `setVideoPlaybackAllowed`, in LIVI's shape: `{type, params: {videoPlaybackAllowed, playbackStatus}}`
/// on the event channel, as `setNightMode` goes. LIVI sends it at RECORD with status `idle`, and again
/// as parking changes; the probe sends it once. No Apple binary the Mac has carries the name, so on
/// CarPlay it stays [I].
pub fn allowed_command(allowed: bool) -> Vec<u8> {
    let mut params = Dictionary::new();
    params.insert("videoPlaybackAllowed".into(), Value::Boolean(allowed));
    params.insert("playbackStatus".into(), Value::String("idle".into()));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("setVideoPlaybackAllowed".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    // A dictionary of three plain values always serializes; an empty body would just fail to send.
    let _ = Value::Dictionary(d).to_writer_binary(&mut body);
    body
}

/// The RCS client types, by `clientTypeUUID`: CarPlaySDK's names (docs/carplay/05_METADATA_AND_CONTROLS.md
/// §1.2) and, for the three the brief expects once video is offered, LIVI item 5's.
pub fn channel_name(client_type: &str) -> &'static str {
    const KNOWN: [(&str, &str); 7] = [
        ("E9459FD0-BCAD-4C45-820F-1E72447EF2F2", "iAP"),
        ("75AD9926-4777-42B2-A7D8-823EBEECF7AA", "LogTransfer"),
        ("3E2F3C61-AAD0-42CB-A8AA-BF22186DA62E", "VehicleDataProtocol"),
        ("FF4A6720-F2BE-4F56-A3E1-DB3B4E37D634", "VehicleDataProtocolHigh"),
        ("A6B27562-B43A-4F2D-B75F-82391E250194", "UrlFling (LIVI: Playback, MatchPoint)"),
        ("E3DC3EA6-E6C3-4B30-847C-B7ACFEBEA654", "OverlayUI (LIVI: Fitness UI overlay)"),
        ("BB493F61-A6B8-4769-8D74-80C23A9F71C4", "SenderSettingsData (LIVI: CarPlay video settings)"),
    ];
    KNOWN
        .iter()
        .find(|(uuid, _)| uuid.eq_ignore_ascii_case(client_type))
        .map_or("unknown client type", |&(_, name)| name)
}

// ---- What the log may say about a value ----

/// How deep a shape goes, and how long it may grow, before it is cut.
const SHAPE_DEPTH: usize = 6;
const SHAPE_MAX: usize = 1200;

/// A plist's shape for the log: keys, value types and sizes. Values never, except a short identifier
/// under `type` (a command's own name) and a URL's scheme and host.
pub fn shape(v: &Value) -> String {
    let mut out = String::new();
    plist_shape(&mut out, None, v, 0);
    out
}

/// [`shape`] for a dictionary already in hand.
pub fn dict_shape(d: &Dictionary) -> String {
    let mut out = String::new();
    dict_into(&mut out, d, 0);
    out
}

fn dict_into(out: &mut String, d: &Dictionary, depth: usize) {
    if depth >= SHAPE_DEPTH {
        out.push_str(&format!("{{{} keys}}", d.len()));
        return;
    }
    out.push('{');
    for (i, (k, v)) in d.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&key_text(k));
        out.push_str(": ");
        plist_shape(out, Some(k), v, depth + 1);
    }
    out.push('}');
}

fn plist_shape(out: &mut String, key: Option<&str>, v: &Value, depth: usize) {
    if out.len() > SHAPE_MAX {
        if !out.ends_with('…') {
            out.push('…');
        }
        return;
    }
    match v {
        Value::Dictionary(d) => dict_into(out, d, depth),
        Value::Array(a) => {
            out.push_str(&format!("[{}", a.len()));
            if let Some(first) = a.first().filter(|_| depth < SHAPE_DEPTH) {
                out.push_str(" of ");
                plist_shape(out, None, first, depth + 1);
            }
            out.push(']');
        }
        Value::String(s) => out.push_str(&string_shape(key, s)),
        Value::Data(b) => out.push_str(&format!("data({})", b.len())),
        Value::Integer(_) => out.push_str("int"),
        Value::Real(_) => out.push_str("real"),
        Value::Boolean(_) => out.push_str("bool"),
        Value::Date(_) => out.push_str("date"),
        Value::Uid(_) => out.push_str("uid"),
        _ => out.push('?'),
    }
}

/// A dictionary key as the log shows it: the key itself when it reads as a protocol name, else its size.
fn key_text(k: &str) -> String {
    if !k.is_empty() && k.len() <= 48 && k.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        k.to_string()
    } else {
        format!("<key of {} B>", k.len())
    }
}

/// A command or message name: short, and made of identifier characters.
fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-.:".contains(c))
}

/// A string as the log shows it: its size; a URL's scheme and host; the value itself only under `type`.
fn string_shape(key: Option<&str>, s: &str) -> String {
    if key == Some("type") && is_identifier(s) {
        return format!("{s:?}");
    }
    match url_host(s) {
        Some(host) => format!("url({host}, {} B)", s.len()),
        None => format!("string({})", s.len()),
    }
}

/// A URL's `scheme://host[:port]`, its userinfo dropped. `None` for a string that isn't a URL.
pub fn url_host(s: &str) -> Option<String> {
    let (scheme, rest) = s.split_once("://")?;
    let scheme_ok = scheme.len() <= 16
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    if !scheme_ok {
        return None;
    }
    let authority = rest.split(&['/', '?', '#'][..]).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    Some(format!("{scheme}://{host}"))
}

/// A request target for the log: an absolute URI's scheme and host, or a path up to its query (whose
/// size stands in for it), cut at 64 characters.
pub fn target(uri: &str) -> String {
    if let Some(host) = url_host(uri) {
        let more = uri.len() > host.len();
        return if more { format!("{host}/…") } else { host };
    }
    let clip = |p: &str| -> String {
        if p.chars().count() > 64 {
            format!("{}…", p.chars().take(64).collect::<String>())
        } else {
            p.to_string()
        }
    };
    match uri.split_once('?') {
        Some((path, query)) => format!("{}?({} B)", clip(path), query.len()),
        None => clip(uri),
    }
}

/// Headers whose values are protocol facts, shown whole. Every other header shows its name and size:
/// `X-Apple-Client-Name` carries the phone's name, and `X-Apple-Device-ID` who it is.
pub const SHOWN_HEADERS: [&str; 8] = [
    "CSeq",
    "Content-Type",
    "Content-Length",
    "Connection",
    "Accept",
    "User-Agent",
    "X-Apple-ProtocolVersion",
    "X-Apple-HKP",
];

/// A request's headers for the log.
pub fn headers(hs: &[(String, String)]) -> String {
    let parts: Vec<String> = hs
        .iter()
        .map(|(k, v)| {
            let name = key_text(k);
            if SHOWN_HEADERS.iter().any(|s| s.eq_ignore_ascii_case(k)) {
                format!("{name}: {}", v.chars().take(64).collect::<String>())
            } else {
                format!("{name} ({} B)", v.len())
            }
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

/// A body's shape: a binary plist's keys, a `text/parameters` body's names, an OPACK value's keys, or
/// only its size and first byte.
pub fn body_shape(content_type: Option<&str>, body: &[u8]) -> String {
    if body.is_empty() {
        return "no body".into();
    }
    if body.starts_with(b"bplist") {
        return match Value::from_reader(std::io::Cursor::new(body)) {
            Ok(v) => format!("{} B bplist {}", body.len(), shape(&v)),
            Err(_) => format!("{} B bplist that doesn't parse", body.len()),
        };
    }
    if content_type.is_some_and(|t| t.eq_ignore_ascii_case("text/parameters")) {
        let names: Vec<String> = String::from_utf8_lossy(body)
            .lines()
            .filter_map(|l| l.split_once(':').map(|(n, _)| key_text(n.trim())))
            .collect();
        return format!("{} B text/parameters named {names:?}", body.len());
    }
    if let Some(s) = opack_shape(body) {
        return format!("{} B opack {s}", body.len());
    }
    format!("{} B, first byte {:#04x}", body.len(), body[0])
}

/// The shape of an OPACK value, as [`shape`] writes a plist's. OPACK is Apple's compact binary
/// encoding; the brief expects the SenderSettingsData channel to carry it. The tag layout is the
/// public one (one tag byte; small ints, strings, data, arrays and dictionaries with their length or
/// count in the tag or the bytes after it). `None` when the bytes don't walk to their end as one value.
pub fn opack_shape(b: &[u8]) -> Option<String> {
    let mut out = String::new();
    let used = opack_value(b, 0, &mut out, None, 0)?;
    (used == b.len()).then_some(out)
}

/// A little-endian length or count of `n` bytes at `b[at..]`.
fn le(b: &[u8], at: usize, n: usize) -> Option<usize> {
    let bytes = b.get(at..at.checked_add(n)?)?;
    let v = bytes.iter().rev().fold(0u64, |v, &x| (v << 8) | u64::from(x));
    usize::try_from(v).ok()
}

/// Walk one OPACK value at `b[at..]`, write its shape, and return where it ends.
fn opack_value(b: &[u8], at: usize, out: &mut String, key: Option<&str>, depth: usize) -> Option<usize> {
    if depth > SHAPE_DEPTH || out.len() > SHAPE_MAX {
        return None;
    }
    let tag = *b.get(at)?;
    let body = at + 1;
    // A payload of `len` bytes from `after`: its length and where it ends, if the bytes hold it.
    let sized = |len: usize, after: usize| -> Option<(usize, usize)> {
        let end = after.checked_add(len)?;
        (end <= b.len()).then_some((len, end))
    };
    match tag {
        0x01 | 0x02 => {
            out.push_str("bool");
            Some(body)
        }
        0x04 => {
            out.push_str("null");
            Some(body)
        }
        0x05 => {
            out.push_str("uuid");
            sized(16, body).map(|(_, end)| end)
        }
        0x06 => {
            out.push_str("date");
            sized(8, body).map(|(_, end)| end)
        }
        0x08..=0x2F => {
            out.push_str("int");
            Some(body)
        }
        0x30..=0x33 => {
            out.push_str("int");
            sized(1 << (tag - 0x30), body).map(|(_, end)| end)
        }
        0x35 | 0x36 => {
            out.push_str("real");
            sized(if tag == 0x35 { 4 } else { 8 }, body).map(|(_, end)| end)
        }
        0x40..=0x60 | 0x61..=0x64 => {
            let (len, after) = if tag <= 0x60 {
                (usize::from(tag - 0x40), body)
            } else {
                let n = usize::from(tag - 0x60);
                (le(b, body, n)?, body + n)
            };
            let (_, end) = sized(len, after)?;
            let text = std::str::from_utf8(b.get(after..end)?).ok()?;
            out.push_str(&string_shape(key, text));
            Some(end)
        }
        0x70..=0x90 | 0x91..=0x94 => {
            let (len, after) = if tag <= 0x90 {
                (usize::from(tag - 0x70), body)
            } else {
                let n = 1usize << (tag - 0x91);
                (le(b, body, n)?, body + n)
            };
            let (len, end) = sized(len, after)?;
            out.push_str(&format!("data({len})"));
            Some(end)
        }
        0xA0..=0xBF => {
            out.push_str("ref");
            Some(body)
        }
        0xD0..=0xDF => {
            let count = (tag < 0xDF).then_some(usize::from(tag - 0xD0));
            let mut at = body;
            let mut n = 0usize;
            out.push('[');
            loop {
                if count == Some(n) || (count.is_none() && b.get(at) == Some(&0x03)) {
                    break;
                }
                if n == 1 {
                    out.push_str(", …");
                }
                let mut scratch = String::new();
                at = opack_value(b, at, if n == 0 { &mut *out } else { &mut scratch }, None, depth + 1)?;
                n += 1;
            }
            if count.is_none() {
                at += 1; // the 0x03 that ends it
            }
            out.push_str(&format!("] ({n})"));
            Some(at)
        }
        0xE0..=0xEF => {
            let count = (tag < 0xEF).then_some(usize::from(tag - 0xE0));
            let mut at = body;
            let mut n = 0usize;
            out.push('{');
            loop {
                if count == Some(n) || (count.is_none() && b.get(at) == Some(&0x03)) {
                    break;
                }
                if n > 0 {
                    out.push_str(", ");
                }
                // The key as a plist key shows: its text when it is a string, else its shape.
                let mut k = String::new();
                let kat = at;
                at = opack_value(b, at, &mut k, None, depth + 1)?;
                let name = opack_string(b, kat);
                out.push_str(&name.map_or(k, key_text));
                out.push_str(": ");
                at = opack_value(b, at, out, name, depth + 1)?;
                n += 1;
            }
            if count.is_none() {
                at += 1;
            }
            out.push('}');
            Some(at)
        }
        _ => None,
    }
}

/// The text of the OPACK string at `b[at..]`, if that is what it is.
fn opack_string(b: &[u8], at: usize) -> Option<&str> {
    let tag = *b.get(at)?;
    let (len, after) = match tag {
        0x40..=0x60 => (usize::from(tag - 0x40), at + 1),
        0x61..=0x64 => {
            let n = usize::from(tag - 0x60);
            (le(b, at + 1, n)?, at + 1 + n)
        }
        _ => return None,
    };
    std::str::from_utf8(b.get(after..after.checked_add(len)?)?).ok()
}

/// One request the session or route C's listener doesn't otherwise handle, for the log.
pub fn log_request(place: &str, req: &Request) {
    line(format_args!(
        "{place}: {} {} {} headers={} body={}",
        key_text(&req.method),
        target(&req.uri),
        req.version.token(),
        headers(&req.headers),
        body_shape(req.header("Content-Type"), &req.body)
    ));
}

/// [`body_shape`], then one level into a `data` field when there is one: a binary plist's `params.data` or
/// `data`, or an OPACK dictionary's `data`. The Playback channel's commands ride POST /command as
/// `{params: {data}}`, and the SenderSettingsData messages are OPACK `{data, messageType}` (the Mac's 10 Nov
/// bench): the play request's keys are in those bytes.
pub fn payload_shape(b: &[u8]) -> String {
    let outer = body_shape(None, b);
    let inner = if b.starts_with(b"bplist") {
        Value::from_reader(std::io::Cursor::new(b))
            .ok()
            .and_then(|v| v.as_dictionary().and_then(dict_data).map(inner_shape))
    } else {
        opack_data(b).map(inner_shape)
    };
    match inner {
        Some(i) => format!("{outer}; its data: {i}"),
        None => outer,
    }
}

/// A plist dictionary's `params.data`, else its own `data`, when that is bytes.
pub fn dict_data(d: &Dictionary) -> Option<&[u8]> {
    let in_params = d.get("params").and_then(Value::as_dictionary).and_then(|p| p.get("data"));
    in_params.or_else(|| d.get("data")).and_then(Value::as_data)
}

/// One level into a message's data, no deeper: an RCS envelope (its kind, its message type, its payload's
/// shape), else the bytes' own shape.
pub fn inner_shape(b: &[u8]) -> String {
    match crate::datastream::unwrap(b) {
        Some(f) => format!("an RCS '{}' '{}' message, {}", f.kind_str(), f.msg_type_str(), body_shape(None, f.payload)),
        None => body_shape(None, b),
    }
}

/// The bytes of an OPACK dictionary's `data` entry, when the value at the top is a dictionary that has one.
fn opack_data(b: &[u8]) -> Option<&[u8]> {
    let tag = *b.first()?;
    if !(0xE0..=0xEF).contains(&tag) {
        return None;
    }
    let count = (tag < 0xEF).then_some(usize::from(tag - 0xE0));
    let mut at = 1;
    let mut n = 0usize;
    let mut scratch = String::new();
    loop {
        if count == Some(n) || (count.is_none() && b.get(at) == Some(&0x03)) {
            return None;
        }
        let key = opack_string(b, at);
        let value_at = opack_value(b, at, &mut scratch, None, 1)?;
        if key == Some("data") {
            return opack_bytes(b, value_at);
        }
        at = opack_value(b, value_at, &mut scratch, None, 1)?;
        scratch.clear();
        n += 1;
    }
}

/// The bytes of the OPACK data value at `b[at..]`.
fn opack_bytes(b: &[u8], at: usize) -> Option<&[u8]> {
    let tag = *b.get(at)?;
    let (len, after) = match tag {
        0x70..=0x90 => (usize::from(tag - 0x70), at + 1),
        0x91..=0x94 => {
            let n = 1usize << (tag - 0x91);
            (le(b, at + 1, n)?, at + 1 + n)
        }
        _ => return None,
    };
    b.get(after..after.checked_add(len)?)
}

/// A TEARDOWN's streams for the log: each entry's `type` and ids by value (`channelID`, `streamID`,
/// `streamConnectionID`, protocol numbers and names), its other keys by shape.
pub fn teardown_streams(streams: &[Value]) -> String {
    const IDS: [&str; 4] = ["type", "channelID", "streamID", "streamConnectionID"];
    let entries: Vec<String> = streams
        .iter()
        .map(|s| match s.as_dictionary() {
            Some(d) => {
                let parts: Vec<String> = d
                    .iter()
                    .map(|(k, v)| match v {
                        Value::Integer(i) if IDS.contains(&k.as_str()) => {
                            let n = i.as_signed().map(|n| n.to_string()).or_else(|| i.as_unsigned().map(|n| n.to_string()));
                            format!("{k}={}", n.unwrap_or_default())
                        }
                        Value::String(t) if IDS.contains(&k.as_str()) && t.len() <= 64 && t.chars().all(|c| c.is_ascii_graphic()) => {
                            format!("{k}={t:?}")
                        }
                        _ => format!("{}: {}", key_text(k), shape(v)),
                    })
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            None => shape(s),
        })
        .collect();
    format!("[{}]", entries.join(", "))
}

// ---- Route C: an ordinary AirPlay receiver beside CarPlay ----

/// Route C's Bonjour instance name.
pub const RECEIVER_NAME: &str = "Carlink Video";
/// Route C's own host name, so its address record never shares a name with the CarPlay service's.
pub const RECEIVER_HOST: &str = "carlinkvideo.local.";
/// The port an AirPlay receiver listens on in the public documentation; any free port if it's taken.
pub const RECEIVER_PORT: u16 = 7000;
/// Requests logged per connection before route C's listener closes it.
const RECEIVER_REQUESTS: usize = 4;

/// A device id for route C that isn't the CarPlay service's: the same address with the
/// locally-administered bit flipped, so the phone can't take the two services for one device.
pub fn receiver_device_id(dev_id: &str) -> String {
    let octets: Vec<u8> = dev_id.split(':').filter_map(|o| u8::from_str_radix(o, 16).ok()).collect();
    let mut mac = match <[u8; 6]>::try_from(octets.as_slice()) {
        Ok(m) => m,
        Err(_) => [0x02, 0, 0, 0, 0, 0x01],
    };
    mac[0] ^= 0x02;
    mac.iter().map(|o| format!("{o:02X}")).collect::<Vec<_>>().join(":")
}

/// Route C's TXT record: [`RECEIVER_TXT`] with route C's own device id, then the switch file's `txt:`
/// changes (which can add any key the documentation lists, `pi`, `pk`, `flags` or `vv` among them).
pub fn receiver_txt(dev_id: &str, changes: &[(String, Option<String>)]) -> Vec<(String, String)> {
    let mut txt: Vec<(String, String)> = vec![("deviceid".into(), receiver_device_id(dev_id))];
    txt.extend(RECEIVER_TXT.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    for (k, v) in changes {
        txt.retain(|(old, _)| old != k);
        if let Some(v) = v {
            txt.push((k.clone(), v.clone()));
        }
    }
    txt
}

/// The rest of route C's TXT record: the Apple TV example in the public protocol documentation, the
/// same in both (nto's "Unofficial AirPlay Protocol Specification" §2.2,
/// <https://nto.github.io/AirPlay.html>, and openairplay's Service Discovery page,
/// <https://openairplay.github.io/airplay-spec/service_discovery.html>). Its features `0x39f7` set
/// Video (bit 0), VideoHTTPLiveStreams (4) and Screen (7) among others: an ordinary video and
/// screen-mirroring receiver. Port 7000, as there.
pub const RECEIVER_TXT: [(&str, &str); 3] = [("features", "0x39f7"), ("model", "AppleTV2,1"), ("srcvers", "130.14")];

/// Route C's listener: bind [`RECEIVER_PORT`] (else any port), and log the first requests of each
/// connection — method, target, headers, body shape — answering each `501 Not Implemented` and closing
/// after [`RECEIVER_REQUESTS`] or 5 quiet seconds. One connection at a time. Returns the bound port.
pub fn spawn_receiver_listener() -> Option<u16> {
    let listener = match TcpListener::bind(("::", RECEIVER_PORT)).or_else(|_| TcpListener::bind(("::", 0))) {
        Ok(l) => l,
        Err(e) => {
            line(format_args!("receiver: no listener ({e}); \"{RECEIVER_NAME}\" is not advertised"));
            return None;
        }
    };
    let port = listener.local_addr().ok()?.port();
    let spawned = std::thread::Builder::new().name("probe-receiver".into()).spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(s) => serve_receiver_connection(s),
                Err(e) => {
                    line(format_args!("receiver: accept failed: {e}"));
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });
    match spawned {
        Ok(_) => Some(port),
        Err(e) => {
            line(format_args!("receiver: no listener thread ({e}); \"{RECEIVER_NAME}\" is not advertised"));
            None
        }
    }
}

fn serve_receiver_connection(mut s: TcpStream) {
    let peer = s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_else(|_| "?".into());
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    line(format_args!("receiver: connection from {peer}"));
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 2048];
    let mut served = 0usize;
    let why = loop {
        if served == RECEIVER_REQUESTS {
            break "enough requests";
        }
        match Request::parse(&buf) {
            Ok(Some((req, used))) => {
                buf.drain(..used);
                served += 1;
                log_request("receiver", &req);
                if s.write_all(&Response::status(&req, 501, "Not Implemented").serialize()).is_err() {
                    break "the answer couldn't be written";
                }
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                line(format_args!(
                    "receiver: {} B that aren't a request ({e:?}), first bytes {:02x?}",
                    buf.len(),
                    &buf[..buf.len().min(8)]
                ));
                break "not a request";
            }
        }
        if buf.len() > rtsp::message::MAX_BODY + 8192 {
            break "a request too large";
        }
        match s.read(&mut chunk) {
            Ok(0) => break "the phone closed it",
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break "quiet for 5 s",
        }
    };
    line(format_args!("receiver: {peer} closed after {served} request(s): {why}"));
    let _ = s.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_and_an_empty_file_turn_nothing_on() {
        assert_eq!(Switches::default(), Switches::parse(""));
        let off = Switches::parse("# nothing yet\n\n");
        assert!(!off.video && !off.bits && !off.allow && !off.receiver);
        assert!(off.txt.is_empty() && off.unknown.is_empty());
        assert_eq!(off.describe(), "on: nothing");
    }

    #[test]
    fn the_file_names_the_switches_in_any_order() {
        let s = Switches::parse("receiver\nvideo bits  # both routes\n");
        assert!(s.video && s.bits && s.allow && s.receiver);
        let s = Switches::parse("video noallow");
        assert!(s.video && !s.allow, "noallow leaves setVideoPlaybackAllowed out");
        let s = Switches::parse("bits receiver");
        assert!(!s.video && !s.allow, "bits and receiver don't turn route A on");
        let s = Switches::parse("vidoe txt:bad key=1 txt:features=0x1 txt:features=0x5,0x1E txt:vv=");
        assert_eq!(s.unknown, ["vidoe", "txt:bad", "key=1"]);
        assert_eq!(s.txt, [("features".to_string(), Some("0x5,0x1E".to_string())), ("vv".to_string(), None)]);
        assert!(!s.video && !s.receiver);
    }

    #[test]
    fn base64_is_rfc_4648s() {
        for (plain, coded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(plain.as_bytes()), coded, "{plain:?}");
        }
    }

    #[test]
    fn features_ex_is_the_feature_bytes_low_first_without_trailing_zeros() {
        // OCBM's /info features 0x61_4444_0B80: 80 0B 44 44 61, the three zero bytes above dropped.
        assert_eq!(features_ex(0x0000_0061_4444_0B80), "gAtERGE=");
        // With bits 0 and 49: 81 0B 44 44 61 00 02.
        assert_eq!(features_ex(0x0000_0061_4444_0B80 | VIDEO_BITS), "gQtERGEAAg==");
        assert_eq!(features_ex(0), "");
        assert_eq!(features_ex(u64::MAX), "//////////8=");
    }

    #[test]
    fn the_info_keys_are_livis_shapes() {
        let [(k1, info), (k2, caps)] = info_keys(0x0000_0061_4444_0B80, false);
        assert_eq!((k1, k2), ("videoPlaybackInfo", "playbackCapabilities"));
        let info = info.into_dictionary().unwrap();
        assert_eq!(info.get("videoPlaybackAllowed"), Some(&Value::Boolean(true)));
        assert_eq!(info.get("featuresEx").and_then(Value::as_string), Some("gAtERGE="));
        assert_eq!(info.len(), 2);
        let caps = caps.into_dictionary().unwrap();
        assert_eq!(caps.get("supportsFPSSecureStop"), Some(&Value::Boolean(false)));
        assert_eq!(caps.get("supportsUIForAudioOnlyContent"), Some(&Value::Boolean(false)));
        assert_eq!(caps.len(), 2);
        let [(_, info), _] = info_keys(0x0000_0061_4444_0B80, true);
        let ex = info.as_dictionary().and_then(|d| d.get("featuresEx")).and_then(Value::as_string);
        assert_eq!(ex, Some("gQtERGEAAg=="), "bits ORs in 0 and 49");
    }

    #[test]
    fn video_is_echoed_only_when_on_and_proposed() {
        let mut req = Dictionary::new();
        req.insert(
            "features".into(),
            Value::Array(vec![Value::String("viewAreas".into()), Value::String("videoPlayback".into())]),
        );
        let on = Switches::parse("video");
        assert!(echoes_video(&on, Some(&req)));
        assert!(!echoes_video(&Switches::default(), Some(&req)), "off by default");
        assert!(!echoes_video(&Switches::parse("receiver bits"), Some(&req)));
        let mut other = Dictionary::new();
        other.insert("features".into(), Value::Array(vec![Value::String("viewAreas".into())]));
        assert!(!echoes_video(&on, Some(&other)), "not proposed, not echoed");
        assert!(!echoes_video(&on, Some(&Dictionary::new())));
        assert!(!echoes_video(&on, None));
    }

    #[test]
    fn set_video_playback_allowed_is_livis_shape() {
        let v = Value::from_reader(std::io::Cursor::new(allowed_command(true))).unwrap();
        let d = v.as_dictionary().unwrap();
        assert_eq!(d.get("type").and_then(Value::as_string), Some("setVideoPlaybackAllowed"));
        let p = d.get("params").and_then(Value::as_dictionary).unwrap();
        assert_eq!(p.get("videoPlaybackAllowed"), Some(&Value::Boolean(true)));
        assert_eq!(p.get("playbackStatus").and_then(Value::as_string), Some("idle"));
    }

    #[test]
    fn the_three_video_channels_are_named() {
        assert_eq!(channel_name("a6b27562-b43a-4f2d-b75f-82391e250194"), "UrlFling (LIVI: Playback, MatchPoint)");
        assert!(channel_name("BB493F61-A6B8-4769-8D74-80C23A9F71C4").starts_with("SenderSettingsData"));
        assert!(channel_name("E3DC3EA6-E6C3-4B30-847C-B7ACFEBEA654").starts_with("OverlayUI"));
        assert_eq!(channel_name("E9459FD0-BCAD-4C45-820F-1E72447EF2F2"), "iAP");
        assert_eq!(channel_name("00000000-0000-0000-0000-000000000000"), "unknown client type");
    }

    #[test]
    fn a_shape_names_keys_and_types_and_never_a_value() {
        let mut params = Dictionary::new();
        params.insert("url".into(), Value::String("https://user:pw@cdn.example.com:443/v/master.m3u8?token=s3cret".into()));
        params.insert("title".into(), Value::String("A Private Title".into()));
        params.insert("rate".into(), Value::Real(1.0));
        params.insert("items".into(), Value::Array(vec![Value::Integer(7.into()), Value::Integer(8.into())]));
        params.insert("blob".into(), Value::Data(vec![1, 2, 3]));
        let mut d = Dictionary::new();
        d.insert("type".into(), Value::String("insertPlayQueueItem".into()));
        d.insert("params".into(), Value::Dictionary(params));
        let s = shape(&Value::Dictionary(d));
        assert_eq!(
            s,
            r#"{type: "insertPlayQueueItem", params: {url: url(https://cdn.example.com:443, 62 B), title: string(15), rate: real, items: [2 of int], blob: data(3)}}"#
        );
        assert!(!s.contains("Private") && !s.contains("s3cret") && !s.contains("master") && !s.contains("pw"));
        // `type` shows only when it reads as a name.
        let mut d = Dictionary::new();
        d.insert("type".into(), Value::String("Scott's iPhone".into()));
        assert_eq!(shape(&Value::Dictionary(d)), "{type: string(14)}");
    }

    #[test]
    fn a_target_keeps_the_host_and_the_path_but_not_the_query() {
        assert_eq!(target("/info"), "/info");
        assert_eq!(target("/info?txtAirPlay&txtRAOP"), "/info?(18 B)");
        assert_eq!(target("rtsp://192.168.43.1/2468013579"), "rtsp://192.168.43.1/…");
        assert_eq!(target("http://cdn.example.com"), "http://cdn.example.com");
        assert_eq!(target(&format!("/{}", "a".repeat(80))).chars().count(), 65);
        assert_eq!(url_host("not a url"), None);
        assert_eq!(url_host("1http://x"), None);
    }

    #[test]
    fn headers_show_protocol_values_and_hide_who_the_phone_is() {
        let hs = vec![
            ("CSeq".to_string(), "3".to_string()),
            ("User-Agent".to_string(), "AirPlay/890.79.5".to_string()),
            ("X-Apple-Client-Name".to_string(), "Scott's iPhone".to_string()),
            ("X-Apple-Device-ID".to_string(), "0xA1B2C3D4E5F6".to_string()),
        ];
        assert_eq!(
            headers(&hs),
            "[CSeq: 3, User-Agent: AirPlay/890.79.5, X-Apple-Client-Name (14 B), X-Apple-Device-ID (14 B)]"
        );
    }

    #[test]
    fn a_body_shows_its_shape() {
        let mut d = Dictionary::new();
        d.insert("qualifier".into(), Value::Array(vec![Value::String("txtAirPlay".into())]));
        let mut b = Vec::new();
        Value::Dictionary(d).to_writer_binary(&mut b).unwrap();
        assert_eq!(body_shape(None, &b), format!("{} B bplist {{qualifier: [1 of string(10)]}}", b.len()));
        let params = b"Content-Location: http://cdn.example.com/v.mp4\r\nStart-Position: 0.0\r\n";
        assert_eq!(
            body_shape(Some("text/parameters"), params),
            format!("{} B text/parameters named [\"Content-Location\", \"Start-Position\"]", params.len())
        );
        assert_eq!(body_shape(None, &[]), "no body");
        assert_eq!(body_shape(None, &[0xFF, 0xFE]), "2 B, first byte 0xff");
    }

    #[test]
    fn opack_shows_its_keys_and_types() {
        // {"_i": "setSettings", "_x": 7, "_c": {"rate": 1.0 (f64), "ids": [uuid, uuid]}}
        let mut b = vec![0xE3];
        b.extend([0x42, b'_', b'i', 0x4B]);
        b.extend(b"setSettings");
        b.extend([0x42, b'_', b'x', 0x0F]);
        b.extend([0x42, b'_', b'c', 0xE2]);
        b.extend([0x44]);
        b.extend(b"rate");
        b.push(0x36);
        b.extend(1.0f64.to_le_bytes());
        b.extend([0x43]);
        b.extend(b"ids");
        b.push(0xD2);
        for _ in 0..2 {
            b.push(0x05);
            b.extend([0u8; 16]);
        }
        assert_eq!(opack_shape(&b).as_deref(), Some("{_i: string(11), _x: int, _c: {rate: real, ids: [uuid, …] (2)}}"));
        // A terminated dictionary, a type name, and data with a length byte.
        let mut b = vec![0xEF, 0x44];
        b.extend(b"type");
        b.push(0x48);
        b.extend(b"setRates");
        b.push(0x42);
        b.extend(b"bl");
        b.extend([0x91, 3, 1, 2, 3, 0x03]);
        assert_eq!(opack_shape(&b).as_deref(), Some(r#"{type: "setRates", bl: data(3)}"#));
        assert_eq!(opack_shape(&[0xE1, 0x41]), None, "cut short");
        assert_eq!(opack_shape(&[0x09, 0x09]), None, "two values, not one");
        assert_eq!(opack_shape(&[0xFF]), None);
    }

    #[test]
    fn a_payload_shows_one_level_into_its_data() {
        let bplist = |d: Dictionary| {
            let mut b = Vec::new();
            Value::Dictionary(d).to_writer_binary(&mut b).unwrap();
            b
        };
        let mut inner = Dictionary::new();
        inner.insert("type".into(), Value::String("setRate".into()));
        let inner = bplist(inner);
        // SenderSettingsData's form: OPACK {data, messageType}.
        let mut opack = vec![0xE2, 0x44];
        opack.extend(b"data");
        opack.extend([0x91, u8::try_from(inner.len()).unwrap()]);
        opack.extend(&inner);
        opack.push(0x4B);
        opack.extend(b"messageType");
        opack.push(0x0B);
        assert_eq!(
            payload_shape(&opack),
            format!(
                "{} B opack {{data: data({n}), messageType: int}}; its data: {n} B bplist {{type: \"setRate\"}}",
                opack.len(),
                n = inner.len()
            )
        );
        // The Playback channel's form: POST /command {params: {data}}, here holding an RCS envelope.
        let envelope = crate::datastream::wrap(&inner);
        let mut params = Dictionary::new();
        params.insert("data".into(), Value::Data(envelope.clone()));
        let mut cmd = Dictionary::new();
        cmd.insert("params".into(), Value::Dictionary(params));
        assert_eq!(dict_data(&cmd), Some(envelope.as_slice()));
        let cmd = bplist(cmd);
        assert!(
            payload_shape(&cmd).ends_with(&format!(
                "; its data: an RCS 'asyn' 'cmnd' message, {} B bplist {{type: \"setRate\"}}",
                inner.len()
            )),
            "{}",
            payload_shape(&cmd)
        );
        // No data field: the shape alone.
        assert_eq!(payload_shape(&inner), format!("{} B bplist {{type: \"setRate\"}}", inner.len()));
        let mut top = Dictionary::new();
        top.insert("data".into(), Value::Data(vec![0xFF, 1]));
        assert_eq!(dict_data(&top), Some(&[0xFF, 1][..]), "a top-level data field too");
    }

    #[test]
    fn a_teardown_shows_its_stream_ids() {
        let mut e = Dictionary::new();
        e.insert("type".into(), Value::Integer(130.into()));
        e.insert("streamID".into(), Value::Integer(7.into()));
        e.insert("channelID".into(), Value::String("AA:BB-RCS-2".into()));
        e.insert("note".into(), Value::String("A Private Title".into()));
        let mut screen = Dictionary::new();
        screen.insert("type".into(), Value::Integer(110.into()));
        assert_eq!(
            teardown_streams(&[Value::Dictionary(e), Value::Dictionary(screen)]),
            r#"[{type=130, streamID=7, channelID="AA:BB-RCS-2", note: string(15)}, {type=110}]"#
        );
    }

    #[test]
    fn route_c_is_its_own_device_and_takes_txt_changes() {
        assert_eq!(receiver_device_id("AA:BB:CC:DD:EE:01"), "A8:BB:CC:DD:EE:01");
        assert_eq!(receiver_device_id("not a mac"), "00:00:00:00:00:01");
        let txt = receiver_txt("AA:BB:CC:DD:EE:01", &[]);
        let get = |t: &[(String, String)], k: &str| t.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get(&txt, "deviceid").as_deref(), Some("A8:BB:CC:DD:EE:01"));
        assert_eq!(get(&txt, "features").as_deref(), Some("0x39f7"));
        assert_eq!(txt.len(), 4, "deviceid, features, model, srcvers: the documentation's example");
        let changed = receiver_txt(
            "AA:BB:CC:DD:EE:01",
            &[("features".into(), Some("0x1,0x0".into())), ("model".into(), None), ("vv".into(), Some("2".into()))],
        );
        assert_eq!(get(&changed, "features").as_deref(), Some("0x1,0x0"));
        assert_eq!(get(&changed, "model"), None);
        assert_eq!(get(&changed, "vv").as_deref(), Some("2"));
    }

    #[test]
    fn route_cs_listener_logs_a_request_answers_and_closes() {
        let Some(port) = spawn_receiver_listener() else {
            panic!("no listener");
        };
        let mut c = TcpStream::connect(("::1", port)).or_else(|_| TcpStream::connect(("127.0.0.1", port))).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(b"GET /info RTSP/1.0\r\nCSeq: 1\r\nX-Apple-ProtocolVersion: 1\r\n\r\n").unwrap();
        let mut got = [0u8; 256];
        let n = c.read(&mut got).unwrap();
        let answer = String::from_utf8_lossy(&got[..n]);
        assert!(answer.starts_with("RTSP/1.0 501 Not Implemented\r\nCSeq: 1\r\n"), "{answer}");
    }
}
