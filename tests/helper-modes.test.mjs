/**
 * P0 helper-mode discovery tests — the pure logic plus null-state robustness.
 * (Live exe probing is exercised by the real callers; here we pin the contract.)
 */
import assert from 'node:assert/strict'
import test from 'node:test'

import { pickCapable, discoverHelperModes, capableHelper } from '../src/helper-modes.js'

test('pickCapable returns the first candidate supporting the mode', () => {
  const infos = [
    { exe: 'a.exe', modes: [], rev: '' },
    { exe: 'b.exe', modes: ['crop'], rev: 'abc' },
    { exe: 'c.exe', modes: ['crop', 'gdi', 'swipe'], rev: 'def' },
  ]
  assert.equal(pickCapable(infos, 'crop').exe, 'b.exe')
  assert.equal(pickCapable(infos, 'swipe').exe, 'c.exe')
  assert.equal(pickCapable(infos, 'nope'), null)
  assert.equal(pickCapable([], 'crop'), null)
})

test('discoverHelperModes works with state === null (uncached, never throws)', () => {
  const infos = discoverHelperModes(null)
  assert.ok(Array.isArray(infos))
  // In this repo the shipped/legacy exes exist, so at least one candidate is probed.
  assert.ok(infos.length >= 1)
  for (const info of infos) {
    assert.equal(typeof info.exe, 'string')
    assert.ok(Array.isArray(info.modes))
  }
})

test('capableHelper caches per state and reports no_helper via telemetry once', () => {
  const state = {}
  const exe = capableHelper(state, 'crop')
  // In this checkout a debug/release exe with the modes branch exists, so crop resolves.
  assert.equal(typeof exe, 'string')
  // Second call uses the cache (no re-probe) — hard to observe directly, but the
  // capability surface must be populated for health.
  assert.ok(state.capabilities.helperModes.modes.includes('crop'))
  assert.ok(state._helperModes instanceof Map)
  // A mode nobody supports records exactly one telemetry event.
  const none = capableHelper(state, 'teleport')
  assert.equal(none, null)
  const counters = [...state.telemetry.counters.keys()]
  assert.ok(counters.includes('driver.no_helper_teleport'))
  capableHelper(state, 'teleport')
  assert.equal(state.telemetry.counters.get('driver.no_helper_teleport'), 1, 'recorded once, not spammed')
})
