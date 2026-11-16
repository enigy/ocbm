//! The one `[startup]` line per wireless session: at the first video frame to the host, the stages btd noted in
//! [`MARKS`] (its `startup` module: host present, Bluetooth up, phone connected, Wi-Fi handoff) and this frame,
//! each as +ms from the host's arrival. A drive log then carries the truck's own connect time (the Mac's 13 Nov
//! brief). Nothing is printed for a session btd didn't note (a wired one), or for a record from an older session.
//!
//! Right after it, in a wireless carplayd, the `[timeline] v1 ...` line (`box_common::timeline`): every
//! critical-path event on the uptime clock, from btd's record and from carplayd's own [`Stage`]s and [`Chip`] times,
//! kept here in memory.

use portable_atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// btd's record of this session's stages (`wireless::startup::MARKS`).
pub const MARKS: &str = "/tmp/startup_marks";

/// A record whose host arrived longer ago than this is an older session's, not this one's.
const STALE_MS: u64 = 10 * 60 * 1000;

static PRINTED: AtomicBool = AtomicBool::new(false);
/// This carplayd's screen nudges (`session.rs` nudge_screen_back): the 0.5 s one and the 3 s one.
static NUDGES: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// The box log, scanned from where it stood when the host came (btd's `boxlog_at` mark) for the
/// PSK-mismatch lines since then (C-2). Bounded: at most [`SCAN_MAX`] bytes are read.
const BOX_LOG: &str = "/tmp/box.log";
const SCAN_MAX: u64 = 512 * 1024;

/// A screen nudge sent: the first (0.5 s) or the second (3 s).
pub fn nudged(second: bool) {
    NUDGES[usize::from(second)].fetch_add(1, Ordering::Relaxed);
}

/// carplayd's own events for the `[timeline]` line (`box_common::timeline::EVENTS`), the first of each standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// carplayd started (main).
    CarplaydStart,
    /// The `_carplay-ctrl` of the phone btd names first resolved (discovery).
    SvcSeen,
    /// The connect-out (`GET /ctrl-int/1/connect`) sent (discovery).
    Dial,
    /// Pair-verify OK: the control channel encrypted.
    PvDone,
    /// `POST /auth-setup` received.
    AuthIn,
    /// `/auth-setup` answered.
    AuthOut,
    /// RECORD answered.
    Record,
    /// The main screen's (type 110) SETUP answered.
    ScreenSetup,
    /// The first video frame to the host.
    FirstFrame,
}

impl Stage {
    const ALL: [Stage; 9] = [
        Stage::CarplaydStart,
        Stage::SvcSeen,
        Stage::Dial,
        Stage::PvDone,
        Stage::AuthIn,
        Stage::AuthOut,
        Stage::Record,
        Stage::ScreenSetup,
        Stage::FirstFrame,
    ];

    /// Its key in the line.
    fn key(self) -> &'static str {
        match self {
            Stage::CarplaydStart => "carplayd_start",
            Stage::SvcSeen => "svc_seen",
            Stage::Dial => "dial",
            Stage::PvDone => "pv_done",
            Stage::AuthIn => "auth_in",
            Stage::AuthOut => "auth_out",
            Stage::Record => "record",
            Stage::ScreenSetup => "screen_setup",
            Stage::FirstFrame => "first_frame",
        }
    }
}

/// auth-setup's chip calls (`LocalMfiSigner`): the signature's time from go to ready, the certificate read's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    Sign,
    Cert,
}

impl Chip {
    const ALL: [Chip; 2] = [Chip::Sign, Chip::Cert];

    fn key(self) -> &'static str {
        match self {
            Chip::Sign => "chip2_ms",
            Chip::Cert => "cert2_ms",
        }
    }
}

/// Not stamped yet.
const UNSET: u64 = u64::MAX;
static STAGES: [AtomicU64; 9] = [
    AtomicU64::new(UNSET),
    AtomicU64::new(UNSET),
    AtomicU64::new(UNSET),
    AtomicU64::new(UNSET),
    AtomicU64::new(UNSET),
    AtomicU64::new(UNSET),
    AtomicU64::new(UNSET),
    AtomicU64::new(UNSET),
    AtomicU64::new(UNSET),
];
static CHIPS: [AtomicU64; 2] = [AtomicU64::new(UNSET), AtomicU64::new(UNSET)];

/// Stamp `stage` now, on the timeline's uptime clock: once per carplayd, the first standing.
pub fn stage(stage: Stage) {
    if let Some(ms) = box_common::timeline::now_ms() {
        let _ = STAGES[stage as usize].compare_exchange(UNSET, ms, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// Note how long one of auth-setup's chip calls took (ms): once per carplayd, the first standing.
pub fn chip(chip: Chip, ms: u64) {
    let _ = CHIPS[chip as usize].compare_exchange(UNSET, ms, Ordering::Relaxed, Ordering::Relaxed);
}

/// What this carplayd stamped, as the timeline line takes it.
fn own() -> Vec<(&'static str, u64)> {
    let stages = Stage::ALL.iter().map(|&s| (s.key(), STAGES[s as usize].load(Ordering::Relaxed)));
    let chips = Chip::ALL.iter().map(|&c| (c.key(), CHIPS[c as usize].load(Ordering::Relaxed)));
    stages.chain(chips).filter(|&(_, v)| v != UNSET).collect()
}

/// Whether this is the wireless carplayd: `CARPLAY_WIRELESS_METADATA` is set at its spawn alone (btd's av.rs), and
/// never for the wired one the supervisor starts, whose first frame may find a wireless session's record.
fn wireless() -> bool {
    std::env::var_os("CARPLAY_WIRELESS_METADATA").is_some()
}

/// The first video frame went to the host: print the line, once per carplayd (one per session), and the timeline
/// after it.
pub fn first_frame() {
    if PRINTED.load(Ordering::Relaxed) || PRINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    stage(Stage::FirstFrame);
    let now = now_ms();
    let marks = std::fs::read_to_string(MARKS).unwrap_or_default();
    let mut out = String::new();
    if let Some(line) = summary(&marks, now) {
        let at = mark_value(&marks, "boxlog_at").unwrap_or(0);
        let head = mark_word(&marks, "boxlog_head");
        let nudges = [NUDGES[0].load(Ordering::Relaxed), NUDGES[1].load(Ordering::Relaxed)];
        out = format!("[startup] {line}{}\n", counts(&marks, nudges, psk_mismatches(BOX_LOG, at, head)));
    }
    // Printed whatever [startup] made of the wall clock: the timeline's own is the uptime.
    if wireless() {
        out.push_str(&box_common::timeline::line(&marks, &own()));
        out.push('\n');
    }
    // One write for both: eprintln! writes a line in pieces, and another thread's println! (stdout, the same file)
    // could land between them.
    if !out.is_empty() {
        use std::io::Write as _;
        let _ = std::io::stderr().write_all(out.as_bytes());
    }
}

/// The counts, for the Mac's bench to read one line per session (its 13 Nov bench-hooks brief, item 8): since the host came,
/// btd's timed-out pages and resets (and of those, the ones cut for no SDP channel after the answer) and the phone's
/// PSK-mismatch tries at the access point; this session's, this carplayd's nudges. btd's record runs from the host's
/// arrival, over every phone session under it, so a later session's line counts the earlier ones' too (the Mac's 2.6.0 review,
/// BH-1).
fn counts(marks: &str, nudges: [u32; 2], psk: Option<Psk>) -> String {
    let tally = |what: &str| marks.lines().filter(|l| l.split_whitespace().eq(["tally", what])).count();
    format!(
        "; since the host came: absent pages {}, resets {} ({} cut, no SDP channel), PSK mismatches {}; this session: nudges {}+{}",
        tally("absent"),
        tally("reset"),
        tally("sdp_cut"),
        match psk {
            None => "-".to_string(),
            Some(Psk { count, cut: true }) => format!(">={count} (box.log cut)"),
            Some(Psk { count, cut: false }) => count.to_string(),
        },
        nudges[0],
        nudges[1],
    )
}

/// The PSK-mismatch count since the host came, and whether the box log was cut since the host came: ocbmd empties it at its cap,
/// and the lines between the host and the cut are gone, so the count is then a lower bound (the Mac's review, BH-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Psk {
    count: usize,
    cut: bool,
}

/// hostapd's AP-STA-POSSIBLE-PSK-MISMATCH lines in the box log after byte `from`, its size when the host came, or all of
/// it when it was cut since: shorter than `from`, or its first bytes no longer `head` (btd's `boxlog_head`, hex), as when
/// it was emptied and grew past `from` again. None when it can't be read.
fn psk_mismatches(path: &str, from: u64, head: Option<&str>) -> Option<Psk> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut first = vec![0u8; head.map_or(0, |h| h.len() / 2)];
    let n = f.read(&mut first).ok()?;
    let cut = from > len || head.is_some_and(|h| hex(&first[..n]) != h);
    let start = if cut { 0 } else { from.max(len.saturating_sub(SCAN_MAX)) };
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(SCAN_MAX).read_to_end(&mut buf).ok()?;
    Some(Psk { count: String::from_utf8_lossy(&buf).matches("AP-STA-POSSIBLE-PSK-MISMATCH").count(), cut })
}

/// Lower-case hex, as btd writes `boxlog_head`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A mark's first word after its name, as text.
fn mark_word<'a>(marks: &'a str, stage: &str) -> Option<&'a str> {
    marks.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some(stage)).then(|| w.next()).flatten()
    })
}

fn mark_value(marks: &str, stage: &str) -> Option<u64> {
    marks.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some(stage)).then(|| w.next().and_then(|v| v.parse::<u64>().ok())).flatten()
    })
}

/// The line for `marks` with the first frame at `frame_ms`, or None without a recent host arrival and a handoff.
fn summary(marks: &str, frame_ms: u64) -> Option<String> {
    let at = |stage: &str| {
        marks.lines().find_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some(stage)).then(|| w.next().and_then(|v| v.parse::<u64>().ok())).flatten()
        })
    };
    let start = at("host_present")?;
    at("handoff")?;
    if frame_ms < start || frame_ms - start > STALE_MS {
        return None;
    }
    let off = |stage: &str| at(stage).map_or_else(|| "-".to_string(), |t| format!("+{} ms", t.saturating_sub(start)));
    // The adapter's uptime when the host came and the boot preload's state then: whether the head unit beat it.
    let word = |stage: &str| {
        marks.lines().find_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some(stage)).then(|| w.next().map(str::to_string)).flatten()
        })
    };
    let boot = match (at("host_uptime"), word("preload")) {
        (Some(u), Some(p)) => format!(" (adapter up {}.{} s, preload {p})", u / 1000, u % 1000 / 100),
        (None, Some(p)) => format!(" (preload {p})"),
        _ => String::new(),
    };
    Some(format!(
        "host present +0 ms{boot}, Bluetooth up {}, phone connected {}, Wi-Fi handoff {}, first frame +{} ms",
        off("bt_up"),
        off("phone"),
        off("handoff"),
        frame_ms - start
    ))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_gives_each_stage_from_the_hosts_arrival() {
        // The Mac's 12 Nov 11:59 bench, as the stages fell.
        let marks = "host_present 1000\nbt_up 11900\nphone 20200\nhandoff 22800\n";
        assert_eq!(
            summary(marks, 27100).as_deref(),
            Some("host present +0 ms, Bluetooth up +10900 ms, phone connected +19200 ms, Wi-Fi handoff +21800 ms, first frame +26100 ms")
        );
    }

    #[test]
    fn the_line_says_when_the_adapter_came_up_and_where_the_preload_was() {
        let marks = "host_present 1000\npreload running\nhost_uptime 4270\nhandoff 5000\n";
        assert_eq!(
            summary(marks, 9000).as_deref(),
            Some("host present +0 ms (adapter up 4.2 s, preload running), Bluetooth up -, phone connected -, Wi-Fi handoff +4000 ms, first frame +8000 ms")
        );
    }

    #[test]
    fn the_session_counts_read_from_the_tallies_and_this_process() {
        let marks = "host_present 1000\ntally absent\ntally reset\ntally sdp_cut\ntally absent\nhandoff 5000\n";
        assert_eq!(
            counts(marks, [1, 0], Some(Psk { count: 2, cut: false })),
            "; since the host came: absent pages 2, resets 1 (1 cut, no SDP channel), PSK mismatches 2; this session: nudges 1+0"
        );
        assert_eq!(
            counts("host_present 1\n", [0, 0], None),
            "; since the host came: absent pages 0, resets 0 (0 cut, no SDP channel), PSK mismatches -; this session: nudges 0+0"
        );
        assert_eq!(summary(marks, 9000).as_deref().map(|l| l.contains("tally")), Some(false), "tallies are no stage");
    }

    /// BH-06: a box log ocbmd rotated since the host came (emptied at its cap) gives a lower bound, said as one; one
    /// that only grew is counted from where it stood.
    #[test]
    fn a_box_log_cut_since_the_host_came_gives_a_lower_bound() {
        let path = std::env::temp_dir().join(format!("receiver_psk_{}.log", std::process::id()));
        let p = path.to_str().unwrap();
        let before = "[ocbmd] up\nwlan0: AP-STA-POSSIBLE-PSK-MISMATCH 1\n";
        let head = hex(&before.as_bytes()[..32]);
        let from = before.len() as u64;
        std::fs::write(&path, format!("{before}wlan0: AP-STA-POSSIBLE-PSK-MISMATCH 2\nwlan0: AP-STA-POSSIBLE-PSK-MISMATCH 3\n")).unwrap();
        assert_eq!(psk_mismatches(p, from, Some(&head)), Some(Psk { count: 2, cut: false }), "only this session's");
        std::fs::write(&path, "AP-STA-POSSIBLE-PSK-MISMATCH\n").unwrap();
        assert_eq!(psk_mismatches(p, from, None), Some(Psk { count: 1, cut: true }), "shorter than it was: cut");
        let grown = format!("[log] rotated 262144 bytes\n{}wlan0: AP-STA-POSSIBLE-PSK-MISMATCH 5\n", "x".repeat(200));
        std::fs::write(&path, grown).unwrap();
        assert_eq!(psk_mismatches(p, from, Some(&head)), Some(Psk { count: 1, cut: true }), "grown past it again: cut");
        assert_eq!(
            counts("", [0, 0], Some(Psk { count: 1, cut: true })),
            "; since the host came: absent pages 0, resets 0 (0 cut, no SDP channel), PSK mismatches >=1 (box.log cut); this session: nudges 0+0"
        );
        let _ = std::fs::remove_file(&path);
        assert_eq!(psk_mismatches(p, 0, None), None, "no log");
    }

    /// The timeline from this carplayd's own stamps and btd's record: carplayd's events by its uptime, btd's and the
    /// shell's from the record, every one from the origin btd copied in (box_common::timeline).
    #[test]
    fn the_timeline_takes_carplayds_own_stamps_and_btds_record() {
        let before = box_common::timeline::now_ms().unwrap();
        stage(Stage::Dial);
        stage(Stage::Dial);
        chip(Chip::Sign, 301);
        chip(Chip::Sign, 999);
        let own = own();
        let dial = own.iter().find(|(k, _)| *k == "dial").map(|&(_, v)| v).expect("stamped");
        assert!(dial >= before && dial <= box_common::timeline::now_ms().unwrap());
        assert_eq!(own.iter().filter(|(k, _)| *k == "dial").count(), 1);
        assert!(own.contains(&("chip2_ms", 301)), "the first stands");
        assert!(!own.iter().any(|(k, _)| *k == "cert2_ms"), "not noted: not there");

        let marks = format!("host_present 1763300000123\nt host {}\nt wifi_rsp {}\n", dial - 3000, dial - 400);
        let line = box_common::timeline::line(&marks, &own);
        assert!(box_common::timeline::is_line(&line), "{line}");
        assert!(line.contains(" wifi_rsp=2600 ") && line.contains(" dial=3000 "), "{line}");
        assert!(line.contains(" chip2_ms=301 ") && line.contains(" cert2_ms=- "), "{line}");
        assert!(line.contains(" page_start=- "), "{line}");
    }

    #[test]
    fn a_missing_stage_shows_a_dash_and_an_old_or_wired_record_prints_nothing() {
        let marks = "host_present 1000\nhandoff 5000\n";
        assert_eq!(
            summary(marks, 9000).as_deref(),
            Some("host present +0 ms, Bluetooth up -, phone connected -, Wi-Fi handoff +4000 ms, first frame +8000 ms")
        );
        assert_eq!(summary("host_present 1000\n", 9000), None, "no handoff: not a wireless session's");
        assert_eq!(summary(marks, 1000 + STALE_MS + 1), None, "an older session's record");
        assert_eq!(summary("", 9000), None);
    }
}
