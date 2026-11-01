/**
 * P0 helper-mode discovery — ask each helper binary ONCE what one-shot modes it supports,
 * cache the answer, and never again waste time running a binary that lacks the mode.
 *
 * Why: the migrations (crop/gdi/swipe) live in helper-rs, but the RUNNING release/bin exe
 * may be an older build (locked by the live sidecar). Before this, every one-shot call
 * walked all candidates and let each stale exe fail silently (~74ms each). Now: one probe
 * per exe per session.
 *
 * Protocol: run the exe with DSH_CU_MODES=1.
 *   modern helper -> prints {"modes":["crop","gdi","swipe"],"rev":...} , exit 0
 *   legacy helper -> prints nothing, exit 0   (treated as modes: [])
 * Cache lives on plugin state (`state._helperModes`, keyed by exe path) and in
 * `capabilities` for health visibility.
 */

import { execFileSync } from 'node:child_process'
import { nativeHelperCandidates } from './paths.js'
import { recordEvent } from './telemetry.js'

/** Pure: pick the first candidate whose modes include `mode`. Testable without a real exe. */
export function pickCapable(candidatesInfo, mode) {
  for (const info of candidatesInfo) {
    if (info && Array.isArray(info.modes) && info.modes.includes(mode)) return info
  }
  return null
}

/** Probe ONE exe for its modes. Never throws. */
export function probeExeModes(exePath) {
  try {
    const raw = execFileSync(exePath, [], {
      encoding: 'utf8',
      timeout: 10_000,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, DSH_CU_MODES: '1' },
    })
    const line = String(raw || '').split(/\r?\n/).find((l) => l.trim().startsWith('{'))
    if (line) {
      const parsed = JSON.parse(line)
      if (parsed && Array.isArray(parsed.modes)) {
        return { exe: exePath, modes: parsed.modes.map(String), rev: String(parsed.rev || '') }
      }
    }
    // Ran fine but no modes JSON -> legacy helper.
    return { exe: exePath, modes: [], rev: '' }
  } catch {
    return { exe: exePath, modes: [], rev: '' }
  }
}

/**
 * Probe every candidate once per session (cached on state) and return the info list.
 * Works with state === null too (uncached local map), so a caller without state never
 * crashes — it just re-probes. `force` re-probes (e.g. after a helper restart).
 */
export function discoverHelperModes(state, { force = false } = {}) {
  const hasState = state && typeof state === 'object'
  const cache = hasState
    ? (state._helperModes instanceof Map ? state._helperModes : (state._helperModes = new Map()))
    : new Map()
  const out = []
  for (const exe of nativeHelperCandidates()) {
    if (force || !cache.has(exe)) cache.set(exe, probeExeModes(exe))
    out.push(cache.get(exe))
  }
  if (hasState) {
    // Surface in capabilities so health shows which helper actually serves one-shot modes.
    const best = out.find((i) => i.modes.length > 0)
    state.capabilities = state.capabilities && typeof state.capabilities === 'object' ? state.capabilities : {}
    state.capabilities.helperModes = {
      modes: best ? best.modes : [],
      rev: best ? best.rev : '',
      probed: out.map((i) => ({ exe: i.exe, modes: i.modes })),
      at: Date.now(),
    }
  }
  return out
}

/**
 * The one call sites use: first candidate that supports `mode`, or null (caller falls back
 * to the ps1 script). Records a telemetry event once per session when NO candidate has the
 * mode, so a stale-binary deployment is visible in health instead of silent.
 */
export function capableHelper(state, mode) {
  const infos = discoverHelperModes(state)
  const picked = pickCapable(infos, mode)
  if (!picked && state) {
    const key = `driver.no_helper_${mode}`
    const t = state.telemetry
    if (!t || !(t.counters instanceof Map) || !t.counters.get(key)) {
      recordEvent(state, 'driver', `no_helper_${mode}`, {})
    }
  }
  return picked ? picked.exe : null
}
