//! The one `[startup]` line per wireless session: at the first video frame to the host, the stages btd noted in
//! [`MARKS`] (its `startup` module: host present, Bluetooth up, phone connected, Wi-Fi handoff) and this frame,
//! each as +ms from the host's arrival. A drive log then carries the truck's own connect time (the Mac's 13 Nov
//! brief). Nothing is printed for a session btd didn't note (a wired one), or for a record from an older session.

use std::sync::atomic::{AtomicBool, Ordering};

/// btd's record of this session's stages (`wireless::startup::MARKS`).
pub const MARKS: &str = "/tmp/startup_marks";

/// A record whose host arrived longer ago than this is an older session's, not this one's.
const STALE_MS: u64 = 10 * 60 * 1000;

static PRINTED: AtomicBool = AtomicBool::new(false);

/// The first video frame went to the host: print the line, once per carplayd (one per session).
pub fn first_frame() {
    if PRINTED.load(Ordering::Relaxed) || PRINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let now = now_ms();
    if let Some(line) = std::fs::read_to_string(MARKS).ok().and_then(|m| summary(&m, now)) {
        eprintln!("[startup] {line}");
    }
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
    Some(format!(
        "host present +0 ms, Bluetooth up {}, phone connected {}, Wi-Fi handoff {}, first frame +{} ms",
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
