//! Reading the user's *live* Hyprland configuration, so that
//! configuring an Omarchy machine keeps working when chonkstep is the
//! window manager.
//!
//! # Why this exists
//!
//! An Omarchy user configures their desktop through `~/.config/hypr/`
//! and `~/.config/omarchy/` — and, crucially, so does Omarchy's own
//! menu. Changing a keybinding, adding a startup app, or picking a
//! monitor layout from their UI *edits Hyprland config*. Chonkstep's
//! answer up to now was [`crate::preset`]: Omarchy's bindings read
//! once, by hand, at development time, and frozen into a table.
//!
//! That table is good work and it is still the fallback, but it has
//! two failures built into it. A user who changes anything through
//! Omarchy's menu sees no effect under us — the menu writes the file,
//! the file is never read, and the machine appears broken. And the
//! table rots: every Omarchy release that moves a chord leaves it
//! silently wrong, with nothing to notice.
//!
//! Reading the configuration live fixes both, and it is what makes the
//! swap invisible: the user's existing setup transfers wholesale, and
//! Omarchy's menu keeps being how you configure your machine.
//!
//! # What is actually on a machine
//!
//! Omarchy 4 ("quattro") configures Hyprland in **Lua**. The shipped
//! defaults are `/usr/share/omarchy/default/hypr/**.lua` — real Lua
//! with loops, conditionals and helper functions — and the user's own
//! files are `~/.config/hypr/*.lua`. Omarchy 3 and everything the
//! upstream Hyprland wiki documents use the classic
//! `keyword = value` **conf** syntax instead.
//!
//! Both are read ([`lua`], [`conf`]), because both are in front of real
//! users, and because a machine mid-upgrade has both trees on disk at
//! once. The development machine for this work was exactly that: the
//! shipped defaults were Lua, the user's personal bindings were still
//! in a `.conf` whose `.lua` replacement the migration had generated
//! empty, and a `~/.local/share/omarchy` symlink pointed at a
//! compatibility shim that a post-boot hook was scheduled to delete. A
//! reader that had assumed either syntax would have been wrong about
//! that machine.
//!
//! Which one is *live* is decided the way Hyprland decides it: the Lua
//! entry point wins where it exists, and the conf entry point is the
//! fallback (see [`Roots::entry`]).
//!
//! # Precedence
//!
//! Three layers, outermost last:
//!
//! 1. **Omarchy's shipped defaults**, `$OMARCHY_PATH/default/hypr/**`.
//! 2. **The user's own Hyprland files**, `~/.config/hypr/**`, which is
//!    where Omarchy's menu writes and where the user's overrides live.
//!    Read after the defaults, in the order the entry file includes
//!    them, so an `unbind` followed by a `bind` does what it says.
//! 3. **Chonkstep's own `config.toml`**, which wins over everything
//!    above it.
//!
//! Layer 3 is not a new rule — it is the rule this format already has.
//! [`crate::preset::base`] applies presets to the defaults *before*
//! the file's own keys are walked, so every value a preset sets is
//! overridden by writing that key out. This read slots into exactly
//! that position: applied over the baked
//! [`crate::preset::OMARCHY_BINDINGS`] table and under everything the
//! user's `config.toml` says. `[keybindings]` in `config.toml` still
//! has the last word on any chord, and `"none"` still unbinds one.
//!
//! # Never a broken session
//!
//! This reads somebody else's file, at session startup, on a machine
//! whose owner may never have heard of chonkstep. The rule is
//! absolute: **a malformed file, an unknown directive or a wild value
//! is a logged warning and a skipped line — never a panic, and never a
//! refusal to start.** Everything here is total. There is no `unwrap`
//! on parsed content, recursion is depth-bounded, loops are
//! iteration-bounded, the file graph is cycle-checked and
//! budget-limited, and patterns are compiled with a size cap. The
//! hostile-input tests exist to keep that true.
//!
//! And nothing here ever *runs* anything out of a config file. The Lua
//! reader parses and evaluates a closed subset of expressions; the two
//! conditions Omarchy branches on are answered by asking the file
//! system, not by executing a command. A configuration file must not
//! be a code-execution path into the window manager.
//!
//! # What is deliberately not read
//!
//! - **Gaps, borders, rounding, blur, shadows, animations, layouts.**
//!   These are Hyprland's look, and this desktop has its own — a
//!   theme, a titlebar, a decoration policy. Following them would mean
//!   drawing a NeXTSTEP frame with Hyprland's border colour on it.
//! - **Layer rules.** They configure Hyprland's layer-shell
//!   implementation; this compositor has its own.
//! - **Unsupported input settings.** Keyboard xkb/repeat values are
//!   carried. Whole-desktop behavior such as `follow_mouse`, touchpad
//!   policy, and gestures belongs to chonkstep and is named and skipped.
//! - **Unsupported `monitor =` lines.** Preferred mode, position and
//!   scale are applied once outputs exist. Disable, mirror, transform,
//!   extras and explicit modes refuse their whole line.
//! - **Anything that commands Hyprland.** `hyprctl` and the
//!   `omarchy-hyprland-*` scripts talk to a compositor that is not
//!   running; those bindings stay unbound, the same filter
//!   `chonk_shell::omarchy_menu` applies to menu rows.
//!
//! Every one of these is logged when it is met, not silently dropped.

pub mod conf;
pub mod directive;
pub mod dispatch;
pub mod keys;
pub mod lua;
pub mod rules;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use directive::{Directive, Include};
use wm_core::KeyCombo;

use crate::preset::Unbound;
use crate::Action;

/// The most files one read will open. Omarchy's own tree is about
/// forty; a graph asking for a thousand is a loop this reader failed
/// to notice, or a machine it should not be reading anyway.
const MAX_FILES: usize = 256;

/// The most bytes one file may contribute. Omarchy's largest is 8 KiB.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// The most bytes a whole read may consume.
const MAX_TOTAL_BYTES: u64 = 8 << 20;

/// How deep the include graph may go.
const MAX_INCLUDE_DEPTH: u32 = 16;

/// Where a read looks, and what it knows about the machine.
///
/// A struct rather than a set of environment lookups scattered through
/// the module, for the reason `chonk_shell::startup` gives about its
/// own resolvers: the policy stays unit-testable without touching the
/// process environment, which parallel tests cannot do safely. Every
/// test in this module points a `Roots` at a scratch tree.
#[derive(Clone, Debug)]
pub struct Roots {
    /// `~/.config/hypr`.
    pub user: PathBuf,
    /// `$OMARCHY_PATH/default/hypr`, or wherever Omarchy's defaults are.
    pub defaults: PathBuf,
    /// The roots Lua module names resolve against, in search order —
    /// Omarchy's `bootstrap.lua` sets `~/.local/state`, `~/.config`,
    /// then `$OMARCHY_PATH`.
    pub module_path: Vec<PathBuf>,
    /// What the branch conditions in Omarchy's Lua are answered from.
    pub facts: lua::Facts,
}

impl Roots {
    /// The roots of this machine, or `None` when there is no `$HOME`
    /// to find a config under.
    pub fn of_this_machine() -> Option<Self> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty() && Path::new(v).is_absolute())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let state = std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty() && Path::new(v).is_absolute())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        let omarchy = std::env::var_os("OMARCHY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/share/omarchy"));
        Some(Self {
            user: config.join("hypr"),
            defaults: omarchy.join("default/hypr"),
            module_path: vec![state, config, omarchy],
            facts: lua::Facts::of_this_machine(),
        })
    }

    /// Every root under one directory, for a test or a nested session
    /// pointed at a scratch copy of a config tree.
    ///
    /// `root` *is* the home directory, so the layout is a real
    /// machine's exactly: `<root>/.config/hypr` is the user's,
    /// `<root>/omarchy` stands in for `$OMARCHY_PATH`, and the module
    /// search path is the three directories `bootstrap.lua` puts on it.
    ///
    /// Modelling a home rather than inventing a tidier layout is what
    /// lets a fixture hold a user's `hyprland.conf` **verbatim** — its
    /// `source = ~/.config/hypr/bindings.conf` lines resolve, and so
    /// does the `~/.local/share/omarchy` symlink an upgrade leaves
    /// behind. A fixture that had to be edited to be readable would no
    /// longer be evidence about the real file.
    pub fn under(root: &Path) -> Self {
        Self {
            user: root.join(".config/hypr"),
            defaults: root.join("omarchy/default/hypr"),
            module_path: vec![
                root.join(".local/state"),
                root.join(".config"),
                root.join("omarchy"),
            ],
            facts: lua::Facts {
                path: Vec::new(),
                home: Some(root.to_path_buf()),
                state_home: Some(root.join(".local/state")),
            },
        }
    }

    /// The entry file, and which syntax it is in.
    ///
    /// Hyprland's own rule, and the one that matters on a machine
    /// mid-upgrade: the Lua entry point wins where it exists. On this
    /// development machine both `hyprland.lua` and `hyprland.conf`
    /// were present and only the first was live — the second was a
    /// migration leftover pointing at a compatibility shim, and a
    /// reader that preferred it would have read a configuration
    /// Hyprland itself no longer used.
    pub fn entry(&self) -> Option<(PathBuf, Syntax)> {
        let lua = self.user.join("hyprland.lua");
        if lua.is_file() {
            return Some((lua, Syntax::Lua));
        }
        let conf = self.user.join("hyprland.conf");
        if conf.is_file() {
            return Some((conf, Syntax::Conf));
        }
        None
    }
}

/// Which of Hyprland's two configuration syntaxes a file is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    Lua,
    Conf,
}

impl Syntax {
    fn of(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some("lua") => Self::Lua,
            _ => Self::Conf,
        }
    }
}

/// Everything one read produced.
#[derive(Debug, Default)]
pub struct Reading {
    /// Chords that mapped onto a chonkstep action, in file order with
    /// later entries winning — Hyprland's own rule for a chord bound
    /// twice.
    pub keybindings: Vec<(KeyCombo, Action)>,
    /// Every accepted binding including release/locked/repeat behavior
    /// and its human description.
    pub bindings: Vec<crate::Binding>,
    pub layer_bindings: BTreeMap<String, Vec<crate::Binding>>,
    /// The argv every [`Action::Run`] above names, keyed by the name
    /// [`dispatch::command_name`] derived from the argv.
    pub commands: BTreeMap<String, Vec<String>>,
    /// `env` lines, in order, later winning.
    pub env: Vec<(String, String)>,
    /// `exec-once` lines that survived the filter, as argv.
    pub autostart: Vec<Vec<String>>,
    /// The float rules, ready to install.
    pub float_rules: rules::FloatRules,
    /// `monitor =` lines, parsed and reported. See [`Monitors`].
    pub monitors: Monitors,
    pub input: crate::InputConfig,
    /// Every file actually read, in order. The [`Watch`]'s signature is
    /// taken over exactly this list.
    pub files: Vec<PathBuf>,
    /// What was skipped, and why. Logged by [`Reading::report`] and
    /// carried so the docs and the tests can name a specific skip.
    pub skipped: Vec<Skipped>,
}

/// One thing this read declined to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// A short category: `bind`, `window-rule`, `env`, `lua`, `syntax`.
    pub kind: String,
    /// What was skipped, in the file's own words where possible.
    pub what: String,
    /// Why.
    pub why: String,
}

/// The `monitor =` lines a configuration carries.
///
/// This parser deliberately keeps the requests unevaluated: preferred
/// mode, automatic DPI scale, and automatic placement can only be
/// resolved after real outputs exist. `wm-wayland` consumes the list at
/// output bootstrap, gives exact-name rules precedence over catch-all
/// rules, and applies preferred mode, position, and scale as one
/// transaction. A line containing disable, mirror, transform, another
/// extra field, or an explicit mode is refused whole and logged.
#[derive(Clone, Debug, Default)]
pub struct Monitors {
    pub lines: Vec<directive::Monitor>,
}

impl Reading {
    /// Whether anything usable came out of the read. A configuration
    /// that produced nothing at all is treated as "nothing to read" by
    /// the caller, which keeps the baked preset rather than replacing a
    /// working keymap with an empty one.
    pub fn is_empty(&self) -> bool {
        self.keybindings.is_empty()
            && self.env.is_empty()
            && self.autostart.is_empty()
            && self.float_rules.is_empty()
    }

    /// Logs the read: one summary line, and one line per thing
    /// skipped.
    ///
    /// Per thing, not per category. The rule for this whole module is
    /// that an unsupported directive is ignored *loudly*, and a count
    /// is not loud — "47 rules ignored" tells a user nothing they can
    /// act on, where "float rule carries match:xwayland 1, which this
    /// reader does not implement" tells them exactly which line to
    /// rewrite. The lines are `debug` rather than `warn` because a
    /// normal Omarchy machine produces around sixty of them and none is
    /// a problem; the summary saying how many there were is `info`.
    pub fn report(&self) {
        tracing::info!(
            files = self.files.len(),
            bindings = self.keybindings.len(),
            commands = self.commands.len(),
            env = self.env.len(),
            autostart = self.autostart.len(),
            float_rules = self.float_rules.len(),
            monitors = self.monitors.lines.len(),
            skipped = self.skipped.len(),
            "hyprland-config: read the desktop's live Hyprland configuration"
        );
        for skip in &self.skipped {
            tracing::debug!(kind = %skip.kind, what = %skip.what, why = %skip.why, "hyprland-config: not carried over");
        }
        for line in &self.monitors.lines {
            tracing::info!(
                output = %if line.output.is_empty() { "<any>" } else { line.output.as_str() },
                mode = %line.mode,
                position = %line.position,
                scale = %line.scale,
                "hyprland-config: monitor line read but not applied; use wlr-randr or kanshi (see docs/hyprland-config.md)"
            );
        }
    }
}

/// Reads the machine's live Hyprland configuration, or `None` when
/// there is none to read.
///
/// Never fails and never panics: every layer below returns whatever it
/// managed to read, so the worst outcome of a broken configuration is
/// an empty [`Reading`] and a log full of reasons.
pub fn load() -> Option<Reading> {
    let roots = Roots::of_this_machine()?;
    let reading = read(&roots);
    if reading.is_empty() {
        tracing::info!(
            user = %roots.user.display(),
            defaults = %roots.defaults.display(),
            "hyprland-config: nothing usable found; keeping chonkstep's own bindings"
        );
        return None;
    }
    reading.report();
    Some(reading)
}

/// Reads a configuration from explicit roots. The testable half of
/// [`load`].
pub fn read(roots: &Roots) -> Reading {
    let mut loader = Loader::new(roots);
    let mut stream = Vec::new();
    if let Some((entry, syntax)) = roots.entry() {
        loader.file(&entry, syntax, &mut stream, 0);
    } else if roots.defaults.join("bindings").is_dir() {
        // No entry file, but Omarchy's defaults are on disk: a user who
        // deleted their `~/.config/hypr` still has a desktop's worth of
        // bindings shipped in `/usr/share`, and reading them is better
        // than reading nothing. The defaults are what the entry file
        // would have required anyway.
        loader.directory(&roots.defaults.join("bindings"), &mut stream, 0);
    }
    lower(stream, loader.finish())
}

// ---- activation and layering ------------------------------------------

/// Whether this session reads the machine's live Hyprland
/// configuration.
///
/// # Why it is not simply "whenever the files exist"
///
/// The files exist on a great many machines that are not asking for
/// this. Hyprland is a popular compositor; a `~/.config/hypr` is left
/// behind by trying it for an afternoon, and Omarchy's defaults sit in
/// `/usr/share` on any machine with the package installed. Reading
/// them automatically would mean that installing a package, or
/// uninstalling a compositor badly, silently replaced a chonkstep
/// user's entire keymap with somebody else's — from a file they have
/// no reason to think anything is still reading. That is the worst
/// kind of surprise: invisible, total, and traced back to the wrong
/// change.
///
/// # Why it is not a new opt-in switch either
///
/// The user who wants this has already said so. `desktop = "omarchy"`
/// means "chonkstep is the window manager for my Omarchy desktop", and
/// it *already* replaces the keymap wholesale with
/// [`crate::preset::OMARCHY_BINDINGS`] — a hand-transcription of the
/// very files this reads. Someone who has accepted a frozen copy of
/// their bindings is not being surprised by the live original; they
/// are getting the thing the frozen copy was standing in for. Making
/// them write a second line to get it would be asking them to opt in
/// twice to one decision.
///
/// So: **the posture decides, and the key overrides the posture.**
/// `keymap = "omarchy"` alone is enough — a chonkstep desk that wants
/// Hyprland chords wants the user's own Hyprland chords — and
/// `hyprland_config = false` turns it off from either posture, which
/// is the escape hatch for "read my file, I will tell you when".
///
/// # And what happens to the baked preset
///
/// It becomes the **fallback**, not a second source of truth. When
/// this returns `true` and a configuration is found, the read replaces
/// the preset's binding table outright — the same replace-don't-merge
/// rule the preset itself applies, for the same reason: a desk
/// answering to both a live binding and a frozen one for the same
/// chord has two answers and no documented winner. When no
/// configuration is found, or nothing usable comes out of it, the
/// preset stands exactly as it did. That is what it is for: the
/// machine where Omarchy is not installed, or is installed in a shape
/// this reader cannot follow.
pub fn wanted(config: &crate::Config) -> bool {
    config.hyprland_config.unwrap_or(
        config.desktop == crate::preset::Desktop::Omarchy
            || config.keymap == crate::preset::Keymap::Omarchy,
    )
}

/// Applies a reading to a config, in the position the module docs
/// describe: over the preset, under the file's own keys.
///
/// Takes `Option` so the "nothing to read" path is one call rather
/// than a condition at every call site — and so a caller cannot forget
/// that nothing-to-read means *keep the preset*.
pub fn apply(config: &mut crate::Config, reading: Option<&Reading>) {
    let Some(reading) = reading else { return };
    if reading.keybindings.is_empty() {
        // Bindings are the load-bearing half. A read that found files
        // but no bindings in them is a read that went wrong somewhere
        // this module did not notice, and replacing a working keymap
        // with an empty one would leave a user with no way to open a
        // terminal and fix it. The rest of the read still applies.
        tracing::warn!(
            "hyprland-config: no bindings came out of the read; keeping the built-in keymap"
        );
    } else {
        config.keybindings = reading.keybindings.clone();
    }
    config.bindings = reading.bindings.clone();
    config.layer_bindings = reading.layer_bindings.clone();
    // Commands are *inserted*, so a `[commands]` entry of the same name
    // read later from the user's own file replaces this one — the same
    // rule `preset::apply_keymap` applies to its own declarations.
    for (name, argv) in &reading.commands {
        config.commands.insert(name.clone(), argv.clone());
    }
    config.session_env = reading.env.clone();
    config.input = reading.input.clone();
    config.monitor_rules = reading.monitors.lines.clone();
    config.autostart = reading.autostart.clone();
    config.float_policy = reading.float_rules.clone().policy();
}

// ---- the file graph ---------------------------------------------------

struct Loader<'a> {
    roots: &'a Roots,
    globals: lua::Globals,
    vars: BTreeMap<String, String>,
    seen: std::collections::BTreeSet<PathBuf>,
    bytes: u64,
    files: Vec<PathBuf>,
    skipped: Vec<Skipped>,
}

impl<'a> Loader<'a> {
    fn new(roots: &'a Roots) -> Self {
        Self {
            roots,
            globals: lua::Globals::default(),
            vars: BTreeMap::new(),
            seen: std::collections::BTreeSet::new(),
            bytes: 0,
            files: Vec::new(),
            skipped: Vec::new(),
        }
    }

    fn finish(self) -> LoadReport {
        LoadReport {
            files: self.files,
            skipped: self.skipped,
        }
    }

    fn note(&mut self, kind: &str, what: impl Into<String>, why: impl Into<String>) {
        self.skipped.push(Skipped {
            kind: kind.to_string(),
            what: what.into(),
            why: why.into(),
        });
    }

    /// Reads one file into `out`, following its includes in place.
    fn file(&mut self, path: &Path, syntax: Syntax, out: &mut Vec<Directive>, depth: u32) {
        if depth > MAX_INCLUDE_DEPTH {
            self.note(
                "include",
                path.display().to_string(),
                "include graph nested too deeply",
            );
            return;
        }
        if self.files.len() >= MAX_FILES {
            self.note(
                "include",
                path.display().to_string(),
                format!("more than {MAX_FILES} files in one read"),
            );
            return;
        }
        // Canonicalized so two names for one file — a symlink into a
        // compatibility shim, `..` in a `source` line — cannot make
        // this read it twice or loop forever.
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !self.seen.insert(key) {
            return;
        }
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_BYTES {
            self.note(
                "include",
                path.display().to_string(),
                format!("larger than {MAX_FILE_BYTES} bytes"),
            );
            return;
        }
        if self.bytes.saturating_add(size) > MAX_TOTAL_BYTES {
            self.note(
                "include",
                path.display().to_string(),
                "read budget exhausted",
            );
            return;
        }
        let Ok(bytes) = std::fs::read(path) else {
            self.note("include", path.display().to_string(), "unreadable");
            return;
        };
        self.bytes = self.bytes.saturating_add(size);
        self.files.push(path.to_path_buf());
        // Lossy rather than strict: a config file with one bad byte in
        // a comment must not cost the user every binding after it.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut local = Vec::new();
        match syntax {
            Syntax::Lua => {
                let mut globals = std::mem::take(&mut self.globals);
                lua::read(&text, &self.roots.facts, &mut globals, &mut local);
                self.globals = globals;
            }
            Syntax::Conf => {
                let mut vars = std::mem::take(&mut self.vars);
                conf::read(&text, &mut vars, &mut local);
                self.vars = vars;
            }
        }
        // Splice includes in place, so ordering matches what Hyprland
        // itself would have done — which is what makes "the user's file
        // is read after the defaults it requires" true.
        for entry in local {
            match entry {
                Directive::Include(include) => self.include(&include, path, out, depth),
                other => out.push(other),
            }
        }
    }

    fn include(&mut self, include: &Include, from: &Path, out: &mut Vec<Directive>, depth: u32) {
        match include {
            Include::Path(spec) => {
                let expanded = self.expand(spec, from);
                if expanded.is_empty() {
                    self.note("include", spec.clone(), "no file matched");
                }
                for path in expanded {
                    let syntax = Syntax::of(&path);
                    self.file(&path, syntax, out, depth + 1);
                }
            }
            Include::Module { name, optional } => match self.resolve_module(name) {
                Some(path) => self.file(&path, Syntax::Lua, out, depth + 1),
                None if *optional => {}
                None => self.note(
                    "include",
                    format!("require(\"{name}\")"),
                    "no module of that name on the search path",
                ),
            },
            Include::ModuleDirectory { prefix } => match self.resolve_directory(prefix) {
                Some(dir) => self.directory(&dir, out, depth + 1),
                None => self.note(
                    "include",
                    format!("require_all.files(…, \"{prefix}\")"),
                    "no directory of that name on the search path",
                ),
            },
        }
    }

    /// Every `*.lua`/`*.conf` directly under a directory, sorted —
    /// Omarchy's own `require_all.files` sorts, and so does the shell
    /// glob its conf-syntax equivalent expands.
    fn directory(&mut self, dir: &Path, out: &mut Vec<Directive>, depth: u32) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            self.note("include", dir.display().to_string(), "unreadable directory");
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && matches!(
                        p.extension().and_then(|e| e.to_str()),
                        Some("lua") | Some("conf")
                    )
            })
            .collect();
        paths.sort();
        for path in paths {
            let syntax = Syntax::of(&path);
            self.file(&path, syntax, out, depth + 1);
        }
    }

    /// A `source =` path: `~` expanded, relative paths taken against
    /// the including file's directory, and a trailing glob expanded by
    /// listing the directory rather than by shelling out.
    fn expand(&self, spec: &str, from: &Path) -> Vec<PathBuf> {
        let spec = spec.trim();
        let path = if let Some(rest) = spec.strip_prefix("~/") {
            match &self.roots.facts.home {
                Some(home) => home.join(rest),
                None => return Vec::new(),
            }
        } else if spec.starts_with('/') {
            PathBuf::from(spec)
        } else {
            from.parent().unwrap_or(Path::new(".")).join(spec)
        };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return Vec::new();
        };
        if !name.contains('*') {
            return if path.is_file() {
                vec![path]
            } else {
                Vec::new()
            };
        }
        // One glob, in the last component, which is the only shape
        // Hyprland's own `source` lines and Omarchy's toggles directory
        // use. A pattern anywhere else is not expanded — deliberately,
        // because a config reader walking arbitrary globs across a file
        // system is a surprise nobody asked for.
        let Some((before, after)) = name.split_once('*') else {
            return Vec::new();
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut matched: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        n.len() >= before.len() + after.len()
                            && n.starts_with(before)
                            && n.ends_with(after)
                    })
            })
            .collect();
        matched.sort();
        matched
    }

    /// `default.hypr.omarchy` against the module search path, exactly
    /// as `bootstrap.lua` sets it up: `~/.local/state`, then
    /// `~/.config`, then `$OMARCHY_PATH`.
    fn resolve_module(&self, name: &str) -> Option<PathBuf> {
        let relative = module_relative(name)?;
        self.roots
            .module_path
            .iter()
            .map(|root| root.join(&relative).with_extension("lua"))
            .find(|p| p.is_file())
    }

    fn resolve_directory(&self, prefix: &str) -> Option<PathBuf> {
        let relative = module_relative(prefix)?;
        self.roots
            .module_path
            .iter()
            .map(|root| root.join(&relative))
            .find(|p| p.is_dir())
    }
}

/// A Lua module name as a relative path, refusing anything that could
/// climb out of a search root. A configuration naming `..` is not a
/// configuration this reader is going to open.
fn module_relative(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.len() > 200 {
        return None;
    }
    let mut relative = PathBuf::new();
    for part in name.split('.') {
        if part.is_empty() || part == ".." || part.contains('/') || part.contains('\\') {
            return None;
        }
        relative.push(part);
    }
    Some(relative)
}

struct LoadReport {
    files: Vec<PathBuf>,
    skipped: Vec<Skipped>,
}

// ---- lowering ---------------------------------------------------------

/// Turns the directive stream into a [`Reading`]: the one place a
/// Hyprland statement becomes a chonkstep setting.
fn lower(stream: Vec<Directive>, report: LoadReport) -> Reading {
    let mut reading = Reading {
        files: report.files,
        skipped: report.skipped,
        ..Default::default()
    };
    let mut window_rules = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    for entry in stream {
        match entry {
            Directive::Bind {
                keys,
                description,
                flags,
                dispatcher,
            } => bind(
                &mut reading,
                &keys,
                description.as_deref(),
                flags,
                &dispatcher,
            ),
            Directive::LayerBind {
                namespace,
                keys,
                description,
                flags,
                dispatcher,
            } => layer_bind(
                &mut reading,
                namespace,
                &keys,
                description.as_deref(),
                flags,
                &dispatcher,
            ),
            Directive::Unbind { keys } => match keys::spec_for(&keys) {
                Ok(spec) => {
                    if let Some(combo) = crate::parse_key(&spec) {
                        reading
                            .keybindings
                            .retain(|(existing, _)| *existing != combo);
                        reading.bindings.retain(|binding| binding.combo != combo);
                    }
                }
                Err(trouble) => reading.skipped.push(Skipped {
                    kind: "unbind".into(),
                    what: keys,
                    why: trouble.reason(),
                }),
            },
            Directive::Env { name, value } => {
                // Later wins, the way a config file's last word does.
                env.retain(|(existing, _)| *existing != name);
                env.push((name, value));
            }
            Directive::Input { name, value } => input(&mut reading, &name, &value),
            Directive::ExecOnce { command } => autostart(&mut reading, &command),
            Directive::WindowRule(rule) => window_rules.push(rule),
            Directive::Monitor(line) => reading.monitors.lines.push(line),
            Directive::Ignored { kind, detail } => reading.skipped.push(Skipped {
                kind: kind.to_string(),
                what: detail,
                why: "not carried over; see docs/hyprland-config.md".into(),
            }),
            // Spliced by the loader; unreachable here, and harmless.
            Directive::Include(_) => {}
        }
    }
    let (float_rules, notes) = rules::compile(&window_rules);
    reading.float_rules = float_rules;
    for note in notes {
        reading.skipped.push(Skipped {
            kind: "window-rule".into(),
            what: note,
            why: "see docs/hyprland-config.md".into(),
        });
    }
    reading.env = filter_env(env, &mut reading.skipped);
    reading
}

fn layer_bind(
    reading: &mut Reading,
    namespace: String,
    keys: &str,
    description: Option<&str>,
    flags: directive::BindFlags,
    dispatcher: &directive::Dispatcher,
) {
    let mut scoped = Reading::default();
    bind(&mut scoped, keys, description, flags, dispatcher);
    reading.commands.extend(scoped.commands);
    reading.skipped.extend(scoped.skipped);
    let Some(binding) = scoped.bindings.pop() else {
        return;
    };
    let bindings = reading.layer_bindings.entry(namespace).or_default();
    bindings
        .retain(|existing| existing.combo != binding.combo || existing.release != binding.release);
    bindings.push(binding);
}

/// One binding, through the three answers in [`dispatch`].
fn bind(
    reading: &mut Reading,
    keys: &str,
    description: Option<&str>,
    flags: directive::BindFlags,
    dispatcher: &directive::Dispatcher,
) {
    let what = match description {
        Some(text) => format!("{keys} ({text})"),
        None => keys.to_string(),
    };
    let spec = match keys::spec_for(keys) {
        Ok(spec) => spec,
        Err(trouble) => {
            reading.skipped.push(Skipped {
                kind: "bind".into(),
                what,
                why: trouble.reason(),
            });
            return;
        }
    };
    let Some(combo) = crate::parse_key(&spec) else {
        reading.skipped.push(Skipped {
            kind: "bind".into(),
            what,
            why: format!("{spec:?} is not a chord this desktop can grab"),
        });
        return;
    };
    let action = match dispatch::verb_for(dispatcher) {
        dispatch::Verb::Action(action) => action,
        dispatch::Verb::Run(argv) => {
            if argv.is_empty() {
                reading.skipped.push(Skipped {
                    kind: "bind".into(),
                    what,
                    why: "empty command".into(),
                });
                return;
            }
            let name = dispatch::command_name(&argv);
            reading.commands.insert(name.clone(), argv);
            Action::Run(name)
        }
        dispatch::Verb::Unbound(reason) => {
            reading.skipped.push(Skipped {
                kind: "bind".into(),
                what,
                why: reason.reason().to_string(),
            });
            return;
        }
    };
    // Press and release are independent namespaces. In particular the
    // F9 release half of push-to-talk must not replace its press half.
    reading
        .bindings
        .retain(|existing| existing.combo != combo || existing.release != flags.release);
    reading.bindings.push(crate::Binding {
        combo,
        action: action.clone(),
        description: description.map(str::to_string),
        locked: flags.locked,
        repeating: flags.repeating,
        release: flags.release,
    });
    if !flags.release {
        reading
            .keybindings
            .retain(|(existing, _)| *existing != combo);
        reading.keybindings.push((combo, action));
    }
}

fn input(reading: &mut Reading, name: &str, value: &str) {
    let value = value.trim().trim_matches(['\"', '\'']).to_string();
    match name.trim().to_ascii_lowercase().as_str() {
        "kb_rules" => reading.input.rules = Some(value),
        "kb_model" => reading.input.model = Some(value),
        "kb_layout" => reading.input.layout = Some(value),
        "kb_variant" => reading.input.variant = Some(value),
        "kb_options" => reading.input.options = Some(value),
        "repeat_rate" => match value.parse::<i32>() {
            Ok(rate) if (1..=1000).contains(&rate) => reading.input.repeat_rate = Some(rate),
            _ => reading.skipped.push(Skipped {
                kind: "input".into(),
                what: format!("repeat_rate = {value}"),
                why: "repeat rate must be an integer from 1 through 1000".into(),
            }),
        },
        "repeat_delay" => match value.parse::<i32>() {
            Ok(delay) if (1..=5000).contains(&delay) => reading.input.repeat_delay = Some(delay),
            _ => reading.skipped.push(Skipped {
                kind: "input".into(),
                what: format!("repeat_delay = {value}"),
                why: "repeat delay must be an integer from 1 through 5000 milliseconds".into(),
            }),
        },
        "follow_mouse" => reading.skipped.push(Skipped {
            kind: "input".into(),
            what: format!("follow_mouse = {value}"),
            why: "focus policy belongs to chonkstep; use focus_follows_mouse in config.toml".into(),
        }),
        other => reading.skipped.push(Skipped {
            kind: "input".into(),
            what: format!("{other} = {value}"),
            why: "input setting is not implemented".into(),
        }),
    }
}

/// One `exec-once` line, filtered.
///
/// Two filters, and both exist because starting the wrong thing here
/// is worse than starting nothing. A command that talks to Hyprland
/// cannot work; and Omarchy's own shell is already started by
/// `chonk_shell::omarchy_shell`, at the point in the session where
/// Hyprland's `autostart` would have started it, so taking it from
/// this list too would start a second copy of the bar.
fn autostart(reading: &mut Reading, command: &str) {
    let compact = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let blanket_systemd_import = compact.contains("systemctl --user import-environment")
        && (compact.contains("$(env") || compact.ends_with(" import-environment"));
    let blanket_dbus_import = compact.contains("dbus-update-activation-environment")
        && compact.split_whitespace().any(|word| word == "--all");
    if blanket_systemd_import || blanket_dbus_import {
        reading.skipped.push(Skipped {
            kind: "exec-once".into(),
            what: command.to_string(),
            why: "blanket activation-environment imports can leak credentials, build variables, or another session's display; chonkstep publishes a curated session set".into(),
        });
        return;
    }
    let argv = dispatch::split_command(command);
    let Some(program) = argv.first().cloned() else {
        return;
    };
    let base = program.rsplit('/').next().unwrap_or(&program).to_string();
    // `uwsm-app -- <thing>`: look past the wrapper, so the checks below
    // see the program Omarchy is actually starting.
    let effective = if base == "uwsm-app" {
        argv.iter()
            .find(|a| *a != "uwsm-app" && *a != "--")
            .cloned()
            .unwrap_or_else(|| base.clone())
    } else {
        base.clone()
    };
    let effective = effective
        .rsplit('/')
        .next()
        .unwrap_or(&effective)
        .to_string();
    if dispatch::commands_hyprland(&base) || dispatch::commands_hyprland(&effective) {
        reading.skipped.push(Skipped {
            kind: "exec-once".into(),
            what: command.to_string(),
            why: Unbound::HyprlandOnly.reason().to_string(),
        });
        return;
    }
    if ALREADY_OURS.contains(&effective.as_str()) {
        reading.skipped.push(Skipped {
            kind: "exec-once".into(),
            what: command.to_string(),
            why:
                "this desktop starts it itself; running it from here too would start a second copy"
                    .into(),
        });
        return;
    }
    if command.contains("&&")
        || command.contains("||")
        || command.contains('|')
        || command.contains('$')
        || command.contains(';')
    {
        reading.autostart.push(vec![
            "bash".into(),
            "-lc".into(),
            command.trim().to_string(),
        ]);
        return;
    }
    reading.autostart.push(argv);
}

/// Autostart entries this desktop already owns.
///
/// `omarchy-launch-shell` is the bar, the menus, the notifications and
/// the lock screen — `chonk_shell::omarchy_shell` starts exactly that,
/// with the theme and scale this session resolved, and
/// `docs/omarchy-mode.md` already says why it must not also be in an
/// autostart list.
const ALREADY_OURS: &[&str] = &["omarchy-launch-shell", "omarchy-shell"];

/// Environment variables, filtered.
///
/// Two of Omarchy's are load-bearing lies under this window manager:
/// `XDG_CURRENT_DESKTOP=Hyprland` and `XDG_SESSION_DESKTOP=Hyprland`
/// tell every portal, screen-sharing picker and toolkit that Hyprland
/// is running. Omarchy sets them deliberately, and under Omarchy they
/// are true. Under chonkstep they are not, and carrying them would
/// route xdg-desktop-portal at `xdg-desktop-portal-hyprland`, which
/// would then try to talk to a compositor that is not there — the same
/// class of failure as a binding that runs `hyprctl`, in the one place
/// where it would break screen sharing rather than one key.
///
/// The session's own display variables are refused for the blunter
/// reason: they name *this* session, the compositor that owns it
/// already sets them correctly, and a stale value out of a config file
/// is how a desktop points its children at a display that does not
/// exist.
fn filter_env(env: Vec<(String, String)>, skipped: &mut Vec<Skipped>) -> Vec<(String, String)> {
    env.into_iter()
        .filter(|(name, value)| {
            let refused = match name.as_str() {
                "XDG_CURRENT_DESKTOP" | "XDG_SESSION_DESKTOP" => {
                    Some("names Hyprland as the running desktop, which under chonkstep it is not")
                }
                "WAYLAND_DISPLAY"
                | "DISPLAY"
                | "XDG_SESSION_TYPE"
                | "XDG_RUNTIME_DIR"
                | "HYPRLAND_INSTANCE_SIGNATURE" => Some("names this session, which the compositor sets for itself"),
                "GDK_SCALE" | "GDK_DPI_SCALE" | "QT_SCALE_FACTOR" | "ELM_SCALE" => Some(
                    "toolkit-wide scale would double-apply or contradict the compositor's per-output monitor scale",
                ),
                _ => None,
            };
            match refused {
                Some(why) => {
                    skipped.push(Skipped { kind: "env".into(), what: format!("{name}={value}"), why: why.into() });
                    false
                }
                None => true,
            }
        })
        .collect()
}

// ---- the watch --------------------------------------------------------

/// Noticing that the user changed their Hyprland configuration, so the
/// session follows it without a restart.
///
/// This is the whole point of the module. Omarchy's menu writes these
/// files; if a rebind through their UI needed a logout to take effect,
/// "your menu still configures your machine" would be a half-truth.
///
/// Polled at one hertz, like `chonk_shell::omarchy_follow`, and for the
/// same two reasons that module gives. The general one: two `stat`
/// calls a second on paths that are usually there is nothing, and a
/// change landing within a second of a menu click is indistinguishable
/// from instant. The specific one is stronger here — these files are
/// *replaced*, not modified. `omarchy-menu` writes a temporary file and
/// renames it over the original, and Omarchy's upgrades move whole
/// trees; an inotify watch on a path that is unlinked and recreated has
/// to be re-armed by exactly the kind of code that goes wrong at
/// 3 a.m., where a signature comparison simply sees a different inode.
///
/// The signature covers every file the last read actually opened, plus
/// the modification time of the directories those files live in — so a
/// *new* file appearing (a user adding `~/.config/hypr/mine.lua`, an
/// upgrade dropping a new default in) is noticed too, which a per-file
/// signature alone would miss until something else changed.
#[derive(Debug)]
pub struct Watch {
    cadence: std::time::Duration,
    last_checked: Option<std::time::Instant>,
    seen: Option<Signature>,
    watched: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

/// What identifies one watched file: its modification time, size and
/// inode — `None` when the file is not there, which is itself a change
/// worth seeing. The inode is in there because these files are written
/// by `rename`-over, which can leave the timestamp alone.
type FileIdentity = Option<(std::time::SystemTime, u64, u64)>;

/// The identity of a set of files: each one's (mtime, size, inode), and
/// each watched directory's mtime. `None` for a file that is not there,
/// which is itself a change worth seeing.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    files: Vec<(PathBuf, FileIdentity)>,
    directories: Vec<(PathBuf, Option<std::time::SystemTime>)>,
}

impl Watch {
    /// A watch over the files a [`Reading`] opened, and the roots they
    /// came from.
    pub fn new(roots: &Roots, reading: &Reading) -> Self {
        Self {
            cadence: std::time::Duration::from_secs(1),
            last_checked: None,
            seen: None,
            watched: reading.files.clone(),
            directories: vec![
                roots.user.clone(),
                roots.defaults.clone(),
                roots.defaults.join("bindings"),
                roots.defaults.join("apps"),
            ],
        }
    }

    /// Whether the configuration changed since the last time this
    /// returned `true` — or since the watch was created, for its first
    /// look.
    ///
    /// The first call baselines and returns `false`: the session just
    /// resolved from the very files being watched, so what is there now
    /// is what it is already wearing. Same contract, and the same
    /// reasoning, as `omarchy_follow::Watch::changed`.
    pub fn changed(&mut self, now: std::time::Instant) -> bool {
        if self
            .last_checked
            .is_some_and(|last| now.duration_since(last) < self.cadence)
        {
            return false;
        }
        self.last_checked = Some(now);
        let signature = self.signature();
        let changed = self.seen.as_ref().is_some_and(|seen| *seen != signature);
        self.seen = Some(signature);
        changed
    }

    /// Re-points the watch at the files a fresh read opened, and
    /// re-baselines.
    ///
    /// Called after a re-read, because the set of files is itself part
    /// of what a config edit can change: a user adding a `source =`
    /// line brings a file into the set that was never watched before,
    /// and a user deleting one takes it out.
    pub fn follow(&mut self, reading: &Reading) {
        self.watched = reading.files.clone();
        self.seen = Some(self.signature());
    }

    fn signature(&self) -> Signature {
        use std::os::unix::fs::MetadataExt;
        Signature {
            files: self
                .watched
                .iter()
                .map(|path| {
                    let identity = std::fs::metadata(path)
                        .ok()
                        .and_then(|m| Some((m.modified().ok()?, m.len(), m.ino())));
                    (path.clone(), identity)
                })
                .collect(),
            directories: self
                .directories
                .iter()
                .map(|path| {
                    (
                        path.clone(),
                        std::fs::metadata(path).and_then(|m| m.modified()).ok(),
                    )
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests;
