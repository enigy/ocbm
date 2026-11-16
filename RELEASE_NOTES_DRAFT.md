# Two phones and connect speed 2: release notes (released 15 Nov 2026 as OCBM 2.6.0 and Carlink 3.20)

OCBM 2.6.0 and Carlink 3.20 (v320, versionCode 320). Released 15 Nov: v2.6.0 and installer-2.6.0 at e472eb1, Carlink
v320 at 2ba6013, and Carlink PR #6 merged (main 1b1c258). The notes for 2.5.0 and 3.10, released 14 Nov, are this file
at e31f249.

## What shipped

- **OCBM 2.6.0: `bench-hooks`, merged to main by PR #4 (e472eb1)** (the release files are the Mac's build of e472eb1).
  The changes are 376c4ce to 5d8f5c8, 24 commits on 2.5.0 (main, merged in at 2318bb5), plus the version (97936c6,
  VERSION only):
  - connect speed 2 (376c4ce, c4679c0, bbd29f3, 251b590, 8a3a8b2, eb0ac2e, 8a7e802, 90fcd2f, e27af19, eb8a1f0): one
    Bluetooth power cycle, the supervisor woken by ocbmd when a host arrives, the accepts every 10 ms, the MFi
    certificate read once per boot and cached only as one DER SEQUENCE (0xAA04 deletes it), the SDP channel's 0.7 s cut
    (counted once a round toward BTP_BOND_LOST), the Wi-Fi key kept in /etc/ocbm/wifi_psk, the kept key and
    /etc/wifi_password read through `tr -cd ' -~'` (BusyBox 1.32.1's tr has no [:print:]) and any printable key written
    with awk from ENVIRON, udhcpd's ARP probe at 250 ms, and the supervisor's teardown race;
  - two phones (2769b69, 8787548): btd names a bonded phone as its Bluetooth session starts, while no CarPlay session is
    up, and carplayd browses again and dials it when the name changes; a phone a switch dropped is never named or
    dialled;
  - the bench hooks (d8cdb5e to ca2bf3a, 9a5530b and 8ee2336): files in /tmp only;
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
  - the wake fixes (5b825bdf; 8830395d's W); 8830395d's CL-8 puts back the seconds 83f71cf had turned into ms, so a
    release build logs "30 s" as 3.10 did;
  - the bench hooks (7d586216 to 8830395d):
    - the receiver is in the debug source set only (no BenchReceiver or carlink3.BENCH in the release dex);
    - the main source set gains only what the receiver drives (CarlinkManager.boxFileTail, pushToBox and dormant,
      DisplayOff.graceMs, AdapterUpdateSession.benchUpdate), and setPhoneOrder, which the Phones page's arrows also
      use;
  - test data with made-up addresses and names only (5b460b96, 075f119e, e2f235ae);
  - PRIVACY.md: the fuel discounts 3.20 keeps (2ba6013, the Mac).
  There is no protocol change, so it should work with OCBM 2.5.0 and 2.0.0 too (not benched).

## Not in this release

For 2.6.x and later; each is also an entry in docs/ops/04_OPEN_ITEMS.md.

- carplayd-1: a capped rename-path dial that misses has no retry and waits for the 90 s never-reached end;
- carplayd-2 (fresh resolves read inside rename()'s replay loop get RENAME_ROUNDS), carplayd-3 (forget_phone can repeat
  a dial made moments earlier under the new name) and btd-p1-carlink-1 (put_back's Connected should be provisional);
- e2e-4 (the same phone back within ~5 s while its own dead session is held is never re-checked; backstop:
  carplay_never_reached at 90 s), btd-5 and btd-6 (from the Mac's round 5 review) and e2e-3 (watch for a second
  connect-out before SETUP);
- MFI-CACHE-SCOPE: btd's Bluetooth iAP2 and carplayd's AirPlay auth-setup still read the chip each session;
- Carlink's lane watcher matching "RX 0x1D03" (after 3.20);
- a Linux CI job for the BusyBox suites under BusyBox 1.32.1;
- the doubled [update] lines in box.log (ocbmd's start_update).

Kept by Scott as a known behaviour: another paired phone's 0x5703 takes a live session in about 1-2 s when the
streaming phone's Bluetooth drops.

## Release texts

Published 15 Nov, on Scott's OK:

- `releases/v2.6.0.md`: release v2.6.0, "OCBM 2.6.0", with ocbm-update-2.6.0.tgz, its .sha256 and
  fdk-aac-2.0.3.tar.gz, as for 2.5.0.
- `releases/installer-2.6.0.md`: release installer-2.6.0, "OCBM 2.6.0 installer image", with A15W_Update.img, its
  .sha256 and fdk-aac-2.0.3.tar.gz, as for 2.5.0.
- `releases/carlink-v320.txt`: Carlink v320's annotated tag message (`git tag -a v320 -F releases/carlink-v320.txt`).
  Its subject is the release title and its body the notes; the workflow adds the commit list. The tagged commit's
  subject starts "[320]".

Their two placeholders were filled from the Mac's build: the commit e472eb1 (twice in each OCBM text) and need_kb 3190
(v2.6.0.md).

The bench figures in them are the Mac's bench of the 2.6.0 candidate with two iPhones: a change of phone, from the phone
joining the adapter's Wi-Fi to the CarPlay picture, 4.6 s from B to A (44 s before) and 5.7 s from A to B (91 s
before); the same phone back 6.6 s (round 5, 821d8fa, one run; 6.3 s before the fix, so no change); a Carlink phone
switch about 9 s from the tap; a cold start from USB OK to the picture 12.6 s (round 6, fe351b0) and 15.6 s (round 5,
821d8fa), one run each, both with phone B (the older iPhone). 2.5.0's 12.1 s median was with phone A (the newer one),
and with phone A round 1 (fd4769c) gave 9.3, 10.3 and 9.3 s. The released 2.5.0 on the Mac's bench: cold 12.1 s, wakes
11.4, 16.6 and 10.6 s, Reset connection about 19 s. No wake or Reset run on the final programs (fe351b0). Earlier
2.6.0 candidates gave a wake of 12.0 s (round 5, 821d8fa, one run, dormant 30 s), wakes of 8.8, 11.2 and 9.3 s (round
1, fd4769c), and a Reset connection of about 18 s (round 3, 043b7e1, phone B).

## Upgrade notes

- **Updating from 1.0.1 or later:** Carlink 3.20 offers it in the app. An adapter still in its install trial finishes
  its install first (Settings › Adapter and about › Remove stock CarPlay), as for 2.5.0: the package needs about
  3190 KB of flash, and a trial adapter has 1252 KB.
- **The Wi-Fi password** is kept in /etc/ocbm/wifi_psk from 2.6.0 on, and a trial's go-back to stock (ocbm_revert.sh)
  leaves it for the next install. The adapter takes hostapd.conf's own first, then the kept one, then
  /etc/wifi_password, then a new random one; stock's known default counts as none. Whether a finished adapter's stock
  restore keeps the key is to check, as the two paths read differently: ocbm_restore.sh runs stock_restore_box.sh
  (onefile profile), whose remove_guarded/is_radio spares `*wifi*`, so it keeps /etc/ocbm/wifi_psk;
  tools/stock_restore_onefile_tree.sh:145 drops /etc/ocbm. Settle which path a release adapter runs.
- **Bench hooks** (docs/ops/02_TESTING.md): /tmp/bench_phantom_first, /tmp/bench_order, /tmp/bench_skip_first_requestui
  and /tmp/bench_update_any_version, and the SDP window's lever /tmp/bench_sdp_after_acl_ms (or BT_SDP_AFTER_ACL_MS).
  Nothing in the product writes them; a Carlink debug build can (box_flag), and "off" reads as unset. A restart clears
  them. /tmp/bench_hooks and /tmp/sdp_after_acl_ms are read by nothing at e472eb1.
- **The [startup] line** gives the adapter's uptime at host present and the preload's state then, and ends "; since the
  host came: absent pages, resets, PSK mismatches; this session: nudges".
- **Going back to stock from a finished adapter wipes its paired phones.** Forget the adapter (CarLink-xxxx) on each phone
  first, and pair them again afterwards.

## Before merging

- [x] Merged: OCBM bench-hooks as PR #4 (e472eb1); Carlink bench-hooks and fuel-discounts as PRs #4 and #5 (6cb924a);
  Carlink rel-3.20 ([320] 3eff123d + PRIVACY.md 2ba6013) as PR #6 (1b1c258).
- [x] The release set's programs byte-identical to the benched fe351b0's (the Mac, 15 Nov 19:05): built from e472eb1:
  btd 4c6fef05, carplayd 7006bf36, ocbmd f20eae3c; 2.0.0's builds: iap2d 69bcbdaa, iap_role_switch 7b8e00e9.
- [x] check_pins.py 13/13 on rel-3.20; the Mac's gate of 3eff123d green; PR #6's 6 checks green.
- [x] Scott read the drafts. v2.6.0 + installer-2.6.0 published 15 Nov (installer-2.6.0 latest); the public files = the
  Mac's release-260.
- [x] v320 (annotated) on 2ba6013, message = releases/carlink-v320.txt @2af59ed; release.yml started.
- [ ] release-notes-2.6.0 merged to main by PR (merge commit).
