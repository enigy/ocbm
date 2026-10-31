#!/usr/bin/env python3
"""pack_tree.py — the stock tree packed for the box, owned as on the adapter (tools/stock_restore.sh).

    tools/pack_tree.py pack  TREE TGZ [OWNERS]   pack TREE, then check it as below; prints PACK_OK
    tools/pack_tree.py check TREE TGZ [OWNERS]   refuse (and delete) a TGZ that is not exactly TREE:
                                                  the same entries, no "._" AppleDouble file, and
                                                  each entry owned as OWNERS says; prints PACK_OK
    tools/pack_tree.py owners SOURCE TREE [--rename FROM=TO]...
                                                  OWNERS for TREE on stdout, from SOURCE: a trial
                                                  archive (.tgz) or a dump's report.tsv
                                                  (tools/jffs2_extract.py); FROM's owner goes to TO

OWNERS: one line per entry, "UID<TAB>GID<TAB>PATH" with PATH relative to the tree. An entry it does
not list is root's (0:0).

The box unpacks the pack as root and stock_restore_box.sh copies with `cp -p`, so the owners in the
pack are the owners on flash. A tree unpacked on this Mac belongs to the Mac's user, and a plain
`tar -czf` packed it that way: restored files came out 502:20 (a tester, Oct 2026). Carlinkit's own
image is not all root's either: most of its rootfs is uid 1000 (some 1000:1000, three files 501:20),
so the owners come from the source, not a blanket root. Python's tarfile writes no "._" entries
whatever the files' extended attributes; names are kept numeric only (no user or group names), so the
box's busybox tar sets exactly these ids.
"""
import os
import sys
import tarfile


def norm(name):
    n = name[2:] if name.startswith("./") else name
    return n.rstrip("/")


def tree_entries(tree):
    out = []
    for root, dirs, files in os.walk(tree):
        for n in dirs + files:
            out.append(os.path.relpath(os.path.join(root, n), tree))
    return sorted(out)


def read_owners(path):
    owners = {}
    if path:
        for line in open(path, encoding="utf-8", errors="surrogateescape"):
            uid, gid, p = line.rstrip("\n").split("\t", 2)
            owners[p] = (int(uid), int(gid))
    return owners


def say(msg):
    print("[onefile-tree] %s" % msg)


def pack(tree, tgz, owners):
    with tarfile.open(tgz, "w:gz", format=tarfile.GNU_FORMAT) as t:
        top = t.gettarinfo(tree, arcname=".")
        top.uid, top.gid, top.uname, top.gname = 0, 0, "", ""
        t.addfile(top)
        for rel in tree_entries(tree):
            p = os.path.join(tree, rel)
            ti = t.gettarinfo(p, arcname="./" + rel)
            ti.uid, ti.gid = owners.get(rel, (0, 0))
            ti.uname = ti.gname = ""
            if ti.isreg():
                with open(p, "rb") as f:
                    t.addfile(ti, f)
            else:
                t.addfile(ti)            # a directory, a link (symlink or hard link), or a node


def check(tree, tgz, owners):
    want = tree_entries(tree)
    got, wrong = [], []
    with tarfile.open(tgz, "r:gz") as t:
        for m in t.getmembers():
            n = norm(m.name)
            if n in ("", "."):
                continue
            got.append(n)
            if (m.uid, m.gid) != owners.get(n, (0, 0)):
                wrong.append((n, m.uid, m.gid))
    got = sorted(set(got))
    extra = sorted(set(got) - set(want))
    missing = sorted(set(want) - set(got))
    apple = sorted({p for p in want + got if os.path.basename(p).startswith("._")})
    if not (extra or missing or apple or wrong):
        say("packed: exactly the tree's %d entries, each owned as on the adapter" % len(want))
        return True
    os.remove(tgz)
    if extra:
        say("in the pack, not in the tree: %d, first %s" % (len(extra), extra[0]))
    if missing:
        say("in the tree, not in the pack: %d, first %s" % (len(missing), missing[0]))
    if apple:
        say("macOS AppleDouble files: %d, first %s" % (len(apple), apple[0]))
    if wrong:
        n, uid, gid = wrong[0]
        w = owners.get(n, (0, 0))
        say("owned otherwise than on the adapter: %d, first %s (%d:%d, not %d:%d)" % (len(wrong), n, uid, gid, w[0], w[1]))
    print("[onefile-tree] ABORT: the pack is not exactly the stock tree; deleted it, nothing goes to the box", file=sys.stderr)
    return False


def owners_from(source, tree, renames):
    src = {}
    if source.endswith(".tsv"):
        for line in open(source, encoding="utf-8", errors="surrogateescape"):
            f = line.rstrip("\n").split("\t")
            if len(f) == 8 and f[0] == "OK":
                src[f[7].split(" -> ")[0].lstrip("/")] = (int(f[3]), int(f[4]))
    else:
        with tarfile.open(source, "r:*") as t:
            for m in t.getmembers():
                src[norm(m.name.lstrip("/"))] = (m.uid, m.gid)
    for r in renames:
        a, b = r.split("=", 1)
        if a in src:
            src[b] = src[a]
    for rel in tree_entries(tree):
        uid, gid = src.get(rel, (0, 0))
        sys.stdout.write("%d\t%d\t%s\n" % (uid, gid, rel))


def main(argv):
    if len(argv) >= 4 and argv[1] in ("pack", "check") and len(argv) <= 5:
        tree, tgz = os.path.abspath(argv[2]), os.path.abspath(argv[3])
        if not os.path.isdir(tree):
            print("[onefile-tree] ABORT: no tree at %s" % tree, file=sys.stderr)
            return 1
        if tgz.startswith(tree + os.sep):
            print("[onefile-tree] ABORT: the pack cannot go inside the tree", file=sys.stderr)
            return 1
        owners = read_owners(argv[4] if len(argv) == 5 else None)
        if argv[1] == "pack":
            if os.path.exists(tgz):
                os.remove(tgz)
            pack(tree, tgz, owners)
        if not os.path.isfile(tgz) or os.path.getsize(tgz) == 0:
            print("[onefile-tree] ABORT: no pack at %s" % tgz, file=sys.stderr)
            return 1
        if not check(tree, tgz, owners):
            return 1
        print("PACK_OK")
        return 0
    if len(argv) >= 4 and argv[1] == "owners":
        renames = []
        rest = argv[4:]
        while rest:
            if rest[0] != "--rename" or len(rest) < 2:
                break
            renames.append(rest[1])
            rest = rest[2:]
        owners_from(argv[2], os.path.abspath(argv[3]), renames)
        return 0
    print(__doc__.split("\n\n")[0], file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
