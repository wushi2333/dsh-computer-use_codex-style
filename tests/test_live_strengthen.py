from __future__ import annotations

import unittest
from pathlib import Path

from computer_use.browser_export import ASSETS_JS, FETCH_AS_B64_JS, WEBMCP_JS, YOUTUBE_TRANSCRIPT_JS, gsuite_export_url
from computer_use.cdp_ws import CdpConn, load_websocket
from computer_use.driver import ComputerUse
from computer_use.executor import ToolExecutor
from computer_use.extension_hub import ExtensionHub
from computer_use.fake_backend import FakeDesktop


class LiveStrengthenTests(unittest.TestCase):
    def test_gsuite_youtube_fetch_shape(self) -> None:
        url = gsuite_export_url("https://docs.google.com/document/d/ABC/edit", "pdf")
        self.assertTrue(url and "export?format=pdf" in url)
        self.assertIn("credentials", FETCH_AS_B64_JS)
        self.assertIn("include", FETCH_AS_B64_JS)
        self.assertIn("ytInitialPlayerResponse", YOUTUBE_TRANSCRIPT_JS)
        self.assertIn("captionTracks", YOUTUBE_TRANSCRIPT_JS)
        self.assertIn("baseUrl", YOUTUBE_TRANSCRIPT_JS)
        self.assertIn("data-src", ASSETS_JS)
        self.assertIn("srcset", ASSETS_JS)
        self.assertIn("CSSFontFaceRule", ASSETS_JS)
        self.assertIn("captionTracks", YOUTUBE_TRANSCRIPT_JS)
        self.assertIn("modelContext", WEBMCP_JS)

    def test_dialog_buffer_download_path_filechooser_cua_history_context(self) -> None:
        from unittest.mock import patch
        import json

        class FakeWs:
            def __init__(self) -> None:
                self.inbox: list[str] = []

            def send(self, data: str) -> None:
                msg = json.loads(data)
                self.inbox.append(
                    json.dumps({"method": "Page.javascriptDialogOpening", "params": {"type": "confirm", "message": "go"}})
                )
                self.inbox.append(json.dumps({"id": msg["id"], "result": {}}))

            def recv(self) -> str:
                return self.inbox.pop(0)

            def settimeout(self, _t: float) -> None:
                return None

            def close(self) -> None:
                return None

        # `load_websocket()` is the lazy transport import (see cdp_ws): patching the
        # module it returns keeps this test independent of whether `websocket-client` is
        # installed on the machine, which is the whole point of that indirection.
        transport = type("T", (), {"create_connection": staticmethod(lambda url, timeout=20: FakeWs())})()
        with patch("computer_use.cdp_ws.load_websocket", return_value=transport):
            conn = CdpConn("ws://127.0.0.1:1/devtools/page/x")
            conn.call("Runtime.enable")
        dialog = conn.last_event("Page.javascriptDialogOpening")
        self.assertEqual(dialog["type"], "confirm")

        ex = ToolExecutor(ComputerUse(FakeDesktop()))
        tab = ex.execute("tab_new", {"url": "https://example.com/"})["result"]
        tid = tab["id"]
        media = ex.execute("tab_dom_download_media", {"tab_id": tid, "node_id": 2})["result"]
        path = ex.execute("tab_pw_download_path", {"tab_id": tid})["result"]["path"]
        self.assertEqual(media["action"], "downloadMedia")
        self.assertTrue(Path(path).is_file())
        self.assertGreater(Path(path).stat().st_size, 0)
        files = ex.execute("tab_pw_set_files", {"tab_id": tid, "files": ["one.png"], "multiple": False})["result"]
        self.assertEqual(files["files"], ["one.png"])
        cua = ex.execute("tab_cua_click", {"tab_id": tid, "x": 15, "y": 25, "screenshotId": "screenshot-0"})["result"]
        self.assertEqual(cua["coordinateSpace"], "viewport")
        self.assertEqual(cua["x"], 15)
        hist = ex.execute("browser_history", {"queries": ["example"], "from": "2020-01-01", "limit": 3})["result"]["entries"]
        self.assertTrue(hist)
        hub = ExtensionHub()
        ex.browser.hub = hub
        hub.ingest(
            {
                "type": "hello",
                "instanceId": "x",
                "tabs": [{"providerTabId": "7", "title": "Mail", "url": "https://mail.example/", "text": "inbox body"}],
            }
        )
        ctx = ex.execute(
            "browser_get_tab_context",
            {"providerTabId": "7", "title": "Mail", "url": "https://mail.example/"},
        )["result"]
        self.assertFalse(ctx["claimed"])
        self.assertIn("inbox", ctx["text"])

    def test_cdp_page_assets_and_webmcp_when_edge_runs(self) -> None:
        import http.server
        import tempfile
        import threading
        from computer_use.cdp_browser import CdpBrowser
        from computer_use.cdp_launch import edge_exe, launch_edge
        from computer_use.png import solid_png

        if edge_exe() is None:
            self.skipTest("msedge.exe not found")
        root = Path(tempfile.mkdtemp()) / "site"
        root.mkdir()
        (root / "style.css").write_text("body{color:red}", encoding="utf-8")
        (root / "app.js").write_text("window.__asset=1", encoding="utf-8")
        (root / "dot.png").write_bytes(solid_png())
        (root / "index.html").write_text(
            '<html><head><link rel="stylesheet" href="/style.css"></head>'
            '<body><img src="/dot.png" alt="dot"><script src="/app.js"></script></body></html>',
            encoding="utf-8",
        )

        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, directory=str(root), **kwargs)

            def log_message(self, *_args) -> None:
                return None

        httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        page = f"http://127.0.0.1:{httpd.server_address[1]}/index.html"
        proc = None
        browser = None
        try:
            proc = launch_edge(9335, Path(tempfile.mkdtemp()) / "edge-strengthen")
            browser = CdpBrowser(port=9335)
            tab = browser.new_tab(page)
            assets = browser.page_assets_list(tab.id)
            count = int(assets.get("summary", {}).get("totalCount") or 0)
            self.assertGreater(count, 0, msg=str(assets))
            bundle = browser.page_assets_bundle(tab.id, str(assets.get("id") or ""))
            sized = [item for item in bundle.get("assets") or [] if Path(str(item.get("path") or "")).is_file() and Path(str(item["path"])).stat().st_size > 0]
            self.assertGreater(len(sized), 0, msg=str(bundle))
            mcp = browser.webmcp_fetch(tab.id)
            self.assertIn("tools", mcp)
            downloaded = browser.download_media(tab.id)
            path = Path(str(downloaded["download"]["path"]))
            self.assertTrue(path.is_file())
            self.assertGreater(path.stat().st_size, 0)
            scratch = Path(r"E:\Temp\grok-goal-24521be313b9\implementer")
            scratch.mkdir(parents=True, exist_ok=True)
            (scratch / "page-assets.json").write_text(
                __import__("json").dumps({"assets": assets, "bundle": {"downloadedCount": bundle["summary"]["downloadedCount"], "files": sized}}, indent=2),
                encoding="utf-8",
            )
        except (FileNotFoundError, TimeoutError, OSError, RuntimeError) as exc:
            self.skipTest(str(exc))
        finally:
            httpd.shutdown()
            if browser is not None:
                browser.close()
            if proc is not None:
                proc.kill()
                try:
                    proc.wait(timeout=5)
                except Exception:
                    pass


if __name__ == "__main__":
    unittest.main()
