//! Live diagnostic controls shared by the control socket and hot paths.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Once, OnceLock};

static INIT: Once = Once::new();
static DAMAGE_LOG: AtomicBool = AtomicBool::new(false);
static IDLE_LOG: AtomicBool = AtomicBool::new(false);
static FULL_DAMAGE: AtomicBool = AtomicBool::new(false);
static NO_DIRECT_SCANOUT: AtomicBool = AtomicBool::new(false);
static NO_CURSOR_PLANE: AtomicBool = AtomicBool::new(false);

type ReloadFilter = dyn Fn(&str) -> Result<(), String> + Send + Sync;
static LOG_FILTER: OnceLock<Box<ReloadFilter>> = OnceLock::new();

fn env_enabled(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| value != "0")
}

pub(crate) fn init() {
    INIT.call_once(|| {
        DAMAGE_LOG.store(env_enabled("CHONKSTEP_DAMAGE_LOG"), Ordering::Relaxed);
        IDLE_LOG.store(env_enabled("CHONKSTEP_IDLE_LOG"), Ordering::Relaxed);
        FULL_DAMAGE.store(env_enabled("CHONKSTEP_FULL_DAMAGE"), Ordering::Relaxed);
        NO_DIRECT_SCANOUT.store(env_enabled("CHONKSTEP_NO_DIRECT_SCANOUT"), Ordering::Relaxed);
        NO_CURSOR_PLANE.store(env_enabled("CHONKSTEP_NO_CURSOR_PLANE"), Ordering::Relaxed);
    });
}

fn value(name: &str) -> Option<&'static AtomicBool> {
    init();
    match name {
        "damage-log" => Some(&DAMAGE_LOG),
        "idle-log" => Some(&IDLE_LOG),
        "full-damage" => Some(&FULL_DAMAGE),
        "no-direct-scanout" => Some(&NO_DIRECT_SCANOUT),
        "no-cursor-plane" => Some(&NO_CURSOR_PLANE),
        _ => None,
    }
}

pub(crate) fn enabled(name: &str) -> bool {
    value(name).is_some_and(|value| value.load(Ordering::Relaxed))
}

pub(crate) fn set(name: &str, enabled: bool) -> Result<(), String> {
    let value = value(name).ok_or_else(|| {
        format!(
            "unknown diagnostic {name}; expected damage-log, idle-log, full-damage, no-direct-scanout or no-cursor-plane"
        )
    })?;
    value.store(enabled, Ordering::Relaxed);
    tracing::info!(diagnostic = name, enabled, "live diagnostic changed");
    Ok(())
}

pub(crate) fn describe() -> String {
    [
        "damage-log",
        "idle-log",
        "full-damage",
        "no-direct-scanout",
        "no-cursor-plane",
    ]
    .into_iter()
    .map(|name| format!("{name}={}", enabled(name)))
    .collect::<Vec<_>>()
    .join(" ")
}

pub fn install_log_filter_reloader(
    reload: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
) -> Result<(), &'static str> {
    LOG_FILTER.set(Box::new(reload)).map_err(|_| "a log-filter reloader is already installed")
}

pub(crate) fn set_log_filter(directive: &str) -> Result<(), String> {
    let reload = LOG_FILTER.get().ok_or_else(|| "this binary did not install a reloadable tracing filter".to_string())?;
    reload(directive)?;
    tracing::info!(filter = directive, "live tracing filter changed");
    Ok(())
}
