//! The dialog's brain: everything that happens between a keystroke and
//! an intent, with no window and no `nmcli` anywhere in it.
//!
//! [`JoinDialog`] folds input into state and returns an [`Action`] when
//! the person has asked for something. It never spawns anything —
//! `main` does, and `main` is the only place in this crate that can.
//! That split is what makes the interesting half of a GUI testable on
//! a machine with no X server, no NetworkManager and, as it happens,
//! no wifi radio.

/// What the dialog wants done. Returned to `main`, which owns the
/// process spawning and hands the answer back through
/// [`JoinDialog::finished`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Run the join. The passphrase travels in this value and in
    /// `nmcli`'s stdin pipe, and nowhere else — see the crate doc.
    Join { ssid: String, passphrase: String },
    /// Take the window down. Cancel, Escape, and the close button
    /// after a successful join all land here; there is one way out.
    Close,
}

/// Which control has the keyboard. Tab walks this list in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Passphrase,
    Reveal,
    Join,
    Cancel,
}

impl Focus {
    /// Tab order, as a cycle in both directions.
    const ORDER: [Focus; 4] = [Focus::Passphrase, Focus::Reveal, Focus::Join, Focus::Cancel];

    fn step(self, back: bool) -> Focus {
        let at = Focus::ORDER.iter().position(|&f| f == self).unwrap_or(0);
        let len = Focus::ORDER.len();
        let next = if back { (at + len - 1) % len } else { (at + 1) % len };
        Focus::ORDER[next]
    }
}

/// Where the dialog is in its one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Taking a passphrase.
    Editing,
    /// `nmcli` is running. Every control is inert: the join is already
    /// in flight and there is nothing useful a second click can mean.
    Joining,
    /// The join failed, with `nmcli`'s own reason. The field is live
    /// again — a mistyped passphrase should not cost the window.
    Failed(String),
    /// The join succeeded. Nothing left to do but close.
    Joined,
}

/// A control that can be clicked. Shared by the layout (which places
/// it), the renderer (which highlights it) and the state machine
/// (which acts on it), exactly as the link panel's `RowKey` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Field,
    Reveal,
    Join,
    Cancel,
}

/// Everything the renderer draws — plain values, so a test can
/// hand-build any face without driving the state machine to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogView {
    pub ssid: String,
    /// What goes in the field: the passphrase itself when revealed,
    /// one bullet per character when not. The renderer is handed this
    /// rather than the secret plus a flag, so there is no path by
    /// which a masked field draws the real thing.
    pub shown: String,
    /// Whether the caret is drawn, and where — measured in characters
    /// of `shown`, which is why `shown` is what gets measured.
    pub caret: Option<usize>,
    pub revealed: bool,
    pub focus: Focus,
    pub phase: Phase,
    pub pressed: Option<Target>,
    /// Whether Join would do anything if clicked.
    pub can_join: bool,
}

/// The mask character. A bullet rather than an asterisk because the
/// dialog is drawn in the desktop's own typeface and an asterisk sits
/// on the baseline's ceiling, which reads as a footnote.
const MASK: char = '•';

pub struct JoinDialog {
    ssid: String,
    passphrase: String,
    revealed: bool,
    focus: Focus,
    phase: Phase,
    pressed: Option<Target>,
}

impl JoinDialog {
    /// A dialog for one network. The SSID is the only thing this
    /// process is told and the only thing it ever puts in an argv.
    pub fn new(ssid: impl Into<String>) -> Self {
        Self {
            ssid: ssid.into(),
            passphrase: String::new(),
            revealed: false,
            focus: Focus::Passphrase,
            phase: Phase::Editing,
            pressed: None,
        }
    }

    /// Everything the renderer needs, derived fresh.
    pub fn view(&self) -> DialogView {
        let shown = if self.revealed { self.passphrase.clone() } else { MASK.to_string().repeat(self.passphrase.chars().count()) };
        let caret = (self.focus == Focus::Passphrase && self.editable()).then(|| shown.chars().count());
        DialogView {
            ssid: self.ssid.clone(),
            shown,
            caret,
            revealed: self.revealed,
            focus: self.focus,
            phase: self.phase.clone(),
            pressed: self.pressed,
            can_join: self.can_join(),
        }
    }

    /// Whether the person may still change anything. False exactly
    /// while a join is in flight or has already succeeded — the two
    /// states where the dialog is reporting rather than asking.
    fn editable(&self) -> bool {
        matches!(self.phase, Phase::Editing | Phase::Failed(_))
    }

    /// An empty passphrase is the one input this dialog rejects on its
    /// own. Everything else — too short for WPA2, wrong for this
    /// network, right for a network that has moved on — is
    /// NetworkManager's to judge, and its refusal is a better error
    /// message than any guess made here.
    fn can_join(&self) -> bool {
        self.editable() && !self.passphrase.is_empty()
    }

    /// One keystroke. Returns an [`Action`] when the key asked for
    /// one; `None` means "state may have changed, repaint".
    pub fn on_key(&mut self, key: crate::keys::Key) -> Option<Action> {
        use crate::keys::Key;
        // Escape is the one key that works in every phase, including
        // mid-join: the window must always be closable, and a join
        // this process stops watching still completes — `nmcli` is a
        // child of the effect runner's thread, not of this window.
        if key == Key::Escape {
            return Some(Action::Close);
        }
        if self.phase == Phase::Joining {
            return None;
        }
        match key {
            Key::Escape => unreachable!("handled above"),
            Key::Tab { back } => self.focus = self.focus.step(back),
            Key::ToggleReveal => self.revealed = !self.revealed,
            Key::Backspace => {
                if self.focus == Focus::Passphrase && self.editable() {
                    self.passphrase.pop();
                }
            }
            Key::Char(c) => {
                if self.phase == Phase::Joined {
                    // Nothing left to type into.
                    return None;
                }
                // Space activates whatever has focus, the way it does
                // in every toolkit — except in the field, where a
                // space is a legal passphrase character and wins.
                if c == ' ' && self.focus != Focus::Passphrase {
                    return self.activate(self.focus);
                }
                if self.focus == Focus::Passphrase {
                    self.clear_error();
                    self.passphrase.push(c);
                }
            }
            Key::Enter => {
                // Enter is the default action wherever focus sits,
                // because a person who has just typed a passphrase
                // should not have to find the button.
                return match self.focus {
                    Focus::Cancel => Some(Action::Close),
                    Focus::Reveal => {
                        self.revealed = !self.revealed;
                        None
                    }
                    _ if self.phase == Phase::Joined => Some(Action::Close),
                    _ => self.activate(Focus::Join),
                };
            }
        }
        None
    }

    /// A press. Returns whether the face changed — the press
    /// highlight is the whole point, so a press on a control repaints
    /// and a press on the background does not.
    pub fn on_press(&mut self, x: i32, y: i32, layout: &crate::render::Layout) -> bool {
        let target = layout.hit(x, y).filter(|_| self.phase != Phase::Joining);
        // Focus follows the press, so a click into the field puts the
        // keyboard there even if the click fires nothing.
        if let Some(target) = target {
            self.focus = match target {
                Target::Field => Focus::Passphrase,
                Target::Reveal => Focus::Reveal,
                Target::Join => Focus::Join,
                Target::Cancel => Focus::Cancel,
            };
        }
        let changed = self.pressed != target || target.is_some();
        self.pressed = target;
        changed
    }

    /// A release. Fires only when it lands on the control the press
    /// did — press-drag-release elsewhere is a change of mind, the
    /// same rule the link panel's rows follow.
    pub fn on_release(&mut self, x: i32, y: i32, layout: &crate::render::Layout) -> Option<Action> {
        let pressed = self.pressed.take()?;
        if layout.hit(x, y) != Some(pressed) {
            return None;
        }
        self.activate(match pressed {
            Target::Field => Focus::Passphrase,
            Target::Reveal => Focus::Reveal,
            Target::Join => Focus::Join,
            Target::Cancel => Focus::Cancel,
        })
    }

    fn activate(&mut self, what: Focus) -> Option<Action> {
        match what {
            Focus::Passphrase => None,
            Focus::Reveal => {
                self.revealed = !self.revealed;
                None
            }
            Focus::Cancel => Some(Action::Close),
            Focus::Join => {
                if self.phase == Phase::Joined {
                    return Some(Action::Close);
                }
                if !self.can_join() {
                    // Join refused because the field is empty, so the
                    // field is where the person needs to be — leaving
                    // the keyboard parked on a button that just did
                    // nothing would silently swallow their next
                    // keystroke.
                    self.focus = Focus::Passphrase;
                    return None;
                }
                self.phase = Phase::Joining;
                Some(Action::Join { ssid: self.ssid.clone(), passphrase: self.passphrase.clone() })
            }
        }
    }

    /// Typing after a failure clears the failure: the message was
    /// about the passphrase that has just stopped being the one in the
    /// field.
    fn clear_error(&mut self) {
        if matches!(self.phase, Phase::Failed(_)) {
            self.phase = Phase::Editing;
        }
    }

    /// `main` reporting what `nmcli` did. `reason` is used only on
    /// failure, and only after [`clean_reason`] has had it.
    pub fn finished(&mut self, ok: bool, reason: &str) {
        self.phase = if ok { Phase::Joined } else { Phase::Failed(clean_reason(reason)) };
        // A finished join takes the keyboard to the button that now
        // means "close", so Enter does the obvious thing.
        self.focus = if ok { Focus::Join } else { Focus::Passphrase };
        if ok {
            // The secret has done its work. Holding it in memory past
            // that buys nothing and is one core dump from a leak.
            self.passphrase.clear();
            self.revealed = false;
        }
    }

    /// The passphrase, for the one caller that needs it. Not `pub`
    /// beyond the crate and not in [`DialogView`]: the renderer is
    /// handed the masked string, so no drawing path can reach this.
    #[cfg(test)]
    fn passphrase(&self) -> &str {
        &self.passphrase
    }
}

/// Reduces `nmcli`'s stderr to one line the dialog can show.
///
/// Two jobs, and the second is the important one. It trims nmcli's
/// `Error: ` prefix and takes the first non-empty line, because the
/// dialog has room for a sentence, not a transcript. And it refuses
/// anything implausibly long, because this text is *echoed from a
/// stream a prompt was written to*: `nmcli --ask` reading from a pipe
/// has no terminal to disable echo on, so its prompt handling is the
/// one place a passphrase could ever come back out. stdout is sent to
/// `/dev/null` for exactly that reason and only stderr reaches here,
/// but a belt-and-braces cap costs one line and removes the last path
/// by which a secret could be drawn on a screen.
pub fn clean_reason(stderr: &str) -> String {
    const LIMIT: usize = 160;
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("nmcli did not say why")
        .trim_start_matches("Error:")
        .trim();
    let mut out: String = line.chars().take(LIMIT).collect();
    if out.is_empty() {
        out = "nmcli did not say why".to_string();
    }
    out.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Key;
    use crate::render::layout;

    fn typing(dialog: &mut JoinDialog, text: &str) {
        for c in text.chars() {
            dialog.on_key(Key::Char(c));
        }
    }

    /// A click through the same layout the renderer draws, so a moved
    /// button cannot silently desync the tests from the pixels.
    fn click(dialog: &mut JoinDialog, target: Target) -> Option<Action> {
        let l = layout(1.0);
        let (x, y) = l.center(target).expect("every target is placed");
        dialog.on_press(x, y, &l);
        dialog.on_release(x, y, &l)
    }

    #[test]
    fn typing_fills_the_field_and_the_view_shows_bullets_not_the_secret() {
        let mut d = JoinDialog::new("Cafe Wifi");
        typing(&mut d, "hunter2");
        assert_eq!(d.passphrase(), "hunter2");
        let view = d.view();
        assert_eq!(view.shown, "•••••••", "a masked field must not render the passphrase");
        assert!(!view.shown.contains("hunter"));
        assert_eq!(view.caret, Some(7), "the caret sits after the last character");
        assert_eq!(view.ssid, "Cafe Wifi");
    }

    #[test]
    fn reveal_shows_the_real_thing_and_hides_it_again() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "hunter2");
        click(&mut d, Target::Reveal);
        assert_eq!(d.view().shown, "hunter2");
        assert!(d.view().revealed);
        d.on_key(Key::ToggleReveal);
        assert_eq!(d.view().shown, "•••••••", "Ctrl+R toggles back");
    }

    #[test]
    fn backspace_removes_one_character_and_an_empty_field_survives_it() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "ab");
        d.on_key(Key::Backspace);
        assert_eq!(d.passphrase(), "a");
        d.on_key(Key::Backspace);
        d.on_key(Key::Backspace);
        assert_eq!(d.passphrase(), "", "backspace on an empty field is not a panic");
    }

    #[test]
    fn a_multibyte_passphrase_masks_and_deletes_by_character() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "café€");
        assert_eq!(d.view().shown.chars().count(), 5, "one bullet per character, not per byte");
        d.on_key(Key::Backspace);
        assert_eq!(d.passphrase(), "café", "backspace removes a whole character");
    }

    #[test]
    fn join_is_the_only_action_that_carries_the_passphrase() {
        let mut d = JoinDialog::new("Cafe Wifi");
        typing(&mut d, "hunter2");
        let action = click(&mut d, Target::Join).expect("join fires");
        assert_eq!(action, Action::Join { ssid: "Cafe Wifi".into(), passphrase: "hunter2".into() });
        assert_eq!(d.view().phase, Phase::Joining, "the dialog says it is working");
    }

    #[test]
    fn an_empty_passphrase_cannot_join() {
        let mut d = JoinDialog::new("Cafe");
        assert!(!d.view().can_join);
        assert_eq!(click(&mut d, Target::Join), None, "an empty field must not spawn nmcli");
        assert_eq!(d.view().phase, Phase::Editing);
        assert_eq!(d.view().focus, Focus::Passphrase, "a refused join sends the keyboard where the missing thing is");
        typing(&mut d, "x");
        assert!(d.view().can_join, "one character is enough — the rest is NetworkManager's call");
    }

    #[test]
    fn a_join_in_flight_ignores_everything_but_escape() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "hunter2");
        click(&mut d, Target::Join);
        typing(&mut d, "zzz");
        assert_eq!(d.passphrase(), "hunter2", "the field is frozen while nmcli runs");
        assert_eq!(click(&mut d, Target::Join), None, "and a second click does not spawn a second nmcli");
        assert_eq!(d.on_key(Key::Escape), Some(Action::Close), "but the window is always closable");
    }

    #[test]
    fn a_failure_shows_the_reason_and_hands_the_field_back() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "wrongpass");
        click(&mut d, Target::Join);
        d.finished(false, "Error: Connection activation failed: (7) Secrets were required.\n");
        let view = d.view();
        assert_eq!(view.phase, Phase::Failed("CONNECTION ACTIVATION FAILED: (7) SECRETS WERE REQUIRED.".into()));
        assert_eq!(view.focus, Focus::Passphrase, "the keyboard goes back where the mistake was");
        assert!(view.can_join, "a mistyped passphrase must not cost the window");
        // And typing clears the stale complaint.
        typing(&mut d, "x");
        assert_eq!(d.view().phase, Phase::Editing);
    }

    #[test]
    fn a_success_forgets_the_passphrase_and_leaves_only_a_way_out() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "hunter2");
        click(&mut d, Target::Join);
        d.finished(true, "");
        assert_eq!(d.view().phase, Phase::Joined);
        assert_eq!(d.passphrase(), "", "the secret is dropped the moment it has done its work");
        assert_eq!(d.view().shown, "");
        assert_eq!(d.on_key(Key::Enter), Some(Action::Close), "enter on a finished join closes");
        assert_eq!(click(&mut d, Target::Join), Some(Action::Close), "and so does the button, which now means close");
    }

    #[test]
    fn tab_cycles_focus_both_ways_and_enter_follows_it() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "hunter2");
        assert_eq!(d.view().focus, Focus::Passphrase);
        for expected in [Focus::Reveal, Focus::Join, Focus::Cancel, Focus::Passphrase] {
            d.on_key(Key::Tab { back: false });
            assert_eq!(d.view().focus, expected);
        }
        d.on_key(Key::Tab { back: true });
        assert_eq!(d.view().focus, Focus::Cancel, "shift+tab walks back");
        assert_eq!(d.on_key(Key::Enter), Some(Action::Close), "enter on Cancel closes");
    }

    #[test]
    fn enter_from_the_field_joins_without_finding_the_button() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "hunter2");
        assert!(matches!(d.on_key(Key::Enter), Some(Action::Join { .. })));
    }

    #[test]
    fn space_activates_a_button_but_types_into_the_field() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "a b");
        assert_eq!(d.passphrase(), "a b", "a space is a legal passphrase character");
        d.on_key(Key::Tab { back: false });
        d.on_key(Key::Char(' '));
        assert!(d.view().revealed, "space on the reveal control toggles it");
    }

    #[test]
    fn escape_closes_from_every_phase() {
        for setup in [Phase::Editing, Phase::Joining, Phase::Joined, Phase::Failed("nope".into())] {
            let mut d = JoinDialog::new("Cafe");
            typing(&mut d, "hunter2");
            match &setup {
                Phase::Joining => {
                    click(&mut d, Target::Join);
                }
                Phase::Joined => d.finished(true, ""),
                Phase::Failed(_) => d.finished(false, "nope"),
                Phase::Editing => {}
            }
            assert_eq!(d.on_key(Key::Escape), Some(Action::Close), "escape must close from {setup:?}");
        }
    }

    #[test]
    fn a_press_on_one_control_released_on_another_fires_nothing() {
        let mut d = JoinDialog::new("Cafe");
        typing(&mut d, "hunter2");
        let l = layout(1.0);
        let (jx, jy) = l.center(Target::Join).unwrap();
        let (cx, cy) = l.center(Target::Cancel).unwrap();
        d.on_press(jx, jy, &l);
        assert_eq!(d.on_release(cx, cy, &l), None, "press-drag-release is a change of mind");
        assert_eq!(d.view().phase, Phase::Editing, "nothing ran");
        assert_eq!(d.view().pressed, None, "and the highlight is cleared");
    }

    #[test]
    fn a_click_on_the_background_fires_nothing_and_keeps_focus() {
        let mut d = JoinDialog::new("Cafe");
        let l = layout(1.0);
        d.on_press(1, 1, &l);
        assert_eq!(d.on_release(1, 1, &l), None);
        assert_eq!(d.view().focus, Focus::Passphrase, "the frame is not a control");
    }

    #[test]
    fn a_click_into_the_field_takes_the_keyboard_there() {
        let mut d = JoinDialog::new("Cafe");
        d.on_key(Key::Tab { back: false });
        assert_eq!(d.view().focus, Focus::Reveal);
        click(&mut d, Target::Field);
        assert_eq!(d.view().focus, Focus::Passphrase);
    }

    #[test]
    fn nmcli_noise_is_reduced_to_one_showable_line() {
        assert_eq!(clean_reason("Error: No network with SSID 'Cafe' found.\n"), "NO NETWORK WITH SSID 'CAFE' FOUND.");
        assert_eq!(clean_reason("\n\n  Error:  Secrets were required.  \nsecond line\n"), "SECRETS WERE REQUIRED.");
        assert_eq!(clean_reason(""), "NMCLI DID NOT SAY WHY", "silence still needs a sentence");
        assert_eq!(clean_reason("Error:"), "NMCLI DID NOT SAY WHY", "and so does a bare prefix");
        let flood = "x".repeat(500);
        assert_eq!(clean_reason(&flood).chars().count(), 160, "the dialog shows a sentence, never a transcript");
    }
}
