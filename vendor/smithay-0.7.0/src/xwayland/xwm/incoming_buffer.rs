//! Private, display-independent buffering for X11-to-Wayland pipe writes.
//! Keep this file independent of XWM state so its exact production code can
//! be exercised by the workspace's display-free dependency-regression tests.

use std::io;

#[derive(Default)]
pub(super) struct IncomingBuffer {
    data: Vec<u8>,
    offset: usize,
}

impl IncomingBuffer {
    pub(super) fn len(&self) -> usize {
        self.data.len() - self.offset
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(super) fn extend(&mut self, bytes: Vec<u8>) {
        if self.is_empty() {
            // x11rb already owns this reply allocation. Do not copy it into
            // a second buffer just to write it to the destination pipe.
            self.data = bytes;
            self.offset = 0;
        } else {
            // Normally one INCR property stays outstanding until drained.
            // Preserve ordering even if a producer publishes another early.
            let remaining = self.len();
            self.data.copy_within(self.offset.., 0);
            self.data.truncate(remaining);
            self.offset = 0;
            self.data.extend(bytes);
        }
    }

    /// One readiness dispatch does at most one write. `true` means drained.
    pub(super) fn write_with(
        &mut self,
        mut write: impl FnMut(&[u8]) -> io::Result<usize>,
    ) -> io::Result<bool> {
        if self.is_empty() {
            return Ok(true);
        }
        match write(&self.data[self.offset..]) {
            Ok(0) => Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => {
                self.offset += written;
                if self.is_empty() {
                    // No retained payload while waiting for the next chunk;
                    // that reply brings its own allocation to adopt.
                    self.data = Vec::new();
                    self.offset = 0;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
}
