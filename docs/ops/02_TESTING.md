# Testing — live session plans and gates

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 46; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

What must be exercised on hardware, and the order that makes a failure legible.

## Live session test plan

<!-- absorbed: ../ops/02_TESTING.md -->

Status: executed 2026-07-25. The `'cmnd'` fix held — `RX 0xAA00 -> CertSent` present, SYN-ACK count 2,
full identification and metadata. Retained as the regression checklist for any change to the wireless
transport. Sections describing hypotheses that the run resolved are marked below.

Companion to `docs/carplay/05_METADATA_AND_CONTROLS.md`.

### The discriminator — one grep, box-side, no phone trace needed

```sh
./host/uart_cmd.sh 'grep -c "RX 0xAA00 -> CertSent" /tmp/carplayd_wl.log' 6
```

`0xAA00` is `RequestAuthenticationCertificate` — the **device's** first move after it accepts the link.
The phone cannot emit it while still retransmitting SYN-ACK. `1` ⇒ our ACK now reaches the phone. `0` ⇒
it still does not.

Faster equivalent, fires ~10 ms earlier: any inbound frame with `ctrl=0x40` (i.e. anything that is not
another SYN-ACK) proves the phone's link layer advanced.

```sh
./host/uart_cmd.sh 'grep "datastream. RX" /tmp/carplayd_wl.log | grep -c "ctrl=0x40"' 6
./host/uart_cmd.sh 'grep -c "ctrl=0xc0" /tmp/carplayd_wl.log' 6   # 1 = accepted; 31 = old behaviour
```

> **⚠️ THE THREE `ctrl=` GREPS ABOVE ARE DEAD — they read 0 on a HEALTHY box**, and a 0 on the
> `ctrl=0xc0` line reads as *better* than "accepted". Both log lines are gated behind
> `CARPLAY_EVENTS_LOG`, which no spawn site sets. Use instead
> `grep -c 'outbound sink registered' $L` ≥ 1 (sink installed) and
> `grep -c 'SETUP phase2 DataStream(130)' $L` (phone asked). [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-46-1`.

Deliberately NOT the discriminator: `[datastream] TX(RCS,'cmnd')`. That only proves we wrote the right
bytes, which was never in doubt.

### Second hypothesis — the write key has never been verified (resolved: the write key is correct)

`'cmnd'` is not the only cause that produces total silence. `probe_datastream_keys` authenticates the
**read** direction only; the write key is *inferred* as the other half of the HKDF pair. Evidence
previously cited as confirming the write path (`iAP2PacketParseBuffer, dataLength: 6/26/9`) is now known
to be Bluetooth and `POST /command` traffic — **nothing we have ever written to the DataStream has been
observed to arrive.** A wrong write key and a wrong message type are indistinguishable from our side.

**Separating experiment (free, zero risk, one extra frame):** send one frame with a deliberately invalid
transport kind at `0x04` (e.g. `'junk'`). Apple's `controlServer_receiveData` rejects an unknown kind
with **`-6717 kFormatErr`** — but it can only do so *after decrypting*. So:

- phone logs `-6717` (or any reaction at all) ⇒ **the write path and crypto are correct**; if the link
  still stalls, `'cmnd'` or something above it is the remaining fault.
- total silence ⇒ **the frame never decrypted**; the write key/direction is the fault, not the type.

Run this only if the primary discriminator says the ACK still is not landing. It costs one frame and
converts an ambiguous negative into a decisive one.

### Next hypothesis if the primary fix is insufficient — `'sync'` at offset 0x04 (not needed; `'asyn'` is accepted)

If the ACK still does not land after the `'cmnd'` fix, **try `'sync'` before anything else.** It is a
two-line change and it is what Apple's own accessory reference does.

`CarPlaySDK::_AirPlayReceiverSessionSendiAPMessage` @ `0x272a9c` checks
`AirPlayAccessoryEnabledFeatureIAP`, and when enabled routes to the **DataStream** (independently
confirming docs/carplay/05_METADATA_AND_CONTROLS.md's whole thesis) with `w1 = 1` — "synchronous". That threads through
`_AirPlayReceiverSessionDataStreamSend` → `SendInternal` → `APMediaDataControlServerSendRequestSync` →
`'sync'` + an 8-byte random messageID + a **10 s** wait (`0x2540be400` ns). For contrast,
`VehicleDataProtocol1Send` passes `w2 = 0` → `'asyn'`. Both kinds are first-class; **Apple picked
`'sync'` for iAP specifically.**

`'asyn'` is judged legal and is the right choice for the first run: `startMessageHandling` never
inspects the kind at all (its only identity branch is the `'died'`/`'cmnd'` filter), it explicitly
supports the reply-less case (`cbz x21` guards the out-reply store), and the phone sends *us* `'asyn'`
on this very channel. Shipping it also avoids a 10 s blocking round-trip per frame.

**Honest gap:** the phone-side code that parses our `0x04` lives in AirPlaySupport, which extracted
without a usable symbol table, so the "`'asyn'` is accepted" conclusion is inference from the layers
above and below — strong, but not the same evidence class as the `'cmnd'` filter itself.

**Note for branch B:** switching to `'sync'` would NOT have exposed the `'comm'` bug. Both the accept and
reject paths leave the out-reply null with OSStatus 0, so a `'rply'` would have come back empty either
way. There was no way to see this from the wire — only from disassembly.

### Carrier discriminator (which carrier a frame actually took)

`carplayd`'s `Event message received from 192.168.43.1:PORT, … Body N bytes, ID 0xNNNN` is the phone
receiving an accessory `POST /command`. The plist overhead is constant, so the body size names the frame:

| iAP2 frame | plist body |
|---|---|
| 6-byte DETECT | **94 B** |
| 26-byte SYN | **116 B** |
| 9-byte ACK | **97 B** |

Use this to prove which carrier carried what. In sess3 the four bodies were `94, 116, 94, 116` and **no
97-byte body existed** — both DETECT+SYN pairs went over `POST /command`, and only the ACK took the
DataStream.

**Corollary, contra docs/carplay/05_METADATA_AND_CONTROLS.md's framing:** `POST /command` is a *working outbound carrier on iOS 27*, not a
dead 2017 mechanism — it delivered every DETECT and SYN in both archived sessions. Only the **inbound**
direction requires the RCS channel.

### Pre-flight gates (do these before starting a session)

1. **Binary identity.** `md5sum /usr/sbin/carplayd` must equal the packed artefact you built. A run
   against a stale binary is worse than no run.
2. **Runtime build identity.** ~~`grep -c "TX(RCS," $L` ≥ 3~~ — **DEAD, corrected 2026-08-16**: that
   line is gated behind `events_log()` and `CARPLAY_EVENTS_LOG` is never set, so it fails on a CORRECT
   binary. Use `grep -c "outbound sink registered" $L` ≥ 1 (ungated, `datastream.rs`) instead;
   the old one logged `TX iAP2`. Non-zero `grep -c "datastream. TX iAP2 "` ⇒ wrong build.
3. **`is_iap` must be true.** If `clientTypeUUID` is absent from the SETUP request the sink is never
   registered, every send falls back to `POST /command`, and the fix is inert while looking like a
   failure. Confirm the `DataStream(130)` line ends `[iAP channel]`.
4. **Phone trace liveness — without spending a session.** Plug any MFi wired accessory (EarPods,
   USB-C→3.5 mm adapter, car cable) in for ~10 s while running
   `idevicesyslog -u <UDID> -p accessoryd --no-colors -o pre.txt`, then `grep -c "LOG;" pre.txt`.
   `> 0` ⇒ `PrintIapPackets` is live, proceed.
   **Do not trust `capture_iphone_carplay.v2.sh`'s PRECHECK** — accessoryd is silent when idle, so it
   warns unconditionally. Measured: 55,001 system-wide `<Debug>` lines with zero from accessoryd.

   **CORRECTED 2026-09-05 — do NOT chase logging profiles.** This gate used to end "`0` with
   accessoryd lines present ⇒ the pref is off; reinstall the CarPlay/iapd profile, **reboot**,
   retest." That instruction cost a session's worth of hope tonight and is at best unverified on the
   current test phone. What was actually measured (separate session, same day, iPhone18,4 / iOS 27.0
   24A5430a):
   - **iOS 27 rejects any profile carrying a `com.apple.system.logging` payload unless Apple-signed**
     — "invalid signature", even with no `Enable-Private-Data` key. The payload TYPE is gated, not the
     private-data key; self-signing does not help.
   - **Capture with ZERO profiles.** `carkitd` and `accessoryd` already emit DEBUG by default. A 76 s
     no-profile capture that caught a connect produced 202 unique `carkitd` message shapes; a
     22-minute FULLY-PROFILED capture with no connect produced 159. **The variable is whether a
     session connect happens, not whether a profile is installed.**
   - Profiles meaningfully add only `bluetoothd`/`wifid` volume and `com.apple.bluetooth`
     unredaction — so the **Bluetooth profile is the one worth keeping**. No Apple profile unredacts
     any `com.apple.car*` subsystem, and that is deliberate, not an oversight (AccountsAuthKit grants
     private data to 12 subsystems at once).
   - **OPEN QUESTION, worth one check:** whether the *network* profile surfaces CarPlay Wi-Fi
     information. Untested. Record the result here when someone tries it.
   - For a redacted `%@`, recover the schema **statically from the `carkitd` binary** instead of
     chasing unredaction (mount the IPSW root fs, read strings). The `mask.hash` set — 52 decorators,
     the BLE pairing path — renders as `<mask.hash: 'base64'>` no matter what and is not recoverable.

   Note the `LOG;` / `PrintIapPackets` trace is a *preference* historically set by Apple's own CarPlay
   debug profile, which is NOT the same mechanism as the hand-rolled logging payloads above — so the
   measurements do not strictly refute this gate, they refute the instinct to reach for a profile.
   Treat a zero `LOG;` count as "this trace is unavailable on this phone", not as "install something".

   **The test phone runs DEVELOPER BETA iOS builds, not public production releases.** Weigh every
   phone-side observation accordingly: a behaviour seen here may be beta-only, may change before
   release, and may differ from what a customer's device does. When a phone-side result matters,
   record the exact build (e.g. iOS 27.0 24A5430a) alongside it — a bare "iOS 27" is not enough to
   reproduce or to trust later.
5. **Arm the log-truncation guard.** `session_supervisor.sh::bound_logs` tail-truncates
   `/tmp/carplayd_wl.log` to 64 KB once it passes 256 KB — destroying exactly the handshake window.
   Snapshot at ~T+30 s: `cp /tmp/carplayd_wl.log /tmp/hs30.log`.
   **`/tmp` is tmpfs — pull it off the box before anything reboots.**

### The trace persists — pull it retroactively

**Why this procedure exists — wired CarPlay owns the phone's USB port (owner-stated 2026-09-05).** In a
wired session the iPhone's only port is plugged into the BOX, and `idevicesyslog` needs the phone
attached to the Mac, so there is NO live phone-side capture during a wired session. Two ways to get
iOS's own view of a wired session:

- **(a) retroactive-wired** — run the session wired, unplug the phone from the box, plug it into the
  Mac, and pull the archive (below). The `LOG;` trace lives in the on-device log store; 226 lines were
  recovered from a two-hour-old window, so nothing has to be started before the session.
- **(b) live-wireless** — test on WIRELESS CarPlay, where projection rides Wi-Fi and the phone's USB
  port is free for a live `idevicesyslog -u <UDID> -p accessoryd -o <file>`.

**What this gates:** every check that needs an iOS-side observation — the `iapreject` /
`Identification info rejected` reason after an `RX 0x1D03` (decision tree below, docs/carplay/05_METADATA_AND_CONTROLS.md §6.5),
Identify acceptance, the `AirPlay  Acc  0x40  ACK` row — is **retroactive-wired or live-wireless, never
live-wired**. A wired-arm Identify-shape change (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3 says "`idevicesyslog -p accessoryd`
on every Identify-shape change") therefore means: run, unplug, re-plug on the Mac, pull. Pre-flight gate 4
(trace liveness) is unaffected — it is done with the phone on the Mac before the session.

Practical caveats: the archive is ~290 MB; `--age-limit` is seconds and must comfortably span the
whole session PLUS the time spent moving the phone (1800 covers a short session, raise it for a long
one — the two-hour recovery above used a wider window); `/usr/bin/log` must be spelled out (see below).

```sh
idevicesyslog -u "$UDID" archive post.tar --age-limit 1800 && tar -xf post.tar -C post.logarchive
/usr/bin/log show --archive post.logarchive --style compact --info --debug \
  --start '<T0 minus 30s>' --predicate 'process == "accessoryd"' > accd.txt
tools/extract_iap2_trace.sh accd.txt
rm -rf post.tar   # ~290 MB
```

The row to look for: `AirPlay  Acc  0x40  ACK` — the phone confirming, in its own words, that our ACK
arrived. Cross-check the summary: `AirPlay iPod SYN-ACK` must be **1**, not 31.

Note `/usr/bin/log` explicitly — a shell function shadows `log` in this environment and swallows args.

### Wired view-area resize matrix (2026-09-07) — retired with the macOS host app

The wired view-area matrix was judged Mac-side, by `tools/va_wired_sweep.sh` and `tools/winshot.sh`
against the macOS host app's control socket. All three left the tree in OCBM 1.0.5; the procedure and
its bench facts are at tag [`v1.0.4`](https://github.com/enigy/ocbm/blob/v1.0.4/docs/ops/02_TESTING.md).
The results stand in `04_OPEN_ITEMS.md` §HANDOFF 2026-09-05 → 09-07. Known-open 7 there (the lockout
thresholds) applies to any re-run with Carlink.

### Negative checks — all must be zero

`datastream. TX iAP2 ` (both = old binary) · `key probe EXHAUSTED` ·
`RX 0x1D03` · `[datastream] decrypt FAILED` · `envelope did NOT parse` (a reassembly failure; `drain_rcs` should make this
impossible. Inbound framing is **16384**, not 1024 — `MAX_WRITE = 1024` is our outbound chunking, while
the phone's DataStream path uses `NetSocketChaCha20Poly1305Configure` at 16 KB and `MAX_READ` matches
it) · `[art] INCOMPLETE` · `TX failed after retries` ·
`refusing to install gen` · `RX 0x1D03` · `tunnel session aborted`

New strings worth reading every session:
`[features] metadata policy (resolved once): param6=… param7=… subscribe=… skip=…` — the ONLY way to
confirm which tier a running process actually pinned; it is resolved once per process, so editing
`/tmp/carplay_metadata` mid-run does nothing · `[features] dropping X — it rides Y, which is not
active` (a rider was orphaned by a skip and correctly dropped) · `[art] OVERLONG id=… > declared …`
(duplicated artwork fragment; see docs/ops/05_AUDITS.md §4) · `[meta] seam write failed — reconnecting on next
message` (was the sink-eviction bug, fixed 2026-07-29 — should no longer appear).

> Note (2026-08-10): the `/tmp/carplay_metadata` / `CARPLAY_METADATA` levers this plan drives are
> interim, box-side controls — per docs/carplay/04_CAPABILITIES_AND_CONFIG.md tier ownership is app-side and the levers migrate to
> app-pushed config. Every operational instruction in this plan (the tier-confirmation log line, the
> `echo proven` recovery, the post-reboot re-arm) remains exactly how the CURRENT build is driven;
> follow them unchanged until the migration lands.

Informational, record but not failures: `SETUP phase2 stream type N NOT IMPLEMENTED` (write the list
down — each is a candidate for the next stream-130-class discovery), `stale connection (gen N) closed`,
`resent SYN only`.

### Decision tree

- **`RX 0xAA00 -> CertSent` present** → `'cmnd'` was the blocker; docs/carplay/05_METADATA_AND_CONTROLS.md §6.1 closed. Follow the log
  forward; whatever stops first is the new frontier.
- **Still many `ctrl=0xc0`, no `0x40` inbound** → *(both counts are now permanently 0 regardless of
  health — see the 2026-08-16 correction above; judge by `outbound sink registered` instead.)* refuted.
  First check `grep -c "outbound sink registered"` ≥ 1: if the
  ACK was never written, the bug is upstream (no sink), not on the wire. Surviving hypotheses: the
  `0x08`/`0x1c` fields are not reserved; `'asyn'` is wrong for accessory→phone (try `'sync'` + `'rply'`);
  our seq/ack values. **This branch requires the phone trace** — box logs cannot separate them.
- **`0xAA00` then `0x1D03`** → transport, link and MFi auth all correct; only the Identify payload is
  wrong. A good outcome. Decode the reject body; do **not** reflexively re-add 0x4157/0x4170.
- **Identified but no metadata** → check whether every subscribe logged `→ sent` (8 under `extended`, 3 under `proven`). Have Apple Music
  **playing before** the session so an idle phone cannot cause this.
- **`RX 0x1D03`** → do NOT bisect. Capture the phone's reason:
  `idevicesyslog -u <udid> -p accessoryd -o /tmp/rej.txt` (live — WIRELESS sessions only; on a WIRED
  session the phone's port is on the box, so pull the archive afterwards per "The trace persists"
  above and grep `accd.txt` instead), then
  `grep -E "iapreject|Identification info rejected" /tmp/rej.txt`. It names the param, the message id
  and a reason from Apple's enum (docs/carplay/05_METADATA_AND_CONTROLS.md §6.5). Recovery is `echo proven > /tmp/carplay_metadata`.
- **Identified but only NowPlaying/RouteGuidance** → check `/tmp/carplay_metadata` says `extended`;
  `/tmp` is tmpfs and a reboot reverts to the baseline. Confirm the subscribe count line
  `[events] iAP2-tunnel metadata: N subscribes`.
- **Metadata but no artwork** → confirm session 2 is routed (`RX session-2 file-transfer fragment`) and
  that each transfer reaches `[art] complete id=0x… N B (expected N)`. `[art] INCOMPLETE` means
  fragments were lost; `envelope did NOT parse` means reassembly failed.
- **No stream-130 SETUP at all** → the phone never opened an iAP channel; look at `/info` and the
  `enabledFeatures` echo, not at anything this round changed.
  **⚠️ Before concluding "at all", check whether the SETUP is ARRIVING and being REFUSED** — this row
  sent the 2026-08-10 diagnosis in the wrong direction. The phone was asking 33 times a session and the
  box was rejecting it, which is a different failure from the phone never asking.

  > **⚠️ THE GREP THIS ROW USED TO PRESCRIBE, `grep -c "SETUP stream type=130"`, CAN NEVER MATCH** —
  > 130 is exempt from the guard that emitted it, so the count is permanently 0, which this row then
  > reads as "the phone never asked". Use the two greps below instead. Full reasoning:
  > [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-46-2`.

      grep -c "SETUP phase2 DataStream(130)" /tmp/carplayd_wl.log   # arrived AND reached its handler
      grep -nE "SETUP stream type=|NOT IMPLEMENTED|skipping" /tmp/carplayd_wl.log   # any refusal, whatever guard adds one next

  Non-zero on the first = the phone asked and we handled it. Zero on the first is **not** by itself proof
  the phone stayed silent — a guard can drop the SETUP with no log at all, which is how the 2026-08-10
  outage hid. Settle it from the request itself: `touch /tmp/setup_dump` (wireless arm, `av.rs`; the wired
  supervisor arms the same `CARPLAY_SETUP_DUMP` via `/tmp/logtransfer_test` or `/tmp/mainbuffered_test`),
  reconnect, and read `/tmp/setup_req.N` — each raw SETUP request plist is written before any guard runs.
  A `type 130` entry there with no `DataStream(130)` line in the log = **the box is refusing a stream the
  phone is asking for**, a box-side bug that nothing in `/info` will explain. Nothing in `/tmp/setup_req.*`
  and nothing in the log = the phone really never asked (then check `/info` + `enabledFeatures`).
- **Stream-130 SETUP arrives but is SKIPPED** → a SETUP-level guard is rejecting it before its handler
  runs. It legitimately carries no `streamConnectionID` (docs/carplay/05_METADATA_AND_CONTROLS.md §1.3), so any guard rejecting a zero
  or absent scid must exempt it. Symptom set: tunnel pinned at `state=Init`, zero `0x5001`, **A/V
  totally healthy**, and zero `iAPSendMessage` 400s — there are no sends to reject, so absence of
  errors here is not evidence of health. Regression-tested at
  `crates/vendor/receiver/tests/setup_stream_130.rs`; history in docs/carplay/05_METADATA_AND_CONTROLS.md §8.

### Session ordering

Do not disable Bluetooth — the WiFi handoff is delivered over the BT iAP2 link, so there is no session
without it. It is also unnecessary: the archive gives BT-Classic control rows **inside the same trace**
as the AirPlay rows — same phone, same daemon instance, same second, one variable.

Change nothing else this session. Eight fixes landed together, but seven are structural prerequisites
for reading the eighth cleanly; with the startup race live a negative result would be uninterpretable.

Near the end, trigger `modesChanged` (change audio app / start-stop navigation) to exercise the closed
re-DETECT routes for free. Expect `link already up (SYN-ACK received) — not re-DETECTing` or
`resent SYN only`. There is no `resent DETECT+SYN` string in the code — the re-DETECT routes were closed, so the
observable is the ABSENCE of a second `TX detect+SYN` line, not a dedicated warning.

### Bench hooks (13 Nov 2026)

For the Mac's unattended bench. Each is a file in `/tmp` that nothing in the product writes, so a reboot clears
it, and each logs a `[bench]` line when it acts: btd's in `/tmp/wl.log`, carplayd's in `/tmp/carplayd_wl.log`,
`ocbm_update.sh`'s in `/tmp/box.log`. Carlink's side (flash by update, display off/on, the box-log tail, these files)
is in its debug builds only.

Carlink's `display_off`, `display_on` and `flicker` call its display handlers as the display watcher does, but the
display stays on: `PowerManager.isInteractive()` doesn't change and the activity's onStop/onStart (the video pause and
resume) don't run. They test Carlink's session lifecycle. Time to video, and the rule that no reconnect is scheduled
while the display is off, need a real display power cycle (the Mac's 14 Nov review, D1).

**On and off.** A flag is set when its file exists and its content, trimmed, is not `off`: an empty file and `on`
are set (`box_common::bench_flag`; `bench_flag` in `ocbm_update.sh`). Carlink writes the files over CH_FILE, which
can write but not delete, so it clears one by writing `off`. From a debug build:

    am broadcast -p com.enigy.carlink3 -a com.enigy.carlink3.BENCH --es do box_flag --es name <bench_name> \
        [--es value <text>] [--ez on false]

- `/tmp/bench_phantom_first`: btd adds `02:00:00:00:00:01`, a locally administered address no phone has, ahead
  of the bonded phones in every policy-order round (not for a switch hold, nor a Connect from the device screen,
  named or not). Its page times out like an absent first-listed phone's, so "first-listed absent" needs no
  airplane mode; and like one, a round that found it absent pages it last next round.
  It is never bonded, listed or counted toward a forgotten pairing.
- `/tmp/bench_order`: one line, `MAC,MAC[,...] [after_ms]` (the MACs as `AA:BB:CC:DD:EE:FF`, `after_ms` 0 when left
  out). The next policy-order round reads and removes it and, `after_ms` into its walk (at the next page
  boundary), pages the rest of the round in that order, through the re-plan path: the mid-round re-plan on cue.
  Bonded phones it leaves out follow it; addresses that aren't bonded are ignored. It holds for that round only,
  in memory: nothing is saved to `projection_policy.json`, Carlink's SET_POLICY neither undoes it nor is undone by
  it, and the next round is the app's order again. A malformed file (or one that lists a phone twice) is refused
  with a `[bench]` line and removed; `off` is no order. An order dropped unused (the round ended or a phone
  connected first, nothing was left to page, a session, a Connect or a switch hold came first) says why in a
  `[bench]` line.
- `/tmp/bench_skip_first_requestui`: carplayd skips the first screen nudge's requestUI once (the file is removed
  when used) and says so, so the 3 s second nudge fires.
- `/tmp/bench_sdp_after_acl_ms` (whole ms): how long btd gives the SDP channel after a page is answered (the link
  BT_CONNECTED) before it cuts the connect; 700 without it, or with `off`. Read at each page; a window not the
  default logs a `[bench]` line. The env lever `BT_SDP_AFTER_ACL_MS` wins over it.
- `/tmp/bench_arm_off`: the supervisor arms no Bluetooth between host sessions (2.6.0's powered-down state), from the
  next host's departure on; `off` (or no file) arms again, from the departure after that. An armed btd already
  running stays until then. The owner's switch for the same, on flash, is `/script/radio_arm_off`. With either, the
  next connects take today's path, so A against B on one bench is: a few connects, the file, a few more.
- `/tmp/bench_together_off` (20 Nov 2026): `radio_hal.sh` attaches Bluetooth in today's Wi-Fi-first order on a unit
  whose own attach branch would take the firmware beside the Wi-Fi driver (the IW416); `off` (or no file) takes it
  again. Read at each attach, so it reaches a host's `bt_on` that attaches (the preload off or failed), never a boot's
  own preload: a power cut clears `/tmp`. For cold boots use the owner's switch on flash, `/script/radio_together_off`
  (docs/wireless/01_BT_AND_RADIO.md, "The cold boot").
- `/tmp/bench_handover_on` (21 Nov 2026, R3-5): asks for the cold boot's handover (docs/wireless/01_BT_AND_RADIO.md,
  "The cold boot", 6), which is off by default: a boot preload hands Bluetooth to a host waiting on it, and the
  supervisor starts that host's btd before Bluetooth is up. `off` (or no file) is the default again. Read by the preload
  at its attach and by the supervisor at each host's arrival, so like the order's file it never reaches a power cut's
  own preload: for cold boots use the owner's `/script/radio_handover_on`. It replaces 4a6ce9c's
  `/tmp/bench_handover_off` and `/script/radio_handover_off`, which nothing reads now.
- `/tmp/bench_dial_early`: carplayd dials the phone btd names at its first resolve, before btd has sent that phone
  its 0x5703 (by default it waits for the name file's ` wifi` mark, A2), and says so per dial with a `[bench]
  dial_early` line. For one experiment: does the phone accept a dial before its Wi-Fi handoff? Still never another
  phone, nor before a phone is named. Read at each dial decision. Its answer is in the timeline (`dial` before
  `dial_ok`) and in `/tmp/carplayd_wl.log`: a connect-out the phone refused is tried again (carplayd-1), one it
  accepted with no control connection after it is not, and leaves the session to btd's 90 s backstop.
- `/tmp/bench_update_any_version`: `ocbm_update.sh` places a package whose VERSION is the same as or older than
  `installed_version` (bench builds keep the last published version with new code), so the bench flashes by update
  instead of the stick. Every other check stands: the sums, product, chip, mode, FROM_MIN, room and the journal; a
  package one of them refuses logs no `[bench] placing` line.

The box `[startup]` line ends with counts: `; since the host came: absent pages N, resets N (N cut, no SDP channel), PSK
mismatches N; this session: nudges first+second`. btd's counts and the PSK count run from the host's arrival, over every
phone session under it (btd carries them into each session it starts under the same host: a phone switch, a session
that never reached CarPlay, a wired preempt), so a second session's line counts the first's too; the nudges are that
session's carplayd's. The phantom's pages count as absent. When ocbmd rotated `box.log` (it empties it at its cap) between
the host's arrival and the first frame, the PSK count is a lower bound: `PSK mismatches >=N (box.log cut)`.

### Round 4 switches (22 Nov 2026; six on by default from 2.7.1)

Round 4 (branch `round4`, from 2.7.0) built these for the bench. The owner's decisions of 22 Nov 2026 made six of them
2.7.1's default (`first_page_slots` at 8192, `same_phone_repage`, `rfcomm_limit_ms` at 1500, `btd_early`, `page_trim`,
`attach_poll`) and left five off (`first_retry_ms`, `early_gap_secs`, `screen_nudge_ms`, `dial_early`,
`preload_nice`). Two changes are no switch: the Bluetooth firmware push bounded, pushed once more when it fails, no
attach without firmware, and a push after each reclaim+reset (docs/wireless/01_BT_AND_RADIO.md, "No attach without
firmware"); and the first screen nudge at once, where 2.7.0 waited 500 ms (`screen_nudge_ms`, below).

**The files, and the rule.** A switch is a file: `/tmp/bench_<name>` (a reboot clears it; Carlink's debug builds can
write it) or, kept across a power cut so a cold boot sees it, `/etc/ocbm/bench_<name>` on flash (OCBM's own state
directory, which updates keep and the full way back to stock removes whole). With neither, the default. `/tmp` is read
first, and the two kinds read it differently:

- **On by default** (the six): the first of the two files that exists decides. `off` gives 2.7.0's behaviour; any
  other text, an empty file or `on` among them, is on, and for a number sets its value. So an `off` in `/tmp` overrides
  the flash for a run (Carlink's `box_flag ... --ez on false` writes it), and a file left on flash from the round 4
  bench with its old content, empty or `on`, still means on. Read by `box_common::bench_override` (btd),
  `r4_on` (radio_hal.sh) and `btd_early_on` (the supervisor).
- **Off by default** (the five): set by the first file that is set by `bench_flag`'s rule (there, and not `off`), so
  `off` reads as no file and an `off` in `/tmp` falls through to the flash's (`box_common::bench_switch`; carplayd's
  `dial_early` and ocbm_boot.sh's `preload_nice` read the same way; `screen_nudge_ms` is `/tmp` only).

2.7.0's behaviour on all of them at once: `off` in each of the six files (for a cold boot in `/etc/ocbm`), none of the
five, and `/tmp/bench_screen_nudge_ms` at `500`; the firmware push's changes have no switch. A number that is not one,
or out of its range, is refused with a line and the default stands. A file that sets a switch or turns one off says so
in a line holding `[bench] round4:` (btd's in `/tmp/wl.log`, carplayd's in `/tmp/carplayd_wl.log`, the scripts' in
`/tmp/box.log`): btd for each file as its loop starts, `off` too; carplayd and the scripts where the switch acts. A
switch off by default says so again where it acts; the six on by default act with plain lines (below), which the
scripts tag `[bench] round4: <name> (<file>):` when a file turned one on, and their `off` says so where the switch
would have acted (`attach_poll=off`, `page_trim=off`, `btd_early=off`). For a cold boot put `btd_early`,
`page_trim`, `attach_poll` and `preload_nice` in `/etc/ocbm`: they act during the boot, while `/tmp` is still empty.
Item 7's `gpio_first` (the stock GPIO set-up before ocbm_boot.sh) was benched 0.25 s slower at the start and dropped;
its `preload_nice` stays.

| Name | Value | Default (2.7.1); `off`, 2.7.0's | Read | Does |
|---|---|---|---|---|
| `first_page_slots` | 1-12800 slots (8 s), or `off` | 8192 (5.12 s); `off`: the round's (6000 over several phones in the first round, 0x2000 otherwise) | btd, each session | the first page of the first round this long; every other page the round's. `6000` gives a 3.75 s first page, which is 2.7.0's only over several phones. No more than 8 s: the SDP connect that carries the page has its own 8 s deadline, which would end a longer page there, as a failure rather than a page timeout |
| `same_phone_repage` | flag, or `off` | on; `off`: the next phone | btd, each session | a re-page after a reset or cut that fails with EALREADY, EMLINK or EBUSY waits for the link (up to 1.5 s, then 0.3 s with none) and pages the same phone again, twice at most, then the next phone |
| `rfcomm_limit_ms` | 1000-8000 ms, or `off` | 1500; `off` (or `8000`): no limit, the plain 8000 ms, then the next phone | btd, each session | the RFCOMM connect cut at this unless a pairing prompt holds it; its link dropped as a cut's, the phone paged once more (counted once a round, as a cut) |
| `btd_early` | flag, or `off` | on; `off`: bt_on, then btd | the supervisor, at the host's arrival | with the boot preload running and the handover not asked for: btd started at once, waiting for bt_on's end, then today's bring-up |
| `page_trim` | flag, or `off` | on; `off`: the name and SCO set, the mark cleared in the attach, the 0.2 s look | radio_hal.sh | with a host waiting on the preload: no SCO lines or name set on its attach, the together guard's mark cleared after its locks; and every lock wait looks every 0.05 s |
| `attach_poll` | flag, or `off` | on; `off`: the 0.5 s look | radio_hal.sh, each attach | hci0 looked at every 0.1 s as the attach converges, not 0.5 s |
| `first_retry_ms` | 0-60000 ms | off: 2000 | btd, each session | the pause after a failed first round, once |
| `early_gap_secs` | 1-60 s | off: 5 | btd, each session | the gap between rounds in the first 120 s |
| `screen_nudge_ms` | 0-2500 ms, `/tmp` only | 0 (2.7.0: 500) | carplayd, each nudge | the first screen nudge's wait. At 0 it goes at once, and nothing spares it; after a wait it is spared when the phone has set its screen up again by then (`500` benches 2.7.0). The 3 s second nudge as before |
| `dial_early` | flag | off | carplayd, each dial | the existing early dial, now also from flash |
| `preload_nice` | flag | off | ocbm_boot.sh, each start | the boot preload reniced to -5 before it starts |

**The first page at 5.12 s** is the owner's decision of 22 Nov 2026, because the phone took over 2 s to answer on 3 of
7 cold boots. It replaces the short first page of the 13 Nov rule; the rest of that rule stands: each phone in the
driver's order, the first round's other pages 3.75 s each when it pages several, later rounds the full 5.12 s. With one
bonded phone every page was 5.12 s already, so nothing changes there.

**The six defaults' lines**, plain: btd `paging 5120 ms for the first page of the first round` and `paging 3750 ms
again: the first page is over` (several phones), `the re-page crossed a link the controller holds; ...`, `RFCOMM
connect to channel N not connected within its 1500 ms limit: closed; ...` and `... answered and opened no RFCOMM
channel within the connect's limit -- paging it once more before the next phone`; radio_hal.sh `the bt lock taken
after about N ms of waiting, looked at every 0.05 s`, `a host is waiting - no SCO lines and no name set on hci0 (...)`
and `the together guard's mark cleared after the locks went`; the supervisor's bring-up line, `... the boot preload is
attaching Bluetooth: btd starts now and waits for it`.

The first screen nudge at once is the owner's decision of 22 Nov 2026, after a bench of 40 wakes: an iPhone 7 set its
screen stream up and tore it down 18-54 ms later with 0 frames on nearly every connect, then set it up again; with no
wait the first frame came about 0.1 s after that second screen SETUP, against about 1.9 s with 500 ms, in every wake. A
phone that never tears its screen down (an iPhone on iOS 27) never reaches the nudge. With no wait the "set its screen
up again within N ms — no nudge" check is not made: the phone's next SETUP comes after the TEARDOWN on the control
connection, so a look at once could only race it. carplayd's line for that nudge reads `the phone tore its screen
stream down (streams live: [...]) — asked for its UI again at once`; after a wait, `no screen stream N ms after the
phone tore it down ...` as before (`crates/vendor/receiver/src/session.rs`, `SCREEN_NUDGE_AFTER`).

Not built in round 4: the AP configured ahead of the cold path, `noscan`, and anything in the handover itself.

### The connect timeline (16 Nov 2026; M1)

Right after `[startup]`, the wireless carplayd prints one more line at the first frame, on the same stream
(`/tmp/carplayd_wl.log`, CH_LOG source `airplayd-wl`), so it reaches Carlink's drive log the same way. Every
critical-path event of the connect, stamped where it happens:

    [timeline] v1 sup_radios=406 bt_armed=0 bt_power=1246 btd_start=1276 bt_up=1286 page_start=1296 acl=1556 sdp=1716 rfcomm=1866 cert_req=1946 sign1_start=2066 sign1_end=2376 ident=2566 wifi_req=2666 wifi_rsp=2696 ap_start=1666 ap_up=1876 ap_enabled=2646 sta_assoc=3266 dhcp_ack=3566 carplayd_start=3166 svc_seen=3866 dial_ok=3906 dial=3916 pv_done=4366 auth_in=4416 auth_out=4766 record_in=4846 record=5066 screen_setup=5116 first_frame=5466 chip1_ms=298 cert1_ms=41 chip2_ms=301 cert2_ms=40 usb_edge_s=11

(the unit test's line, not a measurement). The grammar is the one Carlink stores, `^\[timeline\] v1( [a-z0-9_]+=(-|\d+))+$`;
the keys come in this order always (`crates/box-common/src/timeline.rs`, which owns them). Every event is whole ms
since the origin; `-` is a value not stamped, one before the origin, or every event when there is no origin.

**Benched twice** (the Mac, 16 Nov: the dev adapter on `e75a0ac`, 5 connects, then on `335ef1d`, 4 connects). The line
arrived in the drive log under `[box/airplayd-wl]` right after `[startup]`, one line of about 440 to 480 bytes;
`first_frame` agreed with `[startup]`'s within 6-13 ms, and every shell stamp fell in order with the daemons' (so
`/proc/uptime` and CLOCK_BOOTTIME share their base on this kernel); the chip took 1640-1651 ms from go to ready and
110-170 ms for the certificate, every time. `usb_edge_s` was `-` in all nine (no configure edge after the supervisor's
start). On `335ef1d`: `bt_up` came 30-40 ms after `btd_start`, `record_in` 0-3 ms before `record`; the AP watch ended
each session in 2.2 to 4.6 s with all three lines seen on the wakes (`ap_enabled` is `-` on a cold start, as written
below); `ap_enabled - ap_start` was 850-870 ms in all three wakes, `sta_assoc` 550-610 ms after `rfcomm`, `dhcp_ack`
90-220 ms after `sta_assoc`. Not seen on the device yet: a session ending while its watch still runs, and a session
with more than one control connection. `bt_armed` and the armed path (A1, A3), `dial_ok`, the certificate cache
for all three users (A4) and carplayd's start with btd's host session (A2) came after those benches (16 Nov) and are
not benched yet; A2 moves `carplayd_start` from just after `wifi_rsp` to just after `btd_start`.

| Key | The event | Stamped in |
|---|---|---|
| `sup_radios` | the supervisor starts the radios (`wifi_ap_on` and `bt_on`; from the armed state, the go and `wifi_ap_on`) | `tools/session_supervisor.sh`, `wireless_up`'s block, `wireless_go` |
| `bt_armed` | NOT a time: `1` when the connect started from the armed state (Bluetooth powered between host sessions, the host's arrival a go; docs/wireless/01_BT_AND_RADIO.md), else `0`. From btd's `t bt_armed`, stamped as its first session opens the scans after the go, carried with the radio stamps; a stamp from before the origin is another host's and reads `0` | `wireless/src/main.rs`, `run_active_session` |
| `bt_power` | `bt_on` has the controller up and answering (each of its success paths; not the boot preload). `-` from the armed state: no `bt_on` ran. On a cold boot whose preload handed the controller to the waiting host (R3-5, when asked for): `hci0`'s first answer, stamped by the preload, so `page_start - bt_power` is `hci0` answering to the first page | `radio_hal.sh`, `do_bt_on`; `bt_handover_claim` |
| `btd_start` | btd begins this session's record (from the armed state: after the go, as its session starts; a btd started before Bluetooth is up, R3-5: at the host's arrival, before `bt_power`) | `wireless/src/startup.rs`, `begin` |
| `bt_up` | btd's Bluetooth bring-up done: `[startup]`'s "Bluetooth up", on the timeline's clock (from the armed state: the scans opened after the go; from the boot preload's handover: the scans opened after its proof, no DOWN/UP) | `wireless/src/main.rs`, `run_active_session` |
| `page_start` | the session's first page (the SDP connect that pages) | `sdp_client.rs`, `connect_sdp` |
| `acl` | the phone answered the page, as the 25 ms look first saw it | `sdp_client.rs`, `wait_connected` |
| `sdp` | the phone's iAP2 RFCOMM channel known | `sdp_client.rs`, `query` |
| `rfcomm` | RFCOMM up: out (reconnect) or in (accept) | `reconnect.rs` `attempt`, `main.rs` accept loop |
| `cert_req` | RX 0xAA00 | `bt_driver.rs`, `process_one` |
| `sign1_start`, `sign1_end` | btd's signature goes to the chip, and is back (its retries inside) | `bt_driver.rs`, `execute` |
| `ident` | 0x1D02: Identified | `bt_driver.rs`, `process_one` |
| `wifi_req`, `wifi_rsp` | the first RX 0x5702; the 0x5703 written | `bt_driver.rs`, `process_one` |
| `ap_start` | hostapd launched | `radio_ap_up.sh` |
| `ap_up` | the AP's processes are up: the first `ap_serving` pass (hostapd and udhcpd running, the address on the interface), before the second that confirms it 1 s on. Not the radio: 200-230 ms after `ap_start` on the bench, before hostapd has the AP enabled | `radio_ap_up.sh` |
| `ap_enabled` | the radio's own word: hostapd's `AP-ENABLED` (about 1 s after `ap_start` in earlier logs). Its timing: see the AP watch's buffering note | the AP watch, `wireless/src/ap_watch.rs` |
| `sta_assoc` | a station associated and keyed: hostapd's `AP-STA-CONNECTED`, after the WPA2 handshake (a bare 802.11 association can come earlier and fail). Its timing: see the buffering note | the AP watch |
| `dhcp_ack` | udhcpd's `sending ACK to`, as it sends | the AP watch |
| `carplayd_start` | carplayd's `main`: since A2 as btd's host session begins (`av::session_starting`, in the background, once the box identity's primary source, the Wi-Fi driver's address, holds still on two looks 100 ms apart), or at the 0x5703 when that start did not happen | `ccpa/carplayd/src/main.rs` |
| `svc_seen` | the `_carplay-ctrl` of the phone btd names first resolved, dialled at once or held for its 0x5703 (or a record held from before it was named, as it is) | `discovery.rs` |
| `dial_ok` | discovery first learned that btd sent the named phone its 0x5703, once per naming: the name file's ` wifi` mark as the loop reads it (or the phone named with it, at a handoff or a put-back), or the gate first letting it dial a record of that phone, whichever comes first, so never after the dial it allowed. The signal the connection's dial went after; with `/tmp/bench_dial_early`, one that came after the dial; `-`: none had come | `discovery.rs` (`Loop::note_mark`), taken with `dial` as the connection begins |
| `dial` | the last connect-out written before the control connection that produced the picture (`-`: the phone came by itself) | `discovery.rs` `connect_out`, taken as that connection begins |
| `pv_done` | pair-verify OK, the control channel encrypted | `receiver/src/server.rs`, `feed` |
| `auth_in`, `auth_out` | `POST /auth-setup` received; answered | `server.rs`, `handle` |
| `record_in` | RECORD received | `server.rs`, `handle` |
| `record` | RECORD answered (its handler waits for the event channel, up to about 5 s) | `server.rs`, `handle` |
| `screen_setup` | the main screen's (type 110) SETUP | `session.rs`, `setup_phase2` |
| `first_frame` | the first frame to the host, as `[startup]` | `receiver/src/startup.rs`, `first_frame` |
| `chip1_ms`, `cert1_ms` | btd's signature (chip, go to ready) and certificate read (under the lock), ms; `cert1_ms=0` when the certificate came from the boot's cache (A4) | `wireless/src/mfi_local.rs` |
| `chip2_ms`, `cert2_ms` | auth-setup's, the same two, in the control connection that produced the picture; `cert2_ms=0` from the cache | carplayd's `LocalMfiSigner` |
| `usb_edge_s` | whole seconds from the host-facing USB link's last edge into "configured" to the origin | the supervisor's `usb_observe` |

**One clock.** Every stamp is the uptime on CLOCK_BOOTTIME, the clock `/proc/uptime` shows (the shell's stamps to its
10 ms, the daemons' to the ms), never the wall clock, which has no RTC behind it and jumps when Carlink sets it
mid-session. The origin is the moment ocbmd raises `/tmp/host_present` to 1, the one `[startup]` counts from: ocbmd
writes that uptime (ms) to `/tmp/host_present_uptime` with each raise, and btd copies it into each session's record.
`[startup]` keeps its wall-clock arithmetic; the timeline is printed whatever that made of it. A value wider than 13
digits (over 300 years of uptime) cannot come from this clock, and prints `-`.

**Where the stamps live.** btd's record, `/tmp/startup_marks`, one line each in a single write: `t <key> <uptime ms>`
for an event, `d <key> <ms>` for a duration. The first of a key stands: in btd's session for its own; in carplayd
(kept in memory: it prints the line), in the process for `carplayd_start` and `svc_seen` and in the control connection
for the rest. Each new control connection starts those afresh, auth-setup's chip times with them, so the line
describes the connection that produced the picture, not a failed one before it; its `dial` is the last connect-out
sent since the connection before it began, and its `dial_ok` the 0x5703 signal that dial went after (a name change
forgets the last phone's). The shell's radio stamps are appended before btd begins a session, and btd
(with its own `t bt_armed`, the bring-up's too) carries them from the bring-up's `t sup_radios` on into each session under that bring-up: a phone switch keeps them, a
radio restart brings new ones, an earlier host's are dropped. `ap_enabled` is carried with them (the AP's, not the
phone's); `sta_assoc` and `dhcp_ack` are each session's own. The supervisor's USB edge rides along as `t usb_edge`. A
shell stamp landing in the instant btd rewrites the record for a new session is lost (its value `-`; the AP's are the
ones that can, as `wifi_ap_on` runs beside `bt_on`). A wired carplayd prints no timeline (it is gated on the wireless
spawn's `CARPLAY_WIRELESS_METADATA`). On a unit with the IW416 baseline's own AP scripts (`backend=owned`) `ap_start`
and `ap_up` are `-`: its AP comes up inside them, and chipset scripts are not stamped.

**The AP watch** (`wireless/src/ap_watch.rs`). hostapd and udhcpd stamp nothing of their own, and their lines carry no
time, so btd watches the files they write, and stamps each line as it lands: `/tmp/box.log`, into which
`radio_ap_up.sh` runs hostapd in the foreground, and udhcpd's `/tmp/radio_ap_dhcp.log`. One thread per btd session,
from the session's start until it has seen `AP-STA-CONNECTED` and a DHCP ACK (the phone on the AP, whose AP was
enabled before that), 60 s, or the session's end. Event-driven: inotify on the two files, and the session's end
through a pipe; nothing polls, and between sessions nothing runs. It reads those two files only: hostapd's and
udhcpd's configuration and start-up are untouched. Each key is the session's first matching line, stamped as the watch
wakes. It ends with one line in `/tmp/wl.log`, e.g. `[ap-watch] ended (the phone is on the AP) after 4.1 s:
AP-ENABLED seen, AP-STA-CONNECTED seen (..XX:YY), DHCP ACK seen`: a station's last two octets are all of its address
that appears anywhere, and none of it reaches the timeline.

- **Which phone.** Neither line tells the phone btd serves from another on the AP: the phone joins under a private
  Wi-Fi address, not the one its 0x4E0E names (the 12 Nov bench's iPhone showed two different ones), and udhcpd names
  only an IP. With a second phone on the AP (one a switch dropped, joining again), the first line can be that phone's.
- **What it misses.** A line written before the session began. On a cold start the AP comes up beside a Bluetooth
  attach that can take 7 s (the bench above: `ap_start=4275`, `btd_start=7556`), so hostapd's `AP-ENABLED` is usually
  in `box.log` before btd starts, and `ap_enabled` is `-`. So is `sta_assoc` for a phone that joined the AP, knowing
  it, before btd began. From the armed state btd's session begins at the go, before hostapd is launched, so its
  `AP-ENABLED` is always seen; so it is from a btd started before Bluetooth is up (R3-5, a cold boot with the host
  waiting on the preload), whose session's record and AP watch begin at the host's arrival.
- **Buffering.** udhcpd (BusyBox) writes each line in one write as it sends, so `dhcp_ack` is the ACK's time to within
  the watch's wake. hostapd writes through stdio into a file, which may be block-buffered: the 12 Nov drive log shows
  its station lines arriving at their own times, seconds apart, so they look flushed per line, and the 16 Nov bench agrees (`ap_enabled - ap_start` within 20 ms over three wakes); it is not proven.
  If they are buffered, `ap_enabled` and `sta_assoc` are late by an unknown amount. On a bench, `ap_enabled - ap_start`
  steady at about 1 s says they are not. If they are, the other sources are a passive EAPOL socket or hostapd's
  `ctrl_interface` (a change to the AP itself; this section in git at `e75a0ac` has both, with their costs).

**`usb_edge_s`.** The supervisor reads `/sys/class/udc/ci_hdrc.1/state` (the host-facing UDC) once a tick, with no
fork, and once more as it brings the radios up, and notes the uptime of each edge into `configured`; `wireless_up`
stamps the last one. `-` when the state last read `suspended` (this kernel's UDC still reads `suspended` after a
resume, [02_SESSION_LIFECYCLE.md](../carplay/02_SESSION_LIFECYCLE.md#empirical-basis), so a resume cannot be timed),
when the edge came before the supervisor started, or on a unit without that file. Resolution is the 1 s tick: an
edge seen only after the origin counts 0. A large value is a link that stayed up: the host came back without the
link going down (or into `suspended`) since.

**The chip, in the logs.** Each chip call on the video path logs its time, the signature from "go" to status ready
(the fixed 100 ms wait before its first status read included) and under the lock, the certificate read under the
lock, both with the wait for the lock: btd's in `/tmp/wl.log` (`[mfi-local] cert: 945 B in 41 ms under the lock, 0 ms
waiting for it`, `[mfi-local] sign: chip 298 ms from go to ready, 305 ms under the lock, 0 ms waiting for it`),
auth-setup's as `[carplayd] mfi: cert ...` and `[carplayd] mfi: sign: ...`. The tunnel's signature comes after the
first frame, so it is a line of its own and no timeline value: `[mfi] tunnel sign 305 ms (chip 298 ms from go to
ready, 0 ms waiting for the lock), cert 41 ms (chip)`, or `(cached)` for the boot's cached certificate.

Since A4 (16 Nov) all three certificate users serve the boot's cached certificate (`mfi_i2c_local::cert_cache`,
`/tmp/carplay_mfi_cert.der`), and btd reads it into the cache as a host session begins when none is there yet, so the
chip is read once per boot: `[mfi-local] cert: 945 B in 112 ms under the lock, 0 ms waiting for it (the session's start:
the boot's first read)` in `/tmp/wl.log`, then `[mfi-local] cert: 945 B from the boot's cache, no chip read` at each
0xAA00 and `[carplayd] mfi: cert 945 B from the boot's cache, no chip read` at each auth-setup. A read that is not one
DER certificate ends `; not one DER certificate: sent, not cached`. The cache is dropped, so the next session reads
the chip, by btd when a Bluetooth session that sent the certificate ends without its handoff (`the session sent the
certificate and ended without a handoff: the cached certificate is forgotten`), by carplayd when the phone asks nothing
after auth-setup (`the phone asked nothing after auth-setup: the cached certificate is forgotten`), and by the tunnel
on its 0xAA04. Signatures still go to the chip every time.

**Length.** At most 827 bytes: no value is printed wider than 13 digits (above), `bt_armed` is one digit, and the
widest-line test builds the line with every value at that width; the one above is 503. ocbmd clips a forwarded line at 1024 bytes
(`LOG_MAX_LINE`), and Carlink's drive log clips nothing more: its CH_LOG parser takes each entry's length, and its drive
log writes the text through `LogRedactor`, which only shortens MAC addresses. So one line, never two. (With 20-digit
values, the u64's widest, the three keys added after the bench would have taken the line to 1029 bytes: the 13-digit
cap is what keeps the bound.) **Carlink's connect ledger is stricter** (corrected 20 Nov 2026): `AdapterTimeline.accept`
(Carlink 3.21 to 3.25, `app/src/main/kotlin/com/carlink/ledger/AdapterStartup.kt`, `MAX_CHARS = 800`) keeps the line
only when it is at most 800 characters, and drops a longer one whole. The widest line the box can print, 827, is over
that; but a value has 13 digits only from 10^12 ms (31.7 years) past the host's arrival, and with every value at 12
digits the widest is 792, so no line a box prints in practice is dropped (real ones are 440 to 500). A key added to the
line adds its name and up to 13 digits to the widest: keep the 12-digit widest under 800, or lower the cap. R3-5
(20 Nov) added no key: on its path `hci0` answering to the first page is `page_start - bt_power`.

### The boot's radio line (20 Nov 2026; R3-4)

One line per boot, beside the timeline but not in it, for the cold boot's radio path (docs/wireless/01_BT_AND_RADIO.md,
"The cold boot"):

    [radio-boot] v1 by=preload start=2050 caps=kept caps_ms=61 gpio=2330 order=together off=- wlan_start=2410 wlan_up=4460 wlan_free=4470 btfw_start=2420 btfw_end=4930 btfw=complete btfw_ms=2467 btfw_tries=1 attach=4940 hci_up=5390 handover=on_owner handed=5420 tries=1 fallback=0 rc=0 host=3010

(the unit test's line, not a measurement). `radio_hal.sh` writes it into `/tmp/box.log` (and `/tmp/radio_boot`) once
the first bring-up that attached Bluetooth this boot is over: the boot preload's, or a host's `bt_on` where no preload
attached it; one more only when that one failed and a later one attached. The box log reaches the drive log as the
radio script's `[up N.NN]` lines do, in the replay ocbmd streams when Carlink arms its log (`(replay)`), or live. Its
own tag, so nothing that parses `[startup]` or `[timeline]` changes. Grammar `^\[radio-boot\] v1( [a-z_]+=[A-Za-z0-9_-]+)+$`,
the keys always in this order, the first stamp of a key standing. Times are the uptime in whole ms, the clock the
timeline uses, but from power-up, not from the host's arrival; `-` is a value not stamped. No name, no address.

| Key | Value |
|---|---|
| `by` | the bring-up that wrote it: `preload` (the boot preload) or `bt_on` (a host's session) |
| `start` | the radio work began: the preload's `running`, or that `bt_on`'s attach |
| `caps`, `caps_ms` | where this boot's descriptor came from, `kept` (`/etc/ocbm/kept_radio_caps`) or `detected` (`radio_detect.sh` ran), and how long it took |
| `gpio` | the preload saw the stock GPIO set-up's end (`OCBM_GPIO_DONE`); `-`: none was asked for, or none came in 3 s |
| `order` | the first attach's order: `together` (the Bluetooth firmware beside the Wi-Fi driver), `wlan_first` or `bt_alone` (today's) |
| `off` | why a unit that may take `together` did not: `owner`, `guard`, `bench`, `fallback`; `-` otherwise |
| `wlan_start`, `wlan_up` | the first Wi-Fi module inserted this boot; its interface seen |
| `wlan_free` | `together` in the preload: it let the wlan lock go, and a host's AP bring-up waiting on it may start |
| `btfw_start`, `btfw_end` | the Bluetooth firmware push (`fw_loader_linux`) |
| `btfw`, `btfw_ms` | what the loader said, `complete`, `error` or `other`, and its own time (`time:N`), `-` without one |
| `btfw_tries` | the loader's runs in that bring-up (22 Nov 2026): `1`, and one more for each of: a push that failed and was pushed once more, the together fallback's own push (`fallback=1`), and each reclaim+reset. `btfw` and `btfw_ms` stay the first run's. Two failed pushes with no good one earlier this boot attach nothing (`attach=-`, `rc=1`: no `hciattach`, no reset after them; `radio_hal.sh` `bt_fw_push`); after a good one (`/tmp/.radio_btfw_pushed`) a failed push is not run again and the attach goes on (docs/wireless/01_BT_AND_RADIO.md, "No attach without firmware"). `-` on a part with no loader |
| `attach`, `hci_up` | the first attach launched; `hci0` answered a name read |
| `handover` | whether the handover (R3-5) was asked for as the attach started: `on_owner` (`/script/radio_handover_on`), `on_bench` (`/tmp/bench_handover_on`), or `off`, the default |
| `handed` | the preload handed the controller to the waiting host's session (R3-5, docs/wireless/01_BT_AND_RADIO.md "The cold boot" 6): claimed as `hci0` answered, handed over once the attach's Wi-Fi side was in; `-`: the handover not asked for (the default), no host as it answered, or a `bt_on`'s line |
| `tries` | attach attempts |
| `fallback` | `1`: a `together` attach gave no responsive `hci0`, and the bring-up went on in today's order |
| `rc` | the bring-up's exit code (`0` up, `1` failed) |
| `host` | the host's arrival, ocbmd's `/tmp/host_present_uptime`; `-` before one |

**A cold boot or a restart.** A power cut downloads both firmwares: `btfw_end - btfw_start` about 2.2 to 2.7 s with
`btfw=complete`, `wlan_up - wlan_start` about 2 s (the 20 Nov bench). A `reboot` was taken to keep the chip's firmware,
so that both would come out far shorter; but the three logged (13 Nov, `[bench] reboot under a running host`) each
pushed in full, 2.75 to 2.83 s from the push to the attach: a SoC restart resets the chip too. What leaves the firmware
running is less than that: an attach helper that died, or reclaim+reset's gpio1 pulse (20 Nov, by hand; that run's
"the loader returned at once with no output" was a result file left by the run before it). The session's Bluetooth is
about `max(hci_up, host)` plus the host's `bt_on` confirming it (a lock look and one name read); with the handover
asked for (R3-5), `handed`, and the session's first page about 0.1 s after it (the timeline's
`page_start - bt_power`).

### The armed path (16 Nov 2026; benched the same day)

Bluetooth powered but invisible between host sessions, the host's arrival a go (docs/wireless/01_BT_AND_RADIO.md,
"Between host sessions: armed").

**The first bench** (16 Nov, the dev adapter, `resident-bt` merged with `carplayd-early` as a local `round2`): 12 wakes
and a cold boot (a restart by `reboot`, which keeps the radio chip's firmware: no cold boot for radio timing; "Cold
boot: real power cycles", below) from the armed state, `sup_radios` about 50 ms, `bt_armed=1`, `page_start` about 100 ms, `ap_enabled`
about 1.16 s after the host's arrival; the no-host state as in step 1 below; arming about 1.0 s faster than
`/tmp/bench_arm_off`; the forced fallback (step 4) as written. One bad wake, the first about 90 s after an update was
committed: 24.8 s, of two delays (the Mac's second reading, 17 Nov). A late join, its cause open: `AP-ENABLED` 4.5 s
before the 0x5703, yet the phone authenticated about 7 s after it (`AP-STA-CONNECTED` 8.6 s after RFCOMM, 0.4 s
otherwise). Then about 6.7 s with no DHCP server: no `udhcpd: started` and no `sending ACK`, the phone on its old IPv4
address, its service resolved 6.5 s after the join. Neither repeated after a reboot. Only the second has a likely cause,
fixed since (the last AP's DHCP server, slow over its lease file on flash the commit's garbage collection slowed, taken
for the new AP's; docs/wireless/01_BT_AND_RADIO.md, "The AP, written ahead"). If it recurs, right after it:
`/tmp/wlan.log`, `/tmp/radio_ap_dhcp.log`, `/tmp/box.log` (hostapd's lines for the phone's join, and the time of
`[dispatch] update committed`), `/tmp/startup_marks` and `df /`, and the phone's log archive for the join
(docs/wireless/01_BT_AND_RADIO.md, "Open: one bad wake"); then a wake soon after an update's commit is a case of its own
on the next bench.

A smoke bench (about 30 min), on the dev adapter, wakes being the path it is for:

1. **No host, after boot.** Once the preload is done: box.log `[sup] no host: arming Bluetooth for the next one`; wl.log
   `[armed] armed as "…": Bluetooth powered, scan 0x00 (page and inquiry off), 0 connection(s)`; `/tmp/.btd_armed`
   holds btd's pid; `hciconfig hci0` is `UP RUNNING` with neither `PSCAN` nor `ISCAN`; `hcitool con` lists nothing;
   the AP interface holds no `192.168.43.1`, no hostapd runs, and `/tmp/.radio_ap_prepared` names it. A phone that
   scans does not find the box; a bonded phone shows it "Not Connected" and nothing connects. No `[reconnect]`,
   `[sdp…]` or `[ssp-agent]` line in wl.log before a go.
2. **Five wakes** (display off for 10 s or more, then on). Each: `[sup] host PRESENT + wireless enabled -> armed go`,
   then `[sup] armed go: btd N has the host's session (ok …)`; wl.log `[armed] go: the controller answered in N ms`;
   `[radio_ap_up] AP up: … (AP-ENABLED N ms after it was serving)`. The `[timeline]` line: `bt_armed=1 bt_power=-`,
   `page_start` about 150 or less, `ap_start` about 200 or less, `ap_enabled` about 1300 or less, and `first_frame`.
   Between wakes, box.log shows the teardown, then the arm a few seconds after the host went.
3. **A against B.** `/tmp/bench_arm_off`, then the same wakes: `bt_armed=0`, `bt_power` stamped, today's timings.
4. **The fallback.** While armed, `kill -STOP $(cat /tmp/.btd_armed)`, then a wake: 3 s on, `[sup] armed go failed (no
   answer from btd N in 3 s) -- falling back to bt_on and a fresh btd`, and the connect completes by today's path
   (`bt_armed=0`, `bt_power` stamped, the go's `ap_start` kept).
5. **Pairing** a phone not bonded: "Add phone" in Carlink makes the box discoverable only with the host there.

**Revert** (`touch /script/radio_arm_off`, or the previous build) on any of: the box visible or connected while no host
is here (`PSCAN`/`ISCAN`, a phone's scan finding it, a link in `hcitool con`); a page with no go before it; the AP up
with no host; a go that fails on a healthy controller (`fail` in `/tmp/.btd_go_result`) more than once; a connect the
fallback does not complete; wakes slower than with `/tmp/bench_arm_off`. While there, `/usr/sbin/hostapd -v` says
whether this hostapd knows `noscan` (docs/ops/04_OPEN_ITEMS.md).

### Cold boot: real power cycles (20 Nov 2026; steps 3 and 4 run on `70caa3c`, 10 and 11 on `4a6ce9c`)

The cold boot's radio path (docs/wireless/01_BT_AND_RADIO.md, "The cold boot"): the descriptor kept across boots, the
Bluetooth firmware beside the Wi-Fi driver on the IW416, the preload started first and held behind the stock GPIO
set-up, the boot line, and the handover to a waiting host's session (R3-5, opt-in, off by default since 21 Nov). On
the dev adapter, then the truck. **Run
20 Nov on `70caa3c`** (the coordinating session, the dev adapter in final mode, power really removed, the host up
first and reaching the adapter at 4.2 to 5.0 s): together 3 of 3 (`hci_up` 6.82, 6.91, 7.03 s; `btfw_ms` 2207 to
2239; `tries=1 fallback=0 rc=0`; no Wi-Fi driver error; CarPlay every time), `/script/radio_together_off` 2 of 2
(`hci_up` 9.23 and 9.38 s); the first frame 13.8 to 15.4 s against 16.2 and 17.4; `page_start` 1.60 to 1.93 s after
`hci0` answered, which the handover (steps 10 to 14) is for. **Run 21 Nov on `4a6ce9c`** (steps 10 and 11, ten cold
boots: the table in docs/wireless/01_BT_AND_RADIO.md, "The cold boot", 6): the handover did as designed (`handed -
hci_up` 30 to 40 ms, `page_start - bt_power` 88 to 110 ms against 946 to 1045 without it) but 2 of its 6 boots had link
trouble against 0 of 4 without (0 of 9 with `70caa3c`'s), so it ships off; with the host at 3.3 s (boot 10, the
truck's timing) the loads ran about 1 s slower, `hci_up` 8.01 s, the first frame 16.85 s. `caps_ms` read 780 to 890 with `caps=kept` though the key
takes 0.05 s by hand on the idle adapter: the first 3.4 s of a power-up are slow throughout (cold flash, one busy core). **Every boot here is a power cut**: pull the adapter's USB
cable (it is powered from it) or switch its port, and give it power again. Never `reboot`: the IW416 keeps its firmware
through one, so a restart skips both downloads and times nothing of a cold boot (20 Nov); the line tells them apart
("The boot's radio line", above). Before each power cut, save the box log (Carlink's Save Logs, or `/tmp/box.log` over
the console): `/tmp` goes with the power. Read the boot line with `grep '^\[radio-boot\]' /tmp/box.log`. **On a final
adapter** (`cat /script/ocbm_mode`): in a trial (and on an update's first start) the dispatcher runs the self-test at
every start, whose `radio_detect.sh` fills `/tmp/radio_caps` (the boot line then reads `caps=detected`, and nothing is
kept), and holds the preload until it passes, so neither the kept descriptor nor the early start shows there; the
order does. After an update, step 1 is the first cold boot once the update has been committed.

1. **The first power-up on this build, no host** (Carlink closed or the display off): the descriptor detected and kept.
   Box log `descriptor: kept on flash (/etc/ocbm/kept_radio_caps)`, the boot line `caps=detected`. Over the console:
   `tail -1 /etc/ocbm/kept_radio_caps` reads `RADIO_KEPT_END=1`, `grep -c _MAC= /etc/ocbm/kept_radio_caps` is `0`, and
   `sh /script/radio_hal.sh probe` ends `bt_together=1` and `order=together off=- descriptor=...`. A `bt_together=0`
   here is the unit's own attach script waiting (the truck's 2025.02.25 text is unread): the order step below then
   has nothing to show, and the rest still runs.
2. **Three cold boots, no host.** Each boot line: `caps=kept` with `caps_ms` about 150 or less (the 0.8 s the
   detection cost, saved), `order=together off=-`, `btfw=complete`, `rc=0`, `tries=1`, `fallback=0`, `gpio` at or
   before `wlan_start` and `btfw_start`, those two within a few tens of ms of each other, and `hci_up - start` about
   3.7 to 4.4 s (the preload's 7.5 s before). The box log: `radio order: together`, `the Wi-Fi driver is up (sta0),
   beside the Bluetooth firmware` and `the preload lets the wlan lock go` (before `BT converged`, as the bench's Wi-Fi
   driver took 2.0 s against Bluetooth's 3.24), `BT converged: hci0 responsive on attempt 1`, `preload: done rc=0`,
   then `[sup] no host: arming Bluetooth for the next one`. `dmesg | grep -iE 'woal|mlan|fail|error|timeout'`
   shows nothing 2.6.5's cold boot does not. The preload's `waited about N ms for the radio's SDIO card` is absent or
   a few hundred ms at most, and never `no SDIO card in 3 s` (the 21 Nov review D: write down any N seen).
3. **A against B, the order:** `touch /script/radio_together_off`, three cold boots, then `rm` it. Those lines read
   `order=wlan_first off=owner` with `hci_up - start` about 6.5 to 7 s; together should be 2 s or more shorter.
4. **The truck's path, a host at about 3 s:** Carlink running and the display on before the adapter gets power, five
   cold boots. The boot line's `host` about 3000, `handover=off handed=-` (the default), and `wlan_free` before
   `hci_up`, which comes near 8 s with the host this early (the loads run about 1 s slower than with the host at 4 to
   5 s: `4a6ce9c`'s boot 10); in the box log the host's `[radio] hostapd starting` after the preload's `lets the wlan
   lock go`, the preload's `handover: off (not asked for ...)`, and the host's bt_on `already converged`.
   The `[timeline]` line: `bt_power` about 1 s after `hci_up` (less `host`), `page_start` about 1 s after that,
   `ap_start` and `ap_up` before
   `wifi_req` (`ap_enabled` reads `-` when hostapd enabled the AP before btd's session began, as on a cold start: then
   read `AP up: ... (AP-ENABLED N ms after it was serving)` in `/tmp/wlan.log` over the console), `wifi_rsp - wifi_req`
   about 0 (btd waited for no AP), `first_frame` near 16.5 s of uptime (2.6.5: about 19 s). `[startup]` still says
   `preload running`.
5. **A host before the preload** (its radios a host's `bt_on`'s): `touch /script/radio_preload_off`, two cold boots
   with the host up, then `rm` it. The boot line `by=bt_on order=together rc=0`; the AP up (the box log's
   `[radio] hostapd starting`, then `AP-ENABLED`) beside a Bluetooth bring-up that converges, each loaded once:
   `firmware preload:` once in the box log, and the Wi-Fi driver's start once in `dmesg`.
6. **What the early start costs the link:** three cold boots as in step 4 with `/script/radio_preload_late` on flash
   (the old start, after ocbmd and the supervisor), then `rm` it and three more. Compare the boot line's `host` (the
   SUBSCRIBE, on the uptime clock) and `hci_up`, and Carlink's own `[startup] carlink: ... SUBSCRIBE +N ms`: the early
   start must gain more on `hci_up` than it loses on `host`, and never cost a HELLO.
7. **The stock GPIO set-up first:** in every boot of steps 2 to 6, `gpio` is stamped and no insmod comes before it
   (the box log's `wlan driver:` and `firmware preload:` lines after `[ocbm-boot] radio preload starting`, and a
   `waited about N ms for the stock GPIO set-up` of a few hundred ms at most, or none).
8. **The guard, with real power cuts:** cut the power about 3.5 s after power-on (after `radio order: together`, before
   `BT converged`) on two boots in a row, then a cold boot with the host up. The preload's own guard counts the same
   cuts, so that third boot's preload is off (`radio preload DISABLED by its guard`) and the host's `bt_on` attaches:
   its log `2 together attaches in a row failed or never finished, the last one never finished ... together switched
   OFF`, `/script/radio_together_off` reads `guard: ...`, the boot line `by=bt_on order=wlan_first off=guard`, and
   Bluetooth up. Clean up: `rm /script/radio_together_off /script/radio_preload_off`, and one cold boot back to
   `order=together`.
9. **A restart is no cold boot:** `reboot` over the console once. Its boot line: `btfw_end - btfw_start` and
   `wlan_up - wlan_start` a fraction of step 2's. Write that ratio here, so a later reader can tell the two apart.

**A preload attach that fails** (none is expected; read it wherever a boot line says `by=preload` with `rc=1`, step 8's
cut boots aside): the box log's `preload: the attach failed - its helper stopped (no reset, no rmmod)`, then the host's
`bt_on` with `attach attempt 1` and no `reclaim+reset (attempt 1)` before it (the 21 Nov review A; on 2.6.5 that
attempt began with the reclaim+reset). Bluetooth up after it, and nothing in `dmesg` 2.6.5 does not show.

The handover (R3-5; docs/wireless/01_BT_AND_RADIO.md, "The cold boot", 6): **off by default; steps 10, 13 and 14
ask for it** with `touch /script/radio_handover_on` (the owner's opt-in; the bench's `/tmp/bench_handover_on` never
reaches a power cut's own preload), and `rm /script/radio_handover_on` gives the default back. The OPEN question
(2 of 6 boots with link trouble on `4a6ce9c`) wants many boots per arm: alternate the file and none, one bonded phone
with the second phone's Bluetooth off, and count, per boot, a connect in under 20 s or not, and every `answered, no
SDP channel`, `RFCOMM connect ... failed` and link-loss line in `/tmp/wl.log`. The host up before the adapter has
power unless a step says otherwise. Read, per boot: the boot line (`handover=on_owner`); the box log's
`[sup] ... the boot preload is attaching Bluetooth: btd starts now and waits for it`, the preload's `claimed for the
host's session` and `handover: hci0 is the host's session's from uptime N ms`, and the host's bt_on's `handed over by
the boot preload`; `/tmp/wl.log`'s `[handover] the boot preload handed hci0 over ...` and `[bt-bringup] from the boot
preload's handover ... it answers, scan 0x00`, then `handed over by the boot preload: no down/up` (no `down (+` and no
`up (+` line); and `[timeline] v1`, where **`page_start - bt_power` is `hci0` answering to the first page**
(`bt_power` is the boot line's `hci_up` less `host`, to the shell's 10 ms).

10. **Cold boots asked for (`/script/radio_handover_on`), the truck's path.** `handed` set, `handed - hci_up` about 30 ms (more only when `wlan_up` came
    after `hci_up`: then `handed` just after `wlan_up`); `btd_start` before `bt_power` on the timeline (btd started at
    the host's arrival); `page_start - bt_power` under 300 ms (the estimate is 100 to 150; 1600 to 1930 on `70caa3c`);
    `bt_up - bt_power` about 90 to 140; `ap_enabled` stamped now (btd's AP watch began at the host's arrival); CarPlay
    connects each time, `first_frame` compared with `70caa3c`'s 13.8 to 15.4 s after power-up. Over the console in one of them, 3 s after `handed`: `hciconfig hci0` still `UP RUNNING` (the claim
    held: the kernel's auto power-off did not fire).
11. **A against B, the default:** `rm /script/radio_handover_on`, as many cold boots as in step 10. Those read
    `handover=off handed=-`, `[sup] ... CLEAN bring-up of wireless stack (detached, pairing=...)` with no `btd starts now`, the host's
    bt_on `already converged`, btd's `down (+` and `up (+` lines, `page_start - bt_power` near 1.0 s (946 to 1045 ms
    on `4a6ce9c`; `bt_power` is then the host's `bt_on`'s, after the preload), and `hci_up` to the page near 1.6 to 1.9 s.
    Compare `hci_up` (and `btfw_ms`) between the two sets: btd's earlier start must not cost the attach more than
    about 100 ms.
12. **The kernel's auto power-off, seen once:** one cold boot with no host (Carlink closed) and
    `/script/radio_preload_off` on flash; over the console `: > /tmp/bench_arm_off` (or the supervisor arms the
    controller the next command attaches), then `sh /script/radio_hal.sh bt_on` and, 3 s after it returns,
    `hciconfig hci0`: a `bt_on`'s attach makes no claim, so this reads `DOWN` if the kernel closed it 2 s on (the reason
    for the claim). `rm /script/radio_preload_off /tmp/bench_arm_off` after. (The handover need not be asked for.)
13. **A new phone paired in a handed-over session** (the SSP event mask, which no DOWN/UP re-set): a cold boot as in
    step 10, then Add phone in Carlink and pair a phone the adapter has never seen; Numeric Comparison or Just Works
    completes and CarPlay starts. One known-good phone suffices.
14. **A host that leaves during the wait:** a cold boot as in step 10, and the display off (or Carlink closed) about
    1 s after the SUBSCRIBE, before `hci_up`. The box log: the supervisor's teardown, btd gone (`[handover] SIGTERM
    after N ms waiting`), no page in `/tmp/wl.log`; then, over the console, `hciconfig hci0` `DOWN` once the teardown is
    over, and no `/tmp/.radio_bt_handover`. Then the armed state as after any session.

**Leave the handover off** (`rm /script/radio_handover_on`: the default) on any of these, and treat its being asked for
as ready only after many boots show none: a handed-over session that does not reach CarPlay where step 11's do (2 of
6 on `4a6ce9c`, boots 3 and 4); `page_start - bt_power` over 1 s with `handed`
set; `[bt-bringup] the handed-over controller did not answer`; `hciconfig hci0` `DOWN` or a controller error in
`dmesg` during a handed-over session; step 13's pairing failing (`Pairing Unsuccessful`) where it works with the
switch; step 11 showing `hci_up` more than about 100 ms later with the handover than without; a page, `PSCAN` or
`ISCAN` with no host (step 14); `[handover] no handover and no word from bt_on` in `/tmp/wl.log` (the wait's 30 s bound,
the 21 Nov review I, reached on a boot where neither came). **The default itself** reverts like the rest of the build: it is the cold path `4a6ce9c`
took with its old off file, which boots 7 to 10 ran. A leftover `/script/radio_handover_off` (the dev adapter's) is
read by nothing, and the first start of an update to this build removes it (whichever updater placed it).

**Revert** on any of these. The order (`touch /script/radio_together_off`; the previous build if that does not hold):
`rc=1` or `together gave no responsive hci0` on a healthy unit, no `sta0`, a Wi-Fi driver error 2.6.5 does not show,
the AP missing or AP-ENABLED after `wifi_rsp` in step 4, `wifi_rsp - wifi_req` of 5 s (btd's AP wait ran out), any hang
(no console, no NCM), or a connect that fails with together and works without. The early start
(`touch /script/radio_preload_late`): a HELLO that fails or is late by more than the radios gain (step 6), or `no end
mark from the stock GPIO set-up in 3 s`. The kept descriptor (the previous build; `rm /etc/ocbm/kept_radio_caps` makes
one boot detect and keep it again): a boot line with `caps=kept` on which the radios do not come up but do after the
`rm`, an address in the file, or `caps=detected` on every boot (its key not holding still: report the two keys).

### Release gate — OCBM 1.0.5, the CarPlay-only cleanup (8 Nov 2026)

One build, one bench, on the same branches; a failure is fixed there and the whole list runs again.

- **Wired and wireless CarPlay** on Carlink 2.22: video, media audio, Siri, touch, metadata, the
  cluster stream, and a **phone call in both directions on each transport** (CarPlay's calls ride the
  AirPlay session; btd no longer has HFP/SCO, so a call that falls back to the phone's own Bluetooth
  audio is a fail).
- **Update from 1.0.4** over USB: the adapter reports `caps=0x000007d1`, `/usr/sbin/aa-bridge` is gone
  after the commit and the box log says `retired /usr/sbin/aa-bridge`; a rollback keeps it. Report the
  package size and the free flash after the commit.
- **Trial adapter** (over the 1,252 KB trial limit): Carlink offers "Remove stock firmware" first,
  then updates.
- **Fresh install** from a stick that already holds an `OCBM_BACKUP`: a backup that checks out is kept;
  one that does not is refused, never written over.
- **Compatibility:** Carlink 2.22 against adapters on 1.0.1 to 1.0.4 and on stock; Carlink 2.21
  against 1.0.5.

### Regression gate result — 2026-09-04 (after the AA telephony / metadata / tiers / margins / audio-engine changes)

Wireless CarPlay on the day's final app build (`250ec71`+), handed over from a live wireless AA
session by dropping the Pixel: box switched to mode 2 in 1.4 s, pair-verify/pair-setup, app-driven
SETUP negotiated 2400×960@60 HEVC with the RCS DataStream and the alt screen; video 44–60 fps with
0 decrypt failures, touch via HID, now-playing metadata (250 records), navigation over the tunnel,
Siri as AAC-ELD 16 kHz on the voice lane with ducking and the 16 kHz mic uplink, and a phone call —
all owner-confirmed ("Media, Siri, Phone calls work and sound good"). One single-packet voice-lane
underrun in the whole session. Note for readers of the AVmon line: `(idle)` on CarPlay means the
iPhone sent no frames because the screen was static (it resumes at 59 fps on a tap); Android Auto
streams continuously, so the same word there would be a fault.
