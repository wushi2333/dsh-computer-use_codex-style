#!/usr/bin/env bash
# ==============================================================================
# scripts/x11-headless/session.sh
#
# Dedicated X11 headless session manager using Xvfb.
# Manages the full lifecycle of a virtual X11 display:
# - Starts Xvfb on a parameterized DISPLAY (default :99)
# - Waits for socket and X server readiness via xdpyinfo
# - Detects and launches Window Manager (openbox) if available; otherwise falls
#   back to WM-less operation with explicit EWMH limitation warning
# - Exports X11 environment (DISPLAY, XDG_SESSION_TYPE=x11, unsets WAYLAND_*)
# - Executes any wrapped command inside the session
# - Enforces strict cleanup on EXIT / SIGINT / SIGTERM (zero orphan processes)
#
# Usage:
#   ./scripts/x11-headless/session.sh [command [args...]]
#
# Environment variables:
#   XVFB_DISPLAY: Display number without colon (default: 99 -> :99)
#   XVFB_RES:     Screen resolution and depth (default: 1280x800x24)
#   XVFB_TIMEOUT: Max seconds to wait for Xvfb socket (default: 10)
# ==============================================================================

set -uo pipefail

XVFB_NUM="${XVFB_DISPLAY:-99}"
XVFB_DISPLAY=":${XVFB_NUM}"
XVFB_RES="${XVFB_RES:-1280x800x24}"
XVFB_TIMEOUT="${XVFB_TIMEOUT:-10}"

XVFB_PID=""
WM_PID=""
CHILD_PID=""
CLEANED=0

cleanup() {
  if [ "$CLEANED" -eq 1 ]; then
    return
  fi
  CLEANED=1

  # 1. Terminate child command if active
  if [ -n "$CHILD_PID" ] && kill -0 "$CHILD_PID" 2>/dev/null; then
    kill -TERM "$CHILD_PID" 2>/dev/null || true
    for _ in $(seq 1 20); do
      if ! kill -0 "$CHILD_PID" 2>/dev/null; then break; fi
      sleep 0.1
    done
    kill -KILL "$CHILD_PID" 2>/dev/null || true
    wait "$CHILD_PID" 2>/dev/null || true
  fi

  # 2. Terminate Window Manager if active
  if [ -n "$WM_PID" ] && kill -0 "$WM_PID" 2>/dev/null; then
    kill -TERM "$WM_PID" 2>/dev/null || true
    for _ in $(seq 1 10); do
      if ! kill -0 "$WM_PID" 2>/dev/null; then break; fi
      sleep 0.1
    done
    kill -KILL "$WM_PID" 2>/dev/null || true
    wait "$WM_PID" 2>/dev/null || true
  fi

  # 3. Terminate Xvfb
  if [ -n "$XVFB_PID" ] && kill -0 "$XVFB_PID" 2>/dev/null; then
    kill -TERM "$XVFB_PID" 2>/dev/null || true
    for _ in $(seq 1 20); do
      if ! kill -0 "$XVFB_PID" 2>/dev/null; then break; fi
      sleep 0.1
    done
    kill -KILL "$XVFB_PID" 2>/dev/null || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi

  # 4. Clean up lingering X socket/lock files for this display if left orphaned
  rm -f "/tmp/.X11-unix/X${XVFB_NUM}" "/tmp/.X${XVFB_NUM}-lock" 2>/dev/null || true
}

trap cleanup EXIT INT TERM HUP

# Pre-flight check: Xvfb binary
if ! command -v Xvfb >/dev/null 2>&1; then
  echo "[x11-headless] Error: Xvfb binary not found in PATH." >&2
  exit 127
fi

# Check for existing lock or socket
SOCKET_PATH="/tmp/.X11-unix/X${XVFB_NUM}"
LOCK_PATH="/tmp/.X${XVFB_NUM}-lock"

if [ -e "$SOCKET_PATH" ] || [ -e "$LOCK_PATH" ]; then
  ACTIVE=0
  if [ -f "$LOCK_PATH" ]; then
    LOCK_PID=$(tr -cd '0-9' < "$LOCK_PATH" 2>/dev/null || true)
    if [ -n "$LOCK_PID" ] && kill -0 "$LOCK_PID" 2>/dev/null; then
      ACTIVE=1
    fi
  fi
  if [ "$ACTIVE" -eq 0 ] && command -v xdpyinfo >/dev/null 2>&1 && xdpyinfo -display "$XVFB_DISPLAY" >/dev/null 2>&1; then
    ACTIVE=1
  fi

  if [ "$ACTIVE" -eq 1 ]; then
    echo "[x11-headless] Error: Display $XVFB_DISPLAY is already in use by another active X server." >&2
    exit 98
  else
    echo "[x11-headless] Warning: Removing stale lock/socket for display $XVFB_DISPLAY." >&2
    rm -f "$SOCKET_PATH" "$LOCK_PATH" 2>/dev/null || true
  fi
fi

# Launch Xvfb
XVFB_LOG="$(mktemp -t xvfb-${XVFB_NUM}-XXXXXX.log)"
Xvfb "$XVFB_DISPLAY" -screen 0 "$XVFB_RES" -ac +extension GLX +extension RANDR +render -noreset >"$XVFB_LOG" 2>&1 &
XVFB_PID=$!

# Wait for Xvfb readiness
READY=0
START_TIME=$(date +%s)
while true; do
  NOW=$(date +%s)
  ELAPSED=$((NOW - START_TIME))
  if [ "$ELAPSED" -ge "$XVFB_TIMEOUT" ]; then
    break
  fi

  if [ -S "$SOCKET_PATH" ]; then
    if command -v xdpyinfo >/dev/null 2>&1; then
      if xdpyinfo -display "$XVFB_DISPLAY" >/dev/null 2>&1; then
        READY=1
        break
      fi
    else
      READY=1
      break
    fi
  fi

  if ! kill -0 "$XVFB_PID" 2>/dev/null; then
    echo "[x11-headless] Error: Xvfb died unexpectedly during startup." >&2
    cat "$XVFB_LOG" >&2
    rm -f "$XVFB_LOG"
    exit 1
  fi

  sleep 0.1
done

rm -f "$XVFB_LOG"

if [ "$READY" -ne 1 ]; then
  echo "[x11-headless] Error: Timed out waiting for Xvfb on $XVFB_DISPLAY (${XVFB_TIMEOUT}s)." >&2
  exit 1
fi

echo "[x11-headless] Xvfb active on display $XVFB_DISPLAY ($XVFB_RES) [PID: $XVFB_PID]"

# Configure Environment
export DISPLAY="$XVFB_DISPLAY"
export XDG_SESSION_TYPE="x11"
unset WAYLAND_DISPLAY
unset WAYLAND_SOCKET

# Check for Window Manager
if command -v openbox >/dev/null 2>&1; then
  openbox >/dev/null 2>&1 &
  WM_PID=$!
  sleep 0.3
  if kill -0 "$WM_PID" 2>/dev/null; then
    echo "[x11-headless] Window manager openbox started [PID: $WM_PID]"
  else
    echo "[x11-headless] Warning: openbox failed to start; continuing in WM-less mode." >&2
    WM_PID=""
  fi
else
  echo "[x11-headless] Notice: Window manager (openbox) missing. Running in WM-less mode."
  echo "[x11-headless] Note: EWMH window-management features (_NET_ACTIVE_WINDOW, etc.) are limited without a WM."
  WM_PID=""
fi

# Execute target command or keep session alive
if [ "$#" -gt 0 ]; then
  "$@" &
  CHILD_PID=$!
  wait "$CHILD_PID"
  CMD_EXIT=$?
  CHILD_PID=""
  exit "$CMD_EXIT"
else
  echo "[x11-headless] Session running on DISPLAY=$DISPLAY. Send SIGINT/SIGTERM to terminate."
  while true; do
    sleep 1
  done
fi
