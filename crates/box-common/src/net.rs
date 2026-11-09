//! Box-side network constants that more than one crate has to agree on.
//!
//! One definition, deliberately: the SoftAP gateway address is where the box's AirPlay receiver
//! binds, and the shell AP bring-up serves it.

/// The gateway address the box's own SoftAP serves on the WLAN interface.
///
/// Field-confirmed: `ccpa/rootfs/script/start_bluetooth_wifi.sh` (`WLANIP`) and the AirPlay receiver
/// bind in `crates/vendor/wireless/src/av.rs`. NOT `192.168.4.1`, which an earlier cut used.
///
/// This is an ADDRESS, not an interface name — the interface name is an insmod parameter on this
/// hardware and must never be hardcoded (see the radio rules in CLAUDE.md). The shell AP layer owns
/// the address, so if that ever changes, change it here and the Rust side follows in one place.
pub const AP_IP: &str = "192.168.43.1";
