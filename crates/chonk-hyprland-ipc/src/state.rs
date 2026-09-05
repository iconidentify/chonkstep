//! The desktop as this protocol needs to describe it.
//!
//! These types are the seam between chonkstep's real state and
//! Hyprland's vocabulary. The caller (`wm-wayland`'s `hyprland_ipc`
//! module) builds a [`Snapshot`] fresh from `WindowManager` every time
//! one is needed; nothing here is stored between calls, because a cache
//! is a thing that can be wrong and this server's entire value is that
//! its answers are true.
//!
//! # Two vocabularies that do not line up
//!
//! Translating is most of the work, and three mismatches are worth
//! stating before the field lists, because each one is a place where a
//! plausible-looking answer would be a wrong one.
//!
//! **Workspaces are 0-based here and 1-based there.** chonkstep numbers
//! workspaces from zero on its own control socket (`docs/control-socket.md`
//! §2 says so explicitly). Hyprland numbers them from one, and Omarchy's
//! bar hard-codes `[1, 2, 3, 4, 5]` as the workspaces it always draws
//! (`plugins/bar/widgets/Workspaces.qml`). Serving a workspace 0 would
//! put a workspace in the bar that no key can reach and leave the first
//! real one unlabelled. The conversion happens in exactly two functions,
//! [`Workspace::hypr_id`] and [`workspace_index_from_hypr_id`], and
//! nowhere else.
//!
//! **chonkstep has no tiling, so every window floats.** `wm-core` is a
//! floating window manager; there is no flag recording "floating"
//! because there is no alternative to it. The JSON therefore reports
//! `floating: true` for every window, which is not a stub — it is the
//! truth, and it is why the tiling dispatchers in [`crate::dispatch`]
//! refuse rather than pretend.
//!
//! **A window's monitor is geometric, not stored.** `Client::monitor`
//! in `wm-core` is an unset slotmap key; multi-monitor policy resolves a
//! window's output from its frame centre against the backend's monitor
//! list. The caller must do that resolution when filling in
//! [`Window::monitor`], because reading the field would report every
//! window on monitor zero.

use serde::Serialize;

/// One output.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub id: i32,
    pub name: String,
    pub description: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    /// Whether this is the output the session considers focused.
    pub focused: bool,
    /// The 0-based chonkstep workspace index active on this output.
    pub active_workspace: usize,
    /// EDID make and model as the matching `wl_output` advertises them,
    /// so the two protocols cannot describe the same panel differently.
    /// `serial` has no source on this compositor and stays empty rather
    /// than being invented.
    pub make: String,
    pub model: String,
    pub serial: String,
    /// The current mode's refresh rate in millihertz, or 0 when the
    /// backend drives no real mode. Millihertz because that is what the
    /// mode carries and what the session already divides into a frame
    /// period; the conversion to Hyprland's float belongs at the wire,
    /// not here.
    pub refresh_millihertz: u32,
    /// Every mode this output can drive, current first — the same list
    /// `zwlr_output_management` enumerates for the same head.
    pub modes: Vec<MonitorMode>,
}

/// One mode an output can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorMode {
    pub width: i32,
    pub height: i32,
    pub refresh_millihertz: u32,
}

impl MonitorMode {
    /// Hyprland's `availableModes` spelling: `WIDTHxHEIGHT@RATEHz`.
    fn to_hyprland(self) -> String {
        format!("{}x{}@{:.2}Hz", self.width, self.height, f64::from(self.refresh_millihertz) / 1000.0)
    }
}

/// The rate reported when an output drives no mode with a real refresh.
///
/// A bar divides by this to pace an animation, so 0 is not an option —
/// this is the number to fall back to, not a number anybody measured.
const FALLBACK_REFRESH_HZ: f64 = 60.0;

/// One workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    /// The **0-based** chonkstep index. Converted on the way out.
    pub index: usize,
    /// Name of the monitor this workspace is on.
    pub monitor: String,
    pub monitor_id: i32,
    /// Number of windows on it, counted by the same rule the control
    /// socket uses: miniaturised windows count, withdrawn ones do not.
    pub windows: u32,
    pub has_fullscreen: bool,
}

impl Workspace {
    /// This workspace's id in Hyprland's numbering.
    ///
    /// The one place `+1` is allowed to appear.
    pub fn hypr_id(&self) -> i32 {
        // A workspace index far enough past i32 to overflow cannot
        // exist — it would need four billion workspaces — but saturate
        // rather than wrap, because a wrapped id would be a *negative*
        // one, and negative ids are how Hyprland spells "special
        // workspace". Silently claiming a special workspace is exactly
        // the confident-wrong-answer failure this module exists to
        // avoid.
        i32::try_from(self.index).unwrap_or(i32::MAX - 1).saturating_add(1)
    }

    /// The name Hyprland would give this workspace.
    ///
    /// Hyprland names an unnamed workspace after its own id, and both
    /// Quickshell and Omarchy match workspaces *by name* in several
    /// places (`focusedmon`'s payload, `openwindow`'s payload,
    /// `findWorkspaceByName`), so the name has to be the decimal id
    /// rather than anything friendlier.
    pub fn hypr_name(&self) -> String {
        self.hypr_id().to_string()
    }
}

/// Convert a workspace id as a client sent it back into a chonkstep index.
///
/// Returns `None` for ids chonkstep cannot represent — zero, negatives
/// (Hyprland's special workspaces, which chonkstep does not have) — so
/// the caller reports an error instead of guessing at workspace 0.
pub fn workspace_index_from_hypr_id(id: i32) -> Option<usize> {
    if id <= 0 {
        return None;
    }
    usize::try_from(id - 1).ok()
}

/// One managed window.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// chonkstep's opaque `ClientId::as_u64()`. Rendered as Hyprland's
    /// hex `address`.
    pub id: u64,
    pub title: String,
    pub class: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// 0-based chonkstep workspace index.
    pub workspace: usize,
    /// Resolved geometrically by the caller; see the module doc.
    pub monitor: i32,
    pub pid: i32,
    /// True for an X11 window managed through XWayland.
    pub xwayland: bool,
    pub fullscreen: bool,
    /// Miniaturised. Hyprland's nearest concept is `hidden`, which
    /// `omarchy-capture-region` filters on (`select(.hidden != true)`)
    /// to keep iconified windows out of its rectangle list.
    pub hidden: bool,
    pub urgent: bool,
    pub pinned: bool,
    pub inhibiting_idle: bool,
    pub tags: Vec<String>,
    pub xdg_tag: String,
    pub xdg_description: String,
    /// Position in the focus history, 0 = focused. Omarchy reads
    /// `.focusHistoryID` in one place.
    pub focus_history_id: i32,
}

impl Window {
    /// The window's Hyprland address: `0x` followed by lowercase hex.
    ///
    /// Quickshell parses this with `toULongLong(&ok, 16)`, which accepts
    /// the `0x` prefix, and **skips any entry whose address does not
    /// parse** (`connection.cpp`, "Invalid address in j/clients entry").
    /// Event payloads carry the same value; the two must agree
    /// numerically or Quickshell cannot correlate an event with the
    /// window it is about.
    pub fn address(&self) -> String {
        format!("0x{:x}", self.id)
    }
}

/// Everything the protocol can be asked about, as of one instant.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<Workspace>,
    pub windows: Vec<Window>,
    /// `ClientId::as_u64()` of the focused window, if any.
    pub focused: Option<u64>,
    /// Whether a session lock is in force. Reported to clients as
    /// `LOCK` in every monitor's `solitaryBlockedBy`, which is the only
    /// place Hyprland's IPC exposes lock state and therefore the only
    /// place anything on an Omarchy machine looks for it.
    pub locked: bool,
    /// Real root-coordinate pointer position, absent only before the
    /// compositor has received its first pointer motion.
    pub cursor_position: Option<(i32, i32)>,
    pub bindings: Vec<Binding>,
    /// Effective configuration refusals, one actionable message each.
    pub config_errors: Vec<String>,
    pub devices: Devices,
}

/// One keybinding in the subset `hyprctl binds` exposes to menus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub modifiers: u32,
    pub key: String,
    pub description: String,
    pub dispatcher: String,
    pub argument: String,
    pub locked: bool,
    pub repeating: bool,
    pub release: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Devices {
    pub keyboards: Vec<Keyboard>,
    pub mice: Vec<PointerDevice>,
    pub touch: Vec<PointerDevice>,
    pub tablets: Vec<PointerDevice>,
    pub switches: Vec<PointerDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyboard {
    pub name: String,
    pub layout: String,
    pub active_keymap: String,
    pub active_layout_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerDevice {
    pub name: String,
}

impl Snapshot {
    /// The focused window, if one is focused and still present.
    pub fn focused_window(&self) -> Option<&Window> {
        let focused = self.focused?;
        self.windows.iter().find(|window| window.id == focused)
    }

    /// The workspace the session is on: the active workspace of the
    /// focused monitor, falling back to the first monitor.
    pub fn active_workspace(&self) -> Option<&Workspace> {
        let index =
            self.monitors.iter().find(|monitor| monitor.focused).or_else(|| self.monitors.first())?.active_workspace;
        self.workspaces.iter().find(|workspace| workspace.index == index)
    }

    /// The focused monitor, falling back to the first.
    pub fn focused_monitor(&self) -> Option<&Monitor> {
        self.monitors.iter().find(|monitor| monitor.focused).or_else(|| self.monitors.first())
    }

    fn monitor_name(&self, id: i32) -> String {
        self.monitors.iter().find(|monitor| monitor.id == id).map(|monitor| monitor.name.clone()).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------
// The wire shapes.
//
// Field names below are Hyprland's, not ours, and several of them are
// neither `camelCase` nor `snake_case` but something in between
// (`hasfullscreen`, `lastwindowtitle`, `activeWorkspace`). That is not a
// mistake to tidy up: Omarchy's `jq` filters and Quickshell's
// `object.value("hasfullscreen")` are written against those exact
// spellings, and a "corrected" name is a field the caller reads as
// `null`. Every `#[serde(rename)]` here is load-bearing.
// ---------------------------------------------------------------------

/// The nested `{ "id": N, "name": "N" }` a client or monitor carries.
#[derive(Debug, Serialize)]
pub struct WorkspaceRef {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct MonitorJson {
    pub id: i32,
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub width: i32,
    pub height: i32,
    #[serde(rename = "refreshRate")]
    pub refresh_rate: f64,
    pub x: i32,
    pub y: i32,
    #[serde(rename = "activeWorkspace")]
    pub active_workspace: WorkspaceRef,
    #[serde(rename = "specialWorkspace")]
    pub special_workspace: WorkspaceRef,
    /// `[left, top, right, bottom]`. chonkstep has workareas but does
    /// not publish them per-output here; zero is honest for a desktop
    /// with no reserved struts and wrong for one that has them, so this
    /// is filled in by the caller when it knows.
    pub reserved: [i32; 4],
    pub scale: f64,
    pub transform: i32,
    pub focused: bool,
    #[serde(rename = "dpmsStatus")]
    pub dpms_status: bool,
    pub vrr: bool,
    /// Why a monitor cannot hand a single client the scanout plane —
    /// and, incidentally, the only place Hyprland's IPC exposes lock
    /// state at all. `omarchy-hyprland-session-locked` reads exactly
    /// this field, looking for `LOCK`, because Hyprland reports no lock
    /// any other way; anything that asks "is the screen locked" on an
    /// Omarchy machine is asking this string.
    ///
    /// chonkstep blocks solitary scanout for no reason it could name,
    /// so the only value it ever reports is the lock — `LOCK` while a
    /// session lock is in force, null otherwise, which is also what
    /// Hyprland reports when nothing is blocking. Reporting null
    /// unconditionally, as this did at first, told every caller the
    /// session was unlocked: `omarchy-restart-shell` would then kill
    /// the locker it was supposed to protect and leave the desk open.
    #[serde(rename = "solitaryBlockedBy")]
    pub solitary_blocked_by: Vec<String>,
    #[serde(rename = "activelyTearing")]
    pub actively_tearing: bool,
    #[serde(rename = "directScanoutTo")]
    pub direct_scanout_to: Option<String>,
    pub disabled: bool,
    #[serde(rename = "currentFormat")]
    pub current_format: String,
    #[serde(rename = "mirrorOf")]
    pub mirror_of: String,
    #[serde(rename = "availableModes")]
    pub available_modes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceJson {
    pub id: i32,
    pub name: String,
    pub monitor: String,
    #[serde(rename = "monitorID")]
    pub monitor_id: i32,
    pub windows: u32,
    pub hasfullscreen: bool,
    pub lastwindow: String,
    pub lastwindowtitle: String,
    pub ispersistent: bool,
}

#[derive(Debug, Serialize)]
pub struct ClientJson {
    pub address: String,
    pub mapped: bool,
    pub hidden: bool,
    /// `[x, y]` — an **array**, which `omarchy-capture-region` indexes
    /// as `.at[0]` / `.at[1]`. An object here yields `null,null` in its
    /// output rather than an error.
    pub at: [i32; 2],
    /// `[w, h]`, indexed as `.size[0]` / `.size[1]`.
    pub size: [i32; 2],
    pub workspace: WorkspaceRef,
    /// Always true: chonkstep is a floating window manager and has no
    /// other state for a window to be in. See the module doc.
    pub floating: bool,
    pub pseudo: bool,
    pub monitor: i32,
    pub class: String,
    pub title: String,
    #[serde(rename = "initialClass")]
    pub initial_class: String,
    #[serde(rename = "initialTitle")]
    pub initial_title: String,
    pub pid: i32,
    pub xwayland: bool,
    pub pinned: bool,
    pub fullscreen: i32,
    #[serde(rename = "fullscreenClient")]
    pub fullscreen_client: i32,
    pub grouped: Vec<String>,
    pub tags: Vec<String>,
    pub swallowing: String,
    #[serde(rename = "focusHistoryID")]
    pub focus_history_id: i32,
    #[serde(rename = "inhibitingIdle")]
    pub inhibiting_idle: bool,
    #[serde(rename = "xdgTag")]
    pub xdg_tag: String,
    #[serde(rename = "xdgDescription")]
    pub xdg_description: String,
}

impl Snapshot {
    /// `j/monitors`.
    pub fn monitors_json(&self) -> Vec<MonitorJson> {
        self.monitors
            .iter()
            .map(|monitor| {
                let workspace = self.workspaces.iter().find(|workspace| workspace.index == monitor.active_workspace);
                MonitorJson {
                    id: monitor.id,
                    name: monitor.name.clone(),
                    description: monitor.description.clone(),
                    make: monitor.make.clone(),
                    model: monitor.model.clone(),
                    serial: monitor.serial.clone(),
                    width: monitor.width,
                    height: monitor.height,
                    // Hyprland reports the mode's refresh rate, and so
                    // does this: the session picks a mode per connector
                    // and already divides its millihertz into a frame
                    // period. Only a backend driving no real mode falls
                    // back to [`FALLBACK_REFRESH_HZ`], which exists so a
                    // bar pacing an animation never divides by zero.
                    refresh_rate: if monitor.refresh_millihertz == 0 {
                        FALLBACK_REFRESH_HZ
                    } else {
                        f64::from(monitor.refresh_millihertz) / 1000.0
                    },
                    x: monitor.x,
                    y: monitor.y,
                    active_workspace: WorkspaceRef {
                        id: workspace.map_or(1, Workspace::hypr_id),
                        name: workspace.map_or_else(|| "1".to_string(), Workspace::hypr_name),
                    },
                    // chonkstep has no scratchpad, so there is never a
                    // special workspace. Hyprland spells "none" as id 0
                    // with an empty name, and Quickshell's
                    // `findWorkspaceByName` is never called with it.
                    special_workspace: WorkspaceRef { id: 0, name: String::new() },
                    reserved: [0, 0, 0, 0],
                    scale: monitor.scale,
                    transform: 0,
                    focused: monitor.focused,
                    dpms_status: true,
                    vrr: false,
                    solitary_blocked_by: self.locked.then(|| "LOCK".to_string()).into_iter().collect(),
                    actively_tearing: false,
                    direct_scanout_to: None,
                    disabled: false,
                    current_format: "XRGB8888".to_string(),
                    mirror_of: "none".to_string(),
                    available_modes: monitor.modes.iter().map(|mode| mode.to_hyprland()).collect(),
                }
            })
            .collect()
    }

    /// `j/workspaces`.
    pub fn workspaces_json(&self) -> Vec<WorkspaceJson> {
        self.workspaces.iter().map(|workspace| self.workspace_json(workspace)).collect()
    }

    fn workspace_json(&self, workspace: &Workspace) -> WorkspaceJson {
        // Hyprland reports the workspace's most recently focused window.
        // The nearest true statement chonkstep can make is "the focused
        // window, if it is on this workspace" — it keeps no per-workspace
        // focus history — so say that and leave it empty otherwise
        // rather than naming an arbitrary window as the last one.
        let last = self.focused_window().filter(|window| window.workspace == workspace.index);
        WorkspaceJson {
            id: workspace.hypr_id(),
            name: workspace.hypr_name(),
            monitor: workspace.monitor.clone(),
            monitor_id: workspace.monitor_id,
            windows: workspace.windows,
            hasfullscreen: workspace.has_fullscreen,
            lastwindow: last.map(Window::address).unwrap_or_else(|| "0x0".to_string()),
            lastwindowtitle: last.map(|window| window.title.clone()).unwrap_or_default(),
            ispersistent: false,
        }
    }

    /// `j/clients`.
    pub fn clients_json(&self) -> Vec<ClientJson> {
        self.windows.iter().map(|window| self.client_json(window)).collect()
    }

    fn client_json(&self, window: &Window) -> ClientJson {
        let workspace = self.workspaces.iter().find(|w| w.index == window.workspace);
        ClientJson {
            address: window.address(),
            mapped: !window.hidden,
            hidden: window.hidden,
            at: [window.x, window.y],
            size: [window.width, window.height],
            workspace: WorkspaceRef {
                id: workspace.map_or(1, Workspace::hypr_id),
                name: workspace.map_or_else(|| "1".to_string(), Workspace::hypr_name),
            },
            floating: true,
            pseudo: false,
            monitor: window.monitor,
            class: window.class.clone(),
            title: window.title.clone(),
            // chonkstep does not retain a window's first class or title,
            // and the fields exist because `omarchy-launch-or-focus`
            // matches on `.initialClass`. Reporting the current value is
            // right whenever the window never renamed itself, which is
            // the common case, and is a strictly better match than an
            // empty string would be.
            initial_class: window.class.clone(),
            initial_title: window.title.clone(),
            pid: window.pid,
            xwayland: window.xwayland,
            pinned: window.pinned,
            fullscreen: i32::from(window.fullscreen),
            fullscreen_client: 0,
            grouped: Vec::new(),
            tags: window.tags.clone(),
            swallowing: "0x0".to_string(),
            focus_history_id: window.focus_history_id,
            inhibiting_idle: window.inhibiting_idle,
            xdg_tag: window.xdg_tag.clone(),
            xdg_description: window.xdg_description.clone(),
        }
    }

    /// `j/activewindow` — the focused window's client object, or `{}`.
    ///
    /// Hyprland answers an empty **object**, not `null` and not an
    /// empty array, when nothing is focused;
    /// `omarchy-cmd-terminal-cwd` pipes this straight into `jq`, which
    /// would fail on a bare `null`.
    pub fn active_window_json(&self) -> serde_json::Value {
        match self.focused_window() {
            Some(window) => serde_json::to_value(self.client_json(window)).unwrap_or_else(|_| serde_json::json!({})),
            None => serde_json::json!({}),
        }
    }

    /// `j/activeworkspace`.
    pub fn active_workspace_json(&self) -> serde_json::Value {
        match self.active_workspace() {
            Some(workspace) => {
                serde_json::to_value(self.workspace_json(workspace)).unwrap_or_else(|_| serde_json::json!({}))
            }
            None => serde_json::json!({}),
        }
    }

    /// Resolve the monitor name for a window, for event payloads.
    pub fn window_monitor_name(&self, window: &Window) -> String {
        self.monitor_name(window.monitor)
    }
}
