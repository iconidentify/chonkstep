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
    // `general`, `decoration`, `input`, `animations` — so blocks are
    // skipped whole rather than half-read. Their contents are reported
    // once, by name, on the way in.
    let mut block: Vec<String> = Vec::new();
    // Hyprland submaps are modal scopes, not annotations on the next
    // line. Keep the scope until `submap = reset`; otherwise a bare
    // binding inside the canonical resize submap becomes a global grab
    // here, including ordinary typing keys such as `1`.
    let mut submap: Option<String> = None;
    for raw in source.lines() {
        let stripped = strip_comment(raw);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }
        if line == "}" {
            if let Some(name) = block.pop() {
                let _ = name;
            }
            continue;
        }
        if let Some(name) = line.strip_suffix('{') {
            let name = name.trim();
            if block.is_empty() {
                if !name.eq_ignore_ascii_case("input") {
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
            }
            block.push(name.to_string());
            continue;
        }
        if !block.is_empty() {
            if !block.is_empty() && block[0].eq_ignore_ascii_case("input") {
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
        "workspace" => out.push(Directive::Ignored {
            kind: "workspace-rule",
            detail: truncate(value),
        }),
        // Handled by `read`, which must retain scope between lines.
        "submap" => {}
        // The file graph, emitted in place so the loader splices the
        // sourced file exactly where its line sat — which is what makes
        // "the user's file is read after the defaults" true.
        "source" => out.push(Directive::Include(Include::Path(value.to_string()))),
        // Hyprland's own machinery is unsupported here, but it must be
        // reported like every other declined directive. Silence made a
        // plugin or debug setting look successfully applied.
        "plugin" | "bezier" | "animation" | "blurls" | "debug" => out.push(Directive::Ignored {
            kind: "keyword",
            detail: format!("{keyword} = {} (Hyprland-only machinery)", truncate(value)),
        }),
        _ => out.push(Directive::Ignored {
            kind: "keyword",
            detail: format!("{keyword} = {}", truncate(value)),
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
