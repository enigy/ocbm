//! mfi — MFi authentication for the CarPlay head unit: `sap` (MFi-SAP) and the `MfiSigner` trait in
//! `auth_client`. Production MFi is direct local I2C to the coprocessor — see carplayd's
//! `LocalMfiSigner`, `crates/vendor/mfi-i2c-local`, `crates/vendor/wireless/src/mfi_local.rs` and
//! `ccpa/iap2d`.
#![forbid(unsafe_code)]

pub mod auth_client;
pub mod sap;

/// Length of the challenge digest the chip signs.
pub const DIGEST_LEN: usize = 20;
