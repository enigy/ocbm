# Recovery

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 04; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

Getting a bricked or wedged box back, without touching the signed boot stack.

## Recovery procedures

<!-- absorbed: ../ops/01_RECOVERY.md -->

### Recovery tiers (most robust first)

| Tier | Channel | Survives | Needs |
|---|---|---|---|
| 1 | **SPI programmer** (hardware) | any state (brick-proof) | physical flash access |
| 2 | **UART console** (`ttymxc0`, TX1/RX1 pads) | dead userspace, wedged gadget, hung projection | kernel alive |
| 3 | **OCBM `CONSOLE` mode** (this project) | any failure *downstream of the mode branch* — MFi/WiFi/iAP2/receiver | OCBM daemon + accessory gadget alive |
| 4 | NCM telnet (no login) / NCM SSH (no password) / the same two over the Wi-Fi AP | least | gadget + netdev + DHCP + daemon, or hostapd + radio |

Each lower tier rescues cases the tiers above it cannot. OCBM `CONSOLE` is *much* more robust than
NCM/WiFi (it touches the network stack zero times and rides the primary transport) but does **not**
replace UART (kernel-level) or the programmer (hardware) — it shares fate with the gadget + daemon.

### OCBM `CONSOLE` mode

A host-selected state, not a background channel (see [`../carplay/01_OCBM_PROTOCOL.md`](../carplay/01_OCBM_PROTOCOL.md)
§ Modes). At connect the host sends `MODE_SELECT { CONSOLE }`; the box branches **before any
projection machinery** and wires the accessory pipe straight to a root PTY (`/bin/sh`, root — the
daemon already runs as root). The earlier the branch, the fewer subsystems it depends on, the more
failures it survives.

Solves the two target scenarios directly:
- **"NCM won't activate"** — no `ncm0`, no DHCP, no netdev, no `dropbear`. Rides the accessory
  gadget the box already presents.
- **"Wi-Fi AP won't come online"** — no `hostapd`, no radio.

Rescue tool: **`ocbm-host console`** — there is no separate `ocbm-rescue` binary; that role shipped as
a subcommand of the one host client (`mode_console`, `host/ocbm-host/src/main.rs:375`), which claims
the accessory interface, sends `MODE_SELECT { CONSOLE }`, and bridges the PTY to your terminal (raw
mode). It works even if the main host app is broken, and `tools/ocbm_install.sh finalize` relies on it
for the first-boot dead-man confirm. *(Its log lines still carry the historical `[ocbm-rescue]` tag,
`main.rs:391`.)* Nice-to-haves for cheap: a `resize` TLV (window size/`TERM`), multiple sessions via a
session id in the CONSOLE payload. (Neither exists today; there is no `streamId` field anywhere in
OCBM — see docs/carplay/01_OCBM_PROTOCOL.md §Self-describing streams.)

#### Access model (matches the stock firmware's existing open-access posture)

The console mode adds no additional authentication because the platform already grants unauthenticated local access (see below). The stock vendor firmware already provides open local access (an SSH/telnet login with no password set, a root serial console, a read-write rootfs, and field-reflashability), so local physical access already implies full control by design. Providing a console over the accessory link therefore does not change the access model — it uses the same physical-USB trust boundary. Real protection
would require deep encryption that is explicitly out of scope (and the HAB/OTPMK floor sits below
all of it regardless). `MODE_SELECT` also prevents accidental entry: default is `PROJECTION`; only
our host requests `CONSOLE`.

### Install / deploy models

Getting the open userspace onto a unit needs **no credential** — the access model above *is* the
install model. The unit ships a single account (root) with no password set, and `rcS` re-arms both
shell servers on every boot, so this is the standing configuration rather than a bootstrap window
that closes behind itself.

1. **Live overlay** — take a root shell over USB-NCM (unauthenticated telnet is the default transport
   for the base install; SSH with the unset root password is preferred by `tools/ocbm_install.sh` for
   real exit status and single-`cat` transfers — **no keys or certificates are involved on either
   side**), then copy the daemons onto the read-write jffs2 rootfs. No programmer required. Best for
   iteration. The optional Wi-Fi-AP backstop is the one place a credential exists, and only at the
   link layer: a device-derived WPA2 PSK. The shell behind it is the same open login.
2. **Flashed `mtd2`** — bake the daemons into a custom jffs2 rootfs image, program it with the SPI
   programmer. Reproducible image + guaranteed brick recovery. Best for release.
3. **One-file install** — a STOCK 2025.10.15.1127 unit takes `A15W_Update.img` from a USB stick
   (`tools/make_ocbm_installer.sh`; the stock updater runs its `tmp/once.sh` as root). No Mac and no
   shell. It backs the flash up to the stick, deletes the HiCar/Mirror/web extras, places OCBM and
   arms a trial with stock CarPlay kept: `/script/ocbm_mode` goes `trial` → `confirmed` (an app that
   offers the way back sends `MGMT_INSTALL_CONFIRM`) → `final` (the Carlinkit stack is removed once
   that session ends). The scripts and their contract are in [`../../ccpa/README.md`](../../ccpa/README.md)
   `installer/`; first proven on a factory unit 2026-10-28.

The partition layout is fixed by the vendor kernel/U-Boot (`mtd0` 256 K uboot / `mtd1` 3328 K
kernel / `mtd2` 12800 K rootfs); you control everything inside `mtd2`. See
[`../ops/00_BUILD_AND_DEPLOY.md`](../ops/00_BUILD_AND_DEPLOY.md).

#### Persistent FHS install — the layout that ships

**Commission a unit with `tools/ncm_base_install.sh` then `tools/ocbm_install.sh`**, whose
`manifest()` is the authoritative file set. `tools/install_fhs.sh` is NOT the commissioning path any
more — it is the quick live-overlay refresh of an already-commissioned box. The FHS layout it lays
down is still what ships:

The rootfs is **jffs2 (rw)**, so the open userspace persists across reboots. `tools/install_fhs.sh`
lays it out FHS-style (not binaries-in-`/script`) and repoints the boot hook:

- **daemons → `/usr/sbin/`** — `ocbmd`, `iap2d`, `carplayd`, `rx-connect`;
- **tools → `/usr/bin/`** — `iap_role_switch`;
- **scripts → `/script/`** — `ocbm_boot.sh` (boot hook, launches ocbmd + `session_supervisor.sh`),
  `session_supervisor.sh` (the lifecycle actor), `projection_up.sh` (IDLE→projection bring-up),
  `phone_reset.sh`, `peer_store.sh`, `carplay-status.sh`, and the PID-1 respawn wrappers
  `run_ocbmd.sh` / `run_supervisor.sh` (`install_fhs.sh`'s script loop is the authoritative list).

The installer is ETXTBSY-safe and never drops exec bits. Once installed, a **cold boot comes up with a
live OCBM link and zero bootstrapping** — reboot-proven (Mac gets `HELLO_ACK` with no UART base64
deploy). Note the box `/tmp` is tmpfs (lost on reboot); only the FHS-installed files survive.

#### Boot chain (turnkey — how the box reaches a live OCBM link)

`busybox init` → `/etc/inittab` → **`::sysinit /script/early_console.sh`** (UART root console, FIRST —
the Tier-2 recovery path) + `/etc/init.d/rcS` (mounts, dropbear/telnetd, mdev, network) →
**`/script/start_main_service.sh &`** → (no `ncm_only`/`ncm_wifi` flag ⇒ OCBM appliance default)
**`/script/ocbm_boot.sh`** → arms the host-facing `accessory` gadget (bDeviceClass=0, PID `0x2d00`) →
launches **`/usr/sbin/ocbmd`** + `session_supervisor.sh` (idle-waiting on `/tmp/host_present`).
`ocbm_boot.sh` backgrounds and `exit 0`s so it can never block boot. PID 1 then keeps both alive
independently of that launch: `/etc/inittab` carries `::respawn:/script/run_ocbmd.sh` and
`::respawn:/script/run_supervisor.sh` (task #28) — wrappers that relaunch only a *dead* daemon, so a
crash or OOM costs a restart, not a reboot. `tools/ocbm_install.sh finalize` is what appends them, and
it refuses to touch `inittab` unless `/script/run_ocbmd.sh` is present and executable.
On-demand pieces (iap2d cold-start
via `projection_up.sh`, carplayd via the supervisor) are **not** started at boot — the box
boots to IDLE and only projects when a host app SUBSCRIBEs (see [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md)
and [`../carplay/07_PHONE_SIDE.md`](../carplay/07_PHONE_SIDE.md)).

### Back to stock Carlinkit firmware

`tools/stock_restore.sh` puts a converted box back on its stock firmware with no SPI programmer, from
that unit's own `backup_pre/` (written by `ncm_base_install.sh backup` before anything was stripped).
That backup is stock 2025.10.15.1127 plus lvalen's SSH-minimal and NCM scripts with `/script/ncm_only`
set, so that is the state it restores; removing the flag then boots the Carlinkit stack.

    tools/stock_restore.sh --backup <backup_pre> dryrun   # over the OCBM console; flash untouched
    tools/stock_restore.sh --backup <backup_pre> all      # revert to NCM, apply, coldtest, stock

- **Identity gate.** `/dev/mtd0` and `/dev/mtd1` on the box must be byte-identical to the backup's,
  which proves the backup belongs to this unit and the boot stack was never written.
- **File by file, never `flashcp` of `mtd2`.** Rewriting the partition the system runs from has no
  safe stopping point. `tools/stock_restore_box.sh` instead makes the rootfs match the unpacked backup
  in an order that still boots NCM if it stops anywhere: OCBM's binaries out (that is the room), files
  in (each `.rsnew`, md5-checked, renamed, free space checked first), the five boot files, then the
  `/script` and `/etc` extras (the owned `inittab` names `early_console.sh`, so it goes only after
  `inittab` is back), and the login files last. It never touches `proc sys dev tmp mnt media`, the NCM
  flags, or runtime state under `var/run`, `var/log`, `var/lock` and `var/tmp`, and it is idempotent:
  re-run `apply` after any abort. `apply` refuses on a live box without an NCM flag.
- **One-way door.** Up to `apply`, `ocbm_install.sh finalize` returns the box to OCBM. After it,
  OCBM comes back only by reinstalling it from scratch.
- **Tests.** `tools/stock_restore_test.sh` (run by `tools/run_tests.sh`) exercises plan, apply,
  verify, the space abort, idempotence and type conflicts against synthetic trees, under `sh` and
  `dash`.

### Back to stock from a one-file install

While stock is still on the unit (`trial` or `confirmed`), `ccpa/installer/ocbm_revert.sh` is the one
way back, and five things call it:

- the start-up self-test failing (the gadget is not `1314:2d00`, no `/dev/usb_accessory`, no `ocbmd`,
  or `radio_detect.sh` finds no radio bring-up for this unit);
- two starts in a row that never passed the self-test (a hang or a reboot loop), at the next start;
- a file named `OCBM_RESTORE` on a stick, at any time during the trial (renamed `.done` once used);
- `MGMT_GO_STOCK` from the app, or `ocbm-host install stock` on the bench;
- by hand over the OCBM console: `/script/ocbm_revert.sh "why"`.

It puts `/etc/hostapd.conf` back from `/etc/ocbm/identity.tgz`, and the unit's HiCar identity
(`/etc/hichain`, archived by installs since 31 Oct) with it, and the Bluetooth name the seam rewrote
(`radio_hal.sh bt_set_name` writes `ccpa-<4 hex>` into `/etc/bluetooth_name` and into the flag
`/etc/.custom_bluetooth_name`, and while the flag is there stock's bluetoothDaemon applies that name):
`/etc/bluetooth_name` from the archive, the flag as the install found it (archived since 31 Oct), or
removed when it holds the seam's `ccpa-<4 hex>`; a name set on stock stays. Each comes back with
`cp -p`, mode and owner as the install found them (a plain copy left the dev adapter's 0777
`hostapd.conf` 0755, 31 Oct). It records `reverted version=<VERSION>`
in `/etc/ocbm/last_result.txt` (a stick install then refuses that version unless the stick holds
`OCBM_RETRY`, which an install that uses it renames `OCBM_RETRY.used`; an install over USB from the
app is not checked, since each one is a press of Convert),
removes `/script/ocbm_mode` and restarts. The start script then hands every boot to the
kept `start_main_service.stock.sh`, and the report lands on the next stick plugged in as
`OCBM_RESULT.txt`. HiCar, the Android Mirror helper and the web pages stay deleted. Carlinkit's own
2025.10.15.1127 image puts back what it ships, but not all of it lands: it unpacks its web pages INTO
`/etc/boa/www` (`rm -rf /etc/boa/www/*; tar -xvf www.tar.gz -C /etc/boa/www/`), which an install
before 31 Oct deleted, so the tar failed silently and the web UI stayed blank; and it ships only an
empty `/etc/hichain`, so the unit's HiCar identity (`hks_keystore`, `info1.data`, `info2.data`) can
only come from a backup (a tester, Oct 2026). Since 31 Oct the prune empties those directories instead
of deleting them, and the go-back makes them again where an older install deleted them. The
toolchain leftovers and `test_mic.sh` the prune also deletes are not in Carlinkit's image either;
they are harmless. Everything the prune took is in the first install's flash dump, which the restore
below can write back.

In `final` there is no stock left and `ocbm_revert.sh` refuses. Carlinkit's image cannot rebuild it
either: the finish deletes the stock updater that would run it (`update_box.sh` and
`update_box_ota.sh` in `ocbm_finish.sh`'s `SCRIPT_A`, and the updater call in the stick handlers).
(This read "its own decrypter `ARMimg_maker` among them" until 31 Oct: `ARMimg_maker` is in
`ncm_base_install.sh`'s `SBIN_B`, which only the deep NCM strip removes, and the finish uses
`SBIN_A`; a tester's `final` adapter still had it, and the stick updater worked after the restore.)
The way back without an SPI programmer is `tools/stock_restore.sh --profile onefile`, over the OCBM
console, from one of the unit's own backups:

    tools/stock_restore.sh --profile onefile --backup OCBM_BACKUP/<serial> --dump dryrun   # flash untouched
    tools/stock_restore.sh --profile onefile --backup OCBM_BACKUP/<serial> --dump all
    tools/stock_restore.sh --profile onefile --backup OCBM_BACKUP/<serial> [--archive FILE] all

- **The source.** `--dump` decodes the stick's own `OCBM_BACKUP/<serial>/mtd2.bin` on the Mac
  (`tools/jffs2_extract.py`), so nothing beyond what every install writes to its stick is needed. The
  first install dumps the flash before it changes anything, so its dump is the factory rootfs, the
  pruned extras and the HiCar identity included: the way back to full stock, and the only one for a
  `final` adapter that never had an archive (nothing in the repo writes one). `--archive` takes a
  file-level archive the unit took of its own rootfs during its trial (`rootfs_files_trial.tgz`); it
  never has the extras. A dump taken by a later install holds stock plus a dormant OCBM and is treated
  like an archive.
- **The decoder.** Carlinkit's 3.14.52 kernel stores most data nodes with compression type 22
  (`jffs2_zlib_decompress_aesmykey`, `0x80129ffc` in lvalen91/CPC200-CCPA_resources
  `custom/kernel_dumps/`): swap the bytes `0x32` and `0x60` in the payload, then plain zlib. zlib's
  adler32 is the proof the swap is right: a raw inflate can come out the right length with the wrong
  bytes, and `jefferson` writes type-22 data out raw. A few nodes are rtime (type 2), decoded in the
  tool itself. Every node's header, node and data CRC is checked; the highest version of each entry
  wins; a file is written only when every node it needs decodes, else the restore refuses the dump.
  From a tester's `ccpa_jffs2_extract.py`, which decoded all 2303 type-22 nodes of an IW416 unit. On the
  factory adapter's 28 Oct dump (31 Oct): 2,255 type-22, 5 rtime and 2,350 plain nodes, no bad CRC,
  409 files, 433 links and 61 directories; against the read-only survey of 27 Oct, 859 of 879 entries
  identical, and the 20 others (`riddle.conf`, `hostapd.conf`, the Bluetooth and Wi-Fi names, the
  pairing records, the logos, `shadow`) byte-identical to the trial archive of 28 Oct, so changed by
  use between the two days, not by the decoder.
- **The target.** `tools/stock_restore_onefile_tree.sh` makes stock of the source: from a trial's
  rootfs, OCBM's payload (`make_ocbm_installer.sh --list`), `/etc/ocbm/`, `/etc/carplay/` (OCBM's bond
  store), `/script/ocbm_mode` and OCBM's runtime files (`/etc/carplay_peers.bin`, its peer store,
  among them) dropped, `/etc/hostapd.conf` and `/etc/bluetooth_name` from the identity archive and the
  seam's Bluetooth name flag dropped unless the install found one (as `ocbm_revert.sh` does), and
  `start_main_service.stock.sh` back as `start_main_service.sh`;
  a dump from before any install is taken as it is. The tree always has `etc/boa/www`,
  `etc/boa/cgi-bin`, `etc/boa/images` and `etc/hichain`, so a restored unit takes Carlinkit's image
  whole. It holds the Wi-Fi passphrase and Bluetooth keys, so the stage step makes it in a private temp
  dir that goes on any exit.
- **The pack.** `tools/pack_tree.py` packs the tree with each entry owned as on the adapter, from the
  source: Carlinkit's image leaves most of the rootfs to uid 1000 (1000:0 and 1000:1000, a few root,
  three files 501:20). A plain `tar` from the Mac recorded the Mac's user, and the box, unpacking as
  root and copying with `cp -p`, put 502:20 on flash (a tester, Oct 2026). It writes no `._`
  AppleDouble entry (macOS `tar` adds one per file with extended attributes; the factory adapter's
  first dry run, 28 Oct, planned 702 of them as writes) and refuses a pack that is not exactly the tree
  or not owned as it says. The box's busybox `tar` sets no symlink's owner when it unpacks, though (it
  skips the step for links), so the links in `/tmp/rs` are root's whatever the image has: the stage
  sends each link's owner beside the tree as `/tmp/rs.links`, read from the archive's own headers
  (`pack_tree.py links`; the NCM profile reads its `rootfs.tar.gz` the same way), and the restore gives
  every link it makes that owner with `chown -h`. `plan` names, and `apply` refuses before changing
  anything, a link the list gives no owner for.
- **The order** (`PROFILE=onefile` in `tools/stock_restore_box.sh`). First room: OCBM's programs other
  than `ocbmd`, which carries the console, go (a trial has stock and OCBM on flash, about 1.3 MB free,
  and the extras need about 1.8 MB to come back; this frees about 2 MB, and OCBM still boots its
  console from the start script, `ocbm_boot.sh` and `ocbmd`). Then every stock file goes back
  (`.rsnew`, md5-checked, renamed, free space checked first; a directory made back takes the tree's
  mode and owner, where a bare `mkdir` left the dev adapter's `etc/boa/www`, `cgi-bin`, `images` and
  `etc/hichain` 755 instead of 775, 31 Oct, and a link the owner `/tmp/rs.links` gives, where a bare
  `ln -s` left the dev adapter's 20 library links root's that evening); a check stops the run if
  anything but the start script and OCBM's own files still differs; one rename of the stock start
  script over the dispatcher is the switch; OCBM's files go after it, each through `is_radio()` except OCBM's own
  radio seam (`radio_ap_up.sh`, `radio_detect.sh`, `radio_hal.sh`: stock has none, and without OCBM
  they are dead files; the radio guard still keeps every file of the unit's own) and the seam's
  Bluetooth name flag, `/etc/.custom_bluetooth_name` while it holds `ccpa-<4 hex>` (its name matches
  the guard's `*bluetooth*`, so the dev adapter's restore of 31 Oct kept it and verify passed it by;
  a name set on stock is not `ccpa-<4 hex>` and stays), and then `/etc/carplay`. The OCBM still running (`ocbmd` carries the console until the restart) can write
  `/etc/carplay/bt_link_keys` again: verify tolerates it, and the `stock` phase removes it once more
  right before the restart. Every stopping point boots OCBM, where the restore runs again, or complete
  stock.
- **The monitor.** `apply` starts the box side under `nohup` and polls its log. Under
  `set -euo pipefail`, a poll that found no step line yet ended the script silently right after
  `=== apply`, the box side running on through the switch unwatched and `verify` and `stock` never run
  (a tester, Oct 2026); the step filter (`new_steps`) cannot fail that way now.
- **Refusals**, before anything changes: the start script is not OCBM's dispatcher, `ocbmd` or
  `ocbm_boot.sh` is missing, the mode is not `trial` or `final` (a `confirmed` trial finishes when its
  session ends), a finish is pending, or `ocbm_mode.lock` is held (`ocbm_revert.sh` and
  `ocbm_finish.sh` take the same lock). The identity gate (`mtd0`/`mtd1` against the stick's dump) and
  `software_version` against the source's are checked first; a dump that does not decode cleanly is
  refused.
- **Rehearsal.** In `trial` the same restore works with stock still on the unit; afterwards a stick
  with the installer converts it again. The `radio_*.sh` a restore before 31 Oct kept are replaced by
  the install like the rest of the payload: until 29 Oct `once.sh` refused on them ("already has
  /script/radio_ap_up.sh"), because with `/etc/ocbm` gone it took them for a stranger's files.
- **Tests.** `tools/stock_restore_test.sh` finishes a synthetic trial with the real `ocbm_finish.sh`
  and restores it: the tree from an archive and from dumps (a trial's and a factory one, written as
  JFFS2 images by `tools/jffs2_extract_test.py`), the pack and its owners, room first, the check
  before the switch, the room shortfall, the re-run that converges, the seam, its Bluetooth name flag
  and `/etc/carplay`, every
  refusal, the rehearsal, and the driver's monitor under `pipefail`, under `sh` and `dash`.
  `tools/jffs2_extract_test.py` builds JFFS2 images node by node: every compression type, deletions,
  renames, truncation, hard links, bad CRCs, obsolete nodes, unsafe names, and a type-22 node that was
  never swapped.
- **On an adapter** (28 Oct): the dry run on the factory adapter, in its trial on `153df7f`, passed
  every preflight check with flash unchanged; it found the AppleDouble writes above. The rehearsal then
  ran there (`c15209d`, `all`): `RESTORE_DONE` (the switch, 7 programs and 18 scripts and settings
  removed, the 3 `radio_*.sh` kept) and verify "rootfs matches the backup". With a stick in the
  adapter, stock comes up sharing it as 1d6b:a4a5 rather than 1314:1520, which the `stock` step now
  accepts. On 31 Oct the same adapter, in a trial on `07c94df`, went back to its own 28 Oct factory dump
  (`--dump`, `apply verify stock`): 184 files and 10 directories back (HiCar and its libraries, the
  web pages, the HiCar identity, the Android Mirror helper), 32 of OCBM's files and 2 directories
  gone, `RESTORE_DONE`, verify, stock at 1d6b:a4a5. The radio guard had kept the seam's Bluetooth name
  flag (`ccpa-fe01`), and stock's first boot with it wrote that name into `bluetooth_name`,
  `eir_info`, `hcid.conf` and `riddle.conf`; after a one-off stick removed the flag, a read-only survey
  found 879 of 891 entries identical to the dump: those four, the four directories `mkdir` made 755, a
  0755 `hostapd.conf` from an older go-back's plain copy, and three runtime entries (the DHCP leases,
  the reboot log, the `/var/run` tmpfs). The flag, the directory modes and the copy are fixed since
  (`1ffc503`), and the whole round ran again with the fix the same afternoon: install, `--dump`
  restore (182 files and 6 directories back, nothing kept by the radio guard), stock, survey. 883 of
  891 entries then matched the dump, the Bluetooth name files included after stock's first boot. The
  restore compares content, not the mode or owner of an entry that already exists, so five entries
  the first round had left kept them: the four directories and `hostapd.conf`, root's at 755 where
  the image has 1000:1000 at 775 and 777. A one-off stick set those five back. A survey and a listing
  of every owner then matched the dump in all but the three runtime entries: 888 of 891 entries
  identical in type, content, mode and link target, and every owner on flash as in the image.
  A tester took an IW416 unit from `final` back to full stock with these tools, a dump decoder
  of their own and a few hand steps (Oct 2026): the identity gate, the dry run, the switch order and
  `verify` behaved as documented, and a read-only survey found 669 files identical to the pre-install
  flash; the fixes of 31 Oct are the five problems the tester found on the way (the silent monitor, 502:20
  owners, `/etc/carplay` left over, the pruned directories, the `ARMimg_maker` claim). The same
  evening the dev adapter went the whole way on `f8cc90f`: a stick install into the trial,
  `ocbm-host install confirm`, a host session ended with `CT_STOP`, and the finish after its 60 s grace
  (105 entries removed, the Carlinkit programs, the stock updater and `/etc/boa` among them, the stick
  handlers' updater call commented out; 4,176 KB free). From `final` the `--dump` restore put back 284
  files and links and `udisk_insert.sh` and made 10 directories (6,123 KB in about 2 minutes; the
  tightest free-space check before a file had about 860 KB to spare), switched, removed OCBM's 7
  programs and 22 scripts and settings, verified, and came up as stock sharing the stick. The restored
  updater then ran a read-only survey stick: 888 of 891 entries identical to the dump in type, content,
  mode and link target (the same three runtime entries), 1,728 KB free, the stock daemons running. One
  owner gap was left: the 20 library symlinks the finish had deleted came back root's, where the image
  has 1000:0 (a symlink's owner changes nothing here, but the image is the target). The restore made
  links with a bare `ln -s`, and the tree it worked from could not have said otherwise, since busybox
  `tar` had left that tree's links root's too; since then each link takes its owner from
  `/tmp/rs.links` (see the pack, above). A one-off stick set the dev adapter's 20 to 1000:0 that
  night: its owners then matched the dump in 889 of 891 entries (the other two are runtime,
  `/var/run` and the DHCP lease file) and its survey in 888, as before.
  Later that night the confirm came from Carlink itself, once: a bench build with it on (`4484e28`,
  `-Pcarlink.installConfirm=true`) sent `MGMT_INSTALL_CONFIRM` as soon as its welcome card showed the
  way back (status 0; Settings › Adapter then read "Removed when this drive ends"). Stopping the
  emulator ended the session at the USB level, and `ocbmd` exited for respawn on the read error
  without starting the finish, so the adapter stayed `confirmed`; `ocbmd` now starts it on that exit
  too (`609b9d2`, see `ccpa/README.md`, `ocbm_finish.sh`). The same night the route ran again on
  `609b9d2`: Carlink's confirm, the emulator stopped, and `ocbmd` logged "the confirmed session
  ended" on its way out; after its grace the finish removed the same 105 entries with no replug, and
  the `--dump` restore took the adapter from `final` to stock once more. The next power-on ran the finish from the start script once the self-test had
  passed: the same 105 entries, 4,244 KB free. From `final` the `--dump` restore then gave every link
  it made its owner: straight after it the owners matched the dump in 889 of 891 entries and the
  survey in 888, with 1,732 KB free and no one-off stick. A survey stick fails the update on
  purpose, and after it the adapter went two ways: after the evening's survey and the link-owner
  stick it showed as stock for a second and then stayed off USB, its stick share included, until the
  next power cycle; after one earlier that day and this last one it stayed on USB as `1314:1520`, as
  `tools/stock_survey.sh` describes (CarPlay down until a power cycle). Why is not known.
  Every other Carlink build still leaves the confirm off (`-Pcarlink.installConfirm`). The flash dump
  and an SPI programmer stay the last resort.

### UART console reference

Pads **TX1/RX1** (+ GND) = i.MX6UL UART1 = `ttymxc0`, 3.3 V logic, always-on root serial console (physical-pad access only)
shell. **115200 8N1** — `/script/early_console.sh` `stty`s `ttymxc0` to 115200 unconditionally inside
its respawn loop before the login shell, and every host-side tool assumes it (`host/uart_cmd.sh:8`,
`tools/uart_push.sh:19`). *(**CORRECTED 2026-08-16** — this read "Default **9600 8N1** (a persistent
115200 wrapper via inittab is available)". Whatever state that described predates this repo's
history, which has shipped `early_console.sh` since the 2026-07-25 baseline; there is no 9600 state
on a converted box.)* U-Boot 2015.04
is silenced on the pins (`console=ttyLogFile0`), so nothing prints during boot — only the post-boot
Linux shell. `/script/early_console.sh` runs as the **first** `::sysinit` in `/etc/inittab` (before
`rcS`), so the UART shell is available even if `rcS` itself is broken (e.g. a script that lost its exec
bit). This is the Tier-2 floor for when the gadget/daemon itself is dead.
