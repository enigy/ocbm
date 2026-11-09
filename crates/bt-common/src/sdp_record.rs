//! SDP service-record construction.
//!
//! WHY THIS EXISTS. The CarPlay record shipped as a hand-laid 89-byte array copied from a capture,
//! with a `const RFCOMM_CHAN_IDX: usize = 48` pointing into it. That is correct and proven for the
//! one record it encodes. What it does not survive is EDITING: every sequence in SDP carries its own
//! length prefix, so changing the service name changes the outer length byte, and changing anything
//! before the protocol descriptor list also moves the channel offset. (The name alone does not move
//! that offset — `ServiceName` is the last attribute — which is exactly the kind of detail a
//! hand-laid second array invites getting backwards; see the test of the same name.)
//!
//! A wrong length byte does not fail loudly. The record parses as a truncated or over-long sequence
//! and the phone simply never matches the service. The CCPA gives no interactive debugger to catch
//! that — it is OCBM or NCM, never both, so there is no second channel to watch the box on while a
//! host drives it. The code has to be right before it ships.
//!
//! So: a real encoder, with the proven CarPlay record as its oracle. `tests::carplay_record_matches`
//! asserts this builder reproduces that captured array byte-for-byte. If that test passes, the
//! encoder is trustworthy for records nobody has captured.

/// The iAP2 RFCOMM server channel, matching what the stock CCPA `bluetoothDaemon` allocates.
pub const IAP2_RFCOMM_CHANNEL: u8 = 1;

/// SDP data-element descriptors (type << 3 | size-index).
const DE_UINT8: u8 = 0x08;
const DE_UINT16: u8 = 0x09;
const DE_UINT32: u8 = 0x0a;
const DE_UUID16: u8 = 0x19;
const DE_UUID128: u8 = 0x1c;
const DE_TEXT8: u8 = 0x25; // text string, 8-bit length
const DE_SEQ8: u8 = 0x35; // sequence, 8-bit length

/// Byte offset of the RFCOMM channel in an encoded record.
///
/// Stable for any service NAME, since `ServiceName` is the last attribute — but only for the record
/// SHAPE this module builds (one UUID128 service class, one L2CAP+RFCOMM protocol descriptor, one
/// browse group, one profile descriptor). `ServiceRecord` cannot currently express any other shape,
/// which is the only reason this is safe to expose. If it gains UUID16/32 class lists or a second
/// protocol alternative, this constant must become a value returned from `encode()`.
pub const RFCOMM_CHANNEL_OFFSET: usize = 48;

/// Bytes the encoder emits regardless of the service name: everything up to and including the
/// ServiceName attribute id and its length byte.
const FIXED_BODY_LEN: usize = 73;

/// Longest service name that still fits the outer 8-bit sequence length.
pub const MAX_SERVICE_NAME: usize = u8::MAX as usize - FIXED_BODY_LEN;

/// Attribute ids we emit.
const ATTR_RECORD_HANDLE: u16 = 0x0000;
const ATTR_SERVICE_CLASS_ID_LIST: u16 = 0x0001;
const ATTR_PROTOCOL_DESCRIPTOR_LIST: u16 = 0x0004;
const ATTR_BROWSE_GROUP_LIST: u16 = 0x0005;
const ATTR_PROFILE_DESCRIPTOR_LIST: u16 = 0x0009;
const ATTR_SERVICE_NAME: u16 = 0x0100;

const UUID16_L2CAP: u16 = 0x0100;
const UUID16_RFCOMM: u16 = 0x0003;
const UUID16_PUBLIC_BROWSE_GROUP: u16 = 0x1002;
const UUID16_SERIAL_PORT: u16 = 0x1101;
fn put_uint16(out: &mut Vec<u8>, v: u16) {
    out.push(DE_UINT16);
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_uint32(out: &mut Vec<u8>, v: u32) {
    out.push(DE_UINT32);
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_uuid16(out: &mut Vec<u8>, v: u16) {
    out.push(DE_UUID16);
    out.extend_from_slice(&v.to_be_bytes());
}

/// An 8-bit-length sequence wrapping `body`. Panics above 255 bytes, which no record here reaches —
/// a longer one would need `DE_SEQ16` and is a deliberate not-yet-supported case rather than a
/// silent truncation.
fn put_seq8(out: &mut Vec<u8>, body: &[u8]) {
    assert!(body.len() <= u8::MAX as usize, "SDP sequence too long for an 8-bit length");
    out.push(DE_SEQ8);
    out.push(body.len() as u8);
    out.extend_from_slice(body);
}

/// One attribute: its id as a uint16 element, then its value.
fn put_attr(out: &mut Vec<u8>, id: u16, value: &[u8]) {
    put_uint16(out, id);
    out.extend_from_slice(value);
}

/// What a single-service, RFCOMM-based SDP record needs to say.
#[derive(Clone, Debug)]
pub struct ServiceRecord<'a> {
    pub handle: u32,
    /// The 128-bit service class UUID, big-endian as it appears on the wire.
    pub uuid128: [u8; 16],
    pub rfcomm_channel: u8,
    pub name: &'a str,
}

impl ServiceRecord<'_> {
    /// Encode the record body: a single `DE_SEQ8` sequence of attribute/value pairs.
    ///
    /// Attribute order is ascending by id, which is what the SDP spec requires of a
    /// ServiceAttributeResponse and what the captured CarPlay record does.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(96);

        let mut v = Vec::new();
        put_uint32(&mut v, self.handle);
        put_attr(&mut body, ATTR_RECORD_HANDLE, &v);

        // ServiceClassIDList: a sequence holding the one 128-bit class UUID.
        let mut class = Vec::with_capacity(20);
        class.push(DE_UUID128);
        class.extend_from_slice(&self.uuid128);
        v.clear();
        put_seq8(&mut v, &class);
        put_attr(&mut body, ATTR_SERVICE_CLASS_ID_LIST, &v);

        // ProtocolDescriptorList: seq{ seq{L2CAP}, seq{RFCOMM, channel} }
        let mut l2cap = Vec::new();
        put_uuid16(&mut l2cap, UUID16_L2CAP);
        let mut rfcomm = Vec::new();
        put_uuid16(&mut rfcomm, UUID16_RFCOMM);
        rfcomm.push(DE_UINT8);
        rfcomm.push(self.rfcomm_channel);
        let mut protos = Vec::new();
        put_seq8(&mut protos, &l2cap);
        put_seq8(&mut protos, &rfcomm);
        v.clear();
        put_seq8(&mut v, &protos);
        put_attr(&mut body, ATTR_PROTOCOL_DESCRIPTOR_LIST, &v);

        // BrowseGroupList: seq{ PublicBrowseGroup } — without this the phone's browse never
        // reaches the record, however correct the rest of it is.
        let mut browse = Vec::new();
        put_uuid16(&mut browse, UUID16_PUBLIC_BROWSE_GROUP);
        v.clear();
        put_seq8(&mut v, &browse);
        put_attr(&mut body, ATTR_BROWSE_GROUP_LIST, &v);

        // BluetoothProfileDescriptorList: seq{ seq{ SerialPort, v1.0 } }
        let mut profile = Vec::new();
        put_uuid16(&mut profile, UUID16_SERIAL_PORT);
        put_uint16(&mut profile, 0x0100); // version 1.0
        let mut profiles = Vec::new();
        put_seq8(&mut profiles, &profile);
        v.clear();
        put_seq8(&mut v, &profiles);
        put_attr(&mut body, ATTR_PROFILE_DESCRIPTOR_LIST, &v);

        // ServiceName
        let name = self.name.as_bytes();
        // The OUTER sequence is what actually bounds this, not the name element. The fixed part of
        // the body is FIXED_BODY_LEN bytes, so a name that fits the name element (<=255) can still
        // overflow the outer `put_seq8` and panic there instead of here. Guard the real bound.
        assert!(
            name.len() <= MAX_SERVICE_NAME,
            "service name is {} bytes; the outer sequence allows at most {MAX_SERVICE_NAME}",
            name.len()
        );
        v.clear();
        v.push(DE_TEXT8);
        v.push(name.len() as u8);
        v.extend_from_slice(name);
        put_attr(&mut body, ATTR_SERVICE_NAME, &v);

        let mut out = Vec::with_capacity(body.len() + 2);
        put_seq8(&mut out, &body);
        out
    }
}

/// Parse a UUID string (`00000000-deca-fade-deca-deafdecacaff`) into 16 big-endian bytes.
///
/// A `const` byte array would be shorter, but a UUID written as digits is checkable against the
/// documentation by eye and a byte array is not — and getting it wrong yields a service the phone
/// simply never matches, with no error anywhere.
pub const fn uuid128_from_u128(v: u128) -> [u8; 16] {
    v.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 89-byte iAP2 record captured from a real CCPA (`sdp_server::RECORD_TEMPLATE`), with
    /// RFCOMM channel 1. This is the oracle: the encoder is only trustworthy for records nobody has
    /// captured if it reproduces the one that IS captured, exactly.
    #[rustfmt::skip]
    const CAPTURED_IAP2: [u8; 89] = [
        0x35, 0x57,
        0x09, 0x00, 0x00,  0x0a, 0x00, 0x01, 0x00, 0x00,
        0x09, 0x00, 0x01,  0x35, 0x11, 0x1c,
            0x00, 0x00, 0x00, 0x00, 0xde, 0xca, 0xfa, 0xde,
            0xde, 0xca, 0xde, 0xaf, 0xde, 0xca, 0xca, 0xff,
        0x09, 0x00, 0x04,  0x35, 0x0c,
            0x35, 0x03, 0x19, 0x01, 0x00,
            0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x01,
        0x09, 0x00, 0x05,  0x35, 0x03, 0x19, 0x10, 0x02,
        0x09, 0x00, 0x09,  0x35, 0x08, 0x35, 0x06, 0x19, 0x11, 0x01, 0x09, 0x01, 0x00,
        0x09, 0x01, 0x00,  0x25, 0x0e,
            0x57, 0x69, 0x72, 0x65, 0x6c, 0x65, 0x73, 0x73, 0x20,
            0x69, 0x41, 0x50, 0x76, 0x32,
    ];

    fn iap2_record(chan: u8) -> ServiceRecord<'static> {
        ServiceRecord {
            handle: 0x0001_0000,
            uuid128: uuid128_from_u128(0x00000000_deca_fade_deca_deafdecacaff),
            rfcomm_channel: chan,
            name: "Wireless iAPv2",
        }
    }

    /// THE test. If this fails, nothing else in this module can be trusted.
    #[test]
    fn carplay_record_matches_the_captured_bytes() {
        assert_eq!(iap2_record(1).encode(), CAPTURED_IAP2.to_vec());
    }

    /// The old code patched byte 48 to set the channel. Prove the encoder puts it there too, so the
    /// two agree about more than just the default.
    #[test]
    fn rfcomm_channel_lands_at_the_documented_offset() {
        for chan in [1u8, 2, 7, 30] {
            let enc = iap2_record(chan).encode();
            assert_eq!(enc[RFCOMM_CHANNEL_OFFSET], chan, "channel byte for {chan}");
            let mut expect = CAPTURED_IAP2;
            expect[RFCOMM_CHANNEL_OFFSET] = chan;
            assert_eq!(enc, expect.to_vec());
        }
    }

    /// What a different service name actually changes.
    ///
    /// It does NOT move the RFCOMM channel byte: `ServiceName` is the LAST attribute, so everything
    /// before it — including the protocol descriptor list — keeps its offset. The first version of
    /// this test asserted otherwise and failed, which is the point of having it.
    ///
    /// What the name DOES change is the outer sequence length prefix, and that is the byte a
    /// hand-edited copy of the captured array gets wrong: the record still parses as a truncated or
    /// over-long sequence, and the phone declines to match a service it cannot read.
    #[test]
    fn a_different_name_changes_the_length_prefix_but_not_the_channel_offset() {
        let short = ServiceRecord { name: "iAP", ..iap2_record(3) }.encode();
        let long = ServiceRecord { name: "Wireless iAPv2 (CarLink)", ..iap2_record(3) }.encode();

        assert_ne!(short.len(), long.len(), "a longer name makes a longer record");
        assert_eq!(short[1] as usize, short.len() - 2, "outer length tracks the body");
        assert_eq!(long[1] as usize, long.len() - 2, "outer length tracks the body");

        // The channel byte holds its offset regardless of the name.
        assert_eq!(short[RFCOMM_CHANNEL_OFFSET], 3);
        assert_eq!(long[RFCOMM_CHANNEL_OFFSET], 3);
    }

    /// The name bound must reflect the OUTER sequence, not the name element. A 200-byte name fits
    /// the name element but overflows the record; it must be rejected here with a useful message
    /// rather than panicking deeper in `put_seq8`.
    #[test]
    fn service_name_bound_matches_the_outer_sequence() {
        assert_eq!(MAX_SERVICE_NAME, 182);
        let longest = "x".repeat(MAX_SERVICE_NAME);
        let enc = ServiceRecord { name: &longest, ..iap2_record(1) }.encode();
        assert_eq!(enc[1] as usize, enc.len() - 2);
        assert_eq!(enc.len(), 2 + u8::MAX as usize);
    }

    #[test]
    #[should_panic(expected = "the outer sequence allows at most")]
    fn an_over_long_service_name_is_refused_with_the_real_bound() {
        let too_long = "x".repeat(MAX_SERVICE_NAME + 1);
        let _ = ServiceRecord { name: &too_long, ..iap2_record(1) }.encode();
    }
}
