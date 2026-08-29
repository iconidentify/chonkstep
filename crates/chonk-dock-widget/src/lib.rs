//! The dock widget SDK: the one trait a dock tile implements, and the
//! declarative-sampling vocabulary it is written against.
//!
//! This crate exists to be *small and incapable*. It is the entire
//! dependency surface of `chonk-instruments` (the six built-in
//! instruments) beyond `wm-theme`, `wm-theme-api` and `cosmic-text`,
//! and that crate carries a `clippy.toml` turning file, process and
//! thread APIs into build errors inside it. A lint like that is only
//! worth having if obeying it is *possible*, which means everything a
//! widget legitimately needs has to arrive as data. That is what
//! [`sampling`] is: a widget declares [`Source`]s and is handed
//! [`Samples`]; it returns [`Effect`]s rather than running commands.
//!
//! The three-way split the SDK asks a widget author to hold:
//!
//! * **Sampling** is [`sampling`] — declared here, executed in
//!   `chonk-shell`. This is what takes away a widget's *reason* to
//!   block the compositor's repaint thread, which is a thing that
//!   happened, cost the desktop 3.6-second freezes, and got blamed on
//!   the display driver. See that module's docs.
//! * **Rendering** is `wm-theme`. Widgets call a pure renderer with
//!   plain values instead of drawing, which is what keeps renderers
//!   unit-testable pixel-for-pixel without a live system, and it is the
//!   pattern third-party `chonk-ui` dockapps should copy.
//!   Instrument-style widgets (screens with LED readouts) build on
//!   `wm_theme::panel`, the theme-reactive glass-and-LED kit.
//! * **Acting** is [`Effect`]. A click that has to run `wpctl` or
//!   `nmcli` returns the intent; the dock runs it off-thread.
//!
//! What is left for a widget is the part only it can do: fold this
//! sample into that state, and say what that state looks like. Both are
//! pure and both are testable against fixtures — [`SampleBench`] is
//! how.
//!
//! # This crate is not the third-party SDK
//!
//! `chonk-ui` is. A third party writing a dock tile writes an
//! out-of-process *dockapp* (`chonk-dock-proto`, `chonk_ui::dockapp`),
//! which the shell blits exactly as it blits one of these — see
//! `chonk-shell`'s `widgets::DockItem`. The distinction is deliberate:
//! [`Source::Command`] is arbitrary-argv-by-declaration, and the dock
//! executing an argv on a third party's behalf would blur the
//! accountability line the dockapp boundary exists to draw. So this
//! trait is for tiles that ship with the compositor, and the socket is
//! for everyone else.

use std::time::Duration;

use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

pub mod sampling;

pub use sampling::{DockInput, Effect, Reading, SampleBench, Samples, Slot, Source, SourceId, TreeEntry};

/// Re-exported so an instrument crate can name the button a
/// [`DockInput`] carries without depending on `wm-core` itself. The
/// dependency would be harmless — `wm-core` is the WM's own state
/// machine and does no I/O in anything an instrument could reach — but
/// keeping `chonk-instruments`' dependency list to "the SDK, the theme,
/// and a text shaper" is the point of that crate, and every line of its
/// `Cargo.toml` is part of the argument.
pub use wm_core::MouseButton;

/// A single dock widget.
///
/// The shape of this trait is the whole of Layer 3: there is no entry
/// point at which a widget has any reason to perform I/O. It declares
/// what it needs ([`sources`](DockWidget::sources)), is told what those
/// turned into ([`bind`](DockWidget::bind)), folds already-collected
/// readings into its own state ([`update`](DockWidget::update)), draws
/// that state ([`render`](DockWidget::render)), and returns intents
/// rather than performing them ([`on_input`](DockWidget::on_input)).
/// Every one of those runs on the compositor's single repaint thread,
/// and not one of them is handed anything to wait on.
pub trait DockWidget {
    /// This widget's identity, for the log and for the tombstone tile
    /// its slot shows if the dock ever has to evict it — see
    /// `chonk-shell`'s `SupervisedWidget`. Deliberately not defaulted: "some widget
    /// overran its budget" is not a line anyone can act on, and the
    /// only moment the answer is knowable for free is here, in the
    /// widget's own source.
    ///
    /// Keep it short and stable. It is drawn as-is on the dead-screen
    /// face, so it wants the same shape as the empty-state labels the
    /// built-ins already use ("NET", "SND", "LNK") — three or four
    /// upper-case characters, not a sentence.
    /// # Why `&str` and not `&'static str`
    ///
    /// It was `&'static str` until the dock's column became a mixed
    /// one. A built-in's identity is a literal in its own source, but
    /// the other implementor is now `chonk-shell`'s `DockItem`, whose
    /// remote half carries the `id` of the `.dockapp` file that
    /// declared it — a `String` read off disk at startup. Returning
    /// `&'static str` for one of those means leaking it, permanently,
    /// on every registry scan, so that a *borrow* can satisfy a
    /// lifetime the value never had. Relaxing the lifetime instead
    /// costs the six built-ins nothing (every `&'static str` is
    /// already a `&str`) and costs the dock's supervisor one `String`
    /// per item, cached once at construction so an evicted item still
    /// has a name for its tombstone without calling back into code the
    /// dock has disowned.
    fn name(&self) -> &str;

    /// Everything this widget needs sampled, declared once at
    /// construction. The dock starts one worker per source and never
    /// calls this again, so it is not a place to react to anything —
    /// which is exactly the property that keeps a widget off the
    /// sampling path entirely. Defaulted empty: a widget that draws
    /// only its own state (a future purely-animated tile) declares
    /// nothing.
    fn sources(&self) -> Vec<Source> {
        Vec::new()
    }

    /// The ids for the sources this widget just declared, in the same
    /// order it declared them. Called once, immediately after
    /// [`sources`](DockWidget::sources), before the first
    /// [`update`](DockWidget::update).
    ///
    /// A widget that declares sources and forgets to implement this
    /// keeps [`SourceId::UNBOUND`] and reads nothing forever, which
    /// shows up as a permanently dead instrument rather than a crash —
    /// see that constant for why that is the deliberate failure mode.
    fn bind(&mut self, ids: &[SourceId]) {
        let _ = ids;
    }

    /// Folds this pass's readings into the widget's state. Returns
    /// whether `render` would now produce different pixels, so the dock
    /// only repaints when something actually changed.
    ///
    /// Called once per event-loop iteration — roughly 60Hz against
    /// sources that update at 1Hz, so the overwhelmingly common case is
    /// that nothing is [`fresh`](Samples::fresh) and this returns
    /// `false` immediately. That asymmetry is why widgets fold on
    /// `fresh` rather than on a clock of their own: "a sampler
    /// completed a run" and "a sixtieth of a second went by" are
    /// different questions, and only the first one is news.
    ///
    /// This replaced `tick()`, and the difference is the point.
    /// `tick()` was a moment at which a widget could do anything,
    /// including read `/proc/stat`, `read_dir` sysfs, or wait on
    /// `nmcli` — all of which four of the six built-ins were still
    /// doing on the repaint thread. `update` is handed data that has
    /// already been collected and has nothing to wait on.
    fn update(&mut self, samples: &Samples) -> bool;

    /// Draws the widget's current state into a tile-sized
    /// premultiplied-RGBA buffer.
    ///
    /// `fonts` and `swash` are the dock's own, threaded through rather
    /// than owned per widget: a `cosmic_text::FontSystem` is a full
    /// fontconfig scan, and the five instruments that each built one at
    /// startup were paying that five times over for the shaping caches
    /// of a handful of three-character labels. `Desktop` already owns
    /// one for its menus and switcher; that is the one every tile now
    /// shapes against, so the caches are shared too.
    ///
    /// `&self` rather than `&mut self` on purpose: rendering must be a
    /// function of state that [`update`](DockWidget::update) already
    /// settled, so that the same state cannot draw two different
    /// things.
    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer;

    /// How many `tile`-tall units this widget currently occupies in the
    /// dock's vertical stack. Most widgets are exactly one square tile;
    /// override when a widget's rendered size varies (e.g. by mode).
    fn tile_height(&self) -> u32 {
        1
    }

    /// Pointer input inside this widget's own tile, in that tile's
    /// coordinates (origin at its top-left), with `tile` the tile edge
    /// length — so a widget can carve its face into control zones (a
    /// volume widget's louder/softer halves) without knowing where in
    /// the column the dock put it.
    ///
    /// Returns what it wants done: [`Effect::Repaint`] if its pixels
    /// changed, [`Effect::Run`] for a command, [`Effect::Resample`] to
    /// hurry a sampler. It returns intents rather than acting because a
    /// `wpctl set-volume` or an `nmcli radio wifi off` arrives on the
    /// compositor's repaint thread and can park it exactly as well as a
    /// sample could — the click path was always the sampling problem
    /// wearing a different hat.
    ///
    /// Most widgets have no input behavior; the default no-op covers
    /// them.
    fn on_input(&mut self, input: DockInput, tile: u32) -> Vec<Effect> {
        let _ = (input, tile);
        Vec::new()
    }
}

/// How often the dock re-reads the system on a widget's behalf: the
/// stock [`Source`] interval, shared so the instruments that have no
/// reason to differ visibly agree.
///
/// One second is a reading rate, not a frame rate. `update` is called
/// roughly sixty times a second and folds nothing on fifty-nine of
/// them; that asymmetry is the normal, healthy state of a dock tile
/// and is why [`Samples::fresh`] exists.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);
