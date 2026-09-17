#!/usr/bin/env python3
"""
parity/x11/harness_client.py

Lightweight stdio JSONL client library for X11 parity checks.
Drives the helper binary in request-response lockstep.
"""

import json
import os
import select
import subprocess
import sys
import time


def find_binary(explicit_path=None):
    if explicit_path and os.path.exists(explicit_path):
        return os.path.abspath(explicit_path)

    # Search known candidate paths relative to this file
    here = os.path.dirname(os.path.abspath(__file__))
    repo_root = os.path.abspath(os.path.join(here, "../.."))
    candidates = [
        os.path.join(repo_root, "helper-linux/target/debug/dsh-computer-use"),
        os.path.join(repo_root, "target/debug/dsh-computer-use"),
        os.path.join(repo_root, "helper-linux/target/release/dsh-computer-use"),
        os.path.join(repo_root, "target/release/dsh-computer-use"),
    ]
    for c in candidates:
        if os.path.exists(c) and os.access(c, os.X_OK):
            return c

    raise FileNotFoundError(
        f"Helper binary not found. Run 'cargo build --manifest-path helper-linux/Cargo.toml' first."
    )


class HarnessClient:
    def __init__(self, binary_path=None, timeout=30.0):
        self.binary_path = find_binary(binary_path)
        self.timeout = timeout
        self.proc = None
        self._seq = 0

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()

    def start(self):
        self.proc = subprocess.Popen(
            [self.binary_path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

    def request(self, method, params=None, meta=None, budget=None):
        self._seq += 1
        req_id = self._seq
        budget = budget or self.timeout

        req_obj = {
            "id": req_id,
            "method": method,
            "params": params or {},
            "meta": meta or {},
        }
        self.proc.stdin.write(json.dumps(req_obj) + "\n")
        self.proc.stdin.flush()

        deadline = time.time() + budget
        while True:
            remaining = deadline - time.time()
            if remaining <= 0:
                raise TimeoutError(f"Request {req_id} ({method}) timed out after {budget}s")

            ready, _, _ = select.select([self.proc.stdout], [], [], remaining)
            if not ready:
                raise TimeoutError(f"Request {req_id} ({method}) timed out after {budget}s")

            line = self.proc.stdout.readline()
            if line == "":
                stderr_text = self.proc.stderr.read() if self.proc.stderr else ""
                raise EOFError(f"Helper exited unexpectedly. Stderr: {stderr_text.strip()}")

            if not line.strip():
                continue

            resp = json.loads(line)
            if resp.get("id") == req_id:
                return resp

    def call_tool(self, name, arguments=None, budget=None):
        return self.request("call", {"name": name, "arguments": arguments or {}}, budget=budget)

    def health(self):
        return self.request("health")

    def tools(self):
        return self.request("tools")

    def prompt(self):
        return self.request("prompt")

    def end_turn(self, session_id="parity-s", turn_id="parity-t"):
        return self.request("end_turn", {"session_id": session_id, "turn_id": turn_id})

    def shutdown(self):
        try:
            res = self.request("shutdown")
        except Exception:
            res = None
        self.close()
        return res

    def close(self):
        if self.proc:
            try:
                self.proc.stdin.close()
            except Exception:
                pass
            try:
                self.proc.wait(timeout=3.0)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
            self.proc = None
