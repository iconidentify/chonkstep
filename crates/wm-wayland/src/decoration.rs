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
//! libadwaita headerbar from wearing a chonkstep titlebar above it.
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
    /// The client said it draws its own.
    WantsClientSide,
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
                Some(true) => DecorationEvidence::WantsClientSide,
                Some(false) | None => DecorationEvidence::WantsServerSide,
            };
        }
        if self.kde_object {
            return match self.kde_client_side {
                Some(true) => DecorationEvidence::WantsClientSide,
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
            return DecorationEvidence::WantsClientSide;
        }
        DecorationEvidence::Silent
    }
}

/// The decoration decision for one window: `true` when the client draws
/// its own chrome and this desktop must not frame it.
///
/// `identity` is the client's `app_id` on Wayland, or its `WM_CLASS` on
/// X11 — whichever string a `[decorations]` rule would name.
pub(crate) fn client_draws_own_chrome(
    rules: &wm_config::DecorationRules,
    identity: Option<&str>,
    evidence: DecorationEvidence,
) -> bool {
    // A user override outranks everything, in both directions. This is
    // the layer every mature window manager has and the one this
    // desktop was missing: KWin's "No titlebar and frame" at *Force*
    // strength, labwc's `serverDecoration="yes"`, Window Maker's
    // `IgnoreDecorationChanges`. Without it, a client that answers the
    // protocol wrongly is unanswerable.
    if let Some(force_server_side) = rules.decision_for(identity) {
        return !force_server_side;
    }
    match evidence {
        DecorationEvidence::WantsClientSide => true,
        // Silence is framed — the one guess in this module, argued in
        // the module docs.
        DecorationEvidence::WantsServerSide | DecorationEvidence::Silent => false,
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
            }
            backend.queue(wm_core::BackendEvent::ChromeChanged(id));
        }
    }
}

/// Hand-written rather than delegated, for one reason: smithay's own
/// `bind` reports nothing to the compositor, and *which clients bound
/// this manager* is load-bearing evidence here (see
/// [`DecorationNegotiation::kde_manager_bound`]). The two per-object
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
        assert!(!client_draws_own_chrome(&no_rules(), Some("chrome-discord.com__channels_@me-Default"), chrome_app.evidence()));

        // Chrome's ordinary browser window: asks for client-side and
        // means it — its frame is fused with the tab strip.
        let chrome_browser = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(true), ..Default::default() };
        assert!(client_draws_own_chrome(&no_rules(), Some("google-chrome"), chrome_browser.evidence()));

        // foot asks for server-side outright.
        let foot = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(false), ..Default::default() };
        assert!(!client_draws_own_chrome(&no_rules(), Some("foot"), foot.evidence()));

        // LibreOffice speaks only the KDE protocol, and with the
        // manager advertised it asks for our chrome.
        let libreoffice = DecorationNegotiation { kde_object: true, kde_client_side: Some(false), kde_manager_bound: true, ..Default::default() };
        assert!(!client_draws_own_chrome(&no_rules(), Some("libreoffice-writer"), libreoffice.evidence()));

        // A GTK4 headerbar app: bound the manager, created nothing.
        let nautilus = DecorationNegotiation { kde_manager_bound: true, ..Default::default() };
        assert!(client_draws_own_chrome(&no_rules(), Some("org.gnome.Nautilus"), nautilus.evidence()));
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
        assert!(!client_draws_own_chrome(&no_rules(), Some("sdl2-game"), silent.evidence()));
    }

    /// Both override directions, and the one that rescues a window.
    #[test]
    fn a_rule_overrules_the_protocol_in_both_directions() {
        let asks_client_side = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(true), ..Default::default() };
        let rules = DecorationRules { server_side: vec!["alacritty".into()], client_side: Vec::new() };
        assert!(
            !client_draws_own_chrome(&rules, Some("Alacritty"), asks_client_side.evidence()),
            "a terminal that asks for client-side and draws nothing can be forced back into a frame"
        );

        let asks_server_side = DecorationNegotiation { xdg_object: true, xdg_client_side: Some(false), ..Default::default() };
        let rules = DecorationRules { server_side: Vec::new(), client_side: vec!["stubborn".into()] };
        assert!(client_draws_own_chrome(&rules, Some("stubborn-app"), asks_server_side.evidence()));
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
        assert!(!client_draws_own_chrome(&no_rules(), Some("libreoffice-writer"), released.evidence()));
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
            kde_manager_bound: true,
        };
        assert_eq!(both.evidence(), DecorationEvidence::WantsClientSide);
    }

}
