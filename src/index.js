/**
 * HOST-plane Computer Use sidecar. Pointer, overlay, capture, and the Chrome
 * extension hub are process-wide: they belong here, not in an agent preset.
 * @module dsh-computer-use
 */

import fs from 'node:fs'
import path from 'node:path'
import { Service } from '@deepseek-ai/cordis'
import Schema from '@deepseek-ai/schemastery'
import { Sidecar, BASE_ENV_ALLOWLIST } from './sidecar.js'
import { defaultEngineRoot } from './paths.js'
import { documentationSummary, promptAssetStatus } from './prompt.js'
import { createExperience } from './experience.js'

/**
 * Declarative turn-lifecycle hooks (PKG-04).
 *
 * The official `computer-use` plugin declares `Stop` / `Interrupt` /
 * `SubagentStop` hooks that all call the MCP tool `turn_ended`
 * (`plugin.json:4-55`). DSH has no MCP tool to call, so this table is the
 * executable equivalent: every name maps onto the `session/event` dispatch
 * that triggers the same `end_turn`. The manifest ships it so a rewriter can
 * regenerate the behaviour without reading this file.
 */
export const TURN_LIFECYCLE_HOOKS = {
  Stop: 'session/event:turn/end',
  Interrupt: 'session/event:turn/end',
  SubagentStop: 'session/event:turn/end',
}

/** Official `interface.capabilities` triple (PKG-05), mapped to DSH semantics. */
export const SERVICE_CAPABILITIES = ['Interactive', 'Read', 'Write']

/**
 * Describe the screenshot downscale cap from the single source of truth when it
 * is reachable. `maxImageEdge` is a DSH extension: the official string table has
 * no max-edge concept (`parity/official-constants.json` `maxImageEdge.official`
 * is null, `kind: 'dsh-extension'`). Never silently rewrite the user's value.
 *
 * @param {number} maxImageEdge
 * @param {string} [engineRoot]
 * @returns {{ maxImageEdge: object }}
 */
export function captureLimits(maxImageEdge, engineRoot) {
  const limits = {
    maxImageEdge: {
      value: maxImageEdge,
      official: null,
      kind: 'dsh-extension',
      source: 'built-in',
      note: 'DSH-only downscale on the Python/plugin surface; the Rust helper has no such concept. Raising it multiplies the token cost of every screenshot.',
    },
  }
  try {
    const file = path.join(engineRoot || defaultEngineRoot(), 'parity', 'official-constants.json')
    const constants = JSON.parse(fs.readFileSync(file, 'utf8'))
    if (constants && constants.maxImageEdge) {
      limits.maxImageEdge.official = constants.maxImageEdge.official
      limits.maxImageEdge.kind = constants.maxImageEdge.kind || limits.maxImageEdge.kind
      limits.maxImageEdge.source = 'parity/official-constants.json'
      if (constants.maxImageEdge.note) limits.maxImageEdge.note = constants.maxImageEdge.note
    }
  } catch {
    // The constants file is a repository artifact, not a runtime dependency.
  }
  return limits
}

/**
 * Resolve the desktop backend based on raw configuration and platform.
 *
 * Explicit configuration always takes precedence. When unspecified (undefined,
 * null, or empty string), falls back by platform:
 * - 'linux' -> 'linux'
 * - 'win32' -> 'windows'
 * - other platforms (including 'darwin') -> 'windows' (conservative parity)
 *
 * @param {string|undefined|null} rawBackend
 * @param {string} [platform] defaults to process.platform
 * @returns {string}
 */
export function resolveBackend(rawBackend, platform = process.platform) {
  if (rawBackend !== undefined && rawBackend !== null && rawBackend !== '') {
    return rawBackend
  }
  if (platform === 'linux') {
    return 'linux'
  }
  return 'windows'
}

export const Config = Schema.object({
  pythonPath: Schema.string().default(''),
  engineRoot: Schema.string().default(''),
  backend: Schema.union(['windows', 'linux', 'live', 'fake', 'helper']),
  /**
   * Default tool surface. `computer` is the official 13-method whitelist; the
   * `gated` fallback used to hand the model the whole 18-row table (TC-01).
   */
  surface: Schema.string().default('computer'),
  /**
   * DSH-only switch for the browser-only Python channel on Linux (`auto` by default).
   *
   * The Linux native helper serves the desktop faces and no browser catalog at all; the
   * `tab_*` tools and the ExtensionHub bridge (127.0.0.1:8765) live in the Python engine.
   * `auto` opens that second channel next to a healthy native helper when the configured
   * surface actually needs it (`computer`/`browser`/`all`). `off` restores the previous
   * behaviour (Python only on demand). Windows is never affected: there the Python engine
   * is the primary and must not be started twice.
   */
  browserChannel: Schema.string().default('auto'),
  /**
   * DSH-only override for the ExtensionHub port the Python engine binds
   * (browser_api.py reads COMPUTER_USE_EXTENSION_PORT). Empty keeps the engine default 8765.
   */
  extensionPort: Schema.number().default(0),
  stealFocus: Schema.boolean().default(true),
  /**
   * DSH-only screenshot downscale cap, NOT an official field: the official
   * string table has no maxEdge/maxImageEdge concept
   * (parity/official-constants.json: `maxImageEdge.official = null`,
   * `kind: 'dsh-extension'`). The default stays 1280 because raising it
   * multiplies the vision token cost of every screenshot. Decision D-E: the official
   * behaviour (no cap) is the default, so this knob is opt-in token saving -- set it to
   * e.g. 1280 to downscale, or leave 0 to ship the screenshot the official helper ships.
   */
  maxImageEdge: Schema.number().default(0),
  /**
   * DSH-only scale for the synthetic cursor sprite (1 = the official size). The overlay is
   * driven by the display DPI alone: it neither reads nor writes the Windows cursor-size
   * setting, so this knob is the single place the pointer size is decided.
   */
  cursorScale: Schema.number().default(1),
  /**
   * How the status pill is kept out of the screenshots the model reads.
   *
   * `mask` (default) hides the pill in the compositor for the duration of one capture:
   * the operator keeps seeing it, the model never reads it back. `wda` applies
   * `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` to the pill window instead -- the
   * official-style affinity, which on some Windows/DWM/GPU combinations stops the DWM
   * from presenting the pill *on screen* as well (operator sees the fake cursor and no
   * pill, verified 2026-09-15). `off` leaves the pill in the frame.
   */
  overlayCaptureExclusion: Schema.union(['mask', 'wda', 'off']).default('mask'),
  approveLaunch: Schema.boolean().default(true),
  /** Request budget in ms. Official transport uses 10 s for every request. */
  timeoutMs: Schema.number().default(10000),
  /** Official transport uses 15 s for launch_app. */
  launchAppTimeoutMs: Schema.number().default(15000),
  /**
   * DSH extension: budget for `list_apps`, the one method that builds the installed-app
   * catalog before it can answer. Two constraints set it:
   *
   * - It must stay **below DSH's own tool-call budget (25 s)**. Otherwise the harness
   *   aborts the tool call first, and an abort is a heavier event than a clean timeout.
   * - It must be larger than the official 10 s, because a transport timeout *kills* the
   *   helper and therefore throws the warm catalog away. The sidecar's `keepAlive` keeps
   *   the helper alive for this method, so a slow build no longer costs the cache either.
   *
   * Measured 0.2-0.6 s warm once the request path stopped opening executables; a stale
   * catalog on a machine whose antivirus scans every open is the case this exists for.
   */
  listAppsTimeoutMs: Schema.number().default(20000),
  /**
   * Helper startup budget in ms. Official `.mcp.json` uses
   * `startup_timeout_sec: 120`; a helper that never becomes ready must reject
   * rather than hang the session (MCP-05).
   */
  startupTimeoutMs: Schema.number().default(15000),
  /**
   * Extra environment names forwarded to a helper child on top of
   * `BASE_ENV_ALLOWLIST`. The child never inherits the whole parent
   * environment (MCP-08).
   */
  envAllowlist: Schema.array(Schema.string()).default([]),
  /**
   * Kill and reject everything when a request times out (official default).
   * Set true to keep the helper alive for debugging.
   */
  preserveHelperOnTimeout: Schema.boolean().default(false),
  allowedApps: Schema.array(Schema.string()).default([]),
  /**
   * Server-level instruction (official `resources/server-instructions.md`,
   * 86 B, injected through `NODE_REPL_TOOL_OVERRIDES`). Surfaced through
   * `computer_use_health` because DSH has no MCP server header (MCP-02).
   */
  serverInstructions: Schema.string().default(
    'UI automation through the DeepSeek Harness Computer Use tools using the initialized session. Codex is not required.',
  ),
  /**
   * Local experience layer (DSH extension, not official). Notes written after a task
   * are attached, as an advisory text block, to the first observation of the same app
   * in a later turn. Nothing in this layer can block or gate a Computer Use call, and
   * the store is deliberately inside the plugin folder (decision D-1), gitignored.
   */
  experience: Schema.object({
    enabled: Schema.boolean().default(true),
    store: Schema.string().default(''),
    captureObservations: Schema.boolean().default(true),
    injectDigest: Schema.boolean().default(true),
    maxEntries: Schema.number().default(3),
    maxChars: Schema.number().default(900),
    staleAfterDays: Schema.number().default(90),
    includeStale: Schema.boolean().default(true),
    retainEntries: Schema.number().default(200),
    retainObservationDays: Schema.number().default(30),
    redactPaths: Schema.boolean().default(true),
  }),
})

/**
 * Process-wide Computer Use sidecar, published as the `dshComputerUse` service: the
 * name is DSH-namespaced so the official `computerUse` provider registry (0.1.6+)
 * can be mounted in the same profile without a service-name clash.
 *
 * Pointer, overlay, WGC, and the Chrome
 * extension hub must not be instantiated once per session.
 */
export default class ComputerUseService extends Service {
  static inject = []
  static Config = Config

  constructor(ctx, config) {
    super(ctx, 'dshComputerUse')
    const rawConfig = config || {}
    const backend = resolveBackend(rawConfig.backend)
    const surfaceSpecified = rawConfig.surface !== undefined && rawConfig.surface !== ''
    const surface = surfaceSpecified ? rawConfig.surface : (backend === 'linux' ? 'linux' : 'computer')
    this.config = {
      pythonPath: '',
      engineRoot: defaultEngineRoot(),
      stealFocus: true,
      maxImageEdge: 0,
      cursorScale: 1,
      overlayCaptureExclusion: 'mask',
      approveLaunch: true,
      timeoutMs: 10000,
      launchAppTimeoutMs: 15000,
      listAppsTimeoutMs: 20000,
      startupTimeoutMs: 15000,
      envAllowlist: [],
      preserveHelperOnTimeout: false,
      allowedApps: [],
      browserChannel: 'auto',
      extensionPort: 0,
      serverInstructions:
        'UI automation through the DeepSeek Harness Computer Use tools using the initialized session. Codex is not required.',
      ...config,
      backend,
      surface,
    }
    this.sidecar = new Sidecar(this.config)
    this.experience = createExperience(this.config.experience)
    this.approved = new Set()
    /**
     * Turn id per conversation, taken from the session events (`turn/start` /
     * `turn/end`). Tool calls must stamp THIS into their meta, never their own call id:
     * the sidecar sends `end_turn` whenever the turn scope changes, and the helper treats
     * `end_turn` as "flush the observation lease". A per-call id therefore made every call
     * a new turn, so `get_window_state` followed by `click` answered
     * `coordinate input target is unavailable` -- which is exactly how the first real
     * end-to-end task failed (see parity/sidecar-observe-act.mjs).
     */
    this.turnByConversation = new Map()
    /** Conversation that last drove the desktop, for `session/disposed` cleanup. */
    this.lastConversationId = null
    /** Every conversation with an open CUA turn, for the SubagentStop equivalent. */
    this.activeConversations = new Set()
    // The turn-end release has to live on this host-plane service, not in the
    // user-agent preset. Session events are dispatched with the *session store's*
    // scope key, which is the untagged host scope, and `@deepseek-ai/dsh-scope` only
    // admits a listener registered on an equal or enclosing scope:
    // "a bare session dispatches subject-less: scoped listeners never hear it"
    // (packages/core/session/tests/scoped.spec.ts). Agent sessions are prepared
    // through the loop's own (host) context, so a preset-scoped listener is a
    // descendant scope and never fires. That silently dead hook is why the synthetic
    // cursor was still on the desktop after the task finished.
    // The sidecar is process-wide, so another session ending its turn must not hide the
    // overlay while this session is still driving the desktop: only the conversation
    // that last made a Computer Use call may release it. `lastConversationId` is the
    // agent id the tool row stamps into every call (see tool.js `turnMeta`).
    //
    // Official parity (TURN-1/PKG-04): Stop/Interrupt/SubagentStop all deliver
    // `turn_ended`, i.e. the helper is told the turn ended, not merely asked to
    // hide the overlay. Both are sent here; `endTurn` is a no-op when no helper is
    // running, so an idle session never spawns one just to close a turn.
    ctx.on('session/event', (session, event) => {
      if (!event) return
      const conversationId = session?.id === undefined || session?.id === null ? null : String(session.id)
      if (event.type === 'turn/start') {
        if (conversationId !== null && event.data && event.data.turn !== undefined) {
          this.turnByConversation.set(conversationId, String(event.data.turn))
        }
        return
      }
      if (event.type !== 'turn/end') return
      const turn = event.data && event.data.turn !== undefined ? String(event.data.turn) : undefined
      // Close the experience turn for every session, not only the one that owns the
      // overlay: the observation buffer is keyed by conversation + turn and would leak.
      try {
        this.experience.settleTurn(conversationId, turn)
      } catch {
        // Advisory layer: a broken store must never break the turn lifecycle.
      }
      if (conversationId !== null) this.turnByConversation.delete(conversationId)
      const owns = this.ownsOverlay(session)
      const active = conversationId !== null && this.activeConversations.has(conversationId)
      if (!owns && !active) return
      if (conversationId !== null) this.activeConversations.delete(conversationId)
      if (owns) this.lastConversationId = null
      this.endTurn({
        session_id: conversationId === null ? undefined : conversationId,
        turn_id: turn,
      }).catch(() => {})
      if (owns) this.releaseOverlay().catch(() => {})
    })
    ctx.on('session/disposed', session => {
      const id = session?.id === undefined || session?.id === null ? null : String(session.id)
      try {
        this.experience.settleTurn(id, undefined)
      } catch {
        // Advisory layer: best effort.
      }
      if (id !== null) this.activeConversations.delete(id)
      if (!this.ownsOverlay(session)) return
      this.lastConversationId = null
      this.endTurn({ session_id: id === undefined ? undefined : id }).catch(() => {})
      this.releaseOverlay().catch(() => {})
    })
    ctx.effect(() => () => this.sidecar.dispose(), 'computer-use sidecar')
  }

  /**
   * The DSH turn id for a conversation, as reported by `session/event`. `undefined` means
   * this process has not seen a turn boundary yet; callers must then use a *stable* fallback
   * (never a per-call id).
   *
   * @param {string|number|null|undefined} conversationId
   * @returns {string|undefined}
   */
  currentTurnIdFor(conversationId) {
    if (conversationId === undefined || conversationId === null) return undefined
    return this.turnByConversation.get(String(conversationId))
  }

  /**
   * @returns {Promise<object>} sidecar health payload plus host-plane facts
   */
  async health() {
    const payload = await this.sidecar.request('health')
    const base = payload && typeof payload === 'object' ? payload : { value: payload }
    return {
      ...base,
      codexRequired: false,
      backend: this.config.backend,
      surface: this.config.surface,
      startupTimeoutMs: this.config.startupTimeoutMs,
      // MCP-02: the server-level instruction lives at a position that does not
      // grow with the tool catalog.
      serverInstructions: this.config.serverInstructions,
      capabilities: SERVICE_CAPABILITIES,
      // DSH extension, not official: surfaced so the token-cost trade-off is
      // visible and auditable instead of hidden in the config.
      captureLimits: captureLimits(this.config.maxImageEdge, this.config.engineRoot),
      // MCP-08/MCP-11: names only, so the allowlist itself is auditable.
      env: this.sidecar.envReport,
      envAllowlist: BASE_ENV_ALLOWLIST,
      // DSH extension, not official: the browser-only Python channel next to the Linux
      // native helper (state/surface/endpoint/error). `disabled` and `primary` are honest
      // states, not failures.
      pythonChannel: this.sidecar.pythonChannel,
      // MCP-12/SKILL-01: the on-demand documentation channel.
      promptAssets: promptAssetStatus(),
      documentation: documentationSummary(),
      turnLifecycle: TURN_LIFECYCLE_HOOKS,
      // DSH extension, not official: the machine-local experience layer (store path,
      // counts and the newest notes). Advisory notes only; nothing here gates a call.
      experience: this.experience.stats(),
    }
  }

  /**
   * @param {string} [surface]
   * @returns {Promise<{tools?: object[], surface?: string, deferred?: string}>}
   */
  tools(surface) {
    return this.sidecar.request('tools', { surface: surface || this.config.surface })
  }

  /**
   * @returns {Promise<{prompt?: string}>}
   */
  prompt() {
    return this.sidecar.request('prompt')
  }

  /**
   * @param {string} name
   * @param {object} [arguments_]
   * @param {AbortSignal} [signal]
   * @param {object} [meta]
   * @returns {Promise<object>}
   */
  call(name, arguments_, signal, meta) {
    const conversation = meta?.conversationId ?? meta?.session_id
    if (conversation !== undefined && conversation !== null && conversation !== '') {
      this.lastConversationId = String(conversation)
      this.activeConversations.add(String(conversation))
    }
    return this.sidecar.request('call', { name, arguments: arguments_ || {}, meta: meta || {} }, signal)
  }

  /**
   * @returns {Promise<object>}
   */
  interrupt() {
    return this.sidecar.request('interrupt')
  }

  /**
   * Whether `session` is the conversation that last drove Computer Use. Without a
   * recorded conversation the answer is yes: releasing an idle overlay is harmless,
   * while leaving a live one is the bug being fixed.
   * @param {object} [session]
   * @returns {boolean}
   */
  ownsOverlay(session) {
    if (this.lastConversationId === null) return true
    const id = session?.id
    if (id === undefined || id === null) return true
    return String(id) === this.lastConversationId
  }

  /**
   * Tell the helper the turn is over (official `turn_ended`). No helper is
   * spawned when none is running.
   * @param {{ session_id?: string, turn_id?: string }} [params]
   * @returns {Promise<object>}
   */
  endTurn(params) {
    return this.sidecar.endTurn(params || {})
  }

  /**
   * Hide overlay and restore the system cursor without ending the Computer Use turn.
   * Called at `turn/end` and `session/disposed`; a no-op while no helper is running,
   * because `Sidecar.request` answers `{ ok: true }` instead of spawning one.
   * @returns {Promise<object>}
   */
  releaseOverlay() {
    return this.sidecar.request('cancel', {})
  }

  /**
   * Tear down the helper process after a Computer Use session ends.
   * Next tool call respawns it.
   * @returns {Promise<void>}
   */
  async shutdownSidecar() {
    try {
      await this.sidecar.request('shutdown', {})
    } catch {
      // helper may already be gone
    }
    this.sidecar.dispose()
  }
}
