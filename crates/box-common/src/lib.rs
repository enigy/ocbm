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
/// The renamed levers are the PROTOCOL-NEUTRAL ones: Bluetooth SSP/HFP/HCI/RFCOMM settings that
/// apply to any peer (a Pixel bonds through the same SSP agent an iPhone does), and box-level
/// settings like the SoftAP role. Keeping a `CARPLAY_` prefix on them is what let 3,700 lines of
/// Android Auto code accumulate inside a binary called `carplay-wireless`, and it is why
/// `CARPLAY_AA_HEADSET_PATH` — a CarPlay-prefixed variable whose entire purpose is Android Auto —
/// existed. Genuinely Apple levers (`CARPLAY_DEVICE_ID`, `CARPLAY_PI`, `CARPLAY_HEVC`, …) keep
/// their names: there the prefix is information, not noise.
///
/// The fallback exists because a lever that silently reverts to its default is the worst failure
/// shape here — `CARPLAY_WIFI_AP` was exported by the supervisor and read by NOTHING for as long as
/// the bridge role existed, and nothing reported it. So an old name still works, and says so.
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
