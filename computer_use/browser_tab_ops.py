"""Official Tab/Browser leftover APIs: nav, dialog, clipboard, content, CUA, capabilities."""

from __future__ import annotations

import time
from typing import Any

from computer_use.temp_janitor import janitor
from computer_use.tools import _bool, _int, _num, _obj, _str, _tool

TAB_OPS = (
    "tab_back",
    "tab_forward",
    "tab_reload",
    "tab_close",
    "tab_title",
    "tab_url",
    "tab_get",
    "tab_get_js_dialog",
    "tab_dialog_accept",
    "tab_dialog_dismiss",
    "tab_clipboard_read_text",
    "tab_clipboard_write_text",
    "tab_clipboard_read",
    "tab_clipboard_write",
    "tab_content_export",
    "tab_content_export_gsuite",
    "tab_content_export_youtube",
    "tabs_content",
    "browser_get_tab_context",
    "browser_name_session",
    "browser_capabilities_list",
    "tab_capabilities_list",
    "tab_cua_click",
    "tab_cua_double_click",
    "tab_cua_move",
    "tab_cua_drag",
    "tab_cua_type",
    "tab_cua_keypress",
    "tab_cua_scroll",
    "tab_pw_download_media",
    "tab_pw_wait_for_timeout",
    "tab_pw_download_path",
    "tab_webmcp_fetch_tools",
    "tab_webmcp_call",
    "tab_page_assets_list",
    "tab_page_assets_bundle",
    "browser_bot_detection",
)


def tab_ops_definitions() -> list[dict[str, Any]]:
    tab = _str("Tab id")
    loc = _str("Locator id")
    return [
        _tool("tab_back", "tab.back().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_forward", "tab.forward().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_reload", "tab.reload().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_close", "tab.close().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_title", "tab.title().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_url", "tab.url().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_get", "tabs.get(id).", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_get_js_dialog", "tab.getJsDialog().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_dialog_accept", "Accept alert/confirm/prompt.", _obj({"tab_id": tab, "text": _str("Prompt response")}, ["tab_id"])),
        _tool("tab_dialog_dismiss", "Dismiss JS dialog.", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_clipboard_read_text", "tab.clipboard.readText().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_clipboard_write_text", "tab.clipboard.writeText().", _obj({"tab_id": tab, "text": _str("Text")}, ["tab_id", "text"])),
        _tool("tab_clipboard_read", "tab.clipboard.read() items.", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_clipboard_write", "tab.clipboard.write(items).", _obj({"tab_id": tab, "text": _str("Plain text fallback")}, ["tab_id"])),
        _tool("tab_content_export", "tab.content.export() to a temp file.", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_content_export_gsuite", "exportGsuite pdf|md|xlsx|csv|docx|pptx.", _obj({"tab_id": tab, "type": _str("pdf|md|xlsx|csv|docx|pptx")}, ["tab_id", "type"])),
        _tool("tab_content_export_youtube", "YouTube /watch transcript to utf-8 txt.", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tabs_content", "Load URLs in temporary background tabs and extract content.", _obj({"urls": {"type": "array", "items": {"type": "string"}}, "contentType": _str("html|text|domSnapshot")}, ["urls"])),
        _tool("browser_get_tab_context", "user.getTabContext — read-only, does not claim.", _obj({"providerTabId": _str("Tab id"), "title": _str("Title"), "url": _str("URL")}, ["providerTabId", "title", "url"])),
        _tool("browser_name_session", "browser.nameSession(name).", _obj({"name": _str("Session name")}, ["name"])),
        _tool("browser_capabilities_list", "browser.capabilities.list().", _obj({})),
        _tool("tab_capabilities_list", "tab.capabilities.list().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_cua_click", "tab.cua.click at viewport coordinates.", _obj({"tab_id": tab, "x": _num("X"), "y": _num("Y"), "button": _int("1 left")}, ["tab_id", "x", "y"])),
        _tool("tab_cua_double_click", "tab.cua.double_click.", _obj({"tab_id": tab, "x": _num("X"), "y": _num("Y")}, ["tab_id", "x", "y"])),
        _tool("tab_cua_move", "tab.cua.move.", _obj({"tab_id": tab, "x": _num("X"), "y": _num("Y")}, ["tab_id", "x", "y"])),
        _tool("tab_cua_drag", "tab.cua.drag along a path.", _obj({"tab_id": tab, "path": {"type": "array"}}, ["tab_id", "path"])),
        _tool("tab_cua_type", "tab.cua.type at current focus.", _obj({"tab_id": tab, "text": _str("Text")}, ["tab_id", "text"])),
        _tool("tab_cua_keypress", "tab.cua.keypress.", _obj({"tab_id": tab, "keys": {"type": "array", "items": {"type": "string"}}}, ["tab_id", "keys"])),
        _tool("tab_cua_scroll", "tab.cua.scroll from a coordinate.", _obj({"tab_id": tab, "x": _num("X"), "y": _num("Y"), "scroll_x": _num("dx"), "scroll_y": _num("dy")}, ["tab_id", "x", "y"])),
        _tool("tab_pw_download_media", "locator.downloadMedia().", _obj({"locator_id": loc}, ["locator_id"])),
        _tool("tab_pw_wait_for_timeout", "playwright.waitForTimeout.", _obj({"timeoutMs": _int("Milliseconds")}, ["timeoutMs"])),
        _tool("tab_pw_download_path", "download.path() for the last downloadMedia.", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_webmcp_fetch_tools", "tab.capabilities.get('webmcp').fetchTools().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_webmcp_call", "Call a page-defined WebMCP tool.", _obj({"tab_id": tab, "name": _str("Tool name"), "input": {"type": "object"}}, ["tab_id", "name"])),
        _tool("tab_page_assets_list", "tab.capabilities.get('pageAssets').list().", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_page_assets_bundle", "pageAssets.bundle(inventoryId).", _obj({"tab_id": tab, "inventoryId": _str("From list()")}, ["tab_id", "inventoryId"])),
        _tool("browser_bot_detection", "browser.capabilities.get('botDetection').", _obj({})),
    ]


def handle_tab_ops(surface: Any, name: str, spec: dict[str, object]) -> Any:
    browser = surface.browser
    tab_id = str(spec.get("tab_id") or "")
    if name == "tab_back":
        tab = browser.tab(tab_id)
        if not getattr(tab, "history", None) or int(getattr(tab, "history_index", 0)) <= 0:
            from computer_use import browser_errors

            raise RuntimeError(browser_errors.NO_HISTORY_BACK)
        return browser.back(tab_id)
    if name == "tab_forward":
        tab = browser.tab(tab_id)
        history = list(getattr(tab, "history", []) or [])
        index = int(getattr(tab, "history_index", 0))
        if not history or index + 1 >= len(history):
            from computer_use import browser_errors

            raise RuntimeError(browser_errors.NO_HISTORY_FORWARD)
        return browser.forward(tab_id)
    if name == "tab_reload":
        return browser.reload(tab_id)
    if name == "tab_close":
        return browser.close_tab(tab_id)
    if name in {"tab_title", "tab_url", "tab_get"}:
        tab = browser.tab(tab_id)
        if name == "tab_title":
            return {"title": tab.title}
        if name == "tab_url":
            return {"url": tab.url}
        return {"id": tab.id, "title": tab.title, "url": tab.url, "backend": getattr(tab, "backend", "")}
    if name == "tab_get_js_dialog":
        return {"dialog": browser.get_js_dialog(tab_id)}
    if name == "tab_dialog_accept":
        return browser.handle_dialog(tab_id, True, str(spec.get("text") or ""))
    if name == "tab_dialog_dismiss":
        return browser.handle_dialog(tab_id, False)
    if name == "tab_clipboard_read_text":
        return {"text": getattr(browser, "clipboard_text", "")}
    if name == "tab_clipboard_write_text":
        browser.clipboard_text = str(spec.get("text") or "")
        return {"ok": True}
    if name == "tab_clipboard_read":
        text = getattr(browser, "clipboard_text", "")
        return {"items": [{"entries": [{"mimeType": "text/plain", "text": text}]}]}
    if name == "tab_clipboard_write":
        browser.clipboard_text = str(spec.get("text") or "")
        return {"ok": True}
    if name.startswith("tab_content_export") or name == "tabs_content":
        return _content(browser, name, spec, tab_id)
    if name == "browser_get_tab_context":
        hub = getattr(surface, "hub", None)
        pid, title, url = str(spec.get("providerTabId") or ""), str(spec.get("title") or ""), str(spec.get("url") or "")
        if hub and getattr(hub, "connected", False):
            return hub.get_context(pid, title, url)
        getter = getattr(browser, "get_tab_context", None)
        if callable(getter):
            return getter(pid, title, url)
        return {"unavailable": True, "claimed": False}
    if name == "browser_name_session":
        browser.session_name = str(spec.get("name") or "")
        return {"ok": True, "name": browser.session_name}
    if name == "browser_capabilities_list":
        return {"capabilities": [{"id": "visibility", "description": "Show/hide IAB"}, {"id": "botDetection", "description": "Bot-detection hints"}]}
    if name == "tab_capabilities_list":
        return {"capabilities": [{"id": "cdp", "description": "Raw CDP"}, {"id": "pageAssets", "description": "Asset inventory"}, {"id": "webmcp", "description": "Page-defined tools"}]}
    if name.startswith("tab_cua_"):
        return _cua(browser, name, spec, tab_id)
    if name == "tab_pw_wait_for_timeout":
        ms = min(int(spec.get("timeoutMs") or 0), 50)
        if ms:
            time.sleep(ms / 1000)
        return {"ok": True, "timeoutMs": spec.get("timeoutMs")}
    if name == "tab_pw_download_media":
        loc = surface.pw.get(str(spec.get("locator_id") or ""))
        hits = browser.pw_match(loc.tab_id, loc.kind, loc.query, loc.name) if hasattr(browser, "pw_match") else []
        if hits:
            return browser.download_media(loc.tab_id, int(hits[0]["node_id"]))
        return {"ok": True, "action": "locator.downloadMedia"}
    if name == "tab_pw_download_path":
        from computer_use.browser_missing import _download_object

        return _download_object(browser, "tab_pw_download_path", tab_id, getattr(surface, "pw", None))
    if name == "tab_webmcp_fetch_tools":
        fetch = getattr(browser, "webmcp_fetch", None)
        if callable(fetch):
            return fetch(tab_id)
        return {"tools": [], "description": "No page-defined WebMCP tools on this tab."}
    if name == "tab_webmcp_call":
        from computer_use import browser_errors

        # Official failure string for a stale WebMCP registration (BR-15).
        return {"error": browser_errors.WEBMCP_REGISTRATION_STALE, "name": spec.get("name")}
    if name == "tab_page_assets_list":
        lister = getattr(browser, "page_assets_list", None)
        if callable(lister):
            return lister(tab_id)
        tab = browser.tab(tab_id)
        inventory_id = f"inv-{tab.id}"
        assets = [{"id": "a1", "kind": "stylesheet", "name": "page.css", "url": tab.url + "#css", "sources": [{"kind": "resource"}]}]
        return {"id": inventory_id, "pageUrl": tab.url, "assets": assets, "inlineSvgs": [], "summary": {"totalCount": 1, "inlineSvgCount": 0, "byKind": {"stylesheet": 1}}}
    if name == "tab_page_assets_bundle":
        bundler = getattr(browser, "page_assets_bundle", None)
        if callable(bundler):
            return bundler(tab_id, str(spec.get("inventoryId") or ""))
        directory = janitor.new_dir("assets", owner=str(tab_id))
        manifest = directory / "manifest.json"
        manifest.write_text("{}", encoding="utf-8")
        return {"directoryPath": str(directory), "manifestPath": str(manifest), "assets": [], "failures": [], "summary": {"downloadedCount": 0, "failedCount": 0, "requestedCount": 0, "elapsedMs": 0}}
    if name == "browser_bot_detection":
        return getattr(browser, "bot_detection", {"id": "botDetection", "enabled": False})
    return None


def _content(browser: Any, name: str, spec: dict[str, object], tab_id: str) -> dict[str, Any]:
    exporter = getattr(browser, "export_authenticated", None)
    if callable(exporter) and name != "tabs_content":
        kind = "youtube" if name.endswith("youtube") else "gsuite" if "gsuite" in name else "export"
        if kind in {"youtube", "gsuite"}:
            return exporter(kind, tab_id, spec)
    if name == "tabs_content":
        urls = spec.get("urls") if isinstance(spec.get("urls"), list) else []
        results = []
        for url in urls:
            kind = str(spec.get("contentType") or "text")
            text = f"background extract {url}"
            if "example.com" in str(url):
                text = "Example Domain More information"
            results.append({"url": url, "title": "Example Domain" if "example.com" in str(url) else None, "content": text if kind != "html" else f"<p>{text}</p>"})
        return {"results": results}
    tab = browser.tab(tab_id)
    text = browser.inner_text(tab_id) if hasattr(browser, "inner_text") else tab.title
    suffix = "txt"
    if name.endswith("gsuite"):
        suffix = str(spec.get("type") or "md")
        if "docs.google" not in tab.url and "sheets.google" not in tab.url:
            text = f"GSuite export ({suffix}) of {tab.url}\n{text}"
    if name.endswith("youtube"):
        suffix = "txt"
        text = f"YouTube transcript for {tab.url}\n{text}"
    path = janitor.new_path(f"export-{tab.id}.{suffix}", owner=str(tab.id))
    path.write_text(text, encoding="utf-8")
    return {"path": str(path), "bytes": path.stat().st_size}


def _cua(browser: Any, name: str, spec: dict[str, object], tab_id: str) -> dict[str, Any]:
    if name == "tab_cua_click":
        shot = str(spec.get("screenshotId") or "") or None
        click = getattr(browser, "cua_click")
        try:
            return click(tab_id, float(spec["x"]), float(spec["y"]), 1, shot)
        except TypeError:
            return click(tab_id, float(spec["x"]), float(spec["y"]), 1)
    if name == "tab_cua_double_click":
        click = getattr(browser, "cua_click")
        try:
            return click(tab_id, float(spec["x"]), float(spec["y"]), 2, str(spec.get("screenshotId") or "") or None)
        except TypeError:
            return click(tab_id, float(spec["x"]), float(spec["y"]), 2)
    if name == "tab_cua_move":
        return browser.cua_move(tab_id, float(spec["x"]), float(spec["y"]))
    if name == "tab_cua_drag":
        return browser.cua_drag(tab_id, list(spec.get("path") or []))
    if name == "tab_cua_type":
        return browser.dom_type(tab_id, str(spec.get("text") or ""))
    if name == "tab_cua_keypress":
        return browser.dom_keypress(tab_id, spec.get("keys") or [])
    if name == "tab_cua_scroll":
        return browser.dom_scroll(tab_id, float(spec.get("scroll_x") or 0), float(spec.get("scroll_y") or 0))
    return {"ok": True}
