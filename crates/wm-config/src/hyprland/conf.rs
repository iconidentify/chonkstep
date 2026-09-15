//! The classic `hyprland.conf` syntax, for the machines that still
//! have it.
//!
//! Omarchy 4 moved to Lua, but three kinds of machine still put this
//! syntax in front of us and each is a real user: an Omarchy 3 install
//! that has not upgraded; a machine mid-upgrade, where the `.conf`
//! tree is still on disk beside the new `.lua` one (this is exactly
//! what the development machine looked like — the shipped defaults were
//! Lua while the user's own bindings were still in a `.conf` the
//! migration had not moved); and anyone who wrote a `hyprland.conf` by
//! hand from the upstream wiki, which is what the wiki still documents.
//!
//! It is a far smaller job than the Lua reader. The syntax is
//! line-oriented: `keyword = value`, with `name { … }` blocks around
//! groups of settings and `$name = …` variables substituted textually.
//! There is no control flow, so there is nothing to evaluate — which
//! is why this file is a fifth the size of `lua.rs` and does the same
//! work.
//!
//! # The three window-rule syntaxes
//!
//! Hyprland has changed this syntax twice and a real machine has all
//! three spellings on it:
//!
//! ```text
//! windowrule   = float, ^(steam)$                          # v1
//! windowrulev2 = float, class:^(steam)$, title:^(Steam)$   # v2
//! windowrule   = float on, match:class steam               # 0.53+
//! ```
//!
//! All three are read, because "your rules stopped working when you
//! upgraded Hyprland" is precisely the kind of silent breakage this
//! whole module exists to avoid. They are distinguished by shape
//! rather than by keyword, since 0.53 reused the `windowrule` keyword
//! for the new form.

use super::directive::{BindFlags, Directive, Dispatcher, Include, Matcher, Monitor, WindowRule};

/// Reads one `.conf` file's text into directives.
///
/// `vars` carries `$name` definitions across the file graph the way
/// Hyprland's own do: a variable set in `hyprland.conf` before a
/// `source =` is visible inside the sourced file.
pub fn read(
    source: &str,
    vars: &mut std::collections::BTreeMap<String, String>,
    out: &mut Vec<Directive>,
) {
    // Depth of `name { … }` block nesting. Everything inside a block is
    // a setting for a Hyprland subsystem this desktop does not have —
    // `general`, `decoration`, `layout` — so blocks are skipped whole
    // rather than half-read. Their contents are reported once, by name,
    // on the way in. `input`, `cursor`, `device` and `animations` are
    // the blocks read line by line.
    let mut block: Vec<String> = Vec::new();
    // Hyprland submaps are modal scopes, not annotations on the next
    // line. Keep the scope until `submap = reset`; otherwise a bare
    // binding inside the canonical resize submap becomes a global grab
    // here, including ordinary typing keys such as `1`.
    let mut submap: Option<String> = None;
    let mut device: Option<DeviceBlock> = None;
    for raw in source.lines() {
        let stripped = strip_comment(raw);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }
        if line == "}" {
            let closed = block.pop();
            if block.is_empty() && closed.is_some_and(|name| name.eq_ignore_ascii_case("device")) {
                if let Some(DeviceBlock { name, settings }) = device.take() {
                    out.push(match name {
                        Some(name) => Directive::Device { name, settings },
                        None => Directive::Ignored { kind: "device", detail: "device { … } with no name".into() },
                    });
                }
            }
            continue;
        }
        if let Some(name) = line.strip_suffix('{') {
            let name = name.trim();
            if block.is_empty() && name.eq_ignore_ascii_case("device") {
                device = Some(DeviceBlock::default());
            } else if block.is_empty() {
                if name.eq_ignore_ascii_case("general") {
                    out.push(Directive::Ignored {
                        kind: "block",
                        detail: format!("{name} {{ … }}: only layout is read; the rest is Hyprland's look"),
                    });
                } else if !name.eq_ignore_ascii_case("input")
                    && !name.eq_ignore_ascii_case("cursor")
                    && !name.eq_ignore_ascii_case("binds")
                    && !name.eq_ignore_ascii_case("misc")
                    && !name.eq_ignore_ascii_case("animations")
                    && !name.eq_ignore_ascii_case("decoration")
                {
                    out.push(Directive::Ignored {
                        kind: "block",
                        detail: format!("{name} {{ … }}: a Hyprland subsystem this desktop has its own answer for"),
                    });
                }
            } else if block
                .first()
                .is_some_and(|root| root.eq_ignore_ascii_case("input"))
                && !name.eq_ignore_ascii_case("touchpad")
            {
                out.push(Directive::Ignored {
                    kind: "input",
                    detail: format!("nested input block {name} {{ … }} is not implemented"),
                });
            } else if block.first().is_some_and(|root| root.eq_ignore_ascii_case("cursor")) {
                out.push(Directive::Ignored {
                    kind: "cursor",
                    detail: format!("nested cursor block {name} {{ … }} is not implemented"),
                });
            } else if block.first().is_some_and(|root| root.eq_ignore_ascii_case("binds")) {
                out.push(Directive::Ignored {
                    kind: "binds",
                    detail: format!("nested binds block {name} {{ … }} is not implemented"),
                });
            } else if block.first().is_some_and(|root| root.eq_ignore_ascii_case("misc")) {
                out.push(Directive::Ignored {
                    kind: "misc",
                    detail: format!("nested misc block {name} {{ … }} is not implemented"),
                });
            } else if block.first().is_some_and(|root| root.eq_ignore_ascii_case("decoration")) {
                // `blur { … }`, `shadow { … }`: Hyprland's look, which
                // the top-level skip already names.
            } else if block.first().is_some_and(|root| root.eq_ignore_ascii_case("device")) {
                out.push(Directive::Ignored {
                    kind: "device",
                    detail: format!("nested device block {name} {{ … }} is not implemented"),
                });
            }
            block.push(name.to_string());
            continue;
        }
        if !block.is_empty() {
            if block.len() == 1 && block[0].eq_ignore_ascii_case("device") {
                match (line.split_once('='), device.as_mut()) {
                    (Some((key, value)), Some(DeviceBlock { name, settings })) => {
                        let key = key.trim().to_ascii_lowercase();
                        let value = substitute(value.trim(), vars);
                        if key == "name" {
                            *name = Some(value);
                        } else {
                            settings.push((key, value));
                        }
                    }
                    _ => out.push(Directive::Ignored {
                        kind: "device",
                        detail: truncate(line),
                    }),
                }
            } else if block.len() == 1 && block[0].eq_ignore_ascii_case("cursor") {
                match line.split_once('=') {
                    Some((name, value)) => out.push(Directive::Cursor {
                        name: name.trim().to_ascii_lowercase(),
                        value: substitute(value.trim(), vars),
                    }),
                    None => out.push(Directive::Ignored {
                        kind: "cursor",
                        detail: truncate(line),
                    }),
                }
            } else if block.len() == 1 && block[0].eq_ignore_ascii_case("binds") {
                match line.split_once('=') {
                    Some((name, value)) => out.push(Directive::Binds {
                        name: name.trim().to_ascii_lowercase(),
                        value: substitute(value.trim(), vars),
                    }),
                    None => out.push(Directive::Ignored {
                        kind: "binds",
                        detail: truncate(line),
                    }),
                }
            } else if block.len() == 1 && block[0].eq_ignore_ascii_case("misc") {
                match line.split_once('=') {
                    Some((name, value)) => out.push(Directive::Misc {
                        name: name.trim().to_ascii_lowercase(),
                        value: substitute(value.trim(), vars),
                    }),
                    None => out.push(Directive::Ignored {
                        kind: "misc",
                        detail: truncate(line),
                    }),
                }
            } else if block.len() == 1 && block[0].eq_ignore_ascii_case("animations") {
                match line.split_once('=') {
                    Some((name, value)) => {
                        animation_setting(name.trim(), &substitute(value.trim(), vars), out)
                    }
                    None => out.push(Directive::Ignored {
                        kind: "animation",
                        detail: truncate(line),
                    }),
                }
            } else if block[0].eq_ignore_ascii_case("decoration") {
                // Only the two dim keys, and only at the top of the
                // block; the rest is Hyprland's look, named once as
                // declined rather than once per line.
                match line.split_once('=') {
                    Some((name, value))
                        if block.len() == 1
                            && matches!(
                                name.trim().to_ascii_lowercase().as_str(),
                                "dim_inactive" | "dim_strength"
                            ) =>
                    {
                        out.push(Directive::Decoration {
                            name: name.trim().to_ascii_lowercase(),
                            value: substitute(value.trim(), vars),
                        })
                    }
                    _ => {}
                }
            } else if block[0].eq_ignore_ascii_case("input") {
                match line.split_once('=') {
                    Some((name, value)) => out.push(Directive::Input {
                        name: if block.len() == 2 && block[1].eq_ignore_ascii_case("touchpad") {
                            format!("touchpad:{}", name.trim().to_ascii_lowercase())
                        } else {
                            name.trim().to_ascii_lowercase()
                        },
                        value: substitute(value.trim(), vars),
                    }),
                    None => out.push(Directive::Ignored {
                        kind: "input",
                        detail: truncate(line),
                    }),
                }
            } else if block.len() == 1 && block[0].eq_ignore_ascii_case("general") {
                // The one `general` key that is not Hyprland's look:
                // whether windows tile at all.
                if let Some((name, value)) = line.split_once('=') {
                    if name.trim().eq_ignore_ascii_case("layout") {
                        out.push(Directive::DefaultLayout { layout: substitute(value.trim(), vars) });
                    }
                }
            }
            continue;
        }
        let Some((keyword, value)) = line.split_once('=') else {
            out.push(Directive::Ignored {
                kind: "syntax",
                detail: truncate(line),
            });
            continue;
        };
        let keyword = keyword.trim();
        let value = substitute(value.trim(), vars);
        // `$name = value`: Hyprland's variables, textually substituted
        // into every later line. Defined here rather than skipped
        // because a hand-written config's every binding goes through
        // `$mainMod`.
        if let Some(name) = keyword.strip_prefix('$') {
            vars.insert(name.trim().to_string(), value);
            continue;
        }
        if keyword.eq_ignore_ascii_case("submap") {
            let name = value.trim();
            submap =
                (!name.eq_ignore_ascii_case("reset") && !name.is_empty()).then(|| name.to_string());
            continue;
        }
        let lower = keyword.to_ascii_lowercase();
        if let Some(name) = &submap {
            if lower
                .strip_prefix("bind")
                .is_some_and(|flags| flags.chars().all(|flag| "dlernmicops".contains(flag)))
            {
                let fields: Vec<&str> = value.splitn(3, ',').collect();
                let chord = if fields.len() >= 2 {
                    format!("{} {}", fields[0].trim(), fields[1].trim())
                        .trim()
                        .to_string()
                } else {
                    truncate(&value)
                };
                out.push(Directive::Ignored {
                    kind: "submap-bind",
                    detail: format!(
                        "{chord} in submap {name:?}: scoped submap bindings are unsupported and were not made global"
                    ),
                });
                continue;
            }
        }
        directive(keyword, &value, out);
    }
}

/// One line of `animations { … }`, or a bare `animation =` / `bezier =`.
/// `enabled` is the global switch; `animation = NAME, ONOFF, SPEED,
/// CURVE[, STYLE]` carries its switch under its leaf name and declines
/// the rest of the line by that name; `bezier` defines a curve this
/// desktop has no use for.
fn animation_setting(key: &str, value: &str, out: &mut Vec<Directive>) {
    let lower = key.to_ascii_lowercase();
    match lower.as_str() {
        "enabled" => out.push(match toggle(value) {
            Some(enabled) => Directive::Animation { leaf: "global".into(), enabled },
            None => Directive::Ignored {
                kind: "animation",
                detail: format!("animations:enabled = {}: not a boolean", truncate(value)),
            },
        }),
        "animation" => {
            let mut fields = value.split(',').map(str::trim);
            match (fields.next(), fields.next().and_then(toggle)) {
                (Some(leaf), Some(enabled)) if !leaf.is_empty() => {
                    out.push(Directive::Animation { leaf: leaf.to_string(), enabled });
                    if fields.next().is_some() {
                        out.push(Directive::Ignored {
                            kind: "animation",
                            detail: format!(
                                "animation = {leaf}, …: speed, curve and style not applied; this desktop's motion is one spring whose speed is [motion] speed"
                            ),
                        });
                    }
                }
                _ => out.push(Directive::Ignored {
                    kind: "animation",
                    detail: format!("animation = {}: no readable leaf and on/off switch", truncate(value)),
                }),
            }
        }
        "bezier" => out.push(Directive::Ignored {
            kind: "animation",
            detail: format!(
                "bezier = {}: Hyprland's animation curves; this desktop's motion is one spring",
                truncate(value)
            ),
        }),
        _ => out.push(Directive::Ignored {
            kind: "animation",
            detail: format!(
                "animations:{key} = {}: Hyprland's; only enabled and the animation switches are read",
                truncate(value)
            ),
        }),
    }
}

/// A Hyprland boolean, in the spellings its config accepts.
fn toggle(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// The `device { … }` block being read. A block may give its `name` after
/// its settings, so the directive is emitted only when the block closes.
#[derive(Default)]
struct DeviceBlock {
    name: Option<String>,
    settings: Vec<(String, String)>,
}

fn directive(keyword: &str, value: &str, out: &mut Vec<Directive>) {
    let lower = keyword.to_ascii_lowercase();
    // The `bind` family. Hyprland spells its flags as suffix letters —
    // `bindd`, `bindl`, `binde`, `bindld`, `bindm` — and the only one
    // that changes the shape of the line is `d`, which inserts a
    // description field. `m` is a mouse binding, which cannot become a
    // key chord; the rest (locked, repeating, release, non-consuming)
    // are behavioural and this desktop does not implement them yet, so
    // the binding is taken and the flag is dropped. That is a real
    // difference and it is written down in the docs rather than here.
    if let Some(flags) = lower.strip_prefix("bind") {
        if flags.chars().all(|c| "dlernmicops".contains(c)) {
            return bind(flags, value, out);
        }
    }
    match lower.as_str() {
        "unbind" => out.push(Directive::Unbind {
            keys: value.replace(',', " ").trim().to_string(),
        }),
        "env" | "envd" => match value.split_once(',') {
            Some((name, val)) => out.push(Directive::Env {
                name: name.trim().to_string(),
                value: val.trim().to_string(),
            }),
            None => out.push(Directive::Ignored {
                kind: "env",
                detail: truncate(value),
            }),
        },
        "exec-once" => out.push(Directive::ExecOnce {
            command: value.to_string(),
        }),
        // `exec` re-runs on every config reload, which under this
        // desktop would mean on every poll of the watch. Taking it as
        // an autostart entry would start a second copy of whatever it
        // is each time the user edited their config through Omarchy's
        // menu, so it is refused, loudly, rather than approximated by
        // `exec-once`.
        "exec" | "execr" | "exec-shutdown" => out.push(Directive::Ignored {
            kind: "exec",
            detail: format!(
                "{keyword} re-runs on every reload; only exec-once becomes autostart ({})",
                truncate(value)
            ),
        }),
        "windowrule" | "windowrulev2" => out.push(Directive::WindowRule(window_rule(value))),
        "layerrule" => out.push(Directive::Ignored {
            kind: "layer-rule",
            detail: "layer-shell rules are Hyprland's; this compositor has its own".into(),
        }),
        "monitor" | "monitorv2" => out.push(Directive::Monitor(monitor(value))),
        "gesture" => out.push(Directive::Ignored {
            kind: "gesture",
            detail: truncate(value),
        }),
        "workspace" => workspace_rule(value, out),
        // The colon spelling of `general { layout = … }`.
        "general:layout" => out.push(Directive::DefaultLayout { layout: value.to_string() }),
        // The colon spelling of `misc { focus_on_activate = … }` and
        // `misc { disable_autoreload = … }`, the two `misc` keys with a
        // meaning here.
        "misc:focus_on_activate" | "misc:disable_autoreload" => out.push(Directive::Misc {
            name: keyword["misc:".len()..].to_string(),
            value: value.to_string(),
        }),
        // Handled by `read`, which must retain scope between lines.
        "submap" => {}
        // The file graph, emitted in place so the loader splices the
        // sourced file exactly where its line sat — which is what makes
        // "the user's file is read after the defaults" true.
        "source" => out.push(Directive::Include(Include::Path(value.to_string()))),
        // The animation switches work outside their block too, as they
        // do in Hyprland.
        "animation" | "bezier" => animation_setting(keyword, value, out),
        // Hyprland's own machinery is unsupported here, but it must be
        // reported like every other declined directive. Silence made a
        // plugin or debug setting look successfully applied.
        "plugin" | "blurls" | "debug" => out.push(Directive::Ignored {
            kind: "keyword",
            detail: format!("{keyword} = {} (Hyprland-only machinery)", truncate(value)),
        }),
        _ => out.push(Directive::Ignored {
            kind: "keyword",
            detail: format!("{keyword} = {}", truncate(value)),
        }),
    }
}

/// `workspace = N, layout:dwindle, gapsin:0, …`: Hyprland's workspace
/// rules. Only the layout of a numbered workspace is read. Every other
/// rule earns its own line, and a selector that is not a number from 1
/// to 99 is reported with the reason.
fn workspace_rule(value: &str, out: &mut Vec<Directive>) {
    let mut fields = value.split(',').map(str::trim);
    let selector = fields.next().unwrap_or("");
    let mut layout = None;
    let mut reported = false;
    for rule in fields.filter(|rule| !rule.is_empty()) {
        match rule.split_once(':') {
            Some((key, value)) if key.trim().eq_ignore_ascii_case("layout") => {
                layout = Some(value.trim().to_string());
            }
            _ => {
                reported = true;
                out.push(Directive::Ignored {
                    kind: "workspace-rule",
                    detail: format!("workspace = {}, {}: only layout is read", truncate(selector), truncate(rule)),
                });
            }
        }
    }
    let Some(layout) = layout else {
        if !reported {
            out.push(Directive::Ignored {
                kind: "workspace-rule",
                detail: format!("workspace = {}: names no layout", truncate(value)),
            });
        }
        return;
    };
    match super::directive::workspace_number(selector) {
        Ok(workspace) => out.push(Directive::WorkspaceLayout { workspace, layout }),
        Err(why) => out.push(Directive::Ignored {
            kind: "workspace-rule",
            detail: format!("workspace = {}, layout:{}: {why}", truncate(selector), truncate(&layout)),
        }),
    }
}

/// `bind[flags] = MODS, KEY[, DESCRIPTION], dispatcher[, args]`.
fn bind(flags: &str, value: &str, out: &mut Vec<Directive>) {
    let described = flags.contains('d');
    // Split into at most the fields the shape needs, so a dispatcher
    // argument containing commas (`resizeactive, 100 0` does not, but
    // `exec, foo --a,b` can) survives intact in the last field.
    let fields: Vec<&str> = value.splitn(if described { 5 } else { 4 }, ',').collect();
    let want = if described { 4 } else { 3 };
    if fields.len() < want {
        out.push(Directive::Ignored {
            kind: "bind",
            detail: format!("too few fields: {}", truncate(value)),
        });
        return;
    }
    // A mouse binding cannot be a key chord. Caught here as well as in
    // `super::keys` so the diagnostic names the flag the user wrote.
    if flags.contains('m') {
        out.push(Directive::Ignored {
            kind: "bind",
            detail: format!("bind{flags} is a mouse binding: {}", truncate(value)),
        });
        return;
    }
    let keys = format!("{} {}", fields[0].trim(), fields[1].trim());
    let description = described
        .then(|| fields[2].trim().to_string())
        .filter(|d| !d.is_empty() && d != "nil");
    let (name, arg) = if described {
        (
            fields[3].trim(),
            fields.get(4).map(|a| a.trim()).unwrap_or(""),
        )
    } else {
        (
            fields[2].trim(),
            fields.get(3).map(|a| a.trim()).unwrap_or(""),
        )
    };
    let dispatcher = if name.eq_ignore_ascii_case("exec") {
        Dispatcher::Exec(arg.to_string())
    } else {
        Dispatcher::Verb {
            name: name.to_string(),
            arg: arg.to_string(),
        }
    };
    out.push(Directive::Bind {
        keys: keys.trim().to_string(),
        description,
        flags: BindFlags {
            locked: flags.contains('l'),
            repeating: flags.contains('e'),
            release: flags.contains('r'),
        },
        dispatcher,
    });
}

/// One window rule, in whichever of the three syntaxes it is written.
fn window_rule(value: &str) -> WindowRule {
    let mut rule = WindowRule::default();
    let mut fields = value.split(',').map(str::trim);
    let Some(head) = fields.next() else {
        return rule;
    };
    // The property. `float on` and `size 875 600` split name from
    // value at the first space; a bare `float` (v1 and v2) is `on`.
    let (name, val) = head.split_once(char::is_whitespace).unwrap_or((head, "on"));
    rule.props
        .push((name.trim().to_ascii_lowercase(), val.trim().to_string()));
    for field in fields {
        if field.is_empty() {
            continue;
        }
        // 0.53+: `match:class X`. The key and value are space-separated
        // inside one comma-delimited field.
        if let Some(rest) = field.strip_prefix("match:") {
            let (key, val) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            rule.matchers.push(matcher(key, val.trim()));
            continue;
        }
        // v2: `class:^(steam)$`, colon-separated.
        if let Some((key, val)) = field.split_once(':') {
            // A regex may itself contain a colon, so a "key" that is
            // not one of Hyprland's matcher names means this was really
            // a v1 bare class pattern that happened to contain one.
            if is_matcher_key(key) {
                rule.matchers.push(matcher(key, val.trim()));
                continue;
            }
        }
        // v1: a bare regular expression, matched against the class.
        rule.matchers.push(Matcher::Class(field.to_string()));
    }
    rule
}

fn is_matcher_key(key: &str) -> bool {
    matches!(
        key.trim().to_ascii_lowercase().as_str(),
        "class"
            | "initialclass"
            | "title"
            | "initialtitle"
            | "tag"
            | "xdgtag"
            | "xdg_tag"
            | "xwayland"
            | "floating"
            | "float"
            | "fullscreen"
            | "pinned"
            | "pin"
            | "focus"
            | "workspace"
            | "onworkspace"
            | "content"
            | "fullscreenstate"
            | "monitor"
    )
}

fn matcher(key: &str, value: &str) -> Matcher {
    match key.trim().to_ascii_lowercase().as_str() {
        // `initialclass`/`initialtitle` match the identity a window had
        // when it mapped, which is the only identity this desktop's
        // float rules are ever consulted at — so they are the same
        // matcher here, deliberately.
        "class" | "initialclass" => Matcher::Class(value.to_string()),
        "title" | "initialtitle" => Matcher::Title(value.to_string()),
        "tag" => Matcher::Tag(value.to_string()),
        // `xdgTag:` in the v2 syntax, `match:xdg_tag` in 0.53's; the
        // key has been lowercased by now.
        "xdgtag" | "xdg_tag" => Matcher::XdgTag(value.to_string()),
        other => Matcher::Other {
            key: other.to_string(),
            value: value.to_string(),
        },
    }
}

/// `monitor = NAME, MODE, POSITION, SCALE[, extra…]`.
fn monitor(value: &str) -> Monitor {
    let fields: Vec<String> = value.split(',').map(|f| f.trim().to_string()).collect();
    let at = |i: usize| fields.get(i).cloned().unwrap_or_default();
    Monitor {
        output: at(0),
        mode: at(1),
        position: at(2),
        scale: at(3),
        extra: fields.iter().skip(4).cloned().collect(),
    }
}

/// Everything from an unquoted `#` to the end of the line.
///
/// Hyprland's own escape for a literal `#` is `##`, which Omarchy's
/// user template calls out by name because their web-app bindings
/// contain URLs with fragments in them. Honoured here for the same
/// reason: a comment stripper that ate half a URL would silently
/// rewrite a binding.
fn strip_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '#' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'#') {
            chars.next();
            out.push('#');
            continue;
        }
        break;
    }
    out
}

/// `$name` substitution, longest name first so `$mainModShift` is not
/// eaten by a `$mainMod` that is also defined.
fn substitute(value: &str, vars: &std::collections::BTreeMap<String, String>) -> String {
    if !value.contains('$') || vars.is_empty() {
        return value.to_string();
    }
    let mut names: Vec<&String> = vars.keys().collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let mut out = value.to_string();
    for name in names {
        if let Some(replacement) = vars.get(name) {
            // Bounded: a variable whose value names another variable
            // is not expanded again, so a self-referential definition
            // cannot loop.
            out = out.replace(&format!("${name}"), replacement);
        }
    }
    out
}

fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() > 100 {
        text.chars().take(100).collect::<String>() + "…"
    } else {
        text.to_string()
    }
}
