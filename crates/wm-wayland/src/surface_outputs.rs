//! Output membership from the same clipped elements that reach the renderer.
//!
//! Membership describes placement, including occluded surfaces. Presentation
//! ownership continues to use Smithay's actual visible-pixel/refresh selection.
//! Captures never update this ledger. Scratch maps retain capacity between frames.

use std::collections::HashMap;

use smithay::backend::renderer::element::{Element, Id, PrimaryScanoutOutput, RenderElementState, RenderElementStates};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Rectangle, Size};
use smithay::wayland::compositor::{with_states, SurfaceData};
use smithay::wayland::dmabuf::{DmabufFeedback, SurfaceDmabufFeedbackState};
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct SurfaceOutputs {
    surfaces: HashMap<Id, Surface>,
    outputs: Vec<Membership>,
}

struct Surface {
    surface: WlSurface,
    outputs: Vec<(Output, u64)>,
    feedback: Option<DmabufFeedback>,
    visibility: Visibility,
}

struct Membership {
    output: Output,
    current: HashMap<Id, u64>,
    next: HashMap<Id, u64>,
}

impl SurfaceOutputs {
    pub fn register(&mut self, surface: &WlSurface) {
        self.surfaces.insert(
            surface.into(),
            Surface {
                surface: surface.clone(),
                outputs: Vec::new(),
                feedback: None,
                visibility: Visibility::default(),
            },
        );
    }

    pub fn destroy(&mut self, surface: &WlSurface) {
        let id = Id::from(surface);
        self.surfaces.remove(&id);
        for membership in &mut self.outputs {
            membership.current.remove(&id);
            membership.next.remove(&id);
        }
    }

    pub fn remove_output(&mut self, output: &Output) {
        for surface in self.surfaces.values_mut() {
            if surface.outputs.iter().any(|(member, _)| member == output) {
                output.leave(&surface.surface);
                surface.outputs.retain(|(member, _)| member != output);
                retire_visibility(surface, output);
                surface.feedback = None;
            }
        }
        self.outputs.retain(|membership| &membership.output != output);
    }

    pub fn update_scene<E: Element>(
        &mut self,
        output: &Output,
        size: Size<i32, Physical>,
        elements: &[E],
        default_feedback: Option<&DmabufFeedback>,
    ) {
        let index = self
            .outputs
            .iter()
            .position(|entry| &entry.output == output)
            .unwrap_or_else(|| {
                self.outputs.push(Membership {
                    output: output.clone(),
                    current: HashMap::new(),
                    next: HashMap::new(),
                });
                self.outputs.len() - 1
            });
        let membership = &mut self.outputs[index];
        membership.next.clear();
        let viewport = Rectangle::from_size(size);
        for element in elements {
            if !self.surfaces.contains_key(element.id()) {
                continue;
            }
            let area = clipped_area(element.geometry(1.0.into()), viewport);
            if area == 0 {
                continue;
            }
            // A workspace transition can contain two views of the same surface.
            // Use the largest view instead of falsely doubling its preference.
            membership
                .next
                .entry(element.id().clone())
                .and_modify(|old| *old = (*old).max(area))
                .or_insert(area);
        }
        for id in membership
            .current
            .keys()
            .filter(|id| !membership.next.contains_key(*id))
        {
            if let Some(surface) = self.surfaces.get_mut(id) {
                output.leave(&surface.surface);
                surface.outputs.retain(|(member, _)| member != output);
                retire_visibility(surface, output);
                if let Some(feedback) = default_feedback {
                    set_feedback(surface, feedback);
                } else {
                    surface.feedback = None;
                }
            }
        }
        for (id, area) in &membership.next {
            let Some(surface) = self.surfaces.get_mut(id) else {
                continue;
            };
            if let Some((_, old_area)) = surface.outputs.iter_mut().find(|(member, _)| member == output) {
                *old_area = *area;
            } else {
                output.enter(&surface.surface);
                surface.outputs.push((output.clone(), *area));
            }
        }
        std::mem::swap(&mut membership.current, &mut membership.next);
    }

    pub fn suspend_output(&mut self, output: &Output) {
        for surface in self.surfaces.values_mut() {
            retire_visibility(surface, output);
        }
    }

    pub fn update_primary(
        &mut self,
        surface: &WlSurface,
        output: &Output,
        surface_data: &SurfaceData,
        render_states: &RenderElementStates,
    ) -> Option<Output> {
        let entry = self.surfaces.get_mut(&Id::from(surface))?;
        let state = render_states
            .element_render_state(surface)
            .filter(|state| state.visible_area > 0 && entry.outputs.iter().any(|(member, _)| member == output));
        entry.visibility.update(output, state);
        publish_primary(surface_data, &entry.visibility);
        entry.visibility.primary.clone()
    }

    pub fn feedback_for(&self, surface: &WlSurface) -> Option<DmabufFeedback> {
        self.surfaces
            .get(&Id::from(surface))
            .and_then(|entry| entry.feedback.clone())
    }

    /// Runs after presentation ownership has been updated. A spanning surface
    /// hears from its primary output only; another monitor cannot overwrite its
    /// allocation preference with unrelated capabilities or cadence.
    pub fn send_feedback(
        &mut self,
        output: &Output,
        states: &RenderElementStates,
        support: &crate::dmabuf::DmabufSupport,
    ) {
        let Some(membership) = self.outputs.iter().find(|entry| &entry.output == output) else {
            return;
        };
        for id in membership.current.keys() {
            let Some(surface) = self.surfaces.get_mut(id) else {
                continue;
            };
            if surface.visibility.primary.as_ref() != Some(output) {
                continue;
            }
            let Some(feedback) = support.feedback_for(output, id, states) else {
                continue;
            };
            set_feedback(surface, feedback);
        }
    }

    /// Policy/capability changes and removal immediately withdraw old allocation
    /// hints, including for parked surfaces that will not render another frame.
    pub fn reset_feedback(&mut self, feedback: Option<&DmabufFeedback>) {
        for surface in self.surfaces.values_mut() {
            if let Some(feedback) = feedback {
                set_feedback(surface, feedback);
            } else {
                surface.feedback = None;
            }
        }
    }
}

/// Retain each output's last successfully rendered visibility. A single
/// primary-only cache cannot recover the remaining output when its owner leaves.
#[derive(Default)]
struct Visibility {
    outputs: Vec<(Output, RenderElementState)>,
    primary: Option<Output>,
}

impl Visibility {
    fn update(&mut self, output: &Output, state: Option<RenderElementState>) {
        match (self.outputs.iter().position(|(member, _)| member == output), state) {
            (Some(index), Some(state)) => self.outputs[index].1 = state,
            (None, Some(state)) => self.outputs.push((output.clone(), state)),
            (Some(index), None) => {
                self.outputs.remove(index);
            }
            (None, None) => {}
        }
        // Prefer refresh rate among outputs showing at least half the largest
        // visible area. Tiny slivers on a fast monitor must not pace a window
        // whose pixels are overwhelmingly on another monitor.
        let threshold = self
            .outputs
            .iter()
            .map(|(_, state)| state.visible_area)
            .max()
            .unwrap_or(0)
            .div_ceil(2);
        self.primary = self
            .outputs
            .iter()
            .filter(|(_, state)| state.visible_area >= threshold)
            .max_by_key(|(output, state)| (output.current_mode().map_or(0, |mode| mode.refresh), state.visible_area))
            .map(|(output, _)| output.clone());
    }
}

fn publish_primary(states: &SurfaceData, visibility: &Visibility) {
    states
        .data_map
        .insert_if_missing_threadsafe(Mutex::<PrimaryScanoutOutput>::default);
    let selection = visibility
        .primary
        .as_ref()
        .and_then(|primary| visibility.outputs.iter().find(|(output, _)| output == primary))
        .map(|(output, state)| (output, *state));
    states
        .data_map
        .get::<Mutex<PrimaryScanoutOutput>>()
        .unwrap()
        .lock()
        .unwrap()
        .set_current_output(selection);
}

fn retire_visibility(surface: &mut Surface, output: &Output) {
    if !surface.visibility.outputs.iter().any(|(member, _)| member == output) {
        return;
    }
    surface.visibility.update(output, None);
    with_states(&surface.surface, |states| publish_primary(states, &surface.visibility));
}

fn set_feedback(surface: &mut Surface, feedback: &DmabufFeedback) {
    if surface.feedback.as_ref() == Some(feedback) {
        return;
    }
    with_states(&surface.surface, |states| {
        if let Some(state) = SurfaceDmabufFeedbackState::from_states(states) {
            state.set_feedback(feedback);
        }
    });
    surface.feedback = Some(feedback.clone());
}

fn clipped_area(rect: Rectangle<i32, Physical>, viewport: Rectangle<i32, Physical>) -> u64 {
    rect.intersection(viewport)
        .map_or(0, |rect| rect.size.w.max(0) as u64 * rect.size.h.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_uses_real_visibility_recovers_without_redraw_and_ignores_fast_slivers() {
        use smithay::backend::renderer::element::RenderElementPresentationState;
        let make = |name: &str, refresh| {
            let output = Output::new(
                name.to_string(),
                smithay::output::PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: smithay::output::Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                },
            );
            output.change_current_state(
                Some(smithay::output::Mode {
                    size: (100, 100).into(),
                    refresh,
                }),
                None,
                None,
                None,
            );
            output
        };
        let slow = make("60Hz", 60_000);
        let fast = make("120Hz", 120_000);
        let visible = |area| {
            Some(RenderElementState {
                visible_area: area,
                presentation_state: RenderElementPresentationState::ZeroCopy,
            })
        };
        let mut visibility = Visibility::default();
        visibility.update(&slow, visible(1000));
        visibility.update(&fast, visible(1));
        assert_eq!(visibility.primary, Some(slow.clone()));
        visibility.update(&fast, visible(600));
        assert_eq!(visibility.primary, Some(fast.clone()));
        // Power-off, hot-unplug, hiding and clipping all retire the same
        // rendered state. The other output needs no new frame to win.
        visibility.update(&fast, None);
        assert_eq!(visibility.primary, Some(slow.clone()));
        visibility.update(&slow, None);
        assert_eq!(visibility.primary, None);
        visibility.update(&fast, visible(50));
        assert_eq!(visibility.primary, Some(fast));
    }

    struct Server;
    impl smithay::reexports::wayland_server::Dispatch<WlSurface, ()> for Server {
        fn request(
            _: &mut Self,
            _: &smithay::reexports::wayland_server::Client,
            _: &WlSurface,
            _: <WlSurface as smithay::reexports::wayland_server::Resource>::Request,
            _: &(),
            _: &smithay::reexports::wayland_server::DisplayHandle,
            _: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
        ) {
        }
    }

    #[test]
    fn independent_subsurface_membership_moves_hides_and_survives_output_removal() {
        use smithay::backend::renderer::{
            element::{solid::SolidColorRenderElement, Kind},
            utils::CommitCounter,
            Color32F,
        };
        use smithay::output::PhysicalProperties;
        use smithay::reexports::wayland_server::Display;
        let display = Display::<Server>::new().unwrap();
        let mut handle = display.handle();
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let client = handle.insert_client(socket, std::sync::Arc::new(())).unwrap();
        let parent = client.create_resource::<WlSurface, (), Server>(&handle, 4, ()).unwrap();
        let child = client.create_resource::<WlSurface, (), Server>(&handle, 4, ()).unwrap();
        let make_output = |name: &str| {
            Output::new(
                name.to_string(),
                PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: smithay::output::Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                },
            )
        };
        let left = make_output("left");
        let right = make_output("right");
        let mut tracker = SurfaceOutputs::default();
        tracker.register(&parent);
        tracker.register(&child);
        let draw = |surface: &WlSurface, x: i32, width: i32| {
            SolidColorRenderElement::new(
                Id::from(surface),
                Rectangle::new((x, 0).into(), (width, 80).into()),
                CommitCounter::default(),
                Color32F::BLACK,
                Kind::Unspecified,
            )
        };
        let size = (100, 100).into();
        tracker.update_scene(&left, size, &[draw(&parent, 20, 90), draw(&child, 105, 20)], None);
        tracker.update_scene(&right, size, &[draw(&parent, -80, 90), draw(&child, 5, 20)], None);
        assert_eq!(
            tracker.surfaces[&Id::from(&parent)].outputs,
            vec![(left.clone(), 6400), (right.clone(), 800)]
        );
        assert_eq!(tracker.surfaces[&Id::from(&child)].outputs, vec![(right.clone(), 1600)]);
        // The parent's rectangle and the child's rectangle are independent.
        tracker.update_scene(&left, size, &[] as &[SolidColorRenderElement], None);
        assert_eq!(tracker.surfaces[&Id::from(&parent)].outputs, vec![(right.clone(), 800)]);
        tracker.remove_output(&right);
        assert!(tracker.surfaces.values().all(|surface| surface.outputs.is_empty()));
        tracker.update_scene(&left, size, &[draw(&parent, 5, 50)], None);
        assert_eq!(tracker.surfaces[&Id::from(&parent)].outputs, vec![(left, 4000)]);
        tracker.destroy(&parent);
        assert!(!tracker.surfaces.contains_key(&Id::from(&parent)));
        assert!(tracker
            .outputs
            .iter()
            .all(|output| !output.current.contains_key(&Id::from(&parent))));
    }
    #[test]
    fn membership_uses_clipped_pixels_and_excludes_touching_edges() {
        let output = Rectangle::from_size((5120, 2880).into());
        assert_eq!(
            clipped_area(Rectangle::new((-100, 20).into(), (150, 80).into()), output),
            4000
        );
        assert_eq!(
            clipped_area(Rectangle::new((5120, 0).into(), (100, 100).into()), output),
            0
        );
        assert_eq!(
            clipped_area(Rectangle::new((5100, 2860).into(), (100, 100).into()), output),
            400
        );
        assert_eq!(clipped_area(output, output), 14_745_600);
    }
}
