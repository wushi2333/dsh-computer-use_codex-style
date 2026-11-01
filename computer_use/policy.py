from __future__ import annotations

import os

from computer_use.models import WindowRef

# Official helper rdata terminal table (exes, no extension in the blob).
TERMINAL_STEMS = frozenset(
    {
        "alacritty",
        "bash",
        "cmd",
        "cmder",
        "conemu",
        "conemu64",
        "conemu64c",
        "conemuc",
        "conhost",
        "fluentterminal",
        "git-bash",
        "hyper",
        "kitty",
        "mintty",
        "mobaxterm",
        "openconsole",
        "powershell",
        "powershell_ise",
        "putty",
        "puttytel",
        "pwsh",
        "tabby",
        "terminal",
        "terminus",
        "termius",
        "ttermpro",
        "warp",
        "wezterm",
        "wezterm-gui",
        "windowsterminal",
        "wsl",
        "wt",
    }
)
TERMINAL_APPS = TERMINAL_STEMS | {f"{name}.exe" for name in TERMINAL_STEMS}
CODEX_APPS = {
    "chatgpt.exe",
    "chatgpt",
    "codex.exe",
    "codex",
    "codex-computer-use.exe",
    "codex-computer-use",
}
CODEX_AUMID_NEEDLES = ("chatgpt", "codex", "openai")
WIN_KEY_TOKENS = {
    "meta",
    "windows",
    "win",
    "cmd",
    "command",
    "super",
    "os",
    "lwin",
    "rwin",
}


def deny_press_key(key: str) -> None:
    parts = [part.strip().lower() for part in key.replace("-", "+").split("+") if part.strip()]
    for part in parts:
        token = part.split("_", 1)[0]
        if part in WIN_KEY_TOKENS or token in WIN_KEY_TOKENS or part.startswith("win+"):
            raise PermissionError(
                "Do not use the Windows key or shortcuts involving the Windows key."
            )


BLOCKED_URL_SCHEMES = ("javascript:", "vbscript:", "ms-appx:")


def deny_url(url: str) -> None:
    """Official browser navigation gate.

    Blocked schemes and a locked desktop are hard, non-retryable security
    decisions, so they surface as `BrowserUseSecurityError` with the official
    reason taxonomy and the verbatim non-retryable wrapper sentence (the agent
    must not route around them).
    """
    from computer_use.browser_security import BrowserUseSecurityError

    lowered = url.strip().lower()
    for scheme in BLOCKED_URL_SCHEMES:
        if lowered.startswith(scheme):
            raise BrowserUseSecurityError(
                "navigation_url_policy_blocked",
                f"the {scheme} URL scheme is blocked by the Browser Use navigation policy.",
            )
    from computer_use.detect import lockapp_running

    if lockapp_running():
        raise BrowserUseSecurityError(
            "browser_navigation_blocked",
            "the Windows desktop is locked (LockApp.exe is active), so browser navigation "
            "cannot be performed.",
        )


def normalize_app_id(app: str) -> str:
    cleaned = app.strip().lower().replace("\\", "/")
    if "/" in cleaned:
        cleaned = cleaned.rsplit("/", 1)[-1]
    if cleaned.endswith(".exe"):
        return cleaned
    return cleaned


def _stem(app: str) -> str:
    key = normalize_app_id(app)
    return key[:-4] if key.endswith(".exe") else key


def _split_env(name: str) -> tuple[str, ...]:
    raw = os.environ.get(name) or ""
    return tuple(item.strip().lower() for item in raw.split(",") if item.strip())


def default_app_access() -> dict[str, dict[str, tuple[str, ...]]]:
    """Official default_app_access: allow/deny × aumids/exes."""
    allow_exes = _split_env("COMPUTER_USE_ALLOW_EXES")
    allow_aumids = _split_env("COMPUTER_USE_ALLOW_AUMIDS")
    deny_exes = _split_env("COMPUTER_USE_DENY_EXES")
    deny_aumids = _split_env("COMPUTER_USE_DENY_AUMIDS")
    return {
        "allow": {"exes": allow_exes, "aumids": allow_aumids},
        "deny": {"exes": deny_exes, "aumids": deny_aumids},
    }


def process_aumid(pid: int) -> str:
    if not pid:
        return ""
    try:
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        handle = kernel32.OpenProcess(0x1000, False, int(pid))
        if not handle:
            return ""
        try:
            getter = getattr(kernel32, "GetApplicationUserModelId", None)
            if getter is None:
                return ""
            length = wintypes.UINT(256)
            buf = ctypes.create_unicode_buffer(256)
            getter.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.UINT), wintypes.LPWSTR]
            getter.restype = ctypes.c_long
            hr = getter(handle, ctypes.byref(length), buf)
            if hr == 0:
                return buf.value
            return ""
        finally:
            kernel32.CloseHandle(handle)
    except Exception:
        return ""


def process_name_for_pid(pid: int) -> str:
    if not pid:
        return ""
    try:
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        handle = kernel32.OpenProcess(0x1000, False, int(pid))
        if not handle:
            return ""
        try:
            size = wintypes.DWORD(260)
            buf = ctypes.create_unicode_buffer(260)
            getter = getattr(kernel32, "QueryFullProcessImageNameW", None)
            if getter is None:
                return ""
            getter.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
            getter.restype = wintypes.BOOL
            if getter(handle, 0, buf, ctypes.byref(size)):
                path = buf.value.replace("\\", "/")
                return path.rsplit("/", 1)[-1]
            return ""
        finally:
            kernel32.CloseHandle(handle)
    except Exception:
        return ""


def hwnd_pid(hwnd: int) -> int:
    if not hwnd:
        return 0
    try:
        import ctypes
        from ctypes import wintypes

        user32 = ctypes.WinDLL("user32", use_last_error=True)
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        return int(pid.value)
    except Exception:
        return 0


def deny_app_access(app: str, aumid: str | None = None) -> None:
    key = normalize_app_id(app)
    stem = _stem(app)
    access = default_app_access()
    if stem in TERMINAL_STEMS or key in TERMINAL_APPS:
        raise PermissionError("Do not automate terminal applications.")
    if stem in CODEX_APPS or key in CODEX_APPS:
        raise PermissionError("Do not automate the ChatGPT desktop app UI or Codex CLI.")
    needle = (aumid or "").lower()
    if needle and any(token in needle for token in CODEX_AUMID_NEEDLES):
        raise PermissionError("Do not automate the ChatGPT desktop app UI or Codex CLI.")
    for denied in access["deny"]["exes"]:
        if denied and (denied == key or denied == stem or key.endswith(denied)):
            raise PermissionError(f"Computer Use default_app_access denied exe {app}.")
    for denied in access["deny"]["aumids"]:
        if denied and needle and denied in needle:
            raise PermissionError(f"Computer Use default_app_access denied aumid {aumid}.")
    allow_exes = access["allow"]["exes"]
    allow_aumids = access["allow"]["aumids"]
    if allow_exes or allow_aumids:
        exe_ok = any(item == key or item == stem or key.endswith(item) for item in allow_exes) if allow_exes else False
        aumid_ok = any(item in needle for item in allow_aumids) if (allow_aumids and needle) else False
        if not exe_ok and not aumid_ok:
            raise PermissionError(f"Computer Use default_app_access blocked {app}.")


def deny_allowed_apps(app: str, allowed: list[str] | None) -> None:
    if not allowed:
        return
    key = normalize_app_id(app)
    if not key:
        raise PermissionError("Computer Use allowedApps blocked an empty app id.")
    for item in allowed:
        needle = normalize_app_id(item)
        if not needle:
            continue
        if key == needle or key.endswith(needle) or needle in key:
            return
    raise PermissionError(
        f"Computer Use allowedApps blocked {app}. Allowed: {', '.join(allowed)}"
    )


def deny_target(window: WindowRef | None, allowed_apps: list[str] | None = None) -> None:
    if window is None:
        return
    deny_allowed_apps(window.app, allowed_apps)
    aumid = ""
    if window.id:
        aumid = process_aumid(hwnd_pid(int(window.id)))
    deny_app_access(window.app, aumid)
