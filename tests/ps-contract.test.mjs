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
import os from 'node:os'
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

// --- WMPF critical-path scripts: stdout must ALWAYS be one valid JSON object, even when
// the handle is bogus. These are the routes the exam flow depends on; a null/empty stdout
// here is indistinguishable from "the action silently failed", so the contract is tested.

function runOne(script, args) {
  try {
    const out = execFileSync('powershell.exe',
      ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', path.join(ROOT, 'scripts', script), ...args],
      { encoding: 'utf8', timeout: 30_000, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] })
    return { code: 0, out: String(out || '').trim() }
  } catch (e) {
    return { code: e.status == null ? -1 : e.status, out: String(e.stdout || '').trim() }
  }
}

function assertJsonObject(r, label) {
  const firstLine = r.out.split(/\r?\n/).find((l) => l.trim().startsWith('{'))
  assert.ok(firstLine, `${label}: stdout must contain a JSON object, got: ${r.out.slice(0, 120)}`)
  let parsed
  try { parsed = JSON.parse(firstLine) } catch { parsed = undefined }
  assert.equal(typeof parsed, 'object', `${label}: first JSON line must parse`)
  assert.ok(parsed && 'ok' in parsed, `${label}: JSON must carry an "ok" field`)
  return parsed
}

test('uia-post.ps1 : invalid handle -> valid JSON with ok + per-point FAIL marker', () => {
  const r = runOne('uia-post.ps1', ['-Hwnd', '1', '-Points', '5,5'])
  const parsed = assertJsonObject(r, 'uia-post')
  // Contract: posting issues messages and reports per-point; on a dead handle PostMessage
  // returns false, so the point is tagged :FAIL. ok:true still means "issued", not "worked".
  assert.ok(/5,5:FAIL@/.test(parsed.clicked || ''), `expected :FAIL marker, got ${parsed.clicked}`)
})

test('uia-ticks.ps1 : invalid handle -> valid JSON, ok:false, exit 2', () => {
  const r = runOne('uia-ticks.ps1', ['-Hwnd', '1', '-Points', '5,5'])
  const parsed = assertJsonObject(r, 'uia-ticks')
  assert.equal(parsed.ok, false)
  assert.equal(r.code, 2, 'window-not-visible -> exit 2')
})

test('uia-act.ps1 : invalid handle -> valid JSON, ok:false, exit 1 (no empty stdout)', () => {
  const r = runOne('uia-act.ps1', ['-Hwnd', '1', '-Name', 'x'])
  const parsed = assertJsonObject(r, 'uia-act')
  assert.equal(parsed.ok, false)
  assert.equal(r.code, 1, 'no automation element -> exit 1')
  assert.match(String(parsed.error || ''), /automation element/i)
})

test('crop-image.ps1 : missing input -> valid JSON ok:false, non-zero exit', () => {
  // The caller (cropImageBase64 in tool.js) keys on the non-zero exit to return null; the
  // JSON is informational. Both must hold: valid JSON stdout AND a non-zero exit.
  const out = path.join(os.tmpdir(), `cu-crop-${process.pid}.png`)
  const r = runOne('crop-image.ps1', ['-InPath', 'C:\\definitely\\no\\such\\file.png', '-OutPath', out])
  const parsed = assertJsonObject(r, 'crop-image')
  assert.equal(parsed.ok, false)
  assert.notEqual(r.code, 0, 'failure must be a non-zero exit for the caller')
})
