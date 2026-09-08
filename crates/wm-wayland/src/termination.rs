//! Session termination must not disable signal delivery in launched apps.
//!
//! A signalfd source blocks signals on its thread. Every later worker and
//! child inherits that mask, and exec preserves it: udiskie, shell helpers,
//! and even `uwsm stop` could then wait until systemd's kill timeout. A
//! self-pipe handler instead wakes calloop with ordinary signal delivery;
//! exec resets caught handlers automatically, including in library-spawned
//! children such as XWayland. All teardown stays outside the signal handler.
//! SIGABRT remains unhandled: a panic must still reach the crash supervisor.

use std::io::{self, Read};
use std::os::unix::net::UnixStream;

use signal_hook::{consts::signal::{SIGHUP, SIGINT, SIGTERM}, SigId};
use smithay::reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction};

use crate::state::Compositor;

/// Unregisters even after a partially failed installation. The registered
/// actions own their write descriptors; the event loop owns the reader.
pub(crate) struct Registration(Vec<SigId>);

impl Drop for Registration {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

pub(crate) fn install(handle: &LoopHandle<'_, Compositor>) -> Result<Registration, Box<dyn std::error::Error>> {
    let (reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    writer.set_nonblocking(true)?;
    let mut registration = Registration(Vec::new());
    for signal in [SIGTERM, SIGHUP, SIGINT] {
        registration.0.push(signal_hook::low_level::pipe::register(signal, writer.try_clone()?)?);
    }
    handle.insert_source(Generic::new(reader, Interest::READ, Mode::Level), |_, reader, comp| {
        // A byte confirms a signal, even if several coalesced. Spurious
        // readiness must not log out the user. Only the event loop logs and
        // changes compositor state; the handler only writes to the pipe.
        let mut bytes = [0; 64];
        loop {
            match (&**reader).read(&mut bytes) {
                Ok(0) => return Ok(PostAction::Remove),
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(PostAction::Continue),
                Err(error) => return Err(error),
            }
        }
        tracing::info!("session termination requested; logging out cleanly");
        comp.restart = false;
        comp.running = false;
        Ok(PostAction::Continue)
    })?;

    // Older builds re-execed with signalfd's mask still blocked. Clear only
    // our three signals, after their handlers are installed and before any
    // workers or children start. This also repairs an inherited login mask.
    // SAFETY: all pointers reference initialized, live sigset_t storage.
    // pthread_sigmask changes only this calling thread's mask; installation
    // runs at startup before the compositor creates its worker threads.
    unsafe {
        let mut signals: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut signals);
        for signal in [SIGTERM, SIGHUP, SIGINT] {
            libc::sigaddset(&mut signals, signal);
        }
        let error = libc::pthread_sigmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut());
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error).into());
        }
    }
    Ok(registration)
}
