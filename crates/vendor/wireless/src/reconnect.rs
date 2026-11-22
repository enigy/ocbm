//! Model-B accessory-initiated reconnect (docs/wireless/01_BT_AND_RADIO.md): on boot, drive a bonded iPhone back into wireless
//! CarPlay with no user interaction — the behavior native head units and the stock Carlinkit firmware
//! both have. Purely additive to the accept path (`rfcomm::accept_one` in `main.rs`), which stays the
//! fallback for first-time pairing and any phone-initiated connect.
//!
//! Per the stock firmware (docs/wireless/01_BT_AND_RADIO.md §"What the working implementations do"): page the bonded phone,
//! SDP-*query* it for its iAP2 RFCOMM channel, then become the RFCOMM CLIENT and open the iAP2 channel
//! TO the phone. Steps here:
//!   1. `sdp_client::query`  — L2CAP-connect to the phone's SDP PSM (implicitly pages it) and read the
//!      iAP2 RFCOMM channel. This is also the on-hardware probe for docs/wireless/01_BT_AND_RADIO.md's open unknown (does iOS
//!      expose the service on reconnect?).
//!   2. `rfcomm::connect_to` — RFCOMM-connect OUT to that channel.
//!   3. `bt_driver::run`     — the existing, unchanged iAP2 Identify → auth → WiFi-handoff driver.
//!
//! Only ever attempts while no session is live (`session_active`), so it never fights the accept path
//! or a session already in progress. Bounded backoff (10→60 s) between rounds; reset after any session
//! so a post-drive reconnect is prompt.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::control::Control;
use crate::{bt_driver, control, sdp_client};

/// Bounds on a single SDP/RFCOMM connect to a quiet or absent phone (seconds). Long enough for a real
/// page+connect over BR/EDR, short enough that a missing phone doesn't park the thread for a whole
/// backoff interval.
const CONNECT_TIMEOUT_SECS: i64 = 8;
const BACKOFF_START_SECS: u64 = 10;
const BACKOFF_MAX_SECS: u64 = 60;
/// One short retry when the FIRST round after start-up fails, instead of the full
/// `BACKOFF_START_SECS`. Reset cycle on a CCPA, 2026-10-20: in 3 of 8 reconnects the first page
/// failed ~5 s in (`SDP query failed: Connection reset by peer` twice, `Host is down` once) and the
/// very next attempt succeeded - but only after the 10 s floor, so each cost ~15 s. Once per daemon
/// start, so it cannot spin: every later round still gets the floor and the doubling.
const FIRST_RETRY_SECS: u64 = 2;
/// The first round's page, in 0.625 ms slots (3.75 s): an absent first-listed phone cost the whole 5.12 s default
/// before the next phone was paged (the Mac's 12 Nov bench: 5.2 s of "Host is down"). Scott, 13 Nov: keep the
/// driver's order, page each phone briefly in turn first, and give later rounds the full default, so a phone slow
/// to answer is still reached. 2.5 s was tight: a present iPhone answered in 0.23-2.58 s and missed it once in 11 (the
/// Mac's 13 Nov bench, at times a 2.56 s page-scan interval), which cost 9.7 s. 3.75 s covers that interval.
const FIRST_ROUND_PAGE_SLOTS: u16 = 6_000;

/// Round 4's bench switches for this loop (docs/ops/02_TESTING.md, "Round 4 switches"), read once as it starts
/// ([`Round4::read`]): each at `/tmp/bench_<name>` or, kept across a power cut for a cold boot, at
/// `/etc/ocbm/bench_<name>` ([`box_common::bench_switch`]). None set, the default: the loop as 2.7.0 ran it, and the 13
/// Nov rule (every phone paged briefly in the driver's order first) stands. Each one set says so once, in a
/// `[bench] round4:` line, and again where it acts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Round4 {
    /// Item 1 (`first_page_slots`, 1-65535 slots of 0.625 ms): the first page of the first round, at this length; every
    /// later page at the round's ([`round_page_slots`]). 8192 is the controller's 5.12 s: a phone that answers a cold
    /// page late (3.4-4.3 s on the truck) is reached on that first page.
    first_page_slots: Option<u16>,
    /// Item 2 (`first_retry_ms`, 0-60000 ms): the pause after a failed first round, for [`FIRST_RETRY_SECS`]. Still once
    /// per loop, and a round pages at least one phone, so 0 cannot spin.
    first_retry_ms: Option<u64>,
    /// Item 5 (`early_gap_secs`, 1-60 s): the gap between rounds in the first [`EARLY_WINDOW`], for [`EARLY_GAP_SECS`].
    /// Not 0: a gap is the loop's floor against spinning on pages that fail at once (audit B5).
    early_gap_secs: Option<u64>,
    /// Item 3 (`same_phone_repage`, a flag): a re-page after a reset or a cut that crosses a link the controller still
    /// holds ([`Attempt::Crossed`]) waits for that link to clear and pages the same phone again, up to [`CROSSED_REPAGES`]
    /// times ([`walk`]), before the walk goes on to the next phone as today.
    same_phone_repage: bool,
    /// Item 4 (`rfcomm_limit_ms`, 1000-8000 ms): the RFCOMM connect after SDP gave the channel is cut at this, never while
    /// a pairing prompt holds it (`rfcomm::connect_to_limited`); its link is dropped and the phone paged once more
    /// ([`Attempt::Stalled`]). Not under 1 s: a normal connect takes up to about 0.8 s (sdp to rfcomm, cold boots).
    rfcomm_limit_ms: Option<u64>,
}

impl Round4 {
    /// The switches as `get` reads them (a name to its text and the file it came from, [`box_common::bench_switch`]), and
    /// one line for each that is set: what it does, or why its text was refused (the default then stands).
    fn from(get: impl Fn(&str) -> Option<(String, String)>) -> (Round4, Vec<String>) {
        let mut r = Round4::default();
        let mut lines = Vec::new();
        if let Some((n, at)) = number(&get, &mut lines, "first_page_slots", 1, u64::from(u16::MAX), "slots") {
            let n = n as u16;
            r.first_page_slots = Some(n);
            lines.push(format!(
                "[bench] round4: first_page_slots={n} ({at}): the first page of the first round {} ms; every other page the round's",
                slots_ms(n)
            ));
        }
        if let Some((ms, at)) = number(&get, &mut lines, "first_retry_ms", 0, 60_000, "ms") {
            r.first_retry_ms = Some(ms);
            lines.push(format!(
                "[bench] round4: first_retry_ms={ms} ({at}): after a failed first round the next one in {ms} ms, once (default {} ms)",
                FIRST_RETRY_SECS * 1000
            ));
        }
        if let Some((s, at)) = number(&get, &mut lines, "early_gap_secs", 1, 60, "s") {
            r.early_gap_secs = Some(s);
            lines.push(format!(
                "[bench] round4: early_gap_secs={s} ({at}): rounds {s} s apart in the first {} s (default {EARLY_GAP_SECS} s)",
                EARLY_WINDOW.as_secs()
            ));
        }
        if let Some((_, at)) = get("same_phone_repage") {
            r.same_phone_repage = true;
            lines.push(format!(
                "[bench] round4: same_phone_repage ({at}): a re-page after a reset or a cut that crosses a link the controller \
                 still holds (EALREADY, EMLINK, EBUSY) pages the same phone again, up to {CROSSED_REPAGES} times, before the next phone"
            ));
        }
        let base_ms = CONNECT_TIMEOUT_SECS as u64 * 1000;
        if let Some((ms, at)) = number(&get, &mut lines, "rfcomm_limit_ms", 1000, base_ms, "ms") {
            r.rfcomm_limit_ms = Some(ms);
            lines.push(format!(
                "[bench] round4: rfcomm_limit_ms={ms} ({at}): an RFCOMM connect not up in {ms} ms (and no pairing prompt up) is \
                 closed, its link dropped and the phone paged once more (default: {base_ms} ms, then the next phone)"
            ));
        }
        (r, lines)
    }

    /// The box's switches, each line logged.
    fn read() -> Round4 {
        let (r, lines) = Round4::from(box_common::bench_switch);
        for l in &lines {
            log(l);
        }
        r
    }
}

/// Switch `name`'s whole number in `lo..=hi` (`unit`), and the file it came from; a text that is not one is refused,
/// with a line in `lines`.
fn number(
    get: &impl Fn(&str) -> Option<(String, String)>,
    lines: &mut Vec<String>,
    name: &str,
    lo: u64,
    hi: u64,
    unit: &str,
) -> Option<(u64, String)> {
    let (text, at) = get(name)?;
    match text.parse::<u64>() {
        Ok(n) if (lo..=hi).contains(&n) => Some((n, at)),
        _ => {
            lines.push(refused(&at, &text, &format!("{unit} {lo}-{hi}")));
            None
        }
    }
}

/// A switch's text that is not one: the line that says so.
fn refused(at: &str, text: &str, want: &str) -> String {
    format!("[bench] round4: {at} refused ({text:?}: want {want}): the default")
}

/// A page length in slots of 0.625 ms, in whole ms.
fn slots_ms(slots: u16) -> u64 {
    u64::from(slots) * 5 / 8
}

/// `order` with the phones whose page timed out last round moved last, in their order. After a failed first round the
/// next one paged the absent first-listed phone first again, now for the full 5.12 s; and the present phone,
/// paged straight after an absent one's timeout, answered and reset the link in 4 of 17 connects (the Mac, 13 Nov).
fn absent_last(order: Vec<[u8; 6]>, absent: &[[u8; 6]]) -> Vec<[u8; 6]> {
    let (gone, here): (Vec<_>, Vec<_>) = order.into_iter().partition(|p| absent.contains(p));
    here.into_iter().chain(gone).collect()
}

/// What a round carries to the next, and across a re-plan within one (the Mac's 13 Nov review, R1-R3).
#[derive(Debug, Default)]
struct Rounds {
    /// The phones whose page timed out in the last finished round: the next one pages them last.
    absent_before: Vec<[u8; 6]>,
    /// This round's timed-out pages so far, kept across a re-plan.
    absent_now: Vec<[u8; 6]>,
    /// The phones this round has paged, kept across a re-plan: the re-planned rest skips them.
    paged: Vec<[u8; 6]>,
    /// Whether the last page, in this round, timed out: a reset right after it is a crossing.
    prev_absent: bool,
    /// The app's order came mid-round: the next walk is the rest of this round, not a new one.
    replanning: bool,
}

impl Rounds {
    /// A walk starts: a new round forgets the last one's pages; the rest of a re-planned round keeps them. True for a
    /// new round.
    fn begin(&mut self) -> bool {
        if self.replanning {
            return false;
        }
        self.absent_now.clear();
        self.paged.clear();
        self.prev_absent = false;
        true
    }

    /// The order to page in: the phones absent last round moved last BEFORE `call_order` (the pass-over), so a
    /// passed-over phone stays last (R3), and less the phones this round already paged.
    fn order(&self, bonds: Vec<[u8; 6]>, call_order: impl FnOnce(Vec<[u8; 6]>) -> Vec<[u8; 6]>) -> Vec<[u8; 6]> {
        call_order(absent_last(bonds, &self.absent_before)).into_iter().filter(|p| !self.paged.contains(p)).collect()
    }

    /// One page and its result. False for a crossing reset, one straight after another phone's page timed out: the
    /// two most likely crossed, and it is not counted toward a forgotten pairing (the Mac, 13 Nov).
    fn paged(&mut self, peer: [u8; 6], result: Attempt) -> bool {
        self.paged.push(peer);
        let crossing = matches!(result, Attempt::Reset | Attempt::Cut | Attempt::Stalled) && self.prev_absent;
        self.prev_absent = result == Attempt::Absent;
        if self.prev_absent && !self.absent_now.contains(&peer) {
            self.absent_now.push(peer);
        }
        !crossing
    }

    /// Whether a phone of `bonds` is still to be paged this round.
    fn left_to_page(&self, bonds: &[[u8; 6]]) -> bool {
        bonds.iter().any(|p| !self.paged.contains(p))
    }

    /// The walk ended. True when it re-plans: the app's order came mid-round (`changed`) and a phone is left to
    /// page. Otherwise the round is over, and a session in it clears what it learned (R2).
    fn end(&mut self, ran: bool, changed: bool, bonds: &[[u8; 6]]) -> bool {
        self.replanning = !ran && changed && self.left_to_page(bonds);
        if !self.replanning {
            self.absent_before = if ran { Vec::new() } else { std::mem::take(&mut self.absent_now) };
        }
        self.replanning
    }

    /// A session the phone started itself: nobody is absent, and a re-planned rest is moot.
    fn session(&mut self) {
        self.absent_before.clear();
        self.replanning = false;
    }

    /// The loop went round without a walk (no bonds, or autoConnect off and nothing asked): a re-planned rest that never
    /// ran ends its round here, so a later round, maybe much later, pages every phone again (the Mac's review, F5).
    fn not_walked(&mut self) {
        if self.replanning {
            self.replanning = false;
            self.absent_before = std::mem::take(&mut self.absent_now);
        }
    }
}

/// The wait before the next round: [`FIRST_RETRY_SECS`] once after a failed first round; [`EARLY_GAP_SECS`] in the
/// first [`EARLY_WINDOW`] after a host came (`early`); else the backoff, which starts doubling only after that window.
/// Round 4's bench switches `r4` may set the first two ([`Round4::first_retry_ms`], [`Round4::early_gap_secs`]).
fn round_gap(quick: bool, early: bool, backoff: u64, r4: &Round4) -> Duration {
    if quick {
        r4.first_retry_ms.map_or(Duration::from_secs(FIRST_RETRY_SECS), Duration::from_millis)
    } else if early {
        Duration::from_secs(r4.early_gap_secs.unwrap_or(EARLY_GAP_SECS).min(backoff))
    } else {
        Duration::from_secs(backoff)
    }
}

/// The page timeout for a round: the short one for the first walk over more than one phone, else the default.
fn round_page_slots(first_round: bool, phones: usize) -> u16 {
    if first_round && phones > 1 {
        FIRST_ROUND_PAGE_SLOTS
    } else {
        crate::hci::PAGE_TIMEOUT_DEFAULT
    }
}
/// The pause before the one more try a phone gets when it answers the page and then resets the link
/// ([`Attempt::Reset`]), before the next phone in the call order is paged. Dev bench, 2 Nov: 32 s after
/// a power cut the first-called iPhone accepted the page and reset it 4 s later (`SDP query failed:
/// Connection reset by peer`), and the walk went straight on to the second phone, which got CarPlay; in
/// the truck that hands it to a passenger's phone after a short stop with the engine off. The reset
/// cycle behind [`FIRST_RETRY_SECS`] found the very next attempt succeeds, but that retry only runs once
/// no phone in the round connected.
///
/// None since the Mac's 13 Nov brief (item 11): the re-page 2 s later connected about 1 s in, every time, and a reset
/// is now caught 0.7 s after the answer (`sdp_client`'s watch), so the re-page goes at once.
const RESET_RETRY_SECS: u64 = 0;
/// What the walk's pause says of a page it repeats: a reset, a cut, or (round 4, bench) an RFCOMM connect past its limit.
fn retry_words(result: Attempt) -> &'static str {
    match result {
        Attempt::Cut => "answered and opened no SDP channel",
        Attempt::Stalled => "answered and opened no RFCOMM channel within the bench limit",
        _ => "answered and reset the link",
    }
}

/// For the first [`EARLY_WINDOW`] after btd starts (a host has just come), rounds are this far apart, not the doubling
/// backoff: a phone that comes back (a driver who walks up after the display is on) waited 10, 20, then 40 s for
/// the next page (the Mac, 13 Nov). A phone that said no still gets [`BACKOFF_MAX_SECS`].
const EARLY_GAP_SECS: u64 = 5;
const EARLY_WINDOW: Duration = Duration::from_secs(120);
/// Let bring-up/SSP/SDP settle and give the phone a moment to connect IN on its own before we start
/// paging it ourselves.
///
/// Was 5 s. A GM truck drive log (Carlink3, 2026-10-20) showed it as 5.0 of the 27.5 s from the
/// host's SUBSCRIBE to the first video frame, with the bonded phone answering the page 0.75 s after
/// it ended. Bring-up has already returned when this starts (SSP agent and SDP are serving), and a
/// phone that connects IN meanwhile is still safe: `attempt()`'s compare_exchange on
/// `session_active` refuses to run a second session over it, and a page that rides its link (the kernel reuses
/// the ACL) keeps that link: if its SDP connect is cut ([`Attempt::Cut`], paged once more and counted once a round),
/// `sdp_client` only closes its socket, with no Disconnect for a link that was up before the page or that a live
/// session rides (the Mac's review of bbd29f3, A2). 1 s kept a moment for that inbound
/// connect.
/// Override: `BT_RECONNECT_SETTLE_SECS` or `/tmp/reconnect_settle_secs` (whole seconds, `0` allowed).
/// None when the controller was warm ([`BT_WARM`]): nothing was just attached, so paging starts at once (the Mac's
/// 13 Nov connect-time brief). None after a fresh attach either since its 13 Nov brief (item 6): an adapter still
/// booting when the host came paid it on top of the attach, and the lever stays for a bench that wants it back.
const INITIAL_SETTLE_DEFAULT_SECS: u64 = 0;

/// Left by `radio_hal.sh bt_on` when this session's bring-up found the controller attached and at most powered
/// down, rather than attaching it.
const BT_WARM: &str = "/tmp/.radio_bt_warm";

/// The project's bench-lever convention: env first, then the tmpfs file (`/tmp` clears on reboot); with no lever,
/// none after a warm bring-up and [`INITIAL_SETTLE_DEFAULT_SECS`] after a fresh attach.
fn initial_settle_secs() -> u64 {
    let parse = |v: &str| v.trim().parse::<u64>().ok();
    box_common::lever("BT_RECONNECT_SETTLE_SECS", "CARPLAY_RECONNECT_SETTLE_SECS")
        .and_then(|v| parse(&v))
        .or_else(|| std::fs::read_to_string("/tmp/reconnect_settle_secs").ok().and_then(|v| parse(&v)))
        .unwrap_or_else(|| default_settle_secs(std::path::Path::new(BT_WARM).exists()))
}

/// The settle with no lever set: none when the bring-up found the controller warm.
fn default_settle_secs(warm: bool) -> u64 {
    if warm {
        0
    } else {
        INITIAL_SETTLE_DEFAULT_SECS
    }
}

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [reconnect] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sleep `secs`, but wake every second to observe `shutdown`. Returns early `false` if shutdown fired.
fn interruptible_sleep(secs: u64, shutdown: &AtomicBool) -> bool {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_secs(1));
    }
    !shutdown.load(Ordering::Relaxed)
}

/// As [`interruptible_sleep`], but also cuts short when the device screen has asked for a connect.
///
/// A tap on *Connect* used to do nothing for up to `BACKOFF_MAX_SECS` — the request is read only at
/// the top of the loop — while the app had already answered `{"ok":true}`. Peeks, so the request is
/// still there for the loop to consume.
///
/// Deliberately NOT used by the no-bonds sleep: a request cannot be served with nothing bonded, so
/// waking on it there would spin the loop instead of waiting for a bond.
fn sleep_or_request(secs: u64, shutdown: &AtomicBool, ctrl: &Control) -> bool {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        if ctrl.has_request() {
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    !shutdown.load(Ordering::Relaxed)
}

/// [`sleep_or_request`] to the millisecond, for a round's gap (round 4's bench switches can set one under a second): its
/// whole seconds as [`sleep_or_request`] sleeps them, then the rest unless a request came first. A gap of whole seconds
/// sleeps exactly as before.
fn sleep_or_request_for(gap: Duration, shutdown: &AtomicBool, ctrl: &Control) -> bool {
    if !sleep_or_request(gap.as_secs(), shutdown, ctrl) {
        return false;
    }
    let rest = gap.saturating_sub(Duration::from_secs(gap.as_secs()));
    if !rest.is_zero() && !ctrl.has_request() {
        thread::sleep(rest);
    }
    !shutdown.load(Ordering::Relaxed)
}

/// One reconnect attempt against `peer`: SDP-query → RFCOMM-connect → hand to the iAP2 driver.
/// Returns `true` if a session actually ran (so the caller resets its backoff); `false` also covers
/// losing the single-session claim to the accept path. The slot is CLAIMED via `compare_exchange` right
/// before the driver runs (not held across the connect) and cleared only by the claim owner.
/// What one [`attempt`] did, so the loop can pick the right backoff.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Attempt {
    /// The iAP2 driver ran (a real session, however short).
    Ran,
    /// SDP/RFCOMM failed for an ordinary reason (no iAP2 service, a lost claim, a refused connect).
    Failed,
    /// The page timed out (`EHOSTDOWN`, "Host is down"): the phone is not here. The only result that demotes a phone
    /// in the next round ([`Rounds`]) or makes the next one's reset a crossing.
    Absent,
    /// The phone answered the page and then reset the link (`ConnectionReset` from SDP or RFCOMM): it is
    /// there but not ready, as an iPhone still letting go of a session a power cut ended. Paged once more
    /// before the next phone ([`walk`]).
    Reset,
    /// The phone answered the page and opened no SDP channel within the window (`sdp_client`'s cut): it is there but
    /// not ready, as a phone a power cut left mid-session. Paged once more like a reset, but a round counts at most one
    /// toward a forgotten pairing ([`ResetStreak`]): the re-page is at once, so a not-ready phone could otherwise run up
    /// three cuts in 5-8 s and Carlink would ask to re-pair a paired phone (the Mac's review of bbd29f3, A1).
    Cut,
    /// The phone rejected the SSP re-pair (`rfcomm::connect_to` → `PermissionDenied`):
    /// paging it again just produces another prompt it cannot accept. Hold off for the maximum
    /// backoff and let the operator re-pair from the iPhone.
    PairRejected,
    /// The page failed with EALREADY, EMLINK or EBUSY (HCI 0x0B "ACL Connection Already Exists", 0x09/0x0A "Connection
    /// Limit Exceeded", 0x0C "Command Disallowed"): the controller still holds a link to the phone, the one the last page
    /// left or one the phone is bringing up itself (the truck, 21 Nov: right after a cut, then the other phone and about
    /// 100 s lost). Everywhere exactly [`Attempt::Failed`], save one: as the re-page after a reset or a cut, round 4's
    /// `same_phone_repage` (bench) pages the same phone again ([`walk`]).
    Crossed,
    /// Round 4, item 4 (bench `rfcomm_limit_ms`): the phone answered, SDP gave its iAP2 channel, and the RFCOMM connect
    /// was not up within the limit, so btd closed it and dropped the link (`sdp_client::drop_stalled_link`). Paged once
    /// more like a reset, and counted once a round toward a forgotten pairing like a cut: btd gave up on it, the phone did
    /// not refuse it. Never without the switch.
    Stalled,
}

/// [`Attempt::Cut`] for `sdp_client`'s cut, [`Attempt::Reset`] when the phone reset the link, [`Attempt::Absent`] when the
/// page timed out, [`Attempt::Crossed`] for a link the controller still holds, else [`Attempt::Failed`].
fn failure(e: &std::io::Error) -> Attempt {
    if sdp_client::is_cut(e) {
        Attempt::Cut
    } else if e.kind() == std::io::ErrorKind::ConnectionReset {
        Attempt::Reset
    } else if e.raw_os_error() == Some(libc::EHOSTDOWN) {
        Attempt::Absent
    } else if matches!(e.raw_os_error(), Some(libc::EALREADY | libc::EMLINK | libc::EBUSY)) {
        Attempt::Crossed
    } else {
        Attempt::Failed
    }
}

/// What one walk down the call order did.
#[derive(Debug, Default, PartialEq, Eq)]
struct Walk {
    ran: bool,
    rejected: bool,
}

/// Answered-and-reset pages in a row, across reconnect rounds, for [`ocbm_proto::BTP_BOND_LOST`]. A still-paired
/// iPhone also answers and hangs up once after a power cut (dev adapter, 3 Nov), so one or two say little; every
/// page answered and reset, [`Self::TELL_AT`] or more with nothing else between, means it most likely forgot the
/// pairing (5 Nov bench: six, and Carlink offered pairing only a minute later). Told once per streak.
///
/// A new bond starts the count over, and holds the news back for [`Self::NEW_BOND_HOLD`]: right after a pairing,
/// until the driver accepts CarPlay on the iPhone, it refuses iAP2 and answers-and-resets too (the 1.0.4 bench,
/// 5 Nov: two in a row within a minute of a successful re-pair, after a mis-tapped CarPlay prompt).
#[derive(Debug, Default)]
struct ResetStreak {
    /// Each phone's own run: another phone's page doesn't end it. With two bonded iPhones and one away, every
    /// round paged both, the absent one's failure ended the count, and BOND_LOST never came (the Mac, 6 Nov).
    runs: std::collections::HashMap<[u8; 6], Run>,
    /// When the link-key store was last written, as of the last page.
    bonds: Option<std::time::SystemTime>,
    /// When btd first saw that write, on the monotonic clock: the hold's start when the write's age is unknown.
    seen: Option<Instant>,
    /// The reconnect round, for one counted cut per phone per round ([`Self::new_round`]).
    round: u64,
}

#[derive(Debug, Default)]
struct Run {
    count: u32,
    told: bool,
    /// The round of this run's last counted cut: a round counts one.
    cut_round: Option<u64>,
}

impl ResetStreak {
    const TELL_AT: u32 = 3;
    const NEW_BOND_HOLD: Duration = Duration::from_secs(10 * 60);

    /// One page of `peer`, with the link-key store's last write ([`bonds_written`]), the wall clock and the
    /// monotonic one: true exactly when that phone's run is at [`Self::TELL_AT`] or more and no bond is newer than
    /// [`Self::NEW_BOND_HOLD`]. A session (Ran) or a new bond ends every run; the phone's own failure ends its own.
    /// A write stamped later than the wall clock (it reads 2020 after a restart until Carlink's SETTIME lands, the
    /// Mac, 6 Nov) has no age: its hold then runs from when btd first saw it, so it can't last.
    fn note(
        &mut self,
        peer: [u8; 6],
        result: Attempt,
        bonds: Option<std::time::SystemTime>,
        now: std::time::SystemTime,
        mono: Instant,
    ) -> bool {
        if bonds != self.bonds {
            self.runs.clear();
            self.bonds = bonds;
            self.seen = Some(mono);
        }
        match result {
            Attempt::Reset => {}
            // A cut counts once a round: not none, since a cut at 0.7 s now comes before a phone that really forgot the
            // pairing would reset the link itself, so ignoring cuts could keep its BOND_LOST from ever coming. Round 4's
            // stalled RFCOMM connect is btd's own cut too, and shares the round's one.
            Attempt::Cut | Attempt::Stalled => {
                let run = self.runs.entry(peer).or_default();
                if run.cut_round == Some(self.round) {
                    return false;
                }
                run.cut_round = Some(self.round);
            }
            Attempt::Ran => {
                self.runs.clear();
                return false;
            }
            Attempt::Failed | Attempt::Crossed | Attempt::Absent | Attempt::PairRejected => {
                self.runs.remove(&peer);
                return false;
            }
        }
        let new_bond = match bonds.map(|t| now.duration_since(t)) {
            None => false,
            Some(Ok(age)) => age < Self::NEW_BOND_HOLD,
            Some(Err(_)) => self.seen.is_some_and(|s| mono.duration_since(s) < Self::NEW_BOND_HOLD),
        };
        let run = self.runs.entry(peer).or_default();
        run.count += 1;
        if run.count >= Self::TELL_AT && !run.told && !new_bond {
            run.told = true;
            return true;
        }
        false
    }

    /// A session the phone started itself: it knows the adapter, and every run ends.
    fn session(&mut self) {
        self.runs.clear();
    }

    /// A new round of the reconnect loop (not the re-planned rest of one): each phone may count one more cut.
    fn new_round(&mut self) {
        self.round += 1;
    }
}

/// When the link-key store was last written: a new bond rewrites it.
fn bonds_written() -> Option<std::time::SystemTime> {
    std::fs::metadata(bt_common::ssp_agent::link_key_store()).and_then(|m| m.modified()).ok()
}

/// How many more times a re-page that crossed a link ([`Attempt::Crossed`]) pages the same phone, at most, before the
/// walk goes on to the next (round 4, item 3).
const CROSSED_REPAGES: u32 = 2;

/// Page `targets` in order until `stop` says not to go on. A phone that answers and resets the link
/// ([`Attempt::Reset`]) or is cut ([`Attempt::Cut`], or round 4's [`Attempt::Stalled`]) is paged once more, after `pause`
/// (false: go no further), before the next one. That re-page crossing a link the controller holds ([`Attempt::Crossed`]) pages the phone again, up to
/// [`CROSSED_REPAGES`] times, while `pause` says so for it (false: on to the next phone, as a failure; it says false
/// without round 4's `same_phone_repage`).
fn walk(
    targets: &[[u8; 6]],
    mut stop: impl FnMut() -> bool,
    mut pause: impl FnMut([u8; 6], Attempt) -> bool,
    mut page: impl FnMut([u8; 6]) -> Attempt,
) -> Walk {
    let mut out = Walk::default();
    for &peer in targets {
        if stop() {
            break;
        }
        let mut result = page(peer);
        if matches!(result, Attempt::Reset | Attempt::Cut | Attempt::Stalled) {
            if !pause(peer, result) {
                break;
            }
            result = page(peer);
            let mut again = 0;
            while result == Attempt::Crossed && again < CROSSED_REPAGES && pause(peer, result) {
                again += 1;
                result = page(peer);
            }
        }
        match result {
            Attempt::Ran => out.ran = true,
            Attempt::Failed | Attempt::Crossed | Attempt::Absent | Attempt::Reset | Attempt::Cut | Attempt::Stalled => {}
            Attempt::PairRejected => {
                out.rejected = true;
                break; // do not page the next phone into the same rejection storm
            }
        }
    }
    out
}

fn attempt(
    peer: [u8; 6],
    name: &str,
    shutdown: &AtomicBool,
    session_active: &AtomicBool,
    ctrl: &Control,
    rfcomm_limit: Option<Duration>,
) -> Attempt {
    let services = match sdp_client::query(peer, CONNECT_TIMEOUT_SECS, &sdp_client::Watch { shutdown, session_active }) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("SDP query failed: {e}"));
            return failure(&e);
        }
    };
    let channel = match services.iap2 {
        Some(ch) => ch,
        None => {
            // Not an iPhone, or one that offers no iAP2 service now: nothing to connect to.
            log("the phone offers no iAP2 service — nothing to reconnect");
            return Attempt::Failed;
        }
    };
    let sock = match crate::rfcomm_connect(peer, channel, CONNECT_TIMEOUT_SECS, rfcomm_limit) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            log(&format!(
                "RFCOMM connect to channel {channel} aborted: {e} — holding off {BACKOFF_MAX_SECS}s; \
                 re-pair on the iPhone (Settings ▸ Bluetooth) or switch pairing to Just-Works"
            ));
            return Attempt::PairRejected;
        }
        // Round 4, item 4 (bench): past the limit, the socket closed; the page's link dropped as a cut drops it, so the
        // re-page neither rides a half-open link nor meets it as EALREADY. One up before the page, or a live session's,
        // stays.
        Err(e) if crate::rfcomm::is_limited(&e) => {
            let keep = services.link_before.or_else(|| session_active.load(Ordering::Relaxed).then_some("a session is live"));
            let dropped = sdp_client::drop_stalled_link(peer, keep);
            log(&format!("[bench] round4: rfcomm_limit_ms: RFCOMM connect to channel {channel} {e}: closed; {dropped}"));
            return Attempt::Stalled;
        }
        Err(e) => {
            log(&format!("RFCOMM connect to channel {channel} failed: {e}"));
            return failure(&e);
        }
    };
    log(&format!("RFCOMM connected OUT to the phone (ch {channel}) — starting iAP2 handshake"));
    crate::startup::mark("phone");
    crate::startup::stamp("rfcomm");
    // The real success signal is iAP2 progress inside bt_driver::run, NOT the mgmt DEVICE_CONNECTED
    // for our own outbound connect (docs/wireless/01_BT_AND_RADIO.md §Design notes — the Model-A latching gotcha).
    // Claim the single-session slot ATOMICALLY, immediately before running the driver. compare_exchange
    // (not a plain store) closes the TOCTOU with the accept path: the ~16 s SDP-query + RFCOMM-connect
    // above can overlap an inbound accept that took the slot in the meantime. If we lose the claim, stand
    // down — drop the freshly-connected socket (the phone retries) rather than run a second concurrent
    // bt_driver::run against the same phone. Only the claim owner clears the flag (below), so a finishing
    // session can never clobber another owner's still-live claim.
    // The guard claims the slot, publishes the peer for the device screen, and releases BOTH on
    // every exit from here — so the claim and its release cannot drift apart as this block grows,
    // which is what had already happened between this call site and the accept path's.
    let Some(_claim) = control::SessionClaim::try_claim(session_active, ctrl, Some(peer)) else {
        log("session already active — standing down (dropping outbound connect)");
        return Attempt::Failed;
    };
    bt_driver::run(sock, name, shutdown, Some(peer));
    log("reconnect session ended");
    Attempt::Ran
}

/// The reconnect loop. Spawns nothing. Whenever no session is live, re-reads the bond list and tries
/// each bonded phone in turn with bounded backoff, until `shutdown`. When nothing is bonded it idles
/// (re-checking every `BACKOFF_MAX_SECS`) rather than exiting, so a phone paired via the accept path
/// after boot becomes reconnect-eligible without a daemon restart (audit Fix #22).
///
/// ## Connection policy (Raspberry Pi port)
///
/// The order comes from [`Control::ordered_bonds`] — the projection app's configured
/// first-to-connect list, with any bond it does not mention appended so a newly paired phone is
/// never invisible. With `autoConnect` off the loop does **not** drive on its own; it waits for an
/// explicit request from the device screen, which is GM's "tap to connect".
///
/// An explicit request always wins, including while `autoConnect` is off and including out of
/// order: pressing Connect on a specific phone means that phone, now.
pub fn run(
    name: &str,
    shutdown: &Arc<AtomicBool>,
    session_active: &Arc<AtomicBool>,
    ctrl: &Arc<Control>,
) {
    // Settle before the first probe (bring-up/SSP/SDP, and give the phone a chance to connect IN first).
    let settle = initial_settle_secs();
    log(&format!("settling {settle}s before paging bonded phones"));
    if !interruptible_sleep(settle, shutdown) {
        return;
    }
    let env = OnBox { name, shutdown, session_active, ctrl, started: Instant::now(), round4: Round4::read() };
    run_loop(&env, ctrl);
    log("reconnect loop exiting");
}

/// What the loop asks of the box, so a test drives [`run_loop`] itself with a fake pager and a virtual clock (the
/// Mac's 13 Nov bench-hooks brief, item 9). [`OnBox`] is the real one: the radio, the link-key store, the clock.
trait Env {
    /// The bonded phones in the policy's order ([`Control::ordered_bonds`]).
    fn bonds(&self) -> Vec<[u8; 6]>;
    /// One page and connect ([`attempt`]).
    fn attempt(&self, peer: [u8; 6]) -> Attempt;
    fn shutdown(&self) -> bool;
    fn session_active(&self) -> bool;
    /// Sleep `secs`, false on shutdown ([`interruptible_sleep`]).
    fn sleep(&self, secs: u64) -> bool;
    /// Sleep `secs`, cut short by a connect request, false on shutdown ([`sleep_or_request`]).
    fn sleep_or_request(&self, secs: u64) -> bool;
    /// A round's gap, to the millisecond ([`sleep_or_request_for`]).
    fn sleep_or_request_for(&self, gap: Duration) -> bool;
    /// Since the loop started (a host has just come).
    fn elapsed(&self) -> Duration;
    /// The monotonic and wall clocks and the link-key store's last write, for [`ResetStreak::note`].
    fn now(&self) -> (Instant, std::time::SystemTime, Option<std::time::SystemTime>);
    /// Write_Page_Timeout.
    fn page_slots(&self, slots: u16) -> std::io::Result<()>;
    fn bond_lost(&self);
    fn pair_rejected_over(&self);
    /// The bench's phantom first phone ([`BENCH_PHANTOM_FLAG`]), when set.
    fn phantom(&self) -> Option<[u8; 6]>;
    /// The bench's call order for the round about to start ([`BENCH_ORDER_FILE`]), taken once.
    fn bench_order(&self) -> Option<BenchOrder>;
    /// A [bench] line about the call order: btd's log on the box, kept by a test's fake.
    fn bench(&self, line: &str);
    /// Forget the link a cut left ([`sdp_client::forget_left_link`]): btd's bring-up just reset the controller.
    fn forget_left_link(&self);
    /// Round 4's bench switches ([`Round4`]), as this loop read them at its start.
    fn round4(&self) -> Round4;
    /// Round 4, item 3 (bench): wait for the link a crossed re-page found to clear or come up
    /// ([`sdp_client::settle_crossed`]): what it found.
    fn settle_crossed(&self, peer: [u8; 6]) -> &'static str;
}

/// The real [`Env`].
struct OnBox<'a> {
    name: &'a str,
    shutdown: &'a Arc<AtomicBool>,
    session_active: &'a Arc<AtomicBool>,
    ctrl: &'a Arc<Control>,
    started: Instant,
    round4: Round4,
}

impl Env for OnBox<'_> {
    fn bonds(&self) -> Vec<[u8; 6]> {
        self.ctrl.ordered_bonds()
    }
    fn attempt(&self, peer: [u8; 6]) -> Attempt {
        let limit = self.round4.rfcomm_limit_ms.map(Duration::from_millis);
        let result = attempt(peer, self.name, self.shutdown, self.session_active, self.ctrl, limit);
        match result {
            Attempt::Absent => crate::startup::tally("absent"),
            // A cut is a reset on the [startup] line, with its own count beside it (sdp_client tallies it). So is round 4's
            // stalled RFCOMM connect, which has a [bench] line of its own.
            Attempt::Reset | Attempt::Cut | Attempt::Stalled => crate::startup::tally("reset"),
            _ => {}
        }
        result
    }
    fn shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
    fn session_active(&self) -> bool {
        self.session_active.load(Ordering::Relaxed)
    }
    fn sleep(&self, secs: u64) -> bool {
        interruptible_sleep(secs, self.shutdown)
    }
    fn sleep_or_request(&self, secs: u64) -> bool {
        sleep_or_request(secs, self.shutdown, self.ctrl)
    }
    fn sleep_or_request_for(&self, gap: Duration) -> bool {
        sleep_or_request_for(gap, self.shutdown, self.ctrl)
    }
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
    fn now(&self) -> (Instant, std::time::SystemTime, Option<std::time::SystemTime>) {
        (Instant::now(), std::time::SystemTime::now(), bonds_written())
    }
    fn page_slots(&self, slots: u16) -> std::io::Result<()> {
        crate::hci::dev_id(crate::HCI_DEV).and_then(|dev| crate::hci::write_page_timeout(dev, slots))
    }
    fn bond_lost(&self) {
        bt_driver::publish_bt_phase(ocbm_proto::BTP_BOND_LOST);
    }
    fn pair_rejected_over(&self) {
        let _ = std::fs::remove_file(bt_common::rfcomm::PAIR_REJECTED_FLAG);
    }
    fn phantom(&self) -> Option<[u8; 6]> {
        box_common::bench_flag(BENCH_PHANTOM_FLAG).then_some(BENCH_PHANTOM)
    }
    fn bench_order(&self) -> Option<BenchOrder> {
        take_bench_order(BENCH_ORDER_FILE)
    }
    fn bench(&self, line: &str) {
        log(line);
    }
    fn forget_left_link(&self) {
        sdp_client::forget_left_link();
    }
    fn round4(&self) -> Round4 {
        self.round4
    }
    fn settle_crossed(&self, peer: [u8; 6]) -> &'static str {
        sdp_client::settle_crossed(peer)
    }
}

/// The bench flag that adds [`BENCH_PHANTOM`] to every policy-order round ahead of the bonded phones, as an absent
/// first-listed phone: first in the first round and after a session, a round that connected, a Connect or a switch
/// hold; last after a round that found it absent, as a real one; never in a Connect from the device screen, named or not. No airplane mode, no
/// Bluetooth toggling (the Mac's 13 Nov bench-hooks brief, item 4). Bench only; /tmp, so a reboot
/// clears it; nothing in the product writes it (docs/ops/02_TESTING.md).
const BENCH_PHANTOM_FLAG: &str = "/tmp/bench_phantom_first";

/// 02:00:00:00:00:01 in mgmt byte order: a locally administered address no phone has, so its page times out
/// ("Host is down") like an absent phone's. Never bonded, never offered for pairing, never listed on the control port,
/// never counted toward a forgotten pairing.
const BENCH_PHANTOM: [u8; 6] = [0x01, 0x00, 0x00, 0x00, 0x00, 0x02];

/// The bench's call order for one round (docs/ops/02_TESTING.md): one line, "MAC,MAC[,...] [after_ms]", the MACs as
/// AA:BB:CC:DD:EE:FF and after_ms 0 when left out. The next policy-order round takes it (reads and removes it) and,
/// after_ms into its walk, pages the rest of the round in that order, through the re-plan path, as if the app's order
/// had just come (the Mac's 13 Nov bench-hooks brief, item 5). In memory, for that round only: never set_policy, so
/// nothing reaches projection_policy.json, Carlink's own SET_POLICY sync neither undoes it nor is undone by it, and
/// the next round goes back to the app's order. Carlink's debug builds write it over CH_FILE; "off" is no order.
const BENCH_ORDER_FILE: &str = "/tmp/bench_order";

/// A bench call order ([`BENCH_ORDER_FILE`]): the phones, in mgmt byte order, and how far into the walk it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchOrder {
    macs: Vec<[u8; 6]>,
    after_ms: u64,
}

/// Drop the round's bench order, if it has one, saying why (the Mac's review of the 14 Nov pushes, B1).
fn drop_bench(env: &impl Env, order: &mut Option<BenchOrder>, why: &str) {
    if let Some(b) = order.take() {
        env.bench(&format!("[bench] call order ({} phone(s), {} ms) dropped: {why}", b.macs.len(), b.after_ms));
    }
}

/// Clear the call order a round applied, saying why when its rest never walked by it (the Mac's review of 8a3a8b2, item 5).
fn abandon_order(env: &impl Env, order: &mut Option<Vec<[u8; 6]>>, walked: &mut bool, why: &str) {
    if order.take().is_some() && !*walked {
        env.bench(&format!("[bench] applied call order abandoned: {why}"));
    }
    *walked = false;
}

/// Whether the round's bench order is due: `after_ms` since the round started (`started`, `now`: [`Env::elapsed`]).
fn bench_due(order: Option<&BenchOrder>, started: Duration, now: Duration) -> bool {
    order.is_some_and(|b| now.saturating_sub(started) >= Duration::from_millis(b.after_ms))
}

/// "MAC,MAC[,...] [after_ms]", or why it isn't one.
fn parse_bench_order(text: &str) -> Result<BenchOrder, String> {
    let mut words = text.split_whitespace();
    let list = words.next().ok_or("empty")?;
    let after_ms = match words.next() {
        None => 0,
        Some(w) => w.parse::<u64>().map_err(|_| format!("after_ms {w:?} is not a number of ms"))?,
    };
    if words.next().is_some() {
        return Err("more than a list and a delay".into());
    }
    let macs = list
        .split(',')
        .map(|a| control::parse_addr(a.trim()).ok_or(format!("{a:?} is not a MAC")))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(dup) = macs.iter().enumerate().find_map(|(i, m)| macs[..i].contains(m).then_some(m)) {
        return Err(format!("{} is listed twice", control::fmt_addr(dup)));
    }
    Ok(BenchOrder { macs, after_ms })
}

/// Take [`BENCH_ORDER_FILE`] at `path`: read and remove it. A malformed one is refused, logged and removed; an absent one,
/// or "off", is no order ([`box_common::bench_text`]).
fn take_bench_order(path: &str) -> Option<BenchOrder> {
    let text = box_common::bench_text(path)?;
    let _ = std::fs::remove_file(path);
    match parse_bench_order(&text) {
        Ok(order) => {
            log(&format!(
                "[bench] call order taken from {path}: {}, {} ms into this round",
                order.macs.iter().map(control::fmt_addr).collect::<Vec<_>>().join(","),
                order.after_ms
            ));
            Some(order)
        }
        Err(why) => {
            log(&format!("[bench] {path} refused ({why}): want \"MAC,MAC[,...] [after_ms]\"; removed"));
            None
        }
    }
}

/// Round 4, item 1 (bench): Write_Page_Timeout `slots` unless the controller has it already (`*now`), saying so. True
/// when the controller pages with it now; a write that failed leaves the length as it was.
fn bench_page_slots(env: &impl Env, now: &mut u16, slots: u16, why: &str) -> bool {
    if *now != slots {
        if let Err(e) = env.page_slots(slots) {
            env.bench(&format!("[bench] round4: first_page_slots: page timeout {slots} not set ({e}); paging with the controller's"));
            return false;
        }
        *now = slots;
    }
    env.bench(&format!("[bench] round4: first_page_slots: paging {} ms {why}", slots_ms(slots)));
    true
}

/// [`run`]'s loop, against `env`: see [`run`].
fn run_loop(env: &impl Env, ctrl: &Control) {
    let round4 = env.round4();
    let mut backoff = BACKOFF_START_SECS;
    let mut streak = ResetStreak::default();
    let mut first_round = true;
    // What the controller pages with now: the reset default, until a round asks for another.
    let mut page_slots = crate::hci::PAGE_TIMEOUT_DEFAULT;
    // Round 4, item 1 (bench): the first page's own length, until the loop's first page goes; and whether the controller
    // pages with it now rather than with the round's.
    let mut first_page = round4.first_page_slots;
    let mut first_paged = false;
    // Log only on a change in the bonded/idle state, so the per-round re-read below doesn't spam the log.
    // `None` = nothing logged yet; `Some(true)` = had bonds; `Some(false)` = idle.
    let mut last_state: Option<bool> = None;
    let mut last_manual_log = false;
    let mut rounds = Rounds::default();
    // The next walk is the rest of a re-planned round, not a new round.
    let mut rest = false;
    // Bench (docs/ops/02_TESTING.md): this round's call order from BENCH_ORDER_FILE until it lands, then the order the rest
    // of the round pages in, in memory, and when the round started, for its after_ms.
    let mut round_bench: Option<BenchOrder> = None;
    let mut round_order: Option<Vec<[u8; 6]>> = None;
    let mut round_started = Duration::ZERO;
    // Whether the rest of the round walked by the applied order (abandon_order).
    let mut order_walked = false;
    // The bring-up just reset the controller: no link a cut left before it is valid now. Before the first page.
    env.forget_left_link();
    while !env.shutdown() {
        // Never fight an in-progress session (accept path or a prior reconnect). Advisory fast-path
        // only — the authoritative claim is the compare_exchange in attempt().
        if env.session_active() {
            streak.session();
            rounds.session();
            drop_bench(env, &mut round_bench, "a phone's own session came first");
            abandon_order(env, &mut round_order, &mut order_walked, "a phone's own session came first");
            rest = false;
            if !env.sleep(BACKOFF_START_SECS) {
                break;
            }
            backoff = BACKOFF_START_SECS; // a live session means the phone is here; retry promptly after it
            continue;
        }
        // Re-read the bond list every round (audit Fix #22): a phone paired via the accept path AFTER
        // boot must become reconnect-eligible without a daemon restart. The old code snapshotted bonds
        // once and returned early when the set was empty, so a box that booted unpaired never drove
        // reconnect for a later-paired phone. This just reads the persisted link-key file — cheap, no
        // paging, no side effects — now ordered by the app's policy.
        let bonds = env.bonds();
        let has_bonds = !bonds.is_empty();
        if last_state != Some(has_bonds) {
            if has_bonds {
                log(&format!("{} bonded phone(s) — driving reconnect when idle", bonds.len()));
            } else {
                log("no bonded phones — reconnect idle (accept path handles first pairing; re-checking)");
            }
            last_state = Some(has_bonds);
        }
        if !has_bonds {
            rounds.not_walked();
            drop_bench(env, &mut round_bench, "no phone is bonded");
            abandon_order(env, &mut round_order, &mut order_walked, "no phone is bonded");
            rest = false;
            if !env.sleep(BACKOFF_MAX_SECS) {
                break;
            }
            continue;
        }

        // This round plans by the policy as it is now: only a change while it pages re-plans it.
        ctrl.take_policy_changed();
        if rounds.begin() {
            streak.new_round();
        }
        // An explicit request from the device screen. Consumed here, so it fires exactly once.
        let request = ctrl.take_request();
        let auto = ctrl.policy().auto_connect;

        // Tap-to-connect with nothing pending: stay idle rather than paging phones the driver did
        // not ask for. Logged once per transition — this is a configured state, not a fault.
        if !auto && request.is_none() {
            if !last_manual_log {
                log("autoConnect off — idle until the device screen asks for a phone");
                last_manual_log = true;
            }
            drop_bench(env, &mut round_bench, "autoConnect is off");
            abandon_order(env, &mut round_order, &mut order_walked, "autoConnect is off");
            rest = false;
            rounds.not_walked();
            if !env.sleep_or_request(BACKOFF_START_SECS) {
                break;
            }
            continue;
        }
        last_manual_log = false;

        // A named request is tried alone and first. While a phone switch holds the box, only the
        // phone it holds for is paged, round after round, until it connects or the hold lapses
        // (`Control::request_switch`). Anything else walks the policy order, with a switched-to phone
        // that never reached CarPlay moved last for a while (`Control::switch_stranded`).
        // The round's phones, for the re-plan's "any left" (with the bench's phantom in a policy-order round).
        let mut round_bonds = bonds.clone();
        let mut phantom = None;
        let targets: Vec<[u8; 6]> = match request {
            Some(Some(addr)) => {
                drop_bench(env, &mut round_bench, "a Connect for one phone came first");
                abandon_order(env, &mut round_order, &mut order_walked, "a Connect for one phone came first");
                if bonds.contains(&addr) {
                    log(&format!("explicit connect request for {}", control::fmt_addr(&addr)));
                    vec![addr]
                } else {
                    // Requesting an unbonded phone cannot work — there is no link key to offer, so
                    // the connect would fail at pairing. Say so instead of silently trying it.
                    log(&format!(
                        "connect requested for {} but it is not bonded — ignoring",
                        control::fmt_addr(&addr)
                    ));
                    Vec::new()
                }
            }
            _ => match ctrl.held_for() {
                Some(held) if bonds.contains(&held) => {
                    drop_bench(env, &mut round_bench, "a switch hold came first");
                    abandon_order(env, &mut round_order, &mut order_walked, "a switch hold came first");
                    log(&format!("switch hold -- paging only {}", control::fmt_addr(&held)));
                    vec![held]
                }
                _ => {
                    // The rest of a round whose walk a Connect from the device screen with no phone named ended: its bench
                    // order is no longer due to land (the review of round 3).
                    if rest && request.is_some() {
                        drop_bench(env, &mut round_bench, "a Connect from the device screen came first");
                    }
                    if !rest {
                        // One a session or a Connect left at the last round's end goes now, saying why.
                        let why = if request.is_some() { "a Connect from the device screen came first" } else { "a new round began" };
                        drop_bench(env, &mut round_bench, why);
                        abandon_order(env, &mut round_order, &mut order_walked, why);
                        round_bench = env.bench_order();
                        round_started = env.elapsed();
                    }
                    order_walked |= round_order.is_some();
                    // Bench: the phantom first phone is one of the round's phones, ahead of the bonds, so a round that
                    // found it absent pages it last next round, as a real absent first-listed phone (the Mac's review,
                    // BH-04). Not for a Connect from the device screen, named or not.
                    phantom = if request.is_none() { env.phantom() } else { None };
                    if phantom.is_some() && !rest {
                        log("[bench] the phantom first phone is in this round (/tmp/bench_phantom_first)");
                    }
                    round_bonds = match round_order.clone() {
                        // An applied order holds the phantom of the pass it was built in: a pass without it (a Connect
                        // from the device screen, or the flag gone) pages only its bonds (the Mac's 2.6.0 review, BH-4).
                        Some(order) => order.into_iter().filter(|b| bonds.contains(b) || Some(*b) == phantom).collect(),
                        None => phantom.into_iter().chain(bonds.iter().copied().filter(|b| Some(*b) != phantom)).collect(),
                    };
                    rounds.order(round_bonds.clone(), |b| ctrl.call_order(b))
                }
            },
        };

        let want = round_page_slots(first_round, targets.len());
        if want != page_slots {
            match env.page_slots(want) {
                Ok(()) => {
                    log(&format!("paging {} ms per phone this round", u64::from(want) * 5 / 8));
                    page_slots = want;
                }
                Err(e) => log(&format!("page timeout not set ({e}); paging with the controller's")),
            }
        }
        let Walk { ran, rejected } = walk(
            &targets,
            || {
                env.shutdown()
                    || env.session_active()
                    || ctrl.policy_changed()
                    || bench_due(round_bench.as_ref(), round_started, env.elapsed())
            },
            |peer, result| {
                // Round 4, item 3 (bench): a re-page that crossed a link the controller holds goes again only with the
                // switch, once that link has cleared or come up; without it, as in 2.7.0, on to the next phone.
                if result == Attempt::Crossed {
                    if !round4.same_phone_repage || ctrl.has_request() || env.session_active() {
                        return false;
                    }
                    let found = env.settle_crossed(peer);
                    env.bench(&format!(
                        "[bench] round4: same_phone_repage: the re-page crossed a link the controller holds; {found}"
                    ));
                    return !env.shutdown() && !ctrl.has_request() && !env.session_active();
                }
                // A request or a session first: no line that pages once more when it won't (the Mac's review, A6).
                if ctrl.has_request() || env.session_active() {
                    return false;
                }
                log(&format!("{} {} -- paging it once more before the next phone", control::fmt_addr(&peer), retry_words(result)));
                env.sleep_or_request(RESET_RETRY_SECS) && !ctrl.has_request() && !env.session_active()
            },
            |peer| {
                // Round 4, item 1 (bench): the first round's first page at its own length, the round's again for the page
                // after it. With no switch nothing here runs: the round's length was written above, as in 2.7.0.
                if let Some(n) = first_page.take().filter(|_| first_round) {
                    first_paged = bench_page_slots(env, &mut page_slots, n, "for the first page of the first round");
                } else if std::mem::take(&mut first_paged) && page_slots != want {
                    bench_page_slots(env, &mut page_slots, want, "again: the first page is over");
                }
                let result = env.attempt(peer);
                let (mono, wall, bonds_at) = env.now();
                // A reset straight after another phone's page timed out is most likely the two crossing, not a
                // forgotten pairing: the phone was up on the next page every time (the Mac, 13 Nov). Not counted.
                if !rounds.paged(peer, result) {
                    log("the reset or cut came right after another phone's page timed out -- not counted toward a forgotten pairing");
                } else if Some(peer) != phantom
                    && streak.note(peer, result, bonds_at, wall, mono)
                    && !env.session_active()
                {
                    log(&format!(
                        "{} answered and reset the link, or opened no SDP channel (a cut, one a round), on {} pages in a row: it has most likely forgotten the pairing",
                        control::fmt_addr(&peer),
                        ResetStreak::TELL_AT
                    ));
                    env.bond_lost();
                }
                result
            },
        );
        // The app's policy came while this round paged (btd can page before Carlink's SET_POLICY reaches it): the phones
        // not yet paged go at once, in its order, and a round that has not ended keeps its first-round page.
        // R1: the round's pages and timeouts carry into the rest, and a change with nothing left to page ends it.
        // Bench: its call order lands the same way, after_ms into the round, in memory for the rest of it only.
        // Taken only while a phone is left to page: due at the round's last page, there is nothing for it to re-plan.
        // Not when a session or a Connect ended the walk: the next pass drops it, saying which.
        let interrupted = env.session_active() || ctrl.has_request();
        let due = !ran && !rejected && !interrupted && bench_due(round_bench.as_ref(), round_started, env.elapsed());
        let bench_now = due && rounds.left_to_page(&round_bonds);
        if due && !bench_now {
            drop_bench(env, &mut round_bench, "it came due with nothing left to page");
        }
        if bench_now {
            if let Some(b) = round_bench.take() {
                let order = control::order_bonds(&round_bonds, &b.macs);
                env.bench(&format!(
                    "[bench] call order applied {} ms into the round, for this round only (not saved): {}",
                    env.elapsed().saturating_sub(round_started).as_millis(),
                    order.iter().map(control::fmt_addr).collect::<Vec<_>>().join(",")
                ));
                round_order = Some(order);
            }
        }
        let changed = !ran && !rejected && (bench_now || ctrl.take_policy_changed());
        if rounds.end(ran, changed, &round_bonds) {
            log(if bench_now { "[bench] paging the rest of the round by its call order" } else { "the app's call order came mid-round -- paging the rest by it" });
            rest = true;
            continue;
        }
        rest = false;
        abandon_order(
            env,
            &mut round_order,
            &mut order_walked,
            if interrupted { "a session or a Connect came first" } else { "the round ended" },
        );
        let why = match (bench_due(round_bench.as_ref(), round_started, env.elapsed()), ran, rejected) {
            (_, true, _) => "a phone connected first",
            (_, _, true) => "a phone refused the pairing first",
            (false, _, _) => "the round ended before it was due",
            (true, _, _) => "the round ended",
        };
        if !interrupted {
            drop_bench(env, &mut round_bench, why);
        }
        if rejected {
            backoff = BACKOFF_MAX_SECS; // the phone said no; more paging only rotates codes
        } else if ran {
            backoff = BACKOFF_START_SECS; // reset toward prompt retry after a real attempt…
        }
        // …but ALWAYS sleep at least the current backoff (≥ BACKOFF_START_SECS) before the next attempt
        // (audit B5). `attempt()` returns true when the bt_driver was merely INVOKED, not when it reached a
        // real milestone, so a bonded phone that is RFCOMM-connectable but never completes iAP2 (accepts the
        // DLC then drops, or fails auth fast) makes each attempt return in well under a second. Without this
        // floor the old `continue` spun the loop back-to-back — pinning the single i.MX6UL core and paging
        // the phone continuously. A genuine session that just ended still retries within BACKOFF_START_SECS.
        // The single exception is FIRST_RETRY_SECS, once, after a first round that failed outright.
        let quick = first_round && !ran && !rejected;
        first_round = false;
        if quick {
            match round4.first_retry_ms {
                None => log(&format!("first page round failed -- one quick retry in {FIRST_RETRY_SECS}s")),
                Some(ms) => env.bench(&format!("[bench] round4: first_retry_ms: first page round failed -- one quick retry in {ms} ms")),
            }
        }
        let early = !rejected && env.elapsed() < EARLY_WINDOW;
        if !env.sleep_or_request_for(round_gap(quick, early, backoff, &round4)) {
            break;
        }
        if rejected {
            // The hold-off is over: drop the flag so the next attempt is judged on its own.
            env.pair_rejected_over();
        } else if !ran && !quick && !early {
            // Not in the early window: it would double under the 5 s gaps and jump to 60 s at its end (the Mac, 13 Nov).
            backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{absent_last, round_gap, round_page_slots, run_loop, Env, Rounds, FIRST_ROUND_PAGE_SLOTS};
    use super::{BACKOFF_START_SECS, EARLY_GAP_SECS, FIRST_RETRY_SECS};
    use crate::control::Control;
    use super::{default_settle_secs, failure, walk, Attempt, ResetStreak, Walk, INITIAL_SETTLE_DEFAULT_SECS};
    use std::time::{Duration, Instant};

    #[test]
    fn a_warm_controller_pages_at_once_and_a_fresh_attach_settles() {
        assert_eq!(default_settle_secs(true), 0);
        assert_eq!(default_settle_secs(false), INITIAL_SETTLE_DEFAULT_SECS);
    }

    #[test]
    fn the_first_round_over_several_phones_pages_briefly_and_later_rounds_in_full() {
        assert_eq!(round_page_slots(true, 2), FIRST_ROUND_PAGE_SLOTS);
        assert_eq!(u64::from(FIRST_ROUND_PAGE_SLOTS) * 5 / 8, 3_750, "3.75 s");
        assert_eq!(round_page_slots(false, 2), crate::hci::PAGE_TIMEOUT_DEFAULT, "a later round");
        assert_eq!(round_page_slots(true, 1), crate::hci::PAGE_TIMEOUT_DEFAULT, "one phone: nothing to hand on to");
    }

    const PHONE_A: [u8; 6] = [1, 0, 0, 0, 0, 0];
    const PHONE_B: [u8; 6] = [2, 0, 0, 0, 0, 0];

    /// Walk `[PHONE_A, PHONE_B]` with each phone's answers in turn (Failed once they run out), never stopping and
    /// always ready to retry: which phones were paged, in order, and what the walk did.
    fn walk_with(answers: &[(&[u8; 6], Attempt)]) -> (Vec<[u8; 6]>, Walk) {
        let mut queue: Vec<_> = answers.iter().map(|(p, a)| (**p, *a)).collect();
        let mut paged = Vec::new();
        let walked = walk(
            &[PHONE_A, PHONE_B],
            || false,
            |_, _| true,
            |peer| {
                paged.push(peer);
                match queue.iter().position(|(p, _)| *p == peer) {
                    Some(i) => queue.remove(i).1,
                    None => Attempt::Failed,
                }
            },
        );
        (paged, walked)
    }

    const THIRD: [u8; 6] = [3, 0, 0, 0, 0, 0];

    fn set_order(ctrl: &Control, order: &[[u8; 6]]) {
        ctrl.set_policy(crate::control::Policy { auto_connect: true, order: order.to_vec() });
    }

    /// One walk as `run()` wires it, with the bonds in the policy's order: what it paged, whether each page counted
    /// toward a forgotten pairing, and whether it re-plans.
    fn round(r: &mut Rounds, ctrl: &Control, answer: &mut dyn FnMut([u8; 6]) -> Attempt) -> (Vec<[u8; 6]>, Vec<bool>, bool) {
        ctrl.take_policy_changed();
        r.begin();
        let bonds = ctrl.policy().order;
        let targets = r.order(bonds.clone(), |b| ctrl.call_order(b));
        let (mut paged, mut counted) = (Vec::new(), Vec::new());
        let walked = walk(&targets, || ctrl.policy_changed(), |_, _| true, |peer| {
            let result = answer(peer);
            paged.push(peer);
            counted.push(r.paged(peer, result));
            result
        });
        let changed = !walked.ran && !walked.rejected && ctrl.take_policy_changed();
        let replan = r.end(walked.ran, changed, &bonds);
        (paged, counted, replan)
    }


    // ---- run_loop itself, with a fake box (the Mac's 13 Nov bench-hooks brief, item 9) ----------------------------

    /// A fake box: the bonds are the policy's order, each page answers as `answer(round, peer)` says, and a virtual
    /// clock moves 4 s for a timed-out page, 1 s for any other, and by every gap. Stops after `rounds` rounds.
    struct Fake<'a> {
        ctrl: &'a Control,
        answer: std::cell::RefCell<Box<dyn FnMut(usize, [u8; 6]) -> Attempt + 'a>>,
        rounds: usize,
        phantom: Option<[u8; 6]>,
        round: std::cell::Cell<usize>,
        clock: std::cell::Cell<u64>,
        pages: std::cell::RefCell<Vec<(usize, [u8; 6])>>,
        gaps: std::cell::RefCell<Vec<u64>>,
        bond_lost: std::cell::Cell<u32>,
        base: Instant,
        /// The rounds in which the phone's own session is live (the accept path), at the loop's top.
        sessions: Vec<usize>,
        /// A bench call order for a round, taken once ([`Env::bench_order`]).
        bench: std::cell::RefCell<Option<(usize, super::BenchOrder)>>,
        /// The app's policy as it becomes at a round: (round, autoConnect, order), applied as the gap into it ends.
        policy_at: Vec<(usize, bool, Vec<[u8; 6]>)>,
        /// The [bench] lines about the call order ([`Env::bench`]).
        logs: std::cell::RefCell<Vec<String>>,
        /// How many pages had gone at each [`Env::forget_left_link`].
        forgot_left: std::cell::RefCell<Vec<usize>>,
        /// Round 4's switches ([`Env::round4`]).
        round4: super::Round4,
        /// Each Write_Page_Timeout ([`Env::page_slots`]): how many pages had gone, and the length.
        slots: std::cell::RefCell<Vec<(usize, u16)>>,
        /// Each round's gap in ms ([`Env::sleep_or_request_for`]); `gaps` has them in whole seconds, with the other sleeps.
        gaps_ms: std::cell::RefCell<Vec<u64>>,
        /// The phones each [`Env::settle_crossed`] waited for.
        settled: std::cell::RefCell<Vec<[u8; 6]>>,
    }

    impl<'a> Fake<'a> {
        fn new(ctrl: &'a Control, rounds: usize, answer: impl FnMut(usize, [u8; 6]) -> Attempt + 'a) -> Self {
            Fake {
                ctrl,
                answer: std::cell::RefCell::new(Box::new(answer)),
                rounds,
                phantom: None,
                round: Default::default(),
                clock: Default::default(),
                pages: Default::default(),
                gaps: Default::default(),
                bond_lost: Default::default(),
                base: Instant::now(),
                sessions: Vec::new(),
                bench: Default::default(),
                policy_at: Vec::new(),
                logs: Default::default(),
                forgot_left: Default::default(),
                round4: Default::default(),
                slots: Default::default(),
                gaps_ms: Default::default(),
                settled: Default::default(),
            }
        }

        /// The phones paged in `round`, in order.
        fn paged(&self, round: usize) -> Vec<[u8; 6]> {
            self.pages.borrow().iter().filter(|(r, _)| *r == round).map(|(_, p)| *p).collect()
        }

        /// A gap of `secs` ends: the clock on, the next round, and the app's policy as it becomes there.
        fn next_round(&self, secs: u64) {
            self.gaps.borrow_mut().push(secs);
            self.clock.set(self.clock.get() + secs);
            self.round.set(self.round.get() + 1);
            for (round, auto_connect, order) in &self.policy_at {
                if *round == self.round.get() {
                    self.ctrl.set_policy(crate::control::Policy { auto_connect: *auto_connect, order: order.clone() });
                }
            }
        }
    }

    impl Env for Fake<'_> {
        fn bonds(&self) -> Vec<[u8; 6]> {
            self.ctrl.policy().order
        }
        fn attempt(&self, peer: [u8; 6]) -> Attempt {
            let round = self.round.get();
            self.pages.borrow_mut().push((round, peer));
            let a = (self.answer.borrow_mut())(round, peer);
            self.clock.set(self.clock.get() + if a == Attempt::Absent { 4 } else { 1 });
            a
        }
        fn shutdown(&self) -> bool {
            self.round.get() >= self.rounds
        }
        fn session_active(&self) -> bool {
            self.sessions.contains(&self.round.get())
        }
        fn sleep(&self, secs: u64) -> bool {
            self.sleep_or_request(secs)
        }
        fn sleep_or_request(&self, secs: u64) -> bool {
            if secs > 0 {
                self.next_round(secs);
            }
            !self.shutdown()
        }
        /// A round's gap: always the next round, one of 0 ms too (round 4's first_retry_ms).
        fn sleep_or_request_for(&self, gap: Duration) -> bool {
            self.gaps_ms.borrow_mut().push(gap.as_millis() as u64);
            self.next_round(gap.as_secs());
            !self.shutdown()
        }
        fn elapsed(&self) -> Duration {
            Duration::from_secs(self.clock.get())
        }
        fn now(&self) -> (Instant, std::time::SystemTime, Option<std::time::SystemTime>) {
            let wall = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + self.clock.get());
            (self.base + self.elapsed(), wall, Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(900_000)))
        }
        fn page_slots(&self, slots: u16) -> std::io::Result<()> {
            self.slots.borrow_mut().push((self.pages.borrow().len(), slots));
            Ok(())
        }
        fn bond_lost(&self) {
            self.bond_lost.set(self.bond_lost.get() + 1);
        }
        fn pair_rejected_over(&self) {}
        fn phantom(&self) -> Option<[u8; 6]> {
            self.phantom
        }
        fn bench_order(&self) -> Option<super::BenchOrder> {
            let mut b = self.bench.borrow_mut();
            if b.as_ref().is_some_and(|(round, _)| *round == self.round.get()) {
                b.take().map(|(_, order)| order)
            } else {
                None
            }
        }
        fn bench(&self, line: &str) {
            self.logs.borrow_mut().push(line.to_string());
        }
        fn forget_left_link(&self) {
            self.forgot_left.borrow_mut().push(self.pages.borrow().len());
        }
        fn round4(&self) -> super::Round4 {
            self.round4
        }
        fn settle_crossed(&self, peer: [u8; 6]) -> &'static str {
            self.settled.borrow_mut().push(peer);
            "no link to it: paging it again after a pause"
        }
    }

    /// Rounds 2 s, then 5 s apart for the first two minutes, then 10, 20, 40, 60: the backoff starts at its window's end.
    #[test]
    fn the_loop_waits_5_s_early_then_backs_off_from_the_start() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let fake = Fake::new(&ctrl, 30, |_, _| Attempt::Absent);
        run_loop(&fake, &ctrl);
        let gaps = fake.gaps.borrow().clone();
        assert_eq!(gaps[0], FIRST_RETRY_SECS, "one quick retry after a failed first round");
        let early = gaps[1..].iter().take_while(|g| **g == EARLY_GAP_SECS).count();
        assert!(early >= 10, "5 s apart while early: {gaps:?}");
        assert_eq!(&gaps[1 + early..1 + early + 5], &[10, 20, 40, 60, 60], "{gaps:?}");
    }

    /// The Mac's 2.6.0 review, CS-2: the loop forgets the link a cut left before the bring-up, once, before its first page.
    #[test]
    fn the_loop_forgets_the_left_link_before_its_first_page() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let fake = Fake::new(&ctrl, 2, |_, _| Attempt::Absent);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A]);
        assert_eq!(*fake.forgot_left.borrow(), vec![0], "once, with no page gone");
    }

    // ---- Round 4's bench switches (docs/ops/02_TESTING.md, "Round 4 switches") ---------------------------------------

    /// The switches as read from `set` (a name and its text), each as if from its file on flash.
    fn round4_from(set: &[(&str, &str)]) -> (super::Round4, Vec<String>) {
        super::Round4::from(|name| {
            set.iter().find(|(n, _)| *n == name).map(|(_, text)| (text.to_string(), format!("/etc/ocbm/bench_{name}")))
        })
    }

    #[test]
    fn round4_with_no_switch_set_is_the_default_and_says_nothing() {
        let (r, lines) = round4_from(&[]);
        assert_eq!(r, super::Round4::default());
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn round4_first_page_slots_is_taken_or_refused() {
        let (r, lines) = round4_from(&[("first_page_slots", "8192")]);
        assert_eq!(r.first_page_slots, Some(8192));
        assert_eq!(
            lines,
            ["[bench] round4: first_page_slots=8192 (/etc/ocbm/bench_first_page_slots): the first page of the first round 5120 ms; every other page the round's"]
        );
        for bad in ["0", "65536", "-1", "5.12", "soon", ""] {
            let (r, lines) = round4_from(&[("first_page_slots", bad)]);
            assert_eq!(r, super::Round4::default(), "{bad:?}");
            assert!(lines.len() == 1 && lines[0].contains("refused") && lines[0].ends_with("the default"), "{bad:?}: {lines:?}");
        }
    }

    /// Round 4, item 1 (bench): the first round's first page at the switch's length, the round's again for the page after
    /// it, later rounds as before. No switch: the round's length alone, written before each round as in 2.7.0.
    #[test]
    fn round4_the_first_page_alone_takes_the_switch_s_length() {
        let default = crate::hci::PAGE_TIMEOUT_DEFAULT;
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let fake = Fake::new(&ctrl, 2, |_, _| Attempt::Absent);
        run_loop(&fake, &ctrl);
        assert_eq!(*fake.slots.borrow(), vec![(0, FIRST_ROUND_PAGE_SLOTS), (2, default)], "no switch: 3.75 s in round 1, then 5.12");
        assert!(fake.logs.borrow().is_empty(), "and no [bench] line");

        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let mut fake = Fake::new(&ctrl, 2, |_, _| Attempt::Absent);
        fake.round4.first_page_slots = Some(8192);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A, PHONE_B], "still the driver's order");
        assert_eq!(
            *fake.slots.borrow(),
            vec![(0, FIRST_ROUND_PAGE_SLOTS), (0, 8192), (1, FIRST_ROUND_PAGE_SLOTS), (2, default)],
            "the first page 5.12 s, the second phone's 3.75 s, round 2 the default"
        );
        let logs = fake.logs.borrow();
        assert!(logs.iter().any(|l| l == "[bench] round4: first_page_slots: paging 5120 ms for the first page of the first round"), "{logs:?}");

        // One phone: its first page takes the switch's length too (a 10.24 s page, for an uncensored answer time).
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let mut fake = Fake::new(&ctrl, 2, |_, _| Attempt::Absent);
        fake.round4.first_page_slots = Some(16384);
        run_loop(&fake, &ctrl);
        assert_eq!(*fake.slots.borrow(), vec![(0, 16384), (1, default)]);

        // The first page answered and reset: its re-page at the round's length.
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let mut pages = 0;
        let mut fake = Fake::new(&ctrl, 1, move |_, _| {
            pages += 1;
            if pages == 1 { Attempt::Reset } else { Attempt::Absent }
        });
        fake.round4.first_page_slots = Some(8192);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A, PHONE_A, PHONE_B]);
        assert_eq!(*fake.slots.borrow(), vec![(0, FIRST_ROUND_PAGE_SLOTS), (0, 8192), (1, FIRST_ROUND_PAGE_SLOTS)]);
    }

    /// R2 and R3 through the loop: only the timed-out phone goes last next round; a lost claim moves nobody.
    #[test]
    fn the_loop_pages_a_timed_out_phone_last_and_nobody_else() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let fake = Fake::new(&ctrl, 3, |round, peer| match (round, peer) {
            (0, PHONE_A) => Attempt::Absent,
            _ => Attempt::Failed,
        });
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A, PHONE_B]);
        assert_eq!(fake.paged(1), vec![PHONE_B, PHONE_A], "the timed-out phone last");
        assert_eq!(fake.paged(2), vec![PHONE_A, PHONE_B], "a failure that wasn't a timeout moves nobody");
    }

    /// R1 through the loop: the app's order lands during the first page; the rest go at once by it, and the next round
    /// keeps it, with every phone that timed out last.
    #[test]
    fn the_loop_re_plans_by_an_order_that_lands_mid_round() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B, THIRD]);
        let c = &ctrl;
        let fake = Fake::new(&ctrl, 2, move |round, peer| {
            if round == 0 && peer == PHONE_A {
                set_order(c, &[THIRD, PHONE_B, PHONE_A]);
            }
            Attempt::Absent
        });
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A, THIRD, PHONE_B], "the rest in the new order, at once");
        assert_eq!(fake.gaps.borrow()[0], FIRST_RETRY_SECS, "one round: the re-plan is not a round of its own");
        assert_eq!(fake.paged(1), vec![THIRD, PHONE_B, PHONE_A], "all timed out: the order just set");
    }

    /// BH-04 and the crossing rule, with the bench's phantom: it is one of the round's phones, ahead of the bonds, so the
    /// round after it timed out pages it last, as a real absent first-listed phone; a reset straight after its timeout
    /// is not counted toward a forgotten pairing.
    #[test]
    fn the_phantom_goes_first_then_last_and_its_crossing_reset_is_not_counted() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_B]);
        let mut phone_b_in_round_1 = 0;
        let mut fake = Fake::new(&ctrl, 3, move |round, peer| match (round, peer) {
            (_, p) if p == super::BENCH_PHANTOM => Attempt::Absent,
            (1, _) => {
                phone_b_in_round_1 += 1;
                if phone_b_in_round_1 == 1 { Attempt::Reset } else { Attempt::Failed }
            }
            _ => Attempt::Reset,
        });
        fake.phantom = Some(super::BENCH_PHANTOM);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![super::BENCH_PHANTOM, PHONE_B, PHONE_B], "first, as an absent first-listed phone");
        assert_eq!(fake.paged(1), vec![PHONE_B, PHONE_B, super::BENCH_PHANTOM], "timed out last round: last");
        // Counted: round 0's re-page (1) and round 1's first page (2); round 1's failed re-page ends the run. Had round
        // 0's crossing reset counted, round 1's first page would have been the third, and told.
        assert_eq!(fake.bond_lost.get(), 0);

        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_B]);
        let fake = Fake::new(&ctrl, 2, |_, _| Attempt::Reset);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.bond_lost.get(), 1, "two resets a round, nothing before them: told in the second round");
    }

    /// R2 through the loop: a phone's own session (the accept path) clears who was absent: the round after it pages in
    /// the policy's order again.
    #[test]
    fn the_loop_forgets_who_was_absent_after_a_phone_s_own_session() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let mut fake = Fake::new(&ctrl, 4, |round, peer| if peer == PHONE_A && round < 2 { Attempt::Absent } else { Attempt::Failed });
        fake.sessions = vec![2];
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(1), vec![PHONE_B, PHONE_A], "timed out: last");
        assert!(fake.paged(2).is_empty(), "its own session: nothing paged");
        assert_eq!(fake.paged(3), vec![PHONE_A, PHONE_B], "after the session, the policy's order");
    }

    /// R3 through the loop: a passed-over phone stays last, even behind a phone that timed out last round.
    #[test]
    fn the_loop_keeps_a_passed_over_phone_last() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B, THIRD]);
        ctrl.pass_over(PHONE_B);
        let fake = Fake::new(&ctrl, 2, |round, peer| if round == 0 && peer == PHONE_A { Attempt::Absent } else { Attempt::Failed });
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A, THIRD, PHONE_B]);
        assert_eq!(fake.paged(1), vec![THIRD, PHONE_A, PHONE_B], "the timed-out phone last but one: the passed-over one is last");
    }

    /// The bench's call order (BH-01/02): taken for one round, it lands after_ms into the walk through the re-plan path
    /// and pages the rest of the round in its order; the next round is the app's order again, and nothing was saved.
    #[test]
    fn the_loop_pages_a_bench_order_for_one_round_in_memory() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B, THIRD]);
        let fake = Fake::new(&ctrl, 2, |_, _| Attempt::Absent);
        *fake.bench.borrow_mut() = Some((0, super::BenchOrder { macs: vec![THIRD, PHONE_B], after_ms: 1500 }));
        run_loop(&fake, &ctrl);
        // A timed-out page moves the fake clock 4 s: due after the first page.
        assert_eq!(fake.paged(0), vec![PHONE_A, THIRD, PHONE_B], "the rest of the round in the bench order");
        assert_eq!(fake.gaps.borrow()[0], FIRST_RETRY_SECS, "one round: the bench order is no round of its own");
        assert_eq!(fake.paged(1), vec![PHONE_A, PHONE_B, THIRD], "the next round, the app's order");
        assert_eq!(ctrl.policy().order, vec![PHONE_A, PHONE_B, THIRD], "never saved");
        assert!(!ctrl.take_policy_changed(), "nor seen as the app's");

        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let fake = Fake::new(&ctrl, 1, |_, _| Attempt::Absent);
        *fake.bench.borrow_mut() = Some((0, super::BenchOrder { macs: vec![PHONE_B], after_ms: 0 }));
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_B, PHONE_A], "after_ms 0: before the first page");
    }

    /// A1 through the loop: a phone a power cut left not ready, cut on both pages of one round and the first of the
    /// next before it connects (three cuts in one episode), is not told as having forgotten the pairing; one cut on
    /// every page for three rounds is, once.
    #[test]
    fn the_loop_counts_one_cut_a_round_toward_a_forgotten_pairing() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let mut pages = 0;
        let fake = Fake::new(&ctrl, 3, move |_, _| {
            pages += 1;
            if pages <= 3 { Attempt::Cut } else { Attempt::Ran }
        });
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A, PHONE_A], "a cut is paged once more");
        assert_eq!(fake.paged(1), vec![PHONE_A, PHONE_A]);
        assert_eq!(fake.bond_lost.get(), 0, "three cuts in one not-ready episode: two rounds, two counted");

        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let fake = Fake::new(&ctrl, 5, |_, _| Attempt::Cut);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.bond_lost.get(), 1, "cut on every page of three rounds and more: told once");
    }

    /// A5 (F5) through the loop: a re-plan followed by autoConnect off ends its round, so the next walked round pages
    /// every phone again, not just the rest.
    #[test]
    fn the_loop_ends_a_re_planned_round_that_goes_idle() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let c = &ctrl;
        let mut fake = Fake::new(&ctrl, 3, move |round, peer| {
            if round == 0 && peer == PHONE_A {
                c.set_policy(crate::control::Policy { auto_connect: false, order: vec![PHONE_B, PHONE_A] });
            }
            Attempt::Absent
        });
        fake.policy_at = vec![(2, true, vec![PHONE_B, PHONE_A])];
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A], "the order came mid-round, with autoConnect off: no rest");
        assert!(fake.paged(1).is_empty(), "idle");
        assert_eq!(fake.paged(2), vec![PHONE_B, PHONE_A], "a new round: every phone, the timed-out one last");

        // And a re-plan whose next pass finds no bond (the order emptied mid-round) ends its round the same way.
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let c = &ctrl;
        let mut fake = Fake::new(&ctrl, 2, move |round, peer| {
            if round == 0 && peer == PHONE_A {
                c.set_policy(crate::control::Policy { auto_connect: true, order: Vec::new() });
            }
            Attempt::Absent
        });
        fake.policy_at = vec![(1, true, vec![PHONE_B, PHONE_A])];
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A], "the order came mid-round: re-planned, then no bonds");
        assert_eq!(fake.paged(1), vec![PHONE_B, PHONE_A], "a new round: every phone, the timed-out one last");
    }

    /// B2: the phantom is never counted toward a forgotten pairing, whatever it answers.
    #[test]
    fn the_phantom_s_resets_are_never_counted() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_B]);
        let mut fake = Fake::new(&ctrl, 3, |_, peer| if peer == super::BENCH_PHANTOM { Attempt::Reset } else { Attempt::Failed });
        fake.phantom = Some(super::BENCH_PHANTOM);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![super::BENCH_PHANTOM, super::BENCH_PHANTOM, PHONE_B]);
        assert_eq!(fake.bond_lost.get(), 0, "six phantom resets in three rounds");
    }

    /// B1: a bench order that comes due with nothing left to page is dropped, not applied: no re-planned rest.
    #[test]
    fn a_bench_order_due_with_nothing_left_is_dropped() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let fake = Fake::new(&ctrl, 2, |_, _| Attempt::Absent);
        *fake.bench.borrow_mut() = Some((0, super::BenchOrder { macs: vec![PHONE_A], after_ms: 3000 }));
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A], "paged once, no rest");
        assert_eq!(fake.gaps.borrow()[0], FIRST_RETRY_SECS, "the round ended there");
        assert_eq!(fake.paged(1), vec![PHONE_A]);
        let logs = fake.logs.borrow();
        assert!(logs.iter().any(|l| l.ends_with("dropped: it came due with nothing left to page")), "{logs:?}");
        assert!(!logs.iter().any(|l| l.contains("applied")), "never applied: {logs:?}");
    }

    /// The review of round 3: a bench order whose walk a Connect from the device screen (no phone named) cut short is
    /// dropped, not applied to the re-planned rest: the rest goes in the app's order.
    #[test]
    fn a_connect_with_no_phone_named_drops_a_pending_bench_order() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B, THIRD]);
        let c = &ctrl;
        let fake = Fake::new(&ctrl, 1, move |round, peer| {
            if round == 0 && peer == PHONE_A {
                c.request_connect(None);
                c.set_policy(crate::control::Policy { auto_connect: true, order: vec![PHONE_B, THIRD, PHONE_A] });
            }
            Attempt::Absent
        });
        *fake.bench.borrow_mut() = Some((0, super::BenchOrder { macs: vec![THIRD, PHONE_B], after_ms: 1500 }));
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A, PHONE_B, THIRD], "the rest in the app's new order, not the bench's");
        let logs = fake.logs.borrow();
        assert!(logs.iter().any(|l| l.ends_with("dropped: a Connect from the device screen came first")), "{logs:?}");
        assert!(!logs.iter().any(|l| l.contains("applied")), "never applied: {logs:?}");
    }

    /// The Mac's 2.6.0 review, BH-4: an order applied in a round with the phantom holds it; a Connect from the device screen
    /// that comes with a policy change during the rest's walk re-plans the rest again, and that pass pages no phantom.
    #[test]
    fn a_connect_after_an_applied_order_never_pages_the_phantom() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B, THIRD]);
        let c = &ctrl;
        let mut fake = Fake::new(&ctrl, 1, move |round, peer| {
            if round == 0 && peer == THIRD {
                c.request_connect(None);
                c.set_policy(crate::control::Policy { auto_connect: true, order: vec![PHONE_B, PHONE_A, THIRD] });
            }
            Attempt::Absent
        });
        fake.phantom = Some(super::BENCH_PHANTOM);
        *fake.bench.borrow_mut() = Some((0, super::BenchOrder { macs: vec![THIRD], after_ms: 0 }));
        run_loop(&fake, &ctrl);
        let phantom = crate::control::fmt_addr(&super::BENCH_PHANTOM);
        let logs = fake.logs.borrow();
        assert!(logs.iter().any(|l| l.contains("call order applied") && l.contains(&phantom)), "applied with the phantom: {logs:?}");
        assert_eq!(fake.paged(0), vec![THIRD, PHONE_A, PHONE_B], "the order applied before any page, then the Connect's rest: no phantom");
    }

    /// /tmp/bench_order: read and removed; "off" is no order and is left; a malformed one is refused and removed.
    #[test]
    fn a_bench_order_file_is_taken_once_and_a_bad_one_refused() {
        let path = std::env::temp_dir().join(format!("btd_bench_order_{}", std::process::id()));
        let p = path.to_str().unwrap();
        let a = [0x01, 0xee, 0xdd, 0xcc, 0xbb, 0xaa];
        let b = [0x02, 0xee, 0xdd, 0xcc, 0xbb, 0xaa];
        std::fs::write(&path, "AA:BB:CC:DD:EE:01,aa:bb:cc:dd:ee:02 1500\n").unwrap();
        assert_eq!(super::take_bench_order(p), Some(super::BenchOrder { macs: vec![a, b], after_ms: 1500 }));
        assert!(!path.exists(), "taken once");
        std::fs::write(&path, "AA:BB:CC:DD:EE:01").unwrap();
        assert_eq!(super::take_bench_order(p).map(|o| o.after_ms), Some(0), "after_ms defaults to 0");
        std::fs::write(&path, "off\n").unwrap();
        assert_eq!(super::take_bench_order(p), None);
        assert!(path.exists(), "off is no order, and is left");
        for bad in ["", "AA:BB:CC:DD:EE", "AA:BB:CC:DD:EE:01 soon", "AA:BB:CC:DD:EE:01 1 2", "AA:BB:CC:DD:EE:01,aa:bb:cc:dd:ee:01"] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(super::take_bench_order(p), None, "{bad:?}");
            assert!(!path.exists(), "{bad:?}: refused and removed");
        }
    }

    /// The Mac's 13 Nov bench and review (R1): the app's order arrives while the first phone is paged. The rest go at
    /// once in the new order; the round's timeouts carry over, so the next round keeps that order; and a reset at the
    /// head of the rest, straight after the timeout, is still a crossing.
    #[test]
    fn a_re_planned_round_keeps_what_it_learned() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B, THIRD]);
        let mut r = Rounds::default();
        let (paged, _, replan) = round(&mut r, &ctrl, &mut |peer| {
            if peer == PHONE_A {
                set_order(&ctrl, &[THIRD, PHONE_A, PHONE_B]);
            }
            Attempt::Absent
        });
        assert_eq!((paged, replan), (vec![PHONE_A], true), "the walk stops after the phone in hand");
        let (paged, counted, replan) =
            round(&mut r, &ctrl, &mut |peer| if peer == THIRD { Attempt::Reset } else { Attempt::Absent });
        assert_eq!(paged, vec![THIRD, THIRD, PHONE_B], "the rest, in the new order, skipping the phone paged");
        assert_eq!(counted, vec![false, true, true], "the reset straight after the timeout is a crossing");
        assert!(!replan);
        let (paged, _, _) = round(&mut r, &ctrl, &mut |_| Attempt::Absent);
        assert_eq!(paged, vec![THIRD, PHONE_A, PHONE_B], "all absent last round: the order just set");
    }

    /// R1: a change that lands during the last phone's page leaves nothing to re-plan, and the round's timeouts stand.
    #[test]
    fn a_change_with_nothing_left_to_page_ends_the_round() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let mut r = Rounds::default();
        let (paged, _, replan) = round(&mut r, &ctrl, &mut |peer| {
            if peer == PHONE_B {
                set_order(&ctrl, &[PHONE_B, PHONE_A]);
            }
            if peer == PHONE_A { Attempt::Absent } else { Attempt::Failed }
        });
        assert_eq!((paged, replan), (vec![PHONE_A, PHONE_B], false));
        let (paged, _, _) = round(&mut r, &ctrl, &mut |_| Attempt::Failed);
        assert_eq!(paged, vec![PHONE_B, PHONE_A], "the new order; the absent phone last, where it is anyway");
    }

    /// R2: only a timed-out page demotes, and a session clears it. The Mac, 13 Nov: a crossing reset is not counted.
    #[test]
    fn only_a_page_timeout_demotes_and_a_session_clears_it() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let mut r = Rounds::default();
        let (_, counted, _) =
            round(&mut r, &ctrl, &mut |peer| if peer == PHONE_A { Attempt::Absent } else { Attempt::Reset });
        assert_eq!(counted, vec![true, false, true], "the first reset crossed the timeout; its re-page counts");
        assert_eq!(round(&mut r, &ctrl, &mut |_| Attempt::Failed).0, vec![PHONE_B, PHONE_A], "the absent phone goes last");
        assert_eq!(round(&mut r, &ctrl, &mut |_| Attempt::Failed).0, vec![PHONE_A, PHONE_B], "a lost claim demotes nobody");
        round(&mut r, &ctrl, &mut |peer| if peer == PHONE_A { Attempt::Absent } else { Attempt::Failed });
        r.session();
        assert_eq!(round(&mut r, &ctrl, &mut |_| Attempt::Failed).0, vec![PHONE_A, PHONE_B], "a session: nobody is absent");
        round(&mut r, &ctrl, &mut |peer| if peer == PHONE_A { Attempt::Absent } else { Attempt::Ran });
        assert_eq!(round(&mut r, &ctrl, &mut |_| Attempt::Failed).0, vec![PHONE_A, PHONE_B], "a round that ran clears it");
    }

    /// F5: a re-plan whose rest never walks (autoConnect off, no bonds, a session) ends its round, so a later round pages
    /// every phone again, the timed-out one last.
    #[test]
    fn a_re_plan_whose_rest_never_walks_ends_its_round() {
        let mut r = Rounds::default();
        r.begin();
        r.paged(PHONE_A, Attempt::Absent);
        assert!(r.end(false, true, &[PHONE_A, PHONE_B]), "the app's order came mid-round: a re-plan");
        r.not_walked();
        r.begin();
        assert_eq!(r.order(vec![PHONE_A, PHONE_B], |b| b), vec![PHONE_B, PHONE_A], "every phone, the timed-out one last");

        let mut r = Rounds::default();
        r.begin();
        r.paged(PHONE_A, Attempt::Absent);
        assert!(r.end(false, true, &[PHONE_A, PHONE_B]));
        r.session();
        r.begin();
        assert_eq!(r.order(vec![PHONE_A, PHONE_B], |b| b), vec![PHONE_A, PHONE_B], "after a session nobody is absent");
    }

    /// R3: the demotion comes before the pass-over, so a passed-over phone stays last.
    #[test]
    fn a_passed_over_phone_stays_last() {
        let r = Rounds { absent_before: vec![PHONE_A], ..Rounds::default() };
        let pass_over_phone_b = |b: Vec<[u8; 6]>| {
            let (y, rest): (Vec<_>, Vec<_>) = b.into_iter().partition(|p| *p == PHONE_B);
            rest.into_iter().chain(y).collect()
        };
        assert_eq!(r.order(vec![PHONE_A, PHONE_B, THIRD], pass_over_phone_b), vec![THIRD, PHONE_A, PHONE_B]);
    }

    /// The Mac, 13 Nov: rounds 5 s apart in the early window, and a backoff that starts at its window's end.
    #[test]
    fn rounds_are_5_s_apart_early_then_back_off_from_the_start() {
        let s = Duration::from_secs;
        let none = super::Round4::default();
        assert_eq!(round_gap(true, true, 10, &none), s(FIRST_RETRY_SECS));
        assert_eq!(round_gap(false, true, 10, &none), s(EARLY_GAP_SECS));
        assert_eq!(round_gap(false, false, BACKOFF_START_SECS, &none), s(BACKOFF_START_SECS));
    }

    /// Round 4, item 2 (bench): the quick retry's pause as the switch says, once; nothing else moves.
    #[test]
    fn round4_first_retry_ms_sets_the_quick_retry_alone() {
        let ms = Duration::from_millis;
        let r4 = super::Round4 { first_retry_ms: Some(0), ..Default::default() };
        assert_eq!(round_gap(true, true, 10, &r4), ms(0));
        assert_eq!(round_gap(false, true, 10, &r4), Duration::from_secs(EARLY_GAP_SECS), "the early gap as before");
        assert_eq!(round_gap(false, false, 40, &r4), Duration::from_secs(40), "the backoff as before");
        let (r, lines) = round4_from(&[("first_retry_ms", "0")]);
        assert_eq!(r.first_retry_ms, Some(0));
        assert_eq!(
            lines,
            ["[bench] round4: first_retry_ms=0 (/etc/ocbm/bench_first_retry_ms): after a failed first round the next one in 0 ms, once (default 2000 ms)"]
        );
        for bad in ["60001", "-5", "2s", ""] {
            assert_eq!(round4_from(&[("first_retry_ms", bad)]).0, super::Round4::default(), "{bad:?}");
        }
    }

    /// Round 4, item 5 (bench): the early window's gap as the switch says, still under the backoff; then the backoff from
    /// the window's end, as before.
    #[test]
    fn round4_early_gap_secs_sets_the_early_window_s_gap() {
        let s = Duration::from_secs;
        let r4 = super::Round4 { early_gap_secs: Some(1), ..Default::default() };
        assert_eq!(round_gap(false, true, 10, &r4), s(1));
        assert_eq!(round_gap(false, false, 20, &r4), s(20), "past the window: the backoff");
        assert_eq!(round_gap(true, true, 10, &r4), s(FIRST_RETRY_SECS), "the quick retry as before");
        let r4 = super::Round4 { early_gap_secs: Some(30), ..Default::default() };
        assert_eq!(round_gap(false, true, 10, &r4), s(10), "never past the backoff");
        let (r, lines) = round4_from(&[("early_gap_secs", "1")]);
        assert_eq!(r.early_gap_secs, Some(1));
        assert_eq!(
            lines,
            ["[bench] round4: early_gap_secs=1 (/etc/ocbm/bench_early_gap_secs): rounds 1 s apart in the first 120 s (default 5 s)"]
        );
        for bad in ["0", "61", "0.5", ""] {
            assert_eq!(round4_from(&[("early_gap_secs", bad)]).0, super::Round4::default(), "{bad:?}");
        }

        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let mut fake = Fake::new(&ctrl, 60, |_, _| Attempt::Absent);
        fake.round4.early_gap_secs = Some(1);
        run_loop(&fake, &ctrl);
        let gaps = fake.gaps.borrow().clone();
        assert_eq!(gaps[0], FIRST_RETRY_SECS, "the quick retry first");
        let early = gaps[1..].iter().take_while(|g| **g == 1).count();
        assert!(early >= 20, "1 s apart while early: {gaps:?}");
        assert_eq!(&gaps[1 + early..1 + early + 5], &[10, 20, 40, 60, 60], "then the backoff, as before: {gaps:?}");
    }

    /// Round 4, item 2 through the loop: 0 ms, the next round at once and only once; no switch, 2 s as in 2.7.0.
    #[test]
    fn round4_the_loop_retries_a_failed_first_round_as_the_switch_says() {
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let fake = Fake::new(&ctrl, 3, |_, _| Attempt::Absent);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.gaps_ms.borrow()[..2], [2000, 5000], "no switch: 2 s, then the early window's 5 s");

        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A]);
        let mut fake = Fake::new(&ctrl, 3, |_, _| Attempt::Absent);
        fake.round4.first_retry_ms = Some(0);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.gaps_ms.borrow()[..2], [0, 5000], "at once, once");
        assert_eq!((fake.paged(1), fake.paged(2)), (vec![PHONE_A], vec![PHONE_A]), "each round pages");
        let logs = fake.logs.borrow();
        assert!(logs.iter().any(|l| l == "[bench] round4: first_retry_ms: first page round failed -- one quick retry in 0 ms"), "{logs:?}");
    }

    /// The dev bench, 2 Nov: after a power cut the first-called iPhone answered and reset the link, and the
    /// walk handed CarPlay to the second phone. It is paged once more first.
    #[test]
    fn a_phone_that_answers_and_resets_is_paged_once_more_before_the_next() {
        let (paged, walked) = walk_with(&[(&PHONE_A, Attempt::Reset), (&PHONE_A, Attempt::Ran)]);
        assert_eq!(&paged[..2], &[PHONE_A, PHONE_A]);
        assert!(walked.ran);
        let (paged, walked) = walk_with(&[(&PHONE_A, Attempt::Reset), (&PHONE_A, Attempt::Reset), (&PHONE_B, Attempt::Ran)]);
        assert_eq!(paged, vec![PHONE_A, PHONE_A, PHONE_B], "only once: then the next phone");
        assert!(walked.ran);
    }

    /// The 5 Nov bench: an iPhone that had forgotten the adapter answered six pages and reset each. A still-paired
    /// one also answered and hung up once after a power cut (3 Nov), so only a streak of three says so, once.
    #[test]
    fn three_answered_and_reset_pages_in_a_row_say_the_bond_is_lost_once() {
        let mut s = ResetStreak::default();
        let m = Instant::now();
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Some(t - Duration::from_secs(3600)); // bonded an hour ago
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(s.note(PHONE_A, Attempt::Reset, old, t, m), "the third in a row tells");
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m), "once per streak");
        assert!(!s.note(PHONE_A, Attempt::Failed, old, t, m), "its own failure ends its run");
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Ran, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m), "a session between: the count starts again");
        assert!(s.note(PHONE_A, Attempt::Reset, old, t, m));
    }

    /// A1: a cut counts once a round toward a forgotten pairing. A not-ready phone cut on every page of two rounds
    /// (the re-page is at once) is not told; cut on every page of three rounds, it is, once. A reset still counts each.
    #[test]
    fn a_cut_counts_once_a_round_toward_a_forgotten_pairing() {
        let m = Instant::now();
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Some(t - Duration::from_secs(3600));
        let mut s = ResetStreak::default();
        for _ in 0..2 {
            s.new_round();
            assert!(!s.note(PHONE_A, Attempt::Cut, old, t, m));
            assert!(!s.note(PHONE_A, Attempt::Cut, old, t, m), "the round's second cut is not counted");
        }
        s.new_round();
        assert!(s.note(PHONE_A, Attempt::Cut, old, t, m), "the third round's cut tells");
        assert!(!s.note(PHONE_A, Attempt::Cut, old, t, m), "once");

        let mut s = ResetStreak::default();
        s.new_round();
        assert!(!s.note(PHONE_A, Attempt::Cut, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(s.note(PHONE_A, Attempt::Reset, old, t, m), "a cut and two resets in one round: three");
        assert_eq!(failure(&crate::sdp_client::cut_error()), Attempt::Cut);
    }

    /// The 1.0.4 bench, 5 Nov: re-paired, CarPlay mis-tapped on the iPhone, which then answered-and-reset two pages
    /// within a minute. A new bond starts the count over and holds the news back while it is new.
    #[test]
    fn a_new_bond_starts_the_count_over_and_holds_it_back() {
        let mut s = ResetStreak::default();
        let m = Instant::now();
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Some(t - Duration::from_secs(3600));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        let new = Some(t); // NEW_LINK_KEY: the store is rewritten
        assert!(!s.note(PHONE_A, Attempt::Reset, new, t, m), "the count starts over");
        assert!(!s.note(PHONE_A, Attempt::Reset, new, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, new, t + Duration::from_secs(60), m), "three, but the bond is a minute old");
        let later = t + ResetStreak::NEW_BOND_HOLD + Duration::from_secs(1);
        assert!(s.note(PHONE_A, Attempt::Reset, new, later, m), "still resetting once the bond is no longer new: tell");
    }

    /// The Mac, 6 Nov: after a restart the box clock reads 2020 until Carlink's SETTIME lands, so a bond can be
    /// stamped later than "now". Its age is unknown: the hold runs from when btd first saw it, and ends.
    #[test]
    fn a_bond_stamped_ahead_of_the_clock_is_held_from_when_it_was_seen_not_forever() {
        let mut s = ResetStreak::default();
        let m = Instant::now();
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ahead = Some(t + Duration::from_secs(365 * 24 * 3600)); // written before the restart, by the right clock
        assert!(!s.note(PHONE_A, Attempt::Reset, ahead, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, ahead, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, ahead, t, m + Duration::from_secs(60)), "held while it is new to btd");
        let after = m + ResetStreak::NEW_BOND_HOLD + Duration::from_secs(1);
        assert!(s.note(PHONE_A, Attempt::Reset, ahead, t, after), "the hold ends on the monotonic clock");
    }

    /// The Mac, 6 Nov: two bonded iPhones, the one that forgot the adapter here and the other away. Every round pages
    /// both, and the absent one's failure used to end the count, so it never told. Each phone keeps its own run.
    #[test]
    fn another_phone_away_does_not_hide_the_one_that_forgot() {
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Some(t - Duration::from_secs(3600));
        let m = Instant::now();
        for order in [[PHONE_A, PHONE_B], [PHONE_B, PHONE_A]] {
            let mut s = ResetStreak::default();
            let mut told = Vec::new();
            for round in 1..=3 {
                walk(&order, || false, |_, _| true, |peer| {
                    let result = if peer == PHONE_A { Attempt::Reset } else { Attempt::Failed };
                    if s.note(peer, result, old, t, m) {
                        told.push((round, peer));
                    }
                    result
                });
            }
            assert_eq!(told, vec![(2, PHONE_A)], "{order:?}: told once, in the second round (pages 3 and 4 are its)");
        }
        let mut s = ResetStreak::default();
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        s.session(); // it started a session itself: it knows the adapter
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m), "the run starts over after a session");
        assert!(!s.note(PHONE_B, Attempt::Ran, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m), "any phone's session ends every run");
        assert!(s.note(PHONE_A, Attempt::Reset, old, t, m));
    }

    /// The Mac, 13 Nov: after a failed first round the absent first-listed phone was paged first again.
    #[test]
    fn a_phone_whose_page_failed_goes_last_next_round() {
        const THIRD: [u8; 6] = [3, 0, 0, 0, 0, 0];
        assert_eq!(absent_last(vec![PHONE_A, PHONE_B, THIRD], &[PHONE_A]), vec![PHONE_B, THIRD, PHONE_A]);
        assert_eq!(absent_last(vec![PHONE_A, PHONE_B, THIRD], &[THIRD, PHONE_A]), vec![PHONE_B, PHONE_A, THIRD], "in their own order");
        assert_eq!(absent_last(vec![PHONE_A, PHONE_B], &[]), vec![PHONE_A, PHONE_B]);
    }

    #[test]
    fn an_absent_phone_is_not_paged_twice() {
        let (paged, _) = walk_with(&[(&PHONE_A, Attempt::Failed), (&PHONE_B, Attempt::Ran)]);
        assert_eq!(paged, vec![PHONE_A, PHONE_B]);
    }

    #[test]
    fn a_request_or_a_session_during_the_pause_ends_the_walk() {
        let mut paged = Vec::new();
        let walked = walk(&[PHONE_A, PHONE_B], || false, |_, _| false, |peer| {
            paged.push(peer);
            Attempt::Reset
        });
        assert_eq!(paged, vec![PHONE_A]);
        assert_eq!(walked, Walk::default());
    }

    #[test]
    fn a_rejected_pairing_still_ends_the_walk() {
        let (paged, walked) = walk_with(&[(&PHONE_A, Attempt::PairRejected)]);
        assert_eq!(paged, vec![PHONE_A]);
        assert!(walked.rejected && !walked.ran);
    }

    /// Round 4, item 3: EALREADY, EMLINK and EBUSY are a link the controller still holds; a crossing ends a phone's run
    /// toward a forgotten pairing as a failure does.
    #[test]
    fn a_link_the_controller_still_holds_is_a_crossing_and_counts_as_a_failure() {
        for errno in [libc::EALREADY, libc::EMLINK, libc::EBUSY] {
            assert_eq!(failure(&std::io::Error::from_raw_os_error(errno)), Attempt::Crossed, "errno {errno}");
        }
        let mut s = ResetStreak::default();
        let m = Instant::now();
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Some(t - Duration::from_secs(3600));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Crossed, old, t, m), "a crossing ends its run");
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m));
        assert!(!s.note(PHONE_A, Attempt::Reset, old, t, m), "so two resets after it are not three");
    }

    /// Round 4, item 3: the re-page after a reset or a cut that crossed pages the same phone again while `pause` says so,
    /// CROSSED_REPAGES more times at most; a pause that says no (no switch) goes on to the next phone, as a failure did;
    /// and a first page that crossed is never paged again.
    #[test]
    fn a_crossed_re_page_pages_the_same_phone_again_only_while_pause_says_so() {
        let (paged, walked) = walk_with(&[(&PHONE_A, Attempt::Cut), (&PHONE_A, Attempt::Crossed), (&PHONE_A, Attempt::Ran)]);
        assert_eq!(&paged[..3], &[PHONE_A, PHONE_A, PHONE_A]);
        assert!(walked.ran);
        let crossings = [(&PHONE_A, Attempt::Reset), (&PHONE_A, Attempt::Crossed), (&PHONE_A, Attempt::Crossed), (&PHONE_A, Attempt::Crossed),
            (&PHONE_A, Attempt::Crossed), (&PHONE_B, Attempt::Ran)];
        let (paged, walked) = walk_with(&crossings);
        assert_eq!(paged, vec![PHONE_A, PHONE_A, PHONE_A, PHONE_A, PHONE_B], "two more at most, then the next phone");
        assert!(walked.ran);
        let mut paged = Vec::new();
        let mut a = [Attempt::Cut, Attempt::Crossed].into_iter();
        let walked = walk(&[PHONE_A, PHONE_B], || false, |_, r| r != Attempt::Crossed, |peer| {
            paged.push(peer);
            if peer == PHONE_A { a.next().unwrap_or(Attempt::Failed) } else { Attempt::Ran }
        });
        assert_eq!(paged, vec![PHONE_A, PHONE_A, PHONE_B], "pause says no for the crossing: the next phone");
        assert!(walked.ran);
        let (paged, _) = walk_with(&[(&PHONE_A, Attempt::Crossed), (&PHONE_B, Attempt::Ran)]);
        assert_eq!(&paged[..2], &[PHONE_A, PHONE_B], "a first page that crossed: no re-page");
    }

    /// Round 4, item 3 through the loop: the truck's 21 Nov case, a cut and then a re-page that crossed. No switch: the other
    /// phone next, as in 2.7.0. With it: the same phone again once the link is looked at, and it connects.
    #[test]
    fn round4_the_loop_pages_a_crossed_phone_again_only_with_the_switch() {
        let answers = || {
            let mut n = 0;
            move |_: usize, peer: [u8; 6]| {
                if peer != PHONE_A {
                    return Attempt::Absent;
                }
                n += 1;
                match n {
                    1 => Attempt::Cut,
                    2 => Attempt::Crossed,
                    _ => Attempt::Ran,
                }
            }
        };
        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let fake = Fake::new(&ctrl, 1, answers());
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![PHONE_A, PHONE_A, PHONE_B], "no switch: on to the other phone");
        assert!(fake.settled.borrow().is_empty() && fake.logs.borrow().is_empty());

        let ctrl = Control::default();
        set_order(&ctrl, &[PHONE_A, PHONE_B]);
        let mut fake = Fake::new(&ctrl, 1, answers());
        fake.round4.same_phone_repage = true;
        run_loop(&fake, &ctrl);
        assert_eq!(&fake.paged(0)[..3], &[PHONE_A, PHONE_A, PHONE_A], "the same phone again");
        assert_eq!(*fake.settled.borrow(), vec![PHONE_A], "once, after its link was looked at");
        let logs = fake.logs.borrow();
        assert!(logs.iter().any(|l| l.starts_with("[bench] round4: same_phone_repage: the re-page crossed")), "{logs:?}");
        let (r, lines) = round4_from(&[("same_phone_repage", "")]);
        assert!(r.same_phone_repage && lines[0].starts_with("[bench] round4: same_phone_repage (/etc/ocbm/bench_same_phone_repage)"));
    }

    /// Round 4, item 4 (bench): the limit is taken in 1000-8000 ms; an RFCOMM connect past it is paged once more as a reset
    /// is, counts once a round toward a forgotten pairing as a cut does (sharing the cut's), and right after another
    /// phone's timeout is a crossing, never counted.
    #[test]
    fn round4_a_stalled_rfcomm_connect_is_paged_again_and_counted_as_a_cut() {
        let (r, lines) = round4_from(&[("rfcomm_limit_ms", "1500")]);
        assert_eq!(r.rfcomm_limit_ms, Some(1500));
        assert!(lines[0].starts_with("[bench] round4: rfcomm_limit_ms=1500 (/etc/ocbm/bench_rfcomm_limit_ms): an RFCOMM connect not up in 1500 ms"), "{lines:?}");
        for bad in ["999", "8001", "1.5", ""] {
            assert_eq!(round4_from(&[("rfcomm_limit_ms", bad)]).0, super::Round4::default(), "{bad:?}");
        }

        let (paged, walked) = walk_with(&[(&PHONE_A, Attempt::Stalled), (&PHONE_A, Attempt::Ran)]);
        assert_eq!(&paged[..2], &[PHONE_A, PHONE_A], "paged once more");
        assert!(walked.ran);
        let (paged, _) = walk_with(&[(&PHONE_A, Attempt::Stalled), (&PHONE_A, Attempt::Stalled), (&PHONE_B, Attempt::Ran)]);
        assert_eq!(paged, vec![PHONE_A, PHONE_A, PHONE_B], "only once: then the next phone");

        let m = Instant::now();
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Some(t - Duration::from_secs(3600));
        let mut s = ResetStreak::default();
        for _ in 0..2 {
            s.new_round();
            assert!(!s.note(PHONE_A, Attempt::Stalled, old, t, m));
            assert!(!s.note(PHONE_A, Attempt::Stalled, old, t, m), "the round's second is not counted");
            assert!(!s.note(PHONE_A, Attempt::Cut, old, t, m), "nor a cut beside it: one a round");
        }
        s.new_round();
        assert!(s.note(PHONE_A, Attempt::Stalled, old, t, m), "the third round's tells");

        let mut r = Rounds::default();
        r.begin();
        r.paged(PHONE_A, Attempt::Absent);
        assert!(!r.paged(PHONE_B, Attempt::Stalled), "straight after another phone's timeout: a crossing, not counted");
        assert_eq!(super::retry_words(Attempt::Stalled), "answered and opened no RFCOMM channel within the bench limit");
    }

    #[test]
    fn only_a_reset_link_counts_as_a_reset() {
        let reset = std::io::Error::from_raw_os_error(libc::ECONNRESET);
        assert_eq!(failure(&reset), Attempt::Reset);
        assert_eq!(failure(&std::io::Error::from_raw_os_error(libc::EHOSTDOWN)), Attempt::Absent, "a page timeout");
        for errno in [libc::ETIMEDOUT, libc::ECONNREFUSED] {
            assert_eq!(failure(&std::io::Error::from_raw_os_error(errno)), Attempt::Failed, "errno {errno}");
        }
    }
}
