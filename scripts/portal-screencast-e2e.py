#!/usr/bin/env python3
"""Capture real frames through the public ScreenCast portal API.

This deliberately talks to org.freedesktop.portal.ScreenCast rather than the
backend API. It exercises the same CreateSession -> SelectSources -> Start ->
OpenPipeWireRemote sequence as a browser, then consumes the returned PipeWire
node with GStreamer.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import secrets
import subprocess
import sys

import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib


BUS_NAME = "org.freedesktop.portal.Desktop"
OBJECT_PATH = "/org/freedesktop/portal/desktop"
SCREENCAST_IFACE = "org.freedesktop.portal.ScreenCast"
REQUEST_IFACE = "org.freedesktop.portal.Request"
SESSION_IFACE = "org.freedesktop.portal.Session"


class PortalError(RuntimeError):
    pass


def request_path(bus: dbus.SessionBus, token: str) -> str:
    sender = bus.get_unique_name().removeprefix(":").replace(".", "_")
    return f"/org/freedesktop/portal/desktop/request/{sender}/{token}"


def portal_request(
    bus: dbus.SessionBus,
    call,
    args: tuple,
    options: dict,
    timeout_seconds: int,
) -> dict:
    token = f"chonkstep_{secrets.token_hex(6)}"
    options = {**options, "handle_token": token}
    expected_path = request_path(bus, token)
    response: dict[str, object] = {}
    loop = GLib.MainLoop()

    def on_response(code, results) -> None:
        response["code"] = int(code)
        response["results"] = dict(results)
        loop.quit()

    def on_timeout() -> bool:
        response["timeout"] = True
        loop.quit()
        return GLib.SOURCE_REMOVE

    match = bus.add_signal_receiver(
        on_response,
        signal_name="Response",
        dbus_interface=REQUEST_IFACE,
        bus_name=BUS_NAME,
        path=expected_path,
    )
    timeout_id = GLib.timeout_add_seconds(timeout_seconds, on_timeout)
    try:
        returned_path = str(call(*args, options))
        if returned_path != expected_path:
            raise PortalError(
                f"portal returned unexpected request path {returned_path!r}; "
                f"expected {expected_path!r}"
            )
        loop.run()
    finally:
        match.remove()
        if not response.get("timeout"):
            GLib.source_remove(timeout_id)

    if response.get("timeout"):
        raise PortalError(f"portal request timed out after {timeout_seconds}s")
    if response.get("code") != 0:
        raise PortalError(
            f"portal request was rejected: code={response.get('code')} "
            f"results={response.get('results')}"
        )
    return response["results"]


def capture(output: Path, buffers: int, timeout_seconds: int) -> tuple[int, tuple[int, int]]:
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    desktop = bus.get_object(BUS_NAME, OBJECT_PATH)
    screencast = dbus.Interface(desktop, SCREENCAST_IFACE)
    properties = dbus.Interface(desktop, "org.freedesktop.DBus.Properties")

    version = int(properties.Get(SCREENCAST_IFACE, "version"))
    source_types = int(properties.Get(SCREENCAST_IFACE, "AvailableSourceTypes"))
    if not source_types & 1:
        raise PortalError(f"monitor capture is unavailable (source types={source_types})")
    print(f"ScreenCast portal version={version} AvailableSourceTypes={source_types}")

    session_token = f"chonkstep_session_{secrets.token_hex(6)}"
    created = portal_request(
        bus,
        screencast.CreateSession,
        (),
        {"session_handle_token": session_token},
        timeout_seconds,
    )
    session_path = str(created["session_handle"])
    session = bus.get_object(BUS_NAME, session_path)

    try:
        portal_request(
            bus,
            screencast.SelectSources,
            (dbus.ObjectPath(session_path),),
            {"types": dbus.UInt32(1), "multiple": dbus.Boolean(False)},
            timeout_seconds,
        )
        started = portal_request(
            bus,
            screencast.Start,
            (dbus.ObjectPath(session_path), ""),
            {},
            timeout_seconds,
        )
        streams = list(started["streams"])
        if len(streams) != 1:
            raise PortalError(f"expected one stream, got {streams!r}")

        node_id = int(streams[0][0])
        metadata = dict(streams[0][1])
        size = tuple(int(value) for value in metadata.get("size", (0, 0)))
        print(f"ScreenCast stream node={node_id} size={size}")

        remote = screencast.OpenPipeWireRemote(
            dbus.ObjectPath(session_path), {}, byte_arrays=True
        )
        pipewire_fd = remote.take()
        try:
            output.parent.mkdir(parents=True, exist_ok=True)
            command = [
                "gst-launch-1.0",
                "-q",
                "pipewiresrc",
                f"fd={pipewire_fd}",
                f"path={node_id}",
                f"num-buffers={buffers}",
                "!",
                "videoconvert",
                "!",
                "video/x-raw,format=RGB",
                "!",
                "pnmenc",
                "!",
                "filesink",
                f"location={output}",
            ]
            subprocess.run(
                command,
                check=True,
                pass_fds=(pipewire_fd,),
                timeout=timeout_seconds,
            )
        finally:
            os.close(pipewire_fd)
    finally:
        dbus.Interface(session, SESSION_IFACE).Close()

    if not output.is_file() or output.stat().st_size < 1024:
        raise PortalError(f"capture did not produce a usable frame: {output}")
    return node_id, size


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("/tmp/chonkstep-portal-frame.ppm"),
        help="PPM destination (default: %(default)s)",
    )
    parser.add_argument("--buffers", type=int, default=5)
    parser.add_argument("--timeout", type=int, default=30)
    args = parser.parse_args()

    if args.buffers < 1:
        parser.error("--buffers must be at least 1")
    try:
        node_id, size = capture(args.output, args.buffers, args.timeout)
    except (PortalError, dbus.DBusException, subprocess.SubprocessError) as exc:
        print(f"portal-screencast-e2e: {exc}", file=sys.stderr)
        return 1

    print(
        f"portal-screencast-e2e: captured node {node_id} ({size[0]}x{size[1]}) "
        f"to {args.output} ({args.output.stat().st_size} bytes)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
