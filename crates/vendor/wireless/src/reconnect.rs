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

/// `order` with the phones whose page failed last round moved last, in their order. After a failed first round the
/// next one paged the absent first-listed phone first again, now for the full 5.12 s; and the present phone,
/// paged straight after an absent one's timeout, answered and reset the link in 4 of 17 connects (the Mac, 13 Nov).
fn absent_last(order: Vec<[u8; 6]>, absent: &[[u8; 6]]) -> Vec<[u8; 6]> {
    let (gone, here): (Vec<_>, Vec<_>) = order.into_iter().partition(|p| absent.contains(p));
    here.into_iter().chain(gone).collect()
}

/// A round's order less the phones it had paged before the app's policy re-planned it.
fn unpaged(order: Vec<[u8; 6]>, paged_before: &[[u8; 6]]) -> Vec<[u8; 6]> {
    order.into_iter().filter(|p| !paged_before.contains(p)).collect()
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
const RESET_RETRY_SECS: u64 = 2;
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
/// 13 Nov connect-time brief).
const INITIAL_SETTLE_DEFAULT_SECS: u64 = 1;

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
    /// SDP/RFCOMM failed for an ordinary reason (absent phone, page timeout, lost claim).
    Failed,
    /// The phone answered the page and then reset the link (`ConnectionReset` from SDP or RFCOMM): it is
    /// there but not ready, as an iPhone still letting go of a session a power cut ended. Paged once more
    /// before the next phone ([`walk`]).
    Reset,
    /// The phone rejected the SSP re-pair (`rfcomm::connect_to` → `PermissionDenied`):
    /// paging it again just produces another prompt it cannot accept. Hold off for the maximum
    /// backoff and let the operator re-pair from the iPhone.
    PairRejected,
}

/// [`Attempt::Reset`] when the phone reset the link, else [`Attempt::Failed`].
fn failure(e: &std::io::Error) -> Attempt {
    if e.kind() == std::io::ErrorKind::ConnectionReset {
        Attempt::Reset
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
            Attempt::Failed | Attempt::PairRejected => {
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
            Attempt::Failed | Attempt::Reset => {}
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

    let mut backoff = BACKOFF_START_SECS;
    let mut streak = ResetStreak::default();
    let mut first_round = true;
    // What the controller pages with now: the reset default, until a round asks for another.
    let mut page_slots = crate::hci::PAGE_TIMEOUT_DEFAULT;
    // Log only on a change in the bonded/idle state, so the per-round re-read below doesn't spam the log.
    // `None` = nothing logged yet; `Some(true)` = had bonds; `Some(false)` = idle.
    let mut last_state: Option<bool> = None;
    let mut last_manual_log = false;
    // The phones a round had already paged when the app's policy arrived mid-round: the re-planned rest skips them.
    let mut paged_before: Vec<[u8; 6]> = Vec::new();
    // The phones whose page failed last round: the next one pages them last ([`absent_last`]).
    let mut absent_before: Vec<[u8; 6]> = Vec::new();
    let started = Instant::now();
    while !shutdown.load(Ordering::Relaxed) {
        // Never fight an in-progress session (accept path or a prior reconnect). Advisory fast-path
        // only — the authoritative claim is the compare_exchange in attempt().
        if session_active.load(Ordering::Relaxed) {
            streak.session();
            if !interruptible_sleep(BACKOFF_START_SECS, shutdown) {
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
        let bonds = ctrl.ordered_bonds();
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
            if !interruptible_sleep(BACKOFF_MAX_SECS, shutdown) {
                break;
            }
            continue;
        }

        // This round plans by the policy as it is now: only a change while it pages re-plans it.
        ctrl.take_policy_changed();
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
            if !sleep_or_request(BACKOFF_START_SECS, shutdown, ctrl) {
                break;
            }
            continue;
        }
        last_manual_log = false;

        // A named request is tried alone and first. While a phone switch holds the box, only the
        // phone it holds for is paged, round after round, until it connects or the hold lapses
        // (`Control::request_switch`). Anything else walks the policy order, with a switched-to phone
        // that never reached CarPlay moved last for a while (`Control::switch_stranded`).
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
                _ => absent_last(unpaged(ctrl.call_order(bonds.clone()), &paged_before), &absent_before),
            },
        };
        paged_before.clear();

        let want = round_page_slots(first_round, targets.len());
        if want != page_slots {
            match crate::hci::dev_id(crate::HCI_DEV).and_then(|dev| crate::hci::write_page_timeout(dev, want)) {
                Ok(()) => {
                    log(&format!("paging {} ms per phone this round", u64::from(want) * 5 / 8));
                    page_slots = want;
                }
                Err(e) => log(&format!("page timeout not set ({e}); paging with the controller's")),
            }
        }
        let mut paged = Vec::new();
        let mut absent_now = Vec::new();
        let mut prev_failed = false;
        let Walk { ran, rejected } = walk(
            &targets,
            || shutdown.load(Ordering::Relaxed) || session_active.load(Ordering::Relaxed) || ctrl.policy_changed(),
            |peer| {
                log(&format!(
                    "{} answered and reset the link -- paging it once more in {RESET_RETRY_SECS}s before the next phone",
                    control::fmt_addr(&peer)
                ));
                sleep_or_request(RESET_RETRY_SECS, shutdown, ctrl)
                    && !ctrl.has_request()
                    && !session_active.load(Ordering::Relaxed)
            },
            |peer| {
                paged.push(peer);
                let result = attempt(peer, name, shutdown, session_active, ctrl);
                // A reset straight after another phone's page timed out is most likely the two crossing, not a
                // forgotten pairing: the phone was up on the next page every time (the Mac, 13 Nov). Not counted.
                let after_absent = result == Attempt::Reset && prev_failed;
                prev_failed = result == Attempt::Failed;
                if prev_failed {
                    absent_now.push(peer);
                }
                if after_absent {
                    log("the reset came right after another phone's page timed out -- not counted toward a forgotten pairing");
                } else if streak.note(peer, result, bonds_written(), std::time::SystemTime::now(), Instant::now())
                    && !session_active.load(Ordering::Relaxed)
                {
                    log(&format!(
                        "{} answered and reset the link on {} pages in a row: it has most likely forgotten the pairing",
                        control::fmt_addr(&peer),
                        ResetStreak::TELL_AT
                    ));
                    bt_driver::publish_bt_phase(ocbm_proto::BTP_BOND_LOST);
                }
                result
            },
        );
        // The app's policy came while this round paged (btd can page before Carlink's SET_POLICY reaches it): the phones
        // not yet paged go at once, in its order, and a round that has not ended keeps its first-round page.
        if !ran && !rejected && ctrl.take_policy_changed() {
            log("the app's call order came mid-round -- paging the rest by it");
            paged_before = paged;
            continue;
        }
        absent_before = absent_now;
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
        // In the first [`EARLY_WINDOW`] after a host came, rounds stay [`EARLY_GAP_SECS`] apart, still a floor.
        let early = !rejected && started.elapsed() < EARLY_WINDOW;
        let gap = if quick {
            FIRST_RETRY_SECS
        } else if early {
            EARLY_GAP_SECS.min(backoff)
        } else {
            backoff
        };
        if !sleep_or_request(gap, shutdown, ctrl) {
            break;
        }
        if rejected {
            // The hold-off is over: drop the flag so the next attempt is judged on its own.
            let _ = std::fs::remove_file(bt_common::rfcomm::PAIR_REJECTED_FLAG);
        } else if !ran && !quick {
            backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
        }
    }
    log("reconnect loop exiting");
}

#[cfg(test)]
mod tests {
    use super::{absent_last, round_page_slots, unpaged, FIRST_ROUND_PAGE_SLOTS};
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

    /// The Mac's 13 Nov bench: btd can page before Carlink's SET_POLICY reaches it. A policy that changes while
    /// a round pages ends the walk after the phone in hand, and the rest go in the new order, skipping that one.
    #[test]
    fn a_policy_that_changes_mid_round_re_plans_the_phones_not_yet_paged() {
        const THIRD: [u8; 6] = [3, 0, 0, 0, 0, 0];
        let ctrl = crate::control::Control::default();
        ctrl.take_policy_changed();
        let mut paged = Vec::new();
        let walked = walk(
            &[SCOTT, YODA, THIRD],
            || ctrl.policy_changed(),
            |_| true,
            |peer| {
                paged.push(peer);
                if peer == SCOTT {
                    // The app's order arrives while the first phone is paged.
                    ctrl.set_policy(crate::control::Policy { auto_connect: true, order: vec![THIRD, SCOTT, YODA] });
                }
                Attempt::Failed
            },
        );
        assert_eq!(paged, vec![SCOTT], "the walk stops after the phone in hand");
        assert!(!walked.ran && !walked.rejected);
        assert!(ctrl.take_policy_changed());
        assert_eq!(unpaged(vec![THIRD, SCOTT, YODA], &paged), vec![THIRD, YODA], "the rest, in the new order");
        assert!(!ctrl.policy_changed(), "taken once: the re-planned round is not stopped again");
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
        for errno in [libc::EHOSTDOWN, libc::ETIMEDOUT, libc::ECONNREFUSED] {
            assert_eq!(failure(&std::io::Error::from_raw_os_error(errno)), Attempt::Failed, "errno {errno}");
        }
    }
}
