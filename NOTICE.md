# Third-party material and reference points

The code in this repository is original work. Nothing here is copied from another
project. Several external sources were used as *reference* — to learn a wire format, a
constant, or a behaviour — and are recorded here so the distinction is on the record.

## Referenced, not incorporated

**Apple CarPlay Communication Plug-in SDK** — licensed first-party material accessed under
an active Apple Developer Program membership, used to check protocol conformance. It is not
redistributed here: the reference tree is outside this repository and excluded from it. See
`docs/ops/07_AUTHORIZATION.md` for the scope and authorization statement.

## Linked at build time, not vendored

**libfdk-aac 2.0.3** (Fraunhofer FDK AAC Codec Library) — the AAC-ELD encoder for the CarPlay
mic uplink links against it (`crates/vendor/eld-codec`, behind the `mic-uplink-eld` feature), so
the adapter's `carplayd` contains it. Its source is not included here and is built separately,
from tag `v2.0.3` of https://github.com/mstorsjo/fdk-aac. It is Fraunhofer's software under
Fraunhofer's own licence, which this dedication does not and cannot waive:

- the licence is [`ccpa/carplayd/fdk-aac.NOTICE`](ccpa/carplayd/fdk-aac.NOTICE), the library's
  `NOTICE` file verbatim;
- every installer image carries the same file as `tmp/ocbm/NOTICE.fdk-aac`, and each release's
  notes repeat it;
- each OCBM release from 1.0.5 on carries the library's source as it was built (a `git archive`
  of tag `v2.0.3`) as a release asset.

## Patents

This dedication waives copyright. It grants no patent rights, and none are implied. Media
codecs (AAC, H.264, HEVC) and accessory authentication in this domain are patent-encumbered
independently of copyright; that is the user's responsibility to assess.
