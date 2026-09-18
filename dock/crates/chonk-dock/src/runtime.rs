//! Application lifecycle and input routing. No session daemon stays behind
//! when this process exits.
use crate::{
    apps,
    client::{Client, Event, Platform},
    control::Control,
    desktop::Desktop,
    launchdock::{LaunchDock, LaunchDockAction},
    surface::{Backend, Role},
};
use chonk_dock_proto::wire::PanelCloseReason;
use chonk_dock_widget::{DockInput, MouseButton};
use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wm_theme::{
    cascade::{CascadeMenu, MenuClick},
    menu::MenuItem,
};
use wm_theme_api::{Point, Rect, Size};

pub struct Options {
    pub platform: Platform,
    pub output: Option<String>,
    pub theme: Option<String>,
    pub scale: f32,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            platform: Platform::detect(),
            output: None,
            theme: None,
            scale: 1.0,
        }
    }
}

static QUIT: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

pub fn socket_path(platform: Platform) -> std::io::Result<std::path::PathBuf> {
    let directory = chonk_ipc::socket_dir()?;
    chonk_ipc::ensure_socket_dir(&directory)?;
    Ok(directory.join(format!("dock-control-{}.sock", platform.socket_key())))
}

fn theme(options: &Options, control: &Control, scale: f32) -> wm_theme::Theme {
    let id = options
        .theme
        .as_deref()
        .or_else(|| control.theme.as_ref().map(|(id, _)| id.as_str()))
        .unwrap_or("nextstep-classic");
    let mode = control
        .theme
        .as_ref()
        .map(|(_, mode)| *mode)
        .unwrap_or(wm_theme::Appearance::Dark);
    let theme = if id == "omarchy" {
        wm_theme::omarchy::load_current().ok()
    } else {
        wm_theme::default_theme::theme_variant(id, mode)
    };
    theme
        .unwrap_or_else(wm_theme::default_theme::nextstep_classic)
        .scaled(scale)
}

pub fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    // Binding first makes two simultaneous launches race for a single owner,
    // before either constructs instruments, sampler workers, or dockapp tiles.
    let listener = chonk_ipc::StreamListener::bind(&socket_path(options.platform)?)?;
    // SAFETY: the handlers only store a lock-free atomic flag. They do no I/O,
    // allocation, or work with the dock's ordinary Rust state.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let mut display = Client::connect(options.platform, options.output.as_deref())?;
    let mut control = Control::for_display(options.platform.display_name());
    control.tick();
    let mut screen = display.screen();
    let mut scale = display.scale() * options.scale;
    let mut theme = theme(&options, &control, scale);
    let fonts = wm_theme::FontState::new();
    let applications = apps::scan_applications();
    let mut dock = Desktop::new(&mut display, screen, scale, &theme, fonts.clone());
    let mut launcher = LaunchDock::new(
        &mut display,
        &theme,
        screen,
        crate::desktop::tile_px(scale),
        &applications,
        fonts.clone(),
    );
    let mut menu = CascadeMenu::<u32>::new("Chonk Dock", (128, 129, 159));
    let mut remote_menu: Option<String> = None;
    let mut icons = BTreeMap::<u32, (u32, String)>::new();
    let mut icon_drag: Option<(u32, Point, bool)> = None;
    let mut clients: Vec<(chonk_ipc::Stream, Vec<u8>, Instant)> = Vec::new();
    let mut last_theme = control.theme.clone();
    let mut theme_refresh = Instant::now() + Duration::from_secs(2);
    let mut first = true;
    let mut running = display.running();
    println!("running");
    while !QUIT.load(Ordering::Relaxed) && !display.closed() {
        display.dispatch()?;
        let mut windows_changed = display.take_windows_changed();
        control.tick();
        // Re-read Omarchy's palette while opted into following it; other
        // built-ins restyle solely in response to control events.
        let refresh_palette = Instant::now() >= theme_refresh && theme.id == "omarchy";
        if refresh_palette {
            theme_refresh = Instant::now() + Duration::from_secs(2);
        }
        if display.take_changed() || last_theme != control.theme || refresh_palette {
            last_theme = control.theme.clone();
            let next_scale = display.scale() * options.scale;
            let next_screen = display.screen();
            let next_theme = self::theme(&options, &control, next_scale);
            if next_theme != theme || next_scale != scale || next_screen != screen {
                screen = next_screen;
                scale = next_scale;
                theme = next_theme;
                menu.close(&mut display);
                dock.restyle(&mut display, screen, scale, &theme);
                launcher.restyle(&mut display, &theme, crate::desktop::tile_px(scale));
                launcher.reposition(&mut display, &theme, screen);
                for (_, (surface, _)) in std::mem::take(&mut icons) {
                    display.destroy_shell_surface(surface);
                }
                windows_changed = true;
            }
        }
        let tile = crate::desktop::tile_px(scale);
        if windows_changed {
            running = display.running();
        }
        if windows_changed || first || launcher.running_refresh_needed() {
            first = false;
            launcher.update_running(&mut display, &theme, &running);
            sync_icons(&mut display, &mut icons, &theme, &fonts, screen, tile);
        }
        let (current, count) = if control.connected() {
            (control.current, control.count)
        } else {
            display.workspace().unwrap_or((0, 1))
        };
        dock.set_workspace_display(&mut display, &theme, current, count);
        for event in display.take_events() {
            match event {
                Event::Dismiss(surface) => {
                    menu.close(&mut display);
                    if dock.instrument_panel_owns(surface) {
                        dock.dismiss_instrument_panel(&mut display, PanelCloseReason::Dismissed);
                    }
                }
                Event::Escape => {
                    menu.close(&mut display);
                    dock.dismiss_instrument_panel(&mut display, PanelCloseReason::Dismissed);
                }
                Event::Leave => {
                    dock.update_dock_hover(&mut display, &theme, Point::new(-1, -1));
                    dock.update_panel_hover(&theme, Point::new(-1, -1));
                }
                Event::Motion(surface, local) => {
                    let Some(rect) = display.geometry(surface) else {
                        continue;
                    };
                    let root = Point::new(rect.pos.x + local.x, rect.pos.y + local.y);
                    menu.hover(&mut display, &theme, &mut fonts.system(), surface, local);
                    dock.update_dock_hover(&mut display, &theme, root);
                    dock.update_panel_hover(&theme, root);
                    dock.drag_item_motion(&mut display, &theme, root);
                    launcher.handle_motion(&mut display, &theme, root);
                    if let Some((_, press, moved)) = &mut icon_drag {
                        *moved |= (root.x - press.x).abs().max((root.y - press.y).abs())
                            >= crate::desktop::drag_threshold_px(scale);
                    }
                }
                Event::Button(surface, local, button, pressed) => {
                    if display.is_backdrop(surface) && pressed {
                        menu.close(&mut display);
                        dock.dismiss_instrument_panel(&mut display, PanelCloseReason::Dismissed);
                        continue;
                    }
                    if pressed && button == 0x111 {
                        menu.close(&mut display);
                    }
                    let Some(rect) = display.geometry(surface) else {
                        continue;
                    };
                    let root = Point::new(rect.pos.x + local.x, rect.pos.y + local.y);
                    if pressed && button == 0x110 {
                        if let Some(result) =
                            menu.click(&mut display, &theme, &mut fonts.system(), surface, local)
                        {
                            if let MenuClick::Action(action) = result {
                                if let Some(id) = remote_menu.take() {
                                    dock.remote_action(&mut display, &theme, &id, action);
                                } else if action == u32::MAX {
                                    QUIT.store(true, Ordering::Relaxed);
                                } else if action >= 10000 {
                                    if let Some(app) = applications.get((action - 10000) as usize) {
                                        launcher.try_pin_at(
                                            &mut display,
                                            &theme,
                                            Point::new(screen.pos.x, screen.pos.y),
                                            app,
                                        );
                                    }
                                } else if let Some(app) = applications.get(action as usize) {
                                    launch(app, &theme, options.scale, options.platform);
                                }
                            }
                            continue;
                        }
                        menu.close(&mut display);
                    }
                    if !pressed {
                        if button == 0x112 && dock.end_item_drag(&mut display, &theme) {
                            continue;
                        }
                        if button == 0x110 {
                            if let Some((window, _, moved)) = icon_drag.take() {
                                if moved {
                                    if let Some(app) = display
                                        .window_app_id(window)
                                        .and_then(|id| apps::match_window_class(&applications, id))
                                        .and_then(|index| applications.get(index))
                                    {
                                        launcher.try_pin_at(&mut display, &theme, root, app);
                                    }
                                } else {
                                    display.activate(window);
                                }
                                continue;
                            }
                            if launcher.handle_release(&mut display, &theme, root) {
                                continue;
                            }
                        }
                    }
                    if surface == dock.dock_window() {
                        if local.y < tile as i32 && pressed {
                            dock.dismiss_instrument_panel(
                                &mut display,
                                PanelCloseReason::Dismissed,
                            );
                            remote_menu = None;
                            let items = application_menu(&applications);
                            menu.open(
                                &mut display,
                                &theme,
                                &mut fonts.system(),
                                items,
                                root,
                                screen.size,
                                true,
                            );
                        } else if button == 0x112 && pressed {
                            dock.begin_item_drag(&mut display, &theme, local);
                        } else if button == 0x111 && pressed {
                            if !dock.toggle_builtin_panel(&mut display, &theme, local) {
                                if let Some((id, items)) = dock.remote_menu(local) {
                                    remote_menu = Some(id);
                                    menu.open(
                                        &mut display,
                                        &theme,
                                        &mut fonts.system(),
                                        items,
                                        root,
                                        screen.size,
                                        true,
                                    );
                                }
                            }
                        } else if button == 0x110 {
                            let input = if pressed {
                                DockInput::Press {
                                    local,
                                    button: MouseButton::Left,
                                }
                            } else {
                                DockInput::Release {
                                    local,
                                    button: MouseButton::Left,
                                }
                            };
                            dock.dock_input(&mut display, &theme, input);
                        }
                    } else if surface == dock.clip_window() && button == 0x110 && pressed {
                        dock.click_clip(local);
                    } else if dock.instrument_panel_owns(surface) && button == 0x110 {
                        dock.instrument_panel_click(&theme, local, MouseButton::Left, pressed);
                    } else if launcher.owns_window(surface) && button == 0x110 {
                        if let Some(action) =
                            launcher.handle_click(&mut display, &theme, local, pressed, &running)
                        {
                            match action {
                                LaunchDockAction::Focus(window) => display.activate(window),
                                LaunchDockAction::Launch(app) => {
                                    launch(&app, &theme, options.scale, options.platform)
                                }
                            }
                        }
                    } else if button == 0x110 && pressed {
                        dock.dismiss_instrument_panel(&mut display, PanelCloseReason::Dismissed);
                        if let Some((&window, _)) = icons.iter().find(|(_, (id, _))| *id == surface)
                        {
                            icon_drag = Some((window, root, false));
                        }
                    }
                }
                Event::Scroll(surface, local, delta) => {
                    if surface == dock.dock_window() {
                        dock.dock_input(&mut display, &theme, DockInput::Scroll { local, delta });
                    } else if dock.instrument_panel_owns(surface) {
                        dock.instrument_panel_scroll(&theme, local, delta);
                    }
                }
            }
        }
        if let Some(index) = dock.take_workspace_request() {
            if control.connected() {
                control.focus_workspace(index);
            } else {
                display.focus_workspace(index);
            }
        }
        menu.tick(&mut display, &theme, &mut fonts.system());
        dock.tick_items(&mut display, &theme);
        display.sync_backdrop();
        display.present()?;
        for _ in 0..8 {
            let Some(client) = listener.accept()? else {
                break;
            };
            if clients.len() < 16 && client.peer_is_this_user()? {
                clients.push((client, Vec::new(), Instant::now()));
            }
        }
        let mut reload = false;
        clients.retain_mut(|(client, input, since)| {
            let mut bytes = [0u8; 64];
            match client.recv(&mut bytes) {
                Ok(0) => return false,
                Ok(n) => input.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return false,
            }
            if let Some(end) = input.iter().position(|b| *b == b'\n') {
                let response: &[u8] = match &input[..end] {
                    b"stop" => {
                        QUIT.store(true, Ordering::Relaxed);
                        b"stopping\n"
                    }
                    b"reload" => {
                        reload = true;
                        b"reloaded\n"
                    }
                    b"status" => b"running\n",
                    _ => b"unknown command\n",
                };
                let _ = client.send(response);
                return false;
            }
            input.len() <= 64 && since.elapsed() < Duration::from_secs(2)
        });
        if reload {
            launcher.reload(&mut display, &theme, &applications);
        }
        if QUIT.load(Ordering::Relaxed) {
            break;
        }
        let mut descriptors = vec![libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        if let Some(fd) = control.fd() {
            descriptors.push(libc::pollfd {
                fd,
                events: libc::POLLIN
                    | if control.wants_write() {
                        libc::POLLOUT
                    } else {
                        0
                    },
                revents: 0,
            });
        }
        descriptors.extend(clients.iter().map(|(client, _, _)| libc::pollfd {
            fd: client.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }));
        let mut remote_fds = Vec::new();
        dock.extend_extra_poll_fds(&mut remote_fds);
        descriptors.extend(remote_fds.into_iter().map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        }));
        let now = Instant::now();
        let idle = if dock.instrument_panel_visible() {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(250)
        };
        let deadline = dock
            .next_housekeeping_deadline(now)
            .into_iter()
            .chain(menu.next_deadline())
            .min();
        let timeout = deadline.map_or(idle, |at| at.saturating_duration_since(now).min(idle));
        display.wait(descriptors, timeout)?;
    }
    // Desktop::drop terminates hosted dockapps even on an error above.
    Ok(())
}

fn launch(app: &apps::AppEntry, theme: &wm_theme::Theme, scale: f32, platform: Platform) {
    let mut argv = app.exec.clone();
    if argv.is_empty() {
        return;
    }
    if app.terminal {
        argv.splice(
            0..0,
            [
                std::env::var("TERMINAL").unwrap_or_else(|_| {
                    if platform == Platform::X11 {
                        "xterm"
                    } else {
                        "foot"
                    }
                    .into()
                }),
                "-e".into(),
            ],
        );
    }
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    crate::spawn::spawn_detached_with_env(
        &argv[0],
        &args,
        &crate::startup::launch_env(&theme.id, Some(theme.appearance), scale),
        &[],
    );
}

fn application_menu(apps: &[apps::AppEntry]) -> Vec<MenuItem> {
    let mut groups = BTreeMap::new();
    let mut pins = Vec::new();
    for (index, app) in apps.iter().take(10000).enumerate() {
        groups
            .entry(app.category)
            .or_insert_with(Vec::new)
            .push(MenuItem::Action {
                label: app.name.clone(),
                action: index as u32,
            });
        pins.push(MenuItem::Action {
            label: app.name.clone(),
            action: 10000 + index as u32,
        });
    }
    let mut menu: Vec<_> = groups
        .into_iter()
        .map(|(category, items)| MenuItem::Submenu {
            label: category.label().into(),
            items,
        })
        .collect();
    menu.push(MenuItem::Submenu {
        label: "Pin application".into(),
        items: pins,
    });
    menu.push(MenuItem::Action {
        label: "Quit Dock".into(),
        action: u32::MAX,
    });
    menu
}

fn sync_icons(
    display: &mut Client,
    icons: &mut BTreeMap<u32, (u32, String)>,
    theme: &wm_theme::Theme,
    fonts: &wm_theme::FontState,
    screen: Rect,
    tile: u32,
) {
    let minimized = display.minimized();
    icons.retain(|id, (surface, _)| {
        if minimized.iter().any(|(window, _)| window == id) {
            true
        } else {
            display.destroy_shell_surface(*surface);
            false
        }
    });
    let columns = (screen.size.w.saturating_sub(tile) / tile.max(1)).max(1);
    for (slot, (window, title)) in minimized.into_iter().take(128).enumerate() {
        let rect = Rect {
            pos: Point::new(
                (slot as u32 % columns * tile) as i32,
                screen
                    .size
                    .h
                    .saturating_sub((slot as u32 / columns + 1) * tile) as i32,
            ),
            size: Size::new(tile, tile),
        };
        let (surface, paint) = if let Some((surface, old)) = icons.get_mut(&window) {
            let paint = old != &title;
            *old = title.clone();
            (*surface, paint)
        } else {
            let Some(surface) = display.create_shell_surface(rect, (128, 129, 159), true) else {
                continue;
            };
            display.set_role(surface, Role::Icon);
            display.map_shell_surface(surface);
            icons.insert(window, (surface, title.clone()));
            (surface, true)
        };
        display.configure_shell_surface(surface, rect);
        if paint {
            let pixels = wm_theme::launcher::render_launcher_tile(
                theme,
                &mut fonts.system(),
                &mut fonts.swash(),
                tile,
                &title,
                false,
            );
            display.paint_shell_surface(surface, &pixels);
        }
    }
}
