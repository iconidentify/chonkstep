//! Live engine changes must reflow once and rebase active pointer operations.
use super::*;

struct TallTheme;

impl ThemeEngine for TallTheme {
    fn layout(&self, request: &DecorationRequest) -> DecorationLayout {
        let mut layout = FakeTheme.layout(request);
        layout.frame_size.h += 20;
        layout.client_offset.y += 20;
        layout.titlebar_height += 20;
        layout.shaded_frame_height += 20;
        for (_, rect) in &mut layout.resize_hitboxes { rect.pos.y += 20; }
        layout
    }

    fn render(&self, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationBuffer {
        FakeTheme.render(request, layout)
    }
}

fn desktop(count: usize) -> (WindowManager<FakeBackend>, Vec<ClientId>) {
    let mut wm = wm(FakeBackend::new());
    let mut ids = Vec::new();
    for index in 0..count {
        let window = wm.backend_mut().create_window();
        wm.backend_mut().set_geometry(window, Rect::new(
            Point::new(100 + index as i32 * 25, 150 + index as i32 * 20), Size::new(300, 200)));
        wm.dispatch(BackendEvent::MapRequest(window));
        ids.push(wm.client_for_window(window).unwrap());
    }
    wm.flush_decorations();
    (wm, ids)
}

#[test]
fn live_restyle_reflows_every_frame_once_and_round_trips_content_and_hitboxes() {
    let (mut wm, ids) = desktop(4);
    wm.move_client_to_workspace(ids[1], 1);
    wm.shade(ids[2]);
    wm.miniaturize(ids[3]);
    wm.flush_decorations();
    let original: Vec<_> = ids.iter().map(|id| {
        let c = &wm.clients[*id];
        (c.geometry, c.layout.clone(), c.workspace, c.lifecycle, c.frame.unwrap())
    }).collect();
    let mapped = wm.backend().mapped_frames.clone();
    let raised = wm.backend().raised_frames.clone();
    for tall in [true, false] {
        let before = wm.backend().frame_geometry_count.clone();
        let paints = wm.backend().paint_count.clone();
        let configures = wm.backend().client_resize_count.clone();
        wm.set_theme_engine(if tall { Box::new(TallTheme) } else { Box::new(FakeTheme) });
        wm.flush_decorations();
        for (id, (geometry, layout, workspace, lifecycle, frame)) in ids.iter().zip(&original) {
            let c = &wm.clients[*id];
            assert_eq!(c.geometry, *geometry, "content position and size survive a freeform restyle");
            assert_eq!((c.workspace, c.lifecycle), (*workspace, *lifecycle));
            assert_eq!(c.layout.client_offset.y, if tall { 40 } else { 20 });
            assert_eq!(wm.backend().frame_geometry_count[frame] - before[frame], 1);
            assert_eq!(wm.backend().paint_count[frame] - paints[frame], 1);
            assert_eq!(wm.backend().client_resize_count[&c.window] - configures.get(&c.window).copied().unwrap_or(0), 1);
            assert_eq!(wm.backend().last_client_size[&c.window], geometry.size);
            if tall {
                assert_eq!(hit_test(&c.layout, Point::new(150, 30)), HitTarget::TitlebarDrag);
            } else {
                assert_eq!(&c.layout, layout);
            }
        }
        assert_eq!(wm.backend().mapped_frames, mapped);
        assert_eq!(wm.backend().raised_frames, raised);
    }
}

#[test]
fn live_restyle_reflows_spatial_clients_once_even_when_metrics_do_not_change() {
    for mode in [crate::LayoutMode::Mosaic, crate::LayoutMode::Flow] {
        let (mut wm, ids) = desktop(3);
        wm.set_workspace_layout(0, mode);
        let order = wm.layout_order(0).to_vec();
        for _ in 0..2 {
            // The second switch has identical metrics. It still must repaint
            // the replacement palette, without two configure passes.
            let before = wm.backend().frame_geometry_count.clone();
            let paints = wm.backend().paint_count.clone();
            wm.set_theme_engine(Box::new(TallTheme));
            wm.flush_decorations();
            for id in &ids {
                let c = &wm.clients[*id];
                let frame = c.frame.unwrap();
                assert_eq!(wm.backend().frame_geometry_count[&frame] - before[&frame], 1, "{mode:?}");
                assert_eq!(wm.backend().paint_count[&frame] - paints[&frame], 1, "{mode:?}");
                assert_eq!(c.layout.client_offset.y, 40);
            }
            assert_eq!(wm.layout_order(0), order);
        }
    }
}

#[test]
fn live_restyle_rebases_a_move_without_an_extra_reflow_or_pointer_jump() {
    let (mut wm, ids) = desktop(1);
    let id = ids[0];
    let frame = wm.clients[id].frame.unwrap();
    let start = client_frame_rect(&wm.clients[id]);
    wm.dispatch(frame_press(frame, Point::new(100, 8)));
    let pointer = Point::new(start.pos.x + 150, start.pos.y + 58);
    wm.handle_pointer_motion(pointer);
    wm.flush_decorations();
    let content = wm.clients[id].geometry;
    let before = wm.backend().frame_geometry_count[&frame];
    wm.set_theme_engine(Box::new(TallTheme));
    assert_eq!(wm.backend().frame_geometry_count[&frame] - before, 1);
    assert!(wm.active_move.is_some());
    wm.handle_pointer_motion(pointer);
    assert_eq!(wm.clients[id].geometry, content, "zero pointer delta must preserve content");
    wm.handle_pointer_motion(Point::new(pointer.x + 30, pointer.y + 30));
    assert_eq!(wm.clients[id].geometry.pos, Point::new(content.pos.x + 30, content.pos.y + 30));
    wm.dispatch(frame_release(frame, Point::new(150, 8)));
    assert_eq!(wm.backend().outstanding_pointer_grabs, 0);
}

#[test]
fn live_restyle_rebases_every_resize_edge_and_keeps_its_opposite_edge_fixed() {
    for edge in [ResizeEdge::North, ResizeEdge::South, ResizeEdge::East, ResizeEdge::West,
                 ResizeEdge::NorthEast, ResizeEdge::NorthWest, ResizeEdge::SouthEast, ResizeEdge::SouthWest] {
        let (mut wm, ids) = desktop(1);
        let id = ids[0];
        let window = wm.clients[id].window;
        let frame = wm.clients[id].frame.unwrap();
        let pointer = Point::new(270, 290);
        wm.handle_pointer_motion(pointer);
        wm.dispatch(BackendEvent::ResizeRequest { window, edge });
        let moved = Point::new(pointer.x + 15, pointer.y + 15);
        wm.handle_pointer_motion(moved);
        wm.flush_decorations();
        let content = wm.clients[id].geometry;
        let before = wm.backend().frame_geometry_count[&frame];
        wm.set_theme_engine(Box::new(TallTheme));
        assert_eq!(wm.backend().frame_geometry_count[&frame] - before, 1);
        assert_eq!(wm.active_resize.as_ref().unwrap().start_frame, client_frame_rect(&wm.clients[id]));
        wm.handle_pointer_motion(moved);
        assert_eq!(wm.clients[id].geometry, content, "{edge:?}: zero delta after style switch");
        let start = client_frame_rect(&wm.clients[id]);
        wm.handle_pointer_motion(Point::new(moved.x + 10, moved.y + 10));
        let after = client_frame_rect(&wm.clients[id]);
        if matches!(edge, ResizeEdge::West | ResizeEdge::NorthWest | ResizeEdge::SouthWest) {
            assert_eq!(after.pos.x + after.size.w as i32, start.pos.x + start.size.w as i32);
        }
        if matches!(edge, ResizeEdge::North | ResizeEdge::NorthWest | ResizeEdge::NorthEast) {
            assert_eq!(after.pos.y + after.size.h as i32, start.pos.y + start.size.h as i32);
        }
        wm.dispatch(frame_release(frame, Point::new(0, 0)));
        assert!(wm.active_resize.is_none());
        assert_eq!(wm.backend().outstanding_pointer_grabs, 0);
    }
}

#[test]
fn live_restyle_refits_maximized_content_once_and_preserves_restore_geometry() {
    let (mut wm, ids) = desktop(1);
    let id = ids[0];
    let original = wm.clients[id].geometry;
    let frame = wm.clients[id].frame.unwrap();
    wm.maximize(id, MaximizeDirections::FULL);
    let maximized_frame = wm.backend().last_frame_geometry[&frame];
    let before = wm.backend().frame_geometry_count[&frame];
    wm.set_theme_engine(Box::new(TallTheme));
    assert_eq!(wm.backend().frame_geometry_count[&frame] - before, 1);
    assert_eq!(wm.backend().last_frame_geometry[&frame], maximized_frame);
    assert_eq!(wm.clients[id].geometry.size.h, maximized_frame.size.h - 40);
    wm.unmaximize(id);
    assert_eq!(wm.clients[id].geometry, original);
}

#[test]
fn live_restyle_does_not_submit_geometry_for_withdrawn_clients() {
    let (mut wm, ids) = desktop(1);
    let window = wm.clients[ids[0]].window;
    wm.dispatch(BackendEvent::Unmapped(window));
    let frames = wm.backend().frame_geometry_count.clone();
    let resizes = wm.backend().client_resize_count.clone();
    wm.set_theme_engine(Box::new(TallTheme));
    assert_eq!(wm.backend().frame_geometry_count, frames);
    assert_eq!(wm.backend().client_resize_count, resizes);
}
