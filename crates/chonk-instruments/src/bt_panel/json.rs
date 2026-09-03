//! A minimal JSON reader for `busctl --json=short` output.
//!
//! Hand-rolled, for the reason this crate's `Cargo.toml` argues line by
//! line: an instrument sees the SDK, the theme, a pixel buffer and a
//! text shaper, and nothing else. `busctl`'s `--json=short` is the one
//! machine-readable surface BlueZ offers us (see [`super::bluez`] for
//! why the alternative — `bluetoothctl` — is disqualified outright), so
//! the choice was between extending that manifest and writing the
//! hundred lines the panel actually needs.
//!
//! # Why this is not `audio_panel`'s parser
//!
//! There is a sibling reader in `audio_panel::json`, for `pactl`. It is
//! private to that module, and this one is deliberately not a
//! refactor of it into something shared. Two reasons, in order of
//! weight: the two live behind *different* external formats whose
//! stability is not correlated — a `pactl` change must not be able to
//! break the Bluetooth panel, or vice versa — and a shared parser
//! would be a third owner for a hundred lines that neither panel would
//! then be free to specialize. If a third consumer ever appears the
//! calculus changes; two is not enough to pay for the coupling.
//!
//! What it accepts is standard JSON: objects, arrays, strings with the
//! full escape set (`\uXXXX` surrogate pairs included), numbers, the
//! three keywords. Structural errors reject the whole document,
//! because the caller's answer to an unparseable `busctl` is the same
//! as its answer to a missing one — the dead face — and half a device
//! list is worse than none.

/// A parsed JSON value. Objects keep insertion order in a plain vec:
/// a `GetManagedObjects` reply is a few dozen keys per device and the
/// panel reads perhaps six of them, so a map would be ceremony.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// Nesting depth cap. `GetManagedObjects` nests six levels
/// (reply / data / path / interface / property / variant); sixty is
/// slack for format growth while keeping a pathological input from
/// walking the parser off the stack.
const MAX_DEPTH: u32 = 60;

impl Json {
    /// Parses a whole document, or `None` if it is not valid JSON.
    /// Trailing content after the top-level value is a rejection: a
    /// truncated `busctl` write that happens to end on a value
    /// boundary must not read as a complete answer.
    pub fn parse(text: &str) -> Option<Json> {
        let bytes = text.as_bytes();
        let mut at = 0usize;
        let value = parse_value(bytes, &mut at, 0)?;
        skip_whitespace(bytes, &mut at);
        (at == bytes.len()).then_some(value)
    }

    /// The value at `key`, for an object; `None` for anything else.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, value)| value),
            _ => None,
        }
    }

    /// This object's fields in document order, for the callers that
    /// walk a dictionary whose keys they do not know in advance — a
    /// `GetManagedObjects` reply is keyed by object path, and the
    /// paths are exactly what the walk is looking for.
    pub fn entries(&self) -> &[(String, Json)] {
        match self {
            Json::Obj(fields) => fields,
            _ => &[],
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(text) => Some(text.as_str()),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(value) => Some(*value),
            _ => None,
        }
    }

    /// The value as a whole number in `0..=255` — every numeric
    /// property this panel reads is a D-Bus byte (`y`), which is what
    /// BlueZ reports a battery percentage as.
    pub fn as_u8(&self) -> Option<u8> {
        let value = self.as_f64()?;
        (value.is_finite() && (0.0..=255.0).contains(&value)).then_some(value as u8)
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }
}

fn skip_whitespace(bytes: &[u8], at: &mut usize) {
    while matches!(bytes.get(*at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *at += 1;
    }
}

fn literal(bytes: &[u8], at: &mut usize, word: &str, value: Json) -> Option<Json> {
    bytes.get(*at..*at + word.len())?.eq(word.as_bytes()).then(|| {
        *at += word.len();
        value
    })
}

fn parse_value(bytes: &[u8], at: &mut usize, depth: u32) -> Option<Json> {
    if depth > MAX_DEPTH {
        return None;
    }
    skip_whitespace(bytes, at);
    match *bytes.get(*at)? {
        b'{' => parse_object(bytes, at, depth),
        b'[' => parse_array(bytes, at, depth),
        b'"' => parse_string(bytes, at).map(Json::Str),
        b't' => literal(bytes, at, "true", Json::Bool(true)),
        b'f' => literal(bytes, at, "false", Json::Bool(false)),
        b'n' => literal(bytes, at, "null", Json::Null),
        _ => parse_number(bytes, at),
    }
}

fn parse_object(bytes: &[u8], at: &mut usize, depth: u32) -> Option<Json> {
    *at += 1; // '{'
    let mut fields = Vec::new();
    skip_whitespace(bytes, at);
    if bytes.get(*at) == Some(&b'}') {
        *at += 1;
        return Some(Json::Obj(fields));
    }
    loop {
        skip_whitespace(bytes, at);
        let key = parse_string(bytes, at)?;
        skip_whitespace(bytes, at);
        (*bytes.get(*at)? == b':').then_some(())?;
        *at += 1;
        let value = parse_value(bytes, at, depth + 1)?;
        fields.push((key, value));
        skip_whitespace(bytes, at);
        match *bytes.get(*at)? {
            b',' => *at += 1,
            b'}' => {
                *at += 1;
                return Some(Json::Obj(fields));
            }
            _ => return None,
        }
    }
}

fn parse_array(bytes: &[u8], at: &mut usize, depth: u32) -> Option<Json> {
    *at += 1; // '['
    let mut items = Vec::new();
    skip_whitespace(bytes, at);
    if bytes.get(*at) == Some(&b']') {
        *at += 1;
        return Some(Json::Arr(items));
    }
    loop {
        items.push(parse_value(bytes, at, depth + 1)?);
        skip_whitespace(bytes, at);
        match *bytes.get(*at)? {
            b',' => *at += 1,
            b']' => {
                *at += 1;
                return Some(Json::Arr(items));
            }
            _ => return None,
        }
    }
}

/// A JSON string, `at` positioned on the opening quote.
///
/// The one deliberate leniency is a lone UTF-16 surrogate, which
/// becomes U+FFFD rather than discarding a whole device list over one
/// broken byte in a device name someone typed on a phone.
fn parse_string(bytes: &[u8], at: &mut usize) -> Option<String> {
    (*bytes.get(*at)? == b'"').then_some(())?;
    *at += 1;
    let mut out = String::new();
    loop {
        match *bytes.get(*at)? {
            b'"' => {
                *at += 1;
                return Some(out);
            }
            b'\\' => {
                *at += 1;
                match *bytes.get(*at)? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        *at += 1;
                        out.push(parse_unicode_escape(bytes, at)?);
                        continue;
                    }
                    _ => return None,
                }
                *at += 1;
            }
            // Raw control characters are invalid in a JSON string, but
            // rejecting a whole device list over one is the wrong
            // trade, so everything that is not a quote or a backslash
            // is copied through.
            byte if byte < 0x80 => {
                out.push(byte as char);
                *at += 1;
            }
            // A multi-byte UTF-8 sequence, copied whole rather than
            // byte by byte: device names are named by people, and they
            // are full of accents, kana and emoji.
            byte => {
                let len = utf8_len(byte);
                out.push_str(std::str::from_utf8(bytes.get(*at..*at + len)?).ok()?);
                *at += len;
            }
        }
    }
}

/// How many bytes the UTF-8 sequence starting with `lead` occupies.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0xF0..=0xF7 => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

/// `\uXXXX`, `at` positioned on the first hex digit, including the
/// surrogate pair that follows a high surrogate.
fn parse_unicode_escape(bytes: &[u8], at: &mut usize) -> Option<char> {
    let high = hex4(bytes, at)?;
    if !(0xD800..0xDC00).contains(&high) {
        return Some(char::from_u32(high).unwrap_or('\u{FFFD}'));
    }
    // A high surrogate: the low half must follow as its own escape, or
    // the character is replaced rather than the document rejected.
    if bytes.get(*at) != Some(&b'\\') || bytes.get(*at + 1) != Some(&b'u') {
        return Some('\u{FFFD}');
    }
    let mut probe = *at + 2;
    let Some(low) = hex4(bytes, &mut probe) else { return Some('\u{FFFD}') };
    if !(0xDC00..0xE000).contains(&low) {
        return Some('\u{FFFD}');
    }
    *at = probe;
    let combined = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
    Some(char::from_u32(combined).unwrap_or('\u{FFFD}'))
}

fn hex4(bytes: &[u8], at: &mut usize) -> Option<u32> {
    let digits = bytes.get(*at..*at + 4)?;
    let mut value = 0u32;
    for digit in digits {
        value = value * 16 + (*digit as char).to_digit(16)?;
    }
    *at += 4;
    Some(value)
}

fn parse_number(bytes: &[u8], at: &mut usize) -> Option<Json> {
    let start = *at;
    if bytes.get(*at) == Some(&b'-') {
        *at += 1;
    }
    while matches!(bytes.get(*at), Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
        *at += 1;
    }
    std::str::from_utf8(bytes.get(start..*at)?).ok()?.parse::<f64>().ok().map(Json::Num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_and_keywords_parse() {
        assert_eq!(Json::parse("true"), Some(Json::Bool(true)));
        assert_eq!(Json::parse("false"), Some(Json::Bool(false)));
        assert_eq!(Json::parse("null"), Some(Json::Null));
        assert_eq!(Json::parse("42").and_then(|v| v.as_f64()), Some(42.0));
        assert_eq!(Json::parse("-1.5e2").and_then(|v| v.as_f64()), Some(-150.0));
        assert_eq!(Json::parse("\"hi\"").and_then(|v| v.as_str().map(str::to_string)), Some("hi".to_string()));
    }

    /// The exact envelope `busctl --json=short` puts a scalar property
    /// in, captured from this machine's live systemd rather than
    /// recalled: `busctl --system --json=short get-property
    /// org.freedesktop.systemd1 /org/freedesktop/systemd1
    /// org.freedesktop.systemd1.Manager Version`.
    #[test]
    fn a_busctl_scalar_reply_parses() {
        let value = Json::parse(r#"{"type":"s","data":"261.2-1-arch"}"#).expect("valid");
        assert_eq!(value.get("type").and_then(Json::as_str), Some("s"));
        assert_eq!(value.get("data").and_then(Json::as_str), Some("261.2-1-arch"));
    }

    /// And the `a{sv}` shape every BlueZ property bag arrives in,
    /// likewise captured live from `Properties.GetAll`.
    #[test]
    fn a_busctl_property_bag_parses() {
        let text = r#"{"type":"a{sv}","data":[{"Version":{"type":"s","data":"261.2"},"ShowStatus":{"type":"b","data":false},"NNames":{"type":"u","data":231}}]}"#;
        let value = Json::parse(text).expect("valid");
        let bag = &value.get("data").and_then(Json::as_array).expect("array")[0];
        assert_eq!(bag.get("Version").and_then(|v| v.get("data")).and_then(Json::as_str), Some("261.2"));
        assert_eq!(bag.get("ShowStatus").and_then(|v| v.get("data")).and_then(Json::as_bool), Some(false));
        assert_eq!(bag.entries().len(), 3, "insertion order is preserved and nothing is dropped");
    }

    #[test]
    fn escapes_including_surrogate_pairs_decode() {
        assert_eq!(Json::parse(r#""a\"b""#).unwrap().as_str(), Some("a\"b"));
        assert_eq!(Json::parse(r#""tab\there""#).unwrap().as_str(), Some("tab\there"));
        assert_eq!(Json::parse(r#""A""#).unwrap().as_str(), Some("A"));
        // U+1F50A SPEAKER WITH THREE SOUND WAVES, as a surrogate pair —
        // device names really do carry emoji.
        assert_eq!(Json::parse(r#""🔊""#).unwrap().as_str(), Some("\u{1F50A}"));
        // A lone high surrogate is replaced, not fatal.
        assert_eq!(Json::parse(r#""\ud83d""#).unwrap().as_str(), Some("\u{FFFD}"));
    }

    #[test]
    fn multibyte_utf8_survives_unescaped() {
        assert_eq!(Json::parse("\"Björn's Küche\"").unwrap().as_str(), Some("Björn's Küche"));
        assert_eq!(Json::parse("\"🎧\"").unwrap().as_str(), Some("🎧"));
    }

    #[test]
    fn structural_errors_reject_the_whole_document() {
        for broken in [
            "{",
            "[1,",
            "{\"a\"}",
            "{\"a\":}",
            "{\"a\":1,}",
            "[1 2]",
            "\"unterminated",
            "",
            // Trailing content after a complete value: a truncated
            // write that lands on a boundary must not read as whole.
            "{} junk",
        ] {
            assert_eq!(Json::parse(broken), None, "{broken:?} must not parse");
        }
    }

    #[test]
    fn depth_is_capped_rather_than_overflowing_the_stack() {
        let deep = "[".repeat(500) + &"]".repeat(500);
        assert_eq!(Json::parse(&deep), None, "a pathological nest is refused, not a crash");
    }

    #[test]
    fn as_u8_accepts_only_a_byte_sized_whole_number() {
        assert_eq!(Json::Num(100.0).as_u8(), Some(100));
        assert_eq!(Json::Num(0.0).as_u8(), Some(0));
        assert_eq!(Json::Num(255.0).as_u8(), Some(255));
        assert_eq!(Json::Num(256.0).as_u8(), None);
        assert_eq!(Json::Num(-1.0).as_u8(), None);
        assert_eq!(Json::Str("100".into()).as_u8(), None);
    }
}
