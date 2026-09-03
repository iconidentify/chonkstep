//! The pairing state machine: a pure fold over `bluetoothctl`'s
//! transcript, and the commands to write back at it.
//!
//! Nothing in this module performs I/O. It is handed lines and returns
//! [`Step`]s, which is what lets the whole of pairing — the part that
//! cannot be exercised on a machine with no radio — be pinned by
//! canned transcripts in the tests below.
//!
//! # Why this process runs `bluetoothctl` when the dock never may
//!
//! The dock's Bluetooth instrument is emphatic that chonkstep does not
//! exec `bluetoothctl`: on an adapter-less machine every subcommand
//! hangs forever with no output, and the dock's sampler is a bare
//! `Command::output()` on a worker with no timeout, so one such call
//! wedges that worker for the life of the session. See
//! `chonk_instruments::bt_panel::bluez` for the measurements.
//!
//! This process is the deliberate exception, and the reasoning is the
//! same one that puts a third-party dock tile in its own process: **a
//! separate process may block itself and nobody else.** `chonk-btpair`
//! is a window someone opened on purpose, it does one thing, and its
//! failure mode when BlueZ never answers is a dialog that says so and
//! a close button that ends it — not a frozen desktop. The dock's
//! panel spawns it detached and never waits on it.
//!
//! The exception is also unavoidable, which is the other half of the
//! argument. **Pairing requires an agent**: BlueZ will not pair
//! without some process registering an `org.bluez.Agent1` object that
//! it can call back into to ask "is the passkey on both screens the
//! same?". Registering a D-Bus *object* means serving a bus name, and
//! `busctl` — a client tool — cannot do it. The alternatives are to
//! implement `Agent1` against a D-Bus library (a dependency this
//! workspace does not have and would acquire for one dialog) or to use
//! the agent BlueZ already ships in its own control program. This
//! takes the second.
//!
//! # Which agent mode, and why
//!
//! [`AGENT_CAPABILITY`] is `DisplayYesNo`, and the choice follows from
//! what this window physically is rather than from what pairs most
//! often.
//!
//! The Bluetooth agent capabilities describe a device's *input and
//! output*: `KeyboardDisplay` (the default) promises both, `NoInputNoOutput`
//! promises neither and silently accepts anything, `DisplayOnly` and
//! `KeyboardOnly` promise one each. `DisplayYesNo` promises a screen
//! and exactly two buttons.
//!
//! That is this dialog, exactly. It has a screen, and it has a
//! pointer, and it has **no keyboard at all** — the chonkstep SDK's
//! `App` masks `EXPOSURE | BUTTON_PRESS` and nothing else, and the
//! dock panel that spawns this window takes no keyboard *by design*.
//! `DisplayYesNo` is therefore the honest declaration, and it happens
//! to be the capability that Secure Simple Pairing's numeric
//! comparison — the modern default for anything with a display —
//! wants: BlueZ shows the same six digits here and on the device, and
//! the human confirms they match.
//!
//! Claiming `KeyboardDisplay` instead would be a lie with a
//! consequence: BlueZ would be entitled to ask this agent for a PIN
//! ([`Phase::NeedsKeyboard`]), and there would be no way to type one.
//! When a legacy device demands exactly that, this window says so and
//! names the tool that can do it, rather than hanging on a prompt it
//! cannot answer.
//!
//! # Honesty about what is tested
//!
//! **The machine this was written on has no Bluetooth adapter**, so
//! none of the pairing paths below have ever been exercised against
//! real hardware, and the tests in this file cannot do it either. What
//! they pin is the fold: given this transcript, this phase and these
//! commands. The transcripts are written from `bluetoothctl` 5.87's
//! documented and observed output shapes, and the parser is
//! deliberately lenient about everything it is not looking for — but a
//! reader should treat "the state machine is correct" and "pairing
//! works on your headset" as two different claims, only the first of
//! which has evidence here.

use std::collections::BTreeMap;

/// The agent capability this dialog registers. See the module doc.
pub const AGENT_CAPABILITY: &str = "DisplayYesNo";

/// A device seen during discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub address: String,
    pub name: String,
    /// Whether BlueZ has told us this one is already paired — it stays
    /// in the list, greyed, rather than inviting a second pairing.
    pub paired: bool,
}

/// What the dialog is doing, and therefore what it draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The child is starting and the agent has not registered yet.
    Starting,
    /// Discovery is running; the list is the answer.
    Scanning,
    /// `pair` has been issued and BlueZ has not come back.
    Pairing { address: String },
    /// Numeric comparison: the same six digits should be on the
    /// device's own screen. Two buttons, which is the whole reason for
    /// [`AGENT_CAPABILITY`].
    Confirm { address: String, passkey: String },
    /// BlueZ wants the passkey *typed on the device* (a keyboard being
    /// paired). Nothing to answer here — it is shown and the human
    /// types it over there.
    DisplayPasskey { address: String, passkey: String },
    /// A legacy device is asking this agent to supply a PIN, which a
    /// window with no keyboard cannot do. Named honestly rather than
    /// hung on.
    NeedsKeyboard { address: String },
    Paired { address: String },
    Failed { address: String, reason: String },
    /// No adapter, no BlueZ, or no `bluetoothctl`. Terminal.
    Unavailable { reason: String },
}

/// Something the driver should do about a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Write this to the child's stdin, followed by a newline.
    Send(String),
    /// The window's pixels changed.
    Repaint,
}

pub struct Pairing {
    phase: Phase,
    /// Discovered devices, keyed by address so a `[CHG]` updates the
    /// entry a `[NEW]` created rather than appending a duplicate.
    /// Ordered by address so the list does not reshuffle under the
    /// pointer every time BlueZ re-announces something.
    found: BTreeMap<String, Found>,
    /// Set once `default-agent` has been accepted, so the scan is not
    /// started before there is an agent to answer with.
    agent_ready: bool,
}

impl Pairing {
    pub fn new() -> Self {
        Self { phase: Phase::Starting, found: BTreeMap::new(), agent_ready: false }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The discovery list, in stable order.
    pub fn devices(&self) -> Vec<&Found> {
        self.found.values().collect()
    }

    /// The commands that open the session: register an agent of the
    /// capability this window can actually honor, make it the default,
    /// and only then start discovery.
    pub fn opening(&self) -> Vec<Step> {
        vec![Step::Send(format!("agent {AGENT_CAPABILITY}")), Step::Send("default-agent".to_string())]
    }

    /// A click on a discovered device.
    pub fn pair_with(&mut self, address: &str) -> Vec<Step> {
        if !matches!(self.phase, Phase::Scanning) {
            // One pairing at a time. A second `pair` while BlueZ is
            // mid-negotiation is how a pairing gets confused.
            return Vec::new();
        }
        self.phase = Phase::Pairing { address: address.to_string() };
        // Scanning during a pair is noise on the same radio, and BlueZ
        // recommends against it.
        vec![Step::Send("scan off".to_string()), Step::Send(format!("pair {address}")), Step::Repaint]
    }

    /// The confirm dialog's two buttons.
    pub fn answer(&mut self, yes: bool) -> Vec<Step> {
        let Phase::Confirm { address, .. } = &self.phase else { return Vec::new() };
        let address = address.clone();
        if yes {
            self.phase = Phase::Pairing { address };
            vec![Step::Send("yes".to_string()), Step::Repaint]
        } else {
            self.phase = Phase::Failed { address, reason: "declined here".to_string() };
            vec![Step::Send("no".to_string()), Step::Repaint]
        }
    }

    /// "Try another" from a finished or failed state: back to the list.
    pub fn rescan(&mut self) -> Vec<Step> {
        self.found.clear();
        self.phase = Phase::Scanning;
        vec![Step::Send("scan on".to_string()), Step::Repaint]
    }

    /// The commands that close the session cleanly, so the adapter is
    /// not left discovering after the window goes away.
    pub fn closing(&self) -> Vec<Step> {
        vec![Step::Send("scan off".to_string()), Step::Send("quit".to_string())]
    }

    /// Folds one line of `bluetoothctl` output.
    pub fn on_line(&mut self, raw: &str) -> Vec<Step> {
        let line = clean(raw);
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }

        // Terminal conditions first: these outrank whatever the phase
        // was, because none of the rest can succeed after one.
        if let Some(reason) = unavailable_reason(line) {
            self.phase = Phase::Unavailable { reason };
            return vec![Step::Repaint];
        }

        if line.starts_with("Agent registered") || line.starts_with("Default agent request successful") {
            self.agent_ready = true;
            if matches!(self.phase, Phase::Starting) {
                self.phase = Phase::Scanning;
                return vec![Step::Send("scan on".to_string()), Step::Repaint];
            }
            return vec![Step::Repaint];
        }

        if let Some((address, passkey)) = confirm_request(line) {
            let address = address.or_else(|| self.pairing_address()).unwrap_or_default();
            self.phase = Phase::Confirm { address, passkey };
            return vec![Step::Repaint];
        }

        if let Some((address, passkey)) = display_passkey(line) {
            let address = address.or_else(|| self.pairing_address()).unwrap_or_default();
            self.phase = Phase::DisplayPasskey { address, passkey };
            return vec![Step::Repaint];
        }

        // A PIN request is the capability mismatch this window cannot
        // answer. Say so rather than sit on the prompt forever.
        if line.contains("Enter PIN code") || line.contains("Request PIN code") || line.contains("Request passkey") {
            let address = self.pairing_address().unwrap_or_default();
            self.phase = Phase::NeedsKeyboard { address };
            return vec![Step::Repaint];
        }

        if line.starts_with("Pairing successful") {
            if let Some(address) = self.pairing_address() {
                self.phase = Phase::Paired { address: address.clone() };
                // Trust it so it reconnects on its own afterwards, and
                // connect it now — pairing without connecting leaves
                // someone looking at a paired headset that is silent.
                return vec![
                    Step::Send(format!("trust {address}")),
                    Step::Send(format!("connect {address}")),
                    Step::Repaint,
                ];
            }
            return vec![Step::Repaint];
        }

        if let Some(reason) = failure_reason(line) {
            let address = self.pairing_address().unwrap_or_default();
            self.phase = Phase::Failed { address, reason };
            return vec![Step::Repaint];
        }

        self.fold_device_event(line)
    }

    /// The address currently being negotiated, if any.
    fn pairing_address(&self) -> Option<String> {
        match &self.phase {
            Phase::Pairing { address }
            | Phase::Confirm { address, .. }
            | Phase::DisplayPasskey { address, .. }
            | Phase::NeedsKeyboard { address } => Some(address.clone()),
            _ => None,
        }
    }

    /// `[NEW]`, `[CHG]` and `[DEL]` device lines — the discovery list.
    fn fold_device_event(&mut self, line: &str) -> Vec<Step> {
        let Some((tag, rest)) = device_event(line) else { return Vec::new() };
        let Some((address, tail)) = split_address(rest) else { return Vec::new() };

        match tag {
            "DEL" => {
                if self.found.remove(&address).is_some() {
                    return vec![Step::Repaint];
                }
                Vec::new()
            }
            _ => {
                // A `[CHG] ... Paired: yes` is how an already-known
                // device announces itself; a `[CHG] ... RSSI: -60`
                // carries no name and must not overwrite one.
                if let Some(paired) = tail.strip_prefix("Paired:") {
                    if let Some(entry) = self.found.get_mut(&address) {
                        entry.paired = paired.trim() == "yes";
                        return vec![Step::Repaint];
                    }
                    return Vec::new();
                }
                let name = property_free_name(tail);
                let before = self.found.get(&address).cloned();
                let entry = self.found.entry(address.clone()).or_insert_with(|| Found {
                    address: address.clone(),
                    name: address.clone(),
                    paired: false,
                });
                if let Some(name) = name {
                    entry.name = name;
                }
                if Some(&*entry) != before.as_ref() {
                    return vec![Step::Repaint];
                }
                Vec::new()
            }
        }
    }
}

impl Default for Pairing {
    fn default() -> Self {
        Self::new()
    }
}

/// Strips ANSI SGR sequences and `bluetoothctl`'s prompt.
///
/// The tool colors its tags and prints a `[bluetooth]#` prompt on the
/// same stream, and both land in the middle of otherwise parseable
/// lines when stdout is a pipe rather than a terminal. Neither is
/// content.
fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut bytes = raw.char_indices().peekable();
    while let Some((_, ch)) = bytes.next() {
        if ch == '\u{1b}' {
            // CSI ... final byte in @..~; anything else is dropped to
            // the next letter, which is enough for the SGR this emits.
            for (_, escape) in bytes.by_ref() {
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if ch == '\r' {
            continue;
        }
        out.push(ch);
    }
    // The prompt, wherever it landed.
    let out = out.replace("[bluetooth]#", " ");
    out.trim().to_string()
}

/// The conditions after which nothing else can work.
fn unavailable_reason(line: &str) -> Option<String> {
    for (needle, reason) in [
        ("No default controller available", "no Bluetooth controller"),
        ("Waiting to connect to bluetoothd", "bluetoothd is not running"),
        ("No default controller", "no Bluetooth controller"),
    ] {
        if line.contains(needle) {
            return Some(reason.to_string());
        }
    }
    None
}

/// `Failed to pair: org.bluez.Error.AuthenticationFailed` and friends,
/// reduced to the tail worth showing.
fn failure_reason(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Failed to pair:").or_else(|| line.strip_prefix("Failed to connect:"))?;
    let rest = rest.trim();
    // `org.bluez.Error.AuthenticationCanceled` reads better as its last
    // component; anything that is not a BlueZ error name is shown whole.
    Some(rest.strip_prefix("org.bluez.Error.").unwrap_or(rest).to_string())
}

/// `Confirm passkey 123456 (yes/no):`, and the `[agent]`-prefixed
/// `Request confirmation` form that precedes it.
///
/// Returns the address when the line carries one — the agent prompts
/// usually do not, which is why the caller falls back to the address it
/// asked to pair with.
fn confirm_request(line: &str) -> Option<(Option<String>, String)> {
    let rest = line.strip_prefix("[agent] ").unwrap_or(line);
    let rest = rest.strip_prefix("Confirm passkey ")?;
    let passkey: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!passkey.is_empty()).then_some((None, passkey))
}

/// `[agent] Passkey: 123456` / `Enter passkey 123456 on the device`.
fn display_passkey(line: &str) -> Option<(Option<String>, String)> {
    let rest = line.strip_prefix("[agent] ").unwrap_or(line);
    let rest = rest.strip_prefix("Passkey: ").or_else(|| rest.strip_prefix("Enter passkey "))?;
    let passkey: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!passkey.is_empty()).then_some((None, passkey))
}

/// `[NEW] Device AA:BB:CC:DD:EE:FF Name` → `("NEW", "Device AA:… Name")`.
fn device_event(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('[')?;
    let (tag, rest) = rest.split_once(']')?;
    if !matches!(tag, "NEW" | "CHG" | "DEL") {
        return None;
    }
    let rest = rest.trim_start().strip_prefix("Device ")?;
    Some((tag, rest))
}

/// Splits `AA:BB:CC:DD:EE:FF rest of line` into the address and the
/// rest, rejecting anything that is not a MAC — a `[CHG] Controller …`
/// line must not be mistaken for a device.
fn split_address(rest: &str) -> Option<(String, &str)> {
    let (address, tail) = rest.split_once(' ').unwrap_or((rest, ""));
    let parts: Vec<&str> = address.split(':').collect();
    if parts.len() != 6 || !parts.iter().all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit())) {
        return None;
    }
    Some((address.to_string(), tail.trim()))
}

/// The display name out of a device line's tail, or `None` when the
/// tail is a property update rather than a name.
///
/// `[CHG] Device AA:… RSSI: -60` and `[CHG] Device AA:… ServicesResolved: yes`
/// must not overwrite a real name with `RSSI:`; a property line is
/// recognizable by its `Word:` shape.
fn property_free_name(tail: &str) -> Option<String> {
    if tail.is_empty() {
        return None;
    }
    let first = tail.split_whitespace().next().unwrap_or("");
    if first.ends_with(':') && !first.contains("::") {
        return None;
    }
    Some(tail.to_string())
}

#[cfg(test)]
mod tests;
