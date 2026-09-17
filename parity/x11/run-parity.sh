#!/usr/bin/env bash
# ==============================================================================
# parity/x11/run-parity.sh
#
# Master runner for Linux/X11 headless parity suite.
# Wraps execution in scripts/x11-headless/session.sh and runs all check scripts
# located in parity/x11/checks/.
#
# Usage:
#   ./parity/x11/run-parity.sh
# ==============================================================================

set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SESSION_SH="$REPO_ROOT/scripts/x11-headless/session.sh"

if [ ! -x "$SESSION_SH" ]; then
  echo "[run-parity] Error: session.sh not found or not executable at $SESSION_SH" >&2
  exit 2
fi

echo "======================================================================"
echo "          RUNNING LINUX/X11 HEADLESS PARITY SUITE                     "
echo "======================================================================"

"$SESSION_SH" bash -c '
  set -uo pipefail
  CHECKS_DIR="'"$HERE"'/checks"
  CHECK_SCRIPTS=($(ls -1 "$CHECKS_DIR"/*.py 2>/dev/null | sort))

  if [ "${#CHECK_SCRIPTS[@]}" -eq 0 ]; then
    echo "[run-parity] No check scripts found in $CHECKS_DIR"
    exit 1
  fi

  TOTAL=0
  PASSED=0
  FAILED=0
  FAILURES=()

  for script in "${CHECK_SCRIPTS[@]}"; do
    NAME=$(basename "$script")
    TOTAL=$((TOTAL + 1))
    echo "--- Running $NAME ---"
    python3 "$script"
    RC=$?
    if [ "$RC" -eq 0 ]; then
      PASSED=$((PASSED + 1))
      echo ">>> RESULT: $NAME -> PASS"
    else
      FAILED=$((FAILED + 1))
      FAILURES+=("$NAME")
      echo ">>> RESULT: $NAME -> FAIL (code $RC)"
    fi
    echo
  done

  echo "======================================================================"
  echo "PARITY SUITE RESULTS: $PASSED / $TOTAL PASSED ($FAILED FAILED)"
  echo "======================================================================"

  if [ "$FAILED" -gt 0 ]; then
    echo "Failed checks:"
    for f in "${FAILURES[@]}"; do
      echo "  - $f"
    done
    exit 1
  fi
  exit 0
'
PARITY_RC=$?

exit "$PARITY_RC"
