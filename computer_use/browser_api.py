from __future__ import annotations

import os
import threading
from typing import Any

from computer_use.browser_checks import BrowserSecurityPolicy
from computer_use.browser_extra import EXTRA_TOOLS, extra_tool_definitions, handle_extra
from computer_use.browser_lifecycle import TabLifecycle
from computer_use.browser_meta import response_meta_contribution, take_browser_notifications
from computer_use.browser_official import (
    ALIASES as OFFICIAL_ALIASES,
    AX_SCROLL_DIRECTIONS,
    OFFICIAL_COMMANDS,
    ax_action_kind,
    ax_action_payload,
    official_tool_definitions,
    resolve_official,
    rewrite_spec,
)
from computer_use.browser_fake import FakeBrowser
from computer_use.browser_pw import LocatorStore
from computer_use.browser_url_policy import (
    SUPPORTED_FAMILIES,
    UNSUPPORTED_FAMILIES,
    assert_browser_url_allowed,
)
from computer_use.extension_hub import ExtensionHub
from computer_use.tools import _bool, _int, _num, _obj, _str, _tool

BROWSER_TOOLS = (
    "browser_list",
    "tab_new",
    "tab_goto",
    "tab_ax_write",
    "tab_ax_click",
    "tab_ax_set_value",
    "tab_ax_type_text",
    "tab_ax_press_key",
    "tab_ax_scroll",
    "tab_dom_snapshot",
    "tab_dom_get_visible_dom",
    "tab_dom_click",
    "tab_dom_double_click",
    "tab_dom_type",
    "tab_dom_keypress",
    "tab_dom_scroll",
    "tab_screenshot",
    "tab_pw_locator",
    "tab_pw_get_by_role",
    "tab_pw_get_by_text",
    "tab_pw_get_by_label",
    "tab_pw_click",
    "tab_pw_fill",
    "tab_pw_count",
    "tab_pw_inner_text",
    "tab_pw_evaluate",
    "tab_pw_wait_for_load_state",
    "tab_pw_dom_snapshot",
) + EXTRA_TOOLS + OFFICIAL_COMMANDS


def browser_tool_definitions() -> list[dict[str, Any]]:
    tab = _str("Tab id from tab_new")
    loc = _str("Locator id from tab_pw_locator / get_by_*")
    return [
        _tool("browser_list", "List available browsers (iab/chrome/edge).", _obj({})),
        _tool("tab_new", "Create a tab. In-app browser id is iab.", _obj({"url": _str("Initial URL"), "browser": _str("iab, chrome, or edge")})),
        _tool("tab_goto", "Open a URL in this tab without reloading if already there.", _obj({"tab_id": tab, "url": _str("URL")}, ["tab_id", "url"])),
        _tool("tab_ax_write", "Emit tab accessibility state. Prefer state over screenshot.", _obj({"tab_id": tab, "mode": {"type": "string", "enum": ["state", "screenshot", "both"]}, "disableDiffing": _bool("Full tree")}, ["tab_id"])),
        _tool("tab_ax_click", "Click an AX element_index.", _obj({"tab_id": tab, "element_index": _int("AX index")}, ["tab_id", "element_index"])),
        _tool("tab_ax_set_value", "Set value of an AX element.", _obj({"tab_id": tab, "element_index": _int("AX index"), "value": _str("Value")}, ["tab_id", "element_index", "value"])),
        _tool("tab_ax_type_text", "Type into the focused AX element.", _obj({"tab_id": tab, "text": _str("Text")}, ["tab_id", "text"])),
        _tool("tab_ax_press_key", "Press a key chord in the tab.", _obj({"tab_id": tab, "key": _str("Return, Tab, ...")}, ["tab_id", "key"])),
        _tool("tab_ax_scroll", "Scroll an AX index.", _obj({"tab_id": tab, "element_index": _int("AX index"), "direction": _str("up/down/left/right")}, ["tab_id"])),
        _tool("tab_dom_snapshot", "Alias of tab.dom_cua.get_visible_dom.", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_dom_get_visible_dom", "Filtered visible DOM with node ids (official get_visible_dom).", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_dom_click", "dom_cua.click a node_id.", _obj({"tab_id": tab, "node_id": _int("DOM node id")}, ["tab_id", "node_id"])),
        _tool("tab_dom_double_click", "dom_cua.double_click a node_id.", _obj({"tab_id": tab, "node_id": _int("DOM node id")}, ["tab_id", "node_id"])),
        _tool("tab_dom_type", "dom_cua.type into the focused element (click first).", _obj({"tab_id": tab, "text": _str("Text")}, ["tab_id", "text"])),
        _tool("tab_dom_keypress", "dom_cua.keypress keys at the focused element.", _obj({"tab_id": tab, "keys": {"type": "array", "items": {"type": "string"}}}, ["tab_id", "keys"])),
        _tool("tab_dom_scroll", "dom_cua.scroll by deltas; optional node_id.", _obj({"tab_id": tab, "scroll_x": _num("dx"), "scroll_y": _num("dy"), "node_id": _int("DOM node id")}, ["tab_id"])),
        _tool("tab_screenshot", "Capture a tab screenshot.", _obj({"tab_id": tab}, ["tab_id"])),
        _tool("tab_pw_locator", "playwright.locator(selector).", _obj({"tab_id": tab, "selector": _str("CSS selector")}, ["tab_id", "selector"])),
        _tool("tab_pw_get_by_role", "playwright.getByRole.", _obj({"tab_id": tab, "role": _str("ARIA role"), "name": _str("Accessible name")}, ["tab_id", "role"])),
        _tool("tab_pw_get_by_text", "playwright.getByText.", _obj({"tab_id": tab, "text": _str("Text"), "exact": _bool("Exact match")}, ["tab_id", "text"])),
        _tool("tab_pw_get_by_label", "playwright.getByLabel.", _obj({"tab_id": tab, "text": _str("Label")}, ["tab_id", "text"])),
        _tool("tab_pw_click", "locator.click().", _obj({"locator_id": loc}, ["locator_id"])),
        _tool("tab_pw_fill", "locator.fill(value).", _obj({"locator_id": loc, "value": _str("Value")}, ["locator_id", "value"])),
        _tool("tab_pw_count", "locator.count().", _obj({"locator_id": loc}, ["locator_id"])),
        _tool("tab_pw_inner_text", "locator.innerText().", _obj({"locator_id": loc}, ["locator_id"])),
        _tool("tab_pw_evaluate", "playwright.evaluate read-only JS.", _obj({"tab_id": tab, "expression": _str("JS expression")}, ["tab_id", "expression"])),
        _tool("tab_pw_wait_for_load_state", "page.waitForLoadState.", _obj({"tab_id": tab, "state": _str("load|domcontentloaded|networkidle")}, ["tab_id"])),
        _tool("tab_pw_dom_snapshot", "playwright.domSnapshot string.", _obj({"tab_id": tab}, ["tab_id"])),
    ] + extra_tool_definitions() + official_tool_definitions()


def default_browser() -> Any:
    if os.environ.get("COMPUTER_USE_CDP", "").strip().lower() not in {"1", "true", "yes"}:
        return FakeBrowser()
    from computer_use.cdp_browser import CdpBrowser

    return CdpBrowser.try_connect() or FakeBrowser()


def origin_of(url: str) -> str:
    """Scheme + host for an http(s) URL, else an empty string."""
    lowered = str(url or "").strip().lower()
    for scheme in ("http://", "https://"):
        if lowered.startswith(scheme):
            rest = lowered[len(scheme):]
            return f"{scheme}{rest.split(chr(47), 1)[0]}"
    return ""


#: Command classes the official security layer keys off (04:514-523).
HISTORY_COMMANDS = frozenset({"browser_history"})
UPLOAD_COMMANDS = frozenset({"tab_pw_set_files"})
DOWNLOAD_COMMANDS = frozenset({"tab_pw_download_media", "tab_dom_download_media", "tab_cua_download_media"})
CDP_COMMANDS = frozenset({"tab_cdp_send", "tab_cdp_call", "tab_cdp_read_events", "tab_cdp_events"})
PAGE_ASSET_COMMANDS = frozenset({"tab_page_assets_bundle"})
NAVIGATION_COMMANDS = frozenset({"tab_goto", "navigate_tab_url"})
#: Official documents.json `requiredFor` (BR-18): a command is refused until every
#: listed document has been read with `documentation.get`.
DOCUMENT_REQUIRED: dict[str, tuple[str, ...]] = {
    "tab_cdp_call": ("confirmations", "capabilities/tab/cdp"),
    "tab_cdp_events": ("confirmations", "capabilities/tab/cdp"),
    "webmcp_list_tools": ("confirmations", "webmcp"),
    "webmcp_invoke_tool": ("confirmations", "webmcp"),
    "tab_browser_auth_handoff": ("capabilities/tab/browserAuth",),
}
DOCUMENT_READ_COMMANDS = frozenset({"documentation_get", "get_documentation", "get_browser_documentation"})
#: Commands with no page origin of their own (no browser-origin-access gate).
NON_PAGE_COMMANDS = frozenset({
    "browser_setup", "runtime_config", "browser_list", "tab_list", "tab_selected",
    "browser_open_tabs", "browser_claim_tab", "browser_user_claim_tab",
    "browser_user_get_tab_context", "browser_get", "browser_get_default",
    "browser_get_for_url", "browser_detect", "browser_capabilities_list",
    "browser_capabilities_get", "browser_visibility_get", "browser_visibility_set",
    "browser_viewport_set", "browser_viewport_reset", "browser_management_call",
    "browser_management_get_audit_trail", "tab_browser_auth_handoff",
    "documentation_get", "get_documentation", "get_browser_documentation",
    "list_browsers", "get_browser", "get_default_browser", "get_browser_for_url",
})


class BrowserSurface:
    ALLOWED_ENVIRONMENTS = frozenset({"codex-app", "training", "cloud"})

    def __init__(
        self,
        browser: FakeBrowser | None = None,
        approvals: Any = None,
        *,
        consent_path: str | os.PathLike[str] | None = None,
    ) -> None:
        from computer_use.browser_checks import BrowserSecurityPolicy
        from computer_use.browser_persistence import PersistedConsentStore

        self.browser = browser or default_browser()
        self.pw = LocatorStore()
        self.hub = ExtensionHub()
        #: DSH approval service hook: callable(ConsentRequest) -> decision string.
        self.approvals = approvals
        # BR-17: prompt results persist into the official config.global /
        # config.session(id) sections. The document is DSH-specific and is only
        # opened when the host asks for it (explicit path or $DSH_BROWSER_CONSENT_PATH);
        # otherwise grants stay in process memory. Nothing writes into the user's
        # home directory by default.
        if consent_path is None:
            consent_path = (os.environ.get("DSH_BROWSER_CONSENT_PATH") or "").strip() or None
        self._consent_path = consent_path
        # Official BROWSER_USE_SECURITY_MODE: empty means every check is enforced.
        self.security_policy = BrowserSecurityPolicy(
            mode=os.environ.get("BROWSER_USE_SECURITY_MODE", "").strip(),
            default_decision=os.environ.get("BROWSER_USE_CONSENT_MODE", "ask").strip() or "ask",
            store=PersistedConsentStore.load(consent_path) if consent_path else None,
            conversation_id=(
                os.environ.get("DSH_CONVERSATION_ID")
                or os.environ.get("CODEX_CONVERSATION_ID")
                or ""
            ).strip(),
        )
        self.lifecycle = TabLifecycle()
        self._browser_ready = False
        self._environment: str | None = None
        self._disabled_ids: list[str] = []
        self._tab_locks: dict[str, threading.RLock] = {}
        self._locks_guard = threading.Lock()
        self._viewport: dict[str, int] | None = None
        #: Official client-side default (browser-client.mjs:6118).
        self._runtime_config: dict[str, Any] = {"display_truncate_max_chars": 100_000}
        #: Response-meta contribution (BR-21): the commands seen this turn.
        self.command_log: list[str] = []
        #: BR-21: the per-tab WebMCP serialization cache (official Kp/Eu).
        self._webmcp_cache: dict[Any, str] = {}
        #: BR-21: page events pushed programmatically, drained by the hook.
        self._page_events: list[dict[str, Any]] = []
        #: Documents read this session (BR-18).
        self._docs_read: set[str] = set()
        self.setup("codex-app")
        # DSH 插件场景扩展是标配通道：默认启动 ExtensionHub（仅监听 127.0.0.1 回环，暴露面最小）；
        # 仅在显式设置 COMPUTER_USE_EXTENSION=0/false/no 时关闭（保留端口 COMPUTER_USE_EXTENSION_PORT 语义）。
        if os.environ.get("COMPUTER_USE_EXTENSION", "").strip().lower() not in {"0", "false", "no"}:
            try:
                self.hub.start(int(os.environ.get("COMPUTER_USE_EXTENSION_PORT") or 8765))
            except OSError:
                pass

    # --- persistence (BR-17) -----------------------------------------------

    def enable_persistence(self, path: str | os.PathLike[str] | None = None) -> str:
        """BR-17 wiring hook: load/save the persisted prompt results.

        The DSH host calls this once at startup, normally with
        'browser_persistence.default_consent_path()' (~/.dsh/browser-consent.json).
        Returns the path in use ('' for the in-memory store).
        """
        from computer_use.browser_persistence import PersistedConsentStore

        target = path or self._consent_path
        self._consent_path = target
        self.security_policy.store = PersistedConsentStore.load(target) if target else None
        return str(target or "")

    def set_turn_context(self, conversation_id: str = "", turn_id: str = "") -> None:
        """Official conversation/turn ids: what the persisted decisions and the
        per-turn origin grant belong to."""
        if conversation_id:
            self.security_policy.conversation_id = str(conversation_id)
        self.security_policy.turn_id = str(turn_id or "")

    # --- setup / environment ------------------------------------------------

    def setup(self, environment: str) -> dict[str, Any]:
        if environment not in self.ALLOWED_ENVIRONMENTS:
            raise ValueError("Invalid browser service environment")
        from computer_use.browser_official import POLICY_DISABLED

        self._environment = environment
        self._browser_ready = True
        self._disabled_ids = list(POLICY_DISABLED.get(environment, ()))
        # BR-22: apiManifest is the packaged docs/api.json, not a command list.
        return {
            "apiManifest": self._api_manifest(),
            "disabledMemberIds": list(self._disabled_ids),
            "environment": environment,
            "elicitationDisplayName": "Cloud browser" if environment == "cloud" else "Browser use",
        }

    def _api_manifest(self) -> dict[str, Any]:
        import json
        from pathlib import Path

        bundled = Path.home() / ".codex" / "plugins" / "cache" / "openai-bundled"
        for root in list(bundled.glob("browser/*/docs")) + [Path(__file__).with_name("assets") / "browser_docs"]:
            candidate = root / "api.json"
            if candidate.is_file():
                try:
                    data = json.loads(candidate.read_text(encoding="utf-8"))
                except (OSError, ValueError):
                    continue
                if isinstance(data, dict) and data.get("interfaces"):
                    return data
        return {"interfaces": {}, "root": "Agent", "types": {}, "commands": list(OFFICIAL_COMMANDS)}

    # --- lifecycle ----------------------------------------------------------

    def end_turn(self) -> dict[str, Any]:
        """BR-11 tab reaper + BR-21 notification hook.

        Agent-created unmarked tabs close; claimed unmarked tabs are released
        from browser-session control but left open (tab-cleanup). The
        notification hook (official qB()) drains the page and lifecycle events
        *before* the reaper resets the lifecycle, so a tab acquired this turn
        still contributes its WebMCP notification.
        """
        notifications = self.browser_notifications()
        outcome = self.lifecycle.end_turn()
        closed: list[str] = []
        for tab_id in outcome["closed"]:
            try:
                self.browser.close_tab(tab_id)
                closed.append(tab_id)
            except Exception:
                continue
        result: dict[str, Any] = {
            "ok": True,
            "closed": closed,
            "released": outcome["released"],
        }
        if notifications:
            # Official: the content item is added only when non-empty.
            result["browserNotifications"] = notifications
        return result

    def browser_notifications(self) -> str:
        """BR-21: the official after-submitted-code notification content item.

        The empty string means "the host must not add a response content item",
        which is exactly the official qB() contract.
        """
        page_events: list[dict[str, Any]] = []
        for source in (getattr(self.browser, "take_page_events", None),
                       getattr(self.hub, "take_page_events", None)):
            if not callable(source):
                continue
            try:
                events = source()
            except Exception:
                events = None
            if isinstance(events, list):
                page_events.extend(event for event in events if isinstance(event, dict))
        page_events.extend(self._page_events)
        self._page_events.clear()
        return take_browser_notifications(
            page_events,
            self.lifecycle.take_events(),
            webmcp_enabled=self._webmcp_enabled(),
            list_tools=self._list_webmcp_tools,
            cache=self._webmcp_cache,
            current_session_id=self.security_policy.conversation_id,
        )

    def push_page_event(self, event: dict[str, Any]) -> None:
        """BR-21: accept an extension page event (webmcp_changed, ...)."""
        if isinstance(event, dict):
            self._page_events.append(event)

    @staticmethod
    def _webmcp_enabled() -> bool:
        """Official preferences.isWebMcpEnabled(): unset or true means enabled."""
        value = (os.environ.get("BROWSER_USE_WEBMCP_ENABLED", "") or "").strip().lower()
        return value not in {"0", "false", "no"}

    def _list_webmcp_tools(self, tab_id: Any) -> list[dict[str, Any]]:
        """Official Vp(): the live page-defined tool list for a tab."""
        fetch = getattr(self.browser, "webmcp_fetch", None)
        if not callable(fetch):
            return []
        try:
            result = fetch(str(tab_id))
        except Exception:
            return []
        if isinstance(result, dict):
            tools = result.get("tools")
            return [tool for tool in tools if isinstance(tool, dict)] if isinstance(tools, list) else []
        if isinstance(result, list):
            return [tool for tool in result if isinstance(tool, dict)]
        return []

    def runtime_config(self, spec: dict[str, object]) -> dict[str, Any]:
        value = spec.get("display_truncate_max_chars")
        if value is not None:
            self._runtime_config["display_truncate_max_chars"] = int(value)
        return dict(self._runtime_config)

    # --- dispatch -----------------------------------------------------------

    def dispatch(self, name: str, spec: dict[str, object]) -> Any:
        if name in {"browser_setup", "setup"}:
            return self.setup(str(spec.get("environment") or ""))
        if name in {"browser_turn_end", "end_turn", "turn_end"}:
            # The official conversation/turn ids drive the persisted decisions
            # (BR-17) and the current-session page-event filter (BR-21).
            self.set_turn_context(
                str(spec.get("session_id") or ""), str(spec.get("turn_id") or "")
            )
            return self.end_turn()
        if name in {"browser_notifications", "take_browser_notifications"}:
            return {"notifications": self.browser_notifications()}
        if name == "runtime_config":
            return self.runtime_config(spec)
        if not self._browser_ready:
            raise RuntimeError("Browser runtime has not been initialized")
        disabled = set(getattr(self, "_disabled_ids", []) or [])
        original = name
        spec = dict(spec)
        if name in OFFICIAL_ALIASES or name in OFFICIAL_COMMANDS:
            spec = rewrite_spec(name, spec)
            name = resolve_official(name, spec)
        if original in disabled or name in disabled:
            raise PermissionError(f"command {original} is disabled in environment {self._environment}")
        tab_id = str(spec.get("tab_id") or "")
        if tab_id:
            with self._tab_lock(tab_id):
                return self._dispatch_inner(original, name, spec)
        return self._dispatch_inner(original, name, spec)

    def _tab_lock(self, tab_id: str) -> threading.RLock:
        with self._locks_guard:
            lock = self._tab_locks.get(tab_id)
            if lock is None:
                lock = threading.RLock()
                self._tab_locks[tab_id] = lock
            return lock

    def _dispatch_inner(self, original: str, name: str, spec: dict[str, object]) -> Any:
        tab_id = str(spec.get("tab_id") or "")
        if original in DOCUMENT_READ_COMMANDS:
            self._record_document_read(spec)
        self._enforce_required_documents(original)
        self._guard_command(name, spec)
        self._maybe_safety_precheck(original)
        self._ensure_command_allowed(name, spec)
        if tab_id and self.hub.connected:
            self._maybe_reclaim(tab_id)
        # Official playwright_locator_* schemas carry `selector`, while the DSH
        # implementations take a locator handle: bridge them here.
        if (
            name.startswith("tab_pw_")
            and not spec.get("locator_id")
            and spec.get("selector") is not None
            and name != "tab_pw_locator"
            and not name.startswith("tab_pw_get_by")
        ):
            loc = self.pw.put(tab_id, "css", str(spec.get("selector") or ""))
            spec = {**spec, "locator_id": loc.id}
        if name == "tab_ax_action":
            return self._ax_action(tab_id, spec)
        if name in EXTRA_TOOLS:
            extra = handle_extra(self, name, spec)
            if extra is not None:
                if name in {"tab_mark_handoff", "tab_mark_deliverable", "tab_request_manual_handoff"}:
                    self.lifecycle.record_marked(tab_id)
                    self._record_command(original)
                return extra
        if name.startswith("tab_pw_") or name.startswith("tab_dom_"):
            handled = self._pw_or_dom(name, spec, tab_id)
            if handled is not None:
                self._record_command(original)
                return handled
        if name == "browser_list":
            from computer_use.cdp_http import list_tabs

            listed = list(self.browser.list_browsers())
            live = list_tabs()
            if live:
                listed.append({"id": "cdp-live", "name": "Chrome DevTools", "type": "cdp", "tabs": live})
            return listed
        if name == "tab_new":
            url = str(spec.get("url") or "https://example.com/")
            kind = str(spec.get("browser") or "iab")
            try:
                tab = self.browser.new_tab(url, browser=kind)
            except TypeError:
                tab = self.browser.new_tab(url)
            self.lifecycle.record_created(tab.id)
            self._record_command(original)
            return {
                "id": tab.id,
                "url": tab.url,
                "title": tab.title,
                "backend": getattr(tab, "backend", kind),
                "visible": getattr(tab, "visible", False),
                "providerTabId": getattr(tab, "provider_tab_id", tab.id),
            }
        if name in NAVIGATION_COMMANDS:
            return self._navigate(name, spec)
        if name == "tab_get":
            self.lifecycle.record_acquired(tab_id)
            tab = self.browser.tab(tab_id)
            self._record_command(original)
            return {"id": tab.id, "title": tab.title, "url": tab.url, "backend": getattr(tab, "backend", "")}
        if name == "browser_user_claim_tab":
            result = self._claim_user_tab(tab_id, str(spec.get("browserId") or ""))
            self._record_command(original)
            return result
        if name == "browser_viewport_set":
            return self._set_viewport(spec)
        if name == "browser_viewport_reset":
            return self._set_viewport({}, reset=True)
        if name == "browser_management_call":
            return self._management_call(spec)
        if name == "browser_management_get_audit_trail":
            return {"entries": list(getattr(self.browser, "management_audit", []))}
        if name == "tab_browser_auth_handoff":
            return self._browser_auth_handoff(spec)
        if name == "tab_bot_detection_report":
            return self._bot_detection_report(spec)
        if name == "get_browser_documentation":
            return self._browser_documentation(spec)
        if name == "tab_ax_write":
            return self._ax_get_state(tab_id, spec)
        if name == "tab_ax_click":
            return self.browser.ax_click(tab_id, int(spec["element_index"]))
        if name == "tab_ax_set_value":
            return self.browser.ax_set_value(tab_id, int(spec["element_index"]), str(spec.get("value") or ""))
        if name == "tab_ax_type_text":
            return self.browser.ax_set_value(tab_id, self.browser.tab(tab_id).focused, str(spec.get("text") or ""))
        if name == "tab_ax_press_key":
            return {"ok": True, "action": "ax.pressKey", "key": spec.get("key")}
        if name == "tab_ax_scroll":
            return {"ok": True, "action": "ax.scroll", "element_index": spec.get("element_index"), "direction": spec.get("direction")}
        if name == "tab_screenshot":
            return self._ax_get_state(tab_id, {"mode": "screenshot", "disableDiffing": True})
        raise KeyError(name)

    def _record_command(self, name: str) -> None:
        self.command_log.append(name)

    def response_meta(self) -> dict[str, object] | None:
        backend = "cdp" if str(getattr(self.browser, "backend", "")) == "cdp" else ""
        return response_meta_contribution(self.command_log, backend=backend)

    # --- security -----------------------------------------------------------

    def _browser_family(self) -> str:
        family = str(getattr(self.hub, "family", "") or "").strip().lower()
        if family:
            return family
        return str(os.environ.get("COMPUTER_USE_BROWSER_FAMILY") or "").strip().lower()

    def _tab_origin(self, spec: dict[str, object]) -> str:
        tab_id = str(spec.get("tab_id") or "")
        if not tab_id:
            return ""
        try:
            tab = self.browser.tab(tab_id)
        except Exception:
            return ""
        return origin_of(str(getattr(tab, "url", "")))

    @staticmethod
    def _gate_key(url: str, spec: dict[str, object]) -> str:
        """A stable, non-empty grant key even when the URL is unknown."""
        return url or str(spec.get("tab_id") or "") or "browser-action"

    def _guard_navigation(self, url: str) -> None:
        from computer_use import browser_errors
        from computer_use.policy import deny_url

        if not url:
            raise ValueError(browser_errors.URL_REQUIRED)
        # Official runNavigation: host URL gate first, then the URL policy, then
        # the site-status check, then origin consent.
        assert_browser_url_allowed(
            url,
            family=self._browser_family(),
            security_mode=self.security_policy.mode,
        )
        deny_url(url)
        policy = self.security_policy
        policy.assert_url_policy(url)
        policy.assert_site_status_allowed(url)
        origin = origin_of(url)
        if origin:
            policy.gate("browser-origin-access", origin, approver=self.approvals, origin=origin)
            policy.assert_origin_allowed(origin)

    def _navigate(self, name: str, spec: dict[str, object]) -> dict[str, Any]:
        from computer_use import browser_errors

        url = str(spec.get("url") or "")
        tab_id = str(spec.get("tab_id") or "")
        if not tab_id:
            raise ValueError(browser_errors.URL_TAB_ID_REQUIRED)
        self._guard_navigation(url)
        tab = self.browser.goto(tab_id, url)
        if self.lifecycle.needs_reclaim(tab_id):
            self.lifecycle.record_acquired(tab_id)
        self._record_command(name)
        return {"id": tab.id, "url": tab.url, "title": tab.title}

    def _guard_command(self, name: str, spec: dict[str, object]) -> None:
        if name in NAVIGATION_COMMANDS:
            return
        policy = self.security_policy
        approver = self.approvals
        if name in CDP_COMMANDS:
            origin = self._tab_origin(spec)
            policy.gate("full-cdp", origin, approver=approver, origin=origin)
            policy.assert_full_cdp_allowed(origin)
        if name in UPLOAD_COMMANDS:
            url = str(spec.get("url") or "") or self._tab_origin(spec)
            key = self._gate_key(url, spec)
            policy.gate("file-upload", key, approver=approver, url=url)
            policy.assert_upload_allowed(url, key=key)
        if name in DOWNLOAD_COMMANDS:
            url = str(spec.get("url") or "") or self._tab_origin(spec)
            key = self._gate_key(url, spec)
            policy.gate("file-download", key, approver=approver, url=url)
            policy.assert_download_allowed(url, key=key)
        if name in PAGE_ASSET_COMMANDS:
            url = self._tab_origin(spec)
            host = url.split("://", 1)[-1] if "://" in url else url
            policy.gate("page-asset-cross-origin-fetch", host, approver=approver, host=host, origin=host)
            policy.assert_page_asset_download_allowed(url, cross_origin=True)
        if name in HISTORY_COMMANDS:
            policy.gate("browser-history-read", "browsing_history", approver=approver)
        if name not in NON_PAGE_COMMANDS and spec.get("tab_id"):
            origin = self._tab_origin(spec)
            if origin:
                policy.gate("browser-origin-access", origin, approver=approver, origin=origin)
                policy.assert_origin_allowed(origin)

    @staticmethod
    def _normalize_document_name(raw: object) -> str:
        name = str(raw or "").replace("\\", "/").strip()
        if name.endswith(".md"):
            name = name[:-3]
        return name

    def _record_document_read(self, spec: dict[str, object]) -> None:
        raw = spec.get("name") or spec.get("browser_id") or spec.get("id")
        name = self._normalize_document_name(raw)
        if name:
            self._docs_read.add(name)
            # The official marks guidance documents read as a side effect of
            # rendering documentation(); mirror the two that gate commands.
            for doc in ("confirmations", "capabilities/tab/cdp", "webmcp", "capabilities/tab/browserAuth"):
                if name.endswith(doc) or doc.endswith(name):
                    self._docs_read.add(doc)

    def _enforce_required_documents(self, command: str) -> None:
        required = DOCUMENT_REQUIRED.get(command)
        if not required:
            return
        missing = [name for name in required if name not in self._docs_read]
        if missing:
            from computer_use import browser_errors

            raise RuntimeError(browser_errors.required_documentation(missing))

    def _maybe_safety_precheck(self, tool_name: str) -> None:
        policy = self.security_policy
        if not policy.precheck_required:
            return
        policy.gate(
            "automated-safety-precheck", tool_name, approver=self.approvals, tool=tool_name
        )

    def _ensure_command_allowed(self, name: str, spec: dict[str, object]) -> None:
        """Official ensureCommandAllowed: user-tab commands carry the expected URL
        so a tab that moved underneath the agent fails closed (BR-16)."""
        if name not in {"tab_get", "browser_user_get_tab_context"}:
            return
        from computer_use import browser_errors

        tab_id = str(spec.get("tab_id") or "")
        current = ""
        try:
            current = str(getattr(self.browser.tab(tab_id), "url", ""))
        except Exception:
            current = ""
        provided = str(spec.get("expected_url") or "")
        if provided and current and provided != current:
            raise RuntimeError(browser_errors.MISSING_USER_TAB_URL)
        if current and "expected_url" not in spec:
            spec["expected_url"] = current

    def _maybe_reclaim(self, tab_id: str) -> None:
        """Official extension auto-reclaim: re-claim before a non-claim command."""
        if not tab_id or not self.lifecycle.needs_reclaim(tab_id):
            return
        result = self._claim_user_tab(tab_id, "", record=False)
        if isinstance(result, dict) and result.get("claimed"):
            self.lifecycle.record_acquired(tab_id)

    def _claim_user_tab(self, tab_id: str, browser_id: str = "", *, record: bool = True) -> dict[str, Any]:
        hub = getattr(self, "hub", None)
        if hub is not None and hub.connected and hub.tabs:
            match = self._find_open_tab(hub.tabs, tab_id, browser_id)
            if match is None:
                return {"claimed": False, "unavailable": True, "reason": "tab mention no longer matches"}
            result = hub.claim(
                str(match.get("providerTabId") or match.get("id") or tab_id),
                str(match.get("title") or ""),
                str(match.get("url") or ""),
                instance_id=browser_id,
            )
            if record and isinstance(result, dict) and result.get("claimed"):
                self.lifecycle.record_acquired(str(result.get("id") or tab_id))
            return result
        claim = getattr(self.browser, "claim_tab", None)
        if not callable(claim):
            return {"claimed": False, "unavailable": True}
        snapshot = self._find_open_tab(self.browser.tabs_list(), tab_id, browser_id)
        if snapshot is None:
            return {"claimed": False, "unavailable": True, "reason": "tab mention no longer matches"}
        result = claim(
            str(snapshot.get("providerTabId") or snapshot.get("id") or tab_id),
            str(snapshot.get("title") or ""),
            str(snapshot.get("url") or ""),
        )
        if record and isinstance(result, dict) and result.get("claimed"):
            self.lifecycle.record_acquired(str(result.get("id") or tab_id))
        return result

    @staticmethod
    def _find_open_tab(tabs: list[dict[str, Any]], tab_id: str, browser_id: str = "") -> dict[str, Any] | None:
        wanted = str(tab_id)
        for tab in tabs:
            if browser_id and str(tab.get("extensionInstanceId") or "") != str(browser_id):
                continue
            keys = {str(tab.get("id") or ""), str(tab.get("providerTabId") or "")}
            if wanted in keys:
                return tab
        return None

    # --- accessibility ------------------------------------------------------

    def _ax_get_state(self, tab_id: str, spec: dict[str, object]) -> Any:
        mode = str(spec.get("mode") or "state")
        disable = spec.get("disableDiffing") is True
        payload = self.browser.ax_write(tab_id, mode, disable)
        unavailable = payload.get("screenshot_unavailable") if isinstance(payload, dict) else None
        if mode == "screenshot" and unavailable:
            # Official: `content==="screenshot"` throws the unavailable string.
            raise RuntimeError(str(unavailable))
        return payload

    def _ax_action(self, tab_id: str, spec: dict[str, object]) -> Any:
        payload = ax_action_payload(spec)
        kind = ax_action_kind(spec)
        if kind == "click":
            target = payload.get("target")
            if isinstance(target, list) and len(target) >= 2:
                click = getattr(self.browser, "cua_click", None)
                if callable(click):
                    return click(tab_id, float(target[0]), float(target[1]), int(payload.get("click_count") or 1))
            index = int(target if target is not None else spec.get("element_index"))
            return self.browser.ax_click(tab_id, index)
        if kind == "drag":
            from_pt = payload.get("from") if isinstance(payload.get("from"), list) else [0, 0]
            to_pt = payload.get("to") if isinstance(payload.get("to"), list) else [0, 0]
            fx, fy = float(from_pt[0]), float(from_pt[1])
            tx, ty = float(to_pt[0]), float(to_pt[1])
            dragger = getattr(self.browser, "ax_drag_points", None)
            if callable(dragger):
                return dragger(tab_id, fx, fy, tx, ty)
            return self.browser.cua_drag(tab_id, [{"x": fx, "y": fy}, {"x": tx, "y": ty}])
        if kind == "perform_secondary_action":
            fn = getattr(self.browser, "ax_perform_secondary", None)
            if callable(fn):
                return fn(tab_id, int(payload["element_index"]), str(payload.get("action") or ""))
            return {"ok": True, "action": payload.get("action"), "element_index": payload.get("element_index")}
        if kind == "press_key":
            key = str(payload.get("key") or "")
            presser = getattr(self.browser, "dom_keypress", None)
            if callable(presser):
                return presser(tab_id, [key])
            return {"ok": True, "action": "ax.pressKey", "key": key}
        if kind == "scroll":
            direction = str(payload.get("direction") or "down").lower()
            direction = AX_SCROLL_DIRECTIONS.get(direction, direction)
            return {
                "ok": True,
                "action": "ax.scroll",
                "element_index": payload.get("target"),
                "direction": direction,
                "pages": payload.get("pages"),
            }
        if kind == "select_text":
            selector = getattr(self.browser, "ax_select_text", None)
            if callable(selector):
                return selector(tab_id, int(payload["element_index"]), str(payload.get("text") or ""))
            return {"ok": True, "action": "ax.selectText", "element_index": payload.get("element_index"), "text": payload.get("text")}
        if kind == "set_value":
            return self.browser.ax_set_value(tab_id, int(payload["element_index"]), str(payload.get("value") or ""))
        if kind == "type_text":
            text = str(payload.get("text") or "")
            focused = int(getattr(self.browser.tab(tab_id), "focused", 0))
            return self.browser.ax_set_value(tab_id, focused, text)
        if spec.get("element_index") is not None:
            return self.browser.ax_click(tab_id, int(spec["element_index"]))
        raise KeyError(f"tab_ax_action: unsupported action {kind!r}")

    # --- new official commands ---------------------------------------------

    def _set_viewport(self, spec: dict[str, object], *, reset: bool = False) -> dict[str, Any]:
        self._viewport = None if reset else {
            "width": int(spec.get("width") or 0),
            "height": int(spec.get("height") or 0),
        }
        setter = getattr(self.browser, "set_viewport", None)
        if callable(setter):
            return setter(self._viewport)
        return {"ok": True, "viewport": self._viewport, "backend": "iab"}

    def _management_call(self, spec: dict[str, object]) -> dict[str, Any]:
        area = str(spec.get("area") or "")
        method = str(spec.get("method") or "")
        args = spec.get("args") if isinstance(spec.get("args"), dict) else {}
        handler = getattr(self.browser, "management_call", None)
        if callable(handler):
            return handler(area, method, args)
        return {"ok": True, "area": area, "method": method, "result": None, "backend": "iab"}

    def _bot_detection_report(self, spec: dict[str, object]) -> dict[str, Any]:
        """Official tab.botDetection.report(reason) -- a bounded blocker report."""
        reason = str(spec.get("reason") or "")
        reporter = getattr(self.browser, "bot_detection_report", None)
        if callable(reporter):
            return reporter(str(spec.get("tab_id") or ""), reason)
        return {"ok": True, "reported": True, "reason": reason}

    def _browser_documentation(self, spec: dict[str, object]) -> dict[str, Any]:
        from computer_use.browser_missing import handle_missing

        browser_id = str(spec.get("browser_id") or spec.get("id") or "browser")
        return handle_missing(self, "documentation_get", {"name": browser_id})

    def _browser_auth_handoff(self, spec: dict[str, object]) -> dict[str, Any]:
        from computer_use import browser_errors

        url = str(spec.get("url") or "")
        if not url:
            raise ValueError("browserAuth.request requires a url")
        if os.environ.get("BROWSER_USE_AUTH_CLIENT_UNSUPPORTED", "").strip().lower() == "true":
            raise RuntimeError(browser_errors.CLOUD_TAKEOVER_UNSUPPORTED)
        return {
            "ok": True,
            "kind": "browserAuth",
            "status": "handed-off",
            "url": url,
            "reason": spec.get("reason"),
        }

    # --- playwright / dom ---------------------------------------------------

    def _pw_or_dom(self, name: str, spec: dict[str, object], tab_id: str) -> Any:
        if name in {"tab_dom_snapshot", "tab_dom_get_visible_dom"}:
            return self.browser.dom_snapshot(tab_id)
        if name == "tab_dom_click":
            return self.browser.dom_click(tab_id, int(spec["node_id"]))
        if name == "tab_dom_double_click":
            double = getattr(self.browser, "dom_double_click", None)
            if double:
                return double(tab_id, int(spec["node_id"]))
            return self.browser.dom_click(tab_id, int(spec["node_id"]))
        if name == "tab_dom_type":
            return self.browser.dom_type(tab_id, str(spec.get("text") or ""))
        if name == "tab_dom_keypress":
            return self.browser.dom_keypress(tab_id, spec.get("keys") or [])
        if name == "tab_dom_scroll":
            return self.browser.dom_scroll(tab_id, float(spec.get("scroll_x") or 0), float(spec.get("scroll_y") or 0), spec.get("node_id") if spec.get("node_id") is None else int(spec["node_id"]))
        if name.startswith("tab_pw_"):
            return self._playwright(name, spec, tab_id)
        return None

    def _playwright(self, name: str, spec: dict[str, object], tab_id: str) -> Any:
        if name == "tab_pw_locator":
            loc = self.pw.put(tab_id, "css", str(spec.get("selector") or ""))
            return {"locator_id": loc.id, "kind": loc.kind, "query": loc.query}
        if name == "tab_pw_get_by_role":
            parent = str(spec.get("locator_id") or "")
            if parent:
                loc = self.pw.extend(self.pw.get(parent), "role", str(spec.get("role") or ""), name=str(spec.get("name") or "") or None)
            else:
                loc = self.pw.put(tab_id, "role", str(spec.get("role") or ""), name=str(spec.get("name") or "") or None)
            return {"locator_id": loc.id, "kind": loc.kind, "query": loc.query, "name": loc.name, "parent": parent or None, "chain": loc.chain}
        if name == "tab_pw_get_by_text":
            parent = str(spec.get("locator_id") or "")
            if parent:
                loc = self.pw.extend(self.pw.get(parent), "text", str(spec.get("text") or ""), exact=spec.get("exact") is True)
            else:
                loc = self.pw.put(tab_id, "text", str(spec.get("text") or ""), exact=spec.get("exact") is True)
            return {"locator_id": loc.id, "kind": loc.kind, "query": loc.query, "parent": parent or None, "chain": loc.chain}
        if name == "tab_pw_get_by_label":
            loc = self.pw.put(tab_id, "label", str(spec.get("text") or ""))
            return {"locator_id": loc.id, "kind": loc.kind, "query": loc.query}
        if name == "tab_pw_evaluate":
            expr = str(spec.get("expression") or spec.get("script") or "")
            evaluate = getattr(self.browser, "_eval", None)
            if callable(evaluate):
                return {"value": evaluate(tab_id, expr), "backend": "playwright"}
            return {"value": None, "backend": "fake"}
        if name == "tab_pw_wait_for_load_state":
            if self.pw.page is not None:
                self.pw.page.wait_for_load_state(str(spec.get("state") or "load"))
            return {"ok": True, "state": spec.get("state") or "load"}
        if name == "tab_pw_dom_snapshot":
            snap = self.browser.dom_snapshot(tab_id)
            return {"html": str(snap.get("nodes")), "nodes": snap.get("nodes")}
        locator_id = str(spec.get("locator_id") or "")
        loc = self.pw.get(locator_id)
        handle = self.pw.resolve(loc)
        if handle is not None:
            return self._pw_live(name, handle, spec, loc)
        hits = []
        matcher = getattr(self.browser, "pw_match", None)
        if callable(matcher):
            hits = matcher(loc.tab_id, loc.kind, loc.query, loc.name)
        if name == "tab_pw_count":
            return {"count": len(hits)}
        if name == "tab_pw_inner_text":
            return {"text": (hits[0].get("name") if hits else "")}
        if name == "tab_pw_click" and hits:
            return self.browser.dom_click(loc.tab_id, int(hits[0]["node_id"]))
        if name == "tab_pw_fill" and hits:
            self.browser.dom_click(loc.tab_id, int(hits[0]["node_id"]))
            return self.browser.dom_type(loc.tab_id, str(spec.get("value") or ""))
        return {"ok": True, "locator_id": loc.id, "matches": len(hits)}

    def _pw_live(self, name: str, handle: Any, spec: dict[str, object], loc: Any) -> Any:
        if name == "tab_pw_click":
            handle.first.click()
            return {"ok": True, "action": "locator.click", "locator_id": loc.id, "backend": "playwright"}
        if name == "tab_pw_fill":
            handle.first.fill(str(spec.get("value") or ""))
            return {"ok": True, "action": "locator.fill", "locator_id": loc.id, "backend": "playwright"}
        if name == "tab_pw_count":
            return {"count": handle.count(), "backend": "playwright"}
        if name == "tab_pw_inner_text":
            return {"text": handle.first.inner_text(timeout=8000), "backend": "playwright"}
        return {"ok": True, "locator_id": loc.id, "backend": "playwright"}
