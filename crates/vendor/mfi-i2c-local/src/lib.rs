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
/// Read from the chip once per boot, then from [`CERT_CACHE`]: the certificate is the chip's public one, the same on
/// every read and sent as-is to every phone (the private key never leaves the chip, and signing still goes to it
/// every time). The tunnel's first read of a session failed with a chip NAK and cost a retry in 23 of 23 sessions
/// (the Mac's 13 Nov brief, item 2).
///
/// Only one DER SEQUENCE is cached or served from the cache ([`is_der_sequence`]): one read that completed with bad
/// bytes would otherwise go to every phone for the rest of the boot. A read that isn't one still goes to this phone,
/// as before the cache, and the next session reads the chip again (the Mac's 2.6.0 review, CS-3).
pub fn try_cert() -> Result<Vec<u8>, MfiError> {
    if let Some(c) = read_cert_cache(CERT_CACHE) {
        return Ok(c);
    }
    let _lock = MfiLock::acquire()?;
    let c = cert_locked().ok_or(MfiError::Chip)?;
    if is_der_sequence(&c) {
        write_cert_cache(CERT_CACHE, &c);
    } else {
        eprintln!("[mfi-i2c-local] the certificate read ({} B) is not one DER SEQUENCE: sent, not cached", c.len());
    }
    Ok(c)
}

/// Forget the cached certificate, so the next session reads it from the chip: a phone failed the authentication. True
/// when one was cached, and is gone.
pub fn forget_cert() -> bool {
    std::fs::remove_file(CERT_CACHE).is_ok()
}

/// Whether `c` is one DER SEQUENCE with a two-byte length, as the chip's certificate is (`30 82 hi lo`, 945 bytes on
/// this unit's 2.0C: docs/ops/captures/README.md), its length the rest of the bytes exactly, within the chip's 2048.
fn is_der_sequence(c: &[u8]) -> bool {
    match c {
        [0x30, 0x82, hi, lo, ..] => c.len() <= 2048 && usize::from(u16::from_be_bytes([*hi, *lo])) + 4 == c.len(),
        _ => false,
    }
}

/// The certificate cached at `path`, when it is one DER SEQUENCE; one that isn't is removed.
fn read_cert_cache(path: &str) -> Option<Vec<u8>> {
    let c = std::fs::read(path).ok()?;
    if is_der_sequence(&c) {
        return Some(c);
    }
    let _ = std::fs::remove_file(path);
    None
}

/// Cache `c` at `path`: written aside and renamed, so a reader never sees half of it.
fn write_cert_cache(path: &str, c: &[u8]) {
    let tmp = format!("{path}.{}", std::process::id());
    if std::fs::write(&tmp, c).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The certificate as last read from the chip this boot (tmpfs). `cfg(test)`: a path of its own.
#[cfg(not(test))]
const CERT_CACHE: &str = "/tmp/carplay_mfi_cert.der";
#[cfg(test)]
const CERT_CACHE: &str = "/tmp/mfi_i2c_local_selftest_cert.der";

/// [`sign`] with the failure reason.
pub fn try_sign(chal: &[u8]) -> Result<Vec<u8>, MfiError> {
    let _lock = MfiLock::acquire()?;
    sign_locked(chal).ok_or(MfiError::Chip)
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
/// (`0x10`=1), poll status bit4, read `0x11` len / `0x12` sig.
fn sign_locked(chal: &[u8]) -> Option<Vec<u8>> {
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
    Some(sig)
}

#[cfg(test)]
mod tests {
    use super::{forget_cert, is_der_sequence, read_cert_cache, write_cert_cache, CERT_CACHE};

    /// A certificate's shape: `30 82` and a length that is the rest of it.
    fn der(body: usize) -> Vec<u8> {
        let mut c = vec![0x30, 0x82, (body >> 8) as u8, body as u8];
        c.extend(std::iter::repeat(0xa5).take(body));
        c
    }

    /// The Mac's 2.6.0 review, CS-3: only one whole DER SEQUENCE with a two-byte length passes.
    #[test]
    fn only_one_whole_der_sequence_is_a_certificate() {
        assert!(is_der_sequence(&der(941)), "the 2.0C's 945 bytes, 30 82 03 ad");
        assert!(is_der_sequence(&der(2044)), "the chip's 2048 at most");
        assert!(!is_der_sequence(&der(2045)), "past the chip's 2048");
        let mut c = der(941);
        c.push(0);
        assert!(!is_der_sequence(&c), "a byte past its length");
        assert!(!is_der_sequence(&c[..900]), "cut short");
        c = der(941);
        c[0] = 0x31;
        assert!(!is_der_sequence(&c), "not a SEQUENCE");
        c = der(941);
        c[1] = 0x81;
        assert!(!is_der_sequence(&c), "not a two-byte length");
        assert!(!is_der_sequence(&[0u8; 945]), "a read of zeros");
        assert!(!is_der_sequence(&[0xffu8; 945]), "a read of ones");
        assert!(!is_der_sequence(&[0x30u8, 0x82, 0x00]), "shorter than its header");
        assert!(!is_der_sequence(&[]), "empty");
    }

    /// The cache holds and serves only a certificate; one that isn't is removed when read.
    #[test]
    fn the_cache_serves_only_a_certificate() {
        let path = std::env::temp_dir().join(format!("mfi_i2c_local_cache_{}.der", std::process::id()));
        let p = path.to_str().unwrap();
        write_cert_cache(p, &der(941));
        assert_eq!(read_cert_cache(p), Some(der(941)));
        let mut bad = der(941);
        bad.truncate(600);
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(read_cert_cache(p), None, "a cache that isn't one is not served");
        assert!(!path.exists(), "and is removed, so the next read goes to the chip");
        assert_eq!(read_cert_cache(p), None, "none cached");
    }

    /// R1-3 (the Mac's round 2): forgetting says whether there was a cached certificate to forget.
    #[test]
    fn forgetting_says_whether_one_was_cached() {
        write_cert_cache(CERT_CACHE, &der(941));
        assert!(forget_cert(), "one was cached");
        assert!(!std::path::Path::new(CERT_CACHE).exists());
        assert!(!forget_cert(), "none left to forget");
    }
}
