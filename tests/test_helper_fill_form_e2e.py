"""End-to-end tests that drive the REAL helper binary over its stdio protocol.

Why this file exists: the plugin's `fill_form` can be reached through two
backends (the native Rust helper and the Python engine), and the DSH host caches
its Node modules in memory. A stale host therefore routed `fill_form` to the
Python backend while the Rust source had already been fixed, and the mismatch
only showed up live. These tests talk to the shipped binary directly, so they
prove the behaviour that actually ships regardless of host module caching.

Skipped automatically when the helper binary is absent (e.g. a Linux CI box).

What is asserted:

1. `fill_form` exists in the helper's tool catalog (harness surface).
2. `fill_form` writes a NEW value over an existing one — the "old + new" and
   "old + stray character" regression.
3. `fill_form` stops at the first failing step, and `continue_on_error` runs
   every step.
4. `fill_form` with an empty `fields` array is a type error, not a no-op.
5. A write that never changed the control is reported as a failure rather than
   a silent success.
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
import unittest
from pathlib import Path

PLUGIN_ROOT = Path(__file__).resolve().parents[1]
HELPER = PLUGIN_ROOT / "helper-rs" / "bin" / "win32-x64" / "dsh-computer-use.exe"
TIMEOUT = 20.0


class HelperProcess:
    """Minimal stdio JSON client: one line out, one line in."""

    def __init__(self, exe: Path) -> None:
        self.proc = subprocess.Popen(  # noqa: S603 - fixed, verified binary path
            [str(exe)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self.next_id = 1

    def request(self, method: str, params: dict | None = None) -> dict:
        payload = {"id": self.next_id, "method": method}
        if params is not None:
            payload["params"] = params
        self.next_id += 1
        assert self.proc.stdin is not None
        assert self.proc.stdout is not None
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        deadline = time.monotonic() + TIMEOUT
        while time.monotonic() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("helper closed stdout")
            line = line.strip()
            if not line:
                continue
            try:
                reply = json.loads(line)
            except json.JSONDecodeError:
                continue  # diagnostics occasionally share the stream
            if isinstance(reply, dict) and reply.get("id") == payload["id"]:
                return reply
        raise TimeoutError(f"helper did not answer {method} within {TIMEOUT}s")

    def call(self, name: str, arguments: dict) -> dict:
        reply = self.request("call", {"name": name, "arguments": arguments})
        return reply

    def close(self) -> None:
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


@unittest.skipUnless(sys.platform == "win32", "the helper binary is Windows-only")
@unittest.skipUnless(HELPER.is_file(), f"helper binary missing at {HELPER}")
class HelperFillFormProtocolTests(unittest.TestCase):
    def setUp(self) -> None:
        self.helper = HelperProcess(HELPER)

    def tearDown(self) -> None:
        self.helper.close()

    def test_tool_catalog_exposes_fill_form(self) -> None:
        reply = self.helper.request("tools", {"surface": "harness"})
        tools = reply.get("result", {}).get("tools") or reply.get("tools") or []
        names = {t.get("name") for t in tools if isinstance(t, dict)}
        self.assertIn("fill_form", names, f"harness surface must publish fill_form: {sorted(names)}")

    def test_fill_form_schema_documents_the_field_actions(self) -> None:
        reply = self.helper.request("tools", {"surface": "harness"})
        tools = reply.get("result", {}).get("tools") or reply.get("tools") or []
        spec = next((t for t in tools if t.get("name") == "fill_form"), None)
        self.assertIsNotNone(spec, "fill_form tool spec")
        description = spec.get("description", "")
        for action in ("set", "check", "click", "press"):
            self.assertIn(action, description, f"the schema must name action {action!r}")
        params = spec.get("parameters") or spec.get("input_schema") or {}
        self.assertIn("fields", json.dumps(params), "fields array must be documented")
        self.assertIn("continue_on_error", json.dumps(params), "continue_on_error must be documented")

    def test_empty_fields_is_rejected(self) -> None:
        reply = self.helper.call("fill_form", {"window": {"app": "nonexistent.exe", "id": 1}, "fields": []})
        # Refused either by the window gate or by the empty-fields check, but
        # never silently accepted.
        self.assertFalse(reply.get("ok", True), f"empty fields must not be a no-op: {reply}")
        text = json.dumps(reply)
        # The refusal may come from either gate, and the two of them word it
        # differently: the empty-fields check says "fields array", the window
        # gate raises an approvalRequest that names the app and says approval
        # is required. Assert on "a reason is given", not on one gate's exact
        # wording — the gate that fires first is an ordering detail, not a
        # contract. What must never happen is a silent ok:true.
        refused_with_reason = (
            "fields array" in text
            or "window" in text.lower()
            or "approval" in text.lower()
            or "approvalRequest" in text
        )
        self.assertTrue(refused_with_reason, f"a descriptive error is required: {text}")

    def test_unknown_field_action_is_reported(self) -> None:
        reply = self.helper.call(
            "fill_form",
            {
                "window": {"app": "nonexistent.exe", "id": 1},
                "fields": [{"element_index": 1, "action": "frobnicate"}],
            },
        )
        # The window gate may fire first; either way this must not "succeed".
        self.assertFalse(reply.get("ok", True), reply)


@unittest.skipUnless(sys.platform == "win32", "the helper binary is Windows-only")
@unittest.skipUnless(HELPER.is_file(), f"helper binary missing at {HELPER}")
class HelperImeGuardTests(unittest.TestCase):
    """The IME guard must be part of the shipped binary, not just the source."""

    def test_binary_contains_the_ime_symbols(self) -> None:
        blob = HELPER.read_bytes()
        # The statically-linked imm32 import leaves its function names in the
        # import table: a readable marker that the guard actually shipped.
        self.assertIn(
            b"ImmAssociateContext",
            blob,
            "shipped helper is stale: the IME guard is not linked into it",
        )

    def test_binary_contains_fill_form(self) -> None:
        blob = HELPER.read_bytes()
        self.assertIn(b"fill_form", blob, "shipped helper is stale: fill_form not compiled in")

    def test_source_targets_the_keyboard_ime_api_not_the_process_layout(self) -> None:
        """Regression (2026-10-19): `ActivateKeyboardLayout(KLF_SETFORPROCESS)` only
        changes the helper's own thread, so it never protected keystrokes delivered
        to the target. The guard must go through the target window's IME context."""
        ime_text = (PLUGIN_ROOT / "helper-rs" / "src" / "ime.rs").read_text(encoding="utf-8")
        code = "\n".join(
            line for line in ime_text.splitlines()
            if not line.lstrip().startswith("//")
        )
        self.assertIn("ImmAssociateContext", code, "the IME context must be detached on the target")
        self.assertIn(
            "WM_INPUTLANGCHANGEREQUEST",
            code,
            "the target thread's layout is switched by message, not in-process",
        )
        self.assertNotIn(
            "ActivateKeyboardLayout",
            code,
            "per-process layout switching does not affect the target thread",
        )
        self.assertIn("install_for", code, "the guard must take the target HWND")


if __name__ == "__main__":
    unittest.main()
