//! The built-in instrument panel vocabulary: the detail view behind a
//! tile, for widgets that ship with the compositor.
//!
//! A tile is 56 logical pixels of glanceable reading; the panel is the
//! full story behind it — the traffic graph behind the network
//! sparkline, the nearby-networks list behind the link tile. Remote
//! dockapps got panels in protocol 2 (`OpenPanel`/`PanelFrame`, see
//! `docs/dockapp-protocol.md`); this module is the same capability for
//! the in-process side of the dock, minus the socket: no banding, no
//! generations, no flow control — a widget draws into a granted-size
//! buffer the shell hands it, and the shell presents it inside the
//! same chiseled chrome, with the same placement, the same
//! one-panel-desktop-wide arbitration and the same dismissal gestures
//! (click-away, Escape, tile re-click) as a remote panel.
//!
//! # The same incapability, extended
//!
//! Everything in this module is data, exactly as [`sampling`] is: a
//! widget *describes* the panel it wants ([`PanelSpec`]), draws pixels
//! into a buffer it is handed ([`PanelFrame`]), and answers input with
//! an intent ([`PanelReaction`]) rather than an action. The one
//! reaction that touches the system, [`PanelReaction::Run`], carries
//! the existing [`Effect`] — which means a panel action goes through
//! the exact executor a tile click uses: run detached on a thread the
//! dock owns, with the authoritative answer arriving as the next
//! sample rather than as trust in an exit status.
//!
//! Read [`Effect::Run`]'s two fields as the sentence they are: the
//! **program** is `&'static str` and the **arguments** are
//! `Vec<String>`. The *program* is the whitelist — the set of binaries
//! a built-in widget can run is the set of string literals in this
//! repository's source, and no runtime value can add to it — while the
//! arguments are allowed to come from the running system, because a
//! panel row is usually about a sink, a UUID or an SSID that was named
//! a moment ago. That is the whole split, and [`Argv`] is how the
//! second half is written: static words stay literals, runtime values
//! ride in validated slots that refuse an operand shaped like an
//! option. The compiler is the whitelist for *what runs*; `Argv` is
//! the rule for *what it is told*.
//!
//! [`Argv`]: crate::Argv
//!
//! # No keyboard, ever
//!
//! [`PanelEvent`] is the pointer vocabulary of the remote panel's
//! `PanelInput` and nothing more. A panel is a popover, not a window:
//! it takes no keyboard focus, and the only key it ever "sees" is the
//! Escape the *shell* grabs to dismiss it — which the widget is not
//! told about, because dismissal is the desktop's gesture, not the
//! panel's.
//!
//! [`sampling`]: crate::sampling

use wm_theme::Theme;
use wm_theme_api::{DecorationBuffer, Point};

use crate::Effect;

/// The panel a widget wants, in the same device-pixel space its tile
/// renders in. Returned from
/// [`DockWidget::panel_spec`](crate::DockWidget::panel_spec); `None`
/// (the default) means "no panel", and a tile with no panel ignores
/// the open gesture entirely.
///
/// A spec is a *request*, exactly as a remote `OpenPanel` is: the
/// shell clamps it to the workarea beside the dock and to the
/// protocol's own caps, and the granted size — not the requested one —
/// is what [`PanelFrame`] arrives sized to. A widget that renders by
/// reading [`PanelFrame::width`]/[`PanelFrame::height`] rather than by
/// trusting its own spec is correct on every monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelSpec {
    /// Desired content width, device pixels, chrome excluded — the
    /// shell draws its border *around* this.
    pub width: u32,
    /// Desired content height, device pixels, chrome excluded.
    pub height: u32,
}

impl PanelSpec {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// The granted-size buffer a widget's
/// [`render_panel`](crate::DockWidget::render_panel) draws into:
/// premultiplied RGBA8, row-major, exactly `width * height * 4` bytes.
///
/// Owned by the shell and persistent across repaints, so a widget that
/// only redraws the region that changed keeps the rest — the same
/// deal a remote panel's banded protocol offers, without the bands.
/// It starts fully transparent, and unpainted regions show the
/// desktop's empty well through, so a warming-up panel reads as an
/// instrument, not as a hole.
pub struct PanelFrame {
    buffer: DecorationBuffer,
}

impl PanelFrame {
    /// A transparent frame of the granted size. Constructed by the
    /// shell when a panel opens (or is re-granted); a widget only ever
    /// receives one.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            buffer: DecorationBuffer { width, height, pixels: vec![0; (width as usize) * (height as usize) * 4] },
        }
    }

    /// Granted content width, device pixels.
    pub fn width(&self) -> u32 {
        self.buffer.width
    }

    /// Granted content height, device pixels.
    pub fn height(&self) -> u32 {
        self.buffer.height
    }

    /// The pixels, for drawing in place: premultiplied RGBA8,
    /// row-major, `width * 4` bytes per row.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.buffer.pixels
    }

    /// The frame as the buffer type every renderer in this SDK already
    /// produces and the shell already blits.
    pub fn buffer(&self) -> &DecorationBuffer {
        &self.buffer
    }

    /// Replaces the whole frame with `buffer` — the convenient path
    /// for a widget that renders through `wm-theme`'s pure renderers,
    /// which return a [`DecorationBuffer`] of their own.
    ///
    /// Returns whether the sizes matched. A wrong-sized buffer is
    /// refused and the frame keeps its pixels, mirroring the remote
    /// path's reject-don't-rescale rule ([`RemoteTile::on_frame`]'s
    /// third lock): a widget that rendered against a stale grant shows
    /// its last good frame, never a scaled or cropped one.
    ///
    /// [`RemoteTile::on_frame`]: crate::DockWidget::render
    pub fn adopt(&mut self, buffer: DecorationBuffer) -> bool {
        if (buffer.width, buffer.height) != (self.buffer.width, self.buffer.height)
            || buffer.pixels.len() != (buffer.width as usize) * (buffer.height as usize) * 4
        {
            return false;
        }
        self.buffer = buffer;
        true
    }
}

/// What panel rendering is handed beside the frame: the same theme
/// state and shared text machinery tile rendering
/// ([`DockWidget::render`](crate::DockWidget::render)) already
/// receives, bundled because a panel render takes five arguments where
/// a tile render's four were already at clippy's limit.
///
/// `fonts` and `swash` are the dock's own, threaded through for the
/// same reason they are in `render`: one `FontSystem` per session, one
/// set of shaping caches, shared by every face on the desktop.
pub struct PanelCtx<'a> {
    /// The live theme — render with its palette, exactly as the tile
    /// face does, so a restyle repaints the panel in the new clothes.
    pub theme: &'a Theme,
    /// The dock's current tile edge in device pixels — the scale
    /// yardstick, so a panel's internal metrics can follow
    /// `CHONKSTEP_SCALE` the way every tile's do.
    pub tile: u32,
    pub fonts: &'a mut cosmic_text::FontSystem,
    pub swash: &'a mut cosmic_text::SwashCache,
}

/// A pointer event inside an open panel's content area, in
/// content-local coordinates (origin at the content's top-left, chrome
/// excluded — the border is the shell's, and a widget never hears
/// about it).
///
/// This is the remote panel's `PanelInput` vocabulary, restated as an
/// enum a widget can match on: left press/release, scroll steps,
/// motion, and enter/leave crossings. Left only, by the dock's
/// standing reserved-button policy: middle is reorder, right is the
/// dock's own open/toggle gesture, and neither is ever forwarded. No
/// keyboard variant exists, deliberately — see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelEvent {
    LeftPress { local: Point },
    LeftRelease { local: Point },
    /// One wheel notch; the shell replays a multi-notch gesture as
    /// discrete steps, exactly as it does for tiles. Negative is up.
    Scroll { local: Point, delta: i32 },
    /// The pointer moved inside the content. Deduplicated by the
    /// shell: a stationary pointer costs no events, a moving one at
    /// most one per motion dispatch.
    Motion { local: Point },
    Enter,
    Leave,
}

impl PanelEvent {
    /// Where in the panel's content this landed, for the variants that
    /// carry a position at all — `Enter`/`Leave` are about the panel
    /// as a whole, the same shape [`DockInput::local`] gives tiles.
    ///
    /// [`DockInput::local`]: crate::DockInput::local
    pub fn local(&self) -> Option<Point> {
        match *self {
            PanelEvent::LeftPress { local }
            | PanelEvent::LeftRelease { local }
            | PanelEvent::Scroll { local, .. }
            | PanelEvent::Motion { local } => Some(local),
            PanelEvent::Enter | PanelEvent::Leave => None,
        }
    }
}

/// What a widget wants done about one [`PanelEvent`] — the panel twin
/// of the `Vec<Effect>` a tile click returns: nothing happened, my
/// pixels changed, close me, or run this (or these).
///
/// # Running more than one thing
///
/// [`Run`](PanelReaction::Run) is the single-effect shorthand and
/// [`RunAll`](PanelReaction::RunAll) the general form; `Run(e)` is
/// exactly `RunAll(vec![e])` and the shell implements it as one. The
/// plural is not a convenience — some actions simply *are* several
/// commands, because the tool takes one per invocation and offers no
/// chaining. Switching the default audio sink is the case that forced
/// it: `pactl set-default-sink <name>`, and then one
/// `pactl move-sink-input <index> <name>` per stream already playing,
/// or the sound keeps coming out of the old device.
///
/// The contract the shell keeps:
///
/// * **Effects are performed in the order the widget listed them**,
///   and the [`Effect::Run`]s among them run *sequentially* — one
///   after another on a single worker thread, never overlapping, never
///   on the repaint thread.
/// * **Every `then:` resample fires when its own command exits.**
///   There is no "last one wins": each command's resample is its own,
///   which is why the confirming resample belongs on the command whose
///   completion actually proves the change (the audio panel puts it on
///   `set-default-sink`; the per-stream migrations carry none, because
///   they alter which streams play where, not anything the panel
///   draws).
/// * [`Effect::Repaint`] and [`Effect::Resample`] in the list are
///   applied as they are reached, before the commands behind them have
///   finished — they need no process and must not wait for one.
///
/// An empty `RunAll` is a no-op, deliberately: "this row resolved to
/// no commands" is a perfectly ordinary answer, and a widget should
/// not have to spell it as [`None`](PanelReaction::None).
pub enum PanelReaction {
    /// The event meant nothing.
    None,
    /// The panel's pixels changed:
    /// [`render_panel`](crate::DockWidget::render_panel) will be
    /// called and the result presented.
    Repaint,
    /// The widget wants its own panel closed — an action that
    /// completed, a "done" affordance. The shell tears the panel down
    /// exactly as a dismissal gesture would.
    Close,
    /// Perform an [`Effect`], through the same executor tile clicks
    /// use. [`Effect::Run`] is the intended cargo — the compile-time
    /// argv, the detached runner, the `then:` resample that makes the
    /// next reading the authority on what the command did. A panel
    /// action whose argv carries a word the *system* named (a sink, a
    /// UUID, an SSID) builds it with [`Argv`](crate::Argv), which is
    /// the rule for exactly this case:
    /// [`DockWidget::panel_input`](crate::DockWidget::panel_input)
    /// states it, [`Argv`](crate::Argv) argues it.
    /// [`Effect::Resample`] hurries a sampler with no command in
    /// front; [`Effect::Repaint`] here means the *panel*'s pixels (the
    /// dock repaints its tiles from `update`, which sees the resampled
    /// reading — a panel action never needs to ask for the tile).
    Run(Effect),
    /// Perform several effects, in order — the general form of
    /// [`Run`](PanelReaction::Run), for the actions that are inherently
    /// more than one command. See the type's docs for the ordering and
    /// resample rules; an empty list is a no-op.
    RunAll(Vec<Effect>),
}

impl PanelReaction {
    /// The reaction for a list of effects, collapsing the empty case to
    /// [`None`](PanelReaction::None) — so a widget can build its
    /// effects with `filter_map` (an [`Argv`](crate::Argv) that refused
    /// a value yields nothing) and hand the result straight back
    /// without checking whether anything survived.
    pub fn run_all(effects: Vec<Effect>) -> PanelReaction {
        if effects.is_empty() {
            PanelReaction::None
        } else {
            PanelReaction::RunAll(effects)
        }
    }

    /// Every effect this reaction asks for, in order — one for
    /// [`Run`](PanelReaction::Run), the list for
    /// [`RunAll`](PanelReaction::RunAll), and nothing for the
    /// reactions that ask for no work. The shell dispatches through
    /// this, so the two variants cannot drift apart, and a widget test
    /// can assert on a reaction without matching two shapes.
    pub fn effects(self) -> Vec<Effect> {
        match self {
            PanelReaction::Run(effect) => vec![effect],
            PanelReaction::RunAll(effects) => effects,
            PanelReaction::None | PanelReaction::Repaint | PanelReaction::Close => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_starts_transparent_and_exactly_granted_sized() {
        let frame = PanelFrame::new(40, 30);
        assert_eq!((frame.width(), frame.height()), (40, 30));
        assert_eq!(frame.buffer().pixels.len(), 40 * 30 * 4);
        assert!(frame.buffer().pixels.iter().all(|&b| b == 0), "unpainted regions must show the empty well through");
    }

    #[test]
    fn adopt_takes_a_matching_buffer_and_refuses_a_stale_grant() {
        let mut frame = PanelFrame::new(4, 2);
        let good = DecorationBuffer { width: 4, height: 2, pixels: vec![0xFF; 4 * 2 * 4] };
        assert!(frame.adopt(good), "a renderer's matching buffer replaces the frame");
        assert_eq!(frame.buffer().pixels[0], 0xFF);

        let wrong_size = DecorationBuffer { width: 3, height: 2, pixels: vec![0xAA; 3 * 2 * 4] };
        assert!(!frame.adopt(wrong_size), "a stale-grant render is refused, not rescaled");
        assert_eq!(frame.buffer().pixels[0], 0xFF, "and the last good pixels stay");

        let lying_header = DecorationBuffer { width: 4, height: 2, pixels: vec![0xBB; 7] };
        assert!(!frame.adopt(lying_header), "a buffer whose payload does not match its header is refused");
    }

    /// The two Run arities are one thing: the shell dispatches through
    /// `effects()`, so a widget that returns either is treated
    /// identically, and an action that resolved to no commands is a
    /// reaction that asks for nothing rather than an empty run.
    #[test]
    fn the_two_run_arities_flatten_to_the_same_ordered_list() {
        let one = PanelReaction::Run(Effect::Repaint).effects();
        assert!(matches!(one.as_slice(), [Effect::Repaint]));

        let many = PanelReaction::run_all(vec![
            Effect::Run { program: "pactl", args: vec!["set-default-sink".into(), "hdmi".into()], then: None },
            Effect::Run { program: "pactl", args: vec!["move-sink-input".into(), "7".into(), "hdmi".into()], then: None },
        ])
        .effects();
        assert_eq!(many.len(), 2, "order and count are the widget's, kept");
        match (&many[0], &many[1]) {
            (Effect::Run { args: first, .. }, Effect::Run { args: second, .. }) => {
                assert_eq!(first[0], "set-default-sink", "the authoritative command is first, as listed");
                assert_eq!(second[0], "move-sink-input");
            }
            _ => panic!("both are runs"),
        }

        assert!(matches!(PanelReaction::run_all(Vec::new()), PanelReaction::None), "nothing to run is nothing to do");
        assert!(PanelReaction::Close.effects().is_empty());
        assert!(PanelReaction::Repaint.effects().is_empty());
        assert!(PanelReaction::None.effects().is_empty());
    }

    #[test]
    fn events_report_their_position_like_dock_inputs_do() {
        let at = Point::new(7, 9);
        assert_eq!(PanelEvent::LeftPress { local: at }.local(), Some(at));
        assert_eq!(PanelEvent::LeftRelease { local: at }.local(), Some(at));
        assert_eq!(PanelEvent::Scroll { local: at, delta: -1 }.local(), Some(at));
        assert_eq!(PanelEvent::Motion { local: at }.local(), Some(at));
        assert_eq!(PanelEvent::Enter.local(), None);
        assert_eq!(PanelEvent::Leave.local(), None);
    }
}
