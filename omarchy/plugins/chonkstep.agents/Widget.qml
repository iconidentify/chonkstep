import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import qs.Ui
import qs.Commons

Panel {
  id: root
  moduleName: "chonkstep.agents"
  ipcTarget: "chonkstep.agents"
  manageIpc: false
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  readonly property string bridge: Qt.resolvedUrl("bridge.py").toString().replace(/^file:\/\//, "")
  property var snapshot: ({sessions: [], offline: true})
  property string query: ""
  property string error: ""
  property var choices: []
  property string choiceSession: ""
  property int selected: 0
  property var pendingAction: ({})
  IpcHandler {
    target: "chonkstep.agents"
    function open(): void { root.open() }
    function close(): void { root.close() }
    function toggle(): void { root.toggle() }
    function openSession(id: string): void { root.open(); root.runAction({op: "open", id: id}) }
    function status(): string {
      return JSON.stringify({connected: root.connectedCount, working: root.workingCount,
        waiting: root.waitingCount, offline: !!root.snapshot.offline, open: root.opened,
        choices: root.choices.length, busy: actionProcess.running, error: root.error})
    }
  }
  readonly property var sessions: snapshot.sessions || []
  readonly property int connectedCount: sessions.filter(function(s) { return s.health === "connected" }).length
  readonly property int workingCount: sessions.filter(function(s) { return s.health === "connected" && s.status === "working" }).length
  readonly property int waitingCount: sessions.filter(function(s) { return s.health === "connected" && s.status === "waiting" }).length
  readonly property var rows: {
    var text = query.trim().toLowerCase()
    return sessions.filter(function(s) {
      return !text || [s.provider, s.title, s.cwd, s.session_id, s.detail, s.status].join(" ").toLowerCase().indexOf(text) >= 0
    }).slice().sort(function(a, b) {
      var priority = function(s) { return s.health !== "connected" ? 3 : s.status === "waiting" ? 0 : s.status === "working" ? 1 : 2 }
      return priority(a) - priority(b) || Number(b.updated || 0) - Number(a.updated || 0)
    })
  }
  readonly property var visibleRows: choices.length ? choices : rows
  onVisibleRowsChanged: selected = Math.min(selected, Math.max(0, visibleRows.length - 1))
  onOpenedChanged: {
    if (opened) { error = ""; choices = []; selected = 0 }
  }

  function providerName(s) {
    if (s.client === "t3") return "T3 Code"
    return ({codex: "Codex", claude: "Claude Code", opencode: "OpenCode", grok: "Grok"})[s.provider] || s.provider || "Agent"
  }
  function statusLabel(s) {
    if (s.health && s.health !== "connected") return s.health === "ended" ? "Ended" : "Disconnected"
    return ({working: "Working", waiting: "Needs attention", idle: "Ready", complete: "Complete", error: "Error"})[s.status] || "Unknown"
  }
  function statusColor(s) {
    if (s.health !== "connected") return Color.muted
    return s.status === "waiting" || s.status === "error" ? Color.urgent : s.status === "working" ? Color.accent : Color.foreground
  }
  function runAction(message) {
    if (actionProcess.running) return
    error = ""
    pendingAction = message
    actionProcess.running = true
  }
  function activateIndex(index) {
    if (index < 0 || index >= visibleRows.length) return
    selected = index
    if (choices.length) runAction({op: "activate", id: choiceSession, token: choices[index].token})
    else if (rows[index].health === "connected") runAction({op: "open", id: rows[index].id})
  }
  function move(delta) {
    selected = Math.max(0, Math.min(visibleRows.length - 1, selected + delta))
    list.positionViewAtIndex(selected, ListView.Contain)
  }

  Process {
    id: stream
    command: ["python3", "-u", root.bridge, "stream"]
    running: true
    stdout: SplitParser {
      onRead: function(data) {
        try {
          var value = JSON.parse(data)
          if (Array.isArray(value.sessions)) root.snapshot = value
          else if (value.error) root.error = value.error
        } catch (_) { root.error = "Unable to read agent sessions." }
      }
    }
    onExited: {
      root.snapshot = {sessions: [], offline: true}
      reconnect.restart()
    }
  }
  Timer { id: reconnect; interval: 2000; onTriggered: stream.running = true }
  Process {
    id: actionProcess
    command: ["python3", "-u", root.bridge, "action"]
    stdinEnabled: true
    onStarted: write(JSON.stringify(root.pendingAction) + "\n")
    stdout: StdioCollector {
      onStreamFinished: {
        try {
          var result = JSON.parse(text)
          if (!result.ok) root.error = result.error || "Unable to open this session."
          else if (result.opened) root.close()
          else if (result.choices) { root.choiceSession = result.id; root.choices = result.choices; root.selected = 0 }
        } catch (_) { root.error = "Unable to open this session." }
      }
    }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: "󰚩 " + (root.snapshot.offline ? "—" : String(root.connectedCount))
    active: root.waitingCount > 0
    tooltipText: "Agent sessions · " + root.workingCount + " working · " + root.waitingCount + " need attention"
    onPressed: root.toggle()
  }

  SessionPanel {
    id: popup
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keys
    contentWidth: fittedContentWidth(Style.space(520))
    contentHeight: fittedContentHeight(column.implicitHeight)

    PanelKeyCatcher {
      id: keys
      anchors.fill: parent
      blocked: search.activeFocus
      onMoveRequested: function(dx, dy) { if (dy) root.move(dy) }
      onActivateRequested: root.activateIndex(root.selected)
      onCloseRequested: { if (root.choices.length) root.choices = []; else root.close() }
      onTabRequested: search.forceActiveFocus()
    }
    ColumnLayout {
      id: column
      width: parent.width
      spacing: Style.space(10)
      RowLayout {
        Layout.fillWidth: true
        Text { text: root.choices.length ? "Choose a terminal" : "Agent sessions"; color: Color.foreground; font.family: Style.font.family; font.pixelSize: Style.font.body; font.bold: true; Layout.fillWidth: true }
        Text { text: actionProcess.running ? "Opening…" : root.snapshot.offline ? "Offline" : root.connectedCount + " connected"; color: Color.muted; font.family: Style.font.family; font.pixelSize: Style.font.bodySmall }
      }
      Text {
        visible: !root.choices.length
        text: root.workingCount + " working  ·  " + root.waitingCount + " need attention"
        color: root.waitingCount ? Color.urgent : Color.muted
        font.family: Style.font.family; font.pixelSize: Style.font.bodySmall
      }
      TextField {
        id: search
        Layout.fillWidth: true
        visible: !root.choices.length
        placeholderText: "Find a session, agent, or project"
        font.family: Style.font.family; font.pixelSize: Style.font.body
        color: Color.foreground
        placeholderTextColor: Color.muted
        background: Rectangle { color: Color.background; border.color: search.activeFocus ? Color.accent : Color.muted; radius: Style.space(4) }
        onTextChanged: { root.query = text; root.selected = 0 }
        Keys.onEscapePressed: { if (text) clear(); else root.close() }
        Keys.onDownPressed: { keys.forceActiveFocus(); root.move(1) }
        Keys.onReturnPressed: root.activateIndex(root.selected)
        Keys.onEnterPressed: root.activateIndex(root.selected)
      }
      Text {
        Layout.fillWidth: true
        visible: !!root.error
        text: root.error; textFormat: Text.PlainText; wrapMode: Text.Wrap
        color: Color.urgent; font.family: Style.font.family; font.pixelSize: Style.font.bodySmall
      }
      Text {
        visible: root.visibleRows.length === 0
        Layout.fillWidth: true
        text: root.snapshot.offline ? "The session service is reconnecting…" : root.query ? "No matching sessions." : "Start or resume Codex, Claude Code, or OpenCode to see it here."
        textFormat: Text.PlainText; wrapMode: Text.Wrap
        color: Color.muted; font.family: Style.font.family; font.pixelSize: Style.font.body
      }
      ListView {
        id: list
        Layout.fillWidth: true
        Layout.preferredHeight: Math.min(contentHeight, Style.space(380), Math.max(Style.space(90), popup.availableCardHeight - Style.space(190)))
        clip: true
        spacing: Style.space(5)
        model: root.visibleRows
        boundsBehavior: Flickable.StopAtBounds
        ScrollBar.vertical: ScrollBar {}
        delegate: Rectangle {
          id: row
          required property var modelData
          required property int index
          width: list.width
          height: body.implicitHeight + Style.space(18)
          color: index === root.selected ? Color.background : "transparent"
          border.color: index === root.selected ? Color.accent : "transparent"
          radius: Style.space(4)
          ColumnLayout {
            id: body
            x: Style.space(10); y: Style.space(9); width: parent.width - Style.space(20)
            spacing: Style.space(3)
            RowLayout {
              Layout.fillWidth: true
              Text { Layout.fillWidth: true; text: root.choices.length ? row.modelData.label : root.providerName(row.modelData) + (row.modelData.agent_id ? " · Subagent" : "") + " · " + (row.modelData.title || "Session"); textFormat: Text.PlainText; elide: Text.ElideRight; color: Color.foreground; font.family: Style.font.family; font.pixelSize: Style.font.body; font.bold: true }
              Text { visible: !root.choices.length; text: root.statusLabel(row.modelData); color: root.statusColor(row.modelData); font.family: Style.font.family; font.pixelSize: Style.font.bodySmall }
            }
            Text { visible: !root.choices.length; Layout.fillWidth: true; text: row.modelData.cwd || ""; textFormat: Text.PlainText; elide: Text.ElideMiddle; color: Color.muted; font.family: Style.font.family; font.pixelSize: Style.font.bodySmall }
            Text { visible: !root.choices.length; Layout.fillWidth: true; text: (row.modelData.detail || "") + "  ·  " + String(row.modelData.agent_id || row.modelData.session_id || row.modelData.id).slice(0, 8); textFormat: Text.PlainText; elide: Text.ElideRight; color: Color.muted; font.family: Style.font.family; font.pixelSize: Style.font.bodySmall }
          }
          MouseArea { anchors.fill: parent; hoverEnabled: true; cursorShape: root.choices.length || row.modelData.health === "connected" ? Qt.PointingHandCursor : Qt.ArrowCursor; onEntered: root.selected = row.index; onClicked: root.activateIndex(row.index) }
        }
      }
      Text { text: root.choices.length ? "Esc  Back" : "↑ ↓  Select    Enter  Open session    Esc  Close"; color: Color.muted; font.family: Style.font.family; font.pixelSize: Style.font.bodySmall }
    }
  }
}
