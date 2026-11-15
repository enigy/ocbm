# Two phones and connect speed 2: release notes draft (15 Nov 2026: OCBM 2.6.0 and Carlink 3.20, not merged)

OCBM 2.6.0 and Carlink 3.20 (v320, versionCode 320). Nothing here is released yet. The notes for 2.5.0 and 3.10,
released 14 Nov, are this file at e31f249.

## What would ship

- **OCBM 2.6.0: `bench-hooks`** (a merge-commit PR to main; the release files are the Mac's build from <COMMIT>). The
  changes are 376c4ce to 5d8f5c8, 24 commits on 2.5.0 (main, merged in at 2318bb5), plus the version (97936c6, VERSION
  only):
  - connect speed 2 (376c4ce, c4679c0, bbd29f3, 251b590, 8a3a8b2, eb0ac2e, 8a7e802, 90fcd2f, e27af19, eb8a1f0): one
    Bluetooth power cycle, the supervisor woken by ocbmd when a host arrives, the accepts every 10 ms, the MFi
    certificate read once per boot and cached only as one DER SEQUENCE (0xAA04 deletes it), the SDP channel's 0.7 s cut
    (counted once a round toward BTP_BOND_LOST), the Wi-Fi key kept in /etc/ocbm/wifi_psk, the kept key and
    /etc/wifi_password read through `tr -cd ' -~'` (BusyBox 1.32.1's tr has no [:print:]) and any printable key written
    with awk from ENVIRON, udhcpd's ARP probe at 250 ms, and the supervisor's teardown race;
  - two phones (2769b69, 8787548): btd names a bonded phone as its Bluetooth session starts, while no CarPlay session is
    up, and carplayd browses again and dials it when the name changes; a phone a switch dropped is never named or
    dialled;
  - the bench hooks (d8cdb5e to 9a5530b): files in /tmp only;
  - test data with made-up addresses and names only (711e584, dd5e130, 5d8f5c8), and a repo note (3fab31a).
  There is no protocol change; BTP_BOND_LOST also counts a cut, once a round.
- **Carlink 3.20 (v320): `bench-hooks` at e2f235ae and `fuel-discounts` at 887772eb, both on 3.10 (e17947b0), plus
  the [320] commit** (version 3.20; the v2.6.0 and installer-2.6.0 pins). It is v310 plus:
  - fuel discounts (c2fff742 to 2edc31b9): Settings › Vehicle › Fuel discounts, shown with Nearby stations on; the
    listed brands, each "Not a member" until set, and another station by name; Costco and Sam's Club left out; "Diesel
    and E85 too", on by default; set while parked; one store of a brand, with its number or street, can be its own
    program;
  - the Phones rank arrows' press, and every pane control's touch area kept full size while pressed (821f57b3,
    887772eb);
  - the loading screen's 1 s fade (708b7a1a);
  - the display-off and wake fixes (5b825bdf; 8830395d's W and CL-8);
  - the bench hooks, in the debug source set only (7d586216 to 8830395d);
  - test data with made-up addresses and names only (5b460b96, 075f119e, e2f235ae).
  There is no protocol change, so it should work with OCBM 2.5.0 and 2.0.0 too (not benched).

## Release texts

For the Mac to publish once Scott OKs them:

- `releases/v2.6.0.md`: release v2.6.0, "OCBM 2.6.0", with ocbm-update-2.6.0.tgz, its .sha256 and
  fdk-aac-2.0.3.tar.gz, as for 2.5.0.
- `releases/installer-2.6.0.md`: release installer-2.6.0, "OCBM 2.6.0 installer image", with A15W_Update.img, its
  .sha256 and fdk-aac-2.0.3.tar.gz, as for 2.5.0.
- `releases/carlink-v320.txt`: Carlink v320's annotated tag message (`git tag -a v320 -F releases/carlink-v320.txt`).
  Its subject is the release title and its body the notes; the workflow adds the commit list. The tagged commit's
  subject starts "[320]".

They hold two placeholders for the Mac's build: <COMMIT> (twice in each OCBM text) and <NEED_KB> (v2.6.0.md).

The bench figures in them are the Mac's bench of the 2.6.0 candidate with two iPhones: a change of phone, from the phone
joining the adapter's Wi-Fi to the CarPlay picture, 4.6 s from B to A (44 s before) and 5.7 s from A to B (91 s
before); the same phone back about 6 s; a Carlink phone switch about 9 s from the tap; a cold start 12.6 s from USB OK
to the picture. The released 2.5.0 on the Mac's bench: cold 12.1 s, wakes 11.4, 16.6 and 10.6 s, Reset connection
about 19 s. There are no 2.6.0 wake or Reset figures.

## Upgrade notes

- **Updating from 1.0.1 or later:** Carlink 3.20 offers it in the app. An adapter still in its install trial finishes
  its install first (Settings › Adapter and about › Remove stock CarPlay), as for 2.5.0: the package needs about
  <NEED_KB> KB of flash, and a trial adapter has 1252 KB.
- **The Wi-Fi password** is kept in /etc/ocbm/wifi_psk from 2.6.0 on, and a trial's go-back to stock (ocbm_revert.sh)
  leaves it for the next install. The adapter takes hostapd.conf's own first, then the kept one, then
  /etc/wifi_password, then a new random one; stock's known default counts as none. A finished adapter's stock restore
  (stock_restore_box.sh) removes the extras under /etc, so it likely does not keep the key: to check.
- **Bench hooks** (docs/ops/02_TESTING.md): /tmp/bench_phantom_first, /tmp/bench_hooks, /tmp/bench_skip_first_requestui
  and /tmp/bench_update_any_version, and the SDP window's lever /tmp/sdp_after_acl_ms. Nothing in the product writes
  them; a Carlink debug build can (box_flag), and "off" reads as unset. A restart clears them.
- **The [startup] line** gives the adapter's uptime at host present and the preload's state then, and ends "; since the
  host came: absent pages, resets, PSK mismatches; this session: nudges".
- **Going back to stock from a finished adapter wipes its paired phones.** Forget the adapter (CarLink-xxxx) on each phone
  first, and pair them again afterwards.

## Before merging

- [x] The Mac's bench of the 2.6.0 candidate with two iPhones (figures above).
- [ ] The package checked against every published version from 1.0.1 to 2.5.0: the same 18 files as 2.5.0's, of which
  btd, carplayd, ocbmd, ocbm_boot.sh, ocbm_update.sh, radio_ap_up.sh, radio_hal.sh and session_supervisor.sh change
  against 2.5.0, with VERSION, BUDGET and SHA256SUMS; FROM_MIN 1.0.1.
- [ ] The 2.6.0 files (the Mac): built from <COMMIT> ("2.6.0 ocbm <COMMIT>"). Package <PKG_SHA256> (<PKG_SIZE> B): 18
  files, FROM_MIN 1.0.1, needKb <NEED_KB>, and the updater's file check passes from all eight published versions. Image
  <IMG_SHA256> (<IMG_SIZE> B).
- [ ] The Mac's dev adapter updated in the app from 2.5.0 to 2.6.0.
- [ ] [320] pins the Mac's sha256s and sizes; the Mac's gate and test run, and a check of its debug build on the dev
  adapter.
- [ ] Scott OKs the release texts and gives the go. The Mac merges both PRs (OCBM bench-hooks, and Carlink's), publishes
  v2.6.0 and installer-2.6.0 with gh from `releases/`, and tags v320 from `releases/carlink-v320.txt`.
- [ ] After publishing: the published assets' sha256s and sizes equal [320]'s pins, fromMin is 1.0.1, needKb comes from
  the package's ROOM, and the installer pin points at installer-2.6.0. One connect with v320's minified release APK.
- [ ] A truck check after the release, as with 2.5.0.
