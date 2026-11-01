/**
 * The capture downgrade ladder — how a screenshot is obtained when the fast path fails.
 *
 * Order: WGC frame -> repaint nudge + retry -> GDI PrintWindow (static window WGC cannot
 * frame) -> last-known stale frame -> text-only. Each step is recorded so captureGuidance()
 * (errors/) can tell the caller which path it got and what that means for coordinates.
 *
 *   runCaptureScript/gdiWindowFrame    PowerShell capture (gdi-capture.ps1)
 *   nudgeWindow/frameHash              repaint nudge; a pixel hash as a "did it move" signal
 *   documentSnapshot/documentRect      UIA text + rect for document windows
 *   swipeViaScript                     interpolated SendInput swipe
 *   bumpCaptureStat/frame cache        per-window bookkeeping (frameCache from capture/frames)
 *   captureFallback/observeWithLadder  the ladder itself
 *
 * Depends only on drivers, errors.captureGuidance, capture/frames, uia-fallback, and node
 * built-ins — every one of which is now independent of tool.js, so this moves without a
 * cycle. (It keeps a local execFileSync for the `-File <script> ...args` shape that runScript's
 * name+args form does not cover; that is deliberate, not an oversight.)
 * Extracted from src/tool.js (P0 split).
 */

import { execFileSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import { nativeHelperCandidates } from '../paths.js'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { runScript } from '../drivers/index.js'
import { captureGuidance } from '../errors/index.js'
import { parseJsonLine, rememberFrame, recallFrame } from './frames.js'
import { formatUiaFallback, uiaTreeFallback } from '../uia-fallback/index.js'
const GDI_CAPTURE_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'scripts', 'gdi-capture.ps1')

/**
 * Run `gdi-capture.ps1`. The script writes one JSON line and may exit non-zero
 * (2..6) after printing it, so a thrown `execFileSync` still carries a parseable
 * `stdout`; prefer that over the generic process error.
 *
 * @param {number} hwnd window handle
 * @param {{ nudgeOnly?: boolean, outPath?: string }} options
 * @returns {{ok: boolean, width?: number, height?: number, window?: object, screen?: object, oversize?: boolean, error?: string}}
 */
export function runCaptureScript(hwnd, options = {}) {
  if (!Number.isInteger(hwnd) || hwnd <= 0) return { ok: false, error: 'bad window id' }
  // P1 migration: the Rust helper already owns the full capture_hwnd ladder (WGC -> GDI,
  // nudge, overlay masking) — a superset of gdi-capture.ps1. Its one-shot mode is preferred;
  // the ps1 is the fallback when no helper wrote a frame (nudgeOnly always uses ps1, because
  // the helper's nudge is folded into its own capture path, not a standalone no-output op).
  if (!options.nudgeOnly && options.outPath) {
    const viaHelper = captureWithHelper(hwnd, options.outPath)
    if (viaHelper) return viaHelper
  }
  const args = ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', GDI_CAPTURE_SCRIPT, '-Hwnd', String(hwnd)]
  if (options.nudgeOnly) args.push('-NudgeOnly')
  else if (options.outPath) args.push('-OutPath', String(options.outPath))
  try {
    const raw = execFileSync('powershell.exe', args, {
      encoding: 'utf8', timeout: 20_000, windowsHide: true, maxBuffer: 4 * 1024 * 1024,
    })
    return parseJsonLine(raw) || { ok: false, error: String(raw || '').trim() || 'no output' }
  } catch (error) {
    return parseJsonLine(error && error.stdout)
      || { ok: false, error: String(error && error.message ? error.message : error) }
  }
}

/**
 * P1 migration: capture via the Rust helper's env-triggered one-shot mode
 * (DSH_CU_GDI_CAPTURE_HWND/OUT), which writes a PNG and prints the same geometry JSON
 * gdi-capture.ps1 does. Returns the parsed verdict when a helper wrote the frame, else null
 * so runCaptureScript falls back to the ps1.
 */
function captureWithHelper(hwnd, outPath) {
  for (const candidate of nativeHelperCandidates()) {
    try {
      if (!fs.existsSync(candidate)) continue
      const raw = execFileSync(candidate, [], {
        encoding: 'utf8',
        timeout: 20_000,
        windowsHide: true,
        maxBuffer: 4 * 1024 * 1024,
        stdio: ['ignore', 'pipe', 'pipe'],
        env: {
          ...process.env,
          DSH_CU_GDI_CAPTURE_HWND: String(hwnd),
          DSH_CU_GDI_CAPTURE_OUT: outPath,
        },
      })
      if (fs.existsSync(outPath)) {
        return parseJsonLine(raw) || { ok: true, error: '' }
      }
    } catch { /* try next candidate / fall back to ps1 */ }
  }
  return null
}

/**
 * Parse the single JSON line a helper script prints.
 *
 * `gdi-capture.ps1` and `uia-act.ps1` both print their verdict as one JSON line and
 * THEN exit non-zero on failure, so `execFileSync` throws with the payload sitting on
 * `error.stdout`. Reading only `error.message` threw that away and surfaced
 * `Command failed: powershell.exe …` instead of "no element matching …", which is the
 * one sentence the model needs to fix its call.
 */

/** Force a repaint so the helper's own WGC capture session can produce a frame. */
export function nudgeWindow(hwnd) {
  return runCaptureScript(hwnd, { nudgeOnly: true })
}

/** Interpolated SendInput swipe (no helper capture session needed). */
const UIA_SWIPE_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'scripts', 'uia-swipe.ps1')

/**
 * Hash the window's current pixels (GDI, so it works on windows WGC cannot frame).
 * Second signal for "did the page actually move" when no page counter is readable.
 */
export function frameHash(hwnd) {
  try {
    const frame = gdiWindowFrame(hwnd)
    if (!frame || frame.ok !== true || !frame.data) return null
    return crypto.createHash('sha1').update(frame.data).digest('hex')
  } catch {
    return null
  }
}

/**
 * Text snapshot of the window's UIA tree (the .NET walk the plugin already uses).
 * A slide reader's document text carries the page counter and the completion marker
 * ("最后一页了"), so a text diff is the most reliable "did it move" signal there is —
 * far better than pixels, and it needs neither a capture session nor a lease.
 */
export function documentSnapshot(hwnd) {
  if (!Number.isInteger(hwnd) || hwnd <= 0) return null
  try {
    const nodes = uiaTreeFallback(hwnd, 600)
    if (!Array.isArray(nodes) || nodes.length === 0) return null
    const text = nodes.map(n => [n.name, n.value].filter(Boolean).join(' ')).join('\n')
    return { text, lastPage: /最后一页了|最后一页|已学完|学习完成/.test(text) }
  } catch {
    return null
  }
}

/** The visible Document node's window-relative rect, if the tree reports one. */
export function documentRect(hwnd) {
  if (!Number.isInteger(hwnd) || hwnd <= 0) return null
  try {
    const nodes = uiaTreeFallback(hwnd, 600)
    if (!Array.isArray(nodes) || nodes.length === 0) return null
    // The node objects carry no geometry fields, but the formatted tree prints the UIA
    // bounding rectangle after each label: "Document 核心制度培训… {{x:8,y:56,w:768,h:1564}}".
    // Measured: the first Document line is the visible one, so parse from there.
    const text = formatUiaFallback(nodes)
    const m = /(?:Document|文档)\b[^\n{]*\{\{x:(-?\d+),y:(-?\d+),w:(\d+),h:(\d+)\}\}/.exec(text)
    if (m) {
      const [x, y, width, height] = m.slice(1, 5).map(Number)
      if (width > 40 && height > 40) return { x, y, width, height }
    }
    // Last resort: accept any node whose formatted line has a large rect.
    const loose = /\{\{x:(-?\d+),y:(-?\d+),w:(\d+),h:(\d+)\}\}/g
    let best = null
    for (const hit of text.matchAll(loose)) {
      const [x, y, width, height] = hit.slice(1, 5).map(Number)
      if (width < 200 || height < 200) continue
      if (!best || width * height > best.width * best.height) best = { x, y, width, height }
    }
    return best
  } catch {
    return null
  }
}

/**
 * Swipe with `SendInput` directly, bypassing the helper.
 *
 * The helper's `drag` jumps straight from press to release (4 events, no motion in
 * between), which web galleries and slide readers ignore; it also needs a helper capture
 * session that a window WGC cannot frame never has. This script walks the pointer with N
 * intermediate moves and stamps SYNTHETIC_TAG, so it works on both counts.
 */
export function swipeViaScript(hwnd, box) {
  if (!Number.isInteger(hwnd) || hwnd <= 0) return { ok: false, error: 'bad window id' }
  // P1 migration: the helper has an interpolated-SendInput swipe one-shot that replaces
  // uia-swipe.ps1. Preferred; ps1 is the fallback when no usable helper is present.
  const viaHelper = swipeWithHelper(hwnd, box)
  if (viaHelper) return viaHelper
  const args = [
    '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', UIA_SWIPE_SCRIPT,
    '-Hwnd', String(hwnd),
    '-FromX', String(Math.round(box.fromX)), '-FromY', String(Math.round(box.fromY)),
    '-ToX', String(Math.round(box.toX)), '-ToY', String(Math.round(box.toY)),
    '-DurationMs', '350', '-Steps', '14',
  ]
  try {
    return parseJsonLine(execFileSync('powershell.exe', args, {
      encoding: 'utf8', timeout: 20_000, windowsHide: true, maxBuffer: 1024 * 1024,
    })) || { ok: false, error: 'no output' }
  } catch (error) {
    return parseJsonLine(error && error.stdout)
      || { ok: false, error: String(error && error.message ? error.message : error) }
  }
}

/**
 * P1 migration: swipe via the Rust helper's env-triggered one-shot mode
 * (DSH_CU_SWIPE_*). Returns the parsed verdict when a helper actually ran (ok true OR false —
 * a helper that reports a real refusal is authoritative, so we do NOT fall back on a
 * well-formed ok:false), or null when no helper binary could run so the ps1 is tried.
 */
function swipeWithHelper(hwnd, box) {
  for (const candidate of nativeHelperCandidates()) {
    try {
      if (!fs.existsSync(candidate)) continue
      const raw = execFileSync(candidate, [], {
        encoding: 'utf8',
        timeout: 20_000,
        windowsHide: true,
        maxBuffer: 1024 * 1024,
        stdio: ['ignore', 'pipe', 'pipe'],
        env: {
          ...process.env,
          DSH_CU_SWIPE_HWND: String(hwnd),
          DSH_CU_SWIPE_FROMX: String(Math.round(box.fromX)),
          DSH_CU_SWIPE_FROMY: String(Math.round(box.fromY)),
          DSH_CU_SWIPE_TOX: String(Math.round(box.toX)),
          DSH_CU_SWIPE_TOY: String(Math.round(box.toY)),
          DSH_CU_SWIPE_DURATION: '350',
          DSH_CU_SWIPE_STEPS: '14',
        },
      })
      const parsed = parseJsonLine(raw)
      if (parsed && typeof parsed.ok === 'boolean') return parsed
    } catch (e) {
      // A helper that ran and printed a verdict but exited non-zero: honour the verdict.
      const parsed = parseJsonLine(e && e.stdout)
      if (parsed && typeof parsed.ok === 'boolean') return parsed
      // otherwise try next candidate / fall back to ps1
    }
  }
  return null
}

/**
 * Grab a window with PrintWindow(PW_RENDERFULLCONTENT) as base64 PNG.
 *
 * Visual-only fallback: the helper never saw this frame, so it cannot be the
 * lease behind a coordinate click. It still beats "no screenshot at all" —
 * the model can read the page and act by element name or keyboard.
 */
export function gdiWindowFrame(hwnd) {
  // A restricted sandbox can forbid the child (helper/powershell) writing under %TEMP% even
  // when this process can; retry under the plugin root, which is writable. (Same fallback as
  // cropImageBase64.)
  const attempt = (baseDir) => {
    let dir = null
    try {
      dir = fs.mkdtempSync(path.join(baseDir, 'cu-gdi-'))
    } catch { return null }
    try {
      const outPath = path.join(dir, 'frame.png')
      const res = runCaptureScript(hwnd, { outPath })
      if (!res || res.ok !== true) return null
      if (!fs.existsSync(outPath)) return null
      const buf = fs.readFileSync(outPath)
      if (!buf.length) return null
      return {
        ok: true,
        data: buf.toString('base64'),
        width: Number(res.width) || 0,
        height: Number(res.height) || 0,
        geometry: res,
      }
    } catch {
      return null
    } finally {
      try { if (dir) fs.rmSync(dir, { recursive: true, force: true }) } catch { /* ignore */ }
    }
  }
  const tmpBase = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '.cu-tmp')
  try { fs.mkdirSync(tmpBase, { recursive: true }) } catch { /* best effort */ }
  const first = attempt(os.tmpdir())
  if (first) return first
  const second = attempt(tmpBase)
  if (second) return second
  return { ok: false, error: 'capture failed (helper and ps1 both produced no frame)' }
}

/** Shape a raw frame into the helper's observation shape (image + screenshot entry). */



export function bumpCaptureStat(state, key) {
  state.captureStats = state.captureStats instanceof Map ? state.captureStats : new Map()
  state.captureStats.set(key, (state.captureStats.get(key) || 0) + 1)
}

/**
 * Turn a `capture timed out` into the most usable observation available.
 *
 * Preference order — each step keeps more capability than the one after it:
 *   1. **nudge → retry**: a 1 px size round-trip makes the window repaint, so the
 *      helper's *own* capture session gets a frame. Coordinate input, screenshot
 *      ids and the observation lease all keep working.
 *   2. **PrintWindow**: a real picture of a window WGC may never frame (static
 *      page, DRM surface, window larger than the desktop). Visual only.
 *   3. **stale frame**: the last good frame for this window, flagged stale, so a
 *      coordinate click still has pixel geometry to work from.
 *   4. nothing: text-only observation (the previous behaviour).
 *
 * @param {object} state plugin state (frame cache + nudge bookkeeping)
 * @param {number} hwnd target window handle
 * @param {() => Promise<object>} retry re-issues the helper capture call
 * @returns {Promise<{result: object|null, path: string|null, stale: boolean, ageMs?: number, geometry: object|null, error?: string}>}
 */
export async function captureFallback(state, hwnd, retry) {
  const id = Number.isInteger(hwnd) && hwnd > 0 ? hwnd : 0
  let geometry = null
  if (id) {
    const nudged = nudgeWindow(id)
    geometry = nudged || null
    // A window bigger than the whole desktop cannot be framed by WGC and a nudge
    // cannot change that; skip the retry and go straight to PrintWindow (P2-7).
    const hopeless = Boolean(nudged && nudged.oversize)
    if (nudged && nudged.ok && !hopeless && typeof retry === 'function') {
      try {
        const again = await retry()
        if (again && again.ok !== false) {
          state.nudgeNext = state.nudgeNext instanceof Set ? state.nudgeNext : new Set()
          // Remember that this window needed a repaint, so the NEXT observation nudges
          // *before* asking the helper instead of paying a 15 s timeout again.
          state.nudgeNext.add(id)
          bumpCaptureStat(state, 'wgc-after-nudge')
          return { result: again, path: 'wgc-after-nudge', stale: false, geometry }
        }
      } catch { /* retry timed out too; keep degrading */ }
    }
    const frame = gdiWindowFrame(id)
    if (frame && frame.ok) {
      rememberFrame(state, id, frame)
      bumpCaptureStat(state, 'gdi-printwindow')
      return {
        result: frameObservation(frame, { source: 'gdi-printwindow' }),
        path: 'gdi-printwindow',
        stale: false,
        geometry: frame.geometry || geometry,
      }
    }
    geometry = frame && frame.geometry ? { ...(geometry || {}), ...frame.geometry } : geometry
    bumpCaptureStat(state, 'gdi-failed')
  }
  const cached = recallFrame(state, id)
  if (cached) {
    bumpCaptureStat(state, 'stale-frame')
    return {
      result: frameObservation(cached, { source: 'stale-frame', screenshot: { stale: true } }),
      path: 'stale-frame',
      stale: true,
      ageMs: Date.now() - cached.at,
      geometry,
    }
  }
  return { result: null, path: null, stale: false, geometry, error: 'capture timed out and no frame could be recovered' }
}

/**
 * Observe through the capture-degradation ladder.
 *
 * The plugin's own helpers (`computer_use_wait`, `computer_use_wait_until`, the paging
 * probe) used to call `get_window_state` directly, so they failed with
 * `window capture timed out` on exactly the windows `makeSidecarTool` had learned to
 * recover (static page, bigger than the desktop, covered). This wraps the same
 * nudge→PrintWindow→stale-frame ladder around them.
 *
 * @returns {Promise<object>} the sidecar result (a degraded frame when the live capture failed)
 */
export async function observeWithLadder(ctx, state, exec, turnMeta, window, options = {}) {
  const hwnd = Number(window && window.id)
  // P1-7: a stale handle used to look like success (the old window kept answering, so
  // "20 wheel pulses ok" came back for a window nobody was looking at). Remember the
  // most recent list_windows ids and refuse an id that has dropped out of it.
  const known = state && Array.isArray(state.knownWindowIds) ? state.knownWindowIds : null
  if (known && known.length > 0 && Number.isInteger(hwnd) && !known.includes(hwnd)) {
    const hint = 'window id ' + hwnd + ' is not in the most recent list_windows result (now: '
      + known.join(', ') + '). The app may have reopened with a new handle — call computer_use_list_windows '
      + 'and retry with a current id.'
    const error = new Error(hint)
    error.windowStale = true
    throw error
  }
  const params = { window, include_screenshot: true, include_text: Boolean(options.include_text) }
  const attempt = () => ctx.dshComputerUse.call('get_window_state', params, exec.signal, turnMeta)
  try {
    const first = await attempt()
    // `wait` / `wait_until` have no tool spec, so they cannot run the approval retry
    // makeSidecarTool performs. Returning the refusal as if it were an observation is
    // the worst outcome (measured: wait(observe:true) handed back an approvalRequest as
    // its "observation"), so turn it into one actionable sentence instead.
    if (first && first.approvalRequest) {
      const app = String(first.approvalRequest.app || (window && window.app) || '')
      throw new Error(
        `this tool needs ${app || 'the target app'} approved before it can observe. `
        + 'Call get_window_state once for this window — that runs the approval flow (a full-access '
        + 'session grants it automatically) — then retry this call.',
      )
    }
    return first
  } catch (thrown) {
    const msg = String(thrown && thrown.message ? thrown.message : thrown)
    if (!/capture timed out|window capture timed out|covered by another window|no trustworthy frame/i.test(msg)) throw thrown
    const degraded = await captureFallback(state, hwnd, attempt)
    if (!degraded.result) throw thrown
    const value = degraded.result.value && typeof degraded.result.value === 'object' ? degraded.result.value : {}
    degraded.result.value = value
    if (!value.capture) {
      value.capture = {
        failed: true,
        path: degraded.path,
        stale: Boolean(degraded.stale),
        guidance: captureGuidance(degraded),
      }
    }
    return degraded.result
  }
}

/**
 * Compress a UIA tree dump to the parts a reader or element-picker actually needs.
 *
 * The full tree repeats every container's name at each nesting level; a reader sees the
 * same page text 3-4 times. This keeps:
 *   - the document text line(s), verbatim,
 *   - one line per node that carries a meaning of its own (Button/Edit/RadioButton/
 *     CheckBox/Hyperlink/ListItem/TabItem/MenuItem/Combo/ListItem/Document), with its rect,
 *   - nothing else.
 * Measured on 40-question exam pages: ~1.5k tokens down to ~200.
 */



