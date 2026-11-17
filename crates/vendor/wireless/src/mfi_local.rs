//! mfi_local.rs — MFi 2.0C authentication over the box's LOCAL I2C chip (`/dev/i2c-1 @0x11`).
//!
//! The genuine MFi 2.0C coprocessor sits on `/dev/i2c-1 @0x11` and is driven directly, exactly as the
//! wired `iap2d` daemon does (`ccpa/iap2d/src/main.rs`): the same proven direct-I2C cert-copy +
//! challenge-sign, as `cert()`/`sign()`.
//!
//! One MFi chip, shared with `iap2d` — but the session arbiter guarantees only one transport is
//! active at a time, so wired and wireless never drive the chip concurrently.

use mfi_i2c_local::cert_cache::{Fetched, Source};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MFI_ADDR: u16 = 0x11;
const I2C_M_RD: u16 = 0x0001;
const I2C_RDWR: libc::c_int = 0x0707;

static G_I2C: Mutex<i32> = Mutex::new(-1);

/// Open `/dev/i2c-1` (O_RDWR) on first use and cache the fd. The I2C_RDWR ioctl carries the slave
/// address per message, so no I2C_SLAVE setup is needed — just the fd. Returns <0 if the device
/// can't be opened.
///
/// Retries on the next call after a failure, deliberately: a `Once` latched a transient open failure
/// for the process lifetime, which disabled MFi — and with it every CarPlay session — until the
/// daemon restarted, while the caller's own `mfi_retry` could do nothing about it.
fn i2c_fd() -> i32 {
    let mut cached = G_I2C.lock().unwrap_or_else(|p| p.into_inner());
    if *cached >= 0 {
        return *cached;
    }
    // O_CLOEXEC: this is a PROCESS-LIFETIME fd (G_I2C, opened once) and this crate fork+execs the
    // detached A/V daemons via av::ensure_av_layer(). Without it, carplayd and rx-connect inherit an
    // open handle to the MFi chip's I2C bus and keep it for as long as they live.
    let fd = unsafe { libc::open(c"/dev/i2c-1".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        eprintln!("[mfi-local] cannot open /dev/i2c-1 (MFi chip) -- retrying on the next transaction");
        return fd;
    }
    *cached = fd;
    fd
}

/// Path of the cross-process advisory lock serializing all MFi I2C access (#109). FIVE users share the
/// single `/dev/i2c-1` chip (corrected 2026-07-25 — this used to say "both daemons"): wired `iap2d`,
/// wireless `btd` (here), `carplayd`'s `LocalMfiSigner`, `receiver`'s tunnel handshake
/// via `mfi-i2c-local`, and `ocbmd`'s `CH_MFI` relay. The cert/sign sequences are stateful (write challenge → go → poll status → read
/// result), so any interleaving corrupts both transactions. Every user `flock`s this path for the whole
/// duration of a cert()/sign(), each with a bounded 10s LOCK_NB poll (matching `carplayd`'s `MfiLock`
/// and `mfi-i2c-local`). The session arbiter also enforces single-transport, but this is the low-level
/// guarantee independent of it.
pub const MFI_LOCK_PATH: &[u8] = b"/tmp/carplay_mfi.lock\0";

/// RAII holder of the exclusive MFi lock — released (LOCK_UN + close) on drop, i.e. when the cert/sign
/// scope ends. Returns `None` if the lockfile can't be opened/locked (caller then aborts the op).
struct MfiLock(i32);
impl MfiLock {
    fn acquire() -> Option<MfiLock> {
        let fd = unsafe {
            libc::open(
                MFI_LOCK_PATH.as_ptr() as *const libc::c_char,
                // O_CLOEXEC — an inherited LOCK_EX open-file-description outlives this process in a
                // detached daemon; if the holder dies before Drop can LOCK_UN, every one of the five
                // MFi users blocks to its deadline forever.
                libc::O_CREAT | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return None;
        }
        // BOUNDED acquire (LOCK_NB + deadline), matching `ccpa/carplayd/src/main.rs`'s `MfiLock` and
        // `mfi-i2c-local`'s: a wedged holder must not block this caller forever. The legitimate worst
        // case is the sign path's ~2.1s poll x 3 retries ~= 6.3s, so 10s is a real ceiling.
        const DEADLINE: Duration = Duration::from_secs(10);
        let start = Instant::now();
        loop {
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Some(MfiLock(fd));
            }
            if start.elapsed() >= DEADLINE {
                eprintln!(
                    "[mfi-local] MFi lock not acquired within {}s — giving up rather than blocking \
                     the caller",
                    DEADLINE.as_secs()
                );
                unsafe { libc::close(fd) };
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for MfiLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0, libc::LOCK_UN);
            libc::close(self.0);
        }
    }
}

#[repr(C)]
struct I2cMsg {
    addr: u16,
    flags: u16,
    len: u16,
    buf: *mut u8,
}
#[repr(C)]
struct I2cRdwr {
    msgs: *mut I2cMsg,
    nmsgs: u32,
}

fn i2c_rd(reg: u8, out: &mut [u8]) -> bool {
    let fd = i2c_fd();
    if fd < 0 {
        return false;
    }
    let mut r = reg;
    let mut m = [
        I2cMsg {
            addr: MFI_ADDR,
            flags: 0,
            len: 1,
            buf: &mut r,
        },
        I2cMsg {
            addr: MFI_ADDR,
            flags: I2C_M_RD,
            len: out.len() as u16,
            buf: out.as_mut_ptr(),
        },
    ];
    let mut x = I2cRdwr {
        msgs: m.as_mut_ptr(),
        nmsgs: 2,
    };
    for _ in 0..5 {
        // I2C_RDWR returns the number of messages transferred — require BOTH (a partial transfer
        // means the read leg never happened, and `out` would be consumed uninitialized). Aligned with
        // the byte-for-byte twin in `mfi-i2c-local`, which this file must not drift from: `>= 0`
        // accepted a partial and let an all-zero buffer through as a valid signature (audit 1.6).
        if unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut x) } == 2 {
            return true;
        }
        unsafe { libc::usleep(5000) };
    }
    false
}

fn i2c_wr(reg: u8, data: &[u8]) -> bool {
    let fd = i2c_fd();
    if fd < 0 {
        return false;
    }
    let mut b = Vec::with_capacity(1 + data.len());
    b.push(reg);
    b.extend_from_slice(data);
    let mut m = I2cMsg {
        addr: MFI_ADDR,
        flags: 0,
        len: b.len() as u16,
        buf: b.as_mut_ptr(),
    };
    let mut x = I2cRdwr {
        msgs: &mut m,
        nmsgs: 1,
    };
    for _ in 0..5 {
        // I2C_RDWR returns the number of messages transferred — require the write to have landed.
        if unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut x) } == 1 {
            return true;
        }
        unsafe { libc::usleep(5000) };
    }
    false
}

/// Fetch the accessory certificate for the phone's 0xAA00, fed to iAP2 0xAA01: the boot's cached one when there is
/// one, else the chip's (CopyCertificate: reg `0x30` len (2 BE), reg `0x31` cert, under the lock), cached when it is
/// one DER certificate (`mfi_i2c_local::cert_cache::fetch`). Same return shape as `carplay_iap2_core::mfi::cert()` (the auth op `0x01`).
///
/// The cache is the tunnel's (`mfi_i2c_local::cert_cache`), shared since A4 (MFI-CACHE-SCOPE): btd fills it as a host
/// session begins ([`warm_cert_cache`]), so from the first session of a boot btd's certificate costs no chip read. A
/// session that sent a certificate and ended without handing off drops it ([`session_over`]).
pub fn cert() -> Option<Vec<u8>> {
    let f = certificate(CERT_CACHE, MfiLock::acquire, chip_cert)?; // the lock serializes vs iap2d (#109)
    eprintln!("{}", fetched_line(&f));
    crate::startup::duration("cert1_ms", ms(f.under));
    CERT_SENT.store(true, Ordering::Relaxed);
    Some(f.cert)
}

/// The chip's certificate, read under the lock the caller holds: reg `0x30` its length (2 BE), reg `0x31` its bytes.
fn chip_cert() -> Option<Vec<u8>> {
    let mut lb = [0u8; 2];
    if !i2c_rd(0x30, &mut lb) {
        return None;
    }
    let n = ((lb[0] as usize) << 8) | lb[1] as usize;
    if n == 0 || n > 2048 {
        return None;
    }
    let mut o = vec![0u8; n];
    if !i2c_rd(0x31, &mut o) {
        return None;
    }
    Some(o)
}

/// The boot's cached certificate, the tunnel's (`mfi_i2c_local::cert_cache`). `cfg(test)`: a path of its own.
#[cfg(not(test))]
const CERT_CACHE: &str = mfi_i2c_local::cert_cache::PATH;
#[cfg(test)]
const CERT_CACHE: &str = "/tmp/btd_mfi_local_selftest_cert.der";

/// Whether this Bluetooth session sent the phone a certificate ([`cert`]); read and cleared as it ends ([`session_over`]).
/// One slot: one Bluetooth session runs at a time (`control::SessionClaim`).
static CERT_SENT: AtomicBool = AtomicBool::new(false);

/// A certificate's log line (and the timeline's cert1_ms is its time under the lock).
fn fetched_line(f: &Fetched) -> String {
    match f.from {
        Source::Cache => format!("[mfi-local] cert: {} B from the boot's cache, no chip read", f.cert.len()),
        Source::Chip { cached } => {
            let line = cert_line(f.cert.len(), f.under, f.waited);
            if cached {
                line
            } else {
                format!("{line}; not one DER certificate: sent, not cached")
            }
        }
    }
}

/// The certificate by the cache's rules, shared with the tunnel and carplayd (`mfi_i2c_local::cert_cache::fetch`): the
/// one cached at `path`, else the chip's (`read`) with the lock held (`lock`, None when it cannot be had), cached after
/// the lock is let go when it is one DER certificate.
fn certificate<L>(path: &str, lock: impl FnOnce() -> Option<L>, read: impl FnOnce() -> Option<Vec<u8>>) -> Option<Fetched> {
    mfi_i2c_local::cert_cache::fetch(path, || lock().ok_or(()), || read().ok_or(())).ok()
}

/// What a Bluetooth session's end does with the cache ([`session_over`]): drop it when the session sent a certificate
/// and did not hand off.
fn drop_cache_at_end(sent: bool, handed_off: bool) -> bool {
    sent && !handed_off
}

/// A Bluetooth session ended (`bt_driver::run`, through `av::put_back`); `handed_off` when it sent its 0x5703. One that
/// sent a certificate and did not hand off may have had it refused: the phone reports a failed authentication with
/// 0xAA04, which ends the session there, so the cache is dropped and the next session of any chip user reads the chip,
/// as the tunnel does on its 0xAA04 (A4). A session that ended without a handoff for another reason costs that one read.
pub(crate) fn session_over(handed_off: bool) {
    let sent = CERT_SENT.swap(false, Ordering::Relaxed);
    if drop_cache_at_end(sent, handed_off) && mfi_i2c_local::cert_cache::forget(CERT_CACHE) {
        eprintln!(
            "[mfi-local] the session sent the certificate and ended without a handoff: the cached certificate is \
             forgotten, the next session reads the chip"
        );
    }
}

/// A host session begins (`av::session_starting`): when no certificate is cached yet (the first session of a boot), read
/// it once in the background so the phone's 0xAA00 finds it. No phone is connected yet and carplayd has no control
/// connection, so no signature can be waiting for the lock; the read holds it no longer than any certificate read.
pub(crate) fn warm_cert_cache() {
    if mfi_i2c_local::cert_cache::read(CERT_CACHE).is_some() {
        return;
    }
    let spawned = std::thread::Builder::new().name("mfi-warm".into()).spawn(|| {
        for attempt in 1..=3u32 {
            match certificate(CERT_CACHE, MfiLock::acquire, chip_cert) {
                Some(f) if f.from == Source::Cache => return,
                Some(f) => {
                    eprintln!("{} (the session's start: the boot's first read)", fetched_line(&f));
                    return;
                }
                None if attempt < 3 => std::thread::sleep(Duration::from_millis(20)),
                None => eprintln!("[mfi-local] cert: the session's-start read failed 3 times; the phone's 0xAA00 reads the chip"),
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("[mfi-local] cert: no read at the session's start ({e}); the phone's 0xAA00 reads the chip");
    }
}

/// Sign a challenge digest. CreateSignature: write challenge (`0x20` len, `0x21` data), go
/// (`0x10`=1), poll status bit4, read `0x11` len / `0x12` sig. Same as the auth op `0x02`, fed to
/// iAP2 0xAA03.
pub fn sign(chal: &[u8]) -> Option<Vec<u8>> {
    let asked = Instant::now();
    let lock = MfiLock::acquire()?; // hold the lock across the ENTIRE stateful sequence (#109)
    let held = Instant::now();
    let clen = chal.len();
    if !i2c_wr(0x20, &[(clen >> 8) as u8, clen as u8]) {
        return None;
    }
    if !i2c_wr(0x21, chal) {
        return None;
    }
    if !i2c_wr(0x10, &[0x01]) {
        return None;
    }
    let go = Instant::now();
    unsafe { libc::usleep(100_000) };
    let mut done = false;
    // Bounded by WALL CLOCK, not iteration count — the same defect audit #8 fixed in
    // `mfi-i2c-local`. `for _ in 0..200` with a 10 ms sleep looks like ~2.1 s, and is, but only
    // while every status read succeeds. The status read is itself `for _ in 0..5` with a 5 ms
    // sleep, so under chip NAK each iteration costs ~35 ms and the loop runs ~7.1 s — and with
    // the caller's retries that is ~21 s holding /tmp/carplay_mfi.lock, blowing through every
    // other chip user's 10 s acquire deadline.
    const SIGN_POLL_DEADLINE: std::time::Duration = std::time::Duration::from_millis(2500);
    let poll_start = std::time::Instant::now();
    while poll_start.elapsed() < SIGN_POLL_DEADLINE {
        let mut st = [0u8; 1];
        // Poll control-register 0x10 bit 4 = "signature ready" — byte-identical to the DEVICE-VERIFIED
        // wired iap2d path (ccpa/iap2d/src/main.rs:120). QC #124 flagged this as accepting non-ready
        // codes, but a stricter predicate would DIVERGE from the proven reference with no MFi-2.0C
        // register spec to ground it, risking a regression to working auth. The result is instead
        // validated downstream: a spurious "ready" yields a bogus 0x11 length that the `n == 0 || n >
        // 256` guard below rejects → sign() returns None → the caller's #210 retry re-drives cleanly.
        if i2c_rd(0x10, &mut st) && (st[0] & 0x10) != 0 {
            if st[0] != 0x10 {
                eprintln!("[mfi-local] sign status 0x{:02x} (expected 0x10)", st[0]);
            }
            done = true;
            break;
        }
        unsafe { libc::usleep(10_000) };
    }
    if !done {
        return None;
    }
    let ready = go.elapsed();
    let mut sl = [0u8; 2];
    if !i2c_rd(0x11, &mut sl) {
        return None;
    }
    let n = ((sl[0] as usize) << 8) | sl[1] as usize;
    if n == 0 || n > 256 {
        return None;
    }
    let mut sig = vec![0u8; n];
    if !i2c_rd(0x12, &mut sig) {
        return None;
    }
    let under = held.elapsed();
    drop(lock); // released as the sequence ends, as before; the time is told after
    eprintln!("{}", sign_line(ready, under, held.duration_since(asked)));
    crate::startup::duration("chip1_ms", ms(ready));
    Some(sig)
}

// ---- Instrumentation: how long the chip took (the connect timeline's chip1_ms and cert1_ms) ----------------------

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// A certificate read's line: its bytes and time under the lock, and the wait for the lock.
fn cert_line(bytes: usize, under: Duration, waited: Duration) -> String {
    format!("[mfi-local] cert: {bytes} B in {} ms under the lock, {} ms waiting for it", ms(under), ms(waited))
}

/// A signature's line: the chip's time from go to status ready (the fixed 100 ms first wait included), the whole
/// sequence's time under the lock, and the wait for the lock.
fn sign_line(ready: Duration, under: Duration, waited: Duration) -> String {
    format!(
        "[mfi-local] sign: chip {} ms from go to ready, {} ms under the lock, {} ms waiting for it",
        ms(ready),
        ms(under),
        ms(waited)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A certificate's shape: `30 82` and a length that is the rest of it (945 bytes for 941).
    fn der(body: usize) -> Vec<u8> {
        let mut c = vec![0x30, 0x82, (body >> 8) as u8, body as u8];
        c.extend(std::iter::repeat(0x5a).take(body));
        c
    }

    fn cache_path(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("btd_cert_cache_{tag}_{}.der", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p.to_str().unwrap().to_string()
    }

    /// A4: the first certificate of a boot comes from the chip under the lock and is cached; every later one comes from
    /// the cache with neither the lock nor the chip.
    #[test]
    fn the_boots_first_read_is_cached_and_served_after_without_the_chip() {
        let path = cache_path("served");
        let (locks, reads) = (Cell::new(0), Cell::new(0));
        let lock = || {
            locks.set(locks.get() + 1);
            Some(())
        };
        let f = certificate(&path, lock, || {
            reads.set(reads.get() + 1);
            Some(der(941))
        })
        .unwrap();
        assert_eq!((f.cert.len(), f.from), (945, Source::Chip { cached: true }));
        let again = certificate(&path, lock, || -> Option<Vec<u8>> { panic!("the chip is not read again") }).unwrap();
        assert_eq!(fetched_line(&again), "[mfi-local] cert: 945 B from the boot's cache, no chip read");
        assert_eq!((again.cert, again.from, again.under), (der(941), Source::Cache, Duration::ZERO));
        assert_eq!((locks.get(), reads.get()), (1, 1), "the lock and the chip once, for the first read only");
        let _ = std::fs::remove_file(&path);
    }

    /// CS-3's rule in btd: a read that isn't one DER certificate goes to this phone and is not cached.
    #[test]
    fn a_read_that_is_not_a_certificate_is_sent_once_and_not_cached() {
        let path = cache_path("bad");
        let f = certificate(&path, || Some(()), || Some(vec![0u8; 945])).unwrap();
        assert_eq!(f.from, Source::Chip { cached: false });
        assert_eq!(f.cert, vec![0u8; 945], "sent as read");
        assert!(fetched_line(&f).ends_with("; not one DER certificate: sent, not cached"), "{}", fetched_line(&f));
        assert!(!std::path::Path::new(&path).exists());
        let next = certificate(&path, || Some(()), || Some(der(941))).unwrap();
        assert_eq!(next.from, Source::Chip { cached: true }, "the next session reads the chip again");
        let _ = std::fs::remove_file(&path);
    }

    /// A cache another chip user filled while this one waited for the lock is served, and the chip is not read; no lock,
    /// no certificate (as before: the caller's retry).
    #[test]
    fn the_cache_is_looked_at_again_once_the_lock_is_held() {
        let path = cache_path("filled");
        let fill = || {
            assert!(mfi_i2c_local::cert_cache::keep(&path, &der(941)), "another user caches it as we wait");
            Some(())
        };
        let f = certificate(&path, fill, || -> Option<Vec<u8>> { panic!("filled meanwhile: no chip read") }).unwrap();
        assert_eq!(f.from, Source::Cache);
        let _ = std::fs::remove_file(&path);
        assert_eq!(certificate(&path, || None::<()>, || Some(der(941))), None, "the lock not had: no certificate");
        assert!(!std::path::Path::new(&path).exists(), "and nothing read or cached");
        assert_eq!(certificate(&path, || Some(()), || None), None, "the chip did not answer");
    }

    /// A Bluetooth session that sent the certificate and ended without its handoff drops the cache: the phone reports a
    /// failed authentication with 0xAA04, which ends it there. A handoff proves the phone took it.
    #[test]
    fn a_session_that_sent_the_certificate_and_did_not_hand_off_drops_the_cache() {
        assert!(drop_cache_at_end(true, false), "a refused certificate ends the session before its handoff");
        assert!(!drop_cache_at_end(true, true), "handed off: the phone authenticated us");
        assert!(!drop_cache_at_end(false, false), "no certificate sent (a phone that never asked): left as it is");
        assert!(!drop_cache_at_end(false, true));
    }

    #[test]
    fn the_chip_times_read_as_they_are_logged() {
        let ms = Duration::from_millis;
        assert_eq!(cert_line(945, ms(41), ms(0)), "[mfi-local] cert: 945 B in 41 ms under the lock, 0 ms waiting for it");
        assert_eq!(
            sign_line(ms(298), ms(305), ms(12)),
            "[mfi-local] sign: chip 298 ms from go to ready, 305 ms under the lock, 12 ms waiting for it"
        );
    }
}
