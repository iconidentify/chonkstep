import QtQuick
import Quickshell
import Quickshell.Io

// One client of chonkstep's control socket (docs/control-socket.md in the
// chonkstep repo). It owns the connection, keeps the last message of every
// facet the shell publishes, and reconnects when the socket goes away.
//
// This file is copied verbatim into every chonkstep.* plugin. Omarchy
// installs each plugin as its own git checkout under
// ~/.config/omarchy/plugins/<id>/ and a plugin can only reach files inside
// its own directory, so a shared module has nowhere to live; a few dozen
// duplicated lines is the price of `omarchy plugin add` just working.
// Keep the copies identical (omarchy/tools/check-plugins.sh diffs them).
//
// A QtObject rather than an Item: it draws nothing, and Item's own `focus`
// property would collide with the facet of that name.
QtObject {
  id: root

  // Set to point somewhere other than the running session, e.g. at the fake
  // server in omarchy/tools while developing. Empty means "the session's".
  property string path: ""

  // The shell exports CHONKSTEP_CONTROL_SOCKET to everything it launches,
  // omarchy-shell included; deriving the path is the fallback for a shell
  // started from somewhere else (a terminal, a systemd unit) and mirrors
  // §1.1 of the spec byte for byte.
  readonly property string resolvedPath: path !== "" ? path : root.sessionPath()

  // Connected means "has said hello with a protocol this plugin speaks",
  // not merely "the socket is open": a widget bound to this never draws
  // from a server it has not understood.
  readonly property bool connected: link.item !== null && link.item.connected && root.protocol !== 0

  // The most recent complete statement of each facet, or null before the
  // snapshot arrives. Every event is complete (no diffs), so a widget can
  // bind straight to these and be correct after any single message.
  property var workspaces: null
  property var outputs: null
  property var focus: null
  property var theme: null
  property int protocol: 0

  // Every parsed message, for a widget that wants a facet this component
  // does not keep, or the `error` replies to its own requests.
  signal received(var message)

  function sessionPath() {
    var exported = Quickshell.env("CHONKSTEP_CONTROL_SOCKET")
    if (exported) return String(exported)
    var runtimeDir = Quickshell.env("XDG_RUNTIME_DIR")
    if (!runtimeDir) return ""
    var display = Quickshell.env("WAYLAND_DISPLAY") || Quickshell.env("DISPLAY") || "default"
    display = String(display).replace(/^:+/, "").replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 32)
    // A display that sanitises to nothing (":" alone) is "default", as in
    // chonk-dock-proto's sanitize_display; the shell listens there too.
    if (display === "") display = "default"
    return String(runtimeDir) + "/chonkstep/control-" + display + ".sock"
  }

  // Send one request. Anything not an object is dropped rather than
  // written: an empty line is harmless but a malformed one is a framing
  // error the shell answers by hanging up.
  function request(message) {
    if (!connected || !message || typeof message !== "object") return
    link.item.write(JSON.stringify(message) + "\n")
    link.item.flush()
  }

  function clear() {
    workspaces = null
    outputs = null
    focus = null
    theme = null
    protocol = 0
  }

  function handle(line) {
    var text = String(line).trim()
    if (text === "") return
    var message
    try {
      message = JSON.parse(text)
    } catch (e) {
      console.warn("chonkstep control socket: not JSON, ignoring: " + text.slice(0, 120))
      return
    }
    if (!message || typeof message !== "object") return
    switch (message.event) {
    case "hello":
      // A version this client does not know is the one case the spec says
      // to hang up on rather than guess. Staying dark until the plugin is
      // reloaded beats a strip that draws the wrong thing.
      if (message.protocol !== 1) {
        console.warn("chonkstep control socket: protocol " + message.protocol
          + " is not the 1 this plugin speaks; giving up")
        root.giveUp = true
        link.active = false
        return
      }
      protocol = message.protocol
      break
    case "workspaces": workspaces = message; break
    case "outputs": outputs = message; break
    case "focus": focus = message; break
    case "theme": theme = message; break
    case "error":
      console.warn("chonkstep control socket: " + message.request + ": " + message.message)
      break
    default:
      // Unknown facets are how the shell grows within a version.
      break
    }
    received(message)
  }

  // ------------------------------------------------------------ connection

  property bool giveUp: false
  property int retryDelay: 250

  // Whether this path has ever answered. A socket that was there and went
  // away is a chonkstep restarting, worth polling briskly for; one that
  // has never existed is a bar running under some other compositor, and
  // Quickshell warns on every failed connect, so after enough misses the
  // retry ceiling widens from five seconds to a minute.
  property bool seen: false
  property int misses: 0
  readonly property int patientAfter: 8

  // The Socket lives behind a Loader so a reconnect is a fresh object. A
  // Quickshell Socket whose connect attempt failed (no such file — the
  // compositor is mid-restart) keeps its dead QLocalSocket around, and
  // setting `connected` on it again is a no-op; tearing the object down
  // and making another is the one retry that does not depend on that.
  function reconnect() {
    link.active = false
    if (root.resolvedPath !== "" && !root.giveUp) link.active = true
  }

  property Loader link: Loader {
    id: link
    active: false
    sourceComponent: Socket {
      path: root.resolvedPath
      connected: true
      parser: SplitParser {
        onRead: function(line) { root.handle(line) }
      }
      onConnectionStateChanged: {
        if (connected) {
          root.retryDelay = 250
          root.seen = true
          root.misses = 0
        } else {
          root.clear()
          retry.restart()
        }
      }
      // ServerNotFound and PeerClosed both land here; the disconnect that
      // follows PeerClosed restarts the same timer, harmlessly.
      onError: retry.restart()
    }
    onLoaded: root.clear()
  }

  // Bindings settle (and fire their change signals) before onCompleted, so
  // without this guard a fresh component would connect twice and hang up
  // once — visible on the shell side as a connection reset per widget.
  property bool ready: false

  Component.onCompleted: {
    root.ready = true
    reconnect()
  }

  onResolvedPathChanged: {
    root.giveUp = false
    root.retryDelay = 250
    root.seen = false
    root.misses = 0
    if (root.ready) reconnect()
  }

  // Exponential backoff from a quarter second to five: a hot restart of
  // the compositor is back within a second or two and the strip should
  // reappear promptly. A socket never seen at all is a different case —
  // a session without chonkstep — and after `patientAfter` misses the
  // ceiling becomes a minute, so the bar is not warning every five
  // seconds for a socket that will never exist. The timer fires once per
  // failed attempt (it is restarted on every disconnect), so counting
  // here counts attempts, not the two signals one failure can raise.
  property Timer retry: Timer {
    id: retry
    interval: root.retryDelay
    repeat: false
    onTriggered: {
      root.misses += 1
      var ceiling = root.seen || root.misses < root.patientAfter ? 5000 : 60000
      root.retryDelay = Math.min(root.retryDelay * 2, ceiling)
      root.reconnect()
    }
  }
}
