//! Session-bus idle-inhibition bridges.
//!
//! Native Wayland clients inhibit idle with `zwp_idle_inhibit_manager_v1`,
//! but a large part of the desktop ecosystem instead calls either
//! `org.freedesktop.ScreenSaver` directly or the Inhibit desktop portal.
//! This module owns both service names on one small blocking zbus thread.
//! It keeps caller/request lifetime bookkeeping there and sends only an
//! absolute inhibitor count (or a user-activity edge) into calloop.  Idle
//! policy consequently remains single-threaded in [`crate::idle`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use calloop::channel::{self, Event, SyncSender};
use calloop::LoopHandle;
use zbus::message::Header;
use zbus::object_server::{ObjectServer, ResponseDispatchNotifier};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::state::Compositor;

const SCREEN_SAVER_NAME: &str = "org.freedesktop.ScreenSaver";
const SCREEN_SAVER_PATH: &str = "/org/freedesktop/ScreenSaver";
const PORTAL_NAME: &str = "org.freedesktop.impl.portal.desktop.chonkstep";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_IDLE_FLAG: u32 = 8;

/// Bounds both the compositor count and the dynamic portal objects a
/// hostile bus peer can make the service retain.
const MAX_EXTERNAL_REQUESTS: usize = 4096;

#[derive(Debug)]
enum BusEvent {
    Refresh,
}

/// The one bit of compositor state a synchronous D-Bus getter needs.
/// Lock/unlock edges publish it from `idle::refresh`; no policy travels
/// in the other direction through this atomic.
#[derive(Debug, Default)]
pub(crate) struct ScreenSaverStatus {
    active: AtomicBool,
    external_inhibitors: AtomicU32,
    activity_pending: AtomicBool,
}

impl ScreenSaverStatus {
    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn set_external_inhibitors(&self, count: u32) {
        self.external_inhibitors.store(count, Ordering::Release);
    }

    fn external_inhibitors(&self) -> u32 {
        self.external_inhibitors.load(Ordering::Acquire)
    }

    fn request_activity(&self) {
        self.activity_pending.store(true, Ordering::Release);
    }

    fn take_activity(&self) -> bool {
        self.activity_pending.swap(false, Ordering::AcqRel)
    }
}

/// Register the compositor-side channel first, then start the service.
/// A missing session bus is not fatal to a nested/development compositor:
/// native Wayland inhibition remains available and the thread reports the
/// unavailable compatibility bridge in the log.
pub(crate) fn init(
    loop_handle: &LoopHandle<'static, Compositor>,
) -> Option<Arc<ScreenSaverStatus>> {
    let status = Arc::new(ScreenSaverStatus::default());
    let event_status = Arc::clone(&status);
    // A single queued wake is enough: the atomics hold the newest
    // aggregate and activity is an idempotent timer reset. This keeps
    // D-Bus churn from growing an unbounded event queue.
    let (sender, receiver) = channel::sync_channel(1);
    if let Err(error) = loop_handle.insert_source(receiver, move |event, _, comp| match event {
        Event::Msg(BusEvent::Refresh) => {
            comp.idle
                .set_external_inhibitors(event_status.external_inhibitors());
            if event_status.take_activity() {
                crate::idle::note_activity(comp);
            }
        }
        Event::Closed => tracing::debug!("session-bus inhibit bridge stopped"),
    }) {
        tracing::warn!(?error, "could not register session-bus inhibit events");
        return None;
    }

    let thread_status = Arc::clone(&status);
    if let Err(error) = std::thread::Builder::new()
        .name("chonkstep-inhibit-bus".into())
        .spawn(move || {
            if let Err(error) = serve(sender, thread_status) {
                tracing::warn!(?error, "session-bus idle inhibition unavailable");
            }
        })
    {
        tracing::warn!(?error, "could not start session-bus inhibit service");
        return None;
    }

    Some(status)
}

fn serve(sender: SyncSender<BusEvent>, status: Arc<ScreenSaverStatus>) -> zbus::Result<()> {
    let shared = Shared::new(sender, status);
    let connection = zbus::blocking::connection::Builder::session()?
        .serve_at(
            SCREEN_SAVER_PATH,
            ScreenSaver {
                shared: shared.clone(),
            },
        )?
        .serve_at(
            PORTAL_PATH,
            PortalInhibit {
                shared: shared.clone(),
            },
        )?
        .build()?;

    // Subscribe before publishing either service name.  Otherwise a
    // client could acquire and crash in the small gap between name
    // publication and the NameOwnerChanged match becoming active.
    let proxy = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    let changes = proxy.receive_name_owner_changed()?;
    let name_flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
    let screen_saver_owned = matches!(
        connection.request_name_with_flags(SCREEN_SAVER_NAME, name_flags)?,
        zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner
    );
    let portal_owned = matches!(
        connection.request_name_with_flags(PORTAL_NAME, name_flags)?,
        zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner
    );

    tracing::info!(
        screen_saver = SCREEN_SAVER_NAME,
        portal = PORTAL_NAME,
        screen_saver_owned,
        portal_owned,
        "session-bus idle inhibition ready"
    );

    // Every held item records the unique sender name that created it.
    // The bus emits this edge when that connection vanishes, including
    // crashes; release all its items instead of leaving the session awake.
    for change in changes {
        let args = match change.args() {
            Ok(args) => args,
            Err(error) => {
                tracing::warn!(?error, "ignored malformed NameOwnerChanged signal");
                continue;
            }
        };
        if args.new_owner().as_ref().is_some() {
            continue;
        }
        let removed_paths = shared.release_peer(args.name().as_str());
        for path in removed_paths {
            if let Err(error) = connection.object_server().remove::<PortalRequest, _>(&path) {
                tracing::debug!(?error, request = %path, "portal request already removed");
            }
        }
    }
    // The bus itself went away. Every peer and request went with it;
    // publish zero before the service thread exits so a dead bus cannot
    // leave the compositor permanently inhibited.
    shared.release_all();
    Ok(())
}

#[derive(Clone)]
struct Shared {
    inner: Arc<SharedInner>,
}

struct SharedInner {
    sender: SyncSender<BusEvent>,
    status: Arc<ScreenSaverStatus>,
    ledger: Mutex<Ledger>,
}

impl Shared {
    fn new(sender: SyncSender<BusEvent>, status: Arc<ScreenSaverStatus>) -> Self {
        Self {
            inner: Arc::new(SharedInner {
                sender,
                status,
                ledger: Mutex::new(Ledger::default()),
            }),
        }
    }

    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.inner
            .ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish_count(&self, ledger: &Ledger) {
        self.inner
            .status
            .set_external_inhibitors(ledger.external_count());
        self.wake_compositor();
    }

    fn wake_compositor(&self) {
        // Full means an equivalent refresh is already queued; its
        // callback reads the newest atomic values above.
        let _ = self.inner.sender.try_send(BusEvent::Refresh);
    }

    fn acquire_direct(&self, peer: String) -> zbus::fdo::Result<u32> {
        let mut ledger = self.ledger();
        let cookie = ledger.acquire_direct(peer)?;
        self.publish_count(&ledger);
        Ok(cookie)
    }

    fn release_direct(&self, peer: &str, cookie: u32) -> zbus::fdo::Result<()> {
        let mut ledger = self.ledger();
        ledger.release_direct(peer, cookie)?;
        self.publish_count(&ledger);
        Ok(())
    }

    fn reserve_portal(
        &self,
        path: OwnedObjectPath,
        peer: String,
        idle: bool,
    ) -> zbus::fdo::Result<()> {
        let mut ledger = self.ledger();
        ledger.reserve_portal(path, peer, idle)?;
        self.publish_count(&ledger);
        Ok(())
    }

    fn release_portal(&self, peer: &str, path: &OwnedObjectPath) -> zbus::fdo::Result<()> {
        let mut ledger = self.ledger();
        ledger.release_portal(peer, path)?;
        self.publish_count(&ledger);
        Ok(())
    }

    fn roll_back_portal(&self, path: &OwnedObjectPath) {
        let mut ledger = self.ledger();
        if ledger.portal.remove(path).is_some() {
            self.publish_count(&ledger);
        }
    }

    fn portal_is_reserved(&self, path: &OwnedObjectPath) -> bool {
        self.ledger().portal.contains_key(path)
    }

    fn release_peer(&self, peer: &str) -> Vec<OwnedObjectPath> {
        let mut ledger = self.ledger();
        let before = ledger.request_count();
        let paths = ledger.release_peer(peer);
        if ledger.request_count() != before {
            self.publish_count(&ledger);
        }
        paths
    }

    fn release_all(&self) {
        let mut ledger = self.ledger();
        if ledger.request_count() == 0 {
            return;
        }
        ledger.direct.clear();
        ledger.portal.clear();
        self.publish_count(&ledger);
    }

    fn note_activity(&self) {
        self.inner.status.request_activity();
        self.wake_compositor();
    }
}

#[derive(Debug)]
struct PortalEntry {
    peer: String,
    idle: bool,
}

#[derive(Debug, Default)]
struct Ledger {
    next_cookie: u32,
    direct: HashMap<u32, String>,
    portal: HashMap<OwnedObjectPath, PortalEntry>,
}

impl Ledger {
    fn request_count(&self) -> usize {
        self.direct.len().saturating_add(self.portal.len())
    }

    fn external_count(&self) -> u32 {
        let count = self
            .direct
            .len()
            .saturating_add(self.portal.values().filter(|entry| entry.idle).count());
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn acquire_direct(&mut self, peer: String) -> zbus::fdo::Result<u32> {
        if self.request_count() >= MAX_EXTERNAL_REQUESTS {
            return Err(zbus::fdo::Error::LimitsExceeded(
                "too many live ChonkStep idle-inhibit requests".into(),
            ));
        }
        loop {
            self.next_cookie = self.next_cookie.wrapping_add(1);
            if self.next_cookie != 0 && !self.direct.contains_key(&self.next_cookie) {
                self.direct.insert(self.next_cookie, peer);
                return Ok(self.next_cookie);
            }
        }
    }

    fn release_direct(&mut self, peer: &str, cookie: u32) -> zbus::fdo::Result<()> {
        match self.direct.get(&cookie) {
            Some(owner) if owner == peer => {
                self.direct.remove(&cookie);
                Ok(())
            }
            Some(_) => Err(zbus::fdo::Error::AccessDenied(
                "idle-inhibit cookie belongs to another D-Bus peer".into(),
            )),
            None => Err(zbus::fdo::Error::InvalidArgs(
                "unknown idle-inhibit cookie".into(),
            )),
        }
    }

    fn reserve_portal(
        &mut self,
        path: OwnedObjectPath,
        peer: String,
        idle: bool,
    ) -> zbus::fdo::Result<()> {
        if self.request_count() >= MAX_EXTERNAL_REQUESTS {
            return Err(zbus::fdo::Error::LimitsExceeded(
                "too many live ChonkStep idle-inhibit requests".into(),
            ));
        }
        if self.portal.contains_key(&path) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "duplicate portal request path".into(),
            ));
        }
        self.portal.insert(path, PortalEntry { peer, idle });
        Ok(())
    }

    fn release_portal(&mut self, peer: &str, path: &OwnedObjectPath) -> zbus::fdo::Result<()> {
        match self.portal.get(path) {
            Some(entry) if entry.peer == peer => {
                self.portal.remove(path);
                Ok(())
            }
            Some(_) => Err(zbus::fdo::Error::AccessDenied(
                "portal request belongs to another D-Bus peer".into(),
            )),
            // Close is deliberately idempotent: a frontend may close a
            // request while its own disconnect cleanup is already in flight.
            None => Ok(()),
        }
    }

    fn release_peer(&mut self, peer: &str) -> Vec<OwnedObjectPath> {
        self.direct.retain(|_, owner| owner != peer);
        let removed = self
            .portal
            .iter()
            .filter(|(_, entry)| entry.peer == peer)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        self.portal.retain(|_, entry| entry.peer != peer);
        removed
    }
}

fn sender(header: &Header<'_>) -> zbus::fdo::Result<String> {
    header
        .sender()
        .map(ToString::to_string)
        .ok_or_else(|| zbus::fdo::Error::Failed("D-Bus call has no sender identity".into()))
}

struct ScreenSaver {
    shared: Shared,
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaver {
    fn inhibit(
        &self,
        #[zbus(header)] header: Header<'_>,
        _application_name: &str,
        _reason_for_inhibit: &str,
    ) -> zbus::fdo::Result<u32> {
        self.shared.acquire_direct(sender(&header)?)
    }

    #[zbus(name = "UnInhibit")]
    fn un_inhibit(&self, #[zbus(header)] header: Header<'_>, cookie: u32) -> zbus::fdo::Result<()> {
        self.shared.release_direct(&sender(&header)?, cookie)
    }

    fn get_active(&self) -> bool {
        self.shared.inner.status.active()
    }

    fn simulate_user_activity(&self) {
        self.shared.note_activity();
    }
}

struct PortalInhibit {
    shared: Shared,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl PortalInhibit {
    #[allow(clippy::too_many_arguments)] // The portal ABI fixes all seven input/context arguments.
    async fn inhibit(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        _app_id: &str,
        _window: &str,
        flags: u32,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let peer = sender(&header)?;
        self.shared
            .reserve_portal(handle.clone(), peer.clone(), flags & PORTAL_IDLE_FLAG != 0)?;
        let added = match server
            .at(
                handle.clone(),
                PortalRequest {
                    path: handle.clone(),
                    shared: self.shared.clone(),
                },
            )
            .await
        {
            Ok(added) => added,
            Err(error) => {
                self.shared.roll_back_portal(&handle);
                return Err(zbus::fdo::Error::Failed(error.to_string()));
            }
        };
        if !added {
            self.shared.roll_back_portal(&handle);
            return Err(zbus::fdo::Error::InvalidArgs(
                "portal request path is already exported".into(),
            ));
        }
        // Peer-loss cleanup can race this asynchronous object export.
        // If it already removed the reservation, do not strand the new
        // request object (or claim an inhibit that no caller owns).
        if !self.shared.portal_is_reserved(&handle) {
            let _ = server.remove::<PortalRequest, _>(&handle).await;
            return Err(zbus::fdo::Error::Failed(
                "portal requester disconnected while inhibition was being created".into(),
            ));
        }
        Ok(())
    }

    /// ChonkStep has no login-manager state machine to monitor.  Keep
    /// the method in introspection and answer with a clean cancellation
    /// instead of advertising a method that disappears at call time.
    fn create_monitor(
        &self,
        _handle: OwnedObjectPath,
        _session_handle: OwnedObjectPath,
        _app_id: &str,
        _window: &str,
    ) -> u32 {
        2
    }

    fn query_end_response(&self, _session_handle: OwnedObjectPath) {}
}

struct PortalRequest {
    path: OwnedObjectPath,
    shared: Shared,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl PortalRequest {
    fn close(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<ResponseDispatchNotifier<()>> {
        self.shared.release_portal(&sender(&header)?, &self.path)?;

        // Removing the interface from inside its own method would wait
        // on the method's live interface reference.  The notifier fires
        // after the empty reply has left; remove it from a zbus task then.
        let (response, sent) = ResponseDispatchNotifier::new(());
        let connection = connection.clone();
        let task_connection = connection.clone();
        let path = self.path.clone();
        connection
            .executor()
            .spawn(
                async move {
                    sent.await;
                    let _ = task_connection
                        .object_server()
                        .remove::<PortalRequest, _>(&path)
                        .await;
                },
                "remove closed ChonkStep inhibit request",
            )
            .detach();
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn path(value: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(value).unwrap()
    }

    #[test]
    fn ledger_counts_only_idle_portal_requests() {
        let mut ledger = Ledger::default();
        assert_eq!(ledger.acquire_direct(":1.1".into()).unwrap(), 1);
        ledger
            .reserve_portal(path("/request/idle"), ":1.2".into(), true)
            .unwrap();
        ledger
            .reserve_portal(path("/request/logout"), ":1.2".into(), false)
            .unwrap();
        assert_eq!(ledger.external_count(), 2);
    }

    #[test]
    fn cookies_are_owned_and_peer_loss_releases_every_kind() {
        let mut ledger = Ledger::default();
        let mine = ledger.acquire_direct(":1.7".into()).unwrap();
        let theirs = ledger.acquire_direct(":1.8".into()).unwrap();
        ledger
            .reserve_portal(path("/request/mine"), ":1.7".into(), true)
            .unwrap();

        assert!(matches!(
            ledger.release_direct(":1.8", mine),
            Err(zbus::fdo::Error::AccessDenied(_))
        ));
        assert_eq!(ledger.release_peer(":1.7"), vec![path("/request/mine")]);
        assert_eq!(ledger.external_count(), 1);
        ledger.release_direct(":1.8", theirs).unwrap();
        assert_eq!(ledger.external_count(), 0);
    }

    #[test]
    fn portal_close_is_idempotent_but_not_cross_peer() {
        let mut ledger = Ledger::default();
        let request = path("/request/one");
        ledger
            .reserve_portal(request.clone(), ":1.4".into(), true)
            .unwrap();
        assert!(matches!(
            ledger.release_portal(":1.5", &request),
            Err(zbus::fdo::Error::AccessDenied(_))
        ));
        ledger.release_portal(":1.4", &request).unwrap();
        ledger.release_portal(":1.4", &request).unwrap();
        assert_eq!(ledger.external_count(), 0);
    }

    #[test]
    fn a_queued_wake_coalesces_to_the_latest_count() {
        let (sender, receiver) = channel::sync_channel(1);
        let status = Arc::new(ScreenSaverStatus::default());
        let shared = Shared::new(sender, Arc::clone(&status));
        shared.acquire_direct(":1.20".into()).unwrap();
        shared.acquire_direct(":1.21".into()).unwrap();

        assert!(matches!(receiver.try_recv(), Ok(BusEvent::Refresh)));
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert_eq!(status.external_inhibitors(), 2);
    }

    fn next_event(receiver: &channel::Channel<BusEvent>) -> BusEvent {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match receiver.try_recv() {
                Ok(event) => return event,
                Err(std::sync::mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("timed out waiting for inhibit-bus event: {error}"),
            }
        }
    }

    /// Run the real zbus object server under a private bus so this test
    /// neither collides with nor replaces the developer's desktop
    /// ScreenSaver service. The child marker prevents recursive buses.
    #[test]
    #[allow(clippy::disallowed_methods)] // This blocks only the unit-test harness, never the compositor thread.
    fn dbus_contract_round_trip() {
        const CHILD: &str = "CHONKSTEP_INHIBIT_BUS_TEST_CHILD";
        const TEST: &str = "inhibit_bus::tests::dbus_contract_round_trip";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new("dbus-run-session")
                .arg("--")
                .arg(std::env::current_exe().unwrap())
                .args([TEST, "--exact", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .expect("dbus-run-session must be installed for the Wayland session");
            assert!(
                output.status.success(),
                "private-bus child test failed ({status}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                status = output.status,
                stdout = String::from_utf8_lossy(&output.stdout),
                stderr = String::from_utf8_lossy(&output.stderr),
            );
            return;
        }

        let (sender, receiver) = channel::sync_channel(1);
        let status = Arc::new(ScreenSaverStatus::default());
        let service_status = Arc::clone(&status);
        std::thread::spawn(move || serve(sender, service_status).unwrap());

        let client = zbus::blocking::Connection::session().unwrap();
        let dbus = zbus::blocking::fdo::DBusProxy::new(&client).unwrap();
        let name = zbus::names::BusName::try_from(SCREEN_SAVER_NAME).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !dbus.name_has_owner(name.clone()).unwrap() {
            assert!(
                Instant::now() < deadline,
                "ScreenSaver service name was not acquired"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let screen_saver = zbus::blocking::Proxy::new(
            &client,
            SCREEN_SAVER_NAME,
            SCREEN_SAVER_PATH,
            SCREEN_SAVER_NAME,
        )
        .unwrap();
        let cookie: u32 = screen_saver
            .call("Inhibit", &("test", "playing video"))
            .unwrap();
        assert!(matches!(next_event(&receiver), BusEvent::Refresh));
        assert_eq!(status.external_inhibitors(), 1);
        let active: bool = screen_saver.call("GetActive", &()).unwrap();
        assert!(!active);
        status.set_active(true);
        let active: bool = screen_saver.call("GetActive", &()).unwrap();
        assert!(active);
        let _: () = screen_saver.call("SimulateUserActivity", &()).unwrap();
        assert!(matches!(next_event(&receiver), BusEvent::Refresh));
        assert!(status.take_activity());
        let _: () = screen_saver.call("UnInhibit", &cookie).unwrap();
        assert!(matches!(next_event(&receiver), BusEvent::Refresh));
        assert_eq!(status.external_inhibitors(), 0);

        let portal = zbus::blocking::Proxy::new(
            &client,
            PORTAL_NAME,
            PORTAL_PATH,
            "org.freedesktop.impl.portal.Inhibit",
        )
        .unwrap();
        let request = path("/org/freedesktop/portal/desktop/request/1_0/chonkstep_test");
        let options = HashMap::<String, OwnedValue>::new();
        let _: () = portal
            .call(
                "Inhibit",
                &(
                    request.clone(),
                    "org.example.Test",
                    "",
                    PORTAL_IDLE_FLAG,
                    options,
                ),
            )
            .unwrap();
        assert!(matches!(next_event(&receiver), BusEvent::Refresh));
        assert_eq!(status.external_inhibitors(), 1);
        let request_proxy = zbus::blocking::Proxy::new(
            &client,
            PORTAL_NAME,
            request.as_str(),
            "org.freedesktop.impl.portal.Request",
        )
        .unwrap();
        let _: () = request_proxy.call("Close", &()).unwrap();
        assert!(matches!(next_event(&receiver), BusEvent::Refresh));
        assert_eq!(status.external_inhibitors(), 0);

        // The connection owns this cookie and then disappears without
        // UnInhibit. NameOwnerChanged must return the aggregate to zero.
        let abandoned = zbus::blocking::Connection::session().unwrap();
        let abandoned_proxy = zbus::blocking::Proxy::new(
            &abandoned,
            SCREEN_SAVER_NAME,
            SCREEN_SAVER_PATH,
            SCREEN_SAVER_NAME,
        )
        .unwrap();
        let _cookie: u32 = abandoned_proxy.call("Inhibit", &("test", "crash")).unwrap();
        assert!(matches!(next_event(&receiver), BusEvent::Refresh));
        assert_eq!(status.external_inhibitors(), 1);
        drop(abandoned_proxy);
        drop(abandoned);
        assert!(matches!(next_event(&receiver), BusEvent::Refresh));
        assert_eq!(status.external_inhibitors(), 0);
    }
}
