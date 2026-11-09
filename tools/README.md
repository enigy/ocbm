# tools/ — dev / ops helpers

Reusable helpers for working on the adapter (not part of the shipped userspace).

| File | What it does |
|---|---|
| `stock_restore.sh` | put a converted box back on stock Carlinkit firmware, no SPI programmer. A one-file install restores from its stick's flash backup (`--profile onefile`); a hand-installed box from its own `backup_pre/`, which needs a checkout of tag `v1.0.4` for the hand-install tools: identity gate (box `mtd0`/`mtd1` == backup), `dryrun` over the OCBM console, then revert to NCM → file-by-file `apply` (`stock_restore_box.sh`, ordered so every step still boots NCM) → verify → coldtest → remove `ncm_only`. Tested by `stock_restore_test.sh`. See `../docs/ops/01_RECOVERY.md`. |
| `make_update_img.sh` | pack a staged tree into a Carlinkit stick/OTA update image (`A15W_Update.img`: `update.tar.gz` under AES-128-CBC, key = IV = `AutoPlay9uPT4n17`, tail < 16 B in clear), or unpack one. Root-owned `./` entries, a round-trip check and `SHA256SUMS`; refuses `tmp/AutoBox.img` (vendor `upg` rewrites U-Boot/kernel). Tested by `make_update_img_test.sh`. |
| `make_ocbm_installer.sh` | build the one-file installer `A15W_Update.img` for a STOCK 2025.10.15.1127 adapter: stages `ccpa/installer/` (Stage A `once.sh`, `finish.sh`, `prune.list`, `identity.list`) with the box programs, the boot and radio scripts, the Stage B/C scripts, `retired.list` and libfdk-aac's `NOTICE`, writes `VERSION`, `BUDGET` (JFFS2 estimate) and `SHA256SUMS`, wraps with `make_update_img.sh`. Refuses while an input is missing. Tested end to end by `ocbm_installer_test.sh`, with Stages B and C (dispatcher, self-test, revert, finish) in `ocbm_stageb_test.sh`. |
| `make_ocbm_update.sh` | build the over-USB update package from one version's `SHA256SUMS` to this one (`--from SUMS`): only the files that changed, `ROOM` and the trial-or-not decision against `ccpa/installer/update_budget`. Tested by `ocbm_update_test.sh`. |
| `stock_survey.sh` | build the read-only survey stick for a STOCK adapter: its `tmp/once.sh` (`stock_survey_box.sh`) writes `ocbm_survey_<n>/` (report, rootfs inventory, dmesg) to the stick, then fails the update on purpose so nothing on flash changes. Power-cycle the adapter afterwards. Secret-bearing files are listed by size only. |
| `uart_push.sh` | deploy a file to the box over the UART root console when OCBM is unavailable (app closed → accessory de-enumerated): gzip + base64 stream with XON/XOFF flow control, decode + gunzip on the box, md5-verify end-to-end. Slow but reliable. `uart_push.sh LOCAL REMOTE [mode]`. |

Box binaries are cross-built with **`../build.sh`** (never a bare `cargo zigbuild` — Homebrew's
`rustc` shadows rustup's and has no armv7-musl std, and carplayd's eld-codec needs the `CC`/`AR` zig
pair `build.sh` sets) and reach an adapter in an installer image or an update package. On the bench,
`uart_push.sh` or `ocbm-host push` can place one by hand. Revert gadget/boot experiments via
`reboot -f` (never `echo 0 > enable` with transfers pending — see
[`../docs/carplay/00_ARCHITECTURE.md`](../docs/carplay/00_ARCHITECTURE.md)).

## `i2mspec_dump.py` — Apple's iAP2 message spec, decoded

Decodes `iap2messages-internal.i2mspecarchive` from Xcode's CarPlaySimulator plug-in: 144 messages with
ids, names, source, and the full parameter tree (ids, types, cardinality, enum values, Apple's notes).
The authority for TLV parameter ids — `crates/vendor/iap2-core/src/spec.rs` was generated from it.

    tools/i2mspec_dump.py --message 0x4158 --text     # one message, readable table
    tools/i2mspec_dump.py --message 0x4170            # JSON
    tools/i2mspec_dump.py > spec.json                 # all 144

Does not hit the network or the device; reads the local Xcode install.
