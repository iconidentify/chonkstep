//! The Wayland chonkstep binary: process bootstrap for the compositor
//! in `wm-wayland`, and deliberately nothing more. The X11 binary owns
//! a real event loop because an X client can poll a socket it does not
//! own — but a Wayland compositor *is* the display server, its loop
//! belongs to calloop and Smithay, and it cannot be driven from out
//! here. So everything `chonkstep`'s `main.rs` does inline — keymap
//! interception, motion coalescing, the shell click/resize drains,
//! `Shell::tick` — lives inside `wm_wayland::run`'s dispatch loop
//! instead, mirrored step for step from that file, and this binary is
//! left with exactly the irreducibly process-side jobs: logging setup,
//! configuration loading, and the exit code.

#[cfg(target_os = "linux")]
fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "chonkstep-wayland starting \u{2014} the chonkstep desktop as a native Wayland compositor"
    );

    // Same contract as the X11 binary: `wm_config::load()` never fails.
    // No file yields the defaults, and a broken file logs what is wrong
    // and yields the defaults too — a typo in the config must never
    // cost the user their session, because with the compositor refusing
    // to start there is no terminal to fix the typo from.
    let config = wm_config::load();

    // `run` owns the entire session — backend and renderer init, the
    // Wayland globals, XWayland, the `WindowManager` + `Shell` wiring,
    // and the dispatch loop — and returns only when the session is
    // over: `Ok` for a deliberate exit (the root menu's Exit), `Err`
    // for anything that prevented the session from starting or ended
    // it abnormally. The error is logged rather than propagated as a
    // panic/`Result` from `main` so it lands in the same tracing
    // stream a session launcher captures, not on a raw stderr line
    // formatted differently from every other message.
    if let Err(error) = wm_wayland::run(config) {
        tracing::error!(%error, "compositor exited with an error");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // The crate still *builds* everywhere — `wm-wayland` is cfg-empty
    // off Linux and this crate's dependencies are target-gated — so
    // `cargo test --workspace` on a macOS dev host keeps gating the
    // whole tree. Running, though, is Linux-only by nature, and saying
    // so beats a linker error.
    eprintln!("chonkstep-wayland only runs on Linux");
    std::process::exit(1);
}
