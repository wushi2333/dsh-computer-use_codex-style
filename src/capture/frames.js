/**
 * Capture primitives — the state-only, subprocess-free parts of the capture path.
 *
 * What is here:
 *   parseJsonLine(text)              — read the last JSON object a helper script printed
 *   frameObservation(frame, extra)   — turn a captured frame into a tool-result observation
 *   rememberFrame/recallFrame(state) — one stale frame per window, bounded, for timeouts
 *
 * What is deliberately NOT here: the downgrade ladder itself (observeWithLadder,
 * captureFallback) and the script runners (runCaptureScript, gdiWindowFrame, frameHash,
 * documentSnapshot). Those weave through uiaTreeFallback / classifyAccessibilityTree /
 * cachedCdpProbe, which live in tool.js; moving them here would create an import cycle
 * (capture -> tool.js -> capture). They stay in tool.js for now — see ARCHITECTURE.md §6.
 *
 * Extracted verbatim from src/tool.js (P0 split); tool.js re-exports these names.
 */

export function parseJsonLine(text) {
  const line = String(text || '').split(/\r?\n/).find((l) => l.trim().startsWith('{'))
  if (!line) return null
  try { return JSON.parse(line) } catch { return null }
}

export function frameObservation(frame, extra = {}) {
  return {
    ok: true,
    value: {
      accessibility: null,
      screenshots: [{
        id: 'screenshot-0',
        width: frame.width,
        height: frame.height,
        originX: 0,
        originY: 0,
        source: extra.source || 'gdi-printwindow',
        ...extra.screenshot,
      }],
      ...(extra.value || {}),
    },
    images: [{
      data: frame.data,
      mimeType: 'image/png',
      width: frame.width,
      height: frame.height,
      originX: 0,
      originY: 0,
    }],
  }
}

/** Remember the last good frame per window so a later timeout can serve it as stale. */
export function rememberFrame(state, hwnd, frame) {
  if (!Number.isInteger(hwnd) || hwnd <= 0 || !frame || !frame.data) return
  state.frameCache = state.frameCache instanceof Map ? state.frameCache : new Map()
  state.frameCache.set(hwnd, {
    data: frame.data,
    width: Number(frame.width) || 0,
    height: Number(frame.height) || 0,
    at: Date.now(),
  })
  // One stale frame per window is enough; bound the map so a long session cannot
  // accumulate every window it ever captured.
  if (state.frameCache.size > 6) {
    const oldest = [...state.frameCache.entries()].sort((a, b) => a[1].at - b[1].at)[0]
    if (oldest) state.frameCache.delete(oldest[0])
  }
}

export function recallFrame(state, hwnd) {
  if (!Number.isInteger(hwnd) || !(state.frameCache instanceof Map)) return null
  const hit = state.frameCache.get(hwnd)
  return hit && hit.data ? hit : null
}
