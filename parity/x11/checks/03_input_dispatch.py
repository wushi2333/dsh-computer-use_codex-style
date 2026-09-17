#!/usr/bin/env python3
"""
parity/x11/checks/03_input_dispatch.py

Parity Check: Input Injection Dispatch & Deserialization under X11 Headless
Contract:
  - click with valid coordinates echoes parameters and returns ok: true
  - scroll with direction and pages echoes parameters and returns ok: true
  - scroll missing required 'direction' parameter triggers structured error (ok: false)
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness_client import HarnessClient


def main():
    print("[CHECK 03: Input Injection & Validation]")
    with HarnessClient() as client:
        # 1. Valid Click
        click_resp = client.call_tool("click", {"x": 100, "y": 100})
        assert click_resp.get("ok") is True, f"click failed: {click_resp}"
        click_val = click_resp.get("result", {}).get("value", {})
        assert click_val.get("action") == "click", f"Expected action 'click', got {click_val.get('action')}"
        received_click = click_val.get("received", {})
        assert received_click.get("x") == 100, f"Expected x=100, got {received_click.get('x')}"
        assert received_click.get("y") == 100, f"Expected y=100, got {received_click.get('y')}"
        print(f"  Click (100, 100):            PASS -> {click_val.get('message')}")

        # 2. Valid Scroll
        scroll_resp = client.call_tool("scroll", {"direction": "down", "pages": 1.0})
        assert scroll_resp.get("ok") is True, f"scroll failed: {scroll_resp}"
        scroll_val = scroll_resp.get("result", {}).get("value", {})
        assert scroll_val.get("action") == "scroll", f"Expected action 'scroll', got {scroll_val.get('action')}"
        received_scroll = scroll_val.get("received", {})
        assert received_scroll.get("direction") == "down", f"Expected direction 'down', got {received_scroll.get('direction')}"
        print(f"  Scroll (down, 1.0 page):     PASS -> {scroll_val.get('message')}")

        # 3. Invalid Scroll (missing required direction)
        invalid_resp = client.call_tool("scroll", {"pages": 1.0})
        assert invalid_resp.get("ok") is False, f"Expected ok: false for missing direction, got {invalid_resp}"
        assert "direction" in invalid_resp.get("error", "").lower(), f"Expected direction in error, got {invalid_resp.get('error')}"
        print(f"  Validation refusal:          PASS -> {invalid_resp.get('error')}")

        client.shutdown()

    print("  Status: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
