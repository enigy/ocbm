//! The session's metadata, kept for a host that comes to the session late (ADDENDUM 8 (a) of the Wi-Fi link plan,
//! 24 Nov 2026).
//!
//! The metadata seam (:9004 -> CH_METADATA, `[u32 BE "META"][u32 BE len][marker][payload]`, ocbm-proto) carries what
//! the phone says once and then only as it changes: the now-playing state after the box's subscribe, deltas from then
//! on, the phone's requests for the car's GPS and status, route guidance, the call state, the corner mask at SETUP. In
//! Wi-Fi mode the adapter's own session (M2) runs with no host, and its seam goes to the sink then, so a host that
//! attaches later never had any of it (the 24 Nov bench: Carlink, whose STREAMING waits for a now-playing record,
//! stayed at DEVICE_CONNECTED; the phone's GPS request never reached it). ocbmd reads the seam as it forwards it, keeps
//! the latest of what a host acts on, and replays that at the host's SUBSCRIBE (main.rs `meta_attach`).
//!
//! Kept, per kind, in the order the phone sent it, so a host that takes the replay ends where the live stream left it:
//! - `nowPlaying`, `routeGuidance` (deltas the host merges field by field): a record goes when a newer one carries
//!   every field it has, so the replay merges to the same state, and a change of title comes where it came;
//! - `maneuver` (per `index`) and `laneGuidance` (per `guidanceIndex` and `index`): the same, per key;
//! - `callState`: the last record of each call (`callUuid`, else `remoteId`, as the host keys calls); a call that ended
//!   (status 0 or 6) goes, and the idle record (status 0, no call named) takes them all;
//! - `location`, `vehicleStatus` (the phone's start or stop request), the corner mask, the artwork: the last one.
//!
//! Each kind has a cap (the oldest goes past it). A record over [`RECORD_MAX`] (an artwork that large) passes by
//! unheld; a JSON record over [`JSON_MAX`] is not kept and takes the older ones of its kind and key with it, so none
//! stale is replayed. Not kept: command plists (`requestUI`, `modesChanged`: events and requests), route A's
//! `dataStream` messages (requests the host answers on the stream: replayed, they would be answered long after the
//! phone asked), list items, and the kinds no host acts on. At worst 96 JSON records of 4 KiB, an artwork of 256 KiB
//! and a corner mask of 64 KiB: about 700 KiB, under the 1 MiB bulk queue the replay goes into at once (a few KB in
//! practice), with the one record held while it comes in.
//!
//! The replay goes into the host's stream at a record boundary only. The host reads the channel as one byte stream
//! (Carlink's MetadataSeam), so bytes put inside a record under way would be read as that record's, the replay with
//! them. [`MetaKeep`] follows the seam's records as that reader does (resync byte by byte on the magic; a zero or
//! over-16-MiB length is a false magic), and a replay asked for inside a record waits for its end ([`MetaKeep::feed`]).

/// The seam's record markers (ocbm-proto's CH_METADATA note; iap2-core's metadata.rs).
const META_JSON: u8 = 0x02;
const META_ARTWORK: u8 = 0x03;
const META_CORNERMASK: u8 = 0x04;
const MAGIC: &[u8; 4] = b"META";
/// A length the host's reader takes for a false magic (Carlink MetadataSeam.MAX_MESSAGE).
const LEN_MAX: usize = 16 * 1024 * 1024;
/// The largest record held while it comes in; a larger one passes by unheld and unkept.
const RECORD_MAX: usize = 256 * 1024;
/// The largest JSON record kept.
const JSON_MAX: usize = 4 * 1024;
/// The largest corner mask kept.
const MASK_MAX: usize = 64 * 1024;

/// How a kind's newer record replaces older ones of the same key.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// A delta stream the host merges field by field: an older record goes when the newer one has all its fields.
    Merge,
    /// A state: the newer record replaces the older.
    Last,
    /// iAP2 CallStateUpdate: [`Rule::Last`] per call, and a call that ended goes.
    Call,
}

/// A JSON kind kept: its name, its rule, the fields its key is made of, how many of its records are kept at most.
struct Kind {
    name: &'static str,
    rule: Rule,
    key: &'static [&'static str],
    cap: usize,
}

const KINDS: [Kind; 7] = [
    Kind { name: "nowPlaying", rule: Rule::Merge, key: &[], cap: 12 },
    Kind { name: "routeGuidance", rule: Rule::Merge, key: &[], cap: 6 },
    Kind { name: "maneuver", rule: Rule::Merge, key: &["index"], cap: 48 },
    Kind { name: "laneGuidance", rule: Rule::Merge, key: &["guidanceIndex", "index"], cap: 24 },
    // The key is the first of these with a value: the call's UUID, else its number.
    Kind { name: "callState", rule: Rule::Call, key: &["callUuid", "remoteId"], cap: 4 },
    Kind { name: "location", rule: Rule::Last, key: &[], cap: 1 },
    Kind { name: "vehicleStatus", rule: Rule::Last, key: &[], cap: 1 },
];
/// The two binary kinds, after the JSON ones in [`Kept::kind`]; one of each is kept.
const ARTWORK: usize = KINDS.len();
const CORNERMASK: usize = KINDS.len() + 1;

fn kind_name(k: usize) -> &'static str {
    match k {
        ARTWORK => "artwork",
        CORNERMASK => "cornerMask",
        _ => KINDS[k].name,
    }
}

/// One record kept.
struct Kept {
    /// An index into [`KINDS`], or [`ARTWORK`] / [`CORNERMASK`].
    kind: usize,
    /// Its key's values as they came (raw JSON).
    key: Vec<u8>,
    /// Its top-level field names.
    fields: Vec<Vec<u8>>,
    /// The whole seam record.
    rec: Vec<u8>,
}

/// The seam's latest records of each kind a host acts on, and where the seam's stream is: inside a record, or between.
#[derive(Default)]
pub struct MetaKeep {
    /// The record under way, or up to 7 bytes that may begin one.
    buf: Vec<u8>,
    /// Bytes still to pass of a record too large to hold.
    skip: usize,
    kept: Vec<Kept>,
    /// A replay asked for inside a record, and why (for its log line once it goes).
    pending: Option<&'static str>,
}

impl MetaKeep {
    /// Everything goes: the session, or its seam's producer, is over.
    pub fn clear(&mut self) {
        *self = MetaKeep::default();
    }

    /// Between two records: a replay put into the stream now is read as records of its own.
    pub fn at_boundary(&self) -> bool {
        self.buf.is_empty() && self.skip == 0
    }

    /// A replay waits for the end of the record under way ([`MetaKeep::feed`] says where it ends).
    pub fn defer(&mut self, why: &'static str) {
        self.pending = Some(why);
    }

    /// The host that asked for a waiting replay is gone.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Seam bytes as they come, split anywhere. A replay waiting: they are read up to the end of the record under way
    /// and no further, and `Some((n, why))` says the replay goes after the first `n` bytes of `b` (the rest is for the
    /// next call). Otherwise None: all of `b` read.
    pub fn feed(&mut self, b: &[u8]) -> Option<(usize, &'static str)> {
        let mut i = 0;
        while i < b.len() {
            if self.skip > 0 {
                let n = self.skip.min(b.len() - i);
                self.skip -= n;
                i += n;
            } else {
                // Enough for the header, or for the rest of the record it names.
                let want = if self.buf.len() < 8 { 8 - self.buf.len() } else { 8 + self.len() - self.buf.len() };
                let n = want.min(b.len() - i);
                self.buf.extend_from_slice(&b[i..i + n]);
                i += n;
                self.step();
            }
            if self.at_boundary() {
                if let Some(why) = self.pending.take() {
                    return Some((i, why));
                }
            }
        }
        None
    }

    /// The length the header in `buf` names (the marker and the payload).
    fn len(&self) -> usize {
        u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) as usize
    }

    /// What `buf` holds now, read as the host's reader reads it.
    fn step(&mut self) {
        while self.buf.len() >= 8 {
            let len = self.len();
            if &self.buf[..4] != MAGIC || len == 0 || len > LEN_MAX {
                self.buf.remove(0); // not a header: one byte on
                continue;
            }
            if 8 + len > RECORD_MAX {
                self.skip = 8 + len - self.buf.len();
                self.buf.clear();
            } else if self.buf.len() == 8 + len {
                let rec = std::mem::take(&mut self.buf);
                self.keep(rec);
            }
            return;
        }
    }

    /// A whole record: kept when it is of a kind a host acts on.
    fn keep(&mut self, rec: Vec<u8>) {
        let payload = &rec[9.min(rec.len())..];
        let (kind, key, fields) = match rec.get(8) {
            Some(&META_ARTWORK) if payload.len() > 1 => (ARTWORK, Vec::new(), Vec::new()),
            Some(&META_CORNERMASK) if payload.len() <= MASK_MAX => (CORNERMASK, Vec::new(), Vec::new()),
            Some(&META_JSON) => match self.json(payload) {
                Some(k) => k,
                None => return,
            },
            _ => return,
        };
        let merge = kind < KINDS.len() && KINDS[kind].rule == Rule::Merge;
        self.kept.retain(|k| k.kind != kind || k.key != key || (merge && !k.fields.iter().all(|f| fields.contains(f))));
        self.kept.push(Kept { kind, key, fields, rec });
        let cap = KINDS.get(kind).map_or(1, |d| d.cap);
        while self.kept.iter().filter(|k| k.kind == kind).count() > cap {
            let oldest = self.kept.iter().position(|k| k.kind == kind).unwrap_or(0);
            self.kept.remove(oldest);
        }
    }

    /// A JSON record: `(kind, key, fields)` when it is to be kept. An ended call, the idle call record and a record too
    /// large take the older ones they stand for with them, and are not kept themselves.
    fn json(&mut self, payload: &[u8]) -> Option<(usize, Vec<u8>, Vec<Vec<u8>>)> {
        let m = members(payload)?;
        let value = |name: &str| m.iter().find(|(k, _)| *k == name.as_bytes()).map(|(_, v)| *v);
        let kind = KINDS.iter().position(|d| value("kind").is_some_and(|v| is_string(v, d.name)))?;
        let d = &KINDS[kind];
        let mut key = Vec::new();
        for &f in d.key {
            match value(f) {
                Some(v) if d.rule == Rule::Call => {
                    if key.is_empty() && v.len() > 2 {
                        key = v.to_vec();
                    }
                }
                Some(v) => {
                    key.extend_from_slice(v);
                    key.push(b',');
                }
                None => {}
            }
        }
        if d.rule == Rule::Call {
            let status = value("status")?; // a record with no status is no call state (the host skips it too)
            if status == b"0" || status == b"6" {
                let all = key.is_empty() && status == b"0";
                self.kept.retain(|k| k.kind != kind || (!all && k.key != key));
                return None;
            }
        }
        if payload.len() > JSON_MAX {
            self.kept.retain(|k| k.kind != kind || k.key != key);
            return None;
        }
        Some((kind, key, m.iter().map(|(k, _)| k.to_vec()).collect()))
    }

    /// What is kept, oldest first, as seam bytes.
    pub fn replay(&self) -> Vec<u8> {
        self.kept.iter().flat_map(|k| k.rec.iter().copied()).collect()
    }

    /// "nowPlaying x2, location, cornerMask", in the order of each kind's first record, for the log.
    pub fn kinds(&self) -> String {
        let mut v: Vec<(usize, usize)> = Vec::new();
        for k in &self.kept {
            match v.iter_mut().find(|(n, _)| *n == k.kind) {
                Some((_, c)) => *c += 1,
                None => v.push((k.kind, 1)),
            }
        }
        let part = |&(k, c): &(usize, usize)| match c {
            1 => kind_name(k).to_string(),
            _ => format!("{} x{c}", kind_name(k)),
        };
        v.iter().map(part).collect::<Vec<String>>().join(", ")
    }
}

/// A raw JSON value that is the string `s`.
fn is_string(v: &[u8], s: &str) -> bool {
    v.len() == s.len() + 2 && v[0] == b'"' && v[v.len() - 1] == b'"' && &v[1..v.len() - 1] == s.as_bytes()
}

/// The top-level members of a JSON object, `(name, raw value)`; None when `j` is not one.
fn members(j: &[u8]) -> Option<Vec<(&[u8], &[u8])>> {
    let mut i = ws(j, 0);
    if j.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    let mut out = Vec::new();
    loop {
        i = ws(j, i);
        match *j.get(i)? {
            b'}' => return Some(out),
            b',' => {
                i += 1;
                continue;
            }
            b'"' => {}
            _ => return None,
        }
        let name_end = string_end(j, i)?;
        let name = &j[i + 1..name_end - 1];
        i = ws(j, name_end);
        if j.get(i) != Some(&b':') {
            return None;
        }
        i = ws(j, i + 1);
        let end = value_end(j, i)?;
        out.push((name, &j[i..end]));
        i = end;
    }
}

fn ws(j: &[u8], mut i: usize) -> usize {
    while j.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// Past the string that opens at `i`, its closing quote included.
fn string_end(j: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    loop {
        match *j.get(i)? {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// Past the value that begins at `i`: a string, an object or an array (nested ones and strings inside skipped), or a
/// bare token (a number, true, false, null).
fn value_end(j: &[u8], mut i: usize) -> Option<usize> {
    match *j.get(i)? {
        b'"' => string_end(j, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            loop {
                match *j.get(i)? {
                    b'"' => {
                        i = string_end(j, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        _ => {
            let start = i;
            while j.get(i).is_some_and(|c| !matches!(c, b',' | b'}' | b']') && !c.is_ascii_whitespace()) {
                i += 1;
            }
            (i > start).then_some(i)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One seam record: `[u32 BE "META"][u32 BE len][marker][payload]`.
    fn rec(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut r = MAGIC.to_vec();
        r.extend_from_slice(&((1 + payload.len()) as u32).to_be_bytes());
        r.push(marker);
        r.extend_from_slice(payload);
        r
    }

    fn json(s: &str) -> Vec<u8> {
        rec(META_JSON, s.as_bytes())
    }

    /// Carlink's MetadataSeam as it reads the channel's bytes (`drain`): the records the host takes, (marker, payload).
    fn host_reads(b: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let (mut s, mut out) = (0usize, Vec::new());
        while b.len() - s >= 8 {
            if &b[s..s + 4] != MAGIC {
                s += 1;
                continue;
            }
            let len = u32::from_be_bytes([b[s + 4], b[s + 5], b[s + 6], b[s + 7]]) as usize;
            if len == 0 || len > LEN_MAX {
                s += 1;
                continue;
            }
            if b.len() - s < 8 + len {
                break;
            }
            if len > 1 {
                out.push((b[s + 8], b[s + 9..s + 8 + len].to_vec()));
            }
            s += 8 + len;
        }
        out
    }

    /// The host's field-by-field merge of nowPlaying deltas: each field's last value, and the last title's place.
    fn merged(records: &[(u8, Vec<u8>)]) -> std::collections::BTreeMap<Vec<u8>, Vec<u8>> {
        let mut m = std::collections::BTreeMap::new();
        for (_, p) in records {
            for (k, v) in members(p).unwrap() {
                m.insert(k.to_vec(), v.to_vec());
            }
        }
        m
    }

    fn feed_in_pieces(k: &mut MetaKeep, b: &[u8], piece: usize) {
        for c in b.chunks(piece) {
            assert!(k.feed(c).is_none());
        }
    }

    #[test]
    fn now_playing_deltas_are_kept_so_the_replay_merges_to_the_live_state() {
        let live = [
            json(r#"{"kind":"nowPlaying","title":"A","artist":"X","album":"Y","durationMs":1,"playbackStatus":1}"#),
            json(r#"{"kind":"nowPlaying","elapsedMs":1000}"#),
            json(r#"{"kind":"nowPlaying","elapsedMs":2000}"#),
            json(r#"{"kind":"nowPlaying","album":"Z"}"#), // the next album just before its title (the phone does)
            json(r#"{"kind":"nowPlaying","title":"B","artworkId":2,"durationMs":2}"#),
            json(r#"{"kind":"nowPlaying","elapsedMs":10,"playbackStatus":2}"#),
            json(r#"{"kind":"nowPlaying","elapsedMs":20,"playbackStatus":1}"#),
        ];
        let mut k = MetaKeep::default();
        let mut stream = b"xxMET".to_vec(); // a torn record's tail ahead of the first: passed by, as the host does
        for r in &live {
            stream.extend_from_slice(r);
        }
        feed_in_pieces(&mut k, &stream, 5);
        let replay = k.replay();
        assert_eq!(replay, [&live[0][..], &live[3][..], &live[4][..], &live[6][..]].concat(), "the superseded ones go");
        assert_eq!(k.kinds(), "nowPlaying x4");
        let (all, kept) = (host_reads(&stream), host_reads(&replay));
        assert_eq!(merged(&kept), merged(&all), "the same state, field by field");
        fn title_of(p: &[u8]) -> Option<Vec<u8>> {
            members(p)?.into_iter().find(|(n, _)| *n == b"title").map(|(_, v)| v.to_vec())
        }
        let title = |r: &[(u8, Vec<u8>)]| r.iter().rev().find_map(|(_, p)| title_of(p));
        assert_eq!(title(&kept), Some(b"\"B\"".to_vec()), "the last title last, after the album that came before it");
        // Ticks after ticks: one stands for them all; the cap bounds the rest.
        for e in 0..40 {
            k.feed(&json(&format!(r#"{{"kind":"nowPlaying","elapsedMs":{e}}}"#)));
        }
        assert_eq!(k.kept.len(), 5, "the four, and the last tick");
        for e in 0..40 {
            k.feed(&json(&format!(r#"{{"kind":"nowPlaying","f{e}":1}}"#)));
        }
        assert_eq!(k.kept.len(), KINDS[0].cap, "bounded");
    }

    #[test]
    fn navigation_is_kept_per_maneuver_and_lane_in_the_order_it_came() {
        let m = |i: u32, d: &str| json(&format!(r#"{{"kind":"maneuver","index":{i},"description":"{d}","a":[1]}}"#));
        let lane = |g: u32, i: u32, n: u32| {
            let j = format!(r#"{{"kind":"laneGuidance","guidanceIndex":{g},"index":{i},"laneStatus":1,"n":{n}}}"#);
            json(&j)
        };
        let rg1 = r#"{"kind":"routeGuidance","routeGuidanceState":1,"destinationName":"D","currentManeuverIndex":0}"#;
        let rg1 = json(rg1);
        let rg2 = json(r#"{"kind":"routeGuidance","distanceToManeuverM":120,"currentManeuverIndex":1}"#);
        let rg3 = json(r#"{"kind":"routeGuidance","distanceToManeuverM":80,"currentManeuverIndex":1}"#);
        let mut k = MetaKeep::default();
        let came = [
            rg1.clone(), m(0, "a"), m(1, "b"), m(2, "c"), lane(5, 0, 3), lane(5, 1, 3), lane(5, 2, 3),
            rg2, m(1, "b2"), lane(5, 0, 2), lane(5, 1, 2), rg3.clone(),
        ];
        for r in &came {
            k.feed(r);
        }
        let want = [rg1, m(0, "a"), m(2, "c"), lane(5, 2, 3), m(1, "b2"), lane(5, 0, 2), lane(5, 1, 2), rg3].concat();
        assert_eq!(k.replay(), want, "the route's name stays; a maneuver or lane sent again replaces its old record");
        assert_eq!(k.kinds(), "routeGuidance x2, maneuver x3, laneGuidance x3");
        for i in 0..60 {
            k.feed(&m(100 + i, "x"));
        }
        assert_eq!(k.kept.iter().filter(|r| kind_name(r.kind) == "maneuver").count(), 48, "capped, the oldest gone");
    }

    #[test]
    fn calls_are_kept_while_they_last_and_requests_and_markers_as_their_last() {
        let call = |id: &str, st: u32| {
            json(&format!(r#"{{"kind":"callState","remoteId":"5","status":{st},"direction":1,"callUuid":"{id}"}}"#))
        };
        let mut k = MetaKeep::default();
        for r in [call("a", 2), call("a", 4), call("b", 2)] {
            k.feed(&r);
        }
        assert_eq!(k.replay(), [call("a", 4), call("b", 2)].concat(), "each call's last record");
        k.feed(&call("b", 0));
        assert_eq!(k.replay(), call("a", 4), "an ended call goes");
        k.feed(&json(r#"{"kind":"callState","status":0}"#));
        assert!(k.replay().is_empty(), "the idle record ends them all");
        let by_number = json(r#"{"kind":"callState","remoteId":"7","status":4}"#);
        k.feed(&by_number);
        k.feed(&json(r#"{"kind":"callState","displayName":"no status"}"#));
        assert_eq!(k.replay(), by_number, "a call with no UUID by its number; a record with no status is none");

        let mut k = MetaKeep::default();
        let start = json(r#"{"kind":"location","state":"start","sentences":["GPGGA"]}"#);
        let stop = json(r#"{"kind":"location","state":"stop"}"#);
        let veh = json(r#"{"kind":"vehicleStatus","state":"start","want":[3,4]}"#);
        let (art1, art2) = (rec(META_ARTWORK, &[1, 0xFF, 0xD8]), rec(META_ARTWORK, &[2, 0xFF, 0xD8]));
        let mask = rec(META_CORNERMASK, &[0, 0, 9, 96, 0x89, b'P']);
        for r in [&start, &art1, &mask, &veh, &stop, &art2] {
            k.feed(r);
        }
        assert_eq!(k.replay(), [&mask[..], &veh[..], &stop[..], &art2[..]].concat(), "the last of each");
        assert_eq!(k.kinds(), "cornerMask, vehicleStatus, location, artwork");
    }

    #[test]
    fn what_no_host_should_have_again_is_not_kept() {
        let mut k = MetaKeep::default();
        for r in [
            rec(0x01, b"bplist00 requestUI"),
            json(r#"{"kind":"dataStream","stream":3,"client":"A6B27562","message":{"type":"playbackInfo"}}"#),
            json(r#"{"kind":"recentCall","index":0,"displayName":"x"}"#),
            json(r#"{"kind":"appIcon","id":"x"}"#),
            json(r#"not json"#),
            rec(META_ARTWORK, &[1]),
        ] {
            k.feed(&r);
        }
        assert!(k.replay().is_empty(), "{}", k.kinds());
        // A record too large to keep takes the stale one of its kind and key with it.
        k.feed(&json(r#"{"kind":"nowPlaying","title":"old"}"#));
        let big = format!(r#"{{"kind":"nowPlaying","title":"{}"}}"#, "t".repeat(JSON_MAX));
        k.feed(&json(&big));
        assert!(k.replay().is_empty());
    }

    #[test]
    fn a_replay_asked_for_inside_a_record_goes_at_its_end_and_the_host_reads_every_record() {
        let r1 = json(r#"{"kind":"location","state":"start"}"#);
        let r2 = json(r#"{"kind":"nowPlaying","title":"T","playbackStatus":1}"#);
        let r3 = json(r#"{"kind":"nowPlaying","elapsedMs":5}"#);
        let mut k = MetaKeep::default();
        assert!(k.at_boundary());
        let first = [&r1[..], &r2[..10]].concat(); // the host's SUBSCRIBE comes with r2 half through
        assert!(k.feed(&first).is_none());
        assert!(!k.at_boundary());
        k.defer("attach");
        let second = [&r2[10..], &r3[..]].concat();
        let (cut, why) = k.feed(&second).expect("the replay's place");
        assert_eq!((cut, why), (r2.len() - 10, "attach"), "right after r2");
        let replay = k.replay();
        assert_eq!(replay, [&r1[..], &r2[..]].concat(), "r2 in it, r3 not yet (it is still to come)");
        assert!(k.feed(&second[cut..]).is_none(), "the rest read as usual");
        let to_host = [&first[..], &second[..cut], &replay[..], &second[cut..]].concat();
        let read: Vec<Vec<u8>> = host_reads(&to_host).into_iter().map(|(_, p)| p).collect();
        let p = |r: &[u8]| r[9..].to_vec();
        assert_eq!(read, [p(&r1), p(&r2), p(&r1), p(&r2), p(&r3)], "every record whole: the live ones, the replay's");
        // Contrast: put in at once, inside r2, the replay is read as r2's bytes and lost with them.
        let naive = [&first[..], &replay[..], &second[..]].concat();
        assert_eq!(host_reads(&naive).iter().filter(|(_, x)| *x == p(&r1)).count(), 1, "the replay's r1 eaten");
    }

    #[test]
    fn a_record_too_large_to_hold_is_passed_by_and_the_stream_followed_through_it() {
        let big = rec(META_ARTWORK, &vec![7u8; RECORD_MAX]);
        let after = json(r#"{"kind":"location","state":"start"}"#);
        let mut k = MetaKeep::default();
        k.feed(&big[..20_000]);
        assert!(!k.at_boundary() && k.buf.is_empty(), "passing it by, holding none of it");
        k.defer("attach");
        let rest = [&big[20_000..], &after[..]].concat();
        let (cut, _) = k.feed(&rest).expect("at its end");
        assert_eq!(cut, big.len() - 20_000);
        assert!(k.feed(&rest[cut..]).is_none());
        assert_eq!(k.replay(), after, "the next record found and kept");
        // False magics (a zero length) are stepped over one byte at a time, as the host does.
        let mut k = MetaKeep::default();
        feed_in_pieces(&mut k, &[&b"META\0\0\0\0"[..], &after[..]].concat(), 3);
        assert_eq!(k.replay(), after);
        assert!(k.at_boundary());
        k.clear();
        assert!(k.replay().is_empty() && k.at_boundary());
    }

    #[test]
    fn members_reads_the_producers_json() {
        let j = br#"{"kind":"x","s":"a \"q\" ,}","n":-12,"a":[1,[2],{"b":"]"}],"o":{"k":"}"},"t":true}"#;
        let m = members(j).unwrap();
        let names: Vec<&[u8]> = m.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, [&b"kind"[..], &b"s"[..], &b"n"[..], &b"a"[..], &b"o"[..], &b"t"[..]]);
        assert_eq!(m[1].1, br#""a \"q\" ,}""#);
        assert_eq!(m[3].1, br#"[1,[2],{"b":"]"}]"#);
        assert!(members(b"{\"a\":").is_none() && members(b"[1]").is_none() && members(b"{\"a\" 1}").is_none());
    }
}
