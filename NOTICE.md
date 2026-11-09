# Third-party material and reference points

The code in this repository is original work. Nothing here is copied from another
project. Several external sources were used as *reference* — to learn a wire format, a
constant, or a behaviour — and are recorded here so the distinction is on the record.

## Referenced, not incorporated

**gearhead** (the Android Auto phone app) — decompiled to establish protocol facts:
channel ordinals, keyframe-interval behaviour by requested protocol version, the
`InputReport` field layout, and the keycode set the phone echoes back.

**Apple CarPlay Communication Plug-in SDK** — licensed first-party material accessed under
an active Apple Developer Program membership, used to check protocol conformance. It is not
redistributed here: the reference tree is outside this repository and excluded from it. See
`docs/ops/07_AUTHORIZATION.md` for the scope and authorization statement.

## Linked at build time, not vendored

**libfdk-aac** (Fraunhofer FDK AAC Codec Library) — the AAC-ELD encoder for the CarPlay mic
uplink links against it (`crates/vendor/eld-codec`, behind the `mic-uplink-eld` feature). Its
source is not included here and is built separately. Binaries you distribute that link it
carry Fraunhofer's own license and notice requirements, which this dedication does not and
cannot waive.

## Patents

This dedication waives copyright. It grants no patent rights, and none are implied. Media
codecs (AAC, H.264, HEVC) and accessory authentication in this domain are patent-encumbered
independently of copyright; that is the user's responsibility to assess.
