//! Serialize external preference writes and coalesce pending switches. A slow
//! helper must neither grow a thread queue nor let an old mode finish last.
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use wm_theme::Appearance;

#[derive(Default)]
struct Pending {
    mode: Option<Appearance>,
    closed: bool,
}

#[derive(Default)]
struct Mailbox {
    pending: Mutex<Pending>,
    wake: Condvar,
}

struct Worker(Arc<Mailbox>);

impl Worker {
    fn start(
        mut apply: impl FnMut(Appearance) + Send + 'static,
    ) -> std::io::Result<(Self, JoinHandle<()>)> {
        let shared = Arc::new(Mailbox::default());
        let receiver = shared.clone();
        let thread = std::thread::Builder::new()
            .name("appearance".into())
            .spawn(move || loop {
                let mode = {
                    let mut pending = receiver.pending.lock().unwrap();
                    while pending.mode.is_none() && !pending.closed {
                        pending = receiver.wake.wait(pending).unwrap();
                    }
                    if pending.closed {
                        return;
                    }
                    pending.mode.take().unwrap()
                };
                // Never hold the mailbox while running an external command.
                apply(mode);
            })?;
        Ok((Self(shared), thread))
    }

    fn submit(&self, mode: Appearance) {
        self.0.pending.lock().unwrap().mode = Some(mode);
        self.0.wake.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().closed = true;
        self.0.wake.notify_one();
    }
}

pub(super) fn submit(mode: Appearance) {
    static WORKER: OnceLock<Option<Worker>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| match Worker::start(super::apply_to_applications) {
        Ok((worker, _thread)) => Some(worker),
        Err(error) => {
            tracing::warn!(?error, "could not start appearance propagation worker");
            None
        }
    });
    if let Some(worker) = worker {
        worker.submit(mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_busy_worker_keeps_only_the_latest_pending_mode_and_stops_on_drop() {
        let (started, observed) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let (worker, thread) = Worker::start(move |mode| {
            started.send(mode).unwrap();
            proceed.recv_timeout(Duration::from_secs(5)).unwrap();
        })
        .unwrap();
        worker.submit(Appearance::Dark);
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(5)).unwrap(),
            Appearance::Dark
        );
        for _ in 0..10_000 {
            worker.submit(Appearance::Light);
            worker.submit(Appearance::Dark);
        }
        worker.submit(Appearance::Light);
        assert_eq!(
            observed.try_recv(),
            Err(mpsc::TryRecvError::Empty),
            "requests must not start concurrent writes"
        );
        release.send(()).unwrap();
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(5)).unwrap(),
            Appearance::Light
        );
        drop(worker);
        release.send(()).unwrap();
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "dropping the worker must terminate it, not merely time out"
        );
        thread.join().unwrap();
    }
}
