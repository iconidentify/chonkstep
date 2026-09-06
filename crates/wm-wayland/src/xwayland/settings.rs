//! Publish X11 toolkit scale without overriding unrelated user appearance.
//! Uses an independent XSETTINGS connection, never the XWM event queue.

use chonk_xsettings::{DesktopAppearance, ManagerState, XSettingsError, XSettingsManager};

use crate::state::Compositor;

impl Compositor {
    /// What this session tells X clients about its own appearance.
    ///
    /// Scale and nothing else, deliberately. `DesktopAppearance` can
    /// also carry a widget theme, an icon theme, a cursor theme and a
    /// default font, and every one of those is left unstated because
    /// this desktop does not ship them: there is no GTK theme named
    /// "chonkstep" and no Xcursor theme either, so publishing the name
    /// would not make applications look like chonkstep — it would make
    /// every GTK client on the display fail to find the theme, fall
    /// back to its default, and in the process *override* whatever the
    /// user had configured in their own `gtk-3.0/settings.ini`. Saying
    /// nothing leaves that setting alone, which is the honest answer to
    /// a question this desktop has no opinion on. (`DesktopAppearance`
    /// treats an empty theme name as exactly that — see its `Default`.)
    ///
    /// The scale it does state is the same number, from the same base,
    /// that `chonk_shell::startup::xcursor_size_for` derives
    /// `XCURSOR_SIZE` from. The two mechanisms overlap on purpose and
    /// must not disagree: a client can be reached by either one, and a
    /// pointer that changes size as it crosses a window border is what
    /// disagreement looks like.
    fn appearance(&self) -> DesktopAppearance {
        DesktopAppearance::new(self.ui_scale, "")
    }

    /// Takes the XSETTINGS manager selection on the freshly-started
    /// XWayland display and publishes this session's scale to it.
    ///
    /// # Why a second X connection
    ///
    /// This process already speaks X to Xwayland — that is what
    /// `X11Wm` is — but that connection is smithay's, driven by
    /// smithay's own calloop source, and `XSettingsManager` consumes
    /// the connection it is given and reads its event queue. Two
    /// readers on one queue would each swallow events meant for the
    /// other, which on the window-manager connection means dropped map
    /// requests. So the manager opens its own, exactly as an external
    /// settings daemon would.
    ///
    /// # Why failure is not fatal
    ///
    /// Something else owning `_XSETTINGS_S0` is a legitimate
    /// configuration — a user running `xsettingsd` for their own
    /// reasons — and the crate reports it as a clean `AlreadyOwned`
    /// rather than an error. Standing down is then the correct
    /// behaviour, not a degraded one: two managers fighting over the
    /// selection would leave clients following whichever wrote last.
    /// Everything else that can go wrong here (a display that vanished
    /// between `Ready` and this call, an X server refusing the window)
    /// costs the session its live scale publishing and nothing else,
    /// which is precisely what the session had before this existed.
    pub(crate) fn start_xsettings(&mut self, display_number: u32) {
        // The display is named explicitly rather than inherited from
        // `DISPLAY`: this runs inside the same handler that sets that
        // variable, and letting which display gets the settings depend
        // on the order of two lines in one function is a trap worth not
        // laying. (Called `display_name` because a bare `display` field
        // in a `tracing` macro resolves to `tracing::field::display`,
        // which the expansion has in scope, and a local of that name
        // loses to it — silently, as a type error about `Value`.)
        let display_name = format!(":{display_number}");
        // TakeOverPlaceholder: XWayland claims this selection at startup
        // and publishes an empty settings block — a squatter, not a
        // manager, and its emptiness is why X11 toolkits under this
        // compositor got no DPI at all. The policy takes over only an
        // owner whose property is absent or a valid zero-settings
        // block; a real manager (a user's own xsettingsd) still gets
        // the same respectful refusal as before.
        let mut manager = match XSettingsManager::acquire_with_policy(
            Some(&display_name),
            chonk_xsettings::AcquisitionPolicy::TakeOverPlaceholder,
        ) {
            Ok(manager) => manager,
            Err(error @ XSettingsError::AlreadyOwned { .. }) => {
                tracing::info!(%error, display = display_name, "another XSETTINGS manager owns this display; leaving it alone");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, display = display_name, "could not publish XSETTINGS; X11 clients will only get the scale their launcher gave them");
                return;
            }
        };
        let appearance = self.appearance();
        if let Err(error) = manager.publish_appearance(&appearance) {
            tracing::warn!(%error, "could not publish the initial XSETTINGS");
            return;
        }
        match manager.publish_resource_manager(&appearance) {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!("XWayland RESOURCE_MANAGER already carries the current scale");
            }
            Err(error) => {
                // The XSETTINGS property is already useful and lives on
                // this same connection, so a malformed shared root
                // property must not discard it. A later real scale
                // change retries the resource merge.
                tracing::warn!(%error, "could not publish the initial XWayland resource database");
            }
        }
        tracing::info!(
            display = display_name,
            scale = appearance.ui_scale,
            cursor_px = appearance.effective_cursor_size(),
            "publishing XSETTINGS and X resources to XWayland clients"
        );
        self.xwayland.settings = Some(manager);
    }

    /// Services the XSETTINGS connection: answers selection requests
    /// and notices if another manager has taken over.
    ///
    /// Not optional bookkeeping. Two things go wrong without it, and
    /// only one of them is ours. A client that asks to *convert* the
    /// selection and gets no answer does not fail — it waits out its own
    /// timeout, which the user experiences as an application that hangs
    /// on startup for no reason. And a manager that never learns it was
    /// superseded goes on rewriting a property it no longer owns, which
    /// ICCCM forbids a former owner from doing and which leaves clients
    /// following whichever of the two wrote last.
    ///
    /// Driven off this loop's existing wakeups rather than a calloop
    /// source on the connection's descriptor: `poll` is non-blocking and
    /// drains whatever has arrived, the loop already has a bounded idle
    /// housekeeping poll, and the crate's own documentation says a timer
    /// is a sufficient home for it. The cost of being up to 100 ms late
    /// to notice a takeover is nothing; the cost of a second event source
    /// is a second thing to unregister on teardown.
    pub(crate) fn poll_xsettings(&mut self) {
        let Some(manager) = self.xwayland.settings.as_mut() else {
            return;
        };
        match manager.poll() {
            Ok(ManagerState::Owner) => {}
            Ok(ManagerState::Superseded) => {
                // The crate has already logged the takeover and latched
                // itself into refusing writes; dropping the handle is
                // this session agreeing, and stops every later scale
                // change asking again.
                tracing::info!("another XSETTINGS manager took the selection; standing down");
                self.xwayland.settings = None;
            }
            Err(error) => {
                tracing::warn!(%error, "the XSETTINGS connection failed; giving up on it for this session");
                self.xwayland.settings = None;
            }
        }
    }

    /// Republishes the appearance after a live scale change.
    ///
    /// The whole reason the XSETTINGS crate exists: an environment
    /// variable is read once at launch, so before this, changing the
    /// scale left every already-running X application at the size it
    /// started at until it was restarted. `publish_appearance` writes
    /// the property only when a value actually moved, so calling this
    /// on a reload that changed nothing else costs one map walk and no
    /// round trip — which matters, because writing the property wakes
    /// every client on the display and a GTK application answers by
    /// re-laying out every window it has.
    pub(crate) fn republish_xsettings(&mut self) {
        let appearance = self.appearance();
        let Some(manager) = self.xwayland.settings.as_mut() else {
            return;
        };
        let xsettings_changed = match manager.publish_appearance(&appearance) {
            Ok(true) => true,
            Ok(false) => false,
            Err(error) => {
                // Losing the selection to another manager is one of the
                // ways this fails, and the crate has already latched
                // itself into standing down; dropping our handle stops
                // this session asking again once per scale change for
                // the rest of its life.
                tracing::warn!(%error, "could not republish XSETTINGS; giving up on it for this session");
                self.xwayland.settings = None;
                return;
            }
        };
        match manager.publish_resource_manager(&appearance) {
            Ok(resources_changed) if xsettings_changed || resources_changed => {
                tracing::info!(
                    scale = appearance.ui_scale,
                    xsettings_changed,
                    resources_changed,
                    "told X11 clients about the new UI scale"
                );
            }
            Ok(_) => {}
            Err(error) => {
                // Keep the XSETTINGS manager: an unexpected shared root
                // property is not a reason to stop serving toolkits that
                // use the selection we still own. Since the resource
                // cache advances only after success, a later scale
                // change will retry this merge.
                tracing::warn!(%error, "could not republish the XWayland resource database");
            }
        }
    }
}
