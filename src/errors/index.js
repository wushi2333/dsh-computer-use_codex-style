/**
 * Error shapes and wedge memory — turn low-level failures into sentences that tell the
 * model exactly what to do next, instead of a raw stack trace.
 *
 *   captureGuidance(degraded)              — one sentence per capture-downgrade path
 *   noteWedge/isWedged(state, app)         — remember a wedged UIA provider per app
 *   friendlyActionError(state, input, msg) — map closed-window / locked-desktop / wedged /
 *                                            human-input errors to an action; clears stale
 *                                            observation state; null when unrecognised.
 *
 * These only touch plugin `state` (lastElements/lastWindow/wedged); no other tool.js
 * function is called, so they move cleanly. Extracted from src/tool.js (P0 split);
 * tool.js re-exports the names so existing imports keep working.
 */
export function captureGuidance(degraded) {
  const geometry = degraded && degraded.geometry ? degraded.geometry : null
  const oversize = Boolean(geometry && geometry.oversize)
  const screen = geometry && geometry.screen ? ` (screen ${geometry.screen.width}x${geometry.screen.height})` : ''
  const win = geometry && geometry.window ? ` window ${geometry.window.width}x${geometry.window.height}` : ''
  if (degraded && degraded.path === 'wgc-after-activate') {
    return 'Another window was covering the target, so Windows Graphics Capture had no trustworthy pixels. The target was activated and captured again — that frame is valid, and coordinates work. '
      + 'Call activate_window before observing a background window to skip this round trip.'
  }
  if (degraded && degraded.path === 'wgc-after-nudge') {
    return 'Static window: the capture needed a 1 px repaint nudge and then succeeded, so coordinates are valid. The window is now nudged before each observation automatically.'
  }
  if (degraded && degraded.path === 'gdi-printwindow') {
    return `Screenshot served by the GDI PrintWindow fallback (Windows Graphics Capture could not frame this static window)${win}${screen}. `
      + 'The picture is valid for reading the page and for x/y geometry, but the helper holds no capture session for it: '
      + 'prefer element names (computer_use_uia_act) or keyboard, and expect one automatic re-observe before a coordinate click lands.'
      + (oversize ? ' NOTE: the window is LARGER THAN THE SCREEN, which is why WGC can never frame it — resize it to fit the desktop.' : '')
  }
  if (degraded && degraded.path === 'stale-frame') {
    return `Live capture still timed out; returned the last frame for this window (${Math.round((degraded.ageMs || 0) / 1000)}s old, marked stale). `
      + 'Geometry is probably still right, but re-read the page before trusting a state change.'
  }
  return 'Screenshot unavailable (capture timed out, and the GDI fallback produced no frame either — likely a DRM/hardware-overlay surface, '
    + 'or a window larger than the desktop). Text is served from the UIA fallback; element names and keyboard shortcuts still work, '
    + 'but coordinate clicks need a screenshot.'
    + (oversize ? ' The window is LARGER THAN THE SCREEN: resize it to fit the desktop and the capture will work.' : '')
}
export function noteWedge(state, app) {
  if (!app) return
  state.wedged = state.wedged instanceof Map ? state.wedged : new Map()
  state.wedged.set(app, Date.now() + 60_000)
  if (state.fallbackLatch) {
    state.wedgeLatched = state.wedgeLatched instanceof Set ? state.wedgeLatched : new Set()
    state.wedgeLatched.add(app)
  }
}

export function isWedged(state, app) {
  if (!app) return false
  if (state.wedgeLatched instanceof Set && state.wedgeLatched.has(app)) return true
  if (!(state.wedged instanceof Map)) return false
  const until = state.wedged.get(app)
  return Boolean(until && Date.now() < until)
}

/**
 * Map low-level, actionable errors (window not found/closed, locked desktop, wedged
 * UIA) to a message that tells the model exactly what to do next, clearing stale
 * observation state. Returns null for anything it does not recognise. This is applied
 * BOTH to errors returned as {ok:false,error} AND to exceptions thrown by `call`
 * (the sidecar throws for an unknown/closed window before returning a result).
 */
export function friendlyActionError(state, input, rawMessage, code) {
  const msg = String(rawMessage == null ? '' : rawMessage)
  // P3-8: the helper sometimes answers with an entire window list inside the error
  // text. That hides the actual sentence, so keep the head and point at the tool that
  // owns the full list instead of pasting it into a tool result.
  // The pass-through test reads the ORIGINAL message: the truncation suffix itself
  // mentions list_windows, so testing the compacted text would swallow every long error.
  const alreadyActionable = /call list_windows/i.test(msg)
  const compact = msg.length > 700
    ? `${msg.slice(0, 700)}… (truncated — call list_windows for the full window list)`
    : msg
  // P1: machine codes from the helper take precedence over message matching — they are
  // stable across rewording. The regex fallbacks below keep old (uncoded) binaries working.
  if (code === 'window_gone') {
    // Stale observation state MUST be cleared even when the message is already
    // actionable — the early `alreadyActionable` return used to skip exactly this.
    state.lastElements = []
    state.lastWindow = null
    return null
  }
  if (code === 'desktop_locked') {
    return new Error(`${compact} — the desktop is LOCKED; ask the user to unlock it, then retry.`)
  }
  if (code === 'input_monitor') {
    return new Error(
      `${compact} — this is the monitor seeing an injected click, and the action did NOT run. `
      + 'Call get_window_state (include_screenshot) and retry once; batch_actions re-observes between coordinate steps automatically.',
    )
  }
  if (code === 'wedged') {
    noteWedge(state, input && input.window && input.window.app)
    return new Error(`${compact} — NOTE: screenshots still work; retry without include_text and act by x/y coordinates.`)
  }
  // Already carries actionable guidance (e.g. the Rust helper now appends it) — don't double it.
  if (alreadyActionable) return null
  // P1-4: the human-input monitor cannot always tell our own injected click from the
  // human's (input that came from outside the helper). The action never ran, so say so
  // instead of leaving the model to guess whether its click landed.
  if (/user input was detected/i.test(compact)) {
    return new Error(
      `${compact} — this is the monitor seeing an injected click, and the action did NOT run. `
      + 'Call get_window_state (include_screenshot) and retry once; batch_actions re-observes between coordinate steps automatically.',
    )
  }
  if (/window id \d+ was not found|was not found|IdGone|window .*?not found|window is gone|window was closed/i.test(compact)) {
    state.lastElements = []
    state.lastWindow = null
    return new Error(`${compact} — the target window appears CLOSED or gone; call list_windows to re-locate it, then retry.`)
  }
  if (/desktop is locked|SecureDesktop/i.test(compact)) {
    return new Error(`${compact} — the desktop is LOCKED; ask the user to unlock it, then retry.`)
  }
  if (/wedged|cooldown|no answer within|accessibility dump|UIA/i.test(compact)) {
    noteWedge(state, input && input.window && input.window.app)
    return new Error(`${compact} — NOTE: screenshots still work; retry without include_text and act by x/y coordinates.`)
  }
  if (compact !== msg) return new Error(compact)
  return null
}

