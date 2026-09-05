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
/// Answers `--version` and `-V` before anything else starts.
///
/// It exists so a bug report can name its build. The version was
/// previously reachable only through `pacman -Qi`, which is one more
/// thing a user has to know to produce a report the crash itself
/// cannot produce for them.
///
fn print_version_and_exit_if_asked() {
    let asked = std::env::args()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V");
    if !asked {
        return;
    }
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    println!("source: {}", chonk_build_info::SOURCE_ID);
    match chonk_build_info::current_elf_build_id() {
        Ok(build_id) => println!("build id: {build_id}"),
        Err(error) => println!("build id: unavailable ({error})"),
    }
    std::process::exit(0);
}

#[cfg(target_os = "linux")]
fn main() {
    // Before the subscriber, so `--version` prints one clean line
    // rather than a line preceded by whatever RUST_LOG asked for.
    print_version_and_exit_if_asked();
    let allocator_policy_pinned = chonk_shell::startup::pin_glibc_large_allocation_policy();
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    if !allocator_policy_pinned {
        tracing::warn!("glibc rejected the fixed mmap/trim thresholds; transient buffers may raise the heap high-water mark");
    }

    // A panic anywhere in this process must become an abnormal *process
    // exit* — that is the entire signal the session watchdog
    // (`scripts/wayland-session.sh`) has for telling a crash (re-exec,
    // recover, lock) from a logout (stop). Rust's default only delivers
    // it for the main thread: a panic on any other thread unwinds that
    // thread and leaves the compositor running half-alive — the event
    // loop pumping while whatever the dead thread owned never advances
    // — which the supervisor can neither see nor fix. So: log through
    // the same stream everything else uses (the default hook prints to
    // raw stderr, which reaches the session log too, but unformatted),
    // let the default hook say where and print the backtrace, then
    // abort. SIGABRT is unambiguous to the supervisor and skips
    // running destructors inside a process already known to be wrong.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(%info, "compositor panicked — aborting so the session supervisor can recover");
        default_hook(info);
        std::process::abort();
    }));
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
