#!/bin/sh
# Launcher for scripts/stub-python-engine.mjs.
#
# `pythonCandidates` only accepts a bare command (plus the special `py -3` case), and
# `spawnPython` appends the real engine argv (`-u -m computer_use --backend ... --surface
# ... serve`). This wrapper forwards that argv unchanged to the Node double, which records
# it as evidence of *how* the browser channel was spawned.
exec node "$(dirname "$0")/stub-python-engine.mjs" "$@"
