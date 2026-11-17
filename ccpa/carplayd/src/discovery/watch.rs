//! What wakes discovery besides mDNS (A2): btd's name file changing, and the host's links and addresses changing.
//!
//! Since A2 carplayd starts as btd's host session begins, before any phone is named and often before the AP has its
//! address. Discovery used to read the name once a second; it now sleeps until something happens, and these two
//! threads tell it: inotify on the name file's directory (btd writes the file aside and renames it into place, so a
//! watch on the file itself would be left on the old inode), and a NETLINK_ROUTE socket for new addresses and link
//! changes. Each sends one ping on discovery's wake channel; what changed is read afresh by discovery (the file, the
//! interfaces), so a ping coalesced with another loses nothing. Both block in a read and cost nothing while nothing
//! changes; neither polls.
//!
//! Off Linux (a developer's host, the tests) there is neither: the watches report that they did not start, and
//! discovery falls back to what it does without them (`Net`, `NAME_POLL`).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Discovery's wake channel: one ping per change.
pub type Wake = flume::Sender<()>;

/// Watch `file` through its directory: a ping at each write closed, rename into place and removal of that name. False
/// when the watch could not be set up.
pub fn name_file(file: &Path, wake: Wake) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::name_file(file, wake)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, wake);
        false
    }
}

/// Watch the host's links and addresses: `dirty` set and a ping at each new address and each link change (a link coming
/// up keeps the address it had, and only that is seen then). False when the socket could not be opened.
pub fn addresses(dirty: Arc<AtomicBool>, wake: Wake) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::addresses(dirty, wake)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (dirty, wake);
        false
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Wake;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// The netlink route groups for links and IPv4/IPv6 addresses (`rtnetlink.h`), and its message types.
    const RTMGRP_LINK: u32 = 0x1;
    const RTMGRP_IPV4_IFADDR: u32 = 0x10;
    const RTMGRP_IPV6_IFADDR: u32 = 0x100;
    const RTM_NEWLINK: u16 = 16;
    const RTM_NEWADDR: u16 = 20;

    /// `struct sockaddr_nl` (`linux/netlink.h`).
    #[repr(C)]
    struct SockaddrNl {
        nl_family: u16,
        nl_pad: u16,
        nl_pid: u32,
        nl_groups: u32,
    }

    pub(super) fn name_file(file: &Path, wake: Wake) -> bool {
        let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else { return false };
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        let Ok(dir_c) = CString::new(dir.as_os_str().as_bytes()) else { return false };
        let name = name.as_bytes().to_vec();
        // SAFETY: a plain inotify_init1; the descriptor is owned below.
        let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if raw < 0 {
            eprintln!("[watch] inotify_init1: {}", std::io::Error::last_os_error());
            return false;
        }
        // SAFETY: the descriptor inotify_init1 just returned.
        let ino = unsafe { OwnedFd::from_raw_fd(raw) };
        // btd's renames into place, a removal, and a write closed (a bench writing the file by hand). Nothing else in
        // the directory wakes this thread but these three, and it reads each event's name before it pings.
        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_DELETE;
        // SAFETY: a NUL-terminated path for inotify_add_watch on our own descriptor.
        if unsafe { libc::inotify_add_watch(ino.as_raw_fd(), dir_c.as_ptr(), mask) } < 0 {
            eprintln!("[watch] inotify_add_watch {}: {}", dir.display(), std::io::Error::last_os_error());
            return false;
        }
        std::thread::Builder::new()
            .name("name-watch".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    // SAFETY: a blocking read into our own buffer from our own descriptor.
                    let n = unsafe { libc::read(ino.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                    if n < 0 {
                        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        eprintln!("[watch] the name file's watch ended: {}", std::io::Error::last_os_error());
                        return;
                    }
                    if names_it(&buf[..n as usize], &name) && wake.send(()).is_err() {
                        return; // discovery is gone
                    }
                }
            })
            .is_ok()
    }

    /// Whether a buffer of inotify events holds one for `name`, or an overflow (events lost: look again).
    fn names_it(events: &[u8], name: &[u8]) -> bool {
        let head = std::mem::size_of::<libc::inotify_event>();
        let mut off = 0;
        while off + head <= events.len() {
            // SAFETY: `head` bytes are there; read_unaligned copies them out of the byte buffer.
            let ev: libc::inotify_event = unsafe { std::ptr::read_unaligned(events[off..].as_ptr() as *const _) };
            let end = (off + head + ev.len as usize).min(events.len());
            let ev_name = events[off + head..end].split(|&b| b == 0).next().unwrap_or(&[]);
            if ev.mask & libc::IN_Q_OVERFLOW != 0 || ev_name == name {
                return true;
            }
            off = end;
        }
        false
    }

    pub(super) fn addresses(dirty: Arc<AtomicBool>, wake: Wake) -> bool {
        // SAFETY: a plain socket(2); the descriptor is owned below.
        let raw = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, 0 /* NETLINK_ROUTE */) };
        if raw < 0 {
            eprintln!("[watch] netlink socket: {}", std::io::Error::last_os_error());
            return false;
        }
        // SAFETY: the descriptor socket(2) just returned.
        let sock = unsafe { OwnedFd::from_raw_fd(raw) };
        let sa = SockaddrNl {
            nl_family: libc::AF_NETLINK as u16,
            nl_pad: 0,
            nl_pid: 0,
            nl_groups: RTMGRP_LINK | RTMGRP_IPV4_IFADDR | RTMGRP_IPV6_IFADDR,
        };
        // SAFETY: a sockaddr_nl of our own, its size given.
        let bound = unsafe {
            libc::bind(
                sock.as_raw_fd(),
                &sa as *const SockaddrNl as *const libc::sockaddr,
                std::mem::size_of::<SockaddrNl>() as libc::socklen_t,
            )
        };
        if bound != 0 {
            eprintln!("[watch] netlink bind: {}", std::io::Error::last_os_error());
            return false;
        }
        std::thread::Builder::new()
            .name("addr-watch".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    // SAFETY: a blocking recv into our own buffer from our own socket.
                    let n = unsafe { libc::recv(sock.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
                    let changed = if n < 0 {
                        match std::io::Error::last_os_error().raw_os_error() {
                            Some(libc::EINTR) => continue,
                            // The socket's buffer overran: messages were lost, so look again.
                            Some(libc::ENOBUFS) => true,
                            _ => {
                                eprintln!("[watch] the address watch ended: {}", std::io::Error::last_os_error());
                                return;
                            }
                        }
                    } else {
                        news(&buf[..n as usize])
                    };
                    if changed {
                        dirty.store(true, Ordering::Release);
                        if wake.send(()).is_err() {
                            return;
                        }
                    }
                }
            })
            .is_ok()
    }

    /// Whether a buffer of netlink messages holds a new address or a link change.
    fn news(msgs: &[u8]) -> bool {
        let mut off = 0;
        while off + 16 <= msgs.len() {
            let len = u32::from_ne_bytes([msgs[off], msgs[off + 1], msgs[off + 2], msgs[off + 3]]) as usize;
            let kind = u16::from_ne_bytes([msgs[off + 4], msgs[off + 5]]);
            if kind == RTM_NEWADDR || kind == RTM_NEWLINK {
                return true;
            }
            if len < 16 {
                return false;
            }
            off += (len + 3) & !3;
        }
        false
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn an_event_for_the_name_file_wakes_and_others_do_not() {
            let head = std::mem::size_of::<libc::inotify_event>();
            let event = |name: &[u8], mask: u32| {
                let padded = (name.len() + 1).div_ceil(16) * 16;
                let ev = libc::inotify_event { wd: 1, mask, cookie: 0, len: padded as u32 };
                // SAFETY: the event's bytes, as the kernel writes them.
                let mut b = unsafe { std::slice::from_raw_parts(&ev as *const _ as *const u8, head) }.to_vec();
                b.extend_from_slice(name);
                b.resize(head + padded, 0);
                b
            };
            let name = b"carplay_expect_phone";
            let mut two = event(b"carplay_expect_phone.tmp", libc::IN_CLOSE_WRITE);
            assert!(!names_it(&two, name), "btd's file written aside is not the name file");
            two.extend(event(name, libc::IN_MOVED_TO));
            assert!(names_it(&two, name), "renamed into place");
            assert!(names_it(&event(b"", libc::IN_Q_OVERFLOW), name), "events lost: look again");
            assert!(!names_it(&event(b"startup_marks", libc::IN_CLOSE_WRITE), name));
        }

        #[test]
        fn a_new_address_or_a_link_change_is_news() {
            let msg = |kind: u16| {
                let mut b = 16u32.to_ne_bytes().to_vec();
                b.extend_from_slice(&kind.to_ne_bytes());
                b.resize(16, 0);
                b
            };
            assert!(news(&msg(RTM_NEWADDR)));
            assert!(news(&msg(RTM_NEWLINK)));
            assert!(!news(&msg(21)), "RTM_DELADDR: nothing new to announce on");
            let mut two = msg(21);
            two.extend(msg(RTM_NEWADDR));
            assert!(news(&two), "the second of two");
        }
    }
}
