//! Device-management control socket — the server half of the AAOS projection app's device screen.
//!
//! ⚠️ **PI-VERIFIED ONLY (2026-08-16). This module is new and runs on the CCPA too.**
//!
//! It was written for and exercised on the Raspberry Pi. `btd` is a shared binary, so
//! a CCPA now also: binds `127.0.0.1:9115` for the process lifetime, and — when a policy is pushed
//! — writes `projection_policy.json` into `ssp_agent::state_dir()`, which on a CCPA is
//! `/etc/carplay`, i.e. **flash**. Only on a user toggle, so a handful of writes per box lifetime,
//! but it is a new flash write on a platform that had none from this crate.
//!
//! Nothing here has run on CCPA hardware. A failed bind is non-fatal by design (logged, device
//! management simply unavailable), so the worst case should be a log line — but "should" is doing
//! work in that sentence.
//!
//! ## Why this exists
//!
//! On the CCPA there was no such surface: the macOS app drove everything over OCBM, and the box that
//! owned the radio was the same box the app talked to. On the Raspberry Pi port the radio owner (this
//! process, which drives `hci0` directly) and the UI (`com.carlink.projection`) are separate
//! processes on one host, and the platform's own Bluetooth settings pane cannot substitute — Android's
//! Bluetooth stack is disabled precisely so this process can own the controller, so that pane has no
//! stack behind it and can neither list nor pair a phone.
//!
//! So the app needs somewhere to ask "what phones do I know, and connect to this one". This is it.
//!
//! ## Protocol
//!
//! Newline-delimited JSON over loopback TCP, one request line and one response line per connection.
//! Line-JSON rather than a binary framing on purpose: this is a low-rate control path, and being able
//! to drive it from `nc` during a bring-up session is worth more than efficiency. Every other seam in
//! this system is binary and awkward to inspect by hand.
//!
//! ```text
//!   {"cmd":"list"}     -> {"ok":true,"devices":[{"address":"AA:BB:CC:DD:EE:FF","name":"","bonded":true,"connected":false}]}
//!   {"cmd":"status"}   -> {"ok":true,"active":false,"address":"","autoConnect":true}
//!   {"cmd":"connect","address":"AA:BB:.."} -> {"ok":true}    // address optional: omit = first-to-connect
//!   {"cmd":"switch","address":"AA:BB:.."}  -> {"ok":true,"ending":true}  // make that bonded phone the
//!                                                            // projecting one: ends another phone's
//!                                                            // session ("ending"), see request_switch
//!   {"cmd":"policy","autoConnect":false,"order":["AA:BB:.."]} -> {"ok":true}
//!   {"cmd":"forget","address":"AA:BB:.."}  -> {"ok":true}
//!   {"cmd":"pair_answer","accept":true}    -> {"ok":true}    // the head unit's yes/no for the
//!                                                            // Numeric-Comparison code on screen
//!   {"cmd":"pair_window","seconds":120}    -> {"ok":true,"seconds":120}  // open the pairing window
//!                                                            // (1..300 s; 0 closes it). Security
//!                                                            // plan 2026-10-21 H1, ssp_agent::PairWindow
//! ```
//!
//! ## Address byte order — the thing that will bite
//!
//! `ssp_agent::bonded_addrs` returns each bdaddr in the stored **mgmt little-endian** order, because
//! that is what `rfcomm::connect_to` and `sdp_client::query` want in their sockaddrs. A human-readable
//! `AA:BB:CC:DD:EE:FF` is the **reverse** of that. Every conversion in this file goes through
//! [`fmt_addr`] / [`parse_addr`], which reverse, and nothing else formats an address — getting this
//! wrong yields a device list that looks plausible and connects to nothing.
//!
//! ## What is deliberately NOT implemented
//!
//! `disconnect` returns an error rather than a lie. Tearing down a live session needs a cancellation
//! hook inside `bt_driver::run`, which does not exist; bolting one on blind would risk the proven
//! session path for a convenience command. The app surfaces the failure rather than showing a button
//! that silently does nothing.
//!
//! Device *names* are not available here. The phone's name arrives in the iAP2 Identify, which
//! `carplayd` sees and this process does not, so `name` is always empty and the app falls back to the
//! address. Plumbing it is a follow-up, not a guess.

// `Read` is needed for `Read::take` on the request reader — without it in scope the
// method call resolves to `Iterator::take` and fails to compile against BufReader.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use box_common::flags::{self, ProjectionOwner};

use crate::ssp_agent;

/// Loopback only. Binding every interface would expose device management to the CarPlay SoftAP —
/// the one network where an untrusted device is by definition present.
pub const CONTROL_PORT: u16 = 9115;

/// Bounds a stuck client so one bad connection cannot hold the single-threaded accept loop.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// A request line longer than this is not a request we understand. Bounded before allocation.
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// How long a switch holds the box for the phone asked for ([`Control::request_switch`]), from the request:
/// the session teardown and Bluetooth bring-up come out of it before the first page. The phone
/// just dropped re-dials within seconds; a bonded phone in range answers a page in one to five. Past
/// this the hold lapses, so a phone that is not in the vehicle cannot lock the box: every phone may
/// connect again, in the usual order.
pub const SWITCH_HOLD: Duration = Duration::from_secs(45);

/// The least a switched-to phone's session gets to reach CarPlay, however late in the hold it started
/// ([`Control::switch_stranded`]). On the 1 Nov bench a paged phone was identified 3.5 s after the page
/// and its SETUP arrived 11 s after it.
const SWITCH_WIFI_FLOOR: Duration = Duration::from_secs(20);

/// How long a switched-to phone that never reached CarPlay stays passed over: last in the call order
/// and kept off the accept path, so the phones that can project get the box first.
const PASS_OVER: Duration = SWITCH_HOLD;

/// Where carplayd names the phone whose SETUP it took (`receiver::session::PHONE_IDENT_FILE`). It is
/// written only for a SETUP the phone gate let through, so a switched-to phone that never appears here
/// never reached CarPlay over Wi-Fi.
const PHONE_IDENT_FILE: &str = "/tmp/phone_identity";

/// Where a switch hold is kept while it lasts ([`Control::keeping_holds_in`]): `<address> <deadline>`, the
/// deadline in milliseconds of `CLOCK_MONOTONIC`, which a restarted btd reads on the same clock. tmpfs, so
/// a reboot forgets it, as it should.
pub const SWITCH_HOLD_FILE: &str = "/tmp/btd_switch_hold";

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [control] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Milliseconds of `CLOCK_MONOTONIC`: system-wide, so a deadline written by one btd means the same to the
/// next, and unmoved by the host's `CT_SETTIME`, which sets the wall clock at every connect.
fn mono_ms() -> u64 {
    // Zeroed, not a literal: the libc crate's 32-bit musl `timespec` can carry padding fields
    // (ocbmd's settime fallback does the same).
    // SAFETY: an all-zero timespec is valid, and `ts` is writable for the duration of the call.
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

/// [`SWITCH_HOLD_FILE`]'s one line for a hold on `addr` that lapses at `deadline_ms` (monotonic).
fn hold_line(
    addr: &[u8; 6],
    deadline_ms: u64,
) -> String {
    format!("{} {deadline_ms}\n", fmt_addr(addr))
}

/// The hold [`hold_line`] wrote, and how much of it is left at `now_ms` (monotonic). `None` when it has
/// lapsed, does not parse, or claims more than a whole [`SWITCH_HOLD`], which no hold this btd wrote can.
fn parse_hold(
    line: &str,
    now_ms: u64,
) -> Option<([u8; 6], Duration)> {
    let mut parts = line.split_whitespace();
    let addr = parse_addr(parts.next()?)?;
    let deadline: u64 = parts.next()?.parse().ok()?;
    let left = Duration::from_millis(deadline.checked_sub(now_ms)?);
    (!left.is_zero() && left <= SWITCH_HOLD).then_some((addr, left))
}

// ---- address formatting ---------------------------------------------------------------------

/// mgmt little-endian bdaddr -> the conventional big-endian display form.
pub fn fmt_addr(a: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        a[5], a[4], a[3], a[2], a[1], a[0]
    )
}

/// Display form -> mgmt little-endian bdaddr. Accepts `:` or `-` separators, any case.
pub fn parse_addr(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        let b = u8::from_str_radix(p, 16).ok()?;
        out[5 - i] = b; // reverse into mgmt order
    }
    Some(out)
}

// ---- minimal JSON ---------------------------------------------------------------------------
//
// Hand-rolled rather than pulling in serde: this crate is deliberately dependency-light (see its
// Cargo.toml), the request shape is fixed and tiny, and a full parser would be more code than the
// server. These readers are intentionally forgiving about whitespace and key order and strict about
// nothing else — a malformed request simply fails to match and yields an error response.

/// Extract a string value for `"key"`. Handles the `\"` escape only; no other escape appears in a
/// bdaddr or a command name, and a value containing one simply fails to match.
fn json_str(src: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)? + pat.len();
    let rest = &src[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let mut chars = after.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for (_, c) in chars {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// Extract a boolean value for `"key"`.
fn json_bool(src: &str, key: &str) -> Option<bool> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)? + pat.len();
    let rest = &src[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    if after.starts_with("true") {
        Some(true)
    } else if after.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Extract a non-negative integer for `"key"`; None when absent or not a plain number.
fn json_u64(src: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)? + pat.len();
    let rest = &src[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 9 {
        return None;
    }
    digits.parse().ok()
}

/// Extract an array of strings for `"key"`.
fn json_str_array(src: &str, key: &str) -> Option<Vec<String>> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)? + pat.len();
    let rest = &src[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    if !after.starts_with('[') {
        return None;
    }
    let end = after.find(']')?;
    let body = &after[1..end];
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for c in body.chars() {
        match (c, cur.as_mut()) {
            ('"', None) => cur = Some(String::new()),
            ('"', Some(_)) => out.push(cur.take().unwrap()),
            (ch, Some(s)) => s.push(ch),
            _ => {}
        }
    }
    Some(out)
}

/// Is `key` present as an object key at all?
///
/// The typed readers cannot tell "the client omitted this field" from "the field is there and
/// malformed" — both are `None` — and those two deserve OPPOSITE answers: keep the current value,
/// versus refuse the request.
fn json_has_key(
    src: &str,
    key: &str,
) -> bool {
    let pat = format!("\"{key}\"");
    match src.find(&pat) {
        Some(i) => src[i + pat.len()..].trim_start().starts_with(':'),
        None => false,
    }
}

/// Escape a string for embedding in our responses.
///
/// Control characters MUST be escaped, not only quote and backslash. The one string we echo back is
/// an unknown `cmd`, and the client is Java's `BufferedReader.readLine()`, which terminates a line
/// on a BARE CR as well as on LF — so a raw CR inside an echoed command cut our response in half and
/// the app saw a JSONException instead of the error we were trying to report.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ---- shared state ---------------------------------------------------------------------------

/// Connection policy, owned here and consulted by `reconnect`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// When false the reconnect loop stays idle unless the app asks for a specific phone —
    /// GM's "tap to connect". When true it drives the order below on its own.
    pub auto_connect: bool,
    /// First-to-connect order. Addresses not in the bond list are ignored; bonded addresses not in
    /// this list are tried after it, so a newly paired phone is never invisible to reconnect.
    pub order: Vec<[u8; 6]>,
}

impl Default for Policy {
    fn default() -> Self {
        // Auto-connect on by default: a head unit that needs a tap before it will talk to an
        // already-paired phone is not what a driver expects, and it is what the stock firmware does.
        Self { auto_connect: true, order: Vec::new() }
    }
}

/// Everything the control socket reads or writes. ONE instance for the PROCESS lifetime — it
/// outlives any single session, because `main`'s claim loop re-enters `run_active_session`.
#[derive(Default)]
pub struct Control {
    policy: RwLock<Policy>,
    /// An explicit connect request from the app, consumed by the reconnect loop.
    ///
    /// `Some(None)` means "connect to whatever is next in the order" — i.e. the app pressed connect
    /// without naming a device. `Some(Some(addr))` names one. `None` means nothing pending.
    #[allow(clippy::option_option)]
    pending: Mutex<Option<Option<[u8; 6]>>>,
    /// The peer of the live session, when it is KNOWN. Only the reconnect path knows the bdaddr —
    /// the accept path is handed an already-open socket — so this stays `None` through an
    /// accept-path session. That is exactly why it must never be the source of truth for "is a
    /// session live".
    session_peer: RwLock<Option<[u8; 6]>>,
    /// The single-session claim, SHARED with `main`'s accept loop and `reconnect`. THIS is what
    /// `active`/`connected` report.
    ///
    /// Deriving `active` from `session_peer` alone made every accept-path session — first pairing
    /// and every phone-initiated connect — report `active:false`, and the app then offered
    /// **Forget** for the phone that was actively projecting, which deletes a live phone's link key.
    session_active: Arc<AtomicBool>,
    /// The head unit's yes/no for a Numeric-Comparison pairing prompt, read by the SSP agent's mgmt
    /// loop. PROCESS-lifetime like the rest of this struct, and cloned into each session's agent
    /// thread — pairing happens on the accept path, before any session exists, so a per-session
    /// answer slot would be created after the prompt it is meant to answer.
    pair_answer: Arc<ssp_agent::PairAnswer>,
    /// The pairing window (security plan H1), PROCESS-lifetime for the same reason as
    /// `pair_answer`: "Add phone" arrives before the phone does. Read by the SSP agent's mgmt loop.
    pair_window: Arc<ssp_agent::PairWindow>,
    /// A switch in progress: the phone asked for, and until when only it may start a session.
    /// Set by [`Control::request_switch`], ended when that phone's session is claimed or it lapses.
    switch_hold: Mutex<Option<([u8; 6], Instant)>>,
    /// The live session must end so a switch can page its phone. `run_active_session` polls it and
    /// leaves through the same go-quiet teardown an arbiter preempt takes.
    restart: AtomicBool,
    /// The switched-to phone's session, until it reaches CarPlay or its deadline passes
    /// ([`Control::switch_stranded`]). Set when that session is claimed.
    switch_watch: Mutex<Option<SwitchWatch>>,
    /// A switched-to phone that never reached CarPlay, and until when it is passed over.
    passed_over: Mutex<Option<([u8; 6], Instant)>>,
    /// Where the switch hold is kept while it lasts ([`Control::keeping_holds_in`]); `None` keeps it in
    /// memory only, as the tests do.
    hold_file: Option<PathBuf>,
}

/// A switched-to phone's session on watch: whose, until when, and what `/tmp/phone_identity` held
/// when it started.
#[derive(Clone, Copy)]
struct SwitchWatch {
    addr: [u8; 6],
    until: Instant,
    before: Ident,
}

/// `/tmp/phone_identity` as last written: when, and the phone its `deviceID` names. `None` = no file.
type Ident = Option<(SystemTime, Option<[u8; 6]>)>;

fn read_ident() -> Ident {
    let written = std::fs::metadata(PHONE_IDENT_FILE).and_then(|m| m.modified()).ok()?;
    let named = std::fs::read_to_string(PHONE_IDENT_FILE).ok().and_then(|s| ident_device(&s));
    Some((written, named))
}

/// Whether carplayd took a SETUP from `w`'s phone since its session started: the identity file was
/// rewritten, and names it. A file that already named it then is a leftover, not proof.
fn carplay_reached(w: &SwitchWatch, now: Ident) -> bool {
    now != w.before && now.is_some_and(|(_, named)| named == Some(w.addr))
}

/// The `deviceID` of carplayd's identity document (hand-built JSON, `publish_phone_identity`).
fn ident_device(json: &str) -> Option<[u8; 6]> {
    let rest = json.split_once("\"deviceID\":\"")?.1;
    parse_addr(rest.split('"').next()?)
}

impl Control {
    /// `session_active` is the SAME `Arc` the accept path and `reconnect` claim.
    pub fn new(session_active: Arc<AtomicBool>) -> Self {
        Self {
            policy: RwLock::new(load_policy()),
            session_active,
            ..Default::default()
        }
    }

    /// Keep a switch hold in `path` while it lasts, and take up the one a previous btd left there.
    ///
    /// Bench 2026-11-02: a switch to the second iPhone was lost when the supervisor restarted the radios
    /// inside the hold. The hold lived only in this process, so the new btd paged the owner's phone first
    /// and admitted it. Kept here, the hold outlives that restart: the new btd pages only the phone asked
    /// for and refuses the others until the same deadline.
    pub fn keeping_holds_in(
        mut self,
        path: &str,
    ) -> Self {
        let file = PathBuf::from(path);
        let left = std::fs::read_to_string(&file).ok().and_then(|s| parse_hold(&s, mono_ms()));
        match left {
            Some((addr, left)) => {
                log(&format!(
                    "switch hold for {} taken up from the btd before this one -- {}s left",
                    fmt_addr(&addr),
                    left.as_secs()
                ));
                if let Ok(mut h) = self.switch_hold.lock() {
                    *h = Some((addr, Instant::now() + left));
                }
                self.request_connect(Some(addr));
            }
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
        self.hold_file = Some(file);
        self
    }

    /// Write the hold on `addr`, lapsing in `left`, to the hold file; or clear the file with `None`.
    fn keep_hold(
        &self,
        hold: Option<([u8; 6], Duration)>,
    ) {
        let Some(file) = &self.hold_file else { return };
        match hold {
            Some((addr, left)) => {
                let deadline = mono_ms() + left.as_millis() as u64;
                let tmp = file.with_extension("tmp");
                if std::fs::write(&tmp, hold_line(&addr, deadline)).is_err() || std::fs::rename(&tmp, file).is_err() {
                    log("switch hold not kept on file -- a btd restart inside it would forget it");
                }
            }
            None => {
                let _ = std::fs::remove_file(file);
            }
        }
    }

    /// The slot the SSP agent polls for the head unit's pairing answer. Clone it into the agent
    /// thread; `handle_request`'s `pair_answer` verb writes it.
    pub fn pair_answer(&self) -> Arc<ssp_agent::PairAnswer> {
        self.pair_answer.clone()
    }

    /// The slot the SSP agent polls for the pairing window; `handle_request`'s `pair_window` writes it.
    pub fn pair_window(&self) -> Arc<ssp_agent::PairWindow> {
        self.pair_window.clone()
    }

    /// True while a session is live on EITHER path, named peer or not.
    pub fn session_active(&self) -> bool {
        self.session_active.load(Ordering::Acquire)
    }

    pub fn policy(&self) -> Policy {
        self.policy.read().map(|p| p.clone()).unwrap_or_default()
    }

    /// Replace the policy; writes `projection_policy.json` only when it actually changed. A CCPA
    /// keeps it on flash, and the projection app re-sends its policy every session (it is the
    /// durable copy), so an unconditional write would cost a flash write per connect.
    pub fn set_policy(&self, p: Policy) {
        let changed = match self.policy.write() {
            Ok(mut w) => {
                let changed = *w != p;
                *w = p.clone();
                changed
            }
            Err(_) => true,
        };
        if changed {
            save_policy(&p);
        }
    }

    /// Take any pending connect request. Called by the reconnect loop each round.
    pub fn take_request(&self) -> Option<Option<[u8; 6]>> {
        self.pending.lock().ok().and_then(|mut p| p.take())
    }

    /// Is a connect request waiting? Peeks rather than takes, so the reconnect loop's backoff sleep
    /// can cut short without consuming the request it is about to act on.
    pub fn has_request(&self) -> bool {
        self.pending.lock().map(|p| p.is_some()).unwrap_or(false)
    }

    /// Make `addr` the projecting phone (MGMT_SWITCH_DEVICE, the app's Connect on a phone while another
    /// one projects). `bonded` is the bond list; an unbonded `addr` is refused (`None`), since it could
    /// never connect and the live session would be ended for nothing.
    ///
    /// With ANOTHER phone's session live this asks for `addr`, holds the box for it ([`SWITCH_HOLD`]),
    /// and flags the session to end (`Some(true)`). The hold is what makes the switch stick: the phone
    /// just dropped re-dials at once and would otherwise win the race. With no session live, or
    /// `addr`'s own, it is a plain connect request (`Some(false)`): nothing is ended.
    ///
    /// btd owns only WIRELESS sessions. A wired phone holds the arbiter, so btd has none live and this
    /// never ends a wired session; the app does not offer it then.
    pub fn request_switch(
        &self,
        addr: [u8; 6],
        bonded: &[[u8; 6]],
    ) -> Option<bool> {
        self.request_switch_at(addr, bonded, Instant::now())
    }

    fn request_switch_at(
        &self,
        addr: [u8; 6],
        bonded: &[[u8; 6]],
        now: Instant,
    ) -> Option<bool> {
        if !bonded.contains(&addr) {
            return None;
        }
        self.request_connect(Some(addr));
        let ending = self.session_active() && self.session_peer() != Some(addr);
        if ending {
            if let Ok(mut h) = self.switch_hold.lock() {
                *h = Some((addr, now + SWITCH_HOLD));
            }
            self.keep_hold(Some((addr, SWITCH_HOLD)));
            self.restart.store(true, Ordering::Release);
        }
        Some(ending)
    }

    /// Whether the live session should end for a switch. Peeks: the session's wait loop polls it.
    pub fn restart_requested(&self) -> bool {
        self.restart.load(Ordering::Acquire)
    }

    /// Clear the restart flag; true when it was set.
    pub fn take_restart(&self) -> bool {
        self.restart.swap(false, Ordering::AcqRel)
    }

    /// The phone a switch holds the box for, while the hold lasts.
    pub fn held_for(&self) -> Option<[u8; 6]> {
        self.held_for_at(Instant::now())
    }

    fn held_for_at(&self, now: Instant) -> Option<[u8; 6]> {
        let mut h = self.switch_hold.lock().ok()?;
        match *h {
            Some((addr, until)) if now < until => Some(addr),
            Some(_) => {
                *h = None;
                self.keep_hold(None);
                log("switch hold lapsed -- every bonded phone may connect again");
                None
            }
            None => None,
        }
    }

    /// May `peer` start a session now? Any phone, except while a switch holds the box for another, or
    /// while `peer` is passed over ([`Control::switch_stranded`]).
    pub fn admits(&self, peer: [u8; 6]) -> bool {
        self.admits_at(peer, Instant::now())
    }

    fn admits_at(&self, peer: [u8; 6], now: Instant) -> bool {
        !matches!(self.held_for_at(now), Some(held) if held != peer)
            && self.passed_over_at(now) != Some(peer)
    }

    /// The phone a switch asked for has its session: the hold has done its job, and the session is
    /// watched until it reaches CarPlay ([`Control::switch_stranded`]).
    fn end_hold_for(&self, peer: Option<[u8; 6]>) {
        self.end_hold_for_at(peer, Instant::now(), read_ident);
    }

    fn end_hold_for_at(
        &self,
        peer: Option<[u8; 6]>,
        now: Instant,
        ident: impl FnOnce() -> Ident,
    ) {
        let Some(p) = peer else { return };
        let Ok(mut h) = self.switch_hold.lock() else { return };
        let Some((addr, until)) = *h else { return };
        if addr != p {
            return;
        }
        *h = None;
        self.keep_hold(None);
        if let Ok(mut w) = self.switch_watch.lock() {
            *w = Some(SwitchWatch {
                addr,
                until: until.max(now + SWITCH_WIFI_FLOOR),
                before: ident(),
            });
        }
    }

    /// Fix 6 (bench 2026-11-01 night): a switch to a phone that never reaches Wi-Fi stranded the box. The
    /// phone came up on Bluetooth ("Identified") and took the Wi-Fi details but never joined the AP, and btd
    /// held its session for 3 minutes with no CarPlay for anyone.
    ///
    /// True once, when the switched-to phone's session is still live past the hold (and at least
    /// [`SWITCH_WIFI_FLOOR`] after it started) and carplayd has not taken a SETUP from it. That phone is
    /// then passed over for [`PASS_OVER`], and the caller ends the session, so the box goes back to its
    /// call order. A wireless Android Auto session counts as projecting: it never writes the CarPlay
    /// identity. Polled by `run_active_session`'s wait loop; reads files only at the deadline.
    pub fn switch_stranded(&self) -> bool {
        self.switch_stranded_at(Instant::now(), |w| {
            flags::owner() == ProjectionOwner::WirelessAa || carplay_reached(w, read_ident())
        })
    }

    fn switch_stranded_at(
        &self,
        now: Instant,
        projecting: impl FnOnce(&SwitchWatch) -> bool,
    ) -> bool {
        let Ok(mut w) = self.switch_watch.lock() else { return false };
        let Some(watch) = *w else { return false };
        if self.session_peer() != Some(watch.addr) {
            *w = None; // that session is over: nothing left to end
            return false;
        }
        if now < watch.until {
            return false;
        }
        *w = None;
        if projecting(&watch) {
            return false;
        }
        log(&format!(
            "switch to {} never reached CarPlay over Wi-Fi -- ending its session; it waits {}s while the other phones are called",
            fmt_addr(&watch.addr),
            PASS_OVER.as_secs()
        ));
        if let Ok(mut p) = self.passed_over.lock() {
            *p = Some((watch.addr, now + PASS_OVER));
        }
        true
    }

    /// The phone [`Control::switch_stranded`] ended, while it is passed over.
    pub fn passed_over(&self) -> Option<[u8; 6]> {
        self.passed_over_at(Instant::now())
    }

    fn passed_over_at(&self, now: Instant) -> Option<[u8; 6]> {
        let mut p = self.passed_over.lock().ok()?;
        match *p {
            Some((addr, until)) if now < until => Some(addr),
            Some(_) => {
                *p = None;
                None
            }
            None => None,
        }
    }

    /// `bonds` in the order a round pages them: as given, with a passed-over phone moved last.
    pub fn call_order(&self, bonds: Vec<[u8; 6]>) -> Vec<[u8; 6]> {
        pass_over_last(bonds, self.passed_over())
    }

    /// A connect request; one naming a passed-over phone ends its pass-over, since the app asked for it.
    pub fn request_connect(&self, addr: Option<[u8; 6]>) {
        if let (Some(a), Ok(mut p)) = (addr, self.passed_over.lock()) {
            if p.is_some_and(|(passed, _)| passed == a) {
                *p = None;
            }
        }
        if let Ok(mut p) = self.pending.lock() {
            *p = Some(addr);
        }
    }

    pub fn set_session_peer(&self, addr: Option<[u8; 6]>) {
        if let Ok(mut w) = self.session_peer.write() {
            *w = addr;
        }
    }

    pub fn session_peer(&self) -> Option<[u8; 6]> {
        self.session_peer.read().ok().and_then(|r| *r)
    }

    /// Bonded phones in the order reconnect should try them.
    pub fn ordered_bonds(&self) -> Vec<[u8; 6]> {
        order_bonds(&ssp_agent::bonded_addrs(), &self.policy().order)
    }
}

/// RAII holder of the single-session claim. Clears the flag AND the published peer on every exit
/// from the claimed region.
///
/// ⚠️ Scope note so nobody over-trusts this: the shipped binaries build `--release` and the
/// workspace sets `panic = "abort"`, so a panic terminates the process WITHOUT unwinding and this
/// `Drop` never runs for one. Its value is structural — the claim and its release cannot drift
/// apart as the block grows, which is what had already happened between the two call sites — plus
/// correctness under `cargo test`. Real panic resilience is a supervisor's job, not a guard's.
pub struct SessionClaim<'a> {
    flag: &'a AtomicBool,
    ctrl: &'a Control,
}

impl<'a> SessionClaim<'a> {
    /// Claim the slot, or `None` if another path owns it. `compare_exchange`, not a store: the
    /// ~16 s SDP+RFCOMM connect in `reconnect::attempt` can overlap an inbound accept.
    pub fn try_claim(
        flag: &'a AtomicBool,
        ctrl: &'a Control,
        peer: Option<[u8; 6]>,
    ) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        ctrl.set_session_peer(peer);
        ctrl.end_hold_for(peer);
        Some(Self { flag, ctrl })
    }
}

impl Drop for SessionClaim<'_> {
    fn drop(&mut self) {
        self.ctrl.set_session_peer(None);
        self.flag.store(false, Ordering::Release);
    }
}

/// First-to-connect ordering: policy order first (for entries that are actually bonded), then every
/// remaining bond in its stored order.
///
/// The tail is the load-bearing half. A strict "policy order only" rule would make a phone paired
/// AFTER the policy was last written invisible to reconnect forever — the same class of bug as the
/// snapshotted bond list that audit Fix #22 removed from `reconnect`.
///
/// Free function so it is testable without touching the on-disk link-key store.
pub fn order_bonds(bonds: &[[u8; 6]], order: &[[u8; 6]]) -> Vec<[u8; 6]> {
    let mut out: Vec<[u8; 6]> = order.iter().copied().filter(|a| bonds.contains(a)).collect();
    for b in bonds {
        if !out.contains(b) {
            out.push(*b);
        }
    }
    out
}

/// `order` with `passed` moved to the end: still paged, if it is the only phone around, but after the
/// rest ([`Control::call_order`]).
fn pass_over_last(mut order: Vec<[u8; 6]>, passed: Option<[u8; 6]>) -> Vec<[u8; 6]> {
    if let Some(i) = passed.and_then(|p| order.iter().position(|a| *a == p)) {
        let a = order.remove(i);
        order.push(a);
    }
    order
}

// ---- policy persistence ---------------------------------------------------------------------

/// The connection policy sits in the SAME directory as the link-key store, via the same helper —
/// not a second `BOX_STATE_DIR` read with its own default.
///
/// They had diverged: this defaulted to `/tmp/carplay` while `ssp_agent` defaulted to
/// `/etc/carplay`, so on a CCPA (where the variable is unset) the bonds were persistent while the
/// policy deciding whether to auto-connect to them sat on tmpfs and vanished every reboot. The Pi
/// sets the variable, so both agreed there and the split was invisible on the only platform anyone
/// had tested.
fn policy_path() -> String {
    format!("{}/projection_policy.json", ssp_agent::state_dir())
}

fn load_policy() -> Policy {
    let Ok(s) = std::fs::read_to_string(policy_path()) else {
        // NO FILE is not corruption — it is a box nobody has configured. Auto-connect on is right
        // there (a head unit that needs a tap before it talks to a paired phone is not what a driver
        // expects) and matches the stock firmware.
        return Policy::default();
    };
    let Some(auto_connect) = json_bool(&s, "autoConnect") else {
        // The file EXISTS and does not parse: the user's setting is LOST. Guess in the direction
        // that cannot page a phone they asked us to leave alone. `unwrap_or(true)` inverted an
        // explicit "off" on every corrupt read — silently, and in the one direction with a
        // real-world consequence.
        log("policy file is unparseable — assuming autoConnect OFF until the app pushes a new one");
        return Policy { auto_connect: false, order: Vec::new() };
    };
    let order = json_str_array(&s, "order")
        .unwrap_or_default()
        .iter()
        .filter_map(|a| parse_addr(a))
        .collect();
    Policy { auto_connect, order }
}

fn save_policy(p: &Policy) {
    let order: Vec<String> = p.order.iter().map(|a| format!("\"{}\"", fmt_addr(a))).collect();
    let body = format!(
        "{{\"autoConnect\":{},\"order\":[{}]}}",
        p.auto_connect,
        order.join(",")
    );
    let path = policy_path();
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Atomic: the app may read this while we write, and a torn document reads as "defaults".
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, &body).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        return;
    }
    let _ = std::fs::write(&path, &body);
}

// ---- request handling -----------------------------------------------------------------------

/// Build a response for one request line. Pure, so it is fully testable without a socket.
pub fn handle_request(ctrl: &Control, line: &str) -> String {
    let Some(cmd) = json_str(line, "cmd") else {
        return r#"{"ok":false,"error":"no cmd"}"#.to_string();
    };
    match cmd.as_str() {
        "list" => {
            let peer = ctrl.session_peer();
            let active = ctrl.session_active();
            let items: Vec<String> = ctrl
                .ordered_bonds()
                .iter()
                .map(|a| {
                    format!(
                        r#"{{"address":"{}","name":"{}","bonded":true,"connected":{}}}"#,
                        fmt_addr(a),
                        // Always empty for now — see the module doc. The app falls back to the
                        // address rather than showing a placeholder that looks like a real name.
                        json_escape(""),
                        // Only a NAMED peer marks a row connected. An accept-path session is live
                        // with no bdaddr, so no row claims it and `status.active` is how the app
                        // learns a session exists at all. Do NOT "fix" this by marking every bond
                        // connected — that is a lie the UI would act on by offering Forget.
                        active && peer == Some(*a)
                    )
                })
                .collect();
            format!(r#"{{"ok":true,"devices":[{}]}}"#, items.join(","))
        }
        "switch" => match json_str(line, "address").as_deref().and_then(parse_addr) {
            Some(a) => match ctrl.request_switch(a, &ssp_agent::bonded_addrs()) {
                Some(ending) => {
                    if ending {
                        log(&format!(
                            "switch to {} requested -- ending the live session, holding the box for it {}s",
                            fmt_addr(&a),
                            SWITCH_HOLD.as_secs()
                        ));
                    } else {
                        log(&format!("switch to {} requested -- no other session live, connecting", fmt_addr(&a)));
                    }
                    format!(r#"{{"ok":true,"ending":{ending}}}"#)
                }
                None => r#"{"ok":false,"error":"not bonded"}"#.to_string(),
            },
            None => r#"{"ok":false,"error":"bad address"}"#.to_string(),
        },
        "status" => {
            let peer = ctrl.session_peer();
            format!(
                r#"{{"ok":true,"active":{},"address":"{}","name":"","autoConnect":{}}}"#,
                ctrl.session_active(),
                peer.map(|a| fmt_addr(&a)).unwrap_or_default(),
                ctrl.policy().auto_connect
            )
        }
        "connect" => {
            let addr = json_str(line, "address");
            match addr {
                Some(s) => match parse_addr(&s) {
                    Some(a) => {
                        ctrl.request_connect(Some(a));
                        log(&format!("connect requested for {}", fmt_addr(&a)));
                        r#"{"ok":true}"#.to_string()
                    }
                    None => r#"{"ok":false,"error":"bad address"}"#.to_string(),
                },
                None => {
                    ctrl.request_connect(None);
                    log("connect requested (first-to-connect)");
                    r#"{"ok":true}"#.to_string()
                }
            }
        }
        "policy" => {
            // FAIL CLOSED, and distinguish absent from malformed.
            //
            // `unwrap_or(true)` turned a malformed request into "auto-connect ON" — the opposite of
            // the only setting a user bothers to send, in the direction that pages a phone. And
            // `order` falling back to empty is worse on THIS path than on load, because this one
            // SAVES: a malformed order would overwrite the stored one with nothing.
            let auto_connect = match json_bool(line, "autoConnect") {
                Some(v) => v,
                None if !json_has_key(line, "autoConnect") => ctrl.policy().auto_connect,
                None => {
                    return r#"{"ok":false,"error":"policy: autoConnect must be true or false"}"#
                        .to_string()
                }
            };
            let order = match json_str_array(line, "order") {
                Some(v) => v.iter().filter_map(|a| parse_addr(a)).collect(),
                None if !json_has_key(line, "order") => ctrl.policy().order,
                None => {
                    return r#"{"ok":false,"error":"policy: order must be an array of addresses"}"#
                        .to_string()
                }
            };
            let p = Policy { auto_connect, order };
            log(&format!(
                "policy: autoConnect={} order={} device(s)",
                p.auto_connect,
                p.order.len()
            ));
            ctrl.set_policy(p);
            r#"{"ok":true}"#.to_string()
        }
        // The user's answer to the Numeric-Comparison prompt (macOS app → CT_PAIR_CONFIRM → ocbmd →
        // here). FAIL CLOSED, like `policy`: a malformed or absent `accept` is refused, never read
        // as "pair" — an unparseable request must not be able to complete a bond nobody confirmed.
        //
        // Answering is always `{"ok":true}` even with no prompt outstanding: this port cannot see
        // whether the agent still has one pending (it may have just timed out), and reporting an
        // error for a race the caller cannot avoid would only teach the app to ignore the field.
        // The agent drops a stray answer rather than banking it.
        "pair_answer" => match json_bool(line, "accept") {
            Some(accept) => {
                ctrl.pair_answer.set(accept);
                log(&format!(
                    "pairing answer from the head unit: {}",
                    if accept { "PAIR" } else { "CANCEL" }
                ));
                r#"{"ok":true}"#.to_string()
            }
            None => r#"{"ok":false,"error":"pair_answer: accept must be true or false"}"#.to_string(),
        },
        // "Add phone" (app -> MGMT_PAIR_WINDOW -> ocbmd -> here). The agent applies it within a
        // second. FAIL CLOSED: an absent or malformed `seconds` opens nothing.
        "pair_window" => match json_u64(line, "seconds") {
            Some(secs) => {
                let granted = ctrl.pair_window.set(secs, now_ms());
                log(&format!(
                    "pairing window {}",
                    if granted == 0 { "closed by the app".to_string() } else { format!("opened by the app for {granted} s") }
                ));
                format!(r#"{{"ok":true,"seconds":{granted}}}"#)
            }
            None => r#"{"ok":false,"error":"pair_window: seconds must be a whole number"}"#.to_string(),
        },
        "forget" => match json_str(line, "address").as_deref().and_then(parse_addr) {
            Some(a) => {
                if ssp_agent::forget_bond(&a) {
                    log(&format!("forgot {}", fmt_addr(&a)));
                    r#"{"ok":true}"#.to_string()
                } else {
                    r#"{"ok":false,"error":"not bonded"}"#.to_string()
                }
            }
            None => r#"{"ok":false,"error":"bad address"}"#.to_string(),
        },
        // Honest refusal rather than a no-op that reports success. See the module doc.
        "disconnect" => {
            r#"{"ok":false,"error":"unsupported: no session cancellation hook in bt_driver"}"#
                .to_string()
        }
        other => format!(r#"{{"ok":false,"error":"unknown cmd {}"}}"#, json_escape(other)),
    }
}

// ---- server ---------------------------------------------------------------------------------

/// Bind ONCE, for the process lifetime. Spawns its own thread and returns immediately.
///
/// Called from `main` BEFORE the claim loop, never from `run_active_session`, and that is
/// load-bearing. `run_active_session` is re-entered on every arbiter preempt/re-claim; a bind per
/// entry left the PREVIOUS listener owning the port forever — `for stream in listener.incoming()`
/// blocks in `accept()`, and a shutdown check there only runs once a connection arrives. The second
/// bind then failed EADDRINUSE and every request landed on an ORPHANED `Control`: `connect` queued
/// into a `pending` nobody drained, `status`/`list` read state nothing updated. All silently, with
/// every response still `{"ok":true}`.
///
/// Second reason, and this one bites today: the old call site sat after `bt_bringup::bring_up`, so
/// while bring-up was failing and retrying the socket was never bound at all and the app could not
/// even list or forget a phone. Binding here also keeps device management alive while WIRED holds
/// the arbiter — exactly when a driver goes looking at the screen.
///
/// The listener dies with the process, so there is deliberately no shutdown observation. Do not
/// reintroduce a per-session bind to make one meaningful.
pub fn serve(ctrl: Arc<Control>) {
    std::thread::spawn(move || {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, CONTROL_PORT);
        let listener = match TcpListener::bind(addr) {
            Ok(l) => l,
            Err(e) => {
                // Non-fatal by design: the accessory stack works without a UI, and refusing to
                // start the whole daemon because a management port is taken would be a worse
                // failure than losing the device screen.
                log(&format!("bind 127.0.0.1:{CONTROL_PORT} failed: {e} — device management unavailable"));
                return;
            }
        };
        log(&format!("device management on 127.0.0.1:{CONTROL_PORT}"));
        for stream in listener.incoming() {
            match stream {
                Ok(s) => serve_one(&ctrl, s),
                Err(e) => log(&format!("accept failed: {e}")),
            }
        }
    });
}

/// One request, one response, close. Served inline rather than on a thread per connection: requests
/// are trivial and bounded by CLIENT_TIMEOUT, and a thread per connection would let a misbehaving
/// client spawn without limit.
fn serve_one(ctrl: &Control, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    // take() bounds the read BEFORE allocation — an endless line from a hostile or broken client
    // would otherwise grow `line` without limit.
    let mut limited = (&mut reader).take(MAX_REQUEST_BYTES as u64);
    if limited.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let resp = handle_request(ctrl, line.trim());
    let mut out = stream;
    let _ = out.write_all(resp.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_round_trips_through_the_display_form() {
        // mgmt little-endian: the display form is the REVERSE. This test is the guard on the one
        // conversion that silently produces a plausible-but-wrong device list.
        let mgmt = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66];
        assert_eq!(fmt_addr(&mgmt), "66:55:44:33:22:11");
        assert_eq!(parse_addr("66:55:44:33:22:11"), Some(mgmt));
    }

    #[test]
    fn parse_addr_accepts_dashes_and_lowercase_and_rejects_junk() {
        assert_eq!(parse_addr("aa-bb-cc-dd-ee-ff"), parse_addr("AA:BB:CC:DD:EE:FF"));
        assert_eq!(parse_addr("AA:BB:CC:DD:EE"), None);
        assert_eq!(parse_addr("ZZ:BB:CC:DD:EE:FF"), None);
        assert_eq!(parse_addr(""), None);
    }

    #[test]
    fn json_readers_extract_the_fields_we_use() {
        let s = r#"{"cmd":"policy", "autoConnect": false ,"order":["AA:BB:CC:DD:EE:FF","11:22:33:44:55:66"]}"#;
        assert_eq!(json_str(s, "cmd").as_deref(), Some("policy"));
        assert_eq!(json_bool(s, "autoConnect"), Some(false));
        assert_eq!(
            json_str_array(s, "order"),
            Some(vec![
                "AA:BB:CC:DD:EE:FF".to_string(),
                "11:22:33:44:55:66".to_string()
            ])
        );
        assert_eq!(json_str(s, "missing"), None);
        assert_eq!(json_bool(s, "missing"), None);
    }

    #[test]
    fn an_echoed_command_can_never_terminate_the_response_line() {
        // Java's BufferedReader.readLine() splits on a bare CR too, so an unescaped one truncates
        // the JSON the app is mid-parse on and it reports a JSONException instead of our error.
        let c = Control::default();
        let r = handle_request(&c, "{\"cmd\":\"a\rb\"}");
        assert!(!r.contains('\r') && !r.contains('\n'), "raw control char in response: {r:?}");
        assert!(r.contains("\\r"), "CR should be escaped: {r}");
    }

    /// `has_request` exists so the reconnect backoff sleep can cut short on a tap; it must PEEK,
    /// or the request it woke for would be gone by the time the loop reads it.
    #[test]
    fn has_request_peeks_and_take_request_consumes() {
        let c = Control::default();
        assert!(!c.has_request());
        c.request_connect(Some([1, 2, 3, 4, 5, 6]));
        assert!(c.has_request());
        assert!(c.has_request(), "peeking must not consume");
        assert_eq!(c.take_request(), Some(Some([1, 2, 3, 4, 5, 6])));
        assert!(!c.has_request());
    }

    #[test]
    fn pair_window_opens_closes_and_fails_closed() {
        let ctrl = Control::new(Arc::new(AtomicBool::new(false)));
        assert_eq!(handle_request(&ctrl, r#"{"cmd":"pair_window","seconds":120}"#), r#"{"ok":true,"seconds":120}"#);
        assert!(ctrl.pair_window().is_open(now_ms()));
        assert_eq!(handle_request(&ctrl, r#"{"cmd":"pair_window","seconds":9000}"#), r#"{"ok":true,"seconds":300}"#);
        assert_eq!(handle_request(&ctrl, r#"{"cmd":"pair_window","seconds":0}"#), r#"{"ok":true,"seconds":0}"#);
        assert!(!ctrl.pair_window().is_open(now_ms()));
        for bad in [r#"{"cmd":"pair_window"}"#, r#"{"cmd":"pair_window","seconds":"120"}"#, r#"{"cmd":"pair_window","seconds":-5}"#] {
            assert!(handle_request(&ctrl, bad).contains(r#""ok":false"#), "{bad}");
        }
        assert!(!ctrl.pair_window().is_open(now_ms()), "a refused request opens nothing");
    }

    #[test]
    fn json_str_handles_an_escaped_quote() {
        assert_eq!(json_str(r#"{"n":"a\"b"}"#, "n").as_deref(), Some("a\"b"));
    }

    #[test]
    fn unknown_and_malformed_requests_are_refused_not_ignored() {
        let c = Control::default();
        assert!(handle_request(&c, "not json").contains("\"ok\":false"));
        assert!(handle_request(&c, r#"{"cmd":"nope"}"#).contains("unknown cmd nope"));
        // A refusal, never a silent success — the app shows the failure.
        assert!(handle_request(&c, r#"{"cmd":"disconnect"}"#).contains("\"ok\":false"));
    }

    #[test]
    fn a_malformed_policy_request_is_refused_and_changes_nothing() {
        let c = Control::default();
        let before = c.policy();
        let r = handle_request(&c, r#"{"cmd":"policy","autoConnect":"maybe","order":[]}"#);
        assert!(r.contains("\"ok\":false"), "{r}");
        assert_eq!(c.policy(), before, "a refused request must not mutate the policy");
    }

    #[test]
    fn a_policy_request_that_omits_a_field_leaves_it_alone() {
        // The app's auto-connect switch sends ONLY autoConnect, because it does not manage the
        // first-to-connect order. An omitted field must therefore keep its stored value — sending
        // an empty array would wipe an order the UI never showed the user.
        let c = Control::default();
        c.set_policy(Policy { auto_connect: true, order: vec![[1, 0, 0, 0, 0, 0]] });
        assert!(handle_request(&c, r#"{"cmd":"policy","autoConnect":false}"#).contains("\"ok\":true"));
        assert!(!c.policy().auto_connect);
        assert_eq!(c.policy().order.len(), 1, "an omitted order must not wipe the stored one");
    }

    #[test]
    fn json_has_key_distinguishes_absent_from_malformed() {
        assert!(json_has_key(r#"{"autoConnect":false}"#, "autoConnect"));
        assert!(json_has_key(r#"{"autoConnect" : "bad"}"#, "autoConnect"));
        assert!(!json_has_key(r#"{"order":[]}"#, "autoConnect"));
    }

    #[test]
    fn connect_without_an_address_means_first_to_connect() {
        let c = Control::default();
        assert!(handle_request(&c, r#"{"cmd":"connect"}"#).contains("\"ok\":true"));
        // Some(None): a request is pending, naming no particular device.
        assert_eq!(c.take_request(), Some(None));
        // Consumed exactly once.
        assert_eq!(c.take_request(), None);
    }

    #[test]
    fn connect_with_an_address_parses_into_mgmt_order() {
        let c = Control::default();
        assert!(handle_request(&c, r#"{"cmd":"connect","address":"66:55:44:33:22:11"}"#)
            .contains("\"ok\":true"));
        assert_eq!(c.take_request(), Some(Some([0x11, 0x22, 0x33, 0x44, 0x55, 0x66])));
    }

    const PHONE_A: [u8; 6] = [0x10, 0x25, 0x00, 0x29, 0xA7, 0x68];
    const PHONE_B: [u8; 6] = [0xDE, 0x6A, 0x00, 0x00, 0x00, 0x00];

    /// A Control with `peer`'s session live, as `SessionClaim` leaves it.
    fn live(peer: [u8; 6]) -> Control {
        let c = Control::new(Arc::new(AtomicBool::new(false)));
        c.session_active.store(true, Ordering::Release);
        c.set_session_peer(Some(peer));
        c
    }

    #[test]
    fn a_switch_ends_another_phones_session_and_holds_the_box_for_the_one_asked_for() {
        // Owner 2026-11-01: a second iPhone connected and the owner's could not get in.
        let c = live(PHONE_B);
        let now = Instant::now();
        assert_eq!(c.request_switch_at(PHONE_A, &[PHONE_A, PHONE_B], now), Some(true));
        assert!(c.restart_requested(), "the live session is told to end");
        assert_eq!(c.take_request(), Some(Some(PHONE_A)), "the phone asked for is paged first");
        assert!(!c.admits_at(PHONE_B, now + Duration::from_secs(1)), "the dropped phone may not re-dial in");
        assert!(c.admits_at(PHONE_A, now + Duration::from_secs(1)));
        assert!(c.admits_at(PHONE_B, now + SWITCH_HOLD), "a phone not in the vehicle cannot lock the box");
        assert_eq!(c.held_for_at(now + SWITCH_HOLD), None);
        assert!(c.take_restart());
        assert!(!c.take_restart(), "taken once");
    }

    #[test]
    fn the_hold_ends_once_the_phone_asked_for_has_its_session() {
        let flag = AtomicBool::new(false);
        let c = live(PHONE_B);
        c.request_switch_at(PHONE_A, &[PHONE_A, PHONE_B], Instant::now());
        c.session_active.store(false, Ordering::Release);
        c.set_session_peer(None);
        let claim = SessionClaim::try_claim(&flag, &c, Some(PHONE_A));
        assert!(claim.is_some());
        assert_eq!(c.held_for(), None);
        drop(claim);
        assert!(c.admits(PHONE_B), "after the switched-to session, every phone again");
    }

    /// A Control after a switch to PHONE_A at `t0` whose session started at `claimed`, with `before`
    /// in `/tmp/phone_identity` then.
    fn switched(t0: Instant, claimed: Instant, before: Ident) -> Control {
        let c = live(PHONE_B);
        c.request_switch_at(PHONE_A, &[PHONE_A, PHONE_B], t0);
        c.set_session_peer(Some(PHONE_A));
        c.end_hold_for_at(Some(PHONE_A), claimed, || before);
        c
    }

    fn ident(written: u64, phone: [u8; 6]) -> Ident {
        Some((SystemTime::UNIX_EPOCH + Duration::from_secs(written), Some(phone)))
    }

    /// `switch_stranded` at `when`, with `now` in `/tmp/phone_identity` and no Android Auto session.
    fn stranded(c: &Control, when: Instant, now: Ident) -> bool {
        c.switch_stranded_at(when, |w| carplay_reached(w, now))
    }

    #[test]
    fn a_switched_to_phone_projecting_android_auto_is_left_alone() {
        let t0 = Instant::now();
        let c = switched(t0, t0 + Duration::from_secs(6), None);
        assert!(!c.switch_stranded_at(t0 + SWITCH_HOLD, |_| true));
        assert_eq!(c.passed_over(), None);
    }

    #[test]
    fn a_switched_to_phone_that_never_reaches_carplay_is_ended_after_the_hold_and_passed_over() {
        // Bench 2026-11-01 night: the second iPhone took the Wi-Fi details, never joined the AP, and
        // held the box for 3 minutes.
        let t0 = Instant::now();
        let c = switched(t0, t0 + Duration::from_secs(6), ident(100, PHONE_B));
        let end = t0 + SWITCH_HOLD;
        assert!(!stranded(&c, end - Duration::from_secs(1), ident(100, PHONE_B)), "not before the hold ends");
        assert!(stranded(&c, end, ident(100, PHONE_B)));
        assert!(!stranded(&c, end, ident(100, PHONE_B)), "once");
        assert!(!c.admits_at(PHONE_A, end + Duration::from_secs(1)), "it may not dial straight back in");
        assert!(c.admits_at(PHONE_B, end + Duration::from_secs(1)));
        assert_eq!(c.call_order(vec![PHONE_A, PHONE_B]), vec![PHONE_B, PHONE_A], "still paged, but last");
        assert!(c.admits_at(PHONE_A, end + PASS_OVER), "passed over for a while, not for good");
        assert_eq!(c.call_order(vec![PHONE_A, PHONE_B]), vec![PHONE_A, PHONE_B]);
    }

    #[test]
    fn a_switched_to_phone_whose_setup_arrived_is_left_alone() {
        let t0 = Instant::now();
        let c = switched(t0, t0 + Duration::from_secs(6), ident(100, PHONE_B));
        assert!(!stranded(&c, t0 + SWITCH_HOLD, ident(117, PHONE_A)));
        assert_eq!(c.passed_over(), None);
        assert!(!stranded(&c, t0 + SWITCH_HOLD * 2, None), "watched once");
    }

    #[test]
    fn an_identity_naming_the_phone_from_before_its_session_is_not_proof() {
        let t0 = Instant::now();
        let c = switched(t0, t0 + Duration::from_secs(6), ident(100, PHONE_A));
        assert!(stranded(&c, t0 + SWITCH_HOLD, ident(100, PHONE_A)));
    }

    #[test]
    fn a_session_that_ends_on_its_own_is_not_ended_again() {
        let t0 = Instant::now();
        let c = switched(t0, t0 + Duration::from_secs(6), None);
        c.set_session_peer(None);
        assert!(!stranded(&c, t0 + SWITCH_HOLD, None));
        c.set_session_peer(Some(PHONE_A));
        assert!(!stranded(&c, t0 + SWITCH_HOLD, None), "the watch went with that session");
    }

    #[test]
    fn a_session_that_starts_late_in_the_hold_still_gets_the_floor() {
        let t0 = Instant::now();
        let claimed = t0 + SWITCH_HOLD - Duration::from_secs(2);
        let c = switched(t0, claimed, None);
        assert!(!stranded(&c, t0 + SWITCH_HOLD, None));
        assert!(stranded(&c, claimed + SWITCH_WIFI_FLOOR, None));
    }

    #[test]
    fn asking_for_a_passed_over_phone_ends_its_pass_over() {
        let t0 = Instant::now();
        let c = switched(t0, t0, None);
        assert!(stranded(&c, t0 + SWITCH_HOLD, None));
        assert_eq!(c.passed_over(), Some(PHONE_A));
        c.request_connect(Some(PHONE_A));
        assert_eq!(c.passed_over(), None);
    }

    /// A hold file of this test's own, so parallel tests never share one.
    fn hold_file(test: &str) -> String {
        let p = std::env::temp_dir().join(format!("btd_switch_hold_{test}_{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn a_switch_hold_outlives_a_btd_restart() {
        // Bench 2026-11-02: the supervisor restarted the radios inside a switch's hold, and the new btd
        // paged and admitted the owner's phone instead of the one asked for.
        let path = hold_file("restart");
        let before = live(PHONE_B).keeping_holds_in(&path);
        before.request_switch_at(PHONE_A, &[PHONE_A, PHONE_B], Instant::now());
        drop(before);

        let after = Control::new(Arc::new(AtomicBool::new(false))).keeping_holds_in(&path);
        assert_eq!(after.held_for(), Some(PHONE_A), "the restarted btd holds the box for the same phone");
        assert!(!after.admits(PHONE_B), "and the dropped phone still may not re-dial in");
        assert_eq!(after.take_request(), Some(Some(PHONE_A)), "the phone asked for is paged first");

        let flag = AtomicBool::new(false);
        let claim = SessionClaim::try_claim(&flag, &after, Some(PHONE_A));
        assert!(claim.is_some());
        assert!(!std::path::Path::new(&path).exists(), "the hold is over once that phone has its session");
        drop(claim);
        let again = Control::new(Arc::new(AtomicBool::new(false))).keeping_holds_in(&path);
        assert_eq!(again.held_for(), None);
    }

    #[test]
    fn a_lapsed_or_unreadable_hold_file_holds_nothing() {
        let line = hold_line(&PHONE_A, 50_000);
        assert_eq!(parse_hold(&line, 40_000), Some((PHONE_A, Duration::from_secs(10))));
        assert_eq!(parse_hold(&line, 50_000), None, "lapsed at its deadline");
        assert_eq!(parse_hold(&line, 60_000), None);
        assert_eq!(parse_hold(&hold_line(&PHONE_A, 46_001), 1_000), None, "more than a whole hold is not ours");
        assert_eq!(parse_hold("68:A7:29:00:25:10", 0), None, "no deadline");
        assert_eq!(parse_hold("not-an-address 5000", 0), None);
        assert_eq!(parse_hold("", 0), None);

        let path = hold_file("lapsed");
        std::fs::write(&path, hold_line(&PHONE_A, mono_ms().saturating_sub(1))).unwrap();
        let c = Control::new(Arc::new(AtomicBool::new(false))).keeping_holds_in(&path);
        assert_eq!(c.held_for(), None);
        assert_eq!(c.take_request(), None, "nothing is paged for a hold that has lapsed");
        assert!(!std::path::Path::new(&path).exists(), "and its file is cleared");
    }

    #[test]
    fn the_identity_document_names_its_phone() {
        let json = r#"{"name":"Owner\"s iPhone","deviceID":"68:A7:29:00:25:10","model":"iPhone17,1","osName":"iPhone OS","osVersion":"27.1"}"#;
        assert_eq!(ident_device(json), Some(PHONE_A));
        assert_eq!(ident_device(r#"{"name":"x","deviceID":"","model":""}"#), None);
        assert_eq!(ident_device("{}"), None);
    }

    #[test]
    fn a_switch_with_no_other_session_is_a_plain_connect() {
        let idle = Control::new(Arc::new(AtomicBool::new(false)));
        assert_eq!(idle.request_switch_at(PHONE_A, &[PHONE_A], Instant::now()), Some(false));
        assert!(!idle.restart_requested());
        assert_eq!(idle.held_for(), None);
        assert_eq!(idle.take_request(), Some(Some(PHONE_A)));

        let same = live(PHONE_A);
        assert_eq!(same.request_switch_at(PHONE_A, &[PHONE_A], Instant::now()), Some(false), "already projecting");
        assert!(!same.restart_requested());
    }

    #[test]
    fn a_switch_to_an_unbonded_phone_ends_nothing() {
        let c = live(PHONE_B);
        assert_eq!(c.request_switch_at(PHONE_A, &[PHONE_B], Instant::now()), None);
        assert!(!c.restart_requested());
        assert_eq!(c.take_request(), None);
        // Over the socket: no link keys in a test run, so any address is unbonded.
        assert!(handle_request(&c, r#"{"cmd":"switch","address":"68:A7:29:00:25:10"}"#).contains("not bonded"));
        assert!(handle_request(&c, r#"{"cmd":"switch","address":"nope"}"#).contains("bad address"));
        assert!(handle_request(&c, r#"{"cmd":"switch"}"#).contains("\"ok\":false"));
        assert!(!c.restart_requested());
    }

    #[test]
    fn connect_with_a_bad_address_does_not_queue_anything() {
        let c = Control::default();
        assert!(handle_request(&c, r#"{"cmd":"connect","address":"nope"}"#).contains("\"ok\":false"));
        assert_eq!(c.take_request(), None);
    }

    #[test]
    fn a_pair_answer_reaches_the_slot_the_ssp_agent_polls() {
        let c = Control::new(Arc::new(AtomicBool::new(false)));
        let slot = c.pair_answer();
        assert!(handle_request(&c, r#"{"cmd":"pair_answer","accept":true}"#).contains("\"ok\":true"));
        assert_eq!(slot.take(), Some(true));
        assert!(handle_request(&c, r#"{"cmd":"pair_answer","accept":false}"#).contains("\"ok\":true"));
        assert_eq!(slot.take(), Some(false));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn a_malformed_pair_answer_is_refused_and_confirms_nothing() {
        // The failure that matters: junk being read as "pair" would complete a bond no human
        // approved, which is the whole reason this path exists.
        let c = Control::new(Arc::new(AtomicBool::new(false)));
        let slot = c.pair_answer();
        assert!(handle_request(&c, r#"{"cmd":"pair_answer","accept":"yes"}"#).contains("\"ok\":false"));
        assert!(handle_request(&c, r#"{"cmd":"pair_answer"}"#).contains("\"ok\":false"));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn status_reports_the_live_peer() {
        // The claim, not the peer, is what makes a session "active" — see the test below for why.
        let flag = Arc::new(AtomicBool::new(false));
        let c = Control::new(flag.clone());
        assert!(handle_request(&c, r#"{"cmd":"status"}"#).contains(r#""active":false"#));
        flag.store(true, Ordering::Release);
        c.set_session_peer(Some([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]));
        let r = handle_request(&c, r#"{"cmd":"status"}"#);
        assert!(r.contains(r#""active":true"#));
        assert!(r.contains("66:55:44:33:22:11"));
    }

    /// REGRESSION: an accept-path session is live with NO peer, and `active` must still be true.
    ///
    /// Deriving `active` from `session_peer` alone reported false for every accept-path session —
    /// first pairing and every phone-initiated connect — and the device screen then rendered that
    /// row as merely "Paired", offering **Forget**, which deletes the link key of the phone that is
    /// currently projecting.
    #[test]
    fn status_is_active_during_a_session_with_no_named_peer() {
        let flag = Arc::new(AtomicBool::new(false));
        let c = Control::new(flag.clone());
        flag.store(true, Ordering::Release);
        let r = handle_request(&c, r#"{"cmd":"status"}"#);
        assert!(r.contains(r#""active":true"#), "{r}");
        assert!(r.contains(r#""address":"""#), "no peer is known, so address stays empty: {r}");
    }

    /// The claim is released on EVERY exit from the guarded region, and a second claimer is refused
    /// while it is held.
    #[test]
    fn session_claim_releases_on_drop_and_refuses_a_second_claimer() {
        let flag = AtomicBool::new(false);
        let c = Control::default();
        {
            let _claim = SessionClaim::try_claim(&flag, &c, Some([1, 0, 0, 0, 0, 0])).unwrap();
            assert!(flag.load(Ordering::Acquire));
            assert_eq!(c.session_peer(), Some([1, 0, 0, 0, 0, 0]));
            assert!(
                SessionClaim::try_claim(&flag, &c, None).is_none(),
                "two concurrent bt_driver sessions against one phone must be impossible",
            );
        }
        assert!(!flag.load(Ordering::Acquire), "claim not released");
        assert_eq!(c.session_peer(), None, "peer not cleared — the screen would name a dead session");
    }

    #[test]
    fn policy_order_is_honoured_but_never_hides_a_new_bond() {
        // ordered_bonds() consults ssp_agent, so this exercises the pure ordering rule with an
        // explicit bond list instead.
        let ordered = order_bonds(
            &[[1, 0, 0, 0, 0, 0], [2, 0, 0, 0, 0, 0], [3, 0, 0, 0, 0, 0]],
            &[[3, 0, 0, 0, 0, 0], [9, 0, 0, 0, 0, 0]],
        );
        // 3 first (policy), then the rest in bond order. 9 is not bonded and is dropped; 1 and 2
        // are bonded but unlisted and MUST still appear, or a phone paired after the policy was
        // written would never be tried.
        assert_eq!(
            ordered,
            vec![[3, 0, 0, 0, 0, 0], [1, 0, 0, 0, 0, 0], [2, 0, 0, 0, 0, 0]]
        );
    }

}
