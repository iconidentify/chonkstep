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
//! tables, concatenation, arithmetic, `tostring`, `and`/`or`/`not` and
//! comparisons at Lua's own precedence, and a name bound by `local`, by
//! a loop or by an earlier file's global. Conditions are answered in
//! three-valued logic, so a part only running code could answer decides
//! nothing unless the rest decides the whole. Every other expression is
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
//! expanded, bounded at `MAX_LOOP` iterations.
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

/// The most directives one `read` may contribute. Loops multiply, so a
/// per-loop bound alone bounds neither output nor work: five nested
/// loops of 64 ask for a billion passes, each within its own bound.
/// The captured Omarchy machine's whole tree produces a few hundred.
pub(super) const MAX_DIRECTIVES: usize = 8_192;

/// The most statements one `read` may walk, counting every pass of a
/// loop body. The directive bound's twin: a loop whose body produces
/// nothing still costs its passes.
const MAX_STEPS: usize = 65_536;

/// The most operators one statement's expressions may build. An
/// operator chain is a tree, and `eval`, `render` and `Drop` all
/// recurse over it, so a chain longer than a stack is a stack
/// overflow three different ways. Counted per statement rather than
/// per chain, because parentheses stack chains end to end. Omarchy's
/// longest is four.
const MAX_OPERATORS: usize = 256;

/// The heaviest a value bound to a name may be, in nodes plus one per
/// 64 bytes of text, and the deepest it may nest. A value only grows by
/// being bound and read back — `t = { t, t }` doubles on every pass and
/// `t = { t }` deepens — so binding is where its size is checked.
const MAX_VALUE_WEIGHT: usize = 4_096;
const MAX_VALUE_DEPTH: u32 = 256;

/// How many steps answering one condition may take. A name bound to a
/// name is followed, and `o = o or {}` binds one to itself.
const MAX_TRUTH_STEPS: u32 = 256;

/// Lua's binary operators by precedence, loosest first, and longest
/// spelling first within a level so that `<=` is not read as `<`.
const PRECEDENCE: [&[&str]; 6] = [
    &["or"],
    &["and"],
    &["==", "~=", "<=", ">=", "<", ">"],
    &[".."],
    &["+", "-"],
    &["*", "//", "/", "%"],
];

/// What a function's parameter is bound to inside its body: something
/// only a caller could supply, and this reader never calls anything.
const PARAMETER: &str = "a function parameter";

/// Stands for every name in [`Globals::unknown`], once part of a file
/// went unread. Not a Lua identifier, so it cannot collide with one.
const ANY_NAME: &str = "*";

/// Names Lua or Hyprland define before any configuration file runs, so
/// never an unset `nil`.
const BUILTINS: &[&str] = &[
    "_G", "_VERSION", "arg", "assert", "collectgarbage", "coroutine", "debug", "dofile",
    "error", "getmetatable", "hl", "io", "ipairs", "load", "loadfile", "math", "next", "os",
    "package", "pairs", "pcall", "print", "rawequal", "rawget", "rawlen", "rawset",
    "require", "select", "setmetatable", "string", "table", "tonumber", "tostring", "type",
    "utf8", "xpcall",
];

/// The `nil` an unset global reads as.
static NIL: Value = Value::Nil;

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
    /// `START_EVENT` — because every other function in these files
    /// is a *definition* (`helpers.lua` defines `o.bind` in terms of
    /// `hl.bind`), and walking a definition would emit the bindings its
    /// own body describes rather than the ones its callers ask for.
    ///
    /// Shared rather than owned, so binding a function to a name, or
    /// putting it in a table that is bound again, copies nothing.
    Function(std::sync::Arc<Vec<Stmt>>),
    /// `a .. b`, `a + b`, `a - b` and the rest of Lua's arithmetic, kept
    /// unevaluated.
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
    /// `a or b` and `a and b`, kept unevaluated while `a` is not yet a
    /// value. Once it is, Lua's own value semantics apply: `a or b` is
    /// `a` when `a` is truthy and `b` otherwise, and `and` is the
    /// reverse.
    Or(Box<Value>, Box<Value>),
    And(Box<Value>, Box<Value>),
    /// `not a`, kept unevaluated while `a` is not yet a value.
    Not(Box<Value>),
    /// `a == b`, `a ~= b` and the orderings, kept unevaluated while
    /// either side is not yet a value.
    Compare {
        op: &'static str,
        left: Box<Value>,
        right: Box<Value>,
    },
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
    /// `x = …` with no `local x` in scope: a global, which is how a
    /// user's `hyprland.lua` sets `omarchy_default_bindings = false`
    /// for Omarchy's defaults to read. `_G.x = …` keeps its prefix only
    /// while a `local x` is in scope, so the walk leaves that local be.
    Assign {
        name: String,
        value: Value,
    },
    /// `local x = …`, and a plain `x = …` while a `local x` is in scope.
    /// Seen only by the file that declares it: a `local` never reaches
    /// `_G`, so it never reaches another file.
    Local {
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
    /// so it can be reported. `while`, a malformed `for`, function
    /// definitions, an `if` without `then`, a statement that hit a
    /// bound. Whatever names it could assign were recorded as it was
    /// parsed.
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
    // What the parser skipped over could have assigned these, so none
    // of them is an unset `nil` from here on.
    globals.unknown.append(&mut parser.hidden);
    let mut env = Env::default();
    // This read's own vector, so the directive budget counts this
    // file's output and not the files read before it.
    let mut read = Vec::new();
    let mut steps = 0;
    walk(&body, facts, globals, &mut env, &mut read, &mut steps, 0);
    out.append(&mut read);
}

/// Globals a file may set that a later file reads. Only the two
/// Omarchy documents in its own `hyprland.lua` template are honoured;
/// everything else a file assigns is remembered but unused, which
/// costs a string and keeps the mechanism uniform.
#[derive(Clone, Debug, Default)]
pub struct Globals {
    values: std::collections::BTreeMap<String, Value>,
    /// Names a construct this reader did not walk could have assigned:
    /// the body of an `if` it could not answer, a `while`, a function, a
    /// loop over an unreadable iterator. An unset global is `nil` in
    /// Lua, but reading one of these as `nil` would be a guess.
    unknown: std::collections::BTreeSet<String>,
}

impl Globals {
    fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// `nil`, for a name no file has set and nothing unread could have:
    /// Lua's rule for an unset global, applied only where it is not a
    /// guess. A dotted path reads a field of something, and a builtin
    /// is set before any file runs, so neither reads as `nil` merely
    /// for being unset here.
    fn unset(&self, name: &str) -> Option<&'static Value> {
        let plain = name.starts_with(|c: char| c.is_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_alphanumeric() || c == '_');
        (plain
            && !BUILTINS.contains(&name)
            && !self.unknown.contains(name)
            && !self.unknown.contains(ANY_NAME))
        .then_some(&NIL)
    }
}

/// Names bound in the file being walked: `local`s, loop variables, and
/// the globals the file itself assigned. A flat map rather than a scope
/// chain; the parser has already decided which assignments are to a
/// `local`, which is the part of scoping that changes an answer.
type Env = std::collections::BTreeMap<String, Value>;

// ---- the walk ---------------------------------------------------------

fn walk(
    body: &[Stmt],
    facts: &Facts,
    globals: &mut Globals,
    env: &mut Env,
    out: &mut Vec<Directive>,
    steps: &mut usize,
    depth: u32,
) {
    if depth > MAX_DEPTH {
        unwalked(body, globals);
        out.push(Directive::Ignored {
            kind: "lua",
            detail: "block nested too deeply".into(),
        });
        return;
    }
    for stmt in body {
        if spent(steps, out) {
            // The rest of this file goes unread, so any name may be set.
            globals.unknown.insert(ANY_NAME.into());
            return;
        }
        match stmt {
            Stmt::Call { path, args } => {
                // `hl.on("hyprland.start", function() … end)`: the one
                // handler whose body is read, and the only place a
                // function body is ever walked.
                if path == "hl.on" {
                    let event = args.first().map(|v| eval(v, env));
                    let body = args.get(1).map(|v| eval(v, env));
                    if !matches!(&event, Some(Value::Str(name)) if name == START_EVENT) {
                        // Any other handler runs later, if ever.
                        args.iter().for_each(|arg| unwalked_value(arg, globals));
                    }
                    match (event, body) {
                        (Some(Value::Str(name)), Some(Value::Function(body))) if name == START_EVENT => {
                            walk(&body, facts, globals, env, out, steps, depth + 1);
                        }
                        (Some(Value::Str(name)), Some(Value::Function(body))) if name == "layer.opened" => {
                            match layer_bindings(&body, env, steps) {
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
                // Every other call's function arguments, if any, are
                // handlers or definitions, never walked.
                args.iter().for_each(|arg| unwalked_value(arg, globals));
                if path == "hl.define_submap" {
                    let name = args.first().map(|value| eval(value, env));
                    let body = args.get(1).map(|value| eval(value, env));
                    match (name, body) {
                        (Some(Value::Str(name)), Some(Value::Function(body))) => {
                            note_submap_bindings(&name, &body, env, out, steps, depth + 1);
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
            Stmt::Local { name, value } => {
                unwalked_value(value, globals);
                let value = bind(name, eval(value, env), out);
                env.insert(name.clone(), value);
            }
            Stmt::Assign { name, value } => {
                unwalked_value(value, globals);
                let value = bind(name, eval(value, env), out);
                match name.strip_prefix("_G.") {
                    // `_G.x` while a `local x` is in scope: the global
                    // changes and the local does not.
                    Some(global) => {
                        globals.values.insert(global.to_string(), value);
                    }
                    None => {
                        env.insert(name.clone(), value.clone());
                        globals.values.insert(name.clone(), value);
                    }
                }
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
                    unwalked(body, globals);
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
                    unwalked(body, globals);
                    out.push(Directive::Ignored {
                        kind: "lua",
                        detail: format!("for {var} = … with a step this reader does not expand"),
                    });
                    continue;
                }
                if to.saturating_sub(from) >= MAX_LOOP {
                    unwalked(body, globals);
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
                    // Every pass counts, even one with nothing in it:
                    // nested loops multiply, and their bounds do not.
                    if spent(steps, out) {
                        break;
                    }
                    env.insert(var.clone(), Value::Num(i as f64));
                    walk(body, facts, globals, env, out, steps, depth + 1);
                }
                match shadowed {
                    Some(old) => env.insert(var.clone(), old),
                    None => env.remove(var),
                };
            }
            Stmt::GenericFor { var, values, body } => {
                let values = iterable(&eval(values, env));
                let Some(values) = values else {
                    unwalked(body, globals);
                    out.push(Directive::Ignored {
                        kind: "lua",
                        detail: format!("generic for {var} uses an unreadable iterator"),
                    });
                    continue;
                };
                let shadowed = env.get(var).cloned();
                for value in values.into_iter().take(MAX_LOOP as usize) {
                    if spent(steps, out) {
                        break;
                    }
                    env.insert(var.clone(), value);
                    walk(body, facts, globals, env, out, steps, depth + 1);
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
            } => {
                let mut fuel = MAX_TRUTH_STEPS;
                match truth(cond, facts, globals, env, &mut fuel) {
                    Some(true) => walk(then_body, facts, globals, env, out, steps, depth + 1),
                    Some(false) => walk(else_body, facts, globals, env, out, steps, depth + 1),
                    None => {
                        unwalked(then_body, globals);
                        unwalked(else_body, globals);
                        out.push(Directive::Ignored {
                            kind: "lua",
                            detail: format!(
                                "if {} — a condition this reader cannot answer without running it",
                                describe(cond)
                            ),
                        });
                    }
                }
            }
            Stmt::Block(body) => walk(body, facts, globals, env, out, steps, depth + 1),
            Stmt::Skipped(what) => out.push(Directive::Ignored {
                kind: "lua",
                detail: format!("{what}: not read"),
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
    steps: &mut usize,
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
        if spent(steps, out) {
            return;
        }
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
            Stmt::Block(nested) => note_submap_bindings(name, nested, env, out, steps, depth + 1),
            // The body's own parameters: bookkeeping, not a construct.
            Stmt::Local { value: Value::Opaque(text), .. } if text == PARAMETER => {}
            Stmt::NumericFor { .. }
            | Stmt::GenericFor { .. }
            | Stmt::If { .. }
            | Stmt::Call { .. }
            | Stmt::Assign { .. }
            | Stmt::Local { .. }
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
fn layer_bindings(body: &[Stmt], env: &Env, steps: &mut usize) -> Result<Vec<Directive>, String> {
    fn walk_layer(
        body: &[Stmt],
        env: &mut Env,
        namespace: Option<&str>,
        out: &mut Vec<Directive>,
        steps: &mut usize,
        depth: u32,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("handler nested too deeply".into());
        }
        for stmt in body {
            // The read's own budget: loops nest here as they do there.
            *steps = steps.saturating_add(1);
            if *steps > MAX_STEPS || out.len() >= MAX_DIRECTIVES {
                return Err(format!(
                    "handler walks more than {MAX_STEPS} statements or binds more than {MAX_DIRECTIVES} keys"
                ));
            }
            match stmt {
                Stmt::If {
                    cond,
                    then_body,
                    else_body,
                } => {
                    let discovered = namespace_from_condition(cond);
                    let namespace = discovered.as_deref().or(namespace);
                    walk_layer(then_body, env, namespace, out, steps, depth + 1)?;
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
                        walk_layer(body, env, namespace, out, steps, depth + 1)?;
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
                // The handler's parameter, `layer`, is what the event
                // supplies; bound so the body's names resolve, not read.
                Stmt::Local { name, value: Value::Opaque(text) } if text == PARAMETER => {
                    env.insert(name.clone(), Value::Opaque(text.clone()));
                }
                Stmt::Assign { value, .. } | Stmt::Local { value, .. } => {
                    collect_layer_value(value, env, namespace, out)?
                }
                Stmt::Call { path, args } if path == "table.insert" => {
                    for value in args {
                        collect_layer_value(value, env, namespace, out)?;
                    }
                }
                // Counter assignments and the bind table are lifecycle
                // bookkeeping. Calls other than table.insert would be
                // arbitrary handler side effects and refuse the whole.
                Stmt::Call { path, .. } => return Err(format!("unexpected call {path}")),
                Stmt::Block(body) => walk_layer(body, env, namespace, out, steps, depth + 1)?,
                Stmt::Skipped(kind) => return Err(format!("unsupported {kind} construct")),
            }
        }
        Ok(())
    }

    let mut out = Vec::new();
    let mut env = env.clone();
    walk_layer(body, &mut env, None, &mut out, steps, 0)?;
    Ok(out)
}

/// The namespace a `layer.namespace == "…"` guard names. Exactly that
/// shape and no other: a guard this cannot read leaves the bindings
/// under it unscoped, and an unscoped binding refuses the handler.
fn namespace_from_condition(condition: &Value) -> Option<String> {
    let Value::Compare { op: "==", left, right } = condition else {
        return None;
    };
    match (&**left, &**right) {
        (Value::Name(name), Value::Str(namespace)) if name == "layer.namespace" => {
            Some(namespace.clone())
        }
        _ => None,
    }
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
///
/// `fuel` meters the answer. A name bound to a name is followed, and
/// must be — `local gate = omarchy_default_bindings` then `if gate` is
/// a legitimate two-hop read — but `o = o or {}` binds a name to
/// itself, and a cycle followed without a meter never returns. Out of
/// fuel, a condition is simply unanswerable.
fn truth(cond: &Value, facts: &Facts, globals: &Globals, env: &Env, fuel: &mut u32) -> Option<bool> {
    *fuel = fuel.checked_sub(1)?;
    match cond {
        Value::Bool(b) => Some(*b),
        Value::Nil => Some(false),
        Value::Str(_) | Value::Num(_) | Value::Table(_) | Value::Function(_) => Some(true),
        // Lua truthiness: anything but `nil` and `false` is true, and
        // an unset global is `nil` wherever that is not a guess.
        Value::Name(name) => truth(resolve(name, globals, env)?, facts, globals, env, fuel),
        // Kleene logic. An operand this cannot answer decides nothing,
        // unless the other one decides the whole: `x and false` is
        // false and `x or true` is true, whatever `x` is.
        Value::And(left, right) => {
            let left = truth(left, facts, globals, env, fuel);
            if left == Some(false) {
                return Some(false);
            }
            match (left, truth(right, facts, globals, env, fuel)?) {
                (_, false) => Some(false),
                (Some(true), true) => Some(true),
                _ => None,
            }
        }
        Value::Or(left, right) => {
            let left = truth(left, facts, globals, env, fuel);
            if left == Some(true) {
                return Some(true);
            }
            match (left, truth(right, facts, globals, env, fuel)?) {
                (_, true) => Some(true),
                (Some(false), false) => Some(false),
                _ => None,
            }
        }
        Value::Not(operand) => truth(operand, facts, globals, env, fuel).map(|b| !b),
        // Omarchy's own gate, `_G.omarchy_default_bindings ~= false`,
        // is one of these, and `nil ~= false` is true: an untouched
        // config, or one that sets the global back to `nil`, leaves the
        // default bindings on.
        Value::Compare { op, left, right } => {
            let left = settle(left, facts, globals, env, fuel)?;
            let right = settle(right, facts, globals, env, fuel)?;
            compare(op, &left, &right)
        }
        Value::Call { path, args } => match (path.as_str(), args.first()) {
            ("o.cmd_present", Some(Value::Str(cmd))) => Some(facts.cmd_present(cmd)),
            ("o.cmd_missing", Some(Value::Str(cmd))) => Some(!facts.cmd_present(cmd)),
            // Omarchy's own definition in `helpers.lua`, reproduced:
            // `_G.omarchy_preinstalled_bindings == true` when the global
            // is set, and otherwise whether the preinstalls-removed
            // marker is absent.
            ("o.preinstalled_bindings_enabled", _) => {
                let global = Value::Name("_G.omarchy_preinstalled_bindings".into());
                match settle(&global, facts, globals, env, fuel)? {
                    Value::Nil => Some(!facts.omarchy_state()?.join("preinstalls-removed").exists()),
                    value => Some(value == Value::Bool(true)),
                }
            }
            _ => None,
        },
        // An opaque expression, or arithmetic that never resolved, is
        // exactly what this function has no way to answer, so it says
        // so rather than guessing a default.
        Value::Opaque(_) | Value::Binary { .. } => None,
    }
}

/// What a name reads as: its binding in this file or in `_G`, `nil`
/// where Lua's unset-global rule is not a guess (see
/// [`Globals::unset`]), or `None`. `_G.x` is the global whatever
/// `local x` is in scope.
fn resolve<'a>(name: &str, globals: &'a Globals, env: &'a Env) -> Option<&'a Value> {
    match name.strip_prefix("_G.") {
        Some(global) => globals.get(global).or_else(|| globals.unset(global)),
        None => env
            .get(name)
            .or_else(|| globals.get(name))
            .or_else(|| globals.unset(name)),
    }
}

/// A comparison's operand as a Lua value, or `None` where only running
/// code could say what it is.
fn settle(value: &Value, facts: &Facts, globals: &Globals, env: &Env, fuel: &mut u32) -> Option<Value> {
    *fuel = fuel.checked_sub(1)?;
    match value {
        Value::Nil
        | Value::Bool(_)
        | Value::Num(_)
        | Value::Str(_)
        | Value::Table(_)
        | Value::Function(_) => Some(value.clone()),
        Value::Name(name) => settle(resolve(name, globals, env)?, facts, globals, env, fuel),
        // Each of these is a boolean exactly when it can be answered.
        Value::Not(_) | Value::Compare { .. } | Value::Call { .. } => {
            truth(value, facts, globals, env, fuel).map(Value::Bool)
        }
        // `a or b` is `a` when `a` is truthy and `b` otherwise; `and` is
        // the reverse.
        Value::Or(left, right) | Value::And(left, right) => {
            let or = matches!(value, Value::Or(..));
            let left_decides = truth(left, facts, globals, env, fuel)? == or;
            settle(if left_decides { left } else { right }, facts, globals, env, fuel)
        }
        Value::Opaque(_) | Value::Binary { .. } => None,
    }
}

/// `==`, `~=` and the orderings, as Lua 5.4 answers them, between two
/// values that are values. Different types are unequal. Two tables or
/// two functions are equal only if they are the same one, which a reader
/// that runs nothing cannot tell, and ordering anything but two numbers
/// or two strings is an error in Lua; both are unanswerable here.
fn compare(op: &str, left: &Value, right: &Value) -> Option<bool> {
    let kind = |value: &Value| match value {
        Value::Nil => Some(0),
        Value::Bool(_) => Some(1),
        Value::Num(_) => Some(2),
        Value::Str(_) => Some(3),
        Value::Table(_) => Some(4),
        Value::Function(_) => Some(5),
        _ => None,
    };
    let equal = match (left, right) {
        (Value::Nil, Value::Nil) => Some(true),
        (Value::Bool(a), Value::Bool(b)) => Some(a == b),
        (Value::Num(a), Value::Num(b)) => Some(a == b),
        (Value::Str(a), Value::Str(b)) => Some(a == b),
        _ => (kind(left)? != kind(right)?).then_some(false),
    };
    match (op, left, right) {
        ("==", _, _) => equal,
        ("~=", _, _) => equal.map(|equal| !equal),
        ("<", Value::Num(a), Value::Num(b)) => Some(a < b),
        ("<=", Value::Num(a), Value::Num(b)) => Some(a <= b),
        (">", Value::Num(a), Value::Num(b)) => Some(a > b),
        (">=", Value::Num(a), Value::Num(b)) => Some(a >= b),
        ("<", Value::Str(a), Value::Str(b)) => Some(a < b),
        ("<=", Value::Str(a), Value::Str(b)) => Some(a <= b),
        (">", Value::Str(a), Value::Str(b)) => Some(a > b),
        (">=", Value::Str(a), Value::Str(b)) => Some(a >= b),
        _ => None,
    }
}

/// Records, as possibly set, every global a body the walk did not enter
/// assigns. Its `local`s are left out: one declared in such a body is
/// out of scope past it.
fn unwalked(body: &[Stmt], globals: &mut Globals) {
    for stmt in body {
        match stmt {
            Stmt::Assign { name, value } => {
                globals
                    .unknown
                    .insert(name.trim_start_matches("_G.").to_string());
                unwalked_value(value, globals);
            }
            Stmt::Local { value, .. } => unwalked_value(value, globals),
            Stmt::Call { args, .. } => args.iter().for_each(|arg| unwalked_value(arg, globals)),
            Stmt::NumericFor { body, .. } | Stmt::GenericFor { body, .. } | Stmt::Block(body) => {
                unwalked(body, globals)
            }
            Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                unwalked(then_body, globals);
                unwalked(else_body, globals);
            }
            // Its names were recorded as it was parsed.
            Stmt::Skipped(_) => {}
        }
    }
}

/// The same, for the function bodies a value carries.
fn unwalked_value(value: &Value, globals: &mut Globals) {
    match value {
        Value::Function(body) => unwalked(body, globals),
        Value::Table(fields) => fields
            .iter()
            .for_each(|(_, value)| unwalked_value(value, globals)),
        Value::Call { args, .. } => args.iter().for_each(|arg| unwalked_value(arg, globals)),
        Value::Binary { left, right, .. }
        | Value::Or(left, right)
        | Value::And(left, right)
        | Value::Compare { left, right, .. } => {
            unwalked_value(left, globals);
            unwalked_value(right, globals);
        }
        Value::Not(operand) => unwalked_value(operand, globals),
        Value::Str(_) | Value::Num(_) | Value::Bool(_) | Value::Nil | Value::Name(_) | Value::Opaque(_) => {}
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
        "o.window" => out.push(match window_rule(&arg(0), &arg(1)) {
            Ok(rule) => Directive::WindowRule(rule),
            Err(why) => Directive::Ignored { kind: "window-rule", detail: format!("o.window refused whole: {why}") },
        }),
        "hl.window_rule" => out.push(match window_rule(&Value::Nil, &arg(0)) {
            Ok(rule) => Directive::WindowRule(rule),
            Err(why) => Directive::Ignored { kind: "window-rule", detail: format!("hl.window_rule refused whole: {why}") },
        }),
        "hl.monitor" => match &arg(0) {
            Value::Table(fields) => out.push(match monitor_from(fields) {
                Ok(monitor) => Directive::Monitor(monitor),
                Err(why) => Directive::Ignored { kind: "monitor", detail: format!("hl.monitor refused whole: {why}") },
            }),
            other => out.push(Directive::Ignored { kind: "monitor", detail: format!("hl.monitor({})", describe(other)) }),
        },
        // Recognised, deliberately not acted on. Each is named rather
        // than lumped into one line, because "chonkstep ignored your
        // layer rule" and "chonkstep ignored your gesture" are
        // different sentences to the person reading the log.
        "hl.layer_rule" => out.push(Directive::Ignored { kind: "layer-rule", detail: "layer-shell rules are Hyprland's; this compositor has its own".into() }),
        "hl.config" => emit_config(&arg(0), out),
        // The per-workspace layout Omarchy's own toggle script saves
        // under `~/.local/state/omarchy/workspace-layouts/`.
        "hl.workspace_rule" => workspace_rule(&arg(0), out),
        "hl.gesture" => out.push(Directive::Ignored { kind: "gesture", detail: "touchpad gestures".into() }),
        // A rule for one input device, by the exact name written in it.
        "hl.device" => out.push(match &arg(0) {
            Value::Table(fields) => device_rule(fields),
            other => Directive::Ignored {
                kind: "device",
                detail: format!("hl.device({}): not a table of device settings", describe(other)),
            },
        }),
        // `local disabled_input_device = require("default.hypr.disabled-input-device")`
        // and then `disabled_input_device("touchpad")`: Omarchy re-applying
        // a disable it stored as data. The module's own Lua is never read;
        // the loader reads the one line it names, as a name.
        local if is_disabled_input_device(local, env) => match as_string(&arg(0)).as_deref() {
            Some(kind @ ("touchpad" | "touchscreen")) => {
                out.push(Directive::PersistedDeviceDisable { kind: kind.to_string() })
            }
            _ => out.push(Directive::Ignored {
                kind: "device",
                detail: format!("{local}({}): Omarchy persists only touchpad and touchscreen disables", describe(&arg(0))),
            }),
        },
        "hl.on" => out.push(Directive::Ignored { kind: "event", detail: "hl.on event handlers other than the start handler's body".into() }),
        // Hyprland's animation machinery, the Lua spelling of the
        // `bezier` and `animation` lines the conf reader names.
        "hl.curve" | "hl.animation" => out.push(Directive::Ignored {
            kind: "animation",
            detail: format!("{path}(…): Hyprland's animations; this desktop draws its own"),
        }),
        // Calls that act while Hyprland runs rather than configure it.
        runtime if runtime == "hl.timer" || runtime == "hl.dispatch" || runtime.starts_with("hl.get_") => {
            out.push(Directive::Ignored {
                kind: "lua-call",
                detail: format!("{path}(…): a runtime call, not configuration"),
            })
        }
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
        "dofile" => out.push(Directive::Ignored {
            kind: "include",
            detail: "dofile(…): Omarchy's bootstrap sets package.path, which the module search path already models".into(),
        }),
        // Every other call, named, so that nothing this reader meets
        // vanishes without a line: `disabled_input_device("touchpad")`
        // re-applies a persisted touchpad disable under Hyprland, and a
        // user who sees it dropped here knows why the touchpad is on.
        _ => out.push(Directive::Ignored {
            kind: "lua-call",
            detail: format!("{path}(…): not a configuration call this reader reads"),
        }),
    }
}

/// Omarchy's module whose function re-applies a persisted device disable.
const DISABLED_INPUT_DEVICE: &str = "default.hypr.disabled-input-device";

/// Whether `path` is a name bound by `require` to Omarchy's
/// `disabled-input-device` module, which returns the one function it holds.
fn is_disabled_input_device(path: &str, env: &Env) -> bool {
    matches!(
        env.get(path),
        Some(Value::Call { path: callee, args })
            if callee == "require" && matches!(args.first(), Some(Value::Str(module)) if module == DISABLED_INPUT_DEVICE)
    )
}

/// An `hl.device` table as a [`Directive::Device`]. The name has to be a
/// string written in the file, and a table with any value known only at
/// runtime is refused whole: half of a rule could reach a device the whole
/// rule never named.
fn device_rule(fields: &[(Option<String>, Value)]) -> Directive {
    let refuse = |why: String| Directive::Ignored { kind: "device", detail: format!("hl.device({{ … }}) refused whole: {why}") };
    let mut name = None;
    let mut settings = Vec::new();
    for (key, value) in fields {
        let Some(key) = key else {
            return refuse("a positional field is not a device setting".into());
        };
        match (key.as_str(), value) {
            ("name", Value::Str(text)) => name = Some(text.clone()),
            ("name", other) => return refuse(format!("the name {} is not a string in the file", describe(other))),
            (_, value) => match property_text(value) {
                Some(text) => settings.push((key.clone(), text)),
                None => return refuse(format!("{key} = {} is computed at runtime", describe(value))),
            },
        }
    }
    match name {
        Some(name) => Directive::Device { name, settings },
        None => refuse("a device rule needs the device's name".into()),
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
            if let Value::Table(nested) = value {
                if key == "touchpad" {
                    for (nested_key, nested_value) in nested {
                        let Some(nested_key) = nested_key else { continue };
                        if !matches!(nested_value, Value::Table(_)) {
                            out.push(input_setting(format!("touchpad:{nested_key}"), nested_value));
                        }
                    }
                } else {
                    out.push(Directive::Ignored {
                        kind: "input",
                        detail: format!("nested input setting {key} is not implemented"),
                    });
                }
            } else {
                out.push(input_setting(key.clone(), value));
            }
        }
    } else if input.is_some() {
        out.push(Directive::Ignored {
            kind: "input",
            detail: "hl.config input table is unreadable".into(),
        });
    }
    match root.iter().find(|(key, _)| key.as_deref() == Some("cursor")).map(|(_, value)| value) {
        Some(Value::Table(fields)) => {
            for (key, value) in fields {
                let Some(key) = key else { continue };
                out.push(match (value, property_text(value)) {
                    (Value::Table(_), _) => Directive::Ignored {
                        kind: "cursor",
                        detail: format!("nested cursor setting {key} is not implemented"),
                    },
                    (_, Some(value)) => Directive::Cursor { name: key.clone(), value },
                    (_, None) => Directive::Ignored {
                        kind: "cursor",
                        detail: format!("{key} = {}: computed at runtime, not carried over", describe(value)),
                    },
                });
            }
        }
        Some(_) => out.push(Directive::Ignored {
            kind: "cursor",
            detail: "hl.config cursor table is unreadable".into(),
        }),
        None => {}
    }
    // `general.layout` is the one key of Hyprland's look that is not a
    // look: it decides whether windows tile at all, and this desktop
    // has a style for each name it takes. The rest of `general` stays
    // Hyprland's.
    match root.iter().find(|(key, _)| key.as_deref() == Some("general")).map(|(_, value)| value) {
        Some(Value::Table(fields)) => {
            for (key, value) in fields {
                if key.as_deref() != Some("layout") {
                    continue;
                }
                out.push(match as_string(value) {
                    Some(layout) => Directive::DefaultLayout { layout },
                    None => Directive::Ignored {
                        kind: "layout",
                        detail: format!("general.layout = {}: computed at runtime, not carried over", describe(value)),
                    },
                });
            }
        }
        Some(_) => out.push(Directive::Ignored {
            kind: "config",
            detail: "hl.config general table is unreadable".into(),
        }),
        None => {}
    }
    if root.iter().any(|(key, _)| !matches!(key.as_deref(), Some("input" | "cursor"))) {
        out.push(Directive::Ignored {
            kind: "config",
            detail: "hl.config settings outside input, cursor and general.layout are not carried over".into(),
        });
    }
}

/// `hl.workspace_rule({ workspace = "N", layout = "…" })` — the one
/// line `omarchy-hyprland-workspace-layout-toggle` writes per
/// workspace. Only the layout of a numbered workspace is read. Every
/// other key earns its own line, and a selector that is not a number
/// from 1 to 99 — a special workspace above all — is named and left.
fn workspace_rule(value: &Value, out: &mut Vec<Directive>) {
    let Value::Table(fields) = value else {
        out.push(Directive::Ignored {
            kind: "workspace-rule",
            detail: format!("hl.workspace_rule({}): not a table of workspace settings", describe(value)),
        });
        return;
    };
    for (key, value) in fields {
        match key.as_deref() {
            Some("workspace" | "layout") => {}
            Some(key) => out.push(Directive::Ignored {
                kind: "workspace-rule",
                detail: format!("hl.workspace_rule(…) {key} = {}: only layout is read", describe(value)),
            }),
            None => out.push(Directive::Ignored {
                kind: "workspace-rule",
                detail: format!("hl.workspace_rule(…) with a positional value {}", describe(value)),
            }),
        }
    }
    let field = |name: &str| fields.iter().find(|(key, _)| key.as_deref() == Some(name)).map(|(_, value)| value);
    let workspace = field("workspace");
    let Some(selector) = workspace.and_then(as_string) else {
        out.push(Directive::Ignored {
            kind: "workspace-rule",
            detail: format!(
                "hl.workspace_rule(…): no readable workspace ({})",
                workspace.map(describe).unwrap_or_else(|| "missing".into())
            ),
        });
        return;
    };
    // Quoted and cut to a log line's length; the judgement below is
    // made on the whole selector.
    let shown = describe(&Value::Str(selector.clone()));
    let Some(layout) = field("layout") else {
        out.push(Directive::Ignored {
            kind: "workspace-rule",
            detail: format!("hl.workspace_rule(workspace = {shown}): names no layout"),
        });
        return;
    };
    let Some(layout) = as_string(layout) else {
        out.push(Directive::Ignored {
            kind: "workspace-rule",
            detail: format!(
                "hl.workspace_rule(workspace = {shown}): layout = {} is computed at runtime",
                describe(layout)
            ),
        });
        return;
    };
    match super::directive::workspace_number(&selector) {
        Ok(workspace) => out.push(Directive::WorkspaceLayout { workspace, layout }),
        Err(why) => out.push(Directive::Ignored {
            kind: "workspace-rule",
            detail: format!("hl.workspace_rule(workspace = {shown}): {why}"),
        }),
    }
}

/// One scalar `input` setting, or a named skip when its value is only
/// known at runtime. Omarchy 4's `kb_layout = vconsole.XKBLAYOUT or
/// "us"` is exactly that, and the text of the expression is not a
/// layout: libxkbcommon rejects it, and the session loses every option
/// that came with the keymap.
fn input_setting(name: String, value: &Value) -> Directive {
    match property_text(value) {
        Some(value) => Directive::Input { name, value },
        None => Directive::Ignored {
            kind: "input",
            detail: format!(
                "{name} = {}: computed at runtime, not carried over",
                describe(value)
            ),
        },
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
        // Both axes in the classic spelling, so `super::dispatch` makes
        // the one judgement for both syntaxes; a table missing either
        // keeps the empty argument it refuses.
        "window.fullscreen_state" => {
            let level = |name: &str| match field(name) {
                Some(Value::Num(n)) if n.is_finite() => Some(format_number(*n)),
                _ => None,
            };
            match (level("internal"), level("client")) {
                (Some(internal), Some(client)) => verb("fullscreenstate", format!("{internal} {client}")),
                _ => verb("fullscreenstate", String::new()),
            }
        }
        "window.pseudo" => verb("pseudo", String::new()),
        "window.float" => verb("togglefloating", String::new()),
        "window.pin" => verb("pin", String::new()),
        "window.swap" => verb("swapwindow", text("direction")),
        // `x` and `y` are a delta with `relative = true`, and an exact
        // size without it: Omarchy's `omarchy-hyprland-window-pop` pairs
        // the bare form with the classic `resizeactive exact`. Both are
        // carried in the classic spelling so that `super::dispatch`
        // makes the one judgement for both syntaxes, and a table this
        // cannot read keeps the empty argument it refuses.
        "window.resize" => {
            let number = |name: &str| match field(name) {
                Some(Value::Num(n)) if n.is_finite() => Some(format_number(*n)),
                _ => None,
            };
            match (number("x"), number("y")) {
                (Some(x), Some(y)) if matches!(field("relative"), Some(Value::Bool(true))) => {
                    verb("resizeactive", format!("{x} {y}"))
                }
                (Some(x), Some(y)) => verb("resizeactive", format!("exact {x} {y}")),
                _ => verb("resizeactive", String::new()),
            }
        }
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
///
/// A matcher or property whose value is only known at runtime refuses
/// the whole rule. A rule missing its class matcher would match every
/// window, and one missing a property is some other rule.
fn window_rule(match_arg: &Value, rules: &Value) -> Result<WindowRule, String> {
    let runtime = |key: &str, value: &Value| {
        format!("{key} = {} is computed at runtime", describe(value))
    };
    let mut rule = WindowRule::default();
    let mut push_match = |key: &str, value: &Value| {
        let text = as_string(value).ok_or_else(|| runtime(&format!("match {key}"), value))?;
        rule.matchers.push(match key {
            "class" => Matcher::Class(text),
            "title" => Matcher::Title(text),
            "tag" => Matcher::Tag(text),
            other => Matcher::Other {
                key: other.to_string(),
                value: text,
            },
        });
        Ok::<(), String>(())
    };
    match match_arg {
        Value::Str(class) => push_match("class", &Value::Str(class.clone()))?,
        Value::Table(fields) => {
            for (key, value) in fields {
                if let Some(key) = key {
                    push_match(key, value)?;
                }
            }
        }
        // `hl.window_rule` has no match argument of its own.
        Value::Nil => {}
        other => return Err(runtime("match", other)),
    }
    if let Value::Table(fields) = rules {
        for (key, value) in fields {
            let Some(key) = key else { continue };
            if key == "match" {
                let Value::Table(inner) = value else {
                    return Err(runtime("match", value));
                };
                for (key, value) in inner {
                    if let Some(key) = key {
                        push_match(key, value)?;
                    }
                }
                continue;
            }
            let text = property_text(value).ok_or_else(|| runtime(key, value))?;
            rule.props.push((key.clone(), text));
        }
    }
    Ok(rule)
}

/// A rule property's value as the conf syntax would spell it, so the
/// two front ends hand `super::rules` the same strings: `size = { 875,
/// 600 }` and `size 875 600` both become `"875 600"`.
///
/// `None` for anything that is not yet a value — a name, a call, an
/// unresolved expression. Its source text is not a setting, and a
/// plausible-looking string is worse than a refusal.
fn property_text(value: &Value) -> Option<String> {
    match value {
        Value::Str(text) => Some(text.clone()),
        Value::Num(n) => Some(format_number(*n)),
        Value::Bool(true) => Some("on".into()),
        Value::Bool(false) => Some("off".into()),
        Value::Table(items) => items
            .iter()
            .map(|(_, v)| property_text(v))
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join(" ")),
        _ => None,
    }
}

/// An `hl.monitor` table as a [`Monitor`]. A field that is present but
/// only known at runtime refuses the whole line, the way the lowering
/// already refuses a line it only half understands.
fn monitor_from(fields: &[(Option<String>, Value)]) -> Result<Monitor, String> {
    let text = |key: &str, value: &Value| {
        property_text(value)
            .ok_or_else(|| format!("{key} = {} is computed at runtime", describe(value)))
    };
    let field = |name: &str| match fields.iter().find(|(k, _)| k.as_deref() == Some(name)) {
        Some((_, value)) => text(name, value),
        None => Ok(String::new()),
    };
    let mut extra = Vec::new();
    for (key, value) in fields {
        let Some(key) = key else { continue };
        if !matches!(key.as_str(), "output" | "mode" | "position" | "scale") {
            // Two separate tokens, not one joined "key value" string: the
            // conf front end splits `monitor = …, transform, N` on commas
            // (`conf.rs`'s `fields.iter().skip(4)`), and wm-wayland's
            // `monitor_transform()` only recognizes that exact
            // `["transform", "N"]` shape. A single combined string here
            // silently refused every Lua-configured rotation as an
            // unsupported field.
            extra.push(key.clone());
            extra.push(text(key, value)?);
        }
    }
    Ok(Monitor {
        output: field("output")?,
        mode: field("mode")?,
        position: field("position")?,
        scale: field("scale")?,
        extra,
    })
}

// ---- budgets ----------------------------------------------------------

/// Counts one step of a read's walk, and answers whether the read has
/// spent its budget of steps or of directives — saying so once, the
/// first time. `usize::MAX` marks a read already cut off, so every
/// enclosing loop and block stops without another line.
fn spent(steps: &mut usize, out: &mut Vec<Directive>) -> bool {
    if *steps == usize::MAX {
        return true;
    }
    *steps += 1;
    let detail = if *steps > MAX_STEPS {
        format!(
            "more than {MAX_STEPS} statements walked in one file, counting every pass of a loop; the rest is not read"
        )
    } else if out.len() >= MAX_DIRECTIVES {
        format!("more than {MAX_DIRECTIVES} directives from one file; the rest is not read")
    } else {
        return false;
    };
    *steps = usize::MAX;
    out.push(Directive::Ignored { kind: "lua", detail });
    true
}

/// A value about to be bound to `name`, or an opaque stand-in, said
/// so, when it is too big to keep.
fn bind(name: &str, value: Value, out: &mut Vec<Directive>) -> Value {
    let mut weight = 0;
    if fits(&value, 0, &mut weight) {
        return value;
    }
    out.push(Directive::Ignored {
        kind: "lua",
        detail: format!("{name} = …: a value too large to follow"),
    });
    Value::Opaque("a value too large to follow".into())
}

/// Whether `value` is within [`MAX_VALUE_WEIGHT`] and
/// [`MAX_VALUE_DEPTH`]. Stops at the first bound crossed, so checking a
/// value costs no more than keeping one.
fn fits(value: &Value, depth: u32, weight: &mut usize) -> bool {
    *weight += match value {
        Value::Str(text) | Value::Name(text) | Value::Opaque(text) => 1 + text.len() / 64,
        _ => 1,
    };
    if *weight > MAX_VALUE_WEIGHT || depth > MAX_VALUE_DEPTH {
        return false;
    }
    match value {
        Value::Table(fields) => fields.iter().all(|(_, value)| fits(value, depth + 1, weight)),
        Value::Call { args, .. } => args.iter().all(|value| fits(value, depth + 1, weight)),
        Value::Binary { left, right, .. }
        | Value::Or(left, right)
        | Value::And(left, right)
        | Value::Compare { left, right, .. } => {
            fits(left, depth + 1, weight) && fits(right, depth + 1, weight)
        }
        Value::Not(operand) => fits(operand, depth + 1, weight),
        // A function's body is shared, not copied, by binding it.
        _ => true,
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
        Value::Or(left, right) => fold("or", eval(left, env), eval(right, env)),
        Value::And(left, right) => fold("and", eval(left, env), eval(right, env)),
        Value::Compare { op, left, right } => fold(op, eval(left, env), eval(right, env)),
        Value::Not(operand) => negate(eval(operand, env)),
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
        Value::Binary { .. }
        | Value::Or(..)
        | Value::And(..)
        | Value::Not(_)
        | Value::Compare { .. } => render(value),
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
    /// Operators built by the statement being parsed. See
    /// [`MAX_OPERATORS`].
    operators: usize,
    /// The bound, if any, one of the current statement's expressions
    /// hit. Such a statement is refused whole: a condition, a chord or a
    /// command with its tail cut off is not some other one.
    trouble: Option<&'static str>,
    /// The `local`s in scope, with how many declarations of each name
    /// are live, and the order they were declared in so that a block
    /// can end its own. Scope is what decides whether `x = …` is to a
    /// global.
    scope: std::collections::BTreeMap<String, usize>,
    declared: Vec<String>,
    /// Names something this parser skipped over could assign. See
    /// [`Globals::unknown`].
    hidden: std::collections::BTreeSet<String>,
    /// Whether [`MAX_STATEMENTS`] has been reached, and said.
    capped: bool,
}

impl Parser {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            at: 0,
            statements: 0,
            operators: 0,
            trouble: None,
            scope: Default::default(),
            declared: Vec::new(),
            hidden: Default::default(),
            capped: false,
        }
    }

    /// The stand-in for an expression past [`MAX_DEPTH`]. Like
    /// [`Parser::operator`], it keeps the first bound a statement hit:
    /// what is left after one bound often trips the other.
    fn too_deep(&mut self) -> Value {
        self.trouble.get_or_insert("expression nested too deeply");
        Value::Opaque("an expression nested too deeply".into())
    }

    /// One more operator for the current statement, built by `build`
    /// unless the statement has already built its share.
    fn operator(&mut self, build: impl FnOnce() -> Value) -> Value {
        self.operators += 1;
        if self.operators > MAX_OPERATORS {
            self.trouble.get_or_insert("expression too long");
            return Value::Opaque("an expression too long to read".into());
        }
        build()
    }

    /// Declares a `local`, in scope until the block declaring it ends.
    fn declare(&mut self, name: String) {
        *self.scope.entry(name.clone()).or_default() += 1;
        self.declared.push(name);
    }

    /// Ends the scope of every `local` declared since `mark`.
    fn undeclare(&mut self, mark: usize) {
        let mark = mark.min(self.declared.len());
        for name in self.declared.drain(mark..) {
            if let Some(count) = self.scope.get_mut(&name) {
                *count -= 1;
                if *count == 0 {
                    self.scope.remove(&name);
                }
            }
        }
    }

    /// A plain `name = value`, scoped as Lua scopes it: to the `local`
    /// of that name when one is in scope, and to the global otherwise.
    fn assignment(&self, name: String, value: Value) -> Stmt {
        let in_scope = |path: &str| {
            self.scope
                .contains_key(path.split('.').next().unwrap_or_default())
        };
        if let Some(global) = name.strip_prefix("_G.") {
            if !in_scope(global) {
                return Stmt::Assign {
                    name: global.to_string(),
                    value,
                };
            }
            return Stmt::Assign { name, value };
        }
        if in_scope(&name) {
            Stmt::Local { name, value }
        } else {
            Stmt::Assign { name, value }
        }
    }

    /// Records what a statement refused whole would have assigned, its
    /// own `local`s included: they are in scope after it, bound to
    /// nothing this reader read.
    fn note_hidden(&mut self, stmt: &Stmt) {
        let mut names = Globals::default();
        unwalked(std::slice::from_ref(stmt), &mut names);
        self.hidden.append(&mut names.unknown);
        let declared = match stmt {
            Stmt::Block(stmts) => stmts.as_slice(),
            other => std::slice::from_ref(other),
        };
        for stmt in declared {
            if let Stmt::Local { name, .. } = stmt {
                self.hidden.insert(name.clone());
            }
        }
    }

    /// `a, b, c`: the names a `local`, a `for` or a parameter list
    /// declares, with Lua 5.4's `<const>` and `<close>` attributes
    /// passed over.
    fn names(&mut self) -> Vec<String> {
        let mut names = Vec::new();
        loop {
            let name = self.take_word();
            if name.is_empty() {
                return names;
            }
            names.push(name);
            self.trivia();
            if self.peek() == Some('<') {
                while self.bump().is_some_and(|ch| ch != '>') {}
                self.trivia();
            }
            if self.peek() != Some(',') {
                return names;
            }
            self.at += 1;
        }
    }

    /// `a, b, c`: the values an assignment lists.
    fn values(&mut self) -> Vec<Value> {
        let mut values = vec![self.expr(0)];
        loop {
            self.trivia();
            if self.peek() != Some(',') {
                return values;
            }
            self.at += 1;
            values.push(self.expr(0));
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
        // Each statement gets its own operator budget and its own
        // record of a bound hit. A function body sits inside an
        // expression, so the enclosing statement's are put back after.
        let enclosing = (self.operators, self.trouble.take());
        // And a block is a scope: the `local`s it declares end with it.
        let mark = self.declared.len();
        let mut body = Vec::new();
        loop {
            self.trivia();
            if self.eof() {
                break;
            }
            if self.statements >= MAX_STATEMENTS {
                // Said once, by the innermost block to reach it; every
                // enclosing block stops too. The rest of the file goes
                // unread, so any name may be set in it.
                if !self.capped {
                    self.capped = true;
                    self.hidden.insert(ANY_NAME.into());
                    body.push(Stmt::Skipped("statements past the per-file limit"));
                }
                break;
            }
            let word = self.peek_word();
            match word.as_str() {
                "end" | "else" | "elseif" | "until" => break,
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
            self.operators = 0;
            let stmt = self.statement(&word, depth);
            if let Some(why) = self.trouble.take() {
                if let Some(stmt) = &stmt {
                    self.note_hidden(stmt);
                }
                body.push(Stmt::Skipped(why));
            } else if let Some(stmt) = stmt {
                body.push(stmt);
            }
        }
        self.undeclare(mark);
        (self.operators, self.trouble) = enclosing;
        body
    }

    fn statement(&mut self, word: &str, depth: u32) -> Option<Stmt> {
        if depth > MAX_DEPTH {
            self.skip_to_end();
            return Some(Stmt::Skipped("deeply nested"));
        }
        match word {
            "local" => {
                self.take_word();
                // `local function f() … end`: `f` is in scope from here
                // on, its own body included.
                if self.peek_word() == "function" {
                    self.take_word();
                    let name = self.take_word();
                    self.declare(name);
                    self.skip_to_end();
                    return Some(Stmt::Skipped("function"));
                }
                let names = self.names();
                self.trivia();
                let values = if self.peek() == Some('=') && self.peek_at(1) != Some('=') {
                    self.at += 1;
                    self.values()
                } else {
                    Vec::new()
                };
                // Declared after the values are read: in `local x = x`,
                // the `x` on the right is whichever was already in scope.
                let stmt = assignments(names.clone(), values, |name, value| Stmt::Local {
                    name,
                    value,
                });
                for name in names {
                    self.declare(name);
                }
                stmt
            }
            "for" => {
                self.take_word();
                let vars = self.names();
                let var = vars.first().cloned().unwrap_or_default();
                self.trivia();
                // A loop's variables are in scope for its body only.
                let mark = self.declared.len();
                if self.peek() != Some('=') {
                    if self.take_word() != "in" {
                        self.skip_to_end();
                        return Some(Stmt::Skipped("generic for"));
                    }
                    let values = self.expr(0);
                    if self.take_word() != "do" {
                        self.skip_to_end();
                        return Some(Stmt::Skipped("generic for"));
                    }
                    for name in &vars {
                        self.declare(name.clone());
                    }
                    let body = self.block(depth + 1);
                    self.undeclare(mark);
                    self.take_word();
                    return Some(Stmt::GenericFor {
                        var: vars.last().cloned().unwrap_or_default(),
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
                self.declare(var.clone());
                let body = self.block(depth + 1);
                self.undeclare(mark);
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
                if self.take_word() != "then" {
                    // Something between the condition and `then` was no
                    // part of an expression this reader knows, so the
                    // condition is not what was read. The whole `if` is
                    // skipped rather than the rest of the line walked as
                    // though it were the body.
                    self.skip_to_end();
                    return Some(Stmt::Skipped("if with an unreadable condition"));
                }
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
                // one or more names. Both start with a name path.
                let path = self.take_path();
                if path.is_empty() {
                    self.at += 1;
                    return None;
                }
                self.trivia();
                let single = self.peek() == Some('=') && self.peek_at(1) != Some('=');
                if single || self.peek() == Some(',') {
                    let mut targets = vec![path];
                    while self.peek() == Some(',') {
                        self.at += 1;
                        targets.push(self.take_path());
                        self.trivia();
                    }
                    if self.peek() != Some('=') || self.peek_at(1) == Some('=') {
                        return None;
                    }
                    self.at += 1;
                    let values = self.values();
                    targets.retain(|target| !target.is_empty());
                    return assignments(targets, values, |name, value| self.assignment(name, value));
                }
                match self.peek() {
                    // A call whose result is indexed or called again
                    // (`hl.bind(…):unbind()`) is still the outer call
                    // for our purposes; the tail is skipped.
                    Some('(') => Some(Stmt::Call {
                        path,
                        args: self.call_args(),
                    }),
                    // `require "x"` and `f{…}`: Lua's parenthesis-free
                    // call forms.
                    Some('"') | Some('\'') => Some(Stmt::Call {
                        path,
                        args: vec![Value::Str(self.string())],
                    }),
                    Some('{') => Some(Stmt::Call {
                        path,
                        args: vec![self.table(depth + 1)],
                    }),
                    _ => None,
                }
            }
        }
    }

    /// A `function(…) … end` expression, body parsed. The `function`
    /// keyword has been consumed.
    fn function_body(&mut self, depth: u32) -> Value {
        self.trivia();
        // The parameters are locals of the body, bound to what only a
        // caller could supply. This reader never calls a function, so in
        // the one body it walks a parameter must not read as an unset
        // global's `nil`.
        let mut params = Vec::new();
        if self.peek() == Some('(') {
            self.at += 1;
            params = self.names();
            // Whatever is left of the list, `...` included.
            while self.bump().is_some_and(|ch| ch != ')') {}
        }
        if depth > MAX_DEPTH {
            self.skip_to_end();
            self.too_deep();
            return Value::Function(Default::default());
        }
        let mark = self.declared.len();
        let mut body: Vec<Stmt> = params
            .iter()
            .map(|name| Stmt::Local {
                name: name.clone(),
                value: Value::Opaque(PARAMETER.into()),
            })
            .collect();
        for name in params {
            self.declare(name);
        }
        body.extend(self.block(depth + 1));
        self.undeclare(mark);
        self.take_word();
        Value::Function(body.into())
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
                if self.take_word() != "then" {
                    // As for `if`. The skip takes the chain's one `end`.
                    self.skip_to_end();
                    return vec![Stmt::Skipped("if with an unreadable condition")];
                }
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
    /// so an `end` inside a string literal cannot unbalance it, and a
    /// `repeat` closes at its `until`.
    ///
    /// On the way it records in [`Parser::hidden`] every name the
    /// skipped text could assign: the `a` of `a =` and of `a, b =` that
    /// no `local` or `for` introduces, and the `a` of `_G.a =`. It reads
    /// tokens rather than parsing, so where it errs it records a name
    /// too many, which costs an answer and never gives a wrong one.
    fn skip_to_end(&mut self) {
        const KEYWORDS: &[&str] = &[
            "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto",
            "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
            "while",
        ];
        let mut depth = 1i32;
        // The current `a, b` run of names; whether a `local` or `for`
        // introduced it (or is about to); whether the last token was a
        // comma; and, after a `.`, whether the path so far is `_G`.
        let mut run: Vec<String> = Vec::new();
        let mut declared = false;
        let mut introducing = false;
        let mut comma = false;
        let mut field: Option<bool> = None;
        while !self.eof() {
            self.trivia();
            match (self.peek(), self.peek_at(1)) {
                (None, _) => return,
                (Some('"' | '\''), _) => {
                    let _ = self.string();
                    run.clear();
                    continue;
                }
                (Some('['), Some('[')) => {
                    let _ = self.long_string();
                    run.clear();
                    continue;
                }
                _ => {}
            }
            let word = self.peek_word();
            if word.is_empty() {
                let before = self.at.checked_sub(1).and_then(|at| self.chars.get(at)).copied();
                match self.bump() {
                    Some(',') => {
                        comma = true;
                        continue;
                    }
                    // `a.b`: `b` is a field of `a`, unless `a` is `_G`.
                    // Two dots are concatenation.
                    Some('.') if self.peek() != Some('.') && before != Some('.') => {
                        field = Some(run.pop().as_deref() == Some("_G"));
                        continue;
                    }
                    Some('=')
                        if !declared
                            && self.peek() != Some('=')
                            && !matches!(before, Some('=' | '~' | '<' | '>')) =>
                    {
                        self.hidden.extend(run.drain(..));
                    }
                    _ => {}
                }
                run.clear();
                (declared, introducing, comma, field) = (false, false, false, None);
                continue;
            }
            self.at += word.chars().count();
            match word.as_str() {
                // `do` closes nothing of its own when it opens a
                // `for`/`while` body, which has already been counted;
                // counting it again would swallow the enclosing `end`.
                // Only a *bare* `do` opens a block, and Omarchy writes
                // none.
                "function" | "if" | "for" | "while" | "repeat" => depth += 1,
                "end" | "until" => {
                    depth -= 1;
                    if depth <= 0 {
                        return;
                    }
                }
                _ => {}
            }
            let name = !KEYWORDS.contains(&word.as_str())
                && !word.starts_with(|c: char| c.is_ascii_digit());
            match field.take() {
                // `_G.a`
                Some(true) if name => run.push(word),
                // `a.b`: a field, not a name
                Some(_) => {}
                None if name && comma => run.push(word),
                None if name => {
                    run = vec![word];
                    declared = introducing;
                }
                None => {
                    run.clear();
                    declared = false;
                    introducing = matches!(word.as_str(), "local" | "for");
                    comma = false;
                    continue;
                }
            }
            introducing = false;
            comma = false;
        }
    }

    /// An expression, at Lua's precedence: `or` binds loosest, then
    /// `and`, the comparisons, `..`, `+ -`, `* / // %`, the unary
    /// operators, and `^` tightest. Each binary level is a loop rather
    /// than a recursion, so a long chain costs iterations, not stack.
    fn expr(&mut self, depth: u32) -> Value {
        if depth > MAX_DEPTH {
            return self.too_deep();
        }
        self.binary(0, depth)
    }

    /// One level of [`PRECEDENCE`], folded from the left. `..` is
    /// right-associative in Lua, and folding it from the left makes the
    /// same string.
    fn binary(&mut self, level: usize, depth: u32) -> Value {
        let Some(operators) = PRECEDENCE.get(level) else {
            return self.unary(depth);
        };
        let mut left = self.binary(level + 1, depth);
        while let Some(op) = self.binary_operator(operators) {
            let right = self.binary(level + 1, depth);
            left = self.operator(|| fold(op, left, right));
        }
        left
    }

    /// The operator at the cursor, consumed, when it is one of
    /// `operators`. `trivia` has already eaten comments, so a `-` here
    /// is arithmetic.
    fn binary_operator(&mut self, operators: &[&'static str]) -> Option<&'static str> {
        let save = self.at;
        self.trivia();
        for &op in operators {
            let found = if op.starts_with(char::is_alphabetic) {
                self.peek_word() == op
            } else {
                op.chars()
                    .enumerate()
                    .all(|(i, ch)| self.peek_at(i) == Some(ch))
                    // `...` is a value, not `..` and a stray dot.
                    && !(op == ".." && self.peek_at(2) == Some('.'))
            };
            if found {
                self.at += op.len();
                return Some(op);
            }
        }
        self.at = save;
        None
    }

    /// `not`, `-` and `#`, then [`Parser::power`].
    fn unary(&mut self, depth: u32) -> Value {
        self.trivia();
        let op = if self.peek_word() == "not" {
            "not"
        } else {
            match self.peek() {
                Some('-') => "-",
                Some('#') => "#",
                _ => return self.power(depth),
            }
        };
        self.at += op.len();
        // Unary operators recurse without passing through `expr`, so
        // they check the depth themselves, after consuming the operator
        // so that progress is still guaranteed.
        if depth > MAX_DEPTH {
            return self.too_deep();
        }
        match (op, self.unary(depth + 1)) {
            // A negative literal is a number, not an operator.
            ("-", Value::Num(n)) => Value::Num(-n),
            (op, operand) => self.operator(|| match op {
                "not" => negate(operand),
                "-" => fold("-", Value::Num(0.0), operand),
                _ => Value::Opaque(format!("#{}", render(&operand))),
            }),
        }
    }

    /// A primary expression, raised to a power if one follows. `^` is
    /// right-associative and binds tighter than a unary operator on its
    /// left but not one on its right: `-x^2` is `-(x^2)`, and `2^-1`
    /// is `2^(-1)`.
    fn power(&mut self, depth: u32) -> Value {
        let base = self.primary(depth);
        let save = self.at;
        self.trivia();
        if self.peek() != Some('^') {
            self.at = save;
            return base;
        }
        self.at += 1;
        if depth > MAX_DEPTH {
            return self.too_deep();
        }
        let exponent = self.unary(depth + 1);
        self.operator(|| fold("^", base, exponent))
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
            Some(ch) if ch.is_ascii_digit() => self.number(),
            Some(ch) if ch.is_alphabetic() || ch == '_' => {
                let path = self.take_path();
                match path.as_str() {
                    "true" => return Value::Bool(true),
                    "false" => return Value::Bool(false),
                    "nil" => return Value::Nil,
                    "function" => return self.function_body(depth),
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
            self.too_deep();
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

/// An operator applied to the values this reader can resolve, and kept,
/// unevaluated, over the ones it cannot resolve yet.
fn fold(op: &'static str, left: Value, right: Value) -> Value {
    let keep = |left: Value, right: Value| Value::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
    };
    match op {
        ".." => match (as_string(&left), as_string(&right)) {
            (Some(a), Some(b)) => Value::Str(format!("{a}{b}")),
            // Not resolvable *yet*. Both halves are kept so a later
            // `eval`, with a loop variable bound, can finish the job —
            // this is the deferral `Value::Binary` exists for. A
            // concatenation still unresolved when a binding asks for it
            // is refused there, whole: half a key spec is not a key
            // spec.
            _ => keep(left, right),
        },
        "+" | "-" | "*" | "/" | "//" | "%" | "^" => match (as_number(&left), as_number(&right)) {
            (Some(a), Some(b)) => Value::Num(match op {
                "+" => a + b,
                "-" => a - b,
                "*" => a * b,
                "/" => a / b,
                "//" => (a / b).floor(),
                "%" => a - (a / b).floor() * b,
                _ => a.powf(b),
            }),
            _ => keep(left, right),
        },
        "or" => match plain_truth(&left) {
            Some(true) => left,
            Some(false) => right,
            None => Value::Or(Box::new(left), Box::new(right)),
        },
        "and" => match plain_truth(&left) {
            Some(true) => right,
            Some(false) => left,
            None => Value::And(Box::new(left), Box::new(right)),
        },
        _ => match compare(op, &left, &right) {
            Some(answer) => Value::Bool(answer),
            None => Value::Compare {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
        },
    }
}

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Num(n) => Some(*n),
        Value::Str(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Lua truthiness, for a value that is already a value.
fn plain_truth(value: &Value) -> Option<bool> {
    match value {
        Value::Nil => Some(false),
        Value::Bool(b) => Some(*b),
        Value::Str(_) | Value::Num(_) | Value::Table(_) | Value::Function(_) => Some(true),
        _ => None,
    }
}

/// `not value`.
fn negate(value: Value) -> Value {
    match plain_truth(&value) {
        Some(truthy) => Value::Bool(!truthy),
        None => Value::Not(Box::new(value)),
    }
}

/// `a, b = x, y` as one statement per name, each made by `make`. A name
/// past the last value is `nil`, unless that value is a call, whose
/// further results only running it would give.
fn assignments(
    names: Vec<String>,
    values: Vec<Value>,
    make: impl Fn(String, Value) -> Stmt,
) -> Option<Stmt> {
    let spread = matches!(values.last(), Some(Value::Call { .. }));
    let mut values = values.into_iter();
    let mut stmts: Vec<Stmt> = names
        .into_iter()
        .map(|name| {
            let value = values.next().unwrap_or_else(|| {
                if spread {
                    Value::Opaque("a further result of a call".into())
                } else {
                    Value::Nil
                }
            });
            make(name, value)
        })
        .collect();
    if stmts.len() > 1 {
        Some(Stmt::Block(stmts))
    } else {
        stmts.pop()
    }
}

/// A value rendered back to something resembling its source, for log
/// lines.
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
        Value::Binary { op, left, right } | Value::Compare { op, left, right } => {
            format!("{} {op} {}", render(left), render(right))
        }
        Value::Or(left, right) => format!("{} or {}", render(left), render(right)),
        Value::And(left, right) => format!("{} and {}", render(left), render(right)),
        Value::Not(operand) => format!("not {}", render(operand)),
    }
}
