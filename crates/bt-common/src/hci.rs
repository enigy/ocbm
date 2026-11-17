//! hci.rs — raw HCI command packets for `bt_bringup` and the pairing window.
//!
//! Write_Scan_Enable, sent as a raw command rather than through `hciconfig piscan`/`noscan`, whose HCISETSCAN
//! ioctl loses its wake-up on the CCPA's 3.14 kernel (see [`write_scan_enable`]); Write_Page_Timeout for btd's
//! paging ([`write_page_timeout`]); and bring-up's identity writes ([`write_identity`]). Only the down/up is still
//! `hciconfig`. For btd's armed state (Bluetooth powered but invisible between host sessions): the scan mode written
//! and confirmed, read back from the controller, and the connections the kernel holds ([`write_scan_enable_waited`],
//! [`read_scan_enable`], [`connections`]).
//!
//! Not the mgmt API: mgmt owns EIR and the discoverable flag, and on 3.14 `SET_DISCOVERABLE` is a
//! no-op when the flag already holds the asked-for value — so the scan mode is written here too.

use std::io;

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_HCI: libc::c_int = 1;
/// Raw channel — lets us write HCI command packets directly. (mgmt is channel 3; see module docs.)
const HCI_CHANNEL_RAW: u16 = 0;

// HCI opcode, OGF 0x03 (Host Controller & Baseband). opcode = (ogf << 10) | ocf.
const OP_READ_SCAN_ENABLE: u16 = 0x0C19;
const OP_WRITE_SCAN_ENABLE: u16 = 0x0C1A;
const OP_WRITE_PAGE_TIMEOUT: u16 = 0x0C18;
const OP_WRITE_LOCAL_NAME: u16 = 0x0C13;
const OP_WRITE_CLASS_OF_DEVICE: u16 = 0x0C24;
const OP_WRITE_EIR: u16 = 0x0C52;
const OP_WRITE_SIMPLE_PAIRING_MODE: u16 = 0x0C56;
// OGF 0x01 (Link Control).
const OP_DISCONNECT: u16 = 0x0406;
/// Disconnect reason 0x13, "remote user terminated connection".
const REASON_REMOTE_USER_TERMINATED: u8 = 0x13;

/// The controller's page timeout after a reset: 0x2000 slots of 0.625 ms, 5.12 s.
pub const PAGE_TIMEOUT_DEFAULT: u16 = 0x2000;

/// Scan modes for [`write_scan_enable`]: inquiry scan = discoverable, page scan = connectable.
pub const SCAN_PAGE_AND_INQUIRY: u32 = 0x03;
/// Connectable but not discoverable: bonded phones can still page us, nobody new can find us.
pub const SCAN_PAGE: u32 = 0x02;
pub const SCAN_NONE: u32 = 0x00;

const HCI_COMMAND_PKT: u8 = 0x01;
const HCI_EVENT_PKT: u8 = 0x04;
const EVT_CMD_COMPLETE: u8 = 0x0E;
const EVT_CMD_STATUS: u8 = 0x0F;
/// `SOL_HCI` and its `HCI_FILTER` option (bluetooth/hci.h).
const SOL_HCI: libc::c_int = 0;
const HCI_FILTER: libc::c_int = 2;
/// How long each identity write waits for its Command Complete: a few ms on the IW416, over the 3 Mbit UART.
const IDENTITY_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

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

    /// Receive the Command Complete and Command Status events on this socket ([`HciSocket::cmd_waited`]). A raw socket
    /// starts with an empty filter and receives nothing, which is why the fire-and-forget writes never read.
    fn receive_command_events(&self) -> io::Result<()> {
        // struct hci_ufilter { u32 type_mask; u32 event_mask[2]; le16 opcode; }: event packets, events 0x0E and 0x0F,
        // any opcode ([`command_status`] matches it).
        #[repr(C)]
        struct HciUfilter {
            type_mask: u32,
            event_mask: [u32; 2],
            opcode: u16,
        }
        let f = HciUfilter {
            type_mask: 1 << HCI_EVENT_PKT,
            event_mask: [(1 << EVT_CMD_COMPLETE) | (1 << EVT_CMD_STATUS), 0],
            opcode: 0,
        };
        // SAFETY: a plain struct of the kernel's layout, its size passed with it.
        let rc = unsafe {
            libc::setsockopt(
                self.0,
                SOL_HCI,
                HCI_FILTER,
                &f as *const HciUfilter as *const libc::c_void,
                std::mem::size_of::<HciUfilter>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// [`HciSocket::cmd`], then wait up to `wait` for the controller's answer to `opcode`: Ok on status 0, an error on
    /// any other status or on no answer. Needs [`HciSocket::receive_command_events`] first. Matched by opcode alone: a Complete
    /// for the same opcode the kernel had queued could end the wait early, which only skips this check for that write.
    fn cmd_waited(&self, opcode: u16, params: &[u8], wait: std::time::Duration) -> io::Result<()> {
        self.cmd_answered(opcode, params, wait).map(|_| ())
    }

    /// [`HciSocket::cmd_waited`], returning the Command Complete's return parameters after its status byte (none for a
    /// Command Status).
    fn cmd_answered(&self, opcode: u16, params: &[u8], wait: std::time::Duration) -> io::Result<Vec<u8>> {
        self.cmd(opcode, params)?;
        let until = std::time::Instant::now() + wait;
        let mut buf = [0u8; 260];
        loop {
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("no answer to HCI 0x{opcode:04x}")));
            }
            let mut pfd = libc::pollfd { fd: self.0, events: libc::POLLIN, revents: 0 };
            // SAFETY: one pollfd for this socket.
            let r = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as libc::c_int) };
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if r == 0 {
                continue;
            }
            // SAFETY: reads into the buffer, at most its length.
            let n = unsafe { libc::read(self.0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            match command_answer(&buf[..n as usize], opcode) {
                Some((0, rest)) => return Ok(rest.to_vec()),
                Some((status, _)) => {
                    return Err(io::Error::new(io::ErrorKind::Other, format!("HCI 0x{opcode:04x} failed: status 0x{status:02x}")));
                }
                None => {} // another command's answer
            }
        }
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

/// How long the controller pages before giving up on a phone (Write_Page_Timeout), in 0.625 ms slots: an absent
/// phone costs this much per page ("Host is down"). A raw, fire-and-forget command like [`write_scan_enable`]; a
/// controller reset (btd's DOWN->UP at bring-up) puts back [`PAGE_TIMEOUT_DEFAULT`].
pub fn write_page_timeout(dev: u16, slots: u16) -> io::Result<()> {
    HciSocket::open_bound(dev)?.cmd(OP_WRITE_PAGE_TIMEOUT, &slots.to_le_bytes())
}

/// HCIGETCONNINFO: `_IOR('H', 213, int)`.
const HCIGETCONNINFO: libc::c_ulong = 0x8004_48d5;
const ACL_LINK: u8 = 0x01;
/// `struct hci_conn_info_req` (bdaddr, type, then one `struct hci_conn_info` at offset 8, 4-aligned) as the kernel
/// writes it back: handle @8, bdaddr @10, type @16, out @17, state @18, link_mode @20.
const CONN_INFO_LEN: usize = 24;
/// Where the parser reads the handle and the state in that reply (tested against the structs, mirrored as `#[repr(C)]`).
const HANDLE_AT: usize = 8;
const STATE_AT: usize = 18;

/// The kernel's `BT_CONNECTED`: the page was answered and the link is set up (remote features read).
pub const BT_CONNECTED: u16 = 1;
/// The kernel's `BT_CONNECT`: Create_Connection sent, the page still running. `hcitool con` on the box lists such a
/// link during a page (state 5) as well as an answered one (state 1).
pub const BT_CONNECT: u16 = 5;

/// An ACL link to a phone, as HCIGETCONNINFO gives it: its handle and the kernel's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AclInfo {
    pub handle: u16,
    pub state: u16,
}

impl AclInfo {
    /// The page was answered: the kernel's link is BT_CONNECTED. NOT merely present: on the 3.14 kernel the link is
    /// hashed in BT_CONNECT inside connect(), before Create_Connection is even sent, so its presence says only that a
    /// page started (the Mac's review of 376c4ce, F1).
    pub fn answered(&self) -> bool {
        self.state == BT_CONNECTED
    }
}

/// The handle and state in a HCIGETCONNINFO reply buffer.
fn parse_conn_info(reply: &[u8; CONN_INFO_LEN]) -> AclInfo {
    AclInfo {
        handle: u16::from_ne_bytes([reply[HANDLE_AT], reply[HANDLE_AT + 1]]),
        state: u16::from_ne_bytes([reply[STATE_AT], reply[STATE_AT + 1]]),
    }
}

/// The ACL link to `peer` (mgmt byte order, as a `bdaddr_t`) on controller `dev`, or None when there is none. btd's
/// SDP connect watches it, so a phone that answers the page and then never opens the SDP channel is caught in well
/// under a second instead of the 4-6 s until it resets the link (the Mac's 13 Nov brief, item 3).
pub fn acl_info(dev: u16, peer: [u8; 6]) -> Option<AclInfo> {
    let s = HciSocket::open_bound(dev).ok()?;
    let mut req = [0u8; CONN_INFO_LEN];
    req[..6].copy_from_slice(&peer);
    req[6] = ACL_LINK;
    // SAFETY: the buffer holds the request and one hci_conn_info, as the kernel writes back (ENOENT: no link).
    let rc = unsafe { libc::ioctl(s.0, HCIGETCONNINFO as _, req.as_mut_ptr()) };
    (rc == 0).then(|| parse_conn_info(&req))
}

/// HCI Disconnect (OGF 0x01, OCF 0x0006) for the ACL `handle`, reason "remote user terminated": the link a cut SDP
/// connect leaves, which the kernel would otherwise hold for its 2 s disconnect timeout, so a prompt re-page would ride
/// the same stuck link. Fire-and-forget; the caller watches [`acl_info`] for the link to go.
pub fn disconnect(dev: u16, handle: u16) -> io::Result<()> {
    let h = handle.to_le_bytes();
    HciSocket::open_bound(dev)?.cmd(OP_DISCONNECT, &[h[0], h[1], REASON_REMOTE_USER_TERMINATED])
}

/// Bring-up's identity writes, in order, on one socket: Simple Pairing on, the class of device, the local name and the
/// Extended Inquiry Response. What `hciconfig sspmode 1`, `class`, `name` and `inqdata` sent, without four child
/// processes (0.15-0.2 s of every connect, the Mac's 13 Nov brief). Each waits for its Command Complete, as
/// hciconfig's did ([`IDENTITY_WAIT`]): a failed or unanswered write is an error, and the bring-up then writes them
/// by hciconfig instead (the Mac's review of 376c4ce, BT-2). The kernel takes the name and class it keeps from those
/// same events.
pub fn write_identity(dev: u16, class: u32, name: &str, eir: &[u8]) -> io::Result<()> {
    let eir = eir_params(eir)?;
    let s = HciSocket::open_bound(dev)?;
    s.receive_command_events()?;
    s.cmd_waited(OP_WRITE_SIMPLE_PAIRING_MODE, &[0x01], IDENTITY_WAIT)?;
    s.cmd_waited(OP_WRITE_CLASS_OF_DEVICE, &class_params(class), IDENTITY_WAIT)?;
    s.cmd_waited(OP_WRITE_LOCAL_NAME, &local_name_params(name), IDENTITY_WAIT)?;
    s.cmd_waited(OP_WRITE_EIR, &eir, IDENTITY_WAIT)
}

/// The status an event packet `[0x04][event][len][params]` gives `opcode`: Command Complete `[ncmd][opcode][status]...`
/// or Command Status `[status][ncmd][opcode]`; None for anything else, or another command's.
#[cfg(test)]
fn command_status(pkt: &[u8], opcode: u16) -> Option<u8> {
    command_answer(pkt, opcode).map(|(status, _)| status)
}

/// [`command_status`], with the return parameters after the status: a Command Complete's (`[status][return...]`), none
/// for a Command Status.
fn command_answer(pkt: &[u8], opcode: u16) -> Option<(u8, &[u8])> {
    let (&kind, rest) = pkt.split_first()?;
    let (&event, rest) = rest.split_first()?;
    let (&len, params) = rest.split_first()?;
    let params = params.get(..usize::from(len))?;
    if kind != HCI_EVENT_PKT {
        return None;
    }
    let (op, status, rest) = match event {
        EVT_CMD_COMPLETE => (params.get(1..3)?, *params.get(3)?, params.get(4..)?),
        EVT_CMD_STATUS => (params.get(2..4)?, *params.first()?, &params[..0]),
        _ => return None,
    };
    (u16::from_le_bytes([op[0], op[1]]) == opcode).then_some((status, rest))
}

// ---- The armed controller (btd between host sessions: powered, invisible) ---------------------------------------

/// Write_Scan_Enable as [`write_scan_enable`] does, then wait up to `wait` for its Command Complete: an error on a
/// failed status or no answer. For btd's armed bring-up, which must KNOW the controller took "no scan".
pub fn write_scan_enable_waited(dev: u16, scan: u32, wait: std::time::Duration) -> io::Result<()> {
    let s = HciSocket::open_bound(dev)?;
    s.receive_command_events()?;
    s.cmd_waited(OP_WRITE_SCAN_ENABLE, &[scan as u8], wait)
}

/// Read_Scan_Enable: the scan mode the CONTROLLER holds ([`SCAN_NONE`], 1 inquiry only, [`SCAN_PAGE`],
/// [`SCAN_PAGE_AND_INQUIRY`]), one real round trip under `wait`. Not the kernel's HCI_PSCAN/HCI_ISCAN flags, which
/// only mirror the last command it saw: this is the chip's own answer, so it both proves the controller responsive
/// and says whether anything can see it (btd's armed checks and its go).
pub fn read_scan_enable(dev: u16, wait: std::time::Duration) -> io::Result<u8> {
    let s = HciSocket::open_bound(dev)?;
    s.receive_command_events()?;
    let ret = s.cmd_answered(OP_READ_SCAN_ENABLE, &[], wait)?;
    ret.first()
        .copied()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Read_Scan_Enable answered with no scan mode"))
}

/// HCIGETCONNLIST: `_IOR('H', 212, int)`.
const HCIGETCONNLIST: libc::c_ulong = 0x8004_48d4;
/// How many connections one HCIGETCONNLIST asks for: more than the box ever holds (one phone, maybe two).
const CONN_LIST_MAX: usize = 8;
/// `struct hci_conn_list_req { u16 dev_id; u16 conn_num; struct hci_conn_info conn_info[]; }`: the list starts at 4,
/// each `hci_conn_info` 16 bytes (handle @0, bdaddr @2, type @8, out @9, state @10, link_mode @12).
const CONN_LIST_HEAD: usize = 4;
const CONN_INFO_SIZE: usize = 16;

/// One of the controller's connections: its handle and link type (1 ACL, 0 SCO, 2 eSCO).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conn {
    pub handle: u16,
    pub link_type: u8,
}

/// The connections the kernel holds on controller `dev` (HCIGETCONNLIST, what `hcitool con` lists): btd's armed
/// checks require none, since an armed controller may hold no link to anything.
pub fn connections(dev: u16) -> io::Result<Vec<Conn>> {
    let s = HciSocket::open()?;
    let mut req = [0u8; CONN_LIST_HEAD + CONN_INFO_SIZE * CONN_LIST_MAX];
    req[0..2].copy_from_slice(&dev.to_ne_bytes());
    req[2..4].copy_from_slice(&(CONN_LIST_MAX as u16).to_ne_bytes());
    // SAFETY: the buffer holds the request head and CONN_LIST_MAX hci_conn_info, as many as it asks the kernel for.
    let rc = unsafe { libc::ioctl(s.0, HCIGETCONNLIST as _, req.as_mut_ptr()) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(parse_conn_list(&req))
}

/// The connections in a HCIGETCONNLIST reply buffer: `conn_num` of them, at most as many as the buffer holds.
fn parse_conn_list(reply: &[u8]) -> Vec<Conn> {
    let Some(n) = reply.get(2..4).map(|b| u16::from_ne_bytes([b[0], b[1]]) as usize) else { return Vec::new() };
    (0..n)
        .map_while(|i| reply.get(CONN_LIST_HEAD + i * CONN_INFO_SIZE..CONN_LIST_HEAD + (i + 1) * CONN_INFO_SIZE))
        .map(|c| Conn { handle: u16::from_ne_bytes([c[0], c[1]]), link_type: c[8] })
        .collect()
}

/// Class of Device: 3 bytes, little-endian.
fn class_params(class: u32) -> [u8; 3] {
    let b = class.to_le_bytes();
    [b[0], b[1], b[2]]
}

/// Write_Local_Name: 248 bytes, the UTF-8 name NUL-padded (cut at 248, as hciconfig does).
fn local_name_params(name: &str) -> [u8; 248] {
    let mut p = [0u8; 248];
    let n = name.len().min(248);
    p[..n].copy_from_slice(&name.as_bytes()[..n]);
    p
}

/// Write_Extended_Inquiry_Response: FEC not required (0), then 240 bytes of EIR, zero-padded.
fn eir_params(eir: &[u8]) -> io::Result<[u8; 241]> {
    if eir.len() > 240 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "EIR exceeds 240 bytes"));
    }
    let mut p = [0u8; 241];
    p[1..=eir.len()].copy_from_slice(eir);
    Ok(p)
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

    #[test]
    fn page_timeout_opcode_is_ogf3() {
        assert_eq!(OP_WRITE_PAGE_TIMEOUT, (0x03u16 << 10) | 0x0018);
    }

    #[test]
    fn the_identity_writes_are_shaped_as_the_spec_says() {
        assert_eq!(OP_WRITE_LOCAL_NAME, (0x03u16 << 10) | 0x0013);
        assert_eq!(OP_WRITE_CLASS_OF_DEVICE, (0x03u16 << 10) | 0x0024);
        assert_eq!(OP_WRITE_EIR, (0x03u16 << 10) | 0x0052);
        assert_eq!(OP_WRITE_SIMPLE_PAIRING_MODE, (0x03u16 << 10) | 0x0056);
        assert_eq!(class_params(0x5a_0408), [0x08, 0x04, 0x5a]);
        let name = local_name_params("CarLink-FD6C");
        assert_eq!(&name[..12], b"CarLink-FD6C");
        assert!(name[12..].iter().all(|b| *b == 0));
        let eir = eir_params(&[3, 9, b'A', 0]).unwrap();
        assert_eq!(&eir[..5], &[0, 3, 9, b'A', 0], "FEC 0, then the EIR");
        assert!(eir_params(&[0u8; 241]).is_err());
    }

    /// The Mac's review of 376c4ce (F1): a link mid-page (BT_CONNECT, 5) is not an answer; only BT_CONNECTED is.
    #[test]
    fn a_conn_info_reply_is_answered_only_when_connected() {
        let mut reply = [0u8; CONN_INFO_LEN];
        reply[8..10].copy_from_slice(&0x000cu16.to_ne_bytes()); // handle
        reply[10..16].copy_from_slice(&[1, 2, 3, 4, 5, 6]); // bdaddr
        reply[16] = ACL_LINK;
        reply[18..20].copy_from_slice(&BT_CONNECT.to_ne_bytes());
        let paging = parse_conn_info(&reply);
        assert_eq!(paging, AclInfo { handle: 0x000c, state: BT_CONNECT });
        assert!(!paging.answered(), "a page in progress");
        reply[18..20].copy_from_slice(&BT_CONNECTED.to_ne_bytes());
        assert!(parse_conn_info(&reply).answered());
        reply[18..20].copy_from_slice(&7u16.to_ne_bytes()); // BT_CONFIG: answered, features still being read
        assert!(!parse_conn_info(&reply).answered());
    }

    /// BT-2: each identity write waits for its own Command Complete, and a bad status or a Command Status refusal fails it.
    #[test]
    fn a_command_s_answer_is_found_and_read() {
        let op = OP_WRITE_LOCAL_NAME.to_le_bytes();
        // Command Complete: [0x04][0x0E][len][ncmd][opcode][status]
        assert_eq!(command_status(&[0x04, 0x0E, 0x04, 0x01, op[0], op[1], 0x00], OP_WRITE_LOCAL_NAME), Some(0));
        assert_eq!(command_status(&[0x04, 0x0E, 0x04, 0x01, op[0], op[1], 0x12], OP_WRITE_LOCAL_NAME), Some(0x12));
        // Command Status: [0x04][0x0F][len][status][ncmd][opcode]
        assert_eq!(command_status(&[0x04, 0x0F, 0x04, 0x01, 0x01, op[0], op[1]], OP_WRITE_LOCAL_NAME), Some(0x01));
        assert_eq!(command_status(&[0x04, 0x0E, 0x04, 0x01, op[0], op[1], 0x00], OP_WRITE_EIR), None, "another command's");
        assert_eq!(command_status(&[0x04, 0x05, 0x04, 0x00, 0x0c, 0x00, 0x13], OP_WRITE_EIR), None, "another event");
        assert_eq!(command_status(&[0x04, 0x0E, 0x04, 0x01, op[0]], OP_WRITE_LOCAL_NAME), None, "short");
        assert_eq!(command_status(&[], OP_WRITE_LOCAL_NAME), None);
    }


    /// The Mac's review of bbd29f3 (A3): the parser's offsets against the kernel's structs, mirrored as `#[repr(C)]`
    /// (bdaddr_t as a packed `[u8; 6]`): `struct hci_conn_info_req { bdaddr_t; u8 type; struct hci_conn_info[]; }` and
    /// `struct hci_conn_info { u16 handle; bdaddr_t; u8 type; u8 out; u16 state; u32 link_mode; }`.
    #[test]
    fn the_conn_info_offsets_are_the_kernel_s() {
        #[allow(dead_code)]
        #[derive(Default)]
        #[repr(C)]
        struct HciConnInfo {
            handle: u16,
            bdaddr: [u8; 6],
            kind: u8,
            out: u8,
            state: u16,
            link_mode: u32,
        }
        #[allow(dead_code)]
        #[derive(Default)]
        #[repr(C)]
        struct HciConnInfoReq {
            bdaddr: [u8; 6],
            kind: u8,
            conn_info: HciConnInfo,
        }
        let r = HciConnInfoReq::default();
        let base = &r as *const HciConnInfoReq as usize;
        assert_eq!(&r.conn_info as *const HciConnInfo as usize - base, 8, "conn_info");
        assert_eq!(&r.conn_info.handle as *const u16 as usize - base, HANDLE_AT);
        assert_eq!(&r.conn_info.out as *const u8 as usize - base, 17, "out");
        assert_eq!(&r.conn_info.state as *const u16 as usize - base, STATE_AT);
        assert_eq!(std::mem::size_of::<HciConnInfoReq>(), CONN_INFO_LEN);
    }

    #[test]
    fn disconnect_is_link_control_0x0006() {
        assert_eq!(OP_DISCONNECT, (0x01u16 << 10) | 0x0006);
        assert_eq!(BT_CONNECTED, 1);
    }

    /// The armed checks read the controller's own scan mode: Read_Scan_Enable's Command Complete is
    /// `[ncmd][opcode][status][scan_enable]`, and its return parameter is what the scan mode is.
    #[test]
    fn read_scan_enable_s_answer_carries_the_scan_mode() {
        assert_eq!(OP_READ_SCAN_ENABLE, (0x03u16 << 10) | 0x0019);
        let op = OP_READ_SCAN_ENABLE.to_le_bytes();
        let answer = [0x04, 0x0E, 0x05, 0x01, op[0], op[1], 0x00, 0x02];
        assert_eq!(command_answer(&answer, OP_READ_SCAN_ENABLE), Some((0, &[0x02u8][..])), "page scan on");
        let none = [0x04, 0x0E, 0x05, 0x01, op[0], op[1], 0x00, 0x00];
        assert_eq!(command_answer(&none, OP_READ_SCAN_ENABLE), Some((0, &[0x00u8][..])));
        let refused = [0x04, 0x0F, 0x04, 0x01, 0x01, op[0], op[1]];
        assert_eq!(command_answer(&refused, OP_READ_SCAN_ENABLE), Some((0x01, &[][..])), "a Command Status has no scan mode");
        assert_eq!(command_answer(&none, OP_WRITE_SCAN_ENABLE), None, "another command's");
        assert_eq!(command_answer(&[0x04, 0x0E, 0x09, 0x01, op[0], op[1], 0x00], OP_READ_SCAN_ENABLE), None, "short");
    }

    /// The list HCIGETCONNLIST writes back: `conn_num` entries of 16 bytes after a 4-byte head, against the kernel's
    /// structs mirrored as `#[repr(C)]` (as the HCIGETCONNINFO test does).
    #[test]
    fn the_conn_list_is_read_as_the_kernel_lays_it_out() {
        #[allow(dead_code)]
        #[derive(Default)]
        #[repr(C)]
        struct HciConnInfo {
            handle: u16,
            bdaddr: [u8; 6],
            kind: u8,
            out: u8,
            state: u16,
            link_mode: u32,
        }
        #[allow(dead_code)]
        #[derive(Default)]
        #[repr(C)]
        struct HciConnListReq {
            dev_id: u16,
            conn_num: u16,
            conn_info: [HciConnInfo; 2],
        }
        let r = HciConnListReq::default();
        let base = &r as *const HciConnListReq as usize;
        assert_eq!(&r.conn_info as *const _ as usize - base, CONN_LIST_HEAD);
        assert_eq!(std::mem::size_of::<HciConnInfo>(), CONN_INFO_SIZE);
        assert_eq!(&r.conn_info[0].kind as *const u8 as usize - base, CONN_LIST_HEAD + 8, "the link type");
        assert_eq!(HCIGETCONNLIST, HCIGETCONNINFO - 1, "_IOR('H', 212, int), one before HCIGETCONNINFO's 213");

        let mut reply = [0u8; CONN_LIST_HEAD + CONN_INFO_SIZE * CONN_LIST_MAX];
        assert!(parse_conn_list(&reply).is_empty(), "conn_num 0: no connection");
        reply[2..4].copy_from_slice(&2u16.to_ne_bytes());
        reply[4..6].copy_from_slice(&0x000cu16.to_ne_bytes());
        reply[4 + 8] = ACL_LINK;
        reply[20..22].copy_from_slice(&0x0101u16.to_ne_bytes());
        reply[20 + 8] = 0;
        assert_eq!(
            parse_conn_list(&reply),
            vec![Conn { handle: 0x000c, link_type: ACL_LINK }, Conn { handle: 0x0101, link_type: 0 }]
        );
        reply[2..4].copy_from_slice(&200u16.to_ne_bytes());
        assert_eq!(parse_conn_list(&reply).len(), CONN_LIST_MAX, "never past the buffer");
        assert!(parse_conn_list(&[0u8; 3]).is_empty());
    }
}
