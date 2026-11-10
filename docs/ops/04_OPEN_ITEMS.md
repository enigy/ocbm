# Open items and roadmap

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs OPEN, 06; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

Every known-open item in one place, plus the ordered roadmap. An item leaves this file only when it is closed with evidence.

> **OCBM 1.0.5 (8 Nov 2026) made the adapter CarPlay-only.** Items about what left the tree with it —
> Android Auto, Bluetooth call audio (HFP/HSP/SCO), the host MFi relay, `CH_IP`/`CH_ETH` and the
> benches, the app-driven SETUP relay (`CH_RTSP`), the GM bridge role, the Raspberry Pi and C2Air
> ports, the macOS host app and the in-tree Android apps — are closed by that release, not fixed. The
> sections that held only such items are reduced to a pointer; the items as they stood are at tag
> [`v1.0.4`](https://github.com/enigy/ocbm/blob/v1.0.4/docs/ops/04_OPEN_ITEMS.md). Carlink's own open
> items live in its repository.


## Settled — do not re-raise

- **`pi/evidence/hostapd_5g.conf` carries a `wpa_passphrase`, and that is accepted.** The values in
  it and in gm_ccpa's head-unit credential are deliberate placeholders for a bench AP; they protect
  nothing. Assessed and accepted by the owner 2026-08-31, explicitly including the case where either
  repo gains a remote. No rotation, no history scrub. Three separate audits have flagged the key
  *name* without reading the value — pattern-matching on `wpa_passphrase` is not a finding.
- **This repo's history carries no assistant-session trailers and no conversation links.** Verified
  2026-08-31 by grepping the whole log for both patterns: zero hits. The sibling gm_ccpa repo has 40
  commits that do; that is tracked there, not here. Write neither pattern out literally in a doc —
  the publication mirror's pre-push hook scans added CONTENT as well as commit messages, so even a
  sentence saying there are none will block the push.

## Open items

<!-- absorbed: ../ops/04_OPEN_ITEMS.md -->

Open work in this project is scattered across a 58-document corpus, most of it inside documents
whose own status line now reads `HISTORICAL` or `CURRENT-WITH-CORRECTIONS`. A reader cannot tell
what is still outstanding without reading all of them. **This is that index — one line per item plus
a back-pointer. It is not a spec and not a plan**: the detail, the reasoning and the wire evidence
stay where they already are, and nothing here supersedes, replaces or closes anything.

**Every item below was re-verified against the working tree on 2026-08-16 by the command shown on
its `Verified open` line.** That verification is the point of the document: the source documents are
up to five weeks old and a large number of their "open" items have since shipped. Items the
documents still call open but the code shows closed are listed separately under
[Found closed](#found-closed--documents-still-call-these-open) — that list is a deliverable, not a
failure. Items that could not be settled either way are under [Unverified](#unverified).

Anchors are **symbols and greppable strings, not line numbers** — line-number rot is this corpus's
single largest source of false claims (see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md)
§DO NOT). Grouping is by subsystem, because a reader wants "what is open in the host app", not
"what does docs/ops/05_AUDITS.md say".

**Scope note.** Owner-decided WONTFIX items are recorded under
[Closed by decision](#closed-by-decision--do-not-re-plan-these) so they are not re-planned as work.
Superseded *design positions* (box-owned `/info` policy, the `/tmp/carplay_metadata` lever as a
canonical control, box-autonomous page-on-boot) are **not** open items and are not listed;
[../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) carries those.

---

### Box daemons — `ocbmd` / `carplayd` / `iap2d` / the supervisor

> **Also see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) Phase 4**
> for the ten catalogued code defects (the CT_HELLO `out_console` resync, `custom_init.sh`'s
> `/script/start_bluetooth_wifi.sh` call, the missing `iap_role_switch` install). They are
> deliberately not duplicated here so the two documents cannot drift.

- **Host-app controls waiting on box verbs (2026-09-25, from the Android Settings → OCBM audit;
  app side landed in `host/CarlinkAndroid`, see OCBMANDROID.md "Settings → OCBM action mappings").**
  The app hooks exist and are capability-gated; NOTHING below is implemented box-side, and no verb
  id is assigned — the proposed shapes are in
  [../carplay/01_OCBM_PROTOCOL.md](../carplay/01_OCBM_PROTOCOL.md) ("Proposed MGMT verbs").
- **USB link stalls and the idle "wedged" restarts — one cause, fixed 2026-11-02, benched the same day: pass on the dev adapter, FAILED on the truck (see below).**
  The two-iPhone bench of `b2c92d9` saw three link stalls in ~40 min of CarPlay (Carlink's heartbeat
  writes timed out 5 times, the adapter declared the host gone and tore the wireless stack down), and
  an ocbmd idle with no app was L2-restarted by the supervisor about once a minute for an hour. Both are
  the accessory device blocking: `acc_read` with no host writing, `acc_write` with no host reading,
  O_NONBLOCK notwithstanding, and its poll always says ready
  ([../carplay/00_ARCHITECTURE.md](../carplay/00_ARCHITECTURE.md), "`/dev/usb_accessory` blocks").
  ocbmd now reads and writes it on two relay threads through pipes (`start_acc_relay`), so the poll
  loop keeps running, reading heartbeats and touching `/tmp/ocbmd_alive` while the host is not reading.
  Carlink `bench-1102` stops its own USB read thread waiting more than 1.5 s on a stuck decoder or
  audio track (the likely reason the host stopped reading) and logs a stall snapshot at the first
  failed heartbeat. Benched 2026-11-02 (`e9bb7c8`): 41 min of CarPlay with no stall, no "ocbmd
  wedged" in 10 min idle, and video 15.1 s after a Carlink restart (a frame the pipe holds part of at
  a new HELLO goes out cut short; the host resyncs and its next HELLO is answered). **But on the truck
  adapter `9b2331d` never answered a HELLO (2 Nov, 17:06):** its accessory driver serializes reads and
  writes ([../carplay/00_ARCHITECTURE.md](../carplay/00_ARCHITECTURE.md), "Not every unit's module
  allows that"), the self-test passed with no host to ask, and the unit sat on a dead trial until
  `OCBM_RESTORE` took it back; the truck is on `052fd04` again. Fixed the same day (`a81d3f5`):
  ocbmd falls back to the single-threaded loop on such a driver and keeps that on flash
  (`AccIo::Direct`), and a trial whose `ocbmd` hears a host for two minutes without answering goes
  back by itself (`answer_watch`, [01_RECOVERY.md](01_RECOVERY.md)). **Truck bench, 2 Nov 21:05:**
  the fallback worked (the first `ocbmd` restarted single-threaded, `/etc/ocbm/acc_direct_io` was
  saved, the session ran), but `answer_watch` sent the working trial back at 121 s: the host's answer
  went to the restarted `ocbmd`, which had not seen the HELLO. **Retry, 21:17-21:36: PASS.** The revert
  had kept the flag, so `ocbmd` was single-threaded from its first start: HELLO_ACK in 8 ms, heard and
  answered both written, Scott's iPhone paired, HEVC 8 s after Bluetooth; after a power cycle, HELLO_ACK
  in 4 ms and video 22 s after Carlink connected, `dropped=av:0`. The restart case is fixed in
  `924f497` (a restarted `ocbmd` starts from the heard/answered files; `ocbm-host` sends nothing
  without a HELLO_ACK). **Truck bench, 22:10-22:24: PASS**, a real fresh install: a full
  `stock_restore.sh` (which removes `/etc/ocbm` whole, the flag with it), then a stick install. The
  first `ocbmd` heard 7 HELLOs and restarted single-threaded; the new one's first frame was SUBSCRIBE,
  and it wrote the answer (heard at 4 s, answered at 11 s, uptime). Carlink had its HELLO_ACK 3.5 s
  after its first HELLO, and the trial was still up past 125 s. Carlink's "no CT_SETTIME ack —
  continuing anyway" on that first contact is expected: the SETTIME waits in the driver through the
  restart, and the new `ocbmd` applies it about 3 s late, past Carlink's 2 s wait. Cold restart and
  power cycle after it: single-threaded from the start, HELLO_ACK in 5 ms and 19 ms, no drops or
  stalls. The truck is on the `924f497` trial, unconfirmed. **Dev-adapter bench, 2 Nov: PASS**
  (CarLink-fd6c, two iPhones): a fresh install from stock, relay threads on every `ocbmd` start and no
  flag, HELLO_ACK in 2 ms, heard and answered on record, switches 8 of 8 with video, cold restart and
  power cycle clean. One finding there: after the power cycle the first-called iPhone answered the
  page and reset the link 4 s later, and `btd` went on to the second phone, which got CarPlay. Fixed:
  such a phone is paged once more before the next ([../wireless/01_BT_AND_RADIO.md](../wireless/01_BT_AND_RADIO.md),
  "A phone that answers and resets"), NOT benched: no phone reset the link on the 3 Nov bench.
- **One-file install, dev adapter, 3 Nov (`cb97f01`): PASS.** A retry over a try a power cut stopped
  mid-place removed its files ("2510 KB", then 3760 KB free) and installed; two starts that never
  passed the self-test went back to stock at the third; a finish cut at 49 of 105 deletes resumed
  and completed; in final, `OCBM_RESTORE` was left alone with `OCBM_RESTORE.txt` beside it (the
  trial watcher outliving the finish is covered by Stage B only); two restores from final matched
  the backup. **Still open: stock CarPlay after the prune is unverified in a live session** (Carlink
  2.08 cannot run one on Carlinkit's firmware), while the revert report ("Stock CarPlay and Android
  Auto work as before") and Carlink's Go back dialog both promise it. The wording stays until that
  session is run or Scott rewords it.
  1. **`MGMT_DISCONNECT_PHONE`, transport-aware, plus a capability bit.** Problem: the app's
     Disconnect Phone has no honest verb to send — OCBM's six MGMT verbs are
     `crates/ocbm-proto/src/lib.rs:334-346`, dispatched at `ccpa/ocbmd/src/main.rs:3397-3446`; the
     Android button used to send `MGMT_RESTART_WIRELESS` in disguise (nothing for a wired phone, a
     whole-stack bounce for a wireless one). Proposed semantics — WIRED: end the CarPlay session and
     soft-detach / re-attach the phone-facing USB gadget so iOS sees an unplug (the un-CONFIGURED
     edge `iap2d` already exits on, per CLAUDE.md's wired-tier note; `projection_up` respawns it);
     WIRELESS: drop that phone's BT ACL + Wi-Fi session WITHOUT `wireless_down` (btd's control port
     already addresses one phone, `crates/vendor/wireless/src/control.rs:34-39`); BOTH: a reconnect
     hold-off so the phone does not re-page at once, `MGMT_ACK` 0/1, and a `CAP_*` bit in
     `HELLO_ACK` so a host can enable the button. App hook waiting: `OcbmClient.supportsPhoneDisconnect`
     (one line: `caps and CAP_<bit> != 0`) → `OcbmClient.disconnectPhone()` →
     `CarlinkManager.disconnectPhone()` → the Control-tab button enables itself and its
     "Requires adapter firmware support" line disappears; no UI edit. Acceptance: wired session,
     press Disconnect Phone → iOS ends CarPlay within ~2 s, box emits `SEV_PHONE_ABSENT` then
     `SEV_PHONE_PRESENT`, no re-arm during the hold-off, the phone reconnects on its own afterwards;
     wireless session, same press → `BT_PHASE IDLE`, `PROJ_MODE NONE`, hostapd and hci stay up
     (no `wireless_down` in the supervisor log), the phone re-pages after the hold-off.
  2. **LANDED: `MGMT_CONNECT_DEVICE 0x08` (targeted connect) and, 2026-11-01, `MGMT_SWITCH_DEVICE
     0x0D` (`CAP_PHONE_SWITCH`), which ends ANOTHER phone's wireless session and holds the box 45 s
     for the phone asked for — the wireless half of item 1's need, for switching rather than plain
     disconnecting (see the protocol doc's MGMT row).** Original problem: a Phones-tab card tap used
     to send `MGMT_RESTART_WIRELESS` (wrong with two bonds, a radio bounce for a wired phone); the
     cards are information-only until this exists. Evidence: btd's control port already has
     `{"cmd":"connect","address":..}` (`crates/vendor/wireless/src/control.rs:36`, dispatch `:499`)
     next to the `pair_answer` verb (`:557`) that ocbmd already relays (`send_pair_answer`,
     `ccpa/ocbmd/src/main.rs:2269`) — the MGMT relay is the only missing piece. Semantics: forward
     to btd, `MGMT_ACK` 1 when the MAC is not bonded. App hook waiting: re-add
     `CarlinkManager.connectToDevice(mac)` → `mgmtAction(MGMT_CONNECT_DEVICE, mac)` and the card's
     `onTap`. Acceptance: two bonded phones in range, tap B → `BT_PHASE LINK_UP` →
     `CT_PHONE_IDENT.deviceID == B`. **Benched 2026-11-01 on the dev adapter (IW416, installer
     6affefc) with ONE iPhone only:** `CAP_PHONE_SWITCH` advertised (caps `0x7ff`); pairing, a
     wireless session and a reconnect `btd` paged out all ran on the new `btd` with no hold logged;
     Carlink's Connect stays disabled on the projecting phone's row. **Two iPhones the same afternoon:
     it switches on Bluetooth and fails on Wi-Fi.** `btd` re-entered `run_active_session` in-process
     (the bring-up redone in 2.6 s) and paged only phone B, identified 9 s after the tap. But phone A
     stayed on the AP: the new `carplayd`'s browse loop dialled A's `_carplay-ctrl` first
     (`ccpa/carplayd/src/discovery.rs` dials every phone it resolves) and A projected again; B's
     control connection then preempted it (`main.rs` accept thread, "hijack: new control connection
     preempts the current session"), and B's session ended 15 s later, leaving neither. With A's
     Wi-Fi off, B held. The fix, handed to the remote agent: `btd` tells `carplayd` which phone it
     serves, and `carplayd` dials and keeps only that phone; optionally the dropped phone is
     deauthenticated from the AP for the hold. Until it lands, no adapter, the truck's included,
     should get `3d492a2` or newer. **Landed 2026-11-01 (`b31b7bc`), benched with one iPhone**
     (`ccpa/carplayd/src/phone_gate.rs`): at each 0x5703 `btd` writes the phone's BR/EDR address to
     `/tmp/carplay_expect_phone` and passes that path to the wireless `carplayd`
     (`CARPLAY_EXPECT_PHONE_FILE`; a file, because a latched `carplayd` outlives one phone's link).
     A phase-1 SETUP whose `deviceID` is another phone gets 500 and its address is refused (dropped
     on accept, not dialled) until `btd` names another phone; while the named phone's session is up
     another address does not preempt it. Discovery logs each `_carplay-ctrl` TXT record, and a
     phone leaving now forgets only its own dialled addresses. Not done: deauthenticating the
     dropped phone (fix 2). The unit has no `hostapd_cli`, its `/etc/hostapd.conf` has
     `macaddr_acl=0` and no `ctrl_interface`, and its `hostapd` (v2.7) is packed, so a control
     socket or a deny file has to be tried on an adapter. The same push: forgetting an idle
     phone while another is live drops only its bond through `btd` (no restart; the AirPlay
     pairings and the restart wait for no session, `FORGET_PENDING_FLAG`), and `ocbmd` re-sends
     `/tmp/phone_identity` when `carplayd` rewrites it, so a fresh host learns the same phone
     reconnected. **Benched that evening on the dev adapter** (Carlink `connect-feedback`): with
     one iPhone, pairing, CarPlay, the reconnect at the app's start (video 27 s, nobody touching
     the phone), the gate (the dropped phone's SETUP refused after a switch), the in-place forget
     and the re-sent identity all passed. The two-iPhone switch is still unproven: phone B was
     identified over Bluetooth and got the Wi-Fi details but never joined the AP, and `btd` held
     B's Bluetooth session with no CarPlay for anyone until the app stopped, 3 min later. After the
     hold it should drop a session that never reached Wi-Fi (fix 6). `_carplay-ctrl`'s TXT `id`
     is the phone's BR/EDR address, so discovery can dial only `btd`'s phone (fix 1b).
     **Fixes 6 and 1b landed 2026-11-02, NOT yet benched.** Fix 6 (`Control::switch_stranded`): a
     switched-to phone's session still live when the hold ends (and at least 20 s after it started),
     with no SETUP from it in `/tmp/phone_identity`, is ended through the
     go-quiet teardown; that phone is then last in the call order and refused on the accept path for
     45 s, or until the app asks for it again. Fix 1b (`PhoneGate::wants`): discovery skips a
     `_carplay-ctrl` whose TXT `id` is another phone than the one `btd` names; a TXT without an id
     is dialled as before and the SETUP check still applies. **Benched 2026-11-02 with two iPhones
     (Carlink 2.07 debug, emulator): both pass.** Both phones' TXT `id`s are their BR/EDR addresses
     (no fallback needed); fix 6 ended a Wi-Fi-off phone's session at tap+45 s and the other phone
     streamed at tap+58 s; switches went 4 of 5 and 3 of 3 (15-21 s tap to video) and no dropped
     phone retook CarPlay. The same bench found three more, fixed the same day, NOT yet benched:
     the supervisor's A2 radio gate read `/tmp/carplay_transport` twice per tick, so a switch
     clearing it in between looked like a wired session and the radios were restarted, and the
     restart lost the hold (A2 and `wireless_up` now read it once: `wired_phone_on_bus`); the hold
     now outlives a `btd` restart (`/tmp/btd_switch_hold`, monotonic deadline,
     `Control::keeping_holds_in`); and `carplayd` dialled the phone of its own live session again
     when that phone's record gained addresses, which ended the session (`PhoneGate::serving`).
     Link-local dials now take the scope of the interface holding the AP address (the bench unit's AP
     is `sta0`, not the `wlan0` `btd` passes). Fix 2: the unit's `hostapd` takes
     `ctrl_interface=/tmp/hostapd_ctrl` (probed RAM-only with the AP up), but there is no
     `hostapd_cli`, so `btd` would speak the control socket itself.
     **Benched 2026-11-02, 09:40-11:20 (`e9bb7c8`, Carlink `bench-1102`, two iPhones): the relay
     threads pass.** Idle 10 min with no app: no "ocbmd wedged", no L2 restart, one pid. 41 min of
     CarPlay with 16 switches (13-18 s each, each to the phone asked for): no stall, and `carplayd`
     left its live phone alone every time it advertised again. Carlink restart to video 15.1 s. The
     same bench found four more, fixed the same day, NOT yet benched: (a) after an AP restart a phone's
     address changed under its CarPlay session (the leases were deleted; see
     [../wireless/01_BT_AND_RADIO.md](../wireless/01_BT_AND_RADIO.md), "DHCP leases survive"); (b) when
     a phone's CarPlay ended outside a switch and the phone never advertised again, `btd` held its
     Bluetooth link with no CarPlay for 2+ min. `carplayd` now clears `/tmp/phone_identity` when the
     session ends, and `Control::carplay_lost` ends a session whose phone reached CarPlay and has been
     without it for 20 s, holding the box for that phone as a switch does: it is paged for a fresh
     handoff, and passed over (fix 6) if that does not reach CarPlay either. The cleared identity also
     stops a fresh host being told the last session's phone is live; (c) Carlink kept a frozen frame
     when a session dropped before its first media (Carlink `bench-1102`); (d)
     `tools/stock_restore.sh`'s `usb_has` read "not on USB" under pipefail (`grep -q` SIGPIPEd
     `ioreg`). The stock restore before the next bench found one more: (e) the host-gone teardown left
     the idle wireless `carplayd` running with no app (from 11:16 to the restore at 12:05, where its
     deleted binary held 1.1 MB). `wireless_down` chose its branch from the transport flag alone, and
     `carplayd` clears that flag when a session ends, so with no session live it took the wired-owned
     branch and spared `carplayd`. It now spares it only while the wired arm is ARMED or `carplayd`
     reports a wired peer (`wired_carplayd_live`). The shell suites do not cover the supervisor: bench
     only. **Benched 2026-11-02, 13:23-13:56 (`a19a694`, Carlink `bench-1102`, two iPhones):** a phone
     keeps its address over app restarts (udhcpd ACKed each rejoining phone's request from the kept
     lease file, no DISCOVER); the identity is cleared at every session end and at a SUBSCRIBE with no
     session; Carlink covers the last frame on every drop; the app leaving with no session took the
     COMPLETE teardown and left no `carplayd`; `usb_has` found the adapter at `ioreg` line 355 of 485.
     Not exercised: `carplay_lost`, which needs CarPlay to drop with Bluetooth up (turning Wi-Fi off
     drops both; the address flip that did it is fixed). **Bench 2026-11-12, 06:39 (`a53508f`, one
     iPhone by the box, the other away):** the phone's Wi-Fi blipped mid-session; its next Bluetooth
     session reached pair-verify, the phone withdrew its CarPlay service and never sent SETUP, and
     `carplayd`'s idle backstop ended it. `btd` then held the Bluetooth link, Identified with no CarPlay,
     for 9.5 min until the host re-attached: `carplay_lost` covers only a session that reached CarPlay,
     and fix 6 only a switched-to phone. Fixed the same day, NOT yet benched:
     `Control::carplay_never_reached` ends such a session 10 s after its `carplayd` leaves without
     CarPlay (the transport flag cleared), or 90 s into one with no `carplayd` at all (the supervisor's
     `ESTAB_CONNECT_GRACE`, which covers an Allow tap), passes the phone over, and lets the next session
     call the other phones first and then page it. **Benched 12 Nov (the Mac): FAIL** for a phone that never
     joins: with its Wi-Fi off 2.5 min, btd's own "wireless" claim at the 0x5703 handoff read as a live
     `carplayd`, held the 90 s deadline off, and the box sat 8 min with no page. Fixed, NOT yet benched:
     btd's claim no longer counts as a `carplayd` (`av::claimed_at`), the 90 s deadline holds with a
     `carplayd` live or not, and the supervisor reads milestones only after the last `carplayd` spawn
     mark in `/tmp/carplayd_wl.log`, so an earlier session's "pair-verify OK" no longer latches. Found, fixed the same day, NOT yet benched: the
     AirPlay peer store kept one key per pairing identifier and both iPhones present the same one, so
     every change of phone redid pair-setup (one switch failed when the phone abandoned it after M3);
     it now keeps up to 8 keys per identifier ([../carplay/02_SESSION_LIFECYCLE.md](../carplay/02_SESSION_LIFECYCLE.md)).
     And an iPhone 7 on iOS 15 tore down its screen stream about 30 ms after setting it up in 2 of 5
     sessions (4 of 5 on the bench before), leaving media and no video for minutes; when no new screen
     stream follows within 3 s, `carplayd` now sends `requestUI` again (`AvSession::nudge_screen_back`).
     Why iOS 15 tears it down is not known; the failing sessions sent one more `modesChanged` before the
     screen SETUP than the good ones.
  3. **"Phone drop on host re-SUBSCRIBE" (`PROJ_MODE NONE` then `SESSION_EVENT PHONE_ABSENT` after
     an app relaunch) — NOT reproduced, 2026-09-25.** Three timelines, all wired, the phone never
     left the bus in any: (a) kill-without-STOP relaunch (`adb install -r`): SUBSCRIBE → stale
     `WIRED_CP` replay → `NONE` +2.5 s (the box's forced re-arm teardown) → `WIRED_CP` +4.5 s →
     video +6.8 s; (b) clean CT_STOP then a 27.6 s gap (activity destroyed, relaunched): SUBSCRIBE →
     `NONE` immediately (box already idle) → `WIRED_CP` +2.5 s → video +4.3 s; (c) the app's
     clean-slate reset (CT_STOP, 6 s wait, new nonce): SUBSCRIBE → `NONE` immediately →
     `WIRED_CP` +5.2 s → video +7.1 s. So after a clean STOP plus ≥ 6 s the box is idle at
     re-SUBSCRIBE and `PROJ_MODE NONE` is the replayed CURRENT state, not a transition, and the
     host returning "before teardown completes" is only the (a) shape — which still kept the phone.
     If `PHONE_ABSENT` recurs, look at the supervisor's L1 `phone_reset` rung
     (`tools/session_supervisor.sh`, `escalate`) and the gadget un-CONFIGURED edge, not at host timing.
  4. **Is the A/V session key re-issued to a same-nonce re-SUBSCRIBE?** Answered for the reattach,
     2026-11-10: **no, until the fix below.** On the bridge self-update bench (AAOS emulator, dev
     adapter on `5ffd015`) the head unit's bridge app restarted for its update, Carlink's link came back
     in about a second with the same nonce inside `HEARTBEAT_GRACE`, and the box confirmed presence
     over the live session. carplayd's seams to ocbmd outlived the link, so no `SEAM_KEY` was sent
     again: audio played, video stayed undecryptable, and Carlink showed no phone, on both updates,
     until a Carlink restart (new nonce, so a replacement and a re-arm). **Fix (`76d0c71`),
     device-proven 2026-11-10:** a HELLO while a session is live that is not a replacement latches
     `host_reattached`; its SUBSCRIBE closes the keyed A/V producers after the dispatch pass
     (`rekey_av_seams`: main and cluster video, media and voice audio; metadata stays), and carplayd
     reconnects each with a fresh `SEAM_KEY` (audio also `SEAM_FORMAT`) and a ForceKeyFrame for the
     video ones. A config re-push (SUBSCRIBE with no HELLO) keeps its keys. Bench (the Mac: dev
     adapter on `76d0c71`, AAOS emulator, iPhone on iOS 27.2): the bench bridges reinstalled under a
     live session, 1.0.11 → 1.0.10 → 1.0.11. Both times the audio key and first audio frame came
     about 2.7 s after the bridge died, the video key about 5 s and the first frame about 5.5 s
     (Carlink holds the first key up to 3 s until the box's state at SUBSCRIBE proves live), and
     Settings showed the phone. ocbmd logged `reattached=true` at HELLO and SUBSCRIBE, then
     `closed 3` and `closed 2 keyed A/V seam(s)`. The other
     same-nonce path, the heartbeat re-SUBSCRIBE after `SEV_HOST_GONE` (`OcbmClient.startHeartbeat`),
     follows a presence drop and so a re-arm; it is still unbenched.

- **No inbound `stopSession` handler.** The box declares a five-value `stopSessionReasons`
  vocabulary and then ignores the command; `disconnectReason` plumbing to the supervisor follows
  from the same gap. Both halves sit behind `CARPLAY_SESSION_MGMT`, so shipping the declaration
  without the handler would be worse than not declaring it.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §10 ("the single highest-value fix"). Verified open 2026-08-16:
  `grep -rn "stopSession" crates ccpa` → three hits, all declaration-side (`info.rs`
  `stopSessionReasons`, an `av.rs` comment); nothing in `session.rs`'s `fn command`.

- **`isRemoteControlOnly`, `sessionUUID`, `sessionCorrelationUUID`, `hijackID` are never read.**
  `isRemoteControlOnly` is a protocol-native holding pattern where ours is improvised, and
  `sessionCorrelationUUID` is confirmed live on the wire.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §10. Verified open 2026-08-16:
  `grep -rn "isRemoteControlOnly\|sessionWillBeHijacked\|sessionCorrelationUUID\|hijackID" crates ccpa host`
  → zero hits; `sessionUUID` appears only in a doc comment.

- **P2 lifecycle: `CT_SESSION_EVENT` was never widened to `[state][reason]`.** Every emitter and
  every consumer on all three hosts still sends and parses exactly two bytes, so the box cannot
  stream real lifecycle state or a failure reason to the app.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §Phasing, [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-11-1`. Verified open 2026-08-16:
  `grep -rn "CT_SESSION_EVENT" --include="*.rs" --include="*.swift" --include="*.kt" .` → all
  emitters are `&[p::CT_SESSION_EVENT, sev]`, all consumers gate on `l >= 2`.

- **P2 lifecycle: no single reason-carrying finalize.** `AvSession::teardown` takes only the request
  plist and `reset()` only `&self` — no `OSStatus`-style reason is threaded from clean stop, failure
  or `Drop`, so a failed teardown is indistinguishable from a clean one.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Supporting layers", `R-11-1`. Verified open 2026-08-16:
  `grep -n "fn teardown" crates/vendor/receiver/src/session.rs` → no reason argument on either arm.

- **P2 lifecycle: the `ccpad` Rust supervisor daemon does not exist.** Supervision is still the
  hardened shell loop; the strategic endpoint (dependency DAG, real `fork`/`waitpid`, one
  `::respawn:` entry) has no crate and no binary.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Formalization & strategic direction", `R-11-1`. Verified open 2026-08-16:
  `grep -rn "ccpad" --include="*.rs" --include="*.sh" --include="*.toml" .` → zero hits.

- **Multi-second blocking I2C still runs with the tunnel's `SESSION` mutex held.** The structural fix
  (compute the action, drop the guard, run the MFi op, re-acquire, re-validate) was never written, so
  a chip stall blocks the RCS reader, the event handler, `POST /command` and teardown.
  Source: `docs/ops/05_AUDITS.md` §8 "Still open". Verified open 2026-08-16:
  `grep -n "SESSION\|execute(" crates/vendor/receiver/src/iap_tunnel.rs` → `execute` is called inside
  the `plock(&SESSION)` guard and calls `mfi_retry("cert", …)` straight into the chip.

- **Artwork duplicate-fragment corruption — the iAP2 link layer still has no duplicate-sequence
  suppression.** An over-length buffer is emitted as a corrupt JPEG and acknowledged `Success`, with
  only an `[art] OVERLONG` log line; the fix was designed and deferred to a Phase 2 that never ran.
  Source: `docs/ops/05_AUDITS.md` §4, `docs/ops/05_AUDITS.md` §"Where the agents corrected the audit" (I2). Verified open
  2026-08-16: `grep -ni "duplicate\|dup_seq\|seen_seq" crates/vendor/iap2-core/src/link.rs` → no
  dedupe; `peer_seq` is only assigned.

- **`CT_INPUT_NACK` was never built.** The box still silently discards `CH_INPUT` while the host is
  GONE, with no instant notification back to the host.
  Source: `docs/ops/05_AUDITS.md` §"Also deferred", `docs/wireless/01_BT_AND_RADIO.md` §3. Verified open 2026-08-16:
  `grep -rn "CT_INPUT_NACK\|INPUT_NACK" crates ccpa host` → zero hits.

- **`connect_seam` still resolves through `to_socket_addrs()` rather than numeric-only.** It was
  deferred pending a host-app `CH_IP` usage check; `CH_IP` is retired (1.0.5) and every caller now
  passes a literal `127.0.0.1:<port>`, so numeric-only is safe to make. The resolver path is unchanged.
  Source: `docs/ops/05_AUDITS.md` §"Also deferred". Verified open 2026-08-16:
  `grep -n "fn connect_seam" -A6 ccpa/ocbmd/src/main.rs` → `target.to_socket_addrs().ok()?.next()?`.

- **`av_dropped` / `lo_dropped` are stderr-only.** The app's CCPA tab polls `MGMT_GET_INFO`, so the
  two backpressure counters that matter most for diagnosing A/V loss are invisible to the only UI
  that polls the box.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §3. Verified open 2026-08-16:
  `grep -n "fn box_info_json" -A45 ccpa/ocbmd/src/main.rs` → neither counter is in the emitted JSON.

- **`wireless_down` orphans the bring-up's forked children.** Both teardown branches `pkill` only
  `btd`, `carplayd` and `rx-connect`; `wlan_on.sh`, `bt_on.sh` and
  `attach_bluetooth.sh` survive. The obvious `pkill` fix is the IW416 wedge the file warns about
  elsewhere, so a lock is probably the right shape.
  Source: `../ops/04_OPEN_ITEMS.md` §5 (#28). Verified open 2026-08-16:
  `grep -n "pkill" tools/session_supervisor.sh` → 20 hits, none naming `wlan_on.sh`, `bt_on.sh` or
  `attach_bluetooth.sh`; the `wireless_down` teardown reaps only the three daemons.

- **The sub-16-byte opcode-0 decrypt lane never advances its counter.** A `<16` byte VideoFrame body
  passes through as plaintext without incrementing `counter`, so if that lane ever fires the box and
  host nonce counters desync permanently. Only the host half of the fix landed.
  Source: `docs/ops/05_AUDITS.md` §"Where the agents corrected the audit" #3. Verified open 2026-08-16:
  `grep -n "counter += 1" crates/vendor/receiver/src/session.rs` → exactly one occurrence, inside the
  `if body_size >= 16` arm of the `0 =>` opcode match; the `else` arm returns `body.to_vec()` with no
  counter advance.

- **Resource arbitration is a one-shot `takeScreen` at RECORD and is never revisited.** There is no
  untake, no release and no re-negotiation, where both reference implementations model this as a
  stateful two-party negotiation.
  Source: `docs/carplay/05_METADATA_AND_CONTROLS.md` §II.2, `docs/carplay/02_SESSION_LIFECYCLE.md` §Summary item 2. Verified open 2026-08-16:
  `grep -rn "send_take_screen" crates ccpa` → one definition, one call site, no untake emitter.

- **`send_command` does not enforce Apple's `sessionStarted` gate.** Deliberate and documented, but a
  known deviation from `AirPlayReceiverSession.c`'s `require_action_quiet(inSession->sessionStarted, …)`
  that has never been tested either way — and one of the two surviving explanations for the
  `limitedUI` no-op.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §10 ("untested rather than refuted"). Verified open 2026-08-16:
  `grep -n "session_started" crates/vendor/receiver/src/events.rs` → checked only inside
  `modes_changed_tunnel_nudge()`; `pub fn send_command` has no check.

- **The `carplayd → :9001 → ocbmd` forward path is still local TCP loopback.** The copy elimination
  (unix socket / `splice`) that the 4K@60 track lists as "next" has not started.
  Source: `docs/host/00_MACOS_HOST_APP.md` §"4K@60 optimization track" item 4. Verified open 2026-08-16:
  `grep -n "TcpListener\|UnixListener" ccpa/ocbmd/src/main.rs` → all five A/V seams are
  `TcpListener::bind(("127.0.0.1", port))`; no `UnixListener`, no `splice`.

---

### CarPlay capability surface — the app-pushed config the box does not yet act on

> The single best machine-readable inventory of this workstream is the `EMITTED_BUT_UNREAD` constant
> asserted by `every_emitted_key_is_parsed_or_knowingly_ignored` in
> `crates/vendor/receiver/src/vehicle_config.rs`. It names every key both host apps emit that nothing
> on the box reads. Grep that first.

- **Seven `accessoryConfig` capability keys are still dropped by the box serde gate.**
  `enablesVideoPlayback`, `enablesEnhancedSiri`, `enablesUIContext`, `enablesUISync`,
  `enablesFileTransfer`, `enablesVehicleDataProtocol` and `enablesDCX` reach the box in YAML from
  both host apps and are silently discarded. Adding a field is a one-line formality — it must only
  land in the commit that makes the box act on it.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` TL;DR, [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-53-2`. Verified open 2026-08-16:
  `grep -n "serde(default, rename" crates/vendor/receiver/src/vehicle_config.rs` → `AccessoryConfig`
  declares exactly nine keys, and the same seven are listed under `EMITTED_BUT_UNREAD`.

- **`focusTransfer` is advertise-half-armed — no SETUP `enabledFeatures` echo.** The lever and the
  per-view `/info` flag landed, but no SETUP author pushes the `"focusTransfer"` token, so iOS can
  never negotiate it; runtime focus (`accessoryAcquireFocus` / `deviceOfferFocus` /
  `initialFocusOwner`) is entirely absent.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §1, `R-49-4`. Verified open 2026-08-16:
  `grep -n "feats.push" crates/vendor/receiver/src/session.rs` → eight tokens, none of them
  `focusTransfer`; the same six-token list appears in `VehicleConfig.swift` and `setup_driver.rs`.

- **`uiContext` is completely unimplemented on the box.** No serde field, no lever, no
  `uiContext*URLs` keys in `/info`, no echo, no `changeUIContext` emitter — only an app UI stub.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3, `docs/carplay/03_SDK_GROUND_TRUTH.md` §5. Verified open 2026-08-16:
  `grep -rn "uiContext\|changeUIContext" --include="*.rs" crates ccpa` → zero hits outside
  `vehicle_config.rs` test fixtures.

- **The RCS stream-130 client-type gate is still an allowlist of one.** LogTransfer, the two
  VehicleDataProtocol types and a CarPlayClusterControl SETUP are accepted as streams but their
  frames are logged and dropped. Expanding it once unblocks three capabilities below.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §"Shared infrastructure" 2. Verified open 2026-08-16:
  `grep -n "IAP_CLIENT_TYPE" crates/vendor/receiver/src/session.rs` → one UUID constant is the only
  accept condition, with a doc comment reading "The gate is an allowlist of one".

- **`mainBufferedAudio` Phase B (accept + forward the stream) is unbuilt.** Phase A advertise+echo
  landed and is wired-device-proven; SETUP phase 2 still falls through to the "NOT IMPLEMENTED —
  omitted" arm for MainBuffered/AuxIn/AuxOut, so no buffered audio can arrive. The true
  `mainBufferedInfo` shape and the MainBuffered stream-type number are still unknown, and the
  wireless arm was never captured.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §5, `docs/ops/04_OPEN_ITEMS.md` step 6. Verified open 2026-08-16:
  `grep -n "Still unimplemented and therefore omitted" -A3 crates/vendor/receiver/src/session.rs`.

- **`vehicleDataProtocol` / VDC route-sharing is entirely unbuilt.** The box advertises neither
  `vehicleStateProtocol` nor `vehicleStateProtocolInfo`, never echoes the token, and no
  VDCSchema-driven characteristic server exists.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §4 Mechanism A, `docs/host/00_MACOS_HOST_APP.md` Tier 4. Verified open 2026-08-16:
  `grep -rn "vehicleStateProtocol" crates ccpa --include="*.rs"` → no hits in box code.

- **`videoPlayback` (video while parked) is unbuilt, and the phone side is on.**
  No lever, no `/info` advertisement, no `lunaConfig`/`videoPresetSizes`, no echo. Corrected
  2026-11-10 from the Mac's lookups (handoff doc, "Mac findings, 10 Nov", section 5): it is NOT
  blocked on an entitlement for the box. `com.apple.developer.carplay-video` is what an iPhone app
  needs to offer video in CarPlay (`CRCarPlayAppDeclaration.supportsVideo`) [E, iOS CarKit]. iOS turns
  CarPlay feature bit 6 into the SETUP word `"videoPlayback"` [E, iOS CarKit], and CarPlaySDK 509.11
  doesn't check that word [E]. The wire shape comes from LIVI, an independent receiver that plays it:
  `/info` `videoPlaybackInfo` {`videoPlaybackAllowed`, `featuresEx`} and `playbackCapabilities`
  (macOS's AirPlayReceiver has the last two), `setVideoPlaybackAllowed`, a `changeModes` borrow with
  `borrowID` `VideoPlayback` (the Simulator's string too), and the phone's play-queue verbs on the
  Playback DataStream, client type `A6B27562-B43A-4F2D-B75F-82391E250194` (iOS:
  `kFigEndpointRemoteControlSessionClientTypeUUIDKey_MatchPoint`) [E for the UUID's name and the
  borrow ID; I for the CarPlay keys and for the stream on CarPlay, seen on macOS's receiver].
  **Benched 2026-11-10** (the Mac, dev adapter on `b84a58b`): at SETUP phase 1 Scott's iPhone on iOS
  27.2 proposes 14 words, CarKit's list in its order, with `"videoPlayback"` among them (no
  `fileTransfer` or `uiSync`: no gauge cluster), and the box echoes `hevc`, `viewAreas`, `iAPChannel`
  and `sessionManagement`. So the phone side is on. What iOS then wants from the head unit is still
  [I]; LIVI's `/info` keys, the `videoPlayback` echo and the Playback stream, behind a lever that is
  off by default, are the next things to try. Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §7.
  **The probe for that** is on branch `airplay-video-probe` (not on `main`; the Mac's 10 Nov brief),
  `crates/vendor/receiver/src/probe.rs`. Its two switches are off unless `/etc/ocbm/airplay_probe`
  names them, read when carplayd starts. `video` (route A) echoes `videoPlayback` when the phone
  proposes it, adds LIVI's two `/info` keys (`featuresEx` = `features` as bytes, low byte first,
  trailing zeros dropped, Base64: `gAtERGE=`; `bits` ORs in bits 0 and 49), sends
  `setVideoPlaybackAllowed` (true, `idle`) once at RECORD (`noallow` leaves it out), and logs the
  shape of every non-iAP DataStream message and of each request nothing handles. `receiver` (route C)
  advertises "Carlink Video", an ordinary `_airplay._tcp` service with the Apple TV TXT record from
  nto's and openairplay's public specifications, on port 7000; its listener logs each connection's
  first requests and answers 501. Each line is tagged `[probe]` and gives keys, types and sizes only.
  **Benched 2026-11-10** (the Mac, Scott's iPhone on iOS 27.2): route A works on the phone side. The
  phone opened SenderSettingsData, sent `setVideoPlaybackParameters` and `setProxyParameters`, put the
  Apple TV app in CarPlay's grid (its UI ran on the car screen), and YouTube listed "CarPlay" as an
  AirPlay receiver. A video opened the Playback stream (`A6B27562`, no dedicated socket), whose messages
  ride `POST /command` as `{params: {data}}` with no type; nothing answered it, so nothing played. Route
  C's "Carlink Video" was never listed. Probe 2 logs one level into those `data` bytes and a TEARDOWN's
  stream ids; the TEARDOWN that also stopped the iAP2 channel is fixed (a partial TEARDOWN now stops only
  the RCS channel it names).
  **Route A's relay** (branch `airplay-video`, on top of the probe; behind the same `video` switch):
  carplayd carries the phone's shared DataStreams to the host and back (`receiver/src/playback.rs`). A
  `POST /command` whose `X-Apple-StreamID` header names a stream goes up as a
  `{"kind":"dataStream","stream","client","message"}` record, its `params.data` plist as JSON. The host's
  answers come down as `INPUT_DATASTREAM` (`0x0A`) sub-frames, `[stream u32 LE][JSON]` (the one input
  sub-frame allowed past 64 B, up to 16 KiB), and go to the phone as a binary plist in `{params: {data}}`
  on the event channel, with the same header. Carlink keeps the play queue, answers each request, and
  plays the item, parked only. **Benched 2026-11-10** (the answers only, no player yet): the relay works
  both ways. Safari hands off Apple's unencrypted H.264 example stream, whose `Start-Position` is a
  CMTime. YouTube lists CarPlay but doesn't hand off: it sends `removePlayQueueItem` and tears the stream
  down. While the relay is on, the shared channel's SETUP logs one plain line instead of the NOT
  IMPLEMENTED warning. **The player, benched 2026-11-10** (Carlink `aa05810b`): Safari's stream plays
  full screen on the car, and Close and a drive pause work. FairPlay can't play on any route (the TV
  app, `play-edge.itunes.apple.com`: `ERROR_CODE_DRM_SCHEME_UNSUPPORTED`), and YouTube never hands off.
  `setVideoPlaybackAllowed` now follows the host (`CMD_VIDEO_ALLOWED`, the car known to be parked):
  nothing goes out at RECORD until the host has said, then each word with LIVI's status (`idle`,
  `video`, `audioOnly`), as LIVI sends it at RECORD, at play start and as parking changes.
  **The phone's network** (2026-11-11, unbenched): the phone's `setProxyParameters` reaches the host as
  every command does, and the host fetches the stream through that proxy, as LIVI does: TLS 1.3 with
  the phone's pre-shared key, then CONNECT. The box carries the bytes on `CH_TUNNEL` (`receiver::tunnel`,
  docs/carplay/01_OCBM_PROTOCOL.md), connecting only to the phone. FairPlay still can't play on any
  route; the proxy is for a unit without internet of its own, and for streams only the phone can reach.
  **The screen borrow** (2026-11-11): Carlink borrows the main screen while the phone's video shows
  (`CMD_VIDEO_SCREEN`; `changeModes` `{resourceID 1, transferType 3, transferPriority 500, borrowID
  "VideoPlayback"}`) and gives it back after (`transferType` 4). **Benched 2026-11-11** (the Mac): iOS
  answers the borrow `400 Bad Request` nine times of nine and accepts the give-back, and video plays the
  same without the borrow. Each command reply now names its command by CSeq, and a reply body's `status`
  is logged.
  **OCBM 2.0 (2026-11-12, Scott): route A is always on**, parked-gated by the host's
  `CMD_VIDEO_ALLOWED`. The probe's switches and `/etc/ocbm/airplay_probe` are gone (a file left on an
  adapter is ignored), and with them route C, `bits`, `noallow` and the screen borrow:
  `CMD_VIDEO_SCREEN` `0x14` is retired, never to be reused. `probe.rs` keeps route A's shapes and its
  `[probe]` log lines.

- **`fileTransfer` is blocked-untestable — even the advertise step is byte-unsafe.** The
  `fileTransferInfo` dict's keys are unknown, as is which transport carries the chunk dict.
  Unrelated to the iAP2 session-2 artwork assembler the box already implements.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §8. Verified open 2026-08-16:
  `grep -rn "fileTransferInfo" crates ccpa host` → zero hits outside docs.

- **`uiSync` / the CarPlay Cluster Control Channel is blocked, and the cheap UUID-harvest probe has
  not been run.** The recommended low-risk probe — advertise only, then read the box's own
  stream-130 SETUP log line for the unknown `clientTypeUUID` — turns the biggest unknown into a
  captured fact in one connection and is still untaken.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §9 Step A. Verified open 2026-08-16:
  `grep -rn "uiSync\|ClusterControl" --include="*.rs" crates ccpa` → only test fixtures.

- **DCX is investigation-only — no spatial/APAC pipeline exists anywhere in the tree.** The acronym,
  the `/info` placement and the exchange's wire shape are all open; the doc says write no code
  before the iOS-27 disassembly lands.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §10. Verified open 2026-08-16:
  `grep -rn "DCXEnabled\|APAC\|spatial" --include="*.rs" crates ccpa` → only test fixtures.

- **`nightMode` — RESOLVED 2026-09-02 by dropping it from the emitted YAML (owner decision).
  `rightHandDrive` — REOPENED then RESOLVED 2026-09-05: it IS a CarPlay `/info` key; implemented and
  DEVICE-VERIFIED the same session (2400x960 ultrawide, wireless — iOS moved the app rail, status bar
  and app-grid button to the right edge, owner-confirmed on screen).** The 2026-09-02 resolution below still stands for `nightMode`. For
  `rightHandDrive` the "no consumer" premise was right but the conclusion drawn from it — that CarPlay
  has no key for driver position — was wrong: Apple's licensed R14G17 source defines
  `kAirPlayKey_RightHandDrive "rightHandDrive"` as an **Info Message** boolean
  (`AppleCarPlay/Sources/AirPlayCommon.h:1103`, inserted into `/info` at `AirPlayReceiverServer.c:637-646`,
  Integration Guide line 385 beside `oemIconVisible`/`OSInfo`), and `docs/carplay/03_SDK_GROUND_TRUTH.md`
  §3 had it in the `/info` key list all along. So it was an unimplemented `/info` key, not a
  VehicleConfig key with no meaning. Landing 2026-09-05 (owner): box `/info rightHandDrive` emission
  (`info.rs` `DeviceConfig.right_hand_drive`, default `false`, emitted unconditionally) +
  `vehicle_config.rs` parse + app re-emit (derived from `driverPosition == right`) + fixture regen.
  **Open until a session confirms iOS changes the driver-focused layout**; nothing is device-proven.
  Full account: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §rightHandDrive.
  History of the 2026-09-02 resolution, unchanged: until then both were written by the app and silently dropped by the box: both
  sat on `EMITTED_BUT_UNREAD` with a comment conceding they "READ LIKE REAL SETTINGS and are worth a
  decision rather than an entry here" (they have since been REMOVED from that list —
  `vehicle_config.rs`, the comment at the end of `EMITTED_BUT_UNREAD`). Neither was ever parsed into a
  config field nor emitted in `/info`; the live night-mode path is the runtime `setNightMode` command,
  not the config key.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §4, `docs/carplay/03_SDK_GROUND_TRUTH.md` via `R-26-1`. Verified open 2026-08-16:
  `grep -n "nightMode\|rightHandDrive" crates/vendor/receiver/src/vehicle_config.rs crates/vendor/receiver/src/info.rs`
  → hits only in a test YAML fixture and in the `EMITTED_BUT_UNREAD` list; **zero hits in `info.rs`**,
  and neither is a serde field. **2026-09-02:** both keys were DROPPED from the app's emitted YAML
  (`vehicle_config.rs` `EMITTED_BUT_UNREAD` comment, verify_06 10, owner decision (c)) — the pushed
  document no longer carries either. **2026-09-04:** the app side changed shape again — both persist
  only as UserDefaults `vc.*` keys DERIVED (`rightHandDrive` ← `vc.driverPosition == right`,
  `nightMode` ← `vc.theme == dark`) from the neutral vehicle profile, written by `save()` purely so an
  app DOWNGRADE reads sane values, never read back as authoritative (`App/SettingsWindow.swift`
  `save()`; corrected 2026-09-04 — this entry said they were "emitted only so a downgrade reads sane
  values", but the YAML carries neither). The owner's values were consumed by Android Auto
  (`driver_position`, `night_mode` sensor), retired in 1.0.5. For `nightMode` the box-side item is therefore closed
  rather than open: there is no key on the wire for the box to drop. (This entry used to end "no
  `/info` key for [driver side] is known to this project; `FeatureMatrix` records CarPlay driver
  position as unsupported" — refuted 2026-09-05, see the head of this item. `FeatureMatrix.swift`'s
  `(.driverPosition, .carPlay)` arm still says "The box has no consumer for driver side; nothing is
  pushed" and must change with the implementation.)

- **`altDisplayPanels[]` is parsed but never emitted; `showsInstruments` and `initialURL` are
  hardcoded box constants on a legacy `displays[]` entry.** This is a doctrine gap (box-decided where
  the app authors the value). **Caveat, do not lose it:** the code's own comment records docs/carplay/03_SDK_GROUND_TRUTH.md §5's
  "root cause" framing as REFUTED — cluster content already works via `showUI` query parameters — so
  anyone reviving the emission must first establish what it adds.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §Roadmap item 4, `docs/carplay/03_SDK_GROUND_TRUTH.md` §5, `R-26-2`, `R-49-2`. Verified open 2026-08-16:
  `grep -rn "alt_display_panels\|displayPanels" crates/vendor/receiver/src/{vehicle_config,info}.rs`
  → parsed with the comment "PARSED since 2026-08-10; still not emitted"; `info.rs` has no
  `displayPanels` key and a test named `alt_display_panels_are_parse_only_today`.

- **Second main view area is APP-DRIVEN (2026-09-05, later the same day); the bench lever is the
  app-less fallback.** `vehicle_config.rs::apply` now carries `mainVideoStream.viewAreas[1].viewArea`
  (+ our `initial` extension key) into `DeviceConfig::main_view_area_2`, `info.rs::view_area_2`
  prefers it over `CARPLAY_VIEWAREA2` / `/tmp/carplay_viewarea2` and refuses an uncontained rect with
  the same `***` log the lever gets (a refused config area declares ONE area — it does not fall
  through to stale lever state), and `view_areas_enabled()` auto-arms `viewAreas` for a declarable
  second area the way a real inset does. Host side: `VehicleConfigModel.viewArea2{Enabled,X,Y,W,H}`,
  validated by `ViewArea2Rule` in severity order (containment → all four values EVEN → positive dims,
  each a teardown → the PRODUCT floor 800x480 landscape / 480x800 portrait, a lockout) and emitted
  only when enabled AND legal, so the OFF document
  is byte-identical (fixture guard). `events::request_view_area` answers the Dock button with
  `updateViewArea` — device-proven with `1600x960@800,0` on 2400x960 and `600x400@240,760` /
  `1080x1600@0,160` on 1080x1920 (`docs/carplay/06_AV_PIPELINE.md` §3, "Bench lever, 2026-09-05").
  Still open: the Settings VIEW for the five keys (model half landed, view half in flight),
  `initial` authoring from the app (box parses it; the model does not emit it), `updateDisplayPanels`
  live re-negotiation, and the type-112 second cluster.
  Source: `docs/carplay/06_AV_PIPELINE.md` §2–§4, `docs/carplay/03_SDK_GROUND_TRUTH.md` §10, `docs/carplay/02_SESSION_LIFECYCLE.md` §"Apple's session/lifecycle model".

- **`displays[].features` is a hardcoded `if levers::dpad() { 0x1A } else { 0x0A }`, and four parsed
  `hidConfig` keys drive nothing** (`touchpadSupport`, `steeringWheelSupport`, `mediaButtonsSupport`,
  `touchScreenMode`). `0x02` claims Knobs and `0x10` claims Touchpad with no backing `hidDevices[]`
  entry, while `dPadSupport` — the only input the emission consults — contributes nothing to that
  word in Apple's own mapping. The current value is hardware-validated, so correcting it is a wire
  change needing a session. Tracked as workstream C-7/C-8.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §8 item 5, `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3. Verified open 2026-08-16:
  `grep -n "disp_features" crates/vendor/receiver/src/info.rs`;
  `grep -rn "touchpad_support\|steering_wheel_support\|media_buttons_support\|touch_screen_mode" --include="*.rs" crates ccpa`
  → every hit is a struct field or a test.

- **HID uids are hardcoded 1–5; Apple's runtime allocator is not replicated.** The descriptors
  themselves are done and byte-verified, but `steeringWheelID` consumes an ID while emitting no
  device, and the uid-4/uid-5 entries still await hardware validation.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §8 item 4 and §2 trap 1. Verified open 2026-08-16:
  `grep -n "HID_UID" crates/vendor/receiver/src/info.rs` → five `const` literals, no counter and no
  allocator.

- **Video lanes are fixed and capped at two.** `altVideoStreams[]` parses as an array but only its
  first entry is used, and the video seam's key message carries an `scid` the host discards; a third
  stream would need new channel ids or scid tagging. Audio has no such limit.
  Source: `docs/carplay/01_OCBM_PROTOCOL.md` §"What is genuinely not supported (open item)". Verified open 2026-08-16:
  `grep -n "CH_VIDEO\|CH_ALT_VIDEO" crates/ocbm-proto/src/lib.rs` → exactly two video channels.

- **Accessory-side NMEA/GPS: built, off by default, device-proven on the dev adapter (2026-11-10, LIVI
  item 4).** With the app's `location:` pushed, the AirPlayTunnel Identify declares it and
  `iap_tunnel::send_location` sends `LocationInformation` (0xFFFB) from the app's CH_INPUT `INPUT_LOCATION`
  sentences after the phone's Start (`crates/vendor/iap2-core/src/location.rs`). iOS 15 and 27.2 accepted the
  tunnel Identify and selected GGA and RMC; Maps followed a plausible fix. Open: the truck (a garage or a
  tunnel); PASCD/GPGSV/GPHDT; the wired arm. Until 2026-11-10 it was parse-only:
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4. Verified open 2026-08-16:
  `grep -rn "0xFFFB\|MSG_LOCATION_INFORMATION" --include="*.rs" crates ccpa` → the spec table and
  `crates/vendor/metadata/src/location.rs` (a parser); no emitter.

- **App-owned CarPlay pairing peer store.** Today `carplayd`'s `DiskPeers`
  (`ccpa/carplayd/src/main.rs` ~:342-439) persists `id → Ed25519 LTPK` on box flash, and
  `PeerSaver::save_peer` (`crates/vendor/pairing/src/setup.rs` ~:29-30) is infallible because
  pairing is already cryptographically complete when it runs. Owner decision (2026-09-02): the
  host app owns the store, per app install (macOS `UserDefaults`-class storage; Android app
  storage), cleared on app uninstall; the box keeps peers in memory only — no box-side disk
  fallback, so an app-less bench boot re-pairs; the app supports removing a peer, and the box
  refuses pair-verify for any peer not in the last pushed list. Mechanism: a `peers:` list in the
  `CT_SUBSCRIBE` YAML the app already re-pushes on every SUBSCRIBE (`crates/ocbm-proto/src/lib.rs`
  `CT_SUBSCRIBE`), staged by `ocbmd` next to the existing config file and read by carplayd's
  `load_device_config`; a new box→host `SEV_PEER_ADDED{id, ltpk}` on `CT_SESSION_EVENT` when
  pair-setup completes; `DiskPeers` → `MemPeers` (trait shape unchanged). Touch list: `ocbm-proto`,
  `ocbmd`, `carplayd`, `host/MacHost` (VehicleConfig/Settings push + event handling),
  `host/CarlinkAndroid`. Status: planned, not started.

---

### iAP2 & metadata

> **Also see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) Phase 4**
> for the two catalogued `iap2-core/src/message.rs` defects (the params-6/7 "closed dead end as a
> class" self-contradiction, and the hardcoded Identify serial).

- **Identify param 30 declares only subs 0 and 1, so `lane_guidance` is inert and road/destination
  names cannot populate.** Apple's sub 8 `MaxLaneGuidanceStorageCapacity` reads "Must be included to
  receive Lane Guidance instructions", and subs 2/3 gate `CurrentRoadName`/`DestinationName`.
  **⚠️ CONTRADICTED BY OUR OWN CAPTURE — see [False claims](#false-claims-found-while-verifying).**
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §8 item 1. Verified open 2026-08-16:
  `grep -n "route_guidance as sub" -A8 crates/vendor/iap2-core/src/message.rs` → the emitter pushes
  only `sub::IDENTIFIER` and `sub::NAME`.

- **Param 30 is still UNGATED across all three transports, which blocks its expansion.** Growing it
  today also grows the byte-pinned BT-time `Wireless` Identify; the doc states gating to
  `AirPlayTunnel` first is "a prerequisite, not an option".
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §8 item 2. Verified open 2026-08-16:
  `grep -n "ROUTE_GUIDANCE_DISPLAY_COMPONENT" crates/vendor/iap2-core/src/message.rs` → the emitter
  `push(…)` sits in a bare block with no `matches!(transport, …)` guard; the gate exists only as a
  commented-out `ROLLBACK` suggestion above it.

- **C-3 — RESOLVED 2026-09-01: `iap2d` now passes the app-pushed vehicle identity on the wire.**
  `ccpa/iap2d/src/main.rs` calls `message::build_ident_info_with` (`Action::SendIdentify`) and
  `build_ident_info_excluding_with` (`Action::RetryIdentify`) with the app-pushed
  `vehicle_identity()` (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C-3). `iap2d` still passes
  `message::accessory_name("CarLink")` at both Identify sites — that half of the original claim
  stands; only the tunnel and BT drivers still pass the baseline identity.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3 row C, `../ops/04_OPEN_ITEMS.md` §4. Verified open 2026-09-01:
  `grep -rn "build_ident_info_with" crates ccpa` → hits both `ccpa/iap2d/src/main.rs` call sites
  plus a `#[test]` in `config.rs`; `grep -n "accessory_name" ccpa/iap2d/src/main.rs` → two
  hardcoded call sites, unchanged.

- **C-4 — `features.rs` structurally cannot express vehicle status, which blocks C-5 (param 21).**
  `0xA101` is accessory-sourced while `0xA100`/`0xA102` are device-sourced, inverted versus every
  existing feature, so a naive `Trigger::Subscription` trips the direction-correctness test. The code
  carries a hard DO-NOT-ENABLE warning, and a `0x1D03` is unrecoverable within a session.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §4 Mechanism B, `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3. Verified open 2026-08-16:
  `grep -rn "0xA100\|0xA101\|0xA102" crates/vendor/iap2-core/src/features.rs` → zero hits.

- **What flips `CarPlayAppListAvailable` from 0 Unknown to 2 Accepted is still UNDETERMINED, and the
  proposed Simulator experiment was never run.** It needs no box work. Related known difference,
  recorded not resolved: we omit sub 1 `CarPlayAppListMax` where Apple's Simulator always emits it.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §6, corroborated by `docs/ops/05_AUDITS.md` §2 ("`0xAD01` remains uncaptured"). Verified open
  2026-08-16: `grep -n "CarPlayAppListAvailable" crates/vendor/iap2-core/src/features.rs` → still
  reads "remains UNDETERMINED by every normative source"; no matching capture in `docs/ops/captures/`.

- **Nine `communications` (0x4158) capability keys are emitted by the box and read by nothing.**
  `cellularSupported`, `faceTimeAudioEnabled`, `initiateCallAvailable`, `endAndAcceptAvailable`,
  `holdAndAcceptAvailable`, `swapAvailable`, `mergeAvailable`, `holdAvailable`,
  `faceTimeVideoEnabled`. The raw-record fallback renders the payload, so this is a pane-mapping gap,
  not data loss.
  Source: `docs/ops/05_AUDITS.md` §1 ("Live gap"). Verified open 2026-08-16:
  `grep -rn "cellularSupported\|faceTimeAudioEnabled\|initiateCallAvailable" host/MacHost/` →
  zero hits; the same grep over `--include="*.rs"` hits `iap2-core/src/metadata.rs`.

- **W9 — the accessory BT MAC in the byte-pinned Identify is a hardcoded constant.** Param 17 is
  fleet-identical rather than read from the box's own `hci0`. Reclassified to its own pinned-Identify
  hardware session with a ready revert; that session never ran.
  Source: `docs/ops/05_AUDITS.md` §"Where the agents corrected the audit" #1, §"Hardware session 2". Verified open
  2026-08-16: `grep -rn "ACCESSORY_BT_MAC" crates/vendor/wireless/src` → a fixed 6-byte literal in
  `bt_driver.rs` used at both Identify sites.

- **`0x4E0D WirelessCarPlayUpdate` and `0x5700`–`0x5703` are declared but not acted on.**
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §10. Verified open 2026-08-16:
  `grep -rn "0x4E0D" --include="*.rs" crates ccpa` → every hit is a spec-table entry or a declaration
  list; no handler.

---

### Wireless transport — the RCS DataStream and the BT→WiFi path

- **An inbound RCS `'sync'` obliges a `'rply'`; the box logs and does not send one — BLOCKED, not
  merely unstarted.** `_controlServer_sendResponseInternal` writes `messageType = 0` while the
  phone's only RCS inbound dispatch accepts `'cmnd'`/`'died'`; both cannot be true, and a malformed
  accessory frame has already wedged this link once, so guessing wrong is asymmetrically expensive.
  Source: `docs/carplay/05_METADATA_AND_CONTROLS.md` §5 item 1, `../ops/04_OPEN_ITEMS.md` §5 (#15b). Verified open
  2026-08-16: `grep -n "sync" crates/vendor/receiver/src/session.rs` → logs
  `inbound 'sync' … expects a 'rply' — NOT IMPLEMENTED`; `KIND_RPLY` has no non-test consumer.

- **The tunnel SYN declares 2 link sessions at session version 1 where Apple's transport-type-2
  template declares 3 at version 2, and post-`Identified` frames bypass `state::on_message`.** Both
  were deferred so the first hardware test had one variable; that test has since run. A re-`0x1D00`,
  re-`0xAA00` or `0xAA04` after Identify falls through to the generic dispatcher.
  Source: `docs/carplay/05_METADATA_AND_CONTROLS.md` §5 items 2–4. Verified open 2026-08-16:
  `grep -n "SYN_PARAMS_ZERO_ACK_TUNNEL" -A2 crates/vendor/iap2-core/src/link.rs` → leading version
  byte 1 and two 3-byte session blocks.

- **The event capture still watches the `POST /command` carrier that docs/carplay/05_METADATA_AND_CONTROLS.md refuted.**
  `/tmp/carplay_event_capture.bin` is written from the event-channel plist path, a channel now proven
  not to carry wireless iAP2 (that rides the RCS DataStream, stream type 130).
  Source: `docs/ops/05_AUDITS.md` §8 "Still open". Verified open 2026-08-16:
  `grep -rn "carplay_event_capture" --include="*.rs" crates` → one hit, inside
  `events.rs::handle_inbound_event`'s RTSP-body path; no equivalent in `datastream.rs`.

- **`/tmp/carplay_transport` is still claimed at `ensure_av_layer` entry rather than in the
  `carplayd_up` success branch.** The flag is written before the spawn is confirmed, with a rollback
  on failure instead.
  Source: `docs/ops/05_AUDITS.md` §"2026-08-01" Finding B. Verified open 2026-08-16:
  `grep -n "carplay_transport" crates/vendor/wireless/src/av.rs` → written at the top of
  `ensure_av_layer`, rolled back only in the `else` of `if carplayd_up`.

- **An absent `clientTypeUUID` is treated as iAP, diverging from Apple's teardown path.** Apple's
  `_DataStreamSessionSetup` sends an absent UUID to `-6735` teardown identically to an unrecognised
  one. Knowingly kept as permissive-but-harmless; listed only so the index is complete.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §3. Verified open 2026-08-16:
  `grep -n "clientTypeUUID" crates/vendor/receiver/src/session.rs` →
  `let is_iap = client_type.is_empty() || client_type.eq_ignore_ascii_case(IAP_CLIENT_TYPE);`.

- **`iAPChannel` and `sessionManagement` were never migrated to per-transport app config.** Both are
  still env-gated box-only SETUP tokens with no config key, and the host's phase-1 authoring must
  UNION them back in from the box's own response or silently kill the wireless iAP2 tunnel.
  Source: `docs/ops/04_OPEN_ITEMS.md` §Building blocks step 8 (workstream C/D debt). Verified open 2026-08-16:
  `grep -n "hostAuthorableFeatures" -A4 host/MacHost/carlink_macOS/OCBM/AirPlaySetupSession.swift`
  → six tokens, neither of the two present.

- **Pair-RESUME is absent.** Only pair-setup and pair-verify exist; there is no persisted-session-id
  resume and no resume→verify fallback, so a known device always pays the full pair-verify round trip.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Apple's session/lifecycle model", `R-11-1`. Verified open 2026-08-16:
  `grep -rni "pair-resume\|pair_resume\|pairResume" --include="*.rs" --include="*.swift" --include="*.kt" .`
  → zero hits.

- **The Siri and TEARDOWN fixes from the docs/carplay/03_SDK_GROUND_TRUTH.md pass have never been individually exercised on
  hardware.** Structurally verified only; the third member of that trio (cluster content) has since
  been hardware-confirmed. Given `R-49-7` — that same pass silently killed wireless metadata for ten
  days because whole-session health was read from A/V alone — the remainder deserves its own session.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §10, `R-49-7`. Verified open 2026-08-16:
  `ls crates/vendor/receiver/tests/` → `control.rs net.rs r4_c2_schema.rs setup_stream_130.rs`; no
  Siri or TEARDOWN test and no capture named for either.

---

### Radios, box identity & bring-up

- **Broadcom and NXP mappings are refused, not resolved — those units report `unsupported` and get no
  wireless.** Closing it means slicing the dispatcher to the unit's own SDIO branch and resolving
  that branch's literal `var=value` assignments. Emitting the raw text instead is the documented
  silent-false-success failure (docs/wireless/01_BT_AND_RADIO.md §6d).
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -n "safe_cmd" -A8 ccpa/rootfs/script/radio_detect.sh` → the refusal is still
  `case "$_c" in *'$'*|*'`'*|…) echo "" ;;`, which blanks any vendor line carrying an unexpanded
  variable; no closed-form Broadcom/NXP mapping has been added beside it.

- **Broadcom is entirely untested, and RTL8733BS / SD8987 are unexercised.** The seam resolves them
  on paper from their own dispatchers — that is the whole design claim — but the claim is measured
  only on 0xc822. On Broadcom, `wlan0` does not exist until explicitly created on top of `sta0`, and
  no Broadcom hardware is available to this project.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7, and §5's hardware-validated results (RTL8822CS only). Verified open
  2026-08-16: `grep -n "c822" ccpa/rootfs/script/radio_detect.sh` → `0xc822) CHIP=realtek_rtl8822cs`
  is the id §5 validated; this is a hardware-coverage gap, so no grep can close it.

- **`RADIO_BT_PRELOAD_CMD` is consumed by `radio_hal.sh` but never emitted by `radio_detect.sh`.** A
  mapped-path unit needing `fw_loader_linux` before BT attach would silently never run it. Moot while
  IW416 resolves to the `owned` backend, latent otherwise.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -rn "RADIO_BT_PRELOAD_CMD" ccpa/ tools/ pi/` → three hits, all in `radio_hal.sh`; zero in
  `radio_detect.sh`.

- **"One box, one name" is not achieved — the Rust side never reads `/etc/carplay_ident` and still
  hardcodes `wlan0`.** `btd`, `bt_on.sh` and `ocbmd`'s `bt_name_from()` each derive a
  name independently, and because the supervisor execs `btd` after `radio_hal.sh bt_on`
  the controller advertises `CarLink-<suffix>` rather than the seam's `ccpa-<4hex>`. The suffixes can
  diverge too, not just the prefixes.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §6c/§7, `R-57-1`. Verified open 2026-08-16:
  `grep -rn "carplay_ident" ccpa crates tools pi host` → four hits, all in
  `radio_hal.sh`/`radio_ap_up.sh`, none in Rust; `grep -n "wlan0" crates/vendor/wireless/src/av.rs crates/vendor/wireless/src/box_identity.rs crates/vendor/iap2-core/src/message.rs ccpa/ocbmd/src/main.rs`
  → `const WLAN_IFACE: &str = "wlan0"` plus three `/sys/class/net/wlan0/address` reads.

- ~~**The hardcoded Raspberry Pi BT MAC in `bt_driver.rs` is unparameterised.**~~ **CLOSED** — the
  literal is gone: `bt_driver.rs` reads the controller's address and falls back to a
  locally-administered `PLACEHOLDER_BT_MAC` only when it cannot, and the Pi port is retired (1.0.5).

- **There is no reversible radio verification GATE in the baseline, run while still on NCM.** The
  baseline installs the seam and *reports* the resolved platform, but nothing gates the install on a
  pass/fail radio result or reverts on failure.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -n "radio_detect" tools/ncm_base_install.sh` → the only invocation pipes
  `sh /script/radio_detect.sh` into a `grep -E "CHIP|SDIO_DEVICE|…"` summary print, with no exit-code
  test and no revert path.

- **`ocbm_install.sh` has no generic manifest cross-reference assertion.** The specific Realtek
  failure it would have caught is closed (`--full` now ships the seam alongside the supervisor), but a
  future shipped script referencing a path the target lacks would still slip through.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -n "assert" tools/ocbm_install.sh` → one hit, a comment about the inittab; `manifest()` is a
  flat `local|remote|mode` list consumed by `push_file` with no cross-reference check.

- **The startup decode transient described in `docs/wireless/01_BT_AND_RADIO.md` §6b is unexplained and unfixed.**
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16: no code carries a fix or a tracking marker —
  `grep -rn "decode transient\|startup transient" crates ccpa` → zero hits.

---

### macOS host app

The macOS host app (`host/MacHost`) left the tree in OCBM 1.0.5; its items are at tag
[`v1.0.4`](https://github.com/enigy/ocbm/blob/v1.0.4/docs/ops/04_OPEN_ITEMS.md). The host app is Carlink.

### Tests, tooling & install

> **Also see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) Phase 4**
> item 3 for `ocbm_install.sh --full` not installing `iap_role_switch`, and item 10 for the
> documentation coverage gap.

- **The Swift config emitter has zero automated coverage.** `tests/run_tests.sh` compiles
  `VehicleConfig.swift` but not `SettingsWindow.swift`, where the emitter actually lives — and
  app/box schema drift is the repeated failure mode on this workstream. docs/carplay/04_CAPABILITIES_AND_CONFIG.md calls this the
  highest-value open item on workstream C. A built, mutation-verified proof of concept is preserved
  in `scratchpad/`, but its golden string is stale.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3 row C. Verified open 2026-08-16:
  `grep -n "VehicleConfig.swift\|SettingsWindow" host/MacHost/tests/run_tests.sh` → one hit;
  `grep -n "iapConfigYAML" host/MacHost/carlink_macOS/App/SettingsWindow.swift` → the emitter is
  in the uncompiled file.

- **`EMITTED_KEYS` is an allowlist, not a contract — 36 keys pinned against 76 the host reads.**
  Renaming any of the 40 unpinned keys blanks a pane with the suite green.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16: the `const EMITTED_KEYS` table in
  `crates/vendor/iap2-core/src/metadata.rs` has **36** `("kind", "key")` rows, against
  `grep -oE '(str|int|dbl|num|bool)\("[a-zA-Z]+"\)' host/MacHost/carlink_macOS/App/MetadataWindow.swift | grep -oE '"[a-zA-Z]+"' | sort -u | wc -l`
  → **76**.

- **`host_app_reads_every_emitted_key` still scans the whole Swift file unscoped.** The key half of
  the assertion is a bare whole-file `contains("\"key\"")`, so an unrelated literal anywhere in
  `MetadataWindow.swift` satisfies it.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16:
  `grep -rn "host_app_reads_every_emitted_key" --include="*.rs" .` → the body is
  `if !src.contains(&format!("\"{key}\""))` against the entire file.

- **`skip_list_narrows_the_declaration` is still tautological, and `file_setting()` has no test.** The
  skip lever itself now has genuine coverage via `active_with_skip`, but the named test is unchanged
  and the `/tmp/carplay_metadata` parser has one caller and zero tests.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16:
  `grep -rn "skip_list_narrows_the_declaration\|file_setting" --include="*.rs" crates` → the test is
  still an `Iterator::filter` assertion; `file_setting` has a definition and one caller, no test.

- **`stream.rs` key derivation is unpinned — no known-answer test for the HKDF salt, info labels or
  nonce layout.** Encrypt/decrypt are exact inverses tested only for determinism, so a typo in
  `"DataStream-Salt"` passes the whole suite and kills every session.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16:
  `grep -rn "salt" crates/vendor/receiver/src/stream.rs crates/vendor/receiver/tests/*.rs` → the
  literals appear only in `derive_stream_keys`, never in an assertion.

- **The cross-language ChaCha20 known-answer test was never written.** The other three legs of the
  planned OCBM suite shipped; the KAT that would pin the Swift decrypt against the Rust encrypt
  exists on neither side.
  Source: `docs/ops/05_AUDITS.md` §"Desk-side prep". Verified open 2026-08-16:
  `grep -rn -i "known_answer\|chacha" host/MacHost/tests/main.swift` → no matches.

- **`ocbmd` is still built at `opt-level = "z"`; the "measure first" decision was never taken.**
  `chacha20`, `poly1305`, `chacha20poly1305`, `ocbm-proto` and `receiver` are speed-tuned; the daemon
  that runs the poll loop and the per-frame forward is not.
  Source: `docs/ops/05_AUDITS.md` §"Also deferred", `docs/wireless/01_BT_AND_RADIO.md` §3. Verified open 2026-08-16:
  `grep -n "profile.release.package" Cargo.toml` → five overrides, none for `ocbmd`.

- **No swiftlint config.** The optional config from the structural batch was never created.
  Source: `docs/ops/05_AUDITS.md` §"Structural". Verified open 2026-08-16:
  `find . -name '.swiftlint.yml' -o -name 'swiftlint.yml'` → no matches.

- **The `/info` diff of a live Simulator session against ours on the same phone has never been
  performed.** docs/carplay/03_SDK_GROUND_TRUTH.md §11 names it as the productive move after concluding no further accessory-side
  RE explains why iOS ignores well-formed commands. **Read with `R-49-1`:** the diff as an
  *investigation* survives; "then tune it box-side" does not — `/info` content is app-authored.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §11, `R-49-1`. Verified open 2026-08-16:
  `grep -rn "diff a live Simulator" docs/*.md` → only docs/carplay/03_SDK_GROUND_TRUTH.md's own lines; no follow-up doc or capture.

- **`CARPLAY_EVENTS_LOG` is set by no spawn site, so docs/ops/02_TESTING.md's byte-level discriminator greps read 0
  on a healthy box.** Reviving them means setting it in the `carplayd` spawn env first.
  Source: `docs/ops/02_TESTING.md` §"The discriminator", `R-46-1`. Verified open 2026-08-16:
  `grep -rn "CARPLAY_EVENTS_LOG" --include="*.rs" --include="*.sh" . | grep -v docs/` → four hits,
  all readers; `grep -n "carplayd" tools/session_supervisor.sh` → the spawn env is
  `OCBM_FWD_ENC=1 $CM $LT $MB`.

- **`tools/ocbm_boot.sh` is 76 lines behind `ccpa/rootfs/script/ocbm_boot.sh`.** The rootfs copy
  carries the first-boot dead-man and the NCM failover watchdog; the `tools/` copy has neither. Not a
  deployment hazard today — the installer places the rootfs copy — but the stale duplicate remains.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Supporting layers" ("Reconcile toward the rootfs copy, never away from it").
  Verified open 2026-08-16: `wc -l ccpa/rootfs/script/ocbm_boot.sh tools/ocbm_boot.sh` → 110 vs 34.

- **`carplay-status.sh` — the reader for the `/tmp/carplay_state` verdict — is not installed by the
  current installer.** The supervisor writes the verdict and tells you to "Read it with
  carplay-status", but only the older `install_fhs.sh` subset places the script, so a box built the
  current way has the state file and no reader.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Supporting layers" (Observability). Verified open 2026-08-16:
  `grep -rn "carplay-status" tools/*.sh` → `install_fhs.sh` and the supervisor's own message;
  `ocbm_install.sh`'s manifest does not list it.

- **The box network surface is unhardened — weak default PSK, unauthenticated `telnetd`, bare
  dropbear, no AP isolation, no iptables.** `radio_ap_up.sh` refuses to BLANK the passphrase but only
  writes one when the existing key is absent or under 8 chars, so an existing `12345678` is never
  replaced. **This is a deliberate development posture** (telnetd is the documented rollback channel)
  and therefore an owner decision, not a defect — recorded so it is a decision and not an oversight.
  **Partly closed 2026-10-21 (branch `security-fixes`):** `ocbm_boot.sh` stops telnetd and dropbear
  in OCBM mode (the NCM modes keep them as the rescue channel), and `radio_ap_up.sh` treats
  `12345678` and the old MAC-derived fallback as missing and writes a random passphrase once (the
  phone gets it in the 0x5703 handoff). Still open: `ncm_wifi` runs the AP alongside unbound
  telnetd/dropbear, and the stock dropbear host keys are shared across units.
  Source: `docs/ops/05_AUDITS.md` §7 ("Outside the review's scope, for the owner"). Verified open 2026-08-16:
  `grep -n "wpa_passphrase" ccpa/rootfs/script/radio_ap_up.sh` → the guard is `[ "${#PSK}" -lt 8 ]`;
  `cat ccpa/rootfs/etc/init.d/rcS` → `busybox telnetd -l /bin/sh -p 23 &`;
  `grep -rn "ap_isolate" ccpa/` → no matches.

---

### Closed by decision — do NOT re-plan these

Recorded so a future session does not mistake an owner decision for unfinished work.

- **LogTransfer Tier-2 (serving the archive) will NOT be implemented** — owner directive 2026-08-07,
  no upside. Tier-1 advertise/negotiate is landed and device-proven. The byte-level unknowns
  (numeric `messageType`/`payloadType`, checksum algorithm and width, chunk-dict serialization,
  default/max `TransferChunkSize`) are recorded in `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §2 should it ever be revisited.
- **Enhanced Siri is OUT OF SCOPE** — owner directive 2026-08-07; it needs a full hot-word and
  voice-analysis stack outside an A/V adapter's remit. The box stays advertise-inert and the app
  tooltip already says so (`docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §6). Button-Siri works; the always-listening path does not exist.
- **The docs/carplay/03_SDK_GROUND_TRUTH.md §8 item 3 task "Emit `displayPanels[]` — the alt-content root cause" is REFUTED as a
  causal claim** (`R-49-2`, owner-confirmed on hardware 2026-08-11): cluster content works and its
  elements are toggleable via `showUI` query parameters. The *emission* remains unimplemented and is
  listed above under the capability surface — but as a schema-completeness item, not as the cluster
  fix docs/carplay/03_SDK_GROUND_TRUTH.md §8 still presents it as.
- **Phase 1 of [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) is
  closed in full** by owner decision (device identifiers and the vendor firmware backups). Nothing
  from it belongs in this index.
- **The `MGMT_ENTER_NCM` second-reboot (2026-09-03) is CLOSED BY OWNER DECISION — not to be
  investigated.** A second, unexplained reboot ~60 s into the resulting NCM boot happened once in
  three cycles that hour and `/tmp` did not survive to record a cause; kept for the record only.
- **Numeric-comparison SSP: box-initiated interactive answer is NOT the default, by owner decision
  after measurement (2026-09-04).** iOS refuses to hold a box-initiated numeric-comparison request
  open for a human to answer on the accessory (`pairingComplete result:162` instantly). The box
  confirms its own side immediately by default; the interactive wait lives on only behind
  `BT_SSP_INTERACTIVE=1` / `pairing: numeric_comparison_interactive` as a bench lever. Full
  measurement: [`../wireless/01_BT_AND_RADIO.md`](../wireless/01_BT_AND_RADIO.md).

---

### Found closed — documents still call these open

Every entry here was listed as open, planned or deferred by a source document (or by a status banner)
and is shipped in the current tree. Listing them is the point: a stale "open" is exactly the false
claim this index exists to remove.

**From this file's own tracking (previously listed above under "Box daemons"):**

- **Pairing-hold latch is DEPLOYED, not staged.** `rfcomm_uspace::pairing_aware_connect` (now
  `crates/bt-common/src/rfcomm_uspace.rs`) is on the box and measured against iOS 27
  (2026-09-03/04): [`../wireless/01_BT_AND_RADIO.md`](../wireless/01_BT_AND_RADIO.md) §"Connect hold".

**From `docs/ops/05_AUDITS.md` §DEFERRED and §"DEAD CODE inventory":**

- **MicCapture re-wired, not retired.** `AppDelegate` instantiates it and pipes `onPCMData` to
  `client.sendMicPCM`, started and stopped from `onUplinkGate`.
  Verified 2026-08-16: `grep -rn "MicCapture\|onPCMData" host/MacHost/carlink_macOS`.
- **NowPlayingManager and CallManager retired by deletion** (which also closes M-h).
  Verified: `find host -name 'NowPlayingManager*' -o -name 'CallManager*'` → no matches.
- **The legacy `AdapterProtocol` subsystem is gone**, with `TouchAction`/`CommandID`/the LE helpers
  extracted to `InputTypes.swift` exactly as docs/ops/05_AUDITS.md §Structural planned.
  Verified: `find host -type d -name Protocol` → nothing.
- **`ProtocolSessionRecorder` closed by deletion**, not by arming.
  Verified: `find host -name 'ProtocolSessionRecorder*'` → no matches.
- **The staged box deploy of the video fix happened** and was hardware-validated 2026-08-01.
  Verified: `grep -n "struct OutQueue\|out_video" ccpa/ocbmd/src/main.rs`.
- **M-k stereo PCM uplink RTP timestamp fixed** — the advance is per-channel sample-frames on both
  codec arms. Verified: `grep -n "SAMPLE-FRAMES\|per_ch" crates/vendor/receiver/src/uplink.rs`.
- **Three ocbmd LOWs fixed** — blocking `forward_input`, the 30 s `CT_SRC` srcbench, and the CH_IP
  `conns` leak across sessions. Verified: `grep -n "fn forward_input" -A20 ccpa/ocbmd/src/main.rs`
  and the `conns.clear()` calls on both teardown paths.
- **`net.rs` no longer tears down after one pre-A/V read timeout** — the `WouldBlock` arm consults an
  A/V-activity backstop. Verified:
  `grep -n "av_idle_ms\|AV_IDLE_TEARDOWN_MS" crates/vendor/receiver/src/net.rs`.
- **`AltVideoStream.max_fps` and `HidConfig.knob_support` are applied, not dropped**, and the dead
  `MBTN_*`/`ACODEC_*`/`ATYPE_*`/`sevHostGone`/`videoCounter`/`etaEpoch` symbols are swept.
  Verified: `grep -n "alt_max_fps" crates/vendor/receiver/src/vehicle_config.rs`;
  `grep -rn "MBTN_\|ACODEC_\|ATYPE_" crates/ocbm-proto/src/lib.rs` → none.
- **Communications 0x4158 and List 0x4171 are no longer structurally unreachable** — both are
  Extended-tier features in the generated table and `message.rs` unions `RCV_MSG_IDS` with
  `features::received_ids`. Verified: `grep -n "communications\|call_history" crates/vendor/iap2-core/src/features.rs`.

**From `docs/ops/05_AUDITS.md` (its Phase-2 banner is the source of most of these):**

- **eld-codec `AACENC_SBR_MODE=0` landed** — and the root cause was broader than the doc knew (the
  bitrate was also wrong). Verified: `grep -n "AACENC_SBR_MODE" crates/vendor/eld-codec/csrc/eld_shim.c`.
- **The desk-side batch shipped verbatim** — `frame_into`/`try_frame_into`, the `OutQueue` cursor,
  `enum SendOutcome { sent, droppedNotSubscribed, writeFailed }`, and the
  `OCBMSessionCoordinator.swift` split. Verified: `grep -n "fn frame_into" crates/ocbm-proto/src/lib.rs`;
  `grep -rn "SendOutcome\|onSubscriptionState" host/MacHost`.
- **C1 timeout shrink + C4 blind-retry removal landed**, tagged in-source by audit id.
  Verified: `grep -n "C1\|C4" host/MacHost/carlink_macOS/USB/USBTransport.swift`.
- **V1 AVCC fast path + V4 backpressure landed** as `Video/AVCCFastPath.swift`.
  Verified: `grep -rn "V1\|V4" host/MacHost/carlink_macOS/Video/`.
- **W1 RFCOMM reassembly landed** — a persistent accumulator with a bounded desync drop.
  Verified: `grep -n "reassembl" crates/vendor/wireless/src/bt_driver.rs`.
- **The W-set landed** — ordered thread joins before A/V teardown, and the AV latch vouches for
  rx-connect by tracked pid. Verified: `grep -n "AV_RX_CONNECT_PID" crates/vendor/wireless/src/av.rs`.
- **The TSan scheme is enabled.** Verified: `grep -rn "enableThreadSanitizer" host/MacHost/` → two
  `YES` entries in the shared xcscheme.
- **Finding B's direction 3 landed** (reap `rx-connect` by both name forms) — docs/ops/05_AUDITS.md's own inline
  note says "only fix direction 3 is unlanded", which is off by one; direction 2 is the open one.
  Verified: `grep -n "pkill" crates/vendor/wireless/src/av.rs`.

**From `docs/carplay/02_SESSION_LIFECYCLE.md` / `docs/carplay/06_AV_PIPELINE.md` / `docs/host/00_MACOS_HOST_APP.md` / `docs/ops/04_OPEN_ITEMS.md` (their banners under-report):**

- **Partial-stream teardown SHIPPED** — `AvSession::teardown` branches on stream count exactly as
  `AirPlayReceiverSession.c` does, with the empty-array-is-a-FULL-teardown correction applied. Both
  `docs/carplay/02_SESSION_LIFECYCLE.md` and `R-11-1` list it under "P2 — NONE SHIPPED".
  Verified: `grep -n "Partial teardown\|outDone=false" crates/vendor/receiver/src/session.rs`.
- **The control-channel inactivity watchdog gated on A/V-active flags SHIPPED** — neither the doc nor
  the ledger records it either way. Verified: `grep -rn "av_idle_ms" --include="*.rs" .`.
- **The 24 kHz mic rate SHIPPED on both ends** — the box negotiates AAC-ELD and OPUS 24 k mono and the
  host takes the box-negotiated rate rather than hardcoding 16 k.
  Verified: `grep -rn "24000" --include="*.rs" crates/vendor/receiver ccpa/carplayd`;
  `grep -rn "startCapture(" host/MacHost/carlink_macOS --include="*.swift"`.
- **Backpressure-not-drop is in the tree**, not "BUILT … staged" — per-stream out-queues, video gated
  on its own backlog, audio never gated. Verified: `grep -n -i "backpressure\|never gated" ccpa/ocbmd/src/main.rs`.
- **The knob/telephony descriptors, `lane_guidance` and param 30 all shipped**, so `R-06-1`'s
  remaining-work list is itself partly stale. Verified:
  `grep -n "fn knob_descriptor\|fn telephony_descriptor" crates/vendor/receiver/src/info.rs`;
  `grep -rn "lane_guidance" crates/vendor/iap2-core/src/features.rs`;
  `grep -n "ident_info_wireless_transport_declares_route_guidance_param30" crates/vendor/iap2-core/src/message.rs`.
- **AltVideo shipped in full** — `CH_ALT_VIDEO 0x24`, seam `:9005`, dedicated host decoder and window
  — although `docs/host/00_MACOS_HOST_APP.md` Tier 4 still bundles it into one open bullet with VDC and NMEA GPS.
  Verified: `grep -rn "CH_ALT_VIDEO" crates/ ccpa/ host/MacHost`.
- **docs/ops/04_OPEN_ITEMS.md's other two "Open design items" are genuinely resolved** — the `receiver_core` rootfs
  footprint (≈3.8 MiB measured) and the SETUP-relay latency (p99 2.36 ms, GO). Neither carries a
  surviving "Still open" clause; only media-port coordination does.

**From `docs/carplay/03_SDK_GROUND_TRUTH.md` / `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` / `docs/carplay/05_METADATA_AND_CONTROLS.md` / `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` / `../wireless/00_WIRELESS_CARPLAY.md`:**

- **`enablesUIAppearance` / `enablesMapAppearance` ARE parsed**, and `uiAppearanceUpdate`,
  `mapAppearanceUpdate` and `changeMapZoomLevel` all exist — docs/carplay/03_SDK_GROUND_TRUTH.md §5's body text still calls them
  absent. Verified: `grep -n "enablesMapAppearance" crates/vendor/receiver/src/vehicle_config.rs`;
  `grep -n "changeMapZoomLevel\|uiAppearanceUpdate" crates/vendor/receiver/src/events.rs`.
- **`knob_support` is wired end to end** (docs/carplay/03_SDK_GROUND_TRUTH.md §8 item 6 already annotates this STALE; confirmed so
  the index does not re-open it). Verified: `grep -rn "set_knob_advertised" --include="*.rs" crates ccpa`.
- **focusTransfer's lever and per-view `/info` flag DID land** — docs/carplay/04_CAPABILITIES_AND_CONFIG.md's "no `focustransfer()`
  lever exists / hard-false" is refuted. Only the SETUP echo is still missing (listed above).
  Verified: `grep -n "focus_transfer" crates/vendor/receiver/src/{info,levers}.rs`.
- **docs/carplay/04_CAPABILITIES_AND_CONFIG.md's "Current status" for Mechanism B is overtaken** — Identify param 20 no longer
  hardcodes `EngineType=Gasoline` and no longer omits `SupportedChargingConnectors` /
  `PowerForConnectorType`, and param 21 is emitted when the app pushes a `vehicleStatus:` block. Only
  "features.rs declares nothing" survives (listed above as C-4). Verified:
  `grep -n "engine_types\|SUPPORTED_CHARGING_CONNECTORS\|status_caps" crates/vendor/iap2-core/src/message.rs`.
- **docs/carplay/05_METADATA_AND_CONTROLS.md §5 item 5 shipped on both halves** — `viewAreas` IS echoed in the SETUP feature list, and
  `extendedFeatures` was hoisted out of the HEVC block. Verified:
  `grep -n "viewAreas" crates/vendor/receiver/src/session.rs`;
  `grep -n "extendedFeatures" crates/vendor/receiver/src/info.rs`.
- **docs/carplay/04_CAPABILITIES_AND_CONFIG.md's "a real fix would have the app retry HELLO until HELLO_ACK" shipped**, as did the
  "durable hardening" loss-tolerant decrypt counter (the box now stamps a per-frame `seq`).
  Verified: `grep -n "helloAcked\|HELLO_ACK" host/MacHost/carlink_macOS/OCBM/OCBMClient.swift`.
- **The wireless research's "Still open" items 1 and 3 are closed** — the Bonjour responder on `wlan0`
  ships in `rx-connect`, and the link-key store reuses `carplay_peers.bin`. Verified:
  `grep -n "_airplay._tcp" crates/vendor/rx-connect/src/main.rs`;
  `grep -rn "carplay_peers" --include="*.rs" crates ccpa`.
- **docs/wireless/01_BT_AND_RADIO.md's radio re-gating is CLOSED** (`R-51-3`) — the guard sits on `wireless_up` itself, and the
  two call-site strategies that failed must not be retried.
- **docs/ops/00_BUILD_AND_DEPLOY.md's "`host/CarlinkAndroid` … not yet merged to `main`" is stale** — it is on `main`.
  Verified: `git ls-tree -d --name-only main host/`.

---

### False claims found while verifying

Beyond the "found closed" list, three documents assert something the code or our own captures
contradict. Recorded here, not fixed — this index changes no other document.

- **docs/carplay/05_METADATA_AND_CONTROLS.md's banner says "the host-side pane defect is still open". It is not a defect at all.**
  `docs/ops/05_AUDITS.md` §1 (four days later) diagnosed the empty Telephony/Power/Device panes as a **stale
  binary**: the session log began before either Release build existed, and a string probe confirmed
  `batteryChargeLevel`, `wirelessCarPlayAvailable` and `battery.100.bolt` were absent from the
  binaries that existed at that moment. The real residue is the nine unread `communications`
  capability keys, listed above. Verified 2026-08-16:
  `grep -n "stale binary" ../ops/05_AUDITS.md` → §1's heading and its
  `LogStoreManifest.plist` build-timestamp evidence.

- **docs/carplay/03_SDK_GROUND_TRUTH.md §8.1 — and the `message.rs` comment repeating it verbatim — assert that "iOS can never
  send `0x5204`" without param 30 sub 8. Our own capture says otherwise.** `docs/ops/05_AUDITS.md` §9 records the
  2026-07-29 session delivering `0x5204 LaneGuidanceInformation` ×12 alongside `0x5201` ×574, as the
  first live confirmation that a subscribe-less `Trigger::RidesOn` feature receives data. The param-30
  expansion is still genuinely open; the *reason* stated for it is empirically wrong.
  Verified 2026-08-16: `grep -n "0x5204" ../ops/05_AUDITS.md ../carplay/03_SDK_GROUND_TRUTH.md`;
  `grep -n "iOS can never send" crates/vendor/iap2-core/src/message.rs`.

- **docs/ops/05_AUDITS.md's Phase-2 banner over-closes.** It reads "⚠️ EVERYTHING IN THIS SECTION SHIPPED — do not
  treat any item below as outstanding work", but `R-50-2`'s evidence covers only the desk-side batch
  and the `Protocol/` deletion. At least six items in that same section have not shipped: I2 link
  dedupe, W9 BT MAC, `CT_INPUT_NACK`, `connect_seam` numeric-only, ocbmd `opt-level=2`, and the
  cross-language ChaCha20 KAT — all six are listed above. Separately, docs/ops/05_AUDITS.md's Finding-B inline note
  ("only fix direction 3 is unlanded") is off by one: direction 3 landed, direction 2 did not.
  Verified 2026-08-16: the six `Verified open` commands in the sections above.

Two further items are open with a **wrong stated cause**, worth flagging so they are not re-planned as
the wrong work:

- **docs/host/00_MACOS_HOST_APP.md Tier 1 #5 blames the box** ("the box only advertises a single-touch HID descriptor"). The
  box ships Apple's two-finger descriptor with a unit test against Apple's fill order; the gap is
  entirely host-side. The same stale claim appears as an in-code comment beside
  `didMultiTouchTwo`. Verified 2026-08-16:
  `grep -n "touchscreen_multi_descriptor\|multi_touch()" crates/vendor/receiver/src/info.rs`.
- **`crates/vendor/eld-codec/csrc/eld_shim.c`'s opening comment still narrates `AACENC_SBR_MODE` as
  "left at -1 (auto)"** eighteen lines above the call that pins it to 0. Historical narration of the
  bug, contradicted by the code below it.

---

### Unverified

Listed with exactly what was tried. None of these can be settled by reading the tree.

- **`R4` — "pre-auth gate (Apple 470)"** (`docs/ops/05_AUDITS.md` §"Hardware session 1"). docs/ops/05_AUDITS.md names the item only
  by tag, the underlying fix proposal is in no document in the tree, and no code carries the marker.
  Tried: `grep -rn "470" --include="*.rs" crates ccpa` → nothing; `grep -rn "pre-auth\|preauth" docs/*.md`
  → only docs/ops/05_AUDITS.md's own line. The one in-code "audit R4" tag (`session.rs`, a `/tmp` size cap) is a
  different finding, so the Phase-2 R-numbering does not map onto the in-code audit tags.
- **`R2` — SESSION-lock chip-op restructure** (`docs/ops/05_AUDITS.md` §"Hardware session 2"). The MFi lock
  discipline in `mfi_local.rs` looks sound (bounded non-blocking acquire, held across the whole
  stateful sign sequence), but nothing establishes whether that IS the restructure or the state it
  wanted changed. Tried: `grep -n "flock\|MfiLock::acquire" crates/vendor/wireless/src/mfi_local.rs`;
  the in-code "audit R2" tag is in `events.rs` and is a different finding.
- **`U9` / `U13b` input changes** (`docs/ops/05_AUDITS.md` §"Hardware session 3"). Named only by tag, with no
  description anywhere in the tree. Tried: `grep -rn "U9\|U13" host/MacHost/ ../ops/05_AUDITS.md`
  → one hit, docs/ops/05_AUDITS.md's own line. Contrast V1/V4 and C1/C4, which ARE tagged in-source.
- **Hardware-observation gaps that no grep can settle.** The ACL-drop outcome (plan_A test 3), the
  remaining app/phone checklist steps (plan_A §5 tests 2–8 and 10), the app-pushed metadata tier
  ("landed 2026-08-10, NOT yet hardware-validated"), and `docs/carplay/06_AV_PIPELINE.md`'s ○-marked audio formats
  (advertisable, decode not device-confirmed). Tried: nothing beyond reading the documents — settling
  any of them needs the box and a phone.
- **`docs/carplay/02_SESSION_LIFECYCLE.md` §7 items 2–4 — three open protocol questions.** Whether `disableBluetooth` arrives before
  or after session-start on a real device; the literal bytes CINEMO puts in its first tunnelled
  message; and whether iOS honours a subscribe for an id the *tunnel* Identify declared but the *BT*
  Identify did not. Each needs a device capture or vendor observation, not a code read. Tried:
  `grep -rn "disableBluetooth" --include="*.rs" crates ccpa` → the handler exists, but nothing records
  its observed arrival order.

---

## Roadmap

<!-- absorbed: ../ops/04_OPEN_ITEMS.md -->

### Proven (on hardware)

- **OCBM transport** — `/dev/usb_accessory` bulk (VID `0x1314` PID `0x2d00`, IF0, EP IN `0x81` / OUT
  `0x01`, 512 MPS, USB 2.0 HS) *(**CORRECTED 2026-08-31** — this said `0x83`/`0x02`, which is what
  the gadget enumerated on an earlier build. docs/carplay/00_ARCHITECTURE.md §"Endpoint-address correction" is the authority
  and this line contradicted both it and §GM-AAOS further down THIS file. Do not hard-code either
  pair: enumerate. Live confirmation, every OCBM session this month: `IF0 bulk IN=0x81 OUT=0x01`.)*, **339 Mbps down / 90 Mbps up** *(**CORRECTED 2026-08-16** — this said
  "~335 / 222 up"; docs/carplay/00_ARCHITECTURE.md's measured table is the authority, and it explains the asymmetry as an
  i.MX6UL `acc_read` limit. The 220 Mbps figure elsewhere is a round-trip echo push, not a raw OUT
  rate)*, non-blocking priority scheduler. Channels
  live: CTRL `0x00` (incl. session-control SUBSCRIBE/heartbeat/presence),
  CONSOLE `0x02` (root PTY), **FILE** `0x11` (verified binary deploy —
  `ocbm-host push`, md5-matched),
  **VIDEO/MEDIA_AUDIO** `0x20`/`0x21` (encrypted A/V forward), **ALT_AUDIO** `0x22` (voice sink,
  seam `:9003`), **METADATA** `0x23` (seam `:9004`), **ALT_VIDEO** `0x24` (cluster screen, seam
  `:9005`), **INPUT** `0x30` (HID uplink), **MIC** `0x31` (mic uplink), **MGMT** `0x40` (box
  management / Carlink's adapter page), **LOG** `0x42` (the box log). (MFI, IP, ETH, RTSP, ECHO and
  DISCARD are retired, 1.0.5.) `CT_SETTIME` clock-sync verified. Authoritative constants:
  `crates/ocbm-proto/src/lib.rs`.
- **Root + deploy** — open telnet/SSH over USB-NCM (no password set as shipped; no keys or certificates), UART console (persistent 115200), SPI programmer, live
  overlay iteration; plus OCBM `push` for binaries once `ocbmd` is running. **Persistent FHS install**
  (daemons in `/usr/sbin`, tools in `/usr/bin`, scripts in `/script`) + turnkey boot: a cold boot comes
  up with a live OCBM link, zero bootstrapping (reboot-proven). **CORRECTED 2026-08-16 — the installer:**
  the current path is `tools/ncm_base_install.sh` then `tools/ocbm_install.sh --full` (reversible trial
  before finalize; `--full` also places `btd` and the `radio_detect.sh`/`radio_hal.sh`/
  `radio_ap_up.sh` seam). `tools/install_fhs.sh` still works but is the older OCBM-era subset —
  `ocbmd`/`iap2d`/`carplayd`/`rx-connect` + `iap_role_switch` only, no wireless stack and no radio seam.
  See [`../ops/01_RECOVERY.md`](../ops/01_RECOVERY.md).
- **MFi** — genuine 2.0C coprocessor at `/dev/i2c-1 @0x11`, driven from userspace (`iap2d`).
- **Phone-side iAP2 handshake — reaches Identified** against a real iPhone (SYN-ACK → cert/0xAA01 →
  sign/0xAA03 → 0xAA05 → 0x1D01 → 0x1D02), and holds the link. Unblocked by the `link.rs::parse`
  coalesced-read fix.
- **Adapter pairing + key derivation** — `ccpa/carplayd` on `ncm0` completes pair-setup/pair-verify +
  MFi-SAP (local chip) and derives the ChaCha20 session key against a real iPhone. **Disk-backed
  PeerStore** (`/etc/carplay_peers.bin`) persists pairing — a known device reconnects pair-verify-only.
- **Forward-encrypted A/V** — the box forwards the **encrypted** video + audio + hands the per-stream
  key over OCBM (`CH_VIDEO`/`CH_MEDIA_AUDIO`); a Rust debug receiver (`ocbm-host avdec`) decrypted
  host-side: hundreds of video frames + thousands of audio packets, **0 decrypt failures**.
- **Session lifecycle** — host-app-driven (SUBSCRIBE → IDLE→projection → ARM; STOP/crash → TEARDOWN →
  holding pattern), hardware-validated across a reboot. Full spec: [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md).
- **Foundation-hardening pass (2026-07-10)** — a 12-agent code analysis verified our crypto/protocol implementation against Apple's `CarPlaySDK` reference behavior and the published specification (confirmed byte-for-byte correct); the robustness
  findings it raised were addressed in follow-up work.
- **CH_ETH bridge** proved the iPhone's live link-local (mDNS/NDP) traverses OCBM to the Mac — a
  diagnostic, retired in 1.0.5.
- **Receiver** — `ncm_carplayd/receiver_core` reached a sustained genuine CarPlay session (iOS 27),
  HEVC confirmed; reused (its pairing/session path now runs on the box).

### Building blocks (committed order)

1. ~~OCBM v1 framed multiplexer + channels~~ **DONE.**
2. ~~Phone-side iAP2 handshake to Identified~~ **DONE.**
3. ~~**Adapter pairing path.**~~ **DONE.** `ccpa/carplayd` reuses receiver_core's `ControlServer`
   (pair-setup/verify + `/info`) + `pairing`/`rtsp`/`mfi`, backed by a `LocalMfiSigner` on the local i2c
   chip. Full pair-setup → pair-verify → MFi-SAP → ChaCha20 session key ran end-to-end against a real
   iPhone; mDNS `_airplay._tcp` on `ncm0` via `rx-connect`. **Disk-backed PeerStore**
   (`/etc/carplay_peers.bin`) persists pairing — known device reconnects pair-verify-only.
4. ~~**Session key + encrypted A/V over OCBM.**~~ **DONE.** The box forwards the encrypted media
   untouched + hands the per-stream key over `CH_VIDEO`/`CH_MEDIA_AUDIO`; a Rust debug receiver
   (`ocbm-host avdec`) decrypted host-side (video + audio, 0 failures).
9. ~~**Session lifecycle & host-presence management.**~~ **DONE.** (**NOTE 2026-08-16** — this item and
   "Metadata" below are both numbered `9.` in the original list; the duplicate is left as written. Where
   the banner above says "item 9" it means **Metadata**.) Host-app-driven: SUBSCRIBE (CH_CTRL)
   → IDLE→projection (`projection_up.sh`) → ARM (`session_supervisor.sh` starts carplayd);
   STOP/crash → TEARDOWN → holding pattern (iap2d stays up). Live-UI = backpressure, not drop (per-stream queues,
   gated seam reads; see docs/carplay/02_SESSION_LIFECYCLE.md); ocbmd tracks presence via `/tmp/host_present`. Hardware-validated across a reboot.
   Full spec: [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md).

**Roadmap items 1–10 below are all complete** (5/9/10 hardware-validated; item 8 wired 2026-08-08 and
wireless 2026-08-10). They are kept because the per-item notes record how each was validated; what is
still open is §4 of this file, not this list.

5. **Host app (THE critical path).** ✅ DONE 2026-08-01 (built + hardware-validated). Rebuild from `carlink_macOS` / `ncm_carplayd/macos` carplay-app:
   claim `1314:2d00`, SUBSCRIBE over CH_CTRL, decrypt with the handed-over key, **decode HEVC + render
   pixels** (VideoToolbox), play audio, and **send touch input back** (the input uplink — CH_INPUT,
   ✅ wired: `CarPlayView` touch → `OCBMClient.sendTouch` → CH_INPUT; ~~not yet exercised~~, corrected
   2026-08-01 — box-side touch/media HID arrival confirmed 2026-07-12). This replaces the
   debug receiver and closes the loop to a usable head unit.
6. **Nav voice / mic.** ✅ **DONE — corrected 2026-08-10 (this line was stale).** Nav voice DOES have
   its channel: `forward.rs` tags every AU on `:9003` (`[rate:u32 BE][ch:u16 BE][atype:u8][len:u32 BE][AU]`) and
   `crates/vendor/receiver/src/session.rs` routes **by `audioType`, not stream type** — media → media
   sink (:9002), everything else (telephony/speechRecognition/alert/default) → the voice sink (:9003)
   (search `Route by audioType`; ~`:871`/`:880` as of 2026-08-16). The mic
   uplink exists too: host `MicCapture.swift` → OCBM → carplayd's `MIC_INGEST_ADDR 127.0.0.1:9112`.
   **Owner-confirmed on hardware 2026-08-10: nav prompts audible, Siri invocable and spoken to, phone
   calls working BOTH ways.**
   ~~Nav voice on `:9003` has no OCBM channel yet; plus the mic uplink for Siri.~~
   STILL OPEN, and it is a DIFFERENT feature: **Enhanced Siri** (`AuxOut` 106 / `AuxIn` 107) — the
   always-on mic path with ECNR, the in-car historical ring buffer and the two mandatory detectors
   (authority: `wwdc2019-252.txt:86-134`). `session.rs` records AuxOutAudio / AuxInAudio /
   MainBuffered as unimplemented and therefore omitted from SETUP responses (search "Still unimplemented
   and therefore omitted"; ~`:1516` as of 2026-08-16). Button-Siri works; the always-listening path does not exist.
7. **YAML config consumption.** ✅ **DONE — marked 2026-08-10** (it was already shipped; this line
   simply never got its marker, which made the doc contradict its own "items 1-10 complete" note).
   App-side truth (`VehicleConfig.swift` + `SettingsWindow`), pushed subset consumed by
   `vehicle_config.rs` and applied per control connection by `carplayd::load_device_config` — runtime
   feature flip, no recompile; `base_device_config()` is the app-less fallback template.
8. **Move SETUP app-side.** **RETIRED 2026-11-08 (OCBM 1.0.5)** — the box answers SETUP itself from the
   pushed config again, and the relay below is gone. As it stood: ✅ DONE 2026-08-08 (wired, hardware-validated; the
   `accessoryConfig.appDrivenSetup` toggle default was flipped **ON** 2026-08-09) — commits `84d2b80` (P0 RTT harness + P1 box CH_RTSP relay
   + transparency proof), `692cc80` (P2 Rust-harness authoring + live oracle), `89c457b` (P3 Swift
   port). (**CORRECTED 2026-08-16:** the three hashes previously printed here — `c5af75f`, `b09aee7`,
   `3d56e28` — exist in no ref.) The host app
   drives the post-pairing SETUP/RECORD negotiation over the OCBM `CH_RTSP` relay; the box's local
   response is the always-available fallback. Latency gate PASSED (docs/carplay/02_SESSION_LIFECYCLE.md: p99 2.36 ms under A/V load,
   ~21× inside the 50 ms bar). Go/no-go recorded in **docs/carplay/02_SESSION_LIFECYCLE.md**. Box-driven SETUP (`receiver::session`)
   is now the selectable fallback — app-driven SETUP is the default on BOTH transports (wired flipped
   ON 2026-08-09, wireless 2026-08-10 by owner directive; the docs/carplay/04_CAPABILITIES_AND_CONFIG.md earned-fallback pattern done
   right). The selection gate is `appsetup() && seam_up()` — the `!wireless` term is gone. The
   wireless flip required a phase-1 feature-token union (the host preserves the box-only
   `iAPChannel`/`sessionManagement`); see docs/carplay/04_CAPABILITIES_AND_CONFIG.md for why stripping `iAPChannel` would have killed
   the iAP2 tunnel. **Remaining follow-up:** migrate those two tokens to per-transport app config
   (workstream C/D debt, recorded in docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
   Current pick-up: ../ops/04_OPEN_ITEMS.md. This
   realizes the future-proofing target of README §7 (SDK-evolving SETUP logic app-side).
9. **Metadata** (NowPlaying / Nav) — ✅ DONE (surfaced via `features.rs`-generated params 6/7 + subscribes; see docs/carplay/05_METADATA_AND_CONTROLS.md). ~~not yet surfaced; likely needs a subscribe/command or a channel `AvSession` doesn't currently expose.~~
10. **Wireless CarPlay** (BT-RFCOMM iAP2 + WiFi SoftAP + credential handover). ✅ DONE for transport,
    pairing and A/V (see docs/carplay/05_METADATA_AND_CONTROLS.md). ~~**⚠️ The METADATA plane regressed 2026-07-31 → 2026-08-10** (a
    scid guard rejected the type-130 DataStream SETUP; tunnel stuck at `Init`, zero NowPlaying). Fix
    landed 2026-08-10, **not yet hardware-validated** — do not re-mark this line fully DONE until a
    soak shows `0x5001` records arriving.~~ **RESOLVED — the validation gate this line set was met on
    2026-08-11**: Identify accepted, `decryptFail=0`, NowPlaying and route/lane guidance arriving
    (README §Status, 2026-08-11). Regression evidence retained:
    `docs/ops/captures/2026-08-10_REGRESSION_datastream130_scid_rejected.txt`.
    ~~Rootfs radio scripts exist; handshake documented in `CPC200-CCPA_resources`. Later phase.~~

### Open design items

- ~~OCBM media framing: how encrypted stream payloads + the key handoff are carried.~~ **RESOLVED** —
  per-stream channels each carry a key handoff then raw encrypted frames; validated 0-failure host-side
  decrypt. (The video lanes additionally carry `SEAM_MAGIC` + a `u64 LE seq`, and the key is re-handed on
  every seam reconnect — see docs/carplay/01_OCBM_PROTOCOL.md §"Media transport" for the byte-exact framing.)
  ~~Still open: minimal RTSP framing the box relays vs the app parses (the SETUP box↔app boundary, incl.
  media-port coordination), once SETUP moves app-side (step 8).~~ **RESOLVED 2026-08-16 for the framing
  half** — it shipped with step 8: `CH_RTSP 0x0041` carrying the v1 `RS_OPEN`/`RS_REQ`/`RS_RESP`/
  `RS_CLOSE`/`RS_ERR` sub-frames (`crates/ocbm-proto/src/lib.rs`, constants authoritative in
  `receiver::relay`; wire spec in docs/carplay/01_OCBM_PROTOCOL.md §"RTSP channel (`0x0041`)"). **Still open: media-port
  coordination only** — the box pre-binds and its local response doubles as the oracle, so the app
  cannot yet choose the media ports (docs/carplay/01_OCBM_PROTOCOL.md §"RTSP channel" / §"Media transport").
- ~~`receiver_core` footprint on the 6 MB rootfs (crypto crates; run-from-tmp; size-opt/UPX) — measure
  the armv7-musl build (see [`../ops/00_BUILD_AND_DEPLOY.md`](../ops/00_BUILD_AND_DEPLOY.md)).~~ **RESOLVED** — measured
  2026-07-10 and re-measured 2026-08-16: the pairing/session path cross-compiles clean, and the whole
  shipped set is ≈**3.8 MiB unpacked** (`carplayd` ~1.71 MiB, `rx-connect` ~574 KiB, `iap2d` ~551 KiB,
  `btd` ~537 KiB, `ocbmd` ~443 KiB), roughly half that UPX-3.96-packed — comfortably inside
  the ~6 MB free, and run-from-`/tmp` was never needed. Numbers and method:
  [`../ops/00_BUILD_AND_DEPLOY.md`](../ops/00_BUILD_AND_DEPLOY.md).
- ~~**SETUP relay latency** — driving SETUP app-side relays the control connection box↔app over USB;
  measure it doesn't destabilize the timing-sensitive pair/SETUP/RECORD phase.~~ **RESOLVED 2026-08-08
  (docs/carplay/02_SESSION_LIFECYCLE.md): PASS** — p99 2.36 ms under real A/V load (upper-bound path), ~21× inside the 50 ms gate,
  zero timeouts/cap-clears. Go/no-go in docs/carplay/02_SESSION_LIFECYCLE.md.

### Host-app / integration (not adapter firmware)

- **GM AAOS USB permission prompt: SOLVED and DEVICE-TESTED (2026-08-17).** The reappearing permission
  dialog on GM AAOS (`gminfo37`) is a head-unit bug — `framework-res`'s
  `config_UsbDeviceConnectionHandling_component` points at the stripped `android.car.usb.handler`, so
  the third-party implicit-grant path dead-ends (native CarPlay/AA/storage are unaffected; they route
  around it via system privilege). Fix, proven on a 2024 Silverado: ship the host app **UNDER package
  `android.car.usb.handler`, installed in user 10** → the framework grants USB permission to its UID
  **silently on every attach, no dialog, no root.** A controlled on-vehicle A/B (handler present → no
  dialog; handler removed → dialog returns) confirmed the squat is the sole cause. The grant is
  **per-UID**, so the claiming app must itself BE that package (a separate app still prompts; `MANAGE_USB`
  to re-grant needs priv-app / `/system` write the locked GM lacks — `adb pm grant` refuted). Landed in
  the `gm_ccpa` wireless receiver (`zeno.gmccpa` re-packaged to `android.car.usb.handler`, NoDisplay
  `UsbHostManagementActivity` trampoline). A standalone proof module also exists in
  `host/CarlinkAndroid/usbhandler`. Full analysis, evidence, refuted alternatives and the endpoint
  correction (`0x81/0x01`, not `0x83/0x02`):
  [`../host/01_ANDROID_AND_AAOS.md`](https://github.com/enigy/carlink/blob/main/docs/ANDROID_AND_AAOS.md).
  Adapter side needs no change (the `0x1314:0x2d00` identity is already correct — docs/carplay/00_ARCHITECTURE.md).

### Android host app (`host/CarlinkAndroid`) — 2026-09-18 session

`host/CarlinkAndroid` left the tree in OCBM 1.0.5; it is Carlink now
([enigy/carlink](https://github.com/enigy/carlink)), whose open items live there. This section as it
stood is at tag [`v1.0.4`](https://github.com/enigy/ocbm/blob/v1.0.4/docs/ops/04_OPEN_ITEMS.md).

### Contingencies (future — if/when measurements demand it)

- **Adapter-driven SETUP is the only path since OCBM 1.0.5.** The box answers SETUP locally from the
  app-pushed config, bounded to rendering that config — adapter-owned SETUP capability does not grow
  (docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 4). The app-driven relay it used to fall back from is retired.
- **Decode-on-adapter fallback** (deep contingency): the box could decode/re-emit typed A/V (the
  original CCPA model) — retained only as a last resort; the i.MX6UL has no VPU, so video is forwarded
  either way and this buys little.

## HANDOFF 2026-09-05 → 09-07 — CarPlay view-area resize: the rules, the matrix, and what is left

**STATE: the resize matrix is COMPLETE (9 ACCEPTED / 1 LOCKOUT, both orientations, floor to 4K) and
the whole loop is unattended** — the app arms the rect through its own model fields, commands the
transition over the control socket, and judges the result from the decoded frame. Three things are
open and none of them block using resize: the LOCKOUT detector is wrong (Known-open 7), the
per-panel floor is unbisected (8), and the animation duration is not yet a variable (6).

Device-proven on a **CPC200-CCPA, iPhone18,4 / iOS 27.0 (24A5430a)**. The 09-05 work was **wireless**
CarPlay; everything dated 09-07 is **WIRED** (the phone is plugged into the box, so there is no
phone-side log in that arm at all). Where a claim is arm-specific it says so; the rule set below
holds in both.

### The rule set (settled)

1. **ALL FOUR values must be EVEN** — `x`, `y`, `width`, `height`. An odd value is a **TEARDOWN**:
   `kFigEndpointError_InvalidParameter -16720` originating in **`carEndpoint_copyScreenInfo:7001`**,
   propagating `setupScreenStreams:8536` -> `setupStreams:8957` -> `activateInternal:9987` ->
   `Activate_block_invoke_2:10117`. Isolated to one pixel: `356x400@240,760` renders,
   `357x400@240,760` tears down; `601x400@240,760` (odd w, far above any floor) tears down;
   `600x400@240,761` (odd origin Y only) tears down. Cause is almost certainly HEVC 4:2:0 chroma
   subsampling. **This retroactively explains every `-16720` in the project's history** — the old
   `1416x842@492,59` and `1600x842@800,59` failures both carry an odd origin Y and nothing else was
   ever wrong with them. Prior explanations (size floor, "must touch a vertical edge",
   `focusTransfer`, `cornerMasks`) are all REFUTED; see docs/carplay/06_AV_PIPELINE.md.
   **SCOPE, narrowed 2026-09-07 (wired, 4K panel): parity binds what we DECLARE, not what iOS puts
   back on the wire.** iOS's own transition animation streams odd rects freely — 275 of 292 geometry
   records in one round trip carried an odd origin or extent (`3826x2152@7,4`, `3834x2158@3,1`) with
   no teardown — and a SETTLED rect can be odd too (see "What the wired arm added" below). Do not
   "fix" a harness or a validator to reject those; the gate is on the declaration.
2. **Containment** — `x+w <= panelW && y+h <= panelH`. Violation is a TEARDOWN.
3. **PRODUCT FLOOR (owner, non-negotiable): 800x480 landscape / 480x800 portrait**, CarPlay AND
   Android Auto, orientation chosen by the area's own aspect. This OVERRIDES what iOS tolerates.
4. **No origin constraint** beyond parity + containment. No evenness-of-position rule beyond (1), no
   edge-contact requirement (a floating portrait area touching no edge renders). **Re-proven
   2026-09-07 on a 3840x2160 panel over WIRED CarPlay, which is where the free origin was previously
   only assumed**: `1280x720@200,180` (off-centre upper-left), `800x480@3040,1680` (flush into the
   bottom-right corner, `x+w`/`y+h` exactly equal to the panel) and `2560x1440@640,360` each
   transitioned and settled at exactly the armed rect, no refusals, no teardowns. Diagonal offsets
   are not a special case — the rule really is: even, contained, at or above the floor, anywhere.
5. `cornerMasks` and `viewAreaSupportsFocusTransfer` are both **orthogonal** to acceptance.

**iOS's own tolerance, for protocol understanding only — NOT the shipping rule.** Measured floors on
a 1080x1920 panel: width **350**, height **304** (`350x400` renders, `348x400` locks out;
`600x304` renders, `600x302` locks out). The extracted `385 * 0.65 * scale` / `240 * 0.65 * scale`
formula is REFUTED as the gate — it mispredicted `480x400`, `400x400` and `384x400`, all of which
render. Do not resurrect it.

**Two failure classes, never conflate:** an odd/oversized/non-positive value is a **TEARDOWN**
(session dies). Below the floor is a **LOCKOUT** — `[DBLockOut] lockOutMode updating to
viewAreaTooSmall`, string `LOCKOUT_VIEW_AREA_TOO_SMALL_MESSAGE` -> "CarPlay does not support this
display resolution." Black area, session SURVIVES.

### What the WIRED arm added (2026-09-07)

Same box, same iPhone, but the phone is plugged into the BOX — so there is no phone-side log at all
(docs/ops/02_TESTING.md). Every verdict below came from the app's control socket, the app log and the
pixels. The matrix now runs unattended end to end, with no human in the actuation path:

- **The resize button is the pill at the TOP of the CarPlay Dock rail** — two arrows pointing inward,
  above the clock, not among the app icons. Pressing it from the socket works:
  `tap 133 179` (the app's 0..10000 space, area 0 on a 1200x675 window) -> the phone sends
  `requestViewArea index=1` -> the box answers `updateViewArea index=1` -> the picture animates into
  the sub-rect. **The button moves WITH the area**, so the return press is at different coordinates
  (`tap 4109 4039` for `800x480@1520,840` in a 3840x2160 panel). A harness cannot use one constant.
- **A settled rect is not always the declared rect, and it can be ODD.** `1920x1080@1900,1060` settled
  at `1921x1081@1899,1059` and STAYED there — polled every 2 s for 24 s, geometry-record counter
  frozen at 154, so the animation had finished. The far edges landed exactly (`1899+1921 = 1900+1920
  = 3820`); iOS converged one pixel out toward the origin. Three other rects the same session landed
  byte-exact, so this is a per-rect rounding artifact of the transition's final interpolation step,
  not a correctable offset. **Compare settled rects with a ±2 px tolerance** (`rect_close` in
  `tools/va_wired_sweep.sh`); exact string equality scores a good case NO-MOVE.
- **iOS self-transitions at session start.** Unprompted, it shrank into area 1, held ~2.4 s, and grew
  back to area 0 (answered `updateViewArea -> index=0`). A harness that samples one geometry record
  instead of the settled rect reads this as a pass.
- **THE LOCKOUT FLOOR SCALES WITH THE PANEL — the product floor is not panel-independent.**
  `480x800` inside a **2160x3840** panel LOCKS OUT: iOS renders
  `LOCKOUT_VIEW_AREA_TOO_SMALL_MESSAGE` ("CarPlay does not support this display resolution") and the
  session survives, exactly as the two-failure-classes rule predicts. But the wireless measurements
  above found areas as small as `350x400` RENDERING on a **1080x1920** panel — the same panel at half
  the linear size. So the same rect can render on one panel and lock out on a bigger one, and
  `480x800` — our own declared portrait product floor — is NOT safe on a 4K portrait panel. On
  2160x3840 the floor lies between `480x800` (lockout) and `720x1280` (renders, mean luma 76.6);
  it has not been bisected. This does NOT resurrect the refuted `385 * 0.65 * scale` formula — that
  mispredicted specific rects and stays refuted — but it does mean any floor quoted without its
  panel is meaningless. Bisect before treating 800x480/480x800 as universally shippable.
- **A LOCKOUT is not a black area — do not detect it by luminance.** The banner card is sized to
  FILL the view area, so the armed rect reads mean luma 40.0 / darkFrac 0.01 (content!), while the
  panel *around* it is 95% dark. `tools/va_wired_sweep.sh`'s first thresholds (`BLACK_MEAN=12`,
  `BLACK_DARK=0.85`) therefore scored a textbook lockout as ACCEPTED, and only an eye check caught
  it. The usable discriminator is CHROMA, not brightness: a real CarPlay UI is full of coloured app
  icons, the lockout card is flat achromatic grey with white text.
- **`animationDurationMillis` is honoured literally.** Against the hardcoded 3000 we send, measured
  **2.978 s** (shrink) and **2.988 s** (grow) at ~48 geometry records/s. It is a real duration, not a
  hint. Its LIMITS are unmeasured — see "Known-open" below.

### Tooling left behind

- `tools/va_limit_probe.sh SPEC LABEL [cornermasks] [focustransfer]` — arms a view-area spec,
  captures the PHONE's log across the attempt, classifies ACCEPTED / DEGRADED / REJECTED /
  REFUSED-LOCALLY. Uses `:initial` so the session STARTS in the area — **no button press, no
  screenshots, fully unattended.** Requires WIRELESS CarPlay (the iPhone's USB must be free).
- `tools/va_limit_sweep.sh` — drives panel AND area together, floor-to-4K, both orientations.
- Two traps already fixed in these, do not reintroduce: the capture must NOT be `-pn carplayd`
  (the lockout is logged by CarPlay's UI process, not carplayd), and the app-restart dwell must be
  **12s** — the supervisor's wireless teardown SIGTERM lands ~6s late and will kill the bring-up
  that follows it.
- **2026-09-07, WIRED arm:** `tools/va_wired_sweep.sh` — same matrix and even-centring arithmetic,
  but no phone capture exists in a wired session, so it verdicts from the control socket
  (`viewarea arm` + `save`, then `viewarea request 1` or a `TAP` on the Dock button), the app log,
  and the pixels of the armed rect (`shot`, or `tools/winshot.sh` — window capture by CGWindowID,
  occlusion-proof). Verdicts: ACCEPTED / LOCKOUT (black rect, session alive) / TEARDOWN /
  REFUSED-LOCALLY / INCONCLUSIVE; PNGs under `/tmp/valog/wired/<label>/`. Procedure and the two
  bench traps it encodes (sandboxed-shell launch kills the app; never score a screen region) are in
  docs/ops/02_TESTING.md "Wired view-area resize matrix". **The Dock TAP coordinates are pinned but
  no longer needed** (`tap 133 179` in area 0; the button moves WITH the area, so it is never a
  constant — `viewarea request` replaced it). **The LOCKOUT thresholds are still WRONG — see
  Known-open 7.**
- **Three harness defects the first runs exposed, all the same shape — a case that LOOKS measured
  and is not. Do not reintroduce any of them:**
  1. **Triggering before the geometry settles.** iOS runs its own view-area round trip at session
     start, so a trigger fired in the same second the session comes up lands inside that animation
     and is swallowed; the BEFORE frame is a picture of a rect nobody asked for. `settle_geometry`
     waits for the `changes` counter to hold still.
  2. **Reading `[backfill]` as this session's evidence.** The app replays box log history on
     connect, tagged `[backfill]`. A backfilled `host viewArea index=1 REFUSED` from the PREVIOUS
     case decided `P-720x1280`'s verdict. Verdict-producing scans go through `live_log`.
  3. **Trusting a panel the app silently clamped** — the `PanelRule` bug below. The sweep now
     compares `armed.panel` against what the case asked for and fails it REFUSED-LOCALLY with
     "measures nothing" rather than measuring a rewritten panel.
- **The `PanelRule` bug this cost, fixed 2026-09-07.** The app's panel envelope was per-axis
  (`VehicleConfigModel.minHeight/maxHeight` 480–2160), so `set height 3840` was silently clamped to
  2160 on `save`, every portrait area failed containment against a 2160-tall panel, the emitter
  dropped `viewAreas[1]`, and the box declared one area — while the landscape half passed 5/5 and
  the run looked healthy. The envelope is now `PanelRule` (orientation-agnostic, floor by the
  panel's own aspect) and clamps are reported (`clampNotes` in `save`/`get viewarea`,
  `pendingClamp` in `set`) instead of being silent.

### The matrix — COMPLETE as of 2026-09-07 (wired)

Both orientations, floor to 4K, **9 ACCEPTED / 1 LOCKOUT**. Every case armed through the app door
(`viewarea arm` + `save`, no bench lever) and triggered by `viewarea request` (no tap, no human).
PNGs per case under `/tmp/valog/wired/<label>/`.

| Panel | Area | Verdict |
|---|---|---|
| 3840x2160 | 800x480@1520,840 | ACCEPTED (also OWNER-CONFIRMED by eye, wireless, 09-05) |
| 3840x2160 | 1280x720@1280,720 | ACCEPTED |
| 3840x2160 | 1920x1080@960,540 | ACCEPTED |
| 3840x2160 | 2560x1440@640,360 | ACCEPTED |
| 3840x2160 | 3840x2160@0,0 | ACCEPTED (area [1] == area [0]) |
| 2160x3840 | 480x800@840,1520 | **LOCKOUT** — below iOS's floor FOR THIS PANEL |
| 2160x3840 | 720x1280@720,1280 | ACCEPTED |
| 2160x3840 | 1080x1920@540,960 | ACCEPTED |
| 2160x3840 | 1440x2560@360,640 | ACCEPTED |
| 2160x3840 | 2160x3840@0,0 | ACCEPTED (area [1] == area [0]) |

Plus four off-centre landscape cases proving the free origin at 4K (see rule 4):
`1280x720@200,180`, `800x480@3040,1680` (flush into the corner), `2560x1440@640,360`, and
`1920x1080@1900,1060` (the ±1 settled-rect case).

**The largest panel previously driven was 2400x960**, so 4K panel config in BOTH orientations is
new. The only failure is the portrait floor case, and it is a real device result, not a harness or
config fault — see the panel-scaling finding above and Known-open 8.

The WIRELESS matrix (`tools/va_limit_sweep.sh`) is still one case deep. Nothing here contradicts it —
wired and wireless differ only in carrier — but if a wireless-specific view-area difference is ever
suspected, that is the run to complete.

### Known-open, in priority order

1. **DONE — `ViewArea2Rule` corrected and verified.** Carries the owner product floor
   (800x480 landscape / 480x800 portrait, orientation by the area's own aspect) and the parity rule,
   reported in severity order: containment, parity, positivity, floor. The refuted
   `385 * 0.65 * scale` derivation is gone. `run_tests.sh` 1300 passed; receiver PASS; docs OK.

   **Parity is enforced in the APP ONLY, and that is deliberate — do not "fix" it box-side.**
   CLAUDE.md's design doctrine: anything configurable about CarPlay is app-driven, the box presents
   app-pushed config, and box placement is EARNED (measured app-driven failure + owner approval),
   never designed-in. A box-side parity gate was written, tested and then REVERTED on owner
   direction — the app is where the logic and the options live. There has been no measured
   app-driven failure to earn a box-side copy. The app-less `CARPLAY_VIEWAREA2` bench lever
   therefore still accepts an odd rect and will tear the session down; that is a BENCH tool used by
   an operator who now knows the rule, not a product path.

2. **DONE — transition path commandable, VERIFIED ON DEVICE 2026-09-07.** `:initial` proves iOS
   accepts STARTING in an area; it does not prove iOS animates INTO one on request. Owner-confirmed
   twice by eye — `600x400` on a 1080x1920 panel, and `800x480` inside 3840x2160. The clean fix
   landed: `viewarea request <index>` on the ControlServer → `CMD_VIEW_AREA 0x11` →
   carplayd → `events::switch_view_area`, the same policy function that answers the phone's own
   `requestViewArea` (docs/host/00_MACOS_HOST_APP.md, "Bench control surface"). Two companions for
   the WIRED arm, where there is no phone log: `viewarea arm <WxH@X,Y>` writes the rect through the
   model's own fields (replacing `defaults write` against a stopped app and the `/tmp/carplay_viewarea2`
   lever), and `shot [path]` dumps the decoded frame as PNG — the only LOCKOUT detector this side owns.
   **The rebuilt carplayd IS pushed** (2026-09-07, `tools/ocbm_push.sh … /usr/sbin/carplayd 755`,
   md5 verified against the local cross-build; ocbmd unchanged). Device-proven the same evening:
   `viewarea request 1` → `[events] host viewArea index=1 accepted` → `updateViewArea -> index=1
   duration=3000ms` → the rect settles at the armed area; `request 0` returns; `request 5` is
   REFUSED box-side (`only 2 area(s) declared`) with the session healthy. All ten matrix cases ran
   on this path.
   **This also proves the accessory can drive a transition UNILATERALLY** — nothing asked. The
   earlier finding was that the phone's `requestViewArea` is advisory and the accessory is the
   authority (events.rs); this extends it: no request need exist at all. An old carplayd logs
   `unknown INPUT_COMMAND 0x11 — dropped`, so a box that has not been updated needs
   `TRIGGER_MODE=tap` in the sweep, or the trigger silently looks accepted while nothing moves.
3. **Supervisor teardown/bring-up is not serialised** and reports success for a stack that is dead
   6s later — this is the same defect as the "Restart Wireless stack" button being a destructive
   no-op. Two fixes: serialise on child reap, and verify the bring-up instead of asserting it.
4. `viewArea2*` keys are UserDefaults-only — not in the neutral profile document, so Import/Export
   does not round-trip them. Deferred because the profile schema is shared with the Settings view
   work in another session.
5. `initial` is parsed box-side but not authored by the app model.
6. **CLOSED 2026-09-09 — `animationDurationMillis` is fully characterised; there is no iOS floor or cap.** Swept on hardware (CPC200-CCPA, wired, 1920x1080, second view area): **10 ms renders the switch as an instant flicker; 10000 ms runs the full 10 s.** Honoured literally at both extremes. One nuance worth keeping: at 10000 ms the UI reaches the target geometry at roughly 5 s and then idles showing CarPlay's transition blur until the envelope expires — the duration is the ANIMATION window, not the geometry change, so the accessory gets its settled geometry earlier than the declared duration. Untested: whether a larger area delta occupies more of the envelope instead of finishing early. The shipped range is now **1000–10000** by owner decision — a product limit (sub-second reads as abrupt in a car), NOT a protocol one; 10 ms works and is simply not offered. The original text of this item follows for the record.

   ~~**`animationDurationMillis` limits are unmeasured; its FLOOR is the open question.**~~ (Corrected
   2026-09-09 — it was "hardcoded 3000, not yet a variable"; both halves are now false.) On the bench
   that night 3000 → 1000 was DEVICE-PROVEN to visibly speed up the Dock resize, the first time anyone
   varied it on hardware, and it is now a pushed value: top-level `view_area_anim_ms` in the vehicle
   config (our extension, `wifi_ap`-shaped; absent = 3000, the app emits it only when it differs),
   parsed by `receiver::vehicle_config`, armed per connection into `levers::view_area_anim_ms`, and
   consumed by `events::switch_view_area` for BOTH the inbound `requestViewArea` answer and the
   `viewarea request <index>` door. Out of range CLAMPS to 0..=10000 (logged by carplayd, never a
   rejection), so the ladder below past 10000 — 30000, 60000, `INT32_MAX`, a negative — is NOT
   reachable from the app without widening that clamp deliberately; 0, 1, 100, 250, 1000, 3000, 10000
   are. iOS never acknowledges the field, so the `[events] updateViewArea -> … duration=Nms` line
   (post-clamp value) plus a stopwatch on the geometry stream is the whole measurement. What the
   sources give: `CarPlaySDK.framework` carries the key name only;
   CarKit has an explicit clamp message for the INDEX (`Resetting to first view area … out of range`)
   but **no duration-validation string anywhere in the iOS 27 CarKit or AirPlaySender extracts**;
   R14G17 predates view areas entirely; CINEMO logs it as `duration: %ums`, unsigned, so a negative
   is outside a shipping vendor's own model. Apple's Simulator and WWDC 2019-252 both use 3 s.
   Since iOS honours the value literally (measured above), the ladder worth running is 0, 1, 100, 250,
   1000, 3000 (known good), 10000, 30000, 60000, plus `INT32_MAX` and a negative — watching for
   whether 0 snaps or is refused, whether a long duration blocks a second `requestViewArea` mid-flight,
   and whether anything tears the session down. The per-value recompile that ladder needed is gone
   (push a config with `view_area_anim_ms: N` and reconnect); a per-request `[ms]` argument on the
   `viewarea request` door would only matter for sweeping values within one session.
7. **The sweep's LOCKOUT detector is WRONG and currently scores a lockout as ACCEPTED.** Device-proven
   2026-09-07 on `480x800@840,1520` in a 2160x3840 panel: `BLACK_MEAN=12` / `BLACK_DARK=0.85` never
   fired because **the lockout card is sized to FILL the view area** — the armed rect reads mean luma
   **40.0**, darkFrac **0.01** (i.e. content), while the panel *around* it is 95% dark. The verdict
   was caught only by looking at the PNG. No luminance threshold can fix this: a real render and a
   lockout are both "not black". The discriminator is **CHROMA** — a real CarPlay UI is full of
   coloured app icons, the lockout card is flat achromatic grey with white text. `tools/winshot.swift`
   `stats` needs a saturation metric and the classifier needs to use it. Until then, **every ACCEPTED
   verdict near a floor must be eyeballed** against `$OUT/<label>/after_frame.png`.
8. **Bisect the lockout floor per panel — the shipping floor may be wrong.** `480x800` locks out on
   2160x3840 but the wireless data has `350x400` RENDERING on 1080x1920, so the floor scales with the
   panel and our declared portrait product floor is not safe on a 4K portrait panel. On 2160x3840 it
   lies between `480x800` (lockout) and `720x1280` (renders). Bisect both orientations at 4K, then
   decide whether the product floor stays a constant or becomes panel-relative. Do item 7 first — a
   bisect run on a detector that cannot see a lockout produces a confidently wrong number.
