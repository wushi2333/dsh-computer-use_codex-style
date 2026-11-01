"""In-memory Browser AX + DOM surface recovered from official tab.ax / tab.dom_cua."""

from __future__ import annotations

from dataclasses import dataclass, field

from computer_use.models import Bounds, UINode
from computer_use.png import solid_png
from computer_use.temp_janitor import janitor
from computer_use.tree_format import format_tree


def example_nodes() -> list[UINode]:
    return [
        UINode(0, "WebArea", "Example Domain", Bounds(0, 0, 800, 600), depth=0),
        UINode(1, "heading", "Example Domain", Bounds(40, 40, 400, 40), depth=1),
        UINode(2, "link", "More information", Bounds(40, 120, 200, 24), depth=1, actions=["Invoke"]),
        UINode(3, "textbox", "Search", Bounds(40, 160, 240, 28), depth=1, actions=["SetValue"]),
    ]


def example_dom() -> list[dict[str, object]]:
    return [
        {"node_id": 1, "tag": "h1", "role": "heading", "name": "Example Domain", "selector": "h1"},
        {"node_id": 2, "tag": "a", "role": "link", "name": "More information", "selector": "a"},
        {"node_id": 3, "tag": "input", "role": "textbox", "name": "Search", "selector": "input#q", "value": ""},
        {"node_id": 4, "tag": "input", "role": "checkbox", "name": "Agree", "selector": "input#agree", "checked": False},
        {"node_id": 5, "tag": "select", "role": "combobox", "name": "Color", "selector": "select#color", "value": ""},
    ]


@dataclass
class FakeTab:
    id: str
    url: str = "https://example.com/"
    title: str = "Example Domain"
    nodes: list[UINode] = field(default_factory=example_nodes)
    dom: list[dict[str, object]] = field(default_factory=example_dom)
    focused: int = 3
    prev_tree: str = ""
    backend: str = "iab"
    provider_tab_id: str = ""
    visible: bool = False
    handoff: bool = False
    deliverable: bool = False
    manual_handoff: bool = False
    files: list[str] = field(default_factory=list)
    downloads: list[dict[str, object]] = field(default_factory=list)
    history: list[str] = field(default_factory=list)
    history_index: int = 0
    dialog: dict[str, object] | None = None
    logs: list[dict[str, object]] = field(default_factory=list)
    file_chooser_multiple: bool = False
    selected_text: str = ""
    cdp_seq: int = 0
    cdp_events: list[dict[str, object]] = field(default_factory=list)
    #: BR-07: when set, `content:"screenshot"` reports it instead of an empty image.
    screenshot_unavailable: str = ""


class FakeBrowser:
    def __init__(self) -> None:
        self.session_id = "sess-iab"
        self.extension_id = "ext-local"
        self.visible = False
        self.claimed: list[str] = []
        self.browsers = [
            {"id": "iab", "name": "In-app browser", "type": "iab", "metadata": {"codexSessionId": self.session_id}},
            {"id": "edge", "name": "Microsoft Edge", "type": "cdp"},
            {"id": "chrome", "name": "Google Chrome", "type": "extension", "metadata": {"extensionInstanceId": self.extension_id}},
        ]
        self.tabs: dict[str, FakeTab] = {}
        self._n = 0
        self.audio: dict[str, object] = {"recording": False, "path": ""}
        self.clipboard_text = ""
        self.session_name = ""
        self.bot_detection = {"id": "botDetection", "enabled": False}
        self.viewport: dict[str, int] | None = None
        self.management_audit: list[dict[str, object]] = []
        self.history_entries: list[dict[str, object]] = []
        self.default_browser_id = "iab"

    def list_browsers(self) -> list[dict[str, object]]:
        return list(self.browsers)

    def get_browser(self, browser_id: str) -> dict[str, object]:
        for item in self.browsers:
            if item.get("id") == browser_id or item.get("type") == browser_id:
                return dict(item)
        raise KeyError(f"unknown browser {browser_id}")

    def get_default_browser(self) -> dict[str, object]:
        return self.get_browser(self.default_browser_id)

    def get_browser_for_url(self, url: str) -> dict[str, object]:
        lowered = url.lower()
        if "chrome-extension" in lowered or lowered.startswith("chrome://"):
            return self.get_browser("chrome")
        if lowered.startswith("http") or lowered.startswith("https"):
            return self.get_default_browser()
        return self.get_default_browser()

    def new_tab(self, url: str = "https://example.com/", browser: str = "iab") -> FakeTab:
        self._n += 1
        backend = "iab"
        if browser in {"chrome", "extension"}:
            backend = "extension"
        elif browser in {"edge", "cdp"}:
            backend = "cdp"
        tab = FakeTab(id=f"tab-{self._n}", url=url, backend=backend, provider_tab_id=f"tab-{self._n}", visible=self.visible if backend == "iab" else True)
        if url in ("about:blank", "about:blank/"):
            tab.title = "Blank"
            tab.nodes = [UINode(0, "WebArea", "Blank", Bounds(0, 0, 800, 600))]
            tab.dom = []
        tab.history = [url]
        tab.history_index = 0
        tab.logs = [
            {"level": "log", "message": "navigation", "timestamp": "2026-01-01T00:00:00Z", "url": url}
        ]
        self.tabs[tab.id] = tab
        self.history_entries.append({"url": url, "title": tab.title, "dateVisited": "2026-01-01T00:00:00Z"})
        return tab

    def tabs_list(self) -> list[dict[str, object]]:
        return [
            {
                "id": tab.id,
                "providerTabId": tab.provider_tab_id or tab.id,
                "title": tab.title,
                "url": tab.url,
                "backend": tab.backend,
            }
            for tab in self.tabs.values()
        ]

    def selected_tab(self) -> dict[str, object] | None:
        if not self.tabs:
            return None
        last = list(self.tabs.values())[-1]
        return {"id": last.id, "title": last.title, "url": last.url}

    def set_viewport(self, viewport: dict[str, int] | None) -> dict[str, object]:
        self.viewport = viewport
        return {"ok": True, "viewport": viewport, "backend": "iab"}

    def management_call(self, area: str, method: str, args: dict[str, object] | None = None) -> dict[str, object]:
        entry = {"area": area, "method": method, "args": args or {}}
        self.management_audit.append(entry)
        return {"ok": True, **entry, "result": None, "backend": "iab"}

    def set_visible(self, visible: bool) -> None:
        self.visible = bool(visible)
        for tab in self.tabs.values():
            if tab.backend == "iab":
                tab.visible = self.visible

    def claim_tab(self, provider_tab_id: str, title: str, url: str) -> dict[str, object]:
        for tab in self.tabs.values():
            if tab.provider_tab_id == provider_tab_id and tab.title == title and tab.url == url:
                self.claimed.append(tab.id)
                return {"id": tab.id, "claimed": True, "type": tab.backend, "unavailable": False}
        return {"claimed": False, "unavailable": True, "reason": "tab mention no longer matches"}

    def download_media(self, tab_id: str, node_id: int | None = None, x: float | None = None, y: float | None = None) -> dict[str, object]:
        tab = self.tab(tab_id)
        path = janitor.new_path(f"media-{tab.id}.bin", owner=str(tab.id))
        path.write_bytes(b"media-bytes")
        item = {
            "filename": path.name,
            "suggestedFilename": path.name,
            "url": tab.url,
            "node_id": node_id,
            "x": x,
            "y": y,
            "path": str(path),
            "failure": None,
            "cancelled": False,
        }
        tab.downloads.append(item)
        return {"ok": True, "action": "downloadMedia", "download": item}

    def set_files(self, tab_id: str, files: list[str], multiple: bool = False) -> dict[str, object]:
        tab = self.tab(tab_id)
        tab.files = list(files)
        tab.file_chooser_multiple = bool(multiple) or len(files) > 1
        return {"ok": True, "action": "fileChooser.setFiles", "files": tab.files, "isMultiple": tab.file_chooser_multiple}

    def mark_handoff(self, tab_id: str, kind: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        if kind == "deliverable":
            tab.deliverable = True
        elif kind == "manual":
            tab.manual_handoff = True
        else:
            tab.handoff = True
        return {
            "ok": True,
            "tab_id": tab.id,
            "handoff": tab.handoff,
            "deliverable": tab.deliverable,
            "manual_handoff": tab.manual_handoff,
            "backend": "cloud" if kind == "manual" else tab.backend,
        }

    def start_audio(self) -> dict[str, object]:
        self.audio = {"recording": True, "path": "computer-audio.wav"}
        return {"ok": True, "recording": True, "app": "computer-audio"}

    def stop_audio(self) -> dict[str, object]:
        self.audio["recording"] = False
        return {"ok": True, "recording": False, "path": self.audio.get("path")}

    def tab(self, tab_id: str) -> FakeTab:
        if tab_id not in self.tabs:
            raise KeyError(f"unknown tab {tab_id}")
        return self.tabs[tab_id]

    def goto(self, tab_id: str, url: str) -> FakeTab:
        tab = self.tab(tab_id)
        tab.url = url
        if "example.com" in url:
            tab.title = "Example Domain"
            tab.nodes = example_nodes()
            tab.dom = example_dom()
        tab.history = tab.history[: tab.history_index + 1] + [url]
        tab.history_index = len(tab.history) - 1
        return tab

    def ax_text(self, tab: FakeTab) -> str:
        header = f"Title: {tab.title}\nURL: {tab.url}"
        return header + "\n" + format_tree(tab.nodes, tab.title, "browser")

    def ax_write(self, tab_id: str, mode: str = "state", disable_diff: bool = True) -> dict[str, object]:
        tab = self.tab(tab_id)
        text = self.ax_text(tab)
        diff = False
        shown = text
        if mode in ("state", "both") and tab.prev_tree and not disable_diff:
            from computer_use.compact import tree_diff

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
            "tree": [node.to_dict() for node in tab.nodes] if mode in ("state", "both") else [],
            "dom": tab.dom if mode in ("state", "both") else [],
        }
        if mode in ("state", "both"):
            payload["accessibility"] = {"tree": shown, "diff": diff, "focused_element": f"[{tab.focused}]"}
        if mode in ("screenshot", "both"):
            import base64

            if tab.screenshot_unavailable:
                payload["screenshot_unavailable"] = tab.screenshot_unavailable
            else:
                png = solid_png()
                b64 = base64.b64encode(png).decode("ascii")
                payload["screenshots"] = [
                    {
                        "id": f"{tab.id}-shot",
                        "emitted": False,
                        "bytes": len(png),
                        "url": f"data:image/png;base64,{b64}",
                    }
                ]
                payload["screenshot_bytes"] = len(png)
        return payload

    def ax_click(self, tab_id: str, index: int) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next(item for item in tab.nodes if item.index == index)
        tab.focused = index
        return {"ok": True, "action": "ax.click", "element_index": index, "name": node.name}

    def ax_set_value(self, tab_id: str, index: int, value: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next(item for item in tab.nodes if item.index == index)
        node.value = value
        tab.focused = index
        return {"ok": True, "action": "ax.setValue", "element_index": index, "value": value}

    def dom_snapshot(self, tab_id: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        return {"tab_id": tab.id, "url": tab.url, "nodes": tab.dom}

    def dom_click(self, tab_id: str, node_id: int, count: int = 1) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next(item for item in tab.dom if int(item["node_id"]) == node_id)
        tab.focused = int(node_id)
        action = "dom_cua.double_click" if count > 1 else "dom_cua.click"
        return {"ok": True, "action": action, "node_id": node_id, "selector": node["selector"]}

    def dom_double_click(self, tab_id: str, node_id: int) -> dict[str, object]:
        return self.dom_click(tab_id, node_id, count=2)

    def dom_type(self, tab_id: str, text: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        for node in tab.nodes:
            if node.index == tab.focused or node.role in {"textbox", "Edit"}:
                node.value += text
                break
        return {"ok": True, "action": "dom_cua.type", "text": text}

    def dom_keypress(self, tab_id: str, keys: object) -> dict[str, object]:
        return {"ok": True, "action": "dom_cua.keypress", "keys": keys}

    def dom_scroll(self, tab_id: str, scroll_x: float = 0, scroll_y: float = 0, node_id: int | None = None) -> dict[str, object]:
        return {"ok": True, "action": "dom_cua.scroll", "scroll_x": scroll_x, "scroll_y": scroll_y, "node_id": node_id}

    def pw_match(self, tab_id: str, kind: str, query: str, name: str | None = None) -> list[dict[str, object]]:
        tab = self.tab(tab_id)
        hits = []
        for node in tab.dom:
            role = str(node.get("role") or "")
            label = str(node.get("name") or "")
            selector = str(node.get("selector") or "")
            tag = str(node.get("tag") or "")
            if kind == "css" and query in {selector, tag}:
                hits.append(node)
            elif kind == "role" and role == query and (name is None or name.lower() in label.lower()):
                hits.append(node)
            elif kind in {"text", "label"} and query.lower() in label.lower():
                hits.append(node)
        return hits

    def pw_match_chain(self, tab_id: str, chain: list[dict[str, object]]) -> list[dict[str, object]]:
        hits = list(self.tab(tab_id).dom)
        for step in chain:
            kind = str(step.get("kind") or "css")
            query = str(step.get("query") or "")
            name = step.get("name")
            name_s = str(name) if name else None
            hits = self._filter_hits(hits, kind, query, name_s)
        return hits

    def _filter_hits(self, nodes: list[dict[str, object]], kind: str, query: str, name: str | None) -> list[dict[str, object]]:
        hits = []
        for node in nodes:
            role = str(node.get("role") or "")
            label = str(node.get("name") or "")
            selector = str(node.get("selector") or "")
            tag = str(node.get("tag") or "")
            if kind == "css" and query in {selector, tag}:
                hits.append(node)
            elif kind == "role" and role == query and (name is None or name.lower() in label.lower()):
                hits.append(node)
            elif kind in {"text", "label"} and query.lower() in label.lower():
                hits.append(node)
        return hits

    def back(self, tab_id: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        if tab.history_index > 0:
            tab.history_index -= 1
            tab.url = tab.history[tab.history_index]
        return {"ok": True, "url": tab.url, "action": "back"}

    def forward(self, tab_id: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        if tab.history_index + 1 < len(tab.history):
            tab.history_index += 1
            tab.url = tab.history[tab.history_index]
        return {"ok": True, "url": tab.url, "action": "forward"}

    def reload(self, tab_id: str) -> dict[str, object]:
        return {"ok": True, "url": self.tab(tab_id).url, "action": "reload"}

    def close_tab(self, tab_id: str) -> dict[str, object]:
        self.tabs.pop(tab_id, None)
        # The tab's plugin-generated exports and media captures go with it
        # (SEC-05). A *download* is the user's artifact and is not ours to
        # delete, so it was never registered with the janitor.
        janitor.release_owner(str(tab_id))
        return {"ok": True, "closed": tab_id}

    def get_js_dialog(self, tab_id: str) -> dict[str, object] | None:
        return self.tab(tab_id).dialog

    def handle_dialog(self, tab_id: str, accept: bool, text: str = "", expect: str | None = None) -> dict[str, object]:
        tab = self.tab(tab_id)
        dialog = tab.dialog
        if expect and (not dialog or dialog.get("type") != expect):
            return {"ok": False, "error": f"no {expect} dialog", "dialog": dialog}
        tab.dialog = None
        return {"ok": True, "accepted": accept, "text": text, "had": dialog, "type": (dialog or {}).get("type")}

    def inject_dialog(self, tab_id: str, kind: str, message: str = "hello") -> dict[str, object]:
        tab = self.tab(tab_id)
        tab.dialog = {"type": kind, "message": message, "defaultPrompt": ""}
        tab.logs.append({"level": "log", "message": f"dialog:{kind}:{message}", "timestamp": "2026-01-01T00:00:01Z", "url": tab.url})
        return {"dialog": tab.dialog}

    def ax_get(self, tab_id: str, index: int) -> dict[str, object]:
        node = next(item for item in self.tab(tab_id).nodes if item.index == index)
        return node.to_dict()

    def ax_get_state(self, tab_id: str, mode: str = "state", disable_diff: bool = True) -> dict[str, object]:
        tab = self.tab(tab_id)
        raw = self.ax_write(tab_id, mode, disable_diff)
        acc = raw.get("accessibility") if isinstance(raw.get("accessibility"), dict) else {}
        tree = str(acc.get("tree") or "")
        return {"tree": tree, "emitted": False, "mode": mode}

    def ax_drag_points(self, tab_id: str, from_x: float, from_y: float, to_x: float, to_y: float) -> dict[str, object]:
        return {"ok": True, "action": "ax.drag", "from": [from_x, from_y], "to": [to_x, to_y]}

    def ax_perform_secondary(self, tab_id: str, index: int, action: str) -> dict[str, object]:
        node = next(item for item in self.tab(tab_id).nodes if item.index == index)
        return {"ok": True, "action": action, "element_index": index, "name": node.name}

    def ax_select_text(self, tab_id: str, index: int, text: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next(item for item in tab.nodes if item.index == index)
        tab.selected_text = text
        tab.focused = index
        return {"ok": True, "action": "ax.selectText", "element_index": index, "text": text, "name": node.name}

    def ax_drag(self, tab_id: str, from_index: int, to_index: int) -> dict[str, object]:
        return {"ok": True, "action": "ax.drag", "from_index": from_index, "to_index": to_index}

    def set_checked(self, tab_id: str, node_id: int, checked: bool) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next(item for item in tab.dom if int(item["node_id"]) == node_id)
        node["checked"] = bool(checked)
        return {"ok": True, "checked": bool(checked), "node_id": node_id, "selector": node.get("selector")}

    def select_option(self, tab_id: str, node_id: int, value: str) -> dict[str, object]:
        tab = self.tab(tab_id)
        node = next(item for item in tab.dom if int(item["node_id"]) == node_id)
        node["value"] = value
        return {"ok": True, "value": value, "node_id": node_id}

    def tab_logs(self, tab_id: str, levels: list[str] | None = None, filter_text: str = "", limit: int = 50, url: str = "") -> list[dict[str, object]]:
        rows = list(self.tab(tab_id).logs)
        if levels:
            allowed = set(levels) | ({"warn"} if "warning" in levels else set())
            rows = [row for row in rows if row.get("level") in allowed]
        if filter_text:
            rows = [row for row in rows if filter_text.lower() in str(row.get("message") or "").lower()]
        if url:
            rows = [row for row in rows if url.lower() in str(row.get("url") or "").lower()]
        return rows[: max(int(limit), 1)]

    def cdp_send(self, tab_id: str, method: str, params: dict[str, object] | None = None) -> dict[str, object]:
        tab = self.tab(tab_id)
        tab.cdp_seq += 1
        event = {"sequence": tab.cdp_seq, "method": method, "params": params or {}}
        tab.cdp_events.append(event)
        return {"result": {"ok": True, "method": method}, "sequence": tab.cdp_seq}

    def cdp_read_events(self, tab_id: str, after_sequence: int = 0, methods: list[str] | None = None) -> list[dict[str, object]]:
        allowed = set(methods) if methods else None
        rows = []
        for event in self.tab(tab_id).cdp_events:
            seq = int(event.get("sequence") or 0)
            if seq <= after_sequence:
                continue
            if allowed is not None and str(event.get("method") or "") not in allowed:
                continue
            rows.append(event)
        return rows

    def last_download(self, tab_id: str) -> dict[str, object] | None:
        downloads = self.tab(tab_id).downloads
        return downloads[-1] if downloads else None

    def cancel_download(self, tab_id: str) -> dict[str, object]:
        last = self.last_download(tab_id)
        if last is None:
            return {"cancelled": False, "error": "no download"}
        last["cancelled"] = True
        last["failure"] = "cancelled"
        return {"cancelled": True, "suggestedFilename": last.get("suggestedFilename"), "url": last.get("url")}

    def history_query(self, queries: list[str] | None = None, limit: int = 20, start: str = "", end: str = "") -> list[dict[str, object]]:
        rows = list(self.history_entries)
        if queries:
            needles = [item.lower() for item in queries]
            rows = [row for row in rows if any(n in str(row.get("url") or "").lower() or n in str(row.get("title") or "").lower() for n in needles)]
        if start:
            rows = [row for row in rows if str(row.get("dateVisited") or "") >= start]
        if end:
            rows = [row for row in rows if str(row.get("dateVisited") or "") <= end]
        return rows[: max(int(limit), 1)]

    def cua_click(self, tab_id: str, x: float, y: float, count: int = 1, screenshot_id: str | None = None) -> dict[str, object]:
        return {
            "ok": True,
            "action": "cua.click",
            "x": x,
            "y": y,
            "clickCount": count,
            "screenshotId": screenshot_id,
            "coordinateSpace": "viewport",
        }

    def cua_move(self, tab_id: str, x: float, y: float) -> dict[str, object]:
        return {"ok": True, "action": "cua.move", "x": x, "y": y}

    def cua_drag(self, tab_id: str, path: list[object]) -> dict[str, object]:
        return {"ok": True, "action": "cua.drag", "path": path}

    def get_tab_context(self, provider_tab_id: str, title: str, url: str) -> dict[str, object]:
        for tab in self.tabs.values():
            if tab.provider_tab_id == provider_tab_id and tab.title == title and tab.url == url:
                text = " ".join(node.name for node in tab.nodes)
                return {"kind": "text", "title": tab.title, "url": tab.url, "text": text, "truncated": False, "claimed": False}
        return {"unavailable": True, "claimed": False}

    def inner_text(self, tab_id: str) -> str:
        return " ".join(node.name for node in self.tab(tab_id).nodes)

    def export_authenticated(self, kind: str, tab_id: str, spec: dict[str, object]) -> dict[str, object]:
        from computer_use.browser_export import gsuite_export_url

        tab = self.tab(tab_id)
        if kind == "youtube":
            path = janitor.new_path(f"yt-{tab.id}.txt", owner=str(tab.id))
            path.write_text(f"YouTube transcript for {tab.url}\n{self.inner_text(tab_id)}", encoding="utf-8")
            return {"path": str(path), "bytes": path.stat().st_size, "backend": "page-js"}
        fmt = str(spec.get("type") or "pdf")
        export = gsuite_export_url(tab.url, fmt)
        path = janitor.new_path(f"gsuite-{tab.id}.{fmt}", owner=str(tab.id))
        path.write_text(f"GSuite export of {export or tab.url}\n{self.inner_text(tab_id)}", encoding="utf-8")
        return {"path": str(path), "bytes": path.stat().st_size, "exportUrl": export, "backend": "page-fetch"}
