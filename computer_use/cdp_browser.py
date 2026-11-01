"""Live CDP browser: Page / Accessibility / DOM via WebSocket."""

from __future__ import annotations

import base64
import json
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from typing import Any

from computer_use.browser_dom import VISIBLE_DOM_JS, click_js, scroll_js, type_js
from computer_use.cdp_ax import ax_text, flatten_cdp_ax
from computer_use.cdp_http import list_tabs
from computer_use.cdp_ws import CdpConn
from computer_use.compact import tree_diff
from computer_use.temp_janitor import janitor


@dataclass
class CdpTab:
    id: str
    url: str
    title: str
    ws_url: str
    conn: CdpConn
    ax_nodes: list[dict[str, Any]] = field(default_factory=list)
    focused: int = 0
    prev_tree: str = ""
    dom: list[dict[str, object]] = field(default_factory=list)
    downloads: list[dict[str, object]] = field(default_factory=list)


class CdpBrowser:
    def __init__(self, host: str = "127.0.0.1", port: int = 9222) -> None:
        self.host = host
        self.port = port
        self.tabs: dict[str, CdpTab] = {}
        self.proc = None
        self.browsers = [
            {"id": "edge", "name": "Microsoft Edge", "type": "cdp"},
            {"id": "iab", "name": "In-app browser", "type": "iab"},
            {"id": "chrome", "name": "Google Chrome", "type": "extension"},
        ]

    @classmethod
    def try_connect(cls, port: int = 9222) -> "CdpBrowser | None":
        if not list_tabs(port=port, timeout=0.5):
            return None
        return cls(port=port)

    def list_browsers(self) -> list[dict[str, object]]:
        return list(self.browsers)

    def _http(self, path: str) -> Any:
        url = f"http://{self.host}:{self.port}{path}"
        with urllib.request.urlopen(url, timeout=8) as response:
            return json.loads(response.read().decode("utf-8"))

    def new_tab(self, url: str = "https://example.com/", browser: str = "cdp") -> CdpTab:
        encoded = urllib.parse.quote(url, safe=":/")
        created = None
        for method in ("GET", "PUT"):
            req = urllib.request.Request(f"http://{self.host}:{self.port}/json/new?{encoded}", method=method)
            try:
                with urllib.request.urlopen(req, timeout=8) as response:
                    created = json.loads(response.read().decode("utf-8"))
                    break
            except OSError:
                continue
        if not isinstance(created, dict):
            raise RuntimeError("CDP /json/new failed")
        ws = str(created.get("webSocketDebuggerUrl") or "")
        tab_id = str(created.get("id") or f"cdp-{len(self.tabs)+1}")
        conn = CdpConn(ws)
        conn.call("Page.enable")
        conn.call("Runtime.enable")
        conn.call("DOM.enable")
        conn.call("Accessibility.enable")
        try:
            conn.call("DOM.getDocument", {"depth": 0})
        except RuntimeError:
            pass
        tab = CdpTab(id=tab_id, url=url, title=str(created.get("title") or ""), ws_url=ws, conn=conn)
        self.tabs[tab.id] = tab
        if url and url not in {"about:blank", "about:blank/"}:
            self.goto(tab.id, url)
        return tab

    def tab(self, tab_id: str) -> CdpTab:
        if tab_id not in self.tabs:
            raise KeyError(f"unknown tab {tab_id}")
        return self.tabs[tab_id]

    def goto(self, tab_id: str, url: str) -> CdpTab:
        tab = self.tab(tab_id)
        if tab.url == url and tab.ax_nodes:
            return tab
        tab.conn.call("Page.navigate", {"url": url})
        try:
            tab.conn.wait_event("Page.loadEventFired", timeout=15)
        except TimeoutError:
            pass
        nav = tab.conn.call("Page.getFrameTree")
        frame = nav.get("frameTree", {}).get("frame", {}) if isinstance(nav, dict) else {}
        tab.url = str(frame.get("url") or url)
        title = tab.conn.call("Runtime.evaluate", {"expression": "document.title", "returnByValue": True})
        tab.title = str((title.get("result") or {}).get("value") or tab.title)
        return tab

    def ax_write(self, tab_id: str, mode: str = "state", disable_diff: bool = True) -> dict[str, object]:
        tab = self.tab(tab_id)
        tree = tab.conn.call("Accessibility.getFullAXTree")
        tab.ax_nodes = flatten_cdp_ax(tree)
        text = ax_text(tab.ax_nodes, tab.title, tab.url)
        shown, diff = text, False
        if mode in ("state", "both") and tab.prev_tree and not disable_diff:
            shown = tree_diff(tab.prev_tree, text)
            diff = True
        if mode in ("state", "both"):
            tab.prev_tree = text
        payload: dict[str, object] = {
            "tab_id": tab.id,
            "title": tab.title,
            "url": tab.url,
            "mode": mode,
            "accessibility": None,
            "screenshots": [],
            "tree": tab.ax_nodes if mode in ("state", "both") else [],
            "dom": [],
            "backend": "cdp-ws",
        }
        if mode in ("state", "both"):
            payload["accessibility"] = {"tree": shown, "diff": diff, "focused_element": f"[{tab.focused}]"}
            payload["dom"] = self.dom_snapshot(tab_id)["nodes"]
        if mode in ("screenshot", "both"):
            shot = tab.conn.call("Page.captureScreenshot", {"format": "png"})
            b64 = str(shot.get("data") or "")
            raw = base64.b64decode(b64) if b64 else b""
            # Carry the PNG as a data-URL inside the screenshot entry so
            # images.detach_images harvests it into the top-level images[]
            # (the vision channel). A non-empty screenshots[] list makes
            # detach_images skip the payload node and only recurse into the
            # list, so the bytes must live on the entry, not a top-level key.
            payload["screenshots"] = [
                {
                    "id": f"{tab.id}-shot",
                    "emitted": False,
                    "bytes": len(raw),
                    "url": f"data:image/png;base64,{b64}",
                }
            ]
            payload["screenshot_bytes"] = len(raw)
        return payload

    def ax_click(self, tab_id: str, index: int) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next(item for item in tab.ax_nodes if int(item["index"]) == index)
        backend = node.get("backendDOMNodeId")
        if backend is None:
            raise KeyError(f"AX node {index} has no backendDOMNodeId")
        resolved = tab.conn.call("DOM.resolveNode", {"backendNodeId": int(backend)})
        object_id = (resolved.get("object") or {}).get("objectId")
        tab.conn.call(
            "Runtime.callFunctionOn",
            {"objectId": object_id, "functionDeclaration": "function(){ this.click(); }", "returnByValue": True},
        )
        tab.focused = index
        return {"ok": True, "action": "ax.click", "element_index": index, "name": node.get("name"), "backend": "cdp-ws"}

    def ax_set_value(self, tab_id: str, index: int, value: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next(item for item in tab.ax_nodes if int(item["index"]) == index)
        backend = node.get("backendDOMNodeId")
        resolved = tab.conn.call("DOM.resolveNode", {"backendNodeId": int(backend)})
        object_id = (resolved.get("object") or {}).get("objectId")
        tab.conn.call(
            "Runtime.callFunctionOn",
            {
                "objectId": object_id,
                "functionDeclaration": "function(v){ this.focus(); this.value = v; this.dispatchEvent(new Event('input',{bubbles:true})); }",
                "arguments": [{"value": value}],
                "returnByValue": True,
            },
        )
        tab.focused = index
        return {"ok": True, "action": "ax.setValue", "element_index": index, "value": value, "backend": "cdp-ws"}

    def _eval(self, tab_id: str, expression: str, await_promise: bool = False) -> Any:
        tab = self.tab(tab_id)
        result = tab.conn.call(
            "Runtime.evaluate",
            {"expression": expression, "returnByValue": True, "awaitPromise": await_promise},
        )
        return (result.get("result") or {}).get("value")

    def dom_snapshot(self, tab_id: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        nodes = self._eval(tab_id, VISIBLE_DOM_JS) or []
        tab.dom = nodes if isinstance(nodes, list) else []
        return {"tab_id": tab.id, "url": tab.url, "nodes": tab.dom, "backend": "cdp-ws"}

    def dom_click(self, tab_id: str, node_id: int) -> dict[str, object]:
        snap = self.dom_snapshot(tab_id)
        node = next(item for item in snap["nodes"] if int(item["node_id"]) == node_id)
        selector = str(node.get("selector") or "body")
        tab = self.tab(tab_id)
        tab.conn.call(
            "Runtime.evaluate",
            {"expression": f'document.querySelector({json.dumps(selector)})?.click()', "returnByValue": True},
        )
        return {"ok": True, "action": "dom_cua.click", "node_id": node_id, "selector": selector, "backend": "cdp-ws"}

    def dom_double_click(self, tab_id: str, node_id: int) -> dict[str, object]:
        snap = self.dom_snapshot(tab_id)
        node = next(item for item in snap["nodes"] if int(item["node_id"]) == node_id)
        self._eval(tab_id, click_js(str(node.get("selector") or "body"), 2))
        return {"ok": True, "action": "dom_cua.double_click", "node_id": node_id, "backend": "cdp-ws"}

    def dom_type(self, tab_id: str, text: str) -> dict[str, object]:
        self._eval(tab_id, type_js(text))
        return {"ok": True, "action": "dom_cua.type", "text": text, "backend": "cdp-ws"}

    def dom_keypress(self, tab_id: str, keys: object) -> dict[str, object]:
        tab = self.tab(tab_id)
        combo = keys if isinstance(keys, list) else [str(keys)]
        for key in combo:
            tab.conn.call("Input.dispatchKeyEvent", {"type": "keyDown", "key": str(key)})
            tab.conn.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": str(key)})
        return {"ok": True, "action": "dom_cua.keypress", "keys": combo, "backend": "cdp-ws"}

    def dom_scroll(self, tab_id: str, scroll_x: float = 0, scroll_y: float = 0, node_id: int | None = None) -> dict[str, object]:
        self._eval(tab_id, scroll_js(scroll_x, scroll_y, node_id))
        return {"ok": True, "action": "dom_cua.scroll", "scroll_x": scroll_x, "scroll_y": scroll_y, "backend": "cdp-ws"}

    def back(self, tab_id: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        hist = tab.conn.call("Page.getNavigationHistory")
        idx = int(hist.get("currentIndex") or 0)
        entries = hist.get("entries") or []
        if idx > 0 and isinstance(entries[idx - 1], dict):
            tab.conn.call("Page.navigateToHistoryEntry", {"entryId": entries[idx - 1].get("id")})
        return {"ok": True, "action": "back", "backend": "cdp-ws"}

    def forward(self, tab_id: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        hist = tab.conn.call("Page.getNavigationHistory")
        idx = int(hist.get("currentIndex") or 0)
        entries = hist.get("entries") or []
        if idx + 1 < len(entries) and isinstance(entries[idx + 1], dict):
            tab.conn.call("Page.navigateToHistoryEntry", {"entryId": entries[idx + 1].get("id")})
        return {"ok": True, "action": "forward", "backend": "cdp-ws"}

    def reload(self, tab_id: str) -> dict[str, object]:
        self.tab(tab_id).conn.call("Page.reload")
        return {"ok": True, "action": "reload", "backend": "cdp-ws"}

    def close_tab(self, tab_id: str) -> dict[str, object]:
        tab = self.tabs.pop(tab_id, None)
        if tab is not None:
            try:
                tab.conn.call("Page.close")
            except RuntimeError:
                pass
            tab.conn.close()
        # The tab's plugin-generated exports and media captures go with it
        # (SEC-05). A *download* is the user's artifact and is not ours to
        # delete, so it was never registered with the janitor.
        janitor.release_owner(str(tab_id))
        return {"ok": True, "closed": tab_id}

    def get_js_dialog(self, tab_id: str) -> dict[str, object] | None:
        params = self.tab(tab_id).conn.last_event("Page.javascriptDialogOpening")
        if not params:
            return None
        return {
            "type": params.get("type"),
            "message": params.get("message"),
            "url": params.get("url"),
            "defaultPrompt": params.get("defaultPrompt"),
        }

    def handle_dialog(self, tab_id: str, accept: bool, text: str = "", expect: str | None = None) -> dict[str, object]:
        dialog = self.get_js_dialog(tab_id)
        if expect and (not dialog or dialog.get("type") != expect):
            return {"ok": False, "error": f"no {expect} dialog", "dialog": dialog}
        self.tab(tab_id).conn.call("Page.handleJavaScriptDialog", {"accept": accept, "promptText": text})
        self.tab(tab_id).conn.pop_event("Page.javascriptDialogOpening")
        self.tab(tab_id).conn.pop_event("Page.javascriptDialogClosed")
        return {"ok": True, "accepted": accept, "type": (dialog or {}).get("type"), "backend": "cdp-ws"}

    def viewport_metrics(self, tab_id: str) -> dict[str, float]:
        from computer_use.browser_export import VIEWPORT_JS

        raw = self._eval(tab_id, f"({VIEWPORT_JS})()") or {}
        dpr = float(raw.get("dpr") or 1) or 1.0
        return {
            "dpr": dpr,
            "scale": float(raw.get("scale") or 1) or 1.0,
            "offsetLeft": float(raw.get("offsetLeft") or 0),
            "offsetTop": float(raw.get("offsetTop") or 0),
        }

    def _css_point(self, tab_id: str, x: float, y: float) -> tuple[float, float, float]:
        metrics = self.viewport_metrics(tab_id)
        dpr = metrics["dpr"]
        return x / dpr - metrics["offsetLeft"], y / dpr - metrics["offsetTop"], dpr

    def cua_click(self, tab_id: str, x: float, y: float, count: int = 1, screenshot_id: str | None = None) -> dict[str, object]:
        conn = self.tab(tab_id).conn
        css_x, css_y, dpr = self._css_point(tab_id, x, y)
        for _ in range(max(count, 1)):
            conn.call("Input.dispatchMouseEvent", {"type": "mousePressed", "x": css_x, "y": css_y, "button": "left", "clickCount": count})
            conn.call("Input.dispatchMouseEvent", {"type": "mouseReleased", "x": css_x, "y": css_y, "button": "left", "clickCount": count})
        return {
            "ok": True,
            "action": "cua.click",
            "x": css_x,
            "y": css_y,
            "screenshotX": x,
            "screenshotY": y,
            "dpr": dpr,
            "screenshotId": screenshot_id,
            "coordinateSpace": "viewport",
            "backend": "cdp-ws",
        }

    def cua_move(self, tab_id: str, x: float, y: float) -> dict[str, object]:
        self.tab(tab_id).conn.call("Input.dispatchMouseEvent", {"type": "mouseMoved", "x": x, "y": y})
        return {"ok": True, "action": "cua.move", "x": x, "y": y, "backend": "cdp-ws"}

    def cua_drag(self, tab_id: str, path: list[object]) -> dict[str, object]:
        conn = self.tab(tab_id).conn
        points = [item for item in path if isinstance(item, dict)]
        if points:
            first = points[0]
            conn.call("Input.dispatchMouseEvent", {"type": "mousePressed", "x": first.get("x"), "y": first.get("y"), "button": "left"})
            for point in points[1:]:
                conn.call("Input.dispatchMouseEvent", {"type": "mouseMoved", "x": point.get("x"), "y": point.get("y")})
            last = points[-1]
            conn.call("Input.dispatchMouseEvent", {"type": "mouseReleased", "x": last.get("x"), "y": last.get("y"), "button": "left"})
        return {"ok": True, "action": "cua.drag", "backend": "cdp-ws"}

    def ax_get_state(self, tab_id: str, mode: str = "state", disable_diff: bool = True) -> dict[str, object]:
        raw = self.ax_write(tab_id, mode, disable_diff)
        acc = raw.get("accessibility") if isinstance(raw.get("accessibility"), dict) else {}
        tree = str(acc.get("tree") or "")
        return {"tree": tree, "emitted": False, "mode": mode}

    def ax_perform_secondary(self, tab_id: str, index: int, action: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next((item for item in tab.ax_nodes if int(item.get("index") or -1) == index), None)
        if node and node.get("backendDOMNodeId") is not None:
            resolved = tab.conn.call("DOM.resolveNode", {"backendNodeId": int(node["backendDOMNodeId"])})
            object_id = (resolved.get("object") or {}).get("objectId")
            if object_id:
                tab.conn.call(
                    "Runtime.callFunctionOn",
                    {
                        "objectId": object_id,
                        "functionDeclaration": "function(name){ if (this.ariaExpanded==='false') this.click(); }",
                        "arguments": [{"value": action}],
                        "returnByValue": True,
                    },
                )
        return {"ok": True, "action": action, "element_index": index, "backend": "cdp-ws"}

    def cdp_send(self, tab_id: str, method: str, params: dict[str, object] | None = None) -> dict[str, object]:
        result = self.tab(tab_id).conn.call(method, params)
        return {"result": result, "backend": "cdp-ws"}

    def cdp_read_events(self, tab_id: str, after_sequence: int = 0, methods: list[str] | None = None, timeout_ms: int = 0) -> list[dict[str, object]]:
        return self.tab(tab_id).conn.read_events(after_sequence, methods, timeout_ms)

    def last_download(self, tab_id: str) -> dict[str, object] | None:
        downloads = self.tab(tab_id).downloads
        return downloads[-1] if downloads else None

    def cancel_download(self, tab_id: str) -> dict[str, object]:
        last = self.last_download(tab_id)
        if last is None:
            return {"cancelled": False, "error": "no download"}
        guid = last.get("guid")
        if guid:
            try:
                self.tab(tab_id).conn.call("Browser.cancelDownload", {"guid": guid})
            except RuntimeError:
                pass
        last["cancelled"] = True
        last["failure"] = "cancelled"
        return {"cancelled": True, "suggestedFilename": last.get("suggestedFilename"), "url": last.get("url")}

    def set_files(self, tab_id: str, files: list[str], multiple: bool = False) -> dict[str, object]:
        tab = self.tab(tab_id)
        try:
            tab.conn.call("Page.setInterceptFileChooserDialog", {"enabled": True})
        except RuntimeError:
            pass
        doc = tab.conn.call("DOM.getDocument", {"depth": 0})
        root = (doc.get("root") or {}).get("nodeId")
        node = tab.conn.call("DOM.querySelector", {"nodeId": root, "selector": "input[type=file]"})
        node_id = node.get("nodeId")
        if not node_id:
            return {"ok": False, "error": "no file input", "files": files}
        backend = tab.conn.call("DOM.describeNode", {"nodeId": node_id})
        backend_id = ((backend.get("node") or {}).get("backendNodeId"))
        payload = {"files": files, "nodeId": node_id}
        if backend_id:
            payload["backendNodeId"] = backend_id
        tab.conn.call("DOM.setFileInputFiles", payload)
        return {"ok": True, "action": "fileChooser.setFiles", "files": files, "isMultiple": multiple or len(files) > 1, "backend": "cdp-ws"}

    def tab_logs(self, tab_id: str, levels: list[str] | None = None, filter_text: str = "", limit: int = 50, url: str = "") -> list[dict[str, object]]:
        rows = []
        for msg in self.tab(tab_id).conn.events:
            if msg.get("method") != "Runtime.consoleAPICalled":
                continue
            params = msg.get("params") if isinstance(msg.get("params"), dict) else {}
            level = str(params.get("type") or "log")
            args = params.get("args") if isinstance(params.get("args"), list) else []
            text = " ".join(str((item or {}).get("value") or (item or {}).get("description") or "") for item in args if isinstance(item, dict))
            rows.append({"level": level, "message": text, "timestamp": str(params.get("timestamp") or ""), "url": self.tab(tab_id).url})
        if levels:
            rows = [row for row in rows if row.get("level") in set(levels)]
        if filter_text:
            rows = [row for row in rows if filter_text.lower() in row.get("message", "").lower()]
        if url:
            rows = [row for row in rows if url.lower() in str(row.get("url") or "").lower()]
        return rows[: max(int(limit), 1)]

    def ax_drag_points(self, tab_id: str, from_x: float, from_y: float, to_x: float, to_y: float) -> dict[str, object]:
        result = self.cua_drag(tab_id, [{"x": from_x, "y": from_y}, {"x": to_x, "y": to_y}])
        result["action"] = "ax.drag"
        result["from"] = [from_x, from_y]
        result["to"] = [to_x, to_y]
        return result

    def _enable_downloads(self, tab: CdpTab) -> Path:
        import tempfile
        from pathlib import Path

        # NOT janitor-owned: a download is the **user's** artifact, not a plugin
        # intermediate. The model gets the path back and may copy the file out,
        # so it must survive the tab and the sidecar. (SEC-05's exemption rule:
        # a file the caller was pointed at is not ours to delete.)
        dest = Path(getattr(tab, "_download_dir", "") or tempfile.mkdtemp())
        dest.mkdir(parents=True, exist_ok=True)
        tab._download_dir = dest  # type: ignore[attr-defined]
        try:
            tab.conn.call("Browser.setDownloadBehavior", {"behavior": "allow", "downloadPath": str(dest), "eventsEnabled": True})
        except RuntimeError:
            try:
                tab.conn.call("Page.setDownloadBehavior", {"behavior": "allow", "downloadPath": str(dest)})
            except RuntimeError:
                pass
        return dest

    def _harvest_download_event(self, tab: CdpTab, dest: Path, timeout_ms: int = 4000) -> dict[str, object] | None:
        events = tab.conn.read_events(0, ["Browser.downloadWillBegin", "Page.downloadWillBegin", "Browser.downloadProgress"], timeout_ms)
        begin = next((item for item in events if "downloadWillBegin" in str(item.get("method") or "")), None)
        if begin is None:
            return None
        params = begin.get("params") if isinstance(begin.get("params"), dict) else {}
        suggested = str(params.get("suggestedFilename") or "download.bin")
        url = str(params.get("url") or tab.url)
        tab.conn.read_events(int(begin.get("sequence") or 0), ["Browser.downloadProgress"], timeout_ms)
        path = dest / suggested
        if not path.is_file():
            matches = sorted(dest.glob("*"), key=lambda item: item.stat().st_mtime, reverse=True)
            path = matches[0] if matches else path
        if not path.is_file():
            return None
        item = {
            "filename": path.name,
            "suggestedFilename": suggested,
            "url": url,
            "path": str(path),
            "guid": params.get("guid"),
            "failure": None,
            "cancelled": False,
            "event": True,
        }
        tab.downloads.append(item)
        return item

    def download_media(self, tab_id: str, node_id: int | None = None, x: float | None = None, y: float | None = None) -> dict[str, object]:
        import tempfile
        from pathlib import Path

        from computer_use.browser_export import FETCH_AS_B64_JS

        tab = self.tab(tab_id)
        dest = self._enable_downloads(tab)
        href = tab.url
        if node_id is not None:
            found = self._eval(
                tab_id,
                f"""(() => {{
                  const nodes = {VISIBLE_DOM_JS};
                  const hit = nodes.find((n) => n.node_id === {int(node_id)});
                  if (!hit) return location.href;
                  const el = document.querySelector(hit.selector);
                  if (el && el.click) el.click();
                  return (el && (el.href || el.currentSrc || el.src)) || location.href;
                }})()""",
            )
            href = str(found or tab.url)
        harvested = self._harvest_download_event(tab, dest)
        if harvested is not None:
            harvested["node_id"] = node_id
            harvested["x"] = x
            harvested["y"] = y
            return {"ok": True, "action": "downloadMedia", "download": harvested, "backend": "cdp-download-event"}
        fetched = self._eval(tab_id, f"({FETCH_AS_B64_JS})({json.dumps(href)})", await_promise=True) or {}
        raw = base64.b64decode(str(fetched.get("b64") or "")) if fetched.get("b64") else b""
        if not raw:
            raw = b"cdp-download"
        path = janitor.new_path(f"cdp-media-{tab.id}.bin", owner=str(tab.id))
        path.write_bytes(raw)
        item = {
            "filename": path.name,
            "suggestedFilename": path.name,
            "url": href,
            "node_id": node_id,
            "x": x,
            "y": y,
            "path": str(path),
            "failure": None,
            "cancelled": False,
            "event": False,
        }
        tab.downloads.append(item)
        return {"ok": True, "action": "downloadMedia", "download": item, "backend": "cdp-ws"}

    def history_query(self, queries: list[str] | None = None, limit: int = 20, start: str = "", end: str = "") -> list[dict[str, object]]:
        rows: list[dict[str, object]] = []
        for tab in self.tabs.values():
            try:
                hist = tab.conn.call("Page.getNavigationHistory")
            except RuntimeError:
                continue
            for entry in hist.get("entries") or []:
                if not isinstance(entry, dict):
                    continue
                rows.append({"url": str(entry.get("url") or ""), "title": str(entry.get("title") or ""), "dateVisited": ""})
        if queries:
            needles = [item.lower() for item in queries]
            rows = [row for row in rows if any(n in str(row.get("url") or "").lower() or n in str(row.get("title") or "").lower() for n in needles)]
        from computer_use.chrome_history import read_history

        profile = read_history(queries, limit, start, end)
        seen = {row["url"] for row in rows}
        for row in profile:
            if row["url"] not in seen:
                rows.append(row)
        return rows[: max(int(limit), 1)]

    def start_audio(self) -> dict[str, object]:
        import tempfile
        from pathlib import Path

        from computer_use.audio_win import start_recording

        path = Path(tempfile.gettempdir()) / "computer-audio.wav"
        # The recording is a user artifact that would otherwise stay in the
        # system temp dir forever. Register it so the janitor takes it away when
        # the sidecar exits; the path itself is unchanged, so the stop half of
        # this flow still finds it.
        janitor.track(path)
        handle, backend = start_recording(path)
        self._audio_handle = handle
        self._audio_path = path
        return {"ok": True, "recording": True, "path": str(path), "backend": backend}

    def stop_audio(self) -> dict[str, object]:
        from computer_use.audio_win import stop_recording

        path = Path(getattr(self, "_audio_path", "") or (Path(tempfile.gettempdir()) / "computer-audio.wav"))
        stop_recording(getattr(self, "_audio_handle", None), path)
        self._audio_handle = None
        return {"ok": True, "recording": False, "path": str(path), "bytes": path.stat().st_size if path.is_file() else 0}

    def inner_text(self, tab_id: str) -> str:
        value = self._eval(tab_id, "document.body ? document.body.innerText : ''")
        return str(value or "")

    def page_assets_list(self, tab_id: str) -> dict[str, object]:
        import re

        from computer_use.browser_export import ASSETS_JS, FETCH_AS_B64_JS

        data = self._eval(tab_id, f"({ASSETS_JS})()") or {}
        if not isinstance(data, dict):
            data = {"assets": [], "summary": {"totalCount": 0}}
        assets = list(data.get("assets") or [])
        seen = {str(item.get("url") or "") for item in assets if isinstance(item, dict)}
        sheets = [item for item in assets if isinstance(item, dict) and item.get("kind") == "stylesheet"]
        for sheet in sheets[:8]:
            url = str(sheet.get("url") or "")
            try:
                fetched = self._eval(tab_id, f"({FETCH_AS_B64_JS})({json.dumps(url)})", await_promise=True) or {}
                css = base64.b64decode(str(fetched.get("b64") or "")).decode("utf-8", errors="ignore")
            except Exception:
                continue
            for href in re.findall(r"url\((['\"]?)([^)'\"]+)\1\)", css):
                raw_url = href[1] if isinstance(href, tuple) else href
                if not raw_url or raw_url.startswith("data:"):
                    continue
                if raw_url not in seen:
                    seen.add(raw_url)
                    kind = "font" if any(raw_url.lower().endswith(ext) for ext in (".woff", ".woff2", ".ttf", ".otf", ".eot")) else "image"
                    assets.append({"id": f"a{len(assets)+1}", "kind": kind, "name": raw_url.split("/")[-1].split("?")[0], "url": raw_url, "sources": [{"kind": "css-url"}]})
        by_kind: dict[str, int] = {}
        for item in assets:
            kind = str(item.get("kind") or "other")
            by_kind[kind] = by_kind.get(kind, 0) + 1
        data["assets"] = assets
        summary = data.get("summary") if isinstance(data.get("summary"), dict) else {}
        summary["totalCount"] = len(assets)
        summary["byKind"] = by_kind
        data["summary"] = summary
        return data

    def webmcp_fetch(self, tab_id: str) -> dict[str, object]:
        from computer_use.browser_export import WEBMCP_JS

        data = self._eval(tab_id, f"({WEBMCP_JS})()") or {}
        return data if isinstance(data, dict) else {"tools": [], "description": ""}

    def export_authenticated(self, kind: str, tab_id: str, spec: dict[str, object]) -> dict[str, object]:
        import base64
        import tempfile
        from pathlib import Path

        from computer_use.browser_export import FETCH_AS_B64_JS, YOUTUBE_TRANSCRIPT_JS, gsuite_export_url

        tab = self.tab(tab_id)
        if kind == "youtube":
            from computer_use.browser_export import parse_timedtext_xml

            payload = self._eval(tab_id, f"({YOUTUBE_TRANSCRIPT_JS})()") or {}
            text = payload if isinstance(payload, str) else str((payload or {}).get("text") or "")
            tracks = [] if isinstance(payload, str) else list((payload or {}).get("tracks") or [])
            if not text and tracks:
                track_url = str((tracks[0] or {}).get("baseUrl") or "")
                if track_url:
                    fetched = self._eval(tab_id, f"({FETCH_AS_B64_JS})({json.dumps(track_url)})", await_promise=True) or {}
                    xml = base64.b64decode(str(fetched.get("b64") or "")).decode("utf-8", errors="ignore")
                    text = parse_timedtext_xml(xml)
            path = janitor.new_path(f"yt-{tab.id}.txt", owner=str(tab.id))
            path.write_text(text, encoding="utf-8")
            return {"path": str(path), "bytes": path.stat().st_size, "tracks": len(tracks), "backend": "cdp-timedtext" if tracks else "cdp-login"}
        fmt = str(spec.get("type") or "pdf")
        export_url = gsuite_export_url(tab.url, fmt)
        if not export_url:
            text = self.inner_text(tab_id)
            path = janitor.new_path(f"export-{tab.id}.txt", owner=str(tab.id))
            path.write_text(text, encoding="utf-8")
            return {"path": str(path), "bytes": path.stat().st_size, "backend": "cdp-text"}
        fetched = self._eval(tab_id, f"({FETCH_AS_B64_JS})({json.dumps(export_url)})", await_promise=True) or {}
        raw = base64.b64decode(str(fetched.get("b64") or "")) if fetched.get("b64") else b""
        from computer_use.browser_export import is_login_wall

        if not raw or is_login_wall(raw, str(fetched.get("contentType") or "")):
            return {
                "path": None,
                "bytes": 0,
                "exportUrl": export_url,
                "status": fetched.get("status"),
                "loginRequired": True,
                "backend": "gsuite-login-wall",
            }
        path = janitor.new_path(f"gsuite-{tab.id}.{fmt}", owner=str(tab.id))
        path.write_bytes(raw)
        return {"path": str(path), "bytes": len(raw), "exportUrl": export_url, "status": fetched.get("status"), "loginRequired": False, "backend": "cdp-login"}

    def page_assets_bundle(self, tab_id: str, inventory_id: str = "") -> dict[str, object]:
        import base64
        import json as json_lib
        from pathlib import Path

        from computer_use.browser_export import FETCH_AS_B64_JS

        inventory = self.page_assets_list(tab_id)
        directory = janitor.new_dir("assets", owner=str(tab.id))
        saved = []
        failures = []
        for asset in inventory.get("assets") or []:
            if not isinstance(asset, dict):
                continue
            url = str(asset.get("url") or "")
            try:
                fetched = self._eval(tab_id, f"({FETCH_AS_B64_JS})({json_lib.dumps(url)})", await_promise=True) or {}
                raw = base64.b64decode(str(fetched.get("b64") or "")) if fetched.get("b64") else b""
                dest = directory / str(asset.get("name") or asset.get("id") or "asset")
                dest.write_bytes(raw)
                saved.append({**asset, "path": str(dest), "contentType": fetched.get("contentType")})
            except Exception as exc:
                failures.append({**asset, "reason": str(exc)})
        manifest = directory / "manifest.json"
        manifest.write_text(json_lib.dumps({"id": inventory_id or inventory.get("id"), "assets": saved}, indent=2), encoding="utf-8")
        return {
            "directoryPath": str(directory),
            "manifestPath": str(manifest),
            "assets": saved,
            "failures": failures,
            "summary": {"downloadedCount": len(saved), "failedCount": len(failures), "requestedCount": len(saved) + len(failures), "elapsedMs": 0},
        }

    def get_tab_context(self, provider_tab_id: str, title: str, url: str) -> dict[str, object]:
        if provider_tab_id in self.tabs:
            tab = self.tabs[provider_tab_id]
            text = self.inner_text(tab.id)
            return {"kind": "text", "title": tab.title, "url": tab.url, "text": text[:4000], "truncated": len(text) > 4000, "claimed": False}
        return {"unavailable": True, "claimed": False}

    def close(self) -> None:
        for tab in list(self.tabs.values()):
            tab.conn.close()
        self.tabs.clear()
        if self.proc is not None:
            self.proc.kill()
            self.proc = None
