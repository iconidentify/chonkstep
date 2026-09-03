//! The link panel: the LNK tile's fold-out — every network this
//! machine could be on, and the toggles to move between them. Wifi
//! networks (including joining a new one), wired and VPN connection
//! profiles, NetworkManager-native WireGuard tunnels, and the
//! Tailscale tailnet, in one chiseled instrument.
//!
//! Same discipline as every instrument in this crate: the panel is a
//! pure fold. It declares [`Source`]s (all read-only nmcli/tailscale
//! queries), folds the sampled stdout in [`LinkPanel::update`],
//! renders through [`render::render_link_panel`], and answers input
//! with a [`PanelReaction`] instead of performing anything. No entry
//! point here can reach a syscall — the crate's `clippy.toml` makes
//! that a build error, and its dependency list makes it moot.
//!
//! The host is [`crate::WifiWidget`], through the panel half of the
//! `DockWidget` trait (`panel_spec` / `render_panel` / `panel_input`
//! / `panel_tick`); this type is the widget-side brain those four
//! methods delegate to.
//!
//! # The action table
//!
//! Every argv the panel can ever request, in one place. Programs are
//! compile-time `&'static str` (the executor's whitelist is the
//! compiler); the only runtime-supplied argument is a connection UUID
//! or an SSID, each carried as one argv element and never through a
//! shell:
//!
//! | Row | Action | Argv |
//! |---|---|---|
//! | connection (inactive) | activate | `nmcli connection up <uuid>` |
//! | connection (active) | deactivate | `nmcli connection down <uuid>` |
//! | known wifi network | connect | `nmcli connection up <uuid>` |
//! | unknown open network | connect | `nmcli dev wifi connect <ssid>` |
//! | unknown secured network | join | `chonk-netjoin <ssid>` (the dialog owns the secret) |
//! | tailscale (running) | stop | `tailscale down` |
//! | tailscale (stopped) | start | `tailscale up` |
//! | rescan | survey | `nmcli dev wifi list --rescan yes` |
//!
//! `nmcli connection up`/`down` is polkit-free inside an active local
//! session; `tailscale up`/`down` needs the operator grant, and the
//! panel models the refusal honestly (see below) rather than
//! pretending the toggle worked.
//!
//! # Optimism with a deadline
//!
//! `nmcli connection up` can take seconds, so a clicked row shows a
//! dim BUSY lamp immediately — but the toggle is a request, not a
//! fact. The truth is the next sample: every `Effect::Run` here sets
//! `then:` to the source that can confirm it, a pending row
//! reconciles the moment that sample agrees, and a pending that
//! outlives [`PENDING_DEADLINE_SAMPLES`] fresh samples reverts to
//! showing reality, because an instrument that keeps saying BUSY
//! after the system said no is lying with extra steps.
//!
//! # NeedsOperator
//!
//! Tailscale reads are unprivileged; mutation is root-or-operator,
//! and a refused `tailscale down` prints `Access denied: ...` with
//! the `sudo tailscale set --operator=$USER` remedy in it
//! ([`tailscale::classify_toggle_output`]). The panel cannot know the
//! grant in advance, so the first toggle is attempted; a toggle the
//! status sample never confirms — against an operator state never
//! proven — flips the row to
//! [`tailscale::OperatorState::NeedsOperator`]: the toggle goes
//! inert, shows `LOCKED`, and the remedy line is drawn under it,
//! because the honest answer to a click that cannot work is the
//! command that would make it work. A toggle whose confirmation
//! *does* arrive proves the grant and settles the state at `Granted`
//! — one flaky toggle never unproves it. `scripts/install.sh` offers
//! the grant at install time so most desks never see any of this.
//!
//! ## The one wiring friction, stated plainly
//!
//! The denial is only *visible* in the toggle's own output, and the
//! effect executor (`run_detached`) collects and discards that
//! output. [`LinkPanel::command_finished`] is the precise hook — feed
//! it the combined output and the classification is exact — but
//! nothing can call it today, so the shipping detection is the
//! deadline inference above: right whenever the grant is really the
//! problem, and one unlucky lock (cleared by reopening the session)
//! when a toggle fails for some rarer reason while the grant was
//! never proven. If the executor ever hands output back, wire it to
//! `command_finished` and the inference becomes a fallback.
//!
//! # Sampling cadence
//!
//! A widget's sources are declared once and sample for the life of
//! the session — there is no "only while the panel is open" — so the
//! panel's four queries run at [`PANEL_INTERVAL`]/[`TAILSCALE_INTERVAL`]
//! rather than the tile's 1s: cache reads, but process spawns all the
//! same, and nothing on a closed panel is worth a spawn a second.
//! Every action's `then:` hurries the confirming source, so the
//! panel is snappy exactly when someone is watching it.

pub mod data;
pub mod render;
pub mod tailscale;

use std::time::Duration;

use chonk_dock_widget::{Effect, PanelEvent, PanelReaction, PanelSpec, Samples, Source, SourceId};
use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

use data::{parse_connections, parse_devices, parse_wifi_networks, DeviceKind, DeviceState, NmConnection, NmDevice, WifiNetwork};
use render::{panel_layout, render_link_panel_into, ConnRow, Lamp, LinkHeader, NetRow, PanelView, RowKey, TailscaleRow, WifiSection};
use tailscale::{classify_toggle_output, parse_status, BackendState, OperatorState, TailscaleStatus, ToggleVerdict};

/// The nmcli sources' cadence — see the module doc's "Sampling
/// cadence". Three seconds keeps an open panel's rows at most one
/// beat behind reality (and post-click freshness comes from `then:`
/// resamples, not from the interval).
pub const PANEL_INTERVAL: Duration = Duration::from_secs(3);

/// Tailscale's status cadence: slower still, because the tailnet
/// changes on human time and `tailscale status --json` is the most
/// expensive of the four spawns.
pub const TAILSCALE_INTERVAL: Duration = Duration::from_secs(5);

/// How many fresh wifi-list samples the rescan row stays disarmed
/// after firing. At [`PANEL_INTERVAL`] this is the same "an explicit
/// rescan at most every ~15s" budget the chonk-net dockapp
/// established — kept as a sample count so the fold stays pure (no
/// clock is readable here, by design).
pub const RESCAN_COOLDOWN_SAMPLES: u8 = 5;

/// How many fresh confirming-source samples an optimistic toggle may
/// wait before the row stops claiming BUSY and shows reality again.
/// The first confirming sample arrives almost immediately (`then:`),
/// the rest at [`PANEL_INTERVAL`]; eight allows the ~20s a wifi
/// association with DHCP legitimately takes.
pub const PENDING_DEADLINE_SAMPLES: u8 = 8;

/// A command whose output the panel would need to see — today exactly
/// one, because the Tailscale permission denial exists only in that
/// output. See the module doc's "wiring friction": nothing can
/// observe output yet, so [`LinkPanel::command_finished`] currently
/// has no caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedRun {
    TailscaleToggle,
}

/// An optimistic toggle awaiting its confirming sample.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingToggle {
    key: RowKey,
    /// The state the click asked for: `true` = active/running.
    want_on: bool,
    samples_left: u8,
}

pub struct LinkPanel {
    devices_src: SourceId,
    connections_src: SourceId,
    wifi_src: SourceId,
    tailscale_src: SourceId,

    devices: Vec<NmDevice>,
    connections: Vec<NmConnection>,
    networks: Vec<WifiNetwork>,
    /// See `WifiWidget::wifi_hw` — the `wireless/` sysfs probe is the
    /// tile's; the panel's equivalent evidence is a wifi device row
    /// or a nonempty scan list.
    wifi_hw: bool,
    ts: Option<TailscaleStatus>,
    /// The `tailscale` binary could not be spawned at all: the row is
    /// absent, permanently for this session.
    ts_absent: bool,
    operator: OperatorState,
    header: LinkHeader,
    pending: Vec<PendingToggle>,
    rescan_cooldown: u8,
    hover: Option<RowKey>,
    pressed: Option<RowKey>,
    /// The frame size the shell last handed [`LinkPanel::render`], and
    /// so the geometry the pointer is tested against. `None` until the
    /// panel has drawn once.
    granted: Option<(u32, u32)>,
    /// The panel's pixels no longer match its state. Set by folds and
    /// input, taken by [`LinkPanel::take_dirty`] from `panel_tick`.
    dirty: bool,
}

impl LinkPanel {
    pub fn new() -> Self {
        Self {
            devices_src: SourceId::UNBOUND,
            connections_src: SourceId::UNBOUND,
            wifi_src: SourceId::UNBOUND,
            tailscale_src: SourceId::UNBOUND,
            devices: Vec::new(),
            connections: Vec::new(),
            networks: Vec::new(),
            wifi_hw: false,
            ts: None,
            ts_absent: false,
            operator: OperatorState::Unknown,
            header: LinkHeader::Unknown,
            pending: Vec::new(),
            rescan_cooldown: 0,
            hover: None,
            pressed: None,
            granted: None,
            dirty: false,
        }
    }

    /// The four read-only queries the panel lives on, in [`bind`]
    /// order. All cache reads: the wifi list is `--rescan no` (see
    /// [`crate::wifi`] for the measured 3.5s-vs-11ms difference), and
    /// the only rescan is the explicit, cooldown-gated row.
    ///
    /// [`bind`]: LinkPanel::bind
    pub fn sources(&self) -> Vec<Source> {
        vec![
            Source::Command {
                program: "nmcli",
                args: to_args(&["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device", "status"]),
                interval: PANEL_INTERVAL,
            },
            Source::Command {
                program: "nmcli",
                args: to_args(&["-t", "-f", "NAME,TYPE,ACTIVE,UUID", "connection", "show"]),
                interval: PANEL_INTERVAL,
            },
            Source::Command {
                program: "nmcli",
                args: to_args(&["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list", "--rescan", "no"]),
                interval: PANEL_INTERVAL,
            },
            Source::Command { program: "tailscale", args: to_args(&["status", "--json"]), interval: TAILSCALE_INTERVAL },
        ]
    }

    pub fn bind(&mut self, ids: &[SourceId]) {
        self.devices_src = ids.first().copied().unwrap_or(SourceId::UNBOUND);
        self.connections_src = ids.get(1).copied().unwrap_or(SourceId::UNBOUND);
        self.wifi_src = ids.get(2).copied().unwrap_or(SourceId::UNBOUND);
        self.tailscale_src = ids.get(3).copied().unwrap_or(SourceId::UNBOUND);
    }

    /// The size this panel wants at the dock's current tile scale —
    /// the natural size of its laid-out content. A request: the shell
    /// clamps it, and rendering honors the granted frame, not this.
    pub fn spec(&self, tile: u32) -> PanelSpec {
        let layout = panel_layout(&self.view(), tile);
        PanelSpec::new(layout.width, layout.height)
    }

    /// Folds this pass's readings. Returns whether the panel's face
    /// changed — computed by comparing the derived [`PanelView`]s, so
    /// "changed" means exactly "would render differently" — and
    /// remembers it in the dirty flag either way.
    pub fn update(&mut self, samples: &Samples) -> bool {
        let any = samples.fresh(self.devices_src)
            || samples.fresh(self.connections_src)
            || samples.fresh(self.wifi_src)
            || samples.fresh(self.tailscale_src);
        if !any {
            return false;
        }
        let before = self.view();

        if samples.fresh(self.devices_src) {
            if let Some(text) = samples.text(self.devices_src) {
                self.devices = parse_devices(text);
            }
        }
        if samples.fresh(self.connections_src) {
            if let Some(text) = samples.text(self.connections_src) {
                self.connections = parse_connections(text);
            }
            self.reconcile_connections();
        }
        if samples.fresh(self.wifi_src) {
            if let Some(text) = samples.text(self.wifi_src) {
                self.networks = parse_wifi_networks(text);
            }
            self.rescan_cooldown = self.rescan_cooldown.saturating_sub(1);
            self.reconcile_networks();
        }
        if samples.unusable(self.tailscale_src) {
            self.ts_absent = true;
            self.ts = None;
        } else if samples.fresh(self.tailscale_src) {
            if let Some(status) = samples.text(self.tailscale_src).and_then(parse_status) {
                self.ts = Some(status);
            }
            self.reconcile_tailscale();
        }
        self.wifi_hw = self.devices.iter().any(|d| d.kind == DeviceKind::Wifi) || !self.networks.is_empty();

        let changed = before != self.view();
        self.dirty |= changed;
        changed
    }

    /// The current link, from the LNK tile's own sampler (see
    /// `WifiWidget::link_header`) — the panel does not re-sample what
    /// the tile already knows. Returns whether the face changed.
    pub fn set_link_header(&mut self, header: LinkHeader) -> bool {
        if self.header == header {
            return false;
        }
        self.header = header;
        self.dirty = true;
        true
    }

    /// Whether the panel needs repainting, consumed — the widget's
    /// `panel_tick` is one call to this.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// A pending toggle is confirmed by its source of truth, or times
    /// out.
    fn reconcile_connections(&mut self) {
        let connections = &self.connections;
        self.pending.retain_mut(|p| {
            let RowKey::Conn(uuid) = &p.key else { return true };
            let confirmed = connections.iter().any(|c| &c.uuid == uuid && c.active == p.want_on);
            let gone = !connections.iter().any(|c| &c.uuid == uuid);
            if confirmed || gone {
                return false;
            }
            p.samples_left = p.samples_left.saturating_sub(1);
            p.samples_left > 0
        });
    }

    fn reconcile_networks(&mut self) {
        let networks = &self.networks;
        self.pending.retain_mut(|p| {
            let RowKey::Net(ssid) = &p.key else { return true };
            if networks.iter().any(|n| &n.ssid == ssid && n.in_use == p.want_on) {
                return false;
            }
            p.samples_left = p.samples_left.saturating_sub(1);
            p.samples_left > 0
        });
    }

    /// The tailscale pending's two extra rules: confirmation proves
    /// the operator grant, and expiry against a never-proven grant is
    /// the module doc's NeedsOperator inference.
    fn reconcile_tailscale(&mut self) {
        let backend = self.ts.as_ref().map(|s| s.backend);
        let mut proven = false;
        let mut expired_unproven = false;
        let operator_proven = self.operator == OperatorState::Granted;
        self.pending.retain_mut(|p| {
            if p.key != RowKey::Tailscale {
                return true;
            }
            let want = if p.want_on { BackendState::Running } else { BackendState::Stopped };
            if backend == Some(want) {
                proven = true;
                return false;
            }
            p.samples_left = p.samples_left.saturating_sub(1);
            if p.samples_left == 0 && !operator_proven {
                expired_unproven = true;
            }
            p.samples_left > 0
        });
        if proven {
            self.operator = OperatorState::Granted;
        } else if expired_unproven {
            self.operator = OperatorState::NeedsOperator { hint: "sudo tailscale set --operator=$USER".to_string() };
        }
    }

    /// The precise denial hook: an executor that can observe a
    /// toggle's combined output feeds it here, and `Access denied`
    /// locks the row with the CLI's own remedy line instead of
    /// waiting out the deadline inference. No caller exists yet —
    /// see the module doc's "wiring friction". Returns whether the
    /// face changed.
    pub fn command_finished(&mut self, run: ObservedRun, output: &str) -> bool {
        match run {
            ObservedRun::TailscaleToggle => match classify_toggle_output(output) {
                ToggleVerdict::Denied { hint } => {
                    self.pending.retain(|p| p.key != RowKey::Tailscale);
                    self.operator = OperatorState::NeedsOperator { hint };
                    self.dirty = true;
                    true
                }
                ToggleVerdict::Indeterminate => false,
            },
        }
    }

    /// Everything the renderer draws, derived fresh from the folded
    /// state — the panel's single description of its own face.
    pub fn view(&self) -> PanelView {
        let header = if self.header == LinkHeader::Unknown { self.derive_header() } else { self.header.clone() };
        let connections = self
            .connections
            .iter()
            .map(|c| ConnRow {
                uuid: c.uuid.clone(),
                name: c.name.clone(),
                kind: c.kind,
                lamp: if self.is_pending(&RowKey::Conn(c.uuid.clone())) {
                    Lamp::Pending
                } else if c.active {
                    Lamp::On
                } else {
                    Lamp::Off
                },
                external: c.active
                    && self
                        .devices
                        .iter()
                        .any(|d| d.connection == c.name && d.state == DeviceState::ConnectedExternally),
            })
            .collect();
        let wifi = if self.wifi_hw {
            WifiSection::Networks(
                self.networks
                    .iter()
                    .map(|n| NetRow {
                        ssid: n.ssid.clone(),
                        signal: n.signal,
                        secured: n.secured,
                        known: data::known_profile(&self.connections, &n.ssid).is_some(),
                        in_use: n.in_use,
                        pending: self.is_pending(&RowKey::Net(n.ssid.clone())),
                    })
                    .collect(),
            )
        } else {
            WifiSection::NoHardware
        };
        let tailscale = if self.ts_absent {
            None
        } else {
            Some(TailscaleRow {
                status: self.ts.clone(),
                operator: self.operator.clone(),
                pending: self.is_pending(&RowKey::Tailscale),
            })
        };
        PanelView {
            header,
            connections,
            wifi,
            tailscale,
            rescan_cooling: self.rescan_cooldown > 0,
            hover: self.hover.clone(),
            pressed: self.pressed.clone(),
        }
    }

    /// A panel opened before the tile has spoken still shows an
    /// honest header: the in-use network, else the live ethernet
    /// device, else nothing claimed.
    fn derive_header(&self) -> LinkHeader {
        if let Some(net) = self.networks.iter().find(|n| n.in_use) {
            return LinkHeader::Wifi { ssid: net.ssid.clone(), signal: net.signal };
        }
        if let Some(dev) = self
            .devices
            .iter()
            .find(|d| d.kind == DeviceKind::Ethernet && matches!(d.state, DeviceState::Connected | DeviceState::ConnectedExternally))
        {
            return LinkHeader::Wired { interface: dev.name.clone(), speed_mbps: None };
        }
        LinkHeader::Unknown
    }

    fn is_pending(&self, key: &RowKey) -> bool {
        self.pending.iter().any(|p| &p.key == key)
    }

    /// Renders the panel into the granted frame size — content that
    /// no longer fits a stale grant is clipped by the glass, never
    /// rescaled.
    ///
    /// `&mut self` because the grant is remembered here: the hit test
    /// in [`on_event`] must run against the geometry that was actually
    /// *drawn*, or a clipped row would still answer clicks and a
    /// wide-granted panel's rows would end before its glass does.
    /// Rendering is still a pure function of [`view`] — the only thing
    /// that settles is where the pixels went.
    ///
    /// [`on_event`]: LinkPanel::on_event
    /// [`view`]: LinkPanel::view
    pub fn render(
        &mut self,
        theme: &Theme,
        tile: u32,
        fonts: &mut cosmic_text::FontSystem,
        swash: &mut cosmic_text::SwashCache,
        width: u32,
        height: u32,
    ) -> DecorationBuffer {
        self.granted = Some((width, height));
        render_link_panel_into(theme, fonts, swash, tile, &self.view(), width, height)
    }

    /// The layout the pointer is hit-tested against: the granted one
    /// once a frame has been drawn, the natural one before that (a
    /// pointer event cannot arrive before the first render, but a test
    /// may ask, and the natural layout is the honest answer).
    fn hit_layout(&self, tile: u32) -> render::PanelLayout {
        let layout = panel_layout(&self.view(), tile);
        match self.granted {
            Some((w, h)) => layout.fitted(w, h),
            None => layout,
        }
    }

    /// One pointer event, content-local. Hover follows motion; a
    /// click is a press and a release on the *same* row —
    /// press-drag-release across rows is a change of mind, not a
    /// command. At most one [`Effect`] can result, and its `then:`
    /// makes the confirming sampler hurry.
    pub fn on_event(&mut self, event: PanelEvent, tile: u32) -> PanelReaction {
        let layout = self.hit_layout(tile);
        match event {
            PanelEvent::Motion { local } => {
                let over = layout.row_at(local.x, local.y);
                if over != self.hover {
                    self.hover = over;
                    PanelReaction::Repaint
                } else {
                    PanelReaction::None
                }
            }
            PanelEvent::Enter => PanelReaction::None,
            PanelEvent::Leave => {
                if self.hover.is_none() && self.pressed.is_none() {
                    return PanelReaction::None;
                }
                self.hover = None;
                self.pressed = None;
                PanelReaction::Repaint
            }
            PanelEvent::Scroll { .. } => PanelReaction::None,
            PanelEvent::LeftPress { local } => {
                self.pressed = layout.row_at(local.x, local.y);
                if self.pressed.is_some() {
                    PanelReaction::Repaint
                } else {
                    PanelReaction::None
                }
            }
            PanelEvent::LeftRelease { local } => {
                let target = layout.row_at(local.x, local.y);
                let had_highlight = self.pressed.is_some();
                let fired = self.pressed.take().filter(|p| Some(p) == target.as_ref());
                match fired.and_then(|key| self.activate(&key)) {
                    Some(effect) => {
                        // The row's lamp went BUSY (or the cooldown
                        // started): the next panel_tick sees the
                        // dirty flag and repaints around the Run.
                        self.dirty = true;
                        PanelReaction::Run(effect)
                    }
                    // A press highlight still needs clearing when the
                    // row declined to fire (pending, cooling, or a
                    // fact rather than a control) — but a release that
                    // never had one, on the frame's furniture or past
                    // a short grant's glass, changes no pixels and
                    // must not cost a repaint.
                    None if had_highlight => PanelReaction::Repaint,
                    None => PanelReaction::None,
                }
            }
        }
    }

    /// The action table, applied to one clicked row. Every argv here
    /// is in the module doc's table; nothing else can be requested.
    fn activate(&mut self, key: &RowKey) -> Option<Effect> {
        if self.is_pending(key) {
            return None;
        }
        match key {
            RowKey::Conn(uuid) => {
                let conn = self.connections.iter().find(|c| &c.uuid == uuid)?;
                let verb = if conn.active { "down" } else { "up" };
                let want_on = !conn.active;
                self.push_pending(key.clone(), want_on);
                Some(Effect::Run {
                    program: "nmcli",
                    args: vec!["connection".to_string(), verb.to_string(), uuid.clone()],
                    then: Some(self.connections_src),
                })
            }
            RowKey::Net(ssid) => {
                let net = self.networks.iter().find(|n| &n.ssid == ssid)?;
                if net.in_use {
                    // The associated network's row is a fact, not a
                    // control; disconnecting lives on its connection
                    // row, where "down" is unambiguous.
                    return None;
                }
                if let Some(profile) = data::known_profile(&self.connections, ssid) {
                    let uuid = profile.uuid.clone();
                    self.push_pending(key.clone(), true);
                    return Some(Effect::Run {
                        program: "nmcli",
                        args: vec!["connection".to_string(), "up".to_string(), uuid],
                        then: Some(self.connections_src),
                    });
                }
                if net.secured {
                    // A new secured network needs a passphrase, and a
                    // panel has no keyboard by design (see
                    // `chonk_dock_widget::panel`): hand the SSID to
                    // the join dialog, a real window. Not marked
                    // pending — the dialog owns the attempt, and the
                    // wifi list shows the result. The run thread
                    // waits out the dialog; that is the executor's
                    // deal for every effect, just longer here.
                    return Some(Effect::Run { program: "chonk-netjoin", args: vec![ssid.clone()], then: Some(self.wifi_src) });
                }
                self.push_pending(key.clone(), true);
                Some(Effect::Run {
                    program: "nmcli",
                    args: vec!["dev".to_string(), "wifi".to_string(), "connect".to_string(), ssid.clone()],
                    then: Some(self.wifi_src),
                })
            }
            RowKey::Tailscale => {
                if matches!(self.operator, OperatorState::NeedsOperator { .. }) {
                    return None;
                }
                let verb = match self.ts.as_ref().map(|s| s.backend) {
                    Some(BackendState::Running) => "down",
                    Some(BackendState::Stopped) => "up",
                    // NeedsLogin wants a browser, Starting wants
                    // patience; neither is a toggle.
                    _ => return None,
                };
                self.push_pending(RowKey::Tailscale, verb == "up");
                Some(Effect::Run { program: "tailscale", args: vec![verb.to_string()], then: Some(self.tailscale_src) })
            }
            RowKey::Rescan => {
                if self.rescan_cooldown > 0 {
                    return None;
                }
                self.rescan_cooldown = RESCAN_COOLDOWN_SAMPLES;
                Some(Effect::Run {
                    program: "nmcli",
                    args: to_args(&["dev", "wifi", "list", "--rescan", "yes"]),
                    then: Some(self.wifi_src),
                })
            }
        }
    }

    fn push_pending(&mut self, key: RowKey, want_on: bool) {
        self.pending.push(PendingToggle { key, want_on, samples_left: PENDING_DEADLINE_SAMPLES });
    }
}

impl Default for LinkPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn to_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chonk_dock_widget::SampleBench;
    use data::ConnKind;
    use wm_theme_api::Point;

    const DEVICES: &str = "\
eno1:ethernet:connected:Wired connection 1
wlan0:wifi:disconnected:
tailscale0:tun:connected (externally):tailscale0
";
    const CONNECTIONS: &str = "\
Wired connection 1:802-3-ethernet:yes:uuid-eth
HomeBase:802-11-wireless:no:uuid-home
wg-home:wireguard:no:uuid-wg
";
    const WIFI: &str = "\
 :HomeBase:74:WPA2
 :Cafe:61:WPA2
 :OpenMesh:52:--
";
    const TS_RUNNING: &str = r#"{"BackendState":"Running","Self":{"Online":true},"Health":[],"Peer":{}}"#;
    const TS_STOPPED: &str = r#"{"BackendState":"Stopped","Self":{"Online":false},"Health":[],"Peer":{}}"#;

    struct Rig {
        bench: SampleBench,
        panel: LinkPanel,
        devices: SourceId,
        connections: SourceId,
        wifi: SourceId,
        ts: SourceId,
    }

    fn rig() -> Rig {
        let mut bench = SampleBench::new();
        let devices = bench.text(DEVICES);
        let connections = bench.text(CONNECTIONS);
        let wifi = bench.text(WIFI);
        let ts = bench.text(TS_RUNNING);
        let mut panel = LinkPanel::new();
        panel.bind(&[devices, connections, wifi, ts]);
        assert!(panel.update(&bench.samples()));
        assert!(panel.take_dirty(), "a changed fold marks the panel dirty");
        Rig { bench, panel, devices, connections, wifi, ts }
    }

    /// Panel-local center of a row, for synthetic clicks.
    fn row_center(panel: &LinkPanel, key: &RowKey) -> Point {
        let layout = panel_layout(&panel.view(), 56);
        for y in 0..layout.height as i32 {
            if layout.row_at(layout.width as i32 / 2, y).as_ref() == Some(key) {
                return Point::new(layout.width as i32 / 2, y + 2);
            }
        }
        panic!("row {key:?} not present in the layout");
    }

    fn click(panel: &mut LinkPanel, key: &RowKey) -> PanelReaction {
        let at = row_center(panel, key);
        panel.on_event(PanelEvent::LeftPress { local: at }, 56);
        panel.on_event(PanelEvent::LeftRelease { local: at }, 56)
    }

    /// Unwraps the one shape every fired action takes.
    fn run_of(reaction: PanelReaction) -> (&'static str, Vec<String>, Option<SourceId>) {
        match reaction {
            PanelReaction::Run(Effect::Run { program, args, then }) => (program, args, then),
            _ => panic!("expected PanelReaction::Run(Effect::Run)"),
        }
    }

    #[test]
    fn the_fold_produces_the_full_view() {
        let r = rig();
        let view = r.panel.view();
        assert_eq!(view.header, LinkHeader::Wired { interface: "eno1".into(), speed_mbps: None }, "derived header from the live ethernet device");
        assert_eq!(view.connections.len(), 3);
        assert_eq!(view.connections[0].lamp, Lamp::On);
        assert_eq!(view.connections[2].kind, ConnKind::WireGuard);
        let WifiSection::Networks(nets) = &view.wifi else { panic!("wifi hardware is present") };
        assert_eq!(nets.len(), 3);
        assert!(nets[0].known, "HomeBase has a saved profile");
        assert!(!nets[1].known);
        let ts = view.tailscale.expect("tailscale row present");
        assert_eq!(ts.status.unwrap().backend, BackendState::Running);
        assert!(!view.rescan_cooling);
    }

    #[test]
    fn the_spec_asks_for_the_laid_out_size() {
        let r = rig();
        let spec = r.panel.spec(56);
        let layout = panel_layout(&r.panel.view(), 56);
        assert_eq!((spec.width, spec.height), (layout.width, layout.height));
    }

    #[test]
    fn an_active_connection_click_is_connection_down_by_uuid() {
        let mut r = rig();
        let (program, args, then) = run_of(click(&mut r.panel, &RowKey::Conn("uuid-eth".into())));
        assert_eq!(program, "nmcli");
        assert_eq!(args, ["connection", "down", "uuid-eth"]);
        assert_eq!(then, Some(r.connections));
        // Optimistic: the lamp claims pending immediately, and the
        // dirty flag carries the repaint to the next panel_tick...
        assert!(r.panel.take_dirty());
        assert_eq!(r.panel.view().connections[0].lamp, Lamp::Pending);
        // ...and a second click while pending is inert.
        assert!(matches!(click(&mut r.panel, &RowKey::Conn("uuid-eth".into())), PanelReaction::Repaint), "pending row must not fire twice");
        // The confirming sample is the truth.
        r.bench.set_text(r.connections, "Wired connection 1:802-3-ethernet:no:uuid-eth\n");
        assert!(r.panel.update(&r.bench.samples()));
        assert_eq!(r.panel.view().connections[0].lamp, Lamp::Off);
    }

    #[test]
    fn an_inactive_wireguard_click_is_connection_up_by_uuid() {
        let mut r = rig();
        let (_, args, _) = run_of(click(&mut r.panel, &RowKey::Conn("uuid-wg".into())));
        assert_eq!(args, ["connection", "up", "uuid-wg"]);
    }

    #[test]
    fn a_pending_toggle_expires_back_to_reality() {
        let mut r = rig();
        click(&mut r.panel, &RowKey::Conn("uuid-wg".into()));
        assert_eq!(r.panel.view().connections[2].lamp, Lamp::Pending);
        for _ in 0..PENDING_DEADLINE_SAMPLES {
            r.bench.set_text(r.connections, CONNECTIONS);
            r.panel.update(&r.bench.samples());
        }
        assert_eq!(r.panel.view().connections[2].lamp, Lamp::Off, "an unconfirmed toggle must stop claiming BUSY");
    }

    #[test]
    fn a_known_network_is_one_click_via_its_profile() {
        let mut r = rig();
        let (_, args, then) = run_of(click(&mut r.panel, &RowKey::Net("HomeBase".into())));
        assert_eq!(args, ["connection", "up", "uuid-home"]);
        assert_eq!(then, Some(r.connections));
    }

    #[test]
    fn an_unknown_secured_network_spawns_the_join_dialog() {
        let mut r = rig();
        let (program, args, then) = run_of(click(&mut r.panel, &RowKey::Net("Cafe".into())));
        assert_eq!(program, "chonk-netjoin");
        assert_eq!(args, ["Cafe"]);
        assert_eq!(then, Some(r.wifi), "when the dialog exits, the list catches up immediately");
        assert!(!r.panel.is_pending(&RowKey::Net("Cafe".into())), "the dialog owns the attempt");
    }

    #[test]
    fn an_unknown_open_network_connects_by_ssid() {
        let mut r = rig();
        let (_, args, _) = run_of(click(&mut r.panel, &RowKey::Net("OpenMesh".into())));
        assert_eq!(args, ["dev", "wifi", "connect", "OpenMesh"]);
    }

    #[test]
    fn the_in_use_network_row_is_a_fact_not_a_control() {
        let mut r = rig();
        r.bench.set_text(r.wifi, "*:HomeBase:74:WPA2\n :Cafe:61:WPA2\n");
        r.panel.update(&r.bench.samples());
        assert!(matches!(click(&mut r.panel, &RowKey::Net("HomeBase".into())), PanelReaction::Repaint));
    }

    #[test]
    fn the_tailscale_toggle_runs_and_confirmation_proves_the_grant() {
        let mut r = rig();
        let (program, args, then) = run_of(click(&mut r.panel, &RowKey::Tailscale));
        assert_eq!(program, "tailscale");
        assert_eq!(args, ["down"], "a running backend toggles down");
        assert_eq!(then, Some(r.ts));
        assert!(r.panel.view().tailscale.unwrap().pending);
        r.bench.set_text(r.ts, TS_STOPPED);
        assert!(r.panel.update(&r.bench.samples()));
        let ts = r.panel.view().tailscale.unwrap();
        assert_eq!(ts.operator, OperatorState::Granted);
        assert!(!ts.pending);
        assert_eq!(ts.status.unwrap().backend, BackendState::Stopped);
        // And the toggle now points the other way.
        let (_, args, _) = run_of(click(&mut r.panel, &RowKey::Tailscale));
        assert_eq!(args, ["up"]);
    }

    #[test]
    fn an_unconfirmed_tailscale_toggle_locks_the_row_with_the_remedy() {
        let mut r = rig();
        click(&mut r.panel, &RowKey::Tailscale);
        for _ in 0..PENDING_DEADLINE_SAMPLES {
            r.bench.set_text(r.ts, TS_RUNNING);
            r.panel.update(&r.bench.samples());
        }
        let ts = r.panel.view().tailscale.unwrap();
        match &ts.operator {
            OperatorState::NeedsOperator { hint } => assert!(hint.contains("--operator="), "the remedy must be shown: {hint}"),
            other => panic!("an unconfirmed toggle against an unproven grant must lock, got {other:?}"),
        }
        assert!(!ts.pending);
        // The locked row is inert.
        assert!(matches!(click(&mut r.panel, &RowKey::Tailscale), PanelReaction::Repaint));
    }

    #[test]
    fn a_proven_grant_survives_one_flaky_toggle() {
        let mut r = rig();
        // Prove the grant.
        click(&mut r.panel, &RowKey::Tailscale);
        r.bench.set_text(r.ts, TS_STOPPED);
        r.panel.update(&r.bench.samples());
        assert_eq!(r.panel.view().tailscale.unwrap().operator, OperatorState::Granted);
        // Now a toggle that never confirms.
        click(&mut r.panel, &RowKey::Tailscale);
        for _ in 0..PENDING_DEADLINE_SAMPLES {
            r.bench.set_text(r.ts, TS_STOPPED);
            r.panel.update(&r.bench.samples());
        }
        assert_eq!(
            r.panel.view().tailscale.unwrap().operator,
            OperatorState::Granted,
            "one flaky toggle must not unprove the grant"
        );
    }

    #[test]
    fn the_observed_output_hook_classifies_a_denial_exactly() {
        let mut r = rig();
        click(&mut r.panel, &RowKey::Tailscale);
        let denied = "Access denied: watch IPN bus access denied, must be root or Operator\n\
                      Use 'sudo tailscale up' or 'sudo tailscale set --operator=$USER' to not require root.\n";
        assert!(r.panel.command_finished(ObservedRun::TailscaleToggle, denied));
        let ts = r.panel.view().tailscale.unwrap();
        assert!(matches!(&ts.operator, OperatorState::NeedsOperator { hint } if hint.contains("--operator=")));
        assert!(!ts.pending, "a denied toggle is not in flight");
        assert!(!r.panel.command_finished(ObservedRun::TailscaleToggle, ""), "quiet output decides nothing");
    }

    #[test]
    fn an_absent_tailscale_binary_means_no_row_at_all() {
        let mut bench = SampleBench::new();
        let devices = bench.text(DEVICES);
        let connections = bench.text(CONNECTIONS);
        let wifi = bench.text(WIFI);
        let ts = bench.unusable();
        let mut panel = LinkPanel::new();
        panel.bind(&[devices, connections, wifi, ts]);
        panel.update(&bench.samples());
        assert_eq!(panel.view().tailscale, None);
    }

    #[test]
    fn rescan_fires_once_then_cools_down_by_samples() {
        let mut r = rig();
        let (_, args, then) = run_of(click(&mut r.panel, &RowKey::Rescan));
        assert_eq!(args, ["dev", "wifi", "list", "--rescan", "yes"]);
        assert_eq!(then, Some(r.wifi));
        assert!(r.panel.view().rescan_cooling);
        assert!(matches!(click(&mut r.panel, &RowKey::Rescan), PanelReaction::Repaint), "the cooldown gates the radio");
        for _ in 0..RESCAN_COOLDOWN_SAMPLES {
            r.bench.set_text(r.wifi, WIFI);
            r.panel.update(&r.bench.samples());
        }
        assert!(!r.panel.view().rescan_cooling);
        assert!(matches!(click(&mut r.panel, &RowKey::Rescan), PanelReaction::Run(_)));
    }

    #[test]
    fn hover_follows_motion_and_a_cross_row_release_fires_nothing() {
        let mut r = rig();
        let at = row_center(&r.panel, &RowKey::Tailscale);
        assert!(matches!(r.panel.on_event(PanelEvent::Motion { local: at }, 56), PanelReaction::Repaint));
        assert_eq!(r.panel.view().hover, Some(RowKey::Tailscale));
        assert!(matches!(r.panel.on_event(PanelEvent::Motion { local: at }, 56), PanelReaction::None), "same row, no repaint");

        r.panel.on_event(PanelEvent::LeftPress { local: at }, 56);
        let elsewhere = row_center(&r.panel, &RowKey::Rescan);
        let reaction = r.panel.on_event(PanelEvent::LeftRelease { local: elsewhere }, 56);
        assert!(matches!(reaction, PanelReaction::Repaint), "press on one row, release on another is a change of mind");
        assert!(!r.panel.view().rescan_cooling, "rescan must not have fired");
        assert!(!r.panel.is_pending(&RowKey::Tailscale), "and neither did the pressed row");

        assert!(matches!(r.panel.on_event(PanelEvent::Leave, 56), PanelReaction::Repaint));
        assert_eq!(r.panel.view().hover, None);
        let _ = r.devices;
    }

    #[test]
    fn no_wifi_hardware_is_a_designed_state() {
        let mut bench = SampleBench::new();
        let devices = bench.text("eno1:ethernet:connected:Wired connection 1\n");
        let connections = bench.text("Wired connection 1:802-3-ethernet:yes:uuid-eth\n");
        let wifi = bench.missing();
        let ts = bench.text(TS_RUNNING);
        let mut panel = LinkPanel::new();
        panel.bind(&[devices, connections, wifi, ts]);
        panel.update(&bench.samples());
        let view = panel.view();
        assert_eq!(view.wifi, WifiSection::NoHardware);
        let layout = panel_layout(&view, 56);
        for y in 0..layout.height as i32 {
            assert_ne!(layout.row_at(layout.width as i32 / 2, y), Some(RowKey::Rescan), "no radio, no rescan row");
        }
    }

    #[test]
    fn the_panel_answers_clicks_where_it_actually_drew_them() {
        let mut r = rig();
        let natural = panel_layout(&r.panel.view(), 56);
        let (mut fonts, mut swash) = (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new());
        let theme = wm_theme::default_theme::nextstep_classic();
        // The shell grants half the height it was asked for.
        let short = (natural.width, natural.height / 2);
        let buffer = r.panel.render(&theme, 56, &mut fonts, &mut swash, short.0, short.1);
        assert_eq!((buffer.width, buffer.height), short, "the grant sizes the frame");

        // The rescan row was laid out past the grant, so the pixels
        // under its natural y belong to nothing — and clicking there
        // must not fire the radio.
        let rescan_y = row_center(&r.panel, &RowKey::Rescan).y;
        assert!(rescan_y >= short.1 as i32, "the fixture must actually clip the rescan row");
        let at = Point::new(natural.width as i32 / 2, rescan_y);
        r.panel.on_event(PanelEvent::LeftPress { local: at }, 56);
        let reaction = r.panel.on_event(PanelEvent::LeftRelease { local: at }, 56);
        assert!(matches!(reaction, PanelReaction::None), "a click outside the drawn glass is not a command");
        assert!(!r.panel.view().rescan_cooling, "and the radio was not asked to scan");
    }

    #[test]
    fn the_tile_header_wins_over_the_derived_one() {
        let mut r = rig();
        assert!(r.panel.set_link_header(LinkHeader::Wired { interface: "eno1".into(), speed_mbps: Some(1000) }));
        assert!(r.panel.take_dirty());
        assert_eq!(r.panel.view().header, LinkHeader::Wired { interface: "eno1".into(), speed_mbps: Some(1000) });
        assert!(
            !r.panel.set_link_header(LinkHeader::Wired { interface: "eno1".into(), speed_mbps: Some(1000) }),
            "same header, no repaint"
        );
        r.bench.all_stale();
        assert!(!r.panel.update(&r.bench.samples()));
        let _ = (r.devices, r.wifi);
    }
}
