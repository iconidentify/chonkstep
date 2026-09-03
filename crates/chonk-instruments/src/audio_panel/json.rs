//! A minimal JSON reader for `pactl --format=json` output.
//!
//! Hand-rolled rather than a dependency on purpose. This crate's
//! `Cargo.toml` argues, line by line, that an instrument can see only
//! the SDK, the theme, a pixel buffer and a text shaper — and `pactl`'s
//! JSON is the one machine-readable surface the audio stack offers
//! (`wpctl` has no structured output at all), so the choice was between
//! extending that manifest and writing the hundred lines of parser the
//! panel actually needs. The parser is a pure function from `&str` to a
//! value tree, does no I/O by construction, and is fixture-tested
//! against this machine's real `pactl` output, escapes and all.
//!
//! What it accepts is standard JSON: objects, arrays, strings with the
//! full escape set (`\uXXXX` surrogate pairs included), numbers, the
//! three keywords. Structural errors reject the whole document — the
//! callers' answer to an unparseable `pactl` is the same as to a
//! missing one, and half a sink list is worse than none. The one
//! deliberate leniency is a lone UTF-16 surrogate, which becomes
//! U+FFFD rather than discarding a device list over one broken
//! description byte.

/// A parsed JSON value. Objects keep insertion order in a plain vec —
/// `pactl`'s objects are a couple dozen keys at most, and the panel
/// reads perhaps five of them, so a map would be ceremony.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// Nesting depth cap. `pactl`'s real output nests four levels; sixty
/// is slack for format growth while keeping a pathological input from
/// walking the parser off the stack.
const MAX_DEPTH: u32 = 60;

impl Json {
    /// Parses one complete JSON document. Trailing garbage after the
    /// value rejects the document — a truncated or interleaved read is
    /// not a reading.
    pub fn parse(text: &str) -> Option<Json> {
        let mut p = Parser { bytes: text.as_bytes(), pos: 0 };
        p.skip_ws();
        let value = p.value(0)?;
        p.skip_ws();
        (p.pos == p.bytes.len()).then_some(value)
    }

    /// Member lookup on an object; `None` on anything else. First match
    /// wins, as in every JSON reader.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// The value as a non-negative integer, for ids and indexes.
    /// `pactl` indexes are u32s, well inside f64's exact-integer range.
    pub fn as_u64(&self) -> Option<u64> {
        let n = self.as_f64()?;
        (n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_992.0).then_some(n as u64)
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Option<()> {
        (self.peek() == Some(byte)).then(|| self.pos += 1)
    }

    fn value(&mut self, depth: u32) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Json::Str),
            b't' => self.keyword("true", Json::Bool(true)),
            b'f' => self.keyword("false", Json::Bool(false)),
            b'n' => self.keyword("null", Json::Null),
            _ => self.number(),
        }
    }

    fn keyword(&mut self, word: &str, value: Json) -> Option<Json> {
        let end = self.pos + word.len();
        if self.bytes.get(self.pos..end) == Some(word.as_bytes()) {
            self.pos = end;
            return Some(value);
        }
        None
    }

    fn object(&mut self, depth: u32) -> Option<Json> {
        self.eat(b'{')?;
        let mut members = Vec::new();
        self.skip_ws();
        if self.eat(b'}').is_some() {
            return Some(Json::Obj(members));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.eat(b':')?;
            self.skip_ws();
            members.push((key, self.value(depth + 1)?));
            self.skip_ws();
            if self.eat(b',').is_some() {
                continue;
            }
            self.eat(b'}')?;
            return Some(Json::Obj(members));
        }
    }

    fn array(&mut self, depth: u32) -> Option<Json> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b']').is_some() {
            return Some(Json::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            if self.eat(b',').is_some() {
                continue;
            }
            self.eat(b']')?;
            return Some(Json::Arr(items));
        }
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            match self.peek()? {
                b'"' => {
                    self.pos += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.pos += 1;
                    self.escape(&mut out)?;
                }
                byte if byte < 0x20 => return None,
                _ => {
                    // One whole UTF-8 sequence at a time; the input is
                    // a &str, so sequences are already valid.
                    let start = self.pos;
                    self.pos += 1;
                    while self.bytes.get(self.pos).is_some_and(|b| b & 0xC0 == 0x80) {
                        self.pos += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.bytes[start..self.pos]).ok()?);
                }
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Option<()> {
        let escape = self.peek()?;
        self.pos += 1;
        match escape {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let unit = self.hex4()?;
                let ch = if (0xD800..0xDC00).contains(&unit) {
                    // A high surrogate wants a low one right behind it.
                    // A lone or mismatched surrogate degrades to U+FFFD
                    // instead of rejecting the whole device list.
                    if self.bytes.get(self.pos..self.pos + 2) == Some(b"\\u") {
                        self.pos += 2;
                        let low = self.hex4()?;
                        if (0xDC00..0xE000).contains(&low) {
                            let code = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        } else {
                            '\u{FFFD}'
                        }
                    } else {
                        '\u{FFFD}'
                    }
                } else {
                    char::from_u32(unit).unwrap_or('\u{FFFD}')
                };
                out.push(ch);
            }
            _ => return None,
        }
        Some(())
    }

    fn hex4(&mut self) -> Option<u32> {
        let end = self.pos + 4;
        let digits = std::str::from_utf8(self.bytes.get(self.pos..end)?).ok()?;
        let unit = u32::from_str_radix(digits, 16).ok()?;
        self.pos = end;
        Some(unit)
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        // Rust's f64 parse is laxer than JSON ("+1", ".5"); requiring
        // a JSON-legal head keeps this a JSON reader, not a guesser.
        if !matches!(text.as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
            return None;
        }
        let n: f64 = text.parse().ok()?;
        n.is_finite().then_some(Json::Num(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_scalar_zoo() {
        assert_eq!(Json::parse("null"), Some(Json::Null));
        assert_eq!(Json::parse("true"), Some(Json::Bool(true)));
        assert_eq!(Json::parse("false"), Some(Json::Bool(false)));
        assert_eq!(Json::parse("42"), Some(Json::Num(42.0)));
        assert_eq!(Json::parse("-0.5"), Some(Json::Num(-0.5)));
        assert_eq!(Json::parse("1e3"), Some(Json::Num(1000.0)));
        assert_eq!(Json::parse("\"hi\""), Some(Json::Str("hi".into())));
        assert_eq!(Json::parse("  [1, 2]  "), Some(Json::Arr(vec![Json::Num(1.0), Json::Num(2.0)])));
    }

    #[test]
    fn parses_nested_structures_and_lookup_works() {
        let doc = Json::parse(r#"{"a": {"b": [1, {"c": "d"}]}, "e": null}"#).unwrap();
        let inner = doc.get("a").and_then(|a| a.get("b")).and_then(Json::as_array).unwrap();
        assert_eq!(inner[1].get("c").and_then(Json::as_str), Some("d"));
        assert_eq!(doc.get("e"), Some(&Json::Null));
        assert_eq!(doc.get("missing"), None);
    }

    /// The escape sequence `pactl` actually emits — its `format` field
    /// nests quoted quotes two levels deep.
    #[test]
    fn unescapes_the_pactl_format_field() {
        let doc = Json::parse(r#""format.sample_format = \"\\\"float32le\\\"\"""#).unwrap();
        assert_eq!(doc.as_str(), Some(r#"format.sample_format = "\"float32le\"""#));
    }

    #[test]
    fn unescapes_the_standard_set_and_unicode() {
        let doc = Json::parse(r#""a\n\t\/é😀b""#).unwrap();
        assert_eq!(doc.as_str(), Some("a\n\t/é😀b"));
    }

    #[test]
    fn a_lone_surrogate_degrades_to_replacement_not_rejection() {
        assert_eq!(Json::parse(r#""x\ud800y""#).unwrap().as_str(), Some("x\u{FFFD}y"));
        assert_eq!(Json::parse(r#""x\ud800Ay""#).unwrap().as_str(), Some("x\u{FFFD}Ay"), "a lone high surrogate costs only itself");
        assert_eq!(Json::parse(r#""x\ud800\u0041y""#).unwrap().as_str(), Some("x\u{FFFD}y"), "a mismatched escape pair eats both units");
    }

    #[test]
    fn structural_damage_rejects_the_document() {
        for bad in ["", "{", "[1,", "{\"a\" 1}", "[1] trailing", "\"unterminated", "{\"a\":}", "nul", "+1", "\"\u{1}\"", "Infinity"] {
            assert_eq!(Json::parse(bad), None, "{bad:?} must not parse");
        }
        // Literal control byte inside a string body.
        assert_eq!(Json::parse("\"a\u{0}b\""), None);
    }

    #[test]
    fn depth_is_capped() {
        let deep = "[".repeat(100) + &"]".repeat(100);
        assert_eq!(Json::parse(&deep), None);
        let fine = "[".repeat(40) + &"]".repeat(40);
        assert!(Json::parse(&fine).is_some());
    }

    #[test]
    fn as_u64_takes_integers_only() {
        assert_eq!(Json::parse("19288").unwrap().as_u64(), Some(19288));
        assert_eq!(Json::parse("4294967295").unwrap().as_u64(), Some(4294967295));
        assert_eq!(Json::parse("1.5").unwrap().as_u64(), None);
        assert_eq!(Json::parse("-3").unwrap().as_u64(), None);
    }
}
