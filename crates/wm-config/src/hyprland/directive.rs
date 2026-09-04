//! The one shape both front ends produce, and the only shape the
//! lowering in [`super`] reads.
//!
//! Omarchy ships its Hyprland configuration in two syntaxes across the
//! versions a machine may be sitting on — Lua on 4.x, classic
//! `hyprland.conf` on 3.x and on any hand-written upstream setup — and
//! this desktop has to read whichever one is in front of it. Two
//! recognisers that each mapped straight onto chonkstep's actions would
//! be two places to make the same judgement, and the judgements are the
//! valuable part: which chords are tiling-only, which dispatchers have
//! no verb here, which window properties matter. So the recognisers do
//! one job each — turn text into [`Directive`]s — and every judgement
//! is made once, downstream, against this type.
//!
//! Nothing here is chonkstep's vocabulary. A [`Directive`] is still
//! *Hyprland's* statement, tokenized: the translation happens later.
//! That is deliberate, because it is what lets a directive this
//! desktop cannot honour still be *reported* precisely — "`SUPER + J`
//! runs `layoutmsg togglesplit`, which is tiling-only" is a useful
//! sentence, and it needs the original words to say.

/// One statement read out of a Hyprland configuration file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Directive {
    /// `bind`/`bindd`/`bindl`/`binde`… in conf, `o.bind`/`hl.bind` in
    /// Lua. `keys` is Hyprland's own spelling of the chord, untouched.
    Bind {
        keys: String,
        /// The human label `bindd` and `o.bind`'s second argument
        /// carry. `None` for the undescribed forms.
        description: Option<String>,
        flags: BindFlags,
        dispatcher: Dispatcher,
    },
    /// A binding owned by the lifetime of a layer-shell namespace.
    /// Unlike `Bind`, this is never installed in the global keymap.
    LayerBind {
        namespace: String,
        keys: String,
        description: Option<String>,
        flags: BindFlags,
        dispatcher: Dispatcher,
    },
    /// `unbind = SUPER, SPACE` / `hl.unbind("SUPER + SPACE")`. Applied
    /// in file order, so a user file that unbinds a default and then
    /// binds the chord to something else gets both halves.
    Unbind { keys: String },
    /// `env = NAME,VALUE` / `hl.env("NAME", "VALUE")`.
    Env { name: String, value: String },
    /// One supported key from Hyprland's `input {}` table.
    Input { name: String, value: String },
    /// `exec-once = cmd` / `hl.exec_cmd(cmd)` inside an
    /// `hl.on("hyprland.start", …)` block / `o.launch_on_start(cmd)`.
    ExecOnce { command: String },
    /// `windowrule`/`windowrulev2` / `o.window(match, rules)` /
    /// `hl.window_rule(rules)`.
    WindowRule(WindowRule),
    /// `monitor = …` / `hl.monitor({ … })`.
    Monitor(Monitor),
    /// Another file to read at exactly this point in the stream.
    ///
    /// Carried as a directive rather than resolved by the front ends
    /// because the front ends do no I/O — they turn text into
    /// directives and nothing else — and because *where* an include
    /// sits matters: a user file that unbinds a default and rebinds the
    /// chord only works if the default was read first. The loader in
    /// [`super`] walks the stream in order and splices each include in
    /// place.
    Include(Include),
    /// Something recognisable as a directive that this reader will not
    /// act on, kept so it can be logged with its own words rather than
    /// dropped into silence. The brief's rule: ignore loudly.
    Ignored { kind: &'static str, detail: String },
}

/// Behavioral suffix/options carried by a Hyprland binding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BindFlags {
    pub locked: bool,
    pub repeating: bool,
    pub release: bool,
}

/// What a binding does, normalized across the two syntaxes.
///
/// Hyprland's conf spells a dispatcher as a name and a single argument
/// string (`workspace, 3`); Lua spells it as a call with a table
/// (`hl.dsp.focus({ workspace = "3" })`). They are the same statement,
/// so the Lua front end lowers its call forms onto this shape and the
/// judgement about what a dispatcher *means* is written once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dispatcher {
    /// Run a command line through a shell. Both `exec` and the
    /// `o.bind` helper forms (`{ omarchy = "browser" }`,
    /// `{ webapp = … }`, `{ tui = … }`, `o.bind_toggle`) end here,
    /// already expanded exactly as `helpers.lua` expands them.
    Exec(String),
    /// A compositor verb: the dispatcher name and its argument, with
    /// the argument's own internal commas left alone (`fullscreenstate`
    /// takes `0 2`).
    Verb { name: String, arg: String },
    /// A Lua dispatcher given as a `function() … end` closure, or any
    /// other value this reader cannot evaluate without being a Lua
    /// interpreter. Carried rather than dropped so the binding can be
    /// reported as "seen, not understood" instead of vanishing.
    Opaque(String),
}

/// A window rule: what it matches, and what it asserts.
///
/// Properties are kept as `(name, value)` string pairs rather than a
/// closed enum because Hyprland's property vocabulary is long, mostly
/// irrelevant here, and still growing. Supported geometry, focus,
/// idle, pin and initial-state properties are picked out downstream;
/// every other property rides along so it can earn an individual,
/// matcher-qualified skip line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowRule {
    pub matchers: Vec<Matcher>,
    pub props: Vec<(String, String)>,
}

/// One `match:` clause of a window rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Matcher {
    /// `match:class X` / `class:X` / `o.window("X", …)`. The value is a
    /// regular expression in Hyprland and is treated as one here.
    Class(String),
    /// `match:title X` / `title:X`.
    Title(String),
    /// `match:tag X`. Resolved through the rules that *add* that tag —
    /// see `super::rules`.
    Tag(String),
    /// Any other matcher (`match:xwayland 1`, `match:float 1`,
    /// `match:workspace 5`, …). Kept whole so a rule carrying one can
    /// be refused as a unit: a rule this reader only half-understands
    /// must not be applied on the half it did.
    Other { key: String, value: String },
}

/// A `monitor =` line. The Wayland backend applies the supported
/// preferred-mode, position, and scale subset transactionally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// The output name, or empty for Hyprland's catch-all `monitor=,…`.
    pub output: String,
    pub mode: String,
    pub position: String,
    pub scale: String,
    /// Everything after the fourth field: `transform, 1`, `cm, srgb`,
    /// `bitdepth, 10`, and the `disable`/`mirror` forms.
    pub extra: Vec<String>,
}

/// A file, or set of files, a configuration asks for by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Include {
    /// `source = ~/.config/hypr/bindings.conf`, possibly a glob.
    Path(String),
    /// `require("default.hypr.omarchy")`: a Lua module name, resolved
    /// against the search path Omarchy's `bootstrap.lua` sets up.
    Module { name: String, optional: bool },
    /// `require_all.files(dir, "default.hypr.bindings")`: every `*.lua`
    /// directly under a directory, in sorted order — the fan-out
    /// Omarchy uses for its `bindings/` and `apps/` folders.
    ModuleDirectory { prefix: String },
}
