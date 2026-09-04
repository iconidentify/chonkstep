//! What an Omarchy binding *means* here: the one place a Hyprland
//! dispatcher becomes a chonkstep verb, a command to run, or a
//! deliberate silence with a reason attached.
//!
//! # The three answers, carried over from the baked preset
//!
//! [`crate::preset`] already made this judgement once, chord by chord,
//! against Omarchy's files as they stood when it was written. Reading
//! the files live does not change the judgement — it changes *when* it
//! is made. So the three answers are the preset's three answers, and
//! [`crate::preset::Unbound`] is reused rather than restated:
//!
//! 1. A window or workspace verb chonkstep also has becomes that verb.
//! 2. An ordinary command becomes [`Action::Run`], with the argv
//!    declared under a name derived from the argv itself.
//! 3. Everything else stays unbound and says why.
//!
//! The rule that makes the third answer worth having is the preset's,
//! quoted because it is the thing most easily lost when a table becomes
//! a parser: *an approximation is worse than a dead key.* `SUPER + J`
//! toggles a split on Omarchy; on a stacking desk there is nothing to
//! split, and binding it to the nearest-looking verb turns a key the
//! user would look up in five seconds into a bug report. Every
//! dispatcher below that has no true answer here is written down as
//! having none.
//!
//! # Why this reads a table of dispatchers and not of chords
//!
//! The preset was a table of *chords* because it was transcribed by
//! hand. This is a table of *dispatchers*, which is strictly better for
//! a live read: a user who moves "close window" from `SUPER + W` to
//! `SUPER + Q` through Omarchy's menu keeps a working close binding,
//! because what was recognised was `killactive`, not the key it
//! happened to be on. Rebinding is exactly the thing this whole module
//! exists to follow.

use crate::preset::Unbound;
use crate::{Action, FocusDirection};

use super::directive::Dispatcher;

/// What one binding's dispatcher turned into.
#[derive(Clone, Debug, PartialEq)]
pub enum Verb {
    /// A chonkstep action, ready to bind.
    Action(Action),
    /// A command line to run, and the argv it splits into. The name it
    /// will be declared under is derived from the argv by
    /// [`command_name`], so two bindings naming the same command share
    /// one `[commands]` entry.
    Run(Vec<String>),
    /// Deliberately unbound, with the preset's own reason.
    Unbound(Unbound),
}

/// Reads a dispatcher. Never fails: an unrecognised dispatcher is
/// [`Unbound::NoVerb`], which is the honest answer and also the safe
/// one — a directive this reader has never seen must cost the user that
/// one chord, never the session.
pub fn verb_for(dispatcher: &Dispatcher) -> Verb {
    match dispatcher {
        Dispatcher::Exec(command) => exec_verb(command),
        Dispatcher::Verb { name, arg } => compositor_verb(name, arg),
        // A Lua closure. Omarchy uses these for the universal
        // clipboard chords and the cursor zoom, both of which the
        // preset already leaves unbound; anything else is equally
        // unreadable without being a Lua interpreter, which this is
        // deliberately not.
        Dispatcher::Opaque(_) => Verb::Unbound(Unbound::NoVerb),
    }
}

/// An `exec` dispatcher: a command to run, unless it commands a
/// compositor that is not running.
fn exec_verb(command: &str) -> Verb {
    let argv = split_command(command);
    let Some(program) = argv.first() else {
        return Verb::Unbound(Unbound::NoVerb);
    };
    if commands_hyprland(program) {
        return Verb::Unbound(Unbound::HyprlandOnly);
    }
    // "Open a terminal" is a verb this desktop has, and the preset
    // already decided it wins over running the guest's launcher:
    // `spawn-terminal` starts the one terminal chonkstep can theme end
    // to end — palette, font size and launch geometry all go on its
    // command line — and with `theme = "omarchy"` that palette *is*
    // Omarchy's. Reading their config live does not change that
    // trade-off, so the judgement is carried over rather than re-made.
    // A user who would rather have the terminal Omarchy configured
    // writes one line, exactly as `docs/omarchy-mode.md` already says:
    // `terminal = "omarchy-launch-terminal"`.
    //
    // Only the bare launcher. `omarchy-launch-terminal-tmux` and
    // `…-herdr` run a *program* in a terminal, which is a different
    // request and stays an ordinary command.
    if argv.len() == 1
        && matches!(
            program.rsplit('/').next().unwrap_or(program),
            "omarchy-launch-terminal" | "xdg-terminal-exec"
        )
    {
        return Verb::Action(Action::SpawnTerminal);
    }
    // A command line only a shell can read keeps its shell, exactly as
    // the preset spells its two such entries: `omarchy-screenrecord`
    // and the colour picker both carry a `||`.
    if needs_a_shell(command) {
        return Verb::Run(vec![
            "bash".into(),
            "-lc".into(),
            command.trim().to_string(),
        ]);
    }
    Verb::Run(argv)
}

/// Whether this program talks to Hyprland rather than to the desktop.
///
/// The same filter `chonk_shell::omarchy_menu` applies to menu rows,
/// and deliberately the same *narrow* one: `hyprpicker`, `hyprlock`
/// and `hypridle` are ordinary Wayland clients that happen to carry
/// the prefix in their names, and this compositor implements every
/// protocol they use. Only the two things that speak Hyprland's own
/// IPC are refused.
pub fn commands_hyprland(program: &str) -> bool {
    let base = program.rsplit('/').next().unwrap_or(program);
    base == "hyprctl" || base.starts_with("omarchy-hyprland-")
}

/// Whether a command line contains shell grammar that argv splitting
/// would destroy.
fn needs_a_shell(command: &str) -> bool {
    command.contains("&&")
        || command.contains("||")
        || command.contains('|')
        || command.contains(';')
        || command.contains('$')
        || command.contains('>')
}

/// A compositor dispatcher and its argument.
///
/// Grouped in the order Omarchy's own `bindings/tiling.lua` is written
/// so a diff against a future release is a read down one file rather
/// than a hunt, exactly as [`crate::preset::OMARCHY_BINDINGS`] is.
fn compositor_verb(name: &str, arg: &str) -> Verb {
    let arg = arg.trim();
    match name.trim().to_ascii_lowercase().as_str() {
        "killactive" | "closewindow" => Verb::Action(Action::Close),
        // Hyprland's `fullscreen` takes a mode: 0 takes the whole
        // output with no chrome, 1 fills the workarea and keeps it.
        // Those are exactly this desktop's fullscreen and maximize, and
        // the pair keeps its shape — the plain chord takes the screen,
        // the modified one takes the workarea.
        "fullscreen" => match arg {
            "" | "0" => Verb::Action(Action::ToggleFullscreen),
            "1" => Verb::Action(Action::ToggleMaximize),
            _ => Verb::Unbound(Unbound::NoVerb),
        },
        // `fullscreenstate` sets the *client's* idea and the
        // compositor's separately, which is how Omarchy builds "tiled
        // fullscreen". There is no tiling here to be full inside of.
        "fullscreenstate" => Verb::Unbound(Unbound::TilingOnly),
        "layoutmsg" | "pseudo" | "togglefloating" | "setfloating" | "settiled" | "swapwindow"
        | "swapnext" | "resizeactive" | "moveactive" | "splitratio" | "pin" | "togglesplit"
        | "movewindoworgroup" | "centerwindow" => Verb::Unbound(Unbound::TilingOnly),
        "togglegroup"
        | "changegroupactive"
        | "moveintogroup"
        | "moveoutofgroup"
        | "lockactivegroup"
        | "lockgroups"
        | "denywindowfromgroup" => Verb::Unbound(Unbound::TilingOnly),
        // Directional focus is spatial over the actual floating frame
        // geometry. Directional movement remains a tiling operation:
        // there is no neighbouring slot to move a free-form window into.
        "movefocus" => match arg.to_ascii_lowercase().as_str() {
            "l" | "left" => Verb::Action(Action::Focus(FocusDirection::Left)),
            "r" | "right" => Verb::Action(Action::Focus(FocusDirection::Right)),
            "u" | "up" => Verb::Action(Action::Focus(FocusDirection::Up)),
            "d" | "down" => Verb::Action(Action::Focus(FocusDirection::Down)),
            _ => Verb::Unbound(Unbound::NoVerb),
        },
        "movewindow" => Verb::Unbound(Unbound::TilingOnly),
        // Workspaces. `e+1`/`e-1` are "the next/previous workspace that
        // exists", which is exactly what this desktop's two workspace
        // verbs do; a bare number is a workspace by index, and
        // `previous`, `special:…` and the monitor-relative forms are
        // not verbs here.
        "workspace" | "focusworkspaceoncurrentmonitor" => match workspace_target(arg) {
            WorkspaceTarget::Next => Verb::Action(Action::WorkspaceNext),
            WorkspaceTarget::Prev => Verb::Action(Action::WorkspacePrev),
            WorkspaceTarget::Index(n) => match workspace_index_action(n) {
                Some(action) => Verb::Action(action),
                None => Verb::Unbound(Unbound::NoVerb),
            },
            // `workspace special:scratchpad` *shows* the scratchpad
            // rather than sending a window to it, which is a
            // workspace this desktop does not have.
            WorkspaceTarget::Special | WorkspaceTarget::Other => Verb::Unbound(Unbound::NoVerb),
        },
        // Silent sends are native: the window leaves and the workspace
        // does not. Relative targets are deliberately left alone here
        // because config actions carry a stable workspace number while
        // direct IPC resolves them against its live snapshot.
        "movetoworkspacesilent" => match workspace_target(arg) {
            WorkspaceTarget::Index(n) => match workspace_send_index_action(n) {
                Some(action) => Verb::Action(action),
                None => Verb::Unbound(Unbound::NoVerb),
            },
            WorkspaceTarget::Special => Verb::Action(Action::Miniaturize),
            _ => Verb::Unbound(Unbound::NoVerb),
        },
        "movetoworkspace" => match workspace_target(arg) {
            WorkspaceTarget::Next => Verb::Action(Action::WorkspaceCarryNext),
            WorkspaceTarget::Prev => Verb::Action(Action::WorkspaceCarryPrev),
            WorkspaceTarget::Index(n) => match workspace_carry_index_action(n) {
                Some(action) => Verb::Action(action),
                None => Verb::Unbound(Unbound::NoVerb),
            },
            // "Move this window to the scratchpad": put it out of the
            // way and leave it recoverable. Chonkstep's nearest true
            // verb is `miniaturize` — the window collapses to an icon
            // tile on the desk rather than onto a special workspace,
            // and it comes back by double-clicking that tile rather
            // than by the same chord. The preset made this call; it is
            // carried over here rather than re-argued.
            WorkspaceTarget::Special => Verb::Action(Action::Miniaturize),
            WorkspaceTarget::Other => Verb::Unbound(Unbound::NoVerb),
        },
        "togglespecialworkspace"
        | "movecurrentworkspacetomonitor"
        | "moveworkspacetomonitor"
        | "focusmonitor"
        | "swapactiveworkspaces" => Verb::Unbound(Unbound::NoVerb),
        // Alt-Tab. This desktop's switcher is modal machinery rather
        // than a binding — while it is up the shell owns the keyboard —
        // so the chord is already answered, correctly, by something
        // that is not in the binding table at all. Binding it to
        // anything from here would break it.
        "cyclenext" | "bringactivetotop" | "focuscurrentorlast" | "alterzorder" => {
            Verb::Unbound(Unbound::Declined)
        }
        // Synthesising a key chord at the seat, which is how Omarchy
        // builds its universal copy/paste. No verb here, and no command
        // could stand in: it is the compositor's own input path.
        "sendshortcut" | "sendkeystate" | "send_key_state" | "sendkey" => {
            Verb::Unbound(Unbound::NoVerb)
        }
        // Talking to the compositor about itself.
        "exit"
        | "forcerendererreload"
        | "dpms"
        | "exec-shutdown"
        | "submap"
        | "global"
        | "setprop"
        | "toggleopaque"
        | "renameworkspace" => Verb::Unbound(Unbound::HyprlandOnly),
        _ => Verb::Unbound(Unbound::NoVerb),
    }
}

/// What a workspace argument names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceTarget {
    /// `e+1` / `+1` / `r+1`: the next workspace.
    Next,
    /// `e-1` / `-1` / `r-1`.
    Prev,
    /// A bare index, 1-based as Hyprland counts them.
    Index(u32),
    /// `special`, `special:scratchpad`.
    Special,
    /// `previous`, `empty`, `name:foo`, `m+1`, anything else.
    Other,
}

fn workspace_target(arg: &str) -> WorkspaceTarget {
    let arg = arg.trim();
    if arg.starts_with("special") {
        return WorkspaceTarget::Special;
    }
    // `e`/`r` are Hyprland's "next existing" and "next in range";
    // both are "the workspace after this one" for a desktop whose
    // workspace list has no holes in it.
    let relative = arg
        .strip_prefix('e')
        .or_else(|| arg.strip_prefix('r'))
        .unwrap_or(arg);
    match relative {
        "+1" => return WorkspaceTarget::Next,
        "-1" => return WorkspaceTarget::Prev,
        _ => {}
    }
    match arg.parse::<u32>() {
        Ok(n) if n >= 1 => WorkspaceTarget::Index(n),
        _ => WorkspaceTarget::Other,
    }
}

/// The verb for "switch to workspace `n`", if this desktop has one.
///
/// Hyprland counts workspaces from one and [`Action::Workspace`]
/// carries a 0-based index, so this function is where the two meet on
/// the Hyprland side — the same conversion
/// [`crate::workspace_index`] makes for a chonkstep config file, kept
/// separate because the inputs differ: that one is validating a
/// number a user typed, this one is reading a number Omarchy
/// generated, and the right answer to "workspace 0" differs (a typo
/// there, a workspace Hyprland itself would refuse here).
///
/// `None` for a number past [`crate::MAX_WORKSPACE`], which reads as
/// [`Unbound::NoVerb`] — the honest answer for a chord that would
/// otherwise grow the workspace row to a size no pager can draw.
fn workspace_index_action(index: u32) -> Option<Action> {
    zero_based(index).map(Action::Workspace)
}

/// As [`workspace_index_action`], for "move this window to workspace
/// `n`".
fn workspace_carry_index_action(index: u32) -> Option<Action> {
    zero_based(index).map(Action::WorkspaceCarry)
}

/// As [`workspace_index_action`], for "send this window to workspace
/// `n` and remain here".
fn workspace_send_index_action(index: u32) -> Option<Action> {
    zero_based(index).map(Action::WorkspaceSend)
}

fn zero_based(index: u32) -> Option<usize> {
    let index = usize::try_from(index).ok()?;
    (1..=crate::MAX_WORKSPACE)
        .contains(&index)
        .then(|| index - 1)
}

/// The `[commands]` name an argv is declared under.
///
/// Derived from the argv rather than from the binding's description
/// for two reasons. Descriptions collide — Omarchy has two bindings
/// described "Browser" and several described "Screenshot" — and they
/// are prose a user may reword or translate through the menu, where
/// the argv is the thing actually being run. Deriving from the argv
/// also makes the mapping a *function*: two chords running the same
/// command land on one entry, and re-reading an unchanged file
/// produces the same names, so nothing downstream sees the table churn
/// because a file was touched.
///
/// Prefixed `hypr:` for the same reason the preset prefixes its own
/// entries `omarchy-`: these are names this desktop generated from
/// somebody else's file, and they must not collide with the short
/// names a user's own `[commands]` table wants. The colon is not legal
/// in a name a user would write, which makes the separation total.
///
/// # Why the hash is not decoration
///
/// A readable slug alone is not a function — it is *lossy*, and the
/// loss is not hypothetical. Omarchy's brightness keys run
/// `omarchy-brightness-display +5%` and `omarchy-brightness-display
/// 5%-`; strip the punctuation a slug cannot carry and both become
/// `omarchy-brightness-display-5`. Two different commands, one name,
/// and the second insert silently wins — so the brightness-down key
/// would raise the brightness. The same collision waits for
/// `+1`/`-1`, and for any pair of commands differing only in an
/// argument a slug drops.
///
/// So the name carries the slug *for a human reading a log* and a
/// 32-bit fingerprint of the exact argv *for correctness*. The slug
/// makes the log line legible; the fingerprint makes the name unique.
pub fn command_name(argv: &[String]) -> String {
    let joined = argv.join(" ");
    let mut slug = String::from("hypr:");
    let mut last_was_dash = true;
    for ch in joined.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.extend(ch.to_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
        // A name is a log line and a docs-table row, so the readable
        // half is bounded. The fingerprint below is taken over the
        // whole argv regardless, so truncating here costs legibility
        // and never uniqueness.
        if slug.len() >= 48 {
            break;
        }
    }
    format!(
        "{}-{:08x}",
        slug.trim_end_matches('-'),
        fingerprint(&joined)
    )
}

/// FNV-1a over the argv. Chosen for being four lines of obvious code
/// with no dependency: this is a name-disambiguator, not a checksum,
/// and the only property it needs is that two different argvs almost
/// never agree — which at 32 bits, over the hundred-odd commands one
/// desktop configuration holds, they do not.
fn fingerprint(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Splits a command line into argv the way a shell would for the
/// simple cases, honouring single and double quotes and backslash
/// escapes, and leaving everything else alone.
///
/// Not a shell: a line with grammar in it is handed to `bash -lc`
/// whole (`needs_a_shell`) rather than mis-split here. This function
/// only has to be right about quoting, which is what Omarchy's own
/// `shell_quote` helper produces — `omarchy-launch-or-focus '^obsidian$'
/// 'uwsm-app -- obsidian'` must come out as three arguments, not six.
pub fn split_command(command: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => current.push(c),
            (Some(_), '\\') => match chars.next() {
                Some(escaped) => current.push(escaped),
                None => current.push('\\'),
            },
            (Some(_), c) => current.push(c),
            (None, '\'') | (None, '"') => {
                quote = Some(ch);
                started = true;
            }
            (None, '\\') => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                    started = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    argv.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        argv.push(current);
    }
    argv
}
