//! OCBM over TCP (the Wi-Fi transport): a pre-shared-key handshake, then ChaCha20-Poly1305 records
//! around the unchanged OCBM byte stream.
//!
//! WHY. Over USB the OCBM stream rides a cable only the head unit holds. Over Wi-Fi it rides the car's
//! hotspot, which passengers share, and the stream carries a root console (`CH_CONSOLE`), file push,
//! and CarPlay's per-session A/V keys. So nobody gets a byte through without the link key, and nobody
//! on the hotspot reads one.
//!
//! THE LINK KEY is 32 random bytes the host and the box share, set over USB once (provisioning). It
//! never crosses the network: both sides only ever prove they hold it.
//!
//! HANDSHAKE (all fields fixed length; `K` = link key, `|` = concatenation):
//!
//! ```text
//!   box  -> host   "OCBW" ver=1 box_nonce[32]                                   (37 B)
//!   host -> box    "OCBW" ver=1 host_nonce[32] HMAC(K, "OCBW host" | bn | hn)   (69 B)
//!   box  -> host   HMAC(K, "OCBW box" | hn | bn)                                 (32 B)
//! ```
//!
//! The host proves first, so a stranger who connects learns only a random nonce. The box answers
//! only a host that proved, which also proves the box to the host (a fake box can't answer).
//! Session keys: HKDF-SHA256(ikm = K, salt = bn | hn), info `"OCBW v1 host->box"` / `"OCBW v1 box->host"`.
//!
//! RECORDS: `[u32 BE ciphertext length][ciphertext | 16 B tag]`. The nonce is 4 zero bytes followed by a
//! per-direction 64-bit counter, little-endian, starting at 0, so a record that's dropped, replayed or
//! reordered fails its tag and ends the link. Record plaintext is any chunk of the OCBM stream, at most
//! [`MAX_RECORD`] bytes; record boundaries mean nothing to OCBM.
//!
//! Carlink's Kotlin side implements the same bytes; the test vectors here pin them.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::{Read, Write};

type HmacSha256 = Hmac<Sha256>;

pub const MAGIC: [u8; 4] = *b"OCBW";
pub const VERSION: u8 = 1;
/// TCP port the box listens on.
pub const PORT: u16 = 7020;
/// Largest record plaintext. OCBM frames are at most 16 B + 64 KiB; the relays read 16 KiB at a time.
pub const MAX_RECORD: usize = 64 * 1024;
const TAG: usize = 16;
const NONCE_LEN: usize = 32;

/// The 32-byte link key.
#[derive(Clone)]
pub struct LinkKey(pub [u8; 32]);

impl LinkKey {
    /// From 64 hex digits (whitespace around them is ignored).
    pub fn from_hex(s: &str) -> Option<LinkKey> {
        let s = s.trim();
        if s.len() != 64 {
            return None;
        }
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(LinkKey(k))
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl std::fmt::Debug for LinkKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkKey(<secret>)")
    }
}

#[derive(Debug)]
pub enum LinkError {
    Io(std::io::Error),
    /// The peer didn't speak OCBW v1.
    BadHello,
    /// The peer's proof didn't match: it doesn't hold this link key.
    BadProof,
    /// A record failed its tag (tampered, replayed, reordered, or the wrong key).
    BadRecord,
    /// A record longer than [`MAX_RECORD`] + tag.
    Oversize(usize),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Io(e) => write!(f, "i/o: {e}"),
            LinkError::BadHello => f.write_str("peer didn't open with OCBW v1"),
            LinkError::BadProof => f.write_str("peer's proof failed (wrong link key)"),
            LinkError::BadRecord => f.write_str("record failed authentication"),
            LinkError::Oversize(n) => write!(f, "record of {n} B exceeds the limit"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<std::io::Error> for LinkError {
    fn from(e: std::io::Error) -> Self {
        LinkError::Io(e)
    }
}

/// 32 bytes from the kernel's CSPRNG. `/dev/urandom` rather than `getrandom(2)`: the box's 3.14 kernel
/// predates the syscall, and the file works the same on the box, macOS and Linux.
pub fn random32() -> std::io::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

fn mac(key: &LinkKey, label: &[u8], a: &[u8], b: &[u8]) -> HmacSha256 {
    let mut m = <HmacSha256 as Mac>::new_from_slice(&key.0).expect("HMAC takes any key length");
    m.update(label);
    m.update(a);
    m.update(b);
    m
}

fn proof(key: &LinkKey, label: &[u8], a: &[u8], b: &[u8]) -> [u8; 32] {
    mac(key, label, a, b).finalize().into_bytes().into()
}

/// Constant-time check of a peer's proof.
fn check(key: &LinkKey, label: &[u8], a: &[u8], b: &[u8], got: &[u8]) -> bool {
    mac(key, label, a, b).verify_slice(got).is_ok()
}

/// The box's public id for discovery: the first 8 bytes of HMAC-SHA256(K, "OCBW id"), as 16 hex digits.
/// The mDNS advert carries it (TXT `id=`) so a host can pick ITS box among several on one hotspot
/// without the advert saying anything about the key.
pub fn box_id(key: &LinkKey) -> String {
    let full = proof(key, b"OCBW id", &[], &[]);
    full[..8].iter().map(|b| format!("{b:02x}")).collect()
}

const HOST_LABEL: &[u8] = b"OCBW host";
const BOX_LABEL: &[u8] = b"OCBW box";
const H2B: &[u8] = b"OCBW v1 host->box";
const B2H: &[u8] = b"OCBW v1 box->host";

/// The two directions' record keys, from the link key and both nonces.
fn session_keys(key: &LinkKey, bn: &[u8; 32], hn: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(bn);
    salt[32..].copy_from_slice(hn);
    let hk = Hkdf::<Sha256>::new(Some(&salt), &key.0);
    let (mut h2b, mut b2h) = ([0u8; 32], [0u8; 32]);
    hk.expand(H2B, &mut h2b).expect("32 B is a valid HKDF length");
    hk.expand(B2H, &mut b2h).expect("32 B is a valid HKDF length");
    (h2b, b2h)
}

/// Seals one direction's records.
pub struct Sealer {
    aead: ChaCha20Poly1305,
    ctr: u64,
}

/// Opens the other direction's records, from a byte stream that can split them anywhere.
pub struct Opener {
    aead: ChaCha20Poly1305,
    ctr: u64,
    buf: Vec<u8>,
}

fn nonce(ctr: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&ctr.to_le_bytes());
    n
}

impl Sealer {
    fn new(k: &[u8; 32]) -> Self {
        Sealer { aead: ChaCha20Poly1305::new(Key::from_slice(k)), ctr: 0 }
    }

    /// One record for `plain` (at most [`MAX_RECORD`] bytes), ready to write.
    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        assert!(plain.len() <= MAX_RECORD, "record plaintext over MAX_RECORD");
        let n = nonce(self.ctr);
        self.ctr += 1;
        let ct = self
            .aead
            .encrypt(Nonce::from_slice(&n), Payload { msg: plain, aad: &[] })
            .expect("ChaCha20-Poly1305 encrypt can't fail for in-range lengths");
        let mut out = Vec::with_capacity(4 + ct.len());
        out.extend_from_slice(&(ct.len() as u32).to_be_bytes());
        out.extend_from_slice(&ct);
        out
    }
}

impl Opener {
    fn new(k: &[u8; 32]) -> Self {
        Opener { aead: ChaCha20Poly1305::new(Key::from_slice(k)), ctr: 0, buf: Vec::new() }
    }

    /// Bytes as they came off the socket.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole record's plaintext, `Ok(None)` until one has arrived. An error ends the link.
    pub fn next(&mut self) -> Result<Option<Vec<u8>>, LinkError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_RECORD + TAG || len < TAG {
            return Err(LinkError::Oversize(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let n = nonce(self.ctr);
        let plain = self
            .aead
            .decrypt(Nonce::from_slice(&n), Payload { msg: &self.buf[4..4 + len], aad: &[] })
            .map_err(|_| LinkError::BadRecord)?;
        self.ctr += 1;
        self.buf.drain(..4 + len);
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

/// The box's side, on a freshly accepted connection. Blocking; give the stream a read timeout first so a
/// silent stranger can't hold the listener.
pub fn box_handshake<S: Read + Write>(s: &mut S, key: &LinkKey, box_nonce: [u8; 32]) -> Result<Session, LinkError> {
    let mut hello = [0u8; 37];
    hello[..4].copy_from_slice(&MAGIC);
    hello[4] = VERSION;
    hello[5..].copy_from_slice(&box_nonce);
    s.write_all(&hello)?;
    let mut reply = [0u8; 69];
    s.read_exact(&mut reply)?;
    if reply[..4] != MAGIC || reply[4] != VERSION {
        return Err(LinkError::BadHello);
    }
    let mut hn = [0u8; NONCE_LEN];
    hn.copy_from_slice(&reply[5..37]);
    if !check(key, HOST_LABEL, &box_nonce, &hn, &reply[37..69]) {
        return Err(LinkError::BadProof);
    }
    s.write_all(&proof(key, BOX_LABEL, &hn, &box_nonce))?;
    let (h2b, b2h) = session_keys(key, &box_nonce, &hn);
    Ok(Session { tx: Sealer::new(&b2h), rx: Opener::new(&h2b) })
}

/// The host's side, on a fresh connection to the box.
pub fn host_handshake<S: Read + Write>(s: &mut S, key: &LinkKey, host_nonce: [u8; 32]) -> Result<Session, LinkError> {
    let mut hello = [0u8; 37];
    s.read_exact(&mut hello)?;
    if hello[..4] != MAGIC || hello[4] != VERSION {
        return Err(LinkError::BadHello);
    }
    let mut bn = [0u8; NONCE_LEN];
    bn.copy_from_slice(&hello[5..37]);
    let mut reply = [0u8; 69];
    reply[..4].copy_from_slice(&MAGIC);
    reply[4] = VERSION;
    reply[5..37].copy_from_slice(&host_nonce);
    reply[37..].copy_from_slice(&proof(key, HOST_LABEL, &bn, &host_nonce));
    s.write_all(&reply)?;
    let mut bp = [0u8; 32];
    s.read_exact(&mut bp)?;
    if !check(key, BOX_LABEL, &host_nonce, &bn, &bp) {
        return Err(LinkError::BadProof);
    }
    let (h2b, b2h) = session_keys(key, &bn, &host_nonce);
    Ok(Session { tx: Sealer::new(&h2b), rx: Opener::new(&b2h) })
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
            s.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            box_handshake(&mut s, &LinkKey(box_key), [1u8; 32])
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
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
        let r2 = b.tx.seal(b"hello host");
        let r3 = b.tx.seal(b"again");
        let mut both = r2.clone();
        both.extend_from_slice(&r3);
        h.rx.push(&both);
        assert_eq!(h.rx.next().unwrap().unwrap(), b"hello host");
        assert_eq!(h.rx.next().unwrap().unwrap(), b"again");
        assert!(h.rx.next().unwrap().is_none());
    }

    #[test]
    fn wrong_key_is_refused_by_the_box() {
        let (b, _h) = pair(K, [8u8; 32]);
        assert!(matches!(b, Err(LinkError::BadProof)));
    }

    #[test]
    fn tamper_replay_and_reorder_fail() {
        let (b, h) = pair(K, K);
        let (mut b, mut h) = (b.unwrap(), h.unwrap());
        let r0 = h.tx.seal(b"zero");
        let r1 = h.tx.seal(b"one");
        // reorder: r1 first
        b.rx.push(&r1);
        assert!(matches!(b.rx.next(), Err(LinkError::BadRecord)));
        // tamper
        let (b2, h2) = pair(K, K);
        let (mut b2, mut h2) = (b2.unwrap(), h2.unwrap());
        let mut t = h2.tx.seal(b"data");
        let last = t.len() - 1;
        t[last] ^= 1;
        b2.rx.push(&t);
        assert!(matches!(b2.rx.next(), Err(LinkError::BadRecord)));
        // replay
        let _ = r0;
        let (b3, h3) = pair(K, K);
        let (mut b3, mut h3) = (b3.unwrap(), h3.unwrap());
        let a = h3.tx.seal(b"a");
        b3.rx.push(&a);
        assert!(b3.rx.next().unwrap().is_some());
        b3.rx.push(&a);
        assert!(matches!(b3.rx.next(), Err(LinkError::BadRecord)));
    }

    #[test]
    fn oversize_length_is_refused_before_buffering() {
        let (b, _h) = pair(K, K);
        let mut b = b.unwrap();
        b.rx.push(&((MAX_RECORD + TAG + 1) as u32).to_be_bytes());
        assert!(matches!(b.rx.next(), Err(LinkError::Oversize(_))));
    }

    /// Pins the exact bytes so Carlink's Kotlin implementation can be checked against them.
    #[test]
    fn known_answer_vector() {
        let key = LinkKey([0x11; 32]);
        let bn = [0x22u8; 32];
        let hn = [0x33u8; 32];
        let hp = proof(&key, HOST_LABEL, &bn, &hn);
        let bp = proof(&key, BOX_LABEL, &hn, &bn);
        let (h2b, _b2h) = session_keys(&key, &bn, &hn);
        let mut s = Sealer::new(&h2b);
        let rec = s.seal(b"OCBM");
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        // Carlink's Kotlin test asserts these same four strings.
        assert_eq!(hex(&hp), "37256608df8b1738ce01a824112515ab0f72420ad890eb5f28b8e1378ef3c276");
        assert_eq!(hex(&bp), "6b0464993905c9cb244c394e2114ed5db546a3e01a882fa33e18e9b4a88b5ea0");
        assert_eq!(hex(&h2b), "cdbc30ed5bb1352687e3175ad53c08496750fdf4cbce95eeee76e9f66c4fa29a");
        assert_eq!(hex(&rec), "000000140cfc70b3b120a8adfad9270f9691f573c1d1ad5d");
    }

    #[test]
    fn box_id_is_pinned() {
        // Carlink's Kotlin test asserts the same.
        assert_eq!(box_id(&LinkKey([0x11; 32])), "cbb6a0d2a8710450");
    }

    #[test]
    fn key_hex_round_trip() {
        let k = LinkKey([0xab; 32]);
        assert_eq!(LinkKey::from_hex(&k.to_hex()).unwrap().0, k.0);
        assert!(LinkKey::from_hex("abc").is_none());
    }
}
