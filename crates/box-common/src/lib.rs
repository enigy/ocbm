//! box-common — the box-side foundation the CarPlay daemons share: OCBM, the session flags and the
//! network constants. See Cargo.toml for the layering.

pub mod flags;
pub mod net;

/// The agnostic OCBM framing/channel layer. For now a re-export of `ocbm-proto` so every box crate
/// references ONE definition of the channel ids and frame codec; ocbmd's mux-core (OutQueue/poll
/// dispatch) migrates behind this module in a later, separately-verified step.
pub mod ocbm {
    pub use ocbm_proto::*;
}

/// Read an environment lever by its current name, falling back to the pre-2026-09-08 `CARPLAY_*`
/// name it was renamed from.
///
/// The renamed levers are the PROTOCOL-NEUTRAL ones: Bluetooth SSP/HCI/RFCOMM settings that apply
/// to any peer, and box-level settings. Genuinely Apple levers (`CARPLAY_DEVICE_ID`,
/// `CARPLAY_HEVC`, …) keep their names: there the prefix is information, not noise.
///
/// The fallback exists because a lever that silently reverts to its default is the worst failure
/// shape here. So an old name still works, and says so.
pub fn lever(name: &str, legacy: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        return Some(v);
    }
    match std::env::var(legacy) {
        Ok(v) => {
            eprintln!("[lever] {legacy} is the OLD name for {name} — honouring it, but update the launcher");
            Some(v)
        }
        Err(_) => None,
    }
}

/// A bench flag's text (docs/ops/02_TESTING.md, "Bench hooks"): the trimmed content of `path` when the flag is set,
/// None when it is not. Set means the file exists and its content is not "off": empty and "on" are set. Carlink's debug
/// builds write the flags over CH_FILE, which can write a file but not delete one, so they clear a flag by writing
/// "off". Nothing in the product writes them, and /tmp clears at a reboot.
pub fn bench_text(path: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Some(s.trim().to_string()).filter(|t| t != "off"),
        // Present but unreadable: set, as a file that exists.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Some(String::new()),
        Err(_) => None,
    }
}

/// Whether the bench flag at `path` is set ([`bench_text`]).
pub fn bench_flag(path: &str) -> bool {
    bench_text(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bench_flag_is_set_unless_absent_or_off() {
        let path = std::env::temp_dir().join(format!("box_common_bench_flag_{}", std::process::id()));
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(!bench_flag(p), "absent");
        for (text, set) in [("", true), ("on\n", true), ("  off \n", false), ("off", false), ("1", true)] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(bench_flag(p), set, "{text:?}");
        }
        std::fs::write(&path, " AA:BB:CC:DD:EE:01 1500\n").unwrap();
        assert_eq!(bench_text(p).as_deref(), Some("AA:BB:CC:DD:EE:01 1500"), "the text, trimmed");
        let _ = std::fs::remove_file(&path);
    }
}
