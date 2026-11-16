//! A wireless session's start-up times, for the one `[startup]` line carplayd prints at the first video frame
//! (`receiver::startup`): when the host came (`/tmp/host_present` written), Bluetooth was up, a phone connected and
//! the Wi-Fi handoff (0x5703) went out, each a `<stage> <unix ms>` line in [`MARKS`]. A drive log then carries the
//! truck's own numbers (the Mac's 13 Nov connect-time brief). Best effort: a failed write costs only the line.
//!
//! The same record carries the `[timeline]` line's stamps (`box_common::timeline`), on the uptime clock rather than
//! the wall clock: btd's events ([`stamp`]) and chip times ([`duration`]), the origin and this session's start, and
//! the radio bring-up's, which the shell appends and [`begin`] carries into each session it starts under it.

use std::io::Write as _;

use box_common::timeline::{self, DURATION, STAMP};

/// The stages as written; read back by carplayd (`receiver::startup::MARKS`).
pub const MARKS: &str = "/tmp/startup_marks";

/// The boot radio preload's states this boot, one `<word> <uptime s>` line each as ocbm_boot.sh and radio_hal.sh enter
/// them: waiting (for the self-test), running, done, stood_down (a host came first), skipped, off_guard or off_owner.
pub const PRELOAD_STATE: &str = "/tmp/radio_preload_state";

/// Start this session's record, from the host's arrival: the mtime of `/tmp/host_present`, else now. With it, the
/// adapter's uptime then and the preload's state now, so a truck's cold start shows whether its head unit beat the
/// preload (the Mac's 13 Nov brief, item 5). Under the same host, the counts so far carry over ([`carry_over`]).
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
    // Where the box log stood, so carplayd counts the hostapd lines since the host came (receiver::startup; kept across
    // btd's sessions under one host by carry_over, the Mac's round 3, C-2), and its first
    // bytes, so a log ocbmd rotated since (emptied at its cap) is told from one that only grew (the Mac's review, BH-06).
    if let Ok(mut f) = std::fs::File::open("/tmp/box.log") {
        use std::io::Read as _;
        if let Ok(m) = f.metadata() {
            record.push_str(&format!("boxlog_at {}\n", m.len()));
        }
        let mut head = [0u8; BOXLOG_HEAD];
        if let Ok(n @ 1..) = f.read(&mut head) {
            record.push_str(&format!("boxlog_head {}\n", hex(&head[..n])));
        }
    }
    if let Some(u) = host_uptime {
        record.push_str(&format!("host_uptime {u}\n"));
    }
    // The timeline's origin, as ocbmd stamped it when it raised the flag, and this session's start, on the uptime clock.
    if let Some(origin) = timeline::read_origin(timeline::ORIGIN) {
        record.push_str(&timeline::entry(STAMP, timeline::ORIGIN_KEY, origin));
    }
    if let Some(now) = timeline::now_ms() {
        record.push_str(&timeline::entry(STAMP, "btd_start", now));
    }
    let prev = std::fs::read_to_string(MARKS).unwrap_or_default();
    let _ = std::fs::write(MARKS, carry_over(&prev, present, &record));
}

/// The record a session starts with: `fresh`, and when `prev` is the same host's (its host_present is `present`), its
/// tallies and box log marks, so the [startup] line's counts run from the host's arrival over every session btd starts
/// under it: a phone switch, a session that never reached CarPlay, a wired preempt (the Mac's round 2, R2-1). The
/// stages are always the new session's. Then the radio bring-up's timeline stamps in `prev`, this host's or not
/// ([`radio_bring_up`]): the shell wrote them there before this session began.
fn carry_over(prev: &str, present: u64, fresh: &str) -> String {
    let radio = radio_bring_up(prev, timeline::find(fresh, STAMP, timeline::ORIGIN_KEY));
    carry_counts(prev, present, fresh) + &radio
}

/// The radio bring-up's stamps in `prev` (`timeline::RADIO`), from its last `sup_radios` on: the bring-up this session
/// runs on. Nothing when that came before the origin (`origin`; any, when unknown): an earlier host's, whose radios
/// went down with it. Stamps from btd's earlier sessions are not carried: each session's are its own.
fn radio_bring_up(prev: &str, origin: Option<u64>) -> String {
    let lines: Vec<&str> = prev.lines().collect();
    let opens = |l: &str| timeline::find(l, STAMP, "sup_radios");
    let Some(start) = lines.iter().rposition(|l| opens(l).is_some()) else { return String::new() };
    if origin.is_some_and(|o| opens(lines[start]).is_some_and(|at| at < o)) {
        return String::new();
    }
    let radio = |l: &&&str| timeline::RADIO.iter().any(|key| timeline::find(l, STAMP, key).is_some());
    lines[start..].iter().filter(radio).map(|l| format!("{l}\n")).collect()
}

/// [`carry_over`]'s counts: `fresh`, and the same host's tallies and box log marks.
fn carry_counts(prev: &str, present: u64, fresh: &str) -> String {
    fn word(l: &str) -> &str {
        l.split_whitespace().next().unwrap_or("")
    }
    let same_host = prev.lines().find_map(|l| l.strip_prefix("host_present ")).and_then(|v| v.trim().parse::<u64>().ok())
        == Some(present);
    if !same_host {
        return fresh.to_string();
    }
    let boxlog = |l: &&str| matches!(word(*l), "boxlog_at" | "boxlog_head");
    // The box log's marks from when the host came, when the first session had them; else this one's.
    let from_prev = prev.lines().any(|l| word(l) == "boxlog_at");
    fresh
        .lines()
        .filter(|l| !(from_prev && boxlog(l)))
        .chain(prev.lines().filter(|l| (from_prev && boxlog(l)) || word(*l) == "tally"))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// How many of the box log's first bytes `boxlog_head` keeps (receiver::startup compares them).
pub const BOXLOG_HEAD: usize = 32;

/// Lower-case hex, for the marks.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    append(MARKS, &format!("{stage} {}\n", now_ms()));
}

/// Count one of `what` for this session's line (absent, reset, sdp_cut): a `tally <what>` line per event.
pub fn tally(what: &str) {
    append(MARKS, &format!("tally {what}\n"));
}

/// Note timeline event `key` now, on the uptime clock (`box_common::timeline`), once per session: the first stands.
pub fn stamp(key: &str) {
    if let Some(ms) = timeline::now_ms() {
        stamp_at(key, ms);
    }
}

/// [`stamp`] with the uptime it happened at (`ms`), taken by the caller: the AP watch's, as it woke.
pub fn stamp_at(key: &str, ms: u64) {
    note(MARKS, STAMP, key, ms);
}

/// Note one of btd's chip times for the timeline (`chip1_ms`, `cert1_ms`), once per session: the first stands.
pub fn duration(key: &str, ms: u64) {
    note(MARKS, DURATION, key, ms);
}

/// A `<kind> <key> <ms>` line in the record at `path`, unless it has one for `key` already.
fn note(path: &str, kind: &str, key: &str, ms: u64) {
    let have = std::fs::read_to_string(path).unwrap_or_default();
    if timeline::find(&have, kind, key).is_none() {
        append(path, &timeline::entry(kind, key, ms));
    }
}

/// Append `line` to the record at `path`, when there is one (a session began it), in one write: the shell appends to
/// it too, and a line written in pieces could interleave with one of its.
fn append(path: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
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

    /// R2-1 (the Mac's round 2): a session btd starts under the same host keeps the counts since the host came and where
    /// the box log stood then; its stages are its own. Another host starts afresh.
    #[test]
    fn a_session_under_the_same_host_carries_the_counts_over() {
        let prev = "host_present 1000\npreload done\nboxlog_at 500\nboxlog_head 5b6f\nhost_uptime 4270\nbt_up 1200\n\
                    tally absent\ntally reset\ntally sdp_cut\nphone 3000\nhandoff 3500\ntally absent\n";
        let fresh = "host_present 1000\npreload done\nboxlog_at 900\nboxlog_head 5b6f\nhost_uptime 4270\n";
        let rec = carry_over(prev, 1000, fresh);
        assert_eq!(
            rec,
            "host_present 1000\npreload done\nhost_uptime 4270\nboxlog_at 500\nboxlog_head 5b6f\n\
             tally absent\ntally reset\ntally sdp_cut\ntally absent\n"
        );
        assert!(!noted(&rec, "bt_up") && !noted(&rec, "phone") && !noted(&rec, "handoff"), "the stages are the new session's");

        let no_log = "host_present 1000\npreload done\ntally reset\n";
        assert_eq!(carry_over(no_log, 1000, fresh), format!("{fresh}tally reset\n"), "no box log then: this session's marks");

        let other = fresh.replace("1000", "2000");
        assert_eq!(carry_over(prev, 2000, &other), other, "another host: afresh");
        assert_eq!(carry_over("", 1000, fresh), fresh, "the first session");
    }

    /// The timeline (box_common::timeline): the radio bring-up's stamps, which the shell appended before the session
    /// began, are carried from the bring-up's `sup_radios` on into each session under it; btd's own stamps from an
    /// earlier session are not, nor a bring-up from before this host came.
    #[test]
    fn the_radio_bring_up_is_carried_into_each_session_it_serves() {
        // A new host's first session: the last host's record, its bring-up, then this host's appended by the shell.
        let prev = "host_present 1000\nt host 5000\nt sup_radios 5300\nt bt_power 5550\nt btd_start 5600\nt page_start 5700\n\
                    t sup_radios 61640\nt usb_edge 49700\nt bt_power 62480\n";
        let fresh = "host_present 2000\npreload done\nt host 61234\nt btd_start 62510\n";
        let rec = carry_over(prev, 2000, fresh);
        assert_eq!(rec, format!("{fresh}t sup_radios 61640\nt usb_edge 49700\nt bt_power 62480\n"));

        // A second session under the same host (a phone switch): the bring-up again, with the AP's stamps that came
        // since; the first session's own stamps and chip times stay behind, its counts come along.
        // The AP watch's AP-ENABLED is the bring-up's too; the phone's association and lease are that session's.
        let prev2 = format!(
            "{rec}t page_start 62530\nd chip1_ms 298\nt ap_start 62900\ntally absent\nt ap_up 63600\nt ap_enabled 63880\n\
             t wifi_rsp 63930\nt sta_assoc 64500\nt dhcp_ack 64800\n"
        );
        let fresh2 = "host_present 2000\npreload done\nt host 61234\nt btd_start 90100\n";
        let rec2 = carry_over(&prev2, 2000, fresh2);
        assert_eq!(
            rec2,
            format!(
                "{fresh2}tally absent\nt sup_radios 61640\nt usb_edge 49700\nt bt_power 62480\nt ap_start 62900\nt ap_up 63600\n\
                 t ap_enabled 63880\n"
            )
        );
        assert_eq!(timeline::find(&rec2, STAMP, "page_start"), None, "the stamps are the new session's");
        assert_eq!(timeline::find(&rec2, STAMP, "sta_assoc"), None, "the next phone's association is its own");
        assert_eq!(timeline::find(&rec2, STAMP, "dhcp_ack"), None);
        assert_eq!(timeline::find(&rec2, timeline::DURATION, "chip1_ms"), None);
        assert_eq!(timeline::find(&rec2, STAMP, "btd_start"), Some(90100));

        // The radios were restarted under the same host: the later bring-up is the one.
        let prev3 = format!("{rec2}t sup_radios 95000\nt bt_power 95400\n");
        let rec3 = carry_over(&prev3, 2000, fresh2);
        assert_eq!(rec3, format!("{fresh2}tally absent\nt sup_radios 95000\nt bt_power 95400\n"));

        // A host whose bring-up stamped nothing: the last host's is not carried. With no origin, the last one is.
        let old = "host_present 1000\nt host 5000\nt sup_radios 5300\nt bt_power 5550\n";
        assert_eq!(carry_over(old, 2000, fresh), fresh);
        assert_eq!(carry_over(old, 2000, "host_present 2000\n"), "host_present 2000\nt sup_radios 5300\nt bt_power 5550\n");
    }

    /// btd's stamps and chip times: one whole line each, the first of a key standing, and none without a record.
    #[test]
    fn a_stamp_is_noted_once_and_only_in_a_record() {
        let path = std::env::temp_dir().join(format!("btd_marks_{}", std::process::id()));
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(&path);
        note(p, STAMP, "acl", 62790);
        assert!(!path.exists(), "no session began a record: nothing is created");
        std::fs::write(&path, "host_present 1000\nt host 61234\n").unwrap();
        note(p, STAMP, "acl", 62790);
        note(p, STAMP, "acl", 63000);
        note(p, timeline::DURATION, "chip1_ms", 298);
        note(p, timeline::DURATION, "chip1_ms", 512);
        note(p, STAMP, "chip1_ms", 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "host_present 1000\nt host 61234\nt acl 62790\nd chip1_ms 298\nt chip1_ms 1\n"
        );
        let _ = std::fs::remove_file(&path);
    }
}
