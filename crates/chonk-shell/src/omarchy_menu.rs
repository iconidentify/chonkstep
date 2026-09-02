//! Omarchy's own command menu, rendered as the `Omarchy` submenu of
//! chonkstep's root menu.
//!
//! # The data source
//!
//! Omarchy's whole menu is data, not code: `$OMARCHY_PATH/default/
//! omarchy/omarchy-menu.jsonc` (some 330 entries in Omarchy 4) overlaid
//! by the user's `~/.config/omarchy/extensions/omarchy-menu.jsonc`.
//! Entry ids are the object keys and the dotted id *is* the tree —
//! `trigger.capture.screenshot` sits under `trigger.capture` — so the
//! file carries no parent pointers and can define a child before its
//! parent (the shipped file does: `trigger.capture.screenrecord.stop`
//! precedes `trigger.capture.screenrecord`). Kind is inferred, exactly
//! as Omarchy's `MenuModel.js` infers it: an entry with `action` is a
//! command, one with `target` is a link to another submenu, anything
//! else is a submenu. The user file overlays the default per key — an
//! extension that reuses a shipped id replaces only the fields it
//! declares and keeps the row's original position; new ids append.
//!
//! This module reads those two files and turns them into `MenuItem`s,
//! so the entire Omarchy command surface appears under right-click in
//! chonkstep's own chrome, regenerates on every Omarchy upgrade, and
//! chonkstep maintains no list of its own. Omarchy's shell re-parses on
//! file change (`FileView { watchChanges: true }`); the house-consistent
//! equivalent here is polling the two files' mtimes once a second from
//! the shell tick — the same argument `startup::reload_requested` makes
//! for polling over inotify — plus a re-read on every config reload.
//!
//! # The condition model
//!
//! `when` (hide the row unless it holds), `checked` (append the
//! current-choice marker) and `disabled` (list the row dim and inert,
//! with the marker — the Install lists use it so software already on
//! the machine reads as installed rather than vanishing) are *bash
//! expressions*. Omarchy never evaluates them on the path that opens
//! the menu: every guard in the menu is batched into one `bash -lc`
//! subprocess per (re)load that prints `<id>:<w|c|d>:<0|1>` per line,
//! and the menu opens on the previous batch's answers. This module does
//! the same, with the shell's one hard rule layered on top: the batch
//! runs on a background thread, never the shell thread, and the menu is
//! built from the latest *completed* snapshot. Until the first snapshot
//! lands a row with a `when` is hidden and `checked`/`disabled` rows are
//! plain — hiding is the conservative reading of "unknown" (a row that
//! should not exist yet is a smaller lie than one that should not be
//! there at all). The batch is re-run after every reload and, on a
//! short debounce, after every Omarchy action fired from the menu, so a
//! toggle's marker catches up with what the toggle did. It is bounded
//! by a timeout that kills the whole process group; a killed batch is
//! discarded rather than half-applied, because a `when` that went
//! unanswered would otherwise flip a row's visibility on nothing.
//!
//! The batch prelude is a port of Omarchy's own (`guardHelpers` and the
//! `GUARD_READERS` substitution in `MenuModel.js`): package and command
//! presence are answered in-process from one `pacman -Q` snapshot
//! instead of a fork per row, and the handful of `$(omarchy-default-*)`
//! readers that many sibling rows compare against run once each. This
//! is not an optimization we chose; it is what makes the shipped menu's
//! guards finish in well under a second instead of several, and a batch
//! that took several would routinely lose to the timeout.
//!
//! # The exec model
//!
//! An action is run precisely the way `omarchy-shell` runs it —
//! `Quickshell.execDetached(["bash", "-lc", command])` in
//! `shell/Commons/Util.qml` — as `bash -lc <command>`, detached and
//! never waited on, through the same `spawn::spawn_detached_with_env`
//! every other launch in the shell goes through. The login shell (`-l`)
//! is load-bearing: it is what puts `$OMARCHY_PATH/bin` on the child's
//! `PATH`, and every shipped action is the bare name of a script there.
//!
//! # What is deliberately left out
//!
//! * **Provider-backed submenus** (`provider: "apps"`, `"fonts"`). Their
//!   rows come from Omarchy's shell at runtime, not from the file:
//!   `apps` is the launcher's desktop-entry library, which chonkstep's
//!   own Applications submenu already is, and `fonts` is a QML-side
//!   enumeration with its own action template. A provider submenu is
//!   dropped, and so is any static child it might have carried.
//! * **Links** (`target`). A link is a second route to a submenu that
//!   already appears elsewhere in the same tree — an alias for
//!   Omarchy's search and `summon` routes. A cascade menu has no use
//!   for a duplicate branch, and nothing shipped declares one.
//! * **Hyprland-only actions.** An action that invokes `hyprctl` or an
//!   `omarchy-hyprland-*` script commands a compositor that is not
//!   running; those rows are hidden by [`is_hyprland_only`]. Everything
//!   else stays, including "Learn → Hyprland" — a manual is not a
//!   compositor call.
//! * **Icons, `title`, `aliases`, `description`.** The Nerd Font glyph
//!   is dropped because the menu font need not carry it; `title` is a
//!   header for a menu whose header is its parent row here; the rest
//!   serve Omarchy's search, which this menu does not have.
//! * **The `{"items": {...}}` wrapper form** `MenuModel.js` also
//!   accepts. Nothing shipped or documented uses it, and accepting it
//!   would cost an extra parse of every file to find out.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use wm_theme::menu::MenuItem;

// ----------------------------------------------------------------- paths

/// Where the two menu definition files live for this user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuPaths {
    /// The shipped menu: `$OMARCHY_PATH/default/omarchy/omarchy-menu.jsonc`.
    pub default: PathBuf,
    /// The user's extension: `~/.config/omarchy/extensions/omarchy-menu.jsonc`.
    pub user: PathBuf,
}

impl MenuPaths {
    /// Resolves both paths from the process environment, or `None` if
    /// the shipped file does not exist — which is the whole test for
    /// "is Omarchy installed here": the submenu appears exactly when
    /// there is a menu to mirror.
    pub fn discover() -> Option<Self> {
        let paths = Self::from_env(
            std::env::var_os("OMARCHY_PATH"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        );
        paths.default.is_file().then_some(paths)
    }

    /// The pure half of [`Self::discover`]: `$OMARCHY_PATH` if set (the
    /// variable Omarchy's own shell reads), else the standard install
    /// location under `$HOME`. The user file honours `XDG_CONFIG_HOME`
    /// where Omarchy's shell hard-codes `$HOME/.config`: on a real
    /// Omarchy machine the two agree, and honouring it is what lets an
    /// isolated test session point the shell at its own extension
    /// file rather than the developer's.
    pub fn from_env(omarchy_path: Option<OsString>, xdg_config_home: Option<OsString>, home: Option<OsString>) -> Self {
        let home = home.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let omarchy = omarchy_path
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share/omarchy"));
        let config = xdg_config_home
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Self {
            default: omarchy.join("default/omarchy/omarchy-menu.jsonc"),
            user: config.join("omarchy/extensions/omarchy-menu.jsonc"),
        }
    }
}

// ----------------------------------------------------------------- JSONC

/// Strips `//` line comments, `/* */` block comments and trailing
/// commas from JSONC, leaving strict JSON.
///
/// String-aware, unlike Omarchy's own regex (which removes only
/// whole-line `//` comments and says so in `docs/menu.md`): a `//`
/// inside a quoted value — every `https://` URL in an `action`, and
/// any icon glyph that happens to encode as slashes — must survive, and
/// the file's own comments sit at the ends of lines as well as on lines
/// of their own. Newlines inside comments are kept so a parse error
/// still reports the right line.
pub fn strip_jsonc(raw: &str) -> String {
    let without_comments = strip_comments(raw);
    strip_trailing_commas(&without_comments)
}

fn strip_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                // An escape keeps the next character verbatim, so a
                // `\"` cannot end the string.
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                    }
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'\\' && i + 1 < bytes.len() {
                out.push_str(&text[i..i + 2]);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
        } else if c == b'"' {
            in_string = true;
        } else if c == b',' {
            // A comma whose next non-blank character closes the
            // container is the trailing comma JSON forbids.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }
        // Push whole UTF-8 sequences: the byte walk above only ever
        // stops on ASCII, so a multi-byte character is never split.
        let width = utf8_width(c);
        out.push_str(&text[i..i + width]);
        i += width;
    }
    out
}

fn utf8_width(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

// --------------------------------------------------------------- entries

/// One entry as written in the file, every field optional so the
/// per-key overlay of the user file can tell "not declared" from
/// "declared empty". Fields this module does not act on (`icon`,
/// `aliases`, `title`, `description`, `iconFont`) are not modelled.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RawEntry {
    #[serde(deserialize_with = "string_or_none")]
    pub label: Option<String>,
    #[serde(deserialize_with = "string_or_none")]
    pub action: Option<String>,
    #[serde(deserialize_with = "string_or_none")]
    pub target: Option<String>,
    #[serde(deserialize_with = "string_or_none")]
    pub provider: Option<String>,
    #[serde(deserialize_with = "string_or_none")]
    pub parent: Option<String>,
    #[serde(deserialize_with = "string_or_none")]
    pub when: Option<String>,
    #[serde(deserialize_with = "string_or_none")]
    pub checked: Option<String>,
    #[serde(deserialize_with = "string_or_none")]
    pub disabled: Option<String>,
}

/// A non-string where a string was expected reads as "not declared"
/// rather than failing the whole file: Omarchy's parser does
/// `value.when || ""`, so a stray boolean there costs one field, not
/// every entry in the file, and a user extension with one bad value
/// should degrade the same way here.
fn string_or_none<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(text) if !text.is_empty() => Some(text),
        _ => None,
    })
}

impl RawEntry {
    /// The per-key overlay: every field `other` declares replaces this
    /// one's, everything it leaves out is kept — so an extension can
    /// relabel a shipped row without re-declaring its action.
    fn overlay(&mut self, other: RawEntry) {
        let RawEntry { label, action, target, provider, parent, when, checked, disabled } = other;
        for (slot, value) in [
            (&mut self.label, label),
            (&mut self.action, action),
            (&mut self.target, target),
            (&mut self.provider, provider),
            (&mut self.parent, parent),
            (&mut self.when, when),
            (&mut self.checked, checked),
            (&mut self.disabled, disabled),
        ] {
            if value.is_some() {
                *slot = value;
            }
        }
    }
}

/// The top-level object as an ordered list of `(id, entry)` pairs.
///
/// A custom visitor rather than `serde_json::Map`, because Omarchy
/// renders rows in file order and `serde_json`'s default map is
/// sorted. Visiting the map ourselves sees the keys in document order
/// with no extra crate feature to keep aligned with anyone else's
/// `serde_json` dependency. Values that are not objects are skipped,
/// as `MenuModel.js` skips them.
struct OrderedEntries(Vec<(String, RawEntry)>);

impl<'de> Deserialize<'de> for OrderedEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = OrderedEntries;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object of menu entries keyed by id")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some((id, value)) = map.next_entry::<String, serde_json::Value>()? {
                    if !value.is_object() {
                        continue;
                    }
                    match serde_json::from_value::<RawEntry>(value) {
                        Ok(entry) => entries.push((id, entry)),
                        Err(error) => tracing::warn!(id, %error, "omarchy menu entry could not be read; skipping it"),
                    }
                }
                Ok(OrderedEntries(entries))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

/// Parses one JSONC menu file into ordered entries. A file that fails
/// to parse contributes nothing — Omarchy's documented behaviour: a
/// broken user extension drops every user entry while the shipped menu
/// keeps working — and says so in the log, since silence is how the
/// user would otherwise learn about the typo.
pub fn parse_entries(raw: &str) -> Vec<(String, RawEntry)> {
    let stripped = strip_jsonc(raw);
    if stripped.trim().is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<OrderedEntries>(&stripped) {
        Ok(OrderedEntries(entries)) => entries,
        Err(error) => {
            tracing::warn!(%error, "omarchy menu file did not parse; contributing no entries from it");
            Vec::new()
        }
    }
}

/// Overlays the user's entries on the shipped ones, Omarchy's
/// `mergeMenuSources`: a reused id is patched in place and keeps its
/// position, a new id appends.
pub fn merge_sources(defaults: Vec<(String, RawEntry)>, user: Vec<(String, RawEntry)>) -> Vec<(String, RawEntry)> {
    let mut merged = defaults;
    for (id, entry) in user {
        match merged.iter_mut().find(|(existing, _)| *existing == id) {
            Some((_, existing)) => existing.overlay(entry),
            None => merged.push((id, entry)),
        }
    }
    merged
}

// ------------------------------------------------------------- the model

/// Why an entry from the file has no row in the menu. Kept on the
/// model (see [`MenuModel::skipped`]) so the log and the tests can say
/// exactly which rule dropped what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    /// `provider: "<name>"` — rows come from Omarchy's shell at runtime.
    Provider(String),
    /// `target: "<id>"` — a link to a submenu rendered elsewhere.
    Link(String),
    /// The action commands Hyprland; see [`is_hyprland_only`].
    HyprlandOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NodeKind {
    /// Index into [`MenuModel::actions`].
    Action(usize),
    Submenu,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Node {
    id: String,
    label: String,
    kind: NodeKind,
    when: Option<String>,
    checked: Option<String>,
    disabled: Option<String>,
    /// Indices into the arena, in file order.
    children: Vec<usize>,
}

/// The merged menu as a tree, plus the flat list of commands the
/// action ids index. Built once per (re)load; rendering into
/// `MenuItem`s happens per open, against whatever condition snapshot
/// is current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuModel {
    nodes: Vec<Node>,
    roots: Vec<usize>,
    /// Every action's command, in file order, including rows a
    /// condition may hide: the menu's action id is
    /// `base + index` into this list, and hiding a row must not
    /// renumber its siblings between one open and the next.
    actions: Vec<String>,
    skipped: Vec<(String, Skip)>,
}

impl MenuModel {
    /// Builds the tree from merged, ordered entries.
    pub fn build(entries: Vec<(String, RawEntry)>) -> Self {
        let mut model = MenuModel::default();
        let mut kept_ids: HashSet<String> = HashSet::new();
        let mut kept: Vec<(String, RawEntry)> = Vec::new();
        for (id, entry) in entries {
            // Omarchy injects a synthetic `root` for the top level; a
            // file that declares one is describing the menu itself,
            // not a row in it.
            if id == "root" {
                continue;
            }
            if let Some(provider) = entry.provider.as_deref() {
                model.skipped.push((id, Skip::Provider(provider.to_string())));
                continue;
            }
            if let Some(action) = entry.action.as_deref() {
                if is_hyprland_only(action) {
                    model.skipped.push((id, Skip::HyprlandOnly));
                    continue;
                }
            } else if let Some(target) = entry.target.as_deref() {
                model.skipped.push((id, Skip::Link(target.to_string())));
                continue;
            }
            kept_ids.insert(id.clone());
            kept.push((id, entry));
        }

        let mut index_of: HashMap<String, usize> = HashMap::new();
        for (id, entry) in &kept {
            let kind = match &entry.action {
                Some(command) => {
                    model.actions.push(command.clone());
                    NodeKind::Action(model.actions.len() - 1)
                }
                None => NodeKind::Submenu,
            };
            index_of.insert(id.clone(), model.nodes.len());
            model.nodes.push(Node {
                id: id.clone(),
                label: entry.label.clone().unwrap_or_else(|| id.clone()),
                kind,
                when: entry.when.clone(),
                checked: entry.checked.clone(),
                disabled: entry.disabled.clone(),
                children: Vec::new(),
            });
        }
        // Parents are resolved against the *complete* kept set, after
        // every node exists, which is what lets a child be defined
        // before its parent in the file. Children land under their
        // parent in file order because `kept` is in file order.
        for (id, entry) in &kept {
            let node = index_of[id];
            match parent_of(id, entry.parent.as_deref(), &kept_ids).and_then(|parent| index_of.get(parent.as_str())) {
                Some(&parent) => model.nodes[parent].children.push(node),
                None => model.roots.push(node),
            }
        }
        model
    }

    /// Entries the file declared that have no row here, and why.
    pub fn skipped(&self) -> &[(String, Skip)] {
        &self.skipped
    }

    /// How many commands the action-id range `base..base + n` spans.
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    /// The command behind action index `index`.
    pub fn command(&self, index: usize) -> Option<&str> {
        self.actions.get(index).map(String::as_str)
    }

    /// Every id in the tree, in file order — for tests and the log.
    pub fn ids(&self) -> Vec<&str> {
        self.nodes.iter().map(|node| node.id.as_str()).collect()
    }

    /// Whether any node carries a condition at all — a model without
    /// one needs no batch, and a fixture with none must not fork bash.
    pub fn has_conditions(&self) -> bool {
        self.nodes.iter().any(|node| node.when.is_some() || node.checked.is_some() || node.disabled.is_some())
    }

    /// The one bash script that answers every condition in the model:
    /// Omarchy's `guardScript`, line for line — prelude, then one
    /// `if { <expr>; } >/dev/null 2>&1; then echo <id>:<tag>:1; else
    /// echo <id>:<tag>:0; fi` per condition. `None` when there is
    /// nothing to ask.
    pub fn condition_script(&self) -> Option<String> {
        let mut lines = String::new();
        for node in &self.nodes {
            for (tag, expression) in [("w", &node.when), ("c", &node.checked), ("d", &node.disabled)] {
                if let Some(expression) = expression {
                    lines.push_str(&condition_line(&node.id, tag, expression));
                }
            }
        }
        (!lines.is_empty()).then(|| format!("{}{lines}", condition_prelude(&lines)))
    }

    /// Renders the tree against a condition snapshot. `base` is the
    /// first action id; `inert` is the id a `disabled` row fires (one
    /// that resolves to nothing, so the pick dismisses the menu — the
    /// same device the dock's About rows use, since `MenuItem` has no
    /// disabled variant and growing the theme SDK for this one menu
    /// would touch every menu in the desktop).
    pub fn items(&self, conditions: Option<&Conditions>, base: u32, inert: u32) -> Vec<MenuItem> {
        self.render_level(&self.roots, conditions, base, inert)
    }

    fn render_level(&self, level: &[usize], conditions: Option<&Conditions>, base: u32, inert: u32) -> Vec<MenuItem> {
        // The marker gutter is per level: if any sibling can carry the
        // current-choice marker, every sibling gets the gutter so the
        // labels in the column line up — `bullet_label`'s contract, the
        // same way the Theme and Wallpaper submenus wear it.
        let gutter = level.iter().any(|&index| {
            let node = &self.nodes[index];
            node.checked.is_some() || node.disabled.is_some()
        });
        let mut out = Vec::new();
        for &index in level {
            let node = &self.nodes[index];
            if !self.visible(index, conditions) {
                continue;
            }
            let checked = node.checked.as_deref().is_some_and(|_| conditions.is_some_and(|c| c.get(&node.id, Tag::Checked) == Some(true)));
            let disabled = node.disabled.as_deref().is_some_and(|_| conditions.is_some_and(|c| c.get(&node.id, Tag::Disabled) == Some(true)));
            let label = if gutter { crate::desktop::bullet_label(checked || disabled, &node.label) } else { node.label.clone() };
            match node.kind {
                NodeKind::Action(action) => {
                    let action = if disabled { inert } else { base + action as u32 };
                    out.push(MenuItem::Action { label, action });
                }
                NodeKind::Submenu => {
                    let items = self.render_level(&node.children, conditions, base, inert);
                    out.push(MenuItem::Submenu { label, items });
                }
            }
        }
        out
    }

    /// Omarchy's `isVisible`: a `when` must have answered true; a
    /// submenu additionally needs at least one visible descendant, or
    /// it would open onto nothing.
    fn visible(&self, index: usize, conditions: Option<&Conditions>) -> bool {
        let node = &self.nodes[index];
        if node.when.is_some() && conditions.and_then(|c| c.get(&node.id, Tag::When)) != Some(true) {
            return false;
        }
        match node.kind {
            NodeKind::Action(_) => true,
            NodeKind::Submenu => node.children.iter().any(|&child| self.visible(child, conditions)),
        }
    }
}

/// The parent an entry belongs under: its explicit `parent` if it
/// names a kept entry, else the longest dotted prefix that does.
/// `None` means the top level. Omarchy takes exactly the id minus its
/// last segment and leaves an orphan unreachable when that parent does
/// not exist; the longest-existing-prefix rule agrees wherever the
/// direct parent exists and adopts the orphan upward where it does not
/// — an extension's `personal.notes` with no `personal` still shows.
fn parent_of(id: &str, explicit: Option<&str>, ids: &HashSet<String>) -> Option<String> {
    if let Some(parent) = explicit {
        return match parent {
            "" | "root" => None,
            named if ids.contains(named) => Some(named.to_string()),
            _ => None,
        };
    }
    let mut prefix = id;
    while let Some((head, _)) = prefix.rsplit_once('.') {
        if ids.contains(head) {
            return Some(head.to_string());
        }
        prefix = head;
    }
    None
}

/// Whether an action commands Hyprland rather than the desktop: any
/// shell word of the command whose basename is `hyprctl` or begins
/// with `omarchy-hyprland-`. Words are split on whitespace and on the
/// shell's operator characters, so `pkill x || hyprctl reload` and
/// `a; omarchy-hyprland-foo` both match while `hyprpicker`,
/// `hyprsunset` and a URL mentioning hyprland do not: the rule names
/// the compositor's control surface, not everything with `hypr` in it.
pub fn is_hyprland_only(action: &str) -> bool {
    action
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(' | ')' | '<' | '>' | '\'' | '"' | '`'))
        .filter(|word| !word.is_empty())
        .map(|word| word.rsplit('/').next().unwrap_or(word))
        .any(|word| word == "hyprctl" || word.starts_with("omarchy-hyprland-"))
}

// ----------------------------------------------------------- conditions

/// Which of the three guards a result line answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    When,
    Checked,
    Disabled,
}

impl Tag {
    fn from_letter(letter: &str) -> Option<Self> {
        match letter {
            "w" => Some(Tag::When),
            "c" => Some(Tag::Checked),
            "d" => Some(Tag::Disabled),
            _ => None,
        }
    }
}

/// One completed batch's answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conditions {
    results: HashMap<(String, Tag), bool>,
}

impl Conditions {
    /// Parses the batch's stdout: `<id>:<w|c|d>:<0|1>` per line, split
    /// from the right because ids may contain anything but a colon.
    /// Malformed lines are dropped, not fatal — the batch also carries
    /// whatever a user's login shell prints on the way in.
    pub fn parse(output: &str) -> Self {
        let mut results = HashMap::new();
        for line in output.lines() {
            let line = line.trim();
            let Some((rest, value)) = line.rsplit_once(':') else { continue };
            let Some((id, tag)) = rest.rsplit_once(':') else { continue };
            let Some(tag) = Tag::from_letter(tag) else { continue };
            if id.is_empty() {
                continue;
            }
            results.insert((id.to_string(), tag), value == "1");
        }
        Self { results }
    }

    pub fn get(&self, id: &str, tag: Tag) -> Option<bool> {
        self.results.get(&(id.to_string(), tag)).copied()
    }

    pub fn len(&self) -> usize {
        self.results.len()
    }

    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

fn condition_line(id: &str, tag: &str, expression: &str) -> String {
    format!("if {{ {}; }} >/dev/null 2>&1; then echo {id}:{tag}:1; else echo {id}:{tag}:0; fi\n", substitute_readers(expression))
}

/// Commands a `checked` expression reads a value out of, which every
/// sibling row asks alike (Defaults → Browser has seven rows comparing
/// against `$(omarchy-default-browser)`). Omarchy's `GUARD_READERS`,
/// verbatim: the batch runs each once and substitutes the answer.
const READERS: [&str; 6] = [
    "omarchy-channel-current",
    "omarchy-default-agent",
    "omarchy-default-browser",
    "omarchy-default-editor",
    "omarchy-default-terminal",
    "omarchy-dns",
];

fn reader_slot(index: usize) -> String {
    format!("${{__omarchy_read_{index}}}")
}

/// Replaces the plain `$(reader)` form only — Omarchy's reasoning: the
/// substitution and the variable are interchangeable (both strip
/// trailing newlines, both split alike unquoted), whereas shadowing the
/// reader with a function would also catch `command -v reader` and
/// answer it wrong.
fn substitute_readers(expression: &str) -> String {
    let mut out = expression.to_string();
    for (index, reader) in READERS.iter().enumerate() {
        out = out.replace(&format!("$({reader})"), &reader_slot(index));
    }
    out
}

/// Omarchy's `guardHelpers` plus the reader captures the guard lines
/// actually use. The helpers shadow `omarchy-pkg-present` and friends
/// with in-process lookups against one `pacman -Q` snapshot (provides
/// included, so gvim answers for vim), which is the difference between
/// a batch that finishes in a fraction of a second and one that forks
/// pacman once per Install row.
fn condition_prelude(lines: &str) -> String {
    let mut prelude = String::from(HELPERS);
    for (index, reader) in READERS.iter().enumerate() {
        if lines.contains(&reader_slot(index)) {
            // `|| :` so a reader that exits nonzero cannot take the
            // batch down under a login shell that turned on errexit.
            prelude.push_str(&format!("__omarchy_read_{index}=$({reader} 2>/dev/null) || :\n"));
        }
    }
    prelude
}

const HELPERS: &str = concat!(
    "declare -A __omarchy_pkgs=()\n",
    "mapfile -t __omarchy_pkg_names < <({ pacman -Qq; LC_ALL=C pacman -Qi",
    " | awk '/^[A-Za-z]/ { provides = ($0 ~ /^Provides/); sub(/^[^:]*: /, \"\") }",
    " provides && $0 != \"None\" { n = split($0, p, \" \");",
    " for (i = 1; i <= n; i++) { sub(/[<>=].*/, \"\", p[i]); print p[i] } }'; } 2>/dev/null)\n",
    "for __omarchy_pkg in \"${__omarchy_pkg_names[@]}\"; do __omarchy_pkgs[$__omarchy_pkg]=1; done\n",
    "__omarchy_pkg_has() { [[ -n ${__omarchy_pkgs[$1]-} ]] && return 0; ",
    "[[ $1 == *[\\<\\>=]* ]] && { pacman -Q \"$1\" &>/dev/null; return; }; return 1; }\n",
    "omarchy-pkg-present() { local p; for p in \"$@\"; do __omarchy_pkg_has \"$p\" || return 1; done; return 0; }\n",
    "omarchy-pkg-missing() { local p; for p in \"$@\"; do __omarchy_pkg_has \"$p\" || return 0; done; return 1; }\n",
    "omarchy-cmd-present() { local c; for c in \"$@\"; do command -v \"$c\" &>/dev/null || return 1; done; return 0; }\n",
    "omarchy-cmd-missing() { local c; for c in \"$@\"; do command -v \"$c\" &>/dev/null || return 0; done; return 1; }\n",
);

/// How long one batch may run before it is killed and discarded.
/// Omarchy's shipped menu finishes in well under a second with the
/// prelude above; the allowance is for a slow login profile or a cold
/// `pacman` database, and its only cost is staleness — a menu that
/// keeps showing the previous batch's answers a little longer.
pub const CONDITION_TIMEOUT: Duration = Duration::from_secs(10);

/// How long after an Omarchy action fires before the conditions are
/// re-asked. Long enough for the script the action ran to have done
/// its work (a toggle rewrites a state file and restarts something),
/// short enough that the marker has moved by the time the user opens
/// the menu again to check.
pub const REFRESH_AFTER_ACTION: Duration = Duration::from_millis(1500);

/// The background evaluation of one model's conditions.
///
/// One batch in flight at most: a request that arrives while one runs
/// is remembered and started when it lands, which is Omarchy's
/// `guardsPending` and for the same reason — two concurrent batches
/// would race to publish, and the loser could be the fresher one.
struct Evaluator {
    latest: Arc<Mutex<Option<Conditions>>>,
    in_flight: Arc<AtomicBool>,
    pending: bool,
    refresh_due: Option<Instant>,
}

impl Evaluator {
    fn new() -> Self {
        Self { latest: Arc::new(Mutex::new(None)), in_flight: Arc::new(AtomicBool::new(false)), pending: true, refresh_due: None }
    }

    fn snapshot(&self) -> Option<Conditions> {
        // A poisoned lock reads as "no snapshot yet": the worker that
        // panicked never published, and the conservative rendering is
        // the same one the first open before any batch gets.
        self.latest.lock().ok().and_then(|latest| latest.clone())
    }

    fn request(&mut self) {
        self.pending = true;
    }

    fn request_after(&mut self, now: Instant, delay: Duration) {
        self.refresh_due = Some(now + delay);
    }

    /// Starts a batch if one is due and none is running. Called from
    /// the shell tick; returns immediately in every case.
    fn service(&mut self, now: Instant, script: Option<&str>) {
        if self.refresh_due.is_some_and(|due| due <= now) {
            self.refresh_due = None;
            self.pending = true;
        }
        if !self.pending || self.in_flight.load(Ordering::Acquire) {
            return;
        }
        self.pending = false;
        let Some(script) = script else {
            if let Ok(mut latest) = self.latest.lock() {
                *latest = Some(Conditions::default());
            }
            return;
        };
        self.in_flight.store(true, Ordering::Release);
        let latest = Arc::clone(&self.latest);
        let in_flight = Arc::clone(&self.in_flight);
        let script = script.to_string();
        let spawned = std::thread::Builder::new().name("chonkstep-omarchy-conditions".to_string()).spawn(move || {
            let started = Instant::now();
            if let Some(conditions) = run_condition_batch(&script, CONDITION_TIMEOUT) {
                tracing::info!(answers = conditions.len(), elapsed_ms = started.elapsed().as_millis() as u64, "omarchy menu conditions evaluated");
                if let Ok(mut slot) = latest.lock() {
                    *slot = Some(conditions);
                }
            }
            in_flight.store(false, Ordering::Release);
        });
        if let Err(error) = spawned {
            tracing::warn!(?error, "could not start the omarchy condition thread; the menu keeps its last answers");
            self.in_flight.store(false, Ordering::Release);
        }
    }
}

/// Runs one batch to completion or death. `None` for a batch that was
/// killed or exited nonzero — Omarchy's rule: a run that did not finish
/// has only told us about the rows it reached, and a `when` it never
/// answered would show a row on nothing, so the last complete set
/// stands instead.
///
/// Runs on the evaluator thread only, which is what makes the waiting
/// below legitimate — see `clippy.toml`.
fn run_condition_batch(script: &str, timeout: Duration) -> Option<Conditions> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut command = Command::new("bash");
    command.args(["-lc", script]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    // Its own process group, so the timeout can take every helper the
    // conditions forked down with the shell: killing bash alone would
    // leave a stuck `pacman` holding the stdout pipe open, and the
    // reader below would wait on it for as long as it lived.
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(?error, "could not run bash for the omarchy menu conditions");
            return None;
        }
    };
    let pid = child.id();
    let mut stdout = child.stdout.take()?;
    // Drained on its own thread so a batch that writes more than a
    // pipe buffer cannot deadlock against the wait loop below.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut collected = String::new();
        let _ = stdout.read_to_string(&mut collected);
        let _ = tx.send(collected);
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                tracing::warn!(?timeout, "omarchy menu conditions did not finish in time; killing the batch and keeping the last answers");
                // SAFETY: `kill` on the negated pid of a process group
                // this process created and has not reaped; the only
                // effect is delivering a signal to that group.
                unsafe {
                    libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                }
                // Audited exception to `clippy.toml`'s ban: this is the
                // evaluator's own thread, the child has just been
                // killed, and reaping it is what keeps a zombie out of
                // the process table.
                #[allow(clippy::disallowed_methods)]
                let _ = child.wait();
                break None;
            }
            Err(error) => {
                tracing::warn!(?error, "waiting on the omarchy menu conditions failed");
                break None;
            }
        }
    };
    let status = status?;
    if !status.success() {
        tracing::warn!(%status, "omarchy menu conditions exited abnormally; keeping the last answers");
        return None;
    }
    // The pipe closes when every holder is gone; a helper that
    // daemonized out of the group could keep it open, which is what
    // the bound on this receive is for.
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(output) => Some(Conditions::parse(&output)),
        Err(_) => {
            tracing::warn!("omarchy menu conditions finished but their output never closed; keeping the last answers");
            None
        }
    }
}

// ------------------------------------------------------------- the menu

/// How often the two definition files' mtimes are compared, from the
/// shell tick. Two `stat`s a second on paths that exist.
const FILE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The shell-facing handle: the two files, the model built from them,
/// the condition evaluator, and the change polling that ties a
/// re-read to an edit or an Omarchy upgrade.
pub struct OmarchyMenu {
    paths: MenuPaths,
    model: MenuModel,
    /// The model's batch script, built once per reload: the tick asks
    /// for it every iteration, and rebuilding a few hundred guard lines
    /// at frame rate would be work for nothing.
    script: Option<String>,
    evaluator: Evaluator,
    /// Bumped on every re-read. The root menu session records the
    /// generation it was built from, so an action index fired from a
    /// menu opened before a reload cannot name a different command in
    /// the model the reload produced.
    generation: u64,
    mtimes: (Option<SystemTime>, Option<SystemTime>),
    last_poll: Instant,
}

impl OmarchyMenu {
    /// Reads and builds the menu for the paths found by
    /// [`MenuPaths::discover`], or `None` when Omarchy is not
    /// installed. The first condition batch is requested here and
    /// started by the first [`Self::tick`].
    pub fn discover() -> Option<Self> {
        MenuPaths::discover().map(Self::load)
    }

    /// Reads and builds the menu from `paths`.
    pub fn load(paths: MenuPaths) -> Self {
        let now = Instant::now();
        let mut menu = Self {
            paths,
            model: MenuModel::default(),
            script: None,
            evaluator: Evaluator::new(),
            generation: 0,
            mtimes: (None, None),
            last_poll: now,
        };
        menu.reload();
        menu
    }

    /// Re-reads both files and rebuilds. Also what a config reload and
    /// the mtime poll call.
    pub fn reload(&mut self) {
        self.mtimes = (mtime(&self.paths.default), mtime(&self.paths.user));
        let defaults = read_entries(&self.paths.default);
        let user = read_entries(&self.paths.user);
        self.model = MenuModel::build(merge_sources(defaults, user));
        self.script = self.model.condition_script();
        self.generation = self.generation.wrapping_add(1);
        for (id, why) in self.model.skipped() {
            tracing::debug!(id, ?why, "omarchy menu entry has no row here");
        }
        tracing::info!(
            entries = self.model.nodes.len(),
            actions = self.model.action_count(),
            skipped = self.model.skipped().len(),
            default = %self.paths.default.display(),
            "omarchy menu loaded"
        );
        self.evaluator.request();
    }

    /// The shell-tick hook: polls the files for change at
    /// [`FILE_POLL_INTERVAL`] and starts a condition batch if one is
    /// due. Never blocks.
    pub fn tick(&mut self, now: Instant) {
        if now.duration_since(self.last_poll) >= FILE_POLL_INTERVAL {
            self.last_poll = now;
            if (mtime(&self.paths.default), mtime(&self.paths.user)) != self.mtimes {
                tracing::info!("omarchy menu definition changed on disk; re-reading it");
                self.reload();
            }
        }
        self.evaluator.service(now, self.script.as_deref());
    }

    /// The submenu's rows against the latest completed condition
    /// snapshot. Empty when nothing is visible yet, which the caller
    /// treats as "no submenu".
    pub fn items(&self, base: u32, inert: u32) -> Vec<MenuItem> {
        let snapshot = self.evaluator.snapshot();
        self.model.items(snapshot.as_ref(), base, inert)
    }

    pub fn action_count(&self) -> usize {
        self.model.action_count()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The command behind action index `index` of the model at
    /// `generation`, or `None` if the model has been rebuilt since —
    /// the stale-menu guard the doc on [`Self::generation`] describes.
    pub fn command(&self, generation: u64, index: usize) -> Option<&str> {
        (generation == self.generation).then(|| self.model.command(index)).flatten()
    }

    /// An Omarchy action was just run: re-ask the conditions once it
    /// has had time to act.
    pub fn note_action_fired(&mut self, now: Instant) {
        self.evaluator.request_after(now, REFRESH_AFTER_ACTION);
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// One file's entries; a missing file (the user extension usually) is
/// simply empty, an unreadable one is empty and logged.
fn read_entries(path: &Path) -> Vec<(String, RawEntry)> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_entries(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), ?error, "omarchy menu file could not be read");
            Vec::new()
        }
    }
}

/// The exact exec form Omarchy's shell uses for a menu action —
/// `Quickshell.execDetached(["bash", "-lc", command])` — as the argv
/// `spawn::spawn_detached_with_env` takes. A function rather than a
/// constant so the one place that knows the form is also the one the
/// tests pin.
pub fn action_argv(command: &str) -> (&'static str, [&str; 2]) {
    ("bash", ["-lc", command])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(fields: &[(&str, &str)]) -> RawEntry {
        let mut entry = RawEntry::default();
        for (key, value) in fields {
            let slot = match *key {
                "label" => &mut entry.label,
                "action" => &mut entry.action,
                "target" => &mut entry.target,
                "provider" => &mut entry.provider,
                "parent" => &mut entry.parent,
                "when" => &mut entry.when,
                "checked" => &mut entry.checked,
                "disabled" => &mut entry.disabled,
                other => panic!("unknown field {other}"),
            };
            *slot = Some(value.to_string());
        }
        entry
    }

    fn labels(items: &[MenuItem]) -> Vec<&str> {
        items.iter().map(MenuItem::label).collect()
    }

    fn submenu<'a>(items: &'a [MenuItem], label: &str) -> &'a [MenuItem] {
        match items.iter().find(|item| item.label().trim_start_matches(['\u{2022}', ' ']) == label) {
            Some(MenuItem::Submenu { items, .. }) => items,
            other => panic!("expected submenu {label:?}, found {other:?}"),
        }
    }

    // A faithful excerpt of the shipped file: end-of-line comments, a
    // URL inside an action, a child defined before its parent, both
    // providers, a Hyprland-only action, and every guard kind.
    const FIXTURE: &str = r#"{
  // Omarchy menu definition.
  // Kind is inferred: action -> action, target -> link, otherwise submenu.

  // Root Menu
  "apps": {"icon":"󰀻","label":"Apps","aliases":["app","applications"],"provider":"apps"},
  "learn": {"icon":"󰧑","label":"Learn"},
  "trigger": {"icon":"󱓞","label":"Trigger"},
  "style": {"icon":"","label":"Style"},
  "about": {"icon":"","label":"About","action":"omarchy-launch-about"},

  // Learn
  "learn.omarchy": {"icon":"","label":"Omarchy","action":"omarchy-launch-webapp 'https://omarchy.org/manual/'"}, // trailing comment
  "learn.hyprland": {"icon":"","label":"Hyprland","action":"omarchy-launch-webapp 'https://wiki.hypr.land/'"},

  // Trigger
  "trigger.capture": {"icon":"","label":"Capture"},
  "trigger.capture.screenshot": {"icon":"","label":"Screenshot","action":"omarchy-capture-screenshot"},
  "trigger.capture.screenrecord.stop": {"icon":"","label":"Stop Screenrecording","when":"pgrep -f '^gpu-screen-recorder'","action":"omarchy-capture-screenrecording --stop-recording"},
  "trigger.capture.screenrecord": {"icon":"","label":"Screenrecord"},
  "trigger.capture.screenrecord.no-audio": {"icon":"","label":"With no audio","action":"omarchy-capture-screenrecording"},
  "trigger.capture.color": {"icon":"󰃉","label":"Color","action":"pkill hyprpicker || hyprpicker -a"},
  "trigger.hardware": {"icon":"","label":"Hardware"},
  "trigger.hardware.laptop-display": {"icon":"󰛧","label":"Laptop Display","when":"omarchy-hw-laptop","action":"omarchy-hyprland-monitor-internal toggle"},
  "trigger.toggle": {"icon":"󰔎","label":"Toggle"},
  "trigger.toggle.gaps": {"icon":"","label":"Gaps","action":"omarchy-hyprland-window-gaps-toggle"},
  "trigger.toggle.nightlight": {"icon":"","label":"Nightlight","action":"omarchy-toggle-nightlight"},

  // Style
  "style.font": {"icon":"","label":"Font","provider":"fonts"},
  "style.theme": {"icon":"","label":"Theme"},
  "style.theme.tokyo": {"icon":"","label":"Tokyo Night","checked":"[[ \"$(omarchy-theme-current)\" == \"tokyo-night\" ]]","action":"omarchy-theme-set tokyo-night"},
  "style.theme.link": {"icon":"","label":"Also Theme","target":"style.theme"},

  // Install
  "install": {"icon":"󰉉","label":"Install"},
  "install.editor": {"icon":"","label":"Editor"},
  "install.editor.vim": {"icon":"","label":"Vim","disabled":"omarchy-pkg-present vim","action":"omarchy-install-vim"},
  "install.editor.zed": {"icon":"","label":"Zed","disabled":"omarchy-pkg-present zed","action":"omarchy-install-zed"},
}
"#;

    #[test]
    fn line_comments_are_stripped_only_outside_strings() {
        let stripped = strip_jsonc("{\n  // whole line\n  \"a\": \"https://x.y/z\", // trailing\n  \"b\": \"//not a comment\"\n}");
        assert_eq!(stripped, "{\n  \n  \"a\": \"https://x.y/z\", \n  \"b\": \"//not a comment\"\n}");
    }

    #[test]
    fn block_comments_are_stripped_and_keep_their_newlines() {
        let stripped = strip_jsonc("{ /* one\ntwo */ \"a\": \"/* not */\" }");
        assert_eq!(stripped, "{ \n \"a\": \"/* not */\" }");
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        let stripped = strip_jsonc("{\"a\": \"say \\\"hi\\\" // still text\", // gone\n}");
        assert_eq!(stripped, "{\"a\": \"say \\\"hi\\\" // still text\" \n}");
    }

    #[test]
    fn trailing_commas_are_removed_before_closing_braces_and_brackets() {
        assert_eq!(strip_jsonc("{\"a\": [1, 2,], \"b\": {\"c\": 1,},\n}"), "{\"a\": [1, 2], \"b\": {\"c\": 1}\n}");
        // Inside a string a comma before a brace is content.
        assert_eq!(strip_jsonc("{\"a\": \"x,}\"}"), "{\"a\": \"x,}\"}");
    }

    #[test]
    fn multibyte_glyphs_survive_the_stripping_intact() {
        let text = "{\"icon\":\"󰀻\",\"label\":\"Apps ✓\",}";
        assert_eq!(strip_jsonc(text), "{\"icon\":\"󰀻\",\"label\":\"Apps ✓\"}");
    }

    #[test]
    fn entries_parse_in_file_order_with_unknown_fields_ignored() {
        let entries = parse_entries(FIXTURE);
        let ids: Vec<&str> = entries.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(&ids[..5], &["apps", "learn", "trigger", "style", "about"]);
        let about = &entries[4].1;
        assert_eq!(about.label.as_deref(), Some("About"));
        assert_eq!(about.action.as_deref(), Some("omarchy-launch-about"));
        assert_eq!(entries.len(), 26);
    }

    #[test]
    fn a_non_string_guard_reads_as_undeclared_rather_than_failing_the_file() {
        let entries = parse_entries("{\"a\": {\"label\": \"A\", \"when\": true, \"action\": \"x\"}, \"b\": 3}");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.when, None);
        assert_eq!(entries[0].1.action.as_deref(), Some("x"));
    }

    #[test]
    fn a_file_that_does_not_parse_contributes_nothing() {
        assert!(parse_entries("{\"a\": {").is_empty());
        assert!(parse_entries("").is_empty());
        assert!(parse_entries("   // only a comment\n").is_empty());
    }

    #[test]
    fn a_user_entry_overrides_only_the_fields_it_declares_and_keeps_its_place() {
        let defaults = vec![
            ("about".to_string(), entry(&[("label", "About"), ("action", "omarchy-launch-about")])),
            ("system".to_string(), entry(&[("label", "System")])),
        ];
        let user = vec![
            ("about".to_string(), entry(&[("action", "fastfetch")])),
            ("personal".to_string(), entry(&[("label", "Personal")])),
        ];
        let merged = merge_sources(defaults, user);
        let ids: Vec<&str> = merged.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["about", "system", "personal"]);
        assert_eq!(merged[0].1.label.as_deref(), Some("About"));
        assert_eq!(merged[0].1.action.as_deref(), Some("fastfetch"));
    }

    #[test]
    fn dotted_ids_build_the_tree_even_when_a_child_precedes_its_parent() {
        let model = MenuModel::build(parse_entries(FIXTURE));
        let items = model.items(Some(&Conditions::default()), 0, u32::MAX);
        let capture = submenu(submenu(&items, "Trigger"), "Capture");
        assert_eq!(labels(capture), ["Screenshot", "Screenrecord", "Color"]);
        // `screenrecord.stop` is defined before `screenrecord` in the
        // file and hidden by its `when` here (no answer); `no-audio`
        // is its sibling.
        assert_eq!(labels(submenu(capture, "Screenrecord")), ["With no audio"]);
    }

    #[test]
    fn a_child_with_no_existing_parent_is_adopted_by_its_longest_existing_prefix() {
        let entries = vec![
            ("style".to_string(), entry(&[("label", "Style")])),
            ("style.bar.transparency".to_string(), entry(&[("label", "Transparency"), ("action", "omarchy-bar transparent toggle")])),
            ("personal.notes".to_string(), entry(&[("label", "Notes"), ("action", "omarchy-launch-editor ~/notes")])),
        ];
        let model = MenuModel::build(entries);
        let items = model.items(None, 0, u32::MAX);
        assert_eq!(labels(&items), ["Style", "Notes"]);
        assert_eq!(labels(submenu(&items, "Style")), ["Transparency"]);
    }

    #[test]
    fn an_explicit_parent_field_wins_over_the_dotted_id() {
        let entries = vec![
            ("a".to_string(), entry(&[("label", "A")])),
            ("b".to_string(), entry(&[("label", "B")])),
            ("a.thing".to_string(), entry(&[("label", "Thing"), ("action", "x"), ("parent", "b")])),
        ];
        let items = MenuModel::build(entries).items(None, 0, u32::MAX);
        assert_eq!(labels(&items), ["B"]);
        assert_eq!(labels(submenu(&items, "B")), ["Thing"]);
    }

    #[test]
    fn providers_links_and_hyprland_only_actions_are_skipped_with_a_reason() {
        let model = MenuModel::build(parse_entries(FIXTURE));
        let skipped: Vec<(&str, &Skip)> = model.skipped().iter().map(|(id, why)| (id.as_str(), why)).collect();
        assert!(skipped.contains(&("apps", &Skip::Provider("apps".to_string()))));
        assert!(skipped.contains(&("style.font", &Skip::Provider("fonts".to_string()))));
        assert!(skipped.contains(&("style.theme.link", &Skip::Link("style.theme".to_string()))));
        assert!(skipped.contains(&("trigger.hardware.laptop-display", &Skip::HyprlandOnly)));
        assert!(skipped.contains(&("trigger.toggle.gaps", &Skip::HyprlandOnly)));
        assert_eq!(skipped.len(), 5);
        // Learn → Hyprland is a manual, not a compositor call, and
        // stays; the Color picker is `hyprpicker`, not Hyprland.
        assert!(model.ids().contains(&"learn.hyprland"));
        assert!(model.ids().contains(&"trigger.capture.color"));
    }

    #[test]
    fn a_submenu_whose_only_children_were_skipped_disappears_with_them() {
        let model = MenuModel::build(parse_entries(FIXTURE));
        let items = model.items(Some(&Conditions::default()), 0, u32::MAX);
        // `trigger.hardware` held one Hyprland-only row; `Toggle`
        // keeps Nightlight.
        assert_eq!(labels(submenu(&items, "Trigger")), ["Capture", "Toggle"]);
        assert_eq!(labels(submenu(submenu(&items, "Trigger"), "Toggle")), ["Nightlight"]);
        // The two provider submenus leave `Style` with only its theme
        // list, and `apps` is gone from the top level.
        assert_eq!(labels(&items), ["Learn", "Trigger", "Style", "About", "Install"]);
    }

    #[test]
    fn the_hyprland_rule_matches_shell_words_not_substrings() {
        assert!(is_hyprland_only("hyprctl reload"));
        assert!(is_hyprland_only("pkill foo || hyprctl dispatch exit"));
        assert!(is_hyprland_only("omarchy-hyprland-monitor-internal toggle"));
        assert!(is_hyprland_only("a; omarchy-hyprland-window-gaps-toggle"));
        assert!(is_hyprland_only("/usr/bin/hyprctl version"));
        assert!(is_hyprland_only("(hyprctl keyword misc:x 1)"));
        assert!(!is_hyprland_only("pkill hyprpicker || hyprpicker -a"));
        assert!(!is_hyprland_only("omarchy-restart-hyprsunset"));
        assert!(!is_hyprland_only("omarchy-launch-webapp 'https://wiki.hypr.land/'"));
        assert!(!is_hyprland_only("omarchy-launch-floating-terminal-with-presentation omarchy-refresh-hyprland"));
        assert!(!is_hyprland_only("echo hyprctl-ish"));
    }

    #[test]
    fn condition_results_map_to_hidden_shown_and_marked_rows() {
        let model = MenuModel::build(parse_entries(FIXTURE));
        let base = 100;
        let inert = 7;
        // No snapshot yet: `when` rows hidden, marker rows plain.
        let items = model.items(None, base, inert);
        let record = submenu(submenu(submenu(&items, "Trigger"), "Capture"), "Screenrecord");
        assert_eq!(labels(record), ["With no audio"]);
        let editor = submenu(submenu(&items, "Install"), "Editor");
        assert_eq!(labels(editor), ["  Vim", "  Zed"]);

        let mut answered = Conditions::default();
        answered.results.insert(("trigger.capture.screenrecord.stop".to_string(), Tag::When), true);
        answered.results.insert(("style.theme.tokyo".to_string(), Tag::Checked), true);
        answered.results.insert(("install.editor.vim".to_string(), Tag::Disabled), true);
        answered.results.insert(("install.editor.zed".to_string(), Tag::Disabled), false);
        let items = model.items(Some(&answered), base, inert);
        let record = submenu(submenu(submenu(&items, "Trigger"), "Capture"), "Screenrecord");
        assert_eq!(labels(record), ["Stop Screenrecording", "With no audio"]);
        assert_eq!(labels(submenu(submenu(&items, "Style"), "Theme")), ["\u{2022} Tokyo Night"]);
        let editor = submenu(submenu(&items, "Install"), "Editor");
        assert_eq!(labels(editor), ["\u{2022} Vim", "  Zed"]);
        // The disabled row fires the inert id; the live one its own.
        let zed_index = model.actions.iter().position(|c| c == "omarchy-install-zed").unwrap();
        match (&editor[0], &editor[1]) {
            (MenuItem::Action { action: vim, .. }, MenuItem::Action { action: zed, .. }) => {
                assert_eq!(*vim, inert);
                assert_eq!(*zed, base + zed_index as u32);
            }
            other => panic!("expected two action rows, got {other:?}"),
        }

        let mut refused = Conditions::default();
        refused.results.insert(("trigger.capture.screenrecord.stop".to_string(), Tag::When), false);
        let items = model.items(Some(&refused), base, inert);
        let record = submenu(submenu(submenu(&items, "Trigger"), "Capture"), "Screenrecord");
        assert_eq!(labels(record), ["With no audio"]);
    }

    #[test]
    fn action_indices_are_stable_whether_or_not_a_row_is_hidden() {
        let model = MenuModel::build(parse_entries(FIXTURE));
        let stop = model.actions.iter().position(|c| c.ends_with("--stop-recording")).unwrap();
        let no_audio = model.actions.iter().position(|c| c == "omarchy-capture-screenrecording").unwrap();
        assert_eq!(no_audio, stop + 1);
        let items = model.items(None, 0, u32::MAX);
        let record = submenu(submenu(submenu(&items, "Trigger"), "Capture"), "Screenrecord");
        assert_eq!(record, &[MenuItem::Action { label: "With no audio".to_string(), action: no_audio as u32 }]);
        assert_eq!(model.command(stop), Some("omarchy-capture-screenrecording --stop-recording"));
        assert_eq!(model.command(model.action_count()), None);
    }

    #[test]
    fn the_condition_script_is_omarchys_guard_script_with_readers_captured_once() {
        let model = MenuModel::build(parse_entries(FIXTURE));
        let script = model.condition_script().expect("the fixture has conditions");
        assert!(script.starts_with("declare -A __omarchy_pkgs=()\n"));
        assert!(script.contains("omarchy-pkg-present() {"));
        assert!(script.contains("if { pgrep -f '^gpu-screen-recorder'; } >/dev/null 2>&1; then echo trigger.capture.screenrecord.stop:w:1; else echo trigger.capture.screenrecord.stop:w:0; fi\n"));
        assert!(script.contains("echo install.editor.vim:d:1;"));
        assert!(script.contains("echo style.theme.tokyo:c:1;"));
        // No fixture guard reads a `GUARD_READERS` command, so no
        // capture line is emitted for one.
        assert!(!script.contains("__omarchy_read_"));

        let entries = vec![(
            "setup.default.browser.firefox".to_string(),
            entry(&[("label", "Firefox"), ("checked", "[[ \"$(omarchy-default-browser)\" == \"firefox\" ]]"), ("action", "omarchy-default-browser firefox")]),
        )];
        let script = MenuModel::build(entries).condition_script().unwrap();
        assert!(script.contains("__omarchy_read_2=$(omarchy-default-browser 2>/dev/null) || :\n"));
        assert!(script.contains("if { [[ \"${__omarchy_read_2}\" == \"firefox\" ]]; }"));
        assert!(MenuModel::build(vec![("a".to_string(), entry(&[("action", "x")]))]).condition_script().is_none());
    }

    #[test]
    fn condition_output_parses_by_splitting_from_the_right() {
        let parsed = Conditions::parse("noise from a profile\ntrigger.capture.screenrecord.stop:w:0\ninstall.editor.vim:d:1\n\nstyle.theme.tokyo:c:1\nbad:x:1\n:w:1\n");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed.get("trigger.capture.screenrecord.stop", Tag::When), Some(false));
        assert_eq!(parsed.get("install.editor.vim", Tag::Disabled), Some(true));
        assert_eq!(parsed.get("style.theme.tokyo", Tag::Checked), Some(true));
        assert_eq!(parsed.get("style.theme.tokyo", Tag::When), None);
    }

    #[test]
    fn the_exec_form_is_omarchys_login_shell_dash_c() {
        assert_eq!(action_argv("omarchy-theme-set tokyo-night"), ("bash", ["-lc", "omarchy-theme-set tokyo-night"]));
    }

    #[test]
    fn paths_follow_omarchy_path_and_xdg_config_home_with_home_fallbacks() {
        let paths = MenuPaths::from_env(None, None, Some("/home/u".into()));
        assert_eq!(paths.default, PathBuf::from("/home/u/.local/share/omarchy/default/omarchy/omarchy-menu.jsonc"));
        assert_eq!(paths.user, PathBuf::from("/home/u/.config/omarchy/extensions/omarchy-menu.jsonc"));
        let paths = MenuPaths::from_env(Some("/opt/omarchy".into()), Some("/tmp/cfg".into()), Some("/home/u".into()));
        assert_eq!(paths.default, PathBuf::from("/opt/omarchy/default/omarchy/omarchy-menu.jsonc"));
        assert_eq!(paths.user, PathBuf::from("/tmp/cfg/omarchy/extensions/omarchy-menu.jsonc"));
        // An empty variable is as good as unset.
        let paths = MenuPaths::from_env(Some("".into()), Some("".into()), Some("/home/u".into()));
        assert_eq!(paths.default, PathBuf::from("/home/u/.local/share/omarchy/default/omarchy/omarchy-menu.jsonc"));
    }

    #[test]
    fn a_reload_after_an_edit_changes_the_generation_and_the_tree() {
        let dir = std::env::temp_dir().join(format!("chonk-omarchy-menu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("default/omarchy")).unwrap();
        let paths = MenuPaths::from_env(Some(dir.clone().into()), Some(dir.join("cfg").into()), Some(dir.clone().into()));
        std::fs::write(&paths.default, "{\"a\": {\"label\": \"A\", \"action\": \"true\"}}").unwrap();
        let mut menu = OmarchyMenu::load(paths.clone());
        let first = menu.generation();
        assert_eq!(labels(&menu.items(0, u32::MAX)), ["A"]);
        assert_eq!(menu.command(first, 0), Some("true"));

        std::fs::create_dir_all(paths.user.parent().unwrap()).unwrap();
        std::fs::write(&paths.user, "{\"a\": {\"label\": \"Renamed\"}, \"b\": {\"label\": \"B\", \"action\": \"false\"}}").unwrap();
        menu.reload();
        assert_ne!(menu.generation(), first);
        assert_eq!(labels(&menu.items(0, u32::MAX)), ["Renamed", "B"]);
        // The old generation's index no longer resolves: a menu opened
        // before the reload cannot fire into the new list.
        assert_eq!(menu.command(first, 0), None);
        assert_eq!(menu.command(menu.generation(), 1), Some("false"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_model_without_conditions_gets_an_empty_snapshot_without_forking() {
        let mut evaluator = Evaluator::new();
        assert!(evaluator.snapshot().is_none());
        evaluator.service(Instant::now(), None);
        assert_eq!(evaluator.snapshot(), Some(Conditions::default()));
        assert!(!evaluator.in_flight.load(Ordering::Acquire));
    }

    #[test]
    fn a_post_action_refresh_waits_out_its_debounce() {
        let mut evaluator = Evaluator::new();
        evaluator.service(Instant::now(), None);
        let now = Instant::now();
        evaluator.request_after(now, Duration::from_secs(5));
        assert!(!evaluator.pending);
        evaluator.service(now + Duration::from_secs(4), None);
        assert!(evaluator.refresh_due.is_some(), "not due yet");
        evaluator.service(now + Duration::from_secs(5), None);
        assert!(evaluator.refresh_due.is_none(), "the due refresh was consumed");
    }

    /// Runs a real batch — bash is on every machine this builds on —
    /// to pin the round trip from script to snapshot, including the
    /// kill path.
    #[test]
    fn a_real_batch_round_trips_and_a_stuck_one_is_killed() {
        let script = "echo a:w:1\necho b:c:0\n";
        let conditions = run_condition_batch(script, Duration::from_secs(20)).expect("bash should run");
        assert_eq!(conditions.get("a", Tag::When), Some(true));
        assert_eq!(conditions.get("b", Tag::Checked), Some(false));

        let started = Instant::now();
        assert_eq!(run_condition_batch("echo a:w:1\nsleep 30\n", Duration::from_millis(300)), None);
        assert!(started.elapsed() < Duration::from_secs(10), "the kill must not wait for the sleep");
        assert_eq!(run_condition_batch("echo a:w:1\nexit 3\n", Duration::from_secs(20)), None);
    }

    /// The installed Omarchy menu, when this machine has one: the
    /// whole point of the module is that the real file parses, so the
    /// test reads it rather than trusting the fixture alone. Skips
    /// cleanly elsewhere.
    #[test]
    fn the_installed_omarchy_menu_parses_into_a_full_tree() {
        let Some(paths) = MenuPaths::discover() else {
            eprintln!("no Omarchy menu installed here; skipping");
            return;
        };
        let text = std::fs::read_to_string(&paths.default).unwrap();
        let entries = parse_entries(&text);
        assert!(entries.len() >= 300, "expected the shipped menu to carry 300+ entries, parsed {}", entries.len());
        let model = MenuModel::build(entries);
        assert!(model.action_count() >= 250, "actions: {}", model.action_count());
        assert!(model.skipped().iter().any(|(id, why)| id == "apps" && matches!(why, Skip::Provider(_))));
        assert!(model.ids().contains(&"trigger.capture.screenrecord.stop"));
        assert!(model.ids().contains(&"learn.hyprland"));
        assert!(model.skipped().iter().all(|(_, why)| !matches!(why, Skip::Link(_))), "the shipped menu declares no links");
        let items = model.items(Some(&Conditions::default()), 0, u32::MAX);
        assert!(labels(&items).contains(&"System"));
        assert!(model.condition_script().is_some());
    }

    /// Runs the installed menu's real condition batch through a real
    /// login shell. Ignored by default: it forks `pacman` and every
    /// helper the guards call, takes a second or two, and answers
    /// depend on the machine — run it by hand when the prelude or a
    /// guard line changes.
    #[test]
    #[ignore]
    fn the_installed_menus_conditions_evaluate_within_the_timeout() {
        let Some(paths) = MenuPaths::discover() else { return };
        let model = MenuModel::build(parse_entries(&std::fs::read_to_string(&paths.default).unwrap()));
        let script = model.condition_script().unwrap();
        let started = Instant::now();
        let conditions = run_condition_batch(&script, CONDITION_TIMEOUT).expect("the shipped guards must complete");
        eprintln!("{} answers in {:?}", conditions.len(), started.elapsed());
        let asked = model.nodes.iter().map(|n| [&n.when, &n.checked, &n.disabled].into_iter().flatten().count()).sum::<usize>();
        assert_eq!(conditions.len(), asked, "every guard line must answer exactly once");
    }
}
