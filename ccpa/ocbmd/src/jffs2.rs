//! `ocbmd jffs2-extract MTD2 OUT`: the stock rootfs out of the flash dump the one-file install wrote to
//! its stick (OCBM_BACKUP/<adapter>/mtd2.bin), ON the adapter, so a final adapter can go back to stock
//! from that stick with no computer (ccpa/installer/ocbm_restore.sh; Scott, 3 Nov). A port of
//! tools/jffs2_extract.py, which the computer route uses, with the same rules:
//!
//! Carlinkit's 3.14.52 kernel stores most data nodes with compression type 22: the bytes 0x32 and 0x60
//! are swapped in the payload, which is then plain zlib, and only a complete stream whose adler32
//! matches counts. A few nodes are rtime (2); 0 (none), 1 (zeros) and 6 (zlib) are the kernel's own.
//! Every node's header, node and data CRCs are checked, and a node that fails one is ignored, as the
//! kernel ignores it; obsolete nodes never match. The highest version of each directory entry wins (an
//! entry naming inode 0 is a deletion), and each file's data nodes are applied in version order, a node's
//! size truncating what lies beyond it. A file is written only when every data node it needs decodes to
//! exactly its declared length; anything else is BAD and the exit status is 1. Names with "/", ".", ".."
//! or NUL are refused, and nothing is written through a link or outside OUT/files. Device nodes and FIFOs
//! are reported, not made.
//!
//! OUT/files is the rootfs; OUT/report.tsv is jffs2_extract.py's report (STATUS TYPE MODE UID GID SIZE MD5
//! PATH), except that MD5 is '-': nothing on the adapter reads it, and the restore compares the files
//! themselves. Run as root (on the adapter), each entry also gets the image's owner, and OUT/links.tsv
//! lists each link's owner (UID TAB GID TAB PATH, PATH relative), which stock_restore_box.sh reads as
//! LINKS. The output holds the adapter's Wi-Fi passphrase and Bluetooth link keys: it goes to /tmp only.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAGIC: u16 = 0x1985;
const DIRENT: u16 = 0xE001; // obsolete nodes have the 0x2000 bit cleared, so never match
const INODE: u16 = 0xE002;
const NONE: u8 = 0;
const ZERO: u8 = 1;
const RTIME: u8 = 2;
const ZLIB: u8 = 6;
const AESMYKEY: u8 = 22;
const ROOT_INO: u32 = 1;
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

/// The subcommand, when `args` asks for it: the exit status. None: ocbmd runs as the daemon.
pub fn cli(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some("jffs2-extract") {
        return None;
    }
    let (Some(img), Some(out)) = (args.get(2), args.get(3)) else {
        eprintln!("usage: ocbmd jffs2-extract MTD2_BIN OUT_DIR");
        return Some(2);
    };
    Some(match fs::read(img) {
        Ok(bytes) => extract(&bytes, Path::new(out), unsafe { libc::geteuid() } == 0),
        Err(e) => {
            eprintln!("jffs2-extract: cannot read {img}: {e}");
            2
        }
    })
}

/// JFFS2's crc32: Linux crc32_le seeded with 0, no final inversion.
pub(crate) fn jcrc(b: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &x in b {
        crc ^= x as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    crc
}

/// fs/jffs2/compr_rtime.c, decompress: a byte, then how many to repeat from its last position.
fn rtime(src: &[u8], dsize: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(dsize);
    let mut last = [0usize; 256];
    let mut pos = 0;
    while out.len() < dsize {
        let (value, repeat) = (*src.get(pos)?, *src.get(pos + 1)? as usize);
        pos += 2;
        out.push(value);
        let mut back = last[value as usize];
        last[value as usize] = out.len();
        for _ in 0..repeat {
            let b = *out.get(back)?; // byte by byte: the copy may overlap what it writes
            out.push(b);
            back += 1;
        }
    }
    Some(out)
}

fn decode(compr: u8, data: &[u8], dsize: usize) -> Option<Vec<u8>> {
    use miniz_oxide::inflate::{decompress_to_vec_with_limit, decompress_to_vec_zlib_with_limit};
    let out = match compr {
        NONE => data.get(..dsize)?.to_vec(),
        ZERO => vec![0; dsize],
        RTIME => rtime(data, dsize)?,
        // fs/jffs2/compr_zlib.c: a zlib header when the first two bytes make one, else raw deflate.
        ZLIB if data.len() >= 2 && data[0] & 0x0F == 8 && ((data[0] as u32) << 8 | data[1] as u32) % 31 == 0 => {
            decompress_to_vec_zlib_with_limit(data, dsize).ok()?
        }
        ZLIB => decompress_to_vec_with_limit(data, dsize).ok()?,
        AESMYKEY => {
            let swapped: Vec<u8> = data
                .iter()
                .map(|&b| match b {
                    0x32 => 0x60,
                    0x60 => 0x32,
                    b => b,
                })
                .collect();
            decompress_to_vec_zlib_with_limit(&swapped, dsize).ok()? // a complete stream, adler32 checked
        }
        _ => return None,
    };
    (out.len() == dsize).then_some(out)
}

#[derive(Clone)]
struct Node {
    ver: u32,
    off: usize,
    dsize: usize,
    isize: usize,
    compr: u8,
    data: std::ops::Range<usize>,
    mode: u32,
    uid: u16,
    gid: u16,
    mtime: u32,
}

#[derive(Default)]
struct Scan {
    dirents: HashMap<(u32, Vec<u8>), (u32, u32)>, // (pino, name) -> (version, ino)
    inodes: HashMap<u32, Vec<Node>>,
    n_dirent: usize,
    n_inode: usize,
    bad_crc: usize,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Every valid dirent and inode node.
fn scan(img: &[u8]) -> Scan {
    let mut s = Scan::default();
    let (mut i, n) = (0usize, img.len());
    while i + 12 <= n {
        let (magic, ntype, totlen, hcrc) = (u16_at(img, i), u16_at(img, i + 2), u32_at(img, i + 4) as usize, u32_at(img, i + 8));
        if magic != MAGIC || totlen < 12 || i + totlen > n || jcrc(&img[i..i + 8]) != hcrc {
            i += 4;
            continue;
        }
        if ntype == DIRENT && totlen >= 40 {
            let (pino, ver, ino, nsize) = (u32_at(img, i + 12), u32_at(img, i + 16), u32_at(img, i + 20), img[i + 28] as usize);
            let (ncrc, namecrc) = (u32_at(img, i + 32), u32_at(img, i + 36));
            let name = &img[i + 40..(i + 40 + nsize).min(n)];
            if jcrc(&img[i..i + 32]) != ncrc || 40 + nsize > totlen || jcrc(name) != namecrc {
                s.bad_crc += 1;
            } else {
                s.n_dirent += 1;
                let k = (pino, name.to_vec());
                if !matches!(s.dirents.get(&k), Some(&(v, _)) if v > ver) {
                    s.dirents.insert(k, (ver, ino));
                }
            }
        } else if ntype == INODE && totlen >= 68 {
            let csize = u32_at(img, i + 48) as usize;
            let data = i + 68..(i + 68 + csize).min(n);
            if jcrc(&img[i..i + 60]) != u32_at(img, i + 64) || 68 + csize > totlen || jcrc(&img[data.clone()]) != u32_at(img, i + 60) {
                s.bad_crc += 1;
            } else {
                s.n_inode += 1;
                let node = Node {
                    ver: u32_at(img, i + 16),
                    mode: u32_at(img, i + 20),
                    uid: u16_at(img, i + 24),
                    gid: u16_at(img, i + 26),
                    isize: u32_at(img, i + 28) as usize,
                    mtime: u32_at(img, i + 36),
                    off: u32_at(img, i + 44) as usize,
                    dsize: u32_at(img, i + 52) as usize,
                    compr: img[i + 56],
                    data,
                };
                s.inodes.entry(u32_at(img, i + 12)).or_default().push(node);
            }
        }
        i += (totlen + 3) & !3;
    }
    s
}

/// ino -> paths: a walk from the root inode; unsafe names and directory loops are skipped.
fn paths_of(dirents: &HashMap<(u32, Vec<u8>), (u32, u32)>) -> (BTreeMap<Vec<u8>, u32>, Vec<Vec<u8>>) {
    let mut children: HashMap<u32, Vec<(&[u8], u32)>> = HashMap::new();
    for ((pino, name), &(_v, ino)) in dirents {
        if ino != 0 {
            children.entry(*pino).or_default().push((name, ino));
        }
    }
    let (mut paths, mut refused) = (BTreeMap::new(), Vec::new());
    let mut seen: HashSet<u32> = [ROOT_INO].into();
    let mut stack = vec![(ROOT_INO, Vec::new())];
    while let Some((pino, prefix)) = stack.pop() {
        let mut kids = children.get(&pino).cloned().unwrap_or_default();
        kids.sort();
        for (name, ino) in kids {
            let mut p = prefix.clone();
            p.push(b'/');
            p.extend_from_slice(name);
            if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') || name.contains(&0) {
                refused.push(p);
                continue;
            }
            paths.insert(p.clone(), ino);
            if children.contains_key(&ino) && seen.insert(ino) {
                stack.push((ino, p));
            }
        }
    }
    (paths, refused)
}

/// A file's bytes from its data nodes in version order; None when a needed node will not decode.
fn rebuild(img: &[u8], nodes: &[Node]) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut ok = true;
    for nd in nodes {
        if nd.dsize > 0 {
            match decode(nd.compr, &img[nd.data.clone()], nd.dsize) {
                None => ok = false,
                Some(got) => {
                    if buf.len() < nd.off + nd.dsize {
                        buf.resize(nd.off + nd.dsize, 0);
                    }
                    buf[nd.off..nd.off + nd.dsize].copy_from_slice(&got);
                }
            }
        }
        buf.truncate(nd.isize);
    }
    let last = nodes.last()?.isize;
    if buf.len() < last {
        buf.resize(last, 0);
    }
    ok.then_some(buf)
}

/// `path`'s nearest existing ancestor (itself included) resolves inside `base`.
fn inside(base: &Path, path: &Path) -> bool {
    let mut p = path;
    loop {
        if let Ok(real) = fs::canonicalize(p) {
            return real.starts_with(base);
        }
        match p.parent() {
            Some(up) => p = up,
            None => return false,
        }
    }
}

fn os(p: &[u8]) -> &OsStr {
    OsStr::from_bytes(p)
}

fn set_owner(path: &Path, uid: u16, gid: u16) {
    if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
        unsafe { libc::lchown(c.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) };
    }
}

fn set_mtime(path: &Path, mtime: u32) {
    if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
        let t = libc::timespec { tv_sec: mtime as _, tv_nsec: 0 };
        unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), [t, t].as_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
    }
}

/// Extract `img` to `out` (which must be empty or absent); `owners` also gives each entry the image's owner.
pub fn extract(img: &[u8], out: &Path, owners: bool) -> i32 {
    if fs::read_dir(out).map(|mut d| d.next().is_some()).unwrap_or(false) {
        eprintln!("jffs2-extract: {} exists and is not empty", out.display());
        return 2;
    }
    let s = scan(img);
    if s.dirents.is_empty() || s.inodes.is_empty() {
        eprintln!("jffs2-extract: no JFFS2 nodes (big-endian, or not JFFS2?)");
        return 2;
    }
    let (paths, refused) = paths_of(&s.dirents);
    let files = out.join("files");
    if let Err(e) = DirBuilder::new().recursive(true).mode(0o700).create(&files) {
        eprintln!("jffs2-extract: cannot create {}: {e}", files.display());
        return 2;
    }
    let _ = fs::set_permissions(out, fs::Permissions::from_mode(0o700));
    let base = match fs::canonicalize(&files) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("jffs2-extract: {e}");
            return 2;
        }
    };
    let mut rows: Vec<(Vec<u8>, String)> = Vec::new(); // (path, the line after STATUS..MD5)
    let mut links: Vec<String> = Vec::new();
    let mut later: Vec<(PathBuf, u32, u32, u16, u16)> = Vec::new(); // dst, perm, mtime, uid, gid
    let mut first_path: HashMap<u32, PathBuf> = HashMap::new();
    let (mut bad, mut kinds) = (0usize, [0usize; 4]); // f l d o
    let mut row = |st: &str, kind: &str, perm: String, uid: String, gid: String, size: String, p: &[u8], shown: &[u8]| {
        let line = format!("{st}\t{kind}\t{perm}\t{uid}\t{gid}\t{size}\t-\t{}", String::from_utf8_lossy(shown));
        rows.push((p.to_vec(), line));
    };
    for (p, &ino) in &paths {
        let mut nodes = s.inodes.get(&ino).cloned().unwrap_or_default();
        if nodes.is_empty() {
            row("BAD", "o", "-".into(), "-".into(), "-".into(), "-".into(), p, p);
            bad += 1;
            continue;
        }
        nodes.sort_by_key(|nd| nd.ver);
        let top = nodes.last().cloned().unwrap();
        let (mode, uid, gid) = (top.mode, top.uid, top.gid);
        let perm = format!("{:04o}", mode & 0o7777);
        let dst = files.join(os(&p[1..]));
        if !inside(&base, dst.parent().unwrap_or(&files)) {
            row("BAD", "o", perm, uid.to_string(), gid.to_string(), "-".into(), p, p);
            bad += 1;
            continue;
        }
        if mode & S_IFMT == S_IFDIR {
            let _ = DirBuilder::new().recursive(true).mode(0o700).create(&dst);
            row("OK", "d", perm, uid.to_string(), gid.to_string(), "-".into(), p, p);
            later.push((dst, mode & 0o7777, top.mtime, uid, gid));
            kinds[2] += 1;
            continue;
        }
        let kind = match mode & S_IFMT {
            S_IFREG => "f",
            S_IFLNK => "l",
            _ => "o",
        };
        if kind == "o" {
            row("OK", "o", perm, uid.to_string(), gid.to_string(), "-".into(), p, p);
            kinds[3] += 1;
            continue;
        }
        let Some(data) = rebuild(img, &nodes) else {
            row("BAD", kind, perm, uid.to_string(), gid.to_string(), top.isize.to_string(), p, p);
            bad += 1;
            continue;
        };
        if let Some(up) = dst.parent() {
            let _ = DirBuilder::new().recursive(true).mode(0o700).create(up);
        }
        if kind == "l" {
            let target = OsString::from_vec(data.clone());
            if std::os::unix::fs::symlink(&target, &dst).is_err() {
                row("BAD", "l", perm, uid.to_string(), gid.to_string(), data.len().to_string(), p, p);
                bad += 1;
                continue;
            }
            if owners {
                set_owner(&dst, uid, gid);
            }
            let mut shown = p.clone();
            shown.extend_from_slice(b" -> ");
            shown.extend_from_slice(&data);
            row("OK", "l", perm, uid.to_string(), gid.to_string(), data.len().to_string(), p, &shown);
            links.push(format!("{uid}\t{gid}\t{}", String::from_utf8_lossy(&p[1..])));
            kinds[1] += 1;
            continue;
        }
        let written = if let Some(first) = first_path.get(&ino) {
            fs::hard_link(first, &dst).is_ok() // a hard link stays one
        } else {
            let ok = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&dst)
                .and_then(|mut w| w.write_all(&data))
                .is_ok();
            if ok {
                first_path.insert(ino, dst.clone());
                later.push((dst.clone(), mode & 0o7777, top.mtime, uid, gid));
            }
            ok
        };
        if !written {
            row("BAD", "f", perm, uid.to_string(), gid.to_string(), data.len().to_string(), p, p);
            bad += 1;
            continue;
        }
        row("OK", "f", perm, uid.to_string(), gid.to_string(), data.len().to_string(), p, p);
        kinds[0] += 1;
    }
    // Deepest first, so a directory's own mode comes after what was written into it.
    later.sort_by_key(|(d, ..)| std::cmp::Reverse(d.as_os_str().len()));
    for (dst, perm, mtime, uid, gid) in &later {
        if owners {
            set_owner(dst, *uid, *gid); // before the mode: chown clears setuid and setgid
        }
        if fs::set_permissions(dst, fs::Permissions::from_mode(*perm)).is_err() {
            let _ = fs::set_permissions(dst, fs::Permissions::from_mode(perm & 0o1777)); // a setgid bit this user may not set
        }
        set_mtime(dst, *mtime);
    }
    rows.sort();
    links.sort();
    let report: String = rows.iter().map(|(_, l)| format!("{l}\n")).collect();
    let links_txt: String = links.iter().map(|l| format!("{l}\n")).collect();
    if fs::write(out.join("report.tsv"), report).is_err() || fs::write(out.join("links.tsv"), links_txt).is_err() {
        eprintln!("jffs2-extract: cannot write the report");
        return 2;
    }
    println!(
        "jffs2-extract: {} dirent and {} inode nodes, {} with bad CRCs ignored",
        s.n_dirent, s.n_inode, s.bad_crc
    );
    println!(
        "jffs2-extract: {} files, {} links, {} directories, {} other; {bad} BAD; {} names refused",
        kinds[0],
        kinds[1],
        kinds[2],
        kinds[3],
        refused.len()
    );
    for p in &refused {
        println!("jffs2-extract: refused name: {:?}", String::from_utf8_lossy(p));
    }
    if bad > 0 || !refused.is_empty() {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: u32 = S_IFDIR;
    const REG: u32 = S_IFREG;
    const LNK: u32 = S_IFLNK;

    fn rtime_compress(data: &[u8]) -> Vec<u8> {
        let (mut out, mut last, mut pos) = (Vec::new(), [0usize; 256], 0);
        while pos < data.len() {
            let value = data[pos];
            out.push(value);
            pos += 1;
            let mut back = last[value as usize];
            last[value as usize] = pos;
            let mut run = 0u8;
            while back < pos && pos < data.len() && data[pos] == data[back] && run < 255 {
                pos += 1;
                back += 1;
                run += 1;
            }
            out.push(run);
        }
        out
    }

    fn compress(compr: u8, data: &[u8]) -> Vec<u8> {
        use miniz_oxide::deflate::compress_to_vec_zlib;
        match compr {
            NONE => data.to_vec(),
            ZERO => Vec::new(),
            RTIME => rtime_compress(data),
            ZLIB => compress_to_vec_zlib(data, 6),
            _ => compress_to_vec_zlib(data, 6)
                .into_iter()
                .map(|b| match b {
                    0x32 => 0x60,
                    0x60 => 0x32,
                    b => b,
                })
                .collect(),
        }
    }

    /// An image built node by node with real CRCs, as tools/jffs2_extract_test.py builds them.
    #[derive(Default)]
    struct Image(Vec<u8>);

    impl Image {
        fn put(&mut self, node: Vec<u8>) {
            self.0.extend_from_slice(&node);
            while self.0.len() % 4 != 0 {
                self.0.push(0xFF);
            }
        }
        fn dirent(&mut self, pino: u32, ver: u32, ino: u32, name: &[u8], bad_crc: bool) -> &mut Self {
            let mut head = Vec::new();
            head.extend_from_slice(&MAGIC.to_le_bytes());
            head.extend_from_slice(&DIRENT.to_le_bytes());
            head.extend_from_slice(&(40 + name.len() as u32).to_le_bytes());
            let hcrc = jcrc(&head);
            head.extend_from_slice(&hcrc.to_le_bytes());
            for v in [pino, ver, ino, 0] {
                head.extend_from_slice(&v.to_le_bytes());
            }
            head.extend_from_slice(&[name.len() as u8, 8, 0, 0]);
            let ncrc = jcrc(&head) ^ bad_crc as u32;
            head.extend_from_slice(&ncrc.to_le_bytes());
            head.extend_from_slice(&jcrc(name).to_le_bytes());
            head.extend_from_slice(name);
            self.put(head);
            self
        }
        #[allow(clippy::too_many_arguments)]
        fn inode(&mut self, ino: u32, ver: u32, mode: u32, isize: u32, off: u32, data: &[u8], compr: u8, uid: u16) -> &mut Self {
            let payload = compress(compr, data);
            let mut head = Vec::new();
            head.extend_from_slice(&MAGIC.to_le_bytes());
            head.extend_from_slice(&INODE.to_le_bytes());
            head.extend_from_slice(&(68 + payload.len() as u32).to_le_bytes());
            let hcrc = jcrc(&head);
            head.extend_from_slice(&hcrc.to_le_bytes());
            for v in [ino, ver, mode] {
                head.extend_from_slice(&v.to_le_bytes());
            }
            head.extend_from_slice(&uid.to_le_bytes());
            head.extend_from_slice(&0u16.to_le_bytes());
            for v in [isize, 1_700_000_000, 1_700_000_000, 1_700_000_000, off, payload.len() as u32, data.len() as u32] {
                head.extend_from_slice(&v.to_le_bytes());
            }
            head.extend_from_slice(&[compr, 0, 0, 0]);
            head.extend_from_slice(&jcrc(&payload).to_le_bytes());
            let ncrc = jcrc(&head[..60]);
            head.extend_from_slice(&ncrc.to_le_bytes());
            head.extend_from_slice(&payload);
            self.put(head);
            self
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ocbmd_jffs2_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn report(out: &Path) -> String {
        fs::read_to_string(out.join("report.tsv")).unwrap()
    }

    #[test]
    fn rtime_round_trips() {
        let data = b"aaaaabbbbbaaaaabababab\x00\x00\x00\x00xyz".repeat(30);
        assert_eq!(rtime(&rtime_compress(&data), data.len()).unwrap(), data);
    }

    #[test]
    fn a_tree_of_every_compression_comes_back_as_it_was() {
        let text = b"wpa_passphrase=stock\n".repeat(200);
        let mut img = Image::default();
        img.inode(1, 1, DIR | 0o755, 0, 0, b"", NONE, 0)
            .dirent(1, 1, 2, b"etc", false)
            .inode(2, 1, DIR | 0o755, 0, 0, b"", NONE, 1000)
            .dirent(2, 2, 3, b"hostapd.conf", false)
            .inode(3, 1, REG | 0o644, text.len() as u32, 0, &text[..4096], AESMYKEY, 1000)
            .inode(3, 2, REG | 0o644, text.len() as u32, 4096, &text[4096..], ZLIB, 1000)
            .dirent(1, 3, 4, b"z", false)
            .inode(4, 1, REG | 0o755, 300, 0, &[0u8; 300], ZERO, 0)
            .dirent(1, 4, 5, b"r", false)
            .inode(5, 1, REG | 0o600, 64, 0, &b"ab".repeat(32), RTIME, 0)
            .dirent(1, 5, 6, b"plain", false)
            .inode(6, 1, REG | 0o644, 5, 0, b"hello", NONE, 0)
            .dirent(2, 6, 7, b"link", false)
            .inode(7, 1, LNK | 0o777, 12, 0, b"hostapd.conf", NONE, 1000)
            .dirent(1, 7, 3, b"hard", false);
        let out = scratch("tree");
        assert_eq!(extract(&img.0, &out, false), 0, "{}", report(&out));
        let f = out.join("files");
        assert_eq!(fs::read(f.join("etc/hostapd.conf")).unwrap(), text);
        assert_eq!(fs::read(f.join("z")).unwrap(), vec![0u8; 300]);
        assert_eq!(fs::read(f.join("r")).unwrap(), b"ab".repeat(32));
        assert_eq!(fs::read(f.join("plain")).unwrap(), b"hello");
        assert_eq!(fs::read(f.join("hard")).unwrap(), text, "a hard link names the same file");
        assert_eq!(fs::read_link(f.join("etc/link")).unwrap(), Path::new("hostapd.conf"));
        assert_eq!(fs::metadata(f.join("r")).unwrap().permissions().mode() & 0o7777, 0o600);
        let r = report(&out);
        assert!(r.contains("OK\tf\t0644\t1000\t0\t4200\t-\t/etc/hostapd.conf\n"), "{r}");
        assert!(r.contains("OK\tl\t0777\t1000\t0\t12\t-\t/etc/link -> hostapd.conf\n"), "{r}");
        assert_eq!(fs::read_to_string(out.join("links.tsv")).unwrap(), "1000\t0\tetc/link\n");
        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn versions_deletions_truncation_and_bad_crcs() {
        let mut img = Image::default();
        img.inode(1, 1, DIR | 0o755, 0, 0, b"", NONE, 0)
            .dirent(1, 1, 2, b"f", false)
            .inode(2, 1, REG | 0o644, 10, 0, b"0123456789", AESMYKEY, 0)
            .inode(2, 2, REG | 0o644, 4, 0, b"", NONE, 0) // truncated to 4 bytes
            .dirent(1, 2, 3, b"gone", false)
            .inode(3, 1, REG | 0o644, 3, 0, b"old", NONE, 0)
            .dirent(1, 3, 0, b"gone", false) // a later entry naming inode 0: deleted
            .dirent(1, 4, 4, b"bad", true) // its node CRC is wrong: ignored, as the kernel does
            .inode(4, 1, REG | 0o644, 3, 0, b"bad", NONE, 0);
        let out = scratch("versions");
        assert_eq!(extract(&img.0, &out, false), 0, "{}", report(&out));
        let f = out.join("files");
        assert_eq!(fs::read(f.join("f")).unwrap(), b"0123");
        assert!(!f.join("gone").exists() && !f.join("bad").exists());
        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn a_node_that_does_not_decode_is_bad_and_unsafe_names_are_refused() {
        let mut img = Image::default();
        img.inode(1, 1, DIR | 0o755, 0, 0, b"", NONE, 0)
            .dirent(1, 1, 2, b"ok", false)
            .inode(2, 1, REG | 0o644, 2, 0, b"ok", NONE, 0)
            .dirent(1, 2, 3, b"..", false)
            .inode(3, 1, REG | 0o644, 1, 0, b"x", NONE, 0)
            .dirent(1, 3, 4, b"broken", false);
        // ZLIB (6) the way type 22 is stored: the swap makes it no valid stream.
        let data = b"some text that compresses 22222222 ````````".repeat(10);
        let mut swapped = Image::default();
        swapped.inode(4, 1, REG | 0o644, data.len() as u32, 0, &data, AESMYKEY, 0);
        let mut node = swapped.0.clone();
        node[56] = ZLIB; // claim plain zlib: the CRCs still hold, the payload does not decode
        let ncrc = jcrc(&node[..60]);
        node[64..68].copy_from_slice(&ncrc.to_le_bytes());
        img.0.extend_from_slice(&node);
        let out = scratch("bad");
        assert_eq!(extract(&img.0, &out, false), 1);
        let r = report(&out);
        assert!(r.contains("BAD\tf\t0644\t0\t0\t") && r.contains("/broken\n"), "{r}");
        assert!(out.join("files/ok").exists() && !out.join("files/broken").exists());
        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn nothing_is_written_through_a_link() {
        let mut img = Image::default();
        img.inode(1, 1, DIR | 0o755, 0, 0, b"", NONE, 0)
            .dirent(1, 1, 2, b"a", false)
            .inode(2, 1, LNK | 0o777, 4, 0, b"/tmp", NONE, 0)
            .dirent(2, 2, 3, b"evil", false) // a child of the link's inode
            .inode(3, 1, REG | 0o644, 4, 0, b"evil", NONE, 0);
        let out = scratch("link");
        extract(&img.0, &out, false);
        assert!(!Path::new("/tmp/evil").exists() || fs::read("/tmp/evil").unwrap() != b"evil");
        assert!(report(&out).contains("BAD\to\t0644\t0\t0\t-\t-\t/a/evil\n"), "{}", report(&out));
        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn not_jffs2_and_a_non_empty_out_dir_are_refused() {
        let out = scratch("none");
        assert_eq!(extract(&[0xFFu8; 4096], &out, false), 2);
        fs::create_dir_all(out.join("x")).unwrap();
        let mut img = Image::default();
        img.inode(1, 1, DIR | 0o755, 0, 0, b"", NONE, 0).dirent(1, 1, 2, b"f", false).inode(2, 1, REG | 0o644, 1, 0, b"f", NONE, 0);
        assert_eq!(extract(&img.0, &out, false), 2);
        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn only_the_subcommand_is_taken() {
        assert_eq!(cli(&["ocbmd".into()]), None);
        assert_eq!(cli(&["ocbmd".into(), "jffs2-extract".into()]), Some(2));
    }
}
