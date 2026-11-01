/**
 * Structured degradation telemetry (P2).
 *
 * The lesson that built this: during the 2026-11 exam run nobody could answer "which capture
 * path did this observation actually use, and how often is the fallback being hit?" — the
 * counters existed for capture only, and the driver fallbacks (helper -> powershell) and UIA
 * fallbacks were invisible. Now EVERY degradation emits one structured event into a bounded
 * in-memory store that `computer_use_health` summarises.
 *
 * Shape kept in state.telemetry:
 *   counters: { "<category>.<name>": n }          — every event, aggregated
 *   recent:   [{ at, category, name, detail }]    — last MAX_RECENT events (newest last)
 *
 * Bounded by design: a long session must not accumulate unbounded state. Counters are keyed
 * (finite set of paths), recent is a fixed-size ring.
 */

const MAX_RECENT = 40

function ensure(state) {
  if (!state.telemetry || typeof state.telemetry !== 'object') {
    state.telemetry = { counters: new Map(), recent: [] }
  } else {
    if (!(state.telemetry.counters instanceof Map)) state.telemetry.counters = new Map()
    if (!Array.isArray(state.telemetry.recent)) state.telemetry.recent = []
  }
  return state.telemetry
}

/**
 * Record one degradation/decision event. Never throws — telemetry must not be able to break
 * the path it observes.
 *
 * @param {object} state   plugin state (telemetry lives here)
 * @param {string} category  e.g. "capture" | "driver" | "uia"
 * @param {string} name      e.g. "gdi-printwindow" | "helper.ps1_fallback" | "dead-tree.dotnet_fallback"
 * @param {object|string} [detail]  small context (window app, error head). Kept short.
 */
export function recordEvent(state, category, name, detail) {
  try {
    const t = ensure(state)
    const key = `${category}.${name}`
    t.counters.set(key, (t.counters.get(key) || 0) + 1)
    const entry = {
      at: Date.now(),
      category,
      name,
      ...(detail ? { detail: summarizeDetail(detail) } : {}),
    }
    t.recent.push(entry)
    if (t.recent.length > MAX_RECENT) t.recent.splice(0, t.recent.length - MAX_RECENT)
  } catch { /* telemetry must never break the observed path */ }
}

/** Keep detail small: strings truncated, plain objects shallow-copied with short values. */
function summarizeDetail(detail) {
  try {
    if (typeof detail === 'string') return detail.slice(0, 120)
    if (detail && typeof detail === 'object') {
      const out = {}
      for (const [k, v] of Object.entries(detail)) {
        if (v === null || v === undefined) continue
        out[k] = typeof v === 'string' ? v.slice(0, 80) : v
      }
      return out
    }
    return detail
  } catch { return undefined }
}

/**
 * Health summary: counters as a plain object (sorted) + the most recent events.
 * Returned shape goes straight into `health.telemetry`.
 */
export function telemetrySummary(state, { recentCount = 10 } = {}) {
  try {
    const t = ensure(state)
    const counters = Object.fromEntries([...t.counters.entries()].sort(([a], [b]) => a.localeCompare(b)))
    return {
      counters,
      recent: t.recent.slice(-recentCount),
    }
  } catch {
    return { counters: {}, recent: [] }
  }
}

/** Test helper: wipe the store. */
export function resetTelemetry(state) {
  state.telemetry = { counters: new Map(), recent: [] }
}
