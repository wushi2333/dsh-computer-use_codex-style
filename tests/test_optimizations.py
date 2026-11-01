"""End-to-end tests for the four computer-use ergonomics fixes.

These cover the exact failure mode seen on 2026-10-19 while logging into the
WorkBuddy desktop client: `set_value` on a Chrome/React phone-number field
left the old value in place and appended a stray character, because the
coordinate fallback *inserted* (paste) instead of *replacing*.

What is asserted here:

1. `fill_form` executes its steps in order and reports per-step results
   (`tests/test_optimizations.py::FillFormTests`).
2. `fill_form` routes "set" through `set_value` (replace semantics) rather than
   `type_text` (append semantics) — the regression that produced
   "15168866515v" instead of "17653116208".
3. A failing step stops the fill by default and continues with
   `continue_on_error: true`.
4. `fill_form` is a harness extension: it stays out of the official
   `WINDOW2_TOOLS` contract but is reachable through the executor.
5. The Rust helper keeps `replace_text` (select-all + paste) as the only
   fallback used by `set_value` — asserted against the helper source, since a
   wrong helper binary silently reintroduces the bug.
"""

from __future__ import annotations

import unittest
from pathlib import Path

from computer_use.driver import ComputerUse
from computer_use.executor import ToolExecutor
from computer_use.fake_backend import FakeDesktop
from computer_use.tools import WINDOW2_TOOLS, harness_tool_definitions, tool_names

WINDOW = {"app": "notepad.exe", "id": 1, "title": "Untitled - Notepad"}

PLUGIN_ROOT = Path(__file__).resolve().parents[1]


def _make_executor() -> tuple[ToolExecutor, FakeDesktop]:
    backend = FakeDesktop()
    return ToolExecutor(ComputerUse(backend)), backend


class FillFormTests(unittest.TestCase):
    def setUp(self) -> None:
        self.executor, self.backend = _make_executor()

    def _observe(self) -> None:
        self.executor.execute(
            "get_window_state", {"window": WINDOW, "include_text": True}
        )

    def test_fills_set_check_click_in_order(self) -> None:
        self._observe()
        result = self.executor.execute(
            "fill_form",
            {
                "window": WINDOW,
                "fields": [
                    {"element_index": 1, "value": "17653116208"},
                    {"element_index": 2, "action": "check"},
                    {"element_index": 1, "action": "click"},
                ],
            },
        )
        self.assertTrue(result["result"]["ok"])
        self.assertEqual(result["result"]["filled"], 3)
        self.assertEqual(result["result"]["fields"], 3)
        self.assertFalse(result["result"]["stoppedEarly"])
        kinds = [record.kind for record in self.backend.actions]
        # The whole point: a value is written with set_value, never type_text.
        self.assertEqual(kinds, ["set_value", "perform_secondary_action", "click_element"])

    def test_set_replaces_rather_than_appends(self) -> None:
        """Regression: a stale value must never survive as "old + new"."""
        self._observe()
        # Pre-fill the field with the wrong number, exactly like the session did.
        self.executor.execute(
            "set_value", {"window": WINDOW, "element_index": 1, "value": "15168866515"}
        )
        self.backend.actions.clear()
        self.executor.execute(
            "fill_form",
            {"window": WINDOW, "fields": [{"element_index": 1, "value": "17653116208"}]},
        )
        kinds = [record.kind for record in self.backend.actions]
        self.assertEqual(kinds, ["set_value"], "fill_form must replace, not type/append")
        payload = self.backend.actions[-1].payload
        self.assertEqual(payload["value"], "17653116208")

    def test_missing_value_on_set_is_reported_not_swallowed(self) -> None:
        self._observe()
        result = self.executor.execute(
            "fill_form",
            {"window": WINDOW, "fields": [{"element_index": 1, "action": "set"}]},
        )
        self.assertFalse(result["result"]["ok"])
        self.assertIn("needs a value", result["result"]["results"][0]["error"])

    def test_first_failure_stops_by_default(self) -> None:
        self._observe()
        result = self.executor.execute(
            "fill_form",
            {
                "window": WINDOW,
                "fields": [
                    {"element_index": 1, "value": "ok"},
                    {"element_index": 999, "value": "bad"},
                    {"element_index": 2, "value": "never reached"},
                ],
            },
        )
        self.assertFalse(result["result"]["ok"])
        self.assertTrue(result["result"]["stoppedEarly"])
        self.assertEqual(result["result"]["fields"], 2, "must stop at the failing step")

    def test_continue_on_error_runs_every_step(self) -> None:
        self._observe()
        result = self.executor.execute(
            "fill_form",
            {
                "window": WINDOW,
                "continue_on_error": True,
                "fields": [
                    {"element_index": 1, "value": "ok"},
                    {"element_index": 999, "value": "bad"},
                    {"element_index": 2, "value": "also ran"},
                ],
            },
        )
        self.assertFalse(result["result"]["ok"])
        self.assertFalse(result["result"]["stoppedEarly"])
        self.assertEqual(result["result"]["fields"], 3)
        self.assertEqual(result["result"]["filled"], 2)

    def test_press_action_defaults_to_return(self) -> None:
        self._observe()
        result = self.executor.execute(
            "fill_form",
            {"window": WINDOW, "fields": [{"element_index": 1, "action": "press"}]},
        )
        self.assertTrue(result["result"]["ok"])
        self.assertEqual(self.backend.actions[-1].payload["key"], "Return")

    def test_empty_fields_is_a_type_error(self) -> None:
        self._observe()
        with self.assertRaises(TypeError):
            self.executor.execute("fill_form", {"window": WINDOW, "fields": []})

    def test_inner_steps_survive_a_stale_lease(self) -> None:
        """Regression (2026-10-19): a `batch_actions` refresh that observed the
        overlay window left the lease pointing at the overlay, and every
        `fill_form` step then failed with "last get_window_state was window id
        X". The outer validation is enough; inner steps must not re-validate.
        """
        self._observe()
        # Simulate the lease being moved to another window (e.g. by a refresh).
        other = {"app": "dsh-computer-use.exe", "id": 330550, "title": "overlay"}
        self.executor.execute("get_window_state", {"window": other, "include_text": True})
        result = self.executor.execute(
            "fill_form",
            {"window": WINDOW, "fields": [{"element_index": 1, "value": "survives"}]},
        )
        self.assertTrue(
            result["result"]["ok"],
            f"fill_form must not inherit a stale lease: {result['result']}",
        )
        self.assertEqual(result["result"]["filled"], 1)

    def test_fill_form_self_observes_when_the_python_lease_is_empty(self) -> None:
        """Cross-backend truth (2026-10-19): `get_window_state` is normally served
        by the native helper, so the Python-side lease stays empty even though
        the caller did observe. A `fill_form` must then observe once itself
        rather than failing closed with "no fresh get_window_state".
        """
        fresh_executor, backend = _make_executor()
        self.assertIsNone(fresh_executor.driver.lease.observed_at)
        result = fresh_executor.execute(
            "fill_form",
            {"window": WINDOW, "fields": [{"element_index": 1, "value": "x"}]},
        )
        self.assertTrue(result["result"]["ok"], result["result"])
        # It observed first, then wrote.
        self.assertIsNotNone(fresh_executor.driver.lease.observed_at)
        self.assertEqual(backend.actions[-1].kind, "set_value")

    def test_fill_form_is_not_part_of_the_official_contract(self) -> None:
        """Harness extension: the official 13-method surface must stay exactly 13."""
        names = tool_names()
        self.assertNotIn("fill_form", names)
        self.assertNotIn("fill_form", WINDOW2_TOOLS)
        harness_names = [t["function"]["name"] for t in harness_tool_definitions()]
        self.assertEqual(harness_names, ["fill_form"])
        # ...but it is still reachable through the executor.
        self._observe()
        result = self.executor.execute(
            "fill_form",
            {"window": WINDOW, "fields": [{"element_index": 1, "value": "x"}]},
        )
        self.assertIn("result", result)


class HelperSourceContracts(unittest.TestCase):
    """The helper binary must keep the replace semantics that fix the bug.

    A stale `helper-rs/bin/.../dsh-computer-use.exe` would silently reintroduce
    the "old value + stray character" failure, so the source is asserted too.
    """

    def setUp(self) -> None:
        self.helper = (PLUGIN_ROOT / "helper-rs" / "src").resolve()

    def test_replace_text_is_the_set_value_fallback(self) -> None:
        main = (self.helper / "main.rs").read_text(encoding="utf-8")
        # The coordinate fallback must select-all + paste, never a bare paste.
        self.assertIn("input::replace_text_for(", main)
        self.assertNotIn("input::paste_text(&value)?", main)

    def test_uia_set_value_falls_back_to_replace_text(self) -> None:
        """The fallback lives on the CALLER's thread (audit P2-4, 2026-10-25).

        The UIA monitor thread used to run the select-all + paste fallback
        itself, queueing every other UIA request behind synthetic typing.
        The engine now reports the miss and `main.rs` performs the replace.
        """
        uia = (self.helper / "uia.rs").read_text(encoding="utf-8")
        self.assertNotIn("crate::input::replace_text_for(", uia)
        self.assertNotIn("crate::input::paste_text(value)", uia)
        main = (self.helper / "main.rs").read_text(encoding="utf-8")
        self.assertIn("input::replace_text_for(", main)

    def test_keyboard_input_suppresses_the_ime(self) -> None:
        inp = (self.helper / "input.rs").read_text(encoding="utf-8")
        # Every synthetic-keyboard entry point must install the guard.
        self.assertIn("crate::ime::ImeGuard::install_for(hwnd)", inp)
        for entry in (
            "pub fn press_key_for(hwnd: Option<isize>, chord: &str)",
            "pub fn type_unicode_for(hwnd: Option<isize>, text: &str)",
            "pub fn replace_text_for(hwnd: Option<isize>, text: &str)",
            "pub fn paste_text_for(hwnd: Option<isize>, text: &str)",
        ):
            self.assertIn(entry, inp, f"missing {entry}")

    def test_ime_module_targets_the_window_not_the_process(self) -> None:
        ime_text = (self.helper / "ime.rs").read_text(encoding="utf-8")
        # Drop the //! module docs: they legitimately *mention* the old approach
        # to explain why it was wrong. Only the code matters here.
        code = "\n".join(
            line for line in ime_text.splitlines()
            if not line.lstrip().startswith("//")
        )
        # Per-process layout switching does NOT affect the thread that receives the
        # keystrokes; the guard must act on the target window's IME.
        self.assertIn("ImmAssociateContext", code)
        self.assertIn("WM_INPUTLANGCHANGEREQUEST", code)
        self.assertNotIn(
            "ActivateKeyboardLayout",
            code,
            "per-process layout switching does not affect the target thread",
        )
        self.assertIn("impl Drop for ImeGuard", code)
        self.assertIn("pub fn install_for(hwnd: Option<isize>)", code)
        # No foreground window must not be fatal (observed live on 2026-10-19).
        self.assertIn("install_for(None)", ime_text)

    def test_activate_bumps_z_order_before_focus(self) -> None:
        enum = (self.helper / "enum_windows.rs").read_text(encoding="utf-8")
        self.assertIn("fn bump_to_top(hwnd: HWND)", enum)
        self.assertIn("HWND_TOPMOST", enum)
        self.assertIn("HWND_NOTOPMOST", enum)
        # The bump must run before SetForegroundWindow in activate_hwnd.
        self.assertLess(enum.index("bump_to_top(hwnd);"), enum.index("SetForegroundWindow(hwnd).as_bool()"))


if __name__ == "__main__":
    unittest.main()
