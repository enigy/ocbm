//! The car's own position for the iPhone: the accessory-sourced location plane
//! (docs/carplay/05_METADATA_AND_CONTROLS.md §2.1). LIVI research item 4, OFF by default.
//!
//! DIRECTION (Apple's `source`, `spec.rs:711-713`): `StartLocationInformation` (0xFFFA) and
//! `StopLocationInformation` (0xFFFC) come from the DEVICE, so they are declared in param 7;
//! `LocationInformation` (0xFFFB) is OURS, so it is declared in param 6. Identify param 22
//! (`LocationInformationComponent`) says which sentences we can produce. `features::Feature` cannot
//! express this inverted shape (§2.1's warning), which is why this is its own module.
//!
//! CONTENT IS APP-DRIVEN (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). The app pushes the sentences it can build
//! (`location: [GPGGA, GPRMC]`) and relays each one on CH_INPUT (`INPUT_LOCATION`). The box declares
//! exactly those, tells the app when the phone starts and stops (`{"kind":"location"}` on the metadata
//! seam), and forwards only what the phone selected, no faster than its `NMEASentence*IntervalMin`.
//! With no `location:` pushed nothing is declared, the Identify is byte-identical and every
//! INPUT_LOCATION is dropped.
//!
//! SCOPE: only the AirPlayTunnel arm declares it (`message.rs`). The BT-time Identify stays
//! byte-pinned, and the wired arm (iap2d) does not arm it. Unproven on a device: whether iOS takes a
//! location declaration on the tunnel's Identify is the experiment.

use std::sync::{Mutex, OnceLock};

use crate::message::location_info as cap;
use carplay_metadata::location::{parse_start_location_information, LocationSelection};

/// One sentence type of Apple's location plane: its NMEA name and its param-22 capability sub-id.
/// The StartLocationInformation selector for each is read in [`selected`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sentence {
    pub name: &'static str,
    pub capability: u16,
}

/// Apple's seven, in param-22 sub-id order (`message.rs` `location_info`).
pub const SENTENCES: [Sentence; 7] = [
    Sentence { name: "GPGGA", capability: cap::GLOBAL_POSITIONING_SYSTEM_FIX_DATA },
    Sentence { name: "GPRMC", capability: cap::RECOMMENDED_MINIMUM_SPECIFIC_GPS_TRANSIT_DATA },
    Sentence { name: "GPGSV", capability: cap::GPS_SATELLITES_IN_VIEW },
    Sentence { name: "PASCD", capability: cap::VEHICLE_SPEED_DATA },
    Sentence { name: "PAGCD", capability: cap::VEHICLE_GYRO_DATA },
    Sentence { name: "PAACD", capability: cap::VEHICLE_ACCELEROMETER_DATA },
    Sentence { name: "GPHDT", capability: cap::VEHICLE_HEADING_DATA },
];

/// NMEA-0183's longest sentence, `$` to the checksum, without the CR LF.
pub const MAX_SENTENCE: usize = 80;

/// The pushed `location:` list as sentences, in Apple's order, each once. Unknown names are left out
/// and logged: a typo declares less, never something we can't produce.
pub fn resolve(names: &[String]) -> Vec<Sentence> {
    for n in names {
        if !SENTENCES.iter().any(|s| s.name.eq_ignore_ascii_case(n.trim())) {
            eprintln!("[location] pushed sentence {n:?} isn't one of Apple's seven — left out");
        }
    }
    SENTENCES
        .iter()
        .copied()
        .filter(|s| names.iter().any(|n| s.name.eq_ignore_ascii_case(n.trim())))
        .collect()
}

static ARMED: OnceLock<Vec<Sentence>> = OnceLock::new();

/// Arm the declaration from the pushed list. First arm wins per process, like the metadata tier
/// (`features::arm_pushed_policy`), so the Identify and the forwarding can't disagree in a session.
pub fn arm(names: &[String]) -> bool {
    let wanted = resolve(names);
    if wanted.is_empty() {
        return false;
    }
    let shown: Vec<&str> = wanted.iter().map(|s| s.name).collect();
    match ARMED.set(wanted) {
        Ok(()) => {
            eprintln!("[location] ARMED from pushed config: {shown:?} (first arm wins)");
            true
        }
        Err(rejected) => {
            if ARMED.get() != Some(&rejected) {
                eprintln!("[location] pushed {shown:?} IGNORED: another list is already armed this process");
            }
            false
        }
    }
}

/// What this process declares; empty when nothing was pushed.
pub fn armed() -> &'static [Sentence] {
    ARMED.get().map(Vec::as_slice).unwrap_or(&[])
}

/// The param-22 capability sub-ids for `message.rs`; empty = declare nothing.
pub fn capabilities() -> Vec<u16> {
    armed().iter().map(|s| s.capability).collect()
}

/// Whether the phone's Start selected `name`, and its minimum interval for it.
fn selected(sel: &LocationSelection, name: &str) -> (bool, Option<u32>) {
    match name {
        "GPGGA" => (sel.gps_fix_data, sel.gpgga_interval_min_ms),
        "GPRMC" => (sel.recommended_minimum_gps_transit, sel.gprmc_interval_min_ms),
        "GPGSV" => (sel.gps_satellites_in_view, sel.gpgsv_interval_min_ms),
        "PASCD" => (sel.vehicle_speed_data, sel.pascd_interval_min_ms),
        "PAGCD" => (sel.vehicle_gyro_data, sel.pagcd_interval_min_ms),
        "PAACD" => (sel.vehicle_accelerometer_data, sel.paacd_interval_min_ms),
        "GPHDT" => (sel.vehicle_heading_data, sel.gphdt_interval_min_ms),
        _ => (false, None),
    }
}

/// The phone's current request, from its Start until its Stop or the tunnel's end.
#[derive(Debug, Default)]
pub struct Live {
    sel: LocationSelection,
    /// When each of [`SENTENCES`] last went out, by index.
    last_ms: [Option<u64>; 7],
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

fn live() -> std::sync::MutexGuard<'static, Option<Live>> {
    LIVE.lock().unwrap_or_else(|p| p.into_inner())
}

/// A sentence from the app, against what the phone asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    Send,
    /// No Start from the phone yet, or it stopped.
    NotStarted,
    /// Not one this box declared.
    NotDeclared,
    /// Declared, but the phone's Start didn't select it.
    NotSelected,
    /// Sooner than the phone's minimum interval for it.
    TooSoon,
    /// Not `$<name>,…*<checksum>` with a checksum that holds, or longer than [`MAX_SENTENCE`].
    Malformed,
}

/// The sentence's name when it is well formed: printable ASCII, `$`, a five-letter name, a comma,
/// and an XOR checksum that holds. A trailing CR LF is allowed.
pub fn sentence_name(s: &str) -> Option<&str> {
    let s = s.trim_end_matches(['\r', '\n']);
    if s.len() > MAX_SENTENCE || !s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return None;
    }
    let (body, sum) = s.strip_prefix('$')?.rsplit_once('*')?;
    let want = u8::from_str_radix(sum, 16).ok().filter(|_| sum.len() == 2)?;
    if body.bytes().fold(0u8, |a, b| a ^ b) != want {
        return None;
    }
    let name = body.split(',').next()?;
    (name.len() == 5 && body.len() > 5).then_some(name)
}

/// [`admit`] against an explicit declaration and request, for tests.
pub fn admit_with(declared: &[Sentence], live: Option<&mut Live>, sentence: &str, now_ms: u64) -> Admit {
    let Some(name) = sentence_name(sentence) else {
        return Admit::Malformed;
    };
    let Some(live) = live else {
        return Admit::NotStarted;
    };
    let Some(i) = SENTENCES.iter().position(|s| s.name == name) else {
        return Admit::NotDeclared;
    };
    if !declared.iter().any(|s| s.name == name) {
        return Admit::NotDeclared;
    }
    let (on, min_ms) = selected(&live.sel, name);
    if !on {
        return Admit::NotSelected;
    }
    if let (Some(last), Some(min)) = (live.last_ms[i], min_ms) {
        if now_ms.saturating_sub(last) < u64::from(min) {
            return Admit::TooSoon;
        }
    }
    live.last_ms[i] = Some(now_ms);
    Admit::Send
}

/// Whether one sentence from the app goes to the phone now; a `Send` also stamps its time.
pub fn admit(sentence: &str, now_ms: u64) -> Admit {
    admit_with(armed(), live().as_mut(), sentence, now_ms)
}

/// The `{"kind":"location"}` record for a Start: the sentences the phone selected that we declared
/// (what the app should produce) and the minimum interval for each the phone gave.
pub fn start_record(declared: &[Sentence], sel: &LocationSelection) -> String {
    let mut names = Vec::new();
    let mut gaps = Vec::new();
    for s in declared {
        let (on, min) = selected(sel, s.name);
        if on {
            names.push(format!("\"{}\"", s.name));
            if let Some(ms) = min {
                gaps.push(format!("\"{}\":{ms}", s.name));
            }
        }
    }
    format!(
        "{{\"kind\":\"location\",\"state\":\"start\",\"sentences\":[{}],\"intervalMs\":{{{}}}}}",
        names.join(","),
        gaps.join(",")
    )
}

/// The phone's 0xFFFA: keep its request and return the record for the app. Empty (no record) when
/// this process declared nothing, which the phone can't then have seen.
pub fn on_start(body: &[u8]) -> String {
    let sel = parse_start_location_information(body);
    let declared = armed();
    if declared.is_empty() {
        eprintln!("[location] RX 0xFFFA StartLocationInformation, but nothing is declared — ignored");
        return String::new();
    }
    *live() = Some(Live { sel, last_ms: [None; 7] });
    let record = start_record(declared, &sel);
    eprintln!("[location] RX 0xFFFA StartLocationInformation: {sel:?} → {record}");
    record
}

/// The phone's 0xFFFC: stop forwarding and tell the app.
pub fn on_stop() -> String {
    let was = live().take().is_some();
    eprintln!("[location] RX 0xFFFC StopLocationInformation (was started: {was})");
    STOP_RECORD.to_string()
}

pub const STOP_RECORD: &str = "{\"kind\":\"location\",\"state\":\"stop\"}";

/// The tunnel ended: a new session waits for a new Start.
pub fn reset() {
    live().take();
}

/// The 0xFFFB body: one `NMEASentence` (param 0, utf8), without the CR LF.
pub fn information_body(sentence: &str) -> Vec<u8> {
    let mut t = crate::message::Tlv::new();
    t.str(0, sentence.trim_end_matches(['\r', '\n']));
    t.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const GGA: &str = "$GPGGA,172814.00,4228.3521,N,08308.9126,W,1,09,0.9,190.2,M,,M,,*65";

    /// A well-formed sentence of `name`, its checksum computed.
    fn sentence(name: &str) -> String {
        let body = format!("{name},1,2,3");
        format!("${body}*{:02X}", body.bytes().fold(0u8, |a, b| a ^ b))
    }

    fn started(sel: LocationSelection) -> Live {
        Live { sel, last_ms: [None; 7] }
    }

    #[test]
    fn the_pushed_list_resolves_in_apples_order_and_leaves_unknown_names_out() {
        let got = resolve(&["gprmc".into(), "GPGGA".into(), "GNGGA".into(), "GPRMC".into()]);
        assert_eq!(got.iter().map(|s| s.name).collect::<Vec<_>>(), ["GPGGA", "GPRMC"]);
        assert_eq!(got.iter().map(|s| s.capability).collect::<Vec<_>>(), [17, 18]);
        assert!(resolve(&[]).is_empty());
        assert!(resolve(&["GNGGA".into()]).is_empty(), "only Apple's names declare anything");
    }

    #[test]
    fn a_sentence_is_checked_before_anything_else() {
        assert_eq!(sentence_name(GGA), Some("GPGGA"), "a real fix with its checksum");
        assert_eq!(sentence_name(&format!("{GGA}\r\n")), Some("GPGGA"), "CR LF allowed");
        assert_eq!(sentence_name(&GGA.replace("*65", "*66")), None, "wrong checksum");
        assert_eq!(sentence_name(&GGA.replace("*65", "*6")), None, "short checksum");
        assert_eq!(sentence_name(&GGA[1..]), None, "no $");
        assert_eq!(sentence_name(&sentence("GPGGA").replace('1', "\u{7f}")), None, "not printable");
        assert_eq!(sentence_name(&sentence(&"GPGGA,0".repeat(12))), None, "longer than NMEA allows");
    }

    #[test]
    fn only_a_started_declared_selected_sentence_goes_and_never_faster_than_the_phone_asked() {
        let declared = resolve(&["GPGGA".into(), "GPRMC".into()]);
        let gga = sentence("GPGGA");
        let rmc = sentence("GPRMC");
        assert_eq!(admit_with(&declared, None, &gga, 0), Admit::NotStarted);
        assert_eq!(admit_with(&declared, None, "garbage", 0), Admit::Malformed);

        let mut live = started(LocationSelection { gps_fix_data: true, gpgga_interval_min_ms: Some(1000), ..Default::default() });
        assert_eq!(admit_with(&declared, Some(&mut live), &gga, 5_000), Admit::Send);
        assert_eq!(admit_with(&declared, Some(&mut live), &gga, 5_400), Admit::TooSoon);
        assert_eq!(admit_with(&declared, Some(&mut live), &gga, 6_000), Admit::Send);
        assert_eq!(admit_with(&declared, Some(&mut live), &rmc, 6_000), Admit::NotSelected);
        assert_eq!(admit_with(&declared, Some(&mut live), &sentence("GPGSV"), 6_000), Admit::NotDeclared);
        assert_eq!(admit_with(&declared, Some(&mut live), &sentence("GNGGA"), 6_000), Admit::NotDeclared);

        let mut no_gap = started(LocationSelection { recommended_minimum_gps_transit: true, ..Default::default() });
        assert_eq!(admit_with(&declared, Some(&mut no_gap), &rmc, 1), Admit::Send);
        assert_eq!(admit_with(&declared, Some(&mut no_gap), &rmc, 1), Admit::Send, "no interval = no throttle");
    }

    #[test]
    fn the_start_record_names_what_the_app_should_produce() {
        let declared = resolve(&["GPGGA".into(), "GPRMC".into()]);
        let sel = LocationSelection {
            gps_fix_data: true,
            recommended_minimum_gps_transit: true,
            gps_satellites_in_view: true,
            gprmc_interval_min_ms: Some(500),
            ..Default::default()
        };
        assert_eq!(
            start_record(&declared, &sel),
            r#"{"kind":"location","state":"start","sentences":["GPGGA","GPRMC"],"intervalMs":{"GPRMC":500}}"#,
            "GPGSV was asked for but not declared, so the app isn't asked for it"
        );
        assert_eq!(
            start_record(&declared, &LocationSelection::default()),
            r#"{"kind":"location","state":"start","sentences":[],"intervalMs":{}}"#
        );
        assert_eq!(STOP_RECORD, r#"{"kind":"location","state":"stop"}"#);
    }

    #[test]
    fn the_information_body_is_one_nmea_sentence_param() {
        let body = information_body(&format!("{GGA}\r\n"));
        let mut want = ((4 + GGA.len() + 1) as u16).to_be_bytes().to_vec();
        want.extend_from_slice(&[0, 0]);
        want.extend_from_slice(GGA.as_bytes());
        want.push(0);
        assert_eq!(body, want, "[len][param 0][sentence NUL], CR LF dropped");
        let back = carplay_metadata::location::parse_location_information(&body);
        assert_eq!(back.sentences, [GGA], "the crate's own 0xFFFB parser reads it back");
    }
}
