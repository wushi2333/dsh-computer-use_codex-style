from __future__ import annotations

"""A ``batch_actions`` entry with no method name must say which one and why.

Mirrors the native guard in ``helper-rs/src/main.rs`` (``batch_action_name``).
Before it, a nameless entry answered the bare official prefix
``unsupported method: `` -- nothing after the colon, so the caller could not tell
a malformed entry from a broken helper. The Python engine did something different
and worse: it silently *dropped* non-mapping entries, so a typo vanished.
"""

import unittest

from computer_use.driver import ComputerUse
from computer_use.executor import ToolExecutor
from computer_use.fake_backend import FakeDesktop


class BatchActionNameTests(unittest.TestCase):
    def setUp(self) -> None:
        self.executor = ToolExecutor(ComputerUse(FakeDesktop()))

    def test_nameless_entry_reports_its_index_and_the_expected_shape(self) -> None:
        out = self.executor.execute("batch_actions", {"actions": [{"action": "press_key"}]})
        results = out["result"]["results"]
        self.assertEqual(len(results), 1, "the malformed entry must be reported, not dropped")
        self.assertFalse(results[0]["ok"])
        self.assertEqual(results[0]["index"], 0)
        self.assertIn("name", results[0]["error"])
        self.assertNotEqual(results[0]["error"], "unsupported method: ")

    def test_empty_non_string_and_non_object_entries_are_all_rejected(self) -> None:
        out = self.executor.execute(
            "batch_actions",
            {"actions": [{"name": ""}, {"name": 7}, "not-an-object"]},
        )
        results = out["result"]["results"]
        self.assertEqual([entry["index"] for entry in results], [0, 1, 2])
        self.assertTrue(all(entry["ok"] is False for entry in results))

    def test_a_named_action_still_runs(self) -> None:
        # The guard must not swallow real work: a named entry reaches dispatch.
        #
        # NOTE: only the *failure* entry shape is shared with the Rust helper
        # ({ok:false,index,error}). A dispatched action here carries the Python
        # engine's `execute()` envelope ({name,canonical,result,observation}),
        # while Rust answers {ok:true,index,name,value}. Asserting the former is
        # asserting what this engine actually guarantees.
        out = self.executor.execute(
            "batch_actions", {"actions": [{"name": "diagnostic_state", "arguments": {}}]}
        )
        self.assertEqual(out["result"]["batched"], 1)
        entry = out["result"]["results"][0]
        self.assertNotIn("error", entry, "a named action must not produce a failure entry")
        self.assertEqual(entry["name"], "diagnostic_state")
        self.assertIn("result", entry)


if __name__ == "__main__":
    unittest.main()
