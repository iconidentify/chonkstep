//! Hosting Omarchy's shell.
//!
//! # Why the desktop starts a process it did not write
//!
//! Omarchy 4's user interface is one Quickshell process — `omarchy-shell`,
//! `$OMARCHY_PATH/shell/shell.qml` — and most of Omarchy's scripts are
//! only half a feature without it. `omarchy-menu` opens a panel that
//! shell draws; the speed tests summon one; the theme, background and
//! image pickers are its selectors; notifications, the volume and
//! brightness OSD, the clipboard and emoji pickers and the lock screen
//! are its plugins; `omarchy-theme-set` ends by telling it to repaint.
//! Every one of those rows mirrored into the root menu by
//! [`crate::omarchy_menu`] runs to completion and then, with no shell
//! listening, does nothing visible — thirty-seven of Omarchy's scripts
//! check for the shell and exit quietly when it is not there.
//!
//! Under Hyprland the shell is there because Omarchy's `autostart.lua`
//! execs `omarchy-launch-shell` as the session comes up. Chonkstep
//! stands where Hyprland stood, so it does the same, at the same
//! moment (beside the config's own `autostart` list) and through the
//! same launcher: a small supervising script that runs Quickshell
//! under `systemd-cat` so the shell's log lands in the journal and
//! relaunches it after an abnormal exit. Running the launcher rather
//! than Quickshell itself is deliberate — it is Omarchy's own start
//! path, and whatever Omarchy changes about it next release reaches
//! this desktop without a change here.
//!
//! # What is different from Hyprland
//!
//! The launcher is run as `bash -lc '<path to omarchy-launch-shell>'`,
//! the form every Omarchy menu action takes
//! ([`crate::omarchy_menu::action_argv`]) because the login shell is
//! what exports `OMARCHY_PATH` and puts `$OMARCHY_PATH/bin` on `PATH` —
//! the launcher itself and everything it starts need both.
//!
//! It is *not* gated on [`crate::startup::session_continues`], unlike
//! `autostart`. That gate exists because an X11 hot restart keeps every
//! client alive through the SaveSet, and relaunching would double them.
//! Quickshell is a Wayland client, and a Wayland re-exec closes the
//! display: the shell dies with it, and its supervisor — which asks
//! `hyprctl` whether the compositor is alive, an answer chonkstep never
//! gives — exits rather than relaunching. The new process therefore has
//! no shell unless it starts one. There is no X11 case to double.
//!
//! The launcher is named by its resolved path under the Omarchy root
//! rather than by bare name (Hyprland's autostart says
//! `omarchy-launch-shell` and lets `PATH` find it), because the desk
//! has just checked that very file exists — and because a test can then
//! stand up an Omarchy root of its own, whose launcher is whatever the
//! test needs.
//!
//! # The bar is the user's to show
//!
//! Omarchy's bar is the shell's most visible surface, and this desk
//! already has a Dock and a Clip in the corners it wants. So the bar is
//! *off by default* and switched on from the root menu's `Omarchy Bar`
//! row; the choice is remembered across sessions in chonkstep's own
//! state ([`BarVisibility`]), never in Omarchy's — `omarchy-toggle-bar`
//! writes a flag Omarchy's Hyprland session reads too, and a preference
//! about this desk should not follow the user into that one. Hiding is
//! the compositor's doing (`Backend::set_layer_surface_hidden` on the
//! bar's namespace): the bar keeps running, keeps its clock, and takes
//! no space, no clicks and no pixels until it is asked for.
//!
//! The one part of the shell this desktop declines is its Background
//! plugin: a full-screen surface on the layer-shell `background` layer
//! that would paint Omarchy's wallpaper over chonkstep's own and take
//! every click on the desk — the root menu's right-click included. The
//! compositor keeps the surface configured and answered but neither
//! draws nor hit-tests it (`wm_wayland::layers::declined`); the desk
//! stays chonkstep's, wearing Omarchy's background through
//! [`crate::wallpaper::Wallpaper::Omarchy`] when the theme follows.
//!
//! # When it does not happen
//!
//! `omarchy_shell = false` in the config; an X11 session (Quickshell is
//! Wayland-only); or no shell to start — the test for "installed" is
//! the two files the launcher itself needs, `shell/shell.qml` and
//! `bin/omarchy-launch-shell` under the Omarchy root. Nothing in the
//! session tests any further: a shell that fails to come up costs the
//! user the shell, never the session, which is the same rule
//! `autostart` entries live by.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::spawn::DisplayStack;

/// The file name of the script Omarchy's own `autostart.lua` runs,
/// resolved here under the Omarchy root's `bin/` ([`ShellPaths`]).
pub const LAUNCHER: &str = "omarchy-launch-shell";

/// The two files under the Omarchy root the launcher needs, whose
/// presence is the whole test for "Omarchy's shell is installed here".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellPaths {
    /// `$OMARCHY_PATH/shell/shell.qml`, the file the launcher hands
    /// Quickshell.
    pub shell_qml: PathBuf,
    /// `$OMARCHY_PATH/bin/omarchy-launch-shell` — a symlink into
    /// `/usr/bin` on a packaged install, which `is_file` follows.
    pub launcher: PathBuf,
}

impl ShellPaths {
    /// Resolves both paths from the process environment, or `None` if
    /// either file is missing — an Omarchy without a Quickshell shell
    /// (release 3 and earlier) has nothing here to start.
    pub fn discover() -> Option<Self> {
        let paths = Self::from_env(std::env::var_os("OMARCHY_PATH"), std::env::var_os("HOME"));
        paths.installed().then_some(paths)
    }

    /// The pure half of [`Self::discover`], rooted where
    /// [`crate::omarchy_menu::omarchy_root`] roots the menu.
    pub fn from_env(omarchy_path: Option<OsString>, home: Option<OsString>) -> Self {
        let home = home.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        Self::under(&crate::omarchy_menu::omarchy_root(omarchy_path, &home))
    }

    /// Both paths under one Omarchy root.
    pub fn under(root: &Path) -> Self {
        Self { shell_qml: root.join("shell/shell.qml"), launcher: root.join("bin").join(LAUNCHER) }
    }

    /// Whether both files exist.
    pub fn installed(&self) -> bool {
        self.shell_qml.is_file() && self.launcher.is_file()
    }
}

/// What a session does about Omarchy's shell, and why. Every variant
/// but [`Verdict::Launch`] is a reason not to, in the order the reasons
/// are checked; the shell logs the reason once at boot so a user asking
/// "why is there no bar" finds the answer in the session log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Run the launcher found at these paths.
    Launch(ShellPaths),
    /// `omarchy_shell = false`: the user turned it off.
    Disabled,
    /// Not a Wayland session. Quickshell is a Wayland client, and the
    /// launcher's supervisor would only find no display to connect to.
    NotWayland,
    /// No `shell/shell.qml` or no `omarchy-launch-shell` under the
    /// Omarchy root: nothing to start.
    NotInstalled,
}

/// The rule, pure so the tests can reach every branch: the shell is
/// launched when the key is on, the stack is Wayland, and the files are
/// there — in that order, so the log names the *first* reason.
pub fn verdict(enabled: bool, stack: DisplayStack, installed: Option<ShellPaths>) -> Verdict {
    if !enabled {
        Verdict::Disabled
    } else if stack != DisplayStack::Wayland {
        Verdict::NotWayland
    } else {
        match installed {
            Some(paths) => Verdict::Launch(paths),
            None => Verdict::NotInstalled,
        }
    }
}

/// [`verdict`] for this process: the config key against the running
/// stack and the installed files.
pub fn decide(enabled: bool) -> Verdict {
    verdict(enabled, crate::spawn::current_display_stack(), ShellPaths::discover())
}

/// The command line the shell hands `bash -lc`: the launcher by its
/// resolved path, single-quoted so a path with a space or a `$` in it
/// survives the shell (a quote inside the path — legal, if unlikely —
/// is spelled the one way POSIX allows, `'\''`).
pub fn launch_command(paths: &ShellPaths) -> String {
    let path = paths.launcher.to_string_lossy();
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// The namespace Omarchy's shell gives its bar (`plugins/bar/Bar.qml`).
pub const BAR_NAMESPACE: &str = "omarchy-bar";

/// Whether this desk shows Omarchy's bar. The user's choice, made in
/// the root menu and remembered in chonkstep's own state file; hidden
/// until they make one — see the module docs for why the default and
/// the storage are both this desk's rather than Omarchy's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarVisibility {
    Hidden,
    Shown,
}

impl BarVisibility {
    pub const DEFAULT: Self = Self::Hidden;

    /// The persisted choice, or the default when there is none or it
    /// is unreadable.
    pub fn load() -> Self {
        bar_state_path().map_or(Self::DEFAULT, |path| Self::load_from(&path))
    }

    /// [`Self::load`] against an explicit file.
    pub fn load_from(path: &Path) -> Self {
        Self::from_state(std::fs::read_to_string(path).ok().as_deref())
    }

    /// The pure half of [`Self::load`]: the state file's text.
    pub fn from_state(text: Option<&str>) -> Self {
        match text.map(str::trim) {
            Some("shown") => Self::Shown,
            Some("hidden") => Self::Hidden,
            _ => Self::DEFAULT,
        }
    }

    /// The word the state file holds for this choice.
    pub fn id(self) -> &'static str {
        match self {
            Self::Hidden => "hidden",
            Self::Shown => "shown",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Hidden => Self::Shown,
            Self::Shown => Self::Hidden,
        }
    }

    pub fn is_hidden(self) -> bool {
        self == Self::Hidden
    }

    /// Remembers the choice; a session with nowhere to remember it
    /// (no state directory) succeeds silently, like every other state
    /// file here.
    pub fn persist(self) -> std::io::Result<()> {
        match bar_state_path() {
            Some(path) => self.persist_to(&path),
            None => Ok(()),
        }
    }

    /// [`Self::persist`] to an explicit file, creating its directory.
    pub fn persist_to(self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.id())
    }
}

/// `$XDG_STATE_HOME/chonkstep/omarchy-bar`, beside `wallpaper` and
/// `theme`.
fn bar_state_path() -> Option<PathBuf> {
    crate::startup::state_file("omarchy-bar")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> Option<ShellPaths> {
        Some(ShellPaths::under(Path::new("/usr/share/omarchy")))
    }

    #[test]
    fn the_shell_launches_only_when_every_condition_holds() {
        assert_eq!(verdict(true, DisplayStack::Wayland, installed()), Verdict::Launch(installed().unwrap()));
        assert_eq!(verdict(false, DisplayStack::Wayland, installed()), Verdict::Disabled);
        assert_eq!(verdict(true, DisplayStack::X11, installed()), Verdict::NotWayland);
        assert_eq!(verdict(true, DisplayStack::Wayland, None), Verdict::NotInstalled);
    }

    #[test]
    fn the_first_reason_is_the_one_reported() {
        // Off *and* on X11 *and* not installed: the user's own choice
        // is the reason worth logging, not the machine's limitations.
        assert_eq!(verdict(false, DisplayStack::X11, None), Verdict::Disabled);
        assert_eq!(verdict(true, DisplayStack::X11, None), Verdict::NotWayland);
    }

    #[test]
    fn the_launch_command_is_the_launchers_path_quoted_for_bash() {
        assert_eq!(launch_command(&installed().unwrap()), "'/usr/share/omarchy/bin/omarchy-launch-shell'");
        let odd = ShellPaths::under(Path::new("/tmp/it's here/omarchy"));
        assert_eq!(launch_command(&odd), "'/tmp/it'\\''s here/omarchy/bin/omarchy-launch-shell'");
    }

    #[test]
    fn the_bar_is_hidden_until_the_user_says_otherwise() {
        assert_eq!(BarVisibility::from_state(None), BarVisibility::Hidden);
        assert_eq!(BarVisibility::from_state(Some("shown\n")), BarVisibility::Shown);
        assert_eq!(BarVisibility::from_state(Some("hidden")), BarVisibility::Hidden);
        // Garbage in the file is no choice at all.
        assert_eq!(BarVisibility::from_state(Some("maybe")), BarVisibility::DEFAULT);
        for choice in [BarVisibility::Hidden, BarVisibility::Shown] {
            assert_eq!(BarVisibility::from_state(Some(choice.id())), choice, "round-trips");
            assert_eq!(choice.toggled().toggled(), choice);
            assert_ne!(choice.toggled(), choice);
        }
    }

    #[test]
    fn paths_root_where_the_menu_roots() {
        let paths = ShellPaths::from_env(Some("/usr/share/omarchy".into()), Some("/home/u".into()));
        assert_eq!(paths.shell_qml, PathBuf::from("/usr/share/omarchy/shell/shell.qml"));
        assert_eq!(paths.launcher, PathBuf::from("/usr/share/omarchy/bin/omarchy-launch-shell"));
        // No OMARCHY_PATH (or an empty one): the pre-package location.
        for unset in [None, Some(OsString::new())] {
            let paths = ShellPaths::from_env(unset, Some("/home/u".into()));
            assert_eq!(paths.shell_qml, PathBuf::from("/home/u/.local/share/omarchy/shell/shell.qml"));
        }
    }

    #[test]
    fn the_bar_choice_round_trips_through_its_state_file() {
        let dir = std::env::temp_dir().join(format!("chonk-omarchy-bar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Two directories deep and absent: persisting creates the path.
        let path = dir.join("chonkstep/omarchy-bar");
        assert_eq!(BarVisibility::load_from(&path), BarVisibility::DEFAULT, "no file, the default");
        BarVisibility::Shown.persist_to(&path).unwrap();
        assert_eq!(BarVisibility::load_from(&path), BarVisibility::Shown);
        BarVisibility::Hidden.persist_to(&path).unwrap();
        assert_eq!(BarVisibility::load_from(&path), BarVisibility::Hidden);
        std::fs::write(&path, "nonsense").unwrap();
        assert_eq!(BarVisibility::load_from(&path), BarVisibility::DEFAULT, "a corrupt file is no choice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_means_both_files_and_a_symlinked_launcher_counts() {
        let dir = std::env::temp_dir().join(format!("chonk-omarchy-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shell")).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let paths = ShellPaths::under(&dir);
        assert!(!paths.installed(), "an empty root has no shell");

        std::fs::write(&paths.shell_qml, "// shell").unwrap();
        assert!(!paths.installed(), "the QML alone is not a shell that can be started");

        // Packaged Omarchy puts the launcher in /usr/bin and links it
        // from the root's `bin/`; the test for a file must follow that.
        let real = dir.join("real-launcher");
        std::fs::write(&real, "#!/bin/bash\n").unwrap();
        std::os::unix::fs::symlink(&real, &paths.launcher).unwrap();
        assert!(paths.installed());

        // A dangling link — Omarchy uninstalled under a stale root —
        // is not a launcher.
        std::fs::remove_file(&real).unwrap();
        assert!(!paths.installed());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
