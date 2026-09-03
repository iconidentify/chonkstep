//! Parsing the request wire format of Hyprland's `.socket.sock`.
//!
//! The grammar was not taken from documentation. It was measured, by
//! binding a Unix socket at the path the real `hyprctl` looks for and
//! recording the exact bytes it wrote for each subcommand, and by
//! reading Quickshell's IPC client source (installed on the
//! development machine at
//! `/usr/src/debug/quickshell-git/quickshell/src/wayland/hyprland/ipc/`).
//! `docs/hyprland-ipc.md` §1.6 records the transcript.
//!
//! The two clients do not agree, which is the whole reason this module
//! is more than a `split_once`:
//!
//! ```text
//! hyprctl clients -j   ->  j/clients            flags, then '/', then command
//! hyprctl clients      ->  /clients             empty flags, but the '/' is still there
//! Quickshell dispatch  ->  dispatch focuswindow address:0x5  no '/' at all
//! Quickshell query     ->  j/clients            same as hyprctl
//! ```
//!
//! So the `/` is a *separator between flags and command*, not a prefix,
//! and it is optional. A parser that requires it silently refuses every
//! dispatch Quickshell makes; a parser that treats a leading `/` as
//! decoration to be stripped mis-reads `j/clients` as the command
//! `j/clients`. Both mistakes produce a server that looks fine against
//! `hyprctl` and is dead against the bar, which is why this has tests.
//!
//! There is no length header and no trailing newline: the request is
//! the whole payload of a connection that is then read to EOF.

/// The flag block that precedes the `/` in a request.
///
/// Hyprland has several flags; `j` (answer in JSON) is the only one any
/// caller in the Omarchy inventory uses, and the only one that changes
/// what this server does. The rest are preserved as `raw` rather than
/// rejected, because refusing an unknown flag would turn a request we
/// could have answered into an error, and flags in Hyprland are
/// modifiers on the *presentation* of an answer, never on its meaning.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Flags {
    /// `j` was present: the caller wants JSON.
    pub json: bool,
    /// The flag block exactly as received, for diagnostics.
    pub raw: String,
}

/// One parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub flags: Flags,
    /// The verb, lowercased — `clients`, `dispatch`, `monitors`.
    pub command: String,
    /// Everything after the first space, verbatim and untrimmed of
    /// interior structure. `dispatch` arguments are a language of their
    /// own (see [`crate::dispatch`]), and `keyword` arguments contain
    /// commas and colons that mean something to the caller, so this
    /// module does not tokenize them.
    pub args: String,
}

/// The largest request this server will consider.
///
/// Hyprland's own requests are tens of bytes. This cap exists so that a
/// client which connects and streams forever cannot make the serving
/// thread allocate without bound; it is checked while reading, not
/// after, so the bytes are never held.
pub const MAX_REQUEST: usize = 64 * 1024;

impl Request {
    /// Parse a request payload.
    ///
    /// Returns `None` only for input that cannot name a command at all
    /// — empty, all-whitespace, or a flag block with nothing after it.
    /// Everything else parses; whether the command *exists* is a
    /// separate question answered by the dispatch table, because
    /// "unknown request" is a reply Hyprland gives on the wire rather
    /// than a parse failure.
    pub fn parse(payload: &[u8]) -> Option<Request> {
        // Hyprland reads its socket as bytes and so do we. A request
        // with invalid UTF-8 in it is not a protocol error — window
        // titles and app ids reach us from clients and can be any byte
        // sequence — so replace rather than reject.
        let text = String::from_utf8_lossy(payload);
        let text = text.trim_matches(|c: char| c == '\0' || c == '\n' || c == '\r');

        // Split flags from command at the FIRST '/'. Crucially this
        // only counts when the '/' comes before any space: `dispatch
        // exec foo/bar` has a slash in it, and it is part of an
        // argument, not a flag separator.
        let (flags, rest) = match text.find('/') {
            Some(slash) if !text[..slash].contains(' ') => {
                let raw = text[..slash].to_string();
                (Flags { json: raw.contains('j'), raw }, &text[slash + 1..])
            }
            // No '/' before the first space: Quickshell's form. No flags.
            _ => (Flags::default(), text),
        };

        let rest = rest.trim_start();
        let (command, args) = match rest.find(char::is_whitespace) {
            Some(space) => (&rest[..space], rest[space + 1..].trim()),
            None => (rest, ""),
        };

        if command.is_empty() {
            return None;
        }

        Some(Request {
            flags,
            command: command.to_ascii_lowercase(),
            args: args.to_string(),
        })
    }

    /// True when the caller asked for JSON.
    pub fn wants_json(&self) -> bool {
        self.flags.json
    }
}

/// Split a `[[BATCH]]`-prefixed payload into its component requests.
///
/// `hyprctl --batch "a;b;c"` sends one connection carrying
/// `[[BATCH]]a;b;c`; the literal marker is visible in the installed
/// binary. Returns `None` for a payload that is not a batch, so the
/// caller can take the ordinary path without a second parse.
///
/// Empty segments are dropped rather than reported: `a;;b` and a
/// trailing `;` are both things `hyprctl` produces from ordinary shell
/// quoting, and neither is a mistake worth failing a batch over.
pub fn split_batch(payload: &[u8]) -> Option<Vec<Vec<u8>>> {
    const MARKER: &[u8] = b"[[BATCH]]";
    let rest = payload.strip_prefix(MARKER)?;
    Some(
        rest.split(|b| *b == b';')
            .map(|segment| segment.to_vec())
            .filter(|segment| !segment.iter().all(|b| b.is_ascii_whitespace()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Request {
        Request::parse(s.as_bytes()).expect("should parse")
    }

    /// The measured transcript from `docs/hyprland-ipc.md` §1.6, byte
    /// for byte. If this test fails, either the parser broke or a new
    /// `hyprctl` changed the wire format; re-run the probe before
    /// changing the expectations.
    #[test]
    fn parses_every_measured_hyprctl_request() {
        let cases: &[(&str, bool, &str, &str)] = &[
            ("/clients", false, "clients", ""),
            ("j/clients", true, "clients", ""),
            ("/monitors", false, "monitors", ""),
            ("/monitors all", false, "monitors", "all"),
            ("/activewindow", false, "activewindow", ""),
            ("/workspaces", false, "workspaces", ""),
            ("/activeworkspace", false, "activeworkspace", ""),
            ("/version", false, "version", ""),
            ("j/devices", true, "devices", ""),
            ("j/getoption decoration:rounding", true, "getoption", "decoration:rounding"),
            ("/dispatch workspace 3", false, "dispatch", "workspace 3"),
            ("/keyword monitor eDP-1,disable", false, "keyword", "monitor eDP-1,disable"),
            ("/switchxkblayout kbd next", false, "switchxkblayout", "kbd next"),
            ("/reload", false, "reload", ""),
            ("/configerrors", false, "configerrors", ""),
        ];

        for (wire, json, command, args) in cases {
            let request = parse(wire);
            assert_eq!(request.wants_json(), *json, "json flag for {wire:?}");
            assert_eq!(request.command, *command, "command for {wire:?}");
            assert_eq!(request.args, *args, "args for {wire:?}");
        }
    }

    /// Quickshell omits the `/` for dispatch but keeps it for queries.
    /// Both forms come from the same client in the same session, so
    /// both must work or the bar half-works — which is worse than not
    /// working, because it looks like a chonkstep bug rather than a
    /// protocol one.
    #[test]
    fn parses_quickshells_slashless_dispatch() {
        let request = parse("dispatch focuswindow address:0x55d1a");
        assert!(!request.wants_json());
        assert_eq!(request.command, "dispatch");
        assert_eq!(request.args, "focuswindow address:0x55d1a");

        // ...and its query form, which does use the slash.
        assert_eq!(parse("j/status").command, "status");
        assert!(parse("j/status").wants_json());
    }

    /// A slash inside an argument is not a flag separator. `dispatch
    /// exec /usr/bin/foo` is the case that matters: read the first
    /// slash unconditionally and the command becomes `usr` with flags
    /// `dispatch exec `, which is nonsense that would then be reported
    /// as an unknown command rather than run.
    #[test]
    fn slash_after_a_space_is_an_argument_not_a_separator() {
        let request = parse("dispatch exec /usr/bin/foot");
        assert_eq!(request.command, "dispatch");
        assert_eq!(request.args, "exec /usr/bin/foot");
        assert_eq!(request.flags.raw, "");

        let request = parse("/dispatch exec /usr/bin/foot");
        assert_eq!(request.command, "dispatch");
        assert_eq!(request.args, "exec /usr/bin/foot");
    }

    #[test]
    fn unknown_flags_are_preserved_not_rejected() {
        let request = parse("rj/clients");
        assert!(request.wants_json());
        assert_eq!(request.flags.raw, "rj");
        assert_eq!(request.command, "clients");
    }

    #[test]
    fn commands_are_case_insensitive() {
        assert_eq!(parse("j/CLIENTS").command, "clients");
        assert_eq!(parse("/Dispatch KillActive").command, "dispatch");
        // ...but arguments are not lowercased: window titles, app ids
        // and Lua identifiers all depend on their case.
        assert_eq!(parse("/dispatch KillActive").args, "KillActive");
    }

    // ---- hostile and degenerate input ----

    #[test]
    fn rejects_input_that_names_no_command() {
        assert!(Request::parse(b"").is_none());
        assert!(Request::parse(b"   ").is_none());
        assert!(Request::parse(b"j/").is_none());
        assert!(Request::parse(b"/").is_none());
        assert!(Request::parse(b"\n\n").is_none());
    }

    #[test]
    fn tolerates_invalid_utf8_without_panicking() {
        let request = Request::parse(b"j/dispatch exec \xff\xfe\x00bad").expect("should parse");
        assert_eq!(request.command, "dispatch");
        assert!(request.args.starts_with("exec"));
    }

    #[test]
    fn tolerates_trailing_newline_and_nul() {
        // Nothing in the inventory sends these, but `nc -U` adds a
        // newline and a careless client can send a NUL-terminated C
        // string. Neither should turn `clients` into an unknown command.
        assert_eq!(parse("j/clients\n").command, "clients");
        assert_eq!(parse("j/clients\0").command, "clients");
        assert_eq!(parse("j/clients\r\n").command, "clients");
    }

    #[test]
    fn collapses_runs_of_whitespace_between_command_and_args() {
        assert_eq!(parse("/dispatch    workspace 3").args, "workspace 3");
        assert_eq!(parse("/monitors\tall").args, "all");
    }

    #[test]
    fn very_long_argument_does_not_panic() {
        let long = format!("/dispatch exec {}", "a".repeat(100_000));
        let request = Request::parse(long.as_bytes()).expect("should parse");
        assert_eq!(request.command, "dispatch");
    }

    // ---- batches ----

    #[test]
    fn splits_batches_and_ignores_non_batches() {
        assert!(split_batch(b"j/clients").is_none());

        let batch = split_batch(b"[[BATCH]]/dispatch killactive;j/clients").expect("is a batch");
        assert_eq!(batch.len(), 2);
        assert_eq!(Request::parse(&batch[0]).unwrap().command, "dispatch");
        assert_eq!(Request::parse(&batch[1]).unwrap().command, "clients");
    }

    #[test]
    fn batch_tolerates_empty_and_trailing_segments() {
        let batch = split_batch(b"[[BATCH]]/reload;;/reload; ").expect("is a batch");
        assert_eq!(batch.len(), 2);
    }
}
