//! A wireless session's start-up times, for the one `[startup]` line carplayd prints at the first video frame
//! (`receiver::startup`): when the host came (`/tmp/host_present` written), Bluetooth was up, a phone connected and
//! the Wi-Fi handoff (0x5703) went out, each a `<stage> <unix ms>` line in [`MARKS`]. A drive log then carries the
//! truck's own numbers (the Mac's 13 Nov connect-time brief). Best effort: a failed write costs only the line.

use std::io::Write as _;

/// The stages as written; read back by carplayd (`receiver::startup::MARKS`).
pub const MARKS: &str = "/tmp/startup_marks";

/// Start this session's record, from the host's arrival: the mtime of `/tmp/host_present`, else now.
pub fn begin() {
    let present = std::fs::metadata("/tmp/host_present")
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(now_ms, |d| d.as_millis() as u64);
    let _ = std::fs::write(MARKS, format!("host_present {present}\n"));
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
}
