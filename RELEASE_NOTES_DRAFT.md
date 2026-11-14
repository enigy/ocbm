# Connect speed: release notes draft (14 Nov 2026, after the Mac's check of the release pair, not merged)

Scott's scope (14 Nov): release OCBM a9392a5 first. Nothing here is released yet; it waits for the bench below.

## What would ship

- **OCBM 2.0.1: a9392a5 plus its version bump to 2.0.1** (88afbfb on `rel-2.0.1`, VERSION only; 015598b after it
  changes only tools/make_ocbm_update.sh). The changes are efeb113 to a9392a5, 8 commits on v2.0.0. There is no
  protocol change beyond a new MGMT_INFO field, "preload".
- **Carlink 3.03: `connect-speed-2` at 06c1d09e plus the [303] commit** (version 3.03; the v2.0.1 and installer-2.0.1
  pins). 387985e0, the benched build, is version 3.02 and pins 2.0.0; 06c1d09e adds the low items from the Mac's check
  of the pair: every start's SUBSCRIBE clears a cut again, the CT_STOP stamp counts a written CT_STOP, and tests. It is
  v302 plus:
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

## OCBM: GitHub release notes

**Faster wireless connects.** On the Mac's bench of a pre-release build (9b228f8, 13 Nov), a cold connect took a median
13.2 s from the adapter's USB link coming up to the CarPlay picture. [To replace with the medians from the Mac's bench of
2.0.1 with Carlink 387985e0, one measure throughout: a cold start and a display wake, and 2.0.0 measured the same way.]

- The adapter readies Bluetooth and the Wi-Fi driver while the car starts (the boot preload). The first connect after
  power-up then skips the driver and firmware load.
- Bluetooth and the Wi-Fi network come up side by side. The adapter tries each phone briefly first (about 4 s each), in
  the order set in Carlink's Phones page.
- A phone whose page timed out is tried last in the next round. For two minutes from when the adapter starts its radios,
  rounds are 5 s apart, so a phone that comes back then is found within about 5-7 s, plus about 5 s for each phone listed
  ahead of it that is still away; after that the gap grows from 10 s to 60 s.
- A phone that answers and then drops the link straight after another phone's page timed out is no longer read as having
  forgotten the pairing.
- If the phone drops its screen stream, the adapter asks for the picture again after 0.5 s, and once more at 3 s.
- One `[startup]` line per session times each stage. ocbmd logs every hello from Carlink with the adapter's uptime.

**From 2.0.0, for adapters on 1.0.x** (they update straight to 2.0.1):

- **Video from the iPhone, parked (route A).** The adapter offers AirPlay video inside the CarPlay session (iOS 26 and
  later). With Carlink 3.00 or later, a video you send from the iPhone (Safari, for example) plays full screen on the
  car's screen while the car is parked. It is fetched through the iPhone's own network, never the car's. The iPhone
  decides what it offers; FairPlay-protected video (the TV app's) stays on the iPhone.
- **The car's GPS and status for the iPhone**, when Carlink pushes them (Settings › Vehicle).
- **A stuck wireless session ends by itself.** A Bluetooth session whose iPhone never reaches CarPlay ends 90 s in, and
  the phones are called again, so CarPlay comes back once the iPhone's Wi-Fi does.
- **One name.** The adapter's Wi-Fi has the same name as its Bluetooth, CarLink-<4 hex>.
- Retired: the AirPlay video probe's switch file (/etc/ocbm/airplay_probe) is ignored, and protocol command 0x14 (the
  screen borrow) is gone.

## Upgrade notes

- **The boot preload is on by default.** It loads the radio drivers and attaches Bluetooth at power-up, with nothing a
  phone can see: no Wi-Fi network, and Bluetooth powered down. To turn it off, create `/script/radio_preload_off`.
- **Its guard.** After two unfinished preloads with none finishing between (a hang, or power lost inside its 7 s), the adapter
  switches it off, writes `guard: 2 unfinished in a row` into that flag, and logs it at every boot. Carlink logs
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
- **Updating from 1.0.1 or later:** Carlink 3.03 offers it in the app. An adapter still in its install trial finishes its
  install first (Remove stock firmware), as for 2.0.0: the package needs about 3141 KB of flash, and a trial adapter has
  1252 KB. An adapter that ran a pre-release build (85bff31 to 9b228f8) may hold an older single-event flag; the
  update's first start clears it.

## Carlink 3.03: commit notes

- When the head unit's display wakes, Carlink connects at once while the adapter is still listed. It keeps the 2.5 s USB
  settle only for an adapter that isn't listed, or an open that failed. The open at once waits 3 s for the adapter's
  first reply instead of 20 s.
- Offers the adapter update to OCBM 2.0.1, from 1.0.1 or later.
- A display going off and on during a connect no longer leaves Carlink stuck in Connecting, nor ends a session that had
  already come up. Close app and a handover stay closed.
- Any connect within 6 s of Carlink's own stop (the display-off wait ending a session, Close app, a reset) waits for the
  adapter to finish tearing that session down, on OCBM 2.0.0 and 2.0.1; the adapter-side fix comes with
  `connect-speed-2`.
- Reset connection, Close app and a handover finish even when you leave the settings page during them.
- The Phones page explains the call order when two or more phones are listed.
- Settings › Vehicle › Engine type: no "From the car" option. The car's own type shows selected (Gasoline, the adapter's
  default, until the car reports one) until the driver picks one, and a pick is remembered.
- The Tide water moves while driving too.
- Logs: the adapter's boot preload state, a `[startup] wake:` line timing each wake, and a `[startup] carlink:` line at a
  cold start.

## Before merging

- [ ] Scott picks Carlink 3.03's base: 06c1d09e, as above, or the benched 387985e0 with those items left for later.
- [ ] Scott picks the OCBM release set: the Mac's corrected build (image 718a00b3, package 6a561dd0, with 2.0.0's bytes for
  the unchanged iap2d and iap_role_switch; benched byte for byte) or a rebuild.
- [ ] The 2.0.1 build's byte check: its installer image differs from a9392a5's only in tmp/ocbm/VERSION and SHA256SUMS
  (BUDGET the same), and its update package is built with a --from for every published version from 1.0.1 to 2.0.0
  (015598b's tool), so FROM_MIN stays 1.0.1.
- [ ] The 2.0.1 package checked against every published version from 1.0.1 to 2.0.0: an adapter on each finds every
  file it lacks. For 6a561dd0: the Mac checked 1.0.1, 1.0.3-1.0.5 and 2.0.0; the files it leaves out are also the
  same in 1.0.2's and 1.0.6's published package sums (build agent, 14 Nov).
- [ ] The Mac's bench of OCBM 2.0.1 (a9392a5 + its VERSION bump) with Carlink 387985e0:
  - on the published installer-2.0.0, finished (FINAL), first: a connect, and a display ON at the grace end and about
    2 s after it;
  - an older preload flag planted on that 2.0.0 adapter, then the in-app update to the 2.0.1 package through the
    387985e0 debug build; the flag is gone after it;
  - on 2.0.1: wakes, a cold start, display flickers in the wake's open, the grace-end ONs, and a reboot in final mode;
  - Close app, Reset connection and a refused handover, each with the page left mid-way; a reopen within 6 s of
    Close app.
- [ ] The benched package's sha256 is the one published.
- [ ] Scott publishes 2.0.1, then the Carlink [303] commit pins it, and one connect with [303]'s minified release APK.
- [ ] After [303]: the pin's tag, url, size and sha256 equal the published asset, fromMin is 1.0.1, needKb comes from
  the package's ROOM, and the installer pin points at installer-2.0.1.
- [ ] A truck check after the release, as with 2.0.0.
