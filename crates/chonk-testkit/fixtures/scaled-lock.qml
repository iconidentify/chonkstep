import QtQuick
import Quickshell
import Quickshell.Wayland

// A private, password-free locker using the same Qt role as Omarchy's lock.
Scope {
    WlSessionLock {
        locked: true
        WlSessionLockSurface {
            color: "#081840"
        }
    }
}
