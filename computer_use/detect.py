"""Backend detection matching official IAB / extension / CDP / cloud conditions."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from computer_use.cdp_http import list_tabs

OFFICIAL_EXTENSION_IDS = (
    "hehggadaopoacecdllhhajmbjkdcmajg",
    "odlomjlbamekndcpllcnffbgeohgkmjh",
)
NATIVE_HOST = "com.openai.codexextension"


def _chrome_extensions_dir() -> Path:
    local = Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local")
    return local / "Google" / "Chrome" / "User Data" / "Default" / "Extensions"


def official_extension_present() -> bool:
    root = _chrome_extensions_dir()
    return any((root / ext_id).is_dir() for ext_id in OFFICIAL_EXTENSION_IDS)


def native_host_registered() -> bool:
    if os.name != "nt":
        return False
    try:
        import winreg

        key = winreg.OpenKey(
            winreg.HKEY_CURRENT_USER,
            r"Software\Google\Chrome\NativeMessagingHosts\\" + NATIVE_HOST,
        )
        winreg.CloseKey(key)
        return True
    except OSError:
        return False


def lockapp_running() -> bool:
    """True only on the Windows lock/secure desktop, not merely because LockApp.exe exists."""
    if os.name != "nt" or os.environ.get("PYTEST_CURRENT_TEST"):
        return False
    try:
        import ctypes

        user32 = ctypes.windll.user32
        handle = user32.OpenInputDesktop(0, False, 0x0001)
        if not handle:
            return True
        name = ctypes.create_unicode_buffer(256)
        ok = user32.GetUserObjectInformationW(handle, 2, name, 512, None)
        user32.CloseDesktop(handle)
        if not ok:
            return False
        # Blacklist, not whitelist (audit P2-3, 2026-10-25): pipelines and
        # third-party tools legitimately create their own input desktops;
        # treating every non-Default name as "locked" refused all input there.
        lower = name.value.lower()
        return lower in {"winlogon", "screensavers", "screen-saver", "screensaver", "disconnect", "lock", "consent"}
    except (OSError, AttributeError):
        return False


def detect_backends(hub: Any = None, iab_session: str = "sess-iab") -> dict[str, Any]:
    extension_connected = bool(hub and getattr(hub, "connected", False))
    cdp_tabs = list_tabs()
    return {
        "iab": {
            "available": True,
            "type": "iab",
            "reason": "in-process Codex-style IAB session",
            "metadata": {"codexSessionId": iab_session},
            "visibilityDefault": False,
        },
        "extension": {
            "available": extension_connected or official_extension_present(),
            "type": "extension",
            "connected": extension_connected,
            "officialExtensionInstalled": official_extension_present(),
            "nativeHostRegistered": native_host_registered(),
            "instanceId": getattr(hub, "instance_id", "") if hub else "",
            "reason": "chrome.debugger claim of existing profile tabs",
        },
        "cdp": {
            "available": bool(cdp_tabs),
            "type": "cdp",
            "tabs": len(cdp_tabs),
            "reason": "DevTools WebSocket",
        },
        "cloud": {
            "available": True,
            "type": "cloud",
            "reason": "handoff/deliverable protocol (markHandoff / requestManualHandoff)",
        },
        "lockapp": lockapp_running(),
    }
