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
//! level of indirection is resolved: every rule that adds or removes a
//! tag is kept as an event on that tag, in file order, and a rule
//! matching on the tag asks whether the window carries it.
//!
//! Removal is followed because Omarchy's opacity rules are written
//! with it: every window is tagged `default-opacity` first, each app
//! file that wants an opaque window removes the tag again, and the
//! opacity rule for the tag comes last. Membership is decided the way
//! Hyprland's own repeated rule passes settle it: the last matching
//! add or remove before the rule decides, and when none precedes the
//! rule, the last matching one anywhere in the file does — which is
//! what lets Omarchy's `floating-window` rules sit *above* the lines
//! that add the tag.
//!
//! One level, not arbitrarily many. A rule that tags on the strength
//! of another tag (`match:tag chromium-based-browser` →
//! `tag -default-opacity`, as Omarchy's browser file writes it) is
//! followed when that other tag is carried directly by class, title
//! or xdg tag; a tag whose carriers are themselves tag-matched is refused
//! with a log line, because chasing arbitrary chains is a rule engine
//! in a config reader, for a case Omarchy does not write.
//!
//! # The xdg tag
//!
//! `match:xdg_tag` reads `xdg_toplevel_tag_v1`: the application's own
//! stable, untranslated name for one of its windows (`main`,
//! `preferences`, `quake`). It is a regular expression like `class`
//! and `title`, and is matched against the tag the window carried
//! when it mapped — a client tags a window before its first commit,
//! so the same map-time identity the other two matchers see. A window
//! whose client never tagged it has an empty tag, which only `^$`
//! matches.
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
use wm_core::{
    FloatDecision, FloatPolicy, Point, RuleMetrics, RulePlacement, RuleWorkspace, RuleWorkspaceTarget, Size,
    WindowRuleDecision,
};

use super::directive::{Matcher, WindowRule};
use super::expr::{Expr, Values};

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
        RegexBuilder::new(&format!(r"\A(?:{pattern})\z"))
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
            .ok()
            .map(|regex| Self {
                regex,
                source: pattern.to_string(),
            })
    }

    /// Hyprland's CRegexMatchEngine uses RE2::FullMatch. Anchoring the
    /// whole expression at compile time preserves alternation and explicit
    /// anchors while preventing a rule for title `Steam` from resizing
    /// `Steam Big Picture Mode` or `Sign in to Steam` as a desktop window.
    fn matches(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }
}

/// A pair of layout expressions — a `size` or a `move` — with the
/// text it came from, kept for the log and the docs.
#[derive(Clone, Debug)]
struct Extent {
    x: Expr,
    y: Expr,
    source: String,
}

impl Extent {
    /// `size 875 600`, `move (monitor_w-window_w-40) (monitor_h*0.04)`:
    /// two space-separated expressions, each of which may be a plain
    /// number. Split on whitespace exactly as Hyprland splits the
    /// arguments, so an expression cannot contain a space there either.
    fn parse(value: &str) -> Result<Self, String> {
        let mut parts = value.split_whitespace();
        let (Some(x), Some(y), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err("needs exactly two values".into());
        };
        let x = Expr::parse(x).map_err(|why| format!("{x:?} {why}"))?;
        let y = Expr::parse(y).map_err(|why| format!("{y:?} {why}"))?;
        Ok(Self { x, y, source: value.trim().to_string() })
    }

    /// Both halves as a size, when neither needs a monitor to
    /// evaluate: what the fixed rules Omarchy writes most (`875 600`)
    /// compile to.
    fn constant_size(&self) -> Option<Size> {
        to_size(self.x.constant()?, self.y.constant()?)
    }

    /// Whether neither half needs a monitor.
    fn is_constant(&self) -> bool {
        self.x.constant().is_some() && self.y.constant().is_some()
    }

    fn eval(&self, values: &Values) -> Option<(f64, f64)> {
        Some((self.x.eval(values)?, self.y.eval(values)?))
    }
}

/// A rounded, strictly positive size, or `None`: a zero- or
/// negative-sized window is not a smaller window, it is an absent one.
fn to_size(w: f64, h: f64) -> Option<Size> {
    let (w, h) = (w.round(), h.round());
    (w >= 1.0 && h >= 1.0 && w <= u32::MAX as f64 && h <= u32::MAX as f64).then(|| Size::new(w as u32, h as u32))
}

/// The identity a rule is matched against: the three strings the
/// window manager reads at map time. Bundled so the tag ledger's
/// recursion and the rules do not thread three `&str`s apiece.
#[derive(Clone, Copy)]
struct Subject<'a> {
    class: &'a str,
    title: &'a str,
    /// The `xdg_toplevel_tag_v1` tag, empty for a window whose client
    /// never set one.
    xdg_tag: &'a str,
}

/// One resolved float rule: who it matches and what it says.
#[derive(Clone, Debug)]
struct Rule {
    class: Option<Pattern>,
    title: Option<Pattern>,
    xdg_tag: Option<Pattern>,
    /// The tags the rule matches on, every one of which the window
    /// must carry at this rule's position in the file.
    tags: Vec<String>,
    /// Where the rule sits in the file, for the tag question above.
    index: usize,
    float: Option<bool>,
    center: Option<bool>,
    /// Logical pixels, as Omarchy writes them. Scaled at the point of
    /// use, exactly as the hardcoded 875×600 always was.
    size: Option<Extent>,
    /// The frame's position relative to the monitor it maps on, in the
    /// same logical pixels. Evaluated after `size`, with the resolved
    /// size as `window_w`/`window_h`, which is how Omarchy's `move`
    /// expressions are written.
    position: Option<Extent>,
    idle_inhibit: Option<wm_core::IdleInhibitRule>,
    pin: Option<bool>,
    no_focus: Option<bool>,
    no_initial_focus: Option<bool>,
    focus_on_activate: Option<bool>,
    fullscreen: Option<bool>,
    maximize: Option<bool>,
    suppress_maximize: Option<bool>,
    suppress_fullscreen: Option<bool>,
    /// The touchpad scroll factor over this window.
    scroll_touchpad: Option<f64>,
    /// The workspace to map onto.
    workspace: Option<RuleWorkspace>,
    opacity: Option<wm_core::OpacityRule>,
    no_dim: Option<bool>,
}

impl Rule {
    fn matches(&self, tags: &TagLedger, subject: Subject<'_>) -> bool {
        // A rule with no matcher at all matches everything — which is
        // what `o.window(".*", …)` means and is why Omarchy's
        // `suppress_event` and `tag +default-opacity` rules are written
        // that way. Correct, and harmless here: neither carries a
        // float property.
        self.class.as_ref().is_none_or(|p| p.matches(subject.class))
            && self.title.as_ref().is_none_or(|p| p.matches(subject.title))
            && self.xdg_tag.as_ref().is_none_or(|p| p.matches(subject.xdg_tag))
            && self
                .tags
                .iter()
                .all(|tag| tags.carries(tag, self.index, subject, 0))
    }

    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = &self.class {
            parts.push(format!("class {}", p.source));
        }
        if let Some(p) = &self.title {
            parts.push(format!("title {}", p.source));
        }
        if let Some(p) = &self.xdg_tag {
            parts.push(format!("xdg_tag {}", p.source));
        }
        for tag in &self.tags {
            parts.push(format!("tag {tag}"));
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
    tags: TagLedger,
}

/// One `tag +name` or `tag -name` rule: who it applies to, and where
/// in the file it sits.
#[derive(Clone, Debug)]
struct TagEvent {
    index: usize,
    add: bool,
    class: Option<Pattern>,
    title: Option<Pattern>,
    xdg_tag: Option<Pattern>,
    /// The tags the rule itself matched on — the one level of
    /// chaining this reader follows.
    via: Vec<String>,
}

impl TagEvent {
    fn applies(&self, ledger: &TagLedger, subject: Subject<'_>, depth: u8) -> bool {
        self.class.as_ref().is_none_or(|p| p.matches(subject.class))
            && self.title.as_ref().is_none_or(|p| p.matches(subject.title))
            && self.xdg_tag.as_ref().is_none_or(|p| p.matches(subject.xdg_tag))
            && self
                .via
                .iter()
                .all(|tag| depth == 0 && ledger.carries(tag, self.index, subject, depth + 1))
    }
}

/// Every tag event in the configuration, by tag, in file order — the
/// first pass of [`compile`], kept so the rules can ask the tag
/// question per window rather than being expanded per carrier.
#[derive(Clone, Debug, Default)]
struct TagLedger(std::collections::BTreeMap<String, Vec<TagEvent>>);

impl TagLedger {
    /// Whether a window carries `tag` at rule position `at`.
    ///
    /// The last applying event before the position decides; when none
    /// precedes it, the last applying event anywhere in the file does.
    /// That is the state Hyprland's repeated rule passes converge on:
    /// tags persist on the window between passes, so a rule written
    /// above the line that adds its tag still sees the tag on the
    /// second pass, while a removal written between an add and the
    /// rule is honoured on every pass.
    fn carries(&self, tag: &str, at: usize, subject: Subject<'_>, depth: u8) -> bool {
        let Some(events) = self.0.get(tag) else {
            return false;
        };
        let mut before = None;
        let mut overall = None;
        for event in events {
            if !event.applies(self, subject, depth) {
                continue;
            }
            if event.index < at {
                before = Some(event.add);
            }
            overall = Some(event.add);
        }
        before.or(overall).unwrap_or(false)
    }
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
                if let Some(size) = &rule.size {
                    match size.constant_size() {
                        Some(size) => what.push(format!("{}x{}", size.w, size.h)),
                        None => what.push(format!("size {}", size.source)),
                    }
                }
                if let Some(position) = &rule.position {
                    what.push(format!("at {}", position.source));
                }
                if rule.center == Some(true) {
                    what.push("centered".to_string());
                }
                match rule.idle_inhibit {
                    Some(wm_core::IdleInhibitRule::Always) => what.push("idle inhibited".to_string()),
                    Some(wm_core::IdleInhibitRule::Focus) => what.push("idle inhibited while focused".to_string()),
                    Some(wm_core::IdleInhibitRule::Fullscreen) => {
                        what.push("idle inhibited while fullscreen".to_string())
                    }
                    Some(wm_core::IdleInhibitRule::None) | None => {}
                }
                if let Some(factor) = rule.scroll_touchpad {
                    what.push(format!("touchpad scroll x{factor}"));
                }
                if let Some(rule) = &rule.workspace {
                    let target = match &rule.target {
                        RuleWorkspaceTarget::Special(name) => format!("special:{name}"),
                        RuleWorkspaceTarget::Numbered(index) => (index + 1).to_string(),
                    };
                    what.push(format!("workspace {target}{}", if rule.silent { " silently" } else { "" }));
                }
                for (enabled, label) in [
                    (rule.pin, "pinned"),
                    (rule.no_focus, "never focused"),
                    (rule.no_initial_focus, "no initial focus"),
                    (rule.fullscreen, "fullscreen"),
                    (rule.maximize, "maximized"),
                    (rule.suppress_maximize, "client maximize ignored"),
                    (rule.suppress_fullscreen, "client fullscreen ignored"),
                ] {
                    if enabled == Some(true) {
                        what.push(label.to_string());
                    }
                }
                if rule.focus_on_activate == Some(false) {
                    what.push("activation cannot focus".to_string());
                }
                if let Some(opacity) = rule.opacity {
                    what.push(match opacity.fullscreen {
                        Some(fullscreen) => format!(
                            "opacity {}/{}/{fullscreen}",
                            opacity.active, opacity.inactive
                        ),
                        None => format!("opacity {}/{}", opacity.active, opacity.inactive),
                    });
                }
                if rule.no_dim == Some(true) {
                    what.push("never dimmed".to_string());
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
    fn decision_for(&self, class: &str, title: &str, xdg_tag: &str) -> Option<FloatDecision> {
        let subject = Subject { class, title, xdg_tag };
        let mut float = None;
        let mut center = None;
        let mut size = None;
        for rule in &self.rules {
            if !rule.matches(&self.tags, subject) {
                continue;
            }
            float = rule.float.or(float);
            center = rule.center.or(center);
            size = rule.size.as_ref().or(size);
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
            // A size that needs a monitor is answered by
            // `placement_for`, which has one.
            size: size.and_then(Extent::constant_size),
            center: center.unwrap_or(true),
        })
    }

    /// The same last-wins walk as [`Self::decision_for`], with a
    /// monitor to evaluate against. Size first, then position with the
    /// resolved size as the window's — so `(monitor_w-window_w-40)`
    /// means "40 from the right edge of the window this rule just
    /// sized", which is what Omarchy wrote.
    fn placement_for(&self, class: &str, title: &str, xdg_tag: &str, metrics: &RuleMetrics) -> Option<RulePlacement> {
        let subject = Subject { class, title, xdg_tag };
        let mut float = None;
        let mut center = None;
        let mut size = None;
        let mut position = None;
        for rule in &self.rules {
            if !rule.matches(&self.tags, subject) {
                continue;
            }
            float = rule.float.or(float);
            center = rule.center.or(center);
            size = rule.size.as_ref().or(size);
            position = rule.position.as_ref().or(position);
        }
        if float == Some(false) || (float.is_none() && size.is_none()) {
            return None;
        }
        let visual = |content: Size| Values {
            monitor_w: f64::from(metrics.monitor.w),
            monitor_h: f64::from(metrics.monitor.h),
            window_w: f64::from(content.w) + f64::from(metrics.chrome.w),
            window_h: f64::from(content.h) + f64::from(metrics.chrome.h),
        };
        let size = size.and_then(|extent| {
            let resolved = extent.eval(&visual(metrics.window)).and_then(|(w, h)| to_size(w, h));
            if resolved.is_none() {
                tracing::debug!(
                    rule = %extent.source,
                    ?metrics,
                    "window rule size did not evaluate to a usable size on this monitor: size ignored"
                );
            }
            resolved
        });
        // An explicit `center` is the stronger statement about where
        // the window goes; the default center (a float rule with no
        // position at all) is what `move` replaces.
        let position = position.filter(|_| center != Some(true)).and_then(|extent| {
            let resolved = extent
                .eval(&visual(size.unwrap_or(metrics.window)))
                .map(|(x, y)| Point::new(x.round() as i32, y.round() as i32));
            if resolved.is_none() {
                tracing::debug!(
                    rule = %extent.source,
                    ?metrics,
                    "window rule move did not evaluate to a position on this monitor: move ignored"
                );
            }
            resolved
        });
        Some(RulePlacement { size, position })
    }

    fn window_decision_for(&self, class: &str, title: &str, xdg_tag: &str) -> WindowRuleDecision {
        let subject = Subject { class, title, xdg_tag };
        let mut decision = WindowRuleDecision::for_identity(class);
        for rule in &self.rules {
            if !rule.matches(&self.tags, subject) {
                continue;
            }
            if let Some(value) = rule.idle_inhibit {
                decision.idle_inhibit = value;
            }
            if let Some(factor) = rule.scroll_touchpad {
                decision.touchpad_scroll_factor = Some(factor);
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
            if let Some(value) = rule.suppress_maximize {
                decision.suppress_maximize = value;
            }
            if let Some(value) = rule.suppress_fullscreen {
                decision.suppress_fullscreen = value;
            }
            if let Some(value) = &rule.workspace {
                decision.workspace = Some(value.clone());
            }
            if let Some(value) = rule.opacity {
                decision.opacity = Some(value);
            }
            if let Some(value) = rule.no_dim {
                decision.no_dim = value;
            }
        }
        decision
    }
}

/// Turns the window rules read out of a config into float rules,
/// resolving tags and reporting everything it declines.
///
/// Returns the rules and the lines to log — the caller owns logging so
/// that this function stays pure and testable, which is the same split
/// `startup.rs` makes between its `read_*` and `resolve_*` halves.
pub fn compile(rules: &[WindowRule]) -> (FloatRules, Vec<String>) {
    let mut notes = Vec::new();
    // Pass one: every tag event, in file order.
    let mut ledger = TagLedger::default();
    for (index, rule) in rules.iter().enumerate() {
        for (name, value) in &rule.props {
            if name != "tag" {
                continue;
            }
            let (add, tag) = match (value.strip_prefix('+'), value.strip_prefix('-')) {
                (Some(tag), _) => (true, tag),
                (_, Some(tag)) => (false, tag),
                // Hyprland reads a bare name as an add.
                _ => (true, value.as_str()),
            };
            let matchers = split_matchers(&rule.matchers);
            if let Some(refused) = matchers.refused {
                notes.push(format!(
                    "window rule tagging {tag} carries {refused}, which this reader does not implement: rule skipped"
                ));
                continue;
            }
            let via = tag_matchers(rule);
            let (Some(class), Some(title), Some(xdg_tag)) = (
                compile_pattern(matchers.class, "class", &mut notes),
                compile_pattern(matchers.title, "title", &mut notes),
                compile_pattern(matchers.xdg_tag, "xdg_tag", &mut notes),
            ) else {
                continue;
            };
            ledger
                .0
                .entry(tag.to_string())
                .or_default()
                .push(TagEvent { index, add, class, title, xdg_tag, via });
        }
    }
    // A tag event that hangs on another tag is followed exactly one
    // level: the tag it hangs on has to be carried by class, title or
    // xdg tag.
    // Deciding that needs the whole ledger, so it is a second look.
    let direct: std::collections::BTreeSet<String> = ledger
        .0
        .iter()
        .filter(|(_, events)| events.iter().all(|event| event.via.is_empty()))
        .map(|(tag, _)| tag.clone())
        .collect();
    for (tag, events) in ledger.0.iter_mut() {
        events.retain(|event| {
            for via in &event.via {
                if !direct.contains(via) {
                    notes.push(format!(
                        "window rule tags {tag} based on tag {via}, which is not carried by class, title or xdg_tag: chained tags are not followed"
                    ));
                    return false;
                }
            }
            true
        });
    }
    ledger.0.retain(|_, events| !events.is_empty());
    // Pass two: the rules that actually say something about the window.
    let mut out = FloatRules::default();
    for (index, rule) in rules.iter().enumerate() {
        let Some(spec) = rule_spec(rule, &mut notes) else {
            continue;
        };
        let matchers = split_matchers(&rule.matchers);
        if let Some(refused) = matchers.refused {
            notes.push(format!(
                "float rule carries {refused}, which this reader does not implement: rule skipped"
            ));
            continue;
        }
        let tags = tag_matchers(rule);
        if let Some(tag) = tags.iter().find(|tag| !ledger.0.contains_key(*tag)) {
            notes.push(format!(
                "float rule matches tag {tag}, which no rule in this configuration adds: rule skipped"
            ));
            continue;
        }
        push(&mut out, &mut notes, matchers, tags, index, &spec);
    }
    out.tags = ledger;
    (out, notes)
}

/// The tags a rule matches on.
fn tag_matchers(rule: &WindowRule) -> Vec<String> {
    rule.matchers
        .iter()
        .filter_map(|m| match m {
            Matcher::Tag(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// Compiles an optional matcher pattern: `Some(None)` for no pattern,
/// `None` (with a note) for one that does not compile.
fn compile_pattern(
    text: Option<String>,
    what: &str,
    notes: &mut Vec<String>,
) -> Option<Option<Pattern>> {
    match text {
        Some(text) => match Pattern::compile(&text) {
            Some(pattern) => Some(Some(pattern)),
            None => {
                notes.push(format!(
                    "float rule has an unreadable {what} pattern {text:?}: rule skipped"
                ));
                None
            }
        },
        None => Some(None),
    }
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
            // `size 875 600`, or `size (monitor_h*4/25) (monitor_h*9/50)`:
            // the webcam overlay sizes itself off the monitor, which is
            // compiled here and evaluated when the window maps.
            "size" => match Extent::parse(value) {
                Ok(extent) if !extent.is_constant() || extent.constant_size().is_some() => {
                    spec.size = Some(extent);
                    any = true;
                }
                Ok(_) => notes.push(format!(
                    "window rule size {value:?} on {} is not a positive size: property skipped",
                    describe_matchers(rule)
                )),
                Err(why) => notes.push(format!(
                    "window rule size {value:?} on {}: {why}: property skipped",
                    describe_matchers(rule)
                )),
            },
            // `move (monitor_w-window_w-40) (monitor_h*0.04)`: the
            // frame's position on the monitor, relative to its origin.
            // Hyprland's other spellings — `cursor`, percentages,
            // `onscreen` — are not read, and say so.
            "move" => match Extent::parse(value) {
                Ok(extent) => {
                    spec.position = Some(extent);
                    any = true;
                }
                Err(why) => notes.push(format!(
                    "window rule move {value:?} on {}: {why}: property skipped",
                    describe_matchers(rule)
                )),
            },
            "idle_inhibit" | "idleinhibit" => match idle_inhibit_mode(value) {
                Some(mode) => {
                    spec.idle_inhibit = Some(mode);
                    any = true;
                }
                None => notes.push(format!(
                    "window rule idle_inhibit {value:?} on {} is not one of none, always, focus or fullscreen: property skipped",
                    describe_matchers(rule)
                )),
            },
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
            // A space-separated list of the client requests to ignore.
            // Each event is read or reported on its own.
            "suppress_event" | "suppressevent" => {
                for event in value.split([' ', ',']).filter(|event| !event.is_empty()) {
                    match event.to_ascii_lowercase().as_str() {
                        "maximize" => spec.suppress_maximize = Some(true),
                        "fullscreen" => spec.suppress_fullscreen = Some(true),
                        "activate" | "activatefocus" => spec.focus_on_activate = Some(false),
                        _ => {
                            notes.push(format!(
                                "window rule suppress_event {event:?} on {} is not implemented: event not suppressed",
                                describe_matchers(rule)
                            ));
                            continue;
                        }
                    }
                    any = true;
                }
            }
            // `workspace N`, `workspace special`, `workspace
            // special:NAME`, each with an optional `silent`. Named
            // workspaces (`name:…`) and the relative forms are refused
            // by name: this desktop's workspaces are numbered, and a
            // rule is read long before the row it would be relative to
            // exists.
            "workspace" => match workspace_rule(value) {
                Ok(rule) => {
                    spec.workspace = Some(rule);
                    any = true;
                }
                Err(why) => notes.push(format!(
                    "window rule workspace {value:?} on {} {why}: property skipped",
                    describe_matchers(rule)
                )),
            },
            "scroll_touchpad" | "scrolltouchpad" => match value.trim().parse::<f64>() {
                Ok(factor) if factor.is_finite() && (0.01..=10.0).contains(&factor) => {
                    spec.scroll_touchpad = Some(factor);
                    any = true;
                }
                _ => notes.push(format!(
                    "window rule scroll_touchpad {value:?} on {} must be a number from 0.01 to 10: property skipped",
                    describe_matchers(rule)
                )),
            },
            // `a`, `a b` or `a b c`: the body alpha when focused,
            // unfocused and fullscreen. Hyprland also accepts an
            // `override` word, which says the value beats the
            // window's own opacity request; every rule value already
            // does here, so the word is accepted and means nothing.
            "opacity" => match opacity_rule(value) {
                Some(rule) => {
                    spec.opacity = Some(rule);
                    any = true;
                }
                None => notes.push(format!(
                    "window rule opacity {value:?} on {} must be one to three numbers from 0 to 1: property skipped",
                    describe_matchers(rule)
                )),
            },
            "no_dim" | "nodim" => {
                spec.no_dim = Some(truthy(value));
                any = true;
            }
            // `tag +name` is consumed by compile's first pass. A tag
            // matcher likewise participates in resolution, so neither
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
    size: Option<Extent>,
    position: Option<Extent>,
    idle_inhibit: Option<wm_core::IdleInhibitRule>,
    pin: Option<bool>,
    no_focus: Option<bool>,
    no_initial_focus: Option<bool>,
    focus_on_activate: Option<bool>,
    fullscreen: Option<bool>,
    maximize: Option<bool>,
    suppress_maximize: Option<bool>,
    suppress_fullscreen: Option<bool>,
    scroll_touchpad: Option<f64>,
    workspace: Option<RuleWorkspace>,
    opacity: Option<wm_core::OpacityRule>,
    no_dim: Option<bool>,
}

/// Reads a `workspace` rule's value.
fn workspace_rule(value: &str) -> Result<RuleWorkspace, String> {
    let mut words = value.split_whitespace();
    let target = words.next().ok_or_else(|| "names no workspace".to_string())?;
    let mut silent = false;
    for word in words {
        if word.eq_ignore_ascii_case("silent") {
            silent = true;
        } else {
            return Err(format!("carries {word:?}, which this reader does not implement"));
        }
    }
    let target = if target == "special" || target.starts_with("special:") {
        let name = wm_core::normalize_special_name(target)
            .ok_or_else(|| "names a special workspace this desktop will not create".to_string())?;
        RuleWorkspaceTarget::Special(name)
    } else if let Some(name) = target.strip_prefix("name:") {
        return Err(format!("names workspace {name:?}, and chonkstep workspaces are numbered, not named"));
    } else {
        // Digits only: `+1`, `-1` and `e+1` are relative to a row that
        // does not exist when a rule is read.
        let index = target
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| target.parse::<usize>().ok())
            .flatten()
            .filter(|index| (1..=crate::MAX_WORKSPACE).contains(index))
            .ok_or_else(|| format!("must be 1 to {}, special or special:NAME", crate::MAX_WORKSPACE))?;
        RuleWorkspaceTarget::Numbered(index - 1)
    };
    Ok(RuleWorkspace { target, silent })
}

/// Reads an `opacity` value: one to three finite numbers, each clamped
/// to `0.0..=1.0`. A single number sets both focus states.
fn opacity_rule(value: &str) -> Option<wm_core::OpacityRule> {
    let mut numbers = Vec::new();
    for word in value.split_whitespace() {
        if word.eq_ignore_ascii_case("override") {
            continue;
        }
        let number: f32 = word.parse().ok()?;
        if !number.is_finite() {
            return None;
        }
        numbers.push(number.clamp(0.0, 1.0));
    }
    match numbers.as_slice() {
        [alpha] => Some(wm_core::OpacityRule { active: *alpha, inactive: *alpha, fullscreen: None }),
        [active, inactive] => Some(wm_core::OpacityRule { active: *active, inactive: *inactive, fullscreen: None }),
        [active, inactive, fullscreen] => Some(wm_core::OpacityRule {
            active: *active,
            inactive: *inactive,
            fullscreen: Some(*fullscreen),
        }),
        _ => None,
    }
}

fn push(
    out: &mut FloatRules,
    notes: &mut Vec<String>,
    matchers: Matchers,
    tags: Vec<String>,
    index: usize,
    spec: &Spec,
) {
    let Some(class) = compile_pattern(matchers.class, "class", notes) else {
        return;
    };
    let Some(title) = compile_pattern(matchers.title, "title", notes) else {
        return;
    };
    let Some(xdg_tag) = compile_pattern(matchers.xdg_tag, "xdg_tag", notes) else {
        return;
    };
    out.rules.push(Rule {
        class,
        title,
        xdg_tag,
        tags,
        index,
        float: spec.float,
        center: spec.center,
        size: spec.size.clone(),
        position: spec.position.clone(),
        idle_inhibit: spec.idle_inhibit,
        pin: spec.pin,
        no_focus: spec.no_focus,
        no_initial_focus: spec.no_initial_focus,
        focus_on_activate: spec.focus_on_activate,
        fullscreen: spec.fullscreen,
        maximize: spec.maximize,
        suppress_maximize: spec.suppress_maximize,
        suppress_fullscreen: spec.suppress_fullscreen,
        scroll_touchpad: spec.scroll_touchpad,
        workspace: spec.workspace.clone(),
        opacity: spec.opacity,
        no_dim: spec.no_dim,
    });
}

fn describe_matchers(rule: &WindowRule) -> String {
    let parts: Vec<String> = rule
        .matchers
        .iter()
        .map(|matcher| match matcher {
            Matcher::Class(value) => format!("match:class {value}"),
            Matcher::Title(value) => format!("match:title {value}"),
            Matcher::XdgTag(value) => format!("match:xdg_tag {value}"),
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

/// The pattern matchers of one rule, still as text, as
/// [`split_matchers`] sorts them.
#[derive(Default)]
struct Matchers {
    class: Option<String>,
    title: Option<String>,
    xdg_tag: Option<String>,
    /// The first matcher this reader does not implement, named for
    /// the log; a rule carrying one is refused whole.
    refused: Option<String>,
}

/// Splits a rule's matchers into the class, title and xdg tag
/// patterns this reader implements, and the name of the first one it
/// does not.
fn split_matchers(matchers: &[Matcher]) -> Matchers {
    let mut out = Matchers::default();
    for matcher in matchers {
        match matcher {
            Matcher::Class(pattern) => out.class = Some(pattern.clone()),
            Matcher::Title(pattern) => out.title = Some(pattern.clone()),
            Matcher::XdgTag(pattern) => out.xdg_tag = Some(pattern.clone()),
            Matcher::Tag(_) => {}
            Matcher::Other { key, value } => {
                out.refused = Some(format!("match:{key} {value}"));
                return out;
            }
        }
    }
    out
}

/// The `idle_inhibit` modes. The boolean spellings stay accepted, but
/// only as themselves: `fullscreen` is a condition, not a truthy word.
fn idle_inhibit_mode(value: &str) -> Option<wm_core::IdleInhibitRule> {
    use wm_core::IdleInhibitRule;
    match value.trim().to_ascii_lowercase().as_str() {
        "always" | "on" | "true" | "1" | "yes" => Some(IdleInhibitRule::Always),
        "focus" => Some(IdleInhibitRule::Focus),
        "fullscreen" => Some(IdleInhibitRule::Fullscreen),
        "none" | "off" | "false" | "0" | "no" => Some(IdleInhibitRule::None),
        _ => None,
    }
}

/// Hyprland's spelling of a boolean rule property.
fn truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "off" | "0" | "false" | "no"
    )
}
