#!/usr/bin/env bash
# ==============================================================================
# scripts/x11-headless/run-smoke.sh
#
# One-command runner for X11 headless smoke testing:
# 1. Resolves helper binary (builds if missing)
# 2. Starts headless X11 session via session.sh (Xvfb, parameterized DISPLAY)
# 3. Executes protocol driver across all 7 tools
# 4. Generates machine-readable JSON summary and prints console report
# 5. Guarantees zero orphan processes on exit
#
# Usage:
#   ./scripts/x11-headless/run-smoke.sh [path-to-binary]
# ==============================================================================

set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

BINARY="${1:-}"
if [ -z "$BINARY" ]; then
  if [ -x "$REPO_ROOT/helper-linux/target/debug/dsh-computer-use" ]; then
    BINARY="$REPO_ROOT/helper-linux/target/debug/dsh-computer-use"
  elif [ -x "$REPO_ROOT/target/debug/dsh-computer-use" ]; then
    BINARY="$REPO_ROOT/target/debug/dsh-computer-use"
  else
    echo "[run-smoke] Binary not found. Building helper-linux..."
    CARGO_BIN="${CARGO_BIN:-$HOME/.cargo/bin/cargo}"
    if ! command -v "$CARGO_BIN" >/dev/null 2>&1; then
      CARGO_BIN="cargo"
    fi
    "$CARGO_BIN" build --manifest-path "$REPO_ROOT/helper-linux/Cargo.toml"
    if [ -x "$REPO_ROOT/helper-linux/target/debug/dsh-computer-use" ]; then
      BINARY="$REPO_ROOT/helper-linux/target/debug/dsh-computer-use"
    else
      BINARY="$REPO_ROOT/target/debug/dsh-computer-use"
    fi
  fi
fi

if [ ! -x "$BINARY" ]; then
  echo "[run-smoke] Fatal: Helper binary not found or not executable at: $BINARY" >&2
  exit 2
fi

echo "[run-smoke] Using helper binary: $BINARY"

ARTIFACTS_DIR="${SMOKE_ARTIFACTS_DIR:-$REPO_ROOT/.cu/artifacts}"
mkdir -p "$ARTIFACTS_DIR"

RESPONSES_JSONL="$ARTIFACTS_DIR/x11-smoke-responses.jsonl"
SUMMARY_JSON="$ARTIFACTS_DIR/x11-smoke-summary.json"

# Execute under session.sh
"$HERE/session.sh" bash -c '
  set -uo pipefail
  DRIVER_PY="'"$HERE"'/driver.py"
  SUMMARY_PY="'"$HERE"'/summary.py"
  BIN="'"$BINARY"'"
  JSONL="'"$RESPONSES_JSONL"'"
  SUMM="'"$SUMMARY_JSON"'"

  python3 "$DRIVER_PY" "$BIN" "$JSONL" --timeout 45
  DRIVER_RC=$?

  python3 "$SUMMARY_PY" "$JSONL" --json-out "$SUMM" --exit-code "$DRIVER_RC"
  SUMMARY_RC=$?

  if [ "$DRIVER_RC" -ne 0 ]; then
    exit "$DRIVER_RC"
  fi
  exit "$SUMMARY_RC"
'
SMOKE_RC=$?

echo "[run-smoke] Finished with exit code $SMOKE_RC"
exit "$SMOKE_RC"
