//! Launches apps from the root menu as detached child processes.

use std::process::{Command, Stdio};

/// Returns the spawned child's PID on success — lets a caller correlate
/// a specific launch with whichever window later reports that same PID
/// via `_NET_WM_PID` (see `Backend::window_pid`), rather than matching
/// on something as loose as "any window of the expected class."
pub fn spawn_detached(program: &str, args: &[&str]) -> Option<u32> {
    spawn_detached_with_env(program, args, &[])
}

/// Same as [`spawn_detached`], with extra environment variables set on
/// top of whatever chonkstep's own process already has (which the child
/// inherits regardless — `Command` doesn't clear the parent environment
/// unless asked to). Exists for [`chromium_scale_args`]/[`gtk_qt_scale_env`]:
/// a third-party binary chonkstep doesn't control needs to be *told*
/// about the desktop's scale through whatever convention its own
/// toolkit understands, since it has no way to ask chonkstep for one.
pub fn spawn_detached_with_env(program: &str, args: &[&str], env: &[(String, String)]) -> Option<u32> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    match command.spawn() {
        Ok(child) => {
            let pid = child.id();
            tracing::info!(program, pid, "launched");
            Some(pid)
        }
        Err(e) => {
            tracing::warn!(program, ?e, "failed to launch");
            None
        }
    }
}

/// Command-line flags that make a Chromium-family browser (Edge,
/// Chrome, Brave, ...) render at this desktop's actual `CHONKSTEP_SCALE`
/// instead of guessing its own — Chromium does its own DPI detection
/// and has no way to discover a WM-invented scale factor like this
/// desktop's, so it has to be told explicitly. Every third-party app the
/// root menu launches should scale the same way chonkstep's own chrome
/// does — see also [`gtk_qt_scale_env`] for the same idea applied to
/// GTK/Qt toolkits, which don't read this flag at all.
pub fn chromium_scale_args(scale: f32) -> Vec<String> {
    vec![format!("--force-device-scale-factor={scale}")]
}

/// Works around a real, confirmed hang on this desktop: Omarchy's
/// system-wide Edge/Chromium flags file defaults to
/// `--password-store=gnome-libsecret`, which makes the browser block on
/// the D-Bus-activatable `org.freedesktop.secrets` service before it'll
/// finish starting up. In a minimal WM session (no GNOME session bus
/// autostarting things the usual way), that activation conflicts with
/// the `gnome-keyring-daemon` already running outside of D-Bus
/// activation and never completes — reproduced directly with `busctl
/// --user call org.freedesktop.secrets ... Ping`, which times out after
/// exactly 25 seconds, matching the "spinning for ~30 seconds before a
/// page loads" symptom exactly. `--password-store=basic` switches to
/// Chromium's own local encrypted file store instead, skipping the
/// D-Bus secrets dance entirely. Command-line flags win over the flags
/// file (whichever occurrence of a switch comes *last* takes effect,
/// and the launcher script appends chonkstep's own args after the flags
/// file's contents — see `microsoft-edge-stable`'s wrapper script).
pub fn chromium_avoid_secrets_service_hang_args() -> Vec<String> {
    vec!["--password-store=basic".to_string()]
}

/// Environment variables that make GTK/Qt-based UI — including the
/// native file-open/save dialogs a Chromium browser itself delegates to
/// GTK for on Linux — honor this desktop's scale too. There's no
/// running XSETTINGS daemon or desktop portal in this WM to advertise
/// DPI the way a full desktop environment would, so external toolkits
/// are told directly through the env vars they already fall back to.
///
/// `GDK_SCALE` only accepts a whole number (it's a literal backing-store
/// pixel-doubling factor, not a DPI hint), so any fractional remainder
/// of `scale` is carried by `GDK_DPI_SCALE` instead — GTK's own
/// documented recipe for fractional scaling is exactly this pairing,
/// and the two intentionally multiply back out to `scale` (e.g. 1.5 →
/// `GDK_SCALE=2`, `GDK_DPI_SCALE=0.75`). `QT_SCALE_FACTOR` handles the
/// Qt side directly since it accepts a plain float.
pub fn gtk_qt_scale_env(scale: f32) -> Vec<(String, String)> {
    let integer_scale = scale.round().max(1.0);
    let dpi_remainder = scale / integer_scale;
    vec![
        ("GDK_SCALE".to_string(), (integer_scale as u32).to_string()),
        ("GDK_DPI_SCALE".to_string(), format!("{dpi_remainder:.4}")),
        ("QT_SCALE_FACTOR".to_string(), format!("{scale:.4}")),
        ("QT_AUTO_SCREEN_SCALE_FACTOR".to_string(), "0".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk_dpi_scale_and_gdk_scale_multiply_back_to_the_requested_scale() {
        for scale in [1.0f32, 1.5, 2.0, 3.0, 2.25] {
            let env = gtk_qt_scale_env(scale);
            let gdk_scale: f32 = env.iter().find(|(k, _)| k == "GDK_SCALE").unwrap().1.parse().unwrap();
            let dpi_scale: f32 = env.iter().find(|(k, _)| k == "GDK_DPI_SCALE").unwrap().1.parse().unwrap();
            assert!((gdk_scale * dpi_scale - scale).abs() < 0.01, "scale {scale}: {gdk_scale} * {dpi_scale} should reconstruct it");
        }
    }

    #[test]
    fn gdk_scale_is_always_a_whole_number_even_for_fractional_input() {
        let env = gtk_qt_scale_env(1.5);
        let gdk_scale = &env.iter().find(|(k, _)| k == "GDK_SCALE").unwrap().1;
        assert_eq!(gdk_scale, "2");
    }

    #[test]
    fn chromium_flag_carries_the_exact_scale() {
        let args = chromium_scale_args(2.25);
        assert_eq!(args, vec!["--force-device-scale-factor=2.25".to_string()]);
    }
}
