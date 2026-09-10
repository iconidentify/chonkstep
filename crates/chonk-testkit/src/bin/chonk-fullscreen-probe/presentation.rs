//! Opt-in, client-observed presentation timestamps for native GPU benchmarks.
//! Feedback is booked before the buffer commit (including EGL's swap), never
//! inferred from frame callbacks or the benchmark process's wake-up time.

use super::{say, Probe};
use wayland_client::{protocol::wl_surface::WlSurface, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};

pub(super) fn request(probe: &Probe, surface: &WlSurface, qh: &QueueHandle<Probe>) {
    if let Some(presentation) = &probe.presentation {
        presentation.feedback(surface, qh, ());
    }
}

impl Dispatch<WpPresentation, ()> for Probe {
    fn event(_: &mut Self, _: &WpPresentation, event: wp_presentation::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            say(&format!("presentation clock_id={clk_id}"));
        }
    }
}

impl Dispatch<WpPresentationFeedback, ()> for Probe {
    fn event(_: &mut Self, _: &WpPresentationFeedback, event: wp_presentation_feedback::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi, tv_sec_lo, tv_nsec, refresh, seq_hi, seq_lo, flags,
            } => {
                let seconds = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                let sequence = (u64::from(seq_hi) << 32) | u64::from(seq_lo);
                let flags = match flags { WEnum::Value(flags) => flags.bits(), WEnum::Unknown(bits) => bits };
                say(&format!("presentation presented seconds={seconds} nanoseconds={tv_nsec} refresh={refresh} sequence={sequence} flags={flags}"));
            }
            wp_presentation_feedback::Event::Discarded => say("presentation discarded"),
            _ => {}
        }
    }
}
