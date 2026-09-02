import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Ui

// The workspace strip, drawn from chonkstep's control socket instead of
// Hyprland's. Same glyphs, spacing and dimming as omarchy.workspaces so a
// user swapping one for the other sees nothing move.
//
// Two things are deliberately different. There is no fixed 1–5: chonkstep
// grows workspaces on demand, so the strip shows exactly the workspaces
// that exist. And when the socket is not there — no chonkstep, or one in
// the middle of a hot restart — the widget has no width at all, rather
// than five numbers that mean nothing.
BarWidget {
  id: root
  moduleName: "chonkstep.workspaces"

  // Leave empty workspaces out of the strip instead of dimming them. Off
  // by default to match the first-party widget, where an empty workspace
  // is still a place you can click to go.
  readonly property bool hideEmpty: setting("hideEmpty", false) === true

  readonly property int active: link.workspaces ? link.workspaces.active : -1
  readonly property var tiles: {
    if (!link.connected || !link.workspaces || !Array.isArray(link.workspaces.workspaces)) return []
    var list = link.workspaces.workspaces
    if (!root.hideEmpty) return list
    return list.filter(function(entry) { return entry.windows > 0 || entry.index === root.active })
  }

  // Wire indices are 0-based; the label a person reads is 1-based, and the
  // conversion happens here at the edge and nowhere else.
  function label(index) {
    return String(index + 1)
  }

  function focusWorkspace(index) {
    link.request({ request: "focus-workspace", index: index })
  }

  ControlSocket { id: link }

  readonly property real trailingGap: root.vertical ? 0 : Style.spaceReal(1.5)

  visible: tiles.length > 0
  implicitWidth: visible ? grid.implicitWidth + trailingGap : 0
  implicitHeight: visible ? grid.implicitHeight : 0

  GridLayout {
    id: grid
    anchors.fill: parent
    anchors.rightMargin: root.trailingGap
    columns: root.vertical ? 1 : Math.max(1, root.tiles.length)
    columnSpacing: root.vertical ? 0 : Style.space(1)
    rowSpacing: root.vertical ? Style.space(2) : 0

    Repeater {
      model: root.tiles

      WidgetButton {
        required property var modelData

        readonly property bool occupied: modelData.windows > 0
        readonly property bool focused: modelData.index === root.active

        bar: root.bar
        // U+F14FB, the filled dot omarchy.workspaces marks the focused
        // workspace with; spelled as the same surrogate pair it uses.
        text: focused ? "\uDB85\uDCFB" : root.label(modelData.index)
        opacity: occupied || focused ? 1 : 0.5
        horizontalMargin: 6
        verticalPadding: 6
        fixedWidth: root.vertical ? root.barSize : Style.space(20)
        fixedHeight: root.barSize
        onPressed: function() { root.focusWorkspace(modelData.index) }
      }
    }
  }
}
