#!/usr/bin/env bash
# Protocol-level smoke test for the dsh-computer-use JSONL helper.
#
# Drives the real binary the way the plugin does -- one request, then its response -- and
# prints every reply, so the contract can be read rather than trusted. It does not need a
# working desktop: whatever the real session returns is reported, and only the *contract*
# is asserted.
#
#   scripts/smoke-jsonl.sh [path-to-binary]
#
# Exit 0 when the protocol envelope is correct. Desktop results may legitimately fail on a
# machine without a portal or accessibility backend; that is reported, not hidden.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:-$HERE/../target/debug/dsh-computer-use}"
if [ ! -x "$BIN" ]; then
  echo "helper binary not found or not executable: $BIN" >&2
  echo "build it first: cargo build --bin dsh-computer-use" >&2
  exit 2
fi
echo "binary: $BIN"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
TIMEOUT_SECONDS="${SMOKE_TIMEOUT_SECONDS:-120}"

python3 "$HERE/smoke_drive.py" "$BIN" "$WORK/responses.jsonl" --timeout "$TIMEOUT_SECONDS"
DRIVE_RC=$?
echo "driver exit: $DRIVE_RC"

echo "--- raw responses (large base64 payloads elided) ---"
python3 "$HERE/smoke_assert.py" --pretty "$WORK/responses.jsonl"
echo
echo "--- contract checks ---"
python3 "$HERE/smoke_assert.py" "$WORK/responses.jsonl"
ASSERT_RC=$?
echo
python3 "$HERE/smoke_assert.py" --summary "$WORK/responses.jsonl"

if [ "$DRIVE_RC" -ne 0 ]; then
  exit "$DRIVE_RC"
fi
exit "$ASSERT_RC"
