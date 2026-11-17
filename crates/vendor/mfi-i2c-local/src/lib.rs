//! mfi-i2c-local — MFi 2.0C authentication over the box's LOCAL I2C chip (`/dev/i2c-1 @0x11`).
//!
//! A standalone crate (not a module inside `receiver` or `mfi`) purely because both of those crates
//! carry `#![forbid(unsafe_code)]`, and this is unavoidably `unsafe` (raw ioctl/i2c). This is a direct
//! PORT of `crates/vendor/wireless/src/mfi_local.rs` (byte-for-byte identical protocol and, critically,
//! the SAME `/tmp/carplay_mfi.lock` path) — added for `crates/vendor/receiver`'s AirPlay-tunnel iAP2
//! handshake (`iap_tunnel.rs`), which needs the same direct-I2C cert()/sign() calls `bt_driver.rs` and
//! `iap2d` already make. `wireless/src/mfi_local.rs` is deliberately left untouched (a working,
//! already-proven file) rather than refactored to share this crate — this is a second, independent
//! implementation of the identical protocol, kept in lock-step by the shared lock file path, not by
//! shared code. If the two ever need to diverge, that's a sign they should be unified for real; until
//! then, duplication here is intentional insurance against a working file.
//!
//! One physical MFi chip, THREE potential concurrent users: `iap2d` (wired), `btd`'s
//! `bt_driver.rs` (wireless BT session — which per docs/wireless/00_WIRELESS_CARPLAY.md now stays alive until `disableBluetooth`,
//! i.e. potentially overlapping with the AirPlay session's own iAP2-tunnel handshake), and `carplayd`'s
//! new AirPlay-tunnel iAP2 session (this crate). All three MUST serialize through the same
//! `/tmp/carplay_mfi.lock` flock, or an interleaved i2c transaction corrupts across processes.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

const MFI_ADDR: u16 = 0x11;
const I2C_M_RD: u16 = 0x0001;
const I2C_RDWR: libc::c_int = 0x0707;

static G_I2C: AtomicI32 = AtomicI32::new(-1);
static OPEN_ONCE: Once = Once::new();

/// Lazily open `/dev/i2c-1` once (O_RDWR). The I2C_RDWR ioctl carries the slave address per message,
/// so no I2C_SLAVE setup is needed — just the fd. Returns <0 if the device can't be opened.
fn i2c_fd() -> i32 {
    OPEN_ONCE.call_once(|| {
        let fd = unsafe { libc::open(c"/dev/i2c-1".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            eprintln!("[mfi-i2c-local] cannot open /dev/i2c-1 (MFi chip) — will retry on next use");
        }
        G_I2C.store(fd, Ordering::Relaxed);
    });
    // RETRY on a previously-failed open. `Once` fires exactly once, so a single transient failure used
    // to cache -1 and make every later cert()/sign() return None for the whole process lifetime —
    // unrecoverable even by `mfi_retry`'s three attempts, permanently disabling tunnel authentication.
    let fd = G_I2C.load(Ordering::Relaxed);
    if fd >= 0 {
        return fd;
    }
    let retry = unsafe { libc::open(c"/dev/i2c-1".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if retry < 0 {
        return retry;
    }
    // Publish the reopened fd only if we're first: two callers racing this path would otherwise both
    // store, and the loser's fd leaks (one leaked fd per lost race, for the process lifetime).
    match G_I2C.compare_exchange(fd, retry, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => {
            eprintln!("[mfi-i2c-local] /dev/i2c-1 reopened after an earlier failure");
            retry
        }
        Err(_) => {
            // Another caller won the race — close our duplicate and use theirs.
            unsafe { libc::close(retry) };
            G_I2C.load(Ordering::Relaxed)
        }
    }
}

/// Path of the cross-process advisory lock serializing ALL MFi I2C access, wired and wireless alike —
/// MUST stay byte-identical to `crates/vendor/wireless/src/mfi_local.rs::MFI_LOCK_PATH`.
pub const MFI_LOCK_PATH: &[u8] = b"/tmp/carplay_mfi.lock\0";

/// RAII holder of the exclusive MFi lock — released (LOCK_UN + close) on drop, i.e. when the cert/sign
/// scope ends. [`MfiLock::acquire`] returns [`MfiError::LockUnavailable`] if the lockfile can't be
/// opened and [`MfiError::LockBusy`] if it can't be locked in time (caller then aborts the op).
struct MfiLock(i32);
impl MfiLock {
    fn acquire() -> Result<MfiLock, MfiError> {
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
            // ENOENT here means the containing directory does not exist (Android has no `/tmp`);
            // EACCES/EROFS mean it is not writable. None of these are contention, and saying so is
            // the whole point of this branch.
            let err = std::io::Error::last_os_error();
            eprintln!(
                "[mfi-i2c-local] cannot OPEN the MFi lock at {} ({err}) — this host has no local chip \
                 access. NOT lock contention.",
                String::from_utf8_lossy(&MFI_LOCK_PATH[..MFI_LOCK_PATH.len() - 1])
            );
            return Err(MfiError::LockUnavailable);
        }
        // BOUNDED acquire (LOCK_NB + deadline), matching `ccpa/carplayd/src/main.rs`'s own `MfiLock`
        // and `iap2d`'s. This was the only one of the four chip users still taking an UNBOUNDED
        // `LOCK_EX`, and it is called from `iap_tunnel::execute` with the global SESSION guard held —
        // so a wedged holder stalled the DataStream reader, and with it every inbound iAP2 frame, plus
        // teardown (which also takes SESSION). The legitimate worst case is the sign path's ~2.1 s poll
        // x 3 MFi retries ~= 6.3 s, so 10 s is a real ceiling rather than an arbitrary one.
        const DEADLINE: Duration = Duration::from_secs(10);
        let start = Instant::now();
        loop {
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(MfiLock(fd));
            }
            if start.elapsed() >= DEADLINE {
                eprintln!(
                    "[mfi-i2c-local] MFi lock not acquired within {}s — giving up rather than blocking \
                     the caller (which may hold the iAP2 session lock)",
                    DEADLINE.as_secs()
                );
                unsafe { libc::close(fd) };
                return Err(MfiError::LockBusy);
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
        // means the read leg never happened, and `out` would be consumed uninitialized).
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

/// Why an MFi operation failed. The distinction matters upstream: retrying a chip NAK is the point of
/// `mfi_retry`, but retrying a LOCK TIMEOUT just multiplies a stall by the retry count — 3 x 10 s of
/// the tunnel's SESSION mutex held, for a wait that cannot succeed any sooner the second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MfiError {
    /// Could not acquire `/tmp/carplay_mfi.lock` within its deadline — another chip user genuinely
    /// holds it. Do NOT retry: come back next session.
    LockBusy,
    /// The lock file could not be OPENED AT ALL — the path is not writable on this host.
    ///
    /// Structurally different from [`MfiError::LockBusy`] and it must not be conflated with it, which
    /// is exactly what happened for months: both cases returned `None` from `MfiLock::acquire()`, the
    /// callers mapped every `None` to `LockBusy`, and the resulting log line said "another chip user
    /// holds it". A host without a writable `/tmp` fails `open`, and that hard structural failure
    /// read as transient contention.
    ///
    /// This variant means "this host cannot do local chip access at all": no amount of waiting or
    /// retrying will change it.
    LockUnavailable,
    /// The chip itself refused, NAKed, or timed out. Retrying is reasonable.
    Chip,
}

/// [`cert`] with the failure reason. `cert` is kept as the `Option` shim for existing callers.
///
/// Read from the chip once per boot, then from the boot's cache ([`cert_cache`]): the certificate is the chip's public
/// one, the same on every read and sent as-is to every phone (the private key never leaves the chip, and signing still
/// goes to it every time). The tunnel's first read of a session failed with a chip NAK and cost a retry in 23 of 23
/// sessions (the Mac's 13 Nov brief, item 2). Since A4 the same cache serves btd's Bluetooth authentication
/// (`wireless/src/mfi_local.rs`) and carplayd's auth-setup (`LocalMfiSigner`), and btd fills it as a host session
/// begins.
///
/// Only one DER SEQUENCE is cached or served from the cache ([`cert_cache::is_certificate`]): one read that completed
/// with bad bytes would otherwise go to every phone for the rest of the boot. A read that isn't one still goes to this
/// phone, as before the cache, and the next session reads the chip again (the Mac's 2.6.0 review, CS-3). The cache is
/// looked at again once the lock is held, as another chip user may have filled it while this one waited, and written
/// after the lock is let go, so the lock is held no longer than the chip read.
pub fn try_cert() -> Result<Vec<u8>, MfiError> {
    let f = cert_cache::fetch(CERT_CACHE, MfiLock::acquire, || cert_locked().ok_or(MfiError::Chip))?;
    let cached = matches!(f.from, cert_cache::Source::Cache);
    note_cert(CertTime { took: f.under, cached });
    if f.from == (cert_cache::Source::Chip { cached: false }) {
        eprintln!("[mfi-i2c-local] the certificate read ({} B) is not one DER SEQUENCE: sent, not cached", f.cert.len());
    }
    Ok(f.cert)
}

/// Forget the cached certificate, so the next session reads it from the chip: a phone failed the authentication. True
/// when one was cached, and is gone.
pub fn forget_cert() -> bool {
    cert_cache::forget(CERT_CACHE)
}

/// The boot's cached MFi certificate, one file every chip user that sends the certificate shares: the tunnel here, btd's
/// Bluetooth authentication and carplayd's auth-setup (A4, MFI-CACHE-SCOPE). The rules are the tunnel's since 2.6.0:
/// - only one DER certificate ([`is_certificate`]) is cached or served; a file that isn't one is removed when read;
/// - a read from the chip that isn't one is sent to that phone once and not cached, so the next session reads again;
/// - a phone reporting a failed authentication drops it ([`forget`]), so the next session of any user reads the chip;
/// - signing never comes from here: it always goes to the chip.
///
/// tmpfs, so a reboot clears it. Written aside and renamed, so a reader in another process never sees half of it.
pub mod cert_cache {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    /// Where it lives (`/tmp`, tmpfs).
    pub const PATH: &str = "/tmp/carplay_mfi_cert.der";

    /// Where a certificate came from ([`fetch`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Source {
        /// The boot's cache: no lock, no chip.
        Cache,
        /// The chip, under the lock; whether it was then cached (only one DER certificate is).
        Chip { cached: bool },
    }

    /// A certificate for one authentication, where it came from, its time under the lock (zero for the cache) and the
    /// wait for the lock.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Fetched {
        pub cert: Vec<u8>,
        pub from: Source,
        pub under: Duration,
        pub waited: Duration,
    }

    /// The certificate by the cache's rules, for every user alike: the one cached at `path`; else, holding the chip lock
    /// (`lock`, the user's own: its guard is held until the read is done) and once the cache has been looked at again
    /// (another user may have filled it while this one waited), the chip's (`read`). That read is cached after the lock
    /// is let go when it is one DER certificate, so the lock is held no longer than the chip read; one that isn't is
    /// returned (sent once) and not cached. No lock, or no read: the user's error, and nothing cached.
    pub fn fetch<L, E>(
        path: &str,
        lock: impl FnOnce() -> Result<L, E>,
        read: impl FnOnce() -> Result<Vec<u8>, E>,
    ) -> Result<Fetched, E> {
        let cached = |cert| Fetched { cert, from: Source::Cache, under: Duration::ZERO, waited: Duration::ZERO };
        if let Some(c) = self::read(path) {
            return Ok(cached(c));
        }
        let asked = Instant::now();
        let guard = lock()?;
        let held = Instant::now();
        if let Some(c) = self::read(path) {
            return Ok(cached(c));
        }
        let cert = read()?;
        let under = held.elapsed();
        drop(guard);
        let cached = keep(path, &cert);
        Ok(Fetched { cert, from: Source::Chip { cached }, under, waited: held.duration_since(asked) })
    }

    /// Whether `c` is one DER SEQUENCE with a two-byte length, as the chip's certificate is (`30 82 hi lo`, 945 bytes
    /// on this unit's 2.0C: docs/ops/captures/README.md), its length the rest of the bytes exactly, within the chip's
    /// 2048.
    pub fn is_certificate(c: &[u8]) -> bool {
        match c {
            [0x30, 0x82, hi, lo, ..] => c.len() <= 2048 && usize::from(u16::from_be_bytes([*hi, *lo])) + 4 == c.len(),
            _ => false,
        }
    }

    /// The certificate cached at `path`, when it is one; a file that isn't is removed.
    pub fn read(path: &str) -> Option<Vec<u8>> {
        let c = std::fs::read(path).ok()?;
        if is_certificate(&c) {
            return Some(c);
        }
        let _ = std::fs::remove_file(path);
        None
    }

    /// Cache `c` at `path`, when it is one certificate: written aside (a name of this write's own, as two threads of a
    /// process can write at once) and renamed. Whether it was cached.
    pub fn keep(path: &str, c: &[u8]) -> bool {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        if !is_certificate(c) {
            return false;
        }
        let tmp = format!("{path}.{}.{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
        if std::fs::write(&tmp, c).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        true
    }

    /// Forget the certificate cached at `path`. True when one was cached, and is gone.
    pub fn forget(path: &str) -> bool {
        std::fs::remove_file(path).is_ok()
    }
}

/// The certificate the tunnel caches and reads ([`cert_cache::PATH`]). `cfg(test)`: a path of its own.
#[cfg(not(test))]
const CERT_CACHE: &str = cert_cache::PATH;
#[cfg(test)]
const CERT_CACHE: &str = "/tmp/mfi_i2c_local_selftest_cert.der";

/// [`sign`] with the failure reason.
pub fn try_sign(chal: &[u8]) -> Result<Vec<u8>, MfiError> {
    let asked = Instant::now();
    let lock = MfiLock::acquire()?;
    let held = Instant::now();
    let (sig, ready) = sign_locked(chal).ok_or(MfiError::Chip)?;
    let under = held.elapsed();
    drop(lock); // released as the sequence ends, as before; the time is told after
    let cert = LAST_CERT.lock().unwrap_or_else(|p| p.into_inner()).take();
    eprintln!("{}", tunnel_line(ready, under, held.duration_since(asked), cert));
    Ok(sig)
}

// ---- Instrumentation: how long the chip took. The tunnel's signature comes after the first frame, so it is a log line
// of its own, not one of the connect timeline's values (box_common::timeline). ----------------------------------------

/// A certificate read's time: under the lock from the chip, or from the boot's cache ([`CERT_CACHE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CertTime {
    took: Duration,
    cached: bool,
}

/// The last certificate read's, for the signature's line after it.
static LAST_CERT: std::sync::Mutex<Option<CertTime>> = std::sync::Mutex::new(None);

fn note_cert(t: CertTime) {
    *LAST_CERT.lock().unwrap_or_else(|p| p.into_inner()) = Some(t);
}

/// The tunnel's line for a signature: its time under the lock, the chip's from go to status ready (the fixed 100 ms
/// first wait included) and the wait for the lock; then the certificate read before it, when there was one.
fn tunnel_line(ready: Duration, under: Duration, waited: Duration, cert: Option<CertTime>) -> String {
    let cert = match cert {
        Some(c) => format!("{} ms ({})", c.took.as_millis(), if c.cached { "cached" } else { "chip" }),
        None => "-".to_string(),
    };
    format!(
        "[mfi] tunnel sign {} ms (chip {} ms from go to ready, {} ms waiting for the lock), cert {cert}",
        under.as_millis(),
        ready.as_millis(),
        waited.as_millis()
    )
}

pub fn cert() -> Option<Vec<u8>> {
    try_cert().ok()
}

pub fn sign(chal: &[u8]) -> Option<Vec<u8>> {
    try_sign(chal).ok()
}

/// Fetch the accessory certificate. CopyCertificate: reg `0x30` len (2 BE), reg `0x31` cert.
fn cert_locked() -> Option<Vec<u8>> {
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

/// Sign a challenge digest. CreateSignature: write challenge (`0x20` len, `0x21` data), go
/// (`0x10`=1), poll status bit4, read `0x11` len / `0x12` sig. With it, the chip's time from go to ready.
fn sign_locked(chal: &[u8]) -> Option<(Vec<u8>, Duration)> {
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
    // Bounded by WALL CLOCK, not iteration count. `for _ in 0..200` with a 10 ms sleep looks like
    // ~2.1 s, and is — but only when every status read succeeds. `i2c_rd` is itself `for _ in 0..5`
    // with a 5 ms sleep, so under chip NAK each iteration costs ~35 ms and the loop ran ~7.1 s. That
    // is the case this poll exists for, and it blew straight through the 10 s ceilings that
    // `carplayd`'s `LocalMfiSigner` and `MfiLock::acquire` were sized against — while holding the
    // chip lock AND, upstream, the tunnel's SESSION mutex.
    const SIGN_POLL_DEADLINE: std::time::Duration = std::time::Duration::from_millis(2500);
    let poll_start = std::time::Instant::now();
    let mut done = false;
    while poll_start.elapsed() < SIGN_POLL_DEADLINE {
        let mut st = [0u8; 1];
        if i2c_rd(0x10, &mut st) && (st[0] & 0x10) != 0 {
            if st[0] != 0x10 {
                eprintln!("[mfi-i2c-local] sign status 0x{:02x} (expected 0x10)", st[0]);
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
    Some((sig, ready))
}

#[cfg(test)]
mod tests {
    use super::cert_cache::{forget, is_certificate, keep, read};
    use super::{forget_cert, tunnel_line, CertTime, CERT_CACHE};
    use std::time::Duration;

    #[test]
    fn the_tunnel_line_tells_the_sign_and_the_cert_before_it() {
        let ms = Duration::from_millis;
        assert_eq!(
            tunnel_line(ms(298), ms(305), ms(0), Some(CertTime { took: ms(41), cached: false })),
            "[mfi] tunnel sign 305 ms (chip 298 ms from go to ready, 0 ms waiting for the lock), cert 41 ms (chip)"
        );
        assert_eq!(
            tunnel_line(ms(298), ms(305), ms(7), Some(CertTime { took: ms(0), cached: true })),
            "[mfi] tunnel sign 305 ms (chip 298 ms from go to ready, 7 ms waiting for the lock), cert 0 ms (cached)"
        );
        assert_eq!(
            tunnel_line(ms(298), ms(305), ms(0), None),
            "[mfi] tunnel sign 305 ms (chip 298 ms from go to ready, 0 ms waiting for the lock), cert -"
        );
    }

    /// A certificate's shape: `30 82` and a length that is the rest of it.
    fn der(body: usize) -> Vec<u8> {
        let mut c = vec![0x30, 0x82, (body >> 8) as u8, body as u8];
        c.extend(std::iter::repeat(0xa5).take(body));
        c
    }

    /// The Mac's 2.6.0 review, CS-3: only one whole DER SEQUENCE with a two-byte length passes.
    #[test]
    fn only_one_whole_der_sequence_is_a_certificate() {
        assert!(is_certificate(&der(941)), "the 2.0C's 945 bytes, 30 82 03 ad");
        assert!(is_certificate(&der(2044)), "the chip's 2048 at most");
        assert!(!is_certificate(&der(2045)), "past the chip's 2048");
        let mut c = der(941);
        c.push(0);
        assert!(!is_certificate(&c), "a byte past its length");
        assert!(!is_certificate(&c[..900]), "cut short");
        c = der(941);
        c[0] = 0x31;
        assert!(!is_certificate(&c), "not a SEQUENCE");
        c = der(941);
        c[1] = 0x81;
        assert!(!is_certificate(&c), "not a two-byte length");
        assert!(!is_certificate(&[0u8; 945]), "a read of zeros");
        assert!(!is_certificate(&[0xffu8; 945]), "a read of ones");
        assert!(!is_certificate(&[0x30u8, 0x82, 0x00]), "shorter than its header");
        assert!(!is_certificate(&[]), "empty");
    }

    /// The cache holds and serves only a certificate; one that isn't is removed when read, and a read that isn't one is
    /// never written.
    #[test]
    fn the_cache_serves_only_a_certificate() {
        let path = std::env::temp_dir().join(format!("mfi_i2c_local_cache_{}.der", std::process::id()));
        let p = path.to_str().unwrap();
        assert!(keep(p, &der(941)));
        assert_eq!(read(p), Some(der(941)));
        let mut bad = der(941);
        bad.truncate(600);
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(read(p), None, "a cache that isn't one is not served");
        assert!(!path.exists(), "and is removed, so the next read goes to the chip");
        assert_eq!(read(p), None, "none cached");
        assert!(!keep(p, &bad), "a read that isn't one is sent once, never cached");
        assert!(!path.exists());
        let tmps = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("mfi_i2c_local_cache_{}.der.", std::process::id())))
            .count();
        assert_eq!(tmps, 0, "no file written aside is left");
    }

    fn scratch(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("mfi_i2c_local_fetch_{tag}_{}.der", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p.to_str().unwrap().to_string()
    }

    /// A4: the rules every user shares. The first fetch of a boot reads the chip under the lock and caches the read;
    /// every later one comes from the cache, with neither the lock nor the chip.
    #[test]
    fn a_fetch_reads_the_chip_once_then_serves_the_cache() {
        use super::cert_cache::{fetch, Source};
        let path = scratch("once");
        let locks = std::cell::Cell::new(0);
        let lock = || -> Result<(), ()> {
            locks.set(locks.get() + 1);
            Ok(())
        };
        let first = fetch(&path, lock, || Ok(der(941))).unwrap();
        assert_eq!(first.from, Source::Chip { cached: true });
        let second = fetch(&path, lock, || -> Result<Vec<u8>, ()> { panic!("no chip read once cached") }).unwrap();
        assert_eq!((second.cert, second.from, second.under), (der(941), Source::Cache, Duration::ZERO));
        assert_eq!(locks.get(), 1, "the lock for the chip read only");
        let _ = std::fs::remove_file(&path);
    }

    /// CS-3: a read that isn't one DER certificate is sent once and not cached; a cache another user filled while this
    /// one waited is served without the chip; no lock or no read caches nothing.
    #[test]
    fn a_fetch_caches_only_a_certificate_and_looks_again_under_the_lock() {
        use super::cert_cache::{fetch, keep, Source};
        let path = scratch("rules");
        let bad = fetch(&path, || Ok::<_, ()>(()), || Ok(vec![0u8; 945])).unwrap();
        assert_eq!((bad.cert, bad.from), (vec![0u8; 945], Source::Chip { cached: false }));
        assert!(!std::path::Path::new(&path).exists());
        let filled = fetch(
            &path,
            || {
                assert!(keep(&path, &der(941)), "another user caches it while this one waits");
                Ok::<_, ()>(())
            },
            || panic!("filled meanwhile: no chip read"),
        )
        .unwrap();
        assert_eq!(filled.from, Source::Cache);
        let _ = std::fs::remove_file(&path);
        assert_eq!(fetch(&path, || Err::<(), _>("busy"), || Ok(der(941))), Err("busy"));
        assert_eq!(fetch(&path, || Ok::<_, &str>(()), || Err("nak")), Err("nak"));
        assert!(!std::path::Path::new(&path).exists(), "nothing cached by either");
    }

    /// R1-3 (the Mac's round 2): forgetting says whether there was a cached certificate to forget.
    #[test]
    fn forgetting_says_whether_one_was_cached() {
        assert!(keep(CERT_CACHE, &der(941)));
        assert!(forget_cert(), "one was cached");
        assert!(!std::path::Path::new(CERT_CACHE).exists());
        assert!(!forget_cert(), "none left to forget");
        assert!(!forget(CERT_CACHE));
    }
}
