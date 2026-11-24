//! OCBW v1: the OCBM byte stream over TCP (the Wi-Fi link), behind a pre-shared-key handshake and inside
//! ChaCha20-Poly1305 records.
//!
//! WHY. Over USB the OCBM stream rides a cable that only the head unit holds. Over Wi-Fi it rides the phone's
//! hotspot, which passengers share, and the stream carries a root console (`CH_CONSOLE`), file push and
//! CarPlay's per-session A/V keys. So no byte gets through without the link key, and nobody on the hotspot reads
//! one.
//!
//! THE LINK KEY is 32 random bytes the host makes and hands the box once, in `MGMT_WIFI_LINK` over an OCBM
//! session it already has. It never crosses the network itself: each side only proves that it holds it.
//!
//! HANDSHAKE (all fields fixed length; `K` = the link key, `|` = concatenation):
//!
//! ```text
//!   box  -> host   "OCBW" ver=1 box_nonce[32]                                   (37 B)
//!   host -> box    "OCBW" ver=1 host_nonce[32] HMAC(K, "OCBW host" | bn | hn)   (69 B)
//!   box  -> host   HMAC(K, "OCBW box" | hn | bn)                                 (32 B)
//! ```
//!
//! HMAC is HMAC-SHA256. The host proves first, so a stranger who connects learns only a random nonce; the box
//! answers only a host that proved, which proves the box to the host as well (a fake box cannot answer).
//! Session keys: HKDF-SHA256(ikm = K, salt = bn | hn), info `"OCBW v1 host->box"` and `"OCBW v1 box->host"`,
//! 32 bytes each.
//!
//! RECORDS: `[u32 BE ciphertext length][ciphertext | 16 B tag]`, the length counting the tag. The nonce is 4
//! zero bytes and a per-direction 64-bit counter, little-endian, from 0, so a record that is dropped, replayed
//! or reordered fails its tag and ends the link. A record's plaintext is any chunk of the OCBM stream, at most
//! [`MAX_RECORD`] bytes, and record boundaries mean nothing to OCBM.
//!
//! KEEPALIVE AND THE DEAD LINK (not in the outside pull request; added for the 12 Nov review's must-fix). A record
//! with an EMPTY plaintext (a length of 16: the tag alone) carries no OCBM byte and is the link's keepalive. Each
//! side sends one whenever it has sent nothing for [`KEEPALIVE_AFTER`], and each side ends the link when it has
//! received nothing at all, keepalive or data, for [`DEAD_AFTER`]. OCBM's own heartbeat (`CT_HEARTBEAT`, 1/s)
//! flows only from a subscribed host to the box, so it cannot tell the host that the box has gone, and a bench
//! console sends nothing while its user types nothing: the keepalive covers both, below OCBM, and changes nothing
//! in the stream.
//!
//! Carlink's Kotlin side implements the same bytes; the known-answer vectors in the tests below pin them.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::{Read, Write};
use std::time::Duration;

type HmacSha256 = Hmac<Sha256>;

pub const MAGIC: [u8; 4] = *b"OCBW";
pub const VERSION: u8 = 1;
/// The TCP port the box listens on, on its station interface only.
pub const PORT: u16 = 7020;
/// The largest record plaintext. OCBM frames are at most 16 B + 64 KiB; a writer cuts larger chunks.
pub const MAX_RECORD: usize = 64 * 1024;
/// The Poly1305 tag that ends every record.
pub const TAG: usize = 16;
const NONCE_LEN: usize = 32;
/// The box's opening: magic, version, its nonce.
pub const HELLO_LEN: usize = 4 + 1 + NONCE_LEN;
/// The host's reply: magic, version, its nonce, its proof.
pub const REPLY_LEN: usize = HELLO_LEN + 32;
/// The box's proof.
pub const PROOF_LEN: usize = 32;
/// A side that has sent nothing for this long sends an empty record.
pub const KEEPALIVE_AFTER: Duration = Duration::from_secs(1);
/// A side that has received nothing, not even a keepalive, for this long ends the link.
pub const DEAD_AFTER: Duration = Duration::from_secs(3);
/// The box gives a connection this long to finish the handshake (both directions' reads and writes).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// The 32-byte link key. Its `Debug` says nothing, and it is wiped when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkKey(pub [u8; 32]);

impl LinkKey {
    /// From exactly 64 hex digits (either case; whitespace around them is ignored).
    pub fn from_hex(s: &str) -> Option<LinkKey> {
        let s = s.trim();
        if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(LinkKey(k))
    }

    /// 64 lower-case hex digits.
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
}

impl std::fmt::Debug for LinkKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkKey(<secret>)")
    }
}

impl Drop for LinkKey {
    fn drop(&mut self) {
        for b in self.0.iter_mut() {
            // SAFETY: a plain write through a valid &mut u8; volatile so the wipe is not optimised away.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

/// Lower-case hex, two digits a byte.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Debug)]
pub enum LinkError {
    Io(std::io::Error),
    /// The peer did not open with OCBW v1.
    BadHello,
    /// The peer's proof did not match: it does not hold this link key.
    BadProof,
    /// A record failed its tag (tampered, replayed, reordered, or another key).
    BadRecord,
    /// A record length outside 16 ..= [`MAX_RECORD`] + 16.
    Oversize(usize),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Io(e) => write!(f, "i/o: {e}"),
            LinkError::BadHello => f.write_str("the peer did not open with OCBW v1"),
            LinkError::BadProof => f.write_str("the peer's proof failed (another link key)"),
            LinkError::BadRecord => f.write_str("a record failed authentication"),
            LinkError::Oversize(n) => write!(f, "a record of {n} B is outside the limits"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<std::io::Error> for LinkError {
    fn from(e: std::io::Error) -> Self {
        LinkError::Io(e)
    }
}

/// 32 bytes from the kernel's CSPRNG. `/dev/urandom`, not `getrandom(2)`: the adapter's 3.14 kernel predates the
/// call, and the file reads the same on the adapter, Linux and macOS.
pub fn random32() -> std::io::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

fn mac(key: &LinkKey, label: &[u8], a: &[u8], b: &[u8]) -> HmacSha256 {
    let mut m = <HmacSha256 as Mac>::new_from_slice(&key.0).expect("HMAC takes a key of any length");
    m.update(label);
    m.update(a);
    m.update(b);
    m
}

const HOST_LABEL: &[u8] = b"OCBW host";
const BOX_LABEL: &[u8] = b"OCBW box";
const ID_LABEL: &[u8] = b"OCBW id";
const H2B: &[u8] = b"OCBW v1 host->box";
const B2H: &[u8] = b"OCBW v1 box->host";

/// The host's proof: HMAC(K, "OCBW host" | bn | hn).
pub fn host_proof(key: &LinkKey, bn: &[u8; 32], hn: &[u8; 32]) -> [u8; 32] {
    mac(key, HOST_LABEL, bn, hn).finalize().into_bytes().into()
}

/// The box's proof: HMAC(K, "OCBW box" | hn | bn).
pub fn box_proof(key: &LinkKey, hn: &[u8; 32], bn: &[u8; 32]) -> [u8; 32] {
    mac(key, BOX_LABEL, hn, bn).finalize().into_bytes().into()
}

/// The box's public id for discovery: the first 8 bytes of HMAC(K, "OCBW id"), as 16 hex digits. The mDNS advert
/// carries it (TXT `id=`), so a host picks ITS box among several on one hotspot, and the advert says nothing about
/// the key.
pub fn box_id(key: &LinkKey) -> String {
    let full: [u8; 32] = mac(key, ID_LABEL, &[], &[]).finalize().into_bytes().into();
    hex(&full[..8])
}

/// The two directions' record keys, (host->box, box->host), from the link key and both nonces.
pub fn session_keys(key: &LinkKey, bn: &[u8; 32], hn: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(bn);
    salt[32..].copy_from_slice(hn);
    let hk = Hkdf::<Sha256>::new(Some(&salt), &key.0);
    let (mut h2b, mut b2h) = ([0u8; 32], [0u8; 32]);
    hk.expand(H2B, &mut h2b).expect("32 B is a valid HKDF length");
    hk.expand(B2H, &mut b2h).expect("32 B is a valid HKDF length");
    (h2b, b2h)
}

fn nonce(ctr: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&ctr.to_le_bytes());
    n
}

/// Seals one direction's records.
pub struct Sealer {
    aead: ChaCha20Poly1305,
    ctr: u64,
}

impl Sealer {
    /// A sealer for one direction's key, its counter at 0.
    pub fn new(k: &[u8; 32]) -> Self {
        Sealer { aead: ChaCha20Poly1305::new(Key::from_slice(k)), ctr: 0 }
    }

    /// Append one record for `plain` (at most [`MAX_RECORD`] bytes) to `out`: one copy of the plaintext, sealed
    /// where it lands.
    pub fn seal_into(&mut self, out: &mut Vec<u8>, plain: &[u8]) {
        assert!(plain.len() <= MAX_RECORD, "record plaintext over MAX_RECORD");
        let n = nonce(self.ctr);
        // Never reached (2^64 records), and a counter that wrapped would reuse a nonce: stop instead.
        self.ctr = self.ctr.checked_add(1).expect("OCBW record counter exhausted");
        out.extend_from_slice(&((plain.len() + TAG) as u32).to_be_bytes());
        let at = out.len();
        out.extend_from_slice(plain);
        let tag: Tag = self
            .aead
            .encrypt_in_place_detached(Nonce::from_slice(&n), &[], &mut out[at..])
            .expect("ChaCha20-Poly1305 cannot fail for in-range lengths");
        out.extend_from_slice(&tag);
    }

    /// One record for `plain`, ready to write.
    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + plain.len() + TAG);
        self.seal_into(&mut out, plain);
        out
    }

    /// The keepalive: a record with no plaintext (20 bytes on the wire).
    pub fn keepalive(&mut self) -> Vec<u8> {
        self.seal(&[])
    }
}

/// Opens the other direction's records, from a byte stream that can split them anywhere.
pub struct Opener {
    aead: ChaCha20Poly1305,
    ctr: u64,
    buf: Vec<u8>,
    /// Bytes of `buf` already consumed (opened records), dropped when it is worth a move.
    start: usize,
}

impl Opener {
    /// An opener for one direction's key, its counter at 0.
    pub fn new(k: &[u8; 32]) -> Self {
        Opener { aead: ChaCha20Poly1305::new(Key::from_slice(k)), ctr: 0, buf: Vec::new(), start: 0 }
    }

    /// Bytes as they came off the socket.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 && (self.start == self.buf.len() || self.start >= 64 * 1024) {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes pushed and not yet opened.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }

    /// The next whole record's plaintext (empty for a keepalive), `Ok(None)` until one has arrived. An error ends
    /// the link: nothing after it may be trusted.
    pub fn next(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        let b = &self.buf[self.start..];
        if b.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        if !(TAG..=MAX_RECORD + TAG).contains(&len) {
            return Err(LinkError::Oversize(len));
        }
        if b.len() < 4 + len {
            return Ok(None);
        }
        let mut plain = b[4..4 + len - TAG].to_vec();
        let tag = *Tag::from_slice(&b[4 + len - TAG..4 + len]);
        let n = nonce(self.ctr);
        self.aead
            .decrypt_in_place_detached(Nonce::from_slice(&n), &[], &mut plain, &tag)
            .map_err(|_| LinkError::BadRecord)?;
        self.ctr += 1;
        self.start += 4 + len;
        Ok(Some(plain))
    }
}

/// What a finished handshake gives either side.
pub struct Session {
    /// Seals what this side sends.
    pub tx: Sealer,
    /// Opens what this side receives.
    pub rx: Opener,
}

/// The box's side, on a freshly accepted connection. Blocking: give the stream read and write timeouts first
/// ([`HANDSHAKE_TIMEOUT`]), so a silent stranger cannot hold it.
pub fn box_handshake<S: Read + Write>(s: &mut S, key: &LinkKey, box_nonce: [u8; 32]) -> Result<Session, LinkError> {
    let mut hello = [0u8; HELLO_LEN];
    hello[..4].copy_from_slice(&MAGIC);
    hello[4] = VERSION;
    hello[5..].copy_from_slice(&box_nonce);
    s.write_all(&hello)?;
    s.flush()?;
    let mut reply = [0u8; REPLY_LEN];
    s.read_exact(&mut reply)?;
    if reply[..4] != MAGIC || reply[4] != VERSION {
        return Err(LinkError::BadHello);
    }
    let mut hn = [0u8; NONCE_LEN];
    hn.copy_from_slice(&reply[5..HELLO_LEN]);
    // Constant time.
    if mac(key, HOST_LABEL, &box_nonce, &hn).verify_slice(&reply[HELLO_LEN..]).is_err() {
        return Err(LinkError::BadProof);
    }
    s.write_all(&box_proof(key, &hn, &box_nonce))?;
    s.flush()?;
    let (h2b, b2h) = session_keys(key, &box_nonce, &hn);
    Ok(Session { tx: Sealer::new(&b2h), rx: Opener::new(&h2b) })
}

/// The host's side, on a fresh connection to the box.
pub fn host_handshake<S: Read + Write>(s: &mut S, key: &LinkKey, host_nonce: [u8; 32]) -> Result<Session, LinkError> {
    let mut hello = [0u8; HELLO_LEN];
    s.read_exact(&mut hello)?;
    if hello[..4] != MAGIC || hello[4] != VERSION {
        return Err(LinkError::BadHello);
    }
    let mut bn = [0u8; NONCE_LEN];
    bn.copy_from_slice(&hello[5..]);
    let mut reply = [0u8; REPLY_LEN];
    reply[..4].copy_from_slice(&MAGIC);
    reply[4] = VERSION;
    reply[5..HELLO_LEN].copy_from_slice(&host_nonce);
    reply[HELLO_LEN..].copy_from_slice(&host_proof(key, &bn, &host_nonce));
    s.write_all(&reply)?;
    s.flush()?;
    let mut bp = [0u8; PROOF_LEN];
    s.read_exact(&mut bp)?;
    if mac(key, BOX_LABEL, &host_nonce, &bn).verify_slice(&bp).is_err() {
        return Err(LinkError::BadProof);
    }
    let (h2b, b2h) = session_keys(key, &bn, &host_nonce);
    Ok(Session { tx: Sealer::new(&h2b), rx: Opener::new(&b2h) })
}

// ---- OCBS v1: the set-up exchange (M3) ------------------------------------------------------------------------------
//
// An adapter with no link key offers a time-limited set-up network from its own AP (SSID [`SETUP_SSID_PREFIX`] + its
// four hex digits, WPA2 passphrase [`SETUP_PASSPHRASE`], public on purpose). A host on it agrees keys with the box by an
// unauthenticated P-256 ECDH, so nothing it then sends (the link key, the hotspot's WPA key) is readable from the air;
// trust is first-come within the window (the adapter has no screen for a code). After the 70-byte exchange the session
// is OCBW v1's record layer, the plain OCBM stream inside:
//
// ```text
//   host -> box   "OCBS" ver=1 host_pub[65]     (P-256, SEC1 uncompressed: 0x04 | X | Y)   (70 B)
//   box  -> host  "OCBS" ver=1 box_pub[65]                                                   (70 B)
//   Z = the ECDH shared point's X (32 B, big-endian)
//   keys: HKDF-SHA256(ikm = Z, salt = host_pub | box_pub), info "OCBS v1 host->box" / "OCBS v1 box->host", 32 B each
// ```
//
// The two public points are the transcript the set-up code comes from ([`setup_code`]): four digits Carlink shows and
// the box shows as its Bluetooth name, so the driver sees that Carlink is talking to THEIR adapter. It is a check against
// mistakes, not proof against an attacker in radio range (a Bluetooth name is not an authenticated channel): first-come
// trust within the window stays the security model. No wire change: both sides hold the transcript when it ends.

/// The set-up exchange's magic.
pub const SETUP_MAGIC: [u8; 4] = *b"OCBS";
/// The box listens for the set-up exchange on its AP's address, this port, while the set-up network is up.
pub const SETUP_PORT: u16 = 7021;
/// The set-up network's name: this and the box's four hex digits (those of its CarLink-xxxx name).
pub const SETUP_SSID_PREFIX: &str = "CarLink-Setup-";
/// The set-up network's WPA2 passphrase: fixed and documented; secrecy comes from the ECDH, not from this.
pub const SETUP_PASSPHRASE: &str = "carlinksetup";
/// Each side's opening: magic, version, an uncompressed P-256 point.
pub const SETUP_HELLO_LEN: usize = 4 + 1 + 65;
const SETUP_H2B: &[u8] = b"OCBS v1 host->box";
const SETUP_B2H: &[u8] = b"OCBS v1 box->host";
const SETUP_CODE_LABEL: &[u8] = b"OCBS v1 code";

/// The two public points of a finished exchange, the set-up code's input ([`setup_code`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub host_pub: [u8; 65],
    pub box_pub: [u8; 65],
}

/// A P-256 key pair from 32 bytes of the CSPRNG: None when they are no valid scalar (zero, or not below the order),
/// which [`setup_keypair`] answers by drawing again.
pub fn setup_keypair_from(bytes: &[u8; 32]) -> Option<(p256::SecretKey, [u8; 65])> {
    let sk = p256::SecretKey::from_slice(bytes).ok()?;
    let mut pk = [0u8; 65];
    pk.copy_from_slice(sk.public_key().to_encoded_point(false).as_bytes());
    Some((sk, pk))
}

/// A fresh key pair for one exchange.
pub fn setup_keypair() -> std::io::Result<(p256::SecretKey, [u8; 65])> {
    loop {
        if let Some(kp) = setup_keypair_from(&random32()?) {
            return Ok(kp);
        }
    }
}

/// Z from our secret and the peer's point; None for a point that is not on the curve (or not uncompressed).
pub fn setup_shared(secret: &p256::SecretKey, peer_pub: &[u8; 65]) -> Option<[u8; 32]> {
    if peer_pub[0] != 0x04 {
        return None;
    }
    let peer = p256::PublicKey::from_sec1_bytes(peer_pub).ok()?;
    let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), peer.as_affine());
    let mut z = [0u8; 32];
    z.copy_from_slice(shared.raw_secret_bytes());
    Some(z)
}

/// The two directions' record keys, (host->box, box->host), from Z and the transcript.
pub fn setup_session_keys(z: &[u8; 32], t: &Transcript) -> ([u8; 32], [u8; 32]) {
    let mut salt = [0u8; 130];
    salt[..65].copy_from_slice(&t.host_pub);
    salt[65..].copy_from_slice(&t.box_pub);
    let hk = Hkdf::<Sha256>::new(Some(&salt), z);
    let (mut h2b, mut b2h) = ([0u8; 32], [0u8; 32]);
    hk.expand(SETUP_H2B, &mut h2b).expect("32 B is a valid HKDF length");
    hk.expand(SETUP_B2H, &mut b2h).expect("32 B is a valid HKDF length");
    (h2b, b2h)
}

/// The set-up code: 4 decimal digits, zero-padded, of the first 4 bytes of SHA-256("OCBS v1 code" | host_pub | box_pub)
/// read as a big-endian u32, modulo 10000. The host's point first, as in the key derivation's salt.
pub fn setup_code(t: &Transcript) -> String {
    code_digits(&code_digest(t))
}

/// SHA-256 of the 142 bytes `"OCBS v1 code" | host_pub | box_pub`, padded here to its three blocks and run through the
/// crate's own compression function, which HMAC links into ocbmd already: `Sha256::digest` adds about 0.9 KB to the
/// adapter's binary, where the update package has a few KB left (notes/ocbm.md). The tests hold it to `Sha256::digest`
/// and to the pinned vector.
fn code_digest(t: &Transcript) -> [u8; 32] {
    use sha2::digest::generic_array::GenericArray;
    const LEN: usize = 12 + 65 + 65;
    let mut m = [0u8; 192];
    m[..12].copy_from_slice(SETUP_CODE_LABEL);
    m[12..77].copy_from_slice(&t.host_pub);
    m[77..LEN].copy_from_slice(&t.box_pub);
    m[LEN] = 0x80;
    m[184..].copy_from_slice(&(LEN as u64 * 8).to_be_bytes());
    // FIPS 180-4's initial hash value for SHA-256.
    let mut state = [0x6a09e667u32, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let blocks = [*GenericArray::from_slice(&m[..64]), *GenericArray::from_slice(&m[64..128]), *GenericArray::from_slice(&m[128..])];
    sha2::compress256(&mut state, &blocks);
    let mut out = [0u8; 32];
    for (o, w) in out.chunks_exact_mut(4).zip(state) {
        o.copy_from_slice(&w.to_be_bytes());
    }
    out
}

fn code_digits(digest: &[u8; 32]) -> String {
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    format!("{:04}", n % 10_000)
}

fn setup_hello(pk: &[u8; 65]) -> [u8; SETUP_HELLO_LEN] {
    let mut h = [0u8; SETUP_HELLO_LEN];
    h[..4].copy_from_slice(&SETUP_MAGIC);
    h[4] = VERSION;
    h[5..].copy_from_slice(pk);
    h
}

fn setup_peer(h: &[u8; SETUP_HELLO_LEN]) -> Result<[u8; 65], LinkError> {
    if h[..4] != SETUP_MAGIC || h[4] != VERSION {
        return Err(LinkError::BadHello);
    }
    let mut p = [0u8; 65];
    p.copy_from_slice(&h[5..]);
    Ok(p)
}

/// The box's side: read the host's 70 bytes, answer with its own, derive the keys. Blocking: give the stream the
/// handshake limit first. A point off the curve ends it ([`LinkError::BadProof`]).
pub fn setup_box_handshake<S: Read + Write>(
    s: &mut S,
    secret: &p256::SecretKey,
    box_pub: &[u8; 65],
) -> Result<(Session, Transcript), LinkError> {
    let mut h = [0u8; SETUP_HELLO_LEN];
    s.read_exact(&mut h)?;
    let host_pub = setup_peer(&h)?;
    let z = setup_shared(secret, &host_pub).ok_or(LinkError::BadProof)?;
    s.write_all(&setup_hello(box_pub))?;
    s.flush()?;
    let t = Transcript { host_pub, box_pub: *box_pub };
    let (h2b, b2h) = setup_session_keys(&z, &t);
    Ok((Session { tx: Sealer::new(&b2h), rx: Opener::new(&h2b) }, t))
}

/// The host's side (the tests' and a bench's): send its point first, read the box's.
pub fn setup_host_handshake<S: Read + Write>(
    s: &mut S,
    secret: &p256::SecretKey,
    host_pub: &[u8; 65],
) -> Result<(Session, Transcript), LinkError> {
    s.write_all(&setup_hello(host_pub))?;
    s.flush()?;
    let mut h = [0u8; SETUP_HELLO_LEN];
    s.read_exact(&mut h)?;
    let box_pub = setup_peer(&h)?;
    let z = setup_shared(secret, &box_pub).ok_or(LinkError::BadProof)?;
    let t = Transcript { host_pub: *host_pub, box_pub };
    let (h2b, b2h) = setup_session_keys(&z, &t);
    Ok((Session { tx: Sealer::new(&h2b), rx: Opener::new(&b2h) }, t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    const K: [u8; 32] = [7u8; 32];

    fn pair(box_key: [u8; 32], host_key: [u8; 32]) -> (Result<Session, LinkError>, Result<Session, LinkError>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            box_handshake(&mut s, &LinkKey(box_key), [1u8; 32])
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let h = host_handshake(&mut c, &LinkKey(host_key), [2u8; 32]);
        (t.join().unwrap(), h)
    }

    #[test]
    fn handshake_and_records_both_ways() {
        let (b, h) = pair(K, K);
        let (mut b, mut h) = (b.unwrap(), h.unwrap());
        let rec = h.tx.seal(b"hello box");
        b.rx.push(&rec[..3]); // split anywhere
        assert!(b.rx.next().unwrap().is_none());
        b.rx.push(&rec[3..]);
        assert_eq!(b.rx.next().unwrap().unwrap(), b"hello box");
        let mut both = b.tx.seal(b"hello host");
        b.tx.seal_into(&mut both, b"again");
        h.rx.push(&both);
        assert_eq!(h.rx.next().unwrap().unwrap(), b"hello host");
        assert_eq!(h.rx.next().unwrap().unwrap(), b"again");
        assert!(h.rx.next().unwrap().is_none());
        assert_eq!(h.rx.pending(), 0);
    }

    #[test]
    fn a_keepalive_is_an_empty_record_that_keeps_the_counters_in_step() {
        let (b, h) = pair(K, K);
        let (mut b, mut h) = (b.unwrap(), h.unwrap());
        let ka = h.tx.keepalive();
        assert_eq!(ka.len(), 4 + TAG);
        assert_eq!(&ka[..4], &(TAG as u32).to_be_bytes());
        b.rx.push(&ka);
        assert_eq!(b.rx.next().unwrap().unwrap(), Vec::<u8>::new());
        b.rx.push(&h.tx.seal(b"after"));
        assert_eq!(b.rx.next().unwrap().unwrap(), b"after");
    }

    #[test]
    fn wrong_key_is_refused_by_the_box_and_a_fake_box_by_the_host() {
        let (b, _h) = pair(K, [8u8; 32]);
        assert!(matches!(b, Err(LinkError::BadProof)));
        // The host's side: a box that does not hold the key cannot answer the host's proof.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut hello = [0u8; HELLO_LEN];
            hello[..4].copy_from_slice(&MAGIC);
            hello[4] = VERSION;
            s.write_all(&hello).unwrap();
            let mut reply = [0u8; REPLY_LEN];
            s.read_exact(&mut reply).unwrap();
            s.write_all(&[0u8; PROOF_LEN]).unwrap(); // a guess
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(host_handshake(&mut c, &LinkKey(K), [2u8; 32]), Err(LinkError::BadProof)));
        t.join().unwrap();
    }

    #[test]
    fn a_peer_that_does_not_speak_ocbw_is_refused() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            box_handshake(&mut s, &LinkKey(K), [1u8; 32])
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let mut hello = [0u8; HELLO_LEN];
        c.read_exact(&mut hello).unwrap();
        c.write_all(&[b'G'; REPLY_LEN]).unwrap();
        assert!(matches!(t.join().unwrap(), Err(LinkError::BadHello)));
    }

    #[test]
    fn tamper_replay_and_reorder_fail() {
        // reorder: the second record first
        let (b, h) = pair(K, K);
        let (mut b, mut h) = (b.unwrap(), h.unwrap());
        let _r0 = h.tx.seal(b"zero");
        let r1 = h.tx.seal(b"one");
        b.rx.push(&r1);
        assert!(matches!(b.rx.next(), Err(LinkError::BadRecord)));
        // tamper
        let (b, h) = pair(K, K);
        let (mut b, mut h) = (b.unwrap(), h.unwrap());
        let mut t = h.tx.seal(b"data");
        let last = t.len() - 1;
        t[last] ^= 1;
        b.rx.push(&t);
        assert!(matches!(b.rx.next(), Err(LinkError::BadRecord)));
        // replay
        let (b, h) = pair(K, K);
        let (mut b, mut h) = (b.unwrap(), h.unwrap());
        let a = h.tx.seal(b"a");
        b.rx.push(&a);
        assert!(b.rx.next().unwrap().is_some());
        b.rx.push(&a);
        assert!(matches!(b.rx.next(), Err(LinkError::BadRecord)));
    }

    #[test]
    fn lengths_outside_the_limits_are_refused_before_buffering() {
        let (b, _h) = pair(K, K);
        let mut b = b.unwrap();
        b.rx.push(&((MAX_RECORD + TAG + 1) as u32).to_be_bytes());
        assert!(matches!(b.rx.next(), Err(LinkError::Oversize(_))));
        let (b, _h) = pair(K, K);
        let mut b = b.unwrap();
        b.rx.push(&((TAG - 1) as u32).to_be_bytes());
        assert!(matches!(b.rx.next(), Err(LinkError::Oversize(15))));
    }

    #[test]
    fn a_full_size_record_round_trips_and_the_buffer_is_reclaimed() {
        let (b, h) = pair(K, K);
        let (mut b, mut h) = (b.unwrap(), h.unwrap());
        let big: Vec<u8> = (0..MAX_RECORD).map(|i| i as u8).collect();
        for _ in 0..3 {
            b.rx.push(&h.tx.seal(&big));
            assert_eq!(b.rx.next().unwrap().unwrap(), big);
        }
        assert_eq!(b.rx.pending(), 0);
    }

    /// Pins the exact bytes, so Carlink's Kotlin implementation can be checked against them. The first four strings
    /// are the outside pull request's, unchanged; the rest (the box->host key, the box->host record, the keepalive)
    /// are added with the keepalive.
    #[test]
    fn known_answer_vectors() {
        let key = LinkKey([0x11; 32]);
        let bn = [0x22u8; 32];
        let hn = [0x33u8; 32];
        let (h2b, b2h) = session_keys(&key, &bn, &hn);
        let mut s = Sealer::new(&h2b);
        let rec = s.seal(b"OCBM");
        assert_eq!(hex(&host_proof(&key, &bn, &hn)), "37256608df8b1738ce01a824112515ab0f72420ad890eb5f28b8e1378ef3c276");
        assert_eq!(hex(&box_proof(&key, &hn, &bn)), "6b0464993905c9cb244c394e2114ed5db546a3e01a882fa33e18e9b4a88b5ea0");
        assert_eq!(hex(&h2b), "cdbc30ed5bb1352687e3175ad53c08496750fdf4cbce95eeee76e9f66c4fa29a");
        assert_eq!(hex(&rec), "000000140cfc70b3b120a8adfad9270f9691f573c1d1ad5d");
        // Added: the next record in the same direction is a keepalive (counter 1), and the box's first record.
        assert_eq!(hex(&s.keepalive()), KAT_H2B_KEEPALIVE_CTR1);
        assert_eq!(hex(&b2h), KAT_B2H);
        assert_eq!(hex(&Sealer::new(&b2h).seal(b"OCBM")), KAT_B2H_RECORD);
    }

    const KAT_H2B_KEEPALIVE_CTR1: &str = "000000101d776e024c61ad6502aacb8d5ec4212d";
    const KAT_B2H: &str = "d723f96563fcb333c34f86768c5fd20e3d1abfde199e74fd77865de21deb562e";
    const KAT_B2H_RECORD: &str = "000000147917df2d4400fb351fce0de900778d87878bd4ec";

    #[test]
    fn box_id_is_pinned() {
        // Carlink's Kotlin test asserts the same.
        assert_eq!(box_id(&LinkKey([0x11; 32])), "cbb6a0d2a8710450");
    }

    #[test]
    fn key_hex_round_trip_and_refusals() {
        let k = LinkKey([0xab; 32]);
        assert_eq!(LinkKey::from_hex(&k.to_hex()).unwrap(), k);
        assert_eq!(LinkKey::from_hex(&k.to_hex().to_ascii_uppercase()).unwrap(), k);
        assert!(LinkKey::from_hex("abc").is_none());
        assert!(LinkKey::from_hex(&"g".repeat(64)).is_none());
        assert!(LinkKey::from_hex(&format!("+{}", "a".repeat(63))).is_none(), "from_str_radix would take a sign");
        assert_eq!(format!("{k:?}"), "LinkKey(<secret>)");
    }

    // ---- OCBS v1 ----

    const HOST_PUB: &str = "040217e617f0b6443928278f96999e69a23a4f2c152bdf6d6cdf66e5b80282d4ed194a7debcb97712d2dda3ca85aa8765a56f45fc758599652f2897c65306e5794";
    const BOX_PUB: &str = "04d65a93977caa3d1b081852ff57a79e465f1660577304baead505dd3a48589cf350185e895372df6221ea3a137557e473fddb6755f05bd507c3c533fce9c91285";

    /// The set-up exchange's vectors, as notes/ocbm.md gives them and Carlink reproduced with the JDK's EC classes.
    #[test]
    fn setup_known_answer_vectors() {
        let (hs, hp) = setup_keypair_from(&[0x11; 32]).unwrap();
        let (bs, bp) = setup_keypair_from(&[0x22; 32]).unwrap();
        assert_eq!(hex(&hp), HOST_PUB);
        assert_eq!(hex(&bp), BOX_PUB);
        let z = setup_shared(&hs, &bp).unwrap();
        assert_eq!(z, setup_shared(&bs, &hp).unwrap(), "both ways");
        assert_eq!(hex(&z), "ccfc261f58193c98ca4ad4a53bbac6f0ee29bc4d48438090446908622ca79af6");
        let (h2b, b2h) = setup_session_keys(&z, &Transcript { host_pub: hp, box_pub: bp });
        assert_eq!(hex(&h2b), "263e0d8efff4f691378e9b34501d35b52dc05692b96021e40cc2e2505abcd4e4");
        assert_eq!(hex(&b2h), "e414baa6ea6e0160cc2e31bfce7645e7b6c5db9657c5cb3af58e02e6fa3fada4");
        assert!(setup_keypair_from(&[0; 32]).is_none(), "zero is no scalar");
        assert!(setup_keypair_from(&[0xff; 32]).is_none(), "not below the order");
    }

    #[test]
    fn setup_handshake_runs_both_ways_and_refuses_a_point_off_the_curve() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let (bs, bp) = setup_keypair_from(&[0x22; 32]).unwrap();
            setup_box_handshake(&mut s, &bs, &bp)
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let (hs, hp) = setup_keypair_from(&[0x11; 32]).unwrap();
        let (mut h, ht) = setup_host_handshake(&mut c, &hs, &hp).unwrap();
        let (mut b, bt) = t.join().unwrap().unwrap();
        assert_eq!(ht, bt, "one transcript");
        let rec = h.tx.seal(b"OCBM");
        b.rx.push(&rec);
        assert_eq!(b.rx.next().unwrap().unwrap(), b"OCBM");
        // The first record each way, pinned (Carlink's loopback test pins the same pair).
        assert_eq!(hex(&rec), SETUP_H2B_RECORD);
        assert_eq!(hex(&b.tx.seal(b"OCBM")), SETUP_B2H_RECORD);
        // A point off the curve: the box ends the exchange.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let (bs, bp) = setup_keypair_from(&[0x22; 32]).unwrap();
            setup_box_handshake(&mut s, &bs, &bp).map(|_| ())
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let mut bad = setup_hello(&hp);
        bad[69] ^= 1;
        c.write_all(&bad).unwrap();
        assert!(matches!(t.join().unwrap(), Err(LinkError::BadProof)));
    }

    const SETUP_H2B_RECORD: &str = "00000014e5b08769aa5c6ef9047ce44fca11b38a80cf5655";
    const SETUP_B2H_RECORD: &str = "00000014badbeaa1861799c093909d6513c1d4421648deba";

    /// The set-up code's vector (ADDENDUM 5, the architect's; Carlink pins the same): the note's fixed points give the
    /// digest below and the code 2092. With the points swapped it would be 3978: the trap for the wrong order.
    #[test]
    fn the_set_up_code_is_pinned_and_takes_the_host_s_point_first() {
        let (_, hp) = setup_keypair_from(&[0x11; 32]).unwrap();
        let (_, bp) = setup_keypair_from(&[0x22; 32]).unwrap();
        let t = Transcript { host_pub: hp, box_pub: bp };
        assert_eq!(hex(&code_digest(&t)), "66ce8b3c25c5f65e6e40bed0735ce316977299b0ebb2c5031842d943933a6460");
        assert_eq!(setup_code(&t), "2092");
        let swapped = Transcript { host_pub: bp, box_pub: hp };
        assert_eq!(setup_code(&swapped), "3978", "the points in the wrong order give another code");
        // The padding done by hand is SHA-256's own, for these points and others.
        for t in [t.clone(), swapped, Transcript { host_pub: [0xa5; 65], box_pub: [0; 65] }] {
            use sha2::Digest;
            let whole: Vec<u8> = [&b"OCBS v1 code"[..], &t.host_pub, &t.box_pub].concat();
            assert_eq!(code_digest(&t), <[u8; 32]>::from(Sha256::digest(&whole)));
        }
        // Both sides of a real exchange agree on it.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let b = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let (bs, bp) = setup_keypair().unwrap();
            setup_box_handshake(&mut s, &bs, &bp).map(|(_, t)| setup_code(&t)).unwrap()
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let (hs, hp) = setup_keypair().unwrap();
        let (_, ht) = setup_host_handshake(&mut c, &hs, &hp).unwrap();
        assert_eq!(setup_code(&ht), b.join().unwrap());
    }

    /// Four digits whatever the value: zero-padded below 1000, the remainder of the whole u32.
    #[test]
    fn the_set_up_code_is_four_digits_zero_padded() {
        let d = |b: [u8; 4]| {
            let mut full = [0u8; 32];
            full[..4].copy_from_slice(&b);
            code_digits(&full)
        };
        assert_eq!(d([0, 0, 0, 7]), "0007");
        assert_eq!(d([0, 0, 0x27, 0x10]), "0000", "10000 is 0");
        assert_eq!(d([0xff; 4]), "7295", "4294967295 mod 10000, big-endian");
        assert_eq!(d([0x01, 0, 0, 0]), "7216", "16777216: the first byte is the most significant");
    }
}
