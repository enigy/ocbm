# ccpa/ — box-side (armv7 / i.MX6UL)

The open CCPA userspace that runs on the adapter, on top of the fixed vendor floor
(HAB-signed U-Boot + OTPMK-encrypted kernel, unmodified). See [`../docs/carplay/00_ARCHITECTURE.md`](../docs/carplay/00_ARCHITECTURE.md).

## `rootfs/` — the boot path and the radio seam

Mirrors on-box paths. The installer image places `ocbm_boot.sh` and the three seam scripts
(`tools/make_ocbm_installer.sh`); the six IW416 files are **never** installed on any box:
`wlan_on.sh`/`wlan_off.sh`, `bt_on.sh`/`bt_off.sh`, `attach_bluetooth.sh` and
`start_bluetooth_wifi.sh` carry the **IW416 baseline's** chipset-specific bring-up, and a CCPA ships
only its own chip's drivers (at least six WLAN/BT parts exist, with no fallback set). Each unit keeps
its own vendor copies instead, and `is_radio()` in `once.sh`, `ocbm_finish.sh` and the restore tools
shields every radio file from deletion. Read
[`../docs/wireless/01_BT_AND_RADIO.md`](../docs/wireless/01_BT_AND_RADIO.md)
and `../CLAUDE.md` before pushing any radio file by hand.

| File | Purpose |
|---|---|
| `script/ocbm_boot.sh` | OCBM appliance boot: pure-accessory gadget + `ocbmd` + session supervisor |
| `script/radio_detect.sh` | **chipset-neutral seam** — read-only detection of THIS unit's WLAN/BT platform → `/tmp/radio_caps` |
| `script/radio_hal.sh` | **chipset-neutral seam** — the radio verbs everything above calls: `probe`, `status`, `wifi_ap_on`, `wifi_ap_off`, `bt_on`, `bt_off`, `preload`. `session_supervisor.sh` uses these, not the scripts below |
| `script/radio_ap_up.sh` | **chipset-neutral seam** — the owned SoftAP layer, deliberately NOT named `start_bluetooth_wifi.sh` (every unit already ships a *vendor* file at that path) |
| `script/wlan_on.sh` / `wlan_off.sh` | IW416-only WiFi AP up/down — **reference only, never installed**; reached only as `radio_hal.sh`'s `backend=owned` path on a unit that already has them |
| `script/bt_on.sh` / `bt_off.sh` | IW416-only BT up/down — **reference only, never installed** (same `backend=owned` path) |
| `script/attach_bluetooth.sh` | IW416-only BT HCI attach (slim) — **reference only, never installed** |
| `script/start_bluetooth_wifi.sh` | IW416-only AP bring-up (slim) — **reference only, never installed**; `radio_hal.sh` explicitly refuses to fall back to this path |

The NCM boot overlay (`rcS`, `inittab`, `custom_init.sh` and the rest) left main with the hand
installers in OCBM 1.0.5; it is at tag [`v1.0.4`](https://github.com/enigy/ocbm/tree/v1.0.4/ccpa/rootfs).

## `installer/` — the one-file install for a STOCK adapter

Box-side scripts of `A15W_Update.img`, which converts a stock 2025.10.15.1127 adapter from a USB stick
or over USB from Carlink. `tools/make_ocbm_installer.sh` builds it and `tools/ocbm_installer_test.sh`
runs it end to end against synthetic stock trees.

| File | Purpose |
|---|---|
| `once.sh` | Stage A, run by the stock updater before its copy step: checks (refuse = nothing changed; a payload file already on the adapter is refused unless `/etc/ocbm` exists, except the `radio_*.sh` seam a restore keeps), flash backup to the stick + `/etc/ocbm/identity.tgz`, `prune.list`, place the payload, keep `start_main_service.stock.sh`, then rename the dispatcher over `start_main_service.sh` as the one boot-critical write |
| `finish.sh` | writes `OCBM_RESULT.txt` (once.sh keeps the one before as `OCBM_RESULT.prev.txt`), parks the stick image, restarts into the trial |
| `prune.list` | what goes to make room while stock CarPlay and Android Auto stay as the fallback: HiCar, Android Mirror helper, test leftovers, web UI pages. Every match still passes `is_radio()`; a directory is emptied, never deleted (Carlinkit's own image unpacks its web pages into `etc/boa/www`) |
| `identity.list` | unit identity archived before anything changes, the HiCar identity in `etc/hichain` and the Bluetooth name (`etc/bluetooth_name`, the flag `etc/.custom_bluetooth_name`) included (holds secrets: never log or commit it) |
| `ocbm_dispatch.sh` | Stage B, installed as `/script/start_main_service.sh` (carries `# OCBM_DISPATCH`). No `/script/ocbm_mode`: `exec` the kept `start_main_service.stock.sh`, and copy a pending revert report to the next stick. `trial`/`confirmed`: count the start (two in a row without a passing self-test go back to stock), create `/tmp/not_change_mass_storage` so the stock stick handler leaves the USB port alone, run `ocbm_boot.sh`, respawn `ocbmd` and the supervisor in loops of its own (the install leaves `/etc/inittab` alone), run the self-test, watch the stick for `OCBM_RESTORE`. `final`: OCBM only |
| `ocbm_selftest.sh` | passes when, within 30 s, the gadget is the OCBM accessory (`1314:2d00`), `/dev/usb_accessory` exists, `ocbmd` runs and `radio_detect.sh` resolves this unit's own radio bring-up. No car or app needed. In `confirmed`, a pass runs `ocbm_finish.sh` |
| `ocbm_revert.sh` | back to stock: puts `/etc/hostapd.conf`, `/etc/bluetooth_name` and the HiCar identity (`/etc/hichain`) back from `identity.tgz` (`cp -p`, modes kept), puts back the Bluetooth name flag the install found or removes the seam's `ccpa-<4 hex>` one (stock's bluetoothDaemon applies the name while it is there), makes the directories an older prune deleted again, appends the reason and `reverted version=<VERSION>` to `/etc/ocbm/last_result.txt` (once.sh's same-version guard, stick installs only), writes the stick report, removes `/script/ocbm_mode`, restarts. Refuses in `final` |
| `ocbm_finish.sh` | Stage C: once an app that offers the way back has sent `MGMT_INSTALL_CONFIRM` and that session is over (`ocbmd` starts it on host-gone with a 60 s grace a reconnecting app cancels; the start script runs it after a passing self-test if the power was cut first. When the USB transport itself drops, `ocbmd` starts it on its way out for respawn; until 31 Oct, night, it did not, and the dev adapter stayed `confirmed` until its next start), it writes `final` + `/etc/ocbm/finish_pending`, removes the hand installer's strip set (pinned by `tools/ocbm_stageb_test.sh`; every candidate through `is_radio()`, libraries only when no surviving file names them), comments the updater call out of the stick handlers, and clears `finish_pending`. A start in `final` that finds `finish_pending` resumes it. Shares a lock with `ocbm_revert.sh` |

Tested on the host by `tools/ocbm_installer_test.sh` (Stage A) and `tools/ocbm_stageb_test.sh` (Stages B
and C). On the bench, `ocbm-host install [vid pid] info|confirm|stock` stands in for the app.

After `final` there is no `ocbm_revert.sh` way back. `tools/stock_restore.sh --profile onefile` restores
stock over the OCBM console, keeping the dispatcher booting OCBM until one rename of the stock start
script, from the stick's own flash dump (`--dump`: `OCBM_BACKUP/<serial>/mtd2.bin`, decoded by
`tools/jffs2_extract.py`; the first install's dump is the factory rootfs, the pruned extras included)
or from an archive the unit took of its rootfs during a trial (docs/ops/01_RECOVERY.md, "Back to stock
from a one-file install"; tested by `tools/stock_restore_test.sh` and `tools/jffs2_extract_test.py`).

## Daemons — `ocbmd/`, `iap2d/`, `carplayd/` (shipped, hardware-validated)

Each is a sibling crate (`Cargo.toml` + `src/`), cross-built for `armv7-unknown-linux-musleabihf`:

| Crate | Purpose |
|---|---|
| `ocbmd/` | OCBM bulk-transport multiplexer — owns `/dev/usb_accessory`, MODE_SELECT/CONSOLE, channel demux + per-stream drain to the host over USB |
| `iap2d/` | iAP2 / CarPlay control daemon (Identify, metadata — content selection is app-driven per docs/carplay/04_CAPABILITIES_AND_CONFIG.md; today's box tier levers are interim — session management) |
| `carplayd/` | AirPlay receiver — pairing; SETUP/RECORD answered from the app-pushed YAML; forward-encrypted A/V forwarding — the box never decrypts media by design (in-binary floor `fwd_enc()` in `../crates/vendor/receiver/src/levers.rs`; `OCBM_FWD_ENC` is the current-state mechanism) — input uplink. Links libfdk-aac for the AAC-ELD mic uplink; its licence is `carplayd/fdk-aac.NOTICE` |

The radio/link glue lives in the vendor crates under `../crates/vendor/`.
See [`../docs/carplay/01_OCBM_PROTOCOL.md`](../docs/carplay/01_OCBM_PROTOCOL.md) and [`../docs/ops/04_OPEN_ITEMS.md`](../docs/ops/04_OPEN_ITEMS.md).
Build: **`./build.sh`** from the repo root. Do not invoke `cargo zigbuild` bare — Homebrew's `rustc`
shadows rustup's and has no `armv7-unknown-linux-musleabihf` std, and carplayd's eld-codec needs the
`CC`/`AR` zig pair that `build.sh` sets. `FDK_AAC_PREFIX` defaults to `$PWD/scratchpad/fdk/install`.
