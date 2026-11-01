"""Verbatim official browser failure strings (BR-15).

Every constant is copied byte-for-byte from `browser-service.mjs` (offsets noted
in `analysis/deep-dive/05-browser-surface.md` BR-15). The model uses these exact
sentences to pick a recovery path -- for example "is not available in this
context" must be distinguishable from a dead session -- so a rewording is a
behaviour change, not a cosmetic one.
"""

from __future__ import annotations

import json

from computer_use.browser_security import DEFAULT_DISPLAY_NAME

#: `VU` availability map (~offset 578700).
EXTENSION_UNAVAILABLE = "The browser extension Browser Use plugin is not available in this context."
IAB_UNAVAILABLE = "The Browser Use plugin is not available in this context."
CDP_UNAVAILABLE = "The CDP Browser Use backend is not available in this context."

#: Navigation history, `navigate_tab_back` / `navigate_tab_forward`.
NO_HISTORY_BACK = "Cannot navigate back: no previous page in history."
NO_HISTORY_FORWARD = "Cannot navigate forward: no next page in history."

#: `runNavigation` / `close_tab` argument validation.
URL_REQUIRED = "navigate_tab_url requires a url"
URL_TAB_ID_REQUIRED = "navigate_tab_url requires a positive integer tab_id"
CLOSE_TAB_ID_REQUIRED = "close_tab requires a positive integer tab_id"
BACK_TAB_ID_REQUIRED = "navigate_tab_back requires a positive integer tab_id"
EXPECTED_POSITIVE_INTEGER = "Expected a positive integer"
REQUIRES_NODE_ID = "{method} requires node_id"
MISSING_USER_TAB_URL = "Missing authorized user tab URL"

#: Playwright / DOM actionability.
ELEMENT_NOT_CONNECTED = "Element is not connected"
FRAME_NO_BOUNDING_BOX = "Frame does not have an actionable bounding box"

#: WebMCP and browserAuth.
WEBMCP_REGISTRATION_STALE = "WebMCP tool registration is stale. Call fetchTools() again."
CLOUD_TAKEOVER_UNSUPPORTED = (
    "This ChatGPT client does not support cloud browser takeover. Do not claim the handoff "
    "succeeded. Continue without handing off if possible. If the user explicitly requested "
    "takeover or the task cannot continue without it, briefly tell them to update the "
    "ChatGPT app."
)

#: Extension diagnostics (`docs/chrome-troubleshooting.md` section 4).
EXTENSION_COMMUNICATION_FAILED = (
    "Cannot communicate with the ChatGPT browser extension. Confirm that the extension is "
    "installed and enabled in the selected browser."
)

#: Templates with the display name interpolated.
EXTENSION_UI_BLOCKING = (
    "{display_name} is blocking automation because another extension UI is open on this page. "
    "Complete or dismiss that extension UI in {display_name}, then ask me to continue."
)
EXTENSION_UPDATE_REQUIRED = (
    "Please update the ChatGPT extension in {display_name} to the latest version to continue."
)
CANNOT_ACCESS_EXTENSION_URL = "Cannot access a chrome-extension:// URL of different extension"


def extension_ui_blocking(display_name: str = DEFAULT_DISPLAY_NAME) -> str:
    return EXTENSION_UI_BLOCKING.format(display_name=display_name)


def extension_update_required(display_name: str = DEFAULT_DISPLAY_NAME) -> str:
    return EXTENSION_UPDATE_REQUIRED.format(display_name=display_name)


def requires_node_id(method: str) -> str:
    return REQUIRES_NODE_ID.format(method=method)


def required_documentation(names: list[str]) -> str:
    """Official `assertRequiredDocumentationRead` error (browser-service.mjs:37344)."""
    quoted = ", ".join(json.dumps(name) for name in names)
    hints = "; ".join(
        "get_documentation(name=" + json.dumps(name) + ")" for name in names
    )
    return f"Required documentation has not been read: {quoted}. Read the instructions with {hints}."


__all__ = [
    "BACK_TAB_ID_REQUIRED",
    "CANNOT_ACCESS_EXTENSION_URL",
    "CDP_UNAVAILABLE",
    "CLOSE_TAB_ID_REQUIRED",
    "CLOUD_TAKEOVER_UNSUPPORTED",
    "ELEMENT_NOT_CONNECTED",
    "EXPECTED_POSITIVE_INTEGER",
    "EXTENSION_COMMUNICATION_FAILED",
    "EXTENSION_UNAVAILABLE",
    "EXTENSION_UPDATE_REQUIRED",
    "EXTENSION_UI_BLOCKING",
    "FRAME_NO_BOUNDING_BOX",
    "IAB_UNAVAILABLE",
    "MISSING_USER_TAB_URL",
    "NO_HISTORY_BACK",
    "NO_HISTORY_FORWARD",
    "REQUIRES_NODE_ID",
    "URL_REQUIRED",
    "URL_TAB_ID_REQUIRED",
    "WEBMCP_REGISTRATION_STALE",
    "extension_ui_blocking",
    "extension_update_required",
    "required_documentation",
    "requires_node_id",
]
