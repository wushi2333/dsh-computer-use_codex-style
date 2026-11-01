/**
 * Unit tests for src/capabilities.js — the pure parts (caching + guidance), not the live
 * probes (those need a real desktop and are exercised by health at runtime).
 */
import assert from 'node:assert/strict'
import test from 'node:test'

import { getCapabilities, capabilityGuidance } from '../src/capabilities.js'

test('getCapabilities caches on state and honours maxAgeMs', () => {
  // Pre-seed a fake table so no real probe runs.
  const state = {
    capabilities: {
      psHelper: { ok: true, error: '' },
      uia: { ok: true, error: '' },
      screenCap: { ok: false, error: 'handle is invalid' },
      wgc: { ok: true, note: 'assumed' },
      at: Date.now(),
    },
  }
  const cached = getCapabilities(state, { maxAgeMs: 60_000 })
  assert.equal(cached.screenCap.ok, false, 'should return the cached table')
  assert.equal(cached, state.capabilities, 'same object when fresh')

  // A maxAgeMs of 0 forces a real probe; we cannot predict the host, but it must return a
  // table with the four keys and an `at` timestamp, and must store it back on state.
  const forced = getCapabilities(state, { force: true })
  for (const key of ['psHelper', 'uia', 'screenCap', 'wgc']) {
    assert.ok(forced[key] && typeof forced[key] === 'object', `probe should populate ${key}`)
  }
  assert.equal(typeof forced.at, 'number')
  assert.equal(state.capabilities, forced, 'forced probe replaces the cache')
})

test('capabilityGuidance names the alternative when a capability is missing', () => {
  const allGood = {
    psHelper: { ok: true }, uia: { ok: true }, screenCap: { ok: true },
  }
  assert.deepEqual(capabilityGuidance(allGood), [], 'no notes when everything works')

  const noScreen = {
    psHelper: { ok: true }, uia: { ok: true }, screenCap: { ok: false },
  }
  const notes = capabilityGuidance(noScreen)
  assert.equal(notes.length, 1)
  assert.match(notes[0], /pixel/i, 'must warn that pixel readback is impossible')
  assert.match(notes[0], /UIA tree|accessibility/i, 'must name the alternative')

  const noPs = { psHelper: { ok: false }, uia: { ok: false }, screenCap: { ok: false } }
  const allNotes = capabilityGuidance(noPs)
  assert.equal(allNotes.length, 3, 'one note per missing capability')
})
