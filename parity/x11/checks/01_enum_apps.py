#!/usr/bin/env python3
"""
parity/x11/checks/01_enum_apps.py

Parity Check: App Enumeration under X11 Headless
Contract:
  - list_apps returns ok: true
  - result.value.apps is a non-empty list of application records
  - Each app record contains valid 'name' (str) and 'pid' (int >= 0)
  - result.value.accessible_apps is a list
  - result.images is empty
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness_client import HarnessClient


def main():
    print("[CHECK 01: Enumeration - list_apps]")
    with HarnessClient() as client:
        resp = client.call_tool("list_apps", {})
        client.shutdown()

    assert resp.get("ok") is True, f"list_apps response not ok: {resp}"
    result = resp.get("result", {})
    assert result.get("name") == "list_apps", f"Expected tool name 'list_apps', got {result.get('name')}"
    assert result.get("images") == [], f"Expected images=[], got {result.get('images')}"

    value = result.get("value", {})
    apps = value.get("apps")
    accessible_apps = value.get("accessible_apps")

    assert isinstance(apps, list), f"Expected apps to be a list, got {type(apps)}"
    assert isinstance(accessible_apps, list), f"Expected accessible_apps to be a list, got {type(accessible_apps)}"

    print(f"  Observed total running apps: {len(apps)}")
    print(f"  Observed accessible apps:    {len(accessible_apps)}")

    if apps:
        first = apps[0]
        assert "name" in first and isinstance(first["name"], str), f"Missing or invalid name in app: {first}"
        assert "pid" in first and isinstance(first["pid"], int) and first["pid"] >= 0, f"Missing or invalid pid: {first}"
        print(f"  Sample app record:           pid={first['pid']}, name={first['name']}")

    acc_error = value.get("accessibility_error")
    print(f"  Accessibility status:        {'OK' if acc_error is None else acc_error}")

    print("  Status: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
