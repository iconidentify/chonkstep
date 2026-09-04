//! Omarchy's window rules, reduced to the one question this desktop
//! asks a newly mapped window: how big, and where.
//!
//! # What replaces the hardcoded prefix
//!
//! `wm_core::placement::float_override` has exactly one rule in it: a
//! window whose `app_id` starts `org.omarchy.` maps at 875×600,
//! centered. That number is not chonkstep's — it is a transcription of
//! Omarchy's own `windowrule = size 875 600, match:tag floating-window`,
//! copied in because there was no way to read it. This module is that
//! way, and the hardcoded rule stays behind it as the answer for a
//! machine with no Omarchy configuration to read.
//!
//! Reading it properly is worth more than the one number. Omarchy
//! floats fifteen classes of window at four different sizes — Steam at
//! 1100×700, picture-in-picture at 600×338 pinned to a corner, the
//! About box at 920×480 — and the hardcoded rule gets every one of
//! them wrong, in the direction of "the size Omarchy's terminal
//! windows want". A LocalSend transfer dialog is not a terminal.
//!
//! # Tags, and why they cannot be skipped
//!
//! Omarchy does not write `float` next to a class. It writes two rules:
//!
//! ```text
//! windowrule = tag +floating-window, match:class (org.omarchy.btop|…|imv|mpv)
//! windowrule = float on,             match:tag   floating-window
//! ```
//!
//! A reader that did not resolve tags would find no float rule naming
//! any class at all, and would conclude Omarchy floats nothing. So one
//! level of indirection is resolved: every rule that *adds* a tag
//! contributes its own matchers to that tag's set, and a rule matching
//! on the tag is expanded into one rule per contributing matcher.
//!
//! One level, not arbitrarily many. A rule that tags on the strength
//! of another tag (`match:tag pip` → `tag +foo`) is refused with a log
//! line rather than followed, because chasing tag chains means
//! ordering, removal (`tag -default-opacity`) and dynamic tags — a
//! rule engine, in a config reader, for a case Omarchy does not
//! currently write.
//!
//! # Refusing a rule whole
//!
//! A rule carrying a matcher this reader does not implement —
//! `match:xwayland 1`, `match:workspace 5`, `match:fullscreen 0` — is
//! dropped entirely, and says so. The alternative is to apply it on the
//! matchers that *were* understood, which turns "float this one
//! XWayland window" into "float every window of this class". Applying
//! half a rule is how a config reader becomes a bug report.

use std::sync::Arc;

use regex::{Regex, RegexBuilder};
use wm_core::{FloatDecision, FloatPolicy, Size, WindowRuleDecision};

use super::directive::{Matcher, WindowRule};

/// A compiled pattern with the text it came from, kept for log lines.
#[derive(Clone, Debug)]
struct Pattern {
    regex: Regex,
    source: String,
}

impl Pattern {
    /// Compiles one Hyprland matcher pattern.
    ///
    /// Bounded on purpose. `regex` is linear-time by construction — it
    /// has no backtracking, so no pattern can make matching quadratic —
    /// but a pattern can still ask for a large *compiled program*, and
    /// this one arrives from a file this desktop does not own. The
    /// size limit caps that; a pattern over it is refused like any
    /// other malformed one.
    fn compile(pattern: &str) -> Option<Self> {
        RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
            .ok()
            .map(|regex| Self {
                regex,
                source: pattern.to_string(),
            })
    }

    /// Hyprland matches window rules by *search*, not by full match:
    /// `o.window("localsend", …)` is what floats a window whose class
    /// is `localsend_app`, and half of Omarchy's own patterns carry an
    /// explicit `^…$` precisely because the default is unanchored.
    /// `Regex::is_match` is already a search, so this is the default
    /// behaviour rather than a decision — but it is the decision that
    /// makes fifteen of Omarchy's rules work, so it is written down.
    fn matches(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }
}

/// One resolved float rule: who it matches and what it says.
#[derive(Clone, Debug)]
struct Rule {
    class: Option<Pattern>,
    title: Option<Pattern>,
    float: Option<bool>,
    center: Option<bool>,
    /// Logical pixels, as Omarchy writes them. Scaled at the point of
    /// use, exactly as the hardcoded 875×600 always was.
    size: Option<Size>,
    idle_inhibit: Option<bool>,
    pin: Option<bool>,
    no_focus: Option<bool>,
    no_initial_focus: Option<bool>,
    focus_on_activate: Option<bool>,
    fullscreen: Option<bool>,
    maximize: Option<bool>,
}

impl Rule {
    fn matches(&self, class: &str, title: &str) -> bool {
        // A rule with no matcher at all matches everything — which is
        // what `o.window(".*", …)` means and is why Omarchy's
        // `suppress_event` and `tag +default-opacity` rules are written
        // that way. Correct, and harmless here: neither carries a
        // float property.
        self.class.as_ref().is_none_or(|p| p.matches(class))
            && self.title.as_ref().is_none_or(|p| p.matches(title))
    }

    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = &self.class {
            parts.push(format!("class {}", p.source));
        }
        if let Some(p) = &self.title {
            parts.push(format!("title {}", p.source));
        }
        if parts.is_empty() {
            parts.push("any window".into());
        }
        parts.join(", ")
    }
}

/// The float rules read out of a Hyprland configuration, ready to hand
/// to the window manager.
///
/// Implements [`FloatPolicy`], which is how it reaches the one place
/// this question is asked — the same shape `DecorationRules` already
/// uses to travel from this crate to the backend.
#[derive(Clone, Debug, Default)]
pub struct FloatRules {
    rules: Vec<Rule>,
}

impl FloatRules {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// The rules as one-line descriptions, for the log and the docs.
    pub fn descriptions(&self) -> Vec<String> {
        self.rules
            .iter()
            .map(|rule| {
                let mut what = Vec::new();
                match rule.float {
                    Some(true) => what.push("float".to_string()),
                    Some(false) => what.push("do not float".to_string()),
                    None => {}
                }
                if let Some(size) = rule.size {
                    what.push(format!("{}x{}", size.w, size.h));
                }
                if rule.center == Some(true) {
                    what.push("centered".to_string());
                }
                for (enabled, label) in [
                    (rule.idle_inhibit, "idle inhibited"),
                    (rule.pin, "pinned"),
                    (rule.no_focus, "never focused"),
                    (rule.no_initial_focus, "no initial focus"),
                    (rule.fullscreen, "fullscreen"),
                    (rule.maximize, "maximized"),
                ] {
                    if enabled == Some(true) {
                        what.push(label.to_string());
                    }
                }
                if rule.focus_on_activate == Some(false) {
                    what.push("activation cannot focus".to_string());
                }
                format!("{} -> {}", rule.describe(), what.join(", "))
            })
            .collect()
    }

    /// Wraps these rules for the window manager, or `None` when there
    /// are none — so a session with nothing to read installs no policy
    /// at all and keeps the built-in behaviour exactly.
    pub fn policy(self) -> Option<Arc<dyn FloatPolicy>> {
        if self.rules.is_empty() {
            None
        } else {
            Some(Arc::new(self))
        }
    }
}

impl FloatPolicy for FloatRules {
    /// The last matching rule wins, property by property, which is
    /// Hyprland's own ordering: `apps/system.lua` floats every
    /// `floating-window` at 875×600 and then `apps/steam.lua`, read
    /// after it, gives Steam 1100×700.
    fn decision_for(&self, class: &str, title: &str) -> Option<FloatDecision> {
        let mut float = None;
        let mut center = None;
        let mut size = None;
        for rule in &self.rules {
            if !rule.matches(class, title) {
                continue;
            }
            float = rule.float.or(float);
            center = rule.center.or(center);
            size = rule.size.or(size);
        }
        // A rule that only *sizes* a window still floats it here:
        // every window on this desktop already floats, so "size" and
        // "float, at this size" are the same statement. An explicit
        // `float off` is the one thing that takes it back.
        if float == Some(false) {
            return None;
        }
        if float.is_none() && size.is_none() {
            return None;
        }
        Some(FloatDecision {
            size,
            center: center.unwrap_or(true),
        })
    }

    fn window_decision_for(&self, class: &str, title: &str) -> WindowRuleDecision {
        let mut decision = WindowRuleDecision::default();
        for rule in &self.rules {
            if !rule.matches(class, title) {
                continue;
            }
            if let Some(value) = rule.idle_inhibit {
                decision.idle_inhibit = value;
            }
            if let Some(value) = rule.pin {
                decision.pin = value;
            }
            if let Some(value) = rule.no_focus {
                decision.no_focus = value;
            }
            if let Some(value) = rule.no_initial_focus {
                decision.no_initial_focus = value;
            }
            if let Some(value) = rule.focus_on_activate {
                decision.focus_on_activate = Some(value);
            }
            if let Some(value) = rule.fullscreen {
                decision.fullscreen = value;
            }
            if let Some(value) = rule.maximize {
                decision.maximize = value;
            }
        }
        decision
    }
}

/// Which class/title matcher pairs carry which tag — the first pass of
/// [`compile`], named so the two passes can talk about the same thing.
/// A tag may be added by several rules, and each of them contributes a
/// matcher pair that a rule matching the tag then expands into.
type TagCarriers = std::collections::BTreeMap<String, Vec<(Option<String>, Option<String>)>>;

/// Turns the window rules read out of a config into float rules,
/// resolving tags and reporting everything it declines.
///
/// Returns the rules and the lines to log — the caller owns logging so
/// that this function stays pure and testable, which is the same split
/// `startup.rs` makes between its `read_*` and `resolve_*` halves.
pub fn compile(rules: &[WindowRule]) -> (FloatRules, Vec<String>) {
    let mut notes = Vec::new();
    // Pass one: who carries which tag.
    let mut tagged: TagCarriers = TagCarriers::new();
    for rule in rules {
        for (name, value) in &rule.props {
            if name != "tag" {
                continue;
            }
            let Some(tag) = value.strip_prefix('+') else {
                if let Some(removed) = value.strip_prefix('-') {
                    notes.push(format!(
                        "window rule removes tag {removed}: tag removal is not followed"
                    ));
                }
                continue;
            };
            // A rule that tags on the strength of another tag would
            // need a second resolution pass; see the module docs.
            if rule.matchers.iter().any(|m| matches!(m, Matcher::Tag(_))) {
                notes.push(format!(
                    "window rule tags {tag} based on another tag: chained tags are not followed"
                ));
                continue;
            }
            let (class, title, refused) = split_matchers(&rule.matchers);
            if let Some(refused) = refused {
                notes.push(format!(
                    "window rule tagging {tag} carries {refused}, which this reader does not implement: rule skipped"
                ));
                continue;
            }
            tagged
                .entry(tag.to_string())
                .or_default()
                .push((class, title));
        }
    }
    // Pass two: the rules that actually say something about floating.
    let mut out = FloatRules::default();
    for rule in rules {
        let Some(spec) = rule_spec(rule, &mut notes) else {
            continue;
        };
        let (class, title, refused) = split_matchers(&rule.matchers);
        if let Some(refused) = refused {
            notes.push(format!(
                "float rule carries {refused}, which this reader does not implement: rule skipped"
            ));
            continue;
        }
        let tags: Vec<&String> = rule
            .matchers
            .iter()
            .filter_map(|m| {
                if let Matcher::Tag(t) = m {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        if tags.is_empty() {
            push(&mut out, &mut notes, class, title, &spec);
            continue;
        }
        for tag in tags {
            let Some(carriers) = tagged.get(tag) else {
                notes.push(format!(
                    "float rule matches tag {tag}, which no rule in this configuration adds: rule skipped"
                ));
                continue;
            };
            for (carrier_class, carrier_title) in carriers {
                // The tag rule's own class/title matchers, if it had
                // any, still apply on top of the carrier's.
                push(
                    &mut out,
                    &mut notes,
                    carrier_class.clone().or_else(|| class.clone()),
                    carrier_title.clone().or_else(|| title.clone()),
                    &spec,
                );
            }
        }
    }
    (out, notes)
}

/// The float-relevant half of a rule's properties, or `None` if it has
/// none — which is most of them.
fn rule_spec(rule: &WindowRule, notes: &mut Vec<String>) -> Option<Spec> {
    let mut spec = Spec::default();
    let mut any = false;
    for (name, value) in &rule.props {
        match name.as_str() {
            "float" => {
                spec.float = Some(truthy(value));
                any = true;
            }
            "center" => {
                spec.center = Some(truthy(value));
                any = true;
            }
            "size" => {
                // `size 875 600`. A size given as an expression —
                // Omarchy's picture-in-picture uses
                // `(monitor_w-window_w-40)` for its *position* and
                // plain numbers for its size, but the webcam overlay
                // sizes itself off the monitor — is not evaluated: this
                // reader has no monitor to evaluate it against, and a
                // guessed size is a window in the wrong place.
                let mut parts = value.split_whitespace();
                match (
                    parts.next().and_then(|w| w.parse().ok()),
                    parts.next().and_then(|h| h.parse().ok()),
                ) {
                    (Some(w), Some(h)) if w > 0 && h > 0 => {
                        spec.size = Some(Size::new(w, h));
                        any = true;
                    }
                    _ => {
                        // Still "any": a size this reader cannot
                        // evaluate is a rule it has to *report*, and a
                        // spec that came back `None` would be dropped
                        // silently. It contributes no float and no
                        // size, so the rule ends up saying nothing —
                        // out loud.
                        spec.unreadable_size = Some(value.clone());
                        any = true;
                    }
                }
            }
            "idle_inhibit" | "idleinhibit" => {
                spec.idle_inhibit = Some(truthy(value));
                any = true;
            }
            "pin" | "pinned" => {
                spec.pin = Some(truthy(value));
                any = true;
            }
            "no_focus" | "nofocus" => {
                spec.no_focus = Some(truthy(value));
                any = true;
            }
            "no_initial_focus" | "noinitialfocus" => {
                spec.no_initial_focus = Some(truthy(value));
                any = true;
            }
            "focus_on_activate" | "focusonactivate" => {
                spec.focus_on_activate = Some(truthy(value));
                any = true;
            }
            "fullscreen" => {
                spec.fullscreen = Some(truthy(value));
                any = true;
            }
            "maximize" | "maximized" => {
                spec.maximize = Some(truthy(value));
                any = true;
            }
            // `tag +name` is consumed by compile's first pass. A tag
            // matcher likewise participates in expansion, so neither
            // is a silently dropped property.
            "tag" => {}
            unsupported => notes.push(format!(
                "window rule property {unsupported} on {} is not implemented: property skipped",
                describe_matchers(rule)
            )),
        }
    }
    any.then_some(spec)
}

#[derive(Clone, Debug, Default)]
struct Spec {
    float: Option<bool>,
    center: Option<bool>,
    size: Option<Size>,
    unreadable_size: Option<String>,
    idle_inhibit: Option<bool>,
    pin: Option<bool>,
    no_focus: Option<bool>,
    no_initial_focus: Option<bool>,
    focus_on_activate: Option<bool>,
    fullscreen: Option<bool>,
    maximize: Option<bool>,
}

fn push(
    out: &mut FloatRules,
    notes: &mut Vec<String>,
    class: Option<String>,
    title: Option<String>,
    spec: &Spec,
) {
    if let Some(text) = &spec.unreadable_size {
        notes.push(format!(
            "float rule sizes a window with the expression {text:?}, which needs a monitor to evaluate: size ignored"
        ));
    }
    let class = match class {
        Some(text) => match Pattern::compile(&text) {
            Some(pattern) => Some(pattern),
            None => {
                notes.push(format!(
                    "float rule has an unreadable class pattern {text:?}: rule skipped"
                ));
                return;
            }
        },
        None => None,
    };
    let title = match title {
        Some(text) => match Pattern::compile(&text) {
            Some(pattern) => Some(pattern),
            None => {
                notes.push(format!(
                    "float rule has an unreadable title pattern {text:?}: rule skipped"
                ));
                return;
            }
        },
        None => None,
    };
    out.rules.push(Rule {
        class,
        title,
        float: spec.float,
        center: spec.center,
        size: spec.size,
        idle_inhibit: spec.idle_inhibit,
        pin: spec.pin,
        no_focus: spec.no_focus,
        no_initial_focus: spec.no_initial_focus,
        focus_on_activate: spec.focus_on_activate,
        fullscreen: spec.fullscreen,
        maximize: spec.maximize,
    });
}

fn describe_matchers(rule: &WindowRule) -> String {
    let parts: Vec<String> = rule
        .matchers
        .iter()
        .map(|matcher| match matcher {
            Matcher::Class(value) => format!("match:class {value}"),
            Matcher::Title(value) => format!("match:title {value}"),
            Matcher::Tag(value) => format!("match:tag {value}"),
            Matcher::Other { key, value } => format!("match:{key} {value}"),
        })
        .collect();
    if parts.is_empty() {
        "any window".into()
    } else {
        parts.join(", ")
    }
}

/// Splits a rule's matchers into the class and title patterns this
/// reader implements, and the name of the first one it does not.
fn split_matchers(matchers: &[Matcher]) -> (Option<String>, Option<String>, Option<String>) {
    let mut class = None;
    let mut title = None;
    for matcher in matchers {
        match matcher {
            Matcher::Class(pattern) => class = Some(pattern.clone()),
            Matcher::Title(pattern) => title = Some(pattern.clone()),
            Matcher::Tag(_) => {}
            Matcher::Other { key, value } => {
                return (class, title, Some(format!("match:{key} {value}")))
            }
        }
    }
    (class, title, None)
}

/// Hyprland's spelling of a boolean rule property.
fn truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "off" | "0" | "false" | "no"
    )
}
