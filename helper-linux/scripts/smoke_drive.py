#!/usr/bin/env python3
"""Drive the dsh-computer-use JSONL helper the way the plugin actually does.

The JS sidecar writes one request and waits for its response before writing the next
(`rawRequest` per call). Feeding every line at once would be a *different* protocol
conversation: the helper would see `end_turn` while an earlier call was still running and
would cancel it, which is correct for a turn that really ended but wrong as a test of the
normal path. So this driver is strictly request/response, and only the interrupt test
writes a second line before reading.

Usage: smoke_drive.py BINARY OUT.jsonl [--timeout SECONDS]
"""
import json
import subprocess
import sys
import time


class Driver:
    def __init__(self, binary, out_path, timeout):
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.out = open(out_path, "w", encoding="utf-8")
        self.timeout = timeout
        self.responses = []

    def send(self, payload):
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()

    def read_one(self, budget):
        """Read one response line, bounded by `budget` seconds."""
        import select
        deadline = time.time() + budget
        while True:
            remaining = deadline - time.time()
            if remaining <= 0:
                raise TimeoutError("no response within %ss" % budget)
            ready, _, _ = select.select([self.proc.stdout], [], [], remaining)
            if not ready:
                raise TimeoutError("no response within %ss" % budget)
            line = self.proc.stdout.readline()
            if line == "":
                raise EOFError("helper closed stdout")
            if not line.strip():
                continue
            self.out.write(line)
            self.out.flush()
            message = json.loads(line)
            self.responses.append(message)
            return message

    def request(self, payload, budget):
        self.send(payload)
        return self.read_one(budget)

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=self.timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.out.close()
        return self.proc.returncode


def main(argv):
    if len(argv) < 3:
        print(__doc__)
        return 2
    binary, out_path = argv[1], argv[2]
    timeout = 60.0
    if "--timeout" in argv:
        timeout = float(argv[argv.index("--timeout") + 1])

    driver = Driver(binary, out_path, timeout)
    problems = []
    call_budget = 30.0
    try:
        # --- pure protocol reads ------------------------------------------------
        driver.request({"id": 1, "method": "health", "params": {}, "meta": {}}, timeout)
        driver.request({"id": 2, "method": "tools", "params": {}, "meta": {}}, timeout)
        driver.request({"id": 3, "method": "prompt", "params": {}, "meta": {}}, timeout)

        # --- refusals: an unadvertised native tool, an unknown tool, an unknown method ---
        driver.request(
            {"id": 4, "method": "call", "params": {"name": "drag", "arguments": {}}, "meta": {}},
            timeout,
        )
        driver.request(
            {"id": 5, "method": "call", "params": {"name": "nope_not_a_tool", "arguments": {}}, "meta": {}},
            timeout,
        )
        driver.request({"id": 6, "method": "not_a_method", "params": {}, "meta": {}}, timeout)

        # --- a malformed line must still be answered under its own id -------------
        driver.proc.stdin.write('{"id":7,"method":}\n')
        driver.proc.stdin.flush()
        driver.read_one(timeout)

        # --- the real desktop calls --------------------------------------------
        driver.request(
            {
                "id": 8,
                "method": "call",
                "params": {"name": "list_apps", "arguments": {}},
                "meta": {"x-oai-cua-request-budget-ms": 25000},
            },
            call_budget,
        )
        driver.request(
            {
                "id": 9,
                "method": "call",
                "params": {"name": "get_app_state", "arguments": {"include_screenshot": False}},
                "meta": {"x-oai-cua-request-budget-ms": 25000},
            },
            call_budget,
        )
        # Screenshot last: on a portal-less or unauthorised session this is the call most
        # likely to block on a system dialog, so everything else is already recorded.
        driver.request(
            {
                "id": 10,
                "method": "call",
                "params": {"name": "screenshot", "arguments": {}},
                "meta": {"x-oai-cua-request-budget-ms": 30000},
            },
            call_budget + 30.0,
        )

        # --- lifecycle ----------------------------------------------------------
        driver.request({"id": 11, "method": "end_turn", "params": {"session_id": "s", "turn_id": "t"}, "meta": {}}, timeout)
        driver.request({"id": 12, "method": "shutdown", "params": {}, "meta": {}}, timeout)
    except TimeoutError as exc:
        problems.append("timeout: %s" % exc)
    except EOFError as exc:
        problems.append("helper closed early: %s" % exc)
    except (BrokenPipeError, OSError) as exc:
        problems.append("transport error: %s" % exc)
    finally:
        code = driver.close()

    print("helper exit: %s" % code)
    for problem in problems:
        print("DRIVER PROBLEM: %s" % problem)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
