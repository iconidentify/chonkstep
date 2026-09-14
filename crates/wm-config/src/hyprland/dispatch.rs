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
//! Workspace layout dispatchers select Mosaic/Flow and preserve floating
//! membership. Tree-only messages deliberately succeed without changing the
//! scene; unavailable grouping operations remain explicitly unbound.
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
use wm_core::OutputTarget;

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

/// An `exec` dispatcher: a command to run, unless it commands Hyprland
/// in a way chonkstep does not serve.
fn exec_verb(command: &str) -> Verb {
    let argv = split_command(command);
    let Some(program) = argv.first() else {
        return Verb::Unbound(Unbound::NoVerb);
    };
    if argv.len() == 1
        && program.rsplit('/').next() == Some("omarchy-hyprland-workspace-layout-toggle")
    {
        return Verb::Action(Action::ToggleLayout);
    }
    if let Some(reason) = hyprland_refusal(program) {
        return Verb::Unbound(reason);
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

/// `omarchy-hyprland-*` scripts whose every Hyprland IPC request
/// chonkstep serves with `Outcome::Run`. Proven by
/// `omarchy_scripts_send_only_served_requests` in `chonk-hyprland-ipc`'s
/// protocol tests, not asserted here: that test iterates this list and
/// fails for a name with no fixture of the requests it sends, so a
/// script cannot be admitted without the proof.
///
/// Proof matters in both directions. `hyprctl` exits zero whatever the
/// reply, so a script whose request is refused would be a chord that
/// silently does nothing; and a name test that ignores what the IPC
/// serves leaves working chords dead as coverage grows.
///
/// The layout toggle is here because Omarchy's menu runs it as a
/// script. A binding of the bare command still becomes the native
/// `toggle-layout` in [`exec_verb`].
pub const SERVED_OMARCHY_SCRIPTS: &[&str] = &[
    "omarchy-hyprland-window-pop",
    "omarchy-hyprland-window-width",
    "omarchy-hyprland-window-close-all",
    "omarchy-hyprland-monitor-scaling",
    "omarchy-hyprland-workspace-layout-toggle",
    // Reads `.fullscreenClient` and sends `fullscreen_state`, both
    // axes of which the IPC now serves.
    "omarchy-hyprland-window-tiled-fullscreen-toggle",
];

/// The `omarchy-hyprland-*` scripts Omarchy binds or starts whose
/// requests chonkstep does not serve, each with what it needs. They are
/// refused exactly like any unlisted script; the table only lets the
/// refusal name the missing piece. A script moves to
/// [`SERVED_OMARCHY_SCRIPTS`] in the change that makes its requests
/// served.
pub const UNSERVED_OMARCHY_SCRIPTS: &[(&str, Unbound)] = &[
    ("omarchy-hyprland-window-transparency-toggle", Unbound::OPACITY),
    // Both rewrite a Hyprland config flag and run `hyprctl reload`.
    ("omarchy-hyprland-window-gaps-toggle", Unbound::GAPS),
    ("omarchy-hyprland-window-single-square-aspect-toggle", Unbound::LAYOUT_OPTION),
    ("omarchy-hyprland-monitor-internal", Unbound::OUTPUT_DISABLE),
    ("omarchy-hyprland-monitor-internal-mirror", Unbound::OUTPUT_MIRROR),
    ("omarchy-hyprland-monitor-clamshell", Unbound::OUTPUT_DISABLE),
    // The watcher that re-applies the clamshell and display toggles.
    ("omarchy-hyprland-monitor-watch", Unbound::OUTPUT_DISABLE),
];

/// Why this program may not run because it commands Hyprland, or `None`
/// when it may.
///
/// `hyprctl` itself is refused: its requests are whatever its caller
/// writes, so nothing can prove them served. So is every
/// `omarchy-hyprland-*` script outside [`SERVED_OMARCHY_SCRIPTS`], which
/// is also the conservative answer for a script a future Omarchy adds.
/// The test is deliberately *narrow* otherwise: `hyprpicker`,
/// `hyprlock` and `hypridle` are ordinary Wayland clients that happen
/// to carry the prefix in their names, and this compositor implements
/// every protocol they use.
pub fn hyprland_refusal(program: &str) -> Option<Unbound> {
    let base = program.rsplit('/').next().unwrap_or(program);
    if base == "hyprctl" {
        return Some(Unbound::HyprlandOnly);
    }
    if !base.starts_with("omarchy-hyprland-") || SERVED_OMARCHY_SCRIPTS.contains(&base) {
        return None;
    }
    Some(
        UNSERVED_OMARCHY_SCRIPTS
            .iter()
            .find(|(script, _)| *script == base)
            .map_or(Unbound::HyprlandOnly, |(_, reason)| *reason),
    )
}

/// Whether this program commands Hyprland in a way chonkstep does not
/// serve.
///
/// The one predicate bindings, `exec-once` lines and
/// `chonk_shell::omarchy_menu`'s rows all apply, so a chord, an
/// autostart entry and a menu row cannot disagree about a script.
pub fn commands_hyprland(program: &str) -> bool {
    hyprland_refusal(program).is_some()
}

/// One axis of `fullscreenstate`, in Hyprland's numbering.
fn fullscreen_mode(level: &str) -> Option<wm_core::FullscreenMode> {
    level.parse::<u8>().ok().and_then(wm_core::FullscreenMode::from_level)
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
        // `fullscreenstate <internal> <client>` sets the compositor's
        // idea and the client's separately, which is how Omarchy's
        // classic bindings spell "tiled fullscreen" (`0 2`). Both axes
        // are required, each 0, 1 or 2; anything else is refused rather
        // than rounded to a state the chord did not ask for.
        "fullscreenstate" => match arg.split_whitespace().collect::<Vec<_>>().as_slice() {
            [internal, client] => match (fullscreen_mode(internal), fullscreen_mode(client)) {
                (Some(internal), Some(client)) => Verb::Action(Action::FullscreenState { internal, client }),
                _ => Verb::Unbound(Unbound::NoVerb),
            },
            _ => Verb::Unbound(Unbound::NoVerb),
        },
        "togglefloating" => Verb::Action(Action::Floating(None)),
        "setfloating" => Verb::Action(Action::Floating(Some(true))),
        "settiled" => Verb::Action(Action::Floating(Some(false))),
        "layoutmsg" | "togglesplit" | "swapsplit" | "pseudo" | "splitratio" => {
            Verb::Action(Action::LayoutNoop)
        }
        // `resizeactive x y` is a delta, in logical pixels. Exactly two
        // integers and nothing else: the `exact w h` form sets a size,
        // which this desktop has no binding verb for, and reading its
        // numbers as a delta would grow the window by the size it asked
        // for. Percentages and every other spelling are refused alike.
        "resizeactive" => match arg.split_whitespace().collect::<Vec<_>>().as_slice() {
            [x, y] => match (x.parse::<i32>(), y.parse::<i32>()) {
                (Ok(x), Ok(y)) => Verb::Action(Action::Resize(wm_core::Point::new(x, y))),
                _ => Verb::Unbound(Unbound::NoVerb),
            },
            _ => Verb::Unbound(Unbound::NoVerb),
        },
        "swapnext" | "moveactive" | "pin" | "centerwindow" => Verb::Unbound(Unbound::TilingOnly),
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
        "movewindow" | "swapwindow" | "movewindoworgroup" => {
            match compositor_verb("movefocus", arg) {
                Verb::Action(Action::Focus(direction)) => Verb::Action(Action::Move(direction)),
                _ => Verb::Unbound(Unbound::NoVerb),
            }
        }
        // Workspaces. `e+1`/`e-1` are "the next/previous workspace that
        // exists": only workspaces with windows on them, wrapping, which
        // is what Omarchy's SUPER+TAB means and not what this desktop's
        // own `workspace-next` does (step by index, grow the row), so
        // each keeps its verb. `+1`/`-1` step by index; a bare number
        // is a workspace by index; `previous` is the two-workspace flip;
        // `special` and `special:NAME` name a special workspace, which a
        // switch shows and a move sends a window to; `name:…` and the
        // monitor-relative forms are not verbs here.
        "workspace" | "focusworkspaceoncurrentmonitor" => match workspace_target(arg) {
            WorkspaceTarget::Next => Verb::Action(Action::WorkspaceNext),
            WorkspaceTarget::Prev => Verb::Action(Action::WorkspacePrev),
            WorkspaceTarget::NextExisting => Verb::Action(Action::WorkspaceNextOccupied),
            WorkspaceTarget::PrevExisting => Verb::Action(Action::WorkspacePrevOccupied),
            WorkspaceTarget::Previous => Verb::Action(Action::WorkspacePrevious),
            WorkspaceTarget::Index(n) => match workspace_index_action(n) {
                Some(action) => Verb::Action(action),
                None => Verb::Unbound(Unbound::NoVerb),
            },
            // `workspace special:scratchpad` shows the scratchpad — the
            // same overlay `togglespecialworkspace` drops down.
            WorkspaceTarget::Special(name) => Verb::Action(Action::ToggleSpecial(name)),
            WorkspaceTarget::Other => Verb::Unbound(Unbound::NoVerb),
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
            // Omarchy's "move window to scratchpad": the window goes
            // onto the special workspace and stays hidden until the
            // toggle chord brings the overlay down.
            WorkspaceTarget::Special(name) => Verb::Action(Action::SendToSpecial { name, follow: false }),
            _ => Verb::Unbound(Unbound::NoVerb),
        },
        // Carrying a window to the next existing workspace is carrying
        // it to the next one: a window in tow makes the destination
        // occupied either way.
        "movetoworkspace" => match workspace_target(arg) {
            WorkspaceTarget::Next | WorkspaceTarget::NextExisting => Verb::Action(Action::WorkspaceCarryNext),
            WorkspaceTarget::Prev | WorkspaceTarget::PrevExisting => Verb::Action(Action::WorkspaceCarryPrev),
            WorkspaceTarget::Index(n) => match workspace_carry_index_action(n) {
                Some(action) => Verb::Action(action),
                None => Verb::Unbound(Unbound::NoVerb),
            },
            WorkspaceTarget::Special(name) => Verb::Action(Action::SendToSpecial { name, follow: true }),
            WorkspaceTarget::Previous | WorkspaceTarget::Other => Verb::Unbound(Unbound::NoVerb),
        },
        // The scratchpad toggle. No argument means Hyprland's default
        // special workspace; a name the core would refuse to create
        // (too long, or unprintable) leaves the chord unbound.
        "togglespecialworkspace" => match wm_core::normalize_special_name(arg) {
            Some(name) => Verb::Action(Action::ToggleSpecial(name)),
            None => Verb::Unbound(Unbound::NoVerb),
        },
        // Monitors. `focusmonitor` takes a step, a direction or an
        // output name, and so does `movecurrentworkspacetomonitor`,
        // which moves the active Space under separate Spaces and is
        // refused with a reason on the shared desktop. The forms that
        // name a workspace *and* a monitor stay unbound.
        "focusmonitor" => match output_target(arg) {
            Some(target) => Verb::Action(Action::FocusMonitor(target)),
            None => Verb::Unbound(Unbound::NoVerb),
        },
        "movecurrentworkspacetomonitor" => match output_target(arg) {
            Some(target) => Verb::Action(Action::MoveWorkspaceToMonitor(target)),
            None => Verb::Unbound(Unbound::NoVerb),
        },
        "moveworkspacetomonitor" | "swapactiveworkspaces" => Verb::Unbound(Unbound::NoVerb),
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
        "global" => {
            let target = arg.trim();
            match target.split_once(':') {
                Some((_, id)) if !id.is_empty() => {
                    Verb::Action(Action::GlobalShortcut(target.to_string()))
                }
                _ => Verb::Unbound(Unbound::NoVerb),
            }
        }
        "exit"
        | "forcerendererreload"
        | "dpms"
        | "exec-shutdown"
        | "submap"
        | "setprop"
        | "toggleopaque"
        | "renameworkspace" => Verb::Unbound(Unbound::HyprlandOnly),
        _ => Verb::Unbound(Unbound::NoVerb),
    }
}

/// What a workspace argument names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkspaceTarget {
    /// `+1` / `r+1`: the next workspace by index.
    Next,
    /// `-1` / `r-1`.
    Prev,
    /// `e+1`: the next workspace that has windows on it.
    NextExisting,
    /// `e-1`.
    PrevExisting,
    /// `previous`: the workspace before this one.
    Previous,
    /// A bare index, 1-based as Hyprland counts them.
    Index(u32),
    /// `special`, `special:scratchpad`: a special workspace, by the
    /// name the core will create it under.
    Special(String),
    /// `empty`, `name:foo`, `m+1`, anything else — and a special name
    /// the core would refuse.
    Other,
}

fn workspace_target(arg: &str) -> WorkspaceTarget {
    let arg = arg.trim();
    if arg == "special" || arg.starts_with("special:") {
        return match wm_core::normalize_special_name(arg) {
            Some(name) => WorkspaceTarget::Special(name),
            None => WorkspaceTarget::Other,
        };
    }
    if arg == "previous" {
        return WorkspaceTarget::Previous;
    }
    // `e` is Hyprland's "next existing" and `r` its "next in range";
    // the second is "the workspace after this one" for a desktop whose
    // workspace list has no holes in it, the first is not.
    match arg {
        "e+1" => return WorkspaceTarget::NextExisting,
        "e-1" => return WorkspaceTarget::PrevExisting,
        _ => {}
    }
    let relative = arg.strip_prefix('r').unwrap_or(arg);
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

/// A monitor argument: `+N` / `-N`, `l`/`r`/`u`/`d` (or the words), or
/// an output name as `hyprctl monitors` reports it. `None` for nothing,
/// a name no config file should carry, or a step so large it can only
/// be a mistake; a step wraps around the monitor list regardless.
fn output_target(arg: &str) -> Option<OutputTarget> {
    let arg = arg.trim();
    if arg.is_empty() || arg.len() > 256 || arg.chars().any(char::is_control) {
        return None;
    }
    if let Some(step) = arg.strip_prefix('+').and_then(|digits| digits.parse::<i32>().ok()) {
        return (step.abs() <= 64).then_some(OutputTarget::Relative(step));
    }
    if let Some(step) = arg.strip_prefix('-').and_then(|digits| digits.parse::<i32>().ok()) {
        return (step.abs() <= 64).then_some(OutputTarget::Relative(step.saturating_neg()));
    }
    // A sign that did not read as a step is a malformed step, not an
    // output called `+`.
    if arg.starts_with('+') || arg.starts_with('-') {
        return None;
    }
    Some(match arg.to_ascii_lowercase().as_str() {
        "l" | "left" => OutputTarget::Direction(FocusDirection::Left),
        "r" | "right" => OutputTarget::Direction(FocusDirection::Right),
        "u" | "up" => OutputTarget::Direction(FocusDirection::Up),
        "d" | "down" => OutputTarget::Direction(FocusDirection::Down),
        _ => OutputTarget::Name(arg.to_string()),
    })
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
/// otherwise ask for a workspace beyond the core's fixed ceiling.
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
