import QtQuick 2.0
import SddmComponents 2.0

// Keep the login surface visually native to Omarchy without modifying or
// copying its theme. The integration command enables this thin overlay only
// on Omarchy, where these assets are part of the installed desktop.
Rectangle {
    id: root
    width: 640
    height: 480
    color: "#1a1b26"

    readonly property string omarchyTheme: "file:///usr/share/sddm/themes/omarchy/"
    property string currentUser: userModel.lastUser
    property bool loginFailed: false
    property bool sessionChosenByUser: false

    function selectChonkstepSession(index, name) {
        if (!sessionChosenByUser && name === "chonkstep (uwsm)")
            session.index = index
    }

    // Qt 6's SessionModel exposes its name role to delegates even on builds
    // where calling data(Qt.DisplayRole) from JavaScript yields an empty value.
    // A Repeater also follows rows inserted after the theme was instantiated.
    Repeater {
        model: sessionModel

        Item {
            width: 0
            height: 0
            property variant sessionEntry: model

            Component.onCompleted: root.selectChonkstepSession(index, sessionEntry.name)
        }
    }

    Connections {
        target: sddm

        function onLoginFailed() {
            root.loginFailed = true
            password.text = ""
            password.forceActiveFocus()
        }

        function onLoginSucceeded() {
            root.loginFailed = false
        }
    }

    Column {
        anchors.centerIn: parent
        spacing: 32

        Image {
            id: logo
            source: root.omarchyTheme + "logo.png"
            width: Math.min(sourceSize.width, root.width * 0.8)
            height: sourceSize.width > 0 ? Math.round(width * sourceSize.height / sourceSize.width) : 0
            fillMode: Image.PreserveAspectFit
            anchors.horizontalCenter: parent.horizontalCenter
        }

        Row {
            anchors.horizontalCenter: parent.horizontalCenter
            spacing: 15

            Image {
                source: root.omarchyTheme + (root.loginFailed ? "lock-failed.png" : "lock.png")
                width: 34
                height: 38
                fillMode: Image.PreserveAspectFit
                anchors.verticalCenter: parent.verticalCenter
            }

            Item {
                width: entry.width
                height: entry.height

                Image {
                    id: entry
                    source: root.omarchyTheme + (root.loginFailed ? "entry-failed.png" : "entry.png")
                    anchors.centerIn: parent
                }

                Row {
                    anchors.left: parent.left
                    anchors.leftMargin: 20
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 5

                    Repeater {
                        model: Math.min(password.text.length, 21)

                        Image {
                            source: root.omarchyTheme + "bullet.png"
                            width: 7
                            height: 7
                        }
                    }
                }

                TextInput {
                    id: password
                    anchors.fill: parent
                    anchors.leftMargin: 20
                    anchors.rightMargin: 20
                    verticalAlignment: TextInput.AlignVCenter
                    echoMode: TextInput.Password
                    font.family: "JetBrainsMono Nerd Font"
                    font.pixelSize: 24
                    font.letterSpacing: 5
                    passwordCharacter: "\u2022"
                    color: "transparent"
                    selectionColor: "transparent"
                    selectedTextColor: "transparent"
                    cursorDelegate: Item {}
                    focus: true

                    onTextChanged: root.loginFailed = false

                    Keys.onPressed: {
                        if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                            sddm.login(root.currentUser, password.text, session.index)
                            event.accepted = true
                        }
                    }
                }
            }
        }

        ComboBox {
            id: session
            width: 335
            height: 30
            anchors.horizontalCenter: parent.horizontalCenter
            model: sessionModel
            index: sessionModel.lastIndex
            color: "transparent"
            menuColor: "#1a1b26"
            borderColor: "#414868"
            focusColor: "#7aa2f7"
            hoverColor: "#24283b"
            textColor: "#a9b1d6"
            arrowColor: "#24283b"
            font.family: "JetBrainsMono Nerd Font"
            font.pixelSize: 12
            onValueChanged: root.sessionChosenByUser = true

            Text {
                width: 22
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                horizontalAlignment: Text.AlignHCenter
                text: "\u25be"
                color: "#a9b1d6"
                font.family: "JetBrainsMono Nerd Font"
                font.pixelSize: 11
            }
        }
    }

    Component.onCompleted: {
        password.forceActiveFocus()
    }
}
