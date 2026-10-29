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

It puts `/etc/hostapd.conf` back from `/etc/ocbm/identity.tgz`, records `reverted version=<VERSION>`
in `/etc/ocbm/last_result.txt` (a stick install then refuses that version unless the stick holds
`OCBM_RETRY`, which an install that uses it renames `OCBM_RETRY.used`; an install over USB from the
app is not checked, since each one is a press of Convert),
removes `/script/ocbm_mode` and restarts. The start script then hands every boot to the
kept `start_main_service.stock.sh`, and the report lands on the next stick plugged in as
`OCBM_RESULT.txt`. HiCar, the Android Mirror helper and the web pages stay deleted; Carlinkit's own
2025.10.15.1127 image puts them back.

In `final` there is no stock left and `ocbm_revert.sh` refuses. Carlinkit's image cannot rebuild it
either: it omits files the finish deletes, its own decrypter `ARMimg_maker` among them. The way back
without an SPI programmer is `tools/stock_restore.sh --profile onefile`, over the OCBM console, from a
file-level archive the unit took of its own rootfs during its trial (`rootfs_files_trial.tgz`, kept
with the stick's `OCBM_BACKUP/<serial>/` folder):

    tools/stock_restore.sh --profile onefile --backup OCBM_BACKUP/<serial> dryrun   # flash untouched
    tools/stock_restore.sh --profile onefile --backup OCBM_BACKUP/<serial> all

- **The target.** `tools/stock_restore_onefile_tree.sh` makes stock of the trial's rootfs: OCBM's
  payload (`make_ocbm_installer.sh --list`), `/etc/ocbm/`, `/script/ocbm_mode` and OCBM's runtime files
  dropped, `/etc/hostapd.conf` from the identity archive (as `ocbm_revert.sh` does), and
  `start_main_service.stock.sh` back as `start_main_service.sh`. The pruned extras are not in it.
  The tree holds the Wi-Fi passphrase and Bluetooth keys, so the stage step makes it in a private temp
  dir that goes on any exit. Its `--pack` runs `tar` with `COPYFILE_DISABLE=1` and refuses a pack that
  is not exactly the tree: macOS `tar` otherwise adds a `._` AppleDouble entry per file with extended
  attributes, and the factory adapter's first dry run (28 Oct) planned 702 of them as writes.
- **The order** (`PROFILE=onefile` in `tools/stock_restore_box.sh`). The dispatcher keeps booting OCBM
  while every stock file goes back (`.rsnew`, md5-checked, renamed, free space checked first) and no
  OCBM file is touched; a check then stops the run if anything but the start script and OCBM's own
  files still differs; one rename of the stock start script over the dispatcher is the switch; OCBM's
  files go after it, each through `is_radio()` (the `radio_*.sh` seam stays, inert). Every stopping
  point boots OCBM, where the restore runs again, or complete stock.
- **Refusals**, before anything changes: the start script is not OCBM's dispatcher, `ocbmd` or
  `ocbm_boot.sh` is missing, the mode is not `trial` or `final` (a `confirmed` trial finishes when its
  session ends), a finish is pending, or `ocbm_mode.lock` is held (`ocbm_revert.sh` and
  `ocbm_finish.sh` take the same lock). The identity gate (`mtd0`/`mtd1` against the stick's dump) and
  `software_version` against the archive's are checked first.
- **Rehearsal.** In `trial` the same restore works with stock still on the unit; afterwards a stick
  with the installer converts it again. The kept `radio_*.sh` are replaced by the install like the rest
  of the payload: until 29 Oct `once.sh` refused on them ("already has /script/radio_ap_up.sh"), because
  with `/etc/ocbm` gone it took them for a stranger's files. The stock updater is back, so Carlinkit's
  2025.10.15.1127 image then restores the pruned extras too.
- **Tests.** `tools/stock_restore_test.sh` finishes a synthetic trial with the real `ocbm_finish.sh`
  and restores it: OCBM kept until the switch, the check before the switch, the room shortfall, the
  re-run that converges, every refusal, the rehearsal and the pack check, under `sh` and `dash`.
- **On an adapter** (28 Oct): the dry run on the factory adapter, in its trial on `153df7f`, passed
  every preflight check with flash unchanged; it found the AppleDouble writes above. The rehearsal then
  ran there (`c15209d`, `all`): `RESTORE_DONE` (the switch, 7 programs and 18 scripts and settings
  removed, the 3 `radio_*.sh` kept) and verify "rootfs matches the backup". With a stick in the
  adapter, stock comes up sharing it as 1d6b:a4a5 rather than 1314:1520, which the `stock` step now
  accepts. Until a confirm, finish and restore have run on it, no adapter is confirmed
  (`MGMT_INSTALL_CONFIRM`). The flash dump and an SPI programmer stay the last resort.

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
