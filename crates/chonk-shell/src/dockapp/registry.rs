//! The `.dockapp` registry: which out-of-process tiles this session
//! knows how to launch.
//!
//! # Why not `.desktop` files
//!
//! `apps.rs` already owns a `.desktop` index, and reusing it would have
//! been less code. It would also have put every dockapp in the
//! Applications menu, where launching one produces a process that draws
//! nothing anybody can see — it has no window, only a tile it cannot
//! get because no slot minted it a token. Separate namespaces keep
//! "things you can launch" and "things the dock hosts" from being the
//! same list by accident.
//!
//! # The format
//!
//! One TOML file per dockapp, named `<anything>.dockapp`:
//!
//! ```toml
//! id = "chonk-dockclock"
//! name = "CLK"
//! exec = ["chonk-dockclock"]
//! tile_units = 1
//! restart = "on-crash"
//! ```
//!
//! `exec` is an argv array rather than a `.desktop`-style command
//! string on purpose: a string has to be split, and every splitter
//! either implements shell quoting (and inherits its bugs) or gets
//! paths with spaces wrong. An array has one reading.
//!
//! # Where they are read from
//!
//! `$XDG_DATA_DIRS/chonkstep/dockapps/` for installed ones, then
//! `$XDG_CONFIG_HOME/chonkstep/dockapps/` for the user's own, which
//! wins on an id collision. Same precedence as every other XDG lookup:
//! the user's copy shadows the system's, so a dockapp can be
//! reconfigured without touching a file the package manager owns.
//!
//! # Everything here is hostile input
//!
//! A `.dockapp` file is on disk, which makes it attacker-chosen in the
//! same weak sense as anything else on disk — but its `id` becomes a
//! persistence key, a log field, and a menu label, and its `exec`
//! becomes an argv the shell runs. So each field is validated rather
//! than trusted, and a file that fails validation is dropped with a log
//! line naming the file and the reason, not silently ignored: a dockapp
//! that does not appear and does not say why is indistinguishable from
//! one the shell never looked for.

use std::path::{Path, PathBuf};

use chonk_dock_proto::wire::is_valid_id;
use chonk_dock_proto::MAX_TILE_UNITS;
use serde::Deserialize;

use crate::widgets::BUILTIN_PREFIX;

/// What to do when a dockapp's process goes away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RestartPolicy {
    /// Relaunch after a crash, but not after a clean exit — a dockapp
    /// that exits zero has decided it is done (a battery tile on a
    /// desktop with no battery, say) and relaunching it is an argument
    /// the shell cannot win.
    #[default]
    OnCrash,
    /// Relaunch whatever the exit status. Still subject to the
    /// crash-loop cutoff; "always" is a policy about *why*, never about
    /// *how often*.
    Always,
    /// One launch per session. A tile that goes away stays away until
    /// the user picks Restart from its menu.
    Never,
}

/// One registered dockapp, validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DockappEntry {
    /// The registry key. Also the dock-order persistence line, the
    /// `Hello` id, and the per-tile menu's title.
    pub id: String,
    /// The short label drawn on the tile's dead/starting face — the
    /// same three-or-four-character shape the built-in instruments use
    /// ("NET", "SND", "LNK"), because it is drawn by the same renderer.
    pub name: String,
    /// argv. `exec[0]` is the program.
    pub exec: Vec<String>,
    pub tile_units: u8,
    pub restart: RestartPolicy,
    /// Which file declared this, for the log and the About menu. A
    /// registry problem is always "which file?" first.
    pub source: PathBuf,
}

/// The on-disk shape, before validation. Separate from
/// [`DockappEntry`] so that "what TOML allows" and "what the dock will
/// run" are two types and the conversion between them is a function
/// with a reason for every rejection.
#[derive(Deserialize)]
struct RawDockapp {
    id: String,
    name: Option<String>,
    exec: Vec<String>,
    tile_units: Option<u8>,
    restart: Option<RestartPolicy>,
}

/// Why a `.dockapp` file was not admitted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RejectReason {
    Unparsable(String),
    EmptyId,
    /// The id is not `[A-Za-z0-9._-]{1,64}` — see
    /// `chonk_dock_proto::wire::is_valid_id`. The same rule the wire
    /// enforces, applied here so a file that could never complete a
    /// handshake is rejected where the reason is visible rather than at
    /// a `Hello` nobody is watching.
    MalformedId,
    /// It claimed the built-in namespace. Rejected outright rather than
    /// renamed: an id is a persistence key, and a dockapp that took
    /// `builtin:clock` would inherit the analog clock's line in the
    /// user's `dock-items` file.
    ReservedId,
    EmptyExec,
    /// `tile_units` outside 1..=`MAX_TILE_UNITS`. The dock is a
    /// vertical strip on a real screen; asking for more than four tiles
    /// is asking for the dock, not a slot in it.
    BadTileUnits(u8),
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RejectReason::Unparsable(detail) => write!(f, "not valid TOML: {detail}"),
            RejectReason::EmptyId => write!(f, "`id` is empty"),
            RejectReason::MalformedId => write!(f, "`id` must be 1-64 characters of [A-Za-z0-9._-]"),
            RejectReason::ReservedId => write!(f, "`id` may not begin with the reserved `{BUILTIN_PREFIX}` prefix"),
            RejectReason::EmptyExec => write!(f, "`exec` is empty; it must be an argv array whose first element is the program"),
            RejectReason::BadTileUnits(units) => write!(f, "`tile_units` is {units}, outside 1..={MAX_TILE_UNITS}"),
        }
    }
}

/// Validates one file's contents. Pure over the text, so every
/// rejection above is testable without a filesystem.
pub(crate) fn parse(text: &str, source: &Path) -> Result<DockappEntry, RejectReason> {
    let raw: RawDockapp = toml::from_str(text).map_err(|error| RejectReason::Unparsable(error.to_string()))?;
    if raw.id.trim().is_empty() {
        return Err(RejectReason::EmptyId);
    }
    if raw.id.starts_with(BUILTIN_PREFIX) {
        return Err(RejectReason::ReservedId);
    }
    if !is_valid_id(&raw.id) {
        return Err(RejectReason::MalformedId);
    }
    if raw.exec.is_empty() || raw.exec[0].trim().is_empty() {
        return Err(RejectReason::EmptyExec);
    }
    let tile_units = raw.tile_units.unwrap_or(1);
    if tile_units == 0 || tile_units > MAX_TILE_UNITS {
        return Err(RejectReason::BadTileUnits(tile_units));
    }
    // A missing `name` falls back to the id, upper-cased and clipped to
    // what the dead-tile face can draw. Not an error: a one-tile
    // instrument whose id is already short has nothing else to say, and
    // making every author write the label twice is how labels drift
    // from ids.
    let name = raw.name.unwrap_or_else(|| raw.id.to_uppercase().chars().take(5).collect());
    Ok(DockappEntry {
        id: raw.id,
        name,
        exec: raw.exec,
        tile_units,
        restart: raw.restart.unwrap_or_default(),
        source: source.to_path_buf(),
    })
}

/// Every directory a `.dockapp` file may live in, system first so the
/// user's own copies (scanned last) win an id collision.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for root in data_dirs.split(':').filter(|entry| !entry.is_empty()) {
        dirs.push(PathBuf::from(root).join("chonkstep/dockapps"));
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    if let Some(config) = config_home {
        dirs.push(config.join("chonkstep/dockapps"));
    }
    dirs
}

/// Reads one directory's `*.dockapp` files into `into`, later ids
/// replacing earlier ones.
fn scan_dir(dir: &Path, into: &mut Vec<DockappEntry>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        // A search directory that does not exist is the normal case on
        // a machine with no dockapps installed, so this is not even a
        // debug line.
        return;
    };
    // Sorted so the set of tiles a session comes up with does not
    // depend on inode order — the dock's default column has to be the
    // same on two machines with the same files.
    let mut paths: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "dockapp"))
        .collect();
    paths.sort();

    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(path = %path.display(), ?error, "could not read a .dockapp file");
                continue;
            }
        };
        match parse(&text, &path) {
            Ok(entry) => {
                if let Some(existing) = into.iter_mut().find(|existing| existing.id == entry.id) {
                    tracing::info!(id = %entry.id, shadowed = %existing.source.display(), by = %path.display(), "a dockapp registration is shadowed");
                    *existing = entry;
                } else {
                    into.push(entry);
                }
            }
            Err(reason) => {
                tracing::warn!(path = %path.display(), %reason, "ignoring a .dockapp file");
            }
        }
    }
}

/// Every dockapp this session knows how to launch, in a stable order.
///
/// Scanned once at startup, like `apps::scan_applications`. A dockapp
/// installed mid-session appears at the next restart; rescanning on a
/// schedule is future work, and the failure mode of not doing it is
/// "the new tile shows up when you restart", which is the same deal
/// the Applications menu already offers.
pub(crate) fn scan() -> Vec<DockappEntry> {
    let mut entries = Vec::new();
    for dir in search_dirs() {
        scan_dir(&dir, &mut entries);
    }
    tracing::info!(count = entries.len(), "dockapp registrations found");
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Result<DockappEntry, RejectReason> {
        parse(text, Path::new("/test/x.dockapp"))
    }

    #[test]
    fn a_minimal_registration_gets_the_documented_defaults() {
        let entry = parsed("id = \"clockish\"\nexec = [\"chonk-dockclock\"]\n").unwrap();
        assert_eq!(entry.id, "clockish");
        assert_eq!(entry.tile_units, 1, "one square tile is the default shape");
        assert_eq!(entry.restart, RestartPolicy::OnCrash);
        assert_eq!(entry.name, "CLOCK", "the label falls back to the id, clipped to what the tile face can draw");
    }

    #[test]
    fn every_field_round_trips() {
        let entry = parsed(
            "id = \"chonk-dockclock\"\nname = \"CLK\"\nexec = [\"chonk-dockclock\", \"--big\"]\ntile_units = 2\nrestart = \"always\"\n",
        )
        .unwrap();
        assert_eq!(entry.name, "CLK");
        assert_eq!(entry.exec, ["chonk-dockclock", "--big"]);
        assert_eq!(entry.tile_units, 2);
        assert_eq!(entry.restart, RestartPolicy::Always);
    }

    /// The reserved namespace, enforced where a human can see the
    /// error rather than at a handshake nobody is watching.
    #[test]
    fn a_dockapp_cannot_claim_a_built_in_id() {
        assert_eq!(parsed("id = \"builtin:clock\"\nexec = [\"x\"]\n"), Err(RejectReason::ReservedId));
    }

    #[test]
    fn a_malformed_id_is_rejected_here_rather_than_at_the_handshake() {
        // The same rule the wire enforces: a file that could never
        // complete a `Hello` should not become a dock slot that waits
        // forever for one.
        assert_eq!(parsed("id = \"has spaces\"\nexec = [\"x\"]\n"), Err(RejectReason::MalformedId));
        assert_eq!(parsed("id = \"../../etc/passwd\"\nexec = [\"x\"]\n"), Err(RejectReason::MalformedId));
        assert_eq!(parsed("id = \"\"\nexec = [\"x\"]\n"), Err(RejectReason::EmptyId));
    }

    #[test]
    fn a_registration_with_nothing_to_run_is_not_a_registration() {
        assert_eq!(parsed("id = \"x\"\nexec = []\n"), Err(RejectReason::EmptyExec));
        assert_eq!(parsed("id = \"x\"\nexec = [\"  \"]\n"), Err(RejectReason::EmptyExec));
    }

    #[test]
    fn a_tile_taller_than_the_protocol_carries_is_refused_at_the_registry() {
        assert_eq!(parsed("id = \"x\"\nexec = [\"y\"]\ntile_units = 0\n"), Err(RejectReason::BadTileUnits(0)));
        assert_eq!(parsed("id = \"x\"\nexec = [\"y\"]\ntile_units = 9\n"), Err(RejectReason::BadTileUnits(9)));
        assert!(parsed(&format!("id = \"x\"\nexec = [\"y\"]\ntile_units = {MAX_TILE_UNITS}\n")).is_ok());
    }

    #[test]
    fn a_file_that_is_not_toml_says_so_rather_than_panicking() {
        assert!(matches!(parsed("this is not toml at all ]["), Err(RejectReason::Unparsable(_))));
        assert!(matches!(parsed(""), Err(RejectReason::Unparsable(_))), "a file with no id and no exec is unparsable, not empty-id");
    }

    #[test]
    fn an_unknown_restart_policy_is_a_parse_error_rather_than_a_silent_default() {
        // Silently defaulting would turn a typo in `restart` into a
        // policy the author did not choose, which for `never` vs
        // `always` is the difference between one launch and a
        // supervised one.
        assert!(matches!(parsed("id = \"x\"\nexec = [\"y\"]\nrestart = \"sometimes\"\n"), Err(RejectReason::Unparsable(_))));
    }
}
