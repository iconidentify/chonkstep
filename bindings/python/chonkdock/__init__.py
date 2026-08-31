"""chonkdock: the chonkstep dockapp protocol for Python, stdlib only.

A dockapp is a separate process that draws one (or a few) chonkstep
dock tiles over a private socket. It crashes alone, restyles without
restarting, and survives shell restarts. See
``docs/dockapp-protocol.md`` in the chonkstep repository for the wire
contract this package implements.

The ten-line version::

    from chonkdock import Dockapp

    class Hello(Dockapp):
        def draw(self, ctx, buf):
            for i in range(0, len(buf), 4):
                buf[i:i + 4] = b"\\x30\\x60\\x90\\xff"
            return True

    Hello("hello-instrument").run()

Register it with a ``.dockapp`` file (see the protocol document or
``scripts/chonk-get``) and the dock launches, supervises and restarts
it for you.
"""

from .client import (  # noqa: F401
    Ctx,
    Dockapp,
    DockappError,
    Refused,
    DEFAULT_REDRAW_INTERVAL,
)
from .wire import (  # noqa: F401
    BUTTON_LEFT,
    BUTTON_MIDDLE,
    BUTTON_RIGHT,
    INPUT_ENTER,
    INPUT_LEAVE,
    INPUT_PRESS,
    INPUT_RELEASE,
    INPUT_SCROLL,
    LOG_DEBUG,
    LOG_ERROR,
    LOG_INFO,
    LOG_WARN,
    MAX_TILE_UNITS,
    PROTOCOL_VERSION,
    WANT_ALL,
    WANT_CROSSING,
    WANT_PRESS,
    WANT_RELEASE,
    WANT_SCROLL,
    DecodeError,
    EncodeError,
    InputEvent,
    ThemeState,
    frame_fits,
    is_valid_id,
)

__all__ = [
    "Dockapp", "Ctx", "DockappError", "Refused", "InputEvent",
    "ThemeState", "DecodeError", "EncodeError", "frame_fits",
    "is_valid_id", "PROTOCOL_VERSION",
]
__version__ = "0.1.0"
