//! The `_MOTIF_WM_HINTS` decoration question, answered once for every
//! backend that has to ask it.
//!
//! Three legs of this desktop read this property — the X11 session's
//! own window manager, the XWayland arm of the compositor, and (through
//! smithay's cached parse) anything that consults `X11Surface` — and
//! before this module they read it three different ways. The
//! disagreements were small and real: one accepted a three-word
//! property where another demanded five, and one forgot the hints when
//! the property was deleted while another re-read it as absent. A
//! window that changes its decoration policy at runtime, or a toolkit
//! that writes the short form, got a different answer depending on
//! which session the user had logged into. "A feature lands once and
//! both stacks get it by construction" is the project's claim; for this
//! property it was not true, and this module is how it becomes true.
//!
//! The property is Motif's `PropMotifWmHints`: an array of CARD32s,
//! canonically five, of which two matter here.

/// Index of the `flags` word: which of the other fields carry meaning.
const FLAGS: usize = 0;
/// Index of the `decorations` word.
const DECORATIONS: usize = 2;
/// `MWM_HINTS_DECORATIONS` — set in `flags` when the `decorations`
/// word is meaningful at all.
const HINTS_DECORATIONS: u32 = 1 << 1;

/// The shortest property this reader will act on.
///
/// Motif's own struct is five words and most toolkits write all five,
/// but the decorations field is the third, so a three-word property is
/// already unambiguous — and `wm-x11` has always accepted one. Openbox
/// accepts three as well; KWin demands five and Window Maker four.
/// Reading the short form costs nothing and refusing it silently
/// re-decorates a window whose client asked us not to.
pub const MIN_HINT_WORDS: usize = 3;

/// Whether these `_MOTIF_WM_HINTS` say "the client draws its own
/// chrome" — the decorations bit present in `flags`, and the
/// `decorations` word zero.
///
/// Everything else is `false`, and that direction is the safe one in
/// every failure mode there is: a property that is absent, too short,
/// the wrong format, unreadable, or simply says nothing about
/// decorations leaves the window framed. A window wearing a frame it
/// did not want is a cosmetic complaint its user can act on; a window
/// with no frame and a client that drew none is one they cannot move,
/// resize or close.
///
/// Note what this does *not* do: a non-zero `decorations` word asking
/// for a specific subset of chrome (`MWM_DECOR_BORDER` alone, say) is
/// treated as "decorate it", not as a request to draw part of a frame.
/// This desktop's chrome is a single chiseled composition, not a menu
/// of removable parts, and the one client observed to ask for a subset
/// — Spotify, with `MWM_DECOR_ALL` — wants a frame, not less of one.
pub fn hints_say_client_decorates(hints: &[u32]) -> bool {
    if hints.len() < MIN_HINT_WORDS {
        return false;
    }
    hints[FLAGS] & HINTS_DECORATIONS != 0 && hints[DECORATIONS] == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point: a GTK or Chromium window under X11 says so by
    /// setting the flag and zeroing the field.
    #[test]
    fn the_flag_plus_a_zero_field_means_the_client_decorates() {
        assert!(hints_say_client_decorates(&[HINTS_DECORATIONS, 0, 0, 0, 0]));
        assert!(hints_say_client_decorates(&[HINTS_DECORATIONS, 0, 0]), "the three-word short form counts");
    }

    /// The direction that must never be reached by accident.
    #[test]
    fn everything_else_keeps_the_frame() {
        assert!(!hints_say_client_decorates(&[]), "absent property");
        assert!(!hints_say_client_decorates(&[HINTS_DECORATIONS, 0]), "too short to carry the field");
        assert!(!hints_say_client_decorates(&[0, 0, 0, 0, 0]), "flag clear: the field means nothing");
        assert!(
            !hints_say_client_decorates(&[0, 0, 0xff, 0, 0]),
            "a decorations word with the flag clear is not a request"
        );
    }

    /// A client asking for a *subset* of chrome is asking to be
    /// decorated. Inverting this once stripped Spotify of its frame,
    /// controls and resize bars.
    #[test]
    fn asking_for_some_decorations_still_means_decorate_it() {
        assert!(!hints_say_client_decorates(&[HINTS_DECORATIONS, 0, 0x1, 0, 0]), "MWM_DECOR_ALL");
        assert!(!hints_say_client_decorates(&[HINTS_DECORATIONS, 0, 0x2, 0, 0]), "MWM_DECOR_BORDER");
    }
}
