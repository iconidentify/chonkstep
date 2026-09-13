import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

spec = importlib.util.spec_from_file_location("bar_bridge", Path(__file__).resolve().parents[1] / "bridge.py")
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class Actions(unittest.TestCase):
    def setUp(self):
        self.row = {"id": "live-session", "schema": 2, "health": "connected", "client": "terminal"}
        self.agent = SimpleNamespace(request=Mock(return_value={"sessions": [self.row]}))
        self.nav = SimpleNamespace(discover=Mock(return_value=[{"token": "verified", "label": "Terminal"}]), activate=Mock())

    def test_opens_one_verified_destination(self):
        self.assertTrue(bridge.action(self.agent, {"op": "open", "id": "live-session"}, self.nav)["opened"])
        self.nav.activate.assert_called_once_with(self.row, "verified")
        self.assertEqual(self.agent.request.call_count, 2)

    def test_ambiguous_terminals_require_selection(self):
        self.nav.discover.return_value = [{"token": "one"}, {"token": "two"}]
        result = bridge.action(self.agent, {"op": "open", "id": "live-session"}, self.nav)
        self.assertEqual(len(result["choices"]), 2)
        self.nav.activate.assert_not_called()
        bridge.action(self.agent, {"op": "activate", "id": "live-session", "token": "two"}, self.nav)
        self.nav.activate.assert_called_once_with(self.row, "two")

    def test_session_disconnecting_during_discovery_cannot_be_activated(self):
        self.agent.request.side_effect = [{"sessions": [self.row]}, {"sessions": []}]
        with self.assertRaisesRegex(ValueError, "no longer connected"):
            bridge.action(self.agent, {"op": "open", "id": "live-session"}, self.nav)
        self.nav.activate.assert_not_called()

    def test_disconnected_or_missing_sessions_do_not_navigate(self):
        for rows in [[], [{**self.row, "health": "disconnected"}], [{**self.row, "schema": 1}]]:
            self.agent.request.return_value = {"sessions": rows}
            with self.assertRaises(ValueError):
                bridge.action(self.agent, {"op": "open", "id": "live-session"}, self.nav)
        self.nav.discover.assert_not_called()
        self.nav.activate.assert_not_called()

    def test_browser_action_requires_current_capability(self):
        self.row.update(client="t3", capabilities={"open_browser": False})
        browser = Mock()
        with self.assertRaises(ValueError):
            bridge.action(self.agent, {"op": "open", "id": "live-session"}, self.nav, browser)
        browser.assert_not_called()
        self.row["capabilities"]["open_browser"] = True
        self.assertTrue(bridge.action(self.agent, {"op": "open", "id": "live-session"}, self.nav, browser)["opened"])
        browser.assert_called_once_with(self.row)

    def test_unknown_operations_do_not_read_or_execute(self):
        with self.assertRaises(ValueError):
            bridge.action(self.agent, {"op": "shell", "command": "false"}, self.nav)
        self.agent.request.assert_not_called()


if __name__ == "__main__":
    unittest.main()
