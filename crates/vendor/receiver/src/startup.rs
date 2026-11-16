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

/// carplayd's own events for the `[timeline]` line (`box_common::timeline::EVENTS`). The first of each stands: in the
/// process for `carplayd_start` and `svc_seen`, in the control connection for the rest ([`new_connection`]), so the
/// line tells the connection that produced the picture. `dial` is the last connect-out sent before that connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// carplayd started (main).
    CarplaydStart,
    /// The `_carplay-ctrl` of the phone btd names first resolved (discovery).
    SvcSeen,
    /// A connect-out (`GET /ctrl-int/1/connect`) sent (discovery).
    Dial,
    /// Pair-verify OK: the control channel encrypted.
    PvDone,
    /// `POST /auth-setup` received.
    AuthIn,
    /// `/auth-setup` answered.
    AuthOut,
    /// RECORD received.
    RecordIn,
    /// RECORD answered.
    Record,
    /// The main screen's (type 110) SETUP answered.
    ScreenSetup,
    /// The first video frame to the host.
    FirstFrame,
}

impl Stage {
    const ALL: [Stage; 10] = [
        Stage::CarplaydStart,
        Stage::SvcSeen,
        Stage::Dial,
        Stage::PvDone,
        Stage::AuthIn,
        Stage::AuthOut,
        Stage::RecordIn,
        Stage::Record,
        Stage::ScreenSetup,
        Stage::FirstFrame,
    ];

    /// The stages of one control connection, started afresh at each.
    const PER_CONNECTION: [Stage; 7] = [
        Stage::PvDone,
        Stage::AuthIn,
        Stage::AuthOut,
        Stage::RecordIn,
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
            Stage::RecordIn => "record_in",
            Stage::Record => "record",
            Stage::ScreenSetup => "screen_setup",
            Stage::FirstFrame => "first_frame",
        }
    }
}

/// auth-setup's chip calls (`LocalMfiSigner`): the signature's time from go to ready, the certificate read's. Per
/// control connection, as its stages.
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
#[allow(clippy::declare_interior_mutable_const)]
const NOT_YET: AtomicU64 = AtomicU64::new(UNSET);

/// carplayd's stamps, in memory ([`OWN`]; a test keeps its own).
struct Stamps {
    stages: [AtomicU64; 10],
    chips: [AtomicU64; 2],
    /// The last connect-out sent: the `dial` of the connection after it.
    dial_sent: AtomicU64,
    /// When the current control connection began.
    connection_at: AtomicU64,
}

impl Stamps {
    const fn new() -> Self {
        Stamps { stages: [NOT_YET; 10], chips: [NOT_YET; 2], dial_sent: NOT_YET, connection_at: NOT_YET }
    }

    fn stage(&self, stage: Stage, ms: u64) {
        if stage == Stage::Dial {
            self.dial_sent.store(ms, Ordering::Relaxed);
        } else {
            let _ = self.stages[stage as usize].compare_exchange(UNSET, ms, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    fn chip(&self, chip: Chip, ms: u64) {
        let _ = self.chips[chip as usize].compare_exchange(UNSET, ms, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// A control connection begins at `now`: its stages and chip times start afresh, and its `dial` is the last
    /// connect-out sent since the connection before it began.
    fn new_connection(&self, now: Option<u64>) {
        for s in Stage::PER_CONNECTION {
            self.stages[s as usize].store(UNSET, Ordering::Relaxed);
        }
        for c in &self.chips {
            c.store(UNSET, Ordering::Relaxed);
        }
        let before = match now {
            Some(n) => self.connection_at.swap(n, Ordering::Relaxed),
            None => self.connection_at.load(Ordering::Relaxed),
        };
        let dial = dial_for(self.dial_sent.load(Ordering::Relaxed), before);
        self.stages[Stage::Dial as usize].store(dial, Ordering::Relaxed);
    }

    /// What is stamped, as the timeline line takes it.
    fn own(&self) -> Vec<(&'static str, u64)> {
        let stages = Stage::ALL.iter().map(|&s| (s.key(), self.stages[s as usize].load(Ordering::Relaxed)));
        let chips = Chip::ALL.iter().map(|&c| (c.key(), self.chips[c as usize].load(Ordering::Relaxed)));
        stages.chain(chips).filter(|&(_, v)| v != UNSET).collect()
    }
}

/// A connection's `dial`: the last connect-out (`sent`), when it went since the connection before began (`before`;
/// any, for the first). None (UNSET): the phone came by itself.
fn dial_for(sent: u64, before: u64) -> u64 {
    if sent != UNSET && (before == UNSET || sent >= before) {
        sent
    } else {
        UNSET
    }
}

static OWN: Stamps = Stamps::new();

/// Stamp `stage` now, on the timeline's uptime clock ([`Stage`] says which first stands).
pub fn stage(stage: Stage) {
    if let Some(ms) = box_common::timeline::now_ms() {
        OWN.stage(stage, ms);
    }
}

/// Note how long one of auth-setup's chip calls took (ms): once per control connection, the first standing.
pub fn chip(chip: Chip, ms: u64) {
    OWN.chip(chip, ms);
}

/// A new control connection (carplayd's serve loop): the line describes the one that produces the picture.
pub fn new_connection() {
    OWN.new_connection(box_common::timeline::now_ms());
}

/// What this carplayd stamped, as the timeline line takes it.
fn own() -> Vec<(&'static str, u64)> {
    OWN.own()
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

    /// carplayd's stamps per control connection: a failed first connection's stages and chip times go with it, the
    /// one that produces the picture keeps its own, `dial` is the connect-out before each, and carplayd_start and
    /// svc_seen stay the process's (a local [`Stamps`]: the globals are other tests' too).
    #[test]
    fn the_stages_are_the_connection_that_produced_the_picture() {
        let st = Stamps::new();
        let get = |st: &Stamps, key: &str| st.own().iter().find(|(k, _)| *k == key).map(|&(_, v)| v);
        st.stage(Stage::CarplaydStart, 1000);
        st.stage(Stage::SvcSeen, 1500);
        st.stage(Stage::Dial, 1600);
        assert_eq!(get(&st, "dial"), None, "a dial counts for the connection after it");
        st.new_connection(Some(1700));
        st.stage(Stage::PvDone, 1800);
        st.stage(Stage::PvDone, 1850);
        st.chip(Chip::Sign, 301);
        st.chip(Chip::Sign, 999);
        assert_eq!((get(&st, "dial"), get(&st, "pv_done"), get(&st, "chip2_ms")), (Some(1600), Some(1800), Some(301)));

        // The first connection fails; discovery dials again, and the second produces the picture.
        st.stage(Stage::SvcSeen, 2900);
        st.stage(Stage::Dial, 3000);
        st.stage(Stage::Dial, 3100);
        st.new_connection(Some(3200));
        assert_eq!(get(&st, "pv_done"), None, "the failed connection's stages went with it");
        assert_eq!(get(&st, "chip2_ms"), None, "and its chip times");
        assert_eq!(get(&st, "dial"), Some(3100), "the last connect-out before this connection");
        assert_eq!((get(&st, "carplayd_start"), get(&st, "svc_seen")), (Some(1000), Some(1500)), "the process's");
        st.stage(Stage::RecordIn, 3500);
        st.stage(Stage::Record, 3600);
        st.stage(Stage::FirstFrame, 4000);
        st.stage(Stage::Dial, 4100);
        assert_eq!(get(&st, "dial"), Some(3100), "a dial after the connection began is the next one's");

        // A third connection the phone opened by itself: no dial since the second began.
        st.new_connection(Some(4200));
        assert_eq!(get(&st, "dial"), Some(4100), "the dial at 4100 came after the second began");
        st.new_connection(Some(4300));
        assert_eq!(get(&st, "dial"), None, "none since the third began: the phone came by itself");
        assert_eq!(dial_for(UNSET, UNSET), UNSET);
        assert_eq!(dial_for(500, UNSET), 500, "the first connection takes any dial before it");
    }

    /// The timeline from carplayd's own stamps and btd's record: carplayd's by their uptime, btd's and the shell's from
    /// the record, all from the origin btd copied in (box_common::timeline).
    #[test]
    fn the_timeline_takes_carplayds_own_stamps_and_btds_record() {
        let st = Stamps::new();
        st.stage(Stage::Dial, 64_000);
        st.new_connection(Some(64_100));
        st.stage(Stage::RecordIn, 66_080);
        st.chip(Chip::Sign, 301);
        let marks = "host_present 1763300000123\nt host 61000\nt bt_up 62000\nt wifi_rsp 63600\n";
        let line = box_common::timeline::line(marks, &st.own());
        assert!(box_common::timeline::is_line(&line), "{line}");
        assert!(line.contains(" bt_up=1000 ") && line.contains(" wifi_rsp=2600 ") && line.contains(" dial=3000 "), "{line}");
        assert!(line.contains(" record_in=5080 ") && line.contains(" record=- "), "{line}");
        assert!(line.contains(" chip2_ms=301 ") && line.contains(" cert2_ms=- "), "{line}");
        assert!(line.contains(" page_start=- "), "{line}");

        let before = box_common::timeline::now_ms().unwrap();
        stage(Stage::CarplaydStart);
        let at = own().iter().find(|(k, _)| *k == "carplayd_start").map(|&(_, v)| v).expect("the global stamps it");
        assert!(at >= before && at <= box_common::timeline::now_ms().unwrap());
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
