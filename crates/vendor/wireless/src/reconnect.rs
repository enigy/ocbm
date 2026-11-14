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
    /// A walk starts: a new round forgets the last one's pages; the rest of a re-planned round keeps them.
    fn begin(&mut self) {
        if !self.replanning {
            self.absent_now.clear();
            self.paged.clear();
            self.prev_absent = false;
        }
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
        let crossing = result == Attempt::Reset && self.prev_absent;
        self.prev_absent = result == Attempt::Absent;
        if self.prev_absent && !self.absent_now.contains(&peer) {
            self.absent_now.push(peer);
        }
        !crossing
    }

    /// The walk ended. True when it re-plans: the app's order came mid-round (`changed`) and a phone is left to
    /// page. Otherwise the round is over, and a session in it clears what it learned (R2).
    fn end(&mut self, ran: bool, changed: bool, bonds: &[[u8; 6]]) -> bool {
        self.replanning = !ran && changed && bonds.iter().any(|p| !self.paged.contains(p));
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
fn round_gap(quick: bool, early: bool, backoff: u64) -> u64 {
    if quick {
        FIRST_RETRY_SECS
    } else if early {
        EARLY_GAP_SECS.min(backoff)
    } else {
        backoff
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
/// `session_active` refuses to page over it. 1 s keeps a moment for that inbound connect.
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
    /// The phone rejected the SSP re-pair (`rfcomm::connect_to` → `PermissionDenied`):
    /// paging it again just produces another prompt it cannot accept. Hold off for the maximum
    /// backoff and let the operator re-pair from the iPhone.
    PairRejected,
}

/// [`Attempt::Reset`] when the phone reset the link, [`Attempt::Absent`] when the page timed out, else
/// [`Attempt::Failed`].
fn failure(e: &std::io::Error) -> Attempt {
    if e.kind() == std::io::ErrorKind::ConnectionReset {
        Attempt::Reset
    } else if e.raw_os_error() == Some(libc::EHOSTDOWN) {
        Attempt::Absent
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
}

#[derive(Debug, Default)]
struct Run {
    count: u32,
    told: bool,
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
            Attempt::Ran => {
                self.runs.clear();
                return false;
            }
            Attempt::Failed | Attempt::Absent | Attempt::PairRejected => {
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
}

/// When the link-key store was last written: a new bond rewrites it.
fn bonds_written() -> Option<std::time::SystemTime> {
    std::fs::metadata(bt_common::ssp_agent::link_key_store()).and_then(|m| m.modified()).ok()
}

/// Page `targets` in order until `stop` says not to go on. A phone that answers and resets the link
/// ([`Attempt::Reset`]) is paged once more, after `pause` (false: go no further), before the next one.
fn walk(
    targets: &[[u8; 6]],
    mut stop: impl FnMut() -> bool,
    mut pause: impl FnMut([u8; 6]) -> bool,
    mut page: impl FnMut([u8; 6]) -> Attempt,
) -> Walk {
    let mut out = Walk::default();
    for &peer in targets {
        if stop() {
            break;
        }
        let mut result = page(peer);
        if result == Attempt::Reset {
            if !pause(peer) {
                break;
            }
            result = page(peer);
        }
        match result {
            Attempt::Ran => out.ran = true,
            Attempt::Failed | Attempt::Absent | Attempt::Reset => {}
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
) -> Attempt {
    let services = match sdp_client::query(peer, CONNECT_TIMEOUT_SECS) {
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
    let sock = match crate::rfcomm_connect(peer, channel, CONNECT_TIMEOUT_SECS) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            log(&format!(
                "RFCOMM connect to channel {channel} aborted: {e} — holding off {BACKOFF_MAX_SECS}s; \
                 re-pair on the iPhone (Settings ▸ Bluetooth) or switch pairing to Just-Works"
            ));
            return Attempt::PairRejected;
        }
        Err(e) => {
            log(&format!("RFCOMM connect to channel {channel} failed: {e}"));
            return failure(&e);
        }
    };
    log(&format!("RFCOMM connected OUT to the phone (ch {channel}) — starting iAP2 handshake"));
    crate::startup::mark("phone");
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
    let env = OnBox { name, shutdown, session_active, ctrl, started: Instant::now() };
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
}

/// The real [`Env`].
struct OnBox<'a> {
    name: &'a str,
    shutdown: &'a Arc<AtomicBool>,
    session_active: &'a Arc<AtomicBool>,
    ctrl: &'a Arc<Control>,
    started: Instant,
}

impl Env for OnBox<'_> {
    fn bonds(&self) -> Vec<[u8; 6]> {
        self.ctrl.ordered_bonds()
    }
    fn attempt(&self, peer: [u8; 6]) -> Attempt {
        let result = attempt(peer, self.name, self.shutdown, self.session_active, self.ctrl);
        match result {
            Attempt::Absent => crate::startup::tally("absent"),
            Attempt::Reset => crate::startup::tally("reset"),
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
}

/// The bench flag that pages [`BENCH_PHANTOM`] first in every policy-order round, as an absent first-listed phone:
/// no airplane mode, no Bluetooth toggling (the Mac's 13 Nov bench-hooks brief, item 4). Bench only; /tmp, so a reboot
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

/// [`run`]'s loop, against `env`: see [`run`].
fn run_loop(env: &impl Env, ctrl: &Control) {
    let mut backoff = BACKOFF_START_SECS;
    let mut streak = ResetStreak::default();
    let mut first_round = true;
    // What the controller pages with now: the reset default, until a round asks for another.
    let mut page_slots = crate::hci::PAGE_TIMEOUT_DEFAULT;
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
    while !env.shutdown() {
        // Never fight an in-progress session (accept path or a prior reconnect). Advisory fast-path
        // only — the authoritative claim is the compare_exchange in attempt().
        if env.session_active() {
            streak.session();
            rounds.session();
            (rest, round_bench, round_order) = (false, None, None);
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
            (rest, round_bench, round_order) = (false, None, None);
            if !env.sleep(BACKOFF_MAX_SECS) {
                break;
            }
            continue;
        }

        // This round plans by the policy as it is now: only a change while it pages re-plans it.
        ctrl.take_policy_changed();
        rounds.begin();
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
            (rest, round_bench, round_order) = (false, None, None);
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
                    log(&format!("switch hold -- paging only {}", control::fmt_addr(&held)));
                    vec![held]
                }
                _ => {
                    if !rest {
                        round_bench = env.bench_order();
                        round_order = None;
                        round_started = env.elapsed();
                    }
                    // Bench: the phantom first phone is one of the round's phones, ahead of the bonds, so a round that
                    // found it absent pages it last next round, as a real absent first-listed phone (the Mac's review,
                    // BH-04).
                    phantom = env.phantom();
                    if phantom.is_some() && !rest {
                        log("[bench] the phantom first phone is in this round (/tmp/bench_phantom_first)");
                    }
                    round_bonds = round_order.clone().unwrap_or_else(|| {
                        phantom.into_iter().chain(bonds.iter().copied().filter(|b| Some(*b) != phantom)).collect()
                    });
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
            |peer| {
                log(&format!(
                    "{} answered and reset the link -- paging it once more in {RESET_RETRY_SECS}s before the next phone",
                    control::fmt_addr(&peer)
                ));
                env.sleep_or_request(RESET_RETRY_SECS) && !ctrl.has_request() && !env.session_active()
            },
            |peer| {
                let result = env.attempt(peer);
                let (mono, wall, bonds_at) = env.now();
                // A reset straight after another phone's page timed out is most likely the two crossing, not a
                // forgotten pairing: the phone was up on the next page every time (the Mac, 13 Nov). Not counted.
                if !rounds.paged(peer, result) {
                    log("the reset came right after another phone's page timed out -- not counted toward a forgotten pairing");
                } else if Some(peer) != phantom
                    && streak.note(peer, result, bonds_at, wall, mono)
                    && !env.session_active()
                {
                    log(&format!(
                        "{} answered and reset the link on {} pages in a row: it has most likely forgotten the pairing",
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
        let bench_now = !ran && !rejected && bench_due(round_bench.as_ref(), round_started, env.elapsed());
        if bench_now {
            if let Some(b) = round_bench.take() {
                let order = control::order_bonds(&round_bonds, &b.macs);
                log(&format!(
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
        round_order = None;
        if let Some(b) = round_bench.take() {
            log(&format!("[bench] the round ended before its call order was due ({} ms): dropped", b.after_ms));
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
            log(&format!("first page round failed -- one quick retry in {FIRST_RETRY_SECS}s"));
        }
        let early = !rejected && env.elapsed() < EARLY_WINDOW;
        if !env.sleep_or_request(round_gap(quick, early, backoff)) {
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

    const SCOTT: [u8; 6] = [1, 0, 0, 0, 0, 0];
    const YODA: [u8; 6] = [2, 0, 0, 0, 0, 0];

    /// Walk `[SCOTT, YODA]` with each phone's answers in turn (Failed once they run out), never stopping and
    /// always ready to retry: which phones were paged, in order, and what the walk did.
    fn walk_with(answers: &[(&[u8; 6], Attempt)]) -> (Vec<[u8; 6]>, Walk) {
        let mut queue: Vec<_> = answers.iter().map(|(p, a)| (**p, *a)).collect();
        let mut paged = Vec::new();
        let walked = walk(
            &[SCOTT, YODA],
            || false,
            |_| true,
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
        let walked = walk(&targets, || ctrl.policy_changed(), |_| true, |peer| {
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
            }
        }

        /// The phones paged in `round`, in order.
        fn paged(&self, round: usize) -> Vec<[u8; 6]> {
            self.pages.borrow().iter().filter(|(r, _)| *r == round).map(|(_, p)| *p).collect()
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
                self.gaps.borrow_mut().push(secs);
                self.clock.set(self.clock.get() + secs);
                self.round.set(self.round.get() + 1);
            }
            !self.shutdown()
        }
        fn elapsed(&self) -> Duration {
            Duration::from_secs(self.clock.get())
        }
        fn now(&self) -> (Instant, std::time::SystemTime, Option<std::time::SystemTime>) {
            let wall = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + self.clock.get());
            (self.base + self.elapsed(), wall, Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(900_000)))
        }
        fn page_slots(&self, _slots: u16) -> std::io::Result<()> {
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
    }

    /// Rounds 2 s, then 5 s apart for the first two minutes, then 10, 20, 40, 60: the backoff starts at its window's end.
    #[test]
    fn the_loop_waits_5_s_early_then_backs_off_from_the_start() {
        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT]);
        let fake = Fake::new(&ctrl, 30, |_, _| Attempt::Absent);
        run_loop(&fake, &ctrl);
        let gaps = fake.gaps.borrow().clone();
        assert_eq!(gaps[0], FIRST_RETRY_SECS, "one quick retry after a failed first round");
        let early = gaps[1..].iter().take_while(|g| **g == EARLY_GAP_SECS).count();
        assert!(early >= 10, "5 s apart while early: {gaps:?}");
        assert_eq!(&gaps[1 + early..1 + early + 5], &[10, 20, 40, 60, 60], "{gaps:?}");
    }

    /// R2 and R3 through the loop: only the timed-out phone goes last next round; a lost claim moves nobody.
    #[test]
    fn the_loop_pages_a_timed_out_phone_last_and_nobody_else() {
        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT, YODA]);
        let fake = Fake::new(&ctrl, 3, |round, peer| match (round, peer) {
            (0, SCOTT) => Attempt::Absent,
            _ => Attempt::Failed,
        });
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![SCOTT, YODA]);
        assert_eq!(fake.paged(1), vec![YODA, SCOTT], "the timed-out phone last");
        assert_eq!(fake.paged(2), vec![SCOTT, YODA], "a failure that wasn't a timeout moves nobody");
    }

    /// R1 through the loop: the app's order lands during the first page; the rest go at once by it, and the next round
    /// keeps it, with every phone that timed out last.
    #[test]
    fn the_loop_re_plans_by_an_order_that_lands_mid_round() {
        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT, YODA, THIRD]);
        let c = &ctrl;
        let fake = Fake::new(&ctrl, 2, move |round, peer| {
            if round == 0 && peer == SCOTT {
                set_order(c, &[THIRD, YODA, SCOTT]);
            }
            Attempt::Absent
        });
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![SCOTT, THIRD, YODA], "the rest in the new order, at once");
        assert_eq!(fake.gaps.borrow()[0], FIRST_RETRY_SECS, "one round: the re-plan is not a round of its own");
        assert_eq!(fake.paged(1), vec![THIRD, YODA, SCOTT], "all timed out: the order just set");
    }

    /// BH-04 and the crossing rule, with the bench's phantom: it is one of the round's phones, ahead of the bonds, so the
    /// round after it timed out pages it last, as a real absent first-listed phone; a reset straight after its timeout
    /// is not counted toward a forgotten pairing.
    #[test]
    fn the_phantom_goes_first_then_last_and_its_crossing_reset_is_not_counted() {
        let ctrl = Control::default();
        set_order(&ctrl, &[YODA]);
        let mut yoda_in_round_1 = 0;
        let mut fake = Fake::new(&ctrl, 3, move |round, peer| match (round, peer) {
            (_, p) if p == super::BENCH_PHANTOM => Attempt::Absent,
            (1, _) => {
                yoda_in_round_1 += 1;
                if yoda_in_round_1 == 1 { Attempt::Reset } else { Attempt::Failed }
            }
            _ => Attempt::Reset,
        });
        fake.phantom = Some(super::BENCH_PHANTOM);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![super::BENCH_PHANTOM, YODA, YODA], "first, as an absent first-listed phone");
        assert_eq!(fake.paged(1), vec![YODA, YODA, super::BENCH_PHANTOM], "timed out last round: last");
        // Counted: round 0's re-page (1) and round 1's first page (2); round 1's failed re-page ends the run. Had round
        // 0's crossing reset counted, round 1's first page would have been the third, and told.
        assert_eq!(fake.bond_lost.get(), 0);

        let ctrl = Control::default();
        set_order(&ctrl, &[YODA]);
        let fake = Fake::new(&ctrl, 2, |_, _| Attempt::Reset);
        run_loop(&fake, &ctrl);
        assert_eq!(fake.bond_lost.get(), 1, "two resets a round, nothing before them: told in the second round");
    }

    /// R2 through the loop: a phone's own session (the accept path) clears who was absent: the round after it pages in
    /// the policy's order again.
    #[test]
    fn the_loop_forgets_who_was_absent_after_a_phone_s_own_session() {
        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT, YODA]);
        let mut fake = Fake::new(&ctrl, 4, |round, peer| if peer == SCOTT && round < 2 { Attempt::Absent } else { Attempt::Failed });
        fake.sessions = vec![2];
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(1), vec![YODA, SCOTT], "timed out: last");
        assert!(fake.paged(2).is_empty(), "its own session: nothing paged");
        assert_eq!(fake.paged(3), vec![SCOTT, YODA], "after the session, the policy's order");
    }

    /// R3 through the loop: a passed-over phone stays last, even behind a phone that timed out last round.
    #[test]
    fn the_loop_keeps_a_passed_over_phone_last() {
        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT, YODA, THIRD]);
        ctrl.pass_over(YODA);
        let fake = Fake::new(&ctrl, 2, |round, peer| if round == 0 && peer == SCOTT { Attempt::Absent } else { Attempt::Failed });
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![SCOTT, THIRD, YODA]);
        assert_eq!(fake.paged(1), vec![THIRD, SCOTT, YODA], "the timed-out phone last but one: the passed-over one is last");
    }

    /// The bench's call order (BH-01/02): taken for one round, it lands after_ms into the walk through the re-plan path
    /// and pages the rest of the round in its order; the next round is the app's order again, and nothing was saved.
    #[test]
    fn the_loop_pages_a_bench_order_for_one_round_in_memory() {
        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT, YODA, THIRD]);
        let fake = Fake::new(&ctrl, 2, |_, _| Attempt::Absent);
        *fake.bench.borrow_mut() = Some((0, super::BenchOrder { macs: vec![THIRD, YODA], after_ms: 1500 }));
        run_loop(&fake, &ctrl);
        // A timed-out page moves the fake clock 4 s: due after the first page.
        assert_eq!(fake.paged(0), vec![SCOTT, THIRD, YODA], "the rest of the round in the bench order");
        assert_eq!(fake.gaps.borrow()[0], FIRST_RETRY_SECS, "one round: the bench order is no round of its own");
        assert_eq!(fake.paged(1), vec![SCOTT, YODA, THIRD], "the next round, the app's order");
        assert_eq!(ctrl.policy().order, vec![SCOTT, YODA, THIRD], "never saved");
        assert!(!ctrl.take_policy_changed(), "nor seen as the app's");

        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT, YODA]);
        let fake = Fake::new(&ctrl, 1, |_, _| Attempt::Absent);
        *fake.bench.borrow_mut() = Some((0, super::BenchOrder { macs: vec![YODA], after_ms: 0 }));
        run_loop(&fake, &ctrl);
        assert_eq!(fake.paged(0), vec![YODA, SCOTT], "after_ms 0: before the first page");
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
        for bad in ["", "AA:BB:CC:DD:EE", "AA:BB:CC:DD:EE:01 soon", "AA:BB:CC:DD:EE:01 1 2"] {
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
        set_order(&ctrl, &[SCOTT, YODA, THIRD]);
        let mut r = Rounds::default();
        let (paged, _, replan) = round(&mut r, &ctrl, &mut |peer| {
            if peer == SCOTT {
                set_order(&ctrl, &[THIRD, SCOTT, YODA]);
            }
            Attempt::Absent
        });
        assert_eq!((paged, replan), (vec![SCOTT], true), "the walk stops after the phone in hand");
        let (paged, counted, replan) =
            round(&mut r, &ctrl, &mut |peer| if peer == THIRD { Attempt::Reset } else { Attempt::Absent });
        assert_eq!(paged, vec![THIRD, THIRD, YODA], "the rest, in the new order, skipping the phone paged");
        assert_eq!(counted, vec![false, true, true], "the reset straight after the timeout is a crossing");
        assert!(!replan);
        let (paged, _, _) = round(&mut r, &ctrl, &mut |_| Attempt::Absent);
        assert_eq!(paged, vec![THIRD, SCOTT, YODA], "all absent last round: the order just set");
    }

    /// R1: a change that lands during the last phone's page leaves nothing to re-plan, and the round's timeouts stand.
    #[test]
    fn a_change_with_nothing_left_to_page_ends_the_round() {
        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT, YODA]);
        let mut r = Rounds::default();
        let (paged, _, replan) = round(&mut r, &ctrl, &mut |peer| {
            if peer == YODA {
                set_order(&ctrl, &[YODA, SCOTT]);
            }
            if peer == SCOTT { Attempt::Absent } else { Attempt::Failed }
        });
        assert_eq!((paged, replan), (vec![SCOTT, YODA], false));
        let (paged, _, _) = round(&mut r, &ctrl, &mut |_| Attempt::Failed);
        assert_eq!(paged, vec![YODA, SCOTT], "the new order; the absent phone last, where it is anyway");
    }

    /// R2: only a timed-out page demotes, and a session clears it. The Mac, 13 Nov: a crossing reset is not counted.
    #[test]
    fn only_a_page_timeout_demotes_and_a_session_clears_it() {
        let ctrl = Control::default();
        set_order(&ctrl, &[SCOTT, YODA]);
        let mut r = Rounds::default();
        let (_, counted, _) =
            round(&mut r, &ctrl, &mut |peer| if peer == SCOTT { Attempt::Absent } else { Attempt::Reset });
        assert_eq!(counted, vec![true, false, true], "the first reset crossed the timeout; its re-page counts");
        assert_eq!(round(&mut r, &ctrl, &mut |_| Attempt::Failed).0, vec![YODA, SCOTT], "the absent phone goes last");
        assert_eq!(round(&mut r, &ctrl, &mut |_| Attempt::Failed).0, vec![SCOTT, YODA], "a lost claim demotes nobody");
        round(&mut r, &ctrl, &mut |peer| if peer == SCOTT { Attempt::Absent } else { Attempt::Failed });
        r.session();
        assert_eq!(round(&mut r, &ctrl, &mut |_| Attempt::Failed).0, vec![SCOTT, YODA], "a session: nobody is absent");
        round(&mut r, &ctrl, &mut |peer| if peer == SCOTT { Attempt::Absent } else { Attempt::Ran });
        assert_eq!(round(&mut r, &ctrl, &mut |_| Attempt::Failed).0, vec![SCOTT, YODA], "a round that ran clears it");
    }

    /// F5: a re-plan whose rest never walks (autoConnect off, no bonds, a session) ends its round, so a later round pages
    /// every phone again, the timed-out one last.
    #[test]
    fn a_re_plan_whose_rest_never_walks_ends_its_round() {
        let mut r = Rounds::default();
        r.begin();
        r.paged(SCOTT, Attempt::Absent);
        assert!(r.end(false, true, &[SCOTT, YODA]), "the app's order came mid-round: a re-plan");
        r.not_walked();
        r.begin();
        assert_eq!(r.order(vec![SCOTT, YODA], |b| b), vec![YODA, SCOTT], "every phone, the timed-out one last");

        let mut r = Rounds::default();
        r.begin();
        r.paged(SCOTT, Attempt::Absent);
        assert!(r.end(false, true, &[SCOTT, YODA]));
        r.session();
        r.begin();
        assert_eq!(r.order(vec![SCOTT, YODA], |b| b), vec![SCOTT, YODA], "after a session nobody is absent");
    }

    /// R3: the demotion comes before the pass-over, so a passed-over phone stays last.
    #[test]
    fn a_passed_over_phone_stays_last() {
        let r = Rounds { absent_before: vec![SCOTT], ..Rounds::default() };
        let pass_over_yoda = |b: Vec<[u8; 6]>| {
            let (y, rest): (Vec<_>, Vec<_>) = b.into_iter().partition(|p| *p == YODA);
            rest.into_iter().chain(y).collect()
        };
        assert_eq!(r.order(vec![SCOTT, YODA, THIRD], pass_over_yoda), vec![THIRD, SCOTT, YODA]);
    }

    /// The Mac, 13 Nov: rounds 5 s apart in the early window, and a backoff that starts at its window's end.
    #[test]
    fn rounds_are_5_s_apart_early_then_back_off_from_the_start() {
        assert_eq!(round_gap(true, true, 10), FIRST_RETRY_SECS);
        assert_eq!(round_gap(false, true, 10), EARLY_GAP_SECS);
        assert_eq!(round_gap(false, false, BACKOFF_START_SECS), BACKOFF_START_SECS);
    }

    /// The dev bench, 2 Nov: after a power cut the first-called iPhone answered and reset the link, and the
    /// walk handed CarPlay to the second phone. It is paged once more first.
    #[test]
    fn a_phone_that_answers_and_resets_is_paged_once_more_before_the_next() {
        let (paged, walked) = walk_with(&[(&SCOTT, Attempt::Reset), (&SCOTT, Attempt::Ran)]);
        assert_eq!(&paged[..2], &[SCOTT, SCOTT]);
        assert!(walked.ran);
        let (paged, walked) = walk_with(&[(&SCOTT, Attempt::Reset), (&SCOTT, Attempt::Reset), (&YODA, Attempt::Ran)]);
        assert_eq!(paged, vec![SCOTT, SCOTT, YODA], "only once: then the next phone");
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
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        assert!(s.note(SCOTT, Attempt::Reset, old, t, m), "the third in a row tells");
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m), "once per streak");
        assert!(!s.note(SCOTT, Attempt::Failed, old, t, m), "its own failure ends its run");
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        assert!(!s.note(SCOTT, Attempt::Ran, old, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m), "a session between: the count starts again");
        assert!(s.note(SCOTT, Attempt::Reset, old, t, m));
    }

    /// The 1.0.4 bench, 5 Nov: re-paired, CarPlay mis-tapped on the iPhone, which then answered-and-reset two pages
    /// within a minute. A new bond starts the count over and holds the news back while it is new.
    #[test]
    fn a_new_bond_starts_the_count_over_and_holds_it_back() {
        let mut s = ResetStreak::default();
        let m = Instant::now();
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Some(t - Duration::from_secs(3600));
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        let new = Some(t); // NEW_LINK_KEY: the store is rewritten
        assert!(!s.note(SCOTT, Attempt::Reset, new, t, m), "the count starts over");
        assert!(!s.note(SCOTT, Attempt::Reset, new, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, new, t + Duration::from_secs(60), m), "three, but the bond is a minute old");
        let later = t + ResetStreak::NEW_BOND_HOLD + Duration::from_secs(1);
        assert!(s.note(SCOTT, Attempt::Reset, new, later, m), "still resetting once the bond is no longer new: tell");
    }

    /// The Mac, 6 Nov: after a restart the box clock reads 2020 until Carlink's SETTIME lands, so a bond can be
    /// stamped later than "now". Its age is unknown: the hold runs from when btd first saw it, and ends.
    #[test]
    fn a_bond_stamped_ahead_of_the_clock_is_held_from_when_it_was_seen_not_forever() {
        let mut s = ResetStreak::default();
        let m = Instant::now();
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ahead = Some(t + Duration::from_secs(365 * 24 * 3600)); // written before the restart, by the right clock
        assert!(!s.note(SCOTT, Attempt::Reset, ahead, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, ahead, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, ahead, t, m + Duration::from_secs(60)), "held while it is new to btd");
        let after = m + ResetStreak::NEW_BOND_HOLD + Duration::from_secs(1);
        assert!(s.note(SCOTT, Attempt::Reset, ahead, t, after), "the hold ends on the monotonic clock");
    }

    /// The Mac, 6 Nov: two bonded iPhones, the one that forgot the adapter here and the other away. Every round pages
    /// both, and the absent one's failure used to end the count, so it never told. Each phone keeps its own run.
    #[test]
    fn another_phone_away_does_not_hide_the_one_that_forgot() {
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Some(t - Duration::from_secs(3600));
        let m = Instant::now();
        for order in [[SCOTT, YODA], [YODA, SCOTT]] {
            let mut s = ResetStreak::default();
            let mut told = Vec::new();
            for round in 1..=3 {
                walk(&order, || false, |_| true, |peer| {
                    let result = if peer == SCOTT { Attempt::Reset } else { Attempt::Failed };
                    if s.note(peer, result, old, t, m) {
                        told.push((round, peer));
                    }
                    result
                });
            }
            assert_eq!(told, vec![(2, SCOTT)], "{order:?}: told once, in the second round (pages 3 and 4 are its)");
        }
        let mut s = ResetStreak::default();
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        s.session(); // it started a session itself: it knows the adapter
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m), "the run starts over after a session");
        assert!(!s.note(YODA, Attempt::Ran, old, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m));
        assert!(!s.note(SCOTT, Attempt::Reset, old, t, m), "any phone's session ends every run");
        assert!(s.note(SCOTT, Attempt::Reset, old, t, m));
    }

    /// The Mac, 13 Nov: after a failed first round the absent first-listed phone was paged first again.
    #[test]
    fn a_phone_whose_page_failed_goes_last_next_round() {
        const THIRD: [u8; 6] = [3, 0, 0, 0, 0, 0];
        assert_eq!(absent_last(vec![SCOTT, YODA, THIRD], &[SCOTT]), vec![YODA, THIRD, SCOTT]);
        assert_eq!(absent_last(vec![SCOTT, YODA, THIRD], &[THIRD, SCOTT]), vec![YODA, SCOTT, THIRD], "in their own order");
        assert_eq!(absent_last(vec![SCOTT, YODA], &[]), vec![SCOTT, YODA]);
    }

    #[test]
    fn an_absent_phone_is_not_paged_twice() {
        let (paged, _) = walk_with(&[(&SCOTT, Attempt::Failed), (&YODA, Attempt::Ran)]);
        assert_eq!(paged, vec![SCOTT, YODA]);
    }

    #[test]
    fn a_request_or_a_session_during_the_pause_ends_the_walk() {
        let mut paged = Vec::new();
        let walked = walk(&[SCOTT, YODA], || false, |_| false, |peer| {
            paged.push(peer);
            Attempt::Reset
        });
        assert_eq!(paged, vec![SCOTT]);
        assert_eq!(walked, Walk::default());
    }

    #[test]
    fn a_rejected_pairing_still_ends_the_walk() {
        let (paged, walked) = walk_with(&[(&SCOTT, Attempt::PairRejected)]);
        assert_eq!(paged, vec![SCOTT]);
        assert!(walked.rejected && !walked.ran);
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
