use super::*;
use wm_theme::{DecorationStyle, RasterThemeEngine};

#[test]
fn title_icon_opens_window_menu_on_release_and_dragging_away_cancels() {
    let mut manager = wm(FakeBackend::new());
    manager.set_theme_engine(Box::new(
        RasterThemeEngine::new(wm_theme::default_theme::theme_by_id("os2-warp-4").unwrap())
            .with_style(DecorationStyle::Auto)
            .unwrap(),
    ));
    let window = manager.backend_mut().create_window();
    manager
        .backend_mut()
        .set_geometry(window, Rect::new(Point::new(100, 100), Size::new(600, 300)));
    manager.dispatch(BackendEvent::MapRequest(window));
    let id = manager.client_for_window(window).unwrap();
    while manager.take_notification().is_some() {}
    let client = manager.client(id).unwrap();
    let frame = client.frame.unwrap();
    let geometry = client.geometry;
    let icon = client
        .layout
        .button_hitboxes
        .iter()
        .find(|(kind, _)| *kind == ButtonKind::Menu)
        .unwrap()
        .1;
    let click = |local, pressed| BackendEvent::PointerButton {
        surface: SurfaceRef::Frame(frame),
        local,
        button: MouseButton::Left,
        pressed,
        time_ms: 100,
        mods: Modifiers::empty(),
    };
    manager.dispatch(click(icon.pos, true));
    assert!(manager.take_notification().is_none());
    manager.dispatch(click(Point::new(250, 10), false));
    assert!(manager.take_notification().is_none());
    manager.dispatch(click(icon.pos, true));
    manager.dispatch(click(icon.pos, false));
    assert_eq!(
        manager.take_notification(),
        Some(Notification::WindowMenuRequested {
            id,
            at: geometry.pos
        })
    );
    assert!(manager.backend().close_requests.is_empty());
    assert_eq!(manager.client(id).unwrap().geometry, geometry);
}
