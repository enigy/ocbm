//! The one `[startup]` line per wireless session: at the first video frame to the host, the stages btd noted in
//! [`MARKS`] (its `startup` module: host present, Bluetooth up, phone connected, Wi-Fi handoff) and this frame,
//! each as +ms from the host's arrival. A drive log then carries the truck's own connect time (the Mac's 13 Nov
//! brief). Nothing is printed for a session btd didn't note (a wired one), or for a record from an older session.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// btd's record of this session's stages (`wireless::startup::MARKS`).
pub const MARKS: &str = "/tmp/startup_marks";

/// A record whose host arrived longer ago than this is an older session's, not this one's.
const STALE_MS: u64 = 10 * 60 * 1000;

static PRINTED: AtomicBool = AtomicBool::new(false);
/// This carplayd's screen nudges (`session.rs` nudge_screen_back): the 0.5 s one and the 3 s one.
static NUDGES: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// The box log, scanned from where it stood when the host came (btd's `boxlog_at` mark) for the session's
/// PSK-mismatch lines. Bounded: at most [`SCAN_MAX`] bytes are read.
const BOX_LOG: &str = "/tmp/box.log";
const SCAN_MAX: u64 = 512 * 1024;

/// A screen nudge sent: the first (0.5 s) or the second (3 s).
pub fn nudged(second: bool) {
    NUDGES[usize::from(second)].fetch_add(1, Ordering::Relaxed);
}

/// The first video frame went to the host: print the line, once per carplayd (one per session).
pub fn first_frame() {
    if PRINTED.load(Ordering::Relaxed) || PRINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let now = now_ms();
    let Ok(marks) = std::fs::read_to_string(MARKS) else { return };
    let Some(line) = summary(&marks, now) else { return };
    let at = mark_value(&marks, "boxlog_at").unwrap_or(0);
    let nudges = [NUDGES[0].load(Ordering::Relaxed), NUDGES[1].load(Ordering::Relaxed)];
    eprintln!("[startup] {line}{}", counts(&marks, nudges, psk_mismatches(at)));
}

/// This session's counts, for the Mac's bench to read one line per session (its 13 Nov bench-hooks brief, item 8): btd's
/// timed-out pages and resets (and of those, the ones cut for no SDP channel after the answer), this carplayd's nudges, and the
/// phone's PSK-mismatch tries at the access point.
fn counts(marks: &str, nudges: [u32; 2], psk: Option<usize>) -> String {
    let tally = |what: &str| marks.lines().filter(|l| l.split_whitespace().eq(["tally", what])).count();
    format!(
        "; this session: absent pages {}, resets {} ({} cut, no SDP channel), nudges {}+{}, PSK mismatches {}",
        tally("absent"),
        tally("reset"),
        tally("sdp_cut"),
        nudges[0],
        nudges[1],
        psk.map_or_else(|| "-".to_string(), |n| n.to_string())
    )
}

/// hostapd's AP-STA-POSSIBLE-PSK-MISMATCH lines in the box log after byte `from` (from the start when the log was cut
/// since); None when it can't be read.
fn psk_mismatches(from: u64) -> Option<usize> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut f = std::fs::File::open(BOX_LOG).ok()?;
    let len = f.metadata().ok()?.len();
    let start = if from > len { 0 } else { from.max(len.saturating_sub(SCAN_MAX)) };
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(SCAN_MAX).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).matches("AP-STA-POSSIBLE-PSK-MISMATCH").count())
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
            counts(marks, [1, 0], Some(2)),
            "; this session: absent pages 2, resets 1 (1 cut, no SDP channel), nudges 1+0, PSK mismatches 2"
        );
        assert_eq!(
            counts("host_present 1\n", [0, 0], None),
            "; this session: absent pages 0, resets 0 (0 cut, no SDP channel), nudges 0+0, PSK mismatches -"
        );
        assert_eq!(summary(marks, 9000).as_deref().map(|l| l.contains("tally")), Some(false), "tallies are no stage");
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
