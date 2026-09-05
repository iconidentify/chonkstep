//! Hyprland's spelling of a chord, turned into chonkstep's.
//!
//! Both syntaxes name the same chord differently from this config
//! format — `SUPER SHIFT, RETURN` in conf, `"SUPER + SHIFT + RETURN"`
//! in Lua, `super+shift+return` here — and all three name the same two
//! modifiers and the same key. So this module does not build a
//! [`wm_core::KeyCombo`] itself: it *rewrites the spelling* and hands the result
//! to [`crate::parse_key`], the parser a user's own `[keybindings]`
//! entries go through.
//!
//! That indirection is the point, and it is the same argument
//! `preset::omarchy_keybindings` makes for routing its constants
//! through `parse_key`: one parser means a chord read out of Omarchy's
//! file and a chord typed into `config.toml` cannot disagree about what
//! `super+ctrl+comma` is. It also means every keysym this reader can
//! reach is a keysym the config format documents, so nothing arrives
//! from Omarchy that a user cannot then rebind, unbind or look up.
//!
//! # Keycodes
//!
//! Omarchy binds nineteen chords by bare X keycode (`code:10`,
//! `SUPER + code:20`) because a keycode is layout-independent where a
//! keysym is not — `SUPER + 1` should be the first workspace on an
//! AZERTY keyboard too. Chonkstep's grab table is keysym-based, so a
//! keycode has to be resolved through *a* layout, and the only one
//! available at config-read time is the standard US mapping the
//! numbers were chosen against. [`KEYCODES`] is that mapping, and it
//! covers exactly the range Omarchy actually uses plus the rest of the
//! main block and the numeric keypad; a keycode outside it is refused
//! by name rather than guessed at.
//!
//! This is a real limitation and it is written down rather than
//! papered over: on a non-US layout `code:20` is whatever key sits
//! where `-` sits on a US board, and this reader will call it `minus`.
//! Which is what Omarchy's own comment on those bindings means by
//! choosing keycodes in the first place.

/// What a Hyprland chord turned into, when it did not turn into a
/// chonkstep key spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyTrouble {
    /// A mouse button, a scroll direction, or a hardware switch:
    /// `mouse:272`, `mouse_up`, `switch:on:Lid Switch`. Not a key, and
    /// this config format has no way to express one.
    NotAKey(String),
    /// A `code:N` outside [`KEYCODES`] — a keycode with no key on a
    /// standard board behind it (Omarchy's own `code:201` is one).
    UnknownKeycode(u32),
    /// A key name neither Hyprland's aliases nor `parse_key` knows.
    UnknownKey(String),
    /// A modifier name that is not one of Hyprland's.
    UnknownModifier(String),
    /// Nothing but modifiers, or nothing at all.
    NoKey,
}

impl KeyTrouble {
    /// The one-line reason, for a log line and for the docs table.
    pub fn reason(&self) -> String {
        match self {
            Self::NotAKey(what) => {
                format!("{what} is a pointer or switch binding, not a key chord")
            }
            Self::UnknownKeycode(code) => format!("code:{code} has no key on a standard layout"),
            Self::UnknownKey(name) => format!("no keysym named {name:?}"),
            Self::UnknownModifier(name) => format!("no modifier named {name:?}"),
            Self::NoKey => "modifiers with no key".to_string(),
        }
    }
}

/// Rewrites Hyprland's spelling of a chord into a [`crate::parse_key`]
/// spec, or says why it cannot.
///
/// Accepts every separator the two syntaxes use between modifiers —
/// space, `+`, `_` — because Hyprland accepts all three and a config
/// read from a real machine will contain more than one of them. The
/// last token is the key; everything before it is a modifier.
pub fn spec_for(keys: &str) -> Result<String, KeyTrouble> {
    let keys = keys.trim();
    // Pointer and switch bindings, refused whole: their whole token is
    // the diagnostic, so it is carried into the message rather than
    // reduced to "unknown key".
    let lower = keys.to_ascii_lowercase();
    if lower.starts_with("switch:")
        || lower.contains("mouse:")
        || lower.contains("mouse_up")
        || lower.contains("mouse_down")
    {
        return Err(KeyTrouble::NotAKey(keys.to_string()));
    }
    // `code:N` may carry modifiers before it, and the colon must not be
    // split on, so the tokenizer walks the string rather than using
    // `split(&[' ', '+', '_'])` — `_` appears inside `mouse_up` and
    // inside modifier names Hyprland spells `SUPER_SHIFT`.
    let mut tokens: Vec<String> = Vec::new();
    // A comma is in the list because the conf syntax separates the
    // modifier field from the key field with one (`SUPER SHIFT,
    // RETURN`). `conf::bind` already joins those two fields with a
    // space before calling here, so a comma should never arrive — but
    // a reader of somebody else's file gets no value from being brittle
    // about a separator it can obviously read.
    for chunk in keys.split(['+', ' ', '\t', ',']) {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        // `SUPER_SHIFT` is one chunk holding two modifiers, so `_`
        // has to be split on — but only as far as the modifiers go.
        // This used to split the whole chunk on the premise that "a
        // key name never contains `_`", which is false for every key
        // on the numeric keypad: `KP_Enter` became the two tokens
        // `KP` and `Enter`, and the line was refused with
        // `UnknownModifier("KP")` — the real reason a numpad binding
        // from a Hyprland config never worked, and a reason that
        // named the wrong thing.
        //
        // So the leading run of modifiers is peeled off and whatever
        // follows is one key token, underscores intact. The first
        // part that is not a modifier begins the key, which keeps
        // `SUPER_SHIFT` and `SUPER_KP_Enter` both reading correctly
        // and needs no list of which key names contain `_`.
        if chunk.contains('_') && !chunk.to_ascii_lowercase().starts_with("code:") {
            let parts: Vec<&str> = chunk.split('_').filter(|part| !part.is_empty()).collect();
            let leading_modifiers =
                parts.iter().take_while(|part| modifier_name(part).is_some()).count();
            tokens.extend(parts[..leading_modifiers].iter().map(|part| (*part).to_string()));
            if leading_modifiers < parts.len() {
                tokens.push(parts[leading_modifiers..].join("_"));
            }
        } else {
            tokens.push(chunk.to_string());
        }
    }
    let Some((key, mods)) = tokens.split_last() else {
        return Err(KeyTrouble::NoKey);
    };
    let mut spec = String::new();
    for token in mods {
        let name =
            modifier_name(token).ok_or_else(|| KeyTrouble::UnknownModifier(token.clone()))?;
        spec.push_str(name);
        spec.push('+');
    }
    // A chord that is only modifiers ("SUPER + SHIFT") lands here with
    // the last modifier as its "key"; caught rather than accepted so it
    // cannot become a binding on the `shift` keysym.
    if modifier_name(key).is_some() && !mods.is_empty() {
        return Err(KeyTrouble::NoKey);
    }
    spec.push_str(&key_name(key)?);
    Ok(spec)
}

/// Hyprland's modifier names, mapped to the ones `parse_key` reads.
/// Every alias Hyprland's own parser accepts is here, so a config that
/// spells the Windows key `MOD4` reads the same as one that spells it
/// `SUPER`.
fn modifier_name(token: &str) -> Option<&'static str> {
    match token.trim().to_ascii_lowercase().as_str() {
        "shift" => Some("shift"),
        "ctrl" | "control" => Some("ctrl"),
        "alt" | "mod1" => Some("alt"),
        "super" | "mod4" | "win" | "logo" => Some("super"),
        _ => None,
    }
}

/// One key token, as `parse_key` would spell it.
fn key_name(token: &str) -> Result<String, KeyTrouble> {
    let token = token.trim();
    if let Some(digits) = token.to_ascii_lowercase().strip_prefix("code:") {
        let code: u32 = digits
            .trim()
            .parse()
            .map_err(|_| KeyTrouble::UnknownKey(token.to_string()))?;
        return keycode_name(code)
            .map(str::to_string)
            .ok_or(KeyTrouble::UnknownKeycode(code));
    }
    let lower = token.to_ascii_lowercase();
    // The XF86 prefix and Hyprland's other long names, reduced to the
    // run-together names this format uses. Anything not aliased is
    // tried against `parse_key` as-is, which is what makes plain
    // letters, digits, `F1`, `RETURN` and `SPACE` work with no table.
    let aliased = XKEYSYM_ALIASES
        .iter()
        .find(|(from, _)| *from == lower)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or(lower);
    if crate::parse_key(&aliased).is_some() {
        Ok(aliased)
    } else {
        Err(KeyTrouble::UnknownKey(token.to_string()))
    }
}

/// The name for an X keycode on the standard US mapping — see the
/// module docs for why a layout has to be assumed and which one.
///
/// Only the keys `parse_key` has names for are listed: a keycode whose
/// key this format cannot name is as unbindable as a key name it does
/// not know, and saying so through [`KeyTrouble::UnknownKeycode`] is
/// more useful than resolving it to a name that then fails.
pub fn keycode_name(code: u32) -> Option<&'static str> {
    let name = match code {
        9 => "escape",
        10 => "1",
        11 => "2",
        12 => "3",
        13 => "4",
        14 => "5",
        15 => "6",
        16 => "7",
        17 => "8",
        18 => "9",
        19 => "0",
        20 => "minus",
        21 => "equal",
        22 => "backspace",
        23 => "tab",
        24 => "q",
        25 => "w",
        26 => "e",
        27 => "r",
        28 => "t",
        29 => "y",
        30 => "u",
        31 => "i",
        32 => "o",
        33 => "p",
        34 => "bracketleft",
        35 => "bracketright",
        36 => "return",
        38 => "a",
        39 => "s",
        40 => "d",
        41 => "f",
        42 => "g",
        43 => "h",
        44 => "j",
        45 => "k",
        46 => "l",
        47 => "semicolon",
        48 => "apostrophe",
        49 => "grave",
        51 => "backslash",
        52 => "z",
        53 => "x",
        54 => "c",
        55 => "v",
        56 => "b",
        57 => "n",
        58 => "m",
        59 => "comma",
        60 => "period",
        61 => "slash",
        65 => "space",
        67 => "f1",
        68 => "f2",
        69 => "f3",
        70 => "f4",
        71 => "f5",
        72 => "f6",
        73 => "f7",
        74 => "f8",
        75 => "f9",
        76 => "f10",
        95 => "f11",
        96 => "f12",
        107 => "print",
        110 => "home",
        111 => "up",
        112 => "pageup",
        113 => "left",
        114 => "right",
        115 => "end",
        116 => "down",
        117 => "pagedown",
        118 => "insert",
        119 => "delete",
        // The numeric keypad, in the evdev map's own order — note 63
        // and 125 sit well outside the 79..=91 run the other keypad
        // keys form, which is why this is a list and not a range.
        // Names are `parse_key`'s, so a `code:` binding and a named
        // one resolve to the same keysym; see `crate::keysym_for`'s
        // keypad block for why the digits are named by the key rather
        // than by their NumLock-on keysym.
        63 => "kpmultiply",
        79 => "kp7",
        80 => "kp8",
        81 => "kp9",
        82 => "kpsubtract",
        83 => "kp4",
        84 => "kp5",
        85 => "kp6",
        86 => "kpadd",
        87 => "kp1",
        88 => "kp2",
        89 => "kp3",
        90 => "kp0",
        91 => "kpdecimal",
        104 => "kpenter",
        106 => "kpdivide",
        125 => "kpequal",
        // Omarchy binds this Apple-keyboard position as its menu key;
        // the evdev map aliases X keycode 201 to F23.
        201 => "f23",
        _ => return None,
    };
    Some(name)
}

/// The keycodes this reader resolves, as a documentation handle — the
/// module docs and the tests both want to name the range.
pub const KEYCODES: std::ops::RangeInclusive<u32> = 9..=201;

/// X11 and Hyprland key names, mapped to the run-together names
/// [`crate::parse_key`] reads. Lowercased on both sides: Hyprland's own
/// matching is case-insensitive and Omarchy's files are inconsistent
/// (`RETURN`, `Delete`, `comma`, `XF86AudioPlay`) in a way that is not
/// meaningful.
const XKEYSYM_ALIASES: &[(&str, &str)] = &[
    ("xf86audioraisevolume", "volumeup"),
    ("xf86audiolowervolume", "volumedown"),
    ("xf86audiomute", "volumemute"),
    ("xf86audiomicmute", "micmute"),
    ("xf86audioplay", "playpause"),
    ("xf86audiopause", "audiopause"),
    ("xf86audiostop", "audiostop"),
    ("xf86audionext", "audionext"),
    ("xf86audioprev", "audioprev"),
    ("xf86monbrightnessup", "brightnessup"),
    ("xf86monbrightnessdown", "brightnessdown"),
    ("xf86kbdbrightnessup", "kbdbrightnessup"),
    ("xf86kbdbrightnessdown", "kbdbrightnessdown"),
    ("xf86kbdlightonoff", "kbdlightonoff"),
    ("xf86poweroff", "poweroff"),
    ("xf86calculator", "calculator"),
    ("xf86eject", "eject"),
    ("xf86search", "search"),
    ("xf86touchpadtoggle", "touchpadtoggle"),
    ("xf86touchpadon", "touchpadon"),
    ("xf86touchpadoff", "touchpadoff"),
    // Not an XF86 key, but the same kind of rename: X spells the
    // screenshot key `Print`, and Hyprland's configs use both.
    ("printscreen", "print"),
    ("sys_req", "print"),
    // Hyprland accepts the X names for these; this format uses the
    // short ones.
    ("prior", "pageup"),
    ("next", "pagedown"),
    // The keypad, in X's underscored spelling — Hyprland's configs use
    // these names verbatim.
    //
    // `kp_enter` used to map to `return`, which is not a normalisation
    // but a substitution of one key for another. It never fired: the
    // chunk splitter above tore `KP_Enter` into `KP` + `Enter` before
    // `key_name` — and therefore this table — was ever consulted, so
    // the entry was dead and the line was refused as
    // `UnknownModifier("KP")` instead. That makes it a trap rather
    // than a live bug, and one that fixing the splitter alone would
    // have sprung: `bind()` de-duplicates by combo, so a config
    // binding both `SUPER, Return` and `SUPER, KP_Enter` would have
    // kept only the second action, on the MAIN Enter key, silently.
    // The two had to be fixed together.
    ("kp_enter", "kpenter"),
    ("kp_add", "kpadd"),
    ("kp_subtract", "kpsubtract"),
    ("kp_multiply", "kpmultiply"),
    ("kp_divide", "kpdivide"),
    ("kp_decimal", "kpdecimal"),
    ("kp_separator", "kpseparator"),
    ("kp_equal", "kpequal"),
    ("kp_0", "kp0"),
    ("kp_1", "kp1"),
    ("kp_2", "kp2"),
    ("kp_3", "kp3"),
    ("kp_4", "kp4"),
    ("kp_5", "kp5"),
    ("kp_6", "kp6"),
    ("kp_7", "kp7"),
    ("kp_8", "kp8"),
    ("kp_9", "kp9"),
    // The same keys under their NumLock-off names, which is what a
    // config written against the cursor-mode symbols spells.
    ("kp_home", "kphome"),
    ("kp_end", "kpend"),
    ("kp_up", "kpup"),
    ("kp_down", "kpdown"),
    ("kp_left", "kpleft"),
    ("kp_right", "kpright"),
    ("kp_prior", "kpprior"),
    ("kp_page_up", "kppageup"),
    ("kp_next", "kpnext"),
    ("kp_page_down", "kppagedown"),
    ("kp_begin", "kpbegin"),
    ("kp_insert", "kpinsert"),
    ("kp_delete", "kpdelete"),
];
