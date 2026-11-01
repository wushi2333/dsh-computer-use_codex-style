/**
 * Capability probing — ask the environment what it can actually do, up front, instead of
 * discovering it by failing mid-task.
 *
 * Why this exists: on 2026-11 we shipped read_options/set_options that read a checkbox by
 * screenshotting pixels. They looked fine in tests and failed only when a real call ran
 * inside the DSH host process, where CopyFromScreen() has no interactive desktop. The
 * lesson: a capability must be probed BEFORE the tool that needs it is offered or run, and
 * the answer cached so callers can pick a different path ("use the UIA tree, not pixels")
 * instead of throwing.
 *
 * What is probed (each is cheap and side-effect free):
 *   uia        — does the .NET UIAutomation client answer for a probe? (tree reads work)
 *   screenCap  — can this process read screen pixels at all? (CopyFromScreen works)
 *   wgc        — is Windows Graphics Capture usable? (best-effort; frame timing is async)
 *   psHelper   — is powershell.exe on PATH and runnable? (uiahost / uia-post rely on it)
 *
 * The probe is cached on the plugin state; health reports it and tools may consult it.
 */

import { execFileSync } from 'node:child_process'

const PS = 'powershell.exe'

/** Run a tiny PowerShell snippet; return { ok, out, err }. Never throws. */
function runPs(script, timeoutMs = 8000) {
  try {
    const out = execFileSync(PS, ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', script], {
      encoding: 'utf8',
      timeout: timeoutMs,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    return { ok: true, out: String(out || '').trim(), err: '' }
  } catch (e) {
    return { ok: false, out: '', err: String(e && e.message ? e.message : e) }
  }
}

/** Is powershell.exe runnable at all? */
export function probePowerShell() {
  const r = runPs('Write-Output "ps-ok"', 8000)
  return { ok: r.ok && r.out.includes('ps-ok'), error: r.ok ? '' : r.err }
}

/**
 * Can this process read screen pixels? This is the exact check that would have caught the
 * pixel-tick bug: it calls CopyFromScreen on a fixed on-screen coordinate and reports
 * whether it throws "handle is invalid" (no desktop access) or succeeds.
 */
export function probeScreenCapture() {
  const script = [
    'Add-Type -AssemblyName System.Drawing',
    'try {',
    '  $bmp = New-Object System.Drawing.Bitmap(4,4)',
    '  $g = [System.Drawing.Graphics]::FromImage($bmp)',
    '  $g.CopyFromScreen(0,0,0,0,(New-Object System.Drawing.Size(4,4)))',
    '  $g.Dispose(); $bmp.Dispose()',
    '  Write-Output "cap-ok"',
    '} catch { Write-Output ("cap-fail:" + $_.Exception.Message) }',
  ].join('\n')
  const r = runPs(script, 8000)
  if (r.ok && r.out.includes('cap-ok')) return { ok: true, error: '' }
  const why = r.out.replace(/^cap-fail:/, '') || r.err || 'unknown'
  return { ok: false, error: why }
}

/** Does the .NET UIAutomation client answer for a probe window (the foreground one)? */
export function probeUia() {
  const script = [
    'Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes',
    'try {',
    '  $h = [System.Diagnostics.Process]::GetCurrentProcess().MainWindowHandle',
    '  $root = [System.Windows.Automation.AutomationElement]::RootElement',
    '  $n = $root.Current.Name',
    '  Write-Output "uia-ok"',
    '} catch { Write-Output ("uia-fail:" + $_.Exception.Message) }',
  ].join('\n')
  const r = runPs(script, 8000)
  return { ok: r.ok && r.out.includes('uia-ok'), error: r.ok && r.out.includes('uia-ok') ? '' : (r.out || r.err) }
}

/**
 * Probe everything once and return a capability table. Synchronous and best-effort: a probe
 * that fails just marks the capability false with a reason, never throws.
 *
 * @returns {{uia:{ok,error}, screenCap:{ok,error}, psHelper:{ok,error}, wgc:{ok,note}, at:number}}
 */
export function probeCapabilities() {
  const ps = probePowerShell()
  // If PowerShell itself is missing, the ps-backed probes cannot say anything useful.
  const uia = ps.ok ? probeUia() : { ok: false, error: 'powershell unavailable' }
  const screenCap = ps.ok ? probeScreenCapture() : { ok: false, error: 'powershell unavailable' }
  return {
    psHelper: ps,
    uia,
    screenCap,
    // WGC has no cheap synchronous probe (it needs a live window + frame callback); we mark
    // it "assumed" and let the capture ladder downgrade at runtime if it fails.
    wgc: { ok: true, note: 'assumed; capture ladder downgrades at runtime' },
    at: Date.now(),
  }
}

/**
 * Lazily probe + cache on state.capabilities. Call from health / first tool that needs it.
 * A cached result older than maxAgeMs is refreshed.
 */
export function getCapabilities(state, { maxAgeMs = 5 * 60_000, force = false } = {}) {
  const cached = state && state.capabilities
  if (!force && cached && typeof cached.at === 'number' && Date.now() - cached.at < maxAgeMs) return cached
  const fresh = probeCapabilities()
  if (state) state.capabilities = fresh
  return fresh
}

/**
 * Human/agent-readable guidance for a missing capability: what to do instead.
 * This is the "failure must talk" contract — a probe that is false names the alternative.
 */
export function capabilityGuidance(caps) {
  const notes = []
  if (!caps.psHelper.ok) notes.push('PowerShell is unavailable: uiahost / uia-post / uia-swipe helpers cannot run; rely on the Rust helper only.')
  if (!caps.uia.ok) notes.push('UIA tree reads are failing in this session; fall back to screenshot + coordinates or the .NET ControlView fallback.')
  if (!caps.screenCap.ok) notes.push('Screen pixel reads are unavailable (no interactive desktop); any pixel-based readback is impossible here — use the UIA tree / accessibility values instead of reading the screen.')
  return notes
}
