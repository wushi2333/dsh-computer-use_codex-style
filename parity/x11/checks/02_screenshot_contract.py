#!/usr/bin/env python3
"""
parity/x11/checks/02_screenshot_contract.py

Parity Check: Capture / Screenshot Contract under X11 Headless
Contract:
  - screenshot returns ok: true
  - result.images contains at least one image descriptor
  - Image descriptor mimeType is image/png
  - Image data is valid base64 with PNG signature (\x89PNG\r\n\x1a\n)
  - Decoded byte count matches result.value.bytes
  - width > 0, height > 0, format == "png"
"""

import base64
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness_client import HarnessClient


PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def main():
    print("[CHECK 02: Capture - screenshot]")
    with HarnessClient() as client:
        resp = client.call_tool("screenshot", {})
        client.shutdown()

    assert resp.get("ok") is True, f"screenshot response not ok: {resp}"
    result = resp.get("result", {})
    assert result.get("name") == "screenshot", f"Expected tool name 'screenshot', got {result.get('name')}"

    images = result.get("images", [])
    assert len(images) >= 1, f"Expected at least 1 image in images list, got {len(images)}"

    img_meta = images[0]
    assert img_meta.get("mimeType") == "image/png", f"Expected mimeType image/png, got {img_meta.get('mimeType')}"

    b64_data = img_meta.get("data", "")
    assert isinstance(b64_data, str) and len(b64_data) > 0, "Image base64 data empty or invalid"

    raw_bytes = base64.b64decode(b64_data)
    assert raw_bytes.startswith(PNG_MAGIC), "PNG magic bytes mismatch in decoded screenshot"

    val = result.get("value", {})
    declared_bytes = val.get("bytes")
    declared_w = val.get("width")
    declared_h = val.get("height")
    source = val.get("source")

    assert declared_bytes == len(raw_bytes), f"Declared bytes {declared_bytes} != actual {len(raw_bytes)}"
    assert declared_w is not None and declared_w > 0, f"Invalid width: {declared_w}"
    assert declared_h is not None and declared_h > 0, f"Invalid height: {declared_h}"
    assert val.get("format") == "png", f"Expected format 'png', got {val.get('format')}"

    print(f"  Observed screenshot source:  {source}")
    print(f"  Observed dimensions:         {declared_w}x{declared_h}")
    print(f"  Observed PNG bytes:          {len(raw_bytes)} (base64 chars: {len(b64_data)})")
    print("  Status: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
