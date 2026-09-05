# Idle inhibition

ChonkStep accepts all three idle-inhibition routes used by desktop
applications:

- `zwp_idle_inhibit_manager_v1` for native Wayland clients;
- `org.freedesktop.ScreenSaver.Inhibit` on the session bus;
- `org.freedesktop.portal.Inhibit`, routed by
  `packaging/portal/chonkstep-portals.conf` to the compositor's
  `org.freedesktop.impl.portal.Inhibit` backend.

The D-Bus service runs off the compositor thread. It records the unique
bus peer behind every ScreenSaver cookie and portal request, then sends an
absolute external-inhibitor count into the compositor event loop. A normal
`UnInhibit`, a portal request's `Close`, and `NameOwnerChanged` when a peer
crashes all release the corresponding count. Portal flags other than Idle
are accepted for request-lifecycle compatibility but do not affect the idle
timer.

External inhibition is subject to the same lock boundary as native
Wayland inhibition and window rules. Once the session is locked it cannot
be kept awake by an inhibitor held behind the lock screen. The
`org.freedesktop.ScreenSaver.GetActive` reply follows that lock state, and
`SimulateUserActivity` enters the same idle notifier path as real input.

Set `CHONKSTEP_IDLE_LOG=1` before starting the session to log reconciliations.
Each line reports `protocol_inhibitors` and `external_inhibitors` separately.
At startup, the line `session-bus idle inhibition ready` reports whether the
compositor acquired both service names. A missing session bus is non-fatal
and leaves the native Wayland mechanism available.
