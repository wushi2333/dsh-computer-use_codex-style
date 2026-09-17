#!/usr/bin/env python3
"""
scripts/x11-headless/driver.py

Drives the dsh-computer-use JSONL helper binary in request-response lockstep.
Covers:
  - Protocol reads: health, tools, prompt
  - Contract refusals: unknown method, unknown tool, malformed JSON
  - The 7 P1 tools under X11 headless:
      1. list_apps
      2. get_app_state (without screenshot)
      3. screenshot (full display capture)
      4. click
      5. scroll
      6. press_key
      7. type_text
  - Session lifecycle: end_turn, shutdown

Usage:
  python3 scripts/x11-headless/driver.py <binary-path> <responses.jsonl> [--timeout 60]
"""

import json
import os
import select
import subprocess
import sys
import time


class ProtocolDriver:
    def __init__(self, binary_path, output_jsonl_path, timeout=60.0):
        self.binary_path = binary_path
        self.output_jsonl_path = output_jsonl_path
        self.timeout = timeout
        self.responses = []
        self.raw_records = []
        self.proc = None
        self.out_file = None

    def start(self):
        if not os.path.exists(self.binary_path):
            raise FileNotFoundError(f"Binary not found: {self.binary_path}")
        if not os.access(self.binary_path, os.X_OK):
            raise PermissionError(f"Binary not executable: {self.binary_path}")

        out_dir = os.path.dirname(os.path.abspath(self.output_jsonl_path))
        os.makedirs(out_dir, exist_ok=True)
        self.out_file = open(self.output_jsonl_path, "w", encoding="utf-8")

        self.proc = subprocess.Popen(
            [self.binary_path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

    def send_line(self, line):
        self.proc.stdin.write(line + "\n")
        self.proc.stdin.flush()

    def read_one(self, budget):
        deadline = time.time() + budget
        while True:
            remaining = deadline - time.time()
            if remaining <= 0:
                raise TimeoutError(f"No response within {budget:.1f}s")
            ready, _, _ = select.select([self.proc.stdout], [], [], remaining)
            if not ready:
                raise TimeoutError(f"No response within {budget:.1f}s")
            line = self.proc.stdout.readline()
            if line == "":
                stderr_text = self.proc.stderr.read() if self.proc.stderr else ""
                raise EOFError(f"Helper closed stdout. Stderr: {stderr_text.strip()}")
            if not line.strip():
                continue
            self.out_file.write(line)
            self.out_file.flush()
            parsed = json.loads(line)
            return parsed, line.strip()

    def call_rpc(self, req_id, method, params=None, meta=None, budget=30.0):
        req_obj = {
            "id": req_id,
            "method": method,
            "params": params or {},
            "meta": meta or {},
        }
        t0 = time.time()
        self.send_line(json.dumps(req_obj))
        resp, raw_line = self.read_one(budget)
        elapsed_ms = round((time.time() - t0) * 1000, 2)
        record = {
            "id": req_id,
            "request": req_obj,
            "response": resp,
            "elapsed_ms": elapsed_ms,
        }
        self.raw_records.append(record)
        return resp

    def close(self):
        exit_code = None
        if self.proc:
            try:
                self.proc.stdin.close()
            except Exception:
                pass
            try:
                exit_code = self.proc.wait(timeout=5.0)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                exit_code = self.proc.wait()
        if self.out_file:
            self.out_file.close()
        return exit_code


def run(binary_path, out_jsonl, timeout=60.0):
    driver = ProtocolDriver(binary_path, out_jsonl, timeout)
    driver.start()
    errors = []

    try:
        # 1. Pure Protocol Reads
        driver.call_rpc(1, "health", {}, budget=15.0)
        driver.call_rpc(2, "tools", {}, budget=10.0)
        driver.call_rpc(3, "prompt", {}, budget=10.0)

        # 2. Refusals & Contract Envelopes
        driver.call_rpc(4, "not_a_method", {}, budget=10.0)
        driver.call_rpc(5, "call", {"name": "nope_not_a_tool", "arguments": {}}, budget=10.0)

        # Malformed JSON line
        driver.send_line('{"id": 6, "method": }')
        resp_6, _ = driver.read_one(budget=10.0)
        driver.raw_records.append({"id": 6, "request": "malformed_json", "response": resp_6})

        # 3. P1 7 Tools under X11
        # Tool 1: list_apps
        driver.call_rpc(7, "call", {"name": "list_apps", "arguments": {}}, budget=20.0)

        # Tool 2: get_app_state (without screenshot)
        driver.call_rpc(8, "call", {"name": "get_app_state", "arguments": {"include_screenshot": False}}, budget=20.0)

        # Tool 3: screenshot (full display capture)
        driver.call_rpc(9, "call", {"name": "screenshot", "arguments": {}}, budget=30.0)

        # Tool 4: click (x=150, y=150)
        driver.call_rpc(10, "call", {"name": "click", "arguments": {"x": 150, "y": 150}}, budget=15.0)

        # Tool 5: scroll (direction="down", pages=1.0)
        driver.call_rpc(11, "call", {"name": "scroll", "arguments": {"direction": "down", "pages": 1.0}}, budget=15.0)

        # Tool 6: press_key (key="Return")
        driver.call_rpc(12, "call", {"name": "press_key", "arguments": {"key": "Return"}}, budget=15.0)

        # Tool 7: type_text (text="headless-parity")
        driver.call_rpc(13, "call", {"name": "type_text", "arguments": {"text": "headless-parity"}}, budget=15.0)

        # 4. Lifecycle
        driver.call_rpc(14, "end_turn", {"session_id": "headless-s1", "turn_id": "turn-1"}, budget=10.0)
        driver.call_rpc(15, "shutdown", {}, budget=10.0)

    except Exception as exc:
        errors.append(str(exc))
    finally:
        exit_code = driver.close()

    return exit_code, driver.raw_records, errors


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: driver.py <binary_path> <out.jsonl> [--timeout SECONDS]")
        sys.exit(2)

    bin_path = sys.argv[1]
    out_path = sys.argv[2]
    t = 60.0
    if "--timeout" in sys.argv:
        idx = sys.argv.index("--timeout")
        t = float(sys.argv[idx + 1])

    code, records, errs = run(bin_path, out_path, t)
    print(f"[x11-headless driver] Helper exit code: {code}, total calls: {len(records)}")
    if errs:
        for err in errs:
            print(f"[x11-headless driver error] {err}", file=sys.stderr)
        sys.exit(1)
    sys.exit(0)
