//! The link panel's Tailscale data layer: `tailscale status --json`
//! reduced to the few facts the TAILSCALE row draws, plus the honest
//! classification of a denied toggle.
//!
//! Reads are unprivileged — any user can run `tailscale status` — so
//! the row's *facts* never need a grant. Mutation (`tailscale up`,
//! `tailscale down`) goes through tailscaled's local API, which
//! refuses anyone but root or the configured operator; the CLI prints
//! `Access denied: ...` with the exact `sudo tailscale set
//! --operator=$USER` remedy in it. [`classify_toggle_output`] detects
//! that denial so the panel can model a [`OperatorState::NeedsOperator`]
//! state and *show* it instead of a toggle that silently does nothing
//! — see `scripts/install.sh`, which offers the grant at install time.
//!
//! An absent `tailscale` binary is a permanently unusable source
//! (`Samples::unusable`), and the panel answers it by not having a
//! TAILSCALE row at all.
//!
//! # Why the JSON parser is hand-rolled
//!
//! This crate's dependency list is the enforcement of "an instrument
//! cannot do I/O" (see its `Cargo.toml`), and every line of it is part
//! of that argument: the SDK, the theme, a pixel buffer, a text
//! shaper. `serde` would be harmless — but the list stays short by
//! policy, and the status document needs six fields out of a ~78KB
//! blob. [`Json`] below is a ~150-line recursive-descent parser with a
//! depth cap, fixture-tested against a real `tailscale status --json`
//! capture. If a second instrument ever needs JSON, revisit the
//! trade.

/// A parsed JSON value. Objects keep insertion order in a `Vec` —
/// lookups here are a handful per sample over a handful of keys, so a
/// map would be ceremony.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Parses one JSON document, tolerating trailing whitespace.
    /// `None` for anything malformed — the caller's answer to a
    /// half-written or garbage document is "no reading", never a
    /// panic.
    pub(crate) fn parse(text: &str) -> Option<Json> {
        let mut p = Parser { chars: text.chars().collect(), pos: 0 };
        let value = p.value(0)?;
        p.skip_ws();
        p.at_end().then_some(value)
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub(crate) fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub(crate) fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(entries) => Some(entries),
            _ => None,
        }
    }
}

/// Nesting deeper than this fails the parse rather than the stack.
/// The status document nests about five levels; sixty-four is
/// generous headroom, not a limit anyone hits honestly.
const MAX_DEPTH: usize = 64;

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, want: char) -> Option<()> {
        (self.bump()? == want).then_some(())
    }

    fn literal(&mut self, rest: &str, value: Json) -> Option<Json> {
        for want in rest.chars() {
            self.eat(want)?;
        }
        Some(value)
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        match self.peek()? {
            '{' => self.object(depth),
            '[' => self.array(depth),
            '"' => self.string().map(Json::Str),
            't' => {
                self.pos += 1;
                self.literal("rue", Json::Bool(true))
            }
            'f' => {
                self.pos += 1;
                self.literal("alse", Json::Bool(false))
            }
            'n' => {
                self.pos += 1;
                self.literal("ull", Json::Null)
            }
            '-' | '0'..='9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self, depth: usize) -> Option<Json> {
        self.eat('{')?;
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Some(Json::Obj(entries));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.eat(':')?;
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            self.skip_ws();
            match self.bump()? {
                ',' => continue,
                '}' => return Some(Json::Obj(entries)),
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<Json> {
        self.eat('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Some(Json::Arr(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.bump()? {
                ',' => continue,
                ']' => return Some(Json::Arr(items)),
                _ => return None,
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        self.eat('"')?;
        let mut out = String::new();
        loop {
            match self.bump()? {
                '"' => return Some(out),
                '\\' => match self.bump()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let unit = self.hex4()?;
                        let c = if (0xD800..0xDC00).contains(&unit) {
                            // High surrogate: the low half must follow.
                            self.eat('\\')?;
                            self.eat('u')?;
                            let low = self.hex4()?;
                            if !(0xDC00..0xE000).contains(&low) {
                                return None;
                            }
                            char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))?
                        } else if (0xDC00..0xE000).contains(&unit) {
                            return None;
                        } else {
                            char::from_u32(unit)?
                        };
                        out.push(c);
                    }
                    _ => return None,
                },
                c if (c as u32) < 0x20 => return None,
                c => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            value = value * 16 + self.bump()?.to_digit(16)?;
        }
        Some(value)
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some('0'..='9' | '.' | 'e' | 'E' | '+' | '-')) {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>().ok().filter(|n| n.is_finite()).map(Json::Num)
    }
}

/// tailscaled's answer to "are we on the tailnet", from the status
/// document's `BackendState`. The strings are tailscale's own state
/// machine (ipn.State); anything unrecognized folds to [`BackendState::Other`]
/// and renders as an inert, honestly-labeled row rather than a guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendState {
    Running,
    Stopped,
    Starting,
    NeedsLogin,
    NeedsMachineAuth,
    NoState,
    Other,
}

impl BackendState {
    fn from_name(name: &str) -> BackendState {
        match name {
            "Running" => BackendState::Running,
            "Stopped" => BackendState::Stopped,
            "Starting" => BackendState::Starting,
            "NeedsLogin" => BackendState::NeedsLogin,
            "NeedsMachineAuth" => BackendState::NeedsMachineAuth,
            "NoState" => BackendState::NoState,
            _ => BackendState::Other,
        }
    }
}

/// Everything the TAILSCALE row draws, reduced from one
/// `tailscale status --json` sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailscaleStatus {
    pub backend: BackendState,
    /// `Self.Online` — whether this node currently reaches the
    /// coordination infrastructure. Distinct from `Running`: a
    /// running backend on a dead uplink is offline.
    pub self_online: bool,
    /// The hostname of the peer currently used as an exit node, if
    /// any (the peer whose `ExitNode` is true).
    pub exit_node: Option<String>,
    /// How many peers offer themselves as exit nodes
    /// (`ExitNodeOption`) — drawn as availability, not a menu, in
    /// this iteration.
    pub exit_node_choices: u32,
    /// Current health warnings, verbatim from the `Health` array.
    pub health: Vec<String>,
}

/// Reduces a `tailscale status --json` document. `None` when the text
/// is not the JSON this asks for — a crashed CLI's error text, a
/// truncated read — which the panel folds as "no reading yet", keeping
/// the previous one.
pub fn parse_status(text: &str) -> Option<TailscaleStatus> {
    let doc = Json::parse(text)?;
    let backend = BackendState::from_name(doc.get("BackendState")?.as_str()?);
    let self_online = doc.get("Self").and_then(|s| s.get("Online")).and_then(Json::as_bool).unwrap_or(false);
    let mut exit_node = None;
    let mut exit_node_choices = 0u32;
    if let Some(peers) = doc.get("Peer").and_then(Json::as_obj) {
        for (_, peer) in peers {
            if peer.get("ExitNodeOption").and_then(Json::as_bool).unwrap_or(false) {
                exit_node_choices += 1;
            }
            if peer.get("ExitNode").and_then(Json::as_bool).unwrap_or(false) && exit_node.is_none() {
                exit_node = peer.get("HostName").and_then(Json::as_str).map(str::to_string);
            }
        }
    }
    let health = doc
        .get("Health")
        .and_then(Json::as_arr)
        .map(|items| items.iter().filter_map(|i| i.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Some(TailscaleStatus { backend, self_online, exit_node, exit_node_choices, health })
}

/// Whether this session may move Tailscale at all. `Unknown` until a
/// toggle has been tried; a denial is remembered for the rest of the
/// session (the grant needs `sudo`, so it will not appear between two
/// samples), and one success proves the grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperatorState {
    Unknown,
    Granted,
    /// The toggle was refused. `hint` is the remedy line out of the
    /// CLI's own message — the renderer shows it verbatim, because
    /// the honest answer to a click that cannot work is the command
    /// that would make it work.
    NeedsOperator { hint: String },
}

/// What a completed `tailscale up`/`down` said, classified from its
/// combined output. Success prints nothing (the next status sample is
/// the truth); the one output the *sample cannot show* is the
/// permission denial, which looks like:
///
/// ```text
/// Access denied: watch IPN bus access denied, must be root or Operator
/// Use 'sudo tailscale up' or 'sudo tailscale set --operator=$USER' to not require root.
/// ```
///
/// Everything that is not that is `Indeterminate` — let the sample
/// decide, because inventing failure classes from unversioned CLI
/// text is how a panel starts lying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToggleVerdict {
    Denied { hint: String },
    Indeterminate,
}

pub fn classify_toggle_output(output: &str) -> ToggleVerdict {
    if !output.contains("Access denied") {
        return ToggleVerdict::Indeterminate;
    }
    let hint = output
        .lines()
        .find(|line| line.contains("--operator="))
        .map(|line| line.trim().to_string())
        .unwrap_or_else(|| "sudo tailscale set --operator=$USER".to_string());
    ToggleVerdict::Denied { hint }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_scalars_parse() {
        assert_eq!(Json::parse("null"), Some(Json::Null));
        assert_eq!(Json::parse("true"), Some(Json::Bool(true)));
        assert_eq!(Json::parse("false"), Some(Json::Bool(false)));
        assert_eq!(Json::parse("42"), Some(Json::Num(42.0)));
        assert_eq!(Json::parse("-3.5e2"), Some(Json::Num(-350.0)));
        assert_eq!(Json::parse("\"hi\""), Some(Json::Str("hi".into())));
    }

    #[test]
    fn json_strings_unescape() {
        assert_eq!(Json::parse(r#""a\"b\\c\/d\n""#), Some(Json::Str("a\"b\\c/d\n".into())));
        assert_eq!(Json::parse(r#""é""#), Some(Json::Str("é".into())));
        assert_eq!(Json::parse(r#""😀""#), Some(Json::Str("😀".into())), "surrogate pairs");
        assert_eq!(Json::parse(r#""\ud83d""#), None, "a lone high surrogate is malformed");
        assert_eq!(Json::parse(r#""\udc00""#), None, "a lone low surrogate is malformed");
    }

    #[test]
    fn json_structures_parse_and_lookup() {
        let doc = Json::parse(r#" {"a": [1, {"b": true}], "c": null} "#).unwrap();
        assert_eq!(doc.get("c"), Some(&Json::Null));
        let arr = doc.get("a").unwrap().as_arr().unwrap();
        assert_eq!(arr[1].get("b"), Some(&Json::Bool(true)));
        assert_eq!(doc.get("missing"), None);
    }

    #[test]
    fn json_garbage_is_none_not_a_panic() {
        for bad in ["", "{", "[1,", "{\"a\" 1}", "tru", "01x", "\"unterminated", "{\"a\":1}trailing", "nan", "\u{0}"] {
            assert_eq!(Json::parse(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn json_depth_is_capped_not_a_stack_overflow() {
        let deep = "[".repeat(100_000) + &"]".repeat(100_000);
        assert_eq!(Json::parse(&deep), None);
        // ...while honest nesting well past the status document's
        // depth still parses.
        let ok = "[".repeat(32) + &"]".repeat(32);
        assert!(Json::parse(&ok).is_some());
    }

    /// The shape a real 1.102 `tailscale status --json` has, cut down
    /// to the fields this module reads (captured live on 2026-09-02).
    fn status_doc(backend: &str, online: bool, exit_node: bool) -> String {
        format!(
            r#"{{
  "Version": "1.102.3",
  "BackendState": "{backend}",
  "TailscaleIPs": ["100.99.69.17"],
  "Self": {{
    "HostName": "i9beef",
    "DNSName": "i9beef.tail091c06.ts.net.",
    "Online": {online},
    "ExitNode": false,
    "ExitNodeOption": false
  }},
  "Health": [],
  "Peer": {{
    "nodekey:aaaa": {{
      "HostName": "gateway",
      "Online": true,
      "ExitNode": {exit_node},
      "ExitNodeOption": true
    }},
    "nodekey:bbbb": {{
      "HostName": "laptop",
      "Online": false,
      "ExitNode": false,
      "ExitNodeOption": false
    }}
  }},
  "User": {{"7571899064330385": {{"DisplayName": "chris"}}}}
}}"#
        )
    }

    #[test]
    fn status_reduces_the_running_document() {
        let status = parse_status(&status_doc("Running", true, false)).unwrap();
        assert_eq!(
            status,
            TailscaleStatus {
                backend: BackendState::Running,
                self_online: true,
                exit_node: None,
                exit_node_choices: 1,
                health: vec![]
            }
        );
    }

    #[test]
    fn status_names_the_active_exit_node() {
        let status = parse_status(&status_doc("Running", true, true)).unwrap();
        assert_eq!(status.exit_node.as_deref(), Some("gateway"));
    }

    #[test]
    fn status_reads_the_stopped_and_needs_login_states() {
        assert_eq!(parse_status(&status_doc("Stopped", false, false)).unwrap().backend, BackendState::Stopped);
        assert_eq!(parse_status(&status_doc("NeedsLogin", false, false)).unwrap().backend, BackendState::NeedsLogin);
        assert_eq!(parse_status(&status_doc("SomethingNew", false, false)).unwrap().backend, BackendState::Other);
    }

    #[test]
    fn status_keeps_health_warnings_verbatim() {
        let text = r#"{"BackendState":"Running","Self":{"Online":true},"Health":["update available: 1.103.0","DNS is broken"]}"#;
        let status = parse_status(text).unwrap();
        assert_eq!(status.health, vec!["update available: 1.103.0".to_string(), "DNS is broken".to_string()]);
    }

    #[test]
    fn status_tolerates_missing_optional_sections() {
        let status = parse_status(r#"{"BackendState":"Stopped"}"#).unwrap();
        assert_eq!(status.backend, BackendState::Stopped);
        assert!(!status.self_online);
        assert_eq!(status.exit_node, None);
        assert_eq!(status.exit_node_choices, 0);
        assert!(status.health.is_empty());
    }

    #[test]
    fn status_error_text_is_no_reading() {
        assert_eq!(parse_status("failed to connect to local tailscaled; it doesn't appear to be running\n"), None);
        assert_eq!(parse_status(""), None);
    }

    #[test]
    fn a_denied_toggle_is_detected_with_its_remedy() {
        let output = "Access denied: watch IPN bus access denied, must be root or Operator\n\
                      Use 'sudo tailscale up' or 'sudo tailscale set --operator=$USER' to not require root.\n";
        match classify_toggle_output(output) {
            ToggleVerdict::Denied { hint } => {
                assert!(hint.contains("--operator="), "the hint must carry the remedy: {hint}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn a_denial_without_the_remedy_line_still_hints_the_grant() {
        match classify_toggle_output("Access denied: some future phrasing\n") {
            ToggleVerdict::Denied { hint } => assert_eq!(hint, "sudo tailscale set --operator=$USER"),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn quiet_or_unrelated_output_is_indeterminate() {
        assert_eq!(classify_toggle_output(""), ToggleVerdict::Indeterminate);
        assert_eq!(classify_toggle_output("Success.\n"), ToggleVerdict::Indeterminate);
        assert_eq!(classify_toggle_output("failed to connect to local tailscaled\n"), ToggleVerdict::Indeterminate);
    }
}
