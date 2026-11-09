//! hci.rs — raw HCI command packets for `bt_bringup` and the pairing window.
//!
//! One operation lives here: Write_Scan_Enable, sent as a raw command rather than through
//! `hciconfig piscan`/`noscan`, whose HCISETSCAN ioctl loses its wake-up on the CCPA's 3.14 kernel
//! (see [`write_scan_enable`]). Everything else in bring-up is still `hciconfig`.
//!
//! Not the mgmt API: mgmt owns EIR and the discoverable flag, and on 3.14 `SET_DISCOVERABLE` is a
//! no-op when the flag already holds the asked-for value — so the scan mode is written here too.

use std::io;

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_HCI: libc::c_int = 1;
/// Raw channel — lets us write HCI command packets directly. (mgmt is channel 3; see module docs.)
const HCI_CHANNEL_RAW: u16 = 0;

// HCI opcode, OGF 0x03 (Host Controller & Baseband). opcode = (ogf << 10) | ocf.
const OP_WRITE_SCAN_ENABLE: u16 = 0x0C1A;

/// Scan modes for [`write_scan_enable`]: inquiry scan = discoverable, page scan = connectable.
pub const SCAN_PAGE_AND_INQUIRY: u32 = 0x03;
/// Connectable but not discoverable: bonded phones can still page us, nobody new can find us.
pub const SCAN_PAGE: u32 = 0x02;
pub const SCAN_NONE: u32 = 0x00;

const HCI_COMMAND_PKT: u8 = 0x01;

#[repr(C)]
struct SockaddrHci {
    hci_family: libc::sa_family_t,
    hci_dev: u16,
    hci_channel: u16,
}

/// Parse `hci0` -> 0. Returns `InvalidInput` for anything that is not `hci<N>`.
pub fn dev_id(hci_dev: &str) -> io::Result<u16> {
    hci_dev
        .strip_prefix("hci")
        .and_then(|n| n.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("bad hci device {hci_dev}")))
}

/// An HCI socket. `Drop` closes it — every call site is short-lived, so nothing outlives a command.
struct HciSocket(libc::c_int);

impl Drop for HciSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

impl HciSocket {
    /// Unbound socket; [`HciSocket::open_bound`] binds it to one controller.
    fn open() -> io::Result<Self> {
        // SOCK_CLOEXEC: this must not leak into av.rs's fork+exec of the detached daemons.
        let fd = unsafe {
            libc::socket(
                AF_BLUETOOTH as libc::c_int,
                libc::SOCK_RAW | crate::cloexec::SOCK_CLOEXEC,
                BTPROTO_HCI,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(HciSocket(fd))
    }

    /// Socket bound to one controller on the RAW channel, ready to accept command packets.
    fn open_bound(dev: u16) -> io::Result<Self> {
        let s = Self::open()?;
        let addr = SockaddrHci {
            hci_family: AF_BLUETOOTH,
            hci_dev: dev,
            hci_channel: HCI_CHANNEL_RAW,
        };
        let rc = unsafe {
            libc::bind(
                s.0,
                &addr as *const SockaddrHci as *const libc::sockaddr,
                std::mem::size_of::<SockaddrHci>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(s)
    }

    /// Write one HCI command packet: `[0x01][opcode LE][plen][params]`.
    ///
    /// Fire-and-forget by design. The command-complete event comes back on the same socket, but
    /// reading it would mean filtering the shared RAW stream against everything else the kernel is
    /// doing on this controller. `hciconfig` is equally fire-and-forget for these writes, and the
    /// bring-up is verified by its observable effect (the phone sees the accessory) rather than by
    /// per-command status. A failed *write* is still reported.
    fn cmd(&self, opcode: u16, params: &[u8]) -> io::Result<()> {
        if params.len() > u8::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCI parameter block exceeds 255 bytes",
            ));
        }
        let mut pkt = Vec::with_capacity(4 + params.len());
        pkt.push(HCI_COMMAND_PKT);
        pkt.extend_from_slice(&opcode.to_le_bytes());
        pkt.push(params.len() as u8);
        pkt.extend_from_slice(params);

        let n = unsafe { libc::write(self.0, pkt.as_ptr() as *const libc::c_void, pkt.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if (n as usize) < pkt.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("short HCI write: {n} of {} bytes", pkt.len()),
            ));
        }
        Ok(())
    }
}

/// `piscan` / `noscan` as a RAW Write_Scan_Enable command — for the CCPA's 3.14 kernel ONLY.
///
/// HCISETSCAN waits in `__hci_req_sync` with the 10 s HCI_INIT_TIMEOUT, and that function starts
/// the request before it joins the wait queue, so a controller that answers inside that window has
/// its wake-up lost and the caller sleeps the full 10 s before returning success. Measured
/// 2026-10-20 on an IW416 CCPA: 3 of 9 btd bring-ups spent 10.1 s in `hciconfig hci0 piscan`
/// (wchan `__hci_req_sync` throughout) with no tx timeout in the kernel log.
///
/// A raw command never waits in the kernel. The Command Complete handler
/// (`hci_cc_write_scan_enable`) still updates HCI_PSCAN/HCI_ISCAN from the sent parameter, so
/// `hciconfig` reads back `PSCAN ISCAN` exactly as after the ioctl, and the ssp agent's mgmt
/// SET_CONNECTABLE/SET_DISCOVERABLE that follow bring-up still succeed (both checked on the box).
/// Fire-and-forget like the other writes.
pub fn write_scan_enable(dev: u16, scan: u32) -> io::Result<()> {
    HciSocket::open_bound(dev)?.cmd(OP_WRITE_SCAN_ENABLE, &[scan as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_id_parses_hci_names() {
        assert_eq!(dev_id("hci0").unwrap(), 0);
        assert_eq!(dev_id("hci1").unwrap(), 1);
        assert_eq!(dev_id("hci12").unwrap(), 12);
    }

    #[test]
    fn dev_id_rejects_garbage() {
        for bad in ["", "hci", "wlan0", "hciX", "0"] {
            assert!(dev_id(bad).is_err(), "{bad} should not parse");
        }
    }

    /// opcode = (ogf << 10) | ocf, OGF 0x03.
    #[test]
    fn scan_enable_opcode_is_ogf3() {
        assert_eq!(OP_WRITE_SCAN_ENABLE, (0x03u16 << 10) | 0x001A);
    }
}
