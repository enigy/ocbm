#!/bin/sh
# busybox_suites.sh — the box scripts' suites again, under BusyBox's sh and awk: the adapter's own,
# BusyBox v1.32.1 (Scott, 3 Nov). The hosts' shells and awks accept what the adapter's reject: the Mac's
# and the cross builder's awk ran 5287ffc's newer() fine, and on the adapter it failed for every pair,
# refusing every update.
#
#   tools/busybox_suites.sh        (tools/run_tests.sh runs it)
#
# BusyBox comes from $BUSYBOX, then `busybox` on PATH, then /opt/busybox-1.32.1/busybox; without one
# this says so and exits 0. Its applets are linked into a scratch directory, which the box scripts put
# first on their PATH (OCBM_TEST_PATH: the lines that set PATH honour it; the rest inherit it), and each
# suite runs its box scripts under `busybox sh` (SHELLS). The suites themselves keep the host's tools.
# Not covered: a box script that starts another by its #!/bin/sh line runs that one under the host's
# /bin/sh, with BusyBox's applets still first on PATH.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
BB="${BUSYBOX:-}"
[ -n "$BB" ] || BB="$(command -v busybox 2>/dev/null || true)"
[ -n "$BB" ] || { [ -x /opt/busybox-1.32.1/busybox ] && BB=/opt/busybox-1.32.1/busybox; }
if [ -z "$BB" ] || ! "$BB" true 2>/dev/null; then
  echo "busybox_suites: no busybox on this host; skipped (set BUSYBOX, or install 1.32.1, the adapter's)"
  exit 0
fi
ver="$("$BB" 2>&1 | head -1)"
echo "busybox_suites: $ver"
case "$ver" in *v1.32.1*) ;; *) echo "busybox_suites: not v1.32.1, the adapter's; running anyway" ;; esac

T="$(mktemp -d "${TMPDIR:-/tmp}/busybox_suites.XXXXXX")"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/applets" "$T/shell"
for a in $("$BB" --list); do ln -s "$BB" "$T/applets/$a"; done
printf '#!/bin/sh\nPATH="%s:$PATH" exec "%s" sh "$@"\n' "$T/applets" "$BB" > "$T/shell/busybox-sh"
chmod 755 "$T/shell/busybox-sh"

fail=0
for s in ocbm_stageb_test.sh ocbm_installer_test.sh ocbm_update_test.sh; do
  echo "=== $s under $ver"
  OCBM_TEST_PATH="$T/applets" SHELLS="$T/shell/busybox-sh" bash "$HERE/$s" || fail=1
done
[ "$fail" = 0 ] && echo "busybox_suites: all passed" || { echo "busybox_suites: FAILED"; exit 1; }
