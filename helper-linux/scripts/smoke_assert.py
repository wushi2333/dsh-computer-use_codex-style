#!/usr/bin/env python3
"""Assertions and summaries for the dsh-computer-use JSONL smoke test.

Kept next to smoke-jsonl.sh so the checks stay readable and diffable, rather than
buried in a heredoc where a quoting mistake silently disables them.

  smoke_assert.py --pretty|--summary RESPONSES.jsonl   (informational)
  smoke_assert.py RESPONSES.jsonl                      (assert; non-zero on failure)

Environment-dependent tool results are reported but never asserted, because a missing
portal or accessibility backend is a machine fact, not a protocol bug.
"""
import json
import sys

EXPECTED_TOOLS = [
    "list_apps",
    "get_app_state",
    "screenshot",
    "click",
    "scroll",
    "press_key",
    "type_text",
]
EXPECTED_METHODS = ["call", "end_turn", "health", "interrupt", "prompt", "shutdown", "tools"]
UPSTREAM_COMMIT = "5f7310d71dd02e6e0131deec6fa89d26c8bcaf9c"


def load(path):
    messages = []
    by_id = {}
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if not line.strip():
                continue
            try:
                message = json.loads(line)
            except Exception as exc:  # reported, never raised
                messages.append(("unparsed", "%s: %s" % (exc, line[:200])))
                continue
            messages.append(("parsed", message))
            by_id[message.get("id")] = message
    return messages, by_id


def pretty(path):
    """Print every response, eliding huge base64 blobs so the log stays readable."""
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if not line.strip():
                continue
            try:
                message = json.loads(line)
            except Exception:
                print("<non-JSON line> %s" % line.rstrip()[:200])
                continue
            result = message.get("result")
            if isinstance(result, dict):
                for part in result.get("images") or []:
                    data = part.get("data") or ""
                    if len(data) > 32:
                        part["data"] = "<%d base64 chars, %s>" % (len(data), part.get("mimeType"))
                prompt = result.get("prompt")
                if isinstance(prompt, str) and len(prompt) > 120:
                    result["prompt"] = "<%d chars: %r...>" % (len(prompt), prompt[:60])
            text = json.dumps(message, ensure_ascii=False)
            print(text if len(text) <= 3000 else text[:3000] + " ...<truncated>")


def check(path):
    failures = []
    messages, by_id = load(path)
    if not messages:
        return ["helper produced no output"]
    for kind, item in messages:
        if kind == "unparsed":
            failures.append("non-JSON response line: %s" % item)

    # ids 7/8 touch the live desktop and may exceed the budget; they are reported but
    # never required, because a slow session must not read as a protocol failure.
    for want in (1, 2, 3, 4, 5, 6, 7, 11, 12):
        if want not in by_id:
            failures.append("missing response for id %s" % want)

    def result(ident):
        return by_id.get(ident, {}).get("result")

    health = result(1) or {}
    if health.get("ok") is not True:
        failures.append("health did not report ok")
    if health.get("surface") != "sky.window":
        failures.append("health surface is %r" % (health.get("surface"),))
    if health.get("protocol") != "stdio-jsonl":
        failures.append("health protocol is %r" % (health.get("protocol"),))
    if not isinstance(health.get("degraded"), list):
        failures.append("health has no degraded list")
    if not isinstance(health.get("readiness"), dict):
        failures.append("health has no readiness report")
    if sorted(health.get("methods") or []) != EXPECTED_METHODS:
        failures.append("health methods are %r" % (health.get("methods"),))
    if (health.get("upstream") or {}).get("commit") != UPSTREAM_COMMIT:
        failures.append("health does not pin the upstream commit")

    tools = (result(2) or {}).get("tools")
    names = [t.get("name") for t in tools] if isinstance(tools, list) else []
    if names != EXPECTED_TOOLS:
        failures.append("tools listed %r, expected %r" % (names, EXPECTED_TOOLS))
    for tool in tools or []:
        if not isinstance(tool.get("parameters"), dict):
            failures.append("%s has no parameters schema" % tool.get("name"))
        if not tool.get("description"):
            failures.append("%s has no description" % tool.get("name"))

    prompt = result(3) or {}
    if not isinstance(prompt.get("prompt"), str) or prompt.get("surface") != "sky.window":
        failures.append("prompt did not return the sky.window API reference")

    # An unadvertised *native* tool and an unknown name must be refused identically:
    # neither may be routable, and the host must not be able to tell them apart.
    for ident, name in ((4, "drag"), (5, "nope_not_a_tool")):
        message = by_id.get(ident, {})
        if message.get("ok") is not False:
            failures.append("%s was not refused (id %s)" % (name, ident))
        elif not str(message.get("error", "")).startswith("unsupported method: "):
            failures.append("%s refusal wording: %r" % (name, message.get("error")))

    unknown = by_id.get(6, {})
    if unknown.get("ok") is not False:
        failures.append("an unknown method was not refused")
    elif "unsupported method: not_a_method" not in str(unknown.get("error")):
        failures.append("unknown-method wording: %r" % (unknown.get("error"),))

    malformed = by_id.get(7, {})
    if malformed.get("ok") is not False:
        failures.append("a malformed line did not produce a correlated error")
    elif "invalid request" not in str(malformed.get("error")):
        failures.append("malformed-line wording: %r" % (malformed.get("error"),))

    # Every successful call must be exactly {ok,name,value,images} with no inline pixels.
    for ident in (8, 9, 10):
        message = by_id.get(ident, {})
        if message.get("ok") is not True:
            continue
        payload = message.get("result")
        if not isinstance(payload, dict) or set(payload) != {"ok", "name", "value", "images"}:
            failures.append("call %s result keys are %r" % (ident, sorted(payload or [])))
            continue
        for part in payload.get("images") or []:
            if set(part) != {"mimeType", "data", "name"}:
                failures.append("call %s image part keys are %r" % (ident, sorted(part)))
            if str(part.get("data") or "").startswith("data:"):
                failures.append("call %s image data is still a data URL" % ident)
        blob = json.dumps(payload.get("value"))
        if "base64," in blob:
            failures.append("call %s leaked an inline base64 data URL into value" % ident)

    if (result(11) or {}).get("ended") is not True:
        failures.append("end_turn did not report ended")
    if (result(12) or {}).get("closed") is not True:
        failures.append("shutdown did not report closed")
    return failures


def summary(path):
    _messages, by_id = load(path)
    health = by_id.get(1, {}).get("result") or {}
    platform = health.get("platform") or {}
    print("--- environment-dependent results (reported, never asserted) ---")
    print("platform: session=%s desktop=%s wayland=%s display=%s" % (
        platform.get("xdg_session_type"),
        platform.get("xdg_current_desktop"),
        platform.get("wayland_display"),
        platform.get("display"),
    ))
    for ident, label in ((8, "list_apps"), (9, "get_app_state"), (10, "screenshot")):
        message = by_id.get(ident, {})
        if message.get("ok") is not True:
            print("%s: FAILED -> %s" % (label, message.get("error")))
            continue
        payload = message.get("result") or {}
        value = payload.get("value") or {}
        if label == "list_apps":
            apps = value.get("apps")
            accessible = value.get("accessible_apps")
            print("list_apps: apps=%s accessible_apps=%s accessibility_error=%s images=%s" % (
                len(apps) if isinstance(apps, list) else None,
                len(accessible) if isinstance(accessible, list) else None,
                value.get("accessibility_error"),
                len(payload.get("images") or []),
            ))
        elif label == "screenshot":
            print("screenshot: source=%s %sx%s mime=%s images=%s" % (
                value.get("source"),
                value.get("width"),
                value.get("height"),
                (payload.get("images") or [{}])[0].get("mimeType"),
                len(payload.get("images") or []),
            ))
        else:
            print("get_app_state: backend=%s ax_nodes=%s window_error=%s screenshot_error=%s images=%s" % (
                value.get("backend"),
                len(value.get("accessibility_tree") or []),
                value.get("window_error"),
                value.get("screenshot_error"),
                len(payload.get("images") or []),
            ))
    degraded = health.get("degraded") or []
    print("health degraded checks (%d):" % len(degraded))
    for item in degraded[:15]:
        print("  - %s" % item)
    if not degraded:
        print("  (none reported as failing)")


def main(argv):
    # Assert mode is the bare form: smoke_assert.py RESPONSES.jsonl
    if len(argv) == 2 and not argv[1].startswith("--"):
        failures = check(argv[1])
        if failures:
            print("CONTRACT FAILURES:")
            for item in failures:
                print("  - %s" % item)
            return 1
        print("CONTRACT OK: envelope, 7-tool surface, refusals, image split, end_turn, shutdown")
        return 0
    if len(argv) != 3 or argv[1] not in ("--pretty", "--summary"):
        print(__doc__)
        return 2
    if argv[1] == "--pretty":
        pretty(argv[2])
        return 0
    if argv[1] == "--summary":
        summary(argv[2])
        return 0
    failures = check(argv[2])
    if failures:
        print("CONTRACT FAILURES:")
        for item in failures:
            print("  - %s" % item)
        return 1
    print("CONTRACT OK: envelope, 7-tool surface, refusals, image split, end_turn, shutdown")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
