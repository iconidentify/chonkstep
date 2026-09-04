# No Omarchy command shims are required

This directory is intentionally empty. The former logout shim was
withdrawn when chonkstep gained a uwsm-managed session and clean
SIGTERM logout. The earlier shell, launch-or-focus, restart-shell and
night-light shims became unnecessary when the compositor implemented
their IPC and Wayland protocols directly.

Keeping a copied Omarchy command here would create a second version
that can drift. `docs/omarchy-integration.md` records the native paths
and the remaining deliberate compatibility boundaries.
