#!/usr/bin/env bash
# ==============================================================================
# scripts/x11-headless/run-window2-e2e.sh
#
# One-command runner for the window2 FULL-SURFACE headless end-to-end test.
#
# It is deliberately separate from run-smoke.sh: that one covers the P1
# sky.window surface (7 tools), this one drives the official window2 surface
# (13 methods) over the same real JSONL helper binary.
#
# 1. Resolves the helper binary (builds helper-linux when missing)
# 2. Starts a headless X11 session via session.sh, with an xterm to act on
# 3. Runs window2_driver.py: invokes all 13 methods, asserts the capture came
#    from the native X11 path (composite/direct) and NOT xdg-desktop-portal,
#    validates the returned PNG image part, and proves a window-relative click
#    was actually delivered
# 4. Prints a console report and writes a machine-readable JSON summary
# 5. Guarantees zero orphan processes on exit (session.sh owns cleanup)
#
# Usage:
#   ./scripts/x11-headless/run-window2-e2e.sh [path-to-binary]
#
# Environment variables:
#   XVFB_DISPLAY, XVFB_RES, XVFB_TIMEOUT  -- see session.sh
#   WINDOW2_E2E_ARTIFACTS_DIR              -- override the artifact directory
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
    echo "[run-window2-e2e] Binary not found. Building helper-linux..."
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
  echo "[run-window2-e2e] Fatal: helper binary not found or not executable at: $BINARY" >&2
  exit 2
fi

echo "[run-window2-e2e] Using helper binary: $BINARY"

ARTIFACTS_DIR="${WINDOW2_E2E_ARTIFACTS_DIR:-$REPO_ROOT/.cu/artifacts}"
mkdir -p "$ARTIFACTS_DIR"
SUMMARY_JSON="$ARTIFACTS_DIR/x11-window2-e2e-summary.json"

# The xterm is the window under test. Without a window manager Xvfb gives it no
# decoration, and window2 own WM-less path handles activation; that is the
# supported degraded mode on this machine (openbox is absent by convention).
"$HERE/session.sh" bash -c '
  set -uo pipefail
  DRIVER_PY="'"$HERE"'/window2_driver.py"
  BIN="'"$BINARY"'"
  SUMM="'"$SUMMARY_JSON"'"

  XTERM_TITLE="dsh-window2-e2e-$RANDOM"
  xterm -T "$XTERM_TITLE" >/dev/null 2>&1 &
  XTERM_PID=$!
  cleanup_xterm() { kill "$XTERM_PID" 2>/dev/null || true; }
  trap cleanup_xterm EXIT
  # Give xterm time to map its window and the X server time to publish it.
  sleep 2

  python3 "$DRIVER_PY" "$BIN" --json-out "$SUMM"
  DRIVER_RC=$?

  kill "$XTERM_PID" 2>/dev/null || true
  trap - EXIT
  exit "$DRIVER_RC"
'
E2E_RC=$?

echo "[run-window2-e2e] Finished with exit code $E2E_RC"
exit "$E2E_RC"
