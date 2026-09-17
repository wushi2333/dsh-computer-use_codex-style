from __future__ import annotations

import argparse
import json
import sys
from typing import Any, Sequence

from computer_use.errors import DesktopUnavailable
from computer_use.executor import ToolExecutor
from computer_use.harness import system_prompt
from computer_use.runtime import make_executor
from computer_use.surfaces import tools_for_surface


def parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="computer-use", description="LLM-agnostic Computer Use driver")
    parser.add_argument("--backend", choices=("fake", "live", "windows", "helper", "linux"), default="fake")
    parser.add_argument("--surface", choices=("computer", "mac", "browser", "all", "desktop", "gated"), default="computer")
    parser.add_argument("--no-steal-focus", action="store_true", help="mac-style: do not activate_window before app-targeted clicks")
    parser.add_argument("--max-image-edge", type=int, default=1280, help="Downscale captured PNGs to this long edge for DSH vision")
    parser.add_argument("--ttl-ms", type=int, default=0, help="Reject input if get_window_state is older than this many ms (0 disables; official default)")
    parser.add_argument("--allowed-app", action="append", default=[], help="Restrict desktop actions to this app id (repeatable). Empty means unrestricted.")
    parser.add_argument("--parent-pid", type=int, default=0, help="Exit when this parent pid disappears (official helper --parent-pid).")
    parser.add_argument(
        "--system-cursor-manager",
        action="store_true",
        help="Official helper child: suppress/restore OEM cursors via named events, then SPI_SETCURSORS.",
    )
    sub = parser.add_subparsers(dest="command", required=False)
    sub.add_parser("tools", help="Print official window2 JSON Schema tool definitions")
    sub.add_parser("prompt", help="Print the official SKILL/guidance/confirmations/API harness prompt")
    call = sub.add_parser("call", help="Execute one window2 tool and print JSON")
    call.add_argument("name")
    call.add_argument("--args", default="{}", help="JSON object of tool arguments")
    sub.add_parser("serve", help="JSON-RPC stdio sidecar for the DeepSeek Harness plugin")
    return parser.parse_args(argv)


def tools_payload(surface: str = "computer") -> dict[str, Any]:
    return {"tools": tools_for_surface(surface)}


def call_payload(executor: ToolExecutor, name: str, raw_args: str) -> dict[str, Any]:
    arguments = json.loads(raw_args)
    if not isinstance(arguments, dict):
        raise TypeError("--args must be a JSON object")
    return executor.execute(name, arguments)


def main(argv: Sequence[str] | None = None) -> int:
    if sys.platform == "win32":
        from computer_use.win_dpi import enable_dpi_awareness

        enable_dpi_awareness()
    args = parse_args(argv)
    if args.system_cursor_manager:
        from computer_use.cursor_manager import run_manager

        return run_manager(parent_pid=int(args.parent_pid or 0))
    if not args.command:
        print("computer-use: command required", file=sys.stderr)
        return 2
    if args.command == "tools":
        print(json.dumps(tools_payload(args.surface), indent=2))
        return 0
    if args.command == "prompt":
        print(system_prompt())
        return 0
    if args.command == "serve":
        from computer_use.rpc import serve_stdio

        return serve_stdio(
            backend=args.backend,
            surface=args.surface,
            steal_focus=not args.no_steal_focus,
            # 0 = official (no downscale); see decision D-E.
            max_image_edge=max(0, int(args.max_image_edge)),
            ttl_ms=max(0, int(args.ttl_ms)),
            allowed_apps=list(args.allowed_app or []),
            parent_pid=int(args.parent_pid or 0),
        )
    try:
        payload = call_payload(
            make_executor(
                args.backend,
                steal_focus=not args.no_steal_focus,
                ttl_ms=max(0, int(args.ttl_ms)),
                allowed_apps=list(args.allowed_app or []),
            ),
            args.name,
            args.args,
        )
    except DesktopUnavailable as exc:
        print(json.dumps({"ok": False, "error": str(exc)}, indent=2))
        return 2
    print(json.dumps({"ok": True, **payload}, indent=2, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
