//! Session-arbitration flags — the `/tmp` substrate the box's daemons share so a live session is
//! never interrupted by another connecting phone.
//!
//! `session_supervisor.sh` arbitrates wired↔wireless CarPlay via `/tmp/carplay_transport`
//! (= "wireless" when the wireless arm owns the box) and `/tmp/projection_owner` (= "wired-cp" for
//! wired). `ProjectionOwner::as_str()` is byte-compatible with the shell: "wireless" == WirelessCp, so
//! an existing `carplay_transport` file still reads correctly.
//!
//! Portable (plain file writes); the shell reads the same paths.

use std::fs;
use std::path::Path;

/// Master app-presence gate (written by ocbmd on CT_SUBSCRIBE). Box idles until an app connects.
pub const HOST_PRESENT: &str = "/tmp/host_present";
/// Wired-USB phone presence (written by session_supervisor; mirrored to the host as SEV_PHONE_*).
pub const PHONE_PRESENT: &str = "/tmp/phone_present";
/// App-commanded radio kill switch (CT_RADIO).
pub const RADIO_OFF: &str = "/tmp/radio_off";
/// Single projection owner across ALL transports (supersedes the CarPlay-only `carplay_transport`).
pub const PROJECTION_OWNER: &str = "/tmp/projection_owner";
/// Legacy CarPlay-only ownership flag still written by the wireless arm (av.rs). Read for back-compat.
pub const CARPLAY_TRANSPORT: &str = "/tmp/carplay_transport";
/// M3's set-up code while ocbmd's set-up session runs (ADDENDUM 5, 24 Nov 2026): four ASCII digits, written by ocbmd
/// (`src/setup.rs`) and shown by the armed btd as the adapter's Bluetooth name (`crates/vendor/wireless` `armed.rs`); no
/// file otherwise. tmpfs, and the name lives on the controller only: a power cut leaves nothing behind.
pub const SETUP_CODE: &str = "/tmp/bt_setup_code";
/// The armed btd's pid, written once it is armed (`armed.rs`): the supervisor's go and ocbmd's set-up code wake it.
pub const BTD_ARMED: &str = "/tmp/.btd_armed";

/// The code in [`SETUP_CODE`]'s text: exactly four ASCII digits, whitespace around them ignored; anything else is none.
pub fn setup_code(text: &str) -> Option<&str> {
    let t = text.trim();
    (t.len() == 4 && t.bytes().all(|b| b.is_ascii_digit())).then_some(t)
}

/// The set-up code put in the file at `path` whole (its reader sees the old text or the new: one writer, ocbmd, so one
/// temporary), or the file removed.
pub fn put_setup_code(path: &str, code: Option<&str>) -> std::io::Result<()> {
    match code {
        Some(c) => {
            let tmp = format!("{path}.tmp");
            fs::write(&tmp, c)?;
            fs::rename(&tmp, path)
        }
        None => match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

/// Who owns the single phone-facing controller / projection session right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionOwner {
    /// Idle — no session; a new phone may claim the box.
    None,
    /// Wired CarPlay (iap2d + carplayd over NCM).
    WiredCp,
    /// Wireless CarPlay (btd: BT + WiFi AP). Byte-compatible with the legacy "wireless".
    WirelessCp,
}

impl ProjectionOwner {
    /// On-disk token. `WirelessCp` == "wireless" so an existing `carplay_transport` reads correctly.
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectionOwner::None => "",
            ProjectionOwner::WiredCp => "wired-cp",
            ProjectionOwner::WirelessCp => "wireless",
        }
    }

    pub fn from_str(s: &str) -> ProjectionOwner {
        match s.trim() {
            "wired-cp" => ProjectionOwner::WiredCp,
            "wireless" | "wireless-cp" => ProjectionOwner::WirelessCp,
            _ => ProjectionOwner::None,
        }
    }

    /// The `PM_*` byte ocbmd puts on the wire in `CT_PROJ_MODE`, so the host app learns WHICH
    /// transport owns the box. One mapping, shared by every box-side writer.
    pub fn wire_code(self) -> u8 {
        match self {
            ProjectionOwner::None => ocbm_proto::PM_NONE,
            ProjectionOwner::WiredCp => ocbm_proto::PM_WIRED_CP,
            ProjectionOwner::WirelessCp => ocbm_proto::PM_WIRELESS_CP,
        }
    }
}

/// Read the current projection owner, preferring the unified flag and falling back to the legacy
/// CarPlay-only `carplay_transport` (so this works before the shell is migrated).
pub fn owner() -> ProjectionOwner {
    if let Ok(s) = fs::read_to_string(PROJECTION_OWNER) {
        let o = ProjectionOwner::from_str(&s);
        if o != ProjectionOwner::None {
            return o;
        }
    }
    match fs::read_to_string(CARPLAY_TRANSPORT) {
        Ok(s) => ProjectionOwner::from_str(&s),
        Err(_) => ProjectionOwner::None,
    }
}

/// Claim the session for `who` (first-come-wins is the caller's responsibility: check `owner()` first).
pub fn set_owner(who: ProjectionOwner) -> std::io::Result<()> {
    match who {
        ProjectionOwner::None => clear_owner(),
        _ => write_atomic(PROJECTION_OWNER, who.as_str()),
    }
}

/// Write `content` to `path` via a same-directory temp + `rename` (ocbmd's `write_flag_atomic`
/// idiom). `fs::write` truncates first, and every reader of these flags is a different process
/// polling them: one landing in that window reads "", which `ProjectionOwner::from_str` maps to
/// `None` — "the box is free to claim" — so a re-write of an owner could hand the session away
/// mid-claim. Rename is atomic: a reader sees the whole old value or the whole new one. The pid
/// suffix keeps the temp private to this writer, so a second writer's temp name can never become
/// this one's torn window.
fn write_atomic(path: &str, content: &str) -> std::io::Result<()> {
    let tmp = format!("{path}.tmp.{}", std::process::id());
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Release the session (idle). Removes the unified flag; leaves the legacy flag to its own writer.
pub fn clear_owner() -> std::io::Result<()> {
    match fs::remove_file(PROJECTION_OWNER) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Presence-flag helper: a flag file "exists and is non-'0'" == true (matches the shell convention).
pub fn is_set(path: &str) -> bool {
    match fs::read_to_string(Path::new(path)) {
        Ok(s) => {
            let t = s.trim();
            !t.is_empty() && t != "0"
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_roundtrip_and_legacy_compat() {
        assert_eq!(ProjectionOwner::from_str("wireless"), ProjectionOwner::WirelessCp);
        assert_eq!(ProjectionOwner::WirelessCp.as_str(), "wireless");
        assert_eq!(ProjectionOwner::from_str("wired-cp"), ProjectionOwner::WiredCp);
        // An owner token from an earlier OCBM that this one no longer has reads as idle.
        assert_eq!(ProjectionOwner::from_str("wired-aa"), ProjectionOwner::None);
        assert_eq!(ProjectionOwner::from_str("wireless-aa"), ProjectionOwner::None);
    }

    /// The torn-read window itself is not observable from a single-threaded test; what IS checkable
    /// is that the value lands whole on both create and overwrite and that no `.tmp.<pid>` sibling
    /// survives — a leftover temp would mean the rename never happened and the reader is on stale
    /// data (the same silent failure as the empty window, one step removed).
    #[test]
    fn write_atomic_replaces_content_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("box-common-flags-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("projection_owner");
        let p = path.to_str().unwrap();

        write_atomic(p, ProjectionOwner::WiredCp.as_str()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), ProjectionOwner::WiredCp.as_str());
        // Overwrite with a SHORTER value: no tail of the longer previous owner may survive.
        write_atomic(p, ProjectionOwner::WirelessCp.as_str()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), ProjectionOwner::WirelessCp.as_str());
        assert_eq!(owner_from(&path), ProjectionOwner::WirelessCp);

        let strays: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(strays.is_empty(), "temp file left behind: {strays:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    fn owner_from(path: &Path) -> ProjectionOwner {
        ProjectionOwner::from_str(&fs::read_to_string(path).unwrap())
    }

    /// The set-up code's file, as ocbmd writes it and btd reads it: four digits or nothing.
    #[test]
    fn the_set_up_code_file_holds_four_digits_or_nothing() {
        assert_eq!(setup_code("2092\n"), Some("2092"));
        assert_eq!(setup_code(" 0007 "), Some("0007"));
        for bad in ["", "209", "20920", "20a2", "-092", "2 92"] {
            assert_eq!(setup_code(bad), None, "{bad:?}");
        }
        let path = std::env::temp_dir().join(format!("box-common-setup-code-{}", std::process::id()));
        let p = path.to_str().unwrap();
        put_setup_code(p, Some("3978")).unwrap();
        assert_eq!(setup_code(&fs::read_to_string(&path).unwrap()), Some("3978"));
        put_setup_code(p, None).unwrap();
        assert!(!path.exists());
        put_setup_code(p, None).unwrap(); // removing nothing is fine
    }
}
