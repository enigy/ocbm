#!/usr/bin/env python3
"""jffs2_extract_test.py — tools/jffs2_extract.py against JFFS2 images built here, node by node.

    tools/jffs2_extract_test.py                      run the tests
    tools/jffs2_extract_test.py --make-image DIR IMG  write DIR as a little-endian JFFS2 image the
                                                      way Carlinkit's kernel stores it (data nodes of
                                                      type 22, owned by uid 1000), for
                                                      tools/stock_restore_test.sh

Needs no adapter and no dump: every node carries real CRCs, and the compressors are the kernel's
(rtime from fs/jffs2/compr_rtime.c, type 22 as zlib with the bytes 0x32 and 0x60 swapped).
"""
import hashlib
import os
import stat
import struct
import subprocess
import sys
import tempfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
EXTRACT = os.path.join(HERE, "jffs2_extract.py")
SWAP = bytes.maketrans(b"\x32\x60", b"\x60\x32")


def jcrc(b):
    return zlib.crc32(b, 0xFFFFFFFF) ^ 0xFFFFFFFF


def rtime_compress(data):
    out, last, pos = bytearray(), [0] * 256, 0
    while pos < len(data):
        value = data[pos]
        out.append(value)
        pos += 1
        back = last[value]
        last[value] = pos
        run = 0
        while back < pos and pos < len(data) and data[pos] == data[back] and run < 255:
            pos += 1
            back += 1
            run += 1
        out.append(run)
    return bytes(out)


def compress(compr, data):
    return {0: lambda d: d, 1: lambda d: b"", 2: rtime_compress, 6: zlib.compress,
            22: lambda d: zlib.compress(d).translate(SWAP)}[compr](data)


class Image:
    def __init__(self):
        self.b = bytearray()

    def _put(self, node):
        self.b += node
        self.b += b"\xff" * (-len(node) % 4)

    def dirent(self, pino, ver, ino, name, dtype=8, bad_crc=False, ntype=0xE001):
        name = name.encode() if isinstance(name, str) else name
        totlen = 40 + len(name)
        hdr = struct.pack("<HHI", 0x1985, ntype, totlen)
        body = struct.pack("<IIIIBBH", pino, ver, ino, 0, len(name), dtype, 0)
        head = hdr + struct.pack("<I", jcrc(hdr)) + body
        ncrc = jcrc(head) ^ (1 if bad_crc else 0)
        self._put(head + struct.pack("<II", ncrc, jcrc(name)) + name)

    def inode(self, ino, ver, mode, isize, off=0, data=b"", compr=22, uid=1000, gid=0, mtime=1700000000,
              raw=None, bad_crc=False, ntype=0xE002, dsize=None):
        payload = compress(compr, data) if raw is None else raw
        dsize = len(data) if dsize is None else dsize
        totlen = 68 + len(payload)
        hdr = struct.pack("<HHI", 0x1985, ntype, totlen)
        body = struct.pack("<IIIHHIIIIIIIBBH", ino, ver, mode, uid, gid, isize, mtime, mtime, mtime,
                           off, len(payload), dsize, compr, 0, 0)
        head = hdr + struct.pack("<I", jcrc(hdr)) + body + struct.pack("<I", jcrc(payload))
        ncrc = jcrc(head[:60]) ^ (1 if bad_crc else 0)    # the kernel's node CRC stops before data_crc
        self._put(head + struct.pack("<I", ncrc) + payload)

    def garbage(self, n):
        self.b += bytes((i * 37) & 0xFF for i in range(n))
        self.b += b"\xff" * (-len(self.b) % 4)


DIR, REG, LNK = stat.S_IFDIR, stat.S_IFREG, stat.S_IFLNK


def image_from_dir(top):
    """A tree as Carlinkit's kernel would store it: 4 KiB data nodes of type 22, uid 1000."""
    img, inos, nxt = Image(), {top: 1}, 2
    img.inode(1, 1, DIR | 0o755, 0, compr=0)
    for root, dirs, files in os.walk(top):
        for name in sorted(dirs) + sorted(files):
            p = os.path.join(root, name)
            st = os.lstat(p)
            ino, nxt = nxt, nxt + 1
            inos[p] = ino
            perm = stat.S_IMODE(st.st_mode)
            if stat.S_ISLNK(st.st_mode):
                target = os.readlink(p).encode()
                img.inode(ino, 1, LNK | 0o777, len(target), data=target, compr=0)
                dtype = 10
            elif stat.S_ISDIR(st.st_mode):
                img.inode(ino, 1, DIR | perm, 0, compr=0)
                dtype = 4
            else:
                data = open(p, "rb").read()
                if not data:
                    img.inode(ino, 1, REG | perm, 0, compr=0)
                for v, off in enumerate(range(0, len(data), 4096), 1):
                    img.inode(ino, v, REG | perm, len(data), off=off, data=data[off:off + 4096])
                dtype = 8
            img.dirent(inos[root], ino, ino, name, dtype)
    return bytes(img.b)


def run(*args):
    p = subprocess.run([sys.executable, "-I", EXTRACT] + list(args), capture_output=True)
    return p.returncode, p.stdout.decode(errors="replace") + p.stderr.decode(errors="replace")


fails = 0


def check(cond, what):
    global fails
    print(("  ok   " if cond else "  FAIL ") + what)
    if not cond:
        fails += 1


def report(out):
    rows = {}
    for line in open(os.path.join(out, "report.tsv")):
        st, t, mode, uid, gid, size, md5, p = line.rstrip("\n").split("\t")
        rows[p.split(" -> ")[0]] = (st, t, mode, uid, gid, size, md5, p)
    return rows


def good_image():
    """Every node kind the dumps hold, and the history a rootfs collects."""
    im = Image()
    im.inode(1, 1, DIR | 0o755, 0, compr=0)
    im.dirent(1, 1, 2, "etc", 4); im.inode(2, 1, DIR | 0o755, 0, compr=0)
    im.dirent(1, 2, 3, "bin", 4); im.inode(3, 1, DIR | 0o750, 0, compr=0, uid=0)
    big = (b"OCBM restore test " * 400)[:6000]
    # type 22 over two nodes, and a later node overwriting part of the first
    im.dirent(3, 1, 10, "busybox"); im.inode(10, 1, REG | 0o4755, 6000, 0, big[:4096])
    im.inode(10, 2, REG | 0o4755, 6000, 4096, big[4096:]); im.inode(10, 3, REG | 0o4755, 6000, 100, b"PATCHED")
    im.dirent(2, 1, 11, "plain"); im.inode(11, 1, REG | 0o644, 5, 0, b"hello", compr=0)
    im.inode(11, 2, REG | 0o644, 5, 0, b"WRONG", compr=0, bad_crc=True)       # bad node CRC: ignored
    im.inode(11, 3, REG | 0o644, 5, 0, b"STALE", compr=0, ntype=0xC002)        # marked obsolete: ignored
    rt = b"aaaaabbbbbaaaaabbbbb" * 30 + b"xyz"
    im.dirent(2, 2, 12, "rt"); im.inode(12, 1, REG | 0o600, len(rt), 0, rt, compr=2, uid=501, gid=20)
    im.dirent(2, 3, 13, "z6"); im.inode(13, 1, REG | 0o644, 3000, 0, bytes(range(256)) * 11 + b"x" * 184, compr=6)
    # a hole: zeros, then data
    im.dirent(2, 4, 14, "hole"); im.inode(14, 1, REG | 0o644, 8192, 0, bytes(4096), compr=1)
    im.inode(14, 2, REG | 0o644, 8192, 4096, b"Q" * 4096)
    # written, truncated to 10, extended to 20: bytes 10..14 must be zeros, not the old data
    im.dirent(2, 5, 15, "trunc"); im.inode(15, 1, REG | 0o644, 100, 0, b"T" * 100)
    im.inode(15, 2, REG | 0o644, 10, 10, b"", compr=0); im.inode(15, 3, REG | 0o644, 20, 15, b"EEEEE")
    # deleted, renamed, a link, a hard link
    im.dirent(2, 6, 16, "gone"); im.inode(16, 1, REG | 0o644, 4, 0, b"gone"); im.dirent(2, 7, 0, "gone")
    im.dirent(2, 8, 17, "old"); im.inode(17, 1, REG | 0o644, 3, 0, b"new")
    im.dirent(2, 9, 0, "old"); im.dirent(2, 10, 17, "new")
    im.dirent(3, 2, 18, "sh", 10); im.inode(18, 1, LNK | 0o777, 7, 0, b"busybox", compr=0)
    im.dirent(3, 3, 10, "busybox.hard")
    im.garbage(64)
    return bytes(im.b), big, rt


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "--make-image":
        with open(sys.argv[3], "wb") as f:
            f.write(image_from_dir(sys.argv[2]))
        return 0
    t = tempfile.mkdtemp(prefix="jffs2_extract_test.")
    img, big, rt = good_image()
    open(os.path.join(t, "good.bin"), "wb").write(img)
    out = os.path.join(t, "good")
    rc, log = run(os.path.join(t, "good.bin"), out)
    print("== a clean image")
    check(rc == 0 and "0 BAD" in log, "extracts with no BAD entry (rc=%d)" % rc)
    f = lambda p: open(os.path.join(out, "files", p), "rb").read()
    want = bytearray(big); want[100:107] = b"PATCHED"
    check(f("bin/busybox") == bytes(want), "type 22 over two nodes, a later node overwriting part of one")
    check(f("etc/plain") == b"hello", "a node with a bad CRC and one marked obsolete are ignored")
    check(f("etc/rt") == rt, "rtime (type 2) decodes, overlapping copies included")
    check(f("etc/z6") == bytes(range(256)) * 11 + b"x" * 184, "zlib (type 6) decodes")
    check(f("etc/hole") == bytes(4096) + b"Q" * 4096, "a zero node (type 1) is a hole of zeros")
    check(f("etc/trunc") == b"T" * 10 + bytes(5) + b"EEEEE", "a truncate cuts the old data; the gap after it reads zeros")
    check(not os.path.exists(os.path.join(out, "files/etc/gone")), "a deleted entry (inode 0) is gone")
    check(f("etc/new") == b"new" and not os.path.exists(os.path.join(out, "files/etc/old")), "a renamed file is under its new name only")
    check(os.readlink(os.path.join(out, "files/bin/sh")) == "busybox", "a link keeps its target")
    check(os.stat(os.path.join(out, "files/bin/busybox")).st_ino == os.stat(os.path.join(out, "files/bin/busybox.hard")).st_ino,
          "a hard link stays one file")
    r = report(out)
    check(r["/etc/rt"][2:5] == ("0600", "501", "20") and r["/bin/busybox"][2:5] == ("4755", "1000", "0"),
          "the report keeps each entry's mode, uid and gid")
    check(r["/bin"][1] == "d" and r["/bin"][2] == "0750" and stat.S_IMODE(os.stat(os.path.join(out, "files/bin")).st_mode) == 0o750,
          "a directory gets its own mode")
    check(r["/etc/plain"][6] == hashlib.md5(b"hello").hexdigest(), "the report holds each file's MD5")
    check(stat.S_IMODE(os.stat(out).st_mode) == 0o700, "the output folder is private (0700)")
    rc, log = run("--cat", os.path.join(t, "good.bin"), "/etc/trunc")
    check(rc == 0 and log == "T" * 10 + "\0" * 5 + "EEEEE", "--cat prints one file")
    rc, log = run(os.path.join(t, "good.bin"), out)
    check(rc == 2 and "not empty" in log, "refuses an output folder that is not empty")

    print("== images that must not extract cleanly")
    im = Image()
    im.inode(1, 1, DIR | 0o755, 0, compr=0)
    data = b"plain zlib, never swapped " * 40
    z = zlib.compress(data)
    assert b"\x32" in z or b"\x60" in z
    im.dirent(1, 1, 5, "unswapped"); im.inode(5, 1, REG | 0o644, len(data), 0, raw=z, compr=22, dsize=len(data))
    im.dirent(1, 2, 6, ".."); im.inode(6, 1, REG | 0o644, 4, 0, b"evil", compr=0)
    im.dirent(1, 3, 7, "a/b"); im.inode(7, 1, REG | 0o644, 4, 0, b"evil", compr=0)
    open(os.path.join(t, "bad.bin"), "wb").write(bytes(im.b))
    rc, log = run(os.path.join(t, "bad.bin"), os.path.join(t, "bad"))
    r = report(os.path.join(t, "bad"))
    check(rc == 1 and r["/unswapped"][0] == "BAD" and not os.path.exists(os.path.join(t, "bad/files/unswapped")),
          "a type-22 node that was not swapped fails zlib's check: BAD, not written")
    check("2 names refused" in log and not os.path.exists(os.path.join(t, "evil")) and not os.path.exists(os.path.join(t, "bad/files/a")),
          "names '..' and 'a/b' are refused, nothing written outside")
    open(os.path.join(t, "none.bin"), "wb").write(b"\xff" * 4096)
    rc, log = run(os.path.join(t, "none.bin"), os.path.join(t, "none"))
    check(rc == 2 and "no JFFS2 nodes" in log, "refuses an image with no nodes")

    print("== --make-image round trip")
    src = os.path.join(t, "src")
    os.makedirs(os.path.join(src, "etc/sub"))
    open(os.path.join(src, "etc/sub/f"), "wb").write(os.urandom(9000))
    open(os.path.join(src, "etc/empty"), "wb").close()
    os.symlink("sub/f", os.path.join(src, "etc/l"))
    open(os.path.join(t, "mk.bin"), "wb").write(image_from_dir(src))
    rc, log = run(os.path.join(t, "mk.bin"), os.path.join(t, "mk"))
    m = os.path.join(t, "mk", "files")
    check(rc == 0 and open(os.path.join(m, "etc/sub/f"), "rb").read() == open(os.path.join(src, "etc/sub/f"), "rb").read()
          and os.path.getsize(os.path.join(m, "etc/empty")) == 0 and os.readlink(os.path.join(m, "etc/l")) == "sub/f",
          "a tree written as an image comes back byte for byte")
    subprocess.run(["rm", "-rf", t])
    print("jffs2_extract_test: all passed" if not fails else "jffs2_extract_test: %d failed" % fails)
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
