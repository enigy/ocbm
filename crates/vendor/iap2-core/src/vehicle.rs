//! The car's own status for the iPhone: Apple's VehicleStatus plane, built the way the location plane is
//! (`location.rs`). LIVI research item 3, off unless the app pushes it.
//!
//! DIRECTION (Apple's `source`, `spec.rs`): `StartVehicleStatusUpdates` (0xA100) and
//! `StopVehicleStatusUpdates` (0xA102) come from the DEVICE, so they are declared in param 7;
//! `VehicleStatusUpdate` (0xA101) is OURS, in param 6. Identify param 21 (`VehicleStatusComponent`) says what
//! the car can report, and param 20's EngineType what it runs on.
//!
//! CONTENT IS APP-DRIVEN (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). The app pushes what it can read
//! (`vehicleStatus: [range, outsideTemperature, …]`, Apple's names under OUR top-level key, as `location:` is)
//! and its engine types (`iapConfig.vehicleInfo.engineTypes`, Apple's key). The box declares both on the tunnel,
//! tells the app what the phone's Start asked for (`{"kind":"vehicleStatus"}` on the metadata seam), and sends
//! the app's values (CH_INPUT `INPUT_VEHICLE_STATUS`) as 0xA101, only those the phone asked for. With nothing
//! pushed the tunnel's Identify is as before: Gasoline, no param 21.
//!
//! SCOPE: the AirPlayTunnel arm only (`message.rs`). The BT-time Identify stays byte-pinned, and the wired arm
//! (iap2d) reads Apple's own `iapConfig.vehicleStatus` and still declares none of the messages (C-4), which is
//! why the tunnel's capabilities ride a key of their own. Unproven on a device: whether iOS takes param 21 with
//! its messages on the tunnel is the experiment, and a reject naming params 6, 7 or 21 retries without it.

use std::sync::{Mutex, OnceLock};

use crate::config::VehicleIdentity;
use crate::message::Tlv;
use carplay_metadata::tlv::parse_groups;

static ARMED: OnceLock<VehicleIdentity> = OnceLock::new();

/// Arm the tunnel's vehicle identity: the app's engine types (param 20) and the status it can report
/// (param 21, `status_caps`). First arm wins per process, as the metadata tier and location do (carplayd is
/// one process per session). False when one is armed already.
pub fn arm(identity: VehicleIdentity) -> bool {
    let caps = identity.status_caps.as_ref().map_or(0, Vec::len);
    let engines = identity.engine_types.clone();
    let armed = ARMED.set(identity).is_ok();
    if armed {
        eprintln!("[vehicle] the tunnel declares engine types {engines:?} and {caps} status capabilities");
    }
    armed
}

/// The tunnel Identify's vehicle identity: the armed one, or the baseline (Gasoline, no param 21).
pub fn tunnel_identity() -> VehicleIdentity {
    ARMED.get().cloned().unwrap_or_else(VehicleIdentity::baseline)
}

/// What the tunnel declares it can report (param 21's capability ids), empty when nothing was pushed.
pub fn declared() -> &'static [u16] {
    ARMED.get().and_then(|i| i.status_caps.as_deref()).unwrap_or(&[])
}

/// What the phone's Start asked for, until its Stop or the session's end.
static WANTED: Mutex<Option<Vec<u16>>> = Mutex::new(None);

fn wanted() -> std::sync::MutexGuard<'static, Option<Vec<u16>>> {
    WANTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The phone's 0xA100: the presence flags it sets (Apple's ids 3 to 20) are what it wants, of what is
/// declared. Returns the record for the app, or nothing when nothing is declared.
pub fn on_start(body: &[u8]) -> String {
    on_start_with(declared(), body)
}

fn on_start_with(declared: &[u16], body: &[u8]) -> String {
    if declared.is_empty() {
        eprintln!("[vehicle] RX 0xA100 StartVehicleStatusUpdates, but nothing is declared — ignored");
        return String::new();
    }
    let mut want: Vec<u16> = parse_groups(body).into_iter().map(|(id, _)| id).filter(|id| declared.contains(id)).collect();
    want.sort_unstable();
    want.dedup();
    let record = start_record(&want);
    eprintln!("[vehicle] RX 0xA100 StartVehicleStatusUpdates → {record}");
    *wanted() = Some(want);
    record
}

/// The phone's 0xA102: stop sending, and tell the app.
pub fn on_stop() -> String {
    let was = wanted().take().is_some();
    eprintln!("[vehicle] RX 0xA102 StopVehicleStatusUpdates (was started: {was})");
    STOP_RECORD.to_string()
}

pub const STOP_RECORD: &str = "{\"kind\":\"vehicleStatus\",\"state\":\"stop\"}";

/// `{"kind":"vehicleStatus","state":"start","want":[3,4,…]}`: Apple's 0xA101 parameter ids the phone asked for.
pub fn start_record(want: &[u16]) -> String {
    let ids: Vec<String> = want.iter().map(u16::to_string).collect();
    format!("{{\"kind\":\"vehicleStatus\",\"state\":\"start\",\"want\":[{}]}}", ids.join(","))
}

/// The tunnel ended: a new session waits for a new Start.
pub fn reset() {
    wanted().take();
}

/// How the box sends each value it takes from the app, by 0xA101 parameter id (Apple's table, the Mac's
/// `i2mspec_dump.py` of 10 Nov). The rest (the wiper group, the alerts list, the EV block) waits for a car that
/// has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// uint16, big-endian: a range in km, the barometric pressure in mbar.
    U16,
    /// int16, big-endian: a temperature in °C.
    I16,
    /// bool, one byte: a warning, the passenger seat.
    Bool,
}

fn kind_of(id: u16) -> Option<Kind> {
    match id {
        3 | 9..=12 | 18 => Some(Kind::U16),
        4 | 5 => Some(Kind::I16),
        6 | 13..=16 | 20 => Some(Kind::Bool),
        _ => None,
    }
}

/// The app's values, `[id u8][value]…` (u16 and i16 big-endian, a bool one byte), as the 0xA101 body with only
/// what the phone's Start asked for. `Ok(None)` before a Start, or when none of the values was asked for; `Err`
/// for a frame the box can't read, so nothing half-read reaches the phone.
pub fn update_body(values: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let Some(want) = wanted().clone() else {
        return Ok(None);
    };
    update_body_with(&want, values)
}

fn update_body_with(want: &[u16], values: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut t = Tlv::new();
    let mut at = 0;
    while at < values.len() {
        let id = u16::from(values[at]);
        let kind = kind_of(id).ok_or_else(|| format!("parameter {id} isn't one the box sends"))?;
        let size = if kind == Kind::Bool { 1 } else { 2 };
        let v = values.get(at + 1..at + 1 + size).ok_or_else(|| format!("parameter {id} is cut short"))?;
        if want.contains(&id) {
            match kind {
                Kind::U16 => t.u16v(id, u16::from_be_bytes([v[0], v[1]])),
                Kind::I16 => t.bytes(id, v),
                Kind::Bool => t.boolv(id, v[0] != 0),
            }
        }
        at += 1 + size;
    }
    Ok((!t.0.is_empty()).then_some(t.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::vehicle_status as cap;

    const TRUCK: [u16; 5] = [cap::RANGE, cap::OUTSIDE_TEMPERATURE, cap::RANGE_WARNING, cap::RANGE_DIESEL, cap::RANGE_WARNING_DIESEL];

    fn start(flags: &[u16]) -> Vec<u8> {
        let mut t = Tlv::new();
        for &f in flags {
            t.none(f);
        }
        t.0
    }

    #[test]
    fn the_phones_start_names_what_it_wants_of_what_is_declared() {
        let record = on_start_with(&TRUCK, &start(&[cap::OUTSIDE_TEMPERATURE, cap::RANGE, cap::INSIDE_TEMPERATURE]));
        assert_eq!(record, r#"{"kind":"vehicleStatus","state":"start","want":[3,4]}"#, "inside temperature isn't declared");
        assert_eq!(on_start_with(&[], &start(&[cap::RANGE])), "", "nothing declared: ignored");
        assert_eq!(start_record(&[]), r#"{"kind":"vehicleStatus","state":"start","want":[]}"#);
    }

    #[test]
    fn an_update_carries_only_what_was_asked_for_in_apples_types() {
        // Range 512 km, outside 16 °C, then -7 °C as int16, the low-range warning on, diesel range 512.
        let values = [3, 0x02, 0x00, 4, 0xFF, 0xF9, 6, 1, 10, 0x02, 0x00];
        let body = update_body_with(&[cap::RANGE, cap::OUTSIDE_TEMPERATURE, cap::RANGE_WARNING], &values).unwrap().unwrap();
        let mut want = Tlv::new();
        want.u16v(3, 512);
        want.bytes(4, &[0xFF, 0xF9]);
        want.boolv(6, true);
        assert_eq!(body, want.0, "diesel range wasn't asked for, so it isn't sent");
        assert_eq!(update_body_with(&[cap::RANGE_DIESEL], &[3, 0, 1]).unwrap(), None, "nothing asked for is there");
    }

    #[test]
    fn a_frame_the_box_cant_read_sends_nothing() {
        assert!(update_body_with(&TRUCK, &[3, 0x02]).is_err(), "cut short");
        assert!(update_body_with(&TRUCK, &[19, 0]).is_err(), "the alerts list isn't one the box sends");
        assert!(update_body_with(&TRUCK, &[21, 0, 0, 0, 0]).is_err(), "nor the EV block");
        assert_eq!(update_body_with(&TRUCK, &[]).unwrap(), None);
    }
}
