# Connect speed: release notes draft (13 Nov 2026, not merged)

For Scott's merge decision once the Mac's bench passes. Nothing here is released.

## What would ship

- **OCBM `connect-speed` at a9392a5**: the commits the Mac is benching (85bff31 to a9392a5). A version number is yours to choose; a patch release (2.0.1) fits, since nothing in the protocol changed.
- **Carlink `bench-1113` at 7f9485f1**: `connect-wake`, `phones-order-note` and `engine-auto`, as one build. As a release it would be 3.03, pinning the new OCBM.
- **Not in this release:** OCBM `connect-speed-2` (376c4ce) and Carlink `connect-speed-2` (18f1f525). They are the next round, built on the bench commits, and not benched yet.

## OCBM: GitHub release notes

**Faster wireless connects.** On the Mac's bench a cold connect now takes 14-17 s from Carlink's start to the CarPlay picture, down from 33-42 s on 2.0.0. A connect when the display wakes takes about 15 s.

- The adapter readies Bluetooth and the Wi-Fi driver while the car starts (the boot preload). The first connect after power-up then skips the driver and firmware load.
- Bluetooth and the Wi-Fi network come up side by side. The adapter tries each phone briefly first (about 4 s each), in the order set in Carlink's Phones page.
- A phone whose page timed out is tried last in the next round. A phone that comes back within two minutes is found within 5 s.
- A phone that answers and then drops the link is no longer read as having forgotten the pairing.
- If the phone drops its screen stream, the adapter asks for the picture again after 0.5 s, and once more at 3 s.
- One `[startup]` line per session times each stage. ocbmd logs every hello from Carlink with the adapter's uptime.

## Upgrade notes

- **The boot preload is on by default.** It loads the radio drivers and attaches Bluetooth at power-up, with nothing a phone can see: no Wi-Fi network, and Bluetooth powered down. To turn it off, create `/script/radio_preload_off`.
- **Its guard.** If two boots in a row never finish the preload (a hang, or power lost inside its 7 s), the adapter switches it off, writes `guard: 2 unfinished in a row` into that flag, and logs it at every boot. Carlink logs `boot radio preload off_guard`. One unfinished preload doesn't count on its own: a power dip such as an engine crank is not a hang. To turn it back on, delete the flag.
- **Installs, updates and the go-back to stock** clear the flag, the in-progress marker and the count. A new version starts with the preload on.
- **Trial vs final.**
  - On a trial adapter, or a version still in its update trial, the preload waits until the start-up self-test passes. It never shares a first boot with anything new.
  - On a final adapter it starts at boot.
  - In both, a host that arrives first takes the radios: the preload steps aside, and never powers Bluetooth off under a session.
- **Updating from 2.0.0** needs nothing special. An adapter that ran a pre-release build (85bff31 to 9b228f8) may hold an older single-event flag; the update's first start clears it.

## Carlink 3.03: commit notes

- When the head unit's display wakes, Carlink connects at once while the adapter is still listed. It keeps the 2.5 s USB settle only for an adapter that isn't listed, or an open that failed. That open waits 3 s for the adapter's first reply instead of 20 s.
- A start interrupted by the display going off no longer leaves Carlink stuck in Connecting.
- The Phones page explains the call order when two or more phones are listed.
- Settings › Vehicle › Engine type: no "From the car" option. The car's own type shows selected until the driver picks one, and a pick is remembered.
- Logs: the adapter's boot preload state, and a `[startup] wake:` line timing each wake.

## Before merging

- [ ] The Mac's bench of a9392a5 with the `bench-1113` APK: the re-plan (R1) and the wake series.
- [ ] Your OK on the OCBM version number, and the Carlink [303] commit pinning it.
- [ ] A truck check after the release, as with 2.0.0.
