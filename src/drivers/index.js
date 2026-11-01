/**
 * Unified driver facade — the single place code goes to *reach the machine*.
 *
 * Before this file, a "do something on the desktop" call went through one of three opaque
 * paths scattered across tool.js: the Rust helper (`ctx.dshComputerUse.call`), a fresh
 * PowerShell process (`execFileSync`), or the long-lived UIA host. The downgrade logic that
 * decides *which* to use lived at every call site. This facade does NOT replace those paths
 * (that would be a risky rewrite of every caller); it gives them one front door so that:
 *
 *   - capability checks happen in one place (helper/script calls can preflight the caps and
 *     fail with an actionable alternative instead of a raw stack trace),
 *   - script location + timeout + JSON parsing are defined once,
 *   - new code depends on `drivers` and never re-implements the plumbing.
 *
 * The three drivers:
 *   helper  — the Rust helper over the sidecar JSON-RPC bridge (async, primary)
 *   script  — a one-shot PowerShell script (sync, for things the helper cannot do)
 *   uia     — the long-lived PowerShell UIA host (async, for repeated tree/act/dialog)
 */

import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { pluginRoot } from '../paths.js'
import { getCapabilities } from '../capabilities.js'
import { getUiaHost, getUiaHostReady, prewarmUiaHost, closeUiaHost } from '../uiahost-client.js'

const PS = 'powershell.exe'

/** The scripts/ directory next to src/. */
export function scriptsDir() {
  return path.join(pluginRoot(), 'scripts')
}

/** Resolve a script name (e.g. "uia-post.ps1") or absolute path to its absolute path. */
export function scriptPath(name) {
  return path.isAbsolute(name) ? name : path.join(scriptsDir(), name)
}

/**
 * Run a PowerShell script file once and return its raw stdout.
 *
 * @param {string} name        script filename in scripts/ or an absolute path
 * @param {string[]} args      arguments after `-File <script>`
 * @param {{timeoutMs?:number, parse?:boolean, encoding?:string}} [opts]
 * @returns {string|any}       stdout (string) or parsed JSON when parse:true
 */
export function runScript(name, args = [], opts = {}) {
  const file = scriptPath(name)
  const out = execFileSync(PS, ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', file, ...args], {
    encoding: opts.encoding || 'utf8',
    timeout: opts.timeoutMs || 30_000,
    maxBuffer: opts.maxBuffer || 16 * 1024 * 1024,
    windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  const text = String(out || '').trim()
  if (opts.parse) {
    try { return JSON.parse(text) } catch { return null }
  }
  return text
}

/**
 * Run an inline PowerShell -Command and return raw stdout (used by capability probes and a
 * few one-liners). Kept here so no new call site spawns its own powershell.
 */
export function runPsCommand(script, opts = {}) {
  return execFileSync(PS, ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', script], {
    encoding: 'utf8',
    timeout: opts.timeoutMs || 15_000,
    windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
}

/**
 * The helper driver: one call through the sidecar, with an optional capability preflight.
 * `ctx.dshComputerUse` is the DSH service; it is injected at call time (not captured) so the
 * facade works for any session.
 */
export const helper = {
  /**
   * @param {object} ctx         plugin context (has dshComputerUse)
   * @param {string} name        helper tool name (e.g. "get_window_state")
   * @param {object} args
   * @param {object} [exec]      the tool exec handle (signal/turnMeta)
   * @param {object} [turnMeta]
   */
  call(ctx, name, args, exec, turnMeta) {
    return ctx.dshComputerUse.call(name, args, exec && exec.signal, turnMeta)
  },

  /** Does this session's helper expose this tool? */
  async has(ctx, name) {
    try {
      const listed = await ctx.dshComputerUse.tools('desktop')
      return (listed?.tools || []).some((t) => t && t.name === name)
    } catch { return false }
  },
}

/**
 * Capability gate: throw an actionable error if `capability` is false, BEFORE the caller
 * tries the operation and gets a cryptic failure. Returns the caps table so a caller can
 * also branch on it.
 *
 * @param {object} state  plugin state (capabilities cached here)
 * @param {'psHelper'|'uia'|'screenCap'} capability
 * @param {string} [alternative]  what to do instead, appended to the error
 */
export function requireCapability(state, capability, alternative) {
  const caps = getCapabilities(state)
  const cap = caps[capability]
  if (cap && cap.ok) return caps
  const why = cap && cap.error ? ` (${cap.error})` : ''
  const hint = alternative ? ` Instead: ${alternative}` : ''
  throw new Error(`capability "${capability}" is unavailable in this host${why}.${hint}`)
}

/** Convenience: probe fresh (bypass the cache). */
export function probeNow(state) {
  return getCapabilities(state, { force: true })
}

/** The UIA host driver (thin re-export of uiahost-client, for symmetry). */
export const uia = { getUiaHost, getUiaHostReady, prewarmUiaHost, closeUiaHost }
