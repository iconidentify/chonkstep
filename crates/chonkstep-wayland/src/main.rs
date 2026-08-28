//! chonkstep as a native Wayland compositor - the same desktop as the
//! X11 binary, driven through `wm-wayland`'s Smithay backend.

#[cfg(target_os = "linux")]
fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    // Replaced by the Wayland event loop as wm-wayland lands - the
    // stub keeps the dual-binary structure buildable everywhere while
    // the compositor grows behind it.
    tracing::error!("the wm-wayland backend is still under construction; use the chonkstep (X11) binary");
    std::process::exit(1);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("chonkstep-wayland only runs on Linux");
    std::process::exit(1);
}
