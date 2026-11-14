# Connect speed: release notes draft (14 Nov 2026: OCBM 2.5.0 and Carlink 3.10, not merged)

Scott (14 Nov): release OCBM as 2.5.0 and Carlink as 3.10 (v310). There are no 2.0.1 or 3.03 releases: 2.0.1 is the
benched set's version number only. Nothing here is released yet.

## What would ship

- **OCBM 2.5.0: `rel-2.0.1` at 92ab7a5** (PR #3 to main). The changes are efeb113 to a9392a5, 8 commits on v2.0.0,
  plus the update tool's fix (015598b) and the version (88afbfb set 2.0.1 for the bench, 92ab7a5 sets 2.5.0; both
  VERSION only). There is no protocol change beyond a new MGMT_INFO field, "preload". The release files are the Mac's
  build from 92ab7a5 with the benched 2.0.1 set's program files (2.0.0's bytes for the unchanged iap2d and
  iap_role_switch).
- **Carlink 3.10 (v310): `connect-speed-2` at 06c1d09e plus the [310] commit** (version 3.10; the v2.5.0 and
  installer-2.5.0 pins; Reset connection says "About 20 seconds"). 387985e0 is the benched build; 06c1d09e adds the low
  items from the Mac's check of the pair: every start's SUBSCRIBE clears a cut again, the CT_STOP stamp counts a written
  CT_STOP, and tests. It is v302 plus:
  - the Tide water moving while driving (e165f5fe, merged to main on 12 Nov);
  - `bench-1113`: the wake connecting at once, the Phones call-order line and the engine type selector;
  - the fixes for a display off/on during a connect (the 13 Nov C1 and the 14 Nov reviews' W1-W9, with W1 on
    elapsedRealtime, and the round 3 review's fixes: only a stop that sent CT_STOP holds the next connect);
  - the cold-start log line, and tests.
  It works with OCBM 2.0.0 too.
- **Not in this release:**
  - the OCBM `connect-speed-2` line (8a7e802: one Bluetooth power cycle, the cut of a stuck SDP answer, the woken
    supervisor), after its own bench;
  - the bench hooks (debug builds and /tmp files only);
  - Carlink `fuel-discounts`.

## Release texts

For the Mac to publish once Scott OKs them:

- `releases/v2.5.0.md`: release v2.5.0, "OCBM 2.5.0", with ocbm-update-2.5.0.tgz, its .sha256 and
  fdk-aac-2.0.3.tar.gz, as for 2.0.0.
- `releases/installer-2.5.0.md`: release installer-2.5.0, "OCBM 2.5.0 installer image", with A15W_Update.img, its
  .sha256 and fdk-aac-2.0.3.tar.gz, as for 2.0.0.
- `releases/carlink-v310.txt`: Carlink v310's annotated tag message (`git tag -a v310 -F releases/carlink-v310.txt`).
  Its subject is the release title and its body the notes; the workflow adds the commit list.

The bench figures in them are the Mac's bench of the 2.0.1 set with Carlink 387985e0, one iPhone listed first and in
range: cold connect after an adapter restart, Carlink's USB OK to the CarPlay picture, median 12.1 s over 3 runs,
against 22.5 s (1 run) on the released 2.0.0; wake with the adapter powered, display on to the picture, median 11.3 s
over 5 runs; Bluetooth up 1.5 s after the adapter sees the car, against 11.3 s.

## Upgrade notes

- **The boot preload is on by default.** It loads the radio drivers and attaches Bluetooth at power-up, with nothing a
  phone can see: no Wi-Fi network, and Bluetooth powered down. To turn it off, create `/script/radio_preload_off`.
- **Its guard.** After two unfinished preloads with none finishing between (a hang, or power lost inside its 7 s), the
  adapter switches it off, writes `guard: 2 unfinished in a row` into that flag, and logs it at every boot. Carlink logs
  `boot radio preload off_guard`. One unfinished preload doesn't count on its own: a power dip such as an engine crank
  is not a hang. To turn it back on, delete the flag.
- **Installs, updates and the go-back to stock** clear the flag, the in-progress marker and the count. A new version
  starts with the preload on.
- **Trial vs final.**
  - On a trial adapter, or a version still in its update trial, the preload waits until the start-up self-test passes
    (at most 120 s; with no pass that boot skips it). It shares a first boot with nothing new until the self-test passes.
  - On a final adapter it starts at boot.
  - In both, a host that arrives first takes the radios: the preload steps aside, and never powers Bluetooth off under a
    session.
  - A host that arrives while the preload runs waits for it (up to about 10 s), then finds Bluetooth already up.
- **Updating from 1.0.1 or later:** Carlink 3.10 offers it in the app. An adapter still in its install trial finishes its
  install first (Remove stock firmware), as for 2.0.0: the package needs about 3141 KB of flash, and a trial adapter has
  1252 KB. An adapter that ran a pre-release build (85bff31 to 9b228f8) may hold an older single-event flag; the
  update's first start clears it.
- **Going back to stock from a finished adapter wipes its paired phones.** Forget the adapter (CarLink-xxxx) on each phone
  first, and pair them again afterwards.

## Before merging

- [x] Scott picked Carlink's base: 06c1d09e (14 Nov). [310] also says "About 20 seconds" for Reset connection.
- [x] Scott picked the OCBM release set (14 Nov): the Mac's build with the benched program files (benched as 2.0.1:
  image 718a00b3, package 6a561dd0), rebuilt as 2.5.0 from 92ab7a5.
- [x] The package checked against every published version from 1.0.1 to 2.0.0: for 6a561dd0 the Mac checked 1.0.1,
  1.0.3-1.0.5 and 2.0.0, and from git that the four scripts it leaves out are identical at every tag from v1.0.2 to
  v2.0.0; 1.0.2's and 1.0.6's published package sums agree.
- [ ] The Mac's bench of the 2.0.1 set with Carlink 387985e0 (figures above):
  - on the published installer-2.0.0, finished (FINAL), first: a connect, and a display ON at the grace end and about
    2 s after it;
  - an older preload flag planted on that 2.0.0 adapter, then the in-app update to the 2.0.1 package through the
    387985e0 debug build; the flag is gone after it;
  - on 2.0.1: wakes, a cold start, display flickers in the wake's open, the grace-end ONs, and a reboot in final mode;
  - Close app, Reset connection and a refused handover, each with the page left mid-way; a reopen within 6 s of
    Close app.
- [ ] The 2.5.0 files: built from 92ab7a5 with the benched program files, they differ from the benched 2.0.1 set only
  in VERSION and SHA256SUMS; the package carries the same 18 files, FROM_MIN 1.0.1.
- [ ] The Mac's dev adapter updated in the app from 2.0.1 to 2.5.0.
- [ ] [310] pins the Mac's sha256s and sizes; the Mac's gate and 20-minute check of its debug build on the dev adapter.
- [ ] Scott OKs the release texts and gives the go. The Mac merges both PRs (OCBM #3, Carlink connect-speed-2), publishes
  v2.5.0 and installer-2.5.0 with gh from `releases/`, and tags v310 from `releases/carlink-v310.txt`.
- [ ] After publishing: the published assets' sha256s and sizes equal [310]'s pins, fromMin is 1.0.1, needKb comes from
  the package's ROOM, and the installer pin points at installer-2.5.0. One connect with v310's minified release APK.
- [ ] A truck check after the release, as with 2.0.0.
