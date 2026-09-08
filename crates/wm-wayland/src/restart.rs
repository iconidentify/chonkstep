//! The compositor's upstream display and its children's display are different
//! endpoints. Preserve the former before startup publishes the latter.
use std::ffi::OsString;
use std::process::Command;

pub(crate) struct HostDisplay {
    wayland: Option<OsString>,
    x11: Option<OsString>,
}

impl HostDisplay {
    pub fn capture() -> Self {
        Self {
            wayland: std::env::var_os("WAYLAND_DISPLAY"),
            x11: std::env::var_os("DISPLAY"),
        }
    }

    pub fn configure_restart(&self, command: &mut Command, nested: bool) {
        command.env("CHONKSTEP_BACKEND", if nested { "winit" } else { "drm" });
        // A WAYLAND_SOCKET is an already-consumed protocol connection, not
        // an address a new client can reconnect to after exec.
        command.env_remove("WAYLAND_SOCKET");
        for (key, value) in [("WAYLAND_DISPLAY", &self.wayland), ("DISPLAY", &self.x11)] {
            match value.as_ref().filter(|_| nested) {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn restart_restores_only_the_original_host_addresses() {
        for (wayland, x11) in [
            (Some("wayland-host"), None),
            (None, Some(":7")),
            (Some("/run/user/123/host"), Some(":2")),
            (None, None),
        ] {
            let host = HostDisplay {
                wayland: wayland.map(Into::into),
                x11: x11.map(Into::into),
            };
            for nested in [false, true] {
                let mut command = Command::new("unused");
                command
                    .env("WAYLAND_DISPLAY", "own-dead-socket")
                    .env("DISPLAY", ":99");
                host.configure_restart(&mut command, nested);
                let env: std::collections::HashMap<_, _> = command.get_envs().collect();
                assert_eq!(
                    env[OsStr::new("WAYLAND_DISPLAY")],
                    wayland.filter(|_| nested).map(OsStr::new)
                );
                assert_eq!(
                    env[OsStr::new("DISPLAY")],
                    x11.filter(|_| nested).map(OsStr::new)
                );
                assert_eq!(env[OsStr::new("WAYLAND_SOCKET")], None);
                assert_eq!(
                    env[OsStr::new("CHONKSTEP_BACKEND")],
                    Some(OsStr::new(if nested { "winit" } else { "drm" }))
                );
            }
        }
    }
}
