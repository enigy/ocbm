#!/usr/bin/env bash
# stock_restore.sh — put an OCBM adapter back on its stock Carlinkit firmware, from that unit's own
# pre-conversion backup, with no SPI programmer.
#
# INPUT: the backup_pre/ folder `ncm_base_install.sh backup` wrote before anything was stripped:
# mtd0.bin mtd1.bin mtd2.bin rootfs.tar.gz manifest.txt SHA256SUMS. Its rootfs is stock firmware
# plus lvalen's SSH-minimal and NCM scripts, with /script/ncm_only set.
#
# ============================== WHAT IT DOES =============================================
#   preflight  backup checksums; the box answers; mtd0 + mtd1 on the box are byte-identical to the
#              backup's (proves the backup is THIS unit's and the boot stack was never touched);
#              same software_version; room in /tmp
#   stage      push tools/stock_restore_box.sh + rootfs.tar.gz, unpack the backup to /tmp/rs (RAM)
#   plan       read-only on the box: what would change and whether it fits (stock_restore_box.sh plan)
#   dryrun     preflight + stage + plan, then clean /tmp. Works over the OCBM console, so the box
#              stays on OCBM and nothing on flash changes
#   revert     OCBM -> NCM maintenance mode (tools/ocbm_install.sh revert). Reversible
#   apply      over NCM only: make the rootfs match the backup, file by file, in an order where the
#              box still boots NCM if it stops at any point (see stock_restore_box.sh)
#   verify     read-only: the rootfs matches the backup
#   coldtest   reboot; it must come back on NCM by itself; then verify again
#   stock      remove /script/ncm_only and reboot; the adapter must enumerate as stock 1314:1520/1521
#   all        preflight, revert, stage, plan, apply, verify, coldtest, stock — gated before apply
#              and before stock
#
# The only one-way door is `apply`: it deletes OCBM. Before it, `ocbm_install.sh finalize` (or
# removing /script/ncm_only) takes the box straight back to OCBM. After it, getting OCBM back means
# reinstalling it with ncm_base_install.sh + ocbm_install.sh, exactly as the first time.
#
# Recovery behind it: the UART console (TX1/RX1, armed by the NCM scripts too) and an SPI programmer
# with backup_pre/full_nor_16MB.bin. The boot stack (mtd0/mtd1) is never written.
#
# ============================== USAGE ===================================================
#   tools/stock_restore.sh --backup DIR dryrun     # safe any time; the adapter must be on this Mac
#   tools/stock_restore.sh --backup DIR all
#   tools/stock_restore.sh --backup DIR <phase>...
# Options: --yes (skip the two gates), --run-dir DIR (reports; default scratchpad/stockrestore_<stamp>)
# Run under Homebrew bash, like the other installers.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PAYLOAD="$REPO/tools/stock_restore_box.sh"
OCBM_HOST="$REPO/target/release/ocbm-host"
BOXSH="$REPO/tools/boxsh.py"
NCM_IP=192.168.50.2
BACKUP=""
RUN_DIR=""
ASSUME_YES=0
PHASES=()

while [ $# -gt 0 ]; do
  case "$1" in
    --backup) BACKUP="$2"; shift 2 ;;
    --run-dir) RUN_DIR="$2"; shift 2 ;;
    --yes|-y) ASSUME_YES=1; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) PHASES+=("$1"); shift ;;
  esac
done
[ ${#PHASES[@]} -gt 0 ] || { echo "no phase given (try --help)" >&2; exit 2; }
[ -n "$BACKUP" ] || { echo "--backup DIR is required (the backup_pre folder of THIS adapter)" >&2; exit 2; }
BACKUP="$(cd "$BACKUP" && pwd)"
[ -n "$RUN_DIR" ] || RUN_DIR="$REPO/scratchpad/stockrestore_$(date +%Y%m%d_%H%M%S)"
mkdir -p "$RUN_DIR"

say()  { printf '[stock-restore] %s\n' "$*"; }
warn() { printf '[stock-restore] !! %s\n' "$*" >&2; }
die()  { printf '[stock-restore] ABORT: %s\n' "$*" >&2; exit 1; }
gate() {
  [ "$ASSUME_YES" = 1 ] && return 0
  printf '[stock-restore] %s [type yes] ' "$1"; read -r a
  [ "$a" = yes ] || die "stopped at your request"
}

# ------------------------------------------------------------------------------------------
# Transport. The adapter on this Mac is in one of three states, told apart by USB id and NCM:
#   ocbm   1314:2d00 — root console over OCBM (tools/ocbmcmd.py), files via ocbm-host push/pull
#   ncm    192.168.50.2 answers — ssh (key from ~/.ssh/config), telnet :23 as the fallback
#   stock  1314:1520 or 1314:1521 — Carlinkit firmware; nothing left to do
# ------------------------------------------------------------------------------------------
VIA=""
OCBM_WARM=0
usb_has() { ioreg -p IOUSB -w0 -l 2>/dev/null | grep -q "\"idProduct\" = $1\$"; }
ssh_ok() { ssh -n -o ConnectTimeout=6 -o BatchMode=yes "root@$NCM_IP" true >/dev/null 2>&1; }
telnet_ok() { python3 "$BOXSH" --host "$NCM_IP" --timeout 8 probe >/dev/null 2>&1; }

# The first console attach can swallow a command (the 2026-10-27 dry run lost its first md5),
# so wait for one round trip before trusting the console's answers.
ocbm_warm() {
  [ "$OCBM_WARM" = 1 ] && return 0
  local i
  for i in 1 2 3 4 5; do
    if python3 "$REPO/tools/ocbmcmd.py" --timeout 15 'echo OCBM_$((40+2))' 2>/dev/null | grep -q OCBM_42; then
      OCBM_WARM=1; return 0
    fi
    sleep 3
  done
  die "the adapter is on OCBM but its console does not answer"
}

detect() {
  VIA=""
  if usb_has $((16#2d00)); then VIA=ocbm; ocbm_warm
  elif ssh_ok; then VIA=ssh
  elif telnet_ok; then VIA=telnet
  elif usb_has $((16#1520)) || usb_has $((16#1521)); then VIA=stock
  fi
}

run() {  # $1 command, $2 timeout seconds; prints output, returns the box-side status
  local t="${2:-60}"
  case "$VIA" in
    ocbm)   python3 "$REPO/tools/ocbmcmd.py" --timeout "$t" "$1" ;;
    ssh)    ssh -o ConnectTimeout=8 -o ServerAliveInterval=5 -o ServerAliveCountMax=6 "root@$NCM_IP" "$1" ;;
    telnet) python3 "$BOXSH" --host "$NCM_IP" --timeout "$t" run "$1" ;;
    *)      die "no control channel to the adapter (state: ${VIA:-none})" ;;
  esac
}

push() {  # $1 local, $2 remote
  case "$VIA" in
    ocbm)   "$OCBM_HOST" push 1314 2d00 "$1" "$2" 644 >/dev/null ;;
    ssh)    ssh -o ConnectTimeout=8 "root@$NCM_IP" "cat > '$2'" < "$1" ;;
    telnet) python3 "$BOXSH" --host "$NCM_IP" --mode 644 put "$1" "$2" >/dev/null ;;
    *)      die "no control channel to the adapter" ;;
  esac
}

pull() {  # $1 remote, $2 local
  case "$VIA" in
    ocbm)   "$OCBM_HOST" pull 1314 2d00 "$1" "$2" >/dev/null ;;
    ssh)    ssh -o ConnectTimeout=8 "root@$NCM_IP" "cat '$1'" > "$2" ;;
    telnet) python3 "$BOXSH" --host "$NCM_IP" --timeout 120 get "$1" "$2" >/dev/null ;;
    *)      die "no control channel to the adapter" ;;
  esac
}

# A lookup that finds nothing must return empty, not trip set -e: the caller's die names the problem.
box_md5() { run "md5sum '$1' 2>/dev/null | cut -c1-32" 120 | tr -d ' \r\n' | grep -oE '[0-9a-f]{32}' | tail -1 || true; }

need_ncm() {
  detect
  case "$VIA" in ssh|telnet) ;; *) die "this phase needs NCM maintenance mode (state: ${VIA:-none}); run the revert phase first" ;; esac
}

wait_ncm() {  # $1 seconds
  local end=$(( SECONDS + $1 ))
  while [ $SECONDS -lt $end ]; do
    if ssh_ok; then VIA=ssh; return 0; fi
    if telnet_ok; then VIA=telnet; return 0; fi
    sleep 5
  done
  return 1
}

# ------------------------------------------------------------------------------------------
phase_preflight() {
  say "=== preflight"
  local f
  for f in mtd0.bin mtd1.bin rootfs.tar.gz manifest.txt SHA256SUMS; do
    [ -s "$BACKUP/$f" ] || die "backup is missing $f ($BACKUP)"
  done
  ( cd "$BACKUP" && shasum -a 256 -c SHA256SUMS >/dev/null 2>&1 ) || die "backup checksums do not verify; do not restore from it"
  say "backup checksums verify: $BACKUP"
  local want_sw; want_sw="$(sed -n 's/^sw=//p' "$BACKUP/manifest.txt" | head -1)"
  grep -q 'state=NCM-IMG (pre-strip' "$BACKUP/manifest.txt" || die "this backup was not taken before the strip (manifest: $(head -1 "$BACKUP/manifest.txt"))"

  detect
  case "$VIA" in
    ocbm) say "adapter: on OCBM (console over USB)" ;;
    ssh|telnet) say "adapter: on NCM maintenance mode ($VIA)" ;;
    stock) die "the adapter already enumerates as stock Carlinkit (1314:1520/1521)" ;;
    *) die "no adapter found: plug it into this Mac (OCBM 1314:2d00, or NCM at $NCM_IP)" ;;
  esac

  local m0 m1 b0 b1 sw tmpfree
  b0="$(md5 -q "$BACKUP/mtd0.bin")"; b1="$(md5 -q "$BACKUP/mtd1.bin")"
  m0="$(box_md5 /dev/mtd0)"; m1="$(box_md5 /dev/mtd1)"
  [ -n "$m0" ] && [ -n "$m1" ] || die "could not read /dev/mtd0 and /dev/mtd1 on the box"
  [ "$m0" = "$b0" ] && [ "$m1" = "$b1" ] \
    || die "mtd0/mtd1 on the box differ from the backup's: this backup is NOT from this adapter (box $m0/$m1, backup $b0/$b1)"
  say "identity: mtd0 and mtd1 match the backup byte for byte"
  sw="$(run 'cat /etc/software_version' 20 | tr -d '\r' | grep -E '^[0-9]{4}\.' | tail -1 || true)"
  [ "$sw" = "$want_sw" ] || die "software_version on the box is '$sw', the backup's is '$want_sw'"
  say "software_version: $sw"
  tmpfree="$(run "df -k /tmp | awk 'NR>1{f=\$4} END{print f+0}'" 20 | tr -d '\r' | grep -E '^[0-9]+$' | tail -1 || true)"
  [ "${tmpfree:-0}" -ge 40000 ] || die "only ${tmpfree:-?} KB free in /tmp; staging needs about 40 MB"
  say "/tmp: ${tmpfree} KB free"
  run 'df -k / | tail -1' 20 | tr -d '\r' | sed 's/^/[stock-restore] rootfs: /' | tail -1
}

phase_stage() {
  say "=== stage — backup tree into RAM on the box"
  detect
  [ "$VIA" != stock ] && [ -n "$VIA" ] || die "no control channel to the adapter"
  push "$PAYLOAD" /tmp/stock_restore_box.sh
  push "$BACKUP/rootfs.tar.gz" /tmp/rootfs.tar.gz
  local want got
  want="$(md5 -q "$BACKUP/rootfs.tar.gz")"; got="$(box_md5 /tmp/rootfs.tar.gz)"
  [ "$want" = "$got" ] || die "rootfs.tar.gz did not arrive intact (box $got, host $want)"
  [ "$(md5 -q "$PAYLOAD")" = "$(box_md5 /tmp/stock_restore_box.sh)" ] || die "stock_restore_box.sh did not arrive intact"
  run 'rm -rf /tmp/rs /tmp/rsw && mkdir -p /tmp/rs && tar -xzf /tmp/rootfs.tar.gz -C /tmp/rs && rm -f /tmp/rootfs.tar.gz && echo STAGED' 300 \
    | grep -q STAGED || die "could not unpack the backup on the box"
  say "staged: /tmp/rs ($(run 'du -sk /tmp/rs | cut -f1' 60 | tr -d '\r' | grep -E '^[0-9]+$' | tail -1) KB)"
}

phase_plan() {
  say "=== plan — read-only"
  detect
  run 'sh /tmp/stock_restore_box.sh plan > /tmp/rs_plan.out 2>&1; echo rc=$?' 900 | grep -q 'rc=0' || warn "plan exited non-zero"
  pull /tmp/rs_plan.out "$RUN_DIR/plan.out"
  pull /tmp/rsw/plan.txt "$RUN_DIR/plan.txt" || true
  grep -E '^\[restore\]' "$RUN_DIR/plan.out" | grep -v -E 'full plan' || true
  grep -q RESTORE_PLAN_OK "$RUN_DIR/plan.out" || die "plan failed; see $RUN_DIR/plan.out"
  grep -q 'CONFLICTS' "$RUN_DIR/plan.out" && die "the plan has type conflicts; apply would refuse. See $RUN_DIR/plan.out"
  say "full plan saved: $RUN_DIR/plan.txt"
}

phase_cleanup() {
  run 'rm -rf /tmp/rs /tmp/rsw /tmp/rs_plan.out /tmp/stock_restore_box.sh /tmp/rootfs.tar.gz; echo CLEAN' 60 | grep -q CLEAN \
    && say "cleaned /tmp on the box" || warn "could not clean /tmp on the box (it is RAM; a reboot clears it)"
}

phase_revert() {
  say "=== revert — OCBM to NCM maintenance mode"
  detect
  case "$VIA" in
    ssh|telnet) say "already on NCM"; return 0 ;;
    ocbm) /opt/homebrew/bin/bash "$REPO/tools/ocbm_install.sh" revert ;;
    *) die "no adapter on OCBM or NCM" ;;
  esac
  wait_ncm 60 || die "the adapter did not come back on NCM"
  say "on NCM ($VIA)"
}

phase_apply() {
  say "=== apply — rewrite the rootfs to match the backup"
  need_ncm
  run '[ -e /script/ncm_only ] || [ -e /script/ncm_wifi ]; echo flag=$?' 20 | grep -q 'flag=0' \
    || die "no NCM flag on the box; refusing (it would boot an OCBM box without OCBM)"
  run '[ -d /tmp/rs ] && [ -s /tmp/stock_restore_box.sh ] && echo READY' 20 | grep -q READY || phase_stage
  gate "Apply now? This deletes OCBM and restores the Carlinkit files on the adapter."
  run 'mkdir -p /tmp/rsw; rm -f /tmp/rsw/apply.log; (nohup sh /tmp/stock_restore_box.sh apply > /tmp/rsw/apply.log 2>&1 &) ; echo STARTED' 20 \
    | grep -q STARTED || die "could not start apply on the box"
  local end=$(( SECONDS + 1200 )) seen=0 log
  while [ $SECONDS -lt $end ]; do
    sleep 5
    log="$(run 'cat /tmp/rsw/apply.log 2>/dev/null' 30 2>/dev/null || true)"
    if [ -z "$log" ] && ! ssh_ok; then telnet_ok && VIA=telnet; fi
    printf '%s\n' "$log" | tr -d '\r' | grep -E '^\[restore\] [0-9]/6|^RESTORE_' | tail -n +$((seen + 1)) | while IFS= read -r l; do say "box: $l"; done
    seen=$(printf '%s\n' "$log" | tr -d '\r' | grep -c -E '^\[restore\] [0-9]/6|^RESTORE_' || true)
    if printf '%s' "$log" | grep -q RESTORE_DONE; then break; fi
    if printf '%s' "$log" | grep -q RESTORE_ABORT; then
      printf '%s\n' "$log" > "$RUN_DIR/apply.log"
      die "apply stopped on the box: $(printf '%s\n' "$log" | grep ABORT: | tail -1). The box still boots NCM; fix the cause and re-run apply. Log: $RUN_DIR/apply.log"
    fi
  done
  printf '%s\n' "$log" > "$RUN_DIR/apply.log"
  printf '%s' "$log" | grep -q RESTORE_DONE || die "apply did not finish within 20 minutes; log so far: $RUN_DIR/apply.log"
  say "apply done; log: $RUN_DIR/apply.log"
}

phase_verify() {
  say "=== verify"
  detect
  run '[ -d /tmp/rs ] && [ -s /tmp/stock_restore_box.sh ] && echo READY' 20 | grep -q READY || phase_stage
  run 'sh /tmp/stock_restore_box.sh verify > /tmp/rs_verify.out 2>&1; echo rc=$?' 900 >/dev/null || true
  pull /tmp/rs_verify.out "$RUN_DIR/verify.out"
  grep -q RESTORE_VERIFIED "$RUN_DIR/verify.out" || { sed -n '1,30p' "$RUN_DIR/verify.out"; die "the rootfs does not match the backup"; }
  say "the rootfs matches the backup"
}

phase_coldtest() {
  say "=== coldtest — reboot; it must come back on NCM by itself"
  need_ncm
  run 'sync; (sleep 1; /sbin/reboot) >/dev/null 2>&1 & echo REBOOTING' 20 >/dev/null || true
  sleep 20
  wait_ncm 300 || die "no NCM after the reboot. The UART console (TX1/RX1, 115200) and the SPI image are the way in now"
  say "back on NCM ($VIA)"
  run 'cat /etc/software_version; [ -x /usr/sbin/ARMadb-driver ] && echo VENDOR_OK; [ -e /script/ncm_only ] && echo FLAG_OK; pidof ocbmd >/dev/null || echo NO_OCBMD' 30 \
    | tr -d '\r' > "$RUN_DIR/coldtest.txt"
  grep -q VENDOR_OK "$RUN_DIR/coldtest.txt" && grep -q FLAG_OK "$RUN_DIR/coldtest.txt" && grep -q NO_OCBMD "$RUN_DIR/coldtest.txt" \
    || die "the box came back but does not look restored: $(tr '\n' ' ' < "$RUN_DIR/coldtest.txt")"
  phase_stage
  phase_verify
}

phase_stock() {
  say "=== stock — leave NCM mode and boot the Carlinkit stack"
  need_ncm
  gate "Switch to stock now? NCM goes away; afterwards the adapter is reachable only over its Wi-Fi AP or the UART."
  run 'rm -f /script/ncm_only /script/ncm_wifi; sync; (sleep 1; /sbin/reboot) >/dev/null 2>&1 & echo REBOOTING' 20 >/dev/null || true
  local end=$(( SECONDS + 240 ))
  while [ $SECONDS -lt $end ]; do
    if usb_has $((16#1520)) || usb_has $((16#1521)); then
      say "the adapter enumerates as stock Carlinkit (1314:$(usb_has $((16#1520)) && echo 1520 || echo 1521))"
      say "done. To make it factory-clean as well, put Carlinkit's 2025.10.15.1127 A15W_Update.img on a FAT32 stick and plug it in on a USB charger."
      return 0
    fi
    sleep 5
  done
  die "no stock enumeration within 4 minutes. Unplug and replug the adapter; if it stays silent, the UART console is the way in"
}

run_phase() {
  case "$1" in
    preflight) phase_preflight ;;
    stage)     phase_stage ;;
    plan)      phase_plan ;;
    dryrun)    phase_preflight; phase_stage; phase_plan; phase_cleanup
               say "dry run complete: nothing on the adapter's flash changed" ;;
    revert)    phase_revert ;;
    apply)     phase_apply ;;
    verify)    phase_verify ;;
    coldtest)  phase_coldtest ;;
    stock)     phase_stock ;;
    all)       phase_preflight; phase_revert; phase_stage; phase_plan; phase_apply; phase_verify
               phase_coldtest; phase_stock ;;
    *) die "unknown phase: $1" ;;
  esac
}

say "run dir: $RUN_DIR"
for ph in "${PHASES[@]}"; do run_phase "$ph"; done
