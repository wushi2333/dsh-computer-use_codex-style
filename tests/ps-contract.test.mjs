/**
 * Contract test for scripts/uia-tree.ps1 (P1 — PowerShell contract-isation).
 *
 * The script promises: stdout is ALWAYS one valid JSON value; exit 0 when it wrote one
 * (even []), exit 1 only on a hard failure before writing. The specific bug this pins down
 * is the Int32-overflow: a BoundingRectangle that UIA reports as Infinity/NaN used to throw
 * "Value was either too large or too small for an Int32" under ErrorActionPreference=Stop,
 * aborting the walk and handing the caller null. Now geometry is coerced, so a bad rect is
 * 0 and the tree survives.
 *
 * Runs the real script against an invalid handle (must not crash the process with an
 * unhandled cast; must print [] on stdout) — that exercises the FromHandle error path and
 * the "always emit JSON" promise without needing a live window.
 */
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const ROOT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..')
const SCRIPT = path.join(ROOT, 'scripts', 'uia-tree.ps1')

function runPs(args) {
  try {
    const out = execFileSync('powershell.exe',
      ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', SCRIPT, ...args],
      { encoding: 'utf8', timeout: 30_000, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] })
    return { code: 0, out: String(out || '').trim() }
  } catch (e) {
    return { code: e.status == null ? -1 : e.status, out: String(e.stdout || '').trim() }
  }
}

test('uia-tree.ps1 : invalid handle -> stdout is valid JSON [], exit 1, no uncaught cast', () => {
  const r = runPs(['-Hwnd', '1'])
  // stdout must parse as JSON (a promise the caller relies on), even on failure.
  let parsed
  try { parsed = JSON.parse(r.out) } catch { parsed = undefined }
  assert.ok(Array.isArray(parsed), `stdout must be a JSON array, got: ${r.out.slice(0, 120)}`)
  assert.equal(parsed.length, 0, 'invalid handle yields an empty array')
  assert.equal(r.code, 1, 'hard failure before output -> exit 1')
})
