# test_shells.sh — sourced by the box scripts' suites: which of the shells they loop over to run.
#
#   . "$HERE/test_shells.sh"; for sh in ${SHELLS:-$(distinct_shells sh dash)}; do ...
#
# Each shell installed here, once per program: on Linux /bin/sh is dash, and the suites ran every case
# twice under the same shell (about 270 s of the cross builder's run-tests, 5 Nov). On the Mac /bin/sh is
# bash 3.2 and dash is Homebrew's, so both still run. An explicit SHELLS (tools/busybox_suites.sh) is used
# as given. What is skipped, and why, goes to stderr.
distinct_shells() {
  _ds_seen=""
  _ds_out=""
  for _ds_s in "$@"; do
    _ds_p=$(command -v "$_ds_s" 2>/dev/null) || { echo "== $_ds_s: not installed, skipped" >&2; continue; }
    _ds_r=$(readlink -f "$_ds_p" 2>/dev/null) || _ds_r="$_ds_p"
    [ -n "$_ds_r" ] || _ds_r="$_ds_p"
    case " $_ds_seen " in
      *" $_ds_r "*) echo "== $_ds_s: the same program as a shell already run ($_ds_r), skipped" >&2; continue ;;
    esac
    _ds_seen="$_ds_seen $_ds_r"
    _ds_out="$_ds_out $_ds_s"
  done
  echo $_ds_out
}
