# Connect speed: release notes draft (14 Nov 2026, round 3, not merged)

Scott's scope (14 Nov): release OCBM a9392a5 first. Nothing here is released yet; it waits for the bench below.

## What would ship

- **OCBM 2.0.1: a9392a5 plus its version bump to 2.0.1** (88afbfb on `rel-2.0.1`, VERSION only). The changes are efeb113
  to a9392a5, 8 commits on v2.0.0; the protocol is unchanged.
- **Carlink 3.03 from `connect-speed-2` at 9146fffe**, pinning OCBM 2.0.1. It is v302 plus:
  - the Tide water moving while driving (e165f5fe, merged to main on 12 Nov);
  - `bench-1113`: the wake connecting at once, the Phones call-order line and the engine type selector;
  - the fixes for a display off/on during a connect (the 13 Nov C1 and the 14 Nov reviews' W1-W9, with W1 on
    elapsedRealtime);
  - the cold-start log line, and tests.
  It works with OCBM 2.0.0 too.
- **Not in this release:**
  - the OCBM `connect-speed-2` line (8a3a8b2: one Bluetooth power cycle, the cut of a stuck SDP answer, the woken
    supervisor), after its own bench;
  - the bench hooks (debug builds and /tmp files only);
  - Carlink `fuel-discounts`.

## OCBM: GitHub release notes

**Faster wireless connects.** On the Mac's bench a cold connect now takes 14-17 s from Carlink opening the adapter to
the CarPlay picture, against about 26-28 s on 2.0.0 measured the same way. A connect when the display wakes takes about
15 s. Both were measured with the first-listed phone away; with the driver's own phone listed first, about 10-11 s is
expected (an estimate, not yet measured).

- The adapter readies Bluetooth and the Wi-Fi driver while the car starts (the boot preload). The first connect after
  power-up then skips the driver and firmware load.
- Bluetooth and the Wi-Fi network come up side by side. The adapter tries each phone briefly first (about 4 s each), in
  the order set in Carlink's Phones page.
- A phone whose page timed out is tried last in the next round. For two minutes from when the adapter starts its radios,
  rounds are 5 s apart, so a phone that comes back then is found within about 5-7 s; after that the gap grows from 10 s
  to 60 s.
- A phone that answers and then drops the link straight after another phone's page timed out is no longer read as having
  forgotten the pairing.
- If the phone drops its screen stream, the adapter asks for the picture again after 0.5 s, and once more at 3 s.
- One `[startup]` line per session times each stage. ocbmd logs every hello from Carlink with the adapter's uptime.

## Upgrade notes

- **The boot preload is on by default.** It loads the radio drivers and attaches Bluetooth at power-up, with nothing a
  phone can see: no Wi-Fi network, and Bluetooth powered down. To turn it off, create `/script/radio_preload_off`.
- **Its guard.** If two boots in a row never finish the preload (a hang, or power lost inside its 7 s), the adapter
  switches it off, writes `guard: 2 unfinished in a row` into that flag, and logs it at every boot. Carlink logs
  `boot radio preload off_guard`. One unfinished preload doesn't count on its own: a power dip such as an engine crank
  is not a hang. To turn it back on, delete the flag.
- **Installs, updates and the go-back to stock** clear the flag, the in-progress marker and the count. A new version
  starts with the preload on.
- **Trial vs final.**
  - On a trial adapter, or a version still in its update trial, the preload waits until the start-up self-test passes.
    It shares a first boot with nothing new until the self-test passes.
  - On a final adapter it starts at boot.
  - In both, a host that arrives first takes the radios: the preload steps aside, and never powers Bluetooth off under a
    session.
  - A host that arrives while the preload runs waits for it (up to about 7 s), then finds Bluetooth already up.
- **Updating from 2.0.0** needs nothing special. An adapter that ran a pre-release build (85bff31 to 9b228f8) may hold an
  older single-event flag; the update's first start clears it.

## Carlink 3.03: commit notes

- When the head unit's display wakes, Carlink connects at once while the adapter is still listed. It keeps the 2.5 s USB
  settle only for an adapter that isn't listed, or an open that failed. That open waits 3 s for the adapter's first reply
  instead of 20 s.
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

- [ ] The 2.0.1 build's byte check: its installer image differs from a9392a5's only in tmp/ocbm/VERSION and SHA256SUMS
  (BUDGET the same), and its update package is built --from the 1.0.1 SHA256SUMS, so FROM_MIN stays 1.0.1.
- [ ] The Mac's bench of OCBM 2.0.1 (a9392a5 + its VERSION bump) with Carlink 9146fffe:
  - on the published 2.0.0 first: a connect, and a display ON at the grace end and about 2 s after it;
  - the in-app update of that 2.0.0 adapter to the 2.0.1 package;
  - on 2.0.1: wakes, a cold start, display flickers in the wake's open, the grace-end ONs, and a reboot in final mode;
  - Close app, Reset connection and a refused handover, each with the page left mid-way; a reopen within 6 s of
    Close app.
- [ ] Scott publishes 2.0.1, then the Carlink [303] commit pins it, and one connect with [303]'s minified release APK.
- [ ] A truck check after the release, as with 2.0.0.
