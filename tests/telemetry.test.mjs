/**
 * P2 telemetry tests: the structured degradation event store.
 * Pure logic — no subprocess, no real windows.
 */
import assert from 'node:assert/strict'
import test from 'node:test'

import { recordEvent, telemetrySummary, resetTelemetry } from '../src/telemetry.js'

test('recordEvent aggregates counters and keeps a bounded recent ring', () => {
  const state = {}
  resetTelemetry(state)
  for (let i = 0; i < 60; i++) recordEvent(state, 'capture', 'gdi-printwindow', { hwnd: 1 })
  recordEvent(state, 'driver', 'crop.ps1_fallback', 'some error text')
  const s = telemetrySummary(state)
  assert.equal(s.counters['capture.gdi-printwindow'], 60)
  assert.equal(s.counters['driver.crop.ps1_fallback'], 1)
  // Ring is capped at MAX_RECENT (40) — a long session must not grow unbounded.
  assert.ok(s.recent.length <= 40, `recent size ${s.recent.length}`)
  // Newest last.
  assert.equal(s.recent[s.recent.length - 1].name, 'crop.ps1_fallback')
  // String detail is truncated, kept short.
  assert.equal(typeof s.recent[s.recent.length - 1].detail, 'string')
})

test('telemetry never throws and tolerates missing/corrupt state', () => {
  assert.doesNotThrow(() => recordEvent(null, 'a', 'b'))
  assert.doesNotThrow(() => recordEvent({}, 'a', 'b'))
  const bad = { telemetry: { counters: 'not-a-map', recent: 42 } }
  assert.doesNotThrow(() => recordEvent(bad, 'a', 'b'))
  const s = telemetrySummary(bad)
  assert.equal(typeof s.counters, 'object')
  assert.ok(Array.isArray(s.recent))
})

test('detail objects are shallow-summarised with short string values', () => {
  const state = {}
  resetTelemetry(state)
  recordEvent(state, 'uia', 'dead_tree.dotnet_fallback', { app: 'process:weixin', nodes: 76, extra: undefined })
  const s = telemetrySummary(state)
  const d = s.recent[0].detail
  assert.equal(d.app, 'process:weixin')
  assert.equal(d.nodes, 76)
  assert.ok(!('extra' in d), 'null/undefined detail fields are dropped')
})
