#!/usr/bin/env python3
"""jffs2_extract.py — extract a CPC200-CCPA rootfs dump (mtd2.bin, JFFS2) to a directory, offline.

    tools/jffs2_extract.py MTD2_BIN OUT_DIR    ->  OUT_DIR/files/<the rootfs>, OUT_DIR/report.tsv
    tools/jffs2_extract.py --cat MTD2_BIN PATH  ->  that one file's bytes on stdout (e.g. /etc/software_version)

The one-file install dumps the whole flash to its stick before it changes anything
(OCBM_BACKUP/<adapter>/mtd0.bin mtd1.bin mtd2.bin), so the stock rootfs of a converted adapter is in
mtd2.bin. tools/stock_restore_onefile_tree.sh --dump makes the restore's stock tree from it.

Carlinkit's 3.14.52 kernel stores most data nodes with compression type 22
("jffs2_zlib_decompress_aesmykey", 0x80129ffc in the kernel dumps of lvalen91/CPC200-CCPA_resources,
custom/kernel_dumps/): the bytes 0x32 and 0x60 are swapped in the compressed payload, which is then
plain zlib. zlib's adler32 is what proves the swap right: a raw inflate of the payload can come out
the right LENGTH with the wrong bytes, so a type-22 node decodes only as a complete zlib stream whose
checksum matches. A few nodes are rtime (type 2), decoded here; 0 (none), 1 (zeros) and 6 (zlib) are
the kernel's own.

Every node's header, node and data CRCs are checked, and a node that fails one is ignored, as the
kernel ignores it; so are nodes marked obsolete. The highest version of each directory entry wins (an
entry naming inode 0 is a deletion), and each file's data nodes are applied in version order, a
node's size truncating what lies beyond it. A file is written only when every data node it needs
decodes to exactly its declared length; anything else is BAD in the report and the exit status is 1.
Entry names with "/", ".", ".." or NUL are refused, and nothing is written through a link or outside
OUT_DIR/files. Device nodes and FIFOs are reported, not made.

report.tsv, one line per path, sorted: STATUS TYPE MODE UID GID SIZE MD5 PATH, tab-separated.
STATUS OK or BAD; TYPE f (file), l (link), d (directory), o (other); MODE octal; MD5 of a file's
bytes, '-' otherwise; PATH from "/", and for a link "PATH -> TARGET". UID and GID are the image's
own: Carlinkit's build leaves most of the rootfs to uid 1000, and the restore keeps that.

From a tester's ccpa_jffs2_extract.py (October 2026), which first decoded type 22 on an IW416 unit.
The output holds the adapter's Wi-Fi passphrase and Bluetooth link keys: keep it out of repos, issues
and logs, and delete it when the restore is done.
"""
import hashlib
import os
import stat
import struct
import sys
import zlib

MAGIC = 0x1985
DIRENT, INODE = 0xE001, 0xE002          # obsolete nodes have the 0x2000 bit cleared, so never match
NONE, ZERO, RTIME, ZLIB, AESMYKEY = 0, 1, 2, 6, 22
SWAP = bytes.maketrans(b"\x32\x60", b"\x60\x32")
ROOT_INO = 1


def jcrc(b):
    """JFFS2's crc32: Linux crc32_le seeded with 0, no final inversion."""
    return zlib.crc32(b, 0xFFFFFFFF) ^ 0xFFFFFFFF


def rtime(src, dsize):
    """fs/jffs2/compr_rtime.c, decompress: a byte, then how many to repeat from its last position."""
    out = bytearray()
    last = [0] * 256
    pos = 0
    while len(out) < dsize:
        value, repeat = src[pos], src[pos + 1]
        pos += 2
        out.append(value)
        back = last[value]
        last[value] = len(out)
        for _ in range(repeat):          # byte by byte: the copy may overlap what it writes
            out.append(out[back])
            back += 1
    return bytes(out)


def zlib_kernel(src):
    """fs/jffs2/compr_zlib.c: a zlib header when the first two bytes make one, else raw deflate."""
    if len(src) >= 2 and src[0] & 0x0F == 8 and ((src[0] << 8) + src[1]) % 31 == 0:
        return zlib.decompress(src)
    return zlib.decompress(src, -15)


def decode(compr, data, dsize):
    try:
        if compr == NONE:
            out = data[:dsize]
        elif compr == ZERO:
            out = bytes(dsize)
        elif compr == RTIME:
            out = rtime(data, dsize)
        elif compr == ZLIB:
            out = zlib_kernel(data)
        elif compr == AESMYKEY:
            out = zlib.decompress(data.translate(SWAP))   # a complete stream, adler32 checked
        else:
            return None
    except (zlib.error, IndexError):
        return None
    return out if len(out) == dsize else None


def scan(img):
    """Every valid dirent and inode node: (dirents, inodes, counts)."""
    dirents = {}   # (pino, name) -> (version, ino)
    inodes = {}    # ino -> [(version, offset, dsize, isize, compr, data, mode, uid, gid, mtime)]
    counts = {"dirent": 0, "inode": 0, "bad_crc": 0}
    i, n = 0, len(img)
    while i + 12 <= n:
        magic, ntype, totlen, hcrc = struct.unpack_from("<HHII", img, i)
        if magic != MAGIC or totlen < 12 or i + totlen > n or jcrc(img[i:i + 8]) != hcrc:
            i += 4
            continue
        if ntype == DIRENT and totlen >= 40:
            pino, ver, ino, _mctime, nsize, _dtype = struct.unpack_from("<IIIIBB", img, i + 12)
            ncrc, namecrc = struct.unpack_from("<II", img, i + 32)
            name = img[i + 40:i + 40 + nsize]
            if jcrc(img[i:i + 32]) != ncrc or 40 + nsize > totlen or jcrc(name) != namecrc:
                counts["bad_crc"] += 1
            else:
                counts["dirent"] += 1
                k = (pino, name)
                if k not in dirents or dirents[k][0] <= ver:
                    dirents[k] = (ver, ino)
        elif ntype == INODE and totlen >= 68:
            (ino, ver, mode, uid, gid, isize, _atime, mtime, _ctime, off, csize, dsize, compr, _ucompr,
             _flags, dcrc, ncrc) = struct.unpack_from("<IIIHHIIIIIIIBBHII", img, i + 12)
            data = img[i + 68:i + 68 + csize]
            if jcrc(img[i:i + 60]) != ncrc or 68 + csize > totlen or jcrc(data) != dcrc:
                counts["bad_crc"] += 1
            else:
                counts["inode"] += 1
                counts["compr_%d" % compr] = counts.get("compr_%d" % compr, 0) + 1
                inodes.setdefault(ino, []).append((ver, off, dsize, isize, compr, data, mode, uid, gid, mtime))
        i += (totlen + 3) & ~3
    return dirents, inodes, counts


def paths_of(dirents):
    """ino -> [paths]: a walk from the root inode; unsafe names and directory loops are skipped."""
    children = {}
    for (pino, name), (_ver, ino) in dirents.items():
        if ino:
            children.setdefault(pino, []).append((name, ino))
    paths, refused, seen_dirs = {}, [], {ROOT_INO}
    stack = [(ROOT_INO, "")]
    while stack:
        pino, prefix = stack.pop()
        for name, ino in sorted(children.get(pino, [])):
            if not name or name in (b".", b"..") or b"/" in name or b"\0" in name:
                refused.append(prefix + "/" + name.decode("utf-8", "backslashreplace"))
                continue
            p = prefix + "/" + name.decode("utf-8", "surrogateescape")
            paths.setdefault(ino, []).append(p)
            if ino in children and ino not in seen_dirs:
                seen_dirs.add(ino)
                stack.append((ino, p))
    return paths, refused


def rebuild(nodes):
    """A file's bytes from its data nodes in version order; None when a needed node will not decode."""
    buf = bytearray()
    ok = True
    for ver, off, dsize, isize, compr, data, *_ in nodes:
        if dsize:
            got = decode(compr, data, dsize)
            if got is None:
                ok = False
            else:
                if len(buf) < off:
                    buf.extend(bytes(off - len(buf)))
                buf[off:off + dsize] = got
        if len(buf) > isize:
            del buf[isize:]
    final = nodes[-1][3]
    if len(buf) < final:
        buf.extend(bytes(final - len(buf)))
    return bytes(buf) if ok else None


def inside(base, path):
    real = os.path.realpath(path)
    return real == base or real.startswith(base + os.sep)


def cat(img, path):
    """One file's bytes (its own path, not through a link), or None."""
    dirents, inodes, _counts = scan(img)
    paths, _refused = paths_of(dirents)
    for ino, ps in paths.items():
        if path in ps and inodes.get(ino):
            nodes = sorted(inodes[ino], key=lambda x: x[0])
            if stat.S_ISREG(nodes[-1][6]):
                return rebuild(nodes)
    return None


def main(argv):
    if len(argv) == 4 and argv[1] == "--cat":
        with open(argv[2], "rb") as f:
            data = cat(f.read(), argv[3])
        if data is None:
            print("jffs2_extract: no file %s in %s" % (argv[3], argv[2]), file=sys.stderr)
            return 1
        sys.stdout.buffer.write(data)
        return 0
    if len(argv) != 3:
        print(__doc__.split("\n\n")[0], file=sys.stderr)
        return 2
    img_path, out = argv[1], argv[2]
    with open(img_path, "rb") as f:
        img = f.read()
    if os.path.exists(out) and os.listdir(out):
        print("jffs2_extract: %s exists and is not empty" % out, file=sys.stderr)
        return 2
    dirents, inodes, counts = scan(img)
    if not dirents or not inodes:
        print("jffs2_extract: no JFFS2 nodes in %s (big-endian, or not JFFS2?)" % img_path, file=sys.stderr)
        return 2
    paths, refused = paths_of(dirents)

    files = os.path.join(out, "files")
    os.makedirs(files, mode=0o700)
    os.chmod(out, 0o700)
    base = os.path.realpath(files)
    rows, later, bad = [], [], 0
    first_path = {}
    for p, ino in sorted((p, ino) for ino, ps in paths.items() for p in ps):
        nodes = sorted(inodes.get(ino, []), key=lambda x: x[0])
        if not nodes:
            rows.append(("BAD", "o", "-", "-", "-", "-", "-", p))
            bad += 1
            continue
        _v, _o, _d, _s, _c, _x, mode, uid, gid, mtime = nodes[-1]
        dst = files + p
        if not inside(base, os.path.dirname(dst)):
            rows.append(("BAD", "o", "%04o" % (mode & 0o7777), uid, gid, "-", "-", p))
            bad += 1
            continue
        perm = "%04o" % (mode & 0o7777)
        if stat.S_ISDIR(mode):
            os.makedirs(dst, mode=0o700, exist_ok=True)
            rows.append(("OK", "d", perm, uid, gid, "-", "-", p))
            later.append((dst, mode & 0o7777, mtime))
            continue
        kind = "f" if stat.S_ISREG(mode) else "l" if stat.S_ISLNK(mode) else "o"
        if kind == "o":
            rows.append(("OK", "o", perm, uid, gid, "-", "-", p))
            continue
        data = rebuild(nodes)
        if data is None:
            rows.append(("BAD", kind, perm, uid, gid, nodes[-1][3], "-", p))
            bad += 1
            continue
        os.makedirs(os.path.dirname(dst), mode=0o700, exist_ok=True)
        if kind == "l":
            target = data.decode("utf-8", "surrogateescape")
            os.symlink(target, dst)
            rows.append(("OK", "l", perm, uid, gid, len(data), "-", "%s -> %s" % (p, target)))
            continue
        if ino in first_path:
            os.link(first_path[ino], dst)                       # a hard link stays one
        else:
            fd = os.open(dst, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
            with os.fdopen(fd, "wb") as w:
                w.write(data)
            first_path[ino] = dst
            later.append((dst, mode & 0o7777, mtime))
        rows.append(("OK", "f", perm, uid, gid, len(data), hashlib.md5(data).hexdigest(), p))
    for dst, perm, mtime in sorted(later, key=lambda x: -len(x[0])):   # deepest first: dirs last
        try:
            os.chmod(dst, perm)
        except PermissionError:
            os.chmod(dst, perm & 0o1777)   # a setgid bit this user may not set
        os.utime(dst, (mtime, mtime), follow_symlinks=False)

    with open(os.path.join(out, "report.tsv"), "w") as r:
        for row in sorted(rows, key=lambda x: x[7]):
            r.write("\t".join(str(x) for x in row) + "\n")
    kinds = {}
    for row in rows:
        if row[0] == "OK":
            kinds[row[1]] = kinds.get(row[1], 0) + 1
    print("jffs2_extract: %d dirent and %d inode nodes (%s), %d with bad CRCs ignored"
          % (counts["dirent"], counts["inode"],
             ", ".join("type %s: %d" % (k[6:], v) for k, v in sorted(counts.items()) if k.startswith("compr_")),
             counts["bad_crc"]))
    print("jffs2_extract: %d files, %d links, %d directories, %d other; %d BAD; %d names refused"
          % (kinds.get("f", 0), kinds.get("l", 0), kinds.get("d", 0), kinds.get("o", 0), bad, len(refused)))
    for p in refused:
        print("jffs2_extract: refused name: %r" % p)
    return 1 if bad or refused else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
