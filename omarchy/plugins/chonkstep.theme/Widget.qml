import QtQuick
import qs.Ui

// The name of chonkstep's active theme, e.g. "NeXTSTEP Classic · dark". A
// presence indicator, not a control: theme switching already has a route
// (the Themes submenu, omarchy-theme-set) and the socket's verb set is
// deliberately closed. Zero width until the socket says hello.
BarWidget {
  id: root
  moduleName: "chonkstep.theme"

  readonly property bool showAppearance: setting("showAppearance", true) !== false
  readonly property var theme: link.connected ? link.theme : null
  readonly property string text: {
    if (!theme || !theme.name) return ""
    return showAppearance && theme.appearance ? theme.name + " · " + theme.appearance : theme.name
  }

  ControlSocket { id: link }

  // A theme name does not fit across a 28px vertical bar and has no
  // icon-only form worth showing, so there it steps aside entirely.
  visible: text !== "" && !root.vertical
  implicitWidth: visible ? label.implicitWidth : 0
  implicitHeight: visible ? label.implicitHeight : 0

  WidgetButton {
    id: label
    anchors.fill: parent
    bar: root.bar
    text: root.text
    pressable: false
    tooltipText: !root.theme ? "" : root.theme.following === "omarchy"
      ? root.theme.id + " (following Omarchy's palette)" : root.theme.id
  }
}
