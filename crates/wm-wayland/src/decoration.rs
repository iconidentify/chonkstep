//! Who draws the titlebar.
//!
//! One question, asked of every window that maps, whose wrong answers
//! are both bad in ways users notice immediately: a window wearing two
//! titlebars, or a window wearing none — nothing to drag, no buttons,
//! no resize bar. This module holds the evidence, the policy that reads
//! it, and the second decoration protocol that most of this desktop's
//! clients turn out to speak.
//!
//! # Two protocols, not one
//!
//! `zxdg_decoration_manager_v1` is the standard, and it is not the one
//! GTK speaks. GTK — GTK3 through `libgdk-3.so.0` and GTK4 alike —
//! implements only KDE's older `org_kde_kwin_server_decoration`, and
//! never binds the xdg interface at all. A compositor advertising only
//! xdg-decoration therefore hears *silence* from every GTK application
//! on the system, whatever those applications would have said.
//!
//! That silence is what put two titlebars on LibreOffice. GTK asks
//! `gdk_wayland_display_prefers_ssd()`, finds no KDE manager, concludes
//! this compositor does not do server-side decorations, and draws its
//! own titlebar; we see a client that negotiated nothing, frame it, and
//! the user gets both. Neither side is misbehaving. The bug is the
//! missing protocol, and the fix is to advertise it — which is what
//! KWin, Sway, labwc and Hyprland all do, all with `default_mode =
//! Server`.
//!
//! # What each client actually says
//!
//! Measured on this machine with `WAYLAND_DEBUG=1`, which is the only
//! way any of this was ever going to be settled:
//!
//! | client | protocol | says |
//! |---|---|---|
//! | Chrome `--app=` (a web app) | xdg | `set_mode(server_side)` |
//! | Chrome, ordinary browser window | xdg | `set_mode(client_side)` |
//! | foot | xdg | `set_mode(server_side)` |
//! | alacritty, `decorations = "None"` | xdg | `set_mode(client_side)`, then draws nothing |
//! | LibreOffice (gtk3) | KDE | `create` + `request_mode(server)` |
//! | Nautilus (GTK4, headerbar) | KDE | binds the manager, creates nothing |
//!
//! Every one of those is the client telling the truth about itself, and
//! the policy below is mostly the act of believing it. The desktop that
//! shipped before this module read none of it: it decided from an
//! `app_id` prefix list, which matched `chrome-<host>-<profile>` (a
//! `--app` window — the one asking for *server*-side decorations) while
//! missing `google-chrome` (the browser window, which asks for
//! client-side and draws its own). Both bugs, in one list, in a day.
//!
//! # Who gets the last word
//!
//! "Mostly" believing, because the two protocols mean different things
//! by a client-side request, and only one of them is a request.
//!
//! xdg-decoration is a *negotiation*, and the specification gives the
//! compositor the final say: `set_mode` states a preference, the
//! `configure` event states the decision, and a client "must obey the
//! mode" it is configured with. This desktop's decision is its own
//! chrome. A client that asked for client-side hears `server_side`
//! back and, by the protocol it chose to speak, draws no titlebar of
//! its own; we draw ours. That is what Hyprland does for every
//! xdg-decoration client without exception — which matters here
//! because it means every application Omarchy ships has already been
//! living under a compositor that answers this way. The case that
//! made this the policy rather than an option: Omarchy launches each
//! of its TUIs (`omarchy-update`, About, btop, whatever a user adds)
//! as the same alacritty under a different `org.omarchy.<name>`
//! class, with `decorations = "None"` because Hyprland has no
//! titlebars to want. Each one asks for client-side and draws
//! nothing. Believing the ask leaves an open-ended family of bare
//! rectangles that only a per-name list could rescue, one name at a
//! time; concluding the negotiation frames all of them, and any the
//! user installs tomorrow, with no list at all.
//!
//! The KDE protocol's client-side is a *declaration*. GTK3 asks for
//! `Client` after it has already decided to draw a headerbar, and lays
//! the window out from that decision, not from our answer; GTK4 never
//! asks at all (the asymmetry below). Imposing a frame on either gives
//! two titlebars with no way back, so those are believed as stated.
//! `[decorations] client_side` is the door for an xdg client that has
//! a real reason to stay bare — a borderless game, a kiosk — and
//! `server_side` the one for a KDE-protocol or X11 client that says
//! client-side and then draws nothing.
//!
//! # Three answers
//!
//! Believing a client-side declaration used to mean leaving the window
//! bare, and bare was only half an answer. A header-bar client draws a
//! titlebar and nothing a pointer can resize it by: GTK's resize band
//! lives in the invisible shadow outside its window geometry, and GTK
//! removes the band altogether once a window is tiled, which the layout
//! modes ask for. So a client that draws its own titlebar gets
//! [`ClientChrome::Edges`]: this desktop's themed borders and resize
//! handles around it, and no titlebar or buttons of ours. The toplevel is
//! then told it is tiled on all four edges, so it draws within its window
//! geometry, with no shadow lying over our borders and no resize band
//! competing with our handles.
//!
//! [`ClientChrome::Bare`] is left for a window a user asked to keep bare,
//! through `[decorations] client_side`, and [`ClientChrome::Full`] is
//! everything else. `[decorations] frame_client_drawn` extends full
//! chrome to the one client-side answer that is inferred rather than
//! declared — the GTK4 silence below — for a user who wants this
//! desktop's titlebar and buttons over every header bar.
//!
//! # The GTK4 asymmetry
//!
//! GTK4's `gdk_wayland_toplevel_set_decorated` early-returns when the
//! value is unchanged, and `GdkToplevel:decorated` defaults to
//! `FALSE` — so a GTK4 window that wants to draw its own chrome creates
//! no decoration object at all, while one that wants ours creates it
//! and requests `Server`. The absence is therefore not silence: a
//! client that *bound the manager* and then created nothing for a
//! toplevel has told us that toplevel is client-decorated, because the
//! other branch would have spoken. That distinction is what keeps a
//! libadwaita headerbar from wearing a chonkstep titlebar above it. It
//! is still inference rather than a statement, which is why it is the
//! one answer `frame_client_drawn` may overrule.
//!
//! # Where the asymmetry argument still applies
//!
//! A client that binds neither protocol is genuinely ambiguous, and the
//! xdg preamble's answer ("clients continue to self-decorate as they
//! see fit") is not a safe reading here: SDL2, GLFW without libdecor,
//! and foot configured `csd.preferred=none` are all silent for the
//! opposite reason and draw nothing. So silence is framed. That is a
//! deliberate deviation from the specification, it is the one place
//! this module guesses, and `[decorations] client_side` is how a user
//! corrects it.

use std::sync::atomic::Ordering;

use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::{
    Mode as KdeMode, OrgKdeKwinServerDecoration,
};
use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::{
    Mode as KdeDefaultMode, OrgKdeKwinServerDecorationManager,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DataInit, DisplayHandle, GlobalDispatch, New, WEnum};
use smithay::wayland::shell::kde::decoration::{KdeDecorationHandler, KdeDecorationManagerGlobalData, KdeDecorationState};

use crate::state::{ClientState, Compositor};
use wm_core::ClientChrome;

/// What a client has told us about who draws its chrome.
///
/// Deliberately three-valued. The two-valued version of this question —
/// "did it ask for client-side, yes or no" — is the one that cannot
/// tell a GTK4 headerbar apart from an SDL2 window, and collapsing them
/// is how a desktop ends up choosing which of the two bugs to ship.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecorationEvidence {
    /// The client asked for, or accepted, this desktop's chrome.
    WantsServerSide,
    /// The client asked, over xdg-decoration, to draw its own — a
    /// preference in a negotiation this compositor concludes the other
    /// way, and which the client is bound by its protocol to accept.
    /// Framed; see "Who gets the last word" in the module docs.
    PrefersClientSide,
    /// The client has declared over the KDE protocol, with
    /// `request_mode(Client)`, that it draws its own chrome and will
    /// whatever we answer. Believed: it gets edge chrome, never a second
    /// titlebar.
    DeclaresClientSide,
    /// The client asked over the KDE protocol for `None`: no decoration
    /// from either side. Left bare, like an X11 client whose Motif hint
    /// declines decoration.
    DeclinesDecoration,
    /// The client bound the KDE manager and created no decoration object
    /// for this toplevel, which is GTK4's way of saying it draws its own
    /// chrome (see "The GTK4 asymmetry"). Edge chrome, like a
    /// declaration, unless `[decorations] frame_client_drawn` asks for
    /// the whole frame.
    ClientSideBySilence,
    /// The client bound no decoration protocol at all and has told us
    /// nothing. Framed — see the module docs.
    Silent,
}

/// Everything the two decoration protocols have said about one
/// toplevel, kept as *evidence* rather than as a decision.
///
/// The distinction matters: the fields here are written by protocol
/// handlers, which run in whatever order a client happens to speak in,
/// while the decision has to be re-derivable at any moment — at map
/// time, when an `app_id` finally arrives, when a config reload changes
/// an override. The predecessor of this struct stored the same facts
/// and was *read by nothing*: two fields, written in three places, with
/// a decision made from an unrelated string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DecorationNegotiation {
    /// A `zxdg_toplevel_decoration_v1` exists for this toplevel.
    pub xdg_object: bool,
    /// The last explicit xdg mode request: `Some(true)` for
    /// client-side, `Some(false)` for server-side, `None` for
    /// `unset_mode` or no request yet.
    pub xdg_client_side: Option<bool>,
    /// An `org_kde_kwin_server_decoration` exists for this surface.
    pub kde_object: bool,
    /// The last KDE mode request, in the same shape as the xdg one.
    /// KDE's third mode, `None` ("no decoration at all, from either
    /// side"), records as client-side: it is a refusal of our chrome,
    /// and the client that asks for it has accepted what it costs.
    pub kde_client_side: Option<bool>,
    /// That last KDE request was `None` rather than `Client`: the client
    /// wants no edges either.
    pub kde_declines_decoration: bool,
    /// An `org_kde_kwin_server_decoration` has existed for this surface
    /// at some point, even if it has since been released.
    ///
    /// Distinguishes "never spoke for this toplevel" (the GTK4
    /// client-side tell below) from "spoke and then let go", which is a
    /// return to the mode we advertise on bind — `Server` — not a
    /// refusal of it. Without the distinction, a client that released
    /// the object on a window it was still showing would have had its
    /// frame taken off underneath it.
    pub kde_object_seen: bool,
    /// This surface's client bound `org_kde_kwin_server_decoration_manager`.
    ///
    /// Tracked because for GTK4 the *absence* of a per-surface object
    /// from a client that bound the manager is itself the answer — see
    /// the module docs.
    pub kde_manager_bound: bool,
}

impl DecorationNegotiation {
    /// The evidence these facts add up to.
    ///
    /// xdg outranks KDE where a client has somehow used both (the KDE
    /// protocol's own text calls that combination undefined) because
    /// xdg is the standard one and the one a client would reach for
    /// deliberately.
    pub(crate) fn evidence(&self) -> DecorationEvidence {
        if self.xdg_object {
            // Bound and silent is `unset_mode` or no request yet, which
            // is "you decide" — and this desktop decides server-side.
            // KWin, labwc, Sway and Hyprland all answer that the same
            // way; only smithay's own default goes the other direction,
            // silently, inside `send_configure`.
            return match self.xdg_client_side {
                Some(true) => DecorationEvidence::PrefersClientSide,
                Some(false) | None => DecorationEvidence::WantsServerSide,
            };
        }
        if self.kde_object {
            if self.kde_declines_decoration {
                return DecorationEvidence::DeclinesDecoration;
            }
            return match self.kde_client_side {
                Some(true) => DecorationEvidence::DeclaresClientSide,
                Some(false) | None => DecorationEvidence::WantsServerSide,
            };
        }
        if self.kde_object_seen {
            // Negotiated once and released. The object is how a client
            // states a preference, so letting it go is a return to the
            // default this compositor advertises on bind, which is
            // `Server` — not the GTK4 tell below, which is about a
            // toplevel that never had one.
            return DecorationEvidence::WantsServerSide;
        }
        if self.kde_manager_bound {
            // GTK4's early return: a client that speaks this protocol
            // and creates nothing for a toplevel is declining our
            // chrome for it. A GTK4 window that wanted ours would have
            // created the object and asked.
            return DecorationEvidence::ClientSideBySilence;
        }
        DecorationEvidence::Silent
    }
}

/// The chrome one toplevel wears: the full frame, edges around its own
/// titlebar, or nothing.
///
/// `identity` is the client's `app_id` on Wayland, or its `WM_CLASS` on
/// X11 — whichever string a `[decorations]` rule would name.
///
/// Precedence, strongest first: a `server_side` entry, a `client_side`
/// entry, what the client declared, and `frame_client_drawn` for the one
/// client-side answer nobody declared.
pub(crate) fn client_chrome(
    rules: &wm_config::DecorationRules,
    identity: Option<&str>,
    evidence: DecorationEvidence,
) -> ClientChrome {
    // A user override outranks everything, in both directions. Without
    // it, a client that answers the protocol wrongly is unanswerable.
    if let Some(force_server_side) = rules.decision_for(identity) {
        return if force_server_side { ClientChrome::Full } else { ClientChrome::Bare };
    }
    match evidence {
        // Believed: the client has laid its window out around its own
        // titlebar, so ours would be a second one. The edges are still
        // ours, because nothing else is going to resize it.
        DecorationEvidence::DeclaresClientSide => ClientChrome::Edges,
        DecorationEvidence::DeclinesDecoration => ClientChrome::Bare,
        DecorationEvidence::ClientSideBySilence if rules.frame_client_drawn => ClientChrome::Full,
        DecorationEvidence::ClientSideBySilence => ClientChrome::Edges,
        // An xdg preference for client-side is overruled, not
        // believed: the negotiation ends with our configure, and the
        // client draws nothing once it hears `server_side`. Silence is
        // framed too — the one guess in this module, argued in the
        // module docs.
        DecorationEvidence::PrefersClientSide | DecorationEvidence::WantsServerSide | DecorationEvidence::Silent => {
            ClientChrome::Full
        }
    }
}

// -- org_kde_kwin_server_decoration --------------------------------------

/// The default mode advertised to every client that binds the KDE
/// manager: `Server`.
///
/// This single value decides how every GTK application on the system
/// looks, because `gdk_wayland_display_prefers_ssd()` is a plain
/// equality test against it and feeds `gtk_window_should_use_csd()`.
/// KWin, Sway, labwc and Hyprland all advertise `Server`; cosmic-comp
/// is the lone `Client`.
pub(crate) const KDE_DEFAULT_MODE: KdeDefaultMode = KdeDefaultMode::Server;

impl KdeDecorationHandler for Compositor {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.kde_decoration
    }

    fn new_decoration(&mut self, surface: &WlSurface, decoration: &OrgKdeKwinServerDecoration) {
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface) {
            if let Some(record) = backend.windows.get_mut(&id) {
                record.decoration.kde_object = true;
                record.decoration.kde_object_seen = true;
            }
            backend.queue(wm_core::BackendEvent::ChromeChanged(id));
        }
        // Answer immediately and unprompted. GTK3 creates this object
        // and then waits: it takes the mode event as the compositor's
        // decision and lays out its window from it, so a client that
        // hears nothing here draws a titlebar we are about to draw
        // underneath.
        //
        // A client that creates the object and asks for a mode in the
        // same breath — which is what GDK does — therefore receives two
        // identical mode events. That is deliberate and harmless: the
        // event is idempotent, and the alternative is staying silent to
        // a client that creates the object and waits.
        decoration.mode(KdeMode::Server);
    }

    fn request_mode(&mut self, surface: &WlSurface, decoration: &OrgKdeKwinServerDecoration, mode: WEnum<KdeMode>) {
        let asked_client_side = match mode {
            // KDE's `None` is "no decoration at all, from either side" —
            // a mode the xdg protocol has no word for. Recorded as
            // client-side because it is a refusal of our chrome, and
            // answered honestly below rather than with a mode the
            // client did not ask for.
            WEnum::Value(KdeMode::None) | WEnum::Value(KdeMode::Client) => true,
            WEnum::Value(KdeMode::Server) => false,
            // An unknown mode from a client speaking a newer protocol
            // than we implement. Framing is the recoverable direction.
            _ => false,
        };
        let backend = self.wm.backend_mut();
        let mut answer_client_side = asked_client_side;
        if let Some(id) = backend.window_for_surface(surface) {
            if let Some(record) = backend.windows.get_mut(&id) {
                record.decoration.kde_object = true;
                record.decoration.kde_object_seen = true;
                record.decoration.kde_client_side = Some(asked_client_side);
                record.decoration.kde_declines_decoration = matches!(mode, WEnum::Value(KdeMode::None));
            }
            // A `[decorations]` override has to reach the wire too, not
            // just the frame: a client told "client-side" draws a
            // titlebar, and `server_side = [...]` exists precisely to
            // stop that.
            if let Some(record) = backend.windows.get(&id) {
                let identity = record.app_id.as_deref();
                if let Some(force_server_side) = backend.decoration_rules.decision_for(identity) {
                    answer_client_side = !force_server_side;
                }
            }
            backend.queue(wm_core::BackendEvent::ChromeChanged(id));
        }
        // Answer with our own policy value, never by echoing the
        // request back. smithay's default handler echoes, and its own
        // documentation warns that preventing feedback loops is the
        // compositor's job — an echo is what makes one possible.
        decoration.mode(if answer_client_side { KdeMode::Client } else { KdeMode::Server });
    }

    fn release(&mut self, _decoration: &OrgKdeKwinServerDecoration, surface: &WlSurface) {
        // Releasing the object drops the client's stated preference
        // and returns it to the mode advertised on bind (`Server`) —
        // see `kde_object_seen`. Observed in the wild only as part of
        // teardown: GTK releases it when the toplevel is destroyed, and
        // LibreOffice's startup does that twice before its real window
        // appears.
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface) {
            if let Some(record) = backend.windows.get_mut(&id) {
                record.decoration.kde_object = false;
                record.decoration.kde_client_side = None;
                record.decoration.kde_declines_decoration = false;
            }
            backend.queue(wm_core::BackendEvent::ChromeChanged(id));
        }
    }
}

/// Hand-written rather than delegated, for one reason: smithay's own
/// `bind` reports nothing to the compositor, and *which clients bound
/// this manager* is load-bearing evidence here (see
/// `DecorationNegotiation::kde_manager_bound`). The two per-object
/// dispatches below stay delegated to smithay.
impl GlobalDispatch<OrgKdeKwinServerDecorationManager, KdeDecorationManagerGlobalData> for Compositor {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        client: &Client,
        resource: New<OrgKdeKwinServerDecorationManager>,
        _global_data: &KdeDecorationManagerGlobalData,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        if let Some(data) = client.get_data::<ClientState>() {
            data.kde_decoration_bound.store(true, Ordering::Relaxed);
        }
        let _ = state;
        // The `default_mode` event is what GTK reads, and it is sent on
        // bind — before any surface exists.
        manager.default_mode(KDE_DEFAULT_MODE);
    }

    fn can_view(_client: Client, _global_data: &KdeDecorationManagerGlobalData) -> bool {
        // No filter: every client may ask. smithay's own global data
        // carries one, but its field is private to that crate, and this
        // compositor advertises the protocol to everybody anyway.
        true
    }
}

smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [OrgKdeKwinServerDecorationManager: ()] => KdeDecorationState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [OrgKdeKwinServerDecoration: WlSurface] => KdeDecorationState);

#[cfg(test)]
mod tests {
    use super::*;
    use wm_config::DecorationRules;

    fn no_rules() -> DecorationRules {
        DecorationRules::default()
    }

    /// Each row of the table in the module docs, as the wire actually
    /// carried it — the regression test for both shipped bugs at once.
    #[test]
    fn the_measured_clients_get_the_chrome_they_asked_for() {
        // Chrome's web-app window: asks for server-side, and drew no
        // titlebar of its own when we declined to frame it.
        let chrome_app = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(false), ..Default::default() };
        assert_eq!(client_chrome(&no_rules(), Some("chrome-discord.com__channels_@me-Default"), chrome_app.evidence()), ClientChrome::Full);

        // Chrome's ordinary browser window asks for client-side; the
        // negotiation ends with our `server_side`, which Chrome obeys
        // by folding its own frame away (as it does under Hyprland),
        // so it is framed and wears one titlebar, ours.
        let chrome_browser = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(true), ..Default::default() };
        assert_eq!(chrome_browser.evidence(), DecorationEvidence::PrefersClientSide);
        assert_eq!(client_chrome(&no_rules(), Some("google-chrome"), chrome_browser.evidence()), ClientChrome::Full);

        // alacritty with Omarchy's `decorations = "None"`, under any of
        // the classes Omarchy launches it as: the same ask, the same
        // answer, and a frame with no rule naming it.
        let alacritty = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(true), ..Default::default() };
        for class in ["Alacritty", "org.omarchy.terminal", "org.omarchy.about", "org.omarchy.btop"] {
            assert_eq!(client_chrome(&no_rules(), Some(class), alacritty.evidence()), ClientChrome::Full, "{class} is framed");
        }

        // foot asks for server-side outright.
        let foot = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(false), ..Default::default() };
        assert_eq!(client_chrome(&no_rules(), Some("foot"), foot.evidence()), ClientChrome::Full);

        // LibreOffice speaks only the KDE protocol, and with the
        // manager advertised it asks for our chrome.
        let libreoffice = DecorationNegotiation { kde_object: true, kde_client_side: Some(false), kde_manager_bound: true, ..Default::default() };
        assert_eq!(client_chrome(&no_rules(), Some("libreoffice-writer"), libreoffice.evidence()), ClientChrome::Full);

        // A GTK4 header-bar app: bound the manager, created nothing. Its
        // header bar is the titlebar; the borders and handles are ours.
        let nautilus = DecorationNegotiation { kde_manager_bound: true, ..Default::default() };
        assert_eq!(nautilus.evidence(), DecorationEvidence::ClientSideBySilence);
        assert_eq!(client_chrome(&no_rules(), Some("org.gnome.Nautilus"), nautilus.evidence()), ClientChrome::Edges);

        // A GTK3 app that asked for `Client` over the KDE protocol has
        // already laid out its headerbar: a declaration, believed.
        let gtk3_csd = DecorationNegotiation { kde_object: true, kde_object_seen: true, kde_client_side: Some(true), kde_manager_bound: true, ..Default::default() };
        assert_eq!(gtk3_csd.evidence(), DecorationEvidence::DeclaresClientSide);
        assert_eq!(client_chrome(&no_rules(), Some("gnome-calculator"), gtk3_csd.evidence()), ClientChrome::Edges);
    }

    /// Bound the interface and expressed no preference: ours.
    #[test]
    fn a_client_that_leaves_the_choice_to_us_gets_our_chrome() {
        let unset = DecorationNegotiation { xdg_object: true, xdg_client_side: None, ..Default::default() };
        assert_eq!(unset.evidence(), DecorationEvidence::WantsServerSide);
        let kde_unset = DecorationNegotiation { kde_object: true, kde_client_side: None, ..Default::default() };
        assert_eq!(kde_unset.evidence(), DecorationEvidence::WantsServerSide);
    }

    /// The deliberate deviation from the specification: a client that
    /// binds nothing is framed, because SDL2 and a `csd.preferred=none`
    /// terminal are silent for the opposite reason to GTK's.
    #[test]
    fn a_client_that_says_nothing_at_all_is_framed() {
        let silent = DecorationNegotiation::default();
        assert_eq!(silent.evidence(), DecorationEvidence::Silent);
        assert_eq!(client_chrome(&no_rules(), Some("sdl2-game"), silent.evidence()), ClientChrome::Full);
    }

    /// KDE's `None` asks for no decoration from either side, so not even
    /// edges: bare unless a `server_side` entry insists, and
    /// `frame_client_drawn`, which only overrules silence, leaves it alone.
    #[test]
    fn a_client_that_declines_all_decoration_over_kde_is_left_bare() {
        let declines = DecorationNegotiation {
            kde_object: true,
            kde_object_seen: true,
            kde_client_side: Some(true),
            kde_declines_decoration: true,
            kde_manager_bound: true,
            ..Default::default()
        };
        assert_eq!(declines.evidence(), DecorationEvidence::DeclinesDecoration);
        assert_eq!(client_chrome(&no_rules(), Some("kiosk"), declines.evidence()), ClientChrome::Bare);
        let frame_all = DecorationRules { frame_client_drawn: true, ..Default::default() };
        assert_eq!(client_chrome(&frame_all, Some("kiosk"), declines.evidence()), ClientChrome::Bare);
        let server_side = DecorationRules { server_side: vec!["kiosk".into()], ..Default::default() };
        assert_eq!(client_chrome(&server_side, Some("kiosk"), declines.evidence()), ClientChrome::Full);
    }

    /// Both override directions: the one that rescues a window whose
    /// client-side is a declaration we cannot overrule on the wire, and
    /// the one that lets an xdg client have the bare window it asked
    /// for after all.
    #[test]
    fn a_rule_overrules_the_protocol_in_both_directions() {
        let declares_client_side = DecorationNegotiation { kde_object: true, kde_object_seen: true, kde_client_side: Some(true), kde_manager_bound: true, ..Default::default() };
        let rules = DecorationRules { server_side: vec!["bare-thing".into()], ..Default::default() };
        assert_eq!(
            client_chrome(&rules, Some("bare-thing"), declares_client_side.evidence()),
            ClientChrome::Full,
            "a client that declares client-side and draws nothing can be given the whole frame"
        );

        let prefers_client_side = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(true), ..Default::default() };
        let rules = DecorationRules { client_side: vec!["borderless-game".into()], ..Default::default() };
        assert_eq!(
            client_chrome(&rules, Some("borderless-game"), prefers_client_side.evidence()),
            ClientChrome::Bare,
            "the user can let an xdg client's preference stand"
        );

        let asks_server_side = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(false), ..Default::default() };
        let rules = DecorationRules { client_side: vec!["stubborn".into()], ..Default::default() };
        assert_eq!(client_chrome(&rules, Some("stubborn-app"), asks_server_side.evidence()), ClientChrome::Bare);
    }

    /// The four answers a GTK4 header-bar window can get: edges from its
    /// silence, what each list names, and the whole frame from
    /// `frame_client_drawn`.
    #[test]
    fn a_silent_header_bar_client_gets_edges_unless_a_setting_says_otherwise() {
        let silent = DecorationNegotiation { kde_manager_bound: true, ..Default::default() }.evidence();
        let app = Some("org.gnome.Nautilus");
        assert_eq!(client_chrome(&no_rules(), app, silent), ClientChrome::Edges);
        let client_side = DecorationRules { client_side: vec!["org.gnome.nautilus".into()], ..Default::default() };
        assert_eq!(client_chrome(&client_side, app, silent), ClientChrome::Bare);
        let server_side = DecorationRules { server_side: vec!["org.gnome.nautilus".into()], ..Default::default() };
        assert_eq!(client_chrome(&server_side, app, silent), ClientChrome::Full);
        let frame_all = DecorationRules { frame_client_drawn: true, ..Default::default() };
        assert_eq!(client_chrome(&frame_all, app, silent), ClientChrome::Full);
    }

    /// `frame_client_drawn` reaches only the inferred answer: a client
    /// that declared its own chrome keeps its edges, a list entry outranks
    /// the switch, and a client that never drew its own is untouched.
    #[test]
    fn frame_client_drawn_yields_to_declarations_and_lists() {
        let frame_all = DecorationRules { frame_client_drawn: true, ..Default::default() };
        let declared = DecorationNegotiation { kde_object: true, kde_object_seen: true, kde_client_side: Some(true), kde_manager_bound: true, ..Default::default() };
        assert_eq!(client_chrome(&frame_all, Some("gnome-calculator"), declared.evidence()), ClientChrome::Edges);
        let kiosk = DecorationRules { client_side: vec!["kiosk".into()], frame_client_drawn: true, ..Default::default() };
        let silent = DecorationNegotiation { kde_manager_bound: true, ..Default::default() }.evidence();
        assert_eq!(client_chrome(&kiosk, Some("kiosk"), silent), ClientChrome::Bare);
        assert_eq!(client_chrome(&frame_all, Some("foot"), DecorationNegotiation::default().evidence()), ClientChrome::Full);
    }

    /// Releasing the decoration object must not take the frame off a
    /// window that is still on screen. LibreOffice releases it twice
    /// during startup, and a client is free to do so on a live window.
    #[test]
    fn releasing_the_decoration_object_returns_to_the_advertised_default() {
        let released = DecorationNegotiation {
            kde_object: false,
            kde_object_seen: true,
            kde_client_side: None,
            kde_manager_bound: true,
            ..Default::default()
        };
        assert_eq!(released.evidence(), DecorationEvidence::WantsServerSide);
        assert_eq!(client_chrome(&no_rules(), Some("libreoffice-writer"), released.evidence()), ClientChrome::Full);
    }

    /// xdg outranks KDE when a client has used both.
    #[test]
    fn the_standard_protocol_wins_a_disagreement() {
        let both = DecorationNegotiation {
            xdg_object: true,
            xdg_client_side: Some(true),
            kde_object: true,
            kde_object_seen: true,
            kde_client_side: Some(false),
            kde_declines_decoration: false,
            kde_manager_bound: true,
        };
        assert_eq!(both.evidence(), DecorationEvidence::PrefersClientSide);
    }

}
