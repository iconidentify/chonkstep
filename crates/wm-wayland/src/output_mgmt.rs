//! `zwlr_output_manager_v1`: the protocol `wlr-randr` and `kanshi`
//! configure outputs through — list every head with its modes, then
//! atomically apply position, mode, and scale changes.
//!
//! Smithay 0.7 ships no delegate types for this protocol (its
//! `wayland::output` module is the `wl_output`/`xdg_output` pair), so
//! the `Dispatch` impls are written directly against
//! `wayland-protocols-wlr` on [`Compositor`] itself — the exact shape,
//! and for the exact reasons, `protocols.rs` documents for
//! foreign-toplevel and screencopy: the protocol crate is already a
//! transitive dependency, and one state type needs no delegation layer.
//!
//! # What a configuration can and cannot change, honestly
//!
//! - **Scale** applies everywhere, fractionally: the head's
//!   `OutputEntry::scale` moves, the `wl_output` re-advertises its
//!   ceiling, fractional-scale clients hear the exact value on their
//!   next commit, and a change to the PRIMARY head's scale additionally
//!   restyles the whole chrome through the same
//!   `Shell::apply_session_state` path a config reload takes.
//! - **Position** applies on both backends, normalized so the layout's
//!   top-left corner stays at the global origin — `wm-core` and the
//!   shell assume the screen starts at (0, 0) (see
//!   `state.rs::union_size`), so a layout placed anywhere is translated
//!   whole rather than refused. The ledger's `MonitorInfo` list moves
//!   with it and the resize drain re-hangs the shell's furniture, which
//!   is the same path a host-window resize already exercises.
//! - **Mode** applies for real on the DRM session backend
//!   (`session::apply_mode` re-programs the crtc); the nested backend's
//!   one output is the host window and honestly refuses any mode but
//!   the current one — a window cannot modeset its host desktop.
//! - **Disable, transform, adaptive sync** are refused with `failed()`
//!   and a log line naming the gap. Disabling means tearing an output
//!   out of three index-aligned lists (`Compositor::outputs`, the
//!   ledger's monitors, the session's crtcs) that the lock module and
//!   the shell hold indices into; transforms would be the first
//!   non-`Normal`/`Flipped180` transform in the compositor. Both are
//!   real work, and a truthful `failed` beats a lying `succeeded`.
//!
//! When no output manager is bound, publication is an immediate fast
//! path. Once one binds, retained snapshots keep unchanged passes both
//! allocation- and event-free while every new manager still receives a
//! complete initial listing.
//!
//! # Timing
//!
//! Requests land mid-dispatch, where a modeset and a shell restyle have
//! no business running; a valid `apply` is therefore *validated*
//! immediately (that is pure arithmetic) and parked, and [`refresh`] —
//! called once per dispatch pass, before the damage test, beside
//! `protocols::refresh` — performs it, answers `succeeded`, and pushes
//! the new state (with a fresh serial) to every bound manager. The
//! deferral is one pass, which is the latency every other deferred verb
//! in this compositor already has.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_configuration_head_v1::{
    self, ZwlrOutputConfigurationHeadV1,
};
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_configuration_v1::{
    self, ZwlrOutputConfigurationV1,
};
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_head_v1::{
    self, AdaptiveSyncState, ZwlrOutputHeadV1,
};
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_manager_v1::{
    self, ZwlrOutputManagerV1,
};
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::zwlr_output_mode_v1::{
    self, ZwlrOutputModeV1,
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

use wm_theme_api::{Point, Size};

use crate::state::Compositor;

/// Highest `zwlr_output_manager_v1` we implement. Version 2 added
/// make/model/serial on heads, 3 the release destructors, 4 adaptive
/// sync — which every head reports as disabled, truthfully.
const OUTPUT_MANAGER_VERSION: u32 = 4;

/// Everything the global keeps between dispatch passes.
pub(crate) struct OutputManagement {
    managers: Vec<Manager>,
    /// The serial the most recent `done` carried. A configuration made
    /// against any other serial is answered `cancelled`, per the
    /// protocol: the client was reasoning about a layout that no longer
    /// exists.
    serial: u32,
    /// What was last published, one entry per output, for the diff in
    /// [`refresh`].
    published: Vec<HeadSnapshot>,
    /// Whether compositor-owned output state may differ from
    /// `published`. Every mutation site marks this so a persistent
    /// manager does not make clean dispatches inspect each `Output`.
    dirty: bool,
    /// A validated `apply` waiting for [`refresh`] to perform it.
    pending_apply: Option<PendingApply>,
    /// A primary-scale change from an applied configuration, waiting
    /// for [`refresh`] to restyle the chrome through the same
    /// `SessionState` path a config reload takes.
    pending_primary_scale: Option<f32>,
}

/// One bound manager and the head/mode resources minted for it.
struct Manager {
    resource: ZwlrOutputManagerV1,
    /// Whether this manager has been sent the initial full listing.
    announced: bool,
    heads: Vec<HeadInstance>,
}

/// One `zwlr_output_head_v1` belonging to one manager, for one entry of
/// `Compositor::outputs` (same index — outputs are never removed, so
/// the index is stable for the life of the session).
struct HeadInstance {
    index: usize,
    resource: ZwlrOutputHeadV1,
    /// Mode resources, index-aligned with `OutputEntry::modes`.
    modes: Vec<ZwlrOutputModeV1>,
}

/// The publishable state of one output, for change detection.
#[derive(Clone, PartialEq)]
struct HeadSnapshot {
    position: Point,
    scale: f64,
    current_mode: usize,
}

/// What one configuration asked for on one head.
#[derive(Clone, Default)]
struct HeadConfig {
    enabled: bool,
    /// Index into the head's mode list.
    mode: Option<usize>,
    custom_mode: Option<(i32, i32, i32)>,
    position: Option<(i32, i32)>,
    transform: Option<i32>,
    scale: Option<f64>,
    adaptive_sync: Option<bool>,
}

/// Shared, interior-mutable state of one `zwlr_output_configuration_v1`
/// and its per-head children. A `Mutex` because `Dispatch` hands out
/// only `&UserData`; never contended — protocol dispatch is single
/// threaded.
struct ConfigState {
    serial: u32,
    heads: HashMap<usize, HeadConfig>,
    /// Set once `apply`/`test`/a failure has answered; every later
    /// request but `destroy` is the protocol's `already_used` error.
    used: bool,
}

type SharedConfig = Arc<Mutex<ConfigState>>;

/// Per-config-head resource data: the owning configuration plus which
/// output this child configures.
struct ConfigHeadData {
    config: SharedConfig,
    index: usize,
}

/// A validated apply, waiting to be performed.
struct PendingApply {
    resource: ZwlrOutputConfigurationV1,
    heads: Vec<(usize, HeadConfig)>,
}

/// Registers the global. Called once from `run`, before the listening
/// socket exists, like every other global.
pub(crate) fn init(display_handle: &DisplayHandle) -> OutputManagement {
    let _global = display_handle
        .create_global::<Compositor, ZwlrOutputManagerV1, ()>(OUTPUT_MANAGER_VERSION, ());
    tracing::info!(version = OUTPUT_MANAGER_VERSION, "wlr-output-management advertised");
    OutputManagement {
        managers: Vec::new(),
        serial: 1,
        published: Vec::new(),
        dirty: true,
        pending_apply: None,
        pending_primary_scale: None,
    }
}

impl OutputManagement {
    /// Invalidates the retained protocol snapshot after output state
    /// changed outside this module. The next [`refresh`] performs the
    /// exact diff; repeated mutations coalesce into one batch.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// The once-per-pass reconciliation: perform a parked apply, restyle
/// the chrome for a primary-scale change, then push any layout change
/// (from the apply, a host-window resize, anything) to every manager.
pub(crate) fn refresh(comp: &mut Compositor) {
    perform_pending_apply(comp);
    if let Some(scale) = comp.output_mgmt.pending_primary_scale.take() {
        // The same call, and the same resolved state, as the reload
        // path in `run` — with only the scale overridden, so a
        // wlr-randr scale change and a config-file scale change are one
        // code path from here down (theme restyle, cursor rebuild,
        // XSETTINGS, output re-advertisement, per-surface rescale).
        let mut state = chonk_shell::startup::SessionState::resolve(&wm_config::load());
        state.scale = scale;
        tracing::info!(scale, "primary output scale set via wlr-output-management; restyling the session");
        comp.shell.apply_session_state(&mut comp.wm, state);
    }
    publish(comp);
}

/// Sends the current layout to every manager that has not heard it:
/// new managers get the full listing, existing ones get deltas, and
/// one `done(serial)` closes each batch. Nothing at all on the passes
/// where nothing changed and nobody new bound, which is all of them.
fn publish(comp: &mut Compositor) {
    if comp.output_mgmt.managers.is_empty() {
        return;
    }
    let any_new = comp.output_mgmt.managers.iter().any(|manager| !manager.announced);
    if !comp.output_mgmt.dirty && !any_new {
        return;
    }
    // Compare directly against the retained baseline. Building a fresh
    // Vec here made a persistent `kanshi`/`wlr-randr` connection pay
    // one allocation and free for every unrelated client commit. The
    // baseline is rewritten in place only on the rare changed path.
    let changed = comp.output_mgmt.dirty
        && (comp.outputs.len() != comp.output_mgmt.published.len()
            || comp
                .outputs
                .iter()
                .zip(&comp.output_mgmt.published)
                .any(|(entry, previous)| head_snapshot(entry) != *previous));
    // The potentially-different state has now been compared, even if
    // the mutation proved idempotent and there is no batch to send.
    comp.output_mgmt.dirty = false;
    if !changed && !any_new {
        return;
    }
    if changed {
        comp.output_mgmt.serial = comp.output_mgmt.serial.wrapping_add(1);
    }
    let serial = comp.output_mgmt.serial;
    let display_handle = comp.display_handle.clone();
    let Compositor { outputs, output_mgmt, .. } = comp;
    for manager in output_mgmt.managers.iter_mut() {
        if !manager.announced {
            for (index, entry) in outputs.iter().enumerate() {
                announce_head(&display_handle, manager, index, entry);
            }
            manager.announced = true;
            manager.resource.done(serial);
            continue;
        }
        if changed {
            for head in &manager.heads {
                let Some(entry) = outputs.get(head.index) else { continue };
                let Some(previous) = output_mgmt.published.get(head.index) else { continue };
                update_head(head, entry, previous);
            }
            manager.resource.done(serial);
        }
    }
    if changed {
        output_mgmt.published.clear();
        output_mgmt.published.extend(outputs.iter().map(head_snapshot));
    }
}

fn head_snapshot(entry: &crate::state::OutputEntry) -> HeadSnapshot {
    HeadSnapshot {
        position: entry.position,
        scale: entry.scale,
        current_mode: current_mode_index(entry),
    }
}

/// The index of the output's current mode in its own mode list. The
/// list was built current-first, so 0 is right until a mode change —
/// after which the real answer is found by size+refresh.
fn current_mode_index(entry: &crate::state::OutputEntry) -> usize {
    let Some(current) = entry.output.current_mode() else {
        return 0;
    };
    entry
        .modes
        .iter()
        .position(|mode| mode.size == current.size && mode.refresh == current.refresh)
        .unwrap_or(0)
}

/// Creates one head (and its modes) on one manager and sends its
/// complete state. The only place head resources are minted.
fn announce_head(
    display_handle: &DisplayHandle,
    manager: &mut Manager,
    index: usize,
    entry: &crate::state::OutputEntry,
) {
    let Some(client) = manager.resource.client() else {
        return;
    };
    let version = manager.resource.version();
    let head = match client.create_resource::<ZwlrOutputHeadV1, usize, Compositor>(
        display_handle,
        version,
        index,
    ) {
        Ok(head) => head,
        Err(error) => {
            tracing::warn!(?error, "could not create an output-management head");
            return;
        }
    };
    manager.resource.head(&head);
    head.name(entry.output.name());
    head.description(format!("{} ({})", entry.output.name(), entry.output.physical_properties().model));
    let physical = entry.output.physical_properties().size;
    if physical.w > 0 && physical.h > 0 {
        head.physical_size(physical.w, physical.h);
    }
    if version >= 2 {
        head.make(entry.output.physical_properties().make);
        head.model(entry.output.physical_properties().model);
    }

    let mut modes = Vec::with_capacity(entry.modes.len());
    let preferred = entry.output.preferred_mode();
    for (mode_index, mode) in entry.modes.iter().enumerate() {
        let resource = match client.create_resource::<ZwlrOutputModeV1, (usize, usize), Compositor>(
            display_handle,
            version,
            (index, mode_index),
        ) {
            Ok(resource) => resource,
            Err(error) => {
                tracing::warn!(?error, "could not create an output-management mode");
                continue;
            }
        };
        head.mode(&resource);
        resource.size(mode.size.w, mode.size.h);
        resource.refresh(mode.refresh);
        if preferred.is_some_and(|p| p.size == mode.size && p.refresh == mode.refresh) {
            resource.preferred();
        }
        modes.push(resource);
    }

    // Every output this compositor drives is enabled — a disabled head
    // would be one this session cannot yet produce (see the module
    // docs on disable).
    head.enabled(1);
    let current = current_mode_index(entry);
    if let Some(mode) = modes.get(current) {
        head.current_mode(mode);
    }
    head.position(entry.position.x, entry.position.y);
    // Every output is composed un-transformed; the nested backend's
    // Flipped180 is an EGL-surface correction, not a user-visible
    // orientation, and reporting it would make wlr-randr claim the
    // desktop is upside down.
    head.transform(smithay::reexports::wayland_server::protocol::wl_output::Transform::Normal);
    head.scale(entry.scale);
    if version >= 4 {
        head.adaptive_sync(AdaptiveSyncState::Disabled);
    }
    manager.heads.push(HeadInstance { index, resource: head, modes });
}

/// Sends only what changed about an already-announced head.
fn update_head(head: &HeadInstance, entry: &crate::state::OutputEntry, previous: &HeadSnapshot) {
    if entry.position != previous.position {
        head.resource.position(entry.position.x, entry.position.y);
    }
    if (entry.scale - previous.scale).abs() > f64::EPSILON {
        head.resource.scale(entry.scale);
    }
    let current = current_mode_index(entry);
    if current != previous.current_mode {
        if let Some(mode) = head.modes.get(current) {
            head.resource.current_mode(mode);
        }
    }
}

// ---------------------------------------------------------------------
// Validation and application.
// ---------------------------------------------------------------------

/// Checks a whole configuration against what this compositor can
/// honor. Pure — safe to run mid-dispatch for `test` and for the
/// immediate half of `apply`. Heads the configuration never named keep
/// their current state (the protocol prefers every head configured;
/// treating silence as "unchanged" refuses nobody and surprises
/// nobody).
fn validate(comp: &Compositor, config: &ConfigState) -> Result<Vec<(usize, HeadConfig)>, String> {
    let mut heads = Vec::with_capacity(config.heads.len());
    for (&index, head) in &config.heads {
        let Some(entry) = comp.outputs.get(index) else {
            return Err(format!("head {index} does not exist"));
        };
        let name = entry.output.name();
        if !head.enabled {
            return Err(format!(
                "disabling {name}: not supported yet (this compositor cannot yet remove an output from the session's layout)"
            ));
        }
        if let Some(transform) = head.transform {
            // wl_output.transform normal = 0.
            if transform != 0 {
                return Err(format!("rotating {name}: output transforms are not supported yet"));
            }
        }
        if head.adaptive_sync == Some(true) {
            return Err(format!("adaptive sync on {name}: not supported"));
        }
        if let Some(scale) = head.scale {
            if !(0.125..=8.0).contains(&scale) {
                return Err(format!("scale {scale} on {name} is outside the sane range"));
            }
        }
        if let Some(mode) = head.mode {
            if mode >= entry.modes.len() {
                return Err(format!("mode {mode} does not belong to {name}"));
            }
            if !crate::session::mode_is_applicable(&comp.graphics, index, mode) {
                return Err(format!(
                    "mode change on {name}: the nested backend's output is the host window and keeps its size"
                ));
            }
        }
        if let Some((w, h, refresh)) = head.custom_mode {
            // A custom mode is honored only if it matches a mode the
            // hardware actually advertises — a mode the display never
            // offered is a mode it will refuse, and the nested output
            // has no modes to set at all.
            let matched = entry.modes.iter().position(|mode| {
                mode.size.w == w
                    && mode.size.h == h
                    && (refresh == 0 || mode.refresh == refresh)
            });
            match matched {
                Some(mode) if crate::session::mode_is_applicable(&comp.graphics, index, mode) => {}
                _ => {
                    return Err(format!(
                        "custom mode {w}x{h}@{refresh} on {name} matches no mode the display advertises"
                    ))
                }
            }
        }
        heads.push((index, head.clone()));
    }
    Ok(heads)
}

/// Performs a parked, already-validated apply: scale, mode, position,
/// in that order (position last, because the mode changes the sizes the
/// layout is normalized around), then re-derives everything downstream
/// — the ledger's monitors, the union screen size, the resize drain
/// that re-hangs the shell.
fn perform_pending_apply(comp: &mut Compositor) {
    let Some(pending) = comp.output_mgmt.pending_apply.take() else {
        return;
    };
    let mut scaled_any = false;
    let mut moved_any = false;
    for (index, head) in &pending.heads {
        if let Some(scale) = head.scale {
            let entry = &mut comp.outputs[*index];
            if (entry.scale - scale).abs() > f64::EPSILON {
                entry.scale = scale;
                crate::state::advertise_output_scale_change(entry);
                scaled_any = true;
                if *index == 0 {
                    // The chrome is drawn at the primary's scale; hand
                    // the change to the session-state path on the next
                    // refresh (this function runs inside it).
                    comp.output_mgmt.pending_primary_scale = Some(scale as f32);
                }
                tracing::info!(output = %comp.outputs[*index].output.name(), scale, "output scale set via wlr-output-management");
            }
        }
        let mode = head.mode.or_else(|| {
            head.custom_mode.and_then(|(w, h, refresh)| {
                comp.outputs[*index].modes.iter().position(|mode| {
                    mode.size.w == w && mode.size.h == h && (refresh == 0 || mode.refresh == refresh)
                })
            })
        });
        if let Some(mode_index) = mode {
            if mode_index != current_mode_index(&comp.outputs[*index]) {
                apply_mode(comp, *index, mode_index);
                moved_any = true; // sizes changed; re-layout below.
            }
        }
        if let Some((x, y)) = head.position {
            let entry = &mut comp.outputs[*index];
            if entry.position != Point::new(x, y) {
                entry.position = Point::new(x, y);
                moved_any = true;
            }
        }
    }
    if moved_any {
        normalize_layout(comp);
    }
    if scaled_any || moved_any {
        comp.output_mgmt.mark_dirty();
        comp.sync_monitor_scales();
        relayout_ledger(comp);
        comp.layer_shell.needs_arrange = true;
    }
    pending.resource.succeeded();
}

/// Applies one mode change to one output: the crtc (session backend),
/// the advertised `wl_output` mode, the entry's size, and the damage
/// tracker sized to it.
fn apply_mode(comp: &mut Compositor, index: usize, mode_index: usize) {
    let mode = comp.outputs[index].modes[mode_index];
    if let Err(error) = crate::session::apply_mode(&mut comp.graphics, index, mode_index) {
        tracing::warn!(%error, output = %comp.outputs[index].output.name(), "modeset failed; the output keeps its mode");
        return;
    }
    let entry = &mut comp.outputs[index];
    entry.output.change_current_state(Some(mode), None, None, None);
    entry.size = Size::new(mode.size.w.max(0) as u32, mode.size.h.max(0) as u32);
    entry.damage_tracker = crate::state::physical_damage_tracker(&entry.output, entry.size);
    tracing::info!(
        output = %entry.output.name(),
        mode = %format!("{}x{}@{}", mode.size.w, mode.size.h, mode.refresh),
        "mode set via wlr-output-management"
    );
}

/// Translates the whole layout so its bounding box starts at the global
/// origin — `wm-core`, the shell and the pointer clamp all assume the
/// screen begins at (0, 0), and honoring a layout placed elsewhere by
/// translating it is what lets `wlr-randr --pos` express every
/// *relative* arrangement without teaching three crates about negative
/// space.
fn normalize_layout(comp: &mut Compositor) {
    let positions: Vec<Point> = comp.outputs.iter().map(|entry| entry.position).collect();
    let (dx, dy) = origin_shift(&positions);
    if dx == 0 && dy == 0 {
        return;
    }
    for entry in comp.outputs.iter_mut() {
        entry.position = Point::new(entry.position.x - dx, entry.position.y - dy);
    }
}

/// How far a layout has to be translated for its top-left corner to
/// land on the global origin. Split out of [`normalize_layout`] for
/// the same reason every pure helper in this crate is: the arithmetic
/// is the part that fails silently.
fn origin_shift(positions: &[Point]) -> (i32, i32) {
    (
        positions.iter().map(|position| position.x).min().unwrap_or(0),
        positions.iter().map(|position| position.y).min().unwrap_or(0),
    )
}


// ---------------------------------------------------------------------
// Dispatch.
// ---------------------------------------------------------------------

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for Compositor {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let resource = data_init.init(resource, ());
        // Announced on the next `refresh`, exactly like a foreign-
        // toplevel manager: the bind callback runs mid-dispatch.
        state.output_mgmt.managers.push(Manager { resource, announced: false, heads: Vec::new() });
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        display_handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                let config: SharedConfig = Arc::new(Mutex::new(ConfigState {
                    serial,
                    heads: HashMap::new(),
                    used: false,
                }));
                data_init.init(id, config);
                let _ = display_handle;
            }
            zwlr_output_manager_v1::Request::Stop => {
                resource.finished();
                state.output_mgmt.managers.retain(|manager| &manager.resource != resource);
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ZwlrOutputManagerV1,
        _data: &(),
    ) {
        state.output_mgmt.managers.retain(|manager| &manager.resource != resource);
    }
}

impl Dispatch<ZwlrOutputHeadV1, usize> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrOutputHeadV1,
        request: zwlr_output_head_v1::Request,
        _data: &usize,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // `release` (v3) is the only request; resource teardown is all
        // in `destroyed`.
        let _ = request;
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ZwlrOutputHeadV1,
        _data: &usize,
    ) {
        for manager in state.output_mgmt.managers.iter_mut() {
            manager.heads.retain(|head| &head.resource != resource);
        }
    }
}

impl Dispatch<ZwlrOutputModeV1, (usize, usize)> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrOutputModeV1,
        request: zwlr_output_mode_v1::Request,
        _data: &(usize, usize),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let _ = request; // `release` only.
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, SharedConfig> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &SharedConfig,
        display_handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_v1::Request;
        match request {
            Request::EnableHead { id, head } => {
                let index = *head.data::<usize>().unwrap_or(&usize::MAX);
                let config_head = data_init.init(
                    id,
                    ConfigHeadData { config: data.clone(), index },
                );
                let mut guard = data.lock().unwrap();
                if guard.heads.contains_key(&index) {
                    resource.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                        "this head is already configured",
                    );
                    return;
                }
                guard.heads.insert(index, HeadConfig { enabled: true, ..Default::default() });
                let _ = (config_head, display_handle);
            }
            Request::DisableHead { head } => {
                let index = *head.data::<usize>().unwrap_or(&usize::MAX);
                let mut guard = data.lock().unwrap();
                if guard.heads.contains_key(&index) {
                    resource.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyConfiguredHead,
                        "this head is already configured",
                    );
                    return;
                }
                guard.heads.insert(index, HeadConfig { enabled: false, ..Default::default() });
            }
            Request::Apply | Request::Test => {
                let test = matches!(request, Request::Test);
                let mut guard = data.lock().unwrap();
                if guard.used {
                    resource.post_error(
                        zwlr_output_configuration_v1::Error::AlreadyUsed,
                        "this configuration has already been used",
                    );
                    return;
                }
                guard.used = true;
                if guard.serial != state.output_mgmt.serial {
                    resource.cancelled();
                    return;
                }
                match validate(state, &guard) {
                    Ok(heads) => {
                        if test {
                            resource.succeeded();
                        } else {
                            // Performed (and answered) by the next
                            // `refresh`, one pass away.
                            state.output_mgmt.pending_apply =
                                Some(PendingApply { resource: resource.clone(), heads });
                        }
                    }
                    Err(reason) => {
                        tracing::info!(%reason, test, "output configuration refused");
                        resource.failed();
                    }
                }
            }
            Request::Destroy => {
                if let Some(pending) = &state.output_mgmt.pending_apply {
                    if &pending.resource == resource {
                        state.output_mgmt.pending_apply = None;
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ConfigHeadData> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &ConfigHeadData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_head_v1::Request;
        let mut guard = data.config.lock().unwrap();
        let Some(head) = guard.heads.get_mut(&data.index) else {
            return;
        };
        let already = |resource: &ZwlrOutputConfigurationHeadV1| {
            resource.post_error(
                zwlr_output_configuration_head_v1::Error::AlreadySet,
                "this property has already been set",
            );
        };
        match request {
            Request::SetMode { mode } => {
                if head.mode.is_some() || head.custom_mode.is_some() {
                    already(resource);
                    return;
                }
                let (output_index, mode_index) =
                    *mode.data::<(usize, usize)>().unwrap_or(&(usize::MAX, usize::MAX));
                if output_index != data.index {
                    resource.post_error(
                        zwlr_output_configuration_head_v1::Error::InvalidMode,
                        "this mode belongs to a different head",
                    );
                    return;
                }
                head.mode = Some(mode_index);
            }
            Request::SetCustomMode { width, height, refresh } => {
                if head.mode.is_some() || head.custom_mode.is_some() {
                    already(resource);
                    return;
                }
                if width <= 0 || height <= 0 || refresh < 0 {
                    resource.post_error(
                        zwlr_output_configuration_head_v1::Error::InvalidCustomMode,
                        "custom mode dimensions must be positive",
                    );
                    return;
                }
                head.custom_mode = Some((width, height, refresh));
            }
            Request::SetPosition { x, y } => {
                if head.position.is_some() {
                    already(resource);
                    return;
                }
                head.position = Some((x, y));
            }
            Request::SetTransform { transform } => {
                if head.transform.is_some() {
                    already(resource);
                    return;
                }
                let value = match transform {
                    WEnum::Value(value) => value as i32,
                    WEnum::Unknown(_) => {
                        resource.post_error(
                            zwlr_output_configuration_head_v1::Error::InvalidTransform,
                            "transform value outside the enum",
                        );
                        return;
                    }
                };
                head.transform = Some(value);
            }
            Request::SetScale { scale } => {
                if head.scale.is_some() {
                    already(resource);
                    return;
                }
                if scale <= 0.0 {
                    resource.post_error(
                        zwlr_output_configuration_head_v1::Error::InvalidScale,
                        "scale must be positive",
                    );
                    return;
                }
                head.scale = Some(scale);
            }
            Request::SetAdaptiveSync { state } => {
                if head.adaptive_sync.is_some() {
                    already(resource);
                    return;
                }
                head.adaptive_sync = Some(match state {
                    WEnum::Value(AdaptiveSyncState::Enabled) => true,
                    WEnum::Value(_) => false,
                    WEnum::Unknown(_) => {
                        resource.post_error(
                            zwlr_output_configuration_head_v1::Error::InvalidAdaptiveSyncState,
                            "adaptive sync value outside the enum",
                        );
                        return;
                    }
                });
            }
            _ => {}
        }
    }
}

/// Pushes the (possibly moved, possibly resized) outputs back into
/// every downstream copy of the layout: the wayland globals' positions,
/// the session backend's viewports, the ledger's monitor list, and the
/// resize drain that makes the shell re-hang its furniture.
fn relayout_ledger(comp: &mut Compositor) {
    for entry in comp.outputs.iter() {
        entry
            .output
            .change_current_state(None, None, None, Some((entry.position.x, entry.position.y).into()));
    }
    let layout: Vec<(Point, Size)> =
        comp.outputs.iter().map(|entry| (entry.position, entry.size)).collect();
    crate::session::sync_positions(&mut comp.graphics, &layout);
    let backend = comp.wm.backend_mut();
    for (index, (position, size)) in layout.iter().enumerate() {
        if let Some(monitor) = backend.monitors.get_mut(index) {
            monitor.geometry.pos = *position;
            monitor.geometry.size = *size;
        }
    }
    backend.output_size = crate::state::union_size(&backend.monitors);
    backend.pending_resize = Some(backend.output_size);
    backend.damage = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    // The protocol halves need a display and a client; what a unit test
    // can reach is the layout arithmetic an `apply` runs on.

    #[test]
    fn a_layout_placed_anywhere_is_translated_back_to_the_origin() {
        // wlr-randr --output A --pos 1920,0 --output B --pos 0,0 needs
        // no shift; the same layout described from (-1920, 0) — a
        // monitor placed to the LEFT of the primary — shifts whole.
        assert_eq!(origin_shift(&[Point::new(0, 0), Point::new(1920, 0)]), (0, 0));
        assert_eq!(origin_shift(&[Point::new(-1920, 0), Point::new(0, 0)]), (-1920, 0));
        assert_eq!(origin_shift(&[Point::new(100, 50), Point::new(2020, 50)]), (100, 50));
        assert_eq!(origin_shift(&[]), (0, 0));
    }
}
