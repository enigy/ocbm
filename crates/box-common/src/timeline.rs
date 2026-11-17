//! The connect timeline (docs/ops/02_TESTING.md, "[timeline]"): a wireless connect's critical-path events, each
//! stamped where it happens, and the one `[timeline] v1 key=value ...` line carplayd prints after `[startup]` at the
//! first video frame. Instrumentation only: a stamp that fails costs its value (`-`), never anything else.
//!
//! ONE CLOCK. Every stamp is the uptime in ms on CLOCK_BOOTTIME, the clock `/proc/uptime` shows: the shell's stamps
//! read it there (to the 10 ms it gives), the daemons through [`now_ms`]. Never the wall clock, which has no RTC
//! behind it and jumps when Carlink sets it mid-session. Every value in the line is ms since the ORIGIN: the uptime
//! at which ocbmd raised `/tmp/host_present` to 1 ([`ORIGIN`], written by ocbmd as it raises the flag), the same
//! moment `[startup]` counts from.
//!
//! WHERE THE STAMPS LIVE. btd's session record ([`MARKS`], the file `[startup]` reads) carries them as one line
//! each, `t <key> <ms>` for an event and `d <key> <ms>` for a duration:
//! - btd begins each session's record with the origin (`t host`, copied from [`ORIGIN`]) and `t btd_start`, and
//!   adds its own events and chip durations as they happen, the first of each in a session standing;
//! - the supervisor, `radio_hal.sh` and `radio_ap_up.sh` append the radio bring-up's ([`RADIO`]) with one write
//!   each, btd its [`ARMED_KEY`] when the bring-up was armed; btd carries them from the bring-up's `t sup_radios` on
//!   into each session it starts under it;
//! - btd's AP watch (`wireless::ap_watch`) adds hostapd's and udhcpd's own events as their lines land;
//! - carplayd keeps its own in memory, per control connection, and prints the line itself.

/// btd's record of a session (`wireless::startup::MARKS`, `receiver::startup::MARKS`).
pub const MARKS: &str = "/tmp/startup_marks";

/// The uptime (ms) at which ocbmd last raised `/tmp/host_present` to 1: the origin. One number, written whole.
pub const ORIGIN: &str = "/tmp/host_present_uptime";

/// The kind of a marks line: an event's uptime.
pub const STAMP: &str = "t";
/// The kind of a marks line: a duration in ms.
pub const DURATION: &str = "d";

/// The origin's key in the marks (`t host <ms>`).
pub const ORIGIN_KEY: &str = "host";
/// The host-facing USB link's last edge into "configured", as the supervisor saw it (`t usb_edge <ms>`).
pub const USB_EDGE_KEY: &str = "usb_edge";

/// The events, in the line's order: the radios, Bluetooth and iAP2 (btd), the Wi-Fi handoff and the AP, the AirPlay
/// session (carplayd) and the first frame.
pub const EVENTS: [&str; 30] = [
    "sup_radios",
    "bt_power",
    "btd_start",
    "bt_up",
    "page_start",
    "acl",
    "sdp",
    "rfcomm",
    "cert_req",
    "sign1_start",
    "sign1_end",
    "ident",
    "wifi_req",
    "wifi_rsp",
    "ap_start",
    "ap_up",
    "ap_enabled",
    "sta_assoc",
    "dhcp_ack",
    "carplayd_start",
    "svc_seen",
    "dial_ok",
    "dial",
    "pv_done",
    "auth_in",
    "auth_out",
    "record_in",
    "record",
    "screen_setup",
    "first_frame",
];

/// The chip durations, after the events: btd's signature and certificate read, then auth-setup's.
pub const DURATIONS: [&str; 4] = ["chip1_ms", "cert1_ms", "chip2_ms", "cert2_ms"];

/// The last key: how many whole seconds before the origin the host-facing USB link last came up ([`USB_EDGE_KEY`]).
pub const USB_EDGE_S: &str = "usb_edge_s";

/// Whether the connect started from the armed state (`wireless::armed`: Bluetooth powered but invisible between host
/// sessions, the host's arrival a "go"): `t bt_armed <ms>`, stamped by btd once its go proved the controller. The line
/// prints it right after `sup_radios`, as `bt_armed=1`, else `bt_armed=0`; it is a flag, not a time. From the armed
/// state `bt_power` is `-`: no `bt_on` ran.
pub const ARMED_KEY: &str = "bt_armed";

/// The radio bring-up's stamps (the supervisor's, `radio_hal.sh`'s and `radio_ap_up.sh`'s, btd's [`ARMED_KEY`], and
/// hostapd's own AP-ENABLED as btd's AP watch saw it), which btd carries into each session it starts under that
/// bring-up. `sup_radios` opens a bring-up.
pub const RADIO: [&str; 7] = ["sup_radios", USB_EDGE_KEY, "bt_power", ARMED_KEY, "ap_start", "ap_up", "ap_enabled"];

/// The widest value the line prints: 13 digits, over 300 years of uptime in ms, more than this 32-bit kernel's clock
/// can hold. A larger one cannot have come from the clock and prints `-`, so the line's length has a bound
/// (the widest-line test) under the 1024 bytes ocbmd forwards (`ocbm_proto::LOG_MAX_LINE`).
pub const MAX_VALUE: u64 = 9_999_999_999_999;

/// The uptime in ms on CLOCK_BOOTTIME (`/proc/uptime`'s clock; CLOCK_MONOTONIC off Linux, for the tests), or None if
/// the clock can't be read.
pub fn now_ms() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    // zeroed() and filled by the kernel, not a struct literal: under musl32_time64 timespec has private padding.
    // SAFETY: a timespec of our own for clock_gettime to fill.
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(CLOCK, &mut ts) } != 0 {
        return None;
    }
    Some(ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000)
}

/// One marks line: `<kind> <key> <ms>`.
pub fn entry(kind: &str, key: &str, ms: u64) -> String {
    format!("{kind} {key} {ms}\n")
}

/// The ms of the first `<kind> <key> <ms>` line in `marks`.
pub fn find(marks: &str, kind: &str, key: &str) -> Option<u64> {
    marks.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        let (Some(k), Some(name), Some(ms), None) = (w.next(), w.next(), w.next(), w.next()) else { return None };
        (k == kind && name == key).then(|| ms.parse().ok()).flatten()
    })
}

/// Write `ms` (an uptime) to `path` as the origin ([`ORIGIN`]): aside and renamed, so a reader never sees half of it.
/// False when it could not be written (the line's events are then `-`).
pub fn write_origin(path: &str, ms: u64) -> bool {
    let tmp = format!("{path}.tmp.{}", std::process::id());
    if std::fs::write(&tmp, format!("{ms}\n")).and_then(|_| std::fs::rename(&tmp, path)).is_ok() {
        return true;
    }
    let _ = std::fs::remove_file(&tmp);
    false
}

/// The origin [`write_origin`] left at `path`.
pub fn read_origin(path: &str) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The `[timeline] v1 ...` line for a session's `marks` and carplayd's `own` stamps and durations (`(key, ms)`; an
/// event's ms is its uptime): every event as ms since the origin (`t host`), every duration as noted, and
/// `usb_edge_s` as whole seconds before the origin. A value not there, an event before the origin, or one past
/// [`MAX_VALUE`] is `-`; so is every event when there is no origin. An own value wins over one in the marks.
/// Right after `sup_radios`, [`ARMED_KEY`]: 1 when the marks hold its stamp (not one before the origin), else 0.
pub fn line(marks: &str, own: &[(&str, u64)]) -> String {
    let value = |kind: &str, key: &str| own.iter().find(|(k, _)| *k == key).map(|&(_, v)| v).or_else(|| find(marks, kind, key));
    let origin = find(marks, STAMP, ORIGIN_KEY);
    let armed = find(marks, STAMP, ARMED_KEY).is_some_and(|at| origin.is_none_or(|o| at >= o));
    let mut out = String::from("[timeline] v1");
    for key in EVENTS {
        let since = origin.zip(value(STAMP, key)).and_then(|(o, at)| at.checked_sub(o));
        push(&mut out, key, since);
        if key == "sup_radios" {
            push(&mut out, ARMED_KEY, Some(u64::from(armed)));
        }
    }
    for key in DURATIONS {
        push(&mut out, key, value(DURATION, key));
    }
    // An edge the supervisor saw only after the origin (its tick is 1 s) came less than a tick before: 0.
    let usb = origin.zip(find(marks, STAMP, USB_EDGE_KEY)).map(|(o, edge)| o.saturating_sub(edge) / 1000);
    push(&mut out, USB_EDGE_S, usb);
    out
}

fn push(out: &mut String, key: &str, value: Option<u64>) {
    use std::fmt::Write as _;
    let _ = match value.filter(|&v| v <= MAX_VALUE) {
        Some(v) => write!(out, " {key}={v}"),
        None => write!(out, " {key}=-"),
    };
}

/// Whether `s` is a timeline line as Carlink stores one: `^\[timeline\] v1( [a-z0-9_]+=(-|\d+))+$`.
pub fn is_line(s: &str) -> bool {
    let Some(pairs) = s.strip_prefix("[timeline] v1 ") else { return false };
    pairs.split(' ').all(|pair| match pair.split_once('=') {
        Some((key, value)) => {
            !key.is_empty()
                && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                && (value == "-" || (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())))
        }
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key, in the line's order.
    fn keys(line: &str) -> Vec<&str> {
        line.split(' ').skip(2).map(|p| p.split_once('=').unwrap().0).collect()
    }

    fn get<'a>(line: &'a str, key: &str) -> &'a str {
        line.split(' ').skip(2).find_map(|p| p.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v)).unwrap()
    }

    #[test]
    fn the_line_keeps_the_order_and_the_grammar() {
        let line = line("", &[]);
        let mut want: Vec<&str> = EVENTS.to_vec();
        want.insert(1, ARMED_KEY);
        want.extend(DURATIONS);
        want.push(USB_EDGE_S);
        assert_eq!(keys(&line), want);
        assert!(is_line(&line), "{line}");
        assert_eq!(get(&line, ARMED_KEY), "0", "not armed: 0, never -");
        assert!(
            line.split(' ').skip(2).filter(|p| !p.starts_with("bt_armed=")).all(|p| p.ends_with("=-")),
            "nothing stamped, no origin: every value -"
        );
    }

    /// A connect from the armed state (`wireless::armed`): `bt_armed=1` right after `sup_radios`, and `bt_power` `-`,
    /// since no `bt_on` ran. A stamp from before the origin (an earlier host's bring-up) is not this connect's.
    #[test]
    fn an_armed_connect_says_so_right_after_the_radios() {
        let marks = "t host 61234\nt sup_radios 61270\nt usb_edge 49700\nt btd_start 61290\nt bt_armed 61300\nt bt_up 61301\n\
                     t page_start 61310\n";
        let l = line(marks, &[]);
        assert!(l.starts_with("[timeline] v1 sup_radios=36 bt_armed=1 bt_power=- btd_start=56 bt_up=67 page_start=76 "), "{l}");
        assert!(is_line(&l), "{l}");
        assert_eq!(get(&line("t host 61234\nt bt_armed 5000\n", &[]), ARMED_KEY), "0", "before the origin: another host's");
        assert_eq!(get(&line("t bt_armed 5000\n", &[]), ARMED_KEY), "1", "no origin to place it against: the record's own");
        assert_eq!(get(&line("t host 61234\nt bt_power 61500\n", &[]), ARMED_KEY), "0", "bt_on ran: not armed");
        assert!(RADIO.contains(&ARMED_KEY), "carried with the bring-up into each session under it");
    }

    #[test]
    fn every_event_counts_from_the_origin_and_a_missing_one_is_a_dash() {
        // A connect as it might fall: the origin at 61.234 s of uptime, the shell's stamps to the 10 ms they have.
        let marks = "host_present 1763300000123\npreload done\nt host 61234\nt btd_start 62510\n\
                     t sup_radios 61640\nt usb_edge 49700\nt bt_power 62480\nt bt_up 62520\nt page_start 62530\nt acl 62790\n\
                     t sdp 62950\nt rfcomm 63100\nt cert_req 63180\nd cert1_ms 41\nt sign1_start 63300\nd chip1_ms 298\n\
                     t sign1_end 63610\nt ident 63800\nt wifi_req 63900\nt wifi_rsp 63930\nt ap_start 62900\nt ap_up 63110\n\
                     t ap_enabled 63880\nt sta_assoc 64500\nt dhcp_ack 64800\n";
        let own = [
            ("carplayd_start", 64400),
            ("svc_seen", 65100),
            ("dial_ok", 65140),
            ("dial", 65150),
            ("pv_done", 65600),
            ("auth_in", 65650),
            ("auth_out", 66000),
            ("record_in", 66080),
            ("record", 66300),
            ("screen_setup", 66350),
            ("first_frame", 66700),
            ("chip2_ms", 301),
            ("cert2_ms", 40),
        ];
        let l = line(marks, &own);
        assert_eq!(
            l,
            "[timeline] v1 sup_radios=406 bt_armed=0 bt_power=1246 btd_start=1276 bt_up=1286 page_start=1296 acl=1556 sdp=1716 \
             rfcomm=1866 cert_req=1946 sign1_start=2066 sign1_end=2376 ident=2566 wifi_req=2666 wifi_rsp=2696 \
             ap_start=1666 ap_up=1876 ap_enabled=2646 sta_assoc=3266 dhcp_ack=3566 carplayd_start=3166 svc_seen=3866 \
             dial_ok=3906 dial=3916 pv_done=4366 auth_in=4416 auth_out=4766 record_in=4846 record=5066 screen_setup=5116 \
             first_frame=5466 chip1_ms=298 cert1_ms=41 chip2_ms=301 cert2_ms=40 usb_edge_s=11"
        );
        assert!(is_line(&l));
    }

    #[test]
    fn without_an_origin_every_event_is_a_dash_and_the_durations_stand() {
        let l = line("t btd_start 62510\nd chip1_ms 298\nt usb_edge 49700\n", &[("first_frame", 66700), ("cert2_ms", 40)]);
        assert_eq!(get(&l, "btd_start"), "-");
        assert_eq!(get(&l, "first_frame"), "-");
        assert_eq!(get(&l, "usb_edge_s"), "-", "no origin to count back from");
        assert_eq!(get(&l, "chip1_ms"), "298");
        assert_eq!(get(&l, "cert2_ms"), "40");
        assert!(is_line(&l));
    }

    #[test]
    fn an_event_before_the_origin_is_a_dash_and_the_first_of_each_stands() {
        let marks = "t host 5000\nt page_start 4000\nt acl 6000\nt acl 7000\nd chip1_ms 100\nd chip1_ms 200\n";
        let l = line(marks, &[("acl", 9000)]);
        assert_eq!(get(&l, "page_start"), "-", "before the origin: another host's");
        assert_eq!(get(&l, "acl"), "4000", "an own value wins over the marks'");
        assert_eq!(get(&line(marks, &[]), "acl"), "1000", "the first in the marks");
        assert_eq!(get(&l, "chip1_ms"), "100");
    }

    #[test]
    fn the_usb_edge_is_whole_seconds_before_the_origin() {
        assert_eq!(get(&line("t host 61234\nt usb_edge 49700\n", &[]), "usb_edge_s"), "11");
        assert_eq!(get(&line("t host 61234\nt usb_edge 60300\n", &[]), "usb_edge_s"), "0", "under a second");
        assert_eq!(get(&line("t host 61234\nt usb_edge 61800\n", &[]), "usb_edge_s"), "0", "seen a tick late");
        assert_eq!(get(&line("t host 61234\n", &[]), "usb_edge_s"), "-", "no edge known");
    }

    #[test]
    fn a_marks_line_is_read_whole_or_not_at_all() {
        let marks = "t acl\nt acl x\nt acl 12 13\nd acl 99\nt acl 120\n";
        assert_eq!(find(marks, STAMP, "acl"), Some(120));
        assert_eq!(find(marks, DURATION, "acl"), Some(99));
        assert_eq!(find("tally absent\nbt_up 1200\n", STAMP, "bt_up"), None, "[startup]'s own lines are no stamps");
        assert_eq!(entry(STAMP, "acl", 120), "t acl 120\n");
        assert_eq!(find(&entry(STAMP, "acl", 120), STAMP, "acl"), Some(120));
        assert_eq!(find("t sup_radios 0050\n", STAMP, "sup_radios"), Some(50), "the shell's, below a second of uptime");
    }

    #[test]
    fn the_grammar_is_carlinks() {
        assert!(is_line("[timeline] v1 a=1"));
        assert!(is_line("[timeline] v1 a_1=- b=0"));
        assert!(!is_line("[timeline] v1"), "at least one pair");
        assert!(!is_line("[timeline] v1 "), "a pair, not a space");
        assert!(!is_line("[timeline] v1 a=1 "), "no trailing space");
        assert!(!is_line("[timeline] v1  a=1"), "one space between");
        assert!(!is_line("[timeline] v1 A=1"), "lowercase keys");
        assert!(!is_line("[timeline] v1 a=-1"), "no sign");
        assert!(!is_line("[timeline] v1 a="), "a value");
        assert!(!is_line("[timeline] v1 a=1.5"), "whole numbers");
        assert!(!is_line("[timeline] v1 =1"), "a key");
        assert!(!is_line("[timeline] v2 a=1"));
        assert!(!is_line("@1763300000123 [timeline] v1 a=1"), "the write-time stamp is ocbmd's to strip");
    }

    /// ocbmd clips a forwarded line at LOG_MAX_LINE: the line fits whole even with every value at its widest
    /// ([`MAX_VALUE`], 13 digits), usb_edge_s included, and `bt_armed` at its one digit; one wider prints `-`.
    #[test]
    fn the_widest_line_fits_one_forwarded_log_line() {
        let origin = MAX_VALUE * 1000;
        let mut marks = format!("t host {origin}\nt usb_edge 0\nt {ARMED_KEY} {origin}\n");
        for key in EVENTS {
            marks.push_str(&entry(STAMP, key, origin + MAX_VALUE));
        }
        for key in DURATIONS {
            marks.push_str(&entry(DURATION, key, MAX_VALUE));
        }
        let l = line(&marks, &[]);
        assert!(
            l.split(' ').skip(2).filter(|p| *p != "bt_armed=1").all(|p| p.ends_with(&format!("={MAX_VALUE}"))),
            "every value at its widest: {l}"
        );
        assert!(l.contains(" bt_armed=1 "), "{l}");
        assert!(is_line(&l));
        assert_eq!(l.len(), 827, "the bound the docs give");
        assert!(l.len() < ocbm_proto::LOG_MAX_LINE, "{} bytes", l.len());

        let past = line(&format!("t host 0\nt acl {}\nd chip1_ms {}\n", MAX_VALUE + 1, u64::MAX), &[]);
        assert!(past.contains(" acl=- ") && past.contains(" chip1_ms=- "), "past the widest: a dash, {past}");
    }

    #[test]
    fn the_clock_runs_and_the_origin_round_trips() {
        let a = now_ms().expect("a monotonic clock");
        let b = now_ms().unwrap();
        assert!(b >= a);
        let path = std::env::temp_dir().join(format!("box_common_origin_{}", std::process::id()));
        let p = path.to_str().unwrap();
        assert!(write_origin(p, b));
        assert_eq!(read_origin(p), Some(b), "written whole");
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(read_origin(p), None);
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_origin(p), None);
    }
}
