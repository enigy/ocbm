# OCBM — open-source CarPlay firmware userspace for the Carlinkit CPC200-CCPA

CarPlay, wired and wireless, over one open transport (OCBM), with the host app
[Carlink](https://github.com/enigy/carlink) on an Android Automotive head unit.

> **Documentation:** [docs/README.md](docs/README.md) is the map — three categories
> (`carplay/`, `wireless/`, `ops/`), capped at 10 documents each by `tools/docs_check.py`.
> One topic, one file: correct the owning document, never add a dated sibling.

> **License:** [Unlicense](LICENSE) — public domain. The code is original; external projects
> were reference points only, and what was consulted is recorded in [NOTICE.md](NOTICE.md). The
> adapter's `carplayd` links libfdk-aac, which carries its own licence (see NOTICE.md).

> **Scope & authorization:** licensed Apple/MFi developer, genuine hardware, ordinary accessory development — not security work. See [docs/ops/07_AUTHORIZATION.md](docs/ops/07_AUTHORIZATION.md).

**What it is.** The CPC200-CCPA is a small ARM Linux dongle that plugs into a car's USB port; an
iPhone connects to it and the car gets **CarPlay**. This project replaces the dongle's closed
userspace with open code, and pairs it with a host app that does the decode and rendering. The two
talk **OCBM**: the dongle owns the stable, hardware-bound half, the app owns the half that changes
with every iOS release.

An open, Rust-first replacement for the CCPA's stock projection stack (Carlinkit "riddleBox":
`ARMadb-driver` / `AppleCarPlay` / `ARMiPhoneIAP2`), running as ordinary userspace on the same
hardware. It reuses the adapter's **hardware** — WLAN/BT radios and the onboard genuine Apple
**MFi 2.0C** authentication coprocessor (`/dev/i2c-1 @0x11`), used the way Apple's accessory
program intends — and its **factory vendor boot stack** (HAB-signed U-Boot + OTPMK-encrypted
kernel 3.14.52), which this project never modifies and treats as fixed platform firmware (a BIOS).
Everything above the kernel is rewritten open-source.

> **The problem this solves:** the stock CCPA ships an older, deliberately stripped CarPlay
> implementation — old SDK, H.264 only, features dropped. Replacing the userspace gets current,
> full-featured HEVC CarPlay, and keeps it current: new features land app-side. No phone
> modification, no custom PCB, no soldered MFi chip.

---

## Requirements

- A genuine **Carlinkit CPC200-CCPA** adapter (i.MX6UL, 123 MB RAM, jffs2 rootfs, on-board MFi 2.0C
  coprocessor).
- An **iPhone**, and an active **Apple Developer Program membership** for the licensed CarPlay SDK
  material the protocol work is checked against.
- An Android Automotive head unit running Carlink, which installs and updates the adapter.
- To build: Rust (nightly with `rust-src`), `rustup target add armv7-unknown-linux-musleabihf`, zig
  and `cargo install cargo-zigbuild` (the box binaries cross-compile with the zig linker — no Linux VM
  needed), and the fdk-aac 2.0.3 source at `scratchpad/fdk/src`.

## Getting started

| Step | Where |
|---|---|
| Build the box daemons and the host CLI | `./build.sh` — detail in [`docs/ops/00_BUILD_AND_DEPLOY.md`](docs/ops/00_BUILD_AND_DEPLOY.md) |
| Install OCBM on a stock adapter | Carlink's adapter page, or the installer image (`tools/make_ocbm_installer.sh`) from a USB stick |
| Move a hand-installed adapter to the one-file install (over-the-air updates) | [`MIGRATING.md`](MIGRATING.md) |
| Recover a wedged or bricked box | [`docs/ops/01_RECOVERY.md`](docs/ops/01_RECOVERY.md) |
| Verify a change before committing | `tools/run_tests.sh`, `python3 tools/docs_check.py`, and the test plans in [`docs/ops/02_TESTING.md`](docs/ops/02_TESTING.md) |

## The one hard constraint (the vendor floor)

The i.MX6UL boot ROM enforces HAB signature verification and the kernel is per-chip OTPMK-encrypted,
so **U-Boot + kernel are fixed platform firmware** — untouched here. All CCPA behavior is userspace,
and userspace is 100% open in this project. Practical implication of the fixed kernel: no
configfs/gadgetfs/functionfs, so the adapter uses the kernel's existing USB-gadget functions
(`f_accessory` bulk + `f_ncm`). See [`docs/carplay/00_ARCHITECTURE.md`](docs/carplay/00_ARCHITECTURE.md),
which covers the architecture and the transport.

---

## Status

Open work and the pick-up point: **[docs/ops/04_OPEN_ITEMS.md](docs/ops/04_OPEN_ITEMS.md)**.

**CarPlay — wired and wireless, end to end.** HEVC up to 2400×960, every `audioType` negotiated and
decoded, mic uplink, touch and HID, metadata (NowPlaying, route and lane guidance, call state), the
instrument-cluster stream. Wireless rides BT pair -> WiFi handoff -> AirPlay/RTSP -> OCBM; the
inbound iAP2 carrier is the RemoteControlSession DataStream, stream type 130.

**Installs and updates from the head unit.** Carlink converts a stock adapter with the one-file
install (a reversible trial, then a commit), updates it over USB with an update package, and can
take it back to stock from its own flash backup.

**CarPlay only, since OCBM 1.0.5.** Android Auto, the Bluetooth headset profiles (HFP/HSP/SCO), the
host MFi relay, the IP/Ethernet bridges and benches, the app-driven SETUP relay, the GM bridge role
and the Raspberry Pi and C2Air ports left the tree in that release. Their protocol numbers are
retired, never reused (`crates/ocbm-proto/src/lib.rs`), and tag
[`v1.0.4`](https://github.com/enigy/ocbm/tree/v1.0.4) keeps all of it.

**One protocol definition.** `crates/ocbm-proto` is canonical. Carlink keeps its own copy of the
constants and checks it against this crate.

## Architecture in brief

Full detail: [`docs/carplay/00_ARCHITECTURE.md`](docs/carplay/00_ARCHITECTURE.md). The governing
doctrine is app-driven configuration ([`docs/carplay/04_CAPABILITIES_AND_CONFIG.md`](docs/carplay/04_CAPABILITIES_AND_CONFIG.md)):
anything configurable is owned by the app, the box presents what the app pushes, and box placement is
earned rather than designed in.

**The split is by stability, not by transport.** The adapter owns the parts Apple keeps
backward-compatible forever — the iAP2 accessory handshake, MFi 2.0C authentication on the on-board
coprocessor, HomeKit pair-setup/pair-verify, and the ChaCha20 session-key derivation. The host app
owns what changes with every SDK: A/V decode, UI and input, and the configuration the AirPlay SETUP
negotiation is answered from (codec, resolution, screens, audio formats, feature enablement).

| | Adapter (rarely changes) | Host app (where features live) |
|---|---|---|
| Owns | iAP2 handshake, MFi auth, pairing, key derivation, SETUP answered from the pushed config, the OCBM mux | decode, render, input, the YAML config |
| Media | forwards encrypted A/V untouched — never decodes or re-encodes it | decrypts with the adapter-provided ephemeral key, decodes HEVC |
| Config | presents what the app pushed | single source of truth |

The adapter does parse CarPlay above pairing — it answers SETUP including the `enabledFeatures` echo,
reads the decrypted `/command` channel, decrypts the type-130 DataStream and relays iAP2 TLVs. What it
never touches is the media itself.

**No OS network interface.** The host app claims the adapter by VID:PID (`0x1314:0x2d00`) and speaks a
bulk protocol entirely inside the app — OCBM, the open replacement for the stock `0x55AA55AA` typed-bulk
protocol. The OS only ever sees "an app claimed a USB device".
[`docs/carplay/01_OCBM_PROTOCOL.md`](docs/carplay/01_OCBM_PROTOCOL.md).

**Two secrets, not one.** The MFi private key never leaves the coprocessor; all MFi crypto happens on
the box and the app sees only status. The ChaCha20 session key is derived at pair-verify, is ephemeral
and per-connection, and is what lets the app decrypt A/V the adapter forwarded untouched.

**HEVC, not H.264** — the stock box negotiated H.264 High every session. Both sides of the assertion
are driven by the app's `enablesHEVC`: the adapter emits `hevcInfo` in `/info` and the `hevc` token in
the SETUP `enabledFeatures` echo. [`docs/carplay/06_AV_PIPELINE.md`](docs/carplay/06_AV_PIPELINE.md).

**Config is an Apple-shape `VehicleConfig` YAML** held app-side and pushed at SUBSCRIBE; the box never
persists it. [`docs/carplay/04_CAPABILITIES_AND_CONFIG.md`](docs/carplay/04_CAPABILITIES_AND_CONFIG.md).

Rejected alternatives and why, so they are not re-litigated:
[`docs/carplay/00_ARCHITECTURE.md`](docs/carplay/00_ARCHITECTURE.md).

---

## Reproducibility bar

The one-file install needs only the head unit and a USB stick: the stock firmware runs an update
image from the stick, and that image installs OCBM after writing the adapter's flash backup to the
stick. An **SPI flash programmer** is the brick-proof offline recovery path. No phone modification,
no soldered chip. See [`docs/ops/01_RECOVERY.md`](docs/ops/01_RECOVERY.md).

---

## Layout

| Path | Contents |
|---|---|
| `docs/carplay/`, `docs/wireless/`, `docs/ops/` | The whole corpus, capped at 10 documents per category by `tools/docs_check.py`. Start at [`docs/README.md`](docs/README.md) |
| `crates/ocbm-proto/` | OCBM wire codec (envelope, channels, Reassembler, CRC-32) and the table of retired numbers — shared by box + host |
| `crates/box-common/` | The box-side foundation the daemons share: OCBM re-export, the session/projection-owner flags, the adapter's network constants |
| `crates/bt-common/` | btd's Bluetooth primitives: raw HCI scan enable, kernel RFCOMM, a `bluetoothd`-less SDP server, the pairing agent |
| `crates/vendor/` | Nine crates vendored in on 2026-07-13 to make the tree self-contained: `receiver`, `pairing`, `rtsp`, `mfi`, `eld-codec` from the archived `ncm_carplayd` tree; `iap2-core`, `metadata`, `wireless` from the `carplayd` tree; `mfi-i2c-local` written here. All but `wireless` are path-dependencies only (`Cargo.toml` `exclude`); `wireless` is the workspace member producing `btd` (the BT/WiFi wireless-CarPlay stack) |
| `ccpa/ocbmd/` | Box OCBM daemon (armv7-musl), over `/dev/usb_accessory`: CTRL `0x00` (incl. SUBSCRIBE/heartbeat/presence → `/tmp/host_present`) / CONSOLE `0x02` (root PTY) / FILE `0x11` / VIDEO `0x20` / MEDIA_AUDIO `0x21` / ALT_AUDIO `0x22` (voice sink, seam `:9003`) / METADATA `0x23` (seam `:9004`) / ALT_VIDEO `0x24` (cluster screen, seam `:9005`) / INPUT `0x30` (HID uplink) / MIC `0x31` (mic uplink) / MGMT `0x40` (Carlink's adapter page) / LOG `0x42` (the box log). Authoritative list: `crates/ocbm-proto/src/lib.rs` |
| `ccpa/iap2d/` | Box iAP2 accessory daemon (armv7-musl): handshake + local-i2c MFi → Identify |
| `ccpa/carplayd/` | Box AirPlay daemon (armv7-musl): receiver_core `ControlServer` + `LocalMfiSigner` (local i2c) → pair-setup/verify on `:5000` → derives the session key → forwards **encrypted** A/V and hands the per-stream key; mDNS discovery in-process; disk-backed PeerStore (`/etc/carplay_peers.bin`) |
| `ccpa/installer/` | The one-file install and its update path: `once.sh` (Stage A, from the stick), the start-time dispatcher, self-test, finish, update, go-back and restore scripts, `prune.list`, `retired.list` |
| `ccpa/rootfs/` | The boot path (`ocbm_boot.sh`) and the chipset-neutral radio seam (`radio_detect.sh`, `radio_hal.sh`, `radio_ap_up.sh`) |
| `host/ocbm-host/` | Host OCBM client (rusb/libusb) for the bench: hello/console/settime/ncm/install/push/pull/av/session/**avdec** (SUBSCRIBE + heartbeat + decrypt A/V) |
| `accessory_init/` | `iap_role_switch.c`/`.armv7` — issues Apple's standard `0x51` host-role USB control request over raw usbfs (no libusb, so it runs on the stripped appliance). **First-party, implemented from documentation**; the only C binary the box ships (built by `build.sh`, installed to `/usr/bin`) |
| `tools/` | Build and ops: the installer and update-package builders and their tests, `session_supervisor.sh` + `projection_up.sh` (lifecycle actor + IDLE→projection bring-up), the stock-restore tools, `uart_push.sh` (file deploy over the serial console), `boxsh.py`, `docs_check.py`, `run_tests.sh` |

---

## Reference material (locations + what each is authoritative for)

**Apple CarPlay Kit SDK — the single source of truth for the CarPlay/iAP2 protocol AND the config schema.**
`/Applications/Xcode.app/Contents/SharedFrameworks/DeviceKit.framework/Versions/A/PlugIns/CarPlaySimulator.devicekitplugin`
(Xcode 27 beta; bundled `Contents/Frameworks/CarPlaySDK.framework`). Accessed under an active Apple
Developer Program membership — licensed first-party material used under the developer program, obtained directly from the SDK install. Use it for:
- protocol truth (link/state/message framing, TLV layouts, message ids);
- the **YAML config schema** — `Contents/Resources/VehicleConfigs/Configs/*.yaml` (Standard, Widescreen,
  Portrait, Instrument Cluster, Navigation, Minimum…) and the feature toggles in the `MacOS/CarPlaySimulator`
  binary (`_enablesHEVC`, `_enablesMainBufferedAudio`, `supportsAltScreen`, cluster config…).

**`CPC200-CCPA_resources` — community interoperability documentation for the original firmware + protocol.**
Authoritative for how the stock box worked:
`documentation/02_Protocol_Reference/{usb_protocol,video_protocol,audio_protocol,carplay_handshake,
inbound_session_sequence,command_ids}.md`, `documentation/04_Implementation/host_app_guide.md`,
`documentation/01_Firmware_Architecture/hardware_platform.md`. Documents the `0x55AA55AA` framing, the
full message catalog, video (H.264 + 20-byte sub-header) / audio (PCM + 12-byte sub-header), and the
**host config flow** (`Open` 0x01, `BoxSettings` 0x19, `SendFile` 0x99 → AirPlay SETUP `viewAreas`) —
the field set our YAML absorbs. NOTE: the stock box *terminated + stripped*; we mirror its transport
model, not its scope.

**`ncm_carplayd` — the Rust CarPlay receiver these crates came from.** `receiver_core/` =
independent, from-documentation Rust receiver (RTSP, SRP/25519 pairing, MFi-SAP, ChaCha20 decrypt,
typed A/V seams) — the code cross-compiled to the box.

**`carplayd` — Apple-spec iAP2 core + reinforcing session reference.** `crates/iap2-core/` = the
transport-agnostic iAP2 link/state/message/spec crate `iap2d` depends on (`src/spec.rs`
machine-generated from the Apple plugin; `src/link.rs` holds the coalesced-read fix from this project).

**Chip backups stay out of the repo.** The one-file install writes each adapter's flash backup to its
own USB stick (`OCBM_BACKUP/<id>`), and that backup is what takes it back to stock. It holds the
adapter's Wi-Fi password and Bluetooth keys: keep it private and out of every repository.
