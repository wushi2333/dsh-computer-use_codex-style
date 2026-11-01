from __future__ import annotations

import unittest

from computer_use.driver import ComputerUse
from computer_use.executor import ToolExecutor
from computer_use.fake_backend import FakeDesktop


class BrowserDispatchTests(unittest.TestCase):
    """Dispatch-level semantics. The schema tests in test_parity.py cannot see
    these: BR-03/04/05/06 all survived the old suite because it only read schemas."""

    def setUp(self) -> None:
        self.ex = ToolExecutor(ComputerUse(FakeDesktop()))
        self.surface = self.ex.browser
        self.tab = self.ex.execute("tab_new", {"url": "https://example.com/"})["result"]
        self.tid = self.tab["id"]

    def test_mark_tab_deliverable_is_not_handoff(self) -> None:
        delivered = self.surface.dispatch("mark_tab", {"tab_id": self.tid, "status": "deliverable"})
        self.assertTrue(delivered["deliverable"])
        self.assertFalse(delivered["handoff"])
        other = self.ex.execute("tab_new", {"url": "https://example.com/other"})["result"]
        handoff = self.surface.dispatch("mark_tab", {"tab_id": other["id"], "status": "handoff"})
        self.assertTrue(handoff["handoff"])
        self.assertFalse(handoff["deliverable"])

    def test_tab_ax_action_routes_all_eight_members(self) -> None:
        actions = [
            {"type": "click", "target": 2},
            {"type": "click", "target": [10, 20]},
            {"type": "drag", "from": [1, 2], "to": [3, 4]},
            {"type": "perform_secondary_action", "element_index": 2, "action": "Invoke"},
            {"type": "press_key", "key": "Return"},
            {"type": "scroll", "target": 0, "direction": "d"},
            {"type": "select_text", "element_index": 3, "text": "Search"},
            {"type": "set_value", "element_index": 3, "value": "hello"},
            {"type": "type_text", "text": "world"},
        ]
        for action in actions:
            result = self.surface.dispatch("tab_ax_action", {"tab_id": self.tid, "action": action})
            self.assertIsInstance(result, dict, action["type"])
        self.assertEqual(result["value"], "world")

    def test_dialog_dismiss_is_not_accept(self) -> None:
        self.surface.dispatch(
            "tab_inject_dialog",
            {"tab_id": self.tid, "type": "beforeunload", "message": "leave?"},
        )
        dismissed = self.surface.dispatch(
            "tab_handle_js_dialog", {"tab_id": self.tid, "action": "dismiss"}
        )
        self.assertFalse(dismissed["accepted"])
        self.surface.dispatch("tab_inject_dialog", {"tab_id": self.tid, "type": "prompt"})
        accepted = self.surface.dispatch(
            "tab_handle_js_dialog",
            {"tab_id": self.tid, "action": "accept", "prompt_text": "bob"},
        )
        self.assertTrue(accepted["accepted"])
        self.assertEqual(accepted["text"], "bob")

    def test_ax_get_state_maps_content_to_mode(self) -> None:
        shot = self.surface.dispatch(
            "tab_ax_get_state", {"tab_id": self.tid, "content": "screenshot"}
        )
        self.assertEqual(shot["mode"], "screenshot")
        # Screenshot bytes ride as a data-URL on the entry so detach_images
        # can harvest them into the vision images[] channel (F9).
        self.assertTrue(shot["screenshots"][0]["url"].startswith("data:image/png;base64,"))
        state = self.surface.dispatch("tab_ax_get_state", {"tab_id": self.tid})
        self.assertEqual(state["mode"], "state")
        # Official default returns a diff, not a forced full tree (BR-06).
        self.assertFalse(state["accessibility"]["diff"])

    def test_screenshot_unavailable_is_reported(self) -> None:
        self.surface.browser.tab(self.tid).screenshot_unavailable = "screen capture unavailable"
        with self.assertRaises(RuntimeError) as raised:
            self.surface.dispatch(
                "tab_ax_get_state", {"tab_id": self.tid, "content": "screenshot"}
            )
        self.assertIn("screen capture unavailable", str(raised.exception))

    def test_navigate_tab_url_is_implemented(self) -> None:
        result = self.surface.dispatch(
            "navigate_tab_url", {"tab_id": self.tid, "url": "https://example.com/next"}
        )
        self.assertEqual(result["url"], "https://example.com/next")
        self.assertEqual(result["id"], self.tid)

    def test_navigate_tab_url_requires_a_url_and_a_tab(self) -> None:
        with self.assertRaises(ValueError):
            self.surface.dispatch("navigate_tab_url", {"tab_id": self.tid, "url": ""})
        with self.assertRaises(ValueError):
            self.surface.dispatch("navigate_tab_url", {"url": "https://example.com/"})

    def test_browser_user_claim_tab_uses_open_tabs(self) -> None:
        self.surface.hub.ingest(
            {
                "type": "hello",
                "instanceId": "inst-1",
                "family": "chrome",
                "tabs": [
                    {
                        "id": "7",
                        "providerTabId": "7",
                        "title": "Gmail",
                        "url": "https://mail.google.com/",
                        "extensionInstanceId": "inst-1",
                    }
                ],
            }
        )
        result = self.surface.dispatch("browser_user_claim_tab", {"tab_id": "7"})
        self.assertTrue(result["claimed"])
        missing = self.surface.dispatch("browser_user_claim_tab", {"tab_id": "999"})
        self.assertFalse(missing["claimed"])

    def test_response_meta_prefers_the_last_mutating_command(self) -> None:
        self.surface.dispatch(
            "navigate_tab_url", {"tab_id": self.tid, "url": "https://example.com/z"}
        )
        self.surface.dispatch("tab_ax_get_state", {"tab_id": self.tid})
        meta = self.surface.response_meta()
        self.assertEqual(meta["commandType"], "navigate_tab_url")

    def test_official_playwright_schema_drives_a_locator(self) -> None:
        # Official playwright_locator_* carries `selector`; the dispatcher must
        # bridge it onto the DSH locator handle (BR-01 follow-through).
        count = self.surface.dispatch(
            "playwright_locator_count", {"tab_id": self.tid, "selector": "a"}
        )
        self.assertEqual(count["count"], 1)
        text = self.surface.dispatch(
            "playwright_locator_inner_text", {"tab_id": self.tid, "selector": "a"}
        )
        self.assertEqual(text["text"], "More information")

    def test_required_documentation_gate(self) -> None:
        # BR-18: official documents.json requiredFor refuses the command until
        # the documentation has been read.
        with self.assertRaises(RuntimeError) as raised:
            self.surface.dispatch("webmcp_list_tools", {"tab_id": self.tid})
        self.assertIn("Required documentation has not been read", str(raised.exception))
        self.surface.dispatch("documentation_get", {"name": "confirmations"})
        self.surface.dispatch("documentation_get", {"name": "webmcp"})
        result = self.surface.dispatch("webmcp_list_tools", {"tab_id": self.tid})
        self.assertIn("tools", result)

    def test_expected_url_is_injected_for_user_tab_reads(self) -> None:
        # BR-16: official ensureCommandAllowed writes params.expected_url so a
        # tab that moved underneath the agent fails closed.
        with self.assertRaises(RuntimeError):
            self.surface.dispatch(
                "tab_get",
                {"tab_id": self.tid, "expected_url": "https://moved.test/"},
            )
        ok = self.surface.dispatch(
            "tab_get", {"tab_id": self.tid, "expected_url": "https://example.com/"}
        )
        self.assertEqual(ok["id"], self.tid)
        spec: dict[str, object] = {"tab_id": self.tid}
        self.surface._ensure_command_allowed("tab_get", spec)
        self.assertEqual(spec.get("expected_url"), "https://example.com/")


class OfficialCommandCoverageTests(unittest.TestCase):
    """Every registered official command must reach an implementation.

    A registered-but-unrouted command raises KeyError from BrowserSurface.dispatch;
    this walks the whole catalog with a minimally valid payload so BR-01 cannot
    silently regress."""

    def _value(self, schema: dict, tab_id: str) -> object:
        if not isinstance(schema, dict):
            return "x"
        if "oneOf" in schema and schema["oneOf"]:
            return self._value(schema["oneOf"][0], tab_id)
        kind = schema.get("type")
        if kind == "integer":
            return 1
        if kind == "number":
            return 1.0
        if kind == "boolean":
            return True
        if kind == "array":
            return [self._value(schema.get("items") or {}, tab_id)]
        if kind == "object":
            return {}
        enum = schema.get("enum")
        if enum:
            return enum[0]
        return "x"

    def test_no_official_command_raises_keyerror(self) -> None:
        from computer_use.browser_official import OFFICIAL_COMMANDS
        from computer_use.browser_schemas import schema_for

        ex = ToolExecutor(ComputerUse(FakeDesktop()))
        surface = ex.browser
        tab = surface.dispatch("tab_new", {"url": "https://example.com/"})
        tab_id = tab["id"]
        unrouted: list[str] = []
        for name in OFFICIAL_COMMANDS:
            if name in {"browser_setup", "setup"}:
                continue
            _description, properties, required = schema_for(name)
            spec: dict[str, object] = {}
            if "tab_id" in properties:
                spec["tab_id"] = tab_id
            if "url" in properties and "url" not in spec:
                spec["url"] = "https://example.com/"
            if "providerTabId" in properties:
                spec["providerTabId"] = tab_id
            for field in required:
                if field in spec:
                    continue
                spec[field] = self._value(properties.get(field) or {}, tab_id)
            for field, schema in properties.items():
                if field in spec or field == "tab_id":
                    continue
                if field in {"selector"}:
                    spec[field] = "a"
                elif field in {"name"} and "browser_id" in properties:
                    spec[field] = "browser"
            try:
                surface.dispatch(name, spec)
            except KeyError as raised:
                # A routing hole is `raise KeyError(name)`; backend lookups raise
                # KeyError with a different message.
                if raised.args and str(raised.args[0]) == name:
                    unrouted.append(f"{name}: {raised}")
            except Exception:
                pass
        self.assertEqual(unrouted, [], f"official commands with no route: {unrouted}")


if __name__ == "__main__":
    unittest.main()
