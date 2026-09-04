//! Omarchy 4's Hyprland configuration is Lua. This reads it without
//! being a Lua interpreter.
//!
//! # What is actually in front of us
//!
//! `/usr/share/omarchy/default/hypr/**.lua` is not a data file that
//! happens to have Lua syntax. It is code: it loops to generate the
//! workspace bindings, branches on whether a tool is installed, defines
//! helper functions, and registers event callbacks. A regular
//! expression over it would be a lie, and running it would be worse —
//! this is somebody else's file, read at session startup, and executing
//! it would make a config edit a code-execution path into the window
//! manager. **Nothing in this module ever runs anything.**
//!
//! What it does instead is *parse* Lua's syntax into statements and
//! then evaluate the small, closed subset of expressions Omarchy's
//! configuration actually uses: string literals, numbers, booleans,
//! tables, concatenation, integer arithmetic, `tostring`, and a name
//! bound by `local` or by a numeric `for`. Every other expression is
//! [`Value::Opaque`] and every statement built on one is skipped with
//! a log line naming it. The result is that this reader understands
//! precisely the constructs Omarchy writes, and is honestly ignorant of
//! everything else rather than guessing.
//!
//! # Why the loops and the branches are worth the parser
//!
//! Three of the most valuable binding families in Omarchy's files are
//! generated rather than written out:
//!
//! ```lua
//! for workspace = 1, 10 do
//!   local key = "code:" .. tostring(workspace + 9)
//!   o.bind("SUPER + " .. key, "Switch to workspace " .. workspace, …)
//! ```
//!
//! A reader that skipped `for` blocks would silently lose every
//! workspace chord, every "move window to workspace n" chord, and every
//! bar-panel chord — thirty bindings, and exactly the ones an Omarchy
//! user reaches for first. So numeric `for` over integer literals is
//! expanded, bounded at [`MAX_LOOP`] iterations.
//!
//! The branches earn their keep the same way. Omarchy gates its
//! preinstalled application chords on `o.preinstalled_bindings_enabled()`
//! and its dictation chords on `o.cmd_present("voxtype")` — both of
//! which are *file system questions*, and both of which this module
//! answers by asking the file system, never by running a command. That
//! is a strict improvement on the baked preset, which had to write off
//! twenty-odd chords as [`crate::preset::Unbound::Conditional`] because
//! a table of constants cannot make that test. A live read can.
//!
//! A condition this module cannot answer — anything reaching
//! `o.shell_succeeds`, which would run a shell — is not answered. The
//! block is skipped and said so.

use super::directive::{BindFlags, Directive, Dispatcher, Include, Matcher, Monitor, WindowRule};

/// The most iterations a numeric `for` is expanded to. Omarchy's
/// longest is ten; a file asking for a million is either broken or
/// hostile, and either way the answer is the same one this whole
/// module gives: skip it, say so, keep the session.
const MAX_LOOP: i64 = 64;

/// How deep expression nesting may go before the parser stops. Tables
/// of tables of tables are a stack overflow waiting to happen, and a
/// stack overflow is the one failure mode that is *not* a logged
/// warning — it aborts the process. Omarchy's deepest is three.
const MAX_DEPTH: u32 = 24;

/// The most statements one file may contribute. A guard on total work
/// rather than on file size, so a pathological file costs a bounded
/// amount of parsing whatever shape it takes.
const MAX_STATEMENTS: usize = 20_000;

/// A Lua value, to the extent this reader needs one.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
    Nil,
    /// A table constructor. Keys are `Some` for `{ a = 1 }` and `None`
    /// for the array part `{ 1, 2 }`; Omarchy uses both, sometimes in
    /// one table (`size = { 875, 600 }`).
    Table(Vec<(Option<String>, Value)>),
    /// A call, kept unevaluated: `hl.dsp.window.close()`,
    /// `tostring(n)`, `o.cmd_present("voxtype")`.
    Call {
        path: String,
        args: Vec<Value>,
    },
    /// A bare name — a `local`, a loop variable, or a global.
    Name(String),
    /// `function() … end`, with its body parsed.
    ///
    /// Kept rather than skipped for exactly one call site, and it is a
    /// load-bearing one: Omarchy's entire `autostart.lua` is a single
    /// `hl.on("hyprland.start", function() … end)`, so a reader that
    /// threw function bodies away would find no autostart entries at
    /// all. Only that one handler's body is ever walked — see
    /// [`START_EVENT`] — because every other function in these files
    /// is a *definition* (`helpers.lua` defines `o.bind` in terms of
    /// `hl.bind`), and walking a definition would emit the bindings its
    /// own body describes rather than the ones its callers ask for.
    Function(Vec<Stmt>),
    /// `a .. b`, `a + b`, `a - b`, kept unevaluated.
    ///
    /// Unevaluated on purpose, and this is the single most important
    /// decision in the file. Omarchy writes its workspace bindings as
    /// `"SUPER + " .. key` inside a `for` loop, where `key` is
    /// `"code:" .. tostring(workspace + 9)`. Folding at *parse* time
    /// would evaluate `workspace + 9` before the loop variable
    /// existed, producing an unresolvable expression and losing all
    /// thirty generated chords. Folding at *eval* time, once per
    /// iteration with the variable bound, is what makes them work.
    Binary {
        op: &'static str,
        left: Box<Value>,
        right: Box<Value>,
    },
    /// An expression this parser declines to represent. The string is
    /// the source text, truncated, for the log.
    Opaque(String),
}

/// The Hyprland event whose handler body is read as autostart.
const START_EVENT: &str = "hyprland.start";

/// A statement, to the extent this reader needs one.
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Call {
        path: String,
        args: Vec<Value>,
    },
    /// `local x = …` and plain `x = …`, which is how a user's
    /// `hyprland.lua` sets `omarchy_default_bindings = false`.
    Assign {
        name: String,
        value: Value,
    },
    NumericFor {
        var: String,
        from: Value,
        to: Value,
        step: Option<Value>,
        body: Vec<Stmt>,
    },
    GenericFor {
        var: String,
        values: Value,
        body: Vec<Stmt>,
    },
    If {
        cond: Value,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    /// A bare `do … end` scope: its statements, transparently.
    Block(Vec<Stmt>),
    /// A construct parsed well enough to be skipped over safely, named
    /// so it can be reported. `while`, generic `for`, function
    /// definitions, `return`.
    Skipped(&'static str),
}

/// Everything the caller has to tell this module about the machine, so
/// that nothing here touches the environment directly and every
/// judgement is reproducible in a test.
#[derive(Clone, Debug)]
pub struct Facts {
    /// Directories on `PATH`, for `o.cmd_present`.
    pub path: Vec<std::path::PathBuf>,
    /// `$HOME`, for `o.preinstalled_bindings_enabled`'s marker file.
    pub home: Option<std::path::PathBuf>,
    /// `$XDG_STATE_HOME`, same.
    pub state_home: Option<std::path::PathBuf>,
}

impl Facts {
    /// The facts as this machine actually is.
    pub fn of_this_machine() -> Self {
        Self {
            path: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            home: std::env::var_os("HOME").map(Into::into),
            state_home: std::env::var_os("XDG_STATE_HOME")
                .filter(|v| !v.is_empty())
                .map(Into::into),
        }
    }

    /// `~/.local/state/omarchy` — where the preinstalls marker lives.
    fn omarchy_state(&self) -> Option<std::path::PathBuf> {
        if let Some(state) = &self.state_home {
            return Some(state.join("omarchy"));
        }
        Some(self.home.as_ref()?.join(".local/state/omarchy"))
    }

    fn cmd_present(&self, command: &str) -> bool {
        if command.contains('/') {
            return std::path::Path::new(command).exists();
        }
        self.path.iter().any(|dir| dir.join(command).exists())
    }
}

/// Reads one Lua file's text into directives, appending to `out`, and
/// records anything it declined to read.
///
/// `globals` carries assignments across files the way Lua's own
/// globals do — a user's `hyprland.lua` sets
/// `omarchy_default_bindings = false` *before* requiring Omarchy's
/// defaults, and the defaults read it. It is threaded rather than
/// stored so the reader has no state of its own between runs.
pub fn read(source: &str, facts: &Facts, globals: &mut Globals, out: &mut Vec<Directive>) {
    let mut parser = Parser::new(source);
    let body = parser.block(0);
    let mut env = Env::default();
    walk(&body, facts, globals, &mut env, out, 0);
}

/// Globals a file may set that a later file reads. Only the two
/// Omarchy documents in its own `hyprland.lua` template are honoured;
/// everything else a file assigns is remembered but unused, which
/// costs a string and keeps the mechanism uniform.
#[derive(Clone, Debug, Default)]
pub struct Globals {
    values: std::collections::BTreeMap<String, Value>,
}

impl Globals {
    fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }
}

/// Names bound inside the block being walked: loop variables and
/// `local`s. A flat map rather than a scope chain because Omarchy's
/// files never shadow, and a wrong answer here can only ever produce a
/// binding that is skipped for an unresolvable name.
type Env = std::collections::BTreeMap<String, Value>;

// ---- the walk ---------------------------------------------------------

fn walk(
    body: &[Stmt],
    facts: &Facts,
    globals: &mut Globals,
    env: &mut Env,
    out: &mut Vec<Directive>,
    depth: u32,
) {
    if depth > MAX_DEPTH {
        out.push(Directive::Ignored {
            kind: "lua",
            detail: "block nested too deeply".into(),
        });
        return;
    }
    for stmt in body {
        match stmt {
            Stmt::Call { path, args } => {
                // `hl.on("hyprland.start", function() … end)`: the one
                // handler whose body is read, and the only place a
                // function body is ever walked.
                if path == "hl.on" {
                    let event = args.first().map(|v| eval(v, env));
                    let body = args.get(1).map(|v| eval(v, env));
                    match (event, body) {
                        (Some(Value::Str(name)), Some(Value::Function(body))) if name == START_EVENT => {
                            walk(&body, facts, globals, env, out, depth + 1);
                        }
                        (Some(Value::Str(name)), Some(Value::Function(body))) if name == "layer.opened" => {
                            match layer_bindings(&body, env) {
                                Ok(bindings) if !bindings.is_empty() => out.extend(bindings),
                                Ok(_) => out.push(Directive::Ignored {
                                    kind: "event",
                                    detail: "hl.on(\"layer.opened\") contains no safely scoped bindings".into(),
                                }),
                                Err(why) => out.push(Directive::Ignored {
                                    kind: "event",
                                    detail: format!("hl.on(\"layer.opened\") handler refused whole: {why}"),
                                }),
                            }
                        }
                        (Some(Value::Str(name)), _) => out.push(Directive::Ignored {
                            kind: "event",
                            detail: format!(
                                "hl.on({name:?}) handler: only the {START_EVENT} handler's body becomes autostart"
                            ),
                        }),
                        _ => out.push(Directive::Ignored {
                            kind: "event",
                            detail: "hl.on with an unreadable event".into(),
                        }),
                    }
                    continue;
                }
                if path == "hl.define_submap" {
                    let name = args.first().map(|value| eval(value, env));
                    let body = args.get(1).map(|value| eval(value, env));
                    match (name, body) {
                        (Some(Value::Str(name)), Some(Value::Function(body))) => {
                            note_submap_bindings(&name, &body, env, out, depth + 1);
                        }
                        _ => out.push(Directive::Ignored {
                            kind: "submap",
                            detail: "hl.define_submap with an unreadable name or body".into(),
                        }),
                    }
                    continue;
                }
                emit_call(path, args, env, out);
            }
            Stmt::Assign { name, value } => {
                let value = eval(value, env);
                env.insert(name.clone(), value.clone());
                globals.values.insert(name.clone(), value);
            }
            Stmt::NumericFor {
                var,
                from,
                to,
                step,
                body,
            } => {
                let (Some(from), Some(to)) = (as_int(&eval(from, env)), as_int(&eval(to, env)))
                else {
                    out.push(Directive::Ignored {
                        kind: "lua",
                        detail: format!("for {var} = … over non-integer bounds"),
                    });
                    continue;
                };
                // A step other than 1 is refused rather than
                // implemented: Omarchy has none, and a reader that
                // quietly got a step wrong would generate bindings on
                // chords nobody wrote.
                if step
                    .as_ref()
                    .is_some_and(|s| as_int(&eval(s, env)) != Some(1))
                {
                    out.push(Directive::Ignored {
                        kind: "lua",
                        detail: format!("for {var} = … with a step this reader does not expand"),
                    });
                    continue;
                }
                if to.saturating_sub(from) >= MAX_LOOP {
                    out.push(Directive::Ignored {
                        kind: "lua",
                        detail: format!(
                            "for {var} = {from}, {to} exceeds the {MAX_LOOP}-iteration bound"
                        ),
                    });
                    continue;
                }
                let shadowed = env.get(var).cloned();
                for i in from..=to {
                    env.insert(var.clone(), Value::Num(i as f64));
                    walk(body, facts, globals, env, out, depth + 1);
                }
                match shadowed {
                    Some(old) => env.insert(var.clone(), old),
                    None => env.remove(var),
                };
            }
            Stmt::GenericFor { var, values, body } => {
                let values = iterable(&eval(values, env));
                let Some(values) = values else {
                    out.push(Directive::Ignored {
                        kind: "lua",
                        detail: format!("generic for {var} uses an unreadable iterator"),
                    });
                    continue;
                };
                let shadowed = env.get(var).cloned();
                for value in values.into_iter().take(MAX_LOOP as usize) {
                    env.insert(var.clone(), value);
                    walk(body, facts, globals, env, out, depth + 1);
                }
                match shadowed {
                    Some(old) => {
                        env.insert(var.clone(), old);
                    }
                    None => {
                        env.remove(var);
                    }
                }
            }
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => match truth(cond, facts, globals, env) {
                Some(true) => walk(then_body, facts, globals, env, out, depth + 1),
                Some(false) => walk(else_body, facts, globals, env, out, depth + 1),
                None => out.push(Directive::Ignored {
                    kind: "lua",
                    detail: format!(
                        "if {} — a condition this reader cannot answer without running it",
                        describe(cond)
                    ),
                }),
            },
            Stmt::Block(body) => walk(body, facts, globals, env, out, depth + 1),
            Stmt::Skipped(what) => out.push(Directive::Ignored {
                kind: "lua",
                detail: format!("{what} block"),
            }),
        }
    }
}

/// Report every binding inside a Lua submap without lowering any of
/// them into the global keymap. Definitions may contain simple blocks
/// and numeric loops, so recurse through those shapes; anything more
/// dynamic is named as one unsupported construct rather than partially
/// interpreting a modal scope.
fn note_submap_bindings(
    name: &str,
    body: &[Stmt],
    env: &Env,
    out: &mut Vec<Directive>,
    depth: u32,
) {
    if depth > MAX_DEPTH {
        out.push(Directive::Ignored {
            kind: "submap",
            detail: format!("submap {name:?} nested too deeply"),
        });
        return;
    }
    for stmt in body {
        match stmt {
            Stmt::Call { path, args }
                if matches!(path.as_str(), "hl.bind" | "o.bind" | "o.bind_toggle") =>
            {
                let chord = args
                    .first()
                    .map(|value| eval(value, env))
                    .and_then(|value| as_string(&value))
                    .unwrap_or_else(|| "<unreadable chord>".into());
                out.push(Directive::Ignored {
                    kind: "submap-bind",
                    detail: format!(
                        "{chord} in submap {name:?}: scoped submap bindings are unsupported and were not made global"
                    ),
                });
            }
            Stmt::Block(nested) => note_submap_bindings(name, nested, env, out, depth + 1),
            Stmt::NumericFor { .. }
            | Stmt::GenericFor { .. }
            | Stmt::If { .. }
            | Stmt::Call { .. }
            | Stmt::Assign { .. }
            | Stmt::Skipped(_) => {
                out.push(Directive::Ignored {
                    kind: "submap",
                    detail: format!(
                        "submap {name:?} contains a construct this reader cannot safely scope"
                    ),
                });
            }
        }
    }
}

fn iterable(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::Call { path, args } if path == "ipairs" || path == "pairs" => match args.first()? {
            Value::Table(items) => Some(items.iter().map(|(_, value)| value.clone()).collect()),
            _ => None,
        },
        Value::Table(items) => Some(items.iter().map(|(_, value)| value.clone()).collect()),
        _ => None,
    }
}

/// Compile the deliberately narrow layer-lifetime binding pattern used
/// by Omarchy's selection overlay. The whole handler is validated
/// before any directive is returned, so an unexpected side effect can
/// never leave a partially interpreted modal keymap behind.
fn layer_bindings(body: &[Stmt], env: &Env) -> Result<Vec<Directive>, String> {
    fn walk_layer(
        body: &[Stmt],
        env: &mut Env,
        namespace: Option<&str>,
        out: &mut Vec<Directive>,
        depth: u32,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("handler nested too deeply".into());
        }
        for stmt in body {
            match stmt {
                Stmt::If {
                    cond,
                    then_body,
                    else_body,
                } => {
                    let discovered = namespace_from_condition(cond);
                    let namespace = discovered.as_deref().or(namespace);
                    walk_layer(then_body, env, namespace, out, depth + 1)?;
                    // An else branch may contain alternate bindings and
                    // is safe only when it has no executable content.
                    if !else_body.is_empty() {
                        return Err("an else branch could install a different binding set".into());
                    }
                }
                Stmt::GenericFor { var, values, body } => {
                    let values = iterable(&eval(values, env))
                        .ok_or_else(|| format!("generic for {var} has a dynamic iterator"))?;
                    if values.len() > MAX_LOOP as usize {
                        return Err("iterator exceeds the expansion limit".into());
                    }
                    let shadowed = env.get(var).cloned();
                    for value in values {
                        env.insert(var.clone(), value);
                        walk_layer(body, env, namespace, out, depth + 1)?;
                    }
                    match shadowed {
                        Some(old) => {
                            env.insert(var.clone(), old);
                        }
                        None => {
                            env.remove(var);
                        }
                    }
                }
                Stmt::NumericFor { .. } => {
                    return Err("numeric loops are not a layer-binding lifecycle".into())
                }
                Stmt::Assign { value, .. } => collect_layer_value(value, env, namespace, out)?,
                Stmt::Call { path, args } if path == "table.insert" => {
                    for value in args {
                        collect_layer_value(value, env, namespace, out)?;
                    }
                }
                // Counter assignments and the bind table are lifecycle
                // bookkeeping. Calls other than table.insert would be
                // arbitrary handler side effects and refuse the whole.
                Stmt::Call { path, .. } => return Err(format!("unexpected call {path}")),
                Stmt::Block(body) => walk_layer(body, env, namespace, out, depth + 1)?,
                Stmt::Skipped(kind) => return Err(format!("unsupported {kind} construct")),
            }
        }
        Ok(())
    }

    let mut out = Vec::new();
    let mut env = env.clone();
    walk_layer(body, &mut env, None, &mut out, 0)?;
    Ok(out)
}

fn namespace_from_condition(condition: &Value) -> Option<String> {
    let text = describe(condition);
    let at = text.find("namespace")?;
    let rest = &text[at + "namespace".len()..];
    let quote = rest.find(['\'', '"'])?;
    let delimiter = rest.as_bytes()[quote] as char;
    let value = &rest[quote + 1..];
    let end = value.find(delimiter)?;
    Some(value[..end].to_string())
}

fn collect_layer_value(
    value: &Value,
    env: &Env,
    namespace: Option<&str>,
    out: &mut Vec<Directive>,
) -> Result<(), String> {
    match eval(value, env) {
        Value::Table(items) => {
            for (_, value) in items {
                collect_layer_value(&value, env, namespace, out)?;
            }
        }
        Value::Call { path, args } if path == "hl.bind" => {
            let namespace =
                namespace.ok_or_else(|| "binding is not guarded by layer.namespace".to_string())?;
            let keys = args
                .first()
                .and_then(as_string)
                .ok_or_else(|| "binding has an unreadable chord".to_string())?;
            let dispatcher = args
                .get(1)
                .map(|value| dispatcher_from(value, None))
                .unwrap_or(Dispatcher::Opaque("missing dispatcher".into()));
            let options = args.get(2).cloned().unwrap_or(Value::Nil);
            let description = match &options {
                Value::Table(fields) => fields
                    .iter()
                    .find(|(key, _)| key.as_deref() == Some("description"))
                    .and_then(|(_, value)| as_string(value)),
                _ => None,
            };
            out.push(Directive::LayerBind {
                namespace: namespace.to_string(),
                keys,
                description,
                flags: binding_flags(&options),
                dispatcher,
            });
        }
        Value::Call { path, args } if path == "table.insert" => {
            for value in args {
                collect_layer_value(&value, env, namespace, out)?;
            }
        }
        Value::Nil | Value::Num(_) | Value::Str(_) | Value::Bool(_) | Value::Name(_) => {}
        other => {
            return Err(format!(
                "unreadable binding expression {}",
                describe(&other)
            ))
        }
    }
    Ok(())
}

/// Answers a branch condition, or `None` for one that cannot be
/// answered without running code.
///
/// The two Omarchy actually branches on are file system questions and
/// are answered by asking the file system. `o.shell_succeeds(…)` is
/// the one that would need a shell, and it is refused by name so that
/// the refusal is visible in the log rather than implied by falling
/// through to the default.
fn truth(cond: &Value, facts: &Facts, globals: &Globals, env: &Env) -> Option<bool> {
    match cond {
        Value::Bool(b) => Some(*b),
        Value::Nil => Some(false),
        Value::Str(_) | Value::Num(_) => Some(true),
        Value::Name(name) => {
            // Lua truthiness: anything but `nil` and `false` is true,
            // and an unset global is `nil`.
            let value = env.get(name).or_else(|| globals.get(name))?;
            truth(value, facts, globals, env)
        }
        Value::Call { path, args } => match (path.as_str(), args.first()) {
            ("o.cmd_present", Some(Value::Str(cmd))) => Some(facts.cmd_present(cmd)),
            ("o.cmd_missing", Some(Value::Str(cmd))) => Some(!facts.cmd_present(cmd)),
            // `not file_exists(state/omarchy/preinstalls-removed)`,
            // unless the user set the global — Omarchy's own
            // definition in `helpers.lua`, reproduced.
            ("o.preinstalled_bindings_enabled", _) => {
                if let Some(value) = globals
                    .get("omarchy_preinstalled_bindings")
                    .or_else(|| globals.get("_G.omarchy_preinstalled_bindings"))
                {
                    return Some(matches!(value, Value::Bool(true)));
                }
                Some(!facts.omarchy_state()?.join("preinstalls-removed").exists())
            }
            _ => None,
        },
        // `x ~= false`, the shape Omarchy's own gate is written in, is
        // parsed as an opaque expression carrying its source text; the
        // two spellings it ever takes are recognised here rather than
        // by growing the expression grammar a comparison operator that
        // nothing else needs.
        Value::Opaque(text) => {
            let text = text.replace(char::is_whitespace, "");
            let name = text
                .strip_suffix("~=false")
                .or_else(|| text.strip_suffix("~=nil"))?;
            let name = name.trim_start_matches("_G.");
            match globals.get(name).or_else(|| env.get(name)) {
                Some(Value::Bool(false)) | Some(Value::Nil) => Some(text.ends_with("~=nil")),
                Some(_) => Some(true),
                // Unset is `nil`: `nil ~= false` is true, `nil ~= nil`
                // is false. Omarchy relies on the first — an untouched
                // config leaves the default bindings enabled.
                None => Some(text.ends_with("~=false")),
            }
        }
        Value::Table(_) | Value::Function(_) => Some(true),
        // Arithmetic or concatenation in a condition is not something
        // Omarchy writes. A resolved one would already be a value; an
        // unresolved one is exactly what this function has no way to
        // answer, so it says so rather than guessing a default.
        Value::Binary { .. } => None,
    }
}

/// One recognised call, turned into directives.
fn emit_call(path: &str, args: &[Value], env: &Env, out: &mut Vec<Directive>) {
    let arg = |i: usize| args.get(i).map(|v| eval(v, env)).unwrap_or(Value::Nil);
    match path {
        // `o.bind(keys, description, dispatcher [, options])`
        "o.bind" => {
            let Some(keys) = as_string(&arg(0)) else {
                out.push(Directive::Ignored { kind: "bind", detail: format!("o.bind with unreadable keys: {}", describe(&arg(0))) });
                return;
            };
            out.push(Directive::Bind {
                keys,
                description: as_string(&arg(1)),
                flags: binding_flags(&arg(3)),
                dispatcher: dispatcher_from(&arg(2), as_string(&arg(1)).as_deref()),
            });
        }
        // `o.bind_toggle(keys, description, toggle)` — `helpers.lua`
        // expands this to `omarchy-toggle-<toggle>`.
        "o.bind_toggle" => {
            let (Some(keys), Some(toggle)) = (as_string(&arg(0)), as_string(&arg(2))) else {
                out.push(Directive::Ignored { kind: "bind", detail: "o.bind_toggle with unreadable arguments".into() });
                return;
            };
            out.push(Directive::Bind {
                keys,
                description: as_string(&arg(1)),
                flags: binding_flags(&arg(3)),
                dispatcher: Dispatcher::Exec(format!("omarchy-toggle-{toggle}")),
            });
        }
        // `hl.bind(keys, dispatcher, options)` — the raw form
        // `o.bind` is built on, and what a user writes for a binding
        // with no description.
        "hl.bind" => {
            let Some(keys) = as_string(&arg(0)) else {
                out.push(Directive::Ignored { kind: "bind", detail: format!("hl.bind with unreadable keys: {}", describe(&arg(0))) });
                return;
            };
            let description = match &arg(2) {
                Value::Table(fields) => fields.iter().find(|(k, _)| k.as_deref() == Some("description")).and_then(|(_, v)| as_string(v)),
                _ => None,
            };
            out.push(Directive::Bind {
                keys,
                description: description.clone(),
                flags: binding_flags(&arg(2)),
                dispatcher: dispatcher_from(&arg(1), description.as_deref()),
            });
        }
        "hl.unbind" => match as_string(&arg(0)) {
            Some(keys) => out.push(Directive::Unbind { keys }),
            None => out.push(Directive::Ignored { kind: "unbind", detail: "hl.unbind with unreadable keys".into() }),
        },
        "hl.env" => match (as_string(&arg(0)), as_string(&arg(1))) {
            (Some(name), Some(value)) => out.push(Directive::Env { name, value }),
            _ => out.push(Directive::Ignored { kind: "env", detail: "hl.env with unreadable arguments".into() }),
        },
        // Autostart. `hl.exec_cmd` is what Omarchy's `autostart.lua`
        // calls inside its `hl.on("hyprland.start", …)` handler; the
        // two `o.` wrappers add `uwsm-app --`, which is how Omarchy
        // puts a process in its own systemd scope.
        "hl.exec_cmd" | "o.exec_on_start" => match as_string(&arg(0)) {
            Some(command) => out.push(Directive::ExecOnce { command }),
            None => out.push(Directive::Ignored { kind: "exec-once", detail: "exec with an unreadable command".into() }),
        },
        "o.launch_on_start" => match as_string(&arg(0)) {
            Some(command) => out.push(Directive::ExecOnce { command: format!("uwsm-app -- {command}") }),
            None => out.push(Directive::Ignored { kind: "exec-once", detail: "o.launch_on_start with an unreadable command".into() }),
        },
        "o.window" => out.push(Directive::WindowRule(window_rule(&arg(0), &arg(1)))),
        "hl.window_rule" => out.push(Directive::WindowRule(window_rule(&Value::Nil, &arg(0)))),
        "hl.monitor" => match &arg(0) {
            Value::Table(fields) => out.push(Directive::Monitor(monitor_from(fields))),
            other => out.push(Directive::Ignored { kind: "monitor", detail: format!("hl.monitor({})", describe(other)) }),
        },
        // Recognised, deliberately not acted on. Each is named rather
        // than lumped into one line, because "chonkstep ignored your
        // layer rule" and "chonkstep ignored your gesture" are
        // different sentences to the person reading the log.
        "hl.layer_rule" => out.push(Directive::Ignored { kind: "layer-rule", detail: "layer-shell rules are Hyprland's; this compositor has its own".into() }),
        "hl.config" => emit_config(&arg(0), out),
        "hl.gesture" => out.push(Directive::Ignored { kind: "gesture", detail: "touchpad gestures".into() }),
        "hl.on" => out.push(Directive::Ignored { kind: "event", detail: "hl.on event handlers other than the start handler's body".into() }),
        "hl.timer" | "hl.dispatch" | "hl.get_config" | "hl.get_active_window" => {}
        // The file graph. Emitted as directives rather than followed
        // here, so this module does no I/O and the loader can splice
        // each file in at exactly the point its `require` sat.
        "require" => match as_string(&arg(0)) {
            Some(name) => out.push(Directive::Include(Include::Module { name, optional: false })),
            None => out.push(Directive::Ignored { kind: "include", detail: "require with an unreadable module name".into() }),
        },
        "require_optional.module" => match as_string(&arg(0)) {
            Some(name) => out.push(Directive::Include(Include::Module { name, optional: true })),
            None => out.push(Directive::Ignored { kind: "include", detail: "require_optional with an unreadable module name".into() }),
        },
        // `require_all.files(dir, prefix)`: the directory fan-out
        // Omarchy uses for `bindings/` and `apps/`. The *prefix* is
        // resolvable — it is a module name — where the `dir` argument
        // is `paths.omarchy_path .. "/default/hypr/bindings"`, built
        // from a table returned by another module. Resolving the
        // prefix gets the same directory without this reader having to
        // evaluate `require` for a value.
        "require_all.files" => match as_string(&arg(1)) {
            Some(prefix) => out.push(Directive::Include(Include::ModuleDirectory { prefix })),
            None => out.push(Directive::Ignored {
                kind: "include",
                detail: "require_all.files with no module prefix: the directory is built from a value this reader cannot evaluate".into(),
            }),
        },
        // `dofile` is Omarchy's bootstrap, whose whole job is to set
        // `package.path` — which `super::Roots` already models, so
        // there is nothing in it to read.
        "dofile" | "package" => {}
        _ => {}
    }
}

fn binding_flags(value: &Value) -> BindFlags {
    let Value::Table(fields) = value else {
        return BindFlags::default();
    };
    let enabled = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key.as_deref() == Some(name))
            .is_some_and(|(_, value)| matches!(value, Value::Bool(true)))
    };
    BindFlags {
        locked: enabled("locked"),
        repeating: enabled("repeating"),
        release: enabled("release"),
    }
}

fn emit_config(value: &Value, out: &mut Vec<Directive>) {
    let Value::Table(root) = value else {
        out.push(Directive::Ignored {
            kind: "config",
            detail: format!("hl.config with unreadable value: {}", describe(value)),
        });
        return;
    };
    let input = root
        .iter()
        .find(|(key, _)| key.as_deref() == Some("input"))
        .map(|(_, value)| value);
    if let Some(Value::Table(fields)) = input {
        for (key, value) in fields {
            let Some(key) = key else { continue };
            if matches!(value, Value::Table(_)) {
                out.push(Directive::Ignored {
                    kind: "input",
                    detail: format!("nested input setting {key} is not implemented"),
                });
            } else {
                out.push(Directive::Input {
                    name: key.clone(),
                    value: property_text(value),
                });
            }
        }
    } else if input.is_some() {
        out.push(Directive::Ignored {
            kind: "input",
            detail: "hl.config input table is unreadable".into(),
        });
    }
    if root.iter().any(|(key, _)| key.as_deref() != Some("input")) {
        out.push(Directive::Ignored {
            kind: "config",
            detail: "hl.config settings outside input are not carried over".into(),
        });
    }
}

/// A dispatcher value, normalized onto [`Dispatcher`].
///
/// The `o.bind` helper forms are expanded exactly as `helpers.lua`
/// expands them, which is why this function reads like a transcription
/// of `command_from` in that file: it is one. Getting these wrong
/// would not fail loudly — it would bind a chord to a command that
/// almost works.
fn dispatcher_from(value: &Value, description: Option<&str>) -> Dispatcher {
    match value {
        Value::Str(command) => Dispatcher::Exec(command.clone()),
        Value::Table(fields) => {
            let field = |name: &str| {
                fields
                    .iter()
                    .find(|(k, _)| k.as_deref() == Some(name))
                    .map(|(_, v)| v)
            };
            let text = |name: &str| field(name).and_then(as_string);
            let truthy = |name: &str| matches!(field(name), Some(Value::Bool(true)));
            if let Some(app) = text("omarchy") {
                return Dispatcher::Exec(format!("omarchy-launch-{app}"));
            }
            if let Some(launch) = text("launch") {
                return match text("focus") {
                    Some(focus) => Dispatcher::Exec(format!(
                        "omarchy-launch-or-focus {} {}",
                        shell_quote(&focus),
                        shell_quote(&format!("uwsm-app -- {launch}"))
                    )),
                    None => Dispatcher::Exec(format!("uwsm-app -- {launch}")),
                };
            }
            if let Some(url) = text("webapp") {
                return if truthy("focus") {
                    Dispatcher::Exec(format!(
                        "omarchy-launch-or-focus-webapp {} {}",
                        shell_quote(description.unwrap_or("")),
                        shell_quote(&url)
                    ))
                } else {
                    Dispatcher::Exec(format!("omarchy-launch-webapp {}", shell_quote(&url)))
                };
            }
            if let Some(tui) = text("tui") {
                let program = if truthy("focus") {
                    "omarchy-launch-or-focus-tui"
                } else {
                    "omarchy-launch-tui"
                };
                return Dispatcher::Exec(format!("{program} {}", shell_quote(&tui)));
            }
            Dispatcher::Opaque(describe(value))
        }
        // `hl.dsp.*`: the structured dispatchers, flattened onto the
        // `name, arg` pair the classic conf syntax uses so that
        // `super::dispatch` makes each judgement exactly once for both
        // syntaxes.
        Value::Call { path, args } => dsp(path, args),
        other => Dispatcher::Opaque(describe(other)),
    }
}

/// `hl.dsp.<something>(<table>)` in Hyprland's conf spelling.
///
/// Only the forms Omarchy writes are translated. An unrecognised
/// dispatcher becomes a [`Dispatcher::Verb`] under its own Lua name,
/// which `super::dispatch` will not recognise either and will report
/// as having no verb here — the same outcome, reached without this
/// function having to pretend to know.
fn dsp(path: &str, args: &[Value]) -> Dispatcher {
    let table = args.first();
    let field = |name: &str| match table {
        Some(Value::Table(fields)) => fields
            .iter()
            .find(|(k, _)| k.as_deref() == Some(name))
            .map(|(_, v)| v),
        _ => None,
    };
    let text = |name: &str| field(name).and_then(as_string).unwrap_or_default();
    let verb = |name: &str, arg: String| Dispatcher::Verb {
        name: name.to_string(),
        arg,
    };
    match path.strip_prefix("hl.dsp.").unwrap_or(path) {
        "exec_cmd" => Dispatcher::Exec(args.first().and_then(as_string).unwrap_or_default()),
        "window.close" => verb("killactive", String::new()),
        "window.fullscreen" => verb(
            "fullscreen",
            if text("mode") == "maximized" {
                "1".into()
            } else {
                "0".into()
            },
        ),
        "window.pseudo" => verb("pseudo", String::new()),
        "window.float" => verb("togglefloating", String::new()),
        "window.pin" => verb("pin", String::new()),
        "window.swap" => verb("swapwindow", text("direction")),
        "window.resize" => verb("resizeactive", String::new()),
        "window.drag" => verb("movewindow", String::new()),
        "window.cycle_next" => verb("cyclenext", String::new()),
        "window.bring_to_top" => verb("bringactivetotop", String::new()),
        "window.move" => {
            // `hl.dsp.window.move` is three dispatchers wearing one
            // name: to a workspace, into a group, or out of one.
            if field("into_group").is_some() {
                return verb("moveintogroup", text("into_group"));
            }
            if field("out_of_group").is_some() {
                return verb("moveoutofgroup", String::new());
            }
            let name = if matches!(field("follow"), Some(Value::Bool(false))) {
                "movetoworkspacesilent"
            } else {
                "movetoworkspace"
            };
            verb(name, text("workspace"))
        }
        "focus" => {
            if field("workspace").is_some() {
                return verb("workspace", text("workspace"));
            }
            if field("monitor").is_some() {
                return verb("focusmonitor", text("monitor"));
            }
            verb("movefocus", text("direction"))
        }
        "workspace.toggle_special" => verb(
            "togglespecialworkspace",
            args.first().and_then(as_string).unwrap_or_default(),
        ),
        "workspace.move" => verb("movecurrentworkspacetomonitor", text("monitor")),
        "layout" => verb(
            "layoutmsg",
            args.first().and_then(as_string).unwrap_or_default(),
        ),
        "group.toggle" => verb("togglegroup", String::new()),
        "group.next" | "group.prev" | "group.active" => verb("changegroupactive", String::new()),
        "send_key_state" | "send_shortcut" => verb("sendshortcut", String::new()),
        other => verb(other, String::new()),
    }
}

/// `o.window(match, rules)` and `hl.window_rule(rules)`, both of which
/// end as one [`WindowRule`].
///
/// `helpers.lua`'s `o.window` folds its first argument into the rule
/// table's `match` field — a bare string becomes `match.class` — so
/// this reproduces that fold and then reads one shape.
fn window_rule(match_arg: &Value, rules: &Value) -> WindowRule {
    let mut rule = WindowRule::default();
    let mut push_match = |key: &str, value: &Value| {
        let Some(text) = as_string(value) else { return };
        rule.matchers.push(match key {
            "class" => Matcher::Class(text),
            "title" => Matcher::Title(text),
            "tag" => Matcher::Tag(text),
            other => Matcher::Other {
                key: other.to_string(),
                value: text,
            },
        });
    };
    match match_arg {
        Value::Str(class) => push_match("class", &Value::Str(class.clone())),
        Value::Table(fields) => {
            for (key, value) in fields {
                if let Some(key) = key {
                    push_match(key, value);
                }
            }
        }
        _ => {}
    }
    if let Value::Table(fields) = rules {
        for (key, value) in fields {
            let Some(key) = key else { continue };
            if key == "match" {
                if let Value::Table(inner) = value {
                    for (key, value) in inner {
                        if let Some(key) = key {
                            push_match(key, value);
                        }
                    }
                }
                continue;
            }
            rule.props.push((key.clone(), property_text(value)));
        }
    }
    rule
}

/// A rule property's value as the conf syntax would spell it, so the
/// two front ends hand `super::rules` the same strings: `size = { 875,
/// 600 }` and `size 875 600` both become `"875 600"`.
fn property_text(value: &Value) -> String {
    match value {
        Value::Str(text) => text.clone(),
        Value::Num(n) => format_number(*n),
        Value::Bool(true) => "on".into(),
        Value::Bool(false) => "off".into(),
        Value::Table(items) => items
            .iter()
            .map(|(_, v)| property_text(v))
            .collect::<Vec<_>>()
            .join(" "),
        other => describe(other),
    }
}

fn monitor_from(fields: &[(Option<String>, Value)]) -> Monitor {
    let field = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k.as_deref() == Some(name))
            .map(|(_, v)| property_text(v))
            .unwrap_or_default()
    };
    let mut extra = Vec::new();
    for (key, value) in fields {
        let Some(key) = key else { continue };
        if !matches!(key.as_str(), "output" | "mode" | "position" | "scale") {
            extra.push(format!("{key} {}", property_text(value)));
        }
    }
    Monitor {
        output: field("output"),
        mode: field("mode"),
        position: field("position"),
        scale: field("scale"),
        extra,
    }
}

// ---- evaluation -------------------------------------------------------

/// Resolves names, concatenation and arithmetic against `env`. Pure,
/// total, and deliberately shallow: anything it cannot resolve stays
/// [`Value::Opaque`] and is reported by whoever asked for it.
fn eval(value: &Value, env: &Env) -> Value {
    match value {
        Value::Name(name) => env
            .get(name)
            .cloned()
            .unwrap_or_else(|| Value::Name(name.clone())),
        Value::Table(fields) => Value::Table(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), eval(v, env)))
                .collect(),
        ),
        // The whole point of `Value::Binary`: both halves resolve
        // against the environment *first*, so a loop variable inside
        // one is bound by the time the fold happens.
        Value::Binary { op, left, right } => fold(op, eval(left, env), eval(right, env)),
        Value::Call { path, args } => {
            let args: Vec<Value> = args.iter().map(|v| eval(v, env)).collect();
            // `tostring(n)` is the only Lua builtin Omarchy's config
            // uses in a position this reader has to see through.
            if path == "tostring" {
                if let Some(text) = args.first().and_then(as_string) {
                    return Value::Str(text);
                }
            }
            // ...and these six are `helpers.lua`'s own pure string
            // builders, reproduced. They matter in *expression*
            // position rather than as statements: Omarchy's
            // `autostart.lua` writes `hl.exec_cmd(o.launch("udiskie
            // --automount"))`, so a reader that could not see through
            // `o.launch` would find an autostart entry it could not
            // read the command of, and drop it.
            //
            // Reproduced rather than approximated: each is a
            // transcription of the function of the same name, quoting
            // included, so an expanded call is byte-identical to what
            // Hyprland would have run.
            let text = |i: usize| args.get(i).and_then(as_string);
            let built = match path.as_str() {
                "o.launch" => text(0).map(|c| format!("uwsm-app -- {c}")),
                "o.shell_quote" => text(0).map(|c| shell_quote(&c)),
                "o.launch_webapp" => {
                    text(0).map(|url| format!("omarchy-launch-webapp {}", shell_quote(&url)))
                }
                "o.launch_webapp_sole" => match (text(0), text(1)) {
                    (Some(name), Some(url)) => Some(format!(
                        "omarchy-launch-or-focus-webapp {} {}",
                        shell_quote(&name),
                        shell_quote(&url)
                    )),
                    _ => None,
                },
                "o.launch_sole" => match (text(0), text(1)) {
                    (Some(m), Some(c)) => Some(format!(
                        "omarchy-launch-or-focus {} {}",
                        shell_quote(&m),
                        shell_quote(&format!("uwsm-app -- {c}"))
                    )),
                    _ => None,
                },
                "o.notify" => {
                    text(0).map(|m| format!("omarchy-notification-send -u low {}", shell_quote(&m)))
                }
                _ => None,
            };
            if let Some(receiver) = path.strip_suffix(".upper") {
                if args.is_empty() {
                    if let Some(Value::Str(text)) = env.get(receiver) {
                        return Value::Str(text.to_ascii_uppercase());
                    }
                }
            }
            match built {
                Some(text) => Value::Str(text),
                None => Value::Call {
                    path: path.clone(),
                    args,
                },
            }
        }
        other => other.clone(),
    }
}

/// A value as a string, with Lua's own number-to-string coercion (so
/// `"Bar panel " .. panel` reads `Bar panel 3`, not `Bar panel 3.0`).
fn as_string(value: &Value) -> Option<String> {
    match value {
        Value::Str(text) => Some(text.clone()),
        Value::Num(n) => Some(format_number(*n)),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn format_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Num(n) if n.is_finite() && n.fract() == 0.0 => Some(*n as i64),
        Value::Str(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// A short, safe rendering of a value for a log line. Bounded, because
/// a warning built from a hostile file must not be the thing that
/// fills a disk.
fn describe(value: &Value) -> String {
    let mut text = match value {
        Value::Str(s) => format!("{s:?}"),
        Value::Num(n) => format_number(*n),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".into(),
        Value::Table(_) => "a table".into(),
        Value::Call { path, .. } => format!("{path}(…)"),
        Value::Name(name) => name.clone(),
        Value::Opaque(text) => text.clone(),
        Value::Function(_) => "a function".into(),
        Value::Binary { op, left, right } => format!("{} {op} {}", render(left), render(right)),
    };
    if text.chars().count() > 80 {
        text = text.chars().take(80).collect::<String>() + "…";
    }
    text
}

/// Omarchy's own `shell_quote` from `helpers.lua`, reproduced so an
/// expanded helper form is byte-identical to what Hyprland would have
/// run.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// ---- the parser -------------------------------------------------------

/// A hand-written recursive-descent parser over Lua's expression and
/// statement syntax, covering exactly what a configuration file
/// contains. Every method is total: on anything unexpected it consumes
/// a token and continues, so the worst a malformed file can do is
/// produce fewer directives.
struct Parser {
    chars: Vec<char>,
    at: usize,
    statements: usize,
}

impl Parser {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            at: 0,
            statements: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek();
        if ch.is_some() {
            self.at += 1;
        }
        ch
    }

    fn eof(&self) -> bool {
        self.at >= self.chars.len()
    }

    /// Whether the cursor sits on the `]]` that closes a long string or
    /// a long comment.
    fn at_long_bracket_close(&self) -> bool {
        self.peek() == Some(']') && self.peek_at(1) == Some(']')
    }

    /// Whitespace and comments, including the `--[[ … ]]` long form.
    fn trivia(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.at += 1;
            }
            if self.peek() == Some('-') && self.peek_at(1) == Some('-') {
                self.at += 2;
                if self.peek() == Some('[') && self.peek_at(1) == Some('[') {
                    self.at += 2;
                    while !self.eof() && !self.at_long_bracket_close() {
                        self.at += 1;
                    }
                    self.at = (self.at + 2).min(self.chars.len());
                } else {
                    while !self.eof() && self.peek() != Some('\n') {
                        self.at += 1;
                    }
                }
                continue;
            }
            return;
        }
    }

    /// The next word, without consuming it.
    fn peek_word(&mut self) -> String {
        self.trivia();
        let mut at = self.at;
        let mut word = String::new();
        while let Some(ch) = self.chars.get(at) {
            if ch.is_alphanumeric() || *ch == '_' {
                word.push(*ch);
                at += 1;
            } else {
                break;
            }
        }
        word
    }

    fn take_word(&mut self) -> String {
        let word = self.peek_word();
        self.at += word.chars().count();
        word
    }

    /// A dotted or colon-separated name path: `hl.dsp.window.close`.
    fn take_path(&mut self) -> String {
        let mut path = self.take_word();
        loop {
            let save = self.at;
            self.trivia();
            if self.peek() == Some('.') || self.peek() == Some(':') {
                self.at += 1;
                let next = self.take_word();
                if next.is_empty() {
                    self.at = save;
                    return path;
                }
                path.push('.');
                path.push_str(&next);
            } else {
                self.at = save;
                return path;
            }
        }
    }

    /// A block of statements, stopping at `end`, `else`, `elseif`,
    /// `until` or end of input. The terminator is left unconsumed for
    /// the caller.
    fn block(&mut self, depth: u32) -> Vec<Stmt> {
        let mut body = Vec::new();
        loop {
            self.trivia();
            if self.eof() || self.statements >= MAX_STATEMENTS {
                return body;
            }
            let word = self.peek_word();
            match word.as_str() {
                "end" | "else" | "elseif" | "until" => return body,
                "" => {
                    // Not a word: punctuation this parser has no
                    // statement for. Consume it so progress is
                    // guaranteed and the loop cannot spin.
                    self.at += 1;
                    continue;
                }
                _ => {}
            }
            self.statements += 1;
            if let Some(stmt) = self.statement(&word, depth) {
                body.push(stmt);
            }
        }
    }

    fn statement(&mut self, word: &str, depth: u32) -> Option<Stmt> {
        if depth > MAX_DEPTH {
            self.skip_to_end();
            return Some(Stmt::Skipped("deeply nested"));
        }
        match word {
            "local" => {
                self.take_word();
                let name = self.take_word();
                self.trivia();
                // `local function f() … end`
                if name == "function" {
                    self.skip_to_end();
                    return Some(Stmt::Skipped("function"));
                }
                if self.peek() == Some('=') && self.peek_at(1) != Some('=') {
                    self.at += 1;
                    return Some(Stmt::Assign {
                        name,
                        value: self.expr(0),
                    });
                }
                Some(Stmt::Assign {
                    name,
                    value: Value::Nil,
                })
            }
            "for" => {
                self.take_word();
                let var = self.take_word();
                self.trivia();
                if self.peek() != Some('=') {
                    let mut last_var = var;
                    while self.peek() == Some(',') {
                        self.at += 1;
                        last_var = self.take_word();
                        self.trivia();
                    }
                    if self.take_word() != "in" {
                        self.skip_to_end();
                        return Some(Stmt::Skipped("generic for"));
                    }
                    let values = self.expr(0);
                    if self.take_word() != "do" {
                        self.skip_to_end();
                        return Some(Stmt::Skipped("generic for"));
                    }
                    let body = self.block(depth + 1);
                    self.take_word();
                    return Some(Stmt::GenericFor {
                        var: last_var,
                        values,
                        body,
                    });
                }
                self.at += 1;
                let from = self.expr(0);
                self.trivia();
                let mut to = Value::Nil;
                let mut step = None;
                if self.peek() == Some(',') {
                    self.at += 1;
                    to = self.expr(0);
                    self.trivia();
                    if self.peek() == Some(',') {
                        self.at += 1;
                        step = Some(self.expr(0));
                    }
                }
                if self.take_word() != "do" {
                    // Malformed; skip the block rather than guess at
                    // where its body starts.
                    self.skip_to_end();
                    return Some(Stmt::Skipped("for"));
                }
                let body = self.block(depth + 1);
                self.take_word();
                Some(Stmt::NumericFor {
                    var,
                    from,
                    to,
                    step,
                    body,
                })
            }
            "if" => {
                self.take_word();
                let cond = self.expr(0);
                self.take_word(); // `then`
                let then_body = self.block(depth + 1);
                // The else-chain consumes everything through the single
                // `end` that closes the whole `if`. Its own function,
                // and recursive, because `elseif` is a nested `if` that
                // *shares* the outer one's `end` — taking a second here
                // is how an `if a then … elseif b then … end` swallowed
                // whatever statement came after it.
                let else_body = self.else_chain(depth);
                Some(Stmt::If {
                    cond,
                    then_body,
                    else_body,
                })
            }
            "function" => {
                self.take_word();
                self.skip_to_end();
                Some(Stmt::Skipped("function"))
            }
            "while" | "repeat" => {
                self.take_word();
                self.skip_to_end();
                Some(Stmt::Skipped(if word == "while" {
                    "while"
                } else {
                    "repeat"
                }))
            }
            "do" => {
                self.take_word();
                let body = self.block(depth + 1);
                self.take_word(); // `end`
                                  // A bare `do … end` is only a scope. Its statements are
                                  // the enclosing block's as far as this reader cares —
                                  // which means all of them, not just the first.
                Some(Stmt::Block(body))
            }
            "return" | "break" => {
                self.take_word();
                let _ = self.expr(0);
                None
            }
            _ => {
                // An expression statement: a call, or an assignment to
                // a name. Both start with a name path.
                let path = self.take_path();
                if path.is_empty() {
                    self.at += 1;
                    return None;
                }
                self.trivia();
                if self.peek() == Some('=') && self.peek_at(1) != Some('=') {
                    self.at += 1;
                    let value = self.expr(0);
                    return Some(Stmt::Assign {
                        name: path.trim_start_matches("_G.").to_string(),
                        value,
                    });
                }
                if self.peek() == Some('(') {
                    let args = self.call_args();
                    // A call whose result is indexed or called again
                    // (`hl.bind(…):unbind()`) is still the outer call
                    // for our purposes; the tail is skipped.
                    return Some(Stmt::Call { path, args });
                }
                None
            }
        }
    }

    /// A `function(…) … end` expression, body parsed. The `function`
    /// keyword has been consumed.
    fn function_body(&mut self, depth: u32) -> Value {
        self.trivia();
        // The parameter list, discarded: this reader never calls a
        // function, so a parameter is a name that will simply be
        // unresolvable inside the body — which is the honest result.
        if self.peek() == Some('(') {
            let mut open = 0;
            while let Some(ch) = self.bump() {
                match ch {
                    '(' => open += 1,
                    ')' => {
                        open -= 1;
                        if open == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        if depth > MAX_DEPTH {
            self.skip_to_end();
            return Value::Function(Vec::new());
        }
        let body = self.block(depth + 1);
        self.take_word();
        Value::Function(body)
    }

    /// The tail of an `if`: `elseif …`, `else …`, or nothing — in every
    /// case consuming the one `end` that closes the whole chain.
    ///
    /// An `elseif` is exactly an `if` in the else branch, which is what
    /// the recursion says. The load-bearing part is that the recursive
    /// call consumes the `end`, so the caller must not: one `end` per
    /// chain, however many `elseif`s are in it.
    fn else_chain(&mut self, depth: u32) -> Vec<Stmt> {
        if depth > MAX_DEPTH {
            self.skip_to_end();
            return vec![Stmt::Skipped("deeply nested")];
        }
        self.trivia();
        match self.peek_word().as_str() {
            "elseif" => {
                self.take_word();
                let cond = self.expr(0);
                self.take_word(); // `then`
                let then_body = self.block(depth + 1);
                let else_body = self.else_chain(depth + 1);
                vec![Stmt::If {
                    cond,
                    then_body,
                    else_body,
                }]
            }
            "else" => {
                self.take_word();
                let body = self.block(depth + 1);
                self.take_word(); // `end`
                body
            }
            // `end`, or end of input on a truncated file.
            _ => {
                self.take_word();
                Vec::new()
            }
        }
    }

    /// Consumes tokens until the `end` that closes the construct just
    /// opened, tracking nesting so an inner `if`/`for`/`function` does
    /// not close the outer one. Strings and comments are skipped whole,
    /// so an `end` inside a string literal cannot unbalance it.
    fn skip_to_end(&mut self) {
        let mut depth = 1i32;
        while !self.eof() {
            self.trivia();
            match self.peek() {
                None => return,
                Some('"') | Some('\'') => {
                    let _ = self.string();
                    continue;
                }
                _ => {}
            }
            let word = self.peek_word();
            if word.is_empty() {
                self.at += 1;
                continue;
            }
            self.at += word.chars().count();
            match word.as_str() {
                "function" | "if" | "for" | "while" | "do" => {
                    // `do` closes nothing of its own when it opens a
                    // `for`/`while` body, which has already been
                    // counted; counting it again would swallow the
                    // enclosing `end`. Only a *bare* `do` opens a
                    // block, and Omarchy writes none.
                    if word != "do" {
                        depth += 1;
                    }
                }
                "end" => {
                    depth -= 1;
                    if depth <= 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// An expression, with `..`, `+` and `-` at one precedence level —
    /// enough for the arithmetic-then-concatenation shapes Omarchy
    /// writes, and left-associative like Lua's.
    fn expr(&mut self, depth: u32) -> Value {
        if depth > MAX_DEPTH {
            return Value::Opaque("…".into());
        }
        let mut left = self.primary(depth);
        loop {
            let save = self.at;
            self.trivia();
            let op = match (self.peek(), self.peek_at(1)) {
                (Some('.'), Some('.')) => {
                    self.at += 2;
                    ".."
                }
                (Some('+'), _) => {
                    self.at += 1;
                    "+"
                }
                // A `-` only continues an expression when what follows
                // is not a comment; `trivia` has already eaten
                // comments, so a bare `-` here is arithmetic.
                (Some('-'), Some(c)) if c != '-' => {
                    self.at += 1;
                    "-"
                }
                // Any comparison operator: this parser has no boolean
                // algebra, so the whole expression is carried as source
                // text for `truth` to recognise the two shapes Omarchy
                // writes.
                (Some('~'), Some('=')) | (Some('='), Some('=')) => {
                    let start = save;
                    self.at += 2;
                    let _ = self.primary(depth + 1);
                    let text: String = self.chars[start..self.at].iter().collect();
                    return Value::Opaque(format!("{}{}", render(&left), text));
                }
                _ => {
                    self.at = save;
                    return left;
                }
            };
            let right = self.primary(depth + 1);
            left = fold(op, left, right);
        }
    }

    fn primary(&mut self, depth: u32) -> Value {
        self.trivia();
        match self.peek() {
            None => Value::Nil,
            Some('"') | Some('\'') => Value::Str(self.string()),
            Some('[') if self.peek_at(1) == Some('[') => Value::Str(self.long_string()),
            Some('{') => self.table(depth + 1),
            Some('(') => {
                self.at += 1;
                let inner = self.expr(depth + 1);
                self.trivia();
                if self.peek() == Some(')') {
                    self.at += 1;
                }
                inner
            }
            Some('-') => {
                self.at += 1;
                match self.primary(depth + 1) {
                    Value::Num(n) => Value::Num(-n),
                    other => Value::Opaque(render(&other)),
                }
            }
            Some(ch) if ch.is_ascii_digit() => self.number(),
            Some(ch) if ch.is_alphabetic() || ch == '_' => {
                let path = self.take_path();
                match path.as_str() {
                    "true" => return Value::Bool(true),
                    "false" => return Value::Bool(false),
                    "nil" => return Value::Nil,
                    "function" => return self.function_body(depth),
                    "not" => {
                        let _ = self.primary(depth + 1);
                        return Value::Opaque("not …".into());
                    }
                    "" => {
                        self.at += 1;
                        return Value::Nil;
                    }
                    _ => {}
                }
                self.trivia();
                match self.peek() {
                    Some('(') => {
                        let args = self.call_args();
                        Value::Call { path, args }
                    }
                    // `require "x"` and `f{…}`: Lua's parenthesis-free
                    // call forms.
                    Some('"') | Some('\'') => Value::Call {
                        path,
                        args: vec![Value::Str(self.string())],
                    },
                    Some('{') => {
                        let table = self.table(depth + 1);
                        Value::Call {
                            path,
                            args: vec![table],
                        }
                    }
                    _ => Value::Name(path),
                }
            }
            Some(_) => {
                self.at += 1;
                Value::Opaque(String::new())
            }
        }
    }

    /// A parenthesised argument list. The open paren is at the cursor.
    fn call_args(&mut self) -> Vec<Value> {
        let mut args = Vec::new();
        if self.peek() != Some('(') {
            return args;
        }
        self.at += 1;
        loop {
            self.trivia();
            match self.peek() {
                None => return args,
                Some(')') => {
                    self.at += 1;
                    return args;
                }
                Some(',') => {
                    self.at += 1;
                    continue;
                }
                _ => {}
            }
            let before = self.at;
            args.push(self.expr(1));
            // Guaranteed progress: an expression that consumed nothing
            // would spin here forever on malformed input.
            if self.at == before {
                self.at += 1;
            }
        }
    }

    fn table(&mut self, depth: u32) -> Value {
        let mut fields: Vec<(Option<String>, Value)> = Vec::new();
        if self.peek() != Some('{') {
            return Value::Table(fields);
        }
        self.at += 1;
        if depth > MAX_DEPTH {
            // Do not recurse further; find the matching brace by
            // counting so the caller resumes in the right place.
            let mut braces = 1;
            while let Some(ch) = self.bump() {
                match ch {
                    '{' => braces += 1,
                    '}' => {
                        braces -= 1;
                        if braces == 0 {
                            break;
                        }
                    }
                    '"' | '\'' => {
                        self.at -= 1;
                        let _ = self.string();
                    }
                    _ => {}
                }
            }
            return Value::Table(fields);
        }
        loop {
            self.trivia();
            match self.peek() {
                None => return Value::Table(fields),
                Some('}') => {
                    self.at += 1;
                    return Value::Table(fields);
                }
                Some(',') | Some(';') => {
                    self.at += 1;
                    continue;
                }
                _ => {}
            }
            let before = self.at;
            // `[expr] = value`, which Omarchy uses for its
            // exclusion sets (`["touchpad-disabled"] = true`).
            if self.peek() == Some('[') && self.peek_at(1) != Some('[') {
                self.at += 1;
                let key = self.expr(depth + 1);
                self.trivia();
                if self.peek() == Some(']') {
                    self.at += 1;
                }
                self.trivia();
                if self.peek() == Some('=') {
                    self.at += 1;
                }
                let value = self.expr(depth + 1);
                fields.push((as_string(&key), value));
                if self.at == before {
                    self.at += 1;
                }
                continue;
            }
            // `name = value`, distinguished from a bare array entry by
            // looking past the name for a single `=`.
            let save = self.at;
            let name = self.take_word();
            self.trivia();
            if !name.is_empty() && self.peek() == Some('=') && self.peek_at(1) != Some('=') {
                self.at += 1;
                let value = self.expr(depth + 1);
                fields.push((Some(name), value));
            } else {
                self.at = save;
                let value = self.expr(depth + 1);
                fields.push((None, value));
            }
            if self.at == before {
                self.at += 1;
            }
        }
    }

    /// A quoted string, with Lua's escape sequences. The opening quote
    /// is at the cursor.
    fn string(&mut self) -> String {
        let Some(quote) = self.bump() else {
            return String::new();
        };
        let mut text = String::new();
        while let Some(ch) = self.bump() {
            if ch == quote {
                break;
            }
            if ch != '\\' {
                text.push(ch);
                continue;
            }
            match self.bump() {
                Some('n') => text.push('\n'),
                Some('t') => text.push('\t'),
                Some('r') => text.push('\r'),
                Some('\\') => text.push('\\'),
                Some('"') => text.push('"'),
                Some('\'') => text.push('\''),
                // An escape this reader has no meaning for is kept
                // *with* its backslash, because these strings are
                // regular expressions as often as they are prose:
                // `"^Battle\\.net$"` must not become `^Battle.net$`.
                Some(other) => {
                    text.push('\\');
                    text.push(other);
                }
                None => break,
            }
        }
        text
    }

    /// A `[[ … ]]` long string.
    fn long_string(&mut self) -> String {
        self.at += 2;
        let start = self.at;
        while !self.eof() && !self.at_long_bracket_close() {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at.min(self.chars.len())]
            .iter()
            .collect();
        self.at = (self.at + 2).min(self.chars.len());
        text
    }

    fn number(&mut self) -> Value {
        let start = self.at;
        while self.peek().is_some_and(|c| {
            c.is_ascii_digit() || c == '.' || c == 'x' || c == 'X' || c.is_ascii_hexdigit()
        }) {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            if let Ok(n) = u64::from_str_radix(hex, 16) {
                return Value::Num(n as f64);
            }
        }
        text.parse().map(Value::Num).unwrap_or(Value::Nil)
    }
}

/// `a .. b`, `a + b`, `a - b` on the values this reader can resolve.
fn fold(op: &'static str, left: Value, right: Value) -> Value {
    match op {
        ".." => match (as_string(&left), as_string(&right)) {
            (Some(a), Some(b)) => Value::Str(format!("{a}{b}")),
            // Not resolvable *yet*. Both halves are kept so a later
            // `eval`, with a loop variable bound, can finish the job —
            // this is the deferral `Value::Binary` exists for. A
            // concatenation still unresolved when a binding asks for it
            // is refused there, whole: half a key spec is not a key
            // spec.
            _ => Value::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
        },
        "+" | "-" => match (as_number(&left), as_number(&right)) {
            (Some(a), Some(b)) => Value::Num(if op == "+" { a + b } else { a - b }),
            _ => Value::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
        },
        _ => Value::Opaque(render(&left)),
    }
}

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Num(n) => Some(*n),
        Value::Str(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// A value rendered back to something resembling its source, for the
/// opaque-expression text `truth` matches on and for log lines.
fn render(value: &Value) -> String {
    match value {
        Value::Name(name) => name.clone(),
        Value::Str(text) => format!("{text:?}"),
        Value::Num(n) => format_number(*n),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".into(),
        Value::Opaque(text) => text.clone(),
        Value::Call { path, .. } => format!("{path}(…)"),
        Value::Table(_) => "{…}".into(),
        Value::Function(_) => "function".into(),
        Value::Binary { op, left, right } => format!("{} {op} {}", render(left), render(right)),
    }
}
