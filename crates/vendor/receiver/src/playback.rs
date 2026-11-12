//! Route A's relay of the phone's shared DataStreams between the iPhone and the host app: the video Playback
//! channel (UrlFling, `A6B27562`, LIVI's MatchPoint) and any other the phone opens without a socket of its own.
//! The box only carries and converts. The play-queue logic and the player are the host's, as anything
//! configurable about CarPlay is (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
//!
//! A shared DataStream rides the control and event connections (LIVI's receiver plays it; the Mac's 10 Nov
//! probe bench saw the phone choose it): the phone sends each message as a `POST /command` whose
//! `X-Apple-StreamID` header names the stream and whose body is `{params: {data: <binary plist>}}`, and the
//! receiver answers the same way on the event channel. Each inbound message reaches the host as a JSON record,
//! `{"kind":"dataStream","stream":N,"client":"<clientTypeUUID>","message":{...}}`. The host's answers come back
//! as `INPUT_DATASTREAM` sub-frames, `[stream u32 LE][JSON]` (carplayd strips the type byte), which
//! [`send_reply`] turns back into a binary plist for the phone.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};

use plist::{Dictionary, Value};

/// Whether video may play, as the host last said (ocbm-proto's `CMD_VIDEO_ALLOWED`): 0 not said yet, 1 no, 2 yes.
/// Kept across a session's end: the host says it again at each session's start, and on every change.
static HOST_SAYS: AtomicU8 = AtomicU8::new(0);

/// This session negotiated videoPlayback and is past RECORD: the phone hears each word from the host as it comes.
static TELLING: AtomicBool = AtomicBool::new(false);

/// The host's word on whether video may play (it plays only while the car is known to be parked), with the play
/// queue's `status` word (`idle`, `video` or `audioOnly`, LIVI's words; carplayd maps the wire byte). Told to the
/// phone at once in a session past RECORD that negotiated videoPlayback, else kept for RECORD. True when it went
/// out now.
pub fn host_allows(allowed: bool, status: &str) -> bool {
    HOST_SAYS.store(if allowed { 2 } else { 1 }, Ordering::SeqCst);
    if !TELLING.load(Ordering::SeqCst) {
        crate::probe::line(format_args!("the host says video {}: kept for RECORD", if allowed { "may play" } else { "may not play" }));
        return false;
    }
    tell_allowed(allowed, status)
}

/// RECORD, in a session that negotiated videoPlayback: the phone hears the host's
/// word from now on, starting with the one already given (status `idle`, as LIVI sends at RECORD: nothing is
/// queued yet). Until the host has said, nothing goes out.
pub fn on_record() {
    TELLING.store(true, Ordering::SeqCst);
    match HOST_SAYS.load(Ordering::SeqCst) {
        0 => crate::probe::line(format_args!("RECORD: setVideoPlaybackAllowed waits for the host's word")),
        said => {
            tell_allowed(said == 2, "idle");
        }
    }
}

/// The session ended: the next one tells the phone again from its own RECORD.
pub fn session_ended() {
    TELLING.store(false, Ordering::SeqCst);
}

fn tell_allowed(allowed: bool, status: &str) -> bool {
    let sent = crate::events::send_command(&crate::probe::allowed_command(allowed, status));
    crate::probe::line(format_args!("setVideoPlaybackAllowed({allowed}, {status}) sent={sent}"));
    sent
}

/// The stream a `POST /command` belongs to, from its `X-Apple-StreamID` header: a shared DataStream's message.
pub fn stream_of(req: &rtsp::message::Request) -> Option<i64> {
    let v = req.header("X-Apple-StreamID")?.trim();
    v.parse::<i64>().ok().or_else(|| v.parse::<u64>().ok().and_then(|u| i64::try_from(u).ok()))
}

/// One message from the phone on shared DataStream `stream`, the body of its `POST /command`. Forwarded to
/// the host as a JSON record; the reply to the POST is the empty dictionary the reference sends for a command.
pub fn on_phone_message(stream: i64, client: Option<&str>, body: &[u8]) -> Vec<u8> {
    let name = client.map_or("unknown client type", crate::probe::channel_name);
    let message = plist::Value::from_reader(std::io::Cursor::new(body))
        .ok()
        .and_then(|v| v.into_dictionary())
        .and_then(|d| crate::probe::dict_data(&d).map(<[u8]>::to_vec))
        .and_then(|data| plist::Value::from_reader(std::io::Cursor::new(data)).ok());
    match message {
        Some(m) => {
            crate::probe::line(format_args!("DataStream {name}, stream {stream}, to the host: {}", crate::probe::shape(&m)));
            forward(record(stream, client, &m));
        }
        None => crate::probe::line(format_args!(
            "DataStream {name}, stream {stream}: a {} B command with no plist in its params.data — not forwarded",
            body.len()
        )),
    }
    let mut out = Vec::new();
    let _ = Value::Dictionary(Dictionary::new()).to_writer_binary(&mut out);
    out
}

/// The host's answer on shared DataStream `stream`: `[stream u32 LE][JSON]`, sent to the phone as a binary
/// plist in `{params: {data}}` on the event channel. False when it is malformed or couldn't be sent.
pub fn send_reply(pl: &[u8]) -> bool {
    let Some((stream, json)) = pl.split_first_chunk::<4>().map(|(s, j)| (u32::from_le_bytes(*s), j)) else {
        crate::probe::line(format_args!("a DataStream reply of {} B with no stream id — dropped", pl.len()));
        return false;
    };
    let Some(message) = std::str::from_utf8(json).ok().and_then(from_json).filter(|m| m.as_dictionary().is_some()) else {
        crate::probe::line(format_args!("a DataStream reply for stream {stream} that isn't a JSON object ({} B) — dropped", json.len()));
        return false;
    };
    let mut data = Vec::new();
    if message.to_writer_binary(&mut data).is_err() {
        return false;
    }
    let sent = crate::events::send_stream_message(i64::from(stream), &data);
    // Shape only, no byte count, so a poll's answers repeat word for word and are counted, not logged each time.
    crate::probe::line(format_args!("DataStream stream {stream}, from the host: {} sent={sent}", crate::probe::shape(&message)));
    sent
}

/// The record for the host: `{"kind":"dataStream","stream":N,"client":"<uuid>","message":{...}}`.
pub fn record(stream: i64, client: Option<&str>, message: &Value) -> String {
    let client = client.map_or("null".to_string(), json_string);
    format!(r#"{{"kind":"dataStream","stream":{stream},"client":{client},"message":{}}}"#, to_json(message))
}

/// Hand a record to the metadata seam on a thread of its own, in order. `emit_json` can wait seconds on a busy
/// seam, and the phone's messages arrive on the RTSP control thread, which must never stall.
fn forward(record: String) {
    static TX: OnceLock<Mutex<Option<Sender<String>>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<String>();
        let spawned = std::thread::Builder::new().name("datastream-relay".into()).spawn(move || {
            for r in rx {
                iap2_core::metadata::emit_json(&r);
            }
        });
        Mutex::new(spawned.ok().map(|_| tx))
    });
    let sent = crate::plock(tx).as_ref().is_some_and(|tx| tx.send(record).is_ok());
    if !sent {
        crate::probe::line(format_args!("the DataStream relay thread is gone — a message for the host was dropped"));
    }
}

/// A plist value as JSON. Bytes become Base64 text, a date its Unix time, a real that isn't finite `null`.
pub fn to_json(v: &Value) -> String {
    match v {
        Value::Dictionary(d) => {
            let items: Vec<String> = d.iter().map(|(k, v)| format!("{}:{}", json_string(k), to_json(v))).collect();
            format!("{{{}}}", items.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(to_json).collect::<Vec<_>>().join(",")),
        Value::String(s) => json_string(s),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i
            .as_signed()
            .map(|n| n.to_string())
            .or_else(|| i.as_unsigned().map(|n| n.to_string()))
            .unwrap_or_else(|| "null".into()),
        Value::Real(r) if r.is_finite() => {
            let s = r.to_string();
            if s.contains(['.', 'e', 'E']) { s } else { format!("{s}.0") }
        }
        Value::Data(b) => json_string(&crate::probe::base64(b)),
        Value::Date(d) => std::time::SystemTime::from(*d)
            .duration_since(std::time::UNIX_EPOCH)
            .map_or_else(|_| "null".into(), |t| t.as_secs().to_string()),
        Value::Uid(u) => u.get().to_string(),
        _ => "null".into(),
    }
}

/// A JSON string literal: quotes, backslashes and control characters escaped.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON text as a plist value: an object a dictionary, an integer an integer, a number with a point or an
/// exponent a real. `null` has no plist form, so a key or an element holding it is left out. `None` for text
/// that isn't one JSON value, or is `null`. A reader of its own, small and strict: serde_yaml's generic
/// value reader, which also reads JSON, added ~100 KB to carplayd's armv7 binary.
pub fn from_json(text: &str) -> Option<Value> {
    let mut r = JsonReader { b: text.as_bytes(), i: 0, depth: 0 };
    let v = r.value().ok()?;
    r.ws();
    if r.i != r.b.len() {
        return None;
    }
    v
}

/// The nesting a reply may have: far beyond any answer, and well short of the stack.
const JSON_DEPTH: usize = 32;

struct JsonReader<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl JsonReader<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), ()> {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    /// One value; `Ok(None)` for `null`.
    fn value(&mut self) -> Result<Option<Value>, ()> {
        self.ws();
        match *self.b.get(self.i).ok_or(())? {
            b'{' => self.nested(Self::object),
            b'[' => self.nested(Self::array),
            b'"' => self.string().map(|s| Some(Value::String(s))),
            b't' => self.word(b"true", Some(Value::Boolean(true))),
            b'f' => self.word(b"false", Some(Value::Boolean(false))),
            b'n' => self.word(b"null", None),
            _ => self.number().map(Some),
        }
    }

    fn nested(&mut self, f: fn(&mut Self) -> Result<Value, ()>) -> Result<Option<Value>, ()> {
        if self.depth == JSON_DEPTH {
            return Err(());
        }
        self.depth += 1;
        let v = f(self);
        self.depth -= 1;
        v.map(Some)
    }

    fn object(&mut self) -> Result<Value, ()> {
        self.eat(b'{')?;
        let mut d = Dictionary::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Value::Dictionary(d));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.eat(b':')?;
            if let Some(v) = self.value()? {
                d.insert(k, v);
            }
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Dictionary(d));
                }
                _ => return Err(()),
            }
        }
    }

    fn array(&mut self) -> Result<Value, ()> {
        self.eat(b'[')?;
        let mut a = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Value::Array(a));
        }
        loop {
            if let Some(v) = self.value()? {
                a.push(v);
            }
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Array(a));
                }
                _ => return Err(()),
            }
        }
    }

    fn word(&mut self, w: &[u8], v: Option<Value>) -> Result<Option<Value>, ()> {
        if self.b.get(self.i..self.i + w.len()) == Some(w) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(())
        }
    }

    fn hex4(&mut self) -> Result<u32, ()> {
        let h = self.b.get(self.i..self.i + 4).ok_or(())?;
        let s = std::str::from_utf8(h).map_err(|_| ())?;
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|_| ())
    }

    fn string(&mut self) -> Result<String, ()> {
        if self.b.get(self.i) != Some(&b'"') {
            return Err(());
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or(())?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| ()),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or(())?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) {
                                // A surrogate pair: the low half must follow as its own \u escape.
                                if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err(());
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(());
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            char::from_u32(code).ok_or(())?
                        }
                        _ => return Err(()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err(()),
                c => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Result<Value, ()> {
        let start = self.i;
        while matches!(self.b.get(self.i), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| ())?;
        if s.is_empty() {
            return Err(());
        }
        if s.contains(['.', 'e', 'E']) {
            return s.parse::<f64>().map(Value::Real).map_err(|_| ());
        }
        s.parse::<i64>()
            .map(|n| Value::Integer(n.into()))
            .or_else(|_| s.parse::<u64>().map(|n| Value::Integer(n.into())))
            .map_err(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hosts_word_on_video_waits_for_record_then_goes_out_at_each_change() {
        session_ended();
        assert!(!host_allows(false, "idle"), "before RECORD the word is kept, not sent");
        assert_eq!(HOST_SAYS.load(Ordering::SeqCst), 1);
        on_record();
        assert!(TELLING.load(Ordering::SeqCst), "from RECORD on the phone hears each word");
        // No event channel in a test, so the send itself fails: what matters is that it was tried.
        assert!(!host_allows(true, "video"));
        assert_eq!(HOST_SAYS.load(Ordering::SeqCst), 2, "the last word is kept for the next session");
        session_ended();
        assert!(!TELLING.load(Ordering::SeqCst));
        assert_eq!(HOST_SAYS.load(Ordering::SeqCst), 2);
    }

    fn insert_item() -> Value {
        let mut item = Dictionary::new();
        item.insert("uuid".into(), Value::String("item-1".into()));
        item.insert("Start-Position".into(), Value::Real(0.0));
        item.insert("Content-Location".into(), Value::String("https://cdn.example.com/v/master.m3u8".into()));
        let mut m = Dictionary::new();
        m.insert("type".into(), Value::String("insertPlayQueueItem".into()));
        m.insert("kind".into(), Value::String("request".into()));
        m.insert("messageID".into(), Value::Integer(7.into()));
        m.insert("item".into(), Value::Dictionary(item));
        m.insert("blob".into(), Value::Data(vec![1, 2, 3]));
        m.insert("ok".into(), Value::Boolean(true));
        Value::Dictionary(m)
    }

    #[test]
    fn a_message_goes_to_the_host_as_json_and_back() {
        let m = insert_item();
        let json = to_json(&m);
        assert_eq!(
            json,
            r#"{"type":"insertPlayQueueItem","kind":"request","messageID":7,"item":{"uuid":"item-1","Start-Position":0.0,"Content-Location":"https://cdn.example.com/v/master.m3u8"},"blob":"AQID","ok":true}"#
        );
        let back = from_json(&json).unwrap();
        let d = back.as_dictionary().unwrap();
        assert_eq!(d.get("messageID"), Some(&Value::Integer(7.into())), "an integer stays an integer");
        let item = d.get("item").and_then(Value::as_dictionary).unwrap();
        assert_eq!(item.get("Start-Position"), Some(&Value::Real(0.0)), "a real stays a real");
        assert_eq!(d.get("blob").and_then(Value::as_string), Some("AQID"), "bytes come back as their Base64 text");
    }

    #[test]
    fn the_hosts_answer_takes_reals_lists_and_leaves_out_nulls() {
        let json = r#"{"kind":"response","type":"playbackInfo","messageID":7,
            "value":{"rate":1.0,"position":12.5,"readyToPlay":false,"uuid":null,
                     "seekableTimeRanges":[{"start":0.0,"duration":30.0}]},"note":"\"quoted\" é"}"#;
        let v = from_json(json).unwrap();
        let d = v.as_dictionary().unwrap();
        let value = d.get("value").and_then(Value::as_dictionary).unwrap();
        assert_eq!(value.get("rate"), Some(&Value::Real(1.0)));
        assert_eq!(value.get("readyToPlay"), Some(&Value::Boolean(false)));
        assert!(!value.contains_key("uuid"), "null has no plist form");
        let ranges = value.get("seekableTimeRanges").and_then(Value::as_array).unwrap();
        assert_eq!(ranges[0].as_dictionary().and_then(|r| r.get("duration")), Some(&Value::Real(30.0)));
        assert_eq!(d.get("note").and_then(Value::as_string), Some("\"quoted\" é"));
        assert!(from_json(r#"{"a": [1, 2"#).is_none(), "unterminated");
    }

    #[test]
    fn the_json_reader_is_strict_and_complete() {
        assert_eq!(from_json(r#""😀 é \"q\" \\ \/""#), Some(Value::String("😀 é \"q\" \\ /".into())));
        assert_eq!(from_json("-12"), Some(Value::Integer((-12).into())));
        assert_eq!(from_json("18446744073709551615"), Some(Value::Integer(u64::MAX.into())));
        assert_eq!(from_json("1.5e3"), Some(Value::Real(1500.0)));
        assert_eq!(
            from_json(" [1, null, true] "),
            Some(Value::Array(vec![Value::Integer(1.into()), Value::Boolean(true)])),
            "a null element is left out"
        );
        assert_eq!(from_json("null"), None);
        assert_eq!(from_json("{} x"), None, "trailing text");
        assert_eq!(from_json(r#"{"a":1,}"#), None, "a trailing comma");
        assert_eq!(from_json(r#""\ud83d""#), None, "half a surrogate pair");
        assert_eq!(from_json("\"a\u{1}\""), None, "a raw control character");
        assert_eq!(from_json(&("[".repeat(40) + &"]".repeat(40))), None, "deeper than 32");
        assert!(from_json(&("[".repeat(20) + &"]".repeat(20))).is_some());
    }

    #[test]
    fn the_record_names_the_stream_and_its_client() {
        let mut m = Dictionary::new();
        m.insert("type".into(), Value::String("stop".into()));
        let r = record(3, Some("A6B27562-B43A-4F2D-B75F-82391E250194"), &Value::Dictionary(m.clone()));
        assert_eq!(
            r,
            r#"{"kind":"dataStream","stream":3,"client":"A6B27562-B43A-4F2D-B75F-82391E250194","message":{"type":"stop"}}"#
        );
        assert_eq!(record(4, None, &Value::Dictionary(m)), r#"{"kind":"dataStream","stream":4,"client":null,"message":{"type":"stop"}}"#);
    }

    #[test]
    fn the_stream_comes_from_its_header() {
        let req = |h: Option<&str>| {
            let mut raw = b"POST /command RTSP/1.0\r\nCSeq: 5\r\n".to_vec();
            if let Some(v) = h {
                raw.extend(format!("X-Apple-StreamID: {v}\r\n").as_bytes());
            }
            raw.extend(b"Content-Length: 0\r\n\r\n");
            rtsp::message::Request::parse(&raw).unwrap().unwrap().0
        };
        assert_eq!(stream_of(&req(Some("3"))), Some(3));
        assert_eq!(stream_of(&req(Some(" 18446744073709551615 "))), None, "past i64");
        assert_eq!(stream_of(&req(None)), None, "an ordinary command");
        assert_eq!(stream_of(&req(Some("x"))), None);
    }

    #[test]
    fn a_phone_message_is_answered_with_an_empty_dictionary() {
        let mut inner = Vec::new();
        insert_item().to_writer_binary(&mut inner).unwrap();
        let mut params = Dictionary::new();
        params.insert("data".into(), Value::Data(inner));
        let mut cmd = Dictionary::new();
        cmd.insert("params".into(), Value::Dictionary(params));
        let mut body = Vec::new();
        Value::Dictionary(cmd).to_writer_binary(&mut body).unwrap();
        let resp = on_phone_message(3, Some("A6B27562-B43A-4F2D-B75F-82391E250194"), &body);
        let v = Value::from_reader(std::io::Cursor::new(resp)).unwrap();
        assert_eq!(v.as_dictionary().map(Dictionary::len), Some(0));
    }
}
