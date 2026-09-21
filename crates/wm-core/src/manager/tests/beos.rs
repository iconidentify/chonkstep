use super::*;
use wm_theme::{DecorationStyle, RasterThemeEngine};

#[test]
fn changing_a_tab_title_updates_hitboxes_without_moving_or_configuring_content() {
    let mut manager = wm(FakeBackend::new());
    manager.set_theme_engine(Box::new(
        RasterThemeEngine::new(wm_theme::default_theme::theme_by_id("beos").unwrap())
            .with_style(DecorationStyle::Auto)
            .unwrap(),
    ));
    let window = manager.backend_mut().create_window();
    manager.backend_mut().set_title(window, "A");
    manager
        .backend_mut()
        .set_geometry(window, Rect::new(Point::new(100, 100), Size::new(600, 300)));
    manager.dispatch(BackendEvent::MapRequest(window));
    let id = manager.client_for_window(window).unwrap();
    let before = manager.clients[id].geometry;
    let resize_count = manager.backend().client_resize_count.clone();
    let first = manager.clients[id].layout.clone();
    manager
        .backend_mut()
        .set_title(window, "A much longer terminal title");
    manager.dispatch(BackendEvent::TitleChanged(window));
    let next = &manager.clients[id].layout;
    assert_eq!(manager.clients[id].geometry, before);
    assert_eq!(manager.backend().client_resize_count, resize_count);
    assert_eq!(first.frame_size, next.frame_size);
    assert!(next.input_exclusion.unwrap().pos.x > first.input_exclusion.unwrap().pos.x);
    let zoom = next
        .button_hitboxes
        .iter()
        .find(|(kind, _)| *kind == ButtonKind::Maximize)
        .unwrap()
        .1;
    assert_eq!(
        hit_test(next, zoom.pos),
        HitTarget::Button(ButtonKind::Maximize)
    );
    assert_eq!(
        hit_test(next, next.input_exclusion.unwrap().pos),
        HitTarget::ClientArea
    );
    manager.shade(id);
    manager.backend_mut().set_title(window, "B");
    manager.dispatch(BackendEvent::TitleChanged(window));
    assert_eq!(
        manager.backend().last_frame_geometry[&manager.clients[id].frame.unwrap()]
            .size
            .h,
        manager.clients[id].layout.shaded_frame_height
    );
    manager.unshade(id);
    assert_eq!(manager.clients[id].geometry, before);
}
