//! A wireless session's start-up times, for the one `[startup]` line carplayd prints at the first video frame
//! (`receiver::startup`): when the host came (`/tmp/host_present` written), Bluetooth was up, a phone connected and
//! the Wi-Fi handoff (0x5703) went out, each a `<stage> <unix ms>` line in [`MARKS`]. A drive log then carries the
//! truck's own numbers (the Mac's 13 Nov connect-time brief). Best effort: a failed write costs only the line.

use std::io::Write as _;

/// The stages as written; read back by carplayd (`receiver::startup::MARKS`).
pub const MARKS: &str = "/tmp/startup_marks";

/// The boot radio preload's states this boot, one `<word> <uptime s>` line each as ocbm_boot.sh and radio_hal.sh enter
/// them: waiting (for the self-test), running, done, stood_down (a host came first), skipped, off_guard or off_owner.
pub const PRELOAD_STATE: &str = "/tmp/radio_preload_state";

/// Start this session's record, from the host's arrival: the mtime of `/tmp/host_present`, else now. With it, the
/// adapter's uptime then and the preload's state now, so a truck's cold start shows whether its head unit beat the
/// preload (the Mac's 13 Nov brief, item 5).
pub fn begin() {
    let present = std::fs::metadata("/tmp/host_present")
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(now_ms, |d| d.as_millis() as u64);
    let uptime = std::fs::read_to_string("/proc/uptime").ok().and_then(|u| uptime_ms(&u));
    let host_uptime = uptime.map(|u| u.saturating_sub(now_ms().saturating_sub(present)));
    let states = std::fs::read_to_string(PRELOAD_STATE).unwrap_or_default();
    let preload = host_uptime.map_or_else(|| "unknown".to_string(), |at| preload_at(&states, at));
    let mut record = format!("host_present {present}\npreload {preload}\n");
    if let Some(u) = host_uptime {
        record.push_str(&format!("host_uptime {u}\n"));
    }
    let _ = std::fs::write(MARKS, record);
}

/// `/proc/uptime`'s first field in ms.
fn uptime_ms(text: &str) -> Option<u64> {
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    Some((secs * 1000.0) as u64)
}

/// The preload's state at `at_ms` of uptime: the last one entered by then; "none" when none was (no preload this boot,
/// or not started yet).
fn preload_at(states: &str, at_ms: u64) -> String {
    states
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let word = w.next()?;
            let ms = uptime_ms(w.next()?)?;
            (ms <= at_ms).then(|| word.to_string())
        })
        .last()
        .unwrap_or_else(|| "none".to_string())
}

/// Note `stage` now, once per session: a stage already noted keeps its first time.
pub fn mark(stage: &str) {
    let have = std::fs::read_to_string(MARKS).unwrap_or_default();
    if noted(&have, stage) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(MARKS) {
        let _ = writeln!(f, "{stage} {}", now_ms());
    }
}

/// Whether `marks` already holds a line for `stage`.
fn noted(marks: &str, stage: &str) -> bool {
    marks.lines().any(|l| l.split_whitespace().next() == Some(stage))
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
    fn a_stage_is_noted_once_by_its_first_word() {
        let marks = "host_present 100\nbt_up 200\n";
        assert!(noted(marks, "bt_up"));
        assert!(!noted(marks, "phone"));
        assert!(!noted(marks, "bt"), "a whole word, not a prefix");
    }

    #[test]
    fn the_uptime_and_the_preload_state_read_as_written() {
        assert_eq!(uptime_ms("4.27 3.90\n"), Some(4270));
        assert_eq!(uptime_ms(""), None);
        let states = "waiting 2.10\nrunning 3.60\ndone 10.40\n";
        assert_eq!(preload_at(states, 4_200), "running", "the truck's head unit beat the preload");
        assert_eq!(preload_at(states, 12_000), "done");
        assert_eq!(preload_at(states, 1_000), "none", "before the boot got to it");
        assert_eq!(preload_at("", 5_000), "none");
    }
}
