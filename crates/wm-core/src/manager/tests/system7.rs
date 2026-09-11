use super::*;
use wm_theme::{DecorationStyle, FontState, RasterThemeEngine};

fn system7() -> RasterThemeEngine {
    RasterThemeEngine::with_fonts(wm_theme::default_theme::nextstep_classic(), FontState::new())
        .with_style(DecorationStyle::System7).unwrap()
}

fn desktop(count: usize) -> (WindowManager<FakeBackend>, Vec<ClientId>) {
    let mut manager = wm(FakeBackend::new());
    manager.set_theme_engine(Box::new(system7()));
    let ids = (0..count).map(|_| {
        let window = manager.backend_mut().create_window();
        manager.backend_mut().set_geometry(window, Rect::new(Point::new(140, 140), Size::new(300, 200)));
        manager.dispatch(BackendEvent::MapRequest(window));
        manager.client_for_window(window).unwrap()
    }).collect();
    (manager, ids)
}

#[test]
fn system7_metrics_and_every_resize_edge_match_the_visible_frame() {
    let engine = system7();
    for scale in [1u32, 2] {
        let request = DecorationRequest { title: "Terminal".into(), content_size: Size::new(400 * scale, 240 * scale),
            resizable: true, focused: true, buttons: Vec::new() };
        let layout = engine.layout_at(&request, scale as f32);
        assert_eq!(layout.frame_size, Size::new(411 * scale, 269 * scale));
        assert_eq!(layout.client_offset, Point::new(5 * scale as i32, 23 * scale as i32));
        assert_eq!(layout.shaded_frame_height, 28 * scale);
        assert_eq!(layout.titlebar_height, 18 * scale);
        assert_eq!(layout.button_hitboxes.len(), 2);
        for (kind, x) in [(ButtonKind::Close, 13), (ButtonKind::Maximize, 386)] {
            let rect = layout.button_hitboxes.iter().find(|(k, _)| *k == kind).unwrap().1;
            assert_eq!(rect, Rect::new(Point::new(x * scale as i32, 8 * scale as i32), Size::new(11 * scale, 11 * scale)));
            assert_eq!(hit_test(&layout, rect.pos), HitTarget::Button(kind));
        }
        let edge = |x, y| hit_test(&layout, Point::new(x * scale as i32, y * scale as i32));
        assert_eq!(edge(100, 10), HitTarget::TitlebarDrag);
        for (x, y, expected) in [(100, 1, ResizeEdge::North), (100, 268, ResizeEdge::South),
            (1, 100, ResizeEdge::West), (410, 100, ResizeEdge::East),
            (1, 1, ResizeEdge::NorthWest), (410, 1, ResizeEdge::NorthEast),
            (1, 268, ResizeEdge::SouthWest), (410, 268, ResizeEdge::SouthEast),
            (406, 100, ResizeEdge::East), (100, 264, ResizeEdge::South)] {
            assert_eq!(edge(x, y), HitTarget::ResizeEdge(expected));
        }
        assert_eq!(edge(5, 50), HitTarget::ClientArea, "ring must not steal client pixels");
        let fixed = engine.layout_at(&DecorationRequest { resizable: false, ..request }, scale as f32);
        assert_eq!(fixed.button_hitboxes.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(), [ButtonKind::Close]);
        assert!(fixed.resize_hitboxes.is_empty());
    }
}

#[test]
fn system7_maximize_and_mosaic_fit_visible_bounds_and_restore_exactly() {
    let (mut manager, ids) = desktop(2);
    let original = manager.clients[ids[0]].geometry;
    manager.set_workareas(vec![Rect::new(Point::new(0, 40), Size::new(800, 560))]);
    manager.maximize(ids[0], MaximizeDirections::FULL);
    assert_eq!(manager.clients[ids[0]].visual_geometry(), Rect::new(Point::new(0, 40), Size::new(800, 560)));
    assert_eq!(client_input_rect(&manager.clients[ids[0]]).pos, Point::new(-4, 36));
    manager.unmaximize(ids[0]);
    assert_eq!(manager.clients[ids[0]].geometry, original);
    manager.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    let a = manager.clients[ids[0]].visual_geometry();
    let b = manager.clients[ids[1]].visual_geometry();
    assert!(a.intersection(b).is_none(), "visual frames must not overlap");
    assert!(a.pos.x >= 0 && b.pos.x >= 0 && a.pos.y >= 40 && b.pos.y >= 40);
    assert!(a.pos.x as u32 + a.size.w <= 800 && b.pos.x as u32 + b.size.w <= 800);
}

#[test]
fn system7_snap_and_active_restyle_use_distinct_visual_and_input_bounds() {
    let (mut manager, ids) = desktop(1);
    let id = ids[0];
    let frame = manager.clients[id].frame.unwrap();
    manager.dispatch(frame_press(frame, Point::new(100, 10)));
    manager.handle_pointer_motion(Point::new(98, 170));
    assert_eq!(manager.clients[id].visual_geometry().pos.x, 0, "snap outline to screen, leaving input margin outside");
    assert_eq!(client_input_rect(&manager.clients[id]).pos.x, -4);
    manager.handle_pointer_motion(Point::new(270, 220));
    let before = manager.clients[id].geometry;
    manager.set_theme_engine(Box::new(FakeTheme));
    manager.handle_pointer_motion(Point::new(270, 220));
    assert_eq!(manager.clients[id].geometry, before, "restyle during a move cannot jump");
    manager.set_theme_engine(Box::new(system7()));
    manager.handle_pointer_motion(Point::new(270, 220));
    assert_eq!(manager.clients[id].geometry, before);
    manager.dispatch(frame_release(frame, Point::new(100, 10)));
    manager.shade(id);
    assert_eq!(manager.clients[id].visual_geometry().size.h, 20);
    assert_eq!(manager.backend().last_frame_geometry[&frame].size.h, 28);
    manager.unshade(id);
    assert_eq!(manager.clients[id].geometry, before);
}

#[test]
fn terminal_size_hint_updates_do_not_cancel_a_freeform_ring_resize() {
    let (mut manager, ids) = desktop(1);
    let id = ids[0];
    let client = &manager.clients[id];
    let (window, frame, before) = (client.window, client.frame.unwrap(), client.geometry);
    let input = client_input_rect(client);
    let local = Point::new(input.size.w as i32 - 3, 100);
    let pointer = Point::new(input.pos.x + local.x, input.pos.y + local.y);
    manager.dispatch(frame_press(frame, local));
    manager.handle_pointer_motion(pointer);
    assert_eq!(manager.clients[id].geometry, before, "pressing within a wide resize band must not shrink it");
    for delta in [10, 20, 30] {
        manager.handle_pointer_motion(Point::new(pointer.x + delta, pointer.y));
        manager.dispatch(BackendEvent::SizeHintsChanged(window));
        assert!(manager.active_resize.is_some(), "a terminal can update hints after every configure");
        assert_eq!(manager.clients[id].geometry.size.w, before.size.w + delta as u32);
    }
    manager.dispatch(frame_release(frame, local));
    assert!(!manager.interactive_drag_active());
    assert_eq!(manager.backend().outstanding_pointer_grabs, 0);
}

#[test]
fn mixed_dpi_move_rebases_the_grab_after_chrome_metrics_change() {
    let (mut manager, ids) = desktop(1);
    manager.backend_mut().set_monitors((0..2).map(|index| MonitorInfo {
        geometry: Rect::new(Point::new(index * 800, 0), Size::new(800, 600)),
        name: format!("screen-{index}"), identity: None, primary: index == 0,
    }).collect());
    manager.backend_mut().set_monitor_scales(vec![1.0, 2.0]);
    let id = ids[0];
    let frame = manager.clients[id].frame.unwrap();
    manager.dispatch(frame_press(frame, Point::new(100, 10)));
    manager.handle_pointer_motion(Point::new(1100, 200));
    assert_eq!(manager.clients[id].layout.input_margin, 8);
    let after = manager.clients[id].geometry;
    manager.handle_pointer_motion(Point::new(1100, 200));
    assert_eq!(manager.clients[id].geometry, after, "zero delta after crossing cannot jump");
    manager.handle_pointer_motion(Point::new(1110, 210));
    assert_eq!(manager.clients[id].geometry.pos, Point::new(after.pos.x + 10, after.pos.y + 10));
    manager.dispatch(frame_release(frame, Point::new(100, 10)));
}

#[test]
fn switching_real_system7_chrome_during_every_resize_preserves_content_and_anchor() {
    for edge in [ResizeEdge::North, ResizeEdge::South, ResizeEdge::East, ResizeEdge::West,
        ResizeEdge::NorthEast, ResizeEdge::NorthWest, ResizeEdge::SouthEast, ResizeEdge::SouthWest] {
        let (mut manager, ids) = desktop(1);
        let id = ids[0];
        let window = manager.clients[id].window;
        let frame = manager.clients[id].frame.unwrap();
        manager.handle_pointer_motion(Point::new(300, 300));
        manager.dispatch(BackendEvent::ResizeRequest { window, edge });
        let mut pointer = Point::new(310, 310);
        manager.handle_pointer_motion(pointer);
        for next in [false, true] {
            let content = manager.clients[id].geometry;
            manager.set_theme_engine(if next { Box::new(system7()) } else { Box::new(FakeTheme) });
            manager.handle_pointer_motion(pointer);
            assert_eq!(manager.clients[id].geometry, content, "{edge:?}: style switch introduced a zero-delta jump");
            let before = client_input_rect(&manager.clients[id]);
            pointer = Point::new(pointer.x + 10, pointer.y + 10);
            manager.handle_pointer_motion(pointer);
            let after = client_input_rect(&manager.clients[id]);
            if matches!(edge, ResizeEdge::West | ResizeEdge::NorthWest | ResizeEdge::SouthWest) {
                assert_eq!(before.pos.x + before.size.w as i32, after.pos.x + after.size.w as i32);
            }
            if matches!(edge, ResizeEdge::North | ResizeEdge::NorthWest | ResizeEdge::NorthEast) {
                assert_eq!(before.pos.y + before.size.h as i32, after.pos.y + after.size.h as i32);
            }
        }
        manager.dispatch(frame_release(frame, Point::new(0, 0)));
        assert_eq!(manager.backend().outstanding_pointer_grabs, 0);
    }
}
