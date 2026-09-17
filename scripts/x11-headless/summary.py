#!/usr/bin/env python3
"""
scripts/x11-headless/summary.py

Parses the responses JSONL output from driver.py, validates contracts,
evaluates the behavior of the 7 tools under X11 headless mode, and emits:
  1. A machine-readable JSON summary file.
  2. A clean human-readable console report.

Usage:
  python3 scripts/x11-headless/summary.py <responses.jsonl> [--json-out <summary.json>]
"""

import datetime
import json
import os
import shutil
import sys


def parse_responses(jsonl_path):
    responses = []
    with open(jsonl_path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                responses.append(json.loads(line))
            except json.JSONDecodeError as e:
                responses.append({"parse_error": str(e), "raw": line})
    return responses


def analyze(responses, binary_exit_code=0):
    env_display = os.environ.get("DISPLAY", "")
    env_session = os.environ.get("XDG_SESSION_TYPE", "")
    openbox_path = shutil.which("openbox")
    xvfb_path = shutil.which("Xvfb")

    # Map by id
    by_id = {}
    for r in responses:
        if isinstance(r, dict) and "id" in r:
            by_id[r["id"]] = r

    # 1. Health
    health_resp = by_id.get(1, {})
    health_ok = health_resp.get("ok") is True
    health_res = health_resp.get("result", {})
    capabilities = health_res.get("capabilities", {})
    degraded = health_res.get("degraded", [])
    accessibility_info = health_res.get("accessibility", {})

    # 2. Tools
    tools_resp = by_id.get(2, {})
    tools_ok = tools_resp.get("ok") is True
    tools_res = tools_resp.get("result", {})
    surface = tools_res.get("surface", "")
    tool_list = [t.get("name") for t in tools_res.get("tools", [])]

    # 3. Prompt
    prompt_resp = by_id.get(3, {})
    prompt_ok = prompt_resp.get("ok") is True
    prompt_chars = len(prompt_resp.get("result", {}).get("prompt", ""))

    # 4. Refusals
    refusal_method = by_id.get(4, {})
    refusal_tool = by_id.get(5, {})
    malformed_resp = by_id.get(6, {})
    refusals_ok = (
        refusal_method.get("ok") is False
        and refusal_tool.get("ok") is False
        and malformed_resp.get("ok") is False
    )

    # 5. 7 Tools
    # Tool 1: list_apps (id 7)
    list_apps_resp = by_id.get(7, {})
    list_apps_val = list_apps_resp.get("result", {}).get("value", {})
    apps = list_apps_val.get("apps", [])
    acc_apps = list_apps_val.get("accessible_apps", [])
    acc_error = list_apps_val.get("accessibility_error")

    # Tool 2: get_app_state (id 8)
    app_state_resp = by_id.get(8, {})
    app_state_val = app_state_resp.get("result", {}).get("value", {})
    app_state_backend = app_state_val.get("backend")
    readiness = app_state_val.get("readiness", {})
    readiness_blockers = readiness.get("blockers", [])

    # Tool 3: screenshot (id 9)
    screenshot_resp = by_id.get(9, {})
    screenshot_val = screenshot_resp.get("result", {}).get("value", {})
    screenshot_images = screenshot_resp.get("result", {}).get("images", [])
    has_image = len(screenshot_images) > 0
    img_meta = screenshot_images[0] if has_image else {}
    img_data = img_meta.get("data", "")
    base64_len = len(img_data) if isinstance(img_data, str) else 0

    # Tool 4: click (id 10)
    click_resp = by_id.get(10, {})
    click_val = click_resp.get("result", {}).get("value", {})

    # Tool 5: scroll (id 11)
    scroll_resp = by_id.get(11, {})
    scroll_val = scroll_resp.get("result", {}).get("value", {})

    # Tool 6: press_key (id 12)
    press_key_resp = by_id.get(12, {})
    press_key_val = press_key_resp.get("result", {}).get("value", {})

    # Tool 7: type_text (id 13)
    type_text_resp = by_id.get(13, {})
    type_text_val = type_text_resp.get("result", {}).get("value", {})

    # 6. Lifecycle
    end_turn_resp = by_id.get(14, {})
    shutdown_resp = by_id.get(15, {})
    lifecycle_ok = (
        end_turn_resp.get("ok") is True
        and shutdown_resp.get("ok") is True
        and binary_exit_code == 0
    )

    # Contract Assertions
    contract_ok = (
        health_ok
        and tools_ok
        and surface == "sky.window"
        and len(tool_list) == 7
        and prompt_ok
        and refusals_ok
        and list_apps_resp.get("ok") is True
        and app_state_resp.get("ok") is True
        and screenshot_resp.get("ok") is True
        and click_resp.get("ok") is True
        and scroll_resp.get("ok") is True
        and press_key_resp.get("ok") is True
        and type_text_resp.get("ok") is True
        and lifecycle_ok
    )

    summary = {
        "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "status": "PASS" if contract_ok else "FAIL",
        "environment": {
            "display": env_display,
            "session_type": env_session,
            "xvfb_path": xvfb_path,
            "openbox_path": openbox_path,
            "wm_running": openbox_path is not None,
            "ewmh_limited": openbox_path is None,
        },
        "helper": {
            "exit_code": binary_exit_code,
            "surface": surface,
            "tool_count": len(tool_list),
            "tools": tool_list,
        },
        "protocol_contract": {
            "health_ok": health_ok,
            "tools_ok": tools_ok,
            "prompt_ok": prompt_ok,
            "prompt_chars": prompt_chars,
            "refusals_ok": refusals_ok,
            "lifecycle_ok": lifecycle_ok,
        },
        "tools_7_status": {
            "list_apps": {
                "ok": list_apps_resp.get("ok") is True,
                "apps_count": len(apps),
                "accessible_apps_count": len(acc_apps),
                "accessibility_error": acc_error,
            },
            "get_app_state": {
                "ok": app_state_resp.get("ok") is True,
                "backend": app_state_backend,
                "blockers_count": len(readiness_blockers),
                "blockers": readiness_blockers,
            },
            "screenshot": {
                "ok": screenshot_resp.get("ok") is True,
                "source": screenshot_val.get("source"),
                "width": screenshot_val.get("width"),
                "height": screenshot_val.get("height"),
                "bytes": screenshot_val.get("bytes"),
                "format": screenshot_val.get("format"),
                "has_image": has_image,
                "base64_chars": base64_len,
            },
            "click": {
                "ok": click_resp.get("ok") is True,
                "action": click_val.get("action"),
                "implemented": click_val.get("implemented"),
                "message": click_val.get("message"),
            },
            "scroll": {
                "ok": scroll_resp.get("ok") is True,
                "action": scroll_val.get("action"),
                "implemented": scroll_val.get("implemented"),
                "message": scroll_val.get("message"),
            },
            "press_key": {
                "ok": press_key_resp.get("ok") is True,
                "action": press_key_val.get("action"),
                "implemented": press_key_val.get("implemented"),
                "message": press_key_val.get("message"),
            },
            "type_text": {
                "ok": type_text_resp.get("ok") is True,
                "action": type_text_val.get("action"),
                "implemented": type_text_val.get("implemented"),
                "message": type_text_val.get("message"),
            },
        },
        "diagnostics": {
            "at_spi_bus": accessibility_info.get("at_spi_bus", {}),
            "at_spi_enabled": accessibility_info.get("at_spi_enabled", {}),
            "capabilities": capabilities,
            "degraded_count": len(degraded),
            "degraded": degraded,
        },
    }

    return summary, contract_ok


def print_report(summary):
    print("======================================================================")
    print("           DSH COMPUTER-USE X11 HEADLESS SMOKE REPORT                 ")
    print("======================================================================")
    status_str = "✅ PASS" if summary["status"] == "PASS" else "❌ FAIL"
    print(f"Overall Status:        {status_str}")
    env = summary["environment"]
    print(f"Display:               {env['display']}")
    print(f"Session Type:          {env['session_type']}")
    print(f"Window Manager:        {'openbox' if env['wm_running'] else 'none (WM-less mode)'}")
    print(f"EWMH Support:          {'limited (no WM)' if env['ewmh_limited'] else 'full (openbox)'}")
    print(f"Surface:               {summary['helper']['surface']} ({summary['helper']['tool_count']} tools)")
    print("----------------------------------------------------------------------")
    print("P1 7 Tools Under X11 Headless:")
    for name, info in summary["tools_7_status"].items():
        sub_ok = "✅" if info.get("ok") else "❌"
        msg = info.get("message") or info.get("source") or f"count={info.get('apps_count')}"
        print(f"  [{sub_ok}] {name:<14} -> {msg}")
    print("----------------------------------------------------------------------")
    print(f"AT-SPI Bus:            {summary['diagnostics']['at_spi_bus'].get('ok')}")
    print(f"AT-SPI Enabled:        {summary['diagnostics']['at_spi_enabled'].get('ok')}")
    print(f"Helper Exit Code:      {summary['helper']['exit_code']}")
    print("======================================================================")


def main():
    if len(sys.argv) < 2:
        print("Usage: summary.py <responses.jsonl> [--json-out <summary.json>] [--exit-code <code:int>]")
        sys.exit(2)

    jsonl_path = sys.argv[1]
    json_out = None
    exit_code = 0

    if "--json-out" in sys.argv:
        idx = sys.argv.index("--json-out")
        json_out = sys.argv[idx + 1]

    if "--exit-code" in sys.argv:
        idx = sys.argv.index("--exit-code")
        exit_code = int(sys.argv[idx + 1])

    responses = parse_responses(jsonl_path)
    summary, ok = analyze(responses, exit_code)

    print_report(summary)

    if json_out:
        os.makedirs(os.path.dirname(os.path.abspath(json_out)), exist_ok=True)
        with open(json_out, "w", encoding="utf-8") as f:
            json.dump(summary, f, indent=2, ensure_ascii=False)
        print(f"JSON summary written to: {json_out}")

    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
