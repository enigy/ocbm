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
- `/tmp/bench_skip_first_requestui`: carplayd skips the 0.5 s screen nudge's requestUI once (the file is removed
  when used) and says so, so the 3 s second nudge fires.
- `/tmp/bench_sdp_after_acl_ms` (whole ms): how long btd gives the SDP channel after a page is answered (the link
  BT_CONNECTED) before it cuts the connect; 700 without it, or with `off`. Read at each page; a window not the
  default logs a `[bench]` line. The env lever `BT_SDP_AFTER_ACL_MS` wins over it.
- `/tmp/bench_arm_off`: the supervisor arms no Bluetooth between host sessions (2.6.0's powered-down state), from the
  next host's departure on; `off` (or no file) arms again, from the departure after that. An armed btd already
  running stays until then. The owner's switch for the same, on flash, is `/script/radio_arm_off`. With either, the
  next connects take today's path, so A against B on one bench is: a few connects, the file, a few more.
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

### The connect timeline (16 Nov 2026; M1)

Right after `[startup]`, the wireless carplayd prints one more line at the first frame, on the same stream
(`/tmp/carplayd_wl.log`, CH_LOG source `airplayd-wl`), so it reaches Carlink's drive log the same way. Every
critical-path event of the connect, stamped where it happens:

    [timeline] v1 sup_radios=406 bt_armed=0 bt_power=1246 btd_start=1276 bt_up=1286 page_start=1296 acl=1556 sdp=1716 rfcomm=1866 cert_req=1946 sign1_start=2066 sign1_end=2376 ident=2566 wifi_req=2666 wifi_rsp=2696 ap_start=1666 ap_up=1876 ap_enabled=2646 sta_assoc=3266 dhcp_ack=3566 carplayd_start=3166 svc_seen=3866 dial=3916 pv_done=4366 auth_in=4416 auth_out=4766 record_in=4846 record=5066 screen_setup=5116 first_frame=5466 chip1_ms=298 cert1_ms=41 chip2_ms=301 cert2_ms=40 usb_edge_s=11

(the unit test's line, not a measurement). The grammar is the one Carlink stores, `^\[timeline\] v1( [a-z0-9_]+=(-|\d+))+$`;
the keys come in this order always (`crates/box-common/src/timeline.rs`, which owns them). Every event is whole ms
since the origin; `-` is a value not stamped, one before the origin, or every event when there is no origin.

**Benched once** (the Mac, 16 Nov: the dev adapter on `e75a0ac`, 5 connects). The line arrived in the drive log under
`[box/airplayd-wl]` right after `[startup]`, one line of about 440 bytes; `first_frame` agreed with `[startup]`'s within
6-13 ms, and every shell stamp fell in order with the daemons' (so `/proc/uptime` and CLOCK_BOOTTIME share their base
on this kernel); the chip took 1640-1651 ms from go to ready and 112-159 ms for the certificate, every time.
`usb_edge_s` was `-` in all five (no configure edge after the supervisor's start). `bt_up`, `record_in`, the AP watch's
three keys and the stages per control connection came after that bench, and are not benched yet.

| Key | The event | Stamped in |
|---|---|---|
| `sup_radios` | the supervisor starts the radios (`wifi_ap_on` and `bt_on`; from the armed state, the go and `wifi_ap_on`) | `tools/session_supervisor.sh`, `wireless_up`'s block, `wireless_go` |
| `bt_armed` | NOT a time: `1` when the connect started from the armed state (Bluetooth powered between host sessions, the host's arrival a go; docs/wireless/01_BT_AND_RADIO.md), else `0`. From btd's `t bt_armed`, stamped as its first session opens the scans after the go, carried with the radio stamps; a stamp from before the origin is another host's and reads `0` | `wireless/src/main.rs`, `run_active_session` |
| `bt_power` | `bt_on` has the controller up and answering (each of its success paths; not the boot preload). `-` from the armed state: no `bt_on` ran | `radio_hal.sh`, `do_bt_on` |
| `btd_start` | btd begins this session's record (from the armed state: after the go, as its session starts) | `wireless/src/startup.rs`, `begin` |
| `bt_up` | btd's Bluetooth bring-up done: `[startup]`'s "Bluetooth up", on the timeline's clock (from the armed state: the scans opened after the go) | `wireless/src/main.rs`, `run_active_session` |
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
| `carplayd_start` | carplayd's `main` | `ccpa/carplayd/src/main.rs` |
| `svc_seen` | the `_carplay-ctrl` of the phone btd names first resolved (or a record held from before it was named, as it is) | `discovery.rs` |
| `dial` | the last connect-out written before the control connection that produced the picture (`-`: the phone came by itself) | `discovery.rs` `connect_out`, taken as that connection begins |
| `pv_done` | pair-verify OK, the control channel encrypted | `receiver/src/server.rs`, `feed` |
| `auth_in`, `auth_out` | `POST /auth-setup` received; answered | `server.rs`, `handle` |
| `record_in` | RECORD received | `server.rs`, `handle` |
| `record` | RECORD answered (its handler waits for the event channel, up to about 5 s) | `server.rs`, `handle` |
| `screen_setup` | the main screen's (type 110) SETUP | `session.rs`, `setup_phase2` |
| `first_frame` | the first frame to the host, as `[startup]` | `receiver/src/startup.rs`, `first_frame` |
| `chip1_ms`, `cert1_ms` | btd's signature (chip, go to ready) and certificate read (under the lock), ms | `wireless/src/mfi_local.rs` |
| `chip2_ms`, `cert2_ms` | auth-setup's, the same two, in the control connection that produced the picture | carplayd's `LocalMfiSigner` |
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
sent since the connection before it began. The shell's radio stamps are appended before btd begins a session, and btd
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
  `AP-ENABLED` is always seen.
- **Buffering.** udhcpd (BusyBox) writes each line in one write as it sends, so `dhcp_ack` is the ACK's time to within
  the watch's wake. hostapd writes through stdio into a file, which may be block-buffered: the 12 Nov drive log shows
  its station lines arriving at their own times, seconds apart, so they look flushed per line, but that is not proven.
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

**Length.** At most 805 bytes: no value is printed wider than 13 digits (above), `bt_armed` is one digit, and the
widest-line test builds the line with every value at that width; the one above is 490. ocbmd clips a forwarded line at 1024 bytes
(`LOG_MAX_LINE`), and Carlink clips nothing more: its CH_LOG parser takes each entry's length, and its drive log
writes the text through `LogRedactor`, which only shortens MAC addresses. So one line, never two. (With 20-digit
values, the u64's widest, the three keys added after the bench would have taken the line to 1029 bytes: the 13-digit
cap is what keeps the bound.)

### The armed path (16 Nov 2026; benched the same day)

Bluetooth powered but invisible between host sessions, the host's arrival a go (docs/wireless/01_BT_AND_RADIO.md,
"Between host sessions: armed").

**The first bench** (16 Nov, the dev adapter, `resident-bt` merged with `carplayd-early` as a local `round2`): 12 wakes
and a cold boot from the armed state, `sup_radios` about 50 ms, `bt_armed=1`, `page_start` about 100 ms, `ap_enabled`
about 1.16 s after the host's arrival; the no-host state as in step 1 below; arming about 1.0 s faster than
`/tmp/bench_arm_off`; the forced fallback (step 4) as written. One bad wake, the first about 90 s after an update was
committed: 24.8 s, the phone associated 8.6 s after RFCOMM (0.4 s otherwise), no `sending ACK` from udhcpd in 60 s
though the phone ended up on its old IPv4 address, its service resolved 6.5 s after the join; not repeated after a
reboot. Its likely cause is fixed (the last AP's DHCP server, slow over its lease file on flash the commit's garbage
collection slowed, taken for the new AP's; docs/wireless/01_BT_AND_RADIO.md, "The AP, written ahead"). If it recurs,
right after it: `/tmp/wlan.log`, `/tmp/radio_ap_dhcp.log`, `df /`, and the time of `[dispatch] update committed` in
`/tmp/box.log`; then a wake soon after an update's commit is a case of its own on the next bench.

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
