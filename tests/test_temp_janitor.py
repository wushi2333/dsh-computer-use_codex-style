"""Regression tests for the plugin-owned temp-file janitor (SEC-05).

Ported from the reference project's capture cleanup design
(`git-repo/dsh-computer-use/internal/capture.mjs`); see
`docs/design-essence-dsh-computer-use.md` §2 SEC-05.

The three rules under test:

1. **Only what the plugin produced is deleted.** A path the janitor never
   registered is not ours to remove — the exemption the reference project
   gives a caller-specified `save_path`.
2. **Ownership is a fact, not a guess.** The creating call site names the
   owner (the tab), so "whose file is this" is answered by the caller that
   knows, not by a heuristic.
3. **Cleanup is best effort.** A file that is already gone, or that another
   process holds, must not turn the cleanup itself into a failure.
"""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

from computer_use.temp_janitor import TempJanitor, janitor, sanitize_owner


class TempJanitorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp()) / "plugin-root"
        self.janitor = TempJanitor(root=self.root)

    # ------------------------------------------------------------------ paths --

    def test_new_path_lands_inside_the_owned_root(self) -> None:
        path = self.janitor.new_path("export-tab-1.txt", owner="tab-1")
        self.assertTrue(str(path).startswith(str(self.root)))
        self.assertTrue(path.name.endswith(".txt"))
        self.assertTrue(path.parent.is_dir())

    def test_new_path_is_unique_per_call(self) -> None:
        first = self.janitor.new_path("export-tab-1.txt", owner="tab-1")
        second = self.janitor.new_path("export-tab-1.txt", owner="tab-1")
        self.assertNotEqual(first, second)
        first.write_text("a", encoding="utf-8")
        second.write_text("b", encoding="utf-8")
        self.assertEqual(first.read_text(encoding="utf-8"), "a")
        self.assertEqual(second.read_text(encoding="utf-8"), "b")

    def test_names_cannot_collide_across_instances_sharing_a_root(self) -> None:
        # P3-3 regression: the per-instance `_seq` counters both start at 1, so
        # two TempJanitor instances over the SAME root (two processes must not
        # share one, but a root is also re-creatable within one process) would
        # mint identical names without the random suffix. Sequence numbers alone
        # are only unique within one instance; the uuid suffix covers the rest.
        twin = TempJanitor(root=self.root)
        seen = set()
        for _ in range(32):
            seen.add(self.janitor.new_path("export.txt", owner="tab-1").name)
            seen.add(twin.new_path("export.txt", owner="tab-1").name)
            seen.add(self.janitor.new_dir("assets", owner="tab-1").name)
            seen.add(twin.new_dir("assets", owner="tab-1").name)
        self.assertEqual(len(seen), 128)
        # The extension survives the uniquing: callers type-match on it.
        export_names = [name for name in seen if name.startswith("export")]
        self.assertTrue(export_names)
        self.assertTrue(all(name.endswith(".txt") for name in export_names))

    def test_new_dir_lands_inside_the_owned_root(self) -> None:
        directory = self.janitor.new_dir("assets", owner="tab-1")
        self.assertTrue(str(directory).startswith(str(self.root)))
        self.assertTrue(directory.is_dir())

    def test_an_owner_key_is_made_safe_for_a_path(self) -> None:
        self.assertEqual(sanitize_owner("tab/../../etc"), "tab-etc")
        self.assertEqual(sanitize_owner(""), "")

    # --------------------------------------------------------------- tracking --

    def test_track_is_idempotent(self) -> None:
        path = self.root / "artifact.bin"
        self.janitor.track(path, owner="tab-1")
        self.janitor.track(path, owner="tab-1")
        self.assertEqual(self.janitor.pending_count(), 1)

    def test_an_unowned_path_is_still_cleaned_at_instance_teardown(self) -> None:
        path = self.janitor.new_path("audio.wav")
        path.write_bytes(b"x")
        self.assertEqual(self.janitor.pending_count(), 1)
        self.assertGreaterEqual(self.janitor.release_all(), 1)
        self.assertFalse(path.exists())

    # ---------------------------------------------------------------- cleanup --

    def test_release_owner_takes_only_that_owners_files(self) -> None:
        mine = self.janitor.new_path("export-mine.txt", owner="tab-1")
        mine.write_text("mine", encoding="utf-8")
        theirs = self.janitor.new_path("export-theirs.txt", owner="tab-2")
        theirs.write_text("theirs", encoding="utf-8")

        self.assertEqual(self.janitor.release_owner("tab-1"), 1)
        self.assertFalse(mine.exists())
        self.assertTrue(theirs.exists(), "another owner's file must survive")

    def test_release_owner_removes_a_registered_directory(self) -> None:
        directory = self.janitor.new_dir("assets", owner="tab-1")
        (directory / "asset.css").write_text("body{}", encoding="utf-8")
        self.janitor.release_owner("tab-1")
        self.assertFalse(directory.exists())

    def test_release_all_clears_every_owner_and_the_root(self) -> None:
        first = self.janitor.new_path("a.txt", owner="tab-1")
        second = self.janitor.new_path("b.txt", owner="tab-2")
        first.write_text("a", encoding="utf-8")
        second.write_text("b", encoding="utf-8")

        self.assertGreaterEqual(self.janitor.release_all(), 2)
        self.assertFalse(first.exists())
        self.assertFalse(second.exists())
        self.assertFalse(self.root.exists())
        self.assertEqual(self.janitor.pending_count(), 0)

    def test_a_path_the_janitor_never_saw_survives(self) -> None:
        # The exemption rule: a file the caller was pointed at is not ours to
        # delete. The janitor only knows what it created or was told about.
        stranger = self.root / "caller-owned.png"
        stranger.parent.mkdir(parents=True, exist_ok=True)
        stranger.write_bytes(b"png")
        self.janitor.release_all()
        self.assertTrue(stranger.exists(), "an unregistered file must survive the cleanup")

    def test_cleanup_never_raises_on_a_missing_file(self) -> None:
        path = self.janitor.new_path("gone.txt", owner="tab-1")
        path.write_text("x", encoding="utf-8")
        path.unlink()
        # The file is already gone: the cleanup must treat that as handled and
        # must not raise.
        self.assertEqual(self.janitor.release_owner("tab-1"), 0)
        self.assertEqual(self.janitor.pending_count(), 0)

    def test_the_report_exposes_the_pending_count_and_the_owners(self) -> None:
        self.janitor.new_path("a.txt", owner="tab-1")
        report = self.janitor.report()
        self.assertEqual(report["owners"], ["tab-1"])
        self.assertEqual(report["pending"], 1)
        self.assertEqual(report["root"], str(self.root))


class BrowserExportOwnershipTests(unittest.TestCase):
    """The browser export path must land in the plugin's own root.

    Before the port these files were written straight into the system temp dir
    and were never cleaned up, so a user's exports stayed there forever.
    """

    def setUp(self) -> None:
        self.previous = os.environ.get("DSH_COMPUTER_USE_TEMP_ROOT")
        self.root = Path(tempfile.mkdtemp()) / "plugin-root"
        os.environ["DSH_COMPUTER_USE_TEMP_ROOT"] = str(self.root)
        self.addCleanup(self._restore)

    def _restore(self) -> None:
        if self.previous is None:
            os.environ.pop("DSH_COMPUTER_USE_TEMP_ROOT", None)
        else:
            os.environ["DSH_COMPUTER_USE_TEMP_ROOT"] = self.previous

    def test_an_export_lands_inside_the_plugin_root(self) -> None:
        from computer_use.driver import ComputerUse
        from computer_use.executor import ToolExecutor
        from computer_use.fake_backend import FakeDesktop

        ex = ToolExecutor(ComputerUse(FakeDesktop()))
        tab = ex.execute("tab_new", {"url": "https://example.com/"})["result"]
        exported = ex.execute("tab_content_export", {"tab_id": tab["id"]})["result"]

        path = Path(exported["path"])
        self.assertTrue(str(path).startswith(str(self.root)), path)
        self.assertTrue(path.is_file())
        self.assertTrue(path.read_text(encoding="utf-8"))

    def test_closing_the_tab_takes_its_exports_away(self) -> None:
        from computer_use.driver import ComputerUse
        from computer_use.executor import ToolExecutor
        from computer_use.fake_backend import FakeDesktop

        ex = ToolExecutor(ComputerUse(FakeDesktop()))
        tab = ex.execute("tab_new", {"url": "https://example.com/"})["result"]
        exported = ex.execute("tab_content_export", {"tab_id": tab["id"]})["result"]
        path = Path(exported["path"])
        self.assertTrue(path.is_file())

        ex.execute("tab_close", {"tab_id": tab["id"]})
        self.assertFalse(path.exists(), "the tab's export must go with the tab")


class ModuleJanitorTests(unittest.TestCase):
    def test_the_module_level_janitor_is_the_one_the_browser_uses(self) -> None:
        from computer_use.browser_fake import janitor as fake_janitor

        self.assertIs(fake_janitor, janitor)

    def test_shutdown_janitor_is_callable(self) -> None:
        from computer_use.temp_janitor import shutdown_janitor

        # It must never raise, whatever state the process is in.
        shutdown_janitor()


if __name__ == "__main__":
    unittest.main()
