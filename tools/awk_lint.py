#!/usr/bin/env python3
"""awk_lint.py — find awk constructs the adapter's BusyBox 1.32.1 awk misreads, in shell scripts.

    tools/awk_lint.py FILE...    prints FILE:LINE: problem for each, exit 1 if any

BusyBox's awk reads a name followed by whitespace and "(" as a call to a function of that name: in
ocbm_update.sh's newer(), `return k (p == "" ? "~" : "-" p)` (concatenation to gawk and macOS awk)
was "Call to undefined function" on the dev adapter (3 Nov), so newer() failed for every pair and
every update was refused as not newer. No suite saw it: the hosts' awks accept the line.

Each single-quoted program after a word `awk` is checked, with its strings and comments left out. A
keyword (if, while, for, return, ...) or a builtin function may be followed by a space; any other name
followed by whitespace and "(" is reported. Write `name(...)` for a call, and put a concatenation's
parenthesised part in a variable first.
"""
import re
import sys

OK = {
    # keywords that may precede "(" with a space
    "if", "while", "for", "return", "in", "do", "else", "print", "printf", "getline", "delete", "exit",
    # builtins: BusyBox knows these whichever way they are written
    "length", "substr", "index", "split", "sub", "gsub", "match", "sprintf", "sin", "cos", "atan2",
    "exp", "log", "sqrt", "int", "rand", "srand", "tolower", "toupper", "system", "close", "fflush",
}
PROGRAM = re.compile(r"\bawk\b[^'\n]*?'([^']*)'", re.S)
STRING = re.compile(r'"(?:[^"\\\n]|\\.)*"')
REGEX = re.compile(r"(?<![A-Za-z0-9_)\]])/(?:[^/\\\n]|\\.)+/")
CALL = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)[ \t]+\(")


def problems(path):
    text = open(path, encoding="utf-8", errors="surrogateescape").read()
    for m in PROGRAM.finditer(text):
        start = text.count("\n", 0, m.start(1)) + 1
        for i, line in enumerate(m.group(1).split("\n")):
            code = STRING.sub('""', line)
            code = REGEX.sub("//", code)
            code = code.split("#", 1)[0]
            for c in CALL.finditer(code):
                if c.group(1) not in OK:
                    yield "%s:%d: awk reads `%s (` as a call to a function %s; write it without the space, or use a variable" % (
                        path, start + i, c.group(1), c.group(1))


def main(argv):
    found = [p for f in argv[1:] for p in problems(f)]
    for p in found:
        print(p)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
