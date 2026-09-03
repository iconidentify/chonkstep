//! The gamma-control probe: a night-light daemon reduced to the four
//! things a test needs to watch it do.
//!
//! `wlsunset` and `gammastep` are the real clients of
//! `zwlr_gamma_control_unstable_v1`, and running one of them is the
//! acceptance test that matters. What they are not is *scriptable*:
//! they compute a temperature from the wall clock and a latitude, they
//! decide for themselves when to talk, and they say nothing a test can
//! poll. This probe does the same protocol in the same order and
//! reports each step on stdout, so the two behaviors that protect a
//! user's screen can be asserted rather than eyeballed:
//!
//! - **Exclusivity.** Only one client at a time may hold an output.
//!   `chonk-gamma-probe exclusive` claims the same output twice on one
//!   connection and prints what each claim was answered with; the
//!   second must be `failed`. Without that, two night-light daemons
//!   fight over one screen and each undoes the other every few seconds.
//! - **The restore.** `chonk-gamma-probe hold <kelvin>` claims, warms
//!   the screen, prints `**holding**` and then waits to be killed —
//!   which is what a crashing daemon does, minus the crash. The
//!   compositor must put the original ramp back; a session that does
//!   not leaves the user with an orange display and nothing running to
//!   explain it.
//!
//! It also answers the honest-absence case. Against the nested winit
//! backend there is no crtc to program, so chonkstep advertises no
//! global at all (see `wm-wayland/src/gamma.rs`), and the probe prints
//! `**no gamma-control global**` — the same thing `wlsunset` reports,
//! in a form a test can assert on.
//!
//! Usage:
//!
//! ```text
//! chonk-gamma-probe report              # what the compositor offers
//! chonk-gamma-probe set <kelvin>        # claim, set one ramp, exit
//! chonk-gamma-probe exclusive           # two claims, one output
//! chonk-gamma-probe hold <kelvin>       # claim, set, wait to be killed
//! chonk-gamma-probe bad-table <bytes>   # a deliberately wrong table
//! ```
//!
//! Every mode takes an optional trailing output index (default 0).
//! Checkpoints a test polls for are printed in `**bold**`, the same
//! convention `chonk-lock-probe` uses.

use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::protocol::{wl_output::WlOutput, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::gamma_control::v1::client::{
    zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1,
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

fn fatal(message: &str) -> ! {
    eprintln!("chonk-gamma-probe: {message}");
    std::process::exit(1);
}

/// What one `zwlr_gamma_control_v1` has been told so far. The protocol
/// answers a claim with exactly one of these two events, so a claim
/// whose slot is still `Pending` after a roundtrip is a compositor that
/// answered neither — itself worth reporting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Answer {
    Pending,
    /// `gamma_size` arrived: the claim was granted, at this ramp length.
    Size(u32),
    /// `failed` arrived: refused, or revoked.
    Failed,
}

#[derive(Default)]
struct Probe {
    manager: Option<ZwlrGammaControlManagerV1>,
    outputs: Vec<WlOutput>,
    /// One slot per control created, in creation order — which is the
    /// order the modes below reason about ("the first claim", "the
    /// second claim").
    answers: Vec<Answer>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        probe: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_output" => probe.outputs.push(registry.bind(name, version.min(4), qh, ())),
                "zwlr_gamma_control_manager_v1" => {
                    probe.manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrGammaControlV1, usize> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        slot: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let answer = match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => Answer::Size(size),
            zwlr_gamma_control_v1::Event::Failed => Answer::Failed,
            _ => return,
        };
        if let Some(existing) = probe.answers.get_mut(*slot) {
            // `failed` can arrive later, revoking a granted claim; the
            // newest answer is the true one.
            *existing = answer;
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Probe {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(),
                     _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlOutput, ZwlrGammaControlManagerV1);

/// The per-channel white point of a blackbody at `kelvin`, as every
/// night-light tool computes it (Tanner Helland's fit of the CIE
/// tables, the same approximation `wlsunset` and `redshift` use).
/// Warmer than 6500K means less blue; the probe does not need to be
/// colorimetrically right, it needs to produce the same *shape* of
/// table a real tool sends.
fn white_point(kelvin: f64) -> [f64; 3] {
    let t = (kelvin / 100.0).clamp(10.0, 400.0);
    let clamp = |v: f64| (v / 255.0).clamp(0.0, 1.0);
    let red = if t <= 66.0 { 255.0 } else { 329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2) };
    let green = if t <= 66.0 {
        99.470_802_586 * t.ln() - 161.119_568_166
    } else {
        288.122_169_528 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223 * (t - 10.0).ln() - 305.044_792_730
    };
    [clamp(red), clamp(green), clamp(blue)]
}

/// The gamma table for `kelvin` at this output's ramp size: three
/// consecutive ramps of `size` little-endian `u16`s, red then green
/// then blue. Exactly the layout the protocol specifies, built by hand
/// so a wrong one here would be visible rather than hidden in a
/// library.
fn table(size: u32, kelvin: f64) -> Vec<u8> {
    let white = white_point(kelvin);
    let size = size as usize;
    let mut bytes = Vec::with_capacity(size * 6);
    for channel in white {
        for entry in 0..size {
            let level = if size <= 1 { 1.0 } else { entry as f64 / (size - 1) as f64 };
            let value = (level * channel * f64::from(u16::MAX)).round() as u16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

/// A seekable, unlinked scratch file holding `bytes`, for the fd the
/// protocol wants. Seekable matters: the compositor reads the table
/// with `pread` so a client cannot hang it with a pipe, and a probe
/// that handed over a pipe would be testing that refusal instead of
/// the happy path.
fn table_file(bytes: &[u8]) -> std::fs::File {
    let path = format!(
        "{}/chonk-gamma-probe-{}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/dev/shm".into()),
        std::process::id()
    );
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap_or_else(|e| fatal(&format!("cannot create the table file: {e}")));
    let _ = std::fs::remove_file(&path);
    file.write_all(bytes).unwrap_or_else(|e| fatal(&format!("cannot fill the table file: {e}")));
    file.flush().unwrap_or_else(|e| fatal(&format!("cannot flush the table file: {e}")));
    file
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("report");
    let number = |index: usize, fallback: f64| -> f64 {
        args.get(index).and_then(|value| value.parse().ok()).unwrap_or(fallback)
    };
    // The trailing output index, wherever it lands for this mode.
    let output_index = match mode {
        "set" | "hold" | "bad-table" => number(2, 0.0) as usize,
        _ => number(1, 0.0) as usize,
    };

    let connection = Connection::connect_to_env()
        .unwrap_or_else(|e| fatal(&format!("cannot reach the compositor: {e}")));
    let display = connection.display();
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    display.get_registry(&qh, ());
    let mut probe = Probe::default();
    queue
        .roundtrip(&mut probe)
        .unwrap_or_else(|e| fatal(&format!("registry roundtrip failed: {e}")));

    let Some(manager) = probe.manager.clone() else {
        // The nested backend's documented answer, and what `wlsunset`
        // reports as "compositor doesn't support
        // wlr-gamma-control-unstable-v1".
        println!("**no gamma-control global**");
        return;
    };
    println!("manager present, {} output(s)", probe.outputs.len());
    let Some(output) = probe.outputs.get(output_index).cloned() else {
        fatal(&format!("no output at index {output_index}"));
    };

    // Every mode starts with one claim, because every mode is about
    // what happens to a claim.
    let first = manager.get_gamma_control(&output, &qh, 0usize);
    probe.answers.push(Answer::Pending);
    queue
        .roundtrip(&mut probe)
        .unwrap_or_else(|e| fatal(&format!("claim roundtrip failed: {e}")));
    let size = match probe.answers[0] {
        Answer::Size(size) => {
            println!("**gamma_size {size}**");
            size
        }
        Answer::Failed => {
            println!("**claim failed**");
            return;
        }
        Answer::Pending => {
            println!("**claim unanswered**");
            return;
        }
    };

    match mode {
        "report" => {}
        "exclusive" => {
            // The second claim on the same output, from the same
            // connection — the compositor cannot tell it apart from a
            // second daemon, which is the point.
            let second = manager.get_gamma_control(&output, &qh, 1usize);
            probe.answers.push(Answer::Pending);
            queue
                .roundtrip(&mut probe)
                .unwrap_or_else(|e| fatal(&format!("second claim roundtrip failed: {e}")));
            match probe.answers[1] {
                Answer::Failed => println!("**second claim refused**"),
                Answer::Size(size) => println!("**second claim GRANTED at {size} — exclusivity is broken**"),
                Answer::Pending => println!("**second claim unanswered**"),
            }
            second.destroy();
        }
        "set" | "hold" => {
            let kelvin = number(1, 3000.0);
            let file = table_file(&table(size, kelvin));
            first.set_gamma(file.as_fd());
            queue
                .roundtrip(&mut probe)
                .unwrap_or_else(|e| fatal(&format!("set_gamma was refused: {e}")));
            if probe.answers[0] == Answer::Failed {
                println!("**set_gamma failed**");
                return;
            }
            println!("**set_gamma accepted at {kelvin}K**");
            if mode == "hold" {
                // A night-light daemon's steady state. The test kills
                // this process from here and watches the compositor put
                // the original ramp back.
                println!("**holding**");
                let _ = std::io::stdout().flush();
                loop {
                    if queue.blocking_dispatch(&mut probe).is_err() {
                        return;
                    }
                }
            }
            // Dropping the connection here is the graceful half of the
            // same restore: the control is destroyed, and the screen
            // must go back.
            println!("**releasing**");
        }
        "bad-table" => {
            // Hostile input over the wire, to prove the compositor
            // answers a wrong-sized table with the protocol's
            // `invalid_gamma` error rather than reading off the end of
            // it. `bytes` defaults to one byte short of correct.
            let bytes = args
                .get(1)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or((size as usize * 6).saturating_sub(1));
            let file = table_file(&vec![0u8; bytes]);
            first.set_gamma(file.as_fd());
            match queue.roundtrip(&mut probe) {
                Ok(_) => println!("**bad table of {bytes} bytes ACCEPTED**"),
                Err(error) => println!("**bad table of {bytes} bytes refused: {error}**"),
            }
            return;
        }
        other => fatal(&format!("unknown mode {other}")),
    }

    let _ = std::io::stdout().flush();
}
