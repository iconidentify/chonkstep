//! The desktop's local, configuration-aware shortcut and quick-reference guide.
//! Content is laid out on entry/filter changes, and only visible lines are painted
//! on scroll. Closing releases the surface and both input grabs.
use crate::startup::SessionState;
use wm_config::Action;
use wm_core::{Backend, FocusDirection, KeyCombo, LayoutMode, Modifiers};
use wm_theme::{
    model::{Color, FontSpec, FontWeight, TextAlign},
    paint, FontState, Theme,
};
use wm_theme_api::{DecorationBuffer, Point, PopupGrab, PopupHost, Rect, Size};

#[derive(Clone, Debug)]
struct Entry {
    section: &'static str,
    title: String,
    keys: String,
    detail: String,
    everyday: bool,
}

fn direction(direction: FocusDirection) -> &'static str {
    match direction {
        FocusDirection::Left => "left",
        FocusDirection::Right => "right",
        FocusDirection::Up => "up",
        FocusDirection::Down => "down",
    }
}

/// Labels name the action ChonkStep actually performs, rather than an imported
/// description of a different window manager's interpretation of the same key.
fn action_label(action: &Action) -> (&'static str, String, bool) {
    use Action::*;
    let (section, title, everyday) = match action {
        Overview => ("GETTING AROUND", "Open / close Overview".into(), true),
        Help => ("GETTING AROUND", "Open ChonkStep Help".into(), true),
        RootMenu => ("GETTING AROUND", "Open the desktop menu".into(), true),
        SpawnTerminal => ("GETTING AROUND", "Open a terminal".into(), true),
        CycleApplications(n) => (
            "GETTING AROUND",
            format!(
                "Switch applications{}",
                if *n < 0 { " backward" } else { "" }
            ),
            true,
        ),
        CycleAppWindows(n) => (
            "GETTING AROUND",
            format!(
                "Switch this app's windows{}",
                if *n < 0 { " backward" } else { "" }
            ),
            true,
        ),
        ApplicationOverview => ("GETTING AROUND", "Show this app's windows".into(), false),
        ShowDesktop => ("GETTING AROUND", "Show / restore the desktop".into(), true),
        Close => ("WINDOWS", "Close the focused window".into(), true),
        ToggleMaximize => ("WINDOWS", "Maximize / restore".into(), true),
        ToggleFullscreen => ("WINDOWS", "Enter / leave fullscreen".into(), true),
        Miniaturize => ("WINDOWS", "Minimize the focused window".into(), true),
        WindowMenu => ("WINDOWS", "Open window commands".into(), true),
        ToggleShade => ("WINDOWS", "Roll up / unroll the window".into(), false),
        QuitApplication => ("WINDOWS", "Quit the focused application".into(), false),
        ForceQuitApplications => ("WINDOWS", "Open Force Quit Applications".into(), false),
        HideApplication => ("WINDOWS", "Hide the focused application".into(), false),
        HideOtherApplications => ("WINDOWS", "Hide other applications".into(), false),
        MiniaturizeApplication => ("WINDOWS", "Minimize this app's windows".into(), false),
        ToggleLayout => ("LAYOUTS", "Switch Mosaic / Flow tiling".into(), true),
        Layout(mode) => (
            "LAYOUTS",
            format!(
                "Use {} layout",
                match mode {
                    LayoutMode::Freeform => "Freeform",
                    LayoutMode::Mosaic => "Mosaic",
                    LayoutMode::Flow => "Flow",
                }
            ),
            true,
        ),
        Floating(value) => (
            "LAYOUTS",
            match value {
                Some(true) => "Float this window",
                Some(false) => "Return this window to the layout",
                None => "Toggle this window's floating state",
            }
            .into(),
            true,
        ),
        Focus(d) => (
            "LAYOUTS",
            format!("Focus the window {}", direction(*d)),
            true,
        ),
        Move(d) => (
            "LAYOUTS",
            format!("Move the window {}", direction(*d)),
            false,
        ),
        Resize(delta) => (
            "LAYOUTS",
            match (delta.x, delta.y) {
                (x, _) if x > 0 => "Grow window width",
                (x, _) if x < 0 => "Shrink window width",
                (_, y) if y > 0 => "Grow window height",
                _ => "Shrink window height",
            }
            .into(),
            false,
        ),
        LayoutNoop => ("LAYOUTS", "Reserved key (no action)".into(), false),
        WorkspaceNext => ("SPACES & WORKSPACES", "Go to the next desktop".into(), true),
        WorkspacePrev => (
            "SPACES & WORKSPACES",
            "Go to the previous desktop".into(),
            true,
        ),
        WorkspaceNextOccupied => (
            "SPACES & WORKSPACES",
            "Go to the next desktop with windows".into(),
            true,
        ),
        WorkspacePrevOccupied => (
            "SPACES & WORKSPACES",
            "Go to the previous desktop with windows".into(),
            true,
        ),
        WorkspacePrevious => (
            "SPACES & WORKSPACES",
            "Go back to the desktop you were on".into(),
            true,
        ),
        FocusMonitor(target) => (
            "SPACES & WORKSPACES",
            match target {
                wm_core::OutputTarget::Relative(step) if *step < 0 => "Focus the previous display".into(),
                wm_core::OutputTarget::Relative(_) => "Focus the next display".into(),
                wm_core::OutputTarget::Direction(d) => format!("Focus the display to the {}", direction(*d)),
                wm_core::OutputTarget::Name(name) => format!("Focus display {name}"),
            },
            false,
        ),
        MoveWorkspaceToMonitor(target) => (
            "SPACES & WORKSPACES",
            match target {
                wm_core::OutputTarget::Relative(step) if *step < 0 => "Move the desktop to the previous display".into(),
                wm_core::OutputTarget::Relative(_) => "Move the desktop to the next display".into(),
                wm_core::OutputTarget::Direction(d) => format!("Move the desktop to the display to the {}", direction(*d)),
                wm_core::OutputTarget::Name(name) => format!("Move the desktop to display {name}"),
            },
            false,
        ),
        WorkspaceCarryNext => (
            "SPACES & WORKSPACES",
            "Move window to next desktop and follow".into(),
            false,
        ),
        WorkspaceCarryPrev => (
            "SPACES & WORKSPACES",
            "Move window to previous desktop and follow".into(),
            false,
        ),
        Workspace(n) => (
            "SPACES & WORKSPACES",
            format!("Go to desktop {}", n + 1),
            *n == 0,
        ),
        WorkspaceSend(n) => (
            "SPACES & WORKSPACES",
            format!("Send window to desktop {} without following", n + 1),
            false,
        ),
        WorkspaceCarry(n) => (
            "SPACES & WORKSPACES",
            format!("Move window to desktop {} and follow", n + 1),
            *n == 0,
        ),
        Capture(mode) => (
            "CAPTURE",
            match mode {
                wm_config::CaptureMode::ScreenClipboard => "Copy a screen capture",
                wm_config::CaptureMode::AreaClipboard => "Copy an area capture",
                wm_config::CaptureMode::WindowClipboard => "Copy a window capture",
                wm_config::CaptureMode::Screen => "Capture a screen",
                wm_config::CaptureMode::Area => "Capture an area",
                wm_config::CaptureMode::Window => "Capture a window",
                wm_config::CaptureMode::Toolbar => "Open capture / recording controls",
                wm_config::CaptureMode::Stop => "Stop recording",
            }
            .into(),
            false,
        ),
        Reload => ("DESKTOP", "Reload configuration".into(), false),
        Restart => (
            "DESKTOP",
            "Restart ChonkStep (closes open apps)".into(),
            false,
        ),
        Run(name) => (
            "APPLICATIONS & SYSTEM",
            format!("Custom command: {name}"),
            false,
        ),
        GlobalShortcut(name) => (
            "APPLICATIONS & SYSTEM",
            format!("Application shortcut: {name}"),
            false,
        ),
    };
    (section, title, everyday)
}

fn key_name(key: u32) -> String {
    match key {
        0xff08 => "Backspace".into(),
        0xff09 | 0xfe20 => "Tab".into(),
        0xff0d => "Enter".into(),
        0xff8d => "Keypad Enter".into(),
        0xff1b => "Esc".into(),
        0x20 => "Space".into(),
        0xff50 => "Home".into(),
        0xff51 => "←".into(),
        0xff52 => "↑".into(),
        0xff53 => "→".into(),
        0xff54 => "↓".into(),
        0xff55 => "Page Up".into(),
        0xff56 => "Page Down".into(),
        0xff57 => "End".into(),
        0xff63 => "Insert".into(),
        0xffff => "Delete".into(),
        0xff61 => "Print Screen".into(),
        0xffbe..=0xffe0 => format!("F{}", key - 0xffbe + 1),
        0xffb0..=0xffb9 => format!("Keypad {}", key - 0xffb0),
        0xffaa => "Keypad ×".into(),
        0xffab => "Keypad +".into(),
        0xffad => "Keypad −".into(),
        0xffae => "Keypad .".into(),
        0xffaf => "Keypad /".into(),
        0x1008ff11 => "Volume down".into(),
        0x1008ff12 => "Mute".into(),
        0x1008ff13 => "Volume up".into(),
        0x1008ff14 => "Play / pause".into(),
        0x1008ff15 => "Media stop".into(),
        0x1008ff16 => "Previous track".into(),
        0x1008ff17 => "Next track".into(),
        0x1008ff02 => "Brightness up".into(),
        0x1008ff03 => "Brightness down".into(),
        0x1008ff05 => "Keyboard light up".into(),
        0x1008ff06 => "Keyboard light down".into(),
        _ => char::from_u32(if key & 0xff000000 == 0x01000000 {
            key & 0xffffff
        } else {
            key
        })
        .filter(|c| !c.is_control() && (key < 0xff00 || key & 0xff000000 == 0x01000000))
        .map_or_else(|| format!("Key 0x{key:X}"), |c| c.to_uppercase().collect()),
    }
}

fn modifiers(mods: Modifiers, mac: bool) -> Vec<&'static str> {
    [
        (Modifiers::SUPER, if mac { "Cmd" } else { "Super" }),
        (Modifiers::CONTROL, "Ctrl"),
        (Modifiers::ALT, if mac { "Option" } else { "Alt" }),
        (Modifiers::SHIFT, "Shift"),
    ]
    .into_iter()
    .filter_map(|(flag, text)| mods.contains(flag).then_some(text))
    .collect()
}

fn key_label(combo: KeyCombo, mac: bool) -> String {
    let mut parts: Vec<String> = modifiers(combo.modifiers, mac)
        .into_iter()
        .map(str::to_owned)
        .collect();
    parts.push(key_name(combo.keysym));
    parts.join(" + ")
}

#[derive(Default)]
struct Content {
    shortcuts: Vec<Entry>,
    reference: Vec<Entry>,
    profile: String,
}
impl Content {
    fn new(state: &SessionState) -> Self {
        let mac = state.interaction.mac_keyboard();
        let mut result = Self {
            profile: if mac {
                "Mac keyboard · Experimental"
            } else {
                "Desktop keyboard · Super is the Windows / logo key"
            }
            .into(),
            ..Self::default()
        };
        let effective: std::collections::HashMap<_, _> =
            state.keybindings.iter().cloned().collect();
        for (combo, action) in &effective {
            let (section, mut title, everyday) = action_label(action);
            let metadata = state
                .bindings
                .iter()
                .rev()
                .find(|b| !b.release && b.combo == *combo && b.action == *action);
            if matches!(action, Action::Run(_) | Action::GlobalShortcut(_)) {
                if let Some(description) = metadata
                    .and_then(|b| b.description.as_ref())
                    .filter(|s| !s.trim().is_empty())
                {
                    title.clone_from(description);
                }
            }
            result.shortcuts.push(Entry {
                section,
                title,
                keys: key_label(*combo, mac),
                detail: String::new(),
                everyday,
            });
        }
        // Release bindings do not live in the press map: push-to-talk can have
        // a different action on each edge of the very same chord.
        for binding in state.bindings.iter().filter(|b| b.release) {
            let (section, title, everyday) = action_label(&binding.action);
            result.shortcuts.push(Entry {
                section,
                title: if matches!(binding.action, Action::Run(_) | Action::GlobalShortcut(_)) {
                    binding.description.clone().unwrap_or(title)
                } else {
                    title
                },
                keys: key_label(binding.combo, mac),
                detail: "On key release".into(),
                everyday,
            });
        }
        // Alt+Tab is owned by wm-core rather than the configurable shell keymap.
        // An explicit shell override takes priority, and Mac uses its own map.
        if !mac {
            for (key, title) in [
                ("alt+tab", "Switch windows"),
                ("alt+shift+tab", "Switch windows backward"),
            ] {
                let combo = wm_config::parse_key(key).unwrap();
                if !effective.contains_key(&combo) {
                    result.shortcuts.push(Entry {
                        section: "GETTING AROUND",
                        title: title.into(),
                        keys: key_label(combo, false),
                        detail: "Hold Alt; release it to select. Esc cancels.".into(),
                        everyday: true,
                    });
                }
            }
        }
        for (namespace, bindings) in &state.layer_bindings {
            for binding in bindings {
                let (_, title, _) = action_label(&binding.action);
                result.shortcuts.push(Entry {
                    section: "CONTEXT SHORTCUTS",
                    title: binding.description.clone().unwrap_or(title),
                    keys: key_label(binding.combo, mac),
                    detail: format!(
                        "While {namespace} is open{}",
                        if binding.release {
                            "; on key release"
                        } else {
                            ""
                        }
                    ),
                    everyday: false,
                });
            }
        }
        let sections = [
            "GETTING AROUND",
            "WINDOWS",
            "LAYOUTS",
            "SPACES & WORKSPACES",
            "CAPTURE",
            "DESKTOP",
            "APPLICATIONS & SYSTEM",
            "CONTEXT SHORTCUTS",
        ];
        result.shortcuts.sort_by_key(|e| {
            (
                sections.iter().position(|s| *s == e.section),
                e.title.clone(),
                e.keys.clone(),
            )
        });
        let mut grouped: Vec<Entry> = Vec::new();
        for entry in result.shortcuts.drain(..) {
            if let Some(previous) = grouped.last_mut().filter(|e| {
                e.section == entry.section && e.title == entry.title && e.detail == entry.detail
            }) {
                previous.keys.push_str("   /   ");
                previous.keys.push_str(&entry.keys);
            } else {
                grouped.push(entry);
            }
        }
        result.shortcuts = grouped;
        let mut reference = |title: &str, detail: String| {
            result.reference.push(Entry {
                section: "",
                title: title.into(),
                keys: String::new(),
                detail,
                everyday: false,
            })
        };
        reference("Freeform", "Place windows wherever you like. Drag a titlebar to move; drag a window edge or corner to resize. Windows can overlap.".into());
        reference("Mosaic", "Arrange windows in a tiling layout that fills the available space. Directional focus selects a neighbor; directional move rearranges windows. A floating window can sit above the layout.".into());
        reference("Flow", "Arrange windows in a horizontal scrolling strip. Focus a window to bring it into view; use directional move to reorder it. Changing a window's width changes its place in the strip.".into());
        reference("Changing layouts", "Use the layout shortcuts on the Everyday tab. The layout toggle enters Mosaic from Freeform, then alternates Mosaic and Flow. Use the Freeform shortcut to return to freely placed windows. Each desktop remembers its own layout; switching layouts keeps your windows open.".into());
        reference("Overview", "See live window previews and the desktop strip. Arrow keys select a window; Enter opens it. Click a window to select it, or drag its preview to a desktop in the strip. Esc or the Overview shortcut returns to your desktop with a smooth transition.".into());
        reference(
            "Spaces & desktops",
            if state.interaction.spaces_mode() {
                format!("{} Fullscreen windows get dedicated Spaces. Use desktop shortcuts or the Overview strip to navigate; you do not need gestures. Leave fullscreen to return the window to its desktop.", if state.interaction.separate_spaces { "Each monitor has its own independent row of Spaces." } else { "Your monitors share the active desktop." })
            } else {
                "Workspaces keep groups of windows together. Use desktop shortcuts or the Overview strip to navigate. A send shortcut moves a window without following; a carry shortcut moves it and takes you there.".into()
            },
        );
        reference("Minimized windows", if state.minimized_previews { "Minimized windows stay out of Overview. Click a desktop preview tile to restore its window, or select it in the window switcher." } else { "Minimized windows stay out of Overview. Select a minimized window in the window switcher to restore it." }.into());
        if let Some(modifier) = state.drag_modifier {
            let key = modifiers(modifier, mac).join(" + ");
            reference("Move or resize from anywhere", format!("Hold {key} and drag with the left mouse button to move a window, or the right mouse button to resize. Window commands are also available by right-clicking a ChonkStep titlebar."));
        }
        reference(
            "Touchpad gestures",
            if state.input.gestures.enabled {
                format!("Swipe with {} fingers: up opens Overview, down returns, and left / right moves between desktops. Your keyboard shortcuts work independently of gestures.", match state.input.gestures.fingers { 3 => "three", 4 => "four", _ => "three or four" })
            } else {
                "Desktop gestures are turned off. Use the keyboard shortcuts and Overview's desktop strip instead.".into()
            },
        );
        reference("Keyboard profiles", if mac { "Mac keyboard translation is experimental and enabled in this session. The shortcut list reflects that profile and your overrides. Spaces and fullscreen Spaces are separate supported features." } else { "Ordinary desktop shortcuts are active. Mac keyboard translation is experimental and turned off. Spaces and fullscreen Spaces are separate supported features." }.into());
        reference("Using this guide", "Right-click the desktop and choose ChonkStep Help. Use Tab or the left / right arrows to change sections. Scroll, use the up / down arrows, or Page Up / Page Down to read. Start typing to search all shortcuts; Backspace edits the search and Esc closes Help. The list refreshes when you reopen Help after a configuration change.".into());
        result
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Close,
    Tab(usize),
}
#[derive(Clone, Copy)]
enum Ink {
    Heading,
    Text,
    Key,
    Muted,
}
struct Line {
    text: String,
    rect: Rect,
    ink: Ink,
}

pub(crate) struct HelpPanel<B: Backend> {
    surface: Option<B::ShellId>,
    grab: Option<PopupGrab>,
    content: Content,
    tab: usize,
    query: String,
    scroll: i32,
    rect: Rect,
    scale: f32,
    lines: Vec<Line>,
    content_height: i32,
    body: Rect,
    pressed: Option<Hit>,
}
impl<B: Backend> Default for HelpPanel<B> {
    fn default() -> Self {
        Self {
            surface: None,
            grab: None,
            content: Content::default(),
            tab: 0,
            query: String::new(),
            scroll: 0,
            rect: Rect::default(),
            scale: 1.0,
            lines: Vec::new(),
            content_height: 0,
            body: Rect::default(),
            pressed: None,
        }
    }
}

impl<B: Backend + PopupHost<PopupId = B::ShellId>> HelpPanel<B> {
    pub fn visible(&self) -> bool {
        self.surface.is_some()
    }
    pub fn owns(&self, surface: B::ShellId) -> bool {
        self.surface == Some(surface)
    }
    fn s(&self, n: u32) -> u32 {
        (n as f32 * self.scale).round() as u32
    }
    fn font(&self, theme: &Theme, size: u32, bold: bool) -> FontSpec {
        FontSpec {
            size: size as f32 * self.scale,
            weight: if bold {
                FontWeight::Bold
            } else {
                FontWeight::Normal
            },
            ..theme.menu.item_font.clone()
        }
    }
    pub fn open(&mut self, backend: &mut B, state: &SessionState, area: Rect, fonts: &FontState) {
        self.close(backend);
        self.scale = state.scale;
        let margin = self.s(20);
        let size = Size::new(
            self.s(960)
                .min(area.size.w.saturating_sub(margin * 2))
                .max(1),
            self.s(740)
                .min(area.size.h.saturating_sub(margin * 2))
                .max(1),
        );
        self.rect = Rect::new(
            Point::new(
                area.pos.x + (area.size.w.saturating_sub(size.w) / 2) as i32,
                area.pos.y + (area.size.h.saturating_sub(size.h) / 2) as i32,
            ),
            size,
        );
        self.body = Rect::new(
            Point::new(0, self.s(158) as i32),
            Size::new(size.w, size.h.saturating_sub(self.s(206))),
        );
        self.content = Content::new(state);
        self.tab = 0;
        self.query.clear();
        self.scroll = 0;
        let Some(surface) = backend.create_shell_surface(self.rect, (30, 30, 30), true) else {
            return;
        };
        self.surface = Some(surface);
        self.layout(&state.theme(), fonts);
        self.paint(backend, &state.theme(), fonts);
        backend.map_shell_surface(surface);
        backend.raise_shell_surface(surface);
        self.grab = Some(PopupHost::grab_pointer(backend));
        Backend::grab_keyboard(backend);
    }
    pub fn close(&mut self, backend: &mut B) {
        if let Some(surface) = self.surface.take() {
            backend.destroy_shell_surface(surface);
            if let Some(grab) = self.grab.take() {
                PopupHost::ungrab_pointer(backend, grab);
            }
            Backend::ungrab_keyboard(backend);
        }
        self.lines.clear();
        self.content = Content::default();
        self.query.clear();
        self.pressed = None;
    }
    fn close_rect(&self) -> Rect {
        Rect::new(
            Point::new(
                self.rect.size.w.saturating_sub(self.s(78)) as i32,
                self.s(20) as i32,
            ),
            Size::new(self.s(56), self.s(34)),
        )
    }
    fn tab_rect(&self, index: usize) -> Rect {
        let width = self.rect.size.w.saturating_sub(self.s(48)) / 3;
        Rect::new(
            Point::new(
                self.s(24) as i32 + (index as u32 * width) as i32,
                self.s(83) as i32,
            ),
            Size::new(width, self.s(35)),
        )
    }
    fn hit(&self, point: Point) -> Option<Hit> {
        if self.close_rect().contains(point) {
            return Some(Hit::Close);
        }
        (0..3)
            .find(|i| self.tab_rect(*i).contains(point))
            .map(Hit::Tab)
    }
    pub fn click(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        fonts: &FontState,
        point: Point,
        pressed: bool,
    ) {
        let hit = self.hit(point);
        if pressed {
            self.pressed = hit;
            return;
        }
        if self.pressed.take() != hit {
            return;
        }
        match hit {
            Some(Hit::Close) => self.close(backend),
            Some(Hit::Tab(tab)) => self.change_tab(backend, theme, fonts, tab),
            None => {}
        }
    }
    fn change_tab(&mut self, backend: &mut B, theme: &Theme, fonts: &FontState, tab: usize) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.scroll = 0;
        self.layout(theme, fonts);
        self.paint(backend, theme, fonts);
    }
    pub fn key(&mut self, backend: &mut B, theme: &Theme, fonts: &FontState, combo: KeyCombo) {
        if combo.keysym == 0xff1b {
            self.close(backend);
            return;
        }
        if combo
            .modifiers
            .intersects(Modifiers::SUPER | Modifiers::ALT)
        {
            return;
        }
        match combo.keysym {
            0xff09 | 0xfe20 => self.change_tab(
                backend,
                theme,
                fonts,
                (self.tab
                    + if combo.modifiers.contains(Modifiers::SHIFT) {
                        2
                    } else {
                        1
                    })
                    % 3,
            ),
            0xff51 => self.change_tab(backend, theme, fonts, (self.tab + 2) % 3),
            0xff53 => self.change_tab(backend, theme, fonts, (self.tab + 1) % 3),
            0xff52 => self.scroll_by(backend, theme, fonts, -(self.s(44) as i32)),
            0xff54 => self.scroll_by(backend, theme, fonts, self.s(44) as i32),
            0xff55 => self.scroll_by(backend, theme, fonts, -(self.body.size.h as i32)),
            0xff56 | 0x20 if self.tab != 1 || self.query.is_empty() => {
                self.scroll_by(backend, theme, fonts, self.body.size.h as i32)
            }
            0xff50 => self.scroll_by(backend, theme, fonts, -self.content_height),
            0xff57 => self.scroll_by(backend, theme, fonts, self.content_height),
            0xff08 => {
                self.query.pop();
                self.tab = 1;
                self.scroll = 0;
                self.layout(theme, fonts);
                self.paint(backend, theme, fonts);
            }
            0x75 if combo.modifiers == Modifiers::CONTROL => {
                self.query.clear();
                self.scroll = 0;
                self.layout(theme, fonts);
                self.paint(backend, theme, fonts);
            }
            key if combo.modifiers.is_empty() || combo.modifiers == Modifiers::SHIFT => {
                if let Some(c) = char::from_u32(key).filter(|c| !c.is_control() && key < 0xff00) {
                    if self.query.len() < 128 {
                        self.query.push(c);
                        self.tab = 1;
                        self.scroll = 0;
                        self.layout(theme, fonts);
                        self.paint(backend, theme, fonts);
                    }
                }
            }
            _ => {}
        }
    }
    pub fn wheel(&mut self, backend: &mut B, theme: &Theme, fonts: &FontState, up: i32) {
        self.scroll_by(
            backend,
            theme,
            fonts,
            up.saturating_mul(-(self.s(44) as i32)),
        );
    }
    fn scroll_by(&mut self, backend: &mut B, theme: &Theme, fonts: &FontState, delta: i32) {
        let next = self.scroll.saturating_add(delta).clamp(
            0,
            self.content_height
                .saturating_sub(self.body.size.h as i32)
                .max(0),
        );
        if next != self.scroll {
            self.scroll = next;
            self.paint(backend, theme, fonts);
        }
    }
    fn layout(&mut self, theme: &Theme, fonts: &FontState) {
        let query = self.query.to_lowercase();
        let entries: Vec<_> = if self.tab == 2 {
            self.content.reference.clone()
        } else {
            self.content
                .shortcuts
                .iter()
                .filter(|e| {
                    (self.tab != 0 || e.everyday)
                        && (self.tab != 1
                            || query.is_empty()
                            || format!("{} {} {} {}", e.section, e.title, e.keys, e.detail)
                                .to_lowercase()
                                .contains(&query))
                })
                .cloned()
                .collect()
        };
        self.lines.clear();
        let pad = self.s(24);
        let width = self.rect.size.w.saturating_sub(pad * 2 + self.s(12)).max(1);
        let split = self.rect.size.w >= self.s(700) && self.tab != 2;
        let mut y = 0_i32;
        let mut section = "";
        for entry in entries {
            if section != entry.section && !entry.section.is_empty() {
                y += self.s(16) as i32;
                self.lines.push(Line {
                    text: entry.section.into(),
                    rect: Rect::new(Point::new(pad as i32, y), Size::new(width, self.s(26))),
                    ink: Ink::Heading,
                });
                y += self.s(34) as i32;
                section = entry.section;
            }
            let title_w = if split { width * 54 / 100 } else { width };
            let title_h = self.add_text(
                &entry.title,
                Point::new(pad as i32, y),
                title_w,
                if self.tab == 2 {
                    Ink::Heading
                } else {
                    Ink::Text
                },
                theme,
                fonts,
            );
            let mut height = title_h;
            if !entry.keys.is_empty() {
                let (x, key_y, key_w) = if split {
                    (pad + width * 58 / 100, y, width * 42 / 100)
                } else {
                    (pad, y + title_h as i32 + self.s(4) as i32, width)
                };
                let key_h = self.add_text(
                    &entry.keys,
                    Point::new(x as i32, key_y),
                    key_w,
                    Ink::Key,
                    theme,
                    fonts,
                );
                height = if split {
                    height.max(key_h)
                } else {
                    height + self.s(4) + key_h
                };
            }
            y += height as i32;
            if !entry.detail.is_empty() {
                y += self.s(4) as i32;
                y += self.add_text(
                    &entry.detail,
                    Point::new(pad as i32, y),
                    width,
                    Ink::Muted,
                    theme,
                    fonts,
                ) as i32;
            }
            y += self.s(if self.tab == 2 { 23 } else { 16 }) as i32;
        }
        if self.lines.is_empty() {
            y += self.add_text("No matching shortcuts. Try a key name or an action such as Overview, Flow, or fullscreen.", Point::new(pad as i32, self.s(16) as i32), width, Ink::Muted, theme, fonts) as i32;
        }
        self.content_height = y + self.s(16) as i32;
    }
    fn add_text(
        &mut self,
        text: &str,
        at: Point,
        width: u32,
        ink: Ink,
        theme: &Theme,
        fonts: &FontState,
    ) -> u32 {
        let font = self.font(
            theme,
            if matches!(ink, Ink::Muted) { 14 } else { 15 },
            matches!(ink, Ink::Heading | Ink::Key),
        );
        let lines = wrap(text, width, &font, &mut fonts.system());
        let line_h = self.s(23);
        let count = lines.len() as u32;
        for (i, text) in lines.into_iter().enumerate() {
            self.lines.push(Line {
                text,
                rect: Rect::new(
                    Point::new(at.x, at.y + (i as u32 * line_h) as i32),
                    Size::new(width, line_h),
                ),
                ink,
            });
        }
        count * line_h
    }
    fn paint(&self, backend: &mut B, theme: &Theme, fonts: &FontState) {
        let Some(surface) = self.surface else {
            return;
        };
        let Some(mut pixels) = tiny_skia::Pixmap::new(self.rect.size.w, self.rect.size.h) else {
            return;
        };
        let background = theme.terminal.bg;
        let foreground = theme.terminal.fg;
        let muted = foreground.mix(background, 0.28);
        let line = foreground.mix(background, 0.85);
        let tint = foreground.mix(background, 0.92);
        pixels.fill(paint::sk_color(background));
        paint::fill_rect(
            &mut pixels,
            0,
            0,
            self.rect.size.w,
            self.s(2).max(1),
            foreground,
        );
        let text = |pixels: &mut tiny_skia::Pixmap,
                    text: &str,
                    rect: Rect,
                    size: u32,
                    bold: bool,
                    color: Color| {
            paint::draw_text(
                pixels,
                &mut fonts.system(),
                &mut fonts.swash(),
                text,
                &self.font(theme, size, bold),
                color,
                rect.pos.x,
                rect.pos.y,
                rect.size.w,
                rect.size.h,
                TextAlign::Left,
            );
        };
        text(
            &mut pixels,
            "ChonkStep Help",
            Rect::new(
                Point::new(self.s(24) as i32, self.s(16) as i32),
                Size::new(self.rect.size.w.saturating_sub(self.s(115)), self.s(35)),
            ),
            25,
            true,
            foreground,
        );
        text(
            &mut pixels,
            &self.content.profile,
            Rect::new(
                Point::new(self.s(24) as i32, self.s(52) as i32),
                Size::new(self.rect.size.w.saturating_sub(self.s(48)), self.s(23)),
            ),
            13,
            false,
            muted,
        );
        let close = self.close_rect();
        paint::fill_rect(
            &mut pixels,
            close.pos.x,
            close.pos.y,
            close.size.w,
            close.size.h,
            tint,
        );
        text(
            &mut pixels,
            "Close",
            Rect::new(
                Point::new(close.pos.x + self.s(8) as i32, close.pos.y),
                close.size,
            ),
            13,
            true,
            foreground,
        );
        for (index, label) in ["Everyday", "All shortcuts", "Quick reference"]
            .iter()
            .enumerate()
        {
            let rect = self.tab_rect(index);
            if self.tab == index {
                paint::fill_rect(
                    &mut pixels,
                    rect.pos.x,
                    rect.pos.y,
                    rect.size.w,
                    rect.size.h,
                    tint,
                );
            }
            text(
                &mut pixels,
                label,
                Rect::new(
                    Point::new(rect.pos.x + self.s(10) as i32, rect.pos.y),
                    Size::new(rect.size.w.saturating_sub(self.s(12)), rect.size.h),
                ),
                14,
                self.tab == index,
                foreground,
            );
            if self.tab == index {
                paint::fill_rect(
                    &mut pixels,
                    rect.pos.x,
                    rect.pos.y + rect.size.h as i32 - self.s(2) as i32,
                    rect.size.w,
                    self.s(2).max(1),
                    foreground,
                );
            }
        }
        let hint = match self.tab {
            0 => "Your current shortcuts · Type to search the full list".into(),
            1 if self.query.is_empty() => "Search shortcuts: start typing · Ctrl+U clears".into(),
            1 => format!("Search: {}", self.query),
            _ => "Layouts, windows, Overview, and Spaces".into(),
        };
        text(
            &mut pixels,
            &hint,
            Rect::new(
                Point::new(self.s(24) as i32, self.s(127) as i32),
                Size::new(self.rect.size.w.saturating_sub(self.s(48)), self.s(23)),
            ),
            14,
            false,
            muted,
        );
        if let Some(mut body) = tiny_skia::Pixmap::new(self.body.size.w, self.body.size.h) {
            body.fill(paint::sk_color(background));
            for row in &self.lines {
                let mut rect = row.rect;
                rect.pos.y -= self.scroll;
                if rect.pos.y + rect.size.h as i32 <= 0 || rect.pos.y >= self.body.size.h as i32 {
                    continue;
                }
                if matches!(row.ink, Ink::Key) {
                    paint::fill_rect(
                        &mut body,
                        rect.pos.x - self.s(5) as i32,
                        rect.pos.y,
                        rect.size.w + self.s(5),
                        rect.size.h,
                        tint,
                    );
                }
                text(
                    &mut body,
                    &row.text,
                    rect,
                    if matches!(row.ink, Ink::Muted) {
                        14
                    } else {
                        15
                    },
                    matches!(row.ink, Ink::Heading | Ink::Key),
                    if matches!(row.ink, Ink::Muted) {
                        muted
                    } else {
                        foreground
                    },
                );
            }
            pixels.draw_pixmap(
                self.body.pos.x,
                self.body.pos.y,
                body.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                tiny_skia::Transform::identity(),
                None,
            );
        }
        if self.content_height > self.body.size.h as i32 && self.body.size.h > 0 {
            let track = self.body.size.h;
            let thumb = (u64::from(track) * u64::from(track) / self.content_height as u64)
                .max(u64::from(self.s(18)))
                .min(u64::from(track)) as u32;
            let y = self.body.pos.y
                + (i64::from(self.scroll) * i64::from(track - thumb)
                    / i64::from(self.content_height - track as i32)) as i32;
            paint::fill_rect(
                &mut pixels,
                self.rect.size.w.saturating_sub(self.s(12)) as i32,
                y,
                self.s(3).max(1),
                thumb,
                muted,
            );
        }
        let footer_y = self.rect.size.h.saturating_sub(self.s(40)) as i32;
        paint::fill_rect(
            &mut pixels,
            self.s(24) as i32,
            footer_y,
            self.rect.size.w.saturating_sub(self.s(48)),
            self.s(1).max(1),
            line,
        );
        text(
            &mut pixels,
            "Tab: sections    ↑ ↓: scroll    Esc: close",
            Rect::new(
                Point::new(self.s(24) as i32, footer_y + self.s(5) as i32),
                Size::new(self.rect.size.w.saturating_sub(self.s(48)), self.s(29)),
            ),
            13,
            false,
            muted,
        );
        backend.paint_shell_surface(
            surface,
            &DecorationBuffer {
                width: pixels.width(),
                height: pixels.height(),
                pixels: pixels.take(),
            },
        );
    }
}

fn wrap(
    text: &str,
    width: u32,
    font: &FontSpec,
    fonts: &mut cosmic_text::FontSystem,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let next = if line.is_empty() {
            word.into()
        } else {
            format!("{line} {word}")
        };
        if !line.is_empty() && paint::text_width(fonts, font, &next) > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        if paint::text_width(fonts, font, word) <= width {
            line.push_str(word);
            continue;
        }
        // Long custom command names must wrap too, rather than running under
        // the key column or out of a narrow display.
        for c in word.chars() {
            let mut next = line.clone();
            next.push(c);
            if !line.is_empty() && paint::text_width(fonts, font, &next) > width {
                lines.push(std::mem::take(&mut line));
            }
            line.push(c);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn help_uses_effective_overrides_and_keeps_spaces_separate_from_mac_keys() {
        let config = wm_config::parse("interaction_mode='spaces'\nkeyboard_mode='desktop'\nhyprland_config=false\n[keybindings]\n'super+up'='none'\n'super+o'='overview'\n'alt+tab'='spawn-terminal'\n").unwrap();
        let content = Content::new(&SessionState::resolve(&config));
        let overview: Vec<_> = content
            .shortcuts
            .iter()
            .filter(|e| e.title == "Open / close Overview")
            .collect();
        assert_eq!(overview.len(), 1);
        assert_eq!(overview[0].keys, "Super + O");
        assert!(!content
            .shortcuts
            .iter()
            .any(|e| e.title == "Switch windows"));
        let spaces = content
            .reference
            .iter()
            .find(|e| e.title == "Spaces & desktops")
            .unwrap();
        assert!(
            spaces.detail.contains("Each monitor") && spaces.detail.contains("dedicated Spaces")
        );
        assert!(!spaces.detail.contains("experimental"));
        assert!(content
            .reference
            .iter()
            .find(|e| e.title == "Keyboard profiles")
            .unwrap()
            .detail
            .contains("turned off"));
    }
}
