//! Opt-in guard that limits linux-dmabuf to clients running one Mesa build.
//!
//! `CHONKSTEP_DMABUF_REQUIRE_MESA=<prefix>` is for an experimental session
//! whose render GPU only one separately installed Mesa can drive. Its first
//! user is the Apple M3 session (`scripts/wayland-session-m3gpu.sh`). The
//! distribution's Mesa accepts that GPU and submits command streams for an
//! older generation, and the kernel driver has no fault recovery: one such
//! submission leaves the GPU dead and the desktop frozen until reboot.
//! Clients find the render node through linux-dmabuf feedback. A client that
//! does not see the global falls back to shared-memory buffers and renders in
//! software.
//!
//! A client may see the global if its process maps a library from the
//! prefix, or if its environment will load the prefix when graphics start:
//! `LD_LIBRARY_PATH` names `<prefix>/lib`, every Vulkan driver manifest lies
//! under the prefix, and the process is not a secure-execution process whose
//! loader ignored `LD_LIBRARY_PATH`. A process that maps any Mesa driver
//! library from elsewhere never sees it, and neither does one whose `/proc`
//! entries cannot be read. This keeps
//! services started before the session, sandboxed applications with their
//! own Mesa, capability binaries and bundled drivers on software rendering.
//! It does not stop a process from opening the render node on its own; only
//! the kernel can refuse that.
//!
//! The process is the peer recorded when the connection was admitted
//! ([`record_peer`]). The filter must not ask the display for it: global
//! filters run while wayland-backend holds its state lock (while it answers
//! `wl_display.get_registry`), and `Client::get_credentials` takes that lock
//! again. That froze the M3 session before its first frame as soon as the
//! first GPU client connected. Connections without a recorded peer (the
//! compositor's own in-process sockets) do not see the global. XWayland's
//! socket pair was made by the compositor before spawning: use the child PID
//! retained by Smithay instead of its inherited socket credentials. That PID
//! goes through exactly the same Mesa-library/environment checks.
//!
//! The value `none` hides linux-dmabuf from every client: only the
//! compositor renders on the GPU, and clients draw in software.
//!
//! Unset (the default), every client sees linux-dmabuf exactly as before.

use std::path::{Path, PathBuf};

use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::xwayland::XWaylandClientData;

use crate::state::ClientState;

const VARIABLE: &str = "CHONKSTEP_DMABUF_REQUIRE_MESA";

/// `AT_SECURE` in the ELF auxiliary vector: the loader ran in secure mode
/// (setuid, file capabilities) and ignored `LD_LIBRARY_PATH`.
const AT_SECURE: usize = 23;

#[derive(Clone, Debug)]
pub(crate) struct ClientMesaGuard {
    /// `None` for `none`, or when the variable held an unusable value:
    /// then no client sees linux-dmabuf (fail closed), rather than every
    /// client.
    prefix: Option<PathBuf>,
}

impl ClientMesaGuard {
    /// The guard configured for this process, or `None` when unset.
    pub(crate) fn from_env() -> Option<Self> {
        Self::from_value(std::env::var_os(VARIABLE)?)
    }

    fn from_value(value: std::ffi::OsString) -> Option<Self> {
        if value.is_empty() {
            return None;
        }
        if value == "none" {
            tracing::warn!(variable = VARIABLE,
                "linux-dmabuf is hidden from every client; clients render in software");
            return Some(Self { prefix: None });
        }
        let prefix = PathBuf::from(value);
        if !prefix.is_absolute() {
            tracing::error!(variable = VARIABLE, value = %prefix.display(),
                "not an absolute Mesa prefix; linux-dmabuf is hidden from every client");
            return Some(Self { prefix: None });
        }
        tracing::warn!(variable = VARIABLE, prefix = %prefix.display(),
            "linux-dmabuf is offered only to clients running the Mesa in this prefix");
        Some(Self { prefix: Some(prefix) })
    }

    /// The linux-dmabuf global filter. Runs under the display's lock: it
    /// reads only the client's data and `/proc`, never the display.
    pub(crate) fn allows(&self, client: &Client) -> bool {
        let Some(prefix) = self.prefix.as_deref() else { return false };
        let pid = client.get_data::<ClientState>().and_then(|data| data.peer_pid.get()).copied()
            .or_else(|| client.get_data::<XWaylandClientData>()
                .and_then(|data| i32::try_from(data.process_id()).ok()));
        let Some(pid) = pid else {
            tracing::info!("linux-dmabuf hidden from a client without a recorded peer process");
            return false;
        };
        let proc_dir = PathBuf::from(format!("/proc/{pid}"));
        let verdict = match std::fs::read_to_string(proc_dir.join("maps")) {
            Err(_) => Verdict::Unreadable,
            Ok(maps) => match mesa_mapping(&maps, prefix) {
                MesaMapping::Foreign(library) => Verdict::Foreign(library),
                MesaMapping::Required => Verdict::Allowed,
                MesaMapping::None => {
                    let environ = std::fs::read(proc_dir.join("environ")).unwrap_or_default();
                    let secure = std::fs::read(proc_dir.join("auxv")).ok().and_then(|auxv| at_secure(&auxv));
                    if environment_selects(&environ, prefix) && secure == Some(false) {
                        Verdict::Allowed
                    } else {
                        Verdict::NotSelected
                    }
                }
            },
        };
        let executable = std::fs::read_link(proc_dir.join("exe")).unwrap_or_default();
        if verdict == Verdict::Allowed {
            tracing::debug!(pid, executable = %executable.display(), "linux-dmabuf offered: client runs the required Mesa");
            return true;
        }
        tracing::info!(pid, executable = %executable.display(), reason = ?verdict,
            "linux-dmabuf hidden: client does not run the required Mesa; it falls back to shared memory");
        false
    }
}

/// Records the peer process of a newly admitted connection for
/// [`ClientMesaGuard::allows`]. Call it right after `insert_client`, never
/// from a global filter or anything else the display runs under its lock.
pub(crate) fn record_peer(client: &Client, display: &DisplayHandle) {
    let Some(data) = client.get_data::<ClientState>() else { return };
    match client.get_credentials(display) {
        Ok(credentials) => {
            let _ = data.peer_pid.set(credentials.pid);
        }
        Err(error) => tracing::debug!(?error, "no peer credentials for an admitted client"),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Allowed,
    /// Maps a Mesa driver library from outside the prefix.
    Foreign(String),
    /// Neither maps the prefix nor has an environment that selects it.
    NotSelected,
    /// `/proc/<pid>` is not readable (another user, not dumpable, gone).
    Unreadable,
}

#[derive(Debug, PartialEq, Eq)]
enum MesaMapping {
    Required,
    Foreign(String),
    None,
}

/// Classifies a `/proc/<pid>/maps` text. A foreign Mesa driver anywhere wins
/// over prefix libraries: a process with both loaded may render with either.
fn mesa_mapping(maps: &str, prefix: &Path) -> MesaMapping {
    let mut required = false;
    for line in maps.lines() {
        // Address, permissions, offset, device and inode never contain '/':
        // the first one starts the path. "[heap]" and friends have none.
        let Some(start) = line.find('/') else { continue };
        let path = Path::new(line[start..].trim_end().trim_end_matches(" (deleted)"));
        if path.starts_with(prefix) {
            required = true;
        } else if path.file_name().and_then(|name| name.to_str()).is_some_and(is_mesa_driver_library) {
            return MesaMapping::Foreign(path.display().to_string());
        }
    }
    if required { MesaMapping::Required } else { MesaMapping::None }
}

/// Mesa libraries that select and drive a GPU. The Vulkan loader, glvnd
/// dispatch libraries and lavapipe (a CPU device) are not among them.
fn is_mesa_driver_library(name: &str) -> bool {
    name.starts_with("libgallium-")
        || name.starts_with("libEGL_mesa.so")
        || name.starts_with("libGLX_mesa.so")
        || name.starts_with("libgbm.so")
        || name == "dri_gbm.so"
        || name.ends_with("_dri.so")
        || (name.starts_with("libvulkan_") && !name.starts_with("libvulkan_lvp"))
}

/// Whether a NUL-separated environment block makes the dynamic loader and
/// the Vulkan loader pick the prefix: `LD_LIBRARY_PATH` names `<prefix>/lib`
/// and the Vulkan driver manifests (`VK_DRIVER_FILES`, else
/// `VK_ICD_FILENAMES`, plus any `VK_ADD_DRIVER_FILES`) are all inside the
/// prefix. Without an explicit driver list the loader would also use the
/// system's manifests.
fn environment_selects(environ: &[u8], prefix: &Path) -> bool {
    let value = |name: &str| {
        environ
            .split(|byte| *byte == 0)
            .filter_map(|entry| std::str::from_utf8(entry).ok())
            .find_map(|entry| entry.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')))
    };
    let lib = prefix.join("lib");
    let libraries = value("LD_LIBRARY_PATH").is_some_and(|paths| paths.split(':').any(|entry| Path::new(entry) == lib));
    let inside = |paths: &str| paths.split(':').all(|entry| !entry.is_empty() && Path::new(entry).starts_with(prefix));
    let drivers = value("VK_DRIVER_FILES").filter(|paths| !paths.is_empty()).or_else(|| value("VK_ICD_FILENAMES"));
    let vulkan = drivers.is_some_and(inside) && value("VK_ADD_DRIVER_FILES").is_none_or(inside);
    libraries && vulkan
}

/// Reads `AT_SECURE` from a native-endian `/proc/<pid>/auxv` block.
fn at_secure(auxv: &[u8]) -> Option<bool> {
    const WORD: usize = std::mem::size_of::<usize>();
    auxv.as_chunks::<WORD>().0.as_chunks::<2>().0.iter().find_map(|[key, value]| {
        (usize::from_ne_bytes(*key) == AT_SECURE).then_some(usize::from_ne_bytes(*value) != 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "/home/u/mesa-prefix";

    fn maps(paths: &[&str]) -> String {
        let mut text = String::from("aaaaaaaa0000-aaaaaaaa1000 rw-p 00000000 00:00 0                          [heap]\n");
        for path in paths {
            text.push_str(&format!("ffff80000000-ffff80001000 r-xp 00000000 103:06 4242                       {path}\n"));
        }
        text
    }

    #[test]
    fn a_prefix_library_mapping_admits_the_client() {
        let text = maps(&["/usr/lib/libc.so.6", "/home/u/mesa-prefix/lib/libgallium-26.1.4.so", "/usr/lib/libEGL.so.1"]);
        assert_eq!(mesa_mapping(&text, Path::new(PREFIX)), MesaMapping::Required);
    }

    #[test]
    fn any_foreign_mesa_driver_wins_over_prefix_libraries() {
        for foreign in [
            "/usr/lib/libgallium-26.2.3.so",
            "/usr/lib/libEGL_mesa.so.0.0.0",
            "/usr/lib/libgbm.so.1.0.0",
            "/usr/lib/libvulkan_asahi.so",
            "/usr/lib/dri/asahi_dri.so",
            "/usr/lib/GL/default/lib/dri/libdril_dri.so",
        ] {
            let text = maps(&["/home/u/mesa-prefix/lib/libgbm.so.1.0.0", foreign]);
            assert_eq!(mesa_mapping(&text, Path::new(PREFIX)), MesaMapping::Foreign(foreign.into()), "{foreign}");
        }
    }

    #[test]
    fn loaders_dispatchers_and_cpu_drivers_are_neither_required_nor_foreign() {
        let text = maps(&[
            "/usr/lib/libvulkan.so.1.4.0",
            "/usr/lib/libEGL.so.1.1.0",
            "/usr/lib/libGLESv2.so.2.1.0",
            "/usr/lib/libvulkan_lvp.so",
        ]);
        assert_eq!(mesa_mapping(&text, Path::new(PREFIX)), MesaMapping::None);
    }

    #[test]
    fn a_sibling_directory_sharing_the_prefix_spelling_is_foreign() {
        let text = maps(&["/home/u/mesa-prefix-old/lib/libgallium-26.1.4.so (deleted)"]);
        assert_eq!(
            mesa_mapping(&text, Path::new(PREFIX)),
            MesaMapping::Foreign("/home/u/mesa-prefix-old/lib/libgallium-26.1.4.so".into())
        );
    }

    fn environ(entries: &[&str]) -> Vec<u8> {
        entries.iter().flat_map(|entry| entry.bytes().chain([0])).collect()
    }

    const VK: &str = "VK_DRIVER_FILES=/home/u/mesa-prefix/share/vulkan/icd.d/asahi_icd.aarch64.json";

    #[test]
    fn an_environment_naming_the_prefix_for_libraries_and_vulkan_selects_it() {
        let env = environ(&["HOME=/home/u", "LD_LIBRARY_PATH=/opt/x:/home/u/mesa-prefix/lib/", VK]);
        assert!(environment_selects(&env, Path::new(PREFIX)));
        let legacy = environ(&["LD_LIBRARY_PATH=/home/u/mesa-prefix/lib",
            "VK_ICD_FILENAMES=/home/u/mesa-prefix/share/vulkan/icd.d/asahi_icd.aarch64.json"]);
        assert!(environment_selects(&legacy, Path::new(PREFIX)));
    }

    #[test]
    fn an_environment_that_would_reach_a_system_driver_does_not_select_it() {
        let prefix = Path::new(PREFIX);
        assert!(!environment_selects(&environ(&[VK]), prefix), "no LD_LIBRARY_PATH: system libgallium");
        assert!(!environment_selects(&environ(&["LD_LIBRARY_PATH=/home/u/mesa-prefix/lib"]), prefix),
            "no driver list: the loader adds the system Vulkan manifests");
        assert!(!environment_selects(&environ(&["LD_LIBRARY_PATH=/home/u/mesa-prefix/lib",
            "VK_DRIVER_FILES=/home/u/mesa-prefix/share/vulkan/icd.d/a.json:/usr/share/vulkan/icd.d/asahi_icd.json"]), prefix));
        assert!(!environment_selects(&environ(&["LD_LIBRARY_PATH=/home/u/mesa-prefix/lib", VK,
            "VK_ADD_DRIVER_FILES=/usr/share/vulkan/icd.d/asahi_icd.json"]), prefix));
        assert!(!environment_selects(&environ(&["LD_LIBRARY_PATH=/home/u/mesa-prefix/lib/dri", VK]), prefix));
        assert!(!environment_selects(&environ(&["XLD_LIBRARY_PATH=/home/u/mesa-prefix/lib", VK]), prefix));
    }

    fn auxv(pairs: &[(usize, usize)]) -> Vec<u8> {
        pairs.iter().flat_map(|(key, value)| key.to_ne_bytes().into_iter().chain(value.to_ne_bytes())).collect()
    }

    #[test]
    fn the_variable_parses_to_off_all_hidden_or_one_prefix() {
        let parse = |value: &str| ClientMesaGuard::from_value(value.into()).map(|guard| guard.prefix);
        assert_eq!(parse(""), None, "empty is unset: no filter at all");
        assert_eq!(parse("none"), Some(None), "every client hidden");
        assert_eq!(parse("mesa-prefix"), Some(None), "a relative prefix fails closed");
        assert_eq!(parse(PREFIX), Some(Some(PathBuf::from(PREFIX))));
    }

    #[test]
    fn at_secure_is_read_from_the_auxiliary_vector() {
        assert_eq!(at_secure(&auxv(&[(33, 7), (AT_SECURE, 0), (0, 0)])), Some(false));
        assert_eq!(at_secure(&auxv(&[(AT_SECURE, 1), (0, 0)])), Some(true));
        assert_eq!(at_secure(&auxv(&[(33, 7), (0, 0)])), None);
        assert_eq!(at_secure(&[]), None);
    }

    /// The real path: peer credentials of a connected client, then that
    /// process's own `/proc` entries. The peer is this test process, so the
    /// assertions avoid depending on which libraries other tests loaded.
    #[test]
    fn a_connected_client_is_judged_by_its_own_process() {
        use smithay::reexports::wayland_server::Display;
        let display = Display::<()>::new().unwrap();
        let mut handle = display.handle();
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let client = handle.insert_client(socket, std::sync::Arc::new(ClientState::default())).unwrap();
        let everything = ClientMesaGuard { prefix: Some(PathBuf::from("/")) };
        assert!(!everything.allows(&client), "no recorded peer: hidden");
        record_peer(&client, &handle);
        // Every mapped file lies under "/": required, and nothing is foreign.
        assert!(everything.allows(&client));
        // Nothing is mapped from here, and this environment does not select it.
        let elsewhere = ClientMesaGuard { prefix: Some(PathBuf::from("/nonexistent/chonkstep-mesa-prefix")) };
        assert!(!elsewhere.allows(&client));
        let unusable = ClientMesaGuard { prefix: None };
        assert!(!unusable.allows(&client), "an unusable prefix fails closed");
    }

    /// The filter runs inside the display's own dispatch of
    /// `wl_display.get_registry`, with wayland-backend's state lock held.
    /// Asking the display for credentials there deadlocked the compositor;
    /// this dispatches a real `get_registry` through a global filtered by the
    /// guard, on a thread, and fails instead of hanging if that comes back.
    #[test]
    fn the_filter_runs_inside_a_real_registry_dispatch_without_deadlock() {
        use smithay::reexports::wayland_server::{
            backend::GlobalId, protocol::wl_output::WlOutput, DataInit, Display, GlobalDispatch, New,
        };
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};

        static VIEWED: AtomicUsize = AtomicUsize::new(0);
        struct State;
        impl GlobalDispatch<WlOutput, ClientMesaGuard> for State {
            fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, _: New<WlOutput>, _: &ClientMesaGuard,
                _: &mut DataInit<'_, Self>) {}
            fn can_view(client: Client, guard: &ClientMesaGuard) -> bool {
                let allowed = guard.allows(&client);
                VIEWED.fetch_add(1, Ordering::SeqCst);
                allowed
            }
        }
        impl smithay::reexports::wayland_server::Dispatch<WlOutput, ()> for State {
            fn request(_: &mut Self, _: &Client, _: &WlOutput,
                _: <WlOutput as smithay::reexports::wayland_server::Resource>::Request, _: &(),
                _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
        }

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut display = Display::<State>::new().unwrap();
            let mut handle = display.handle();
            let _global: GlobalId = handle.create_global::<State, WlOutput, _>(
                4, ClientMesaGuard { prefix: Some(PathBuf::from("/")) });
            let (server, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let client = handle.insert_client(server, std::sync::Arc::new(ClientState::default())).unwrap();
            record_peer(&client, &handle);
            // wl_display.get_registry(new id 2), then wl_display.sync(new id 3).
            let mut request = Vec::new();
            for (opcode, id) in [(1u32, 2u32), (0, 3)] {
                request.extend_from_slice(&1u32.to_ne_bytes());
                request.extend_from_slice(&((12u32 << 16) | opcode).to_ne_bytes());
                request.extend_from_slice(&id.to_ne_bytes());
            }
            peer.write_all(&request).unwrap();
            display.dispatch_clients(&mut State).unwrap();
            display.flush_clients().unwrap();
            peer.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            let mut events = [0u8; 4096];
            let read = peer.read(&mut events).unwrap_or(0);
            done_tx.send(read).unwrap();
        });
        let read = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the registry dispatch deadlocked inside the global filter");
        assert!(read > 0, "the client got no registry events");
        assert!(VIEWED.load(Ordering::SeqCst) >= 1, "the filter was never consulted");
    }

    #[test]
    fn this_test_process_reads_as_not_secure() {
        let own = std::fs::read("/proc/self/auxv").unwrap();
        assert_eq!(at_secure(&own), Some(false));
    }
}
