# Migrating a hand-installed adapter to the one-file install

This takes a CPC200-CCPA that was converted to OCBM by hand (`tools/ncm_base_install.sh`, then
`tools/ocbm_install.sh`) to the one-file install: the layout Carlink installs on a stock adapter, and
the only one that takes updates over the air. It runs in two stages: back to stock from the
adapter's own backup, then the one-file installer from a USB stick. It took about 30 minutes on the
first adapter migrated, on 1 November 2026, to ocbm `052fd04`.

You lose every phone pairing, so each phone pairs again, and the adapter's Bluetooth name may
change. Its Wi-Fi password and other settings return to what the backup holds.

Not sure which install you have? A hand-installed adapter reports no `"install"` value in
`ocbm-host install 1314 2d00 info`, has no `/etc/ocbm`, and its capabilities lack `0x200`
(`CAP_UPDATE`). The stick installer refuses it: "This adapter was converted to OCBM by hand".

## What you need

- **The adapter's own `backup_pre/` folder**, written by `tools/ncm_base_install.sh backup` before
  anything was stripped: `mtd0.bin`, `mtd1.bin`, `mtd2.bin`, `rootfs.tar.gz`, `manifest.txt` and
  `SHA256SUMS`. Without it this guide can't help; stock then needs an SPI programmer. It holds the
  adapter's Wi-Fi password and Bluetooth keys, so keep it private and out of every repository.
- **A Mac or Linux host** with this repository at the release you are installing and `python3`. On
  macOS, run the tools with Homebrew bash (`/opt/homebrew/bin/bash`); the system's bash 3.2 is too old.
- **The build toolchain**, until an installer image is published with a release: rustup with nightly
  and `rust-src`, the `armv7-unknown-linux-musleabihf` target, zig and `cargo-zigbuild`, and the
  [fdk-aac 2.0.3](https://github.com/mstorsjo/fdk-aac) source unpacked at `scratchpad/fdk/src`
  (`carplayd` links it). `./build.sh` then builds the adapter's programs and `target/release/ocbm-host`.
- **A FAT32 USB stick for this adapter alone.** The install writes the adapter's flash backup to it.
- **The adapter on the host's USB, and nothing else holding it**: no emulator passing it through, no
  Carlink attached. Keep the host awake; a sleeping Mac can cut USB power and restart the adapter
  in the middle of a step.

## 1. Check the backup and the adapter

```
(cd /path/to/backup_pre && shasum -a 256 -c SHA256SUMS)
chmod -R go-rwx /path/to/backup_pre
./target/release/ocbm-host install 1314 2d00 info
```

Every file must say `OK`. The last line of `info` is the adapter's state as JSON.

## 2. Dry run: nothing changes

```
/opt/homebrew/bin/bash tools/stock_restore.sh --backup /path/to/backup_pre \
  --run-dir scratchpad/restore_dryrun dryrun
```

Look for these lines; on the first adapter it took 23 s.

- `identity: mtd0 and mtd1 match the backup byte for byte`. If they don't, stop: the backup is
  another unit's.
- `software_version: 2025.10.15.1127`.
- The plan. The first adapter's was 245 files, 25 links and 9 directories back, 6 of OCBM's programs
  and 30 of its scripts and settings removed, and 4,108 KB free before against about 1,728 KB after.
- `dry run complete: nothing on the adapter's flash changed`.

## 3. Back to stock: the one-way step

```
/opt/homebrew/bin/bash tools/stock_restore.sh --backup /path/to/backup_pre --yes \
  --run-dir scratchpad/restore_all all
```

It switches OCBM to NCM maintenance mode, stages the backup in RAM, plans, then applies: OCBM is
deleted, and the adapter still boots NCM if it stops at any point, so `apply` can simply run again.
It verifies, reboots to prove NCM comes back by itself, verifies again, and finally removes
`/script/ncm_only` and restarts on Carlinkit's firmware. Without `--yes` it asks before `apply` and
before that restart.

Expect `RESTORE_DONE`, `the rootfs matches the backup` twice, and `the adapter enumerates as stock
Carlinkit (1314:1520)`. It took 209 s. With no phone attached, stock then comes and goes on USB as
`1314:1521` every 13 s or so; that is normal.

The restored firmware is whatever the backup holds: stock, plus the SSH and NCM scripts of the
community firmware the backup was taken under. The installer accepts it. From here, OCBM is gone
until step 5.

## 4. The installer image on the stick

Until an image is published with a release, build it:

```
./build.sh
bash tools/make_ocbm_installer.sh ~/ocbm-image
```

This writes `A15W_Update.img`, labelled `1.0.0 ocbm <commit>`, and its `SHA256SUMS`. Copy it to the
root of the stick and check the copy (macOS shown):

```
cp ~/ocbm-image/A15W_Update.img /Volumes/STICK/A15W_Update.img
shasum -a 256 /Volumes/STICK/A15W_Update.img
diskutil eject /Volumes/STICK
```

The hash must match `SHA256SUMS`.

## 5. Install from the stick

1. Put the stick in the adapter.
2. Unplug the adapter and plug it in again. Stock looks for the image as it starts.

About a minute and a half later the adapter appears as `1314:2d00`: OCBM, in its trial. It has backed
its flash up to the stick (`OCBM_BACKUP/<id>`), renamed the image `A15W_Update.img.installed` and
written `OCBM_RESULT.txt`. Check it:

```
./target/release/ocbm-host install 1314 2d00 info
```

Expect `caps=0x000007ff` in the `HELLO_ACK` line, which includes `CAP_INSTALL` (`0x100`) and
`CAP_UPDATE` (`0x200`), and `"install":"trial"` and `"update":"none"` in the JSON.

Take the stick out before driving and keep it: `OCBM_BACKUP` is the adapter's way back to stock from
now on, and it holds the Wi-Fi password and Bluetooth keys. Copying that folder to the host, kept
private, is a good idea.

If the image was renamed `.refused` instead, `OCBM_RESULT.txt` says why and nothing was changed.
"This adapter was converted to OCBM by hand" means step 3 did not finish.

## 6. Pair the phones again

On each iPhone, forget the adapter's old name in Settings › Bluetooth and under Settings › General ›
CarPlay. In Carlink, tap Pair iPhone and pick the adapter's name on the phone. The name can differ
from before: it now comes from the adapter's Wi-Fi address (`CarLink-<last 4 hex digits>`). The first
adapter went from `CarLink-21f7` to `CarLink-0c94`.

## 7. Check it

CarPlay should show within a minute of pairing. Then restart Carlink without touching the phone: it
should come back on its own, as at the start of every drive. On the first adapter it took 20 s.

## After the migration: trial, final and updates

| State | What it means | Free flash |
| --- | --- | --- |
| Trial | Where step 5 leaves it. OCBM runs and Carlinkit's firmware stays on the flash as a fallback; the self-test runs at every start; Carlink offers the way back to stock. | About 1 MB (964 KB on the first adapter, whose backup carried the community firmware's extras) |
| Final | After a Carlink build that confirms the install (`MGMT_INSTALL_CONFIRM`) has run a session, the Carlinkit stack is removed when that session ends. | About 4 MB |

An adapter with `CAP_UPDATE` takes an update package the app pushes over the OCBM link and installs it
as a trial that falls back to the version it replaced if the self-test fails
(`ccpa/installer/ocbm_update.sh`). It needs flash for the changed files, at about 0.7 of their size,
plus 384 KB spare. In trial, small updates fit; one that replaces `carplayd` (about 1.9 MB) needs final.

**From a USB stick, with no internet and no computer.** Put the package and its checksum file
(`ocbm-update-X.Y.Z.tgz` and `ocbm-update-X.Y.Z.tgz.sha256`, from the enigy/ocbm releases page) on any
FAT stick, alone or next to the install image, plug it into the adapter and start the adapter (turn the
car on, or unplug and replug it). In trial or final, OCBM looks for one package in the first 20 s after
it starts. If the checksum matches, it installs the package the same way as an update from Carlink:
same checks, then a trial that falls back to the version it replaced. The adapter restarts once to run
it. A stick plugged in during a drive is used at the next start, never during the drive.
`OCBM_RESULT.txt` on the stick then says UPDATED, ROLLED BACK (and why) or REFUSED (and why). The
package is renamed `.used`, or `.refused`, so leaving the stick in doesn't run it again. In a trial
without room for the update, the report says so: remove stock CarPlay in Carlink first, or go back to
stock and install the full image. While the install waits for its finish (confirmed), the package is
left untouched until a later start.

**With the image instead.** The same `A15W_Update.img` that converts a stock adapter also updates one
already on OCBM, in trial or final: put it on a stick and start the adapter with it in. It installs
as an update (newer versions only, same trial and fallback), and `OCBM_RESULT.txt` says UPDATING, then
UPDATED or ROLLED BACK. A stick put in during a drive waits for the next start. A final adapter also
runs Carlinkit's own images from a stick, as stock does; see `docs/ops/01_RECOVERY.md` for what that
leaves.

**Your hardware, your choice of software.** The stick routes take any package or backup that is well
formed: the `.sha256` only catches a damaged copy, and nothing checks who built it. That is
deliberate (Scott, 3 Nov). It is how a factory Carlinkit adapter treats a stick, and an owner keeps
the right to put whatever they choose on their own adapter, OCBM's image updates included. **No
update is guaranteed to succeed, least of all one the OCBM project did not build.** The update trial
falls back to the version it replaced when the new one fails its self-test, but it cannot catch
everything a package might do.

Where it stands on 1 November 2026: Carlink's Play releases don't yet send the install confirm, its
adapter-update screen isn't released, and no installer image is published with a release. A migrated
adapter is ready for all three.

## Back to stock later

**From the stick, with no computer.** On the stick the first install was made from (the one with
`OCBM_BACKUP/<id>`), add an empty file named `OCBM_RESTORE`. Plug the stick into the adapter and start
it (unplug and replug, or turn the car off and on). In trial, OCBM goes back to the stock firmware it
kept. In final, it rebuilds stock from that backup on the adapter itself, checking first that the
backup is this adapter's. It takes a few minutes, then restarts on stock, and `OCBM_RESULT.txt` on the
stick says BACK TO STOCK or why not. The details are in `docs/ops/01_RECOVERY.md`.

**From a computer.** Copy the stick's `OCBM_BACKUP/<id>` folder to the host, keep it private, and
restore from it:

```
/opt/homebrew/bin/bash tools/stock_restore.sh --profile onefile --backup /path/to/OCBM_BACKUP-id \
  --dump --run-dir scratchpad/onefile_dryrun dryrun
/opt/homebrew/bin/bash tools/stock_restore.sh --profile onefile --backup /path/to/OCBM_BACKUP-id \
  --dump --yes --run-dir scratchpad/onefile_apply apply verify stock
```

`docs/ops/01_RECOVERY.md` has the details; it has been run several times on a factory unit.

## Updating in place instead

If you want new code but not over-the-air updates, a hand-installed adapter can stay as it is and take
the new files with the tool that installed it. The first adapter did this the same evening, before
its migration. Back up each file the installer places first (`ocbm-host pull 1314 2d00 <remote>
<local>`), then:

```
/opt/homebrew/bin/bash tools/ocbm_install.sh --full revert
/opt/homebrew/bin/bash tools/ocbm_install.sh --full place verify
/opt/homebrew/bin/bash tools/ocbm_install.sh --full reboot
/opt/homebrew/bin/bash tools/ocbm_install.sh --full trial
/opt/homebrew/bin/bash tools/ocbm_install.sh --full --yes finalize
```

Three things to know:

- **Skip `preflight`.** It adds up every file, including the ones already on the adapter, and
  refuses for space, while `place` only needs room for the largest file.
- **`trial` can exit 1 after the trial passed.** Its last telnet read can come before telnetd is up.
  Check over ssh that `/script/ncm_only` is set and the new files are in place.
- **Check `finalize`'s confirm.** It sends `rm -f /script/ocbm_trial` the moment the OCBM console
  attaches, and a command sent that early can be dropped; the dead-man would then restore NCM 240 s
  later. Confirm with `python3 tools/ocbmcmd.py 'ls /script/ocbm_trial'` that the file is gone, and
  remove it the same way if it isn't.
