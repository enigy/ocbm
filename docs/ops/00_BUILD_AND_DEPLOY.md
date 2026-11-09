# Build, footprint and deployment

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 05; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

How the box binaries are built, what they cost in flash/RAM, and how they are pushed.

## Build footprint

<!-- absorbed: ../ops/00_BUILD_AND_DEPLOY.md -->

### Toolchain

- **Box (armv7)** — target `armv7-unknown-linux-musleabihf`, **static (musl)**. The Rust daemons
  cross-build with `cargo zigbuild --release --target armv7-unknown-linux-musleabihf` (zig serves as
  the C cross-linker); `carplayd` additionally needs `FDK_AAC_PREFIX=$PWD/scratchpad/fdk/install` for
  its eld-codec. C probes use `zig cc -target arm-linux-musleabihf -static -Os -s`. Size profile below.
- **Host** — the host app is Carlink (enigy/carlink, Kotlin/Gradle, built and gated in its own
  repository). `host/ocbm-host` is native **Rust** (`rusb`) for the bench, and "`ocbm-rescue`" is a
  *role* of that same binary (`ocbm-host console`), not a separate build. When `crates/ocbm-proto`
  changes, refresh Carlink's copy (`proto/ocbm-proto.lib.rs`) and run its `tools/proto_check.py`.
  - **Emulator constraint:** exactly ONE AVD instance may run at a time. It holds the USB
    passthrough claim on the adapter (vendor id `0x1314`), and a second instance fights it for the
    claim. Reshape the running device with `wm size` / `wm density` instead of booting a second AVD.

Rust size profile (`Cargo.toml`) — size-first everywhere **except** the per-frame hot paths:
```toml
[profile.release]
opt-level = "z"      # optimize for size
lto = true
panic = "abort"      # no unwinding
codegen-units = 1
strip = true

## Five per-package overrides opt OUT of size-first because they sit on the per-frame
## critical path (ChaCha20-Poly1305 on every 4K video + audio frame, ocbm-proto's
## Reassembler, receiver's A/V forwarding). Paired with target-cpu=cortex-a7 in
## .cargo/config.toml. NOTE this is SCALAR: the armv7 musl target spec carries an
## explicit -neon that target-cpu does not override, so nothing we ship is vectorized.
[profile.release.package.chacha20]
opt-level = 2
[profile.release.package.poly1305]
opt-level = 2
[profile.release.package.chacha20poly1305]
opt-level = 2
[profile.release.package.ocbm-proto]
opt-level = 2
[profile.release.package.receiver]
opt-level = 2
```
The size cost of those five lands almost entirely in `carplayd` — the only binary carrying crypto
**and** full A/V forwarding. `Cargo.toml` is the authority; this is a quote of it.

### Storage budget (the box has very little)

- Flash: **16 MB** SPI NOR — `mtd0` uboot 256 K / `mtd1` kernel 3328 K / `mtd2` **rootfs 12800 K
  (jffs2)**. Partition sizes are fixed by the vendor kernel/U-Boot.
- Live free after stripping riddleBox: **~6 MB** on the jffs2 rootfs. jffs2 **compresses on write**,
  so compressible binaries consume less on-media than their raw size.
- RAM: **128 MB**, ~107 MB free (tmpfs) — a real staging option.
- The one-file install on a STOCK unit (measured on a factory adapter, 2026-10-28): 1,680 KB free
  on stock; deleting the HiCar/Mirror/web extras frees 2,080 KB; the OCBM payload takes 2,568 KB on
  jffs2 (whole-file gzip says 2,119 KB, but jffs2 compresses 4 KiB pages); **1,256 KB free** during
  the trial, with stock kept. `tools/make_ocbm_installer.sh` writes the payload's jffs2 estimate into
  the image (`BUDGET`: the model plus 5%, 2,644 KB against the 2,568 measured), and once.sh refuses
  a unit without that room plus 384 KB. On a unit an earlier try left OCBM files on (a revert, or a
  try a power cut stopped part way), once.sh removes them, half-written `.new` files included, before
  it measures: counting them at 0.6 of their size, and the `.new` not at all, refused the dev adapter's
  retry after a cut mid-place (3 Nov: 2,188 KB free and "about 732 KB more", while the cut try's
  files held ~1,620 KB).

### Component footprint

*(The pre-build estimate table that stood here is dropped: its three C rows described helpers that
were never written — every box daemon is Rust — and MEASURED figures follow below.)*

### Fitting the heavy case (stackable)

1. **Rust size profile** (above) — often 2–4× smaller than default release.
2. **UPX** — an *optional manual* shrink, NOT applied by the installer (**3.96**, run in the Lima `ccpa-build` VM; host
   UPX 5.x segfaults the box's 3.14 kernel, so `upx -t`-verify the packed output); ~50% further shrink.
   Measured: ocbmd 454,864 → 230,316 B. (`tools/upx_pack.sh`, the one-command form, left main with the
   hand-install tools in 1.0.5; it is at tag `v1.0.4`.)
3. **jffs2 auto-compression** — free on-media reduction.
4. **Run from `/tmp` (RAM)** — persist a small `.tar.xz` of binaries in rootfs, unpack to the 107 MB
   tmpfs at boot via a tiny loader; running binaries never touch flash.
5. **Lean custom `mtd2`** — reflash a rootfs that drops more vendor cruft.

### What the size plan got right, and what it did not

The original recommendation was "keep the low-level glue in C, write OCBM in Rust". **The C half was
not taken** — every box daemon is Rust — and the "hard measured number" it deferred was produced (see
the measured blocks above). What held: the Rust size profile, UPX packing, and the box's scope —
pairing + key-derivation + the RTSP relay, with no decode on the box.
