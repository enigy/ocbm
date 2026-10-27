#!/bin/sh
# stock_survey_box.sh — read-only survey of a STOCK Carlinkit CPC200-CCPA. tools/stock_survey.sh ships
# it as tmp/once.sh inside an A15W_Update.img (M0 of the one-file install plan).
#
# The stock updater runs tmp/once.sh as root before it copies anything, and gives up when it fails
# (update_box.sh: `once.sh || updateErrThenExit`). So this writes its report to the stick and then
# exits 1 on purpose: nothing on flash changes and the box does not reboot. After a failed update the
# stock stack keeps CarPlay down until the adapter is power-cycled (phone_link_deamon.sh).
#
# It writes only to the stick and to its own /tmp folder. Files that can hold secrets (Wi-Fi
# passphrase, Bluetooth keys, passwords, pairings) are listed by size only: never hashed, never copied.
#
# Bench run on any box, no stick:  STICK=/tmp/survey_bench sh stock_survey_box.sh; echo rc=$?
STICK="${STICK:-/mnt/UPAN}"
W=/tmp/ocbm_survey_w
cd / || exit 1

n=1
while [ -e "$STICK/ocbm_survey_$n" ]; do n=$((n + 1)); done
OUT="$STICK/ocbm_survey_$n"
mkdir -p "$OUT" "$W" || exit 1
R="$OUT/report.txt"
t0=$(cut -d. -f1 /proc/uptime)

sec() { printf '\n=== %s\n' "$1"; }
show() { printf '$ %s\n' "$1"; sh -c "$1" 2>&1; }   # $1 command line, run read-only

secret() {  # $1 path: true when the file may hold a secret (names from a stock 2025.10.15.1127 rootfs)
  case "$1" in
    */shadow*|*/passwd-|*/dropbear/*|*/.ssh/*|*.pem|*.key|*link_key*|\
    /etc/hostapd*.conf|*supplicant*.conf|/etc/riddle.conf|/etc/bluetooth/*.tar.gz|/var/lib/bluetooth/*|\
    /etc/RiddleBoxData/*|/var/lib/lockdown/*) return 0 ;;
  esac
  return 1
}

{
  sec "identity"
  show 'date; cat /proc/uptime'
  show 'cat /etc/software_version; echo; cat /etc/box_product_type; echo; cat /etc/box_version 2>/dev/null; echo'
  show 'uname -a'
  show 'busybox 2>&1 | head -1'
  show 'cat /proc/cmdline'
  show 'grep -E "^(Hardware|Revision|model name|Processor)" /proc/cpuinfo'
  show 'cat /proc/mtd'
  show 'md5sum /dev/mtd0 /dev/mtd1'
  show 'echo "update_status=$(cat /tmp/update_status 2>/dev/null) progress=$(cat /tmp/update_progress 2>/dev/null)"'

  sec "storage"
  show 'df -k'
  show 'cat /proc/mounts'
  show 'free'
  show 'du -xsk / /usr /usr/sbin /usr/lib /lib /etc /script 2>/dev/null'
  show 'ls -la /tmp'

  sec "radio"
  for d in /sys/bus/sdio/devices/*; do
    [ -e "$d" ] || continue
    printf '%s vendor=%s device=%s class=%s\n' "$d" "$(cat "$d/vendor" 2>/dev/null)" \
      "$(cat "$d/device" 2>/dev/null)" "$(cat "$d/class" 2>/dev/null)"
  done
  show 'lsmod'
  show 'ls -la /sys/class/net/ /sys/class/bluetooth/ 2>/dev/null'
  for i in /sys/class/net/*; do
    [ -e "$i/device/driver" ] && printf '%s driver=%s\n' "$i" "$(readlink "$i/device/driver")"
  done
  show 'lsusb'

  sec "led"
  show 'ls -la /sys/class/leds/ /sys/class/gpio/ /sys/class/pwm/ 2>/dev/null'
  for l in /sys/class/leds/*; do
    [ -e "$l" ] || continue
    printf '%s max=%s brightness=%s trigger=%s\n' "$l" "$(cat "$l/max_brightness" 2>/dev/null)" \
      "$(cat "$l/brightness" 2>/dev/null)" "$(cat "$l/trigger" 2>/dev/null)"
  done
  show 'cat /etc/rgbLight.conf 2>/dev/null'
  show 'ls -la /dev'

  sec "processes"
  show 'ps w'
  show 'netstat -lntu 2>/dev/null'
} > "$R" 2>&1

dmesg > "$OUT/dmesg.txt" 2>&1
[ -e /proc/config.gz ] && cp /proc/config.gz "$OUT/kernel_config.gz"

# Inventory of the rootfs (TYPE MODE SIZE MD5|TARGET PATH, tab-separated): the base the installer's
# prune list and its "nothing else changed" check are worked out from on the Mac.
find / -xdev \( -path /proc -o -path /sys -o -path /dev -o -path /tmp -o -path /mnt -o -path /media \) \
  -prune -o -print 2>/dev/null | sort > "$W/paths"
TAB="$(printf '\t')"
while IFS= read -r p; do
  if [ -L "$p" ]; then
    echo "l${TAB}-${TAB}-${TAB}$(readlink "$p")${TAB}$p"
  elif [ -d "$p" ]; then
    echo "d${TAB}$(stat -c %a "$p")${TAB}-${TAB}-${TAB}$p"
  elif [ -f "$p" ]; then
    if secret "$p"; then h=secret; else h=$(md5sum "$p" 2>/dev/null | cut -c1-32); fi
    echo "f${TAB}$(stat -c %a "$p")${TAB}$(stat -c %s "$p")${TAB}${h:--}${TAB}$p"
  else
    echo "o${TAB}-${TAB}-${TAB}-${TAB}$p"
  fi
done < "$W/paths" > "$OUT/inventory.tsv"

echo "seconds=$(( $(cut -d. -f1 /proc/uptime) - t0 )) entries=$(wc -l < "$OUT/inventory.tsv")" > "$OUT/SURVEY_DONE"
rm -rf "$W"
# The updater has already copied the image to /tmp. Renaming it on the stick means the next insert
# finds no image, so start_mass_storage.sh hands the stick through to the host and the report can be
# read without moving the stick, instead of the survey running again.
img="$STICK/$(cat /etc/box_product_type 2>/dev/null)_Update.img"
[ -f "$img" ] && mv "$img" "$img.surveyed"
sync
echo "ocbm survey written to $OUT; stopping the update on purpose" > /dev/console 2>/dev/null
exit 1
