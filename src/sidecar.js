import { spawn } from 'node:child_process'
import readline from 'node:readline'
import fs from 'node:fs'
import { defaultEngineRoot, nativeHelperCandidates, pythonCandidates } from './paths.js'

/**
 * Environment a helper child is allowed to see.
 *
 * Official `.mcp.json` sets `env_vars: []` (zero inheritance) plus an explicit
 * `env` block; the DSH sidecar used to forward the whole parent environment,
 * leaking every host credential to a process that can drive the desktop
 * (MCP-08, MCP-11). Only names a helper actually needs are copied; `DSH_*` is
 * the plugin's own namespace and is always forwarded.
 */
export const BASE_ENV_ALLOWLIST = [
  'PATH', 'PATHEXT', 'SystemRoot', 'SystemDrive', 'windir', 'TEMP', 'TMP',
  'USERPROFILE', 'HOMEDRIVE', 'HOMEPATH', 'HOME', 'LOCALAPPDATA', 'APPDATA',
  'PROGRAMDATA', 'PROGRAMFILES', 'ProgramW6432', 'ProgramFiles(x86)',
  'COMMONPROGRAMFILES', 'COMMONPROGRAMFILES(X86)', 'NUMBER_OF_PROCESSORS',
  'PROCESSOR_ARCHITECTURE', 'PROCESSOR_IDENTIFIER', 'COMSPEC', 'COMPUTERNAME',
  'USERNAME', 'USERDOMAIN', 'SESSIONNAME', 'OS',
  'PYTHON', 'PYTHONPATH', 'PYTHONHOME', 'PYTHONIOENCODING', 'PYTHONUTF8',
  'CODEX_HOME', 'DSH_HOME',
]

/**
 * Build the child environment from an explicit allowlist.
 *
 * @param {object} [extra] values the caller adds (already-known keys)
 * @param {string[]} [extraAllowlist] additional names to forward from the host
 * @returns {{ env: object, injected: string[], excluded: string[] }}
 */
export function sanitizedEnvironment(extra = {}, extraAllowlist = []) {
  const allow = new Set(
    BASE_ENV_ALLOWLIST.concat(Array.isArray(extraAllowlist) ? extraAllowlist : [])
      .map(name => String(name).toLowerCase()),
  )
  const env = {}
  const excluded = []
  for (const [name, value] of Object.entries(process.env)) {
    if (value === undefined) continue
    const lower = name.toLowerCase()
    if (allow.has(lower) || lower.startsWith('dsh_') || name.startsWith('DSH_COMPUTER_USE_')) env[name] = value
    else excluded.push(name)
  }
  for (const [name, value] of Object.entries(extra)) {
    if (value !== undefined && value !== null) env[name] = String(value)
  }
  return { env, injected: Object.keys(env).sort(), excluded: excluded.sort() }
}

class HelperProcess {
  constructor(preserveOnTimeout = false, onInterrupt = null) {
    this.onInterrupt = onInterrupt
    this.child = null
    this.rl = null
    this.pending = new Map()
    this.nextId = 1
    this.kind = ''
    this.preserveOnTimeout = preserveOnTimeout
    /** Times a request budget was enforced by killing this process (diagnostics). */
    this.killedOnTimeout = 0
  }

  get alive() {
    return Boolean(this.child) && this.child.exitCode == null
  }

  attach(child, kind) {
    this.dispose()
    this.kind = kind
    this.child = child
    child.stdout?.setEncoding?.('utf8')
    child.stderr?.setEncoding?.('utf8')
    this.rl = readline.createInterface({ input: child.stdout })
    this.rl.on('line', line => this.onLine(line))
    child.stderr?.on('data', chunk => {
      const text = String(chunk).trim()
      // The helper routes diagnostics to stderr only (official: eprintln!).
      if (text) console.error(`[dsh-computer-use:${kind}] ${text}`)
    })
    // Official helper_transport: never let the sidecar keep the host alive, and
    // kill it when the host exits so no orphan helper survives.
    this.unrefHandle(child)
    this.unrefHandle(child.stdin)
    this.unrefHandle(child.stdout)
    this.unrefHandle(child.stderr)
    const killOnParentExit = () => {
      if (child.exitCode == null && child.signalCode == null) {
        try {
          child.kill()
        } catch {
          // already gone
        }
      }
    }
    process.once('exit', killOnParentExit)
    child.on('exit', (code, signal) => {
      process.off('exit', killOnParentExit)
      const error = new Error(
        code === 130
          ? USER_INTERRUPT_MESSAGE
          : `computer-use ${kind} sidecar exited (${code ?? signal ?? 'unknown'})`,
      )
      for (const pending of [...this.pending.values()]) pending.reject(error)
      if (this.child === child) this.child = null
      // Official helper_transport: a physical Escape does not just fail the call in
      // flight, it ends Computer Use for the rest of the turn. Without this the next
      // request would silently respawn the helper and the model would keep driving
      // the desktop after the user told it to stop.
      if (typeof this.onInterrupt === 'function') {
        this.onInterrupt(code)
      }
    })
  }

  unrefHandle(handle) {
    if (handle && typeof handle.unref === 'function') handle.unref()
  }

  onLine(line) {
    const text = String(line).trim()
    if (!text) return
    let message
    try {
      message = JSON.parse(text)
    } catch {
      console.error(`[dsh-computer-use] non-JSON sidecar line: ${text.slice(0, 200)}`)
      return
    }
    const pending = this.pending.get(message.id)
    if (!pending) return
    if (message.ok === false) {
      if (message.approvalRequest) {
        pending.resolve({ approvalRequest: message.approvalRequest, error: message.error, code: message.code, kind: message.kind })
        return
      }
      const err = new Error(message.error || 'computer-use helper request failed')
      // P1: structured classification from the helper (err.code wins over regex matching
      // in errors/friendlyActionError).
      err.code = message.code
      err.kind = message.kind
      pending.reject(err)
      return
    }
    if (message.ok === true) {
      pending.resolve(message.result)
      return
    }
    // Compatibility: a JSON-RPC v2 helper answers `{jsonrpc,id,result}` or
    // `{jsonrpc,id,error}`. The sidecar no longer *writes* `jsonrpc` (RPC-1),
    // but a future pipe host or a replayed fixture may.
    if (message.error) {
      const approval = message.error.data && message.error.data.approvalRequest
      if (approval) {
        pending.resolve({ approvalRequest: approval, error: message.error.message })
        return
      }
      const err = new Error(message.error.message || 'sidecar error')
      err.code = message.error.code
      err.kind = message.error.kind
      pending.reject(err)
      return
    }
    pending.resolve(message.result)
  }

  write(message) {
    if (!this.child?.stdin) throw new Error('computer-use sidecar is not running')
    this.child.stdin.write(`${JSON.stringify(message)}\n`)
  }

  rawRequest(method, params = {}, signal, timeoutMs, meta, keepAlive = false) {
    const id = this.nextId++
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        // The note after the method name is the model-visible contract: a transport
        // timeout here KILLS the helper (below), so the next Computer Use call pays
        // a cold start — helper spawn plus (for Chromium targets) the accessibility
        // tree warm-up. Without the note a model that retries immediately reads the
        // slower response as a second failure instead of the cost of the first one.
        const error = new Error(
          `computer-use request timed out: ${method} ` +
            '(the helper process was killed to enforce the budget; ' +
            'the next Computer Use call cold-starts the helper and will be slower)',
        )
        // Official transport: a timeout kills the helper and rejects every
        // pending request, because the helper's state can no longer be trusted.
        // preserveHelperOnTimeout keeps it alive for debugging only.
        //
        // `keepAlive` is the narrower version of that for a request that is *slow* rather
        // than wedged: the installed-app catalog of `list_apps` keeps making progress (the
        // slow-request log shows it finishing after the budget), and killing the helper
        // throws the warm catalog away, so the next attempt would be cold again.
        if (this.preserveOnTimeout || keepAlive) {
          finish(reject, error)
          return
        }
        const child = this.child
        finish(reject, error)
        this.killedOnTimeout += 1
        for (const pending of [...this.pending.values()]) {
          clearTimeout(pending.timer)
          pending.reject(error)
        }
        this.pending.clear()
        this.dispose()
        if (child) {
          try {
            child.kill()
          } catch {
            // already gone
          }
        }
      }, timeoutMs)
      const finish = (fn, value) => {
        if (!this.pending.has(id)) return
        this.pending.delete(id)
        clearTimeout(timer)
        signal?.removeEventListener('abort', onAbort)
        fn(value)
      }
      const onAbort = () => {
        try {
          // `cancel` only drops the overlay and the in-flight action
          // (`interrupt::cancel_work`). It must NOT be `interrupt`: that RPC latches the
          // turn as stopped, which the helper reports as "stopped by the user with the
          // physical Escape key". A DSH tool call is aborted for reasons that have nothing
          // to do with the user -- the harness kills tool calls at its own budget (25 s) --
          // and latching there ended a turn the operator never stopped (session b9bdf958:
          // `list_apps` hit the harness budget, the abort latched STOPPED, and the next
          // call told the model the user had pressed Escape while the operator pressed
          // nothing).
          this.write({ id: this.nextId++, method: 'cancel', params: {} })
        } catch {
          // sidecar may already be gone
        }
        finish(reject, Object.assign(new Error('Computer Use call aborted'), { name: 'AbortError' }))
      }
      if (signal?.aborted) {
        onAbort()
        return
      }
      if (signal) signal.addEventListener('abort', onAbort, { once: true })
      this.pending.set(id, {
        resolve: value => finish(resolve, value),
        reject: error => finish(reject, error),
        timer,
      })
      try {
        this.write({
          id,
          method,
          params,
          meta: Object.assign({ 'x-oai-cua-request-budget-ms': timeoutMs }, meta || {}),
        })
      } catch (error) {
        this.pending.delete(id)
        finish(reject, error)
      }
    })
  }

  dispose() {
    const child = this.child
    this.child = null
    try {
      this.rl?.close()
    } catch {
      // already closed
    }
    this.rl = null
    for (const pending of this.pending.values()) {
      pending.reject(new Error('computer-use sidecar disposed'))
    }
    this.pending.clear()
    if (!child) return
    try {
      child.stdin?.end()
    } catch {
      // already closed
    }
    child.kill()
  }
}

/**
 * Official helper_transport.js USER_INTERRUPT_MESSAGE: what the model is told
 * when a physical Escape stops Computer Use (helper exit status 130).
 */
export const USER_INTERRUPT_MESSAGE =
  'Computer Use was stopped by the user with the physical Escape key. ' +
  'Stop your work, do not call further Computer Use tools in this turn, ' +
  'and send a final message noting that the user stopped Computer Use.'

/**
 * Turn scope key, matching the helper's `<session>\u0000<turn>` identity. `null` when
 * the caller did not carry a turn, in which case interrupts are not turn-scoped.
 */
export function turnScopeOf(meta) {
  const conversation = String(meta?.conversationId || meta?.session_id || '')
  const turn = String(meta?.turnId || meta?.turn_id || '')
  return conversation && turn ? conversation + '\u0000' + turn : null
}

/**
 * Official interrupt marker: `<home>/cache/computer-use/interrupts/<session>/<turn>`,
 * sanitised exactly like the helper's notify::interrupt_flag_path.
 */
export function interruptMarkerPath(scope) {
  if (!scope) return null
  const [conversation, turn] = String(scope).split('\u0000')
  if (!conversation || !turn) return null
  const safe = value => String(value).replace(/[^A-Za-z0-9._-]/g, '_')
  const home =
    process.env.DSH_HOME ||
    process.env.CODEX_HOME ||
    pathJoin(process.env.USERPROFILE || process.env.HOME || '', '.dsh')
  if (!home) return null
  return pathJoin(home, 'cache', 'computer-use', 'interrupts', safe(conversation), safe(turn))
}

function pathJoin(...parts) {
  const filtered = parts.filter(part => part !== undefined && part !== null && part !== '')
  if (filtered.length === 0) return ''
  return filtered
    .map((part, index) => {
      const text = String(part)
      if (index === 0) return text.replace(/[\\/]+$/, '')
      return text.replace(/^[\\/]+/, '').replace(/[\\/]+$/, '')
    })
    .join('\\')
}

const NATIVE_CALLS = new Set([
  'list_windows',
  'get_window',
  'list_apps',
  'launch_app',
  'get_window_state',
  'click',
  'click_element',
  'press_key',
  'type_text',
  'scroll',
  'scroll_element',
  'set_value',
  'drag',
  'perform_secondary_action',
  'select_text',
  'activate_window',
  'batch_actions',
  'fill_form',
  'session_note',
  'session_state',
  'end_turn',
  'diagnostic_state',
  // Native arms in helper-rs/src/main.rs: audio at :582-587 and the window-v1
  // aliases at :418-421, so these must NOT fall through to the Python catalog.
  // (This block previously named `get_app_state`/`paste`/`select_text`; those have
  // no native arm, so they were removed and `exports-check`'s "paste must use
  // Python" assertion holds.)
  'start_audio_recording',
  'stop_audio_recording',
  // DSH extension (helper-rs/src/main.rs): clear the human-input monitor without a
  // capture. Not listed here it would fall through to the Python catalog and fail.
  'clear_user_input',
  'observe',
  'type',
  'keypress',
  'window',
])

export function pythonCatalog(surface) {
  const name = String(surface || '')
  if (name === 'browser' || name === 'all') return true
  if (name === 'mac') return process.platform !== 'darwin'
  return false
}

export function usesPython(method, params = {}) {
  if (method === 'tools') return pythonCatalog(params.surface)
  if (method !== 'call') return false
  const name = String(params.name || '')
  if (!name) return false
  if (name === 'batch_actions') {
    const actions = params.arguments && Array.isArray(params.arguments.actions) ? params.arguments.actions : []
    if (actions.some(item => item && item.name && !NATIVE_CALLS.has(String(item.name)))) return true
  }
  return !NATIVE_CALLS.has(name)
}

export function mergeToolLists(native, py, surface) {
  const nativeTools = Array.isArray(native?.tools) ? native.tools : []
  const pyTools = Array.isArray(py?.tools) ? py.tools : []
  const seen = new Set(nativeTools.map(tool => tool && tool.name).filter(Boolean))
  const extra = pyTools.filter(tool => tool && tool.name && !seen.has(tool.name))
  return {
    tools: nativeTools.concat(extra),
    surface,
    deferred: extra.length ? 'python' : native?.deferred,
    disabledMemberIds: py?.disabledMemberIds || native?.disabledMemberIds || [],
  }
}

export class Sidecar {
  constructor(config) {
    this.config = config
    const preserve = config.preserveHelperOnTimeout === true
    this.primary = new HelperProcess(preserve, code => this.noteHelperExit(code))
    this.python = new HelperProcess(preserve)
    this.chain = Promise.resolve()
    /**
     * Official transport: the current turn scope, used to send `end_turn` on change.
     * This state belongs to the Sidecar -- it was previously initialised on the helper
     * process instead, so the Sidecar read `undefined`, treated it as "a previous
     * turn exists" and sent a bogus `end_turn` before every first call.
     */
    this.turnKey = null
    this.turnMeta = null
    /**
     * The turn scope stopped by a physical Escape, as a single key.
     *
     * Official helper_transport keeps exactly one `#interruptedTurn` (`y`) and clears it
     * as soon as a *different* turn arrives, rather than accumulating scopes.
     */
    this.interruptedTurn = null
    /**
     * What the last spawn actually injected: variable names only, never values
     * (MCP-08 audit trail surfaced through `computer_use_health`).
     */
    this.envReport = null
  }

  /**
   * The helper owned the turn state, so when it exits there is no turn left to end:
   * clearing the scope stops the next turn from sending a pointless `end_turn` to a
   * helper that never served it. An exit status of 130 also pins the turn as stopped.
   */
  noteHelperExit(code) {
    if (code === 130 && this.turnKey) this.interruptedTurn = this.turnKey
    this.turnKey = null
    this.turnMeta = null
  }

  /**
   * Official helper_transport tracks the interrupt marker as well as exit status 130,
   * so a turn stays stopped even when the helper died before the transport noticed.
   */
  turnIsInterrupted(scope) {
    if (this.interruptedTurn !== null) {
      if (scope === this.interruptedTurn) return true
      // A different turn resets the stopped state, exactly like the official `y` field.
      if (scope) this.interruptedTurn = null
    }
    if (!scope) return false
    const marker = interruptMarkerPath(scope)
    if (marker && fs.existsSync(marker)) {
      this.interruptedTurn = scope
      return true
    }
    return false
  }

  get child() {
    return this.primary.child
  }

  /**
   * `backend: fake` means "no real desktop". The native helper has no fake mode: with a built
   * `dsh-computer-use.exe` on the machine it would answer the call by observing and driving the
   * *real* machine, which is both wrong for tests and unsafe. The Python engine implements the
   * fake window2 surface, so a fake backend must start and use Python as its primary.
   */
  get fakeBackend() {
    return String(this.config.backend || '').toLowerCase() === 'fake'
  }

  async start() {
    this.dispose()
    const engineRoot = this.config.engineRoot || defaultEngineRoot()
    const errors = []
    for (const exe of this.fakeBackend ? [] : nativeHelperCandidates(engineRoot)) {
      if (!exe || !fs.existsSync(exe)) continue
      try {
        await this.spawnNative(engineRoot, exe)
        await this.primary.rawRequest('health', {}, undefined, this.config.timeoutMs || 10_000)
        return
      } catch (error) {
        errors.push(`${exe}: ${error.message}`)
        this.primary.dispose()
      }
    }
    for (const invocation of pythonCandidates(this.config.pythonPath)) {
      try {
        await this.spawnPython(engineRoot, invocation, this.primary)
        await this.primary.rawRequest('health', {}, undefined, this.config.timeoutMs || 10_000)
        return
      } catch (error) {
        errors.push(`${invocation.command}: ${error.message}`)
        this.primary.dispose()
      }
    }
    throw new Error(`computer-use sidecar failed to start (${errors.join('; ')})`)
  }

  /**
   * Official CLI surface: exactly `--parent-pid <pid>` plus the bare invocation.
   * The official helper has no `serve` subcommand, no observation-expiry flag
   * and no `--allowed-app`; its hand-written parser aborts on unknown argv. DSH knobs
   * therefore travel through the environment (see DSH_COMPUTER_USE_*) and the
   * allow-list stays enforced inside the helper's policy layer.
   */
  async spawnNative(engineRoot, exe) {
    const args = ['--parent-pid', String(process.pid)]
    const extra = {}
    const allowed = (this.config.allowedApps || []).filter(Boolean)
    if (allowed.length > 0) extra.DSH_COMPUTER_USE_ALLOWED_APPS = allowed.join(',')
    // DSH-only knob: the synthetic cursor sprite scale (1 = the official size). It has to
    // travel through the environment because the official helper's argv parser rejects
    // unknown flags, and the helper must stay able to run a stock official binary.
    const cursorScale = Number(this.config.cursorScale)
    if (Number.isFinite(cursorScale) && cursorScale > 0 && cursorScale !== 1) {
      extra.DSH_COMPUTER_USE_CURSOR_SCALE = String(cursorScale)
    }
    // DSH-only knob: how the pill is kept out of the model's screenshots. Sent on every
    // spawn, including the default, so the plugin's default and the helper's fallback
    // cannot drift apart (a mismatch used to mean "the pill is invisible on some GPUs").
    const exclusion = String(this.config.overlayCaptureExclusion || 'mask')
    if (['mask', 'wda', 'off'].includes(exclusion)) {
      extra.DSH_CU_OVERLAY_CAPTURE_EXCLUSION = exclusion
    }
    const { env, injected, excluded } = sanitizedEnvironment(extra, this.config.envAllowlist)
    const child = spawn(exe, args, {
      cwd: engineRoot,
      env,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    })
    this.recordEnvironment('native', injected, excluded)
    await waitSpawn(child, this.startupBudget())
    this.primary.attach(child, 'native')
  }

  async spawnPython(engineRoot, { command, prefixArgs }, target) {
    const args = [
      ...prefixArgs,
      '-u',
      '-m',
      'computer_use',
      '--backend',
      this.config.backend || 'windows',
      '--surface',
      this.config.surface || 'computer',
      '--max-image-edge',
      // `|| 1280` silently turned the new 0 (official, no cap) back into a downscale.
      String(this.config.maxImageEdge ?? 0),
    ]
    if (this.config.stealFocus === false) args.push('--no-steal-focus')
    // Python engine keeps its own CLI, where --allowed-app is repeatable.
    for (const app of this.config.allowedApps || []) {
      if (app) args.push('--allowed-app', String(app))
    }
    args.push('--parent-pid', String(process.pid))
    args.push('serve')
    const live = (this.config.backend || 'live') !== 'fake'
    const extra = {
      PYTHONUNBUFFERED: '1',
      PYTHONPATH: engineRoot + (process.env.PYTHONPATH ? `${pathSep()}${process.env.PYTHONPATH}` : ''),
      COMPUTER_USE_CDP: live ? '1' : (process.env.COMPUTER_USE_CDP || '0'),
      COMPUTER_USE_EXTENSION: live ? '1' : (process.env.COMPUTER_USE_EXTENSION || '0'),
    }
    const { env, injected, excluded } = sanitizedEnvironment(extra, this.config.envAllowlist)
    const child = spawn(command, args, {
      cwd: engineRoot,
      env,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    })
    this.recordEnvironment('python', injected, excluded)
    await waitSpawn(child, this.startupBudget())
    target.attach(child, 'python')
  }

  /** Official `.mcp.json` `startup_timeout_sec: 120` upper bound, DSH default 15 s. */
  startupBudget() {
    const value = Number(this.config.startupTimeoutMs)
    return Number.isFinite(value) && value > 0 ? value : 15_000
  }

  recordEnvironment(kind, injected, excluded) {
    this.envReport = {
      kind,
      injected,
      // Names only: a value would re-leak exactly what the allowlist removed.
      excludedNames: excluded.slice(0, 64),
      excludedCount: excluded.length,
      allowlist: BASE_ENV_ALLOWLIST,
    }
  }

  async ensurePython() {
    if (this.primary.kind === 'python' && this.primary.alive) return this.primary
    if (this.python.alive) return this.python
    const engineRoot = this.config.engineRoot || defaultEngineRoot()
    const errors = []
    for (const invocation of pythonCandidates(this.config.pythonPath)) {
      try {
        await this.spawnPython(engineRoot, invocation, this.python)
        await this.python.rawRequest('health', {}, undefined, this.config.timeoutMs || 10_000)
        return this.python
      } catch (error) {
        errors.push(`${invocation.command}: ${error.message}`)
        this.python.dispose()
      }
    }
    throw new Error(`computer-use python sidecar failed to start (${errors.join('; ')})`)
  }

  /**
   * Official helper_transport #ensureTurn: when the turn scope changes, send
   * `end_turn` for the previous scope (failures ignored) before the next request.
   * The helper treats that as "hide the overlay, flush the observation lease and
   * re-arm for the next turn". The previous turn's meta travels with it (TURN-2),
   * exactly like official `#currentTurnMeta`.
   *
   * @param {object} meta request meta ({ conversationId, turnId, ... })
   * @param {number} timeoutMs
   */
  async ensureTurn(meta, timeoutMs) {
    const nextKey = turnScopeOf(meta)
    const previousKey = this.turnKey
    if (previousKey !== null && previousKey !== nextKey && this.primary.alive) {
      try {
        // Sent straight to the helper. Going through this.request() would queue behind
        // the very call that is awaiting this method, deadlocking the whole sidecar --
        // which is exactly what happened the first time a turn actually changed.
        await this.primary.rawRequest('end_turn', {}, undefined, timeoutMs, this.turnMeta || {})
      } catch {
        // official: end_turn failures are ignored
      }
    }
    this.turnKey = nextKey
    this.turnMeta = nextKey === null ? null : meta
  }

  /**
   * Host-initiated turn end (TURN-1/PKG-04): the official plugin declares
   * Stop/Interrupt/SubagentStop hooks that call `turn_ended`, and DSH's host
   * service calls this at `turn/end`/session disposal. Unlike `request()` it
   * never spawns a helper: an idle sidecar has no turn to end.
   *
   * @param {{ session_id?: string, turn_id?: string }} [params]
   * @returns {Promise<{ ok: boolean, skipped?: boolean }>}
   */
  async endTurn(params = {}) {
    const meta = this.turnMeta || {}
    this.turnKey = null
    this.turnMeta = null
    if (!this.primary.alive) return { ok: true, skipped: true }
    try {
      await this.primary.rawRequest('end_turn', params, undefined, this.timeoutFor('end_turn', params), meta)
    } catch {
      // official: end_turn failures are ignored
    }
    return { ok: true }
  }

  /**
   * @param {string} method
   * @param {object} [params]
   * @returns {number} request timeout in ms (official 10 s / launch_app 15 s)
   */
  timeoutFor(method, params) {
    const base = Number(this.config.timeoutMs) > 0 ? Number(this.config.timeoutMs) : 10_000
    if (method === 'call' && params && params.name === 'launch_app') {
      const launch = Number(this.config.launchAppTimeoutMs)
      return launch > 0 ? launch : 15_000
    }
    if (method === 'call' && params && params.name === 'list_apps') {
      // The catalog build is the one request that can plausibly outrun 10 s on a cold or
      // busy machine. This budget is used verbatim rather than clamped to the base budget:
      // it has to stay below the harness's 25 s tool budget (see Config.listAppsTimeoutMs)
      // and a caller who asks for a short one means it.
      const listApps = Number(this.config.listAppsTimeoutMs)
      return listApps > 0 ? listApps : 20_000
    }
    if (method === 'call' && params && params.name === 'get_window_state') {
      // Same shape as list_apps: the accessibility dump of a heavy Electron/Chromium
      // window is the other request that can plausibly outrun the base budget. Since
      // AX-WEDGE the helper answers within its budget (degraded tree), so this knob
      // only decides how long a *healthy* dump may run; keep it below the harness's
      // 25 s tool budget. 0/absent falls back to the base budget.
      const windowState = Number(this.config.windowStateTimeoutMs)
      if (windowState > 0) return windowState
    }
    if (method === 'call' && params && params.name === 'type_text') {
      // The landing-check ladder makes several accessibility reads per paste; on a
      // slow window those add up. Same constraints as above. 0/absent falls back to
      // the base budget.
      const typeText = Number(this.config.typeTextTimeoutMs)
      if (typeText > 0) return typeText
    }
    return base
  }

  request(method, params = {}, signal) {
    if (method === 'interrupt' || method === 'shutdown' || method === 'cancel') {
      const jobs = []
      if (this.primary.alive) jobs.push(this.primary.rawRequest(method, params, signal, 5_000).catch(() => ({ ok: true })))
      if (this.python.alive) jobs.push(this.python.rawRequest(method, params, signal, 5_000).catch(() => ({ ok: true })))
      if (jobs.length === 0) return Promise.resolve({ ok: true })
      return Promise.all(jobs).then(() => ({ ok: true }))
    }
    const run = async () => {
      if (method === 'call') {
        const scope = turnScopeOf(params?.meta)
        if (this.turnIsInterrupted(scope)) {
          throw new Error(USER_INTERRUPT_MESSAGE)
        }
      }
      if (!this.primary.alive) await this.start()
      // Official transport budget: 10 s for every request, 15 s for launch_app.
      // `x-oai-cua-request-budget-ms` carries this into the helper, which honours
      // it as a whole-request budget ("computer-use request budget exhausted").
      const timeoutMs = this.timeoutFor(method, params)
      if (method === 'tools') return this.listTools(params, signal, timeoutMs)
      // A catalog build that outruns its budget must not cost the warm catalog.
      const keepAlive = method === 'call' && params?.name === 'list_apps'
      const session = usesPython(method, params) ? await this.ensurePython() : this.primary
      // Turn bookkeeping lives on the Sidecar (it owns the previous turn scope), not
      // on the helper process. Calling it on `session` threw a TypeError and made
      // every Computer Use tool call fail before it ever reached the helper.
      if (method === 'call') await this.ensureTurn(params?.meta || {}, timeoutMs)
      return session.rawRequest(method, params, signal, timeoutMs, undefined, keepAlive)
    }
    const task = this.chain.then(run, run)
    this.chain = task.then(() => undefined, () => undefined)
    return task
  }

  rawRequest(method, params = {}, signal) {
    return this.request(method, params, signal)
  }

  /**
   * How often each helper process was killed to enforce a request budget.
   * Surfaced through `computer_use_health`: a rising number means some call class
   * regularly outruns its budget (before AX-WEDGE that call was get_window_state
   * on heavy Electron windows), and every kill costs the next call a cold start.
   * @returns {{ native: number, python: number }}
   */
  killStats() {
    return {
      native: this.primary.killedOnTimeout,
      python: this.python.killedOnTimeout,
    }
  }

  async listTools(params = {}, signal, timeoutMs) {
    const surface = String(params.surface || this.config.surface || 'computer')
    const payload = { ...params, surface }
    if (surface === 'browser' || surface === 'mac') {
      return (await this.ensurePython()).rawRequest('tools', payload, signal, timeoutMs)
    }
    const nativeSurface = surface === 'all' ? 'desktop' : surface
    const native = await this.primary.rawRequest('tools', { ...payload, surface: nativeSurface }, signal, timeoutMs)
    if (surface !== 'all' && native?.deferred !== 'python') return native
    try {
      const py = await (await this.ensurePython()).rawRequest(
        'tools',
        { ...payload, surface: surface === 'all' ? 'all' : surface },
        signal,
        timeoutMs,
      )
      return mergeToolLists(native, py, surface)
    } catch (error) {
      if (surface === 'browser' || surface === 'mac') throw error
      return { ...native, pythonError: String(error.message || error) }
    }
  }

  dispose() {
    this.turnKey = null
    this.turnMeta = null
    this.python.dispose()
    this.primary.dispose()
  }
}

/**
 * Wait for a freshly spawned child to become ready.
 *
 * The official transport budgets helper startup at `startup_timeout_sec: 120`;
 * this function used to wait forever, so a helper that never became ready hung
 * the session with no diagnostic. A startup timeout must be distinguishable
 * from a per-request timeout ("computer-use request timed out: <method>").
 *
 * @param {import('node:child_process').ChildProcess} child
 * @param {number} timeoutMs
 */
export async function waitSpawn(child, timeoutMs) {
  const budget = Number.isFinite(timeoutMs) && timeoutMs > 0 ? timeoutMs : 15_000
  await new Promise((resolve, reject) => {
    let settled = false
    function done(error) {
      if (settled) return
      settled = true
      clearTimeout(timer)
      child.off('error', onError)
      if (error) {
        try {
          child.kill()
        } catch {
          // already gone
        }
        reject(error)
        return
      }
      resolve()
    }
    function onError(error) {
      done(error)
    }
    function rejectNotReady() {
      done(new Error(`computer-use helper did not become ready within ${budget} ms (startup timeout)`))
    }
    const timer = setTimeout(rejectNotReady, budget)
    child.once('error', onError)
    if (child.pid) done()
    else child.once('spawn', () => done())
  })
  await new Promise(resolve => setTimeout(resolve, 40))
  if (child.exitCode != null) {
    throw new Error(`computer-use sidecar exited (${child.exitCode})`)
  }
}

function pathSep() {
  return process.platform === 'win32' ? ';' : ':'
}

